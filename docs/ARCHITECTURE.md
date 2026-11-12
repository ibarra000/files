# Architecture

`files` answers one question very fast: *which files in these volumes have this in their name?* Listing a large network folder takes seconds to minutes, so the app never lists on demand. Each volume is read in the background, saved to a local SQLite shard, and held in memory, where a search is a parallel, vectorized scan that takes about a millisecond per million names.

It is one WPF executable (`src/Files`, .NET 10, self-contained) with a test project (`tests/Files.Tests`).

## Layers

| Folder | Role | Depends on |
|---|---|---|
| `Core/` | Names, the in-memory index, query parsing, ranking, the folder walk, live search, favorites, history and recall. No UI, no SQLite | — |
| `Data/` | `catalog.db` (the volume list) and the per-volume shards, with an embedded migration runner | Core |
| `Services/` | Settings, the indexer schedule, opening files, updates, the hotkey, start-up registration | Core, Data |
| `UI/` | The search panel, the settings window, the tray icon, the DWM backdrop | everything |
| `App.xaml.cs` | Start-up, single instance, wiring, and applying settings | everything |

## Storage

| What | Where | Format |
|---|---|---|
| Settings | `%APPDATA%\files\settings.json` | JSON, written through a temp file and a rename |
| Search history | `%APPDATA%\files\history.txt` | Searches a result was opened from; one per line, newest first, at most 50 |
| Recently opened | `%APPDATA%\files\opened.txt` | Paths opened or shown in their folder; same format, at most 50 |
| Favorites | `%APPDATA%\files\favorites.json` | JSON array of `{ Kind: File/Folder/Search, Target }`, newest first |
| Volume list | `%LOCALAPPDATA%\files\catalog.db` | SQLite, `Data/Sql/catalog-1.sql` |
| File names | `%LOCALAPPDATA%\files\volumes\<uid>.db` | SQLite, one shard per volume, `Data/Sql/shard-1.sql` |
| Staged updates | `%LOCALAPPDATA%\files\updates` | The MSI and the update helper |

**Migrations.** `Data/Migrations.cs` runs embedded scripts named `<kind>-<n>.sql` above the database's `PRAGMA user_version`, each in its own transaction. To change a schema, add the next numbered script; never edit one that has shipped.

**The catalog.** Each volume has a local row id, a random 16-byte `uid`, a normalized `root`, a `vtype` (`smb`, `nfs` or `local`, from the path), a display name, a `status` (`offline`, `online`, `scanning`), an optional `last_error` (`path_not_found`, `drive_not_found`, `permission_denied`) and timestamps. Triggers keep `status_changed_at` current and refuse any change to `uid`, `root` or `vtype`: to move a volume, remove it and add a new one.

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
- **Walk** (`Core/Walker.cs`). Eight threads share a stack of folders, because on a network share each listing is mostly waiting. Listing uses `FileSystemEnumerable` with a 64 KiB buffer, so one round trip returns many names. Limits: depth 32, and no fixed file count. A walk stops when its names would no longer fit: the 2.1 billion characters one arena can hold, or the machine's memory at 16 bytes per character (`Walker.CharLimit`). An unexpected error in a worker thread ends the walk and is rethrown to the caller rather than ending the process. Junctions and other reparse points are not followed. A folder that cannot be read is counted and skipped; 32 failures in a row abort the walk, so a share that drops mid-walk fails fast.
- **Live search** (`Core/LiveSearch.cs`). <kbd>Shift</kbd>+<kbd>Enter</kbd> walks every volume with the same walker, at most two volumes at once, but keeps only names that match (plus up to 64 other files of a folder whose path matches), so it needs little memory. Every 250 ms the panel builds a small `VolumeIndex` from what has been kept and runs the normal search over it, so ranking, filters and opening are the same as for indexed results. Nothing is saved; the next scheduled read still updates the shards. A volume that cannot be read is named on the status line and the others continue.
- **Failure.** A failed scan keeps the last good index, marks the volume `offline` with its reason, and is retried after 5 minutes.

There are no change notifications: a periodic rescan is simpler and works the same on every server.

## UI

- **Panel** (`UI/SearchWindow.xaml`). A borderless, topmost window. It keeps the resizable window styles so Windows snaps it; `UI/Snap.cs` answers `WM_NCCALCSIZE` so no frame is drawn, `WM_NCHITTEST` with the resize edges, and `WM_GETMINMAXINFO` so maximized means the work area. Its size and position are saved in settings when it hides. The background is the theme's solid one; `UI/Backdrop.cs` asks DWM for rounded corners and their shadow, no backdrop and no outline. Results are a virtualized `ListBox`; the matched part of each name is bold. With the box empty it shows the home list (`UI/HomeRows.cs`): a Favorites section (five, then "See all") and Recently opened, under headers that cannot be selected, so the arrow keys pass over them. Whether favorites still exist is checked on a background task each time the list is shown. ↑ on the top row of an empty box steps back through the search history (`Core/Recall.cs`). The footer reads "Enter to open"; the status line beside it stays empty unless something needs attention: a volume reading or unreachable, an invalid search, a live search's progress; short messages ("Copied …") replace it for four seconds.
- **Settings** (`UI/SettingsWindow.xaml`). In the same process. Every change is saved at once. Programs are edited in a grid and checked on save; invalid rows are reported and not saved.
- **Tray** (`UI/Tray.cs`). A WinForms `NotifyIcon`, the only WinForms type used.
- **Hotkey** (`Services/Hotkey.cs`). `RegisterHotKey` on a message-only window. There are two: one shows the panel, the other searches for the text under the mouse.
- **Text under the mouse** (`Services/ScreenText.cs`, after ibarra000/hover). First UI Automation: the `TextPattern` range at the pointer, expanded to its word and line, and the whitespace-separated token of the line that holds the word, within 400 ms. Otherwise `Windows.Media.Ocr` on a 600×200 crop around the pointer, taking the recognized word under it or nearest to it. Surrounding punctuation is trimmed. The app targets `net10.0-windows10.0.19041.0` for the WinRT OCR API.
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
