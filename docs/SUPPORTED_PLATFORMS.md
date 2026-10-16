# Supported platforms, storage and use cases

This page states what `files` supports, what merely works, and what it doesn't do. It describes the code as it is, not as the product name might suggest. When it says something is *unverified*, nothing in the code or tests covers that case. Try it with the [qualification steps](#qualifying-a-new-share) before relying on it.

**Status labels used below:**

| Label | Meaning |
|---|---|
| **Supported** | Designed for, with specific code paths, and exercised in tests |
| **Works** | Handled by the general code path. Expected to work, but not a design target |
| **Unverified** | Might work through generic fallbacks. Nothing confirms it |
| **Not supported** | Not built for, or known not to work |

## Use cases

`files` is for people who need to find a file by name on large network shares, fast and repeatedly, and then open it.

| Use case | How `files` handles it | Mapping kind |
|---|---|---|
| Find a file by name in one huge folder holding hundreds of thousands to millions of files | The whole listing is held in memory. A search sweeps it in milliseconds. A cheap stamp probe every ~60 s catches changes | `flat` |
| Find a file anywhere in a project hierarchy | The tree is walked once in the background and kept current by server change notifications. F5 refreshes on demand | `tree` |
| Search a share that is too big or too volatile to index | Each search asks the server directly, to a chosen depth (1–4), within a strict time budget | `live` |
| Keep searching while the network is down or slow | The last index is memory-mapped from local disk at start-up. Results stay available, and the status line says how old they are | any indexed kind |
| Open a multi-page document set as one PDF | `NAME.pdf`, `NAME_Page1.pdf`, … are merged into one PDF (up to 512 pages) and opened in your PDF viewer | — |
| Send certain file types to a specific program | A `[[viewer]]` claims extensions, and <kbd>F2</kbd> switches the default action | — |
| Reuse frequent searches | Aliases expand a short name to a full search. Recent searches come back with <kbd>Up</kbd> | — |
| Roll out updates on a closed network | `update_from` points at a share folder holding `latest.toml` and the MSI. No internet is needed | — |
| Diagnose a slow or unreachable share | `files-cli --doctor` reports drive type, protocol and dialect, reachability, round-trip time, cache state and watch status | — |

It isn't a full-text search engine: it matches **file and folder names**, not file contents. It isn't a sync client or file manager either.

## Operating system

| Platform | Status | Notes |
|---|---|---|
| **Windows 11**, x64 | **Supported** | Full appearance: Mica/Acrylic/Tabbed backdrops and rounded corners |
| **Windows 10**, x64 | **Supported** | Without the Windows 11 backdrops, the panel paints its own background |
| Windows Server 2016 or later, x64 | **Works** | Same code as Windows 10. Terminal-server sessions are kept apart: the single-instance lock and the settings pipe are both per session |
| Windows 8 / 8.1 | **Not supported** | The manifest still declares them, but current Rust's standard library for `x86_64-pc-windows-msvc` requires Windows 10 |
| Windows on ARM64 | **Not supported** | No ARM64 build or installer. The x64 MSI may run under emulation, untested |
| 32-bit Windows | **Not supported** | The installer is 64-bit only |
| Linux, macOS | **Not supported** | Some portable code paths exist so tests can run, but the tray, hotkey, single-instance and settings pipe are Windows-only. Nothing builds or packages for these platforms |

Other requirements:

- **The application manifest.** It is embedded at build time and gives the binary per-monitor DPI awareness (PerMonitorV2), `longPathAware`, the UTF-8 active code page and Common Controls v6. It runs `asInvoker`, so it never asks for elevation. The installer does need elevation (per-machine install).
- **Fonts.** Segoe UI and the Segoe icon fonts are loaded from `C:\Windows\Fonts`. If Windows is installed on another drive letter, the panel falls back to other fonts, so text and icons look different.

## Hardware

