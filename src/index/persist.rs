//! On-disk index, memory-mapped at startup.
//!
//! Without this, a cold start is unusable until a full network enumeration
//! finishes - seconds at best, minutes over a VPN. With it, the previous
//! listing is available in a few milliseconds and the refresh happens behind
//! the user. In *felt* speed this is plausibly the largest single improvement
//! in the crate.
//!
//! # Layout
//!
//! Little-endian throughout (Windows is little-endian only; the assumption is
//! asserted at compile time). A 96-byte header, then four 64-byte-aligned
//! sections:
//!
//! ```text
//! header | prefix | offsets [u32; n+1] | lower [u8] | orig [u8]
//! ```
//!
//! # Two Windows-specific traps, designed out
//!
//! **Renaming over a mapped file fails.** `fs::rename` onto a destination
//! that any process currently has memory-mapped returns
//! `ERROR_SHARING_VIOLATION`. That bug appears on the *second* run and is
//! invisible on a Linux CI machine. Avoided entirely by never rewriting an
//! index in place: each write goes to a fresh versioned filename, and a tiny
//! `latest` pointer file - which is never mapped - is what gets replaced.
//!
//! **A mapped file can change underneath you.** `Mmap::map` is `unsafe`
//! precisely because the OS makes no promise the bytes stay fixed. This is
//! mitigated three ways: the file is opened with a share mode that denies
//! other writers, it lives in a per-user directory, and - most importantly -
//! **no slice bound ever depends on live mapped bytes**. The offsets table is
//! validated and copied into owned memory at load time, so even under
//! adversarial mutation the worst outcome is garbled filenames, never
//! out-of-bounds access. Names are also always decoded lossily, never with
//! `from_utf8_unchecked`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;

use super::snapshot::{Arenas, Snapshot};
use super::{DirStamp, StampKind};
use crate::config::MAX_INDEX_AGE;

const _: () = assert!(
    cfg!(target_endian = "little"),
    "the index format is little-endian"
);

pub const MAGIC: [u8; 8] = *b"FILESIDX";

/// Bumped to 2 when cache files became per-directory; to 3 when the directory
/// stamp came under the checksum and gained a kind.
///
/// A v1 index was named only by volume serial, so two directories on the same
/// volume produced colliding names and a single shared `latest` pointer. A v2
/// index left the stamp outside the hash, so a single flipped bit there was
/// undetectable and presented as "the directory changed" - a full
/// re-enumeration of a million-entry share, on every load, with no symptom.
///
/// Each bump discards every pre-existing index once, cleanly, rather than
/// relying on the new validation to reject them one at a time. That costs one
/// cold start.
pub const FORMAT_VERSION: u16 = 3;
pub const HEADER_SIZE: usize = 96;
const ALIGN: usize = 64;

/// Bit 0: entries are NUL-separated. Bit 1: the listing was truncated.
const FLAG_NUL_SEPARATED: u32 = 1 << 0;
const FLAG_TRUNCATED: u32 = 1 << 1;
const FLAG_HAS_STAMP: u32 = 1 << 2;
/// Bit 3: the stamp came from the attribute fallback, so `change_time` is a
/// copy of `last_write` rather than an independent value. Recorded because
/// comparing stamps of different kinds is meaningless - see
/// [`crate::index::StampKind`].
const FLAG_STAMP_WRITE_ONLY: u32 = 1 << 3;

/// Stable cache identity for one indexed directory.
///
/// Derived from the directory, deliberately *not* from a mapping's name or
/// its position in the config: a rename or a reorder must not orphan a
/// perfectly good index, and — more importantly — repointing a mapping at a
/// different directory *must* change the key, so it cannot find the previous
/// share's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MappingKey(u64);

impl MappingKey {
    pub fn of(dir: &Path) -> Self {
        Self(crate::util::winpath::path_key(dir))
    }

    pub fn hex(self) -> String {
        format!("{:016x}", self.0)
    }
}

/// What a persisted index must match to be accepted.
#[derive(Debug, Clone, Copy)]
pub struct Expect<'a> {
    /// The directory the caller believes the index describes.
    pub dir: &'a Path,
    /// `None` means unknown, and the volume check is skipped. The directory
    /// check below still runs, which is what makes an unknown serial
    /// survivable rather than a hole.
    pub volume_serial: Option<u32>,
}

impl<'a> Expect<'a> {
    pub fn new(dir: &'a Path, volume_serial: Option<u32>) -> Self {
        Self { dir, volume_serial }
    }
}

/// Why a persisted index was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadError {
    Missing,
    TooSmall,
    BadMagic,
    /// Written by a different version of this program.
    VersionMismatch {
        found: u16,
    },
    /// The drive letter now points at a different volume, so the listing
    /// belongs to somebody else's share.
    VolumeMismatch {
        found: u32,
        expected: u32,
    },
    /// The stored listing describes a different directory than the mapping
    /// now points at. Serving it would return another share's file list.
    ///
    /// Backstops the per-directory cache key: the key already makes a
    /// repointed mapping look elsewhere, so reaching this means either a
    /// 64-bit collision or a hand-edited pointer. One string compare per cold
    /// start, against a failure whose symptom is "results from the wrong
    /// share".
    PrefixMismatch {
        found: String,
        expected: String,
    },
    Truncated,
    ChecksumMismatch,
    /// Older than [`MAX_INDEX_AGE`].
    TooOld,
    /// A structural invariant failed.
    Invalid(&'static str),
    Io(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Missing => write!(f, "no cached index"),
            Self::TooSmall => write!(f, "cached index is too small"),
            Self::BadMagic => write!(f, "cached index has a bad header"),
            Self::VersionMismatch { found } => {
                write!(
                    f,
                    "cached index is format v{found}, expected v{FORMAT_VERSION}"
                )
            }
            Self::VolumeMismatch { found, expected } => write!(
                f,
                "cached index belongs to volume {found:08X}, drive is now {expected:08X}"
            ),
            Self::PrefixMismatch { found, expected } => write!(
                f,
                "cached index describes {found}, but this mapping points at {expected}"
            ),
            Self::Truncated => write!(f, "cached index is truncated"),
            Self::ChecksumMismatch => write!(f, "cached index failed its checksum"),
            Self::TooOld => write!(f, "cached index is too old"),
            Self::Invalid(why) => write!(f, "cached index is invalid: {why}"),
            Self::Io(e) => write!(f, "cached index unreadable: {e}"),
        }
    }
}

