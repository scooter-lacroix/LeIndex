//! Query planning: which trigrams must a matching file contain?
//!
//! The pattern is parsed with `regex-syntax` and reduced to a boolean formula
//! over required literals (the same idea as Zoekt / Google Code Search):
//!
//! * a literal run needs all of its trigrams,
//! * a concatenation needs everything its parts need,
//! * an alternation needs any one branch (and gives up if a branch needs
//!   nothing),
//! * anything the index cannot speak to (`.*`, short classes, `x?`) needs
//!   nothing, which only ever widens the candidate set.
//!
//! A formula that needs nothing means "scan every file". The planner is
//! conservative by construction: it may return extra candidates, never fewer,
//! and the verification pass applies the real regex.

use super::index::TextIndex;
use super::trigram::literal_trigrams;
use regex_syntax::hir::{Class, Hir, HirKind};

/// A required-literal formula.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    /// No requirement derivable: every file is a candidate.
    Any,
    /// The file must contain this literal (case-folded, bytes).
    Lit(Vec<u8>),
    /// All sub-formulas must hold.
    And(Vec<Expr>),
    /// At least one sub-formula must hold.
    Or(Vec<Expr>),
}

/// Parse `pattern` into a requirement formula.
pub fn plan(pattern: &str, case_insensitive: bool) -> Result<Expr, String> {
    let hir = regex_syntax::ParserBuilder::new()
        .case_insensitive(case_insensitive)
        .build()
        .parse(pattern)
        .map_err(|error| error.to_string())?;
    Ok(simplify(build(&hir)))
}

/// Byte a small class stands for once ASCII case is folded, if it is
/// unambiguous. `(?i)k` also matches U+212A (Kelvin) and `(?i)s` U+017F; the
/// index folds ASCII only, so those two are accepted as the ASCII letter (a
/// file that spells a word only with them would be missed by the pre-filter).
fn class_byte(class: &Class) -> Option<u8> {
    let mut found: Option<u8> = None;
    let mut fold = |byte: u8| -> bool {
        let lowered = byte.to_ascii_lowercase();
        match found {
            None => {
                found = Some(lowered);
                true
            }
            Some(existing) => existing == lowered,
        }
    };
    match class {
        Class::Unicode(unicode) => {
            let mut seen = 0u32;
            for range in unicode.ranges() {
                let (start, end) = (u32::from(range.start()), u32::from(range.end()));
                seen += end - start + 1;
                if seen > 8 {
                    return None;
                }
                for value in start..=end {
                    match char::from_u32(value) {
                        Some(c) if c.is_ascii() => {
                            if !fold(c as u8) {
                                return None;
                            }
                        }
                        Some('\u{212A}') | Some('\u{17F}') => {}
                        _ => return None,
                    }
                }
            }
        }
        Class::Bytes(bytes) => {
            let mut seen = 0u32;
            for range in bytes.ranges() {
                seen += u32::from(range.end() - range.start()) + 1;
                if seen > 8 {
                    return None;
                }
                for value in range.start()..=range.end() {
                    if !value.is_ascii() || !fold(value) {
                        return None;
                    }
                }
            }
        }
    }
    found
}

fn literal_bytes(hir: &Hir) -> Option<Vec<u8>> {
    match hir.kind() {
        HirKind::Literal(literal) => Some(literal.0.to_vec()),
        HirKind::Class(class) => class_byte(class).map(|b| vec![b]),
        _ => None,
    }
}

fn build(hir: &Hir) -> Expr {
    match hir.kind() {
        HirKind::Empty | HirKind::Look(_) => Expr::Any,
        HirKind::Literal(_) | HirKind::Class(_) => literal_bytes(hir).map_or(Expr::Any, Expr::Lit),
        HirKind::Repetition(rep) if rep.min >= 1 => build(&rep.sub),
        HirKind::Repetition(_) => Expr::Any,
        HirKind::Capture(capture) => build(&capture.sub),
        HirKind::Alternation(branches) => {
            let parts: Vec<Expr> = branches.iter().map(build).collect();
            if parts
                .iter()
                .any(|p| matches!(simplify(p.clone()), Expr::Any))
            {
                Expr::Any
            } else {
                Expr::Or(parts)
            }
        }
        HirKind::Concat(items) => {
            let mut parts = Vec::new();
            let mut run: Vec<u8> = Vec::new();
            for item in items {
                // Adjacent literal characters (including case-folded classes)
                // form one run, so `(?i)hello` yields the trigrams of "hello".
                if let Some(bytes) = literal_bytes(item) {
                    run.extend_from_slice(&bytes);
                    continue;
                }
                if !run.is_empty() {
                    parts.push(Expr::Lit(std::mem::take(&mut run)));
                }
                parts.push(build(item));
            }
            if !run.is_empty() {
                parts.push(Expr::Lit(run));
            }
            Expr::And(parts)
        }
    }
}

