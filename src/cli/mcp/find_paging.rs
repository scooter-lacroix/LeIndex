//! Stable continuation tokens for the list-returning tools (brief §6.8).
//!
//! A `next` token lets a caller page on without re-sending the query: the
//! token encodes a hash of the original call's arguments plus the next
//! offset, and the arguments themselves are kept in a process-local store.
//! The token is opaque — callers pass it back verbatim; decoding it by hand
//! is neither supported nor needed, and a token whose hash does not match
//! anything in the store is rejected as stale instead of silently re-running
//! a different query.
//!
//! Deliberate limits, both documented in the schemas:
//! * the store lives in the serving process, so tokens survive across MCP
//!   calls to the daemon but not across separate one-shot CLI invocations;
//! * `offset` keeps working unchanged — a caller who prefers explicit
//!   offsets never needs a token.

use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Continuations kept at once. LRU: the oldest entry is evicted when the
/// cap is hit, and a `next` past eviction is a stale-token error telling
/// the caller to re-send the query.
const STORE_CAP: usize = 256;
/// How long a stored continuation stays resumable.
const STORE_TTL: Duration = Duration::from_secs(3600);

struct Store {
    /// issue-order of hashes, oldest first, for LRU eviction.
    order: Vec<u64>,
    entries: HashMap<u64, Entry>,
}

struct Entry {
    tool: &'static str,
    args: Value,
    /// Per-file resume cursors keyed by the token's `next_offset`: a page
    /// issues several tokens for the same query (one per offset), and each
    /// token must replay the cursor state that was current when IT was
    /// issued — a later page for the same query must not overwrite the
    /// cursors an earlier token still needs.
    cursors: HashMap<usize, Value>,
    at: Instant,
}

fn store() -> &'static Mutex<Store> {
    static STORE: std::sync::LazyLock<Mutex<Store>> = std::sync::LazyLock::new(|| {
        Mutex::new(Store {
            order: Vec::new(),
            entries: HashMap::new(),
        })
    });
    &STORE
}

/// A fresh, caller-owned store.
///
/// The production store is process-wide, so a test that CHURNS past
/// `STORE_CAP` evicts other tests' live continuations — which showed up as an
/// intermittent stale-token failure in the handlers' own paging tests.
/// Eviction tests therefore run against an instance of their own; every other
/// caller keeps using [`store`].
#[cfg(test)]
fn empty_store() -> Store {
    Store {
        order: Vec::new(),
        entries: HashMap::new(),
    }
}

/// The arguments a continuation replays: the original call minus the paging
/// keys, which the token itself carries. `resume_cursors` is a paging key
/// too: the token owns the per-file resume state (§4.2e/§6.8), and a stable
/// query hash must not churn with it.
const PAGING_KEYS: &[&str] = &["offset", "next_offset", "next", "resume_cursors"];

fn hash_args(tool: &str, args: &Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // The tool is part of the identity: the same query arguments issued by
    // two different tools must occupy separate store entries, or one tool's
    // token could resolve into the other tool's entry (and be rejected as
    // wrong-tool — or, worse, overwrite the other tool's live continuation).
    let mut hasher = DefaultHasher::new();
    tool.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    hasher.finish()
}

/// Strip paging keys and normalise the shape so a hash depends only on the
/// query, not on how the caller spelled the previous page's position.
fn continuation_args(args: &Value) -> Value {
    let mut object = match args {
        Value::Object(map) => map.clone(),
        _ => return Value::Null,
    };
    for key in PAGING_KEYS {
        object.remove(*key);
    }
    Value::Object(object)
}

/// Issue a continuation token for `tool` at `next_offset`. The stored
/// arguments are the caller's arguments minus every paging key, so a
/// resumed call replays the query exactly and takes its position only from
/// the token.
pub fn issue(tool: &'static str, args: &Value, next_offset: usize) -> String {
    issue_with_cursors(tool, args, next_offset, &Value::Null)
}

/// `issue` for hit pages whose stream carries per-file resume cursors
/// (`SearchOutput::resume`): the token replays them so the next page skips
/// exactly what earlier pages already delivered, per file.
pub fn issue_with_cursors(
    tool: &'static str,
    args: &Value,
    next_offset: usize,
    cursors: &Value,
) -> String {
    issue_in(store(), tool, args, next_offset, cursors)
}

