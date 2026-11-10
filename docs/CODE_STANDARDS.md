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

Only where .NET has no managed API. Today that is `RegisterHotKey` (`Services/Hotkey.cs`) and the DWM attributes (`UI/Backdrop.cs`). Use `DllImport` with blittable signatures, keep each call in the class that needs it, and don't enable unsafe code.

## Vocabulary

`files` is a general-purpose tool, so names, messages, test data and docs use general words:

| Say | For |
|---|---|
| **volume** | a folder the user added to search |
| **search input** / **term** | what the user types |
| **folder** | a directory in a volume |
| **document name** | the name a PDF page set shares (`NAME.pdf`, `NAME_Page2.pdf`, …) |
| **program** | a program set up under Opening to open files with |

Sample data should look like sample data: `AB-C-0704.pdf`, `report.pdf`, `\\server\share`.

## Tests

- xUnit, named as sentences about behavior: `A_search_that_extends_the_newest_replaces_it`.
- Use real temporary folders and databases (`TempFolder`) rather than mocks.
- New behavior in `Core`, `Data` or `Services` comes with tests.
