# Supported platforms, storage and limits

**Labels:** **Supported** means designed for and tested. **Works** means it goes through the general code path and is expected to work. **Unverified** means nothing confirms it yet. **Not supported** means it is not built for that.

## What it is for

Finding a file by **name** in large folders and network shares, fast and repeatedly, and then opening it. It does not search file contents, sync files or manage them.

## Operating system

| Platform | Status | Notes |
|---|---|---|
| Windows 11, x64 | **Supported** | Acrylic backdrop and rounded corners |
| Windows 10, x64 | **Supported** | Plain window background |
| Windows Server 2016 or later, x64 | **Works** | The single-instance lock is per session, so terminal-server users don't collide |
| Windows on ARM64, 32-bit Windows | **Not supported** | The installer is x64 only. The x64 build may run under emulation, untested |
| Linux, macOS | **Not supported** | WPF is Windows-only |

The app is self-contained, so the .NET runtime need not be installed. It runs without elevation; the installer needs elevation (per-machine install).

## Hardware

| Component | Requirement |
|---|---|
| CPU | x64. Searches use every core, so more cores help on very large volumes |
| Memory | About 4 bytes per character of file name plus 4 per file: roughly 200 MB for five million files |
| Local disk | Shards in `%LOCALAPPDATA%\files\volumes`, about the size of the names they hold. Merged PDFs are cached there for 7 days |
| Graphics | Anything WPF runs on, including RDP and virtual machines (software rendering is the fallback) |

## Storage and protocols

| Storage | Status | Notes |
|---|---|---|
| Local disks | **Supported** | |
| SMB 2/3: mapped drive letters or `\\server\share` paths | **Supported** | The main target. Listings use a 64 KiB buffer, so one round trip returns many names |
| NFS (Windows Client for NFS) | **Unverified** | Same calls as SMB, recorded with type `nfs` |
| DFS namespaces | **Works**, with a caveat | Folder junctions and other reparse points are not followed, and DFS links often look like reparse points. Add the link targets as volumes instead of the namespace root |
| SMB 1 | **Unverified** | Disabled by default in current Windows |
| Cloud placeholders (OneDrive and similar) | **Works** | Names are listed without downloading the files |

## Limits

| Limit | Value |
|---|---|
| Results shown | 300, best first (the count of all matches is shown too) |
| Files listed per matching folder | 64 |
| Folder depth | 32 |
| Files per volume | 8 million |
| Pages in a combined PDF | 512 |
| Scans at once | 2 |
| Recent searches kept | 200 |

## Freshness

A volume is read again on a schedule (every hour by default; every 4 hours, daily or only on <kbd>F5</kbd> are the other choices) and on <kbd>F5</kbd>. There are no change notifications, so a file added since the last read appears after the next one. While a volume is unreachable, its last file list is still searched, and the status line says how old it is.