/// [`issue_with_cursors`] against an explicit store. Production uses the
/// process-wide one ([`store`]); the eviction tests use an instance of their
/// own so churning past the cap cannot evict another test's live token.
fn issue_in(
    store: &Mutex<Store>,
    tool: &'static str,
    args: &Value,
    next_offset: usize,
    cursors: &Value,
) -> String {
    let stored = continuation_args(args);
    if stored.is_null() {
        return String::new();
    }
    let hash = hash_args(tool, &stored);
    let Ok(mut guard) = store.lock() else {
        return String::new();
    };
    if !guard.entries.contains_key(&hash) {
        guard.order.push(hash);
        if guard.order.len() > STORE_CAP {
            let oldest = guard.order.remove(0);
            guard.entries.remove(&oldest);
        }
    } else if let Some(position) = guard.order.iter().position(|known| *known == hash) {
        // Re-use refreshes recency: without this, a token re-issued for the
        // same query keeps its original insertion slot and a hot entry can be
        // evicted as "oldest" while it is the one being paged through. Move it
        // to the back — the LRU is now actually least-RECENTLY-used.
        let recent = guard.order.remove(position);
        guard.order.push(recent);
    }
    let mut cursors_by_offset = guard
        .entries
        .get(&hash)
        .map(|entry| entry.cursors.clone())
        .unwrap_or_default();
    cursors_by_offset.insert(next_offset, cursors.clone());
    guard.entries.insert(
        hash,
        Entry {
            tool,
            args: stored,
            cursors: cursors_by_offset,
            at: Instant::now(),
        },
    );
    format!("{hash:016x}.{next_offset}")
}

/// A continuation recovered from a token.
#[derive(Debug)]
pub struct Continuation {
    /// The stored arguments (no paging keys); merge the caller's
    /// `project_path` (and any other explicit overrides) over this.
    pub args: Value,
    /// The position the token encodes.
    pub offset: usize,
    /// Per-file resume cursors the token replays (empty/Null for pages
    /// whose stream needs none).
    pub cursors: Value,
}

/// Resolve a token issued by `tool`. `None` with a reason when the token is
/// stale, truncated, or was issued by a different tool — the caller gets an
/// actionable message instead of a silently different query.
pub fn resume(tool: &'static str, token: &str) -> Result<Continuation, String> {
    resume_in(store(), tool, token)
}

/// [`resume`] against an explicit store (the eviction tests' own instance).
fn resume_in(
    store: &Mutex<Store>,
    tool: &'static str,
    token: &str,
) -> Result<Continuation, String> {
    let (hash_part, offset_part) = token
        .split_once('.')
        .ok_or_else(|| "malformed next token; re-send the query".to_string())?;
    let hash = u64::from_str_radix(hash_part, 16)
        .map_err(|_| "malformed next token; re-send the query".to_string())?;
    let offset: usize = offset_part
        .parse()
        .map_err(|_| "malformed next token; re-send the query".to_string())?;
    let Ok(mut guard) = store.lock() else {
        return Err("continuation store unavailable; re-send the query".to_string());
    };
    let Some(entry) = guard.entries.get(&hash) else {
        return Err(
            "next token is stale or from another process; re-send the query with offset"
                .to_string(),
        );
    };
    if entry.tool != tool {
        return Err(format!(
            "next token was issued by {} not {}; re-send the query",
            entry.tool, tool
        ));
    }
    if entry.at.elapsed() > STORE_TTL {
        guard.entries.remove(&hash);
        // The eviction order must not keep a dead slot: a later reissue of
        // the same query would push a fresh hash onto `order` again, and if
        // the stale slot were still there, churn could evict the fresh entry
        // through it.
        if let Some(position) = guard.order.iter().position(|known| *known == hash) {
            guard.order.remove(position);
        }
        return Err("next token expired; re-send the query".to_string());
    }
    let cursors = entry.cursors.get(&offset).cloned().unwrap_or(Value::Null);
    Ok(Continuation {
        args: entry.args.clone(),
        offset,
        cursors,
    })
}

