//! End-to-end tests of the daemon architecture with the real binaries:
//! `leindex mcp` (the shim) -> `leindexd` (one shared server per user).
//!
//! Each test uses its own `LEINDEX_HOME`, so its daemon, socket and lock are
//! private, and kills that daemon when it finishes.

#![cfg(all(feature = "cli", feature = "daemon-client", unix))]

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

fn leindex() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leindex"))
}

fn leindexd() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_leindexd"))
}

/// A private LeIndex home whose daemon is killed on drop.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("home"),
        }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn socket(&self) -> PathBuf {
        self.path().join("run").join("leindexd.sock")
    }

    fn daemon_pid(&self) -> Option<u32> {
        let text = std::fs::read_to_string(self.path().join("run/daemon.endpoint")).ok()?;
        serde_json::from_str::<Value>(&text).ok()?["pid"]
            .as_u64()
            .map(|pid| pid as u32)
    }

    fn apply(&self, command: &mut Command) {
        command
            .env("LEINDEX_HOME", self.path())
            .env("LEINDEXD_BIN", leindexd())
            .env("LEINDEX_FEATURE_PRECISION_INGEST", "0")
            .env("LEINDEX_WATCHER", "0")
            .env_remove("LEINDEX_FEATURE_DAEMON_CLIENT");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(pid) = self.daemon_pid() {
            // SAFETY: signalling the daemon this test started.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

fn process_alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks existence.
    let exists = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    exists
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|stat| !stat.contains(") Z"))
            .unwrap_or(false)
}

/// An MCP client speaking newline-delimited JSON-RPC to a `leindex mcp` shim.
struct Client {
    child: Child,
    stdin: ChildStdin,
    replies: Receiver<(Instant, Value)>,
    backlog: Vec<(Instant, Value)>,
    next_id: u64,
}

impl Client {
    fn start(home: &Home, cwd: &Path) -> Self {
        Self::start_with(home, cwd, |_| {})
    }

