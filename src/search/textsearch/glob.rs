//! Minimal path globbing for include/exclude filters.
//!
//! Supports `*` (within one path segment), `**` (across segments), `?`, a
//! trailing `/` to mean "this directory", and — like gitignore — treats a
//! pattern without a `/` as a match on the file name at any depth.

/// One compiled pattern.
#[derive(Debug, Clone)]
pub struct Glob {
    pattern: String,
    basename_only: bool,
    directory: bool,
}

impl Glob {
    /// Compile `pattern` (forward slashes; a leading `./` or `/` is ignored).
    pub fn new(pattern: &str) -> Self {
        let trimmed = pattern
            .trim()
            .trim_start_matches("./")
            .trim_start_matches('/');
        let directory = trimmed.ends_with('/');
        let body = trimmed.trim_end_matches('/').to_string();
        Self {
            basename_only: !body.contains('/'),
            pattern: body,
            directory,
        }
    }

    /// Does the project-relative `path` (forward slashes) match?
    pub fn matches(&self, path: &str) -> bool {
        if self.pattern.is_empty() {
            return false;
        }
        if self.directory {
            return if self.basename_only {
                path.split('/')
                    .rev()
                    .skip(1)
                    .any(|segment| wild(self.pattern.as_bytes(), segment.as_bytes()))
            } else {
                // Any leading run of segments matches the directory pattern.
                let mut end = 0;
                for segment in path.split('/') {
                    end += segment.len();
                    if end >= path.len() {
                        break;
                    }
                    if wild(self.pattern.as_bytes(), &path.as_bytes()[..end]) {
                        return true;
                    }
                    end += 1;
                }
                false
            };
        }
        if self.basename_only {
            let name = path.rsplit('/').next().unwrap_or(path);
            wild(self.pattern.as_bytes(), name.as_bytes())
        } else {
            wild(self.pattern.as_bytes(), path.as_bytes())
        }
    }
}

fn wild(p: &[u8], t: &[u8]) -> bool {
    match p.first() {
        None => t.is_empty(),
        Some(b'*') if p.get(1) == Some(&b'*') => {
            let rest = &p[2..];
            let rest = rest.strip_prefix(b"/").unwrap_or(rest);
            (0..=t.len()).any(|i| wild(rest, &t[i..]))
        }
        Some(b'*') => {
            let rest = &p[1..];
            for i in 0..=t.len() {
                if wild(rest, &t[i..]) {
                    return true;
                }
                if t.get(i) == Some(&b'/') || i == t.len() {
                    break;
                }
            }
            false
        }
        Some(b'?') => t.first().is_some_and(|c| *c != b'/') && wild(&p[1..], &t[1..]),
        Some(c) => t.first() == Some(c) && wild(&p[1..], &t[1..]),
    }
}

/// Include/exclude/scope filter over project-relative paths.
#[derive(Debug, Clone, Default)]
pub struct FileFilter {
    include: Vec<Glob>,
    exclude: Vec<Glob>,
    /// Project-relative directory or file the search is confined to.
    scope: Option<String>,
}

impl FileFilter {
    /// Build a filter. Empty `include` admits everything.
    pub fn new(include: &[String], exclude: &[String], scope: Option<&str>) -> Self {
        Self {
            include: include.iter().map(|p| Glob::new(p)).collect(),
            exclude: exclude.iter().map(|p| Glob::new(p)).collect(),
            scope: scope
                .map(|s| s.trim_start_matches("./").trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty()),
        }
    }

    /// Is `rel` (forward slashes) admitted?
    pub fn allows(&self, rel: &str) -> bool {
        if let Some(scope) = &self.scope {
            let inside = rel == scope
                || rel
                    .strip_prefix(scope.as_str())
                    .is_some_and(|rest| rest.starts_with('/'));
            if !inside {
                return false;
            }
        }
        if !self.include.is_empty() && !self.include.iter().any(|g| g.matches(rel)) {
            return false;
        }
        !self.exclude.iter().any(|g| g.matches(rel))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basename_patterns_match_at_any_depth() {
        let g = Glob::new("*.rs");
        assert!(g.matches("main.rs"));
        assert!(g.matches("src/deep/main.rs"));
        assert!(!g.matches("src/main.rs.bak"));
    }

    #[test]
    fn test_path_patterns_and_double_star() {
        assert!(Glob::new("src/**/*.rs").matches("src/a/b/c.rs"));
        assert!(Glob::new("src/**/*.rs").matches("src/c.rs"));
        assert!(!Glob::new("src/*.rs").matches("src/a/c.rs"));
        assert!(Glob::new("src/*.rs").matches("src/c.rs"));
        assert!(Glob::new("**/tests/**").matches("a/tests/x/y.rs"));
    }

    #[test]
    fn test_directory_patterns() {
        assert!(Glob::new("tests/").matches("tests/a.rs"));
        assert!(Glob::new("tests/").matches("crate/tests/a.rs"));
        assert!(!Glob::new("tests/").matches("tests"));
        assert!(!Glob::new("tests/").matches("mytests/a.rs"));
        assert!(Glob::new("docs/api/").matches("docs/api/x.md"));
        assert!(!Glob::new("docs/api/").matches("docs/apix/x.md"));
    }

    #[test]
    fn test_filter_combines_scope_include_exclude() {
        let f = FileFilter::new(
            &["*.rs".to_string()],
            &["*_test.rs".to_string()],
            Some("src"),
        );
        assert!(f.allows("src/a.rs"));
        assert!(!f.allows("src/a_test.rs"));
        assert!(!f.allows("tests/a.rs"));
        assert!(!f.allows("srcx/a.rs"));
        assert!(!f.allows("src/a.py"));
        assert!(FileFilter::default().allows("anything/at/all"));
    }
}
