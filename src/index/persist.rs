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
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use memmap2::Mmap;

use super::snapshot::{Arenas, Snapshot};
use super::store::TreeCoverage;
use super::tree::{Run, TreeIndex, TreeSegment};
use super::{DirStamp, StampKind};

const _: () = assert!(
    cfg!(target_endian = "little"),
    "the index format is little-endian"
);

pub const MAGIC: [u8; 8] = *b"FILESIDX";

/// Bumped to 2 when cache files became per-directory; to 3 when the directory
/// stamp came under the checksum and gained a kind; to 4 when a whole walked
/// tree became something this format can hold; to 5 when a tree could hold
/// directories nobody had enumerated.
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
pub const FORMAT_VERSION: u16 = 5;
/// 128 rather than 96, and that costs nothing: `align_up(96)` is already 128,
/// so v3 wrote exactly 32 bytes of padding here. Every section offset of a
/// flat index is byte-identical across the change.
pub const HEADER_SIZE: usize = 128;
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
/// Bit 4: this file holds a walked tree, not a single directory listing, so
/// the sections after the flat ones are present and must be read.
///
/// Checked in both directions. Without that a tree actor pointed at a flat
/// cache would decode the file happily - same magic, same version - and
/// publish one directory as if it were the whole share.
const FLAG_TREE: u32 = 1 << 4;
/// Bit 5: the walk that produced the tree reached the end of it. A partial
/// index is still worth caching, but the difference has to survive the round
/// trip: results missing because a subtree was unreadable look exactly like
/// results that do not exist.
const FLAG_TREE_COMPLETE: u32 = 1 << 5;
/// Bit 6: every directory in the tree was seen through a *search* rather than
/// read, which only a `kind = "live"` share produces.
///
/// A stronger claim than the absence of [`FLAG_TREE_COMPLETE`], and kept apart
/// from it for that reason: an incomplete walk still read every folder it
/// reached, whereas this says no pass covered any of them. Losing the
/// distinction across a restart would let a handful of remembered names load
/// back as if they were a listing, and "no matches" would start meaning
/// something it has never been allowed to mean here.
const FLAG_TREE_OBSERVED: u32 = 1 << 6;

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

/// Copies a little-endian `u32` table out of the file bytes.
///
/// `try_cast_slice`, never the panicking `cast_slice`: a panic while loading a
/// corrupt cache file would be a crash at startup. The unaligned fallback is
/// not theoretical either - a mapped file is page aligned, but an owned read
/// lands wherever the allocator put it.
fn read_u32s(bytes: &[u8]) -> Box<[u32]> {
    match bytemuck::try_cast_slice::<u8, u32>(bytes) {
        Ok(slice) => slice.to_vec().into_boxed_slice(),
        Err(_) => bytes
            .chunks_exact(4)
            .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    }
}

/// FNV-1a, in streaming form.
///
/// Streaming rather than "concatenate then hash" because a tree's covered
/// bytes run to about 24 MB across four separate tables, and building a
/// throwaway copy of them to feed a one-shot hash would double the peak
/// memory of a save for no benefit. Hashing the parts in order is
/// bit-identical to hashing their concatenation.
struct Fnv1a(u64);

impl Fnv1a {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= u64::from(b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01B3);
        }
    }

    fn finish(self) -> u64 {
        self.0
    }
}

/// FNV-1a over one slice. Only the tests hash a single buffer; every caller
/// in the program streams its tables through [`checksum`].
#[cfg(test)]
fn hash64(bytes: &[u8]) -> u64 {
    let mut h = Fnv1a::new();
    h.write(bytes);
    h.finish()
}

/// The checksum over the whole header except the checksum field itself,
/// followed by each index table in file order.
///
/// One function, used by both the writer and the reader, so the two cannot
/// disagree about the coverage - which is how the stamp came to sit outside
/// it in v2.
///
/// `tables` is every table whose corruption could be *silent*: the offsets,
/// and for a tree the directory offsets and the run table too. The name
/// arenas are deliberately excluded, and the asymmetry is the point. Bounds
/// safety already comes from `check_invariants`, which is strictly stronger
/// than a checksum; a corrupt arena byte can only garble a displayed name,
/// whereas a corrupt run would file a real file under a path it is not at -
/// a wrong answer with no symptom. Excluding the arenas is also what
/// preserves lazy mapping, so a load does not fault in 300 MB.
fn checksum(header: &[u8], tables: &[&[u8]]) -> u64 {
    let mut h = Fnv1a::new();
    h.write(&header[0..48]);
    h.write(&header[56..HEADER_SIZE]);
    for t in tables {
        h.write(t);
    }
    h.finish()
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

/// Byte offsets of a tree index's sections.
///
/// The flat sections come first and unchanged, so the file opens with exactly
/// the layout a flat index has; the directory listing, its offsets and the run
/// table follow. That ordering is deliberate: the tables the checksum covers
/// are the small ones, and keeping the two 150 MB arenas contiguous in the
/// middle is what lets them stay unread until a query touches them.
#[derive(Debug, Clone, Copy)]
struct TreeSections {
    root: usize,
    offsets: usize,
    lower: usize,
    orig: usize,
    dir_offsets: usize,
    dir_lower: usize,
    dir_orig: usize,
    runs: usize,
    total: usize,
}

fn tree_sections(
    root_len: usize,
    file_count: usize,
    arena_len: usize,
    dir_count: usize,
    dir_arena_len: usize,
    run_count: usize,
) -> TreeSections {
    let root = align_up(HEADER_SIZE);
    let offsets = align_up(root + root_len);
    let lower = align_up(offsets + (file_count + 1) * 4);
    let orig = align_up(lower + arena_len);
    let dir_offsets = align_up(orig + arena_len);
    let dir_lower = align_up(dir_offsets + (dir_count + 1) * 4);
    let dir_orig = align_up(dir_lower + dir_arena_len);
    let runs = align_up(dir_orig + dir_arena_len);
    TreeSections {
        root,
        offsets,
        lower,
        orig,
        dir_offsets,
        dir_lower,
        dir_orig,
        runs,
        total: runs + run_count * 8,
    }
}

// --- writing ---------------------------------------------------------------

/// Writes sections at fixed offsets, padding the gaps.
///
/// Extracted from `save` when the tree format arrived: eight sections written
/// by hand is eight chances to pad to the wrong place, and a section that
/// starts one byte early is a corruption the checksum reports but cannot
/// explain.
struct SectionWriter<'a> {
    f: &'a mut File,
    written: usize,
}