fn simplify(expr: Expr) -> Expr {
    match expr {
        Expr::Lit(bytes) if bytes.len() < 3 => Expr::Any,
        Expr::And(parts) => {
            let parts: Vec<Expr> = parts
                .into_iter()
                .map(simplify)
                .filter(|p| !matches!(p, Expr::Any))
                .collect();
            match parts.len() {
                0 => Expr::Any,
                1 => parts.into_iter().next().unwrap_or(Expr::Any),
                _ => Expr::And(parts),
            }
        }
        Expr::Or(parts) => {
            let parts: Vec<Expr> = parts.into_iter().map(simplify).collect();
            if parts.iter().any(|p| matches!(p, Expr::Any)) {
                Expr::Any
            } else if parts.len() == 1 {
                parts.into_iter().next().unwrap_or(Expr::Any)
            } else {
                Expr::Or(parts)
            }
        }
        other => other,
    }
}

/// Most-selective trigrams used per literal; more lists cost more than they save.
const MAX_TRIGRAMS_PER_LITERAL: usize = 8;

fn intersect(a: &[u32], b: &[u32]) -> Vec<u32> {
    let (small, large) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let mut out = Vec::with_capacity(small.len());
    let mut from = 0;
    for &value in small {
        // Gallop: lists are sorted, so the search window only moves forward.
        match large[from..].binary_search(&value) {
            Ok(pos) => {
                out.push(value);
                from += pos + 1;
            }
            Err(pos) => from += pos,
        }
        if from >= large.len() {
            break;
        }
    }
    out
}

fn union(a: Vec<u32>, b: Vec<u32>) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len() + b.len());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => {
                out.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                out.push(b[j]);
                j += 1;
            }
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out.extend_from_slice(&a[i..]);
    out.extend_from_slice(&b[j..]);
    out
}

