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
    /// Per-file resume cursors the token replays (`Null` when none).
    cursors: Value,
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

/// The arguments a continuation replays: the original call minus the paging
/// keys, which the token itself carries. `resume_cursors` is a paging key
/// too: the token owns the per-file resume state (§4.2e/§6.8), and a stable
/// query hash must not churn with it.
const PAGING_KEYS: &[&str] = &["offset", "next_offset", "next", "resume_cursors"];

fn hash_args(args: &Value) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
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
    let stored = continuation_args(args);
    if stored.is_null() {
        return String::new();
    }
    let hash = hash_args(&stored);
    let Ok(mut guard) = store().lock() else {
        return String::new();
    };
    if !guard.entries.contains_key(&hash) {
        guard.order.push(hash);
        if guard.order.len() > STORE_CAP {
            let oldest = guard.order.remove(0);
            guard.entries.remove(&oldest);
        }
    }
    guard.entries.insert(
        hash,
        Entry {
            tool,
            args: stored,
            cursors: cursors.clone(),
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
    let (hash_part, offset_part) = token
        .split_once('.')
        .ok_or_else(|| "malformed next token; re-send the query".to_string())?;
    let hash = u64::from_str_radix(hash_part, 16)
        .map_err(|_| "malformed next token; re-send the query".to_string())?;
    let offset: usize = offset_part
        .parse()
        .map_err(|_| "malformed next token; re-send the query".to_string())?;
    let Ok(mut guard) = store().lock() else {
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
        return Err("next token expired; re-send the query".to_string());
    }
    Ok(Continuation {
        args: entry.args.clone(),
        offset,
        cursors: entry.cursors.clone(),
    })
}

/// Merge a resumed continuation with the caller's arguments: the stored
/// query wins; the caller's `project_path` (and any other keys they
/// explicitly sent) overlay it; the position and the per-file resume
/// cursors come from the token.
pub fn merge(stored: &Value, caller: &Value, offset: usize, cursors: &Value) -> Value {
    let mut merged = stored.clone();
    if let (Some(base), Some(over)) = (merged.as_object_mut(), caller.as_object()) {
        for (key, value) in over {
            if key != "offset" && key != "next" && key != "resume_cursors" {
                base.insert(key.clone(), value.clone());
            }
        }
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

    #[test]
    fn test_next_token_round_trips_query_and_offset() {
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
        let args = json!({"pattern": "needle"});
        let token = issue("leindex_find", &args, 10);
        let error = resume("leindex_structural", &token).unwrap_err();
        assert!(error.contains("issued by leindex_find"), "{error}");
        assert!(resume("leindex_find", "nonsense").is_err());
        assert!(resume("leindex_find", "deadbeef.5").is_err());
    }

    #[test]
    fn test_merge_overlay_keeps_query_and_takes_token_offset() {
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
}
