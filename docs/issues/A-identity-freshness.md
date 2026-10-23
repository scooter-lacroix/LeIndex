# A — File identity & cache freshness (Issues #88, #97)

Verification-only root-cause analysis on branch `fix/backlog-86-103-rootcause` (HEAD `f31f107e`).
Scope: textindex dirty detection (`src/search/textsearch/engine.rs`) and the ONNX content digest memo (`src/search/onnx/client.rs`).
No code changed. All line references were read directly; "hypothesis" items are flagged as such.

---

## Issue #88 — Windows file-freshness identity uses creation time instead of write-sensitive metadata

**Verdict: CONFIRMED**

**Evidence (verified by reading):**

The freshness identity and its per-platform implementation live entirely in `src/search/textsearch/engine.rs`:

- `FileIdentity` — `engine.rs:67-80`, four fields:
  ```rust
  pub struct FileIdentity {
      pub size: u64,
      pub mtime_ns: i64,
      pub ctime_ns: i64,   // doc: "ctime (unix) or creation time (windows)"
      pub ino: u64,        // doc: "Inode number (unix); 0 elsewhere."
  }
  ```
- `identity(meta)` — `engine.rs:82-95` builds it from `meta.len()`, `meta.modified()`, and `platform_identity(meta)`.
- **`#[cfg(windows)] platform_identity` — `engine.rs:104-111` (this is the reported bug):**
  ```rust
  #[cfg(windows)]
  fn platform_identity(meta: &std::fs::Metadata) -> (i64, u64) {
      let created = meta
          .created()                       // <- creation time, NOT write time
          .ok()
          .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
          .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
      (created, 0)                         // ino always 0
  }
  ```
- Unix counterpart `engine.rs:98-101` returns `(meta.ctime_nsec(), meta.ino())`; fallback `engine.rs:113-116` returns `(0, 0)`.
- The dirty check compares the full tuple — `engine.rs:266-274`:
  ```rust
  Ok(meta) => {
      identity(&meta)
          != FileIdentity {
              size: known.size,
              mtime_ns: known.mtime_ns,
              ctime_ns: known.ctime_ns,
              ino: known.ino,
          }
  }
  ```
- The tuple is persisted into the index format — `src/search/textsearch/index.rs:172-173` serialize `ctime_ns`/`ino` into the per-file entry (`FILE_ENTRY = 56`, `index.rs:41`; format v3, `index.rs:36-38`).

**True root cause:** On Windows the `ctime_ns` slot of the freshness tuple is populated with the file **creation time** (`metadata.created()`), which is a constant for the file's lifetime — Windows does not update it on writes (it is set once at file creation, and `std::fs::Metadata::created()` is the only "third timestamp" std exposes; there is no NTFS ctime equivalent in std). `ino` is hard-coded to `0`. Therefore the effective Windows identity degenerates to **`(size, mtime_ns)`**: creation time adds zero discriminator power, and no file index / USN marker is used. A same-length in-place edit whose mtime is restored (or landed within the same coarse mtime tick) leaves every compared field identical, `dirty_files` reports the file clean, and the stale trigram plan excludes it — the live verifier never re-reads it, so edits stay missing until a rebuild. This contradicts the module-level correctness claim “results are never stale” (`engine.rs:6-8`) and the “ctime … part of the comparison wherever the platform records them” comment (`engine.rs:263-265`): Windows *records* the field but it is write-invariant, so the claim of protection is unsound. On Unix the same scenario is caught by ctime/ino (`engine.rs:98-101`, tests at `engine.rs:1919-1984`).

**Per-platform write-sensitivity (in-place same-length edit):**
- Unix: `ctime` (inode change time) is kernel-managed and updated on every `write(2)`; it is un-forgeable by userspace (`utimensat` cannot set it). `mtime` is write-sensitive but forgeable. `size`/`ino` are not write-sensitive (ino changes only on replace-by-rename).
- Windows: `modified()` (NTFS last-write time, ~100 ns granularity) is the *only* std-exposed write-sensitive field. `created()` is write-invariant. The file index (`MetadataExt::file_index()`, stable std) changes only on replace-by-rename/recreation, not on in-place writes. There is no std-exposed USN journal. Hence: an in-place same-length edit changes only mtime; if mtime is restored, **no metadata tuple composed of std `Metadata` fields can detect it**.

