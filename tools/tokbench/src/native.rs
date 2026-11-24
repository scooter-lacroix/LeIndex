//! NATIVE arm: byte-level emulation of Claude Code's tool outputs.
//!
//! Every function here documents where its format comes from, so a reviewer
//! can verify the emulation without owning Claude Code. Sources:
//!
//! - **Read** (`read_lines`): Claude Code's Read renders files exactly like
//!   `cat -n`: `format!("{:>6}\t{}", line_number, line)` with a 2000-line
//!   default limit and optional 1-based `offset`/`limit` parameters. System
//!   reminders about truncation are prompt-side scaffolding, not tool output,
//!   and are not emulated.
//! - **Grep** (`grep_content`/`grep_files`/`grep_count`): Claude Code's Grep
//!   shells out to ripgrep and returns its text verbatim — content mode with
//!   `-n` line numbers, `-C N` context, `--color=never --no-heading -S`
//!   (smart case) and `--sort=path` (deterministic order), plus the
//!   `files_with_matches` (`-l`) and `count` (`-c`) output modes; `-F` for
//!   literal-pattern searches. Ripgrep is invoked FOR REAL. When `rg` is not
//!   installed these functions return `None` and callers must skip the call
//!   loudly (`rg not available — skipped`), never fabricate output.
//! - **Edit** (`edit_message`): Claude Code's Edit returns a short success
//!   message; the emulated string mirrors its shape. The file mutation is
//!   performed for real by the task runner; only the message is emulated.
//! - **Git** (`git`): real `git` CLI invocations with verbatim stdout
//!   (`status`, `diff`, `log --oneline`, `show --stat`, `add`, `commit -m`);
//!   stderr is appended on failure so error text is measured too.
//!
//! The arm never truncates on its own: Read's 2000-line default and any
//! explicit offset/limit are the model-visible behaviors, and `rg`/`git`
//! are invoked exactly as Claude Code would invoke them.

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

/// Claude Code `Read`: `cat -n` style rendering with the 2000-line default.
pub fn read_lines(
    file: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> std::io::Result<String> {
    let content = std::fs::read_to_string(file)?;
    let start = offset.unwrap_or(1).max(1);
    // Claude Code Read defaults to 2000 lines when no limit is given.
    let max_lines = limit.unwrap_or(2000);
    let mut out = String::new();
    for (idx, line) in content.lines().enumerate().skip(start - 1).take(max_lines) {
        let _ = writeln!(out, "{:>6}\t{}", idx + 1, line);
    }
    Ok(out)
}

/// Claude Code `Grep`, `output_mode: "content"` — real ripgrep, `None`
/// when `rg` is not on PATH.
pub fn grep_content(
    pattern: &str,
    root: &Path,
    context: usize,
    glob: Option<&str>,
    fixed_strings: bool,
) -> Option<String> {
    let mut cmd = rg_command()?;
    // --sort=path pins output line order: rg's default walk order is
    // filesystem-dependent, which would leak nondeterminism into token
    // counts. Claude Code's Grep accepts the same flag.
    cmd.arg("--sort=path")
        .arg(pattern)
        .arg("-n")
        .arg("--color=never")
        .arg("--no-heading")
        .arg("-S");
    if context > 0 {
        cmd.arg("-C").arg(context.to_string());
    }
    if let Some(glob) = glob {
        cmd.arg("--glob").arg(glob);
    }
    if fixed_strings {
        cmd.arg("-F");
    }
    cmd.arg(root);
    Some(run_stdout(&mut cmd))
}

/// Claude Code `Grep`, `output_mode: "files_with_matches"` — real ripgrep.
pub fn grep_files(pattern: &str, root: &Path, fixed_strings: bool) -> Option<String> {
    let mut cmd = rg_command()?;
    cmd.arg("--sort=path")
        .arg(pattern)
        .arg("-l")
        .arg("--color=never")
        .arg("-S");
    if fixed_strings {
        cmd.arg("-F");
    }
    cmd.arg(root);
    Some(run_stdout(&mut cmd))
}

/// Claude Code `Grep`, `output_mode: "count"` — real ripgrep.
pub fn grep_count(pattern: &str, root: &Path, fixed_strings: bool) -> Option<String> {
    let mut cmd = rg_command()?;
    cmd.arg("--sort=path")
        .arg(pattern)
        .arg("-c")
        .arg("--color=never")
        .arg("-S");
    if fixed_strings {
        cmd.arg("-F");
    }
    cmd.arg(root);
    Some(run_stdout(&mut cmd))
}

/// Locate `rg` on PATH and start every benchmark search with `--no-config`,
/// so a developer's `RIPGREP_CONFIG_PATH` (for example `--max-count=1`)
/// cannot change what the NATIVE arm measures. Each helper still appends its
/// own explicit flags on top of this constructor.
///
/// Claude Code's Grep requires ripgrep; when absent the NATIVE arm cannot
/// honestly run grep steps and callers must skip loudly.
fn rg_command() -> Option<Command> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join("rg");
        if is_executable(&candidate) {
            let mut command = Command::new(candidate);
            command.arg("--no-config");
            return Some(command);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Claude Code's short Edit success message, mirroring its phrasing. The
/// task runner performs the real file mutation; only this message is
/// emulated tool output.
pub fn edit_message() -> String {
    "The file has been edited. ".to_string()
}

/// Real `git` CLI output, verbatim. Stderr is appended on failure so the
/// measured text still contains what the agent would see.
pub fn git(args: &[&str], cwd: &Path) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .map_err(|e| anyhow::anyhow!("git {} failed to spawn: {e}", args.join(" ")))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    if !output.status.success() {
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    Ok(text)
}

/// Run a command and return stdout verbatim.
fn run_stdout(cmd: &mut Command) -> String {
    match cmd.output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(error) => format!("rg failed to spawn: {error}"),
    }
}

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;
