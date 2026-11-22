# kensaku in files

Content search uses [kensaku](https://github.com/yjid/kensaku) (MIT), built from source and installed beside `files.exe`. Everything about it lives in four places:

| Where | What |
|---|---|
| `third_party/kensaku/` | A git submodule of kensaku, pinned to a release tag (now `v1.0.18`). |
| `tools/build_kensaku.ps1` | Builds `kensaku-daemon.exe` and `kensaku-cli.exe`, downloads ONNX Runtime (checked against a pinned SHA256), and copies them with their licences to `artifacts\kensaku`. The pins are at the top of the script. |
| `wix/kensaku.wxs` | Installs that folder as `Program Files\files\kensaku`. `tools/make_msi.ps1` builds and adds it unless given `-SkipKensaku`. |
| `src/Files/Services/Kensaku/` | Runs the two programs and asks them for matches (see [ARCHITECTURE.md](ARCHITECTURE.md) and [UI_STATES.md](UI_STATES.md)). |

kensaku's own desktop app is not built or shipped.

## Building

See the prerequisites in [CONTRIBUTING.md](../CONTRIBUTING.md#setup), then:

```powershell
git submodule update --init third_party/kensaku
pwsh tools/build_kensaku.ps1
```

Notes on the build:
- kensaku's `cli/Cargo.lock` lags its dependencies, so cargo resolves the missing ones (as kensaku's own release build does) and the script puts the lock file back afterwards.
- `kensaku-daemon.exe` needs a manifest asking for Common Controls v6, which kensaku's build does not give it; the script embeds kensaku's own `windows-app-manifest.xml` with `mt.exe`. Without it Windows ends the daemon at once (`0xC0000139`).
- ONNX Runtime comes from Microsoft's `Microsoft.ML.OnnxRuntime` NuGet package, the same build as the GitHub zip kensaku's release names.

## Updating kensaku

1. Move the submodule to the new tag: `git -C third_party/kensaku fetch --tags && git -C third_party/kensaku checkout vX.Y.Z`
2. Check kensaku's `.github/workflows/release.yml` ("Stage ONNX Runtime (Windows)") for a new ONNX Runtime version. If it changed, update `$OrtVersion` and `$OrtSha256` at the top of `tools/build_kensaku.ps1`: download the package once and take `(Get-FileHash -Algorithm SHA256 <file>).Hash`. Check the minimum Rust version too.
3. Check that `config.json`'s required fields (`src-tauri/src/config.rs`, the fields without `#[serde(default)]`), `CURRENT_CONFIG_VERSION`, the pipe protocol (`cli/src/bin/kensaku-daemon.rs`, `src-tauri/src/daemon.rs`) and `/health` and `/search` (`src-tauri/src/agent/http.rs`) still match `KensakuConfig`, `KensakuPipe` and `Kensaku`.
4. Run `pwsh tools/build_kensaku.ps1`, then the smoke test: run `files` from source with content search on and a volume holding a text file with a word nowhere else, wait for Settings to say it read the file, and search for the word.
5. Commit the submodule bump (`git add third_party/kensaku`) with any changes to the pins.
