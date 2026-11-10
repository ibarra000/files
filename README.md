# files

Instant file-name search across folders and network shares on Windows.

`files` lives in the tray. Press a global hotkey (default <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd>) and a small panel appears. Start typing, and matching files from every volume you added show up within milliseconds, even across millions of names. Press <kbd>Enter</kbd> to open one. File lists are kept in memory and saved on the local disk, so search still works while a share is slow or offline.

## Features

- **Volumes.** Add any folder, drive or `\\server\share` in Settings. Each one is read in the background and read again on a schedule (every hour by default) or when you press <kbd>F5</kbd>.
- **Fast matching.** Every name is held in memory and searched in parallel: about a millisecond per million names. Results from every volume are merged into one ranked list. Files inside a folder whose name matches are listed too, below direct matches.
- **Search syntax.** `term*` starts with, `*term` ends with (before the extension), and `ext:pdf,dwg` filters by type.
- **Opening.** <kbd>Enter</kbd> opens with the Windows default, a program you choose, or as a combined PDF. <kbd>Ctrl</kbd>+<kbd>O</kbd> shows the file in its folder. Right-click a result for every option. Drag a result into another app to hand it the file.
- **PDF page sets.** `NAME.pdf`, `NAME_Page2.pdf`, `NAME-p3.pdf`, … in one folder can be merged into one document (<kbd>Ctrl</kbd>+<kbd>D</kbd>).
- **Typing helpers.** Aliases (a short name that searches for something longer) and recent searches, shown when the box is empty.
- **Updates** from GitHub releases or a shared folder, with SHA-256 verification.

## Keys

| Key | Action |
|---|---|
| <kbd>Enter</kbd> | Open (or use the selected recent search) |
| <kbd>Ctrl</kbd>+<kbd>O</kbd> or <kbd>Ctrl</kbd>+<kbd>Enter</kbd> | Show in folder |
| <kbd>Ctrl</kbd>+<kbd>D</kbd> | Open as combined PDF |
| <kbd>Ctrl</kbd>+<kbd>C</kbd> / <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>C</kbd> | Copy path / copy name (when no text is selected) |
| <kbd>↑</kbd> <kbd>↓</kbd> <kbd>PgUp</kbd> <kbd>PgDn</kbd> | Move through results |
| <kbd>Esc</kbd> | Clear the box, then hide |
| <kbd>F5</kbd> | Read every volume again |
| <kbd>Ctrl</kbd>+<kbd>,</kbd> | Settings |
| <kbd>Ctrl</kbd>+<kbd>Q</kbd> | Quit |

## Install

1. Download `files-<version>-x64.msi` from the [releases](https://github.com/ibarra000/files/releases) and run it. It installs per machine and asks for elevation. "Start with Windows" is on by default.
2. Start **files** from the Start menu. The first time, Settings opens on **Volumes**: add the folders you want to search.
3. Press the hotkey and search.

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
