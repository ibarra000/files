# files

Instant file-name search across large network shares on Windows.

`files` lives in the tray. Press a global hotkey (default <kbd>Ctrl</kbd>+<kbd>Shift</kbd>+<kbd>Space</kbd>) and a small panel appears. Start typing and matching files from every configured share show up within a few milliseconds, even in folders holding millions of entries. Press <kbd>Enter</kbd> to open one. The index sits in memory and is cached on local disk, so search still works while a share is slow or offline.

## Features

- **Three kinds of share.** Each share is set up as one of these:
  - **flat**: one huge directory, kept fresh by a cheap change check.
  - **tree**: a folder hierarchy, kept current by change notifications.
  - **live**: too big to index, so it is asked directly on each search.
- **Fast matching.** Matching is SIMD and multi-threaded, with stable, deterministic ranking. Results from every share are merged into one list.
- **Opening files.** You can open with the Windows default, with a viewer you configure for chosen file types, or reveal the file in Explorer.
- **PDF page sets.** A set of PDFs such as `NAME.pdf`, `NAME_Page1.pdf`, … can be merged into one document and opened.
- **Typing helpers.** Aliases (short names that expand to a longer search), search history on <kbd>Up</kbd>, and `ext:` filters.
- **Settings and diagnostics.** A settings window and a tray menu. `files-cli --doctor` and `--bench` check a share's health and speed.
- **Updates.** Update checks come from GitHub releases or from a folder on a share. Installers are verified with SHA-256.

## Install

1. Download `files-<version>-x64.msi` from the [releases](https://github.com/ibarra000/files/releases) and run it. It installs per machine and asks for elevation. A "Start with Windows" option is on by default.
2. Start **files** from the Start menu. On first run it writes a commented template to `%APPDATA%\files\config.toml`.
3. Edit that file: add one `[[mapping]]` per share and set `enabled = true`. Then validate it:

   ```powershell
   files-cli --check-config
   files-cli --doctor        # reachability, protocol, timings for every share
   ```

4. Press the hotkey and search.

See [docs/SUPPORTED_PLATFORMS.md](docs/SUPPORTED_PLATFORMS.md) for what hardware, Windows versions, network drives and protocols are supported.

## Build from source

You need Windows 10/11 x64, Rust 1.95 or newer (MSVC), and Visual Studio Build Tools with the Windows SDK. The full list is in [CONTRIBUTING.md](CONTRIBUTING.md#setup).

```powershell
git clone https://github.com/ibarra000/files
cd files
cargo run --bin files -- --demo    # the panel over synthetic data; no shares needed
```

## Documentation

| Document | What it covers |
|---|---|
| [CONTRIBUTING.md](CONTRIBUTING.md) | Setup, building, testing, the branch/commit/PR workflow, releasing |
| [docs/CODE_STANDARDS.md](docs/CODE_STANDARDS.md) | Formatting, naming, the `unsafe` policy, errors, comments, and the checks that enforce them |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Processes, threads, the index and search pipelines, UI layering, config, updates |
| [docs/SUPPORTED_PLATFORMS.md](docs/SUPPORTED_PLATFORMS.md) | Use cases, OS and hardware requirements, network drives and protocols, limits |

## License

[MIT](LICENSE)
