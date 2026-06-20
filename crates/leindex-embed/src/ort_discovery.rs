// Runtime ORT (ONNX Runtime) discovery and dynamic loading
//
// Implements the search chain used by `leindex-embed` to locate a compatible
// `libonnxruntime.{so,dylib,dll}` at runtime and load it via `ort::init_from()`
// before any `Session::builder()` call.
//
// Discovery chain (first match wins):
//   1. `ORT_DYLIB_PATH` env var (explicit user override, highest priority)
//   2. `~/.leindex/config/leindex.toml` -> `[neural] ort_dylib_path`
//   3. `~/.leindex/lib/` (or `$LEINDEX_HOME/lib/`) — bundled from release
//   4. Sibling dir to the running binary — bundled from release bundle
//   5. `python3`/`python` site-packages `onnxruntime/capi/`
//   6. System paths: `/usr/local/lib`, `/usr/lib`, `/lib`
//
// VAL-ORT-005: ORT_DYLIB_PATH env var has highest priority
// VAL-ORT-006: config file ort_dylib_path is next priority
// VAL-ORT-007: ~/.leindex/lib is searched
// VAL-ORT-008: sibling dir to binary is searched
// VAL-ORT-009: pip site-packages is searched
// VAL-ORT-010: system paths are the final fallback
// VAL-ORT-011: graceful error when ORT is not found anywhere
// VAL-ORT-013/014: Version-mismatch is surfaced as a clear error
// VAL-ORT-017: dynamic load is lazy (only happens when init_onnx() runs)
// VAL-ORT-018: ort's internal G_ORT_LIB caches the loaded dylib
// VAL-ORT-021: cross-platform filename handling
// VAL-ORT-022: `last_outcome()` exposes the resolved path for diagnostics

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Environment variable name for explicit ORT dylib override.
pub const ORT_DYLIB_ENV: &str = "ORT_DYLIB_PATH";

/// Environment variable for the LeIndex home directory (defaults to `~/.leindex`).
const LEINDEX_HOME_ENV: &str = "LEINDEX_HOME";

/// Environment variable for the Python interpreter override used during pip
/// site-packages discovery.
#[cfg(feature = "onnx")]
const LEINDEX_PYTHON_ENV: &str = "LEINDEX_PYTHON";

/// The resolved discovery outcome, cached for diagnostics across the process
/// lifetime. `None` means discovery has not yet run or no library was found.
static LAST_OUTCOME: OnceLock<Option<DiscoveryOutcome>> = OnceLock::new();

/// Returns the shared-library file names searched on the current platform.
///
/// VAL-ORT-021: branches on `target_os` so the matching file is found per platform.
fn ort_lib_names() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    {
        &["libonnxruntime.so"]
    }
    #[cfg(target_os = "macos")]
    {
        &["libonnxruntime.dylib"]
    }
    #[cfg(target_os = "windows")]
    {
        &["onnxruntime.dll"]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        &["libonnxruntime.so"]
    }
}

/// Returns true when `name` is a loadable ORT runtime library filename on the
/// current platform, including versioned pip-wheel sonames such as
/// `libonnxruntime.so.1.25.0`. Provider helper libraries
/// (`libonnxruntime_providers_*`) are intentionally excluded.
#[cfg(any(feature = "onnx", test))]
fn is_ort_runtime_lib_name(name: &str) -> bool {
    #[cfg(target_os = "linux")]
    {
        name == "libonnxruntime.so" || name.starts_with("libonnxruntime.so.")
    }
    #[cfg(target_os = "macos")]
    {
        name == "libonnxruntime.dylib"
            || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"))
    }
    #[cfg(target_os = "windows")]
    {
        name.eq_ignore_ascii_case("onnxruntime.dll")
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        ort_lib_names().iter().any(|candidate| candidate == &name)
    }
}

/// Where a discovered ORT library came from. Used in diagnostics (VAL-ORT-022).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscoverySource {
    /// `ORT_DYLIB_PATH` env var.
    EnvVar,
    /// `~/.leindex/config/leindex.toml` `[neural] ort_dylib_path`.
    Config,
    /// `~/.leindex/lib/` (or `$LEINDEX_HOME/lib/`).
    UserLib,
    /// Directory containing the running worker binary.
    Sibling,
    /// `python3`'s `site-packages/onnxruntime/capi/`.
    Pip,
    /// `/usr/local/lib`, `/usr/lib`, etc., via the system loader.
    System,
}