/// Merge a resumed continuation with the caller's arguments: the stored
/// query wins exactly as issued — only `project_path` (and the token-owned
/// position/cursors) may overlay it, so a caller cannot silently change the
/// result stream a token was issued for. Any other key the caller re-sends
/// is ignored: the token is the authority on the query.
pub fn merge(stored: &Value, caller: &Value, offset: usize, cursors: &Value) -> Value {
    let mut merged = stored.clone();
    if let (Some(base), Some(over)) = (merged.as_object_mut(), caller.as_object())
        && let Some(path) = over.get("project_path")
    {
        base.insert("project_path".to_string(), path.clone());
    }
    if let Some(base) = merged.as_object_mut() {
        base.insert("offset".to_string(), json!(offset));
        if cursors.is_object() && !cursors.as_object().is_none_or(Map::is_empty) {
            base.insert("resume_cursors".to_string(), cursors.clone());
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The continuation store is process-wide, so the tests in this module
    /// would otherwise evict each other's entries (the LRU test deliberately
    /// churns past `STORE_CAP`). One lock per test keeps the store's contents
    /// deterministic.
    fn test_guard() -> std::sync::MutexGuard<'static, ()> {
        static GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());
        GUARD
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn test_next_token_round_trips_query_and_offset() {
        let _guard = test_guard();
        let args = json!({"pattern": "needle", "project_path": "/tmp/p", "offset": 0});
        let token = issue("leindex_find", &args, 50);
        let resumed = resume("leindex_find", &token).expect("fresh token resumes");
        assert_eq!(resumed.offset, 50);
        assert_eq!(resumed.args["pattern"], "needle");
        assert!(
            resumed.args.get("offset").is_none(),
            "paging keys must not be stored: the token owns the position"
        );
    }

    #[test]
    fn test_resume_rejects_foreign_and_stale_tokens() {
        let _guard = test_guard();
        let args = json!({"pattern": "needle"});
        let token = issue("leindex_find", &args, 10);
        let error = resume("leindex_structural", &token).unwrap_err();
        assert!(error.contains("issued by leindex_find"), "{error}");
        assert!(resume("leindex_find", "nonsense").is_err());
        assert!(resume("leindex_find", "deadbeef.5").is_err());
    }

    #[test]
    fn test_merge_overlay_keeps_query_and_takes_token_offset() {
        let _guard = test_guard();
        let stored = json!({"pattern": "needle", "case": "smart"});
        let caller = json!({"project_path": "/elsewhere", "offset": 999, "next": "x"});
        let merged = merge(&stored, &caller, 25, &Value::Null);
        assert_eq!(merged["pattern"], "needle");
        assert_eq!(merged["project_path"], "/elsewhere");
        assert_eq!(merged["offset"], 25);
        assert!(merged.get("next").is_none());
    }

    #[test]
    fn test_cursors_round_trip_through_issue_and_merge() {
        let _guard = test_guard();
        let args = json!({"pattern": "needle", "per_file_cap": 20});
        let cursors = json!({"0:src/a.rs": 20, "0:src/b.rs": 20});
        let token = issue_with_cursors("leindex_find", &args, 20, &cursors);
        let resumed = resume("leindex_find", &token).expect("fresh token resumes");
        assert_eq!(resumed.cursors, cursors, "the token replays the cursors");
        let merged = merge(
            &resumed.args,
            &json!({"project_path": "/p"}),
            20,
            &resumed.cursors,
        );
        assert_eq!(
            merged["resume_cursors"], cursors,
            "merge injects the token's cursors for the next page"
        );
        // Bare `issue` stays cursor-free: pages whose stream needs none
        // must not grow a stale resume state.
        let plain = issue("leindex_find", &args, 50);
        let resumed_plain = resume("leindex_find", &plain).unwrap();
        let merged_plain = merge(&resumed_plain.args, &json!({}), 50, &resumed_plain.cursors);
        assert!(merged_plain.get("resume_cursors").is_none());
    }

    /// Re-issuing a token for the SAME query must refresh its recency.
    ///
    /// Regression (integration review, F6): `issue` only appended to `order`
    /// for a hash it had never seen, so a caller paging through one query kept
    /// the entry at its ORIGINAL insertion slot — it was the first thing
    /// evicted while actively in use.
    ///
    /// The structure matters: the hot hash is re-issued WHILE IT IS STILL
    /// PRESENT (the only way the refresh branch runs), then a full cap of
    /// distinct insertions follows. A twin store takes the identical setup
    /// WITHOUT the re-issue, and the same churn must evict it — that control
    /// is what makes this test fail if the refresh branch is deleted (an
    /// earlier version re-issued only after eviction, took the INSERT branch,
    /// and passed with the fix removed).
    ///
    /// Each store is a private instance ([`empty_store`]), so the churn cannot
    /// evict another test's live continuation from the process-wide store.
    #[test]
    fn test_reissue_refreshes_lru_position_of_an_existing_hash() {
        let args = json!({ "pattern": "hot-query" });
        // `tag` keeps each churn's hashes distinct: re-inserting the SAME
        // filler names would take the refresh branch and churn nothing.
        let churn = |store: &Mutex<Store>, tag: &str| {
            for index in 0..STORE_CAP - 1 {
                issue_in(
                    store,
                    "leindex_find",
                    &json!({ "pattern": format!("{tag}-{index}") }),
                    10,
                    &Value::Null,
                );
            }
        };

        // Twin A: hot entry issued, then RE-ISSUED while still present (the
        // only way the refresh branch runs).
        let refreshed_store = Mutex::new(empty_store());
        issue_in(&refreshed_store, "leindex_find", &args, 10, &Value::Null);
        churn(&refreshed_store, "first");
        let token = issue_in(&refreshed_store, "leindex_find", &args, 20, &Value::Null);
        churn(&refreshed_store, "second");

        // Twin B (control): identical setup, NO re-issue — nothing refreshes
        // the hot entry's position, so the same churn reaches it first.
        let stale_store = Mutex::new(empty_store());
        let stale_token = issue_in(&stale_store, "leindex_find", &args, 10, &Value::Null);
        churn(&stale_store, "first");
        churn(&stale_store, "second");

        assert_eq!(
            token.split('.').next(),
            stale_token.split('.').next(),
            "the same query keeps one hash"
        );
        assert!(
            resume_in(&stale_store, "leindex_find", &stale_token).is_err(),
            "control: without the refresh the entry is the oldest and the same churn evicts it"
        );
        assert!(
            resume_in(&refreshed_store, "leindex_find", &token).is_ok(),
            "the refreshed entry moved to the back, so a full cap of later insertions cannot evict it"
        );
    }

    /// #003: the same query arguments issued by two different tools must not
    /// share a store entry — each tool's token resolves only into its own
    /// continuation, and issuing one tool's token never corrupts the other's.
    #[test]
    fn test_same_query_arguments_stay_separate_across_tools() {
        let args = json!({ "pattern": "needle", "per_file_cap": 20 });
        let find_token = issue("leindex_find", &args, 10);
        let search_token = issue("leindex_search", &args, 10);
        assert_ne!(
            find_token, search_token,
            "different tools must get different token hashes for the same query"
        );
        let resumed = resume("leindex_find", &find_token).expect("find token resumes");
        assert_eq!(resumed.args["pattern"], "needle");
        assert!(resume("leindex_search", &find_token).is_err());
        assert!(resume("leindex_search", &search_token).is_ok());
        assert!(resume("leindex_find", &find_token).is_ok());
    }

    /// #002: cursor state is bound to the token that was issued, not to the
    /// query entry. A later page for the same query must not change what an
    /// earlier token replays.
    #[test]
    fn test_cursor_state_is_bound_to_the_issued_token() {
        let args = json!({ "pattern": "needle" });
        let cursors_a = json!({ "0:src/a.rs": 20 });
        let token_a = issue_with_cursors("leindex_find", &args, 10, &cursors_a);
        let cursors_b = json!({ "0:src/a.rs": 40, "0:src/b.rs": 5 });
        let _token_b = issue_with_cursors("leindex_find", &args, 20, &cursors_b);
        let resumed = resume("leindex_find", &token_a).expect("first token still resumes");
        assert_eq!(
            resumed.cursors, cursors_a,
            "the first token replays the cursor state it was issued with"
        );
    }

    /// #001: a caller cannot override the stored query when resuming — only
    /// `project_path` may change; anything else would silently alter the
    /// result stream the token was issued for.
    #[test]
    fn test_resume_rejects_overrides_that_change_the_query_stream() {
        let stored = json!({ "pattern": "needle", "case": "smart" });
        let caller = json!({
            "project_path": "/elsewhere",
            "pattern": "OTHER",
            "case": "sensitive",
        });
        let merged = merge(&stored, &caller, 25, &Value::Null);
        assert_eq!(
            merged["pattern"], "needle",
            "a resume must not change the query"
        );
        assert_eq!(merged["case"], "smart");
        assert_eq!(merged["project_path"], "/elsewhere");
        assert_eq!(merged["offset"], 25);
    }

    /// #096: expiry removes BOTH the entry and its eviction-order slot, so a
    /// reissued query cannot leave a duplicate slot that later churn evicts
    /// the fresh entry through.
    #[test]
    fn test_expired_entry_is_removed_from_the_eviction_order() {
        let args = json!({ "pattern": "hot-query" });
        let store = Mutex::new(empty_store());
        let token = issue_in(&store, "leindex_find", &args, 10, &Value::Null);
        let hash_part = token.split('.').next().unwrap();
        let hash = u64::from_str_radix(hash_part, 16).unwrap();
        {
            let mut guard = store.lock().unwrap();
            if let Some(entry) = guard.entries.get_mut(&hash) {
                entry.at = Instant::now() - STORE_TTL - Duration::from_secs(1);
            }
        }
        assert!(
            resume_in(&store, "leindex_find", &token).is_err(),
            "the aged token expires"
        );
        {
            let guard = store.lock().unwrap();
            assert!(!guard.entries.contains_key(&hash), "expired entry removed");
            assert!(
                !guard.order.contains(&hash),
                "no stale eviction slot remains"
            );
        }
        let fresh = issue_in(&store, "leindex_find", &args, 10, &Value::Null);
        {
            let guard = store.lock().unwrap();
            assert_eq!(
                guard.order.iter().filter(|known| **known == hash).count(),
                1,
                "reissuing after expiry must not duplicate the eviction slot"
            );
        }
        for index in 0..STORE_CAP - 1 {
            issue_in(
                &store,
                "leindex_find",
                &json!({ "pattern": format!("filler-{index}") }),
                10,
                &Value::Null,
            );
        }
        assert!(
            resume_in(&store, "leindex_find", &fresh).is_ok(),
            "the fresh entry survives churn"
        );
    }
}
