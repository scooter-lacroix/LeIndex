//! Connection preamble between a stdio shim and `leindexd` (wire v2).
//!
//! A daemon serves many editors at once, so it cannot infer the client's
//! working directory (the inline server used its own cwd as the default
//! project). The shim therefore opens every connection with one line,
//!
//! ```text
//! {"leindex_hello":{"wire":2,"version":"2.0.0","cwd":"/work/proj","pid":123}}
//! ```
//!
//! and the daemon answers with one line, `{"leindex_ack":{...}}`, before any
//! JSON-RPC flows. Both lines are consumed by the shim/daemon pair; the MCP
//! client never sees them, and after the ack the stream is byte-transparent.
//!
//! The daemon uses `cwd` to (a) start warming that project's graph and search
//! engine immediately, and (b) fill in `project_path` on `tools/call` requests
//! that omit it, exactly as the inline server's default project did.

use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Wire version of the hello/ack preamble. Bumped on incompatible changes.
pub const WIRE_VERSION: u32 = 2;

const HELLO_KEY: &str = "leindex_hello";
const ACK_KEY: &str = "leindex_ack";

/// First line a shim sends.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    /// Preamble version the shim speaks.
    pub wire: u32,
    /// Shim's LeIndex version.
    pub version: String,
    /// Project the client works in (its cwd or `--project`), if any.
    pub cwd: Option<String>,
    /// Shim process id, for diagnostics.
    pub pid: u32,
}

/// The daemon's reply.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Ack {
    /// Preamble version the daemon speaks.
    pub wire: u32,
    /// Daemon's LeIndex version.
    pub version: String,
    /// Daemon process id (lets a shim replace a stale daemon).
    pub pid: u32,
    /// When the daemon started, in Unix milliseconds. A `leindexd` binary
    /// modified after this is newer than the running daemon.
    pub started_ms: u64,
    /// Connections currently attached, including this one.
    pub clients: usize,
    /// `false` when the daemon refuses this client; see `error`.
    pub ok: bool,
    /// Why the client was refused.
    pub error: Option<String>,
}

/// Encode a hello line (with trailing newline).
pub fn hello_line(hello: &Hello) -> String {
    format!("{}\n", serde_json::json!({ HELLO_KEY: hello }))
}

/// Encode an ack line (with trailing newline).
pub fn ack_line(ack: &Ack) -> String {
    format!("{}\n", serde_json::json!({ ACK_KEY: ack }))
}

/// Parse a hello line; `None` when the line is anything else (an ordinary
/// JSON-RPC frame from a client that predates the preamble).
pub fn parse_hello(line: &str) -> Option<Hello> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    serde_json::from_value(value.get(HELLO_KEY)?.clone()).ok()
}

/// Parse an ack line.
pub fn parse_ack(line: &str) -> Option<Ack> {
    let value: Value = serde_json::from_str(line.trim()).ok()?;
    serde_json::from_value(value.get(ACK_KEY)?.clone()).ok()
}

/// A cwd that names a place to work rather than "wherever the launcher was
/// started". Indexing `/` or the whole home directory because an editor was
/// opened without a folder would be a disaster, so those get no default.
pub fn is_projectish_cwd(cwd: &Path) -> bool {
    if cwd.parent().is_none() {
        return false;
    }
    let home = dirs::home_dir();
    home.as_deref() != Some(cwd)
}

/// If `payload` is a `tools/call` without a `project_path`, return it with
/// `project_path` set to `cwd`. `None` means "forward unchanged".
pub fn with_default_project(payload: &str, cwd: &str) -> Option<String> {
    // Cheap pre-check: most traffic is not a tool call.
    if !payload.contains("tools/call") {
        return None;
    }
    let mut value: Value = serde_json::from_str(payload).ok()?;
    if value.get("method")?.as_str()? != "tools/call" {
        return None;
    }
    let params = value.get_mut("params")?.as_object_mut()?;
    let arguments = params
        .entry("arguments")
        .or_insert_with(|| Value::Object(Default::default()));
    if arguments.is_null() {
        *arguments = Value::Object(Default::default());
    }
    let arguments = arguments.as_object_mut()?;
    if arguments
        .get("project_path")
        .is_some_and(|existing| existing.as_str().is_some_and(|s| !s.is_empty()))
    {
        return None;
    }
    arguments.insert("project_path".into(), Value::String(cwd.to_string()));
    serde_json::to_string(&value).ok()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_hello_round_trip() {
        let hello = Hello {
            wire: WIRE_VERSION,
            version: "2.0.0".into(),
            cwd: Some("/work/p".into()),
            pid: 7,
        };
        let line = hello_line(&hello);
        assert!(line.ends_with('\n'));
        assert_eq!(parse_hello(&line), Some(hello));
    }

    #[test]
    fn test_ack_round_trip() {
        let ack = Ack {
            wire: WIRE_VERSION,
            version: "2.0.0".into(),
            pid: 9,
            started_ms: 123,
            clients: 2,
            ok: true,
            error: None,
        };
        assert_eq!(parse_ack(&ack_line(&ack)), Some(ack));
    }

    #[test]
    fn test_ordinary_frames_are_not_hello() {
        assert_eq!(
            parse_hello(r#"{"jsonrpc":"2.0","id":1,"method":"initialize"}"#),
            None
        );
        assert_eq!(parse_hello("not json"), None);
        assert_eq!(parse_ack(r#"{"jsonrpc":"2.0"}"#), None);
    }

    #[test]
    fn test_default_project_fills_only_missing_project_path() {
        let call = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"leindex_explore","arguments":{"mode":"find","pattern":"x"}}}"#;
        let patched = with_default_project(call, "/work/p").expect("patched");
        let value: Value = serde_json::from_str(&patched).unwrap();
        assert_eq!(value["params"]["arguments"]["project_path"], "/work/p");
        assert_eq!(value["params"]["arguments"]["pattern"], "x");
        assert_eq!(value["id"], 3);

        let explicit = r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"t","arguments":{"project_path":"/other"}}}"#;
        assert_eq!(with_default_project(explicit, "/work/p"), None);
    }

    #[test]
    fn test_default_project_creates_missing_arguments_and_ignores_other_methods() {
        let call =
            r#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"leindex_analyze"}}"#;
        let patched = with_default_project(call, "/w").expect("patched");
        let value: Value = serde_json::from_str(&patched).unwrap();
        assert_eq!(value["params"]["arguments"]["project_path"], "/w");

        let list = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        assert_eq!(with_default_project(list, "/w"), None);
        let notification_mentioning_it =
            r#"{"jsonrpc":"2.0","method":"notifications/x","params":{"note":"tools/call"}}"#;
        assert_eq!(with_default_project(notification_mentioning_it, "/w"), None);
    }

    #[test]
    fn test_root_and_home_are_not_projects() {
        assert!(!is_projectish_cwd(Path::new("/")));
        if let Some(home) = dirs::home_dir() {
            assert!(!is_projectish_cwd(&home));
        }
        assert!(is_projectish_cwd(Path::new("/work/some/project")));
    }
}