| Component | Requirement |
|---|---|
| **CPU** | x64. The matcher uses SIMD substring search (`memchr`) and spreads work over all cores (rayon), so more cores means faster searches on very large shares |
| **GPU / graphics driver** | **A working OpenGL driver is required.** The UI renders through OpenGL (`glow`), and there is no software fallback. If the driver lacks it, the panel fails to open with a message. Virtual machines and RDP sessions without OpenGL acceleration may fail this way |
| **Memory** | Each indexed share's names are held in memory: roughly the total length of its file names, twice (original and case-folded), plus 4 bytes per entry. Run `files-cli --bench --walk` to get a projected index size for a tree share |
| **Local disk** | The index cache lives in `%LOCALAPPDATA%\files`, about the same size as the in-memory index (on the order of 100 MB for a share of a few million files). It is rewritten after each full scan; changes from notifications are saved at most every 5 minutes. **It must be on a local disk**, because it is memory-mapped. Merged PDFs are cached there too and deleted after 7 days |
| **Display** | Any resolution. The panel is 600×400 logical pixels, scaled per monitor, or the work area's full width when docked |
| **Input** | Keyboard first. A global hotkey (default <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd>; the Copilot key works as `shift+win+f23`) |

## Network drives and protocols

### How a share is named

| Form | Example | Status |
|---|---|---|
| Mapped drive letter | `'X:\'`, `'X:\projects'` | **Supported**. The main target. `WNetGetConnectionW` resolves the drive to its UNC path for diagnostics |
| UNC path | `'\\server\share\folder'` | **Supported** |
| Extended-length prefix | `'\\?\X:\folder'` | **Works**. The prefix is accepted and stripped |
| Server without a share | `'\\server'` | **Not supported**. There is no volume to index |

Write paths as TOML literal strings (single quotes), so backslashes stay as typed.

### Protocols and storage types

| Storage | Status | What the code does |
|---|---|---|
| **SMB 2 / SMB 3** (Windows Server, Samba, most NAS) | **Supported** | The only protocol with dedicated handling, described below this table |
| SMB 1 / CIFS | **Unverified** | Disabled by default in current Windows. If enabled, it goes through the same calls, slower |
| **NFS** (Windows "Client for NFS") | **Unverified** | No NFS-specific code. It would run through the same Win32 calls, falling back from the large-buffer listing to `FindFirstFileExW` or `std::fs` if a call is refused, and marking change notifications unavailable if they are refused. Whether directory timestamps move when files are added, which the flat-share freshness check depends on, varies by server. **Check before relying on it:** `files-cli --bench --allow-write` creates one temporary file to confirm the stamp moves |
| **DFS namespaces** | **Works**, with a caveat | Handled by the normal SMB redirector, and start-up touches each root so DFS referrals resolve early. **Caveat:** tree walks never follow reparse-point folders. DFS links often look like reparse points to the client, so a `tree` mapping on a namespace *root* may skip its links. Map the link targets themselves, or check the skipped count with `files-cli --bench --walk` |
| Local NTFS / ReFS | **Works** | Used in the unit tests. Searching a local folder works; it just isn't what the tuning targets |
| USB and removable drives, RAM disks, optical drives | **Works** | Recognized by `--doctor`. Indexed like any folder; nothing specific |
| OneDrive / cloud-sync folders | **Works**, with a caveat | Placeholder *files* are listed without downloading them. Opening or merging one downloads it. Placeholder *folders* may be reparse points, which tree walks skip |
| WebDAV (`\\server@SSL\DavWWWRoot`) | **Unverified** | No specific code. It would appear as a remote drive with an unknown protocol |
| Junctions and symbolic links to folders | **Not followed** | Tree walks never descend into reparse-point folders, and no setting changes that. `--bench --walk` reports how many were skipped |
| Raw volume / MFT reading | **Not supported** | On purpose. `files` reads through the file-sharing protocol only |

#### SMB specifics

