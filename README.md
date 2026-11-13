# files

Instant file-name search across folders and network shares on Windows.

`files` lives in the tray. Press a global hotkey (default <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd>) and a small panel appears. Start typing, and matching files from every volume you added show up within milliseconds, even across millions of names. Press <kbd>Enter</kbd> to open one. File lists are kept in memory and saved on the local disk, so search still works while a share is slow or offline.

## Features

- **Volumes.** Add any folder, drive or `\\server\share` in Settings. Each one is read in the background and read again on a schedule (every hour by default) or when you press <kbd>F5</kbd>.
- **Fast matching.** Every name is held in memory and searched in parallel: about a millisecond per million names. Results from every volume are merged into one ranked list. Files inside a folder whose name matches are listed too, below direct matches.
- **Live search.** <kbd>Shift</kbd>+<kbd>Enter</kbd> reads every volume now instead of using its saved list, so files added since the last read are found too. Results appear as folders are read; <kbd>Esc</kbd> stops it. When a normal search finds nothing, the footer offers <kbd>Shift</kbd>+<kbd>Enter</kbd> for a live search (<kbd>Enter</kbd> starts one too).
- **Search what you are looking at.** Point at a file name or drawing number anywhere on screen (an email, a PDF, a scanned sheet) and press <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Space</kbd>: the panel opens searching for it. The text comes from the app when it shares it, else from text recognition: the engine built into Windows, or PaddleOCR (Settings → General → Text recognition), which reads letters more reliably and also runs entirely on your PC, and a code such as `AB-C-0704_Page1.pdf` is kept whole. The shortcut can be changed or turned off in Settings.
- **Easy to scan.** Results come grouped as the best match, then matching folders (found by their own name), then files. Each has an icon for its kind (PDF, drawing, sheet, image, archive, video, audio, code or document) and its folder in monospace below; the volume is named only when you have more than one. Settings → Search → Results can put the folder on the same line, for one line per file. The header counts the results and says how long the search took; the footer says how many files are indexed and when they were last read, with a turning icon while volumes are read.
- **Filters.** While you type, chips narrow the results to All, Files, Folders, Code or Documents (<kbd>Ctrl</kbd>+<kbd>1</kbd> to <kbd>Ctrl</kbd>+<kbd>5</kbd>). With more than one volume, the menu in the header searches just one of them.
- **Preview.** <kbd>Ctrl</kbd>+<kbd>P</kbd> opens a pane beside the results with the selected file's size, date and type, and its first lines, a small copy of an image, or a folder's first entries, with Open, Show in folder and Copy path buttons. Only the selected file is read, off to the side, so a slow share never holds up typing.
- **Actions.** <kbd>Tab</kbd> or right-click opens every action for the selected result with its shortcut; type to filter them and press <kbd>Enter</kbd> to run one.
- **Search syntax.** `term*` starts with, `*term` ends with (before the extension), and `ext:pdf,dwg` filters by type.
- **Opening.** <kbd>Enter</kbd> opens with the Windows default or a program you add in Settings. <kbd>Ctrl</kbd>+<kbd>O</kbd> shows the file in its folder. <kbd>Tab</kbd> or right-click a result for every option. Drag a result into another app to hand it the file.
- **Favorites.** <kbd>Ctrl</kbd>+<kbd>D</kbd> adds the selected file to favorites, or removes it; with no file selected it saves the search you typed. The action menu (<kbd>Tab</kbd>) can also pin a result's folder or save the search. A saved search fills the box and runs. A favorite that is no longer at its path is shown greyed out as "Not found".
- **Home list.** With the box empty, the panel shows your favorites (five, with "See all" for the rest) and then the files you recently opened or showed in their folder, under Recent (<kbd>Del</kbd> takes one off).
- **Search history.** A search is remembered only when you open or show a result from it, so typos are not kept (the newest 50). <kbd>↑</kbd> in an empty box brings back earlier searches one by one, like a terminal, and <kbd>↓</kbd> goes forward again.
- **History control.** So file names don't show up while your screen is shared, Settings → General → History can pause history (nothing new is recorded, nothing is deleted), hide Recently opened from the home list, and clear recent searches or recently opened files separately. Favorites are never cleared.
- **Placing the panel.** Drag it anywhere by its edge padding (also out of maximized or snapped; double-click there maximizes or restores), resize it from its edges, or snap it like any window: <kbd>Win</kbd>+<kbd>←</kbd>/<kbd>→</kbd>, drag to a screen edge, or <kbd>Win</kbd>+<kbd>Z</kbd> for the Windows 11 snap layouts. Its size and place are remembered.
- **Signed updates** from GitHub releases or a shared folder. An update is installed only if it is signed with the files release certificate.