impl SectionWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        self.f.write_all(bytes)?;
        self.written += bytes.len();
        Ok(())
    }

    fn pad_to(&mut self, target: usize) -> std::io::Result<()> {
        debug_assert!(self.written <= target, "sections must not overlap");
        while self.written < target {
            let chunk = (target - self.written).min(ALIGN);
            self.f.write_all(&vec![0u8; chunk])?;
            self.written += chunk;
        }
        Ok(())
    }

    /// Pads to `target`, then writes.
    fn at(&mut self, target: usize, bytes: &[u8]) -> std::io::Result<()> {
        self.pad_to(target)?;
        self.write(bytes)
    }
}

/// Writes a file to a unique temporary, renames it into place, repoints this
/// mapping and collects what it superseded.
///
/// The atomic-rename dance is identical for a flat listing and for a tree, and
/// getting it subtly different in two places is how a half-written cache comes
/// to be pointed at. `body` returns the total size it meant to write, which is
/// checked against what it did.
fn commit<F>(
    dir: &Path,
    key: MappingKey,
    volume_serial: u32,
    captured_nanos: i64,
    body: F,
) -> Result<PathBuf, LoadError>
where
    F: FnOnce(&mut SectionWriter<'_>) -> std::io::Result<usize>,
{
    // A unique temp name in the same directory, so the rename is atomic and
    // never lands on an existing (possibly mapped) file.
    //
    // The temp name carries the mapping key too, and that is not cosmetic:
    // `gc` sweeps stale temporaries, so with one index actor per mapping an
    // unscoped sweep would delete a sibling mapping's temp file mid-write.
    let tmp = dir.join(format!(
        "{}{:x}.tmp",
        temp_prefix(key),
        crate::util::once::now_nanos()
    ));
    // The write time, not just the capture time, so every save is a distinct
    // file.
    //
    // A patched index deliberately keeps the capture time of the walk it grew
    // from - it really is that old, a few folders excepted - so naming the
    // file after the capture time alone meant a re-save landed on the name the
    // running process already had mapped. Renaming over a mapped file is the
    // `ERROR_SHARING_VIOLATION` this module's header is about, and it would
    // have bitten only on the second save of a session.
    let final_name = format!(
        "{}-{:08x}-{:016x}-{:016x}.idx",
        key.hex(),
        volume_serial,
        captured_nanos.max(0) as u64,
        crate::util::once::now_nanos()
    );
    let final_path = dir.join(&final_name);

    {
        let mut f = File::create(&tmp).map_err(|e| LoadError::Io(e.to_string()))?;
        (|| -> std::io::Result<()> {
            let mut w = SectionWriter {
                f: &mut f,
                written: 0,
            };
            let total = body(&mut w)?;
            debug_assert_eq!(w.written, total);
            // Required: without it a power loss can leave a renamed but
            // zero-length file, which the next start would have to reject.
            f.sync_all()
        })()
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            LoadError::Io(e.to_string())
        })?;
    }

    std::fs::rename(&tmp, &final_path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        LoadError::Io(e.to_string())
    })?;

    write_pointer(dir, key, &final_name)?;
    gc(dir, key, &final_name);
    Ok(final_path)
}

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
    let checksum = checksum(&header, &[&offsets_bytes]);
    header[48..56].copy_from_slice(&checksum.to_le_bytes());

    commit(dir, key, snapshot.volume_serial(), captured_nanos, |w| {
        w.write(&header)?;
        w.at(s.prefix, prefix)?;
        w.at(s.offsets, &offsets_bytes)?;
        w.at(s.lower, lower)?;
        w.at(s.orig, orig)?;
        Ok(s.total)
    })
}

