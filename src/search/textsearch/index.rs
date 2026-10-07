//! On-disk trigram index (`textindex/index.bin`).
//!
//! Layout (little endian, every section 8-byte aligned):
//!
//! ```text
//! header   96 B   magic "LETXIDX1", version, flags, counts, section offsets
//! files    56 B * file_count   path, size, mtime_ns, ctime_ns, ino,
//!                              symbol range, flags
//! paths    string pool
//! syms     16 B * total        start, end, name_off, kind|name_len
//! names    string pool
//! tri      16 B * tri_count    trigram, post_off, post_bytes, doc_count (sorted)
//! post     varint-delta file-id lists
//! trailer  8 B    "LETXEND1"
//! ```
//!
//! The file is written to a temporary name and renamed into place, so a reader
//! never observes a partial index. Postings are read straight from the
//! memory map; nothing is deserialized at open.

use super::trigram::Trigram;
use memmap2::Mmap;
use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

const MAGIC: &[u8; 8] = b"LETXIDX1";
const TRAILER: &[u8; 8] = b"LETXEND1";
// v2: the file table records ctime/ino alongside size/mtime. v1 freshness
// missed same-size edits that preserve mtime (metadata-preserving copies,
// coarse-resolution filesystems) — the stale trigram plan then excluded the
// edited file from the scan forever. Old v1 files fail this check on open and
// are transparently rebuilt by the caller.
// v3: ctime recorded at NANOSECOND resolution. Second-granular ctime missed
// an in-place same-length edit whose mtime was restored within the same
// clock second (only ctime_nsec moved), which the stale plan excluded
// forever. v2 files fail the version check and rebuild transparently.
const VERSION: u32 = 3;
const HEADER_LEN: usize = 96;
const FILE_ENTRY: usize = 56;
const SYM_ENTRY: usize = 16;
const TRI_ENTRY: usize = 16;

/// The file has NUL bytes in its head: never searched.
pub const FLAG_BINARY: u32 = 1;
/// Too large to trigram-index: always scanned directly.
pub const FLAG_ALWAYS_SCAN: u32 = 2;

/// Symbol kinds stored as one byte.
pub const KINDS: [&str; 13] = [
    "function",
    "method",
    "class",
    "struct",
    "enum",
    "trait",
    "interface",
    "module",
    "variable",
    "constant",
    "type",
    "impl",
    "other",
];

/// Map a PDG/SQLite node-type string to a stored kind code.
pub fn kind_code(kind: &str) -> u8 {
    let lowered = kind.to_ascii_lowercase();
    KINDS
        .iter()
        .position(|k| lowered.contains(k))
        .unwrap_or(KINDS.len() - 1) as u8
}

/// A named byte range inside one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SymbolSpan {
    /// Start byte offset.
    pub start: u32,
    /// End byte offset (exclusive).
    pub end: u32,
    /// Kind code (see [`KINDS`]).
    pub kind: u8,
    /// Symbol name.
    pub name: String,
}

/// One file's data at build time.
#[derive(Debug, Clone)]
pub struct FileInput {
    /// Path relative to the index root, forward slashes.
    pub rel_path: String,
    /// Size in bytes when indexed.
    pub size: u64,
    /// Modification time (ns since epoch) when indexed.
    pub mtime_ns: i64,
    /// Replacement-sensitive identity: inode change time (ns, unix) or
    /// creation time (windows); 0 where the platform offers neither. Catches
    /// same-size edits that preserve mtime, including within the same clock
    /// second.
    pub ctime_ns: i64,
    /// Inode number (unix); 0 elsewhere. Catches replace-by-rename edits even
    /// when timestamps are preserved.
    pub ino: u64,
    /// [`FLAG_BINARY`] / [`FLAG_ALWAYS_SCAN`].
    pub flags: u32,
    /// Distinct, sorted trigrams (empty for flagged files).
    pub trigrams: Vec<Trigram>,
    /// Symbol spans, any order.
    pub symbols: Vec<SymbolSpan>,
}