#[inline]
fn align_up(v: usize) -> usize {
    v.div_ceil(ALIGN) * ALIGN
}

/// The bytes the checksum covers: the whole header except the checksum field
/// itself, followed by the offsets table.
///
/// One function, used by both the writer and the reader, so the two cannot
/// disagree about the coverage - which is how the stamp came to sit outside
/// it.
fn hashable(header: &[u8], offsets_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(HEADER_SIZE + offsets_bytes.len());
    out.extend_from_slice(&header[0..48]);
    out.extend_from_slice(&header[56..HEADER_SIZE]);
    out.extend_from_slice(offsets_bytes);
    out
}

/// FNV-1a. Used for corruption detection only, never for security.
fn hash64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// Validated header, layout, prefix and offsets, ready to assemble.
type Decoded = (Header, Sections, Box<str>, Box<[u32]>);

/// Byte offsets of each section, derived from the header.
#[derive(Debug, Clone, Copy)]
struct Sections {
    prefix: usize,
    offsets: usize,
    lower: usize,
    orig: usize,
    total: usize,
}

fn sections(prefix_len: usize, entry_count: usize, arena_len: usize) -> Sections {
    let prefix = align_up(HEADER_SIZE);
    let offsets = align_up(prefix + prefix_len);
    let lower = align_up(offsets + (entry_count + 1) * 4);
    let orig = align_up(lower + arena_len);
    Sections {
        prefix,
        offsets,
        lower,
        orig,
        total: orig + arena_len,
    }
}

// --- writing ---------------------------------------------------------------

/// Writes a snapshot to `dir`, returning the file written.
///
/// Every step is best-effort from the caller's point of view: a persistence
/// failure must never affect the running program.
pub fn save(dir: &Path, key: MappingKey, snapshot: &Snapshot) -> Result<PathBuf, LoadError> {
    std::fs::create_dir_all(dir).map_err(|e| LoadError::Io(e.to_string()))?;

    let prefix = snapshot.prefix().as_bytes();
    let entry_count = snapshot.len();
    let lower = snapshot.lower();
    let orig = snapshot.orig();
    let arena_len = lower.len();
    let s = sections(prefix.len(), entry_count, arena_len);

    let captured_nanos = snapshot
        .captured_at()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let mut flags = FLAG_NUL_SEPARATED;
    if snapshot.truncated() {
        flags |= FLAG_TRUNCATED;
    }
    if let Some(stamp) = snapshot.stamp() {
        flags |= FLAG_HAS_STAMP;
        if stamp.kind == StampKind::WriteOnly {
            flags |= FLAG_STAMP_WRITE_ONLY;
        }
    }
    let stamp = snapshot.stamp().unwrap_or(DirStamp::new(0, 0));
    let avg_name_len = arena_len
        .checked_div(entry_count)
        .map(|n| n.saturating_sub(1) as u32)
        .unwrap_or(0);

    // Offsets as raw bytes, so the header hash can cover them.
    let mut offsets_bytes = Vec::with_capacity((entry_count + 1) * 4);
    for &o in snapshot.offsets() {
        offsets_bytes.extend_from_slice(&o.to_le_bytes());
    }

    let mut header = vec![0u8; HEADER_SIZE];
    header[0..8].copy_from_slice(&MAGIC);
    header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
    header[12..16].copy_from_slice(&flags.to_le_bytes());
    header[16..20].copy_from_slice(&snapshot.volume_serial().to_le_bytes());
    header[20..24].copy_from_slice(&(entry_count as u32).to_le_bytes());
    header[24..32].copy_from_slice(&captured_nanos.to_le_bytes());
    header[32..36].copy_from_slice(&(prefix.len() as u32).to_le_bytes());
    header[36..40].copy_from_slice(&(arena_len as u32).to_le_bytes());
    header[40..44].copy_from_slice(&snapshot.max_name_len().to_le_bytes());
    header[44..48].copy_from_slice(&avg_name_len.to_le_bytes());
    // 48..56 is the hash, written after it is computed.
    header[56..64].copy_from_slice(&stamp.last_write.to_le_bytes());
    header[64..72].copy_from_slice(&stamp.change_time.to_le_bytes());

    // The hash deliberately covers the whole header and the offsets table,
    // but not the arenas. Hashing 60 MB of arenas would force every page
    // resident and defeat the lazy mapping this format exists for; a corrupt
    // *arena* can only produce wrong text, never an out-of-bounds read,
    // because all indexing goes through the validated offsets.
    //
    // v2 stopped at byte 48 and so left the stamp unprotected, which is the
    // one field whose corruption is both silent and expensive.
    let checksum = hash64(&hashable(&header, &offsets_bytes));
    header[48..56].copy_from_slice(&checksum.to_le_bytes());

    // A unique temp name in the same directory, so the rename is atomic and
    // never lands on an existing (possibly mapped) file.
    //
    // The temp name carries the mapping key too, and that is not cosmetic:
    // `gc` sweeps stale temporaries, so with one index actor per mapping an
    // unscoped sweep would delete a sibling mapping's temp file mid-write.
    let key_hex = key.hex();
    let tmp = dir.join(format!(
        "{}{:x}.tmp",
        temp_prefix(key),
        crate::util::once::now_nanos()
    ));
    let final_name = format!(
        "{key_hex}-{:08x}-{:016x}.idx",
        snapshot.volume_serial(),
        captured_nanos.max(0) as u64
    );
    let final_path = dir.join(&final_name);

    {
        let mut f = File::create(&tmp).map_err(|e| LoadError::Io(e.to_string()))?;
        let mut written = 0usize;
        let pad_to = |f: &mut File, written: &mut usize, target: usize| -> std::io::Result<()> {
            while *written < target {
                let chunk = (target - *written).min(ALIGN);
                f.write_all(&vec![0u8; chunk])?;
                *written += chunk;
            }
            Ok(())
        };

        let write = |f: &mut File, written: &mut usize, bytes: &[u8]| -> std::io::Result<()> {
            f.write_all(bytes)?;
            *written += bytes.len();
            Ok(())
        };

        (|| -> std::io::Result<()> {
            write(&mut f, &mut written, &header)?;
            pad_to(&mut f, &mut written, s.prefix)?;
            write(&mut f, &mut written, prefix)?;
            pad_to(&mut f, &mut written, s.offsets)?;
            write(&mut f, &mut written, &offsets_bytes)?;
            pad_to(&mut f, &mut written, s.lower)?;
            write(&mut f, &mut written, lower)?;
            pad_to(&mut f, &mut written, s.orig)?;
            write(&mut f, &mut written, orig)?;
            // Required: without it a power loss can leave a renamed but
            // zero-length file, which the next start would have to reject.
            f.sync_all()
        })()
        .map_err(|e| LoadError::Io(e.to_string()))?;

        debug_assert_eq!(written, s.total);
    }

    std::fs::rename(&tmp, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        LoadError::Io(e.to_string())
    })?;

    write_pointer(dir, key, &final_name)?;
    gc(dir, key, &final_name);
    Ok(final_path)
}

