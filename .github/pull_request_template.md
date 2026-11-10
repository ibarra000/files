## What changes for the user

<!-- One or two sentences, in the same voice as the commit subject. -->

## How it was tested

- [ ] `dotnet build Files.slnx` and `dotnet test Files.slnx` pass
- [ ] UI change: looked at it in the running app, in light and dark
- [ ] Network behavior change: tried against a real share (say which kind)

## Checklist

- [ ] New tests are named as sentences about behavior
- [ ] Schema changes are a new numbered script in `src/Files/Data/Sql`, not an edit to a shipped one
- [ ] Docs updated if behavior or architecture changed (`README.md`, `docs/`)
- [ ] Commit subjects follow `type: what the user sees` (see CONTRIBUTING.md)