fn push_varint(out: &mut Vec<u8>, mut value: u32) {
    while value >= 0x80 {
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
    out.push(value as u8);
}

fn read_varint(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let mut value = 0u32;
    let mut shift = 0;
    loop {
        let byte = *bytes.get(*pos)?;
        *pos += 1;
        value |= u32::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Some(value);
        }
        shift += 7;
        if shift > 28 {
            return None;
        }
    }
}

fn align8(buf: &mut Vec<u8>) {
    while buf.len() % 8 != 0 {
        buf.push(0);
    }
}

/// Serialize `files` (already in final id order) to `path` atomically.
pub fn write_index(path: &Path, files: &[FileInput], has_symbols: bool) -> io::Result<()> {
    // Postings: trigram -> ascending file ids. Files arrive in id order, so
    // every list is built already sorted.
    let mut postings: HashMap<Trigram, Vec<u32>> = HashMap::new();
    for (id, file) in files.iter().enumerate() {
        for &t in &file.trigrams {
            postings.entry(t).or_default().push(id as u32);
        }
    }
    let mut trigrams: Vec<Trigram> = postings.keys().copied().collect();
    trigrams.sort_unstable();

    let mut paths = Vec::new();
    let mut names = Vec::new();
    let mut file_table = Vec::with_capacity(files.len() * FILE_ENTRY);
    let mut sym_table = Vec::new();
    let mut sym_total = 0u32;
    for file in files {
        let path_off = paths.len() as u32;
        paths.extend_from_slice(file.rel_path.as_bytes());
        let mut spans: Vec<&SymbolSpan> = file.symbols.iter().filter(|s| s.end > s.start).collect();
        spans.sort_by_key(|s| (s.start, std::cmp::Reverse(s.end)));
        file_table.extend_from_slice(&path_off.to_le_bytes());
        file_table.extend_from_slice(&(file.rel_path.len() as u32).to_le_bytes());
        file_table.extend_from_slice(&file.size.to_le_bytes());
        file_table.extend_from_slice(&file.mtime_ns.to_le_bytes());
        file_table.extend_from_slice(&file.ctime_ns.to_le_bytes());
        file_table.extend_from_slice(&file.ino.to_le_bytes());
        file_table.extend_from_slice(&sym_total.to_le_bytes());
        file_table.extend_from_slice(&(spans.len() as u32).to_le_bytes());
        file_table.extend_from_slice(&file.flags.to_le_bytes());
        file_table.extend_from_slice(&0u32.to_le_bytes());
        for span in spans {
            let name_off = names.len() as u32;
            let name = span.name.as_bytes();
            let name = &name[..name.len().min(0xff_ffff)];
            names.extend_from_slice(name);
            sym_table.extend_from_slice(&span.start.to_le_bytes());
            sym_table.extend_from_slice(&span.end.to_le_bytes());
            sym_table.extend_from_slice(&name_off.to_le_bytes());
            let packed = (u32::from(span.kind) << 24) | name.len() as u32;
            sym_table.extend_from_slice(&packed.to_le_bytes());
            sym_total += 1;
        }
    }

    let mut post = Vec::new();
    let mut tri_table = Vec::with_capacity(trigrams.len() * TRI_ENTRY);
    for t in &trigrams {
        let ids = &postings[t];
        let start = post.len() as u32;
        let mut previous = 0u32;
        for (i, id) in ids.iter().enumerate() {
            push_varint(&mut post, if i == 0 { *id } else { id - previous });
            previous = *id;
        }
        tri_table.extend_from_slice(&t.to_le_bytes());
        tri_table.extend_from_slice(&start.to_le_bytes());
        tri_table.extend_from_slice(&(post.len() as u32 - start).to_le_bytes());
        tri_table.extend_from_slice(&(ids.len() as u32).to_le_bytes());
    }

    let mut body = vec![0u8; HEADER_LEN];
    let section = |buf: &mut Vec<u8>, data: &[u8]| -> u64 {
        align8(buf);
        let offset = buf.len() as u64;
        buf.extend_from_slice(data);
        offset
    };
    let off_files = section(&mut body, &file_table);
    let off_paths = section(&mut body, &paths);
    let off_syms = section(&mut body, &sym_table);
    let off_names = section(&mut body, &names);
    let off_tri = section(&mut body, &tri_table);
    let off_post = section(&mut body, &post);
    align8(&mut body);
    body.extend_from_slice(TRAILER);
    let total_len = body.len() as u64;

    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.extend_from_slice(&VERSION.to_le_bytes());
    header.extend_from_slice(&u32::from(has_symbols).to_le_bytes());
    header.extend_from_slice(&(files.len() as u32).to_le_bytes());
    header.extend_from_slice(&(trigrams.len() as u32).to_le_bytes());
    header.extend_from_slice(&u64::from(sym_total).to_le_bytes());
    for value in [
        off_files, off_paths, off_syms, off_names, off_tri, off_post, total_len,
    ] {
        header.extend_from_slice(&value.to_le_bytes());
    }
    header.resize(HEADER_LEN, 0);
    body[..HEADER_LEN].copy_from_slice(&header);

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Unique per build: two concurrent in-process sessions (MCP connections
    // sharing one daemon) build the same project's index; a pid-only name
    // made them truncate each other's temp file and publish an interleaved,
    // permanently-invalid index.
    static BUILD_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = BUILD_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp{}.{}", std::process::id(), seq));
    // The temp is removed on every failure path: with unique names a failed
    // build used to leave a full-size orphan index behind (nothing sweeps
    // `index.tmp*`), one multi-megabyte file per interrupted build.
    if let Err(error) = write_index_tmp(&tmp, &body).and_then(|()| std::fs::rename(&tmp, path)) {
        let _ = std::fs::remove_file(&tmp);
        return Err(error);
    }
    Ok(())
}

fn write_index_tmp(tmp: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    let mut file = File::create(tmp)?;
    file.write_all(body)?;
    file.sync_all()
}

/// A file's entry in a loaded index.
#[derive(Debug, Clone, Copy)]
pub struct FileMeta<'a> {
    /// Relative path (forward slashes).
    pub path: &'a str,
    /// Size when indexed.
    pub size: u64,
    /// mtime (ns) when indexed.
    pub mtime_ns: i64,
    /// ctime/creation (ns) when indexed; see [`FileInput::ctime_ns`].
    pub ctime_ns: i64,
    /// Inode when indexed (unix); see [`FileInput::ino`].
    pub ino: u64,
    /// Flag bits.
    pub flags: u32,
    sym_start: u32,
    sym_count: u32,
}