/// Replaces this mapping's pointer atomically.
///
/// One pointer per mapping, not one per cache directory: several indexed
/// directories share the cache, and a single `latest` would mean each save
/// hid the others.
///
/// The file is deliberately tiny and never mapped, so renaming over it can
/// never hit a sharing violation.
fn write_pointer(dir: &Path, key: MappingKey, name: &str) -> Result<(), LoadError> {
    let tmp = dir.join(format!(
        "ptr-{}-{:x}.tmp",
        key.hex(),
        crate::util::once::now_nanos()
    ));
    {
        let mut f = File::create(&tmp).map_err(|e| LoadError::Io(e.to_string()))?;
        f.write_all(name.as_bytes())
            .map_err(|e| LoadError::Io(e.to_string()))?;
        f.sync_all().map_err(|e| LoadError::Io(e.to_string()))?;
    }
    std::fs::rename(&tmp, pointer_path(dir, key)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        LoadError::Io(e.to_string())
    })
}

fn pointer_path(dir: &Path, key: MappingKey) -> PathBuf {
    dir.join(format!("latest-{}", key.hex()))
}

/// Removes this mapping's superseded files, best effort.
///
/// Scoped to `key`. An unscoped sweep would delete other mappings' indexes
/// and, worse, their in-flight temporaries: index actors run one per mapping
/// and share this directory.
///
/// A file another process still has mapped cannot be deleted; that is
/// expected, not an error, and the next run will collect it.
///
/// Temporaries belonging to *other processes* are left alone. Two instances of
/// the app index the same directory under the same key, so an unqualified
/// sweep would delete the other one's file between its rename and its pointer
/// write - leaving a pointer aimed at nothing, and a guaranteed cold start.
fn gc(dir: &Path, key: MappingKey, keep: &str) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let prefix = key.hex();
    let ours = temp_prefix(key);
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with(&prefix) {
            continue;
        }
        let stale_tmp = name.ends_with(".tmp") && name.starts_with(&ours);
        let superseded = name.ends_with(".idx") && name != keep;
        if stale_tmp || superseded {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// The `<key>-<pid>-` prefix every temporary this process writes shares.
fn temp_prefix(key: MappingKey) -> String {
    format!("{}-{:x}-", key.hex(), std::process::id())
}

/// Removes cache files belonging to directories that are no longer
/// configured.
///
/// Without this the cache grows forever as mappings are edited: `gc` only
/// ever sweeps keys that are still in use. Best effort, called once at
/// startup.
pub fn gc_orphans(dir: &Path, live: &[MappingKey]) {
    // An empty live set would mean "collect every cache file there is", which
    // is never what a caller means. It means the configuration could not be
    // resolved - no enabled mapping, an unreadable config - and doing nothing
    // is the only safe reading of that.
    if live.is_empty() {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let live: Vec<String> = live.iter().map(|k| k.hex()).collect();
    for entry in rd.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let owned_by_live = live.iter().any(|k| {
            name.starts_with(k.as_str())
                || name == format!("latest-{k}")
                || name.starts_with(&format!("ptr-{k}-"))
        });
        if owned_by_live {
            continue;
        }
        // Also collect the v1 layout, whose names carried no key at all.
        let looks_like_ours = name.ends_with(".idx")
            || name.ends_with(".tmp")
            || name == "latest"
            || name.starts_with("latest-");
        if looks_like_ours {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

// --- reading ---------------------------------------------------------------

/// Loads this mapping's most recent index from `dir`.
///
/// Three independent checks stop one share's listing being served for
/// another: the pointer is per-mapping, the recorded volume serial is
/// compared when known, and the directory the index describes is compared
/// against the one the caller expects.
pub fn load(dir: &Path, key: MappingKey, expect: Expect<'_>) -> Result<Snapshot, LoadError> {
    let pointer =
        std::fs::read_to_string(pointer_path(dir, key)).map_err(|_| LoadError::Missing)?;
    let name = pointer.trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(LoadError::Missing);
    }
    load_file(&dir.join(name), expect)
}

/// Loads a specific index file.
pub fn load_file(path: &Path, expect: Expect<'_>) -> Result<Snapshot, LoadError> {
    let file = open_shared_read(path)?;

    // Try the mapped path, then an owned read. A 66 MB `fs::read` is roughly
    // 30-60 ms warm, which is a perfectly acceptable fallback and still two
    // orders of magnitude better than a network enumeration.
    // SAFETY: see the module docs. The mapping is read-only, opened with a
    // share mode denying other writers, and no slice bound derived later
    // depends on the mapped bytes staying constant.
    match unsafe { Mmap::map(&file) } {
        Ok(map) => decode_mapped(map, expect),
        Err(_) => {
            let bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;
            decode_owned(&bytes, expect)
        }
    }
}

#[cfg(windows)]
fn open_shared_read(path: &Path) -> Result<File, LoadError> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LoadError::Missing,
            _ => LoadError::Io(e.to_string()),
        })
}