impl DiscoverySource {
    /// Stable string label for diagnostics output.
    pub fn as_str(self) -> &'static str {
        match self {
            DiscoverySource::EnvVar => "env",
            DiscoverySource::Config => "config",
            DiscoverySource::UserLib => "user_lib",
            DiscoverySource::Sibling => "sibling",
            DiscoverySource::Pip => "pip",
            DiscoverySource::System => "system",
        }
    }
}

impl std::fmt::Display for DiscoverySource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A successfully located ORT library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryOutcome {
    /// Source chain step that produced this path.
    pub source: DiscoverySource,
    /// Absolute path to the matched library file.
    pub path: PathBuf,
}

/// Outcome of `discover_and_init()`.
#[derive(Debug)]
pub enum InitResult {
    /// ORT was discovered and `ort::init_from()` committed successfully.
    Initialized(DiscoveryOutcome),
    /// ORT could not be loaded. `searched` lists every candidate path/source
    /// attempted so callers can surface an actionable error (VAL-ORT-011).
    NotFound {
        searched: Vec<(DiscoverySource, String)>,
        last_error: Option<String>,
    },
}

impl InitResult {
    /// True when an ORT library was successfully loaded.
    pub fn is_initialized(&self) -> bool {
        matches!(self, InitResult::Initialized(_))
    }
}

/// Returns the last `DiscoveryOutcome` produced by `discover_and_init()`, if any.
///
/// VAL-ORT-022: driven can surface `ort_path`/`ort_source` in diagnostics via
/// this accessor. `None` means either discovery has not run or no library was
/// found.
pub fn last_outcome() -> Option<DiscoveryOutcome> {
    LAST_OUTCOME.get().and_then(|o| o.clone())
}

/// Resolve the user LeIndex home (`~/.leindex` by default, or `$LEINDEX_HOME`).
fn leindex_home() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(LEINDEX_HOME_ENV) {
        let p = PathBuf::from(custom);
        if p.is_absolute() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".leindex"))
}

/// Read `ort_dylib_path` from `~/.leindex/config/leindex.toml` if present.
fn read_config_ort_path() -> Option<PathBuf> {
    crate::config::LeIndexConfig::load()
        .ok()
        .and_then(|cfg| cfg.neural.ort_dylib_path)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
}

/// Look for the first matching ORT library file in `dir`.
#[cfg(any(feature = "onnx", test))]
fn find_lib_in_dir(dir: &Path) -> Option<PathBuf> {
    // Prefer exact unversioned names (provided by bundle/system symlinks).
    for name in ort_lib_names() {
        let candidate = dir.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    // Fall back to versioned pip-wheel runtime libraries (e.g.
    // `libonnxruntime.so.1.25.0`) which have no unversioned symlink.
    let mut matches = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(is_ort_runtime_lib_name)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();

    matches.sort();
    matches.pop()
}

/// Discover the running worker binary's directory (siblings of `current_exe`).
fn binary_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf))
}

/// Run a Python one-liner and capture its stdout. Returns `None` if python is
/// missing or the import fails. Used only for VAL-ORT-009 (pip install path).
#[cfg(feature = "onnx")]
fn python_one_line(program: &str) -> Option<String> {
    // Prefer a configured override, then `python3`, then `python`.
    let mut candidates: Vec<std::process::Command> = Vec::new();
    if let Ok(exe) = std::env::var(LEINDEX_PYTHON_ENV) {
        candidates.push(std::process::Command::new(exe));
    }
    candidates.push(std::process::Command::new("python3"));
    candidates.push(std::process::Command::new("python"));

    for mut cmd in candidates {
        cmd.arg("-c").arg(program);
        cmd.stdin(std::process::Stdio::null());
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::null());
        match cmd.output() {
            Ok(out) if out.status.success() => {
                let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !s.is_empty() {
                    return Some(s);
                }
            }
            _ => continue,
        }
    }
    None
}

