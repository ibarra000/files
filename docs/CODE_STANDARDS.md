# Code standards

These are the rules every change follows. Most of them are checked by machine: `sh tools/check.sh` runs them locally and in the pre-push hook, and CI runs them on every pull request (see [CONTRIBUTING.md](../CONTRIBUTING.md#before-you-push)). The rest are checked in review. Examples marked ✅ are taken from this codebase unless they say otherwise.

| Rule | Enforced by |
|---|---|
| [Formatting](#formatting) | `cargo fmt --check` |
| [Naming](#naming) | rustc `nonstandard_style` (denied), naming audit in `tools/check.sh`, review |
| [Unsafe code](#unsafe-code) | rustc `unsafe_code` (denied), clippy `undocumented_unsafe_blocks` (denied), `tools/unsafe-allowlist.txt` |
| [Vocabulary](#vocabulary) | vocabulary audit in `tools/check.sh` |
| [Errors and logging](#errors-and-logging), [comments](#comments-and-docs), [architecture](#architecture-rules), [tests](#testing), [dependencies](#dependencies) | clippy `-D warnings`, review |

The lint levels are set in the `[lints]` table of `Cargo.toml`, so `cargo build`, `cargo clippy` and rust-analyzer all report the same thing.

## Formatting

**rustfmt decides formatting, with no exceptions and no `#[rustfmt::skip]`.** The settings are in `rustfmt.toml`:

```toml
max_width = 100
use_field_init_shorthand = true
```

`use_field_init_shorthand` writes `Point { x, y }` rather than `Point { x: x, y: y }`.

- Run `cargo fmt --all` before every commit, or turn on format-on-save in your editor (rust-analyzer: `"editor.formatOnSave": true`).
- The pre-push hook refuses to push unformatted code. CI refuses to merge it.
- A formatting-only change goes in its own commit, so it never hides a real diff.

## Naming

rustc's case rules are compiler errors here (`nonstandard_style = "deny"`): `snake_case` for functions, variables and modules, `UpperCamelCase` for types and traits, `UPPER_SNAKE_CASE` for constants and statics. The house rules below go further.

### Files and modules

Use short, singular, `snake_case` nouns for what the module *is*: `alias`, `doctor`, `placement`, `topk`, `latest_slot`, `humanize`.

Platform-specific implementations sit next to their portable counterparts and are named by role:

| Prefix | Meaning | Examples |
|---|---|---|
| `win_` | Win32 implementation, `#[cfg(windows)]` | `src/index/win_enum.rs`, `win_watch.rs`, `win_util.rs`, `src/hotkey/win_hwnd.rs` |
| `std_` | portable `std` implementation of the same trait | `src/index/std_enum.rs` |
| `fake_` | in-memory test double of the same trait | `src/index/fake_source.rs` |

```text
✅ src/index/win_watch.rs       the ReadDirectoryChangesW watcher
❌ src/index/watcher_windows.rs
❌ src/index/utils.rs           (a grab bag; name what it holds)
```

### Types

Use `UpperCamelCase` nouns: `IndexStore`, `SnapshotBuilder`, `TreeIndex`, `CancelToken`, `WalkPermits`.

Error types are named `<Area>Error`, one per area, and written by hand, with no `thiserror` or `anyhow`. New ones follow the full pattern: an enum, `impl Display`, and an empty `impl std::error::Error`. Variants carry the raw OS code when there is one, and messages are lowercase phrases that name the thing that failed (`src/index/errors.rs`):

```rust
pub enum EnumError {
    /// The directory exists and nothing matched. Not a failure.
    Empty,
    PathNotFound(u32),
    NotADirectory(u32),
    AccessDenied(u32),
    /// Worth one retry; a mapped drive often reconnects lazily.
    Transient(u32),
    // ...
}

Self::AccessDenied(code::LOGON_FAILURE) => {
    detail(format!("credentials rejected for {target}"))
}
Self::Transient(_) => detail(format!("{target} unreachable")),
```

`HotkeyParseError` follows the same pattern. Some older ones, such as `LaunchError`, `WriteError`, `LoadError` and `HttpError`, still lack `Display` or the `Error` impl; add them when you touch those types.

### Functions

| Pattern | Use for | Examples |
|---|---|---|
| `new` | the obvious constructor | `Epoch::new`, `SnapshotBuilder::new`, `FakeDirSource::new` |
| `of(x)` | derive a value from something else | `MappingKey::of(path)`, `IndexOverview::of(routes, statuses)`, `Source::of(settings)` |
| `from_*` | build from a specific representation | `EnumError::from_win(code)`, `Settings::from_env()` |
| `with_*` | builder-style variant or option | `with_capacity`, `with_max_depth`, `FakeDirSource::with_dir` |
| `is_*` / `has_*` | `bool` predicates | `is_empty`, `is_live`, `is_dark`, `has_selection` |
| plain verb | actions | `spawn`, `load`, `save`, `publish`, `probe`, `describe` |

**No `get_` prefix.** Name the thing returned. The naming audit fails on any `fn get_*`.

```rust
✅ pub fn entries(&self) -> &[String]
✅ pub fn folder(&self, dir: &Path, ttl: Duration) -> Cached
❌ pub fn get_entries(&self) -> &[String]
```

### Constants

Use `UPPER_SNAKE_CASE`, with the unit in the name or the type: `SEARCH_DEBOUNCE: Duration`, `MAX_RESULTS`, `SEGMENT_MAX_BYTES`, `WATCH_DIRTY_CAP`. Tunables live in `src/config/mod.rs`, each with a comment saying why it has that value.

```rust
❌ if attempts > 32 { ... }
✅ if attempts > ABORT_AFTER_CONSECUTIVE_TRANSIENT { ... }   // src/index/walk.rs, with its reason
```

### Test names

**Test names are sentences that state the behavior.** They are lowercase, with no `test_` prefix (the naming audit fails on one). A failing test name should read as a bug report.

```rust
✅ fn a_burst_of_typing_dispatches_exactly_one_search()
✅ fn escape_drops_the_selection_before_it_clears_the_search()
✅ fn a_version_this_cannot_compare_is_refused_rather_than_guessed_at()
❌ fn test_debounce()
❌ fn search_works()
```

Families use a shared prefix: pixel snapshot tests are `looks_right_<situation>` (`looks_right_with_a_toast`), and matcher parity tests are `parity_<case>`.

### Everything else

| Thing | Convention | Examples |
|---|---|---|
| Cargo features | kebab-case | `test-fonts`, `ui-snapshots` |
| Config keys | `snake_case`; tables singular | `max_concurrent_scans`, `hide_on_blur`, `[[mapping]]`, `[[viewer]]`, `[[alias]]` |
| Config values | lowercase, kebab-case when multi-word | `"handle-dirinfo"`, `"ctrl+shift+space"`, `"auto"` |
| Environment variables | `FILES_` + `UPPER_SNAKE` | `FILES_LOG`, `FILES_THEME`, `FILES_VIEWER` |
| Branches | kebab-case description | `settings-window`, `result-columns` |
| Threads | `files-<role>` | `files-search`, `files-index`, `files-ipc-accept` (`taskbar-theme` predates the rule) |

## Unsafe code

**Short version:** don't write `unsafe`. The compiler rejects it in every file except a short, reviewed list of Win32 FFI modules. In those files every `unsafe` block carries a `// SAFETY:` comment.

### Why there is any at all

`files` talks to Windows directly through `windows-sys`, for named pipes, `ReadDirectoryChangesW`, `RegisterHotKey`, WinHTTP, BCrypt, DWM, the clipboard and the registry. Every one of those calls is `unsafe` in Rust. Pulling in higher-level crates for each would multiply dependencies without removing the unsafety, only hiding it. So the policy is *containment*, not zero. Unsafe code lives only in thin platform modules that wrap each call in a safe API. Everything else (the search engine, the state machine, the view layer, config, utilities) is safe Rust, and the compiler keeps it that way.

### How it is enforced

```toml
# Cargo.toml
[lints.rust]
unsafe_code = "deny"                    # any `unsafe` is a compile error ...

[lints.clippy]
undocumented_unsafe_blocks = "deny"     # ... and every allowed block needs `// SAFETY:`
```

A file opts out with an inner attribute at the top, which must name its reason:

```rust
//! System clipboard access, implemented directly over `windows-sys`.

// Win32 FFI: OpenClipboard, GlobalAlloc and SetClipboardData.
#![allow(unsafe_code)]
```

`tools/check.sh` then requires every file that opts out to be listed in `tools/unsafe-allowlist.txt`, and every listed file to still need it. Adding a line to that file is the review-visible moment: a reviewer must agree that the code cannot be written safely.

**Never** opt out in the non-test code of `src/search`, `src/app`, `src/view`, `src/util`, `src/config` or `src/paths.rs`. If logic there seems to need `unsafe`, the design is wrong.

### Writing an FFI wrapper

1. Put the code in a `win_*` module, or an existing one on the allowlist, behind a safe function or type.
2. In new code, keep each `unsafe` block to one call, with the checks (null, return code, length) outside it in safe code. Some older blocks in the allowlisted modules span a few calls; shrink them when you touch them.
3. Put a `// SAFETY:` comment directly above each block. It states the invariant that makes the call sound, not what the call does.
4. Own every handle in a type with `Drop`, so it is closed exactly once, including on panic (`panic = "unwind"` is kept for this reason).
5. Treat buffers the server or kernel fills as untrusted. Bounds-check every offset before reading (see `src/index/win_enum.rs`).

✅ From `src/clipboard.rs`: one call per block, and each comment proves the call is sound:

```rust
// SAFETY: a plain allocation request; the result is checked for null.
let handle: HGLOBAL = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
if handle.is_null() {
    return Err(ClipboardError::Os("out of memory".into()));
}

// SAFETY: `handle` is a live GMEM_MOVEABLE block from the call above.
let ptr = unsafe { GlobalLock(handle) };
if ptr.is_null() {
    // SAFETY: still owned here - SetClipboardData has not been called,
    // so freeing it is correct and happens exactly once.
    unsafe { GlobalFree(handle) };
    return Err(ClipboardError::Os("could not lock clipboard memory".into()));
}
```

❌ Rejected forms:

```rust
unsafe { CloseHandle(h) };                     // no SAFETY comment: clippy error

// SAFETY: calling CloseHandle.                // restates the code; proves nothing
unsafe { CloseHandle(h) };

// SAFETY: for every call below.               // one comment for many blocks: each block needs its own
unsafe { a() }; unsafe { b() }; unsafe { c() };

let n = unsafe {                               // new code: logic inside the block; keep only the call there
    let r = ReadFile(...);
    if r == 0 { return Err(...) }
    len as usize
};
```

`unsafe impl Send` / `Sync` for a handle wrapper needs the same justification. Write it on the type, and point each `SAFETY:` comment at it (`src/index/win_watch.rs`):

```rust
/// `HANDLE` is a raw pointer and so neither `Send` nor `Sync`. Both are
/// asserted because the only cross-thread operations are
/// `ReadDirectoryChangesW` on one thread and `CancelIoEx` on the other, the
/// documented way to interrupt a blocked read. The handle is closed once, on
/// drop, after the pump thread has been joined.
struct SharedHandle(OwnedHandle);

// SAFETY: see the type's own documentation.
unsafe impl Send for SharedHandle {}
// SAFETY: see the type's own documentation.
unsafe impl Sync for SharedHandle {}
```

Test-only `unsafe`, such as `std::env::set_var` (unsafe since edition 2024), goes on the test module alone with `#[allow(unsafe_code)]`, never on the whole file.

## Vocabulary

`files` is a general-purpose tool, so names, comments, messages, test data and docs use general words:

| Say | For |
|---|---|
| **search input** / **search term** | what the user types into the panel |
| **share** / **mapping** | a configured place to search (`[[mapping]]`) |
| **flat share** / **tree share** / **live share** | the three mapping kinds |
| **folder** | a directory on a share |
| **base name** | the name a PDF page set shares (`NAME.pdf`, `NAME_Page1.pdf`, …) |
| **viewer** | a program configured in `[[viewer]]` |

Sample data should look like sample data: `AB-C-0704.pdf`, `report.pdf`, `V:\shared\files`, `R:\projects`. The vocabulary audit in `tools/check.sh` holds the list of organization-specific terms that are refused. The only exception is the legacy values that the configuration loader and migration (`src/config/migrate.rs`, `src/config/file.rs`) must still read from old configuration files.

## Errors and logging

- **Return `Result`, propagate with `?`, and classify at the boundary.** Win32 failures become typed errors carrying the raw code (`EnumError::from_win`). Callers match on the variant, never on the message text.
- **No `.unwrap()` in new non-test code** (a few remain in `src/index/persist.rs` and `src/index/snapshot.rs`). Use `.expect()` only for a true invariant, and make its message state that invariant:

  ```rust
  ✅ self.heap.peek().expect("a full heap has a root")
  ✅ ….expect("the shipped default configuration must parse")    // src/config/file.rs, abbreviated
  ❌ self.heap.peek().unwrap()
  ❌ config.expect("failed")
  ```

- **A failure on a share is never fatal.** Keep the last good index, record the failure in the share's health, and retry with backoff.
- **Messages a user sees** name the thing and the problem in plain words: `"credentials rejected for V:\shared\files"`. They don't show error codes. Developer detail (`(os error 1326)`) appears only with `dev_mode = true`.
- **Logging** uses the `log` macros, with lowercase messages that carry the path or value involved:

  ```rust
  log::warn!("could not re-tint the tray icon: {err}");
  ```

  Use them sparingly: `warn!` for something the user may need to know, `info!`/`debug!` for tracing. No `println!` outside `files-cli` output, and no leftover debug prints.

## Comments and docs

- **Every file starts with `//!`** saying what the module is for and anything surprising about it (`src/search/mod.rs` is the one file still missing it).
- **`///` on public items explains why and what for**, plus the contract (invariants, panics, cost). Don't repeat the signature.
- **Comments state constraints the code can't show.** For example: why a buffer is 64 KiB (a network path rejects larger), why a call happens on a particular thread, which Windows behavior is being worked around.

  ```rust
  ✅ /// The largest buffer a network path accepts. See constraint 1.    // src/index/win_watch.rs
  ❌ /// The buffer size.
  ```

- **No commented-out code, and no `TODO` without an issue link.**

## Architecture rules

These keep the program testable and the UI responsive. [ARCHITECTURE.md](ARCHITECTURE.md) explains them in depth.

1. **`src/app` is pure.** `AppState::update(event, now)` does no I/O, reads no clock, sends on no channel and never names `egui`. Effects leave as `Cmd` values, and only `Actors::dispatch` executes them.
2. **`src/view` produces words, not widgets.** Its functions turn state into text runs with an emphasis. Colors and layout belong in `src/gui`.
3. **Dependencies point one way:** `gui → view → app`. Nothing in `app` or `view` imports `gui`.
4. **The UI thread never waits on a share.** Anything that can touch the network runs on a worker and reports back through `Events`.
5. **One writer per piece of state.** Each share's index slot is written only by its owner (its index actor, or the live worker for a live share) and read lock-free by everyone else.
6. **No new long-lived threads after start-up.** New work goes to an existing worker through its `LatestSlot` or channel. The only short-lived threads are one-shot helpers for blocking calls (clipboard, saving the viewer choice).

## Testing

- **Every behavior change comes with a test that fails without it.** Bug fixes start with the failing test.
- **Prefer the pure layers.** Most behavior can be tested through `AppState::update` with a fake clock (`tests/state_machine.rs`) or `Scheduler::on` (`src/index/schedule.rs`), with no threads and no network.
- **Never use a real share in a test.** Use `FakeDirSource` for listings, failures, hangs and call counts.
- **Make tests deterministic.** Drive time through the `now` parameter, and wait on conditions with a deadline, never with a bare `sleep`.
- **Use property tests (`proptest`) for parsers and invariants** such as the query parser, page-set collection, scheduler spacing and matcher parity. Commit their regression files.
- **UI changes need snapshot tests** updated and inspected (see [CONTRIBUTING.md](../CONTRIBUTING.md#tests)).

## Dependencies

- **Add a crate only when the standard library and `windows-sys` really cannot do it.** The project uses its own WinHTTP client, BCrypt hashing and config parser on purpose.
- **Every new dependency in `Cargo.toml` gets a comment saying why it is there**, and why its feature set is what it is. (Some older ones, such as `rayon`, `parking_lot` and `lru`, predate the rule.)

  ```toml
  # Span-aware parsing for `file:line:col` config errors without serde or `syn`;
  # `display` enables format-preserving writes to the hand-edited config.
  toml_edit = { version = "0.22", default-features = false, features = ["parse", "display"] }
  ```

- **Turn `default-features` off and opt into what is used.** Commit `Cargo.lock`. CI builds with `--locked`.
- **A new `windows-sys` feature** gets a comment naming the API it is for.