/// Writes a whole walked tree to `dir`, returning the file written.
///
/// # One segment on disk
///
/// The index in memory is a list of segments because a walk has to publish
/// results while it is still running, and rebuilding a five-million-entry
/// snapshot on every publish would be quadratic. None of that applies to a
/// file: it is written once and read whole. So the segments are *concatenated*
/// into a single compacted segment as they are written.
///
/// Concatenation is all it takes, and that is a property of the arena layout
/// rather than a coincidence. Each arena is a run of NUL-terminated names, so
/// laying them end to end produces exactly the arena a single builder would
/// have produced; only the offset and run tables need shifting by the running
/// base. Nothing is copied, and the peak memory of a save is the 24 MB of
/// tables, not a second copy of the 300 MB of names.
pub fn save_tree(
    dir: &Path,
    key: MappingKey,
    index: &TreeIndex,
    coverage: Option<&TreeCoverage>,
) -> Result<PathBuf, LoadError> {
    std::fs::create_dir_all(dir).map_err(|e| LoadError::Io(e.to_string()))?;

    let root = index.root().as_bytes();
    let segments = index.segments();

    let file_count = index.len();
    let dir_count = index.dir_count();
    let run_count: usize = segments.iter().map(|s| s.runs().len()).sum();

    // The format addresses entries and arena bytes with `u32`. At the share
    // this was built for - 5M files, ~150 MB of names - there is an order of
    // magnitude of headroom, but refusing to write a file whose offsets would
    // wrap is the difference between "no cache this run" and a cache that
    // decodes into confident nonsense.
    let (offsets_bytes, arena_len) = flatten_offsets(segments.iter().map(|s| s.files()))?;
    let (dir_offsets_bytes, dir_arena_len) = flatten_offsets(segments.iter().map(|s| s.dirs()))?;
    if file_count > u32::MAX as usize || dir_count > u32::MAX as usize {
        return Err(LoadError::Invalid("tree index is too large to persist"));
    }

    let mut runs_bytes = Vec::with_capacity(run_count * 8);
    let mut file_base: u32 = 0;
    let mut dir_base: u32 = 0;
    for seg in segments {
        for r in seg.runs() {
            runs_bytes.extend_from_slice(&(file_base + r.first_file()).to_le_bytes());
            runs_bytes.extend_from_slice(&(dir_base + r.dir()).to_le_bytes());
        }
        file_base += seg.file_count();
        dir_base += seg.dir_count();
    }

    let s = tree_sections(
        root.len(),
        file_count,
        arena_len,
        dir_count,
        dir_arena_len,
        run_count,
    );

    let captured_nanos = index
        .captured_at()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);

    let mut flags = FLAG_NUL_SEPARATED | FLAG_TREE;
    if index.observed() {
        flags |= FLAG_TREE_OBSERVED;
    }
    if index.complete() {
        flags |= FLAG_TREE_COMPLETE;
    }

    let max_name_len = segments
        .iter()
        .map(|s| s.files().max_name_len())
        .max()
        .unwrap_or(0);
    let max_dir_len = segments
        .iter()
        .map(|s| s.dirs().max_name_len())
        .max()
        .unwrap_or(0);
    let avg_name_len = arena_len
        .checked_div(file_count)
        .map(|n| n.saturating_sub(1) as u32)
        .unwrap_or(0);

    let mut header = vec![0u8; HEADER_SIZE];
    header[0..8].copy_from_slice(&MAGIC);
    header[8..10].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    header[10..12].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
    header[12..16].copy_from_slice(&flags.to_le_bytes());
    header[16..20].copy_from_slice(&index.volume_serial().to_le_bytes());
    header[20..24].copy_from_slice(&(file_count as u32).to_le_bytes());
    header[24..32].copy_from_slice(&captured_nanos.to_le_bytes());
    header[32..36].copy_from_slice(&(root.len() as u32).to_le_bytes());
    header[36..40].copy_from_slice(&(arena_len as u32).to_le_bytes());
    header[40..44].copy_from_slice(&max_name_len.to_le_bytes());
    header[44..48].copy_from_slice(&avg_name_len.to_le_bytes());
    // 48..56 is the hash. 56..72 is the directory stamp, which a tree never
    // has: no single timestamp can stand for a whole share, and pretending
    // otherwise is what would make the scheduler claim the index was proven
    // fresh while a thousand files had been added underneath it.
    header[72..76].copy_from_slice(&(dir_count as u32).to_le_bytes());
    header[76..80].copy_from_slice(&(dir_arena_len as u32).to_le_bytes());
    header[80..84].copy_from_slice(&(run_count as u32).to_le_bytes());
    header[84..88].copy_from_slice(&max_dir_len.to_le_bytes());
    // What the walk could not reach, carried across the restart.
    //
    // Without these four numbers a share with an unreadable subtree would
    // come back from its cache looking perfectly healthy: the index is a
    // faithful copy of a partial walk, and nothing in it says so. The status
    // line would then be silent about exactly the failure this whole rewrite
    // exists to make visible, until the next re-walk half an hour later.
    //
    // The example paths are not persisted. They are a debugging aid, they
    // would need a section of their own, and the re-walk restores them.
    header[88..92].copy_from_slice(&coverage.map_or(0, |c| c.holes).to_le_bytes());
    header[92..96].copy_from_slice(&coverage.map_or(0, |c| c.vanished).to_le_bytes());
    header[96..100].copy_from_slice(&coverage.map_or(0, |c| c.skipped_junctions).to_le_bytes());
    header[100..104].copy_from_slice(
        &coverage
            .map_or(0, |c| {
                c.elapsed.as_millis().min(u128::from(u32::MAX)) as u32
            })
            .to_le_bytes(),
    );

    let sum = checksum(&header, &[&offsets_bytes, &dir_offsets_bytes, &runs_bytes]);
    header[48..56].copy_from_slice(&sum.to_le_bytes());

    commit(dir, key, index.volume_serial(), captured_nanos, |w| {
        w.write(&header)?;
        w.at(s.root, root)?;
        w.at(s.offsets, &offsets_bytes)?;
        w.pad_to(s.lower)?;
        for seg in segments {
            w.write(seg.files().lower())?;
        }
        w.pad_to(s.orig)?;
        for seg in segments {
            w.write(seg.files().orig())?;
        }
        w.at(s.dir_offsets, &dir_offsets_bytes)?;
        w.pad_to(s.dir_lower)?;
        for seg in segments {
            w.write(seg.dirs().lower())?;
        }
        w.pad_to(s.dir_orig)?;
        for seg in segments {
            w.write(seg.dirs().orig())?;
        }
        w.at(s.runs, &runs_bytes)?;
        Ok(s.total)
    })
}