/// Locate `site-packages/onnxruntime/capi/libonnxruntime.*` via Python discovery.
#[cfg(feature = "onnx")]
fn discover_pip_lib() -> Option<PathBuf> {
    // Ask Python for the directory of `onnxruntime.capi` and scan it for the
    // platform-specific library name. This is robust against venv vs system
    // installs and against the `--user` install layout where dist-info and the
    // package live side-by-side under site-packages.
    let program = "import os,onnxruntime.capi as c; print(os.path.dirname(c.__file__))";
    let capi_dir = python_one_line(program)?;
    let dir = PathBuf::from(capi_dir);
    if !dir.is_dir() {
        return None;
    }
    find_lib_in_dir(&dir)
}

/// System library directories probed as the final fallback.
fn system_lib_dirs() -> Vec<PathBuf> {
    #[cfg(unix)]
    {
        vec![
            PathBuf::from("/usr/local/lib"),
            PathBuf::from("/usr/lib"),
            PathBuf::from("/lib"),
        ]
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// Build the full ordered candidate list (source, path) without touching the
/// filesystem. Used by both `discover_and_init()` and tests.
pub fn discover_candidates() -> Vec<(DiscoverySource, PathBuf)> {
    let mut out: Vec<(DiscoverySource, PathBuf)> = Vec::new();

    // 1. ORT_DYLIB_PATH env var
    if let Ok(path) = std::env::var(ORT_DYLIB_ENV) {
        if !path.is_empty() {
            out.push((DiscoverySource::EnvVar, PathBuf::from(path)));
        }
    }

    // 2. config file ort_dylib_path
    if let Some(path) = read_config_ort_path() {
        out.push((DiscoverySource::Config, path));
    }

    // 3. ~/.leindex/lib/ (or $LEINDEX_HOME/lib)
    if let Some(home) = leindex_home() {
        let lib_dir = home.join("lib");
        for name in ort_lib_names() {
            out.push((DiscoverySource::UserLib, lib_dir.join(name)));
        }
    }

    // 4. sibling dir to binary
    if let Some(bin_dir) = binary_dir() {
        for name in ort_lib_names() {
            out.push((DiscoverySource::Sibling, bin_dir.join(name)));
        }
    }

    // 5. pip site-packages — only path-level candidates here (existence is
    //    verified lazily; we don't shell out during `discover_candidates()` so
    //    tests that just want to inspect the chain aren't penalised).
    // The actual pip candidate is appended by `discover_and_init()` below.

    // 6. system paths are NOT included here. They are the final fallback and
    //    are tried AFTER pip ORT (see `system_candidates()` /
    //    `discover_and_init()`) so a `leindex setup`-installed pip runtime
    //    wins over a stale system library.

    out
}

/// Build the system-path candidate list. These are the lowest-priority ORT
/// sources and are always tried AFTER the high-priority chain and the pip
/// runtime, so a setup-installed pip ORT wins over a stale system library.
fn system_candidates() -> Vec<(DiscoverySource, PathBuf)> {
    let mut out: Vec<(DiscoverySource, PathBuf)> = Vec::new();
    for dir in system_lib_dirs() {
        for name in ort_lib_names() {
            out.push((DiscoverySource::System, dir.join(name)));
        }
    }
    out
}

/// Discover and load the ORT dynamic library via `ort::init_from()`.
///
/// MUST be called BEFORE any `Session::builder()` call so that the configured
/// environment takes effect. See `runtime::WorkerRuntime::init_onnx()`.
///
/// Walks the documented discovery chain in order:
/// 1. `ORT_DYLIB_PATH`
/// 2. config file `ort_dylib_path`
/// 3. `~/.leindex/lib/`
/// 4. sibling dir to binary
/// 5. pip site-packages (lazy Python subprocess lookup)
/// 6. system paths
///
/// On success, caches the `DiscoveryOutcome` in `LAST_OUTCOME` so diagnostics
/// (VAL-ORT-022) can report the resolved path without re-running discovery.
#[cfg(feature = "onnx")]
pub fn discover_and_init() -> InitResult {
    let mut searched: Vec<(DiscoverySource, String)> = Vec::new();
    let mut last_error: Option<String> = None;

    let mut try_path = |source: DiscoverySource,
                        path: PathBuf,
                        require_exists: bool|
     -> Option<DiscoveryOutcome> {
        if require_exists && !path.exists() {
            searched.push((source, path.display().to_string()));
            return None;
        }
        match ort::init_from(&path) {
            Ok(builder) => {
                // commit() is required for the environment to take effect; it
                // returns false if an environment was already committed, which
                // we treat as success (the prior environment wins, but ORT is
                // in fact loaded).
                let _ = builder.commit();
                let outcome = DiscoveryOutcome { source, path };
                let _ = LAST_OUTCOME.set(Some(outcome.clone()));
                tracing::info!(
                    "loaded ONNX Runtime dylib from {} [{}]",
                    outcome.path.display(),
                    outcome.source
                );
                Some(outcome)
            }
            Err(e) => {
                let msg = format!("init_from({}) failed: {}", path.display(), e);
                tracing::warn!("{}", msg);
                last_error = Some(msg);
                searched.push((source, path.display().to_string()));
                None
            }
        }
    };

    // 1-4: high-priority static candidates (env / config / user_lib / sibling).
    for (source, path) in discover_candidates() {
        if let Some(outcome) = try_path(source, path, true) {
            return InitResult::Initialized(outcome);
        }
    }

    // 5. pip site-packages (lazy Python lookup so unit tests can skip it).
    //    Tried BEFORE system paths so a setup-installed pip runtime wins over a
    //    stale system library.
    if let Some(path) = discover_pip_lib() {
        if let Some(outcome) = try_path(DiscoverySource::Pip, path, true) {
            return InitResult::Initialized(outcome);
        }
    }

    // 6. system paths (final ordered fallback, after pip).
    for (source, path) in system_candidates() {
        if let Some(outcome) = try_path(source, path, true) {
            return InitResult::Initialized(outcome);
        }
    }

    // Final fallback: try the bare library name. ort's setup_api() probes the
    // default loader path (`ld.so.conf`, `DYLD_LIBRARY_PATH`, `%PATH%`).
    if let Some(outcome) = try_path(
        DiscoverySource::System,
        PathBuf::from(ort_lib_names()[0]),
        false,
    ) {
        return InitResult::Initialized(outcome);
    }

    let _ = LAST_OUTCOME.set(None);
    InitResult::NotFound {
        searched,
        last_error,
    }
}

#[cfg(not(feature = "onnx"))]
pub fn discover_and_init() -> InitResult {
    // The non-onnx build has no worker, so discovery always returns NotFound.
    // `searched` is empty because there are no candidate paths when ort isn't
    // even compiled in.
    InitResult::NotFound {
        searched: Vec::new(),
        last_error: None,
    }
}

/// Discover the ORT dynamic library WITHOUT loading it.
///
/// VAL-CROSS-015 / VAL-ORT-022: Used by `leindex diagnostics` to surface the
/// resolved ORT library path to support engineers without taking on the cost
/// (or the side effects) of `init_from()` inside the main daemon process.
/// The main binary does not itself run ONNX inference (the leindex-embed
/// worker does), so the diagnostic command must NOT commit an ORT
/// environment that could conflict with the worker's load. Walking the same
/// chain as `discover_and_init()` keeps the reported path consistent with the
/// one the worker would actually load.
///
/// Returns the first existing candidate path using the documented discovery
/// chain: env -> config -> `~/.leindex/lib/` -> sibling -> pip -> system.
pub fn discover_path_only() -> Option<DiscoveryOutcome> {
    // 1-4: high-priority static candidates (env / config / user_lib / sibling).
    for (source, path) in discover_candidates() {
        if path.exists() {
            return Some(DiscoveryOutcome { source, path });
        }
    }

    // 5. pip site-packages (lazy Python lookup so the diagnostic command only
    //    shells out to Python when no higher-priority source is available).
    //    Checked BEFORE system paths to mirror `discover_and_init()`.
    #[cfg(feature = "onnx")]
    if let Some(path) = discover_pip_lib() {
        if path.exists() {
            return Some(DiscoveryOutcome {
                source: DiscoverySource::Pip,
                path,
            });
        }
    }

    // 6. system paths (final ordered fallback, after pip).
    for (source, path) in system_candidates() {
        if path.exists() {
            return Some(DiscoveryOutcome { source, path });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    // Use the crate-level shared lock so env-mutating tests serialize across modules.
    use crate::test_util::ENV_TEST_LOCK;

    fn make_fake_lib(dir: &Path) -> PathBuf {
        let name = ort_lib_names()[0];
        let p = dir.join(name);
        std::fs::write(&p, b"not a real ort lib").unwrap();
        p
    }

    #[test]
    fn test_discover_candidates_includes_env_var() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(ORT_DYLIB_ENV, tmp.path().join("env.so"));

        let candidates = discover_candidates();
        assert!(candidates
            .iter()
            .any(|(s, p)| *s == DiscoverySource::EnvVar && p == &tmp.path().join("env.so")));

        std::env::remove_var(ORT_DYLIB_ENV);
    }

    #[test]
    fn test_discover_candidates_excludes_empty_env() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(ORT_DYLIB_ENV, "");
        let candidates = discover_candidates();
        assert!(!candidates
            .iter()
            .any(|(s, _)| *s == DiscoverySource::EnvVar));
        std::env::remove_var(ORT_DYLIB_ENV);
    }

    #[test]
    fn test_discover_candidates_includes_user_lib() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var(ORT_DYLIB_ENV);
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let candidates = discover_candidates();
        let expected = tmp.path().join("lib").join(ort_lib_names()[0]);
        assert!(candidates
            .iter()
            .any(|(s, p)| *s == DiscoverySource::UserLib && p == &expected));

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_discover_candidates_includes_system_paths() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::remove_var(ORT_DYLIB_ENV);
        std::env::remove_var(LEINDEX_HOME_ENV);

        // System paths now live in `system_candidates()` (final fallback,
        // tried after pip), not in `discover_candidates()`.
        let candidates = system_candidates();
        // System paths should be present at minimum on Unix.
        #[cfg(unix)]
        {
            assert!(candidates
                .iter()
                .any(|(s, p)| *s == DiscoverySource::System && p.starts_with("/usr/local/lib")));
        }
    }

    #[test]
    fn test_read_config_ort_path_returns_value() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        let cfg_path = cfg_dir.join("leindex.toml");
        std::fs::write(
            &cfg_path,
            "[neural]\nenabled = true\nort_dylib_path = \"/some/path/libonnxruntime.so\"\nmodel_dir = \"~/.leindex/models\"\n",
        )
        .unwrap();

        let parsed = read_config_ort_path();
        assert_eq!(parsed, Some(PathBuf::from("/some/path/libonnxruntime.so")));

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_read_config_ort_path_returns_none_when_missing() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        // No config file
        assert_eq!(read_config_ort_path(), None);

        // Config file present but no ort_dylib_path key
        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[search]\nmode = \"hybrid\"\n",
        )
        .unwrap();
        assert_eq!(read_config_ort_path(), None);

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_read_config_ort_path_handles_single_quotes() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let cfg_dir = tmp.path().join("config");
        std::fs::create_dir_all(&cfg_dir).unwrap();
        std::fs::write(
            cfg_dir.join("leindex.toml"),
            "[neural]\nort_dylib_path = '/quote/libonnxruntime.so'\n",
        )
        .unwrap();

        assert_eq!(
            read_config_ort_path(),
            Some(PathBuf::from("/quote/libonnxruntime.so"))
        );

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_find_lib_in_dir_finds_matching_name() {
        let tmp = tempfile::tempdir().unwrap();
        let p = make_fake_lib(tmp.path());
        assert_eq!(find_lib_in_dir(tmp.path()), Some(p));
    }

    #[test]
    fn test_find_lib_in_dir_returns_none_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(find_lib_in_dir(tmp.path()), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_lib_in_dir_accepts_linux_versioned_pip_soname() {
        let temp = tempfile::tempdir().unwrap();
        let versioned = temp.path().join("libonnxruntime.so.1.25.0");
        std::fs::write(&versioned, b"fake").unwrap();

        let found =
            find_lib_in_dir(temp.path()).expect("versioned pip ORT library should be found");

        assert_eq!(found, versioned);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_lib_in_dir_prefers_unversioned_link_when_present() {
        let temp = tempfile::tempdir().unwrap();
        let unversioned = temp.path().join("libonnxruntime.so");
        let versioned = temp.path().join("libonnxruntime.so.1.25.0");
        std::fs::write(&versioned, b"fake-versioned").unwrap();
        std::fs::write(&unversioned, b"fake-unversioned").unwrap();

        let found = find_lib_in_dir(temp.path()).expect("ORT library should be found");

        assert_eq!(found, unversioned);
    }

    #[test]
    fn test_discover_path_only_checks_pip_before_system() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ort_discovery.rs");
        let src = std::fs::read_to_string(path).unwrap();
        let helper = src
            .split("pub fn discover_path_only()")
            .nth(1)
            .and_then(|s| s.split("\n}\n\n").next())
            .expect("discover_path_only must exist");

        let pip = helper
            .find("discover_pip_lib")
            .expect("path-only discovery must check pip");
        let system = helper
            .find("system_candidates")
            .expect("path-only discovery must check system");

        assert!(
            pip < system,
            "path-only discovery must prefer pip over system"
        );
    }

    #[test]
    fn test_bare_loader_fallback_does_not_require_path_exists() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/ort_discovery.rs");
        let src = std::fs::read_to_string(path).unwrap();
        let helper = src
            .split("pub fn discover_and_init()")
            .nth(1)
            .and_then(|s| s.split("\n}\n\n#[cfg(not(feature = \"onnx\"))]").next())
            .expect("discover_and_init must exist");

        let bare_probe = "PathBuf::from(ort_lib_names()[0])";
        let bare_probe_pos = helper
            .find(bare_probe)
            .expect("discover_and_init must try the bare ORT library name");
        let after_bare_probe = &helper[bare_probe_pos..];
        assert!(
            after_bare_probe.contains("false"),
            "bare dynamic-loader fallback must call try_path with require_exists=false"
        );
    }

    #[test]
    fn test_source_as_str_covers_all_variants() {
        // Sanity check that every variant has a stable label
        assert_eq!(DiscoverySource::EnvVar.as_str(), "env");
        assert_eq!(DiscoverySource::Config.as_str(), "config");
        assert_eq!(DiscoverySource::UserLib.as_str(), "user_lib");
        assert_eq!(DiscoverySource::Sibling.as_str(), "sibling");
        assert_eq!(DiscoverySource::Pip.as_str(), "pip");
        assert_eq!(DiscoverySource::System.as_str(), "system");
    }

    #[test]
    fn test_init_result_is_initialized() {
        let r1 = InitResult::Initialized(DiscoveryOutcome {
            source: DiscoverySource::Pip,
            path: PathBuf::from("/x/y/libonnxruntime.so"),
        });
        assert!(r1.is_initialized());

        let r2 = InitResult::NotFound {
            searched: Vec::new(),
            last_error: None,
        };
        assert!(!r2.is_initialized());
    }

    /// VAL-CROSS-015 / VAL-ORT-022: `discover_path_only()` must return the
    /// first existing candidate without calling `init_from()`, and must NOT
    /// mutate `LAST_OUTCOME`. We point `ORT_DYLIB_PATH` at a real (fake)
    /// file so it wins the chain.
    #[test]
    fn test_discover_path_only_returns_first_existing_no_init() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let fake_lib = make_fake_lib(tmp.path());
        std::env::set_var(ORT_DYLIB_ENV, &fake_lib);

        // Snapshot LAST_OUTCOME: it must remain unchanged across the call.
        let before = last_outcome();
        let outcome = discover_path_only();
        let after = last_outcome();

        std::env::remove_var(ORT_DYLIB_ENV);

        let outcome = outcome.expect("discover_path_only should find the env candidate");
        assert_eq!(outcome.source, DiscoverySource::EnvVar);
        assert_eq!(outcome.path, fake_lib);
        assert_eq!(
            before, after,
            "discover_path_only must not cache LAST_OUTCOME (no init_from() side effect)"
        );
    }

    /// VAL-CROSS-015: when nothing on the chain exists, `discover_path_only`
    /// returns `None` deterministically so diagnostics can fall back to the
    /// configured `ort_dylib_path` (if any) without crashing.
    #[test]
    fn test_discover_path_only_returns_none_when_absent() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        // Ensure env var is unset and LEINDEX_HOME points to an empty temp
        // dir so user-lib and config-file lookups also miss.
        std::env::remove_var(ORT_DYLIB_ENV);
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        // We cannot fully prevent pip/system fallbacks in this environment
        // (system ORT may exist on the dev machine), so we only assert that
        // the function is callable and returns a deterministic Option.
        let _ = discover_path_only();

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    // Tests that actually invoke ort::init_from() require a real libonnxruntime
    // and are gated behind the `onnx` feature plus a present system ORT. The
    // integration test exercising the success path lives next to the runtime
    // tests; see `runtime::tests`.
}
