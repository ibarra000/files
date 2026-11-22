# Third-party notices

The installer includes these programs from others. Their licence files are installed beside them, in `Program Files\files\kensaku`.

| Component | Version | Licence | Source |
|---|---|---|---|
| kensaku (`kensaku-daemon.exe`, `kensaku-cli.exe`) | 1.0.18 | MIT, `LICENSE-kensaku.txt` | https://github.com/yjid/kensaku, built from `third_party/kensaku` |
| ONNX Runtime (`onnxruntime.dll`) | 1.31.0 | MIT, `LICENSE-onnxruntime.txt`; its own third-party notices in `ThirdPartyNotices-onnxruntime.txt` | https://www.nuget.org/packages/Microsoft.ML.OnnxRuntime |

kensaku's Rust dependencies are listed in `third_party/kensaku/cli/Cargo.lock` and `third_party/kensaku/src-tauri/Cargo.toml`.
