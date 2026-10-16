# Contributing to files

Thanks for helping. This guide takes you from a fresh machine to a merged pull request. The rules for the code itself are in [docs/CODE_STANDARDS.md](docs/CODE_STANDARDS.md). How the program fits together is in [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Setup

`files` is a Windows program. It builds and runs only on Windows 10 or 11, x64.

| Tool | Why | Install |
|---|---|---|
| Rust **1.95+**, MSVC toolchain | Compiler. 1.95 is the minimum because of `eframe`/`egui_kittest` 0.36. `rust-toolchain.toml` selects stable and installs `rustfmt` and `clippy` | <https://rustup.rs>, then accept the `x86_64-pc-windows-msvc` default |
| Visual Studio Build Tools, workload **Desktop development with C++**, with a **Windows 10/11 SDK** | The MSVC linker. The SDK provides `rc.exe`, which `build.rs` uses to embed the icon, version block and application manifest | <https://visualstudio.microsoft.com/visual-cpp-build-tools/> |
| Git for Windows | Source control. Its Git Bash runs the pre-push hook and `tools/check.sh` | <https://git-scm.com/download/win> |
| *Optional:* .NET SDK + WiX 5 | Only for building the installer (`tools/make_msi.ps1`) | `dotnet tool install --global wix --version 5.0.2` then `wix extension add --global WixToolset.UI.wixext/5.0.2` |
| *Optional:* Python 3 | Only for regenerating icons (`tools/make_icon.py`, standard library only) | <https://www.python.org> |
| *Optional:* GitHub CLI `gh` | Only for publishing a release (`make_msi.ps1 -Release`) | <https://cli.github.com> |

> **Watch for this warning.** If `rc.exe` is missing, `build.rs` does not fail. It prints `warning: ...` and the binary ships without its icon and manifest. A build without the manifest loses long-path support and per-monitor DPI. If you see that warning, install the Windows SDK.

Then clone the repository and turn on the hooks:

```powershell
git clone https://github.com/ibarra000/files
cd files
git config core.hooksPath .githooks     # once per clone: runs tools/check.sh before every push
cargo build
```

## Build and run

| Command | What it does |
|---|---|
| `cargo build` | Debug build of both binaries, `files.exe` (the tray app) and `files-cli.exe` (the console tool) |
| `cargo run --bin files -- --demo` | The panel over synthetic data, with no shares or config needed. Press the hotkey to summon it |
| `cargo run --bin files -- --settings` | The settings window on its own |
| `cargo run --bin files-cli -- --check-config` | Validates your `config.toml` without touching the network |
| `cargo run --bin files-cli -- --doctor` | Reports reachability, protocol, cache and hotkey status for every configured share |
| `cargo run --bin files-cli -- --bench` | Times each directory-listing strategy against your shares |
| `cargo build --release` | Optimized build (fat LTO) |
| `cargo build --profile release-dbg` | Release optimizations with debug symbols, for profiling |

Only one panel runs per Windows session. If `files` is already running from the Start menu, launching a second copy just summons the first. Quit the tray icon before running your build.

**Logging.** Set `FILES_LOG=info`, `debug` or `trace`; the default is `warn`. `files.exe` has no console, so read its output with [DebugView](https://learn.microsoft.com/sysinternals/downloads/debugview) or an attached debugger. `files-cli.exe` prints to stderr.

`files-cli --help` lists the flags and `FILES_*` environment variables (except `FILES_LOG`, above, and the internal `--page` / `--pinned`). Precedence is command line, then environment, then `config.toml`, then built-in defaults.

## Tests

```powershell
cargo test                                   # the whole suite: ~1,300 unit tests + integration tests
cargo test --test state_machine              # one integration test file
cargo test matcher::                         # tests whose path contains "matcher::"
cargo test --features ui-snapshots           # adds the pixel snapshot tests (needs a GPU adapter)
cargo test --lib update::http -- --ignored   # the one test that needs the internet
cargo bench                                  # criterion benchmarks for the matcher (benches/matcher.rs)
```

Where tests live:

- **Unit tests.** These sit in a `#[cfg(test)] mod tests` at the bottom of the file they test. Windows-only ones use `#[cfg(all(test, windows))]`.
- **Integration tests.** These are in `tests/*.rs` and use only the public library API.
  - `tests/state_machine.rs` drives the whole app state with a fake clock.
  - `tests/panel.rs` and `tests/settings.rs` drive the real egui UI with `egui_kittest`, and check its accessibility tree.
- **Property tests (`proptest`).** If one fails, proptest writes the failing case to a `*.proptest-regressions` file. Commit that file, so the case is retried forever after.
- **Fake share.** `files::index::fake_source::FakeDirSource` is an in-memory share that can fail, hang or count calls. Use it instead of the real network in index and search tests.
- **Pixel snapshots.** The PNGs live in `tests/snapshots/`.
  - After an intentional UI change, regenerate them and look at every changed image before committing it:

    ```powershell
    $env:UPDATE_SNAPSHOTS = 1; cargo test --features ui-snapshots; Remove-Item Env:UPDATE_SNAPSHOTS
    ```

  - `*.new.png`, `*.diff.png` and `*.old.png` are ignored by git.
  - CI cannot run these tests because it has no GPU, so running them is up to you whenever you touch `src/gui` or `src/view`.

## Before you push

One script runs every check. The pre-push hook runs it for you, and CI runs it on every pull request:

```powershell
sh tools/check.sh          # fmt, clippy, unsafe allowlist, naming, vocabulary, tests
sh tools/check.sh --fast   # the same without the test suite (what the hook runs)
```

| Check | Fails when | Fix |
|---|---|---|
| rustfmt | any file is not formatted | `cargo fmt --all` |
| clippy | any warning, including an `unsafe` block without a `// SAFETY:` comment or `unsafe` in a file that does not opt in | fix the warning; see [the unsafe policy](docs/CODE_STANDARDS.md#unsafe-code) |
| unsafe allowlist | a file opts into `unsafe` without being listed in `tools/unsafe-allowlist.txt`, or a listed file no longer needs it | use a safe API, or add the file with a reason (a reviewer must agree) |
| naming | a `fn get_*`, or a test named `test_*` | see [naming](docs/CODE_STANDARDS.md#naming) |
| vocabulary | organization-specific words in code or docs | see [vocabulary](docs/CODE_STANDARDS.md#vocabulary) |
| tests | `cargo test` fails | fix the code. Change a test only if the test itself is wrong |

Example of a failing run:

```text
==> rustfmt
Diff in src\search\query.rs:42: ...
FAILED: code is not formatted. Fix: cargo fmt --all
...
Some checks failed; see FAILED lines above.
error: failed to push some refs to 'https://github.com/ibarra000/files'
```

Run `cargo fmt --all`, commit, and push again. Format on save in your editor (rust-analyzer: `"editor.formatOnSave": true`) and this never happens. Set `FILES_HOOK_TESTS=1` if you want the hook to run the tests as well.

## Workflow

1. **Branch from `master`.** Use a short kebab-case name that says what the work is, with no prefix: `settings-window`, `result-columns`, `share-health-report`.
2. **Commit in small, reviewable steps.** Each commit should build and pass its tests.
3. **Write commit subjects as `type: what changes, in plain words`.** Use lowercase, present tense, and no trailing period. Describe what the user sees, not which files changed.

   | Type | Use for |
   |---|---|
   | `feat` | new behavior |
   | `fix` | a bug |
   | `perf` | faster or lighter, same behavior |
   | `refactor` | structure only |
   | `docs` | documentation and comments |
   | `test` | tests only |
   | `chore` | tooling, dependencies |
   | `ci` | the workflow |

   ✅ Good subjects from this repository's history:

   ```text
   feat: dock the panel along the top or bottom of the screen
   fix: two update checks at once no longer share a download
   perf: the drive list stops stat-ing an unreachable share sixty times a second
   refactor: the panel starts the settings window instead of drawing it
   ```

   ❌ Avoid:

   ```text
   configurable view setting added            (no type; says nothing about what it does)
   Fix bug in actor.rs                        (names a file, not the behavior)
   feat: Updated the settings.                (capitalized, past tense, trailing period)
   ```

   Use the body for the why, and for anything a reviewer would otherwise have to ask.

4. **Open a pull request against `master`.** The template asks what changed for the user and how you tested it. CI must be green: `fmt, clippy, audits, tests` and `minimum Rust (1.95)`.
5. **Get it merged.** PRs land as merge commits ("Merge pull request #N …"), which keeps each branch's commits visible in history.

Keep a pull request to one concern. A refactor that makes a feature possible goes in its own PR, or at least its own commit.

### Changes that need extra care

- **Network behavior** (`src/index`, `src/search/live.rs`, `src/search/verify.rs`). Test against a real SMB share as well as `FakeDirSource`, and run `files-cli --bench` / `--bench --walk`. Say in the PR what you ran it against.
- **New `unsafe` / Win32 FFI.** Read [the unsafe policy](docs/CODE_STANDARDS.md#unsafe-code) first. Adding a file to the allowlist needs a reviewer's explicit agreement.
- **Config keys.** Update `assets/default_config.toml`, which is the user-facing documentation and is checked by tests. If a key is renamed or removed, add a step to `src/config/migrate.rs` and bump `CONFIG_VERSION`.
- **On-disk index format** (`src/index/persist.rs`). Bump `FORMAT_VERSION`. Old caches are then rejected and rebuilt, never misread.
- **New dependencies.** Each one needs a comment in `Cargo.toml` saying why. Prefer `windows-sys` and the standard library over a new crate.

## Releasing

Maintainers only:

1. Bump `version` in `Cargo.toml`, run `cargo build` so `Cargo.lock` updates, and commit `chore: release <version>`.
2. Build and publish the installer:

   ```powershell
   pwsh tools/make_msi.ps1                     # target\release\files-<version>-x64.msi
   pwsh tools/make_msi.ps1 -Publish \\server\share\files-updates
   pwsh tools/make_msi.ps1 -Release            # GitHub release v<version> with the MSI and latest.toml
   ```

   - `-Publish` copies the MSI to a share folder and writes the `latest.toml` manifest beside it, for installs that set `update_from`.
   - `-Release` publishes the MSI and `latest.toml` as GitHub release `v<version>`. The `v` prefix is required, because the updater looks for the tag in exactly that form.

## Getting help

Open an issue with the output of `files-cli --doctor` (remove server names if they are private) and the steps to reproduce.