/// Concatenates several snapshots' offset tables into one, shifting each by
/// the arena bytes before it.
///
/// Returns the table and the total arena length, which must agree with what
/// writing the arenas back to back produces - so they are computed by the same
/// loop rather than separately.
fn flatten_offsets<'a>(
    snapshots: impl Iterator<Item = &'a Snapshot>,
) -> Result<(Vec<u8>, usize), LoadError> {
    let mut out: Vec<u8> = Vec::new();
    let mut base: u64 = 0;
    for snap in snapshots {
        let offsets = snap.offsets();
        // The final entry is this snapshot's arena length, and it is the next
        // snapshot's first offset - so it is dropped here and re-emitted once,
        // at the end, as the table's terminator.
        let (last, rest) = offsets
            .split_last()
            .expect("a snapshot always has its initial zero");
        for &o in rest {
            out.extend_from_slice(&((base + u64::from(o)) as u32).to_le_bytes());
        }
        base += u64::from(*last);
        if base > u32::MAX as u64 {
            return Err(LoadError::Invalid("tree index is too large to persist"));
        }
    }
    out.extend_from_slice(&(base as u32).to_le_bytes());
    Ok((out, base as usize))
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
    load_file(&dir.join(read_pointer(dir, key)?), expect)
}

/// This mapping's current index file name.
///
/// The separator check is not paranoia about a hostile file: the pointer is
/// joined onto the cache directory, so a name containing one would read an
/// arbitrary path chosen by whatever wrote it.
fn read_pointer(dir: &Path, key: MappingKey) -> Result<String, LoadError> {
    let pointer =
        std::fs::read_to_string(pointer_path(dir, key)).map_err(|_| LoadError::Missing)?;
    let name = pointer.trim();
    if name.is_empty() || name.contains(['/', '\\']) {
        return Err(LoadError::Missing);
    }
    Ok(name.to_string())
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
        Ok(map) => decode_mapped(Arc::new(map), expect),
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
    /// Zero throughout for a flat index, whose bytes 72..88 are padding.
    dir_count: usize,
    dir_arena_len: usize,
    run_count: usize,
    max_dir_len: u32,
    holes: u32,
    vanished: u32,
    skipped_junctions: u32,
    elapsed_ms: u32,
}

impl Header {
    fn is_tree(&self) -> bool {
        self.flags & FLAG_TREE != 0
    }
}

fn parse_header(bytes: &[u8], expect: Expect<'_>, want_tree: bool) -> Result<Header, LoadError> {
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
    // Deliberately not rejected for age. See `MAX_INDEX_AGE`: an old index is
    // reported as stale and served, because discarding it leaves nothing to
    // serve and starts the very full pass the on-demand policy avoids.

    let stamp = if flags & FLAG_HAS_STAMP == 0 {
        None
    } else if flags & FLAG_STAMP_WRITE_ONLY != 0 {
        Some(DirStamp::write_only(i64at(56)))
    } else {
        Some(DirStamp::new(i64at(56), i64at(64)))
    };

    // Both directions. A flat actor reading a tree cache would decode one
    // segment's filenames with no directory table and serve them as if they
    // were a single folder; a tree actor reading a flat cache would publish
    // one directory as the whole share. Same magic, same version, so nothing
    // else catches it.
    if (flags & FLAG_TREE != 0) != want_tree {
        return Err(LoadError::Invalid(if want_tree {
            "cached index is a flat listing, not a tree"
        } else {
            "cached index is a tree, not a flat listing"
        }));
    }

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
        dir_count: u32at(72) as usize,
        dir_arena_len: u32at(76) as usize,
        run_count: u32at(80) as usize,
        max_dir_len: u32at(84),
        holes: u32at(88),
        vanished: u32at(92),
        skipped_junctions: u32at(96),
        elapsed_ms: u32at(100),
    })
}