/// A memory-mapped, read-only text index.
pub struct TextIndex {
    map: Mmap,
    file_count: u32,
    tri_count: u32,
    has_symbols: bool,
    off_files: usize,
    off_paths: usize,
    off_syms: usize,
    off_names: usize,
    off_tri: usize,
    off_post: usize,
    by_path: HashMap<String, u32>,
}

impl std::fmt::Debug for TextIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextIndex")
            .field("files", &self.file_count)
            .field("trigrams", &self.tri_count)
            .finish()
    }
}

fn bad(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, format!("text index: {message}"))
}

fn u32_at(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}

struct ParsedHeader {
    file_count: u32,
    tri_count: u32,
    has_symbols: bool,
    off_files: usize,
    off_paths: usize,
    off_syms: usize,
    off_names: usize,
    off_tri: usize,
    off_post: usize,
}

fn parse_offsets(data: &[u8]) -> io::Result<[usize; 7]> {
    let mut offsets = [0usize; 7];
    for (i, slot) in offsets.iter_mut().enumerate() {
        *slot = u64_at(data, 32 + i * 8).ok_or_else(|| bad("truncated"))? as usize;
    }
    Ok(offsets)
}

fn validate_bounds(
    data: &[u8],
    offsets: &[usize; 7],
    file_count: u32,
    tri_count: u32,
) -> io::Result<()> {
    let [
        off_files,
        off_paths,
        off_syms,
        off_names,
        off_tri,
        off_post,
        total_len,
    ] = *offsets;
    if total_len != data.len() || &data[data.len() - 8..] != TRAILER {
        return Err(bad("length or trailer mismatch"));
    }
    if [off_files, off_paths, off_syms, off_names, off_tri, off_post]
        .iter()
        .any(|o| *o > data.len())
        || off_files + file_count as usize * FILE_ENTRY > data.len()
        || off_tri + tri_count as usize * TRI_ENTRY > data.len()
    {
        return Err(bad("section out of bounds"));
    }
    Ok(())
}