    fn start_with(home: &Home, cwd: &Path, tweak: impl FnOnce(&mut Command)) -> Self {
        let mut command = Command::new(leindex());
        command
            .arg("mcp")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        home.apply(&mut command);
        tweak(&mut command);
        let mut child = command.spawn().expect("spawn leindex mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(value) = serde_json::from_str::<Value>(&line) {
                    if tx.send((Instant::now(), value)).is_err() {
                        break;
                    }
                }
            }
        });
        let mut client = Self {
            child,
            stdin,
            replies,
            backlog: Vec::new(),
            next_id: 0,
        };
        let init = client.call(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "t", "version": "1"}}),
        );
        assert!(init["result"]["serverInfo"].is_object(), "{init}");
        client
    }

    fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let message = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{message}").unwrap();
        self.stdin.flush().unwrap();
        id
    }

    fn wait(&mut self, id: u64, timeout: Duration) -> (Instant, Value) {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(pos) = self.backlog.iter().position(|(_, v)| v["id"] == id) {
                return self.backlog.remove(pos);
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.replies.recv_timeout(remaining) {
                Ok(reply) => self.backlog.push(reply),
                Err(_) => panic!("no reply to request {id} within {timeout:?}"),
            }
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.wait(id, Duration::from_secs(120)).1
    }

    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        self.call("tools/call", json!({"name": name, "arguments": arguments}))
    }

    fn tool_text(&mut self, name: &str, arguments: Value) -> String {
        let reply = self.tool(name, arguments);
        reply["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn stderr(&mut self) -> String {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let mut text = String::new();
        if let Some(mut stderr) = self.child.stderr.take() {
            let _ = stderr.read_to_string(&mut text);
        }
        text
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project_with(file: &str, body: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("project");
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(dir.path().join("src").join(file), body).unwrap();
    dir
}

#[test]
fn test_two_clients_share_one_daemon_and_get_their_own_default_project() {
    let home = Home::new();
    let alpha = project_with("alpha.rs", "pub fn only_in_alpha_project() -> u32 { 1 }\n");
    let beta = project_with("beta.rs", "pub fn only_in_beta_project() -> u32 { 2 }\n");

    let mut first = Client::start(&home, alpha.path());
    let daemon = home.daemon_pid().expect("the shim started a daemon");
    let mut second = Client::start(&home, beta.path());
    assert_eq!(
        home.daemon_pid(),
        Some(daemon),
        "a second client must reuse the daemon, not start another"
    );
    assert!(process_alive(daemon));

    // No project_path given: each client's own cwd is its default project.
    let in_alpha = first.tool_text(
        "leindex_explore",
        json!({"mode": "find", "pattern": "only_in_alpha_project"}),
    );
    assert!(in_alpha.contains("only_in_alpha_project"), "{in_alpha}");
    let alpha_sees_beta = first.tool_text(
        "leindex_explore",
        json!({"mode": "find", "pattern": "only_in_beta_project"}),
    );
    assert!(
        !alpha_sees_beta.contains("beta.rs"),
        "client one must not see client two's project: {alpha_sees_beta}"
    );

    let in_beta = second.tool_text(
        "leindex_explore",
        json!({"mode": "find", "pattern": "only_in_beta_project"}),
    );
    assert!(in_beta.contains("only_in_beta_project"), "{in_beta}");
}

#[test]
fn test_ping_is_answered_while_a_tool_call_runs_on_the_same_connection() {
    let home = Home::new();
    // Big enough that a blocking index takes a visible amount of time.
    let project = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(project.path().join("src")).unwrap();
    for file in 0..300 {
        let mut body = String::new();
        for function in 0..25 {
            body.push_str(&format!(
                "pub fn f_{file}_{function}(a: u32) -> u32 {{ let b = a + {function}; if b > 3 {{ b * 2 }} else {{ b }} }}\n"
            ));
        }
        std::fs::write(project.path().join("src").join(format!("m{file}.rs")), body).unwrap();
    }
    let mut client = Client::start(&home, project.path());

    let slow = client.send(
        "tools/call",
        json!({"name": "leindex_manage", "arguments": {
            "action": "index", "project_path": project.path(), "wait": true}}),
    );
    let ping = client.send("ping", json!({}));
    let (ping_at, pong) = client.wait(ping, Duration::from_secs(20));
    assert!(pong["result"].is_object(), "{pong}");
    let (slow_at, _) = client.wait(slow, Duration::from_secs(300));
    assert!(
        ping_at < slow_at,
        "ping must not queue behind a running tool call"
    );
}

/// Speak the preamble directly, without the shim.
fn raw_connect(home: &Home) -> UnixStream {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(stream) = UnixStream::connect(home.socket()) {
            return stream;
        }
        assert!(Instant::now() < deadline, "daemon never listened");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn read_line(stream: &mut UnixStream) -> String {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    while stream.read(&mut byte).unwrap_or(0) == 1 {
        if byte[0] == b'\n' {
            break;
        }
        line.push(byte[0]);
    }
    String::from_utf8_lossy(&line).into_owned()
}

/// A spawned daemon that is killed and reaped when the test ends.
struct Daemon(Child);

impl std::ops::Deref for Daemon {
    type Target = Child;
    fn deref(&self) -> &Child {
        &self.0
    }
}

impl std::ops::DerefMut for Daemon {
    fn deref_mut(&mut self) -> &mut Child {
        &mut self.0
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start_daemon(home: &Home, idle_secs: u64) -> Daemon {
    std::fs::create_dir_all(home.path().join("run")).unwrap();
    let mut command = Command::new(leindexd());
    command
        .args(["--idle-timeout-secs", &idle_secs.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    home.apply(&mut command);
    Daemon(command.spawn().expect("spawn leindexd"))
}

#[test]
fn test_daemon_stays_up_for_an_attached_idle_client_and_exits_after_it_leaves() {
    let home = Home::new();
    let mut daemon = start_daemon(&home, 1);
    let mut stream = raw_connect(&home);
    let hello = json!({"leindex_hello": {"wire": 2, "version": env!("CARGO_PKG_VERSION"), "cwd": null, "pid": 1}});
    writeln!(stream, "{hello}").unwrap();
    let ack: Value = serde_json::from_str(&read_line(&mut stream)).unwrap();
    assert_eq!(ack["leindex_ack"]["ok"], true, "{ack}");

    // Quiet for three idle-timeout periods, but attached: must survive.
    std::thread::sleep(Duration::from_secs(3));
    assert!(daemon.try_wait().unwrap().is_none(), "daemon left a client");
    writeln!(stream, r#"{{"jsonrpc":"2.0","id":9,"method":"ping"}}"#).unwrap();
    let pong = read_line(&mut stream);
    assert!(pong.contains("\"id\":9"), "{pong}");

    drop(stream);
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if daemon.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "daemon did not exit after its last client left"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn test_daemon_refuses_an_incompatible_wire_version() {
    let home = Home::new();
    let _daemon = start_daemon(&home, 30);
    let mut stream = raw_connect(&home);
    let hello = json!({"leindex_hello": {"wire": 99, "version": "9.9.9", "cwd": null, "pid": 1}});
    writeln!(stream, "{hello}").unwrap();
    let ack: Value = serde_json::from_str(&read_line(&mut stream)).unwrap();
    assert_eq!(ack["leindex_ack"]["ok"], false, "{ack}");
    assert!(
        ack["leindex_ack"]["error"]
            .as_str()
            .unwrap_or_default()
            .contains("wire"),
        "{ack}"
    );
}

#[test]
fn test_second_daemon_refuses_to_start() {
    let home = Home::new();
    let _first = start_daemon(&home, 30);
    let _ = raw_connect(&home);
    let mut second = Command::new(leindexd())
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .stdin(Stdio::null())
        .env("LEINDEX_HOME", home.path())
        .spawn()
        .unwrap();
    let status = second.wait().unwrap();
    assert!(!status.success(), "second daemon must not run");
    let mut stderr = String::new();
    second
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("already live"), "{stderr}");
}

#[test]
fn test_shim_falls_back_to_inline_server_when_no_daemon_binary_exists() {
    let home = Home::new();
    let project = project_with("solo.rs", "pub fn solo_fallback_symbol() {}\n");
    let mut client = Client::start_with(&home, project.path(), |command| {
        command.env("LEINDEXD_BIN", "/nonexistent/leindexd");
    });
    let text = client.tool_text(
        "leindex_explore",
        json!({"mode": "find", "pattern": "solo_fallback_symbol"}),
    );
    assert!(text.contains("solo_fallback_symbol"), "{text}");
    assert!(home.daemon_pid().is_none(), "no daemon may have started");
    assert!(
        client.stderr().contains("running standalone"),
        "the fallback must be explained on stderr"
    );
}

#[test]
fn test_daemon_can_be_disabled_with_the_feature_flag() {
    let home = Home::new();
    let project = project_with("off.rs", "pub fn flag_off_symbol() {}\n");
    let mut client = Client::start_with(&home, project.path(), |command| {
        command.env("LEINDEX_FEATURE_DAEMON_CLIENT", "0");
    });
    let text = client.tool_text(
        "leindex_explore",
        json!({"mode": "find", "pattern": "flag_off_symbol"}),
    );
    assert!(text.contains("flag_off_symbol"), "{text}");
    assert!(home.daemon_pid().is_none());
}
