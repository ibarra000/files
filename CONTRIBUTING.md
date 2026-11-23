# Contributing to files

## Setup

- Windows 10 or 11, x64
- [.NET 10 SDK](https://dotnet.microsoft.com/download) (`winget install Microsoft.DotNet.SDK.10`)
- Any editor; Visual Studio 2026 or VS Code with the C# Dev Kit work well
- For installers only: the WiX 5 tool, `dotnet tool install --global wix --version 5.0.2` and `wix extension add --global WixToolset.UI.wixext/5.0.2`
- For content search (the bundled kensaku, see [KENSAKU.md](docs/KENSAKU.md)): `pwsh tools/fetch_kensaku.ps1`, which downloads the pinned kensaku-headless release and checks its hash. No Rust is needed. A build run from source finds it in `artifacts\kensaku`; without it, content search says "kensaku is not fetched". CI does not fetch kensaku.

## Build and run

```powershell
dotnet build Files.slnx            # warnings are errors
dotnet run --project src/Files     # starts the tray app; Settings opens if no volume is set up
```

The app keeps its data in `%APPDATA%\files` and `%LOCALAPPDATA%\files` (see [ARCHITECTURE.md](docs/ARCHITECTURE.md#storage)). Delete those folders to start over.

To run a build beside the installed app, give it a folder of its own: `$env:FILES_HOME = "$env:TEMP\files-dev"; dotnet run --project src/Files`. It keeps its settings and volumes there and runs as a separate instance; running it again with the same `FILES_HOME` shows its panel. Its shortcuts clash with the installed app's, so set others in its Settings.

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
   The release machine needs the signing certificate (subject `CN=files`, thumbprint `5EAA01DFACB29C29C8D7C21F961F616CFB6B3728`) in `Cert:\CurrentUser\My` and signtool from the Windows SDK. Keep a backup of the certificate's private key (an exported, password-protected `.pfx`) somewhere safe and off the repository: installed copies refuse updates signed with anything else.
2. Build and publish the installer:

   ```powershell
   pwsh tools/make_msi.ps1                                   # artifacts\files-<version>-x64.msi
   pwsh tools/make_msi.ps1 -Publish \\server\share\files-updates
   pwsh tools/make_msi.ps1 -Release                          # GitHub release v<version>
   ```

   The script fetches kensaku first (`tools/fetch_kensaku.ps1`); `-SkipKensaku` leaves content search out of the installer. `-Publish` copies the MSI to a shared folder and writes `latest.json` beside it, for installs that set an update folder. `-Release` attaches both to GitHub release `v<version>`; the `v` prefix is required, because the updater downloads from that tag. The script signs `files.exe` and the MSI, and stops if either is not signed by the release certificate.

### An installer with preset volumes

For one organisation's PCs, `-Volumes` builds a list of volumes into the installer:

```powershell
pwsh tools/make_msi.ps1 -Volumes volumes.json -Publish \\server\share\files-updates
```

`volumes.json` is a JSON array, for example `[{ "name": "Drawings", "folder": "\\\\server\\drawings" }, { "name": "Archive", "folder": "\\\\server\\archive", "liveOnly": true }]`. It is installed beside `files.exe`, and at start-up each volume in it is added once for each Windows user on that PC (recorded in `%LOCALAPPDATA%\files\bundled.txt`, beside the volume list). Comments and trailing commas are allowed. After that the volume belongs to the user: editing or removing it sticks, and a volume added to the list in a later installer is added on upgrade. A folder that is already a volume is left as it is. `-Volumes` cannot be combined with `-Release`, since a public release is for everyone. Publish it to the organisation's update folder instead, so updates keep the list.

## Getting help

Open an issue with the steps to reproduce and what the Volumes page in Settings shows (remove server names if they are private).
