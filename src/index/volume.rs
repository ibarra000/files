//! Volume interrogation: drive type, serial number, filesystem flags, and
//! the SMB dialect in use.
//!
//! This is what `--doctor` reports, and it is how several decisions get made
//! at runtime instead of being guessed at build time:
//!
//! * The **volume serial** validates the persisted index. Without it, a drive
//!   letter remapped to a different share would serve the previous mapping's
//!   file list.
//! * **`FILE_CASE_SENSITIVE_SEARCH`** is reported but deliberately not acted
//!   on. It says the filesystem *supports* case-sensitive names, which NTFS
//!   always does, not that lookups resolve case-sensitively - so branching on
//!   it would disable server-side filtering on every ordinary volume.
//! * The **SMB dialect** determines the negotiated maximum transfer size, and
//!   therefore whether large directory buffers can help at all. On SMB 2.0.2
//!   and some NAS firmware the cap is 64 KiB, which collapses the 1 MiB
//!   strategy back onto the 64 KiB one.

use std::path::Path;

use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::NetworkManagement::WNet::WNetGetConnectionW;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_REMOTE_PROTOCOL_INFO, FILE_SHARE_DELETE,
    FILE_SHARE_READ, FILE_SHARE_WRITE, FileRemoteProtocolInfo, GetDriveTypeW,
    GetFileInformationByHandleEx, GetVolumeInformationW, OPEN_EXISTING,
};
use windows_sys::Win32::System::WindowsProgramming::{
    DRIVE_CDROM, DRIVE_FIXED, DRIVE_NO_ROOT_DIR, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
};

use super::win_util::wide_path;
use crate::util::winpath;

/// Filesystem flag: the volume supports case-sensitive filename lookup.
/// Not exported by `windows-sys`, so it is spelled out here.
pub const FILE_CASE_SENSITIVE_SEARCH: u32 = 0x0000_0001;

/// `WNNC_NET_SMB`, the protocol id reported for an SMB share.
pub const WNNC_NET_SMB: u32 = 0x0002_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Unknown,
    NoRootDir,
    Removable,
    Fixed,
    Remote,
    CdRom,
    RamDisk,
    Other(u32),
}

impl DriveKind {
    fn from_raw(raw: u32) -> Self {
        match raw {
            DRIVE_UNKNOWN_RAW => Self::Unknown,
            DRIVE_NO_ROOT_DIR => Self::NoRootDir,
            DRIVE_REMOVABLE => Self::Removable,
            DRIVE_FIXED => Self::Fixed,
            DRIVE_REMOTE => Self::Remote,
            DRIVE_CDROM => Self::CdRom,
            DRIVE_RAMDISK => Self::RamDisk,
            other => Self::Other(other),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::Unknown => "DRIVE_UNKNOWN".into(),
            Self::NoRootDir => "DRIVE_NO_ROOT_DIR".into(),
            Self::Removable => "DRIVE_REMOVABLE".into(),
            Self::Fixed => "DRIVE_FIXED (local)".into(),
            Self::Remote => "DRIVE_REMOTE (network)".into(),
            Self::CdRom => "DRIVE_CDROM".into(),
            Self::RamDisk => "DRIVE_RAMDISK".into(),
            Self::Other(v) => format!("unrecognised ({v})"),
        }
    }

    /// Whether a volume handle (`\\.\X:`) could conceivably be opened.
    ///
    /// Only meaningful for MFT-style enumeration, which this crate does not
    /// implement: both target drives are network shares, where no MFT is
    /// exposed. `--doctor` reports this so the conclusion is visible rather
    /// than assumed.
    pub fn supports_volume_handle(self) -> bool {
        matches!(self, Self::Fixed | Self::RamDisk)
    }
}

const DRIVE_UNKNOWN_RAW: u32 = 0;

/// What is known about one root.
#[derive(Debug, Clone, Default)]
pub struct VolumeInfo {
    /// The directory as configured, which may be a subdirectory of the volume.
    pub root: String,
    /// The volume root the volume-level queries were actually issued against.
    pub volume_root: Option<String>,
    /// True when `root` is deeper than its volume root, so `--doctor` can say
    /// "subdirectory of V:\" rather than reporting a bare failure.
    pub is_subdirectory: bool,
    pub kind: Option<DriveKind>,
    pub unc_target: Option<String>,
    pub filesystem: Option<String>,
    pub volume_serial: Option<u32>,
    pub max_component_len: Option<u32>,
    pub filesystem_flags: Option<u32>,
    pub remote_protocol: Option<RemoteProtocol>,
    /// Populated when the volume could not be interrogated.
    pub error: Option<u32>,
}

