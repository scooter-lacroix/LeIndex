//! Process-wide cached config with reload-on-change.
//!
//! `leindex.toml` must reach long-lived processes (the MCP daemon lives for
//! `[mcp] idle_timeout_secs`), so the cached config is re-stat'd whenever its
//! `(mtime, size, inode)` fingerprint changes instead of being frozen at startup.
//! Before this existed, an edit — switching `remote_service`, changing
//! `remote_model`, retuning `[services.gemma]` args — silently kept serving the
//! old route for the life of the process.
//!
//! This lives in its own module because the machinery is self-contained: four
//! statics, a throttled probe, and the install path. Keeping it beside the TOML
//! schema in `config.rs` mixed "what the config contains" with "how the process
//! tracks the file", and the schema half is the part readers come for.
//!
//! # Ownership
//!
//! One owned snapshot behind an `RwLock`. Readers clone the `Arc` under a read
//! lock (uncontended, nanoseconds) and use it for the duration of their call; a
//! reload swaps in a new snapshot and the previous one is freed once its last
//! reader drops it.
//!
//! This replaces a raw `AtomicPtr` + `Box::leak`, which handed out `&'static`
//! references and therefore could NEVER free a replaced config — every observed
//! `leindex.toml` edit leaked a full [`LeIndexConfig`] for the life of the
//! process. That is unbounded in exactly the workload the reload exists for (an
//! editor saving the file repeatedly), so ownership is now expressed in the type
//! rather than by leaking.
//!
//! The observable contract is unchanged: callers that need a snapshot across
//! statements hold the `Arc`, so it cannot be freed mid-use.

use super::{
    CONFIG_REFRESH_THROTTLE, LeIndexConfig, config_file_path, default_legacy_indexing_config,
    default_legacy_neural_config,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::{Duration, Instant};

/// Installed config snapshot. `None` until the first successful load.
static CACHED_CONFIG: RwLock<Option<Arc<LeIndexConfig>>> = RwLock::new(None);

/// When the config file was last stat'd, as monotonic time since process start.
///
/// `None` means "never probed". An `Option<Duration>` states that directly — the
/// previous `u64` sentinel (`0`, then `u64::MAX`) had to encode "never" inside
/// the value range, and the `0` version silently meant BOTH "never probed" and
/// "probed at process start", which suppressed the very first probe in any
/// process younger than the throttle.
static LAST_PROBE: Mutex<Option<Duration>> = Mutex::new(None);

/// File fingerprint the installed config was parsed from: `(mtime, size, inode)`
/// hashed into one u64. A mismatch means the file changed.
static CACHED_CONFIG_STAMP: AtomicU64 = AtomicU64::new(0);

/// Serializes installs so two threads observing the same change cannot both
/// parse and install a config (one would win the swap; the other would be wasted
/// work, and worse, could install an older parse over a newer one).
static CONFIG_REFRESH_LOCK: Mutex<()> = Mutex::new(());

/// Monotonic clock for probe throttling. `Instant` cannot live in a static, so
/// measure elapsed time from a lazily initialised process-start instant.
fn probe_clock() -> Duration {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed()
}

/// Fingerprint the config file's identity. `0` means "cannot stat" (missing
/// file, unreadable directory) — treated as a stable stamp so a missing config
/// does not trigger a re-parse on every probe.
fn config_file_stamp() -> u64 {
    let Some(path) = config_file_path() else {
        return 0;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return 0;
    };
    // Nanosecond mtime: a same-second edit that preserves size would otherwise be
    // invisible to a second-granularity stamp.
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(u64::MAX as u128) as u64)
        .unwrap_or(0);
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    use std::hash::{Hash, Hasher};
    mtime_ns.hash(&mut hasher);
    meta.len().hash(&mut hasher);
    // Inode distinguishes an atomic replace (write-temp-then-rename, which is how
    // `save()` writes) from an in-place rewrite with a coincidentally equal mtime
    // and size.
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.ino().hash(&mut hasher);
    }
    hasher.finish()
}

/// Whether the installed snapshot is absent, i.e. the first load has not
/// happened yet.
fn config_snapshot_missing() -> bool {
    installed_snapshot().is_none()
}

