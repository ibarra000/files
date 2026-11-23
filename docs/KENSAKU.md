# kensaku in files

Content search uses [kensaku-headless](https://github.com/ibarra000/kensaku-headless) (MIT), a fork of [kensaku](https://github.com/yjid/kensaku) without its desktop app. Its release ships one `kensaku.exe`, which runs as the CLI, the daemon, an HTTP server and an MCP server. `files` runs it only as the daemon. Everything about it lives in four places:

| Where | What |
|---|---|
| `tools/fetch_kensaku.ps1` | Downloads the pinned release zip for Windows x86_64, checks its SHA256, and puts `kensaku.exe`, `onnxruntime.dll` and `LICENSE` in `artifacts\kensaku`. The pins are at the top of the script. |
| `wix/kensaku.wxs` | Installs those three files as `Program Files\files\kensaku`. `tools/make_msi.ps1` fetches them and adds them unless given `-SkipKensaku`. |
| `src/Files/Services/Kensaku/` | Runs the daemon and asks it for matches (see [ARCHITECTURE.md](ARCHITECTURE.md) and [UI_STATES.md](UI_STATES.md)). |
| `THIRD_PARTY_NOTICES.md` | The licences. |

No Rust or other build tool is needed.

## What runs

- **The program:** `kensaku.exe daemon run --data-dir %LOCALAPPDATA%\files\kensaku`, hidden, with `ORT_DYLIB_PATH` set to the `onnxruntime.dll` beside it. A build run from source finds it in `artifacts\kensaku` instead.
- **The data folder** holds kensaku's config, index, token and OCR models. Only this app uses it, so a kensaku the user runs on their own keeps its own index.
- **The pipe:** `\\.\pipe\kensaku-<id>`, where `<id>` is the first 12 hex digits of the SHA-256 of the lowercased data folder. It speaks newline-delimited JSON-RPC 2.0. Every request carries `token` (the contents of `daemon.token`) and `protocol_version: 2`.
- **The methods used:** `ping`, `health`, `setup`, `patch_config`, `pack_download`, `get_status`, `index_state`, `search`, `record_open` and `shutdown`.
- **What it reads:** `watched_dirs` is the roots of the volumes that are not live-only, and `ocr_enabled` follows the setting.

## Updating kensaku

1. Pick the new release on https://github.com/ibarra000/kensaku-headless/releases. Take the hash of `kensaku-<version>-x86_64-pc-windows-msvc.zip` from its `SHA256SUMS`.
2. Set `$Version` and `$Sha256` at the top of `tools/fetch_kensaku.ps1`, then run `pwsh tools/fetch_kensaku.ps1`.
3. Check the release's `docs/API.md` and `docs/CONFIG.md` against `KensakuPipe`, `KensakuSetup`, `KensakuQuery` and `Kensaku`. Look at the protocol version, the pipe name, the methods above, `watched_dirs` and `ocr_enabled`, and how the query language reads `ext:` and `in:`.
4. Update the versions in `THIRD_PARTY_NOTICES.md`.
5. Smoke test. Run `files` from source with content search on and a volume holding a text file with a word that is nowhere else. Wait for Settings to say it read the file, then search for the word.