impl VolumeInfo {
    /// Whether the volume reports `FILE_CASE_SENSITIVE_SEARCH`.
    ///
    /// **Informational only.** The flag means the filesystem *supports*
    /// case-sensitive names, not that lookups are performed case-sensitively:
    /// NTFS sets it on every volume while Win32 still resolves names
    /// case-insensitively. Treating it as a disqualifier for server-side
    /// filtering - as an earlier version of this code did - switched the
    /// feature off everywhere and made `--doctor` raise an alarm about an
    /// entirely ordinary drive.
    ///
    /// The question that actually matters, "does this server's pattern
    /// matching agree with ours", is answered behaviourally: by the
    /// superset audit in [`crate::search::verify`] and by `--bench`.
    pub fn case_sensitive_search(&self) -> bool {
        self.filesystem_flags
            .is_some_and(|f| f & FILE_CASE_SENSITIVE_SEARCH != 0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteProtocol {
    pub protocol: u32,
    pub major: u16,
    pub minor: u16,
    pub revision: u16,
    pub flags: u32,
}

impl RemoteProtocol {
    pub fn is_smb(self) -> bool {
        self.protocol == WNNC_NET_SMB
    }

    pub fn dialect(self) -> String {
        if self.is_smb() {
            format!("SMB {}.{}.{}", self.major, self.minor, self.revision)
        } else {
            format!("protocol 0x{:08X}", self.protocol)
        }
    }

    /// Best guess at whether the negotiated maximum transfer size allows
    /// directory buffers above 64 KiB.
    ///
    /// SMB 2.0.2 caps at 64 KiB; later dialects normally negotiate 1 MiB, but
    /// a given server may still cap lower - the enumerator halves its buffer
    /// adaptively rather than trusting this.
    pub fn likely_large_transactions(self) -> bool {
        self.is_smb() && (self.major > 2 || (self.major == 2 && self.minor >= 1))
    }
}

/// Reads the drive type of the volume containing `path`.
///
/// `GetDriveTypeW` answers `DRIVE_NO_ROOT_DIR` for anything that is not a
/// volume root, so a configured subdirectory such as `V:\Documents\custpro`
/// would otherwise stop being recognised as a network share. The volume root
/// is derived first.
pub fn drive_kind(path: &Path) -> DriveKind {
    let Some(root) = winpath::volume_root_of(path) else {
        return DriveKind::NoRootDir;
    };
    let wide = wide_path(&root, true);
    // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call,
    // and GetDriveTypeW only reads it.
    DriveKind::from_raw(unsafe { GetDriveTypeW(wide.as_ptr()) })
}

/// Resolves the mapped drive containing `path` to its UNC target.
pub fn unc_target(path: &Path) -> Option<String> {
    // WNetGetConnectionW wants a bare device name such as `V:` - not a path.
    // Trimming trailing separators is not enough once the configured root is
    // a subdirectory.
    let letter = winpath::drive_letter_of(path)?;
    let wide = wide_nul(&letter);

    let mut buf = vec![0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: `wide` is NUL-terminated; `buf`/`len` describe a writable buffer
    // of exactly `len` u16s, which is what the API contract requires.
    let rc = unsafe { WNetGetConnectionW(wide.as_ptr(), buf.as_mut_ptr(), &mut len) };
    if rc != 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Interrogates the volume containing `path`, degrading field by field.
///
/// `path` is the *configured* directory, which may be a subdirectory. Every
/// volume-level query is issued against its volume root instead, because
/// `GetVolumeInformationW` requires one and fails with `ERROR_DIR_NOT_ROOT`
/// otherwise - taking the volume serial, the filesystem flags, and therefore
/// the persisted index's identity check down with it.
pub fn volume_info(path: &Path) -> VolumeInfo {
    let volume_root = winpath::volume_root_of(path);
    let mut info = VolumeInfo {
        root: path.to_string_lossy().into_owned(),
        volume_root: volume_root
            .as_ref()
            .map(|r| r.to_string_lossy().into_owned()),
        is_subdirectory: !winpath::is_volume_root(path),
        ..Default::default()
    };

    info.kind = Some(drive_kind(path));
    info.unc_target = unc_target(path);

    let Some(volume_root) = volume_root else {
        // Nothing volume-shaped to interrogate - a relative path, or a UNC
        // path with no share component.
        info.remote_protocol = remote_protocol(path);
        return info;
    };
    let wide = wide_path(&volume_root, true);
    let mut name_buf = [0u16; 256];
    let mut fs_buf = [0u16; 256];
    let mut serial = 0u32;
    let mut max_component = 0u32;
    let mut flags = 0u32;

    // SAFETY: all four out-parameters are valid, correctly aligned locals, and
    // both buffers are described by their true element counts.
    let ok = unsafe {
        GetVolumeInformationW(
            wide.as_ptr(),
            name_buf.as_mut_ptr(),
            name_buf.len() as u32,
            &mut serial,
            &mut max_component,
            &mut flags,
            fs_buf.as_mut_ptr(),
            fs_buf.len() as u32,
        )
    };

    if ok == 0 {
        info.error = Some(last_error());
    } else {
        info.volume_serial = Some(serial);
        info.max_component_len = Some(max_component);
        info.filesystem_flags = Some(flags);
        let end = fs_buf.iter().position(|&c| c == 0).unwrap_or(fs_buf.len());
        info.filesystem = Some(String::from_utf16_lossy(&fs_buf[..end]));
    }

    // Deliberately the configured directory, not the volume root: this one
    // goes through CreateFileW, which is happy with any directory, and the
    // dialect of the share the files actually live on is what matters.
    info.remote_protocol = remote_protocol(path);
    info
}

/// The serial of the volume containing `path`, for validating a persisted
/// index.
///
/// Works for a configured subdirectory, because `volume_info` queries the
/// volume root rather than the path it is given.
pub fn volume_serial(path: &Path) -> Option<u32> {
    volume_info(path).volume_serial
}

/// Reads the remote protocol in use, when the root is a network share.
pub fn remote_protocol(root: &Path) -> Option<RemoteProtocol> {
    let handle = open_dir(root)?;
    let mut info: FILE_REMOTE_PROTOCOL_INFO = unsafe { std::mem::zeroed() };
    info.StructureVersion = 2;
    info.StructureSize = std::mem::size_of::<FILE_REMOTE_PROTOCOL_INFO>() as u16;

    // SAFETY: `handle` is a live directory handle; the buffer is a correctly
    // sized, correctly aligned local of exactly the type the class expects.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            handle.0,
            FileRemoteProtocolInfo,
            (&raw mut info).cast(),
            std::mem::size_of::<FILE_REMOTE_PROTOCOL_INFO>() as u32,
        )
    };

    if ok == 0 {
        // Expected on a local volume: the drive simply is not remote.
        return None;
    }
    Some(RemoteProtocol {
        protocol: info.Protocol,
        major: info.ProtocolMajorVersion,
        minor: info.ProtocolMinorVersion,
        revision: info.ProtocolRevision,
        flags: info.Flags,
    })
}

/// A handle closed on drop, so no early return can leak it.
struct Handle(windows_sys::Win32::Foundation::HANDLE);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: constructed only from a CreateFileW success, and closed
        // exactly once because Handle is not Copy or Clone.
        unsafe { CloseHandle(self.0) };
    }
}

fn open_dir(dir: &Path) -> Option<Handle> {
    // A volume root needs its trailing separator; anything deeper must not
    // have one. Same rule as `win_enum::open_dir_handle`.
    let wide = wide_path(dir, winpath::is_volume_root(dir));
    // SAFETY: `wide` is NUL-terminated and outlives the call. The flag
    // combination is the documented one for opening a directory; without
    // FILE_FLAG_BACKUP_SEMANTICS this fails with ERROR_ACCESS_DENIED, which
    // is the classic misdiagnosis.
    let h = unsafe {
        CreateFileW(
            wide.as_ptr(),
            0, // no access rights needed for metadata queries
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE || h.is_null() {
        None
    } else {
        Some(Handle(h))
    }
}

fn wide_nul(s: &str) -> Vec<u16> {
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    v
}

fn last_error() -> u32 {
    // SAFETY: GetLastError has no preconditions.
    unsafe { windows_sys::Win32::Foundation::GetLastError() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_the_documented_drive_types() {
        assert_eq!(DriveKind::from_raw(0), DriveKind::Unknown);
        assert_eq!(DriveKind::from_raw(1), DriveKind::NoRootDir);
        assert_eq!(DriveKind::from_raw(3), DriveKind::Fixed);
        assert_eq!(DriveKind::from_raw(4), DriveKind::Remote);
        assert_eq!(DriveKind::from_raw(99), DriveKind::Other(99));
    }

    #[test]
    fn only_local_volumes_could_expose_a_volume_handle() {
        // Both target drives are DRIVE_REMOTE, which is why no MFT path
        // exists in this crate.
        assert!(!DriveKind::Remote.supports_volume_handle());
        assert!(DriveKind::Fixed.supports_volume_handle());
    }

    /// The flag is read and reported, but it is deliberately not a
    /// disqualifier: NTFS sets it on every volume, so acting on it would
    /// disable server-side filtering everywhere.
    #[test]
    fn case_sensitive_search_reflects_the_volume_flag() {
        let mut v = VolumeInfo {
            filesystem_flags: Some(FILE_CASE_SENSITIVE_SEARCH),
            ..Default::default()
        };
        assert!(v.case_sensitive_search());

        v.filesystem_flags = Some(0x0000_0002);
        assert!(!v.case_sensitive_search());
    }

    #[test]
    fn an_uninterrogated_volume_does_not_claim_case_sensitivity() {
        assert!(!VolumeInfo::default().case_sensitive_search());
    }

    /// Guards the regression that prompted the change: an ordinary NTFS
    /// volume reports the flag, and that must not read as a problem.
    #[test]
    fn an_ordinary_ntfs_volume_reports_the_flag_without_it_meaning_anything() {
        let info = volume_info(Path::new("C:\\"));
        assert_eq!(info.filesystem.as_deref(), Some("NTFS"));
        // Whether it is set is the filesystem's business; the point is that
        // nothing in the crate branches on it.
        let _ = info.case_sensitive_search();
    }

    #[test]
    fn formats_the_smb_dialect() {
        let p = RemoteProtocol {
            protocol: WNNC_NET_SMB,
            major: 3,
            minor: 1,
            revision: 1,
            flags: 0,
        };
        assert!(p.is_smb());
        assert_eq!(p.dialect(), "SMB 3.1.1");
        assert!(p.likely_large_transactions());
    }

    #[test]
    fn smb_2_0_2_is_flagged_as_small_transaction() {
        let p = RemoteProtocol {
            protocol: WNNC_NET_SMB,
            major: 2,
            minor: 0,
            revision: 2,
            flags: 0,
        };
        assert!(
            !p.likely_large_transactions(),
            "SMB 2.0.2 caps transfers at 64 KiB, so 1 MiB buffers cannot help"
        );
    }

    #[test]
    fn a_non_smb_protocol_is_described_rather_than_guessed_at() {
        let p = RemoteProtocol {
            protocol: 0x12345678,
            major: 1,
            minor: 0,
            revision: 0,
            flags: 0,
        };
        assert!(!p.is_smb());
        assert_eq!(p.dialect(), "protocol 0x12345678");
        assert!(!p.likely_large_transactions());
    }

    #[test]
    fn interrogating_a_nonexistent_drive_reports_an_error_rather_than_panicking() {
        // Q: is very unlikely to be mapped; either way this must not panic.
        let info = volume_info(Path::new("Q:\\"));
        assert_eq!(info.root, "Q:\\");
        assert!(info.kind.is_some());
    }

    #[test]
    fn the_system_drive_reports_a_serial_and_filesystem() {
        let info = volume_info(Path::new("C:\\"));
        assert_eq!(info.kind, Some(DriveKind::Fixed));
        assert!(info.volume_serial.is_some(), "C:\\ should report a serial");
        assert!(info.filesystem.is_some());
        assert!(!info.is_subdirectory);
    }

    /// The regression this change exists to prevent.
    ///
    /// `GetVolumeInformationW` and `GetDriveTypeW` only accept a volume root,
    /// so interrogating a configured *subdirectory* used to lose the serial,
    /// the filesystem flags and the drive type - which in turn disabled the
    /// persisted index's only identity check and let a stale index be served.
    #[test]
    fn a_subdirectory_reports_the_same_volume_as_its_root() {
        let root = volume_info(Path::new("C:\\"));
        let sub = volume_info(Path::new("C:\\Windows"));

        assert!(sub.is_subdirectory, "C:\\Windows is not a volume root");
        assert_eq!(sub.volume_root.as_deref(), Some("C:\\"));
        assert_eq!(
            sub.volume_serial, root.volume_serial,
            "a subdirectory must report its volume's serial"
        );
        assert_eq!(
            sub.kind,
            Some(DriveKind::Fixed),
            "a subdirectory must not be reported as DRIVE_NO_ROOT_DIR"
        );
        assert_eq!(sub.filesystem, root.filesystem);
        assert_eq!(sub.filesystem_flags, root.filesystem_flags);
    }

    #[test]
    fn a_trailing_separator_does_not_change_the_answer() {
        let a = volume_info(Path::new("C:\\Windows"));
        let b = volume_info(Path::new("C:\\Windows\\"));
        assert_eq!(a.volume_serial, b.volume_serial);
        assert_eq!(a.kind, b.kind);
    }

    #[test]
    fn a_path_with_no_volume_degrades_without_panicking() {
        let info = volume_info(Path::new("relative\\thing"));
        assert_eq!(info.kind, Some(DriveKind::NoRootDir));
        assert_eq!(info.volume_root, None);
        assert!(info.volume_serial.is_none());
    }
}
