# LeIndex Troubleshooting (Rust Runtime)

Last updated: 2026-02-05

---

## 1) `leindex` command not found

### Linux
- Verify binary exists: `/usr/local/bin/leindex`
- Ensure `/usr/local/bin` is in `PATH`

### macOS
- Verify binary exists: `~/.leindex/bin/leindex`
- Add to shell path if required

### Windows
- Verify binary exists: `%USERPROFILE%\.leindex\bin\leindex.exe`
- Restart terminal to refresh PATH

---

## 2) Installer succeeds but phase command missing

Run:

```bash
leindex phase --help
```

If it fails, rerun installer. Current installers include phase/mcp/smoke verification and should fail fast when capabilities are missing.

---

## 3) Indexing errors

### Common causes
- Unsupported or malformed source files
- Permission errors on project paths
- Very large repos with constrained memory

### What to do

```bash
leindex diagnostics
leindex index /path/to/project
```

If needed, reduce scope or exclude heavy generated directories.

---

## 4) Unexpected incremental behavior

LeIndex uses freshness hashing + incremental refresh by default.

Force full refresh when diagnosing:

```bash
leindex phase --all --path /path/to/project --no-incremental-refresh
```

---

## 5) MCP issues

### Validate MCP mode

```bash
leindex mcp --help
```

### Validate HTTP mode

```bash
leindex serve --host 127.0.0.1 --port 47268
```

Check for port conflicts and JSON-RPC client misconfiguration.

---

## 6) Build from source issues

```bash
cargo check -p leindex -p lephase -p lerecherche -p lestockage
```

Ensure Rust toolchain is up to date (1.75+).

---

## 7) High token usage during analysis

Use phase-first triage to compress context before deep reading:

```bash
leindex phase --all --path /path/to/project
```

Then run focused `search`/`analyze` and manual file review.

---

## 8) Still stuck?

Open an issue with:
- command run,
- exact error output,
- OS/toolchain version,
- whether installed via script or cargo.
