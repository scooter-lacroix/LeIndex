//! File identity for the text-search freshness check.
//!
//! Extracted from `engine.rs` by pure code motion (Large File Detection gate:
//! `engine.rs` crossed 2000 lines). Re-exported from `engine` so
//! `engine::FileIdentity` keeps its path.

use std::path::Path;
use std::time::UNIX_EPOCH;

/// The freshness identity recorded at index time and re-read at search time.
///
/// Size + mtime alone miss same-size edits that preserve mtime (metadata-
/// preserving copies, coarse-resolution filesystems): the stale trigram plan
/// then excludes the edited file and the live-content verifier never reads
/// it, so results introduced by the edit stay missing until a rebuild. ctime
/// (inode change time) advances on any content write and ino changes on
/// replace-by-rename; both are compared whenever the platform provides them
/// (0 elsewhere, where the check degrades to the old size/mtime behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// Size in bytes.
    pub size: u64,
    /// Modification time (ns since epoch).
    pub mtime_ns: i64,
    /// ctime: inode change time (unix) or NTFS change time (windows), ns
    /// since the UNIX epoch; 0 if unavailable. NANOSECOND resolution:
    /// second-granular ctime misses an in-place same-length edit whose mtime
    /// is restored within the same clock second (only ctime_nsec moved),
    /// which the stale plan then excluded forever. On Windows this is the
    /// write-sensitive change time, NOT creation time (#88): creation time
    /// does not advance on an in-place write.
    pub ctime_ns: i64,
    /// Inode number (unix); NTFS file index (windows); 0 elsewhere.
    pub ino: u64,
}

pub(super) fn identity(path: &Path, meta: &std::fs::Metadata) -> FileIdentity {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
    let (ctime_ns, ino) = platform_identity(path, meta);
    FileIdentity {
        size: meta.len(),
        mtime_ns: mtime,
        ctime_ns,
        ino,
    }
}

#[cfg(unix)]
fn platform_identity(_path: &Path, meta: &std::fs::Metadata) -> (i64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.ctime_nsec(), meta.ino())
}

// v4 Windows identity (#88): creation time does NOT advance on an in-place
// write, so a same-length edit that restores mtime left every compared field
// unchanged and dirty_files treated the file as clean forever. The
// write-sensitive fields come from GetFileInformationByHandleEx — the NTFS
// change time (the Windows analogue of Unix ctime) and the file index (the
// ino analogue). v3 files fail the textindex version check and rebuild
// transparently.
//
// Direct kernel32 FFI rather than std::os::windows::fs::MetadataExt:
// `change_time`/`file_index` are unstable (`windows_change_time` /
// `windows_by_handle`) and cannot compile at this crate's MSRV (1.88), and
// those APIs need a file handle anyway (not just &Metadata), so the handle
// is opened here. kernel32 is linked by default on MSVC targets — no new
// dependency, exactly what windows-sys does internally.
#[cfg(windows)]
fn platform_identity(path: &Path, _meta: &std::fs::Metadata) -> (i64, u64) {
    use std::os::windows::io::AsRawHandle;
    use std::os::windows::io::RawHandle;

    const FILE_BASIC_INFO_CLASS: u32 = 0;
    const FILE_ID_INFO_CLASS: u32 = 21;

    // FILE_BASIC_INFO: four FILETIMEs then attributes — the full struct is
    // required; a partial buffer fails the call with insufficient size.
    #[repr(C)]
    #[derive(Default)]
    struct FileBasicInfo {
        creation_time: i64,
        last_access_time: i64,
        last_write_time: i64,
        change_time: i64,
        file_attributes: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct FileIdInfo {
        volume_serial_number: u64,
        file_id: [u8; 16],
    }

    unsafe extern "system" {
        fn GetFileInformationByHandleEx(
            file: RawHandle,
            info_class: u32,
            info: *mut core::ffi::c_void,
            buf_len: u32,
        ) -> i32;
    }

    let Ok(file) = std::fs::File::open(path) else {
        return (0, 0);
    };
    let handle = file.as_raw_handle();
    unsafe {
        let mut basic = FileBasicInfo::default();
        let basic_ok = GetFileInformationByHandleEx(
            handle,
            FILE_BASIC_INFO_CLASS,
            &mut basic as *mut FileBasicInfo as *mut core::ffi::c_void,
            std::mem::size_of::<FileBasicInfo>() as u32,
        ) != 0;
        let mut id = FileIdInfo::default();
        let id_ok = GetFileInformationByHandleEx(
            handle,
            FILE_ID_INFO_CLASS,
            &mut id as *mut FileIdInfo as *mut core::ffi::c_void,
            std::mem::size_of::<FileIdInfo>() as u32,
        ) != 0;
        // FILETIME is 100-ns units since the Windows epoch (1601-01-01); this
        // field is ns since the UNIX epoch like every other member. Rebase in
        // i128: the naive `as i64 * 100` overflows (ns-since-1601 for 2026 is
        // ~1.3e19 > i64::MAX), and clamps guard pre-1970 timestamps and clock
        // oddities.
        const WINDOWS_TO_UNIX_EPOCH_100NS: i128 = 11_644_473_600 * 10_000_000;
        let change_time_ns = if basic_ok {
            ((i128::from(basic.change_time) - WINDOWS_TO_UNIX_EPOCH_100NS) * 100)
                .clamp(0, i64::MAX as i128) as i64
        } else {
            0
        };
        // NTFS stores the 64-bit MFT record number in the low half of the
        // 128-bit identifier; the high half is 0 on NTFS (nonzero on ReFS).
        let ino = if id_ok {
            u64::from_le_bytes(id.file_id[0..8].try_into().unwrap_or([0; 8]))
        } else {
            0
        };
        (change_time_ns, ino)
    }
}

#[cfg(not(any(unix, windows)))]
fn platform_identity(_path: &Path, _meta: &std::fs::Metadata) -> (i64, u64) {
    (0, 0)
}