/// The installed snapshot, cloned out from under the read lock.
///
/// `None` only before the first successful install. Callers that see `None` have
/// skipped [`probe_due`], which returns true precisely while this is empty.
pub(super) fn installed_snapshot() -> Option<Arc<LeIndexConfig>> {
    CACHED_CONFIG
        .read()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

/// Replace the last-probe time with `when`. Test-only escape hatch so a test can
/// open the throttle deterministically instead of sleeping.
#[cfg(test)]
fn set_last_probe(when: Option<Duration>) {
    *LAST_PROBE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = when;
}

/// Whether the throttled stat should run now. Also true when nothing is
/// installed yet, so the first call always initialises.
pub(super) fn probe_due() -> bool {
    if config_snapshot_missing() {
        return true;
    }
    let last = *LAST_PROBE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match last {
        None => true,
        Some(last) => probe_clock().saturating_sub(last) >= CONFIG_REFRESH_THROTTLE,
    }
}

/// Stat, and if the file changed, re-parse and install. Returns the new snapshot
/// only when this call performed the swap; `None` means "no change" (the caller
/// re-reads the installed snapshot).
///
/// A parse failure keeps the previously installed config — a half-written
/// `leindex.toml` must not take the process down to defaults mid-session.
pub(super) fn refresh_cached_config() -> Option<Arc<LeIndexConfig>> {
    *LAST_PROBE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(probe_clock());
    let stamp = config_file_stamp();
    let installed_stamp = CACHED_CONFIG_STAMP.load(Ordering::Acquire);
    let installed_missing = config_snapshot_missing();
    if !installed_missing && stamp == installed_stamp {
        return None;
    }

    let _guard = CONFIG_REFRESH_LOCK.lock().unwrap_or_else(|poison| {
        // A panic while holding the lock cannot leave the config inconsistent
        // (the install is a single write to the snapshot slot), so recovering is
        // safe.
        poison.into_inner()
    });
    // Re-check under the lock: another thread may have installed while we waited.
    let installed_stamp = CACHED_CONFIG_STAMP.load(Ordering::Acquire);
    let installed_missing = config_snapshot_missing();
    if !installed_missing && stamp == installed_stamp {
        return None;
    }

    let loaded = match LeIndexConfig::load() {
        Ok(config) => config,
        Err(err) => {
            if installed_missing {
                tracing::warn!(
                    error = %err,
                    "failed to load leindex.toml for caching; preserving legacy disabled defaults"
                );
                LeIndexConfig {
                    neural: default_legacy_neural_config(),
                    indexing: default_legacy_indexing_config(),
                    ..LeIndexConfig::default()
                }
            } else {
                // A reload that fails (partial write, syntax error mid-edit) must
                // leave the working config in place. Say so and try again later.
                tracing::warn!(
                    error = %err,
                    "leindex.toml reload failed; keeping the previously loaded config"
                );
                CACHED_CONFIG_STAMP.store(stamp, Ordering::Release);
                return None;
            }
        }
    };

    if !installed_missing {
        let previous = {
            let slot = CACHED_CONFIG
                .read()
                .unwrap_or_else(|poison| poison.into_inner());
            // A read guard cannot outlive this block, so clone the handle.
            slot.clone()
        };
        if let Some(previous) = previous {
            if previous.as_ref() == &loaded {
                // Content-identical rewrite (formatting, comments): adopt the new
                // stamp so we stop re-parsing, but install nothing.
                CACHED_CONFIG_STAMP.store(stamp, Ordering::Release);
                return None;
            }
            let changed = previous.route_identity_changes(&loaded);
            if changed.is_empty() {
                tracing::info!(
                    "leindex.toml reloaded; no embedding-route keys changed — in-process caches remain valid"
                );
            } else {
                tracing::warn!(
                    changed = ?changed,
                    "leindex.toml embedding route changed and was reloaded. Components that resolved \
                     the previous route at startup (model, endpoint, exec provider) pick the new value \
                     on their next construction; vectors cached under the old identity stay keyed to it \
                     and will be re-embedded under the new one."
                );
            }
        }
    }

    let snapshot = Arc::new(loaded);
    *CACHED_CONFIG
        .write()
        .unwrap_or_else(|poison| poison.into_inner()) = Some(Arc::clone(&snapshot));
    CACHED_CONFIG_STAMP.store(stamp, Ordering::Release);
    Some(snapshot)
}

/// An owned, refcounted snapshot of the process-wide cached config.
///
/// Returned by [`LeIndexConfig::load_cached`]. It derefs to `&LeIndexConfig`, so
/// every existing usage — `load_cached().neural.enabled`,
/// `config.indexing.embeddings`, passing it to a fn taking `&LeIndexConfig` —
/// reads exactly as before, but the snapshot is *owned*: it stays valid until the
/// guard drops, and a concurrent reload frees the old config only once the last
/// guard for it is gone.
///
/// The distinction matters when a single operation reads the config more than
/// once. With a bare reference each read could observe a different snapshot if a
/// reload landed between them; with the guard, one operation sees one config.
#[derive(Debug, Clone)]
pub struct ConfigGuard(pub(super) Arc<LeIndexConfig>);

impl std::ops::Deref for ConfigGuard {
    type Target = LeIndexConfig;

    fn deref(&self) -> &LeIndexConfig {
        &self.0
    }
}

impl std::borrow::Borrow<LeIndexConfig> for ConfigGuard {
    fn borrow(&self) -> &LeIndexConfig {
        &self.0
    }
}

impl AsRef<LeIndexConfig> for ConfigGuard {
    fn as_ref(&self) -> &LeIndexConfig {
        &self.0
    }
}

impl From<ConfigGuard> for Arc<LeIndexConfig> {
    fn from(guard: ConfigGuard) -> Self {
        guard.0
    }
}

/// Takes ownership of the process-wide config cache and `LEINDEX_HOME`, restoring
/// both on drop.
///
/// Lives here rather than beside the tests because it manipulates this module's
/// private statics; a test-only handle is narrower than widening four statics to
/// `pub(super)` purely so a test can poke them.
///
/// A panic inside a test body unwinds straight past any cleanup written at the
/// end of the function, leaving that test's `leindex.toml` route installed for
/// every other test in the binary. A drop guard runs on the unwinding path too,
/// so the cache is restored either way.
///
/// The caller must hold the config tests' `ENV_LOCK` for as long as this lives.
#[cfg(test)]
pub(super) struct CacheSandbox {
    previous: Option<Arc<LeIndexConfig>>,
    previous_stamp: u64,
    previous_probe: Option<Duration>,
    previous_home: Option<std::ffi::OsString>,
}

#[cfg(test)]
impl CacheSandbox {
    /// Take ownership of the process-global cache and env state.
    pub(super) fn new() -> Self {
        let previous = CACHED_CONFIG
            .write()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let previous_stamp = CACHED_CONFIG_STAMP.load(Ordering::Acquire);
        let previous_probe = *LAST_PROBE
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let previous_home = std::env::var_os(super::LEINDEX_HOME_ENV);
        // Start from empty so the first load under test is genuinely cold instead
        // of reading whatever an earlier test installed.
        CACHED_CONFIG_STAMP.store(0, Ordering::Release);
        set_last_probe(None);
        Self {
            previous,
            previous_stamp,
            previous_probe,
            previous_home,
        }
    }

    /// Reopen the probe throttle so the next `load_cached()` stats the file
    /// instead of short-circuiting on the throttle window.
    pub(super) fn reopen_throttle(&self) {
        set_last_probe(None);
    }
}

#[cfg(test)]
impl Drop for CacheSandbox {
    fn drop(&mut self) {
        *CACHED_CONFIG
            .write()
            .unwrap_or_else(|poison| poison.into_inner()) = self.previous.take();
        CACHED_CONFIG_STAMP.store(self.previous_stamp, Ordering::Release);
        set_last_probe(self.previous_probe);
        // SAFETY: the caller holds ENV_LOCK for this guard's whole lifetime.
        match self.previous_home.take() {
            Some(home) => unsafe { std::env::set_var(super::LEINDEX_HOME_ENV, home) },
            None => unsafe { std::env::remove_var(super::LEINDEX_HOME_ENV) },
        }
    }
}