fn parse_header(data: &[u8]) -> io::Result<ParsedHeader> {
    if data.len() < HEADER_LEN + TRAILER.len() || &data[..8] != MAGIC {
        return Err(bad("bad magic"));
    }
    if u32_at(data, 8) != Some(VERSION) {
        return Err(bad("unsupported version"));
    }
    let has_symbols = u32_at(data, 12).ok_or_else(|| bad("truncated"))? & 1 == 1;
    let file_count = u32_at(data, 16).ok_or_else(|| bad("truncated"))?;
    let tri_count = u32_at(data, 20).ok_or_else(|| bad("truncated"))?;
    let offsets = parse_offsets(data)?;
    validate_bounds(data, &offsets, file_count, tri_count)?;
    let [
        off_files,
        off_paths,
        off_syms,
        off_names,
        off_tri,
        off_post,
        _,
    ] = offsets;
    Ok(ParsedHeader {
        file_count,
        tri_count,
        has_symbols,
        off_files,
        off_paths,
        off_syms,
        off_names,
        off_tri,
        off_post,
    })
}

fn build_path_index(index: &TextIndex, file_count: u32) -> io::Result<HashMap<String, u32>> {
    let mut by_path = HashMap::with_capacity(file_count as usize);
    for id in 0..file_count {
        let meta = index.file(id).ok_or_else(|| bad("bad file entry"))?;
        by_path.insert(meta.path.to_string(), id);
    }
    Ok(by_path)
}

