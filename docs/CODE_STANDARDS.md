# Code standards

The checks that enforce these are the compiler (`TreatWarningsAsErrors`, nullable reference types on) and the tests. Everything else is for reviewers.

## Style

- Standard .NET conventions: `PascalCase` for types and members, `camelCase` for locals, `_camelCase` for private fields, file-scoped namespaces, one main type per file named after it.
- Readable over short. Use braces on every `if`, early returns over nesting, and named constants over magic numbers. Don't squeeze logic into one line to save lines.
- Functions under about 50 lines, files under about 400.
- Prefer immutable data: `record`s with `init` properties and `with`, `IReadOnlyList`, and swapping whole immutable collections rather than locking around mutation.

## Layers

`Core` knows nothing about SQLite or WPF. `Data` knows nothing about WPF. Only `UI` and `App.xaml.cs` touch windows. Keep logic that can be tested out of code-behind.

## Errors

- Catch the specific exceptions an operation is known to throw (`IOException`, `UnauthorizedAccessException`, `SqliteException`, …), never bare `Exception`.
- Anything that can fail in a way the user can act on returns or shows a message in plain words ("That folder is already a volume.").
- An exception escaping a WPF event handler, an `async void` method or a timer callback ends the process, so those catch what they call.
- A failed scan never discards the last good index.

## P/Invoke

Only where .NET has no managed API. Today that is `RegisterHotKey` (`Services/Hotkey.cs`), the DWM attributes (`UI/Backdrop.cs`), a folder's file id (`CreateFileW` + `GetFileInformationByHandle` in `Core/Walker.cs`, so a followed link is read once), Windows' file thumbnails, `SHCreateItemFromParsingName` with `GetObject`/`DeleteObject` for the bitmap it gives (`Services/ShellThumbnail.cs`), making a folder only if nothing is there (`CreateDirectoryW` in `Services/FileTree.cs`; `Directory.CreateDirectory` also succeeds on one that exists), and telling a subst drive from its disk (`QueryDosDeviceW` in `Services/RecycleBin.cs`). Use `DllImport` with blittable signatures, keep each call in the class that needs it, and don't enable unsafe code.

COM follows the same rule. The one use is restoring from the Recycle Bin (`Services/RecycleBin.cs`): `Shell.Application`, created with `Type.GetTypeFromProgID` and called late-bound through `InvokeMember` on an STA thread of its own, with no interop assembly and no `dynamic`. Its failures (`COMException`, `TargetInvocationException`, …) become a message, never a crash. An item's deletion time and size are read from its `$I` record beside it rather than from shell properties. Sending to the Recycle Bin uses `Microsoft.VisualBasic.FileIO.FileSystem`, which is part of .NET's Windows Desktop runtime, so no package is needed, with `UIOption.AllDialogs`: the lighter option tells the shell not to confirm, and it then deletes for good, silently, whatever cannot go to the Recycle Bin.

## Vocabulary

`files` is a general-purpose tool, so names, messages, test data and docs use general words:

| Say | For |
|---|---|
| **volume** | a folder the user added to search |
| **search input** / **term** | what the user types |
| **folder** | a directory in a volume |
| **favorite** | a file, folder or saved search pinned to the home list |
| **program** | a program set up under Opening to open files with |

Sample data should look like sample data: `AB-C-0704.pdf`, `report.pdf`, `\\server\share`.

## Tests

- xUnit, named as sentences about behavior: `A_search_that_extends_the_newest_replaces_it`.
- Use real temporary folders and databases (`TempFolder`) rather than mocks.
- New behavior in `Core`, `Data` or `Services` comes with tests.
