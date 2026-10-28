//! The single revision/ref gate for every `leindex_git` operation.
//!
//! Two distinct failures, because they need different fixes from the caller:
//!
//! * [`RefError::Invalid`] — the string could not be a ref at all (syntax).
//!   Retrying is pointless; the argument is wrong.
//! * [`RefError::NotFound`] — the string is well-formed but does not resolve in
//!   this repository. Retrying the same call is pointless too, but the argument
//!   is fine: the ref simply does not exist here.
//!
//! Collapsing them into one message is what made a mistyped flag and a missing
//! branch look identical.
//!
//! ## Why `-` is allowed inside but never first
//!
//! Refs are passed as single argv entries (never through a shell), so the only
//! injection surface left is git's own option parser: an argument starting with
//! `-` is read as a flag. Branch names like `feature/foo` are ordinary, and
//! `cw32/30b-admission`-style names are legitimate, so `-` must stay legal in
//! the middle. Git's own rule is the same one enforced here: a name may not
//! begin with `-`.
//!
//! `:` is refused. `rev:path` reaches outside the repository (`HEAD:../../etc/passwd`)
//! and no revision syntax needs it — ranges use `..`, the reflog uses `@{0}`.
//!
//! ## Ranges
//!
//! `git diff` accepts `a..b` and `a...b`. Each side is validated as a ref, so a
//! range cannot smuggle anything a single ref could not.

use super::protocol::JsonRpcError;

/// Why a ref was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefError {
    /// Not usable as a ref at all — syntax. Carries the offending text.
    Invalid,
    /// Well-formed, but this repository has no such revision.
    NotFound,
}

/// Longest ref accepted. Git's own practical ceiling for a branch name is far
/// below this; the bound exists so a pathological string is refused cheaply.
const MAX_REF_LEN: usize = 200;

/// Characters that may appear in a ref this surface accepts.
///
/// Restricted to what revision syntax actually uses, plus `-` for names. No
/// whitespace, no `:`, no shell metacharacters that would confuse a reader into
/// thinking this is a command line.
fn is_ref_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '.' | '_' | '/' | '~' | '^' | '@' | '{' | '}' | '+' | '=' | ',' | '%' | '-'
        )
}

/// Whether `raw` is a syntactically usable single ref.
fn is_valid_single_ref(raw: &str) -> bool {
    !raw.is_empty()
        && raw.len() <= MAX_REF_LEN
        // Option injection: git reads a leading `-` as a flag.
        && !raw.starts_with('-')
        // No component may be `.` or `..`
        && !raw.contains("..")
        && !raw.ends_with('.')
        // `@{` is reflog syntax git resolves itself, and it is legitimate
        // (`HEAD@{0}`, `main@{yesterday}`). Reject only an UNCLOSED one — that
        // is malformed rather than a ref. This is safe because `:` is refused
        // below, so `@{` can never be part of a `rev:path` escape.
        && raw.matches("@{").count() <= raw.matches('}').count()
        && raw.chars().all(is_ref_char)
}

/// Validate a ref or a `a..b` / `a...b` range.
///
/// Returns the validated string (trimmed) or [`RefError::Invalid`].
pub fn validate_ref(raw: &str) -> Result<String, RefError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(RefError::Invalid);
    }
    // `...` must be tested before `..`, or the triple-dot range would be split
    // into sides that each fail validation.
    let separator = if trimmed.contains("...") { "..." } else { ".." };
    if trimmed.contains(separator) {
        let mut sides = trimmed.splitn(2, separator);
        let left = sides.next().unwrap_or_default();
        let right = sides.next().unwrap_or_default();
        // `a..` and `..b` are legal git (implicit HEAD), and both sides empty is
        // handled by the caller as the default spec.
        if (!left.is_empty() && !is_valid_single_ref(left))
            || (!right.is_empty() && !is_valid_single_ref(right))
        {
            return Err(RefError::Invalid);
        }
        return Ok(trimmed.to_string());
    }
    if is_valid_single_ref(trimmed) {
        Ok(trimmed.to_string())
    } else {
        Err(RefError::Invalid)
    }
}

/// The message a caller sees for a syntax refusal.
pub fn invalid_message(raw: &str) -> String {
    format!(
        "'{raw}' is not a usable revision: it is not valid ref syntax, so this is an \
         argument problem, not a missing ref"
    )
}

/// The message a caller sees for a well-formed ref that does not resolve.
pub fn not_found_message(raw: &str) -> String {
    format!(
        "revision '{raw}' was not found in this repository: its syntax is valid, but no \
         branch, tag, or commit with that name exists here"
    )
}

/// The shared hint attached to both ref errors.
pub const REF_HINT: &str = "Use a branch, tag or commit oid as written in `git branch` / `git log --oneline` \
     (a '-' may appear inside a name, but never first)";

/// Map a [`RefError`] to the JSON-RPC error the surface returns.
pub fn rpc_error(raw: &str, error: RefError) -> JsonRpcError {
    match error {
        RefError::Invalid => {
            JsonRpcError::invalid_params_with_suggestion(invalid_message(raw), REF_HINT)
        }
        RefError::NotFound => {
            JsonRpcError::invalid_params_with_suggestion(not_found_message(raw), REF_HINT)
        }
    }
}