## Keys

| Key | Action |
|---|---|
| <kbd>Enter</kbd> | Open (or run the selected saved search) |
| <kbd>Shift</kbd>+<kbd>Enter</kbd> | Live search: read the volumes now |
| <kbd>Ctrl</kbd>+<kbd>O</kbd> or <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Show in folder |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> | Add to or remove from favorites (the search itself when no file is selected) |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy path / copy name (when no text is selected) |
| <kbd>Tab</kbd> | Action menu for the selected result (type to filter, <kbd>Enter</kbd> runs, <kbd>Esc</kbd> closes) |
| <kbd>Ctrl</kbd>+<kbd>P</kbd> | Show or hide the preview pane |
| <kbd>Ctrl</kbd>+<kbd>1</kbd> … <kbd>Ctrl</kbd>+<kbd>5</kbd> | Filter: All, Files, Folders, Code, Documents |
| <kbd>Del</kbd> | Remove the selected file from Recent (box empty) |
| <kbd>↑</kbd> <kbd>↓</kbd> <kbd>PgUp</kbd> <kbd>PgDn</kbd> | Move through results; <kbd>↑</kbd> at the top of an empty box recalls earlier searches |
| <kbd>Esc</kbd> | Stop a live search, leave "See all", clear the box, then hide |
| <kbd>F5</kbd> | Read every volume again |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings (or the gear at the bottom right) |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

## Install

1. Download `files-<version>-x64.msi` from the [releases](https://github.com/ibarra000/files/releases) and run it. It installs per machine and asks for elevation. "Start with Windows" is on by default.
2. Start **files** from the Start menu. The first time, Settings opens on **Volumes**: add the folders you want to search.
3. Press the hotkey and search.

Releases are signed with a self-signed certificate, so Windows shows the publisher as unknown and SmartScreen may warn on the first install. To check a download, its signature (right-click → Properties → Digital Signatures) should name `files` with thumbprint `5EAA01DFACB29C29C8D7C21F961F616CFB6B3728`. To have Windows trust it, import [assets/files-signing.cer](assets/files-signing.cer) into *Trusted Root Certification Authorities* and *Trusted Publishers*.

Upgrading from a 0.x (Rust) version: uninstall it first in Apps & Features. Settings are not carried over; the old `config.toml` in `%APPDATA%\files` can be deleted.

## Build from source

You need Windows 10/11 x64 and the [.NET 10 SDK](https://dotnet.microsoft.com/download).

```powershell
git clone https://github.com/ibarra000/files
cd files
dotnet test Files.slnx
dotnet run --project src/Files
```

## Documentation

| Document | What it covers |
|---|---|
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, building, testing, the branch/commit/PR workflow, releasing |
| [docs/CODE_STANDARDS.md](docs/CODE_STANDARDS.md) | Style, naming, errors, P/Invoke, tests |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Threads, storage, the index and search, the UI |
| [docs/SUPPORTED_PLATFORMS.md](docs/SUPPORTED_PLATFORMS.md) | Windows versions, hardware, network storage, limits |

## License

[MIT](LICENSE)