/// Validates the layout and extracts the offsets table.
///
/// The offsets are copied into owned memory even for a mapped index. That
/// costs about a millisecond for four megabytes and buys a soundness
/// property: no later slice bound depends on bytes that could change
/// underneath the mapping.
fn decode_common(bytes: &[u8], expect: Expect<'_>) -> Result<Decoded, LoadError> {
    let h = parse_header(bytes, expect, false)?;
    let s = sections(h.prefix_len, h.entry_count, h.arena_len);

    if bytes.len() != s.total {
        return Err(LoadError::Truncated);
    }

    let offsets_bytes = bytes
        .get(s.offsets..s.offsets + (h.entry_count + 1) * 4)
        .ok_or(LoadError::Truncated)?;

    // Verify before trusting a single offset.
    if checksum(&bytes[0..HEADER_SIZE], &[offsets_bytes]) != h.hash {
        return Err(LoadError::ChecksumMismatch);
    }

    let offsets = read_u32s(offsets_bytes);

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

fn decode_mapped(map: Arc<Mmap>, expect: Expect<'_>) -> Result<Snapshot, LoadError> {
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

/// A persisted tree index and the walk report that produced it.
///
/// The two travel together because the index alone cannot say whether it is
/// complete coverage of the share or a partial view of one with an unreadable
/// subtree - and those look identical from the inside.
#[derive(Debug)]
pub struct LoadedTree {
    pub index: TreeIndex,
    pub coverage: TreeCoverage,
}

/// Loads this mapping's most recent tree index from `dir`.
pub fn load_tree(dir: &Path, key: MappingKey, expect: Expect<'_>) -> Result<LoadedTree, LoadError> {
    load_tree_file(&dir.join(read_pointer(dir, key)?), expect)
}

/// Loads a specific tree index file.
pub fn load_tree_file(path: &Path, expect: Expect<'_>) -> Result<LoadedTree, LoadError> {
    let file = open_shared_read(path)?;
    // SAFETY: see the module docs. The mapping is read-only, opened with a
    // share mode denying other writers, and no slice bound derived later
    // depends on the mapped bytes staying constant.
    match unsafe { Mmap::map(&file) } {
        Ok(map) => decode_tree(Arc::new(map), expect),
        Err(_) => {
            let bytes = std::fs::read(path).map_err(|e| LoadError::Io(e.to_string()))?;
            decode_tree_owned(&bytes, expect)
        }
    }
}

/// Header, layout and every table a tree index carries, validated.
struct DecodedTree {
    h: Header,
    s: TreeSections,
    root: Box<str>,
    offsets: Box<[u32]>,
    dir_offsets: Box<[u32]>,
    runs: Box<[Run]>,
}

/// Validates the layout and extracts the three tables the checksum covers.
///
/// As in the flat decoder, every table is copied into owned memory even for a
/// mapped index: no later slice bound may depend on bytes that could change
/// underneath the mapping. That is 24 MB at five million files, against the
/// 300 MB of arenas which stay mapped and unread.
fn decode_tree_common(bytes: &[u8], expect: Expect<'_>) -> Result<DecodedTree, LoadError> {
    let h = parse_header(bytes, expect, true)?;
    debug_assert!(h.is_tree());
    let s = tree_sections(
        h.prefix_len,
        h.entry_count,
        h.arena_len,
        h.dir_count,
        h.dir_arena_len,
        h.run_count,
    );

    if bytes.len() != s.total {
        return Err(LoadError::Truncated);
    }

    let offsets_bytes = bytes
        .get(s.offsets..s.offsets + (h.entry_count + 1) * 4)
        .ok_or(LoadError::Truncated)?;
    let dir_offsets_bytes = bytes
        .get(s.dir_offsets..s.dir_offsets + (h.dir_count + 1) * 4)
        .ok_or(LoadError::Truncated)?;
    let runs_bytes = bytes
        .get(s.runs..s.runs + h.run_count * 8)
        .ok_or(LoadError::Truncated)?;

    // Verify before trusting a single offset or run.
    if checksum(
        &bytes[0..HEADER_SIZE],
        &[offsets_bytes, dir_offsets_bytes, runs_bytes],
    ) != h.hash
    {
        return Err(LoadError::ChecksumMismatch);
    }

    let root_bytes = bytes
        .get(s.root..s.root + h.prefix_len)
        .ok_or(LoadError::Truncated)?;
    let root: Box<str> = String::from_utf8_lossy(root_bytes)
        .into_owned()
        .into_boxed_str();

    // The root the index claims to describe must be the one the caller asked
    // for, for exactly the reason the flat decoder checks its prefix: the
    // volume serial is unchanged when a mapping is repointed within a drive,
    // so without this a re-pointed tree mapping would serve the previous
    // share's five million paths.
    if !crate::util::winpath::same_dir(Path::new(root.as_ref()), expect.dir) {
        return Err(LoadError::PrefixMismatch {
            found: root.into_string(),
            expected: expect.dir.to_string_lossy().into_owned(),
        });
    }

    let runs: Box<[Run]> = runs_bytes
        .chunks_exact(8)
        .map(|c| {
            Run::new(
                u32::from_le_bytes(c[0..4].try_into().unwrap()),
                u32::from_le_bytes(c[4..8].try_into().unwrap()),
            )
        })
        .collect::<Vec<_>>()
        .into_boxed_slice();

    Ok(DecodedTree {
        h,
        s,
        root,
        offsets: read_u32s(offsets_bytes),
        dir_offsets: read_u32s(dir_offsets_bytes),
        runs,
    })
}

fn decode_tree(map: Arc<Mmap>, expect: Expect<'_>) -> Result<LoadedTree, LoadError> {
    let d = decode_tree_common(&map, expect)?;
    // One `Mmap` behind both arenas of both snapshots. Mapping the file four
    // times would work and would be a waste; more importantly each mapping is
    // an independent kernel object, so the filenames and the directory names
    // of one index could then be backed by different views of the same bytes.
    let files = Arenas::Mapped {
        map: Arc::clone(&map),
        lower: d.s.lower..d.s.lower + d.h.arena_len,
        orig: d.s.orig..d.s.orig + d.h.arena_len,
    };
    let dirs = Arenas::Mapped {
        map,
        lower: d.s.dir_lower..d.s.dir_lower + d.h.dir_arena_len,
        orig: d.s.dir_orig..d.s.dir_orig + d.h.dir_arena_len,
    };
    finish_tree(d, files, dirs)
}

fn decode_tree_owned(bytes: &[u8], expect: Expect<'_>) -> Result<LoadedTree, LoadError> {
    let d = decode_tree_common(bytes, expect)?;
    let owned = |r: std::ops::Range<usize>| bytes[r].to_vec().into_boxed_slice();
    let files = Arenas::Owned {
        lower: owned(d.s.lower..d.s.lower + d.h.arena_len),
        orig: owned(d.s.orig..d.s.orig + d.h.arena_len),
    };
    let dirs = Arenas::Owned {
        lower: owned(d.s.dir_lower..d.s.dir_lower + d.h.dir_arena_len),
        orig: owned(d.s.dir_orig..d.s.dir_orig + d.h.dir_arena_len),
    };
    finish_tree(d, files, dirs)
}

fn finish_tree(d: DecodedTree, files: Arenas, dirs: Arenas) -> Result<LoadedTree, LoadError> {
    let DecodedTree {
        h,
        root,
        offsets,
        dir_offsets,
        runs,
        ..
    } = d;

    if offsets.len() != h.entry_count + 1 {
        return Err(LoadError::Invalid(
            "offsets length disagrees with entry count",
        ));
    }
    if dir_offsets.len() != h.dir_count + 1 {
        return Err(LoadError::Invalid(
            "directory offsets length disagrees with directory count",
        ));
    }

    // A segment's snapshots carry placeholders for the timestamps and the
    // serial, exactly as `SegmentBuilder::seal` leaves them: those belong to
    // the index as a whole, and storing them twice is storing them
    // inconsistently.
    let part = |offsets: Box<[u32]>, arenas: Arenas, max: u32| {
        Snapshot::try_from_parts(
            "".into(),
            offsets,
            arenas,
            max,
            SystemTime::UNIX_EPOCH,
            0,
            None,
            false,
        )
        .map_err(LoadError::Invalid)
    };
    let files = part(offsets, files, h.max_name_len)?;
    let dirs = part(dir_offsets, dirs, h.max_dir_len)?;

    // Sampled rather than exhaustive, as in the flat decoder: checking every
    // terminator would touch every page and defeat the lazy mapping.
    files.sample_separators(1024).map_err(LoadError::Invalid)?;
    dirs.sample_separators(1024).map_err(LoadError::Invalid)?;

    // The run table is checked in full, and that asymmetry is deliberate. A
    // bad arena byte garbles a name on screen; a run pointing one directory
    // to the left reports a real file under a path it is not at, which is a
    // wrong answer nobody can tell is wrong.
    let segment = TreeSegment::from_parts(files, dirs, runs).map_err(LoadError::Invalid)?;

    let index = TreeIndex::empty(&root)
        .appended(Arc::new(segment))
        .with_metadata(h.captured_at, h.volume_serial)
        .with_complete(h.flags & FLAG_TREE_COMPLETE != 0)
        .with_observed(h.flags & FLAG_TREE_OBSERVED != 0);

    let coverage = TreeCoverage {
        dirs: index.dir_count(),
        files: index.len(),
        holes: h.holes,
        vanished: h.vanished,
        // Not persisted: a debugging aid that would need a section of its
        // own, and the next re-walk restores it.
        examples: Vec::new(),
        skipped_junctions: h.skipped_junctions,
        elapsed: Duration::from_millis(u64::from(h.elapsed_ms)),
    };

    Ok(LoadedTree { index, coverage })
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

    /// An old index is served, not discarded.
    ///
    /// This used to reject. Rejecting leaves nothing to serve, which starts a
    /// full pass - and with every client's cache written on the same rollout
    /// day, they would all have started one within hours of each other. The
    /// age is reported instead; see `MAX_INDEX_AGE`.
    #[test]
    fn an_index_that_is_too_old_is_served_rather_than_discarded() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = SnapshotBuilder::new(&dir_path().to_string_lossy());
        b.push_str("a.txt");
        let ancient = b.finish(UNIX_EPOCH + Duration::from_secs(1), 1, None);
        let path = save(dir.path(), key(), &ancient).unwrap();
        let loaded = load_file(&path, expect(None)).expect("an old index still loads");
        assert_eq!(loaded.len(), 1);
        assert!(
            SystemTime::now()
                .duration_since(loaded.captured_at())
                .is_ok_and(|age| age > crate::config::MAX_INDEX_AGE),
            "and it still reports how old it is"
        );
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

/// The tree layout, which shares a header and an atomic-rename path with the
/// flat one but almost nothing else.
///
/// The property under test throughout is that a *segmented* index in memory
/// becomes a *single compacted* segment on disk and comes back describing the
/// same files at the same paths. Compaction is done by concatenating arenas
/// and shifting tables, so if the shift is ever off by one the paths are the
/// symptom - not a crash, which is exactly why it is checked by path and not
/// by count.
#[cfg(test)]
mod tree_tests {
    use super::*;
    use crate::index::store::TreeCoverage;
    use crate::index::tree::{SegmentBuilder, TreeIndex};

    const ROOT: &str = r"R:\";

    fn root_path() -> &'static Path {
        Path::new(ROOT)
    }

    fn tree_key() -> MappingKey {
        MappingKey::of(root_path())
    }

    fn tree_expect(serial: Option<u32>) -> Expect<'static> {
        Expect::new(root_path(), serial)
    }

    /// One segment's worth of directories.
    type Group<'a> = &'a [(&'a str, &'a [&'a str])];

    /// Builds an index whose segments are sealed exactly where `groups` says.
    ///
    /// The split points are given explicitly rather than left to the sink's
    /// size ladder, because the whole point is to exercise more than one
    /// segment without building a share large enough to trigger a real seal.
    fn tree_of(groups: &[Group<'_>]) -> TreeIndex {
        let mut index = TreeIndex::empty(ROOT);
        for group in groups {
            let mut b = SegmentBuilder::new();
            for (dir, files) in *group {
                let owned: Vec<String> = files.iter().map(|f| (*f).to_string()).collect();
                assert!(b.push_dir(dir, &owned), "the fixture must fit one segment");
            }
            index = index.appended(Arc::new(b.seal()));
        }
        index
            .with_metadata(SystemTime::now(), 0xABCD_1234)
            .with_complete(true)
    }

    /// Every file the index holds, by full path, in ordinal order.
    fn paths(index: &TreeIndex) -> Vec<String> {
        (0..index.len() as u32)
            .map(|i| index.full_path(i).expect("every ordinal has a path"))
            .collect()
    }

    /// Three segments, a directory with no files, a file straight in the root,
    /// and a nested path - the four shapes whose run and offset arithmetic
    /// differs.
    fn fixture() -> TreeIndex {
        tree_of(&[
            &[
                ("", &["readme.txt"] as &[&str]),
                ("11d", &["quote.pdf", "drawing.dwg"]),
            ],
            &[
                ("11d\\sub", &["nested.pdf"]),
                ("empty folder", &[] as &[&str]),
            ],
            &[(
                "archive\\2019\\odd name",
                &["11-3-0704 survey.pdf", "notes.txt"],
            )],
        ])
    }

    fn save_fixture(index: &TreeIndex, coverage: Option<&TreeCoverage>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        save_tree(dir.path(), tree_key(), index, coverage).unwrap();
        dir
    }

    /// FORMAT_VERSION 5 exists for this one bit. Lose it across a restart and
    /// a handful of remembered names load back as if they were a listing -
    /// after which "no matches" starts meaning something it has never been
    /// allowed to mean here.
    #[test]
    fn an_observed_tree_is_still_observed_after_a_round_trip() {
        let original = TreeIndex::empty(r"R:\")
            .with_observed_files(&[("p12345".to_string(), vec!["a.pdf".to_string()])])
            .expect("something was new")
            .with_metadata(std::time::SystemTime::UNIX_EPOCH, 0xABCD_1234);
        assert!(original.observed());

        let dir = save_fixture(&original, None);
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(Some(0xABCD_1234))).unwrap();

        assert!(loaded.index.observed(), "the observed bit was lost");
        assert!(!loaded.index.complete());
        assert_eq!(paths(&loaded.index), paths(&original));
    }

    /// The dual, and the one that would fail silently: a walked tree must not
    /// come back claiming it was only ever searched.
    #[test]
    fn a_walked_tree_is_not_observed_after_a_round_trip() {
        let original = fixture();
        assert!(!original.observed());

        let dir = save_fixture(&original, None);
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap();

        assert!(!loaded.index.observed());
    }

    #[test]
    fn a_segmented_tree_round_trips_as_one_compacted_segment() {
        let original = fixture();
        assert_eq!(
            original.segments().len(),
            3,
            "the fixture must be segmented"
        );

        let dir = save_fixture(&original, None);
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(Some(0xABCD_1234))).unwrap();

        assert_eq!(
            loaded.index.segments().len(),
            1,
            "segments are a streaming concern, not a storage one"
        );
        assert_eq!(paths(&loaded.index), paths(&original));
        assert_eq!(loaded.index.len(), original.len());
        assert_eq!(loaded.index.dir_count(), original.dir_count());
        assert_eq!(loaded.index.root(), original.root());
        assert_eq!(loaded.index.volume_serial(), 0xABCD_1234);
        assert_eq!(loaded.index.captured_at(), original.captured_at());
        assert!(loaded.index.complete());
    }

    /// The reason the directory dimension is persisted at all: a job code
    /// usually names a folder, not a file.
    #[test]
    fn a_folder_still_owns_its_files_after_a_round_trip() {
        let dir = save_fixture(&fixture(), None);
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap();

        let seg = &loaded.index.segments()[0];
        let folder = (0..seg.dir_count())
            .find(|d| seg.dir_path(*d) == "11d")
            .expect("the folder survived");

        let files: Vec<String> = seg
            .files_of(folder)
            .map(|i| seg.files().display_name(i).into_owned())
            .collect();
        assert_eq!(files, vec!["quote.pdf", "drawing.dwg"]);
    }

    #[test]
    fn an_empty_tree_round_trips() {
        let index = TreeIndex::empty(ROOT).with_metadata(SystemTime::now(), 7);
        let dir = save_fixture(&index, None);
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(Some(7))).unwrap();
        assert_eq!(loaded.index.len(), 0);
        assert!(!loaded.index.complete());
    }

    /// A partial walk that is cached must still look partial after a restart.
    /// Otherwise the one restart between a walk and its re-walk is a window in
    /// which the status line claims coverage the index never had.
    #[test]
    fn an_incomplete_walks_coverage_survives_the_restart() {
        let index = tree_of(&[&[("11d", &["a.pdf"] as &[&str])]]).with_complete(false);
        let coverage = TreeCoverage {
            dirs: 900,
            files: 1,
            holes: 4,
            vanished: 11,
            examples: vec!["restricted".into()],
            skipped_junctions: 2,
            elapsed: Duration::from_millis(1234),
        };

        let dir = save_fixture(&index, Some(&coverage));
        let loaded = load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap();

        assert!(!loaded.index.complete());
        assert_eq!(loaded.coverage.holes, 4);
        assert_eq!(loaded.coverage.vanished, 11);
        assert_eq!(loaded.coverage.skipped_junctions, 2);
        assert_eq!(loaded.coverage.elapsed, Duration::from_millis(1234));
        assert!(
            loaded.coverage.examples.is_empty(),
            "the example paths are deliberately not persisted"
        );
    }

    /// Same magic, same version, same key length - so nothing but the flag
    /// stands between a tree actor and one directory served as a whole share.
    #[test]
    fn a_flat_index_is_not_readable_as_a_tree() {
        let dir = tempfile::tempdir().unwrap();
        let mut b = crate::index::builder::SnapshotBuilder::new(ROOT);
        b.push_str("a.pdf");
        let flat = b.finish(SystemTime::now(), 7, None);
        save(dir.path(), tree_key(), &flat).unwrap();

        assert_eq!(
            load_tree(dir.path(), tree_key(), tree_expect(Some(7))).unwrap_err(),
            LoadError::Invalid("cached index is a flat listing, not a tree")
        );
    }

    #[test]
    fn a_tree_is_not_readable_as_a_flat_index() {
        let dir = save_fixture(&fixture(), None);
        assert_eq!(
            load(dir.path(), tree_key(), tree_expect(Some(0xABCD_1234))).unwrap_err(),
            LoadError::Invalid("cached index is a tree, not a flat listing")
        );
    }

    #[test]
    fn a_tree_written_for_another_root_is_refused() {
        let dir = save_fixture(&fixture(), None);
        let other = Path::new(r"S:\");
        assert!(matches!(
            load_tree_file(
                &dir.path()
                    .join(read_pointer(dir.path(), tree_key()).unwrap()),
                Expect::new(other, None),
            )
            .unwrap_err(),
            LoadError::PrefixMismatch { .. }
        ));
    }

    /// The run table is last in the file, so the final byte is inside it.
    ///
    /// This is the corruption the checksum exists for. A bad arena byte
    /// garbles a name on screen; a bad run reports a real file under a path it
    /// is not at, which is a wrong answer nobody can tell is wrong.
    #[test]
    fn a_corrupt_run_table_is_rejected() {
        let dir = save_fixture(&fixture(), None);
        let path = dir
            .path()
            .join(read_pointer(dir.path(), tree_key()).unwrap());

        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap_err(),
            LoadError::ChecksumMismatch
        );
    }

    /// A run that survives the checksum but points at a directory that is not
    /// there must still be refused, because the checksum only proves the bytes
    /// are the ones that were written - not that they meant anything.
    #[test]
    fn a_run_naming_a_missing_directory_is_rejected() {
        let index = fixture();
        let dir = save_fixture(&index, None);
        let path = dir
            .path()
            .join(read_pointer(dir.path(), tree_key()).unwrap());

        let mut bytes = std::fs::read(&path).unwrap();
        let total = bytes.len();
        let run_count: usize = index.segments().iter().map(|s| s.runs().len()).sum();
        let runs_at = total - run_count * 8;

        // The `dir` field of the *last* run, pushed past the end of the
        // directory listing. The last one specifically, so the run table is
        // still ascending and it is the range check that rejects it rather
        // than the ordering check.
        let at = runs_at + (run_count - 1) * 8;
        bytes[at + 4..at + 8].copy_from_slice(&9_999u32.to_le_bytes());

        // Re-checksum, so the structural check is what rejects it rather than
        // the corruption detector.
        let h = parse_header(&bytes, tree_expect(None), true).unwrap();
        let layout = tree_sections(
            h.prefix_len,
            h.entry_count,
            h.arena_len,
            h.dir_count,
            h.dir_arena_len,
            h.run_count,
        );
        let offsets = bytes[layout.offsets..layout.offsets + (h.entry_count + 1) * 4].to_vec();
        let dir_offsets =
            bytes[layout.dir_offsets..layout.dir_offsets + (h.dir_count + 1) * 4].to_vec();
        let runs = bytes[layout.runs..layout.runs + h.run_count * 8].to_vec();
        let sum = checksum(&bytes[0..HEADER_SIZE], &[&offsets, &dir_offsets, &runs]);
        bytes[48..56].copy_from_slice(&sum.to_le_bytes());
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(
            load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap_err(),
            LoadError::Invalid("a run names a directory that does not exist")
        );
    }

    #[test]
    fn a_missing_tree_cache_is_missing_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            load_tree(dir.path(), tree_key(), tree_expect(None)).unwrap_err(),
            LoadError::Missing
        );
    }

    /// A tree file must obey the same alignment rule every section of the flat
    /// format does, or a mapped `u32` table is read unaligned on the fallback
    /// path and byte-by-byte on the fast one.
    #[test]
    fn tree_sections_are_aligned_and_ordered() {
        let s = tree_sections(3, 1_000, 24_000, 90, 1_800, 90);
        for offset in [
            s.root,
            s.offsets,
            s.lower,
            s.orig,
            s.dir_offsets,
            s.dir_lower,
            s.dir_orig,
            s.runs,
        ] {
            assert_eq!(offset % ALIGN, 0, "section at {offset} is unaligned");
        }
        assert!(s.root >= HEADER_SIZE);
        assert!(s.offsets >= s.root + 3);
        assert!(s.lower >= s.offsets + 1_001 * 4);
        assert!(s.orig >= s.lower + 24_000);
        assert!(s.dir_offsets >= s.orig + 24_000);
        assert!(s.dir_lower >= s.dir_offsets + 91 * 4);
        assert!(s.dir_orig >= s.dir_lower + 1_800);
        assert!(s.runs >= s.dir_orig + 1_800);
        assert_eq!(s.total, s.runs + 90 * 8);
    }

    /// Growing the header from 96 to 128 bytes had to be free, and this is the
    /// arithmetic that makes it so: both round up to the same alignment, so no
    /// flat section moved.
    #[test]
    fn the_larger_header_moved_no_flat_section() {
        assert_eq!(align_up(96), align_up(HEADER_SIZE));
        assert_eq!(align_up(HEADER_SIZE), 128);
        assert_eq!(sections(0, 0, 0).prefix, 128);
    }
}
