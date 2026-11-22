# Architecture

`files` answers one question very fast: *which files in these volumes have this in their name?* Listing a large network folder takes seconds to minutes, so the app never lists on demand. Each volume is read in the background, saved to a local SQLite shard, and held in memory, where a search is a parallel, vectorized scan that takes about a millisecond per million names.

It is one WPF executable (`src/Files`, .NET 10, self-contained) with a test project (`tests/Files.Tests`).

## Layers

| Folder | Role | Depends on |
|---|---|---|
| `Core/` | Names, the in-memory index, query parsing, ranking, the folder walk, live search, favorites, history and recall. No UI, no SQLite | — |
| `Data/` | `catalog.db` (the volume list) and the per-volume shards, with an embedded migration runner | Core |
| `Services/` | Settings, the indexer schedule, opening files, the preview of one file, updates, the hotkey, start-up registration | Core, Data |
| `UI/` | The search panel (its theme, rows, filters, preview and action menu), the settings window, the tray icon, the DWM backdrop | everything |
| `App.xaml.cs` | Start-up, single instance, wiring, and applying settings | everything |

## Storage

| What | Where | Format |
|---|---|---|
| Settings | `%APPDATA%\files\settings.json` | JSON, written through a temp file and a rename |
| Search history | `%APPDATA%\files\history.txt` | Searches a result was opened from; one per line, newest first, at most 50 |
| Recently opened | `%APPDATA%\files\opened.txt` | Paths opened or shown in their folder; same format, at most 50. Neither file grows while `RememberSearches` is off; each is emptied by its Clear button in Settings |
| Favorites | `%APPDATA%\files\favorites.json` | JSON array of `{ Kind: File/Folder/Search, Target }`, newest first |
| Volume list | `%LOCALAPPDATA%\files\catalog.db` | SQLite, `Data/Sql/catalog-1.sql` |
| File names | `%LOCALAPPDATA%\files\volumes\<uid>.db` | SQLite, one shard per volume, `Data/Sql/shard-1.sql` |
| Staged updates | `%LOCALAPPDATA%\files\updates` | The MSI and the update helper |
| Preset volumes | `volumes.json` beside `files.exe`, from the installer | JSON array of `{ name, folder, liveOnly }`, added once per user on each PC by `Services/BundledVolumes.cs` at start-up |
| Preset volumes added | `%LOCALAPPDATA%\files\bundled.txt` | Roots of the preset volumes already added, one per line, so a removed one is not added again. Local, like the catalog, because the volume list is per PC; checked by path alone, so start-up never waits on a mapped drive |

**Migrations.** `Data/Migrations.cs` runs embedded scripts named `<kind>-<n>.sql` above the database's `PRAGMA user_version`, each in its own transaction. To change a schema, add the next numbered script; never edit one that has shipped.

**The catalog.** Each volume has a local row id, a random 16-byte `uid`, a normalized `root`, a `vtype` (`smb`, `nfs` or `local`, from the path), a display name, a `status` (`offline`, `online`, `scanning`), an optional `last_error` (`path_not_found`, `drive_not_found`, `permission_denied`) and timestamps. Triggers keep `status_changed_at` current and refuse any change to `uid`, `root` or `vtype`. Pointing a volume at another folder (`Catalog.Move`) therefore replaces it, in one transaction, with a new row of the same name; the old shard is deleted and the new folder read from scratch. `live_only` (migration 2) marks a volume that is never read in the background and has no shard: the indexer never scans it, indexed search passes over it, and only a live search reads it.

**Shards.** A shard holds `meta(volume_uid, scanned_at)`, `folders(id, path)` with paths relative to the root (the root is id 0, path `''`), and `files(folder_id, name)`. A shard whose `volume_uid` does not match the catalog is ignored. A scan writes a complete new shard to `<uid>.db.tmp` with journaling off and renames it over the old one, so a crash mid-write leaves the previous shard intact.

## The in-memory index

```text
Names (Core/Names.cs)
  orig     "AB-C-0704.pdf\0AB-C-0704_Page1.pdf\0…"  original spelling, for display
  lower    "ab-c-0704.pdf\0ab-c-0704_page1.pdf\0…"  ToLowerInvariant, for matching
  offsets  [0, 14, 34, …]                            one table for both arenas

VolumeIndex (Core/VolumeIndex.cs)
  Files            Names of every file, grouped by folder
  Folders          Names of folder paths relative to the root
  FolderFirstFile  folder d owns files [FolderFirstFile[d], FolderFirstFile[d + 1])
```