/// Candidate file ids for `expr`. `None` means "no restriction" (scan all).
pub fn candidates(index: &TextIndex, expr: &Expr) -> Option<Vec<u32>> {
    match expr {
        Expr::Any => None,
        Expr::Lit(bytes) => {
            let mut trigrams = literal_trigrams(bytes);
            if trigrams.is_empty() {
                return None;
            }
            // A trigram no file contains proves there is no match at all.
            let mut counted: Vec<(u32, u32)> = trigrams
                .drain(..)
                .map(|t| (index.doc_count(t), t))
                .collect();
            if counted.iter().any(|(count, _)| *count == 0) {
                return Some(Vec::new());
            }
            counted.sort_unstable();
            counted.truncate(MAX_TRIGRAMS_PER_LITERAL);
            let mut result: Option<Vec<u32>> = None;
            for (_, t) in counted {
                let list = index.postings(t);
                result = Some(match result {
                    None => list,
                    Some(current) => intersect(&current, &list),
                });
                if result.as_ref().is_some_and(Vec::is_empty) {
                    break;
                }
            }
            result
        }
        Expr::And(parts) => {
            let mut result: Option<Vec<u32>> = None;
            for part in parts {
                if let Some(list) = candidates(index, part) {
                    result = Some(match result {
                        None => list,
                        Some(current) => intersect(&current, &list),
                    });
                    if result.as_ref().is_some_and(Vec::is_empty) {
                        break;
                    }
                }
            }
            result
        }
        Expr::Or(parts) => {
            let mut result: Vec<u32> = Vec::new();
            for part in parts {
                result = union(result, candidates(index, part)?);
            }
            Some(result)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::index::{FileInput, write_index};
    use super::super::trigram::literal_trigrams as tris;
    use super::*;

    fn lit(s: &str) -> Expr {
        Expr::Lit(s.as_bytes().to_vec())
    }

    #[test]
    fn test_literal_pattern_requires_itself() {
        assert_eq!(plan("hello", false).unwrap(), lit("hello"));
        assert_eq!(
            plan("hi", false).unwrap(),
            Expr::Any,
            "too short for a trigram"
        );
    }

    #[test]
    fn test_case_insensitive_literal_becomes_one_folded_run() {
        assert_eq!(plan("Hello", true).unwrap(), lit("hello"));
    }

    #[test]
    fn test_concat_keeps_required_literals_and_drops_wildcards() {
        assert_eq!(
            plan(r"foo\d+bar", false).unwrap(),
            Expr::And(vec![lit("foo"), lit("bar")])
        );
        assert_eq!(plan(r"foo.*", false).unwrap(), lit("foo"));
        assert_eq!(plan(r".*", false).unwrap(), Expr::Any);
    }

    #[test]
    fn test_alternation_needs_every_branch_constrained() {
        assert_eq!(
            plan("alpha|beta", false).unwrap(),
            Expr::Or(vec![lit("alpha"), lit("beta")])
        );
        assert_eq!(plan("alpha|b", false).unwrap(), Expr::Any);
        assert_eq!(plan("(?:alpha)?beta", false).unwrap(), lit("beta"));
    }

    #[test]
    fn test_repetition_with_min_one_keeps_its_body() {
        assert_eq!(plan("(abc)+", false).unwrap(), lit("abc"));
        assert_eq!(plan("(abc)*x", false).unwrap(), Expr::Any);
    }

    #[test]
    fn test_invalid_regex_is_an_error() {
        assert!(plan("(unclosed", false).is_err());
    }

    fn sample_index() -> (tempfile::TempDir, TextIndex) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.bin");
        let texts = [
            "fn parse_config() {}",
            "fn parse_args() {}",
            "struct Config;",
            "nothing here",
        ];
        let files: Vec<FileInput> = texts
            .iter()
            .enumerate()
            .map(|(i, t)| FileInput {
                rel_path: format!("f{i}.rs"),
                size: t.len() as u64,
                mtime_ns: 0,
                flags: 0,
                trigrams: tris(t.as_bytes()),
                symbols: vec![],
            })
            .collect();
        write_index(&path, &files, false).unwrap();
        let index = TextIndex::open(&path).unwrap();
        (dir, index)
    }

    #[test]
    fn test_candidates_intersect_union_and_prove_absence() {
        let (_dir, index) = sample_index();
        let get = |p: &str, ci: bool| candidates(&index, &plan(p, ci).unwrap());
        assert_eq!(get("parse_", false), Some(vec![0, 1]));
        assert_eq!(get("config", true), Some(vec![0, 2]));
        assert_eq!(get("parse_config", false), Some(vec![0]));
        assert_eq!(get("parse_args|struct", false), Some(vec![1, 2]));
        assert_eq!(
            get("qqqzzz", false),
            Some(vec![]),
            "absent trigram => no candidates"
        );
        assert_eq!(get(".*", false), None);
    }

    #[test]
    fn test_prefilter_never_drops_a_real_match() {
        // For each pattern, every file the regex matches must be a candidate.
        let (_dir, index) = sample_index();
        let texts = [
            "fn parse_config() {}",
            "fn parse_args() {}",
            "struct Config;",
            "nothing here",
        ];
        for pattern in [
            "parse_[a-z]+",
            "(?i)CONFIG",
            "fn \\w+\\(\\)",
            "here$",
            "pars.*fig",
            "a|b",
        ] {
            let ci = pattern.starts_with("(?i)");
            let candidate = candidates(&index, &plan(pattern, ci).unwrap());
            let re = regex::Regex::new(pattern).unwrap();
            for (id, text) in texts.iter().enumerate() {
                if re.is_match(text) {
                    assert!(
                        candidate.as_ref().is_none_or(|c| c.contains(&(id as u32))),
                        "pattern {pattern} lost file {id}"
                    );
                }
            }
        }
    }
}
