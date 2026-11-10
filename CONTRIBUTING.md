# Contributing to files

## Setup

- Windows 10 or 11, x64
- [.NET 10 SDK](https://dotnet.microsoft.com/download) (`winget install Microsoft.DotNet.SDK.10`)
- Any editor; Visual Studio 2026 or VS Code with the C# Dev Kit work well
- For installers only: the WiX 5 tool, `dotnet tool install --global wix --version 5.0.2` and `wix extension add --global WixToolset.UI.wixext/5.0.2`

## Build and run

```powershell
dotnet build Files.slnx            # warnings are errors
dotnet run --project src/Files     # starts the tray app; Settings opens if no volume is set up
```

The app keeps its data in `%APPDATA%\files` and `%LOCALAPPDATA%\files` (see [ARCHITECTURE.md](docs/ARCHITECTURE.md#storage)). Delete those folders to start over.

## Tests

```powershell
dotnet test Files.slnx
dotnet test Files.slnx --collect:"XPlat Code Coverage"
```

Tests cover `Core`, `Data` and `Services` against temporary folders and databases. `SearchTests.A_million_names_search_in_milliseconds` prints the search time; run it in Release (`-c Release`) for a meaningful number. UI changes are checked by running the app: take a look in light and dark, and with a volume that is unreachable.

## Workflow

1. **Branch from `master`** with a short kebab-case name that says what the work is: `settings-window`, `result-columns`.
2. **Commit in small steps** that each build and pass their tests.
3. **Write commit subjects as `type: what changes, in plain words`**: lowercase, present tense, no trailing period, describing what the user sees rather than which files changed.

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

   ```text
   feat: dock the panel along the top or bottom of the screen
   fix: two update checks at once no longer share a download
   ```

4. **Open a pull request against `master`.** CI must be green.
5. PRs land as merge commits, which keeps each branch's commits visible in history.

### Changes that need extra care

- **Database schemas.** Add the next numbered script in `src/Files/Data/Sql`; never edit one that has shipped.
- **Network behavior** (`Core/Walker.cs`, `Services/Indexer.cs`). Try it against a real share as well as a local folder, and say in the PR what you ran it against.
- **New P/Invoke.** Only where .NET has no managed API. See [CODE_STANDARDS.md](docs/CODE_STANDARDS.md#pinvoke).
- **New packages.** Prefer the base class library. Say in the PR why a package is needed.

## Releasing

Maintainers only:

1. Bump `<Version>` in `src/Files/Files.csproj` and commit `chore: release <version>`.
2. Build and publish the installer:

   ```powershell
   pwsh tools/make_msi.ps1                                   # artifacts\files-<version>-x64.msi
   pwsh tools/make_msi.ps1 -Publish \\server\share\files-updates
   pwsh tools/make_msi.ps1 -Release                          # GitHub release v<version>
   ```

   `-Publish` copies the MSI to a shared folder and writes `latest.json` beside it, for installs that set an update folder. `-Release` attaches both to GitHub release `v<version>`; the `v` prefix is required, because the updater downloads from that tag.

## Getting help

Open an issue with the steps to reproduce and what the Volumes page in Settings shows (remove server names if they are private).