#[cfg(not(windows))]
fn open_shared_read(path: &Path) -> Result<File, LoadError> {
    OpenOptions::new()
        .read(true)
        .open(path)
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => LoadError::Missing,
            _ => LoadError::Io(e.to_string()),
        })
}

/// Everything the header says, once validated.
#[derive(Debug, Clone, Copy)]
struct Header {
    flags: u32,
    volume_serial: u32,
    entry_count: usize,
    captured_at: SystemTime,
    prefix_len: usize,
    arena_len: usize,
    max_name_len: u32,
    hash: u64,
    stamp: Option<DirStamp>,
}

fn parse_header(bytes: &[u8], expect: Expect<'_>) -> Result<Header, LoadError> {
    if bytes.len() < HEADER_SIZE {
        return Err(LoadError::TooSmall);
    }
    if bytes[0..8] != MAGIC {
        return Err(LoadError::BadMagic);
    }
    let u16at = |o: usize| u16::from_le_bytes([bytes[o], bytes[o + 1]]);
    let u32at = |o: usize| u32::from_le_bytes(bytes[o..o + 4].try_into().unwrap());
    let i64at = |o: usize| i64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());
    let u64at = |o: usize| u64::from_le_bytes(bytes[o..o + 8].try_into().unwrap());

    let version = u16at(8);
    if version != FORMAT_VERSION {
        return Err(LoadError::VersionMismatch { found: version });
    }
    if u16at(10) as usize != HEADER_SIZE {
        return Err(LoadError::Invalid("unexpected header size"));
    }

    let flags = u32at(12);
    let volume_serial = u32at(16);
    // Zero means "unknown", and it has to mean that symmetrically.
    //
    // The writer records `0` when the serial could not be resolved - which
    // happens when the startup query races the SMB session warm-up - and the
    // reader used to reject exactly that as a volume mismatch. The result was
    // a cache written by one run and thrown away by the next, at random,
    // depending on how quickly the redirector woke up. The directory check
    // below still runs either way, which is what makes an unknown serial
    // survivable rather than a hole.
    if let Some(expected) = expect.volume_serial
        && expected != 0
        && volume_serial != 0
        && volume_serial != expected
    {
        return Err(LoadError::VolumeMismatch {
            found: volume_serial,
            expected,
        });
    }

    let captured_nanos = i64at(24);
    let captured_at = if captured_nanos <= 0 {
        UNIX_EPOCH
    } else {
        UNIX_EPOCH + Duration::from_nanos(captured_nanos as u64)
    };
    if let Ok(age) = SystemTime::now().duration_since(captured_at)
        && age > MAX_INDEX_AGE
    {
        return Err(LoadError::TooOld);
    }

    let stamp = if flags & FLAG_HAS_STAMP == 0 {
        None
    } else if flags & FLAG_STAMP_WRITE_ONLY != 0 {
        Some(DirStamp::write_only(i64at(56)))
    } else {
        Some(DirStamp::new(i64at(56), i64at(64)))
    };

    Ok(Header {
        flags,
        volume_serial,
        entry_count: u32at(20) as usize,
        captured_at,
        prefix_len: u32at(32) as usize,
        arena_len: u32at(36) as usize,
        max_name_len: u32at(40),
        hash: u64at(48),
        stamp,
    })
}