**Minimal correct fix (root, not patch):**
1. `engine.rs:104-111` — stop stuffing creation time into `ctime_ns`. Use the Windows file index for the `ino` slot:
   ```rust
   #[cfg(windows)]
   fn platform_identity(meta: &std::fs::Metadata) -> (i64, u64) {
       use std::os::windows::fs::MetadataExt;
       (0, meta.file_index().unwrap_or(0))   // replacement-sensitive; std API, no new dep
   }
   ```
   This makes the tuple honest: `(size, mtime, 0, file_index)`, catching replace-by-rename with preserved size+mtime (the common editor-save path: temp file + rename), exactly as ino does on Unix. Requires **no new dependency** (`MetadataExt::file_index` is stable std since Rust 1.63).
2. Update the docs at `engine.rs:58-80` and `index.rs:98-105` to state the Windows identity is `(size, mtime, file_index)` — creation time is deliberately **not** used (write-invariant).
3. Scope the “never stale” claim (`engine.rs:6-8`): on Windows, an in-place same-length write with forged/restored mtime is **undetectable by metadata alone**; closing that residual gap requires content verification (hash the file when the metadata tuple matches), which is a larger design (see Risks). If the strong guarantee is required, the right place is a content-hash verification pass over identity-matching files (feature-gated), not a metadata substitute.

**Failing test to add** (`src/search/textsearch/engine.rs`, tests module): a `#[cfg(windows)]` counterpart to `test_dirty_files_detects_same_size_mtime_preserving_edit` (`engine.rs:1919-1956`):
- Same-size in-place rewrite (`"fn alpha() {}\n"` → `"fn delta() {}\n"`), then restore mtime (via `SetFileTime` / `std::fs::File::set_modified`), assert `dirty_files(root, &index)` **contains** `"a.rs"`.
- Today: FAILS (identity unchanged → clean). After fix (1): still FAILS for in-place (file index does not move) — this test defines the *strong* end-state and only passes once content verification (3) lands. To have a test that passes with the minimal change, also add a `#[cfg(windows)]` rename-replace test (write staged file, set same mtime, rename over `a.rs`) asserting `dirty_files` contains the file — that FAILS today and PASSES after fix (1).

**Blast radius / risks:**
- The index wire format (`index.rs:172-173`) already stores `ctime_ns`/`ino`; changing what `platform_identity` records changes per-file values but not the layout — v3 files stay readable, no version bump, no rebuild required (same 56-byte entries). A format bump is only needed if a field were *added*.
- `dirty_files` consumers: search candidate set (`engine.rs:603-615`), `search_symbols` (`symbols.rs:45`), and any caller of `dirty_files` — all go through the same identity compare, so the fix is centralized in one function.
- `identity()`/`platform_identity` are internal to `engine.rs`; no external API contract.
- Windows-side correctness becomes strictly better (rename-swap caught); the residual in-place+forged-mtime gap must be documented rather than claimed closed.
- Cross-platform: behavior on Unix unchanged (no regression; existing Unix tests at `engine.rs:1919-1984` must stay green).

**Open questions:** Does the team require the strong “never stale” guarantee on Windows (→ content-verification design), or is documented degradation to `(size, mtime, file_index)` acceptable? Windows CI availability for a `#[cfg(windows)]` test?

---

## Issue #97 — content digest memoization returns stale digest on same-size rewrite that preserves ctime

**Verdict: PARTIAL**

The reported *symptom* (stale digest served after a same-size rewrite preserving metadata) is real, but the stated *mechanism* — “keys by length/mtime/ctime/inode … ctime can be unchanged” — is wrong for the platform where it matters most, and the real exposure is the persisted Engram identity, not the memo itself.

**Evidence (verified by reading):**

- Memo key type — `src/search/onnx/client.rs:1924-1928`:
  ```rust
  /// Memo key for [`content_digest`]: `(len, mtime_ns, ctime, inode)`. ctime is
  /// the full `seconds << 32 | nanoseconds` value — nanoseconds alone can
  /// repeat across rewrites landing at the same offset within the second.
  #[cfg(feature = "cli")]
  type DigestKey = (u64, u128, u128, u64);
  ```