- **Listing.** The default strategy, `handle-dirinfo` (`GetFileInformationByHandleEx`), asks for 1 MiB of directory entries per round trip. SMB 2.0.2 servers cap transfers at 64 KiB, and the buffer shrinks to match. `files-cli --doctor` reports the negotiated dialect and predicts which you will get.
- **Change notifications.** Tree shares watch the root with one `ReadDirectoryChangesW` call, which the server sees as a single SMB2 `CHANGE_NOTIFY`. The buffer is 64 KiB, the maximum over the network. Some NAS firmware refuses or never fires notifications. `--doctor` shows whether the watch was accepted. If it never fires, a tree share with `refresh = "auto"` still re-walks every 30 minutes; a `manual` one needs F5.
- **Server-side wildcard filtering** (`server_filter`) is **off by default.** If you turn it on, every answer is still checked against the local index, and the filter turns itself off for the session after three disagreements. Run `files-cli --bench` before enabling it.
- **Credentials.** `files` uses your existing Windows session to the share. It never prompts and never stores credentials. "credentials rejected" and "is not mapped" are reported as such, not as generic failures.

### Behavior by mapping kind

| | `flat` | `tree` | `live` |
|---|---|---|---|
| Kept in memory | yes | yes | only results already seen |
| Cached on disk | yes | yes | results already seen |
| Notices changes by | stamp probe every 60 s ± 10 % | change notifications (debounced 2 s) | asking on each search |
| Full rescan | when the stamp moves, or at least hourly | F5; also every 30 min (+ up to 25 % spread) with `refresh = "auto"` | — |
| Default `refresh` | `auto` | `manual` (F5) | not allowed |
| Per-search network cost | none (optional verify on one flat share) | none | up to 64 round trips within 1.2 s, at most once a second per share |

### When a share is slow or offline

- **Start-up never waits on a share.** The cached index loads first. Pre-warming and volume checks happen on background threads.
- **A failed refresh keeps the last good index.** The share's status changes to *unreachable* with a retry countdown. Retries back off with jitter: probes from 5 s to 5 min, rescans from 30 s to 10 min.
- **During a walk,** a folder that fails is recorded as a gap, and the rest is indexed. After 32 consecutive network failures the walk stops, rather than paying a ~20 s SMB timeout for every remaining folder.
- **A share that suddenly lists as empty** must list empty twice in a row before it replaces a populated index.
- **On quit,** `files` doesn't wait on network calls, because an SMB call can't be cancelled.

## Limits

| Limit | Value |
|---|---|
| Results shown | 300, best first (the total match count is still reported) |
| Tree walk | depth 32, 1 000 000 folders, 8 000 000 entries |
| Files shown under a matching folder | 64 per folder |
| PDF page set | 512 pages |
| Name length | 65 535 bytes per name |
| Paths longer than 260 characters | **Need the Windows `LongPathsEnabled` policy** in addition to the manifest's `longPathAware`. `files` doesn't set or check that policy, and without it deeper files are not listed |
| Case-insensitive matching | Full for ASCII, Latin, Greek and Cyrillic. A few characters whose lowercase form changes length (`İ`, `ẞ`, the Kelvin and Ångström signs) fall back to a slower full-Unicode comparison |
| Server-side wildcard queries | 3–64 ASCII characters, literal text only |
| Concurrent full walks | `max_concurrent_scans` (default 2) across all shares |
| Index cache age | still served after 7 days, labelled stale |

## Qualifying a new share

Before you rely on a share, especially an NFS, DFS, NAS or WebDAV one, run these and read the output:

```powershell
files-cli --check-config               # config only, no network
files-cli --doctor                     # drive type, UNC target, protocol + dialect, reachability, RTT, cache, watch
files-cli --bench                      # times each listing strategy; checks they return the same count
files-cli --bench --walk               # tree shares: folders, files, depth, duration, projected index size, skipped junctions
files-cli --bench --allow-write        # creates ONE temporary file to confirm the folder's stamp moves
```

If the strategies disagree, if the stamp doesn't move, or if the watch is "unavailable", choose the mapping kind to match:

- **The stamp doesn't move:** prefer `tree`, or accept the hourly floor on a `flat` share.
- **No change notifications:** set `refresh = "auto"` on the `tree` share so it re-walks every 30 minutes, or rely on F5.
- **Too slow to walk:** use `live`.

Report results for a new storage type in an issue, so this page can move it from *Unverified* to *Works*.