/// Validates the layout and extracts the offsets table.
///
/// The offsets are copied into owned memory even for a mapped index. That
/// costs about a millisecond for four megabytes and buys a soundness
/// property: no later slice bound depends on bytes that could change
/// underneath the mapping.
fn decode_common(bytes: &[u8], expect: Expect<'_>) -> Result<Decoded, LoadError> {
    let h = parse_header(bytes, expect)?;
    let s = sections(h.prefix_len, h.entry_count, h.arena_len);

    if bytes.len() != s.total {
        return Err(LoadError::Truncated);
    }

    let offsets_bytes = bytes
        .get(s.offsets..s.offsets + (h.entry_count + 1) * 4)
        .ok_or(LoadError::Truncated)?;

    // Verify before trusting a single offset.
    if hash64(&hashable(&bytes[0..HEADER_SIZE], offsets_bytes)) != h.hash {
        return Err(LoadError::ChecksumMismatch);
    }

    // `try_cast_slice`, never the panicking `cast_slice`: a panic while
    // loading a corrupt cache file would be a crash at startup.
    let offsets: Box<[u32]> = match bytemuck::try_cast_slice::<u8, u32>(offsets_bytes) {
        Ok(slice) => slice.to_vec().into_boxed_slice(),
        Err(_) => offsets_bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };

    let prefix_bytes = bytes
        .get(s.prefix..s.prefix + h.prefix_len)
        .ok_or(LoadError::Truncated)?;
    let prefix: Box<str> = String::from_utf8_lossy(prefix_bytes)
        .into_owned()
        .into_boxed_str();

    // The directory the index claims to describe must be the one the caller
    // asked for. Checked here, after the header hash has verified and before
    // any arena byte is touched, so both the mapped and the owned decoder get
    // it without duplication.
    //
    // Without this the crate had no way at all to notice a mapping being
    // repointed: the volume serial is unchanged when a directory moves within
    // the same drive, which is exactly what happened when the CustomPro root
    // became a subdirectory of the volume it already lived on.
    if !crate::util::winpath::same_dir(Path::new(prefix.as_ref()), expect.dir) {
        return Err(LoadError::PrefixMismatch {
            found: prefix.into_string(),
            expected: expect.dir.to_string_lossy().into_owned(),
        });
    }

    Ok((h, s, prefix, offsets))
}

fn decode_mapped(map: Mmap, expect: Expect<'_>) -> Result<Snapshot, LoadError> {
    let (h, s, prefix, offsets) = decode_common(&map, expect)?;
    let arenas = Arenas::Mapped {
        map,
        lower: s.lower..s.lower + h.arena_len,
        orig: s.orig..s.orig + h.arena_len,
    };
    finish(h, prefix, offsets, arenas)
}

fn decode_owned(bytes: &[u8], expect: Expect<'_>) -> Result<Snapshot, LoadError> {
    let (h, s, prefix, offsets) = decode_common(bytes, expect)?;
    let arenas = Arenas::Owned {
        lower: bytes[s.lower..s.lower + h.arena_len]
            .to_vec()
            .into_boxed_slice(),
        orig: bytes[s.orig..s.orig + h.arena_len]
            .to_vec()
            .into_boxed_slice(),
    };
    finish(h, prefix, offsets, arenas)
}