impl TextIndex {
    /// Map and validate `path`.
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        // SAFETY: the index is only ever replaced by an atomic rename of a new
        // file; the mapped inode is never modified in place.
        let map = unsafe { Mmap::map(&file)? };
        let parsed = parse_header(&map)?;
        let mut index = Self {
            map,
            file_count: parsed.file_count,
            tri_count: parsed.tri_count,
            has_symbols: parsed.has_symbols,
            off_files: parsed.off_files,
            off_paths: parsed.off_paths,
            off_syms: parsed.off_syms,
            off_names: parsed.off_names,
            off_tri: parsed.off_tri,
            off_post: parsed.off_post,
            by_path: HashMap::new(),
        };
        index.by_path = build_path_index(&index, parsed.file_count)?;
        Ok(index)
    }

    /// Number of files in the table.
    pub fn file_count(&self) -> u32 {
        self.file_count
    }

    /// Number of distinct trigrams.
    pub fn trigram_count(&self) -> u32 {
        self.tri_count
    }

    /// Were symbol spans recorded at build time?
    pub fn has_symbols(&self) -> bool {
        self.has_symbols
    }

    /// Size of the index file in bytes.
    pub fn bytes(&self) -> usize {
        self.map.len()
    }

    /// Id of a relative path.
    pub fn id_of(&self, rel: &str) -> Option<u32> {
        self.by_path.get(rel).copied()
    }

    /// File table entry.
    pub fn file(&self, id: u32) -> Option<FileMeta<'_>> {
        if id >= self.file_count {
            return None;
        }
        let at = self.off_files + id as usize * FILE_ENTRY;
        let data = &self.map[..];
        let path_off = u32_at(data, at)? as usize;
        let path_len = u32_at(data, at + 4)? as usize;
        let path = data.get(self.off_paths + path_off..self.off_paths + path_off + path_len)?;
        Some(FileMeta {
            path: std::str::from_utf8(path).ok()?,
            size: u64_at(data, at + 8)?,
            mtime_ns: i64::from_le_bytes(data.get(at + 16..at + 24)?.try_into().ok()?),
            ctime_ns: i64::from_le_bytes(data.get(at + 24..at + 32)?.try_into().ok()?),
            ino: u64_at(data, at + 32)?,
            sym_start: u32_at(data, at + 40)?,
            sym_count: u32_at(data, at + 44)?,
            flags: u32_at(data, at + 48)?,
        })
    }

    /// Iterate file ids.
    pub fn file_ids(&self) -> impl Iterator<Item = u32> {
        0..self.file_count
    }

    fn tri_entry(&self, tri: Trigram) -> Option<(usize, usize, u32)> {
        let data = &self.map[..];
        let (mut lo, mut hi) = (0usize, self.tri_count as usize);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let at = self.off_tri + mid * TRI_ENTRY;
            let value = u32_at(data, at)?;
            match value.cmp(&tri) {
                std::cmp::Ordering::Equal => {
                    return Some((
                        u32_at(data, at + 4)? as usize,
                        u32_at(data, at + 8)? as usize,
                        u32_at(data, at + 12)?,
                    ));
                }
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
            }
        }
        None
    }

    /// Number of files containing `tri` (0 when absent).
    pub fn doc_count(&self, tri: Trigram) -> u32 {
        self.tri_entry(tri).map_or(0, |(_, _, count)| count)
    }

    /// Ascending file ids containing `tri`.
    pub fn postings(&self, tri: Trigram) -> Vec<u32> {
        let Some((offset, len, count)) = self.tri_entry(tri) else {
            return Vec::new();
        };
        let Some(bytes) = self
            .map
            .get(self.off_post + offset..self.off_post + offset + len)
        else {
            return Vec::new();
        };
        // `count` is an unvalidated header field; a torn index claiming
        // u32::MAX postings must not turn into a ~17 GiB allocation before
        // the varint decode (which is bounded by the section bytes anyway).
        let capacity = (count as usize).min(bytes.len());
        let mut out = Vec::with_capacity(capacity);
        let (mut pos, mut previous) = (0usize, 0u32);
        while pos < bytes.len() {
            let Some(delta) = read_varint(bytes, &mut pos) else {
                return Vec::new();
            };
            previous = if out.is_empty() {
                delta
            } else {
                previous + delta
            };
            out.push(previous);
        }
        out
    }

    /// Every recorded symbol of file `id` as `(name, kind, start, end)`, by start offset.
    pub fn symbols(&self, id: u32) -> Vec<(String, &'static str, u32, u32)> {
        let Some(meta) = self.file(id) else {
            return Vec::new();
        };
        let data = &self.map[..];
        (0..meta.sym_count)
            .filter_map(|i| {
                let at = self.off_syms + (meta.sym_start + i) as usize * SYM_ENTRY;
                let (start, end) = (u32_at(data, at)?, u32_at(data, at + 4)?);
                let name_off = u32_at(data, at + 8)? as usize;
                let packed = u32_at(data, at + 12)?;
                let len = (packed & 0xff_ffff) as usize;
                let name = data.get(self.off_names + name_off..self.off_names + name_off + len)?;
                let kind = KINDS
                    .get((packed >> 24) as usize)
                    .copied()
                    .unwrap_or("other");
                Some((String::from_utf8_lossy(name).into_owned(), kind, start, end))
            })
            .collect()
    }

    /// The innermost recorded symbol containing byte offset `offset` in file `id`.
    pub fn enclosing_symbol(
        &self,
        id: u32,
        offset: usize,
    ) -> Option<(String, &'static str, u32, u32)> {
        let meta = self.file(id)?;
        let data = &self.map[..];
        let mut best: Option<(u32, u32, u32, u32)> = None; // start,end,name_off,packed
        for i in 0..meta.sym_count {
            let at = self.off_syms + (meta.sym_start + i) as usize * SYM_ENTRY;
            let (start, end) = (u32_at(data, at)?, u32_at(data, at + 4)?);
            if start as usize > offset {
                break; // spans are sorted by start
            }
            if (offset as u64) < u64::from(end)
                && best.is_none_or(|(bs, be, _, _)| end - start <= be - bs)
            {
                best = Some((start, end, u32_at(data, at + 8)?, u32_at(data, at + 12)?));
            }
        }
        let (start, end, name_off, packed) = best?;
        let len = (packed & 0xff_ffff) as usize;
        let name =
            data.get(self.off_names + name_off as usize..self.off_names + name_off as usize + len)?;
        let kind = KINDS
            .get((packed >> 24) as usize)
            .copied()
            .unwrap_or("other");
        Some((String::from_utf8_lossy(name).into_owned(), kind, start, end))
    }
}