- Lowercasing a UTF-16 char never changes its length, so one offsets table serves both arenas.
- The NUL separator means a substring hit can never span two names.
- A file's folder is found by binary search over `FolderFirstFile`, so no per-file parent is stored.
- Memory is about four bytes per character of file name, plus four per file: roughly 200 MB for five million files.

## Search

1. **Typing.** The panel waits 150 ms after the last key. A paste, a recalled search or a saved one searches at once. Each search gets a new epoch, so a slow answer to an old search is dropped.
2. **Parsing** (`Core/Query.cs`). The line is split on whitespace. `ext:a,b` tokens become a type filter; everything else is joined back with single spaces. One `*` at the start means ends-with (compared before the extension), one at the end means starts-with. Problems are reported, never thrown.
3. **Matching.** The file arena is cut into 8,192-name chunks and scanned in parallel with `MemoryExtensions.IndexOf` (vectorized). Each hit maps to its name by binary search, then the scan jumps past the name, so a name counts once at its leftmost hit. The mode, the `ext:` filter and the hidden extensions are applied before a name is counted.
4. **Folders.** A folder whose relative path contains the term adds up to 64 of its files as "inherited" results, ranked after direct matches. Files that also matched by their own name are not listed twice.
5. **Ranking** (`Core/TopK.cs`). Each candidate gets a packed `ulong`: inherited bit, match position, name length, file ordinal. A bounded heap keeps the best 300. Volumes are merged by the same key without the ordinal, then by path, so the order never depends on thread timing or volume order.

## Keeping volumes current

`Services/Indexer.cs` owns every volume's `VolumeState` (catalog row, last good index, scan progress, problem) in an immutable dictionary that is swapped on each change, and raises `Changed` from worker threads.