fn finish(
    h: Header,
    prefix: Box<str>,
    offsets: Box<[u32]>,
    arenas: Arenas,
) -> Result<Snapshot, LoadError> {
    if offsets.len() != h.entry_count + 1 {
        return Err(LoadError::Invalid(
            "offsets length disagrees with entry count",
        ));
    }
    let snapshot = Snapshot::try_from_parts(
        prefix,
        offsets,
        arenas,
        h.max_name_len,
        h.captured_at,
        h.volume_serial,
        h.stamp,
        h.flags & FLAG_TRUNCATED != 0,
    )
    .map_err(LoadError::Invalid)?;

    // Sampled rather than exhaustive: checking every terminator would touch
    // every page and defeat the lazy mapping. A missing separator can only
    // yield a wrong result, never unsafety, since all indexing goes through
    // offsets that have already been proven consistent.
    snapshot
        .sample_separators(1024)
        .map_err(LoadError::Invalid)?;
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::builder::SnapshotBuilder;

    /// A deep directory, deliberately: the flat root is a subdirectory of its
    /// volume, and the previous fixtures only ever exercised a drive root.
    const DIR: &str = r"V:\Documents\custpro";

    fn dir_path() -> &'static Path {
        Path::new(DIR)
    }

    fn key() -> MappingKey {
        MappingKey::of(dir_path())
    }

    fn expect(serial: Option<u32>) -> Expect<'static> {
        Expect::new(dir_path(), serial)
    }

    fn snap_at(prefix: &str, names: &[&str], serial: u32) -> Snapshot {
        let mut b = SnapshotBuilder::new(prefix);
        for n in names {
            b.push_str(n);
        }
        b.finish(SystemTime::now(), serial, Some(DirStamp::new(111, 222)))
    }

    fn snap(names: &[&str], serial: u32) -> Snapshot {
        snap_at(DIR, names, serial)
    }

    /// Snapshot has no PartialEq (arenas may be mapped), so compare errors.
    fn err(r: Result<Snapshot, LoadError>) -> LoadError {
        match r {
            Ok(s) => panic!("expected a rejection, got {} entries", s.len()),
            Err(e) => e,
        }
    }

    fn roundtrip(names: &[&str]) -> (tempfile::TempDir, Snapshot) {
        let dir = tempfile::tempdir().unwrap();
        let original = snap(names, 0xABCD_1234);
        save(dir.path(), key(), &original).unwrap();
        let loaded = load(dir.path(), key(), expect(Some(0xABCD_1234))).unwrap();
        (dir, loaded)
    }

    /// A serial of zero means "unknown", and it has to mean that on both
    /// sides. The writer records zero when the startup query lost its race
    /// with the SMB session warm-up; the reader used to call that a volume
    /// mismatch and throw the cache away, at random, on the next launch.
    #[test]
    fn a_snapshot_written_with_an_unknown_serial_is_still_accepted() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf"], 0)).unwrap();

        let loaded = load(dir.path(), key(), expect(Some(0xABCD_1234)))
            .expect("an unknown serial is unknown, not wrong");
        assert_eq!(loaded.len(), 1);
    }

    /// The other direction still has to be rejected, or a drive letter
    /// remapped to a different share serves the previous one's file list.
    #[test]
    fn two_known_but_different_serials_are_still_a_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf"], 0xAAAA)).unwrap();
        assert!(matches!(
            err(load(dir.path(), key(), expect(Some(0xBBBB)))),
            LoadError::VolumeMismatch { .. }
        ));
    }

    /// v2 hashed only bytes 0..48, leaving the stamp at 56..72 unprotected.
    /// A flipped bit there is silent and expensive: it reads as "the
    /// directory changed", which costs a full enumeration of a million-entry
    /// share on load, every load.
    #[test]
    fn a_corrupted_stamp_is_caught_by_the_checksum() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();

        let mut bytes = std::fs::read(&path).unwrap();
        bytes[57] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            err(load(dir.path(), key(), expect(Some(7)))),
            LoadError::ChecksumMismatch
        );
    }

    #[test]
    fn the_stamp_kind_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = SnapshotBuilder::new(DIR);
        b.push_str("a.pdf");
        let original = b.finish(SystemTime::now(), 7, Some(DirStamp::write_only(999)));
        save(dir.path(), key(), &original).unwrap();

        let loaded = load(dir.path(), key(), expect(Some(7))).unwrap();
        assert_eq!(
            loaded.stamp(),
            Some(DirStamp::write_only(999)),
            "a write-only stamp must not come back looking like a full one"
        );
    }

    /// Two instances index the same directory under the same key, so an
    /// unqualified sweep of `*.tmp` deletes the other one's file between its
    /// rename and its pointer write.
    #[test]
    fn gc_leaves_another_process_temp_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();

        let theirs = dir.path().join(format!("{}-deadbeef-1.tmp", key().hex()));
        std::fs::write(&theirs, b"in flight").unwrap();
        let ours = dir.path().join(format!("{}0.tmp", temp_prefix(key())));
        std::fs::write(&ours, b"ours").unwrap();

        gc(dir.path(), key(), "nothing-matches-this");

        assert!(theirs.exists(), "another process was still writing that");
        assert!(!ours.exists(), "our own leftovers are ours to collect");
    }

    /// An empty live set means the configuration could not be resolved, not
    /// "delete everything".
    #[test]
    fn orphan_collection_does_nothing_when_nothing_is_configured() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();
        let before = std::fs::read_dir(dir.path()).unwrap().count();

        gc_orphans(dir.path(), &[]);

        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            before,
            "an empty live set must not be read as a licence to wipe the cache"
        );
    }

    #[test]
    fn orphan_collection_keeps_every_configured_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let other = MappingKey::of(Path::new(r"S:rchive"));
        save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();
        save(dir.path(), other, &snap_at(r"S:rchive", &["b.pdf"], 7)).unwrap();

        gc_orphans(dir.path(), &[key(), other]);

        assert!(load(dir.path(), key(), expect(Some(7))).is_ok());
        assert!(
            load(
                dir.path(),
                other,
                Expect::new(Path::new(r"S:rchive"), Some(7))
            )
            .is_ok(),
            "a second configured mapping is not an orphan"
        );
    }

    #[test]
    fn round_trips_a_snapshot_exactly() {
        let names = ["Report_A1.PDF", "drawing_b.dwg", "Écoles-Été.pdf"];
        let (_dir, loaded) = roundtrip(&names);

        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.prefix(), DIR);
        assert_eq!(loaded.volume_serial(), 0xABCD_1234);
        assert_eq!(loaded.stamp(), Some(DirStamp::new(111, 222)));
        let got: Vec<String> = loaded.iter_names().map(|c| c.into_owned()).collect();
        assert_eq!(got, names);
        loaded.check_invariants().unwrap();
    }

    #[test]
    fn round_trips_the_folded_arena_so_matching_still_works() {
        let (_dir, loaded) = roundtrip(&["MiXeD_Case.TXT"]);
        assert_eq!(loaded.name_lower(0), b"mixed_case.txt");
        assert_eq!(loaded.name_orig(0), b"MiXeD_Case.TXT");
    }

    #[test]
    fn round_trips_an_empty_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let original = SnapshotBuilder::new(DIR).finish(SystemTime::now(), 7, None);
        save(dir.path(), key(), &original).unwrap();
        let loaded = load(dir.path(), key(), expect(Some(7))).unwrap();
        assert!(loaded.is_empty());
        assert_eq!(loaded.stamp(), None);
    }

    #[test]
    fn round_trips_a_large_snapshot() {
        let names: Vec<String> = (0..20_000)
            .map(|i| format!("job_{i:07}_report.pdf"))
            .collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let (_dir, loaded) = roundtrip(&refs);
        assert_eq!(loaded.len(), 20_000);
        assert_eq!(loaded.display_name(19_999), "job_0019999_report.pdf");
        loaded.check_invariants().unwrap();
    }

    #[test]
    fn a_missing_index_is_reported_as_missing_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            err(load(dir.path(), key(), expect(None))),
            LoadError::Missing
        );
    }

    /// The check that stops a remapped drive letter serving another share's
    /// file list.
    #[test]
    fn rejects_an_index_from_a_different_volume() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.txt"], 0x1111_1111)).unwrap();
        assert_eq!(
            err(load(dir.path(), key(), expect(Some(0x2222_2222)))),
            LoadError::VolumeMismatch {
                found: 0x1111_1111,
                expected: 0x2222_2222
            }
        );
    }

    #[test]
    fn accepts_an_index_when_the_volume_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.txt"], 0x1111_1111)).unwrap();
        assert!(load(dir.path(), key(), expect(None)).is_ok());
    }

    #[test]
    fn rejects_a_bad_magic() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["a.txt"], 1)).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[0] = b'X';
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(err(load_file(&path, expect(None))), LoadError::BadMagic);
    }

    #[test]
    fn rejects_a_future_format_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["a.txt"], 1)).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        bytes[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            err(load_file(&path, expect(None))),
            LoadError::VersionMismatch {
                found: FORMAT_VERSION + 1
            }
        );
    }

    #[test]
    fn rejects_a_truncated_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["aaa.txt", "bbb.txt"], 1)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 8]).unwrap();
        assert_eq!(err(load_file(&path, expect(None))), LoadError::Truncated);
    }

    #[test]
    fn rejects_a_file_shorter_than_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stub.idx");
        std::fs::write(&path, b"FILESIDX").unwrap();
        assert_eq!(err(load_file(&path, expect(None))), LoadError::TooSmall);
    }

    #[test]
    fn rejects_a_corrupted_offsets_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["aaa.txt", "bbb.txt"], 1)).unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let s = sections(3, 2, 16);
        bytes[s.offsets + 4] ^= 0xFF;
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            err(load_file(&path, expect(None))),
            LoadError::ChecksumMismatch
        );
    }

    #[test]
    fn rejects_an_index_that_is_too_old() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = SnapshotBuilder::new("V:\\");
        b.push_str("a.txt");
        let ancient = b.finish(UNIX_EPOCH + Duration::from_secs(1), 1, None);
        let path = save(dir.path(), key(), &ancient).unwrap();
        assert_eq!(err(load_file(&path, expect(None))), LoadError::TooOld);
    }

    #[test]
    fn a_corrupt_pointer_file_is_treated_as_missing() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.txt"], 1)).unwrap();
        std::fs::write(pointer_path(dir.path(), key()), r"..\..\escape.idx").unwrap();
        assert_eq!(
            err(load(dir.path(), key(), expect(None))),
            LoadError::Missing
        );
    }

    /// Rewriting must never rename over a file that could be mapped: that is
    /// `ERROR_SHARING_VIOLATION`, and it only shows up on the second run.
    #[test]
    fn saving_twice_while_the_first_index_is_still_mapped_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.txt"], 1)).unwrap();
        let held = load(dir.path(), key(), expect(Some(1))).unwrap();

        // The first index is live and mapped while the second is written.
        save(dir.path(), key(), &snap(&["a.txt", "b.txt"], 1)).unwrap();

        assert_eq!(held.len(), 1, "the mapped view stays valid");
        assert_eq!(load(dir.path(), key(), expect(Some(1))).unwrap().len(), 2);
    }

    #[test]
    fn superseded_index_files_are_collected() {
        let dir = tempfile::tempdir().unwrap();
        for i in 1..=4 {
            let names: Vec<String> = (0..i).map(|j| format!("f{j}.txt")).collect();
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            save(dir.path(), key(), &snap(&refs, 1)).unwrap();
        }
        let idx_files = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".idx"))
            .count();
        assert_eq!(idx_files, 1, "old indexes should not accumulate");
    }

    #[test]
    fn no_temporary_files_are_left_behind() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.txt"], 1)).unwrap();
        let tmps = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
            .count();
        assert_eq!(tmps, 0);
    }

    #[test]
    fn sections_are_aligned_and_non_overlapping() {
        let s = sections(3, 1000, 32_000);
        assert_eq!(s.prefix % ALIGN, 0);
        assert_eq!(s.offsets % ALIGN, 0);
        assert_eq!(s.lower % ALIGN, 0);
        assert_eq!(s.orig % ALIGN, 0);
        assert!(s.prefix + 3 <= s.offsets);
        assert!(s.offsets + 1001 * 4 <= s.lower);
        assert!(s.lower + 32_000 <= s.orig);
        assert_eq!(s.total, s.orig + 32_000);
    }

    #[test]
    fn the_owned_decoder_agrees_with_the_mapped_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = save(dir.path(), key(), &snap(&["one.txt", "two.pdf"], 5)).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let mapped = load_file(&path, expect(Some(5))).unwrap();
        let owned = decode_owned(&bytes, expect(Some(5))).unwrap();

        assert_eq!(mapped.offsets(), owned.offsets());
        assert_eq!(mapped.lower(), owned.lower());
        assert_eq!(mapped.orig(), owned.orig());
        assert_eq!(mapped.prefix(), owned.prefix());
    }

    // --- several indexed directories sharing one cache ---------------------

    const OTHER: &str = r"V:\Documents\archive";

    fn other_key() -> MappingKey {
        MappingKey::of(Path::new(OTHER))
    }

    fn other_expect(serial: Option<u32>) -> Expect<'static> {
        Expect::new(Path::new(OTHER), serial)
    }

    /// The collision the per-directory key exists to prevent.
    ///
    /// Both directories live on the same volume, so the volume serial is
    /// identical and cannot distinguish them - which is exactly the situation
    /// once the flat root became a subdirectory of a drive that may hold
    /// others.
    #[test]
    fn two_directories_on_one_volume_keep_separate_indexes() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf", "b.pdf"], 7)).unwrap();
        save(
            dir.path(),
            other_key(),
            &snap_at(OTHER, &["x.pdf", "y.pdf", "z.pdf"], 7),
        )
        .unwrap();

        let first = load(dir.path(), key(), expect(Some(7))).unwrap();
        let second = load(dir.path(), other_key(), other_expect(Some(7))).unwrap();

        assert_eq!(first.len(), 2);
        assert_eq!(first.prefix(), DIR);
        assert_eq!(second.len(), 3);
        assert_eq!(second.prefix(), OTHER);
    }

    #[test]
    fn saving_one_mapping_does_not_collect_anothers_index() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), other_key(), &snap_at(OTHER, &["x.pdf"], 7)).unwrap();

        // Several saves for the first mapping, each of which runs gc.
        for n in 1..=3 {
            let names: Vec<String> = (0..n).map(|i| format!("f{i}.pdf")).collect();
            let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
            save(dir.path(), key(), &snap(&refs, 7)).unwrap();
        }

        assert!(
            load(dir.path(), other_key(), other_expect(Some(7))).is_ok(),
            "the other mapping's index must survive"
        );
    }

    /// With one index actor per mapping, an unscoped temp sweep would delete a
    /// sibling's file mid-write.
    #[test]
    fn gc_does_not_delete_another_mappings_in_flight_temp() {
        let dir = tempfile::tempdir().unwrap();
        let foreign = dir
            .path()
            .join(format!("{}-1234-5678.tmp", other_key().hex()));
        std::fs::write(&foreign, b"half-written index").unwrap();

        save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();

        assert!(
            foreign.exists(),
            "another mapping's temp file was collected mid-write"
        );
    }

    /// The scenario that prompted all of this: the flat root moves from `V:\`
    /// to `V:\Documents\custpro`. The old index must not be served.
    #[test]
    fn a_repointed_mapping_does_not_find_the_old_directorys_index() {
        let dir = tempfile::tempdir().unwrap();
        let old_dir = Path::new(r"V:\");
        let old_key = MappingKey::of(old_dir);
        save(dir.path(), old_key, &snap_at(r"V:\", &["stale.pdf"], 7)).unwrap();

        // The mapping now points somewhere else. Same volume, same serial.
        assert_eq!(
            err(load(dir.path(), key(), expect(Some(7)))),
            LoadError::Missing,
            "a repointed mapping must not resolve the previous directory's index"
        );
    }

    /// The backstop, exercised by pointing one mapping's pointer at another's
    /// file - which is what a key collision would look like.
    #[test]
    fn an_index_describing_another_directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let foreign = save(dir.path(), other_key(), &snap_at(OTHER, &["x.pdf"], 7)).unwrap();

        let name = foreign.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::write(pointer_path(dir.path(), key()), &name).unwrap();

        match err(load(dir.path(), key(), expect(Some(7)))) {
            LoadError::PrefixMismatch { found, expected } => {
                assert_eq!(found, OTHER);
                assert_eq!(expected, DIR);
            }
            other => panic!("expected a directory mismatch, got {other:?}"),
        }
    }

    #[test]
    fn the_mismatch_message_names_both_directories() {
        let e = LoadError::PrefixMismatch {
            found: r"V:\".into(),
            expected: DIR.into(),
        };
        let msg = e.to_string();
        assert!(msg.contains(r"V:\"), "{msg}");
        assert!(msg.contains(DIR), "{msg}");
    }

    #[test]
    fn orphan_collection_keeps_live_mappings_and_removes_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), key(), &snap(&["a.pdf"], 7)).unwrap();
        save(dir.path(), other_key(), &snap_at(OTHER, &["x.pdf"], 7)).unwrap();

        // `OTHER` is dropped from the configuration.
        gc_orphans(dir.path(), &[key()]);

        assert!(load(dir.path(), key(), expect(Some(7))).is_ok());
        assert_eq!(
            err(load(dir.path(), other_key(), other_expect(Some(7)))),
            LoadError::Missing,
            "a mapping removed from the config should not keep its cache forever"
        );
    }

    /// The previous layout had no key in any filename and one global pointer.
    #[test]
    fn orphan_collection_removes_the_previous_layout() {
        let dir = tempfile::tempdir().unwrap();
        let stale_idx = dir.path().join("abcd1234-0000000000000001.idx");
        let stale_ptr = dir.path().join("latest");
        std::fs::write(&stale_idx, b"v1 index").unwrap();
        std::fs::write(&stale_ptr, b"abcd1234-0000000000000001.idx").unwrap();

        gc_orphans(dir.path(), &[key()]);

        assert!(!stale_idx.exists());
        assert!(!stale_ptr.exists());
    }

    #[test]
    fn the_cache_key_follows_the_directory_not_its_spelling() {
        assert_eq!(key(), MappingKey::of(Path::new(r"v:/documents/CUSTPRO/")));
        assert_ne!(key(), other_key());
        assert_ne!(key(), MappingKey::of(Path::new(r"V:\")));
    }

    #[test]
    fn hash_detects_single_bit_changes() {
        let a = hash64(b"the quick brown fox");
        let b = hash64(b"the quick brown fox!");
        let c = hash64(b"the quick brown foy");
        assert_ne!(a, b);
        assert_ne!(a, c);
    }
}