/// Validate, then prove the ref resolves, returning the message on failure.
pub fn resolve_ref(root: &std::path::Path, raw: &str) -> Result<String, JsonRpcError> {
    let validated = validate_ref(raw).map_err(|error| rpc_error(raw, error))?;
    if resolves(root, &validated) {
        Ok(validated)
    } else {
        Err(rpc_error(raw, RefError::NotFound))
    }
}

/// Validate without touching the repository.
pub fn validate_ref_or_error(raw: &str) -> Result<String, JsonRpcError> {
    validate_ref(raw).map_err(|error| rpc_error(raw, error))
}

/// argv for the read-only "does this resolve?" probe.
///
/// Lives here so the guard cannot be forgotten at a call site:
/// `rev-parse` still parses a leading `-` as a flag, so validation must happen
/// before the argv exists.
pub fn probe_argv(raw: &str) -> Result<Vec<String>, RefError> {
    let target = validate_ref(raw)?;
    Ok(vec![
        "rev-parse".to_string(),
        "--verify".to_string(),
        "--quiet".to_string(),
        format!("{target}^{{commit}}"),
    ])
}

/// Whether `validated` (already syntax-checked) resolves as a commit.
///
/// `rev-parse --verify --quiet` prints nothing and exits non-zero for an
/// unknown revision — that is the not-found signal, and it is why this must be
/// distinguished from a spawn failure, which is a real error.
pub fn resolves(root: &std::path::Path, validated: &str) -> bool {
    let Ok(argv) = probe_argv(validated) else {
        return false;
    };
    match super::git_diff_handler::git_output_capped(root, &argv, 4096) {
        Ok((text, _)) => !text.trim().is_empty(),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_slash_and_dash_inside_a_ref_are_accepted() {
        // `cw32/30b-admission` is the shape this surface must not break: a
        // slash AND a dash inside a name.
        for good in [
            "cw32/30b-admission",
            "HEAD",
            "main",
            "origin/main",
            "feature/foo-bar",
            "v1.2.3",
            "HEAD~3",
            "a1b2c3d^",
            "HEAD@{0}",
        ] {
            assert_eq!(validate_ref(good), Ok(good.to_string()), "{good}");
        }
    }

    #[test]
    fn test_leading_dash_is_rejected_because_it_is_read_as_an_option() {
        for bad in ["-x", "-u", "--upload-pack=x", "-p"] {
            assert_eq!(validate_ref(bad), Err(RefError::Invalid), "{bad}");
        }
    }

    #[test]
    fn test_path_smuggling_and_whitespace_are_rejected() {
        for bad in ["HEAD:../../etc/passwd", "", "a b", "a:b"] {
            assert_eq!(validate_ref(bad), Err(RefError::Invalid), "{bad}");
        }
    }

    #[test]
    fn test_ranges_validate_each_side() {
        assert_eq!(
            validate_ref("cw32/30b-admission..HEAD"),
            Ok("cw32/30b-admission..HEAD".to_string())
        );
        assert_eq!(
            validate_ref("HEAD~1...HEAD"),
            Ok("HEAD~1...HEAD".to_string())
        );
        // A hostile side must fail the whole range.
        assert_eq!(validate_ref("-x..HEAD"), Err(RefError::Invalid));
        assert_eq!(validate_ref("HEAD..-x"), Err(RefError::Invalid));
        assert_eq!(
            validate_ref("HEAD:../../etc/passwd..HEAD"),
            Err(RefError::Invalid)
        );
    }

    #[test]
    fn test_invalid_and_not_found_carry_different_messages() {
        let invalid = rpc_error("-x", RefError::Invalid);
        let missing = rpc_error("nosuchref-x", RefError::NotFound);
        assert!(
            invalid.message.contains("not valid ref syntax"),
            "{}",
            invalid.message
        );
        assert!(
            missing.message.contains("was not found in this repository"),
            "{}",
            missing.message
        );
        assert_ne!(invalid.message, missing.message);
    }

    #[test]
    fn test_probe_argv_never_builds_argv_for_an_invalid_ref() {
        // An earlier version rebuilt argv from the caller's raw string at the
        // probe site and passed its test for the wrong reason.
        assert_eq!(probe_argv("-u"), Err(RefError::Invalid));
        assert_eq!(
            probe_argv("HEAD").unwrap(),
            ["rev-parse", "--verify", "--quiet", "HEAD^{commit}"]
        );
    }

    /// The three cases named in the directive, against a real repository.
    #[test]
    fn test_resolution_distinguishes_present_missing_and_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(root)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t")
                .status()
                .expect("git available");
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        std::fs::write(root.join("f.txt"), "a\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", "one"]);
        git(&["branch", "cw32/30b-admission"]);

        assert_eq!(
            resolve_ref(root, "cw32/30b-admission").unwrap(),
            "cw32/30b-admission"
        );

        let invalid = resolve_ref(root, "-x").unwrap_err();
        assert!(invalid.message_with_hint().contains("not valid ref syntax"));
        assert!(resolve_ref(Path::new("/nonexistent"), "-x").is_err());

        let missing = resolve_ref(root, "nosuchref-x").unwrap_err();
        assert!(
            missing
                .message_with_hint()
                .contains("was not found in this repository"),
            "{}",
            missing.message_with_hint()
        );
        // Well-formed but absent is NOT an invalid-ref error.
        assert!(!missing.message_with_hint().contains("not valid ref syntax"));
    }
}
