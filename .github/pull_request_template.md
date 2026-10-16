## What changes for the user

<!-- One or two sentences, in the same voice as the commit subject. -->

## How it was tested

- [ ] `sh tools/check.sh` passes (fmt, clippy, unsafe allowlist, naming, vocabulary, tests)
- [ ] UI change: `cargo test --features ui-snapshots` passes, and any updated `tests/snapshots/*.png` were looked at
- [ ] Network behavior change: tried against a real share with `files-cli --doctor` / `--bench`

## Checklist

- [ ] No new `unsafe` outside `tools/unsafe-allowlist.txt`; every `unsafe` block has a `// SAFETY:` comment
- [ ] New tests are named as sentences about behavior
- [ ] Docs updated if behavior, config keys, or architecture changed (`README.md`, `docs/`)
- [ ] Commit subjects follow `type: what the user sees` (see CONTRIBUTING.md)