- **Start-up.** Every shard is loaded on a background task, so search works at once even when shares are offline. A `scanning` status left by a crash is reset to `offline`.
- **Schedule.** Once a minute, any volume never read, or last read longer ago than the rescan interval (1 h by default, or never), is queued. <kbd>F5</kbd> queues them all. At most two scans run at once.
- **Walk** (`Core/Walker.cs`). Eight threads share a stack of folders, because on a network share each listing is mostly waiting. Listing uses `FileSystemEnumerable` with a 64 KiB buffer, so one round trip returns many names. Limits: depth 32, and no fixed file count. A walk stops when its names would no longer fit: the 2.1 billion characters one arena can hold, or the machine's memory at 16 bytes per character (`Walker.CharLimit`). An unexpected error in a worker thread ends the walk and is rethrown to the caller rather than ending the process. Folder links (junctions, DFS links, a server's mount points) are followed only on a network share, where they can hold whole subtrees, and each target once, by its volume serial and file id, so a loop ends; on a local disk they are skipped. A folder that cannot be read is counted and skipped. After 32 network failures in a row the root is listed again: if it still reads, the walk goes on, otherwise it stops, so a share that drops mid-walk fails fast. A path too long or a name the server refuses never counts as a network failure. What was skipped (unreadable folders, links not followed, folders past depth 32) is shown on the volume in Settings and at the end of a live search.
- **Content search** (`Services/Kensaku/`, `UI/SearchWindow.Contents.cs`). Off by default. kensaku is built from the pinned submodule `third_party/kensaku` by `tools/build_kensaku.ps1` and installed in `kensaku\` beside `files.exe` (`wix/kensaku.wxs`; updating it: [KENSAKU.md](KENSAKU.md)). While content search is on, `KensakuHost` runs two of its programs hidden, with `KENSAKU_DATA_DIR=%LOCALAPPDATA%\files\kensaku`: `kensaku-daemon`, which indexes and watches the roots of the volumes that are not live-only, and `kensaku-cli serve --port 8766`, which answers searches. `serve` decides on its first request whether to use the daemon, so it is started only after the daemon answers a ping on its pipe (`\\.\pipe\kensaku`, with the token from `daemon.token`); if either stops, both start again. `KensakuConfig` writes `config.json` (every field kensaku requires, `is_setup_complete`, `ocr_enabled` from the setting); a volume added, removed or switched between live and indexed changes it, which restarts kensaku. After the name results are on screen, the panel asks `GET /search?q=…&limit=20` (bearer token read each time from `http.token`), with an 800 ms limit and cancelled by the next keystroke. Results kensaku found only by name are dropped (the panel lists names itself), as are files already listed; the rest are appended as a Contents group (`RowKind.Content`). The answer is checked by shape, not by version: an unknown shape is "not supported". Every failure is one footer message; nothing is stored. On quit the daemon is asked to shut down and whatever is left is ended. See docs/UI_STATES.md for the host's states.
- **Live search** (`Core/LiveSearch.cs`). <kbd>Shift</kbd>+<kbd>Enter</kbd> walks every volume with the same walker, at most two volumes at once, but keeps only names that match (plus up to 64 other files of a folder whose path matches), so it needs little memory. Every 250 ms the panel builds a small `VolumeIndex` from what has been kept and runs the normal search over it, so ranking, filters and opening are the same as for indexed results. Nothing is saved; the next scheduled read still updates the shards. A volume that cannot be read is named on the status line and the others continue.
- **Failure.** A failed scan keeps the last good index, marks the volume `offline` with its reason, and is retried after 5 minutes.

There are no change notifications: a periodic rescan is simpler and works the same on every server.

## UI

The panel's and Settings' states, what moves between them and the rules they keep are in [UI_STATES.md](UI_STATES.md).

- **Panel** (`UI/SearchWindow.xaml`, code in the `SearchWindow.*.cs` parts). A borderless, topmost window. It keeps the resizable window styles so Windows snaps it; `UI/Snap.cs` answers `WM_NCCALCSIZE` so no frame is drawn, `WM_NCHITTEST` with the resize edges and, over the empty space around the header, `HTCAPTION` (so Windows drags it, also out of maximized or snapped, where WPF's `DragMove` does nothing), and `WM_GETMINMAXINFO` so maximized means the work area. Its size and position are saved in settings when it hides, without the preview pane's width. Colors are tokens in `UI/PanelTheme.Dark.xaml` and `UI/PanelTheme.Light.xaml`, which `App.ApplyTheme` swaps app-wide with the theme; `UI/PanelStyles.xaml` (merged in `App.xaml`) holds the styles that use them, and `UI/Backdrop.cs` asks DWM for rounded corners and their shadow, no backdrop and no outline. The search box is the header: a search icon, no box chrome, a placeholder that counts the files searched, then the result count and time and, with more than one volume, a menu that searches just one. While searching, chips (`UI/SearchFilter.cs`) narrow the results: Code and Documents narrow the query to their types (`Query.Within`), Files and Folders act on the results. Results are a virtualized `ListBox` grouped by `UI/SearchRows.cs`: the best match, up to five folders found by their own name (`VolumeIndex.SearchFolders`), then files, under labels that cannot be selected. Each row has an icon from the extension alone (`Core/FileKind.cs`, `UI/FileIcons.cs`, so no file or share is touched), the name with the match in the accent color, and the folder in monospace below it or, when `ShowFolders` is off, after it; the volume badge shows only with more than one volume. With the box empty it shows the home list (`UI/HomeRows.cs`): Favorites (five, then "See all") and Recent (unless `ShowRecentlyOpened` is off). Whether favorites still exist is checked on a background task each time the list is shown. ↑ on the top row of an empty box steps back through the search history (`Core/Recall.cs`). The preview pane (Ctrl+P, `Services/Preview.cs`) reads only the selection, off the UI thread, 120 ms after it settles: size, date, type, and the first 40 lines of a text file, a small decoded copy of an image, a PDF's first page (rendered by Windows' own engine, `Windows.Data.Pdf`, in `Services/PdfThumbnail.cs`, for files up to 256 MB), the preview picture saved inside a DWG drawing (`Services/DwgThumbnail.cs` reads the image section the header points to, PNG or bitmap, checking every offset against the file), the preview a Creo part, assembly or drawing saves (`Services/CreoThumbnail.cs` finds the `#THMB_IMG_MAIN` section near the end of the file and walks its JPEG's segments to find where it ends; some are compressed with Unix compress, which `Services/UnixCompress.cs` expands, a bounded port of pigz's decoder; a file without one says so, since Creo's own thumbnail handler draws only that same picture), a drawing's picture, or a folder's first entries. Drawings are drawn where the format allows it, as lines through `Services/LineDrawing.cs`: a DXF's geometry, blocks and text (`Services/DxfPreview.cs`, with its saved THUMBNAILIMAGE when nothing else draws), an HP-GL plot's pen moves, also inside a printer driver's PCL (`Services/PltPreview.cs`), and a STEP model's edges when it cannot be shaded. A STEP model is otherwise shaded, in an isometric view (`Services/StepPreview.cs`): `StepFile.cs` indexes and parses the file; `StepShape.cs` reads each solid's faces and places an assembly's parts (which side of a placement is the assembly comes from the product structure, as exporters disagree); `StepSurfaces.cs` gives each face's surface (plane, cylinder, cone, sphere, torus, B-spline) a (u, v) both ways; `Mesher.cs` lays each face's boundary out in (u, v), fills it by ear clipping (`Triangulation.cs`) and splits the triangles until they follow the surface; `ShadedDrawing.cs` draws them with a depth buffer at twice the size, the visible edges over them, one colour per part. Faces on another kind of surface are left unfilled, and past 600,000 triangles the model is drawn as edges. DXF, plot and STEP files over 64 MB are not read. Other CAD files (SOLIDWORKS, Inventor, MicroStation, ...) show the thumbnail Windows gets from the handler their CAD program installed (`Services/ShellThumbnail.cs`, `IShellItemImageFactory` on its own STA thread, given up after 5 s). Tab or right-click opens the action menu (`UI/RowActions.cs`), a popup that the search box keeps typing into as a filter. The footer shows a status dot with how many files are indexed and when, unless something needs attention: "Scanning…" with a turning icon while any volume is read, and for at least a second after F5 so a fast read is still seen (the icon stays still when Windows animations are off, and never turns while the panel is hidden), a volume unreachable, an invalid search, a live search's progress; short messages ("Copied …") replace it for four seconds. On the right are key caps, as many as fit, and the settings button.
- **Settings** (`UI/SettingsWindow.xaml`). In the same process, in the panel's look: a sidebar of pages, settings grouped in cards under section labels, switches, fields, dropdowns and grids styled in `UI/SettingsStyles.xaml` (merged into this window only, so its implicit control styles never reach the panel). `UI/Backdrop.cs` gives its title bar the body's color. Every change is saved at once. Programs are edited in a grid and checked on save; invalid rows are reported and not saved.
- **Tray** (`UI/Tray.cs`). A WinForms `NotifyIcon`, the only WinForms type used.
- **Hotkey** (`Services/Hotkey.cs`). `RegisterHotKey` on a message-only window. There are two: one shows the panel, the other searches for the text under the mouse.
- **Text under the mouse** (`Services/ScreenText.cs`, after ibarra000/hover). First UI Automation: the `TextPattern` range at the pointer, expanded to its word and line, and the whitespace-separated token of the line that holds the word, within 400 ms. Otherwise a text-recognition engine (`Services/ITextReader.cs`) reads a 600×200 crop around the pointer, enlarged with bicubic scaling to the size that engine reads best, and the recognized word under the pointer or nearest to it is taken. The engine is chosen in Settings (`OcrEngine`): `WindowsTextReader` (`Windows.Media.Ocr`, crop enlarged 3×; good with digits, weaker with letters) or `PaddleTextReader` (PaddleOCR's PP-OCRv5 Latin models through RapidOcrNet and ONNX Runtime, crop enlarged 2×; better with letters, nothing leaves the PC). PaddleOCR's models (about 14 MB) ship in `models\v5` beside `files.exe` and load once, in the background as soon as PaddleOCR is chosen; if they are missing or the runtime cannot load (`TextReaderLoadException`), Windows OCR reads instead and the panel says so (`ScreenText.ReadWithFallbackAsync`); other errors are not hidden. One engine instance is loaded, behind a lock shared by loading, reading and closing. Surrounding punctuation is trimmed. Then the removal patterns from Settings (`ScreenTextRemovals`, `Core/TextCleanup.cs`) are applied to the text however it was read: regular expressions, ignoring case, each match limited to 100 ms so a badly backtracking pattern leaves the text as read with a note; patterns that do not compile are never saved, and text they leave empty is not searched. The app targets `net10.0-windows10.0.19041.0` for the WinRT OCR API.
- **Single instance.** A named mutex; a second launch sets a named event that makes the first instance show its panel, then exits.
- **Theme.** WPF's Fluent theme (`ThemeMode`), following Windows or forced light or dark.

## Opening files

- **Default**: `ShellExecute` on the file, or on a folder, which opens it in Explorer.
- **A program**: started with the path quoted (file names cannot contain `"`).
- **Show in folder**: `explorer.exe /select,"path"`.

## Updates

1. One minute after start, then every 4 hours, `Services/Updater.cs` reads `latest.json` (`version`, `msi`, `sha256`, `notes`) from the update folder, or from the latest GitHub release.
2. If it is newer, the tray offers to install it. The installer is copied or downloaded. Its SHA-256 must match, and `Services/Signature.cs` must find an intact Authenticode signature from the release certificate, pinned by thumbprint in `Updater.PublisherThumbprint`.
3. A copy of `files.exe` is started as `files-update.exe --apply-update <msi> <pid> <exe>`. It waits for the app to exit, runs `msiexec /i … /qb`, and relaunches only if the install succeeded.

The digest catches an incomplete download; the signature catches tampering. Because the certificate is self-signed, `WinVerifyTrust` reports an untrusted root, which is accepted: trust comes from the pinned thumbprint, and a signature from any other certificate is refused. Replacing the certificate takes one release signed with the old certificate that carries the new thumbprint.