- `content_digest` — `client.rs:1941-1996`. The guard is cfg-split:
  - **Unix** `client.rs:1942-1955`:
    ```rust
    #[cfg(unix)]
    fn guard(meta: &std::fs::Metadata) -> Option<DigestKey> {
        use std::os::unix::fs::MetadataExt;
        Some((
            meta.len(),
            meta.modified()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos(),
            ((meta.ctime() as u128) << 32) | meta.ctime_nsec() as u128,
            meta.ino(),
        ))
    }
    ```
  - **non-Unix (incl. Windows)** `client.rs:1956-1968`:
    ```rust
    #[cfg(not(unix))]
    fn guard(meta: &std::fs::Metadata) -> Option<DigestKey> {
        Some((meta.len(), meta.modified()?...as_nanos(), 0, 0))
    }
    ```
    → On Windows the key is **`(len, mtime_ns)`** — no ctime, no file index. The issue's premise “keys by length/mtime/ctime/inode” is false on Windows.
- Memo is a **process-local** `static DIGESTS: OnceLock<Mutex<HashMap<PathBuf, (DigestKey, String)>>>` — `client.rs:1970`; keyed **by PATH** (`PathBuf`), `client.rs:1980-1994`. Not persisted; not content-keyed.
- Consumer: `engram_identity` — `client.rs:1395-1422`; the stamp embeds the digest: `Some(format!("{}:{}:{}", meta.len(), nanos, content_digest(path, &meta)?))` (`client.rs:1410-1415`). The identity then keys the **persisted on-disk Engram phrase-book**: `src/search/engram.rs:198-211` (`Engram::key` = `blake3("engram-key-v1\0" ‖ identity ‖ dim ‖ text)`; `row_path` = `~/.leindex/engram/{xx}/{hex}.vec`; `default_root` `engram.rs:451`), checked at `src/cli/leindex/query.rs:834-852` (`engram.get(identity, query, dim)` before embedding).
- Existing tests document the platform split:
  - Unix forged-swap → digest **must change**: `client_test.rs:431-464` (`test_content_digest_changes_when_content_swapped_within_forged_metadata`, asserts `assert_ne!`).
  - non-Unix forged-swap → digest **is a memo hit** (cfoded weakness): `client_test.rs:478-509` (`test_content_digest_forged_swap_is_a_memo_hit_without_unix_metadata`, asserts `assert_eq!`).
  - Stability under unchanged metadata: `client_test.rs:466-476`.

**True root cause:** `content_digest` memoizes under a metadata guard whose discriminative power is platform-dependent, and the digest feeds a *persisted* content-addressed cache:
- On **Unix** the guard includes full ctime (seconds<<32|nsec) + ino. ctime is kernel-managed and updated on every `write(2)` with the filesystem's clock granularity. On ext4/xfs/btrfs/tmpfs (nsec granularity) two distinct app-level rewrites cannot produce the same ctime, and userspace cannot forge ctime — so the claimed “rapid same-size rewrites leave ctime unchanged” is **not reachable** on mainstream local Linux: the memo misses and re-hashes, exactly as documented (`client.rs:1930-1939`). The mechanism *is* reachable on coarse-granularity filesystems (NFS v3 ~1 s, some FUSE/9p): two rewrites within the same second produce identical `(seconds, nsec=0)` ctime → stale memo hit.
- On **Windows** there is no ctime term at all (guard = `(len, mtime)`), so a same-size rewrite with restored/coincidentally-equal mtime is *always* a memo hit — but this is the documented, deliberate weakness (`client_test.rs:481-485`), not an “unchanged ctime” assumption.
- Severity: the memo itself is process-local and path-keyed (bounded per process), but its output determines the **Engram identity**, which keys a **persisted, cross-process, cross-branch** table (`engram.rs:198-211`, `451`). A stale digest → unchanged identity → the process serves old `.vec` rows computed for the *previous* model weights for the *new* model. That is the real blast radius, and it outlives the process.

**Stale-hit reachability & minimal reproduction:**
- Windows (always reachable, no coarse-granularity requirement): in one process, (1) `engram_identity()` with model files A; (2) rewrite `model.bin` in place, same byte length, different bytes; restore mtime (e.g. `file.set_modified(old)`); (3) `engram_identity()` again → same guard (`len`, `mtime`), memo hit → same digest → same identity → `engram.get(identity, query, dim)` returns the old embedding for the new model.
- Unix: same fixture **does not** trigger on ext4/xfs/btrfs/tmpfs (ctime changes); it triggers only on coarse-granularity filesystems (needs fs-level ctime tick ≤ write interval), which is a hypothesis to confirm on the target fs if that deployment matters.