#[cfg(test)]
mod tests {
    use super::super::trigram::literal_trigrams;
    use super::*;

    fn input(path: &str, text: &str, symbols: Vec<SymbolSpan>) -> FileInput {
        FileInput {
            rel_path: path.to_string(),
            size: text.len() as u64,
            mtime_ns: 7,
            ctime_ns: 0,
            ino: 0,
            flags: 0,
            trigrams: literal_trigrams(text.as_bytes()),
            symbols,
        }
    }

    #[test]
    fn test_varint_round_trip() {
        for value in [0u32, 1, 127, 128, 300, 16_383, 16_384, u32::MAX >> 4] {
            let mut buf = Vec::new();
            push_varint(&mut buf, value);
            let mut pos = 0;
            assert_eq!(read_varint(&buf, &mut pos), Some(value));
            assert_eq!(pos, buf.len());
        }
        assert_eq!(read_varint(&[0x80], &mut 0), None);
    }

    #[test]
    fn test_index_round_trip_postings_files_and_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("textindex/index.bin");
        let files = vec![
            input(
                "a.rs",
                "fn alpha() { beta(); }",
                vec![SymbolSpan {
                    start: 0,
                    end: 22,
                    kind: kind_code("function"),
                    name: "alpha".into(),
                }],
            ),
            input("b.rs", "fn beta() {}", vec![]),
            FileInput {
                flags: FLAG_BINARY,
                ..input("c.bin", "", vec![])
            },
        ];
        write_index(&path, &files, true).unwrap();
        let index = TextIndex::open(&path).unwrap();

        assert_eq!(index.file_count(), 3);
        assert!(index.has_symbols());
        assert_eq!(index.file(1).unwrap().path, "b.rs");
        assert_eq!(index.file(2).unwrap().flags, FLAG_BINARY);
        assert_eq!(index.id_of("a.rs"), Some(0));

        let beta = literal_trigrams(b"beta");
        assert_eq!(index.postings(beta[0]), vec![0, 1]);
        let alpha = literal_trigrams(b"alph");
        assert_eq!(index.postings(alpha[0]), vec![0]);
        assert_eq!(index.doc_count(alpha[0]), 1);
        assert!(index.postings(literal_trigrams(b"zzz")[0]).is_empty());

        let (name, kind, start, end) = index.enclosing_symbol(0, 10).unwrap();
        assert_eq!(
            (name.as_str(), kind, start, end),
            ("alpha", "function", 0, 22)
        );
        assert!(index.enclosing_symbol(0, 22).is_none());
        assert!(index.enclosing_symbol(1, 3).is_none());
    }

    #[test]
    fn test_innermost_symbol_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.bin");
        let spans = vec![
            SymbolSpan {
                start: 0,
                end: 100,
                kind: kind_code("class"),
                name: "Outer".into(),
            },
            SymbolSpan {
                start: 20,
                end: 40,
                kind: kind_code("method"),
                name: "inner".into(),
            },
        ];
        write_index(&path, &[input("x.py", &"x".repeat(100), spans)], true).unwrap();
        let index = TextIndex::open(&path).unwrap();
        assert_eq!(index.enclosing_symbol(0, 25).unwrap().0, "inner");
        assert_eq!(index.enclosing_symbol(0, 50).unwrap().0, "Outer");
    }

    #[test]
    fn test_corrupt_or_truncated_files_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("i.bin");
        write_index(&path, &[input("a.rs", "hello world", vec![])], false).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 5]).unwrap();
        assert!(TextIndex::open(&path).is_err());
        std::fs::write(&path, b"not an index at all, definitely not one........").unwrap();
        assert!(TextIndex::open(&path).is_err());
    }
}