**Minimal correct fix (root, not patch):**
1. **Stop memoizing where the guard has no un-forgeable write-sensitive field.** The memo exists to avoid re-reading multi-hundred-MB model files; on non-Unix the guard cannot distinguish a rewrite, so it must not serve a cached digest. Simplest correct change: make the `#[cfg(not(unix))]` `guard` return `None` (or bypass the memo entirely in `content_digest`) so every call re-hashes — model files are read once per process anyway (the memo's win is only for repeated identity queries in the same process). This flips `client_test.rs:505` to `assert_ne!`.
2. **Share the platform identity contract with the textsearch engine** (see Cross-cutting): have `content_digest`'s guard consume the same `FileIdentity`/`platform_identity` helper as `engine.rs`, so Unix and (future) Windows file-index improvements propagate to both paths in one place.
3. Optionally, on Windows add `MetadataExt::file_index()` to the guard to catch replace-by-rename swaps of model files (same class as #88); this does *not* cover in-place writes, which point (1) already handles by re-hashing.

**Failing test to add** (`src/search/onnx/client_test.rs`):
- Generalize the forged-swap fixture to all platforms: rename (or re-gate) `test_content_digest_forged_swap_is_a_memo_hit_without_unix_metadata` (`client_test.rs:478-509`) to assert `assert_ne!(digest1, digest2)` — i.e. `test_content_digest_rehashes_after_same_size_same_mtime_rewrite`. Today it FAILS on Windows/non-Unix (memo hit); after fix (1) it passes there. The Unix twin (`client_test.rs:431-464`) already passes and stays as the guard for the Unix memo path.

**Blast radius / risks:**
- `content_digest` is `#[cfg(feature = "cli")]` and its only caller is `engram_identity` (`client.rs:1414`); changing memo behavior touches only that path. Re-hashing per process on non-Unix costs one model read per process — acceptable (same order as today's first digest computation).
- The Engram on-disk format is untouched (keys are blake3 of identity; identities change value, not layout). No format migration.
- No change to the text index wire format. Unix behavior unchanged; existing Unix tests must stay green.
- Feature gates: tests must stay behind `#[cfg(all(feature = "cli", unix))]` / `#[cfg(all(feature = "cli", not(unix)))]` (or the equivalent split) to keep the onnx-without-cli compile matrix clean.

**Open questions:** Is NFS/coarse-granularity filesystem support in scope for Unix (would need a stricter guard or content fallback there too)? Should the Engram identity additionally include the model file's content hash unconditionally (killing the memo entirely) if model files are small enough?

---

## Cross-cutting invariant

Both issues are the same invariant, enforced twice with different levels of fidelity:

> **A recorded file identity must include at least one kernel-managed, write-sensitive discriminator. Where the platform exposes none (Windows), the identity must not claim ctime/ino parity; the code must either degrade honestly or verify by content.**

- The canonical tuple to share: **`(size, mtime_ns, ctime_ns, file_id)`**, where `ctime_ns` is Unix ctime (un-forgeable, write-sensitive), `file_id` is Unix ino / Windows `file_index()` (replacement-sensitive), and on Windows `ctime_ns = 0` — **never** creation time.
- Where the contract should live: a new shared module, e.g. `src/search/file_identity.rs`, exporting `FileIdentity` + `identity(&Metadata) -> FileIdentity` (the current `engine.rs:67-116` code), consumed by:
  - `engine.rs` (`identity()` at `:82`, `dirty_files` at `:267`), and
  - `client.rs` guard (`:1942-1968`) — replace the private `DigestKey` derivation with the shared identity.
  - On-disk index (`index.rs:172-173`) already persists exactly these four fields, so the wire format can stay v3.
- The invariant should be formalized as a doc/contract on the shared `FileIdentity` type (fields + per-platform semantics) plus the two tests above; the `platform_identity` `#[cfg]` variants should stay in the shared module so both consumers inherit platform fixes (e.g. the Windows file-index change) in one place.
