namespace Files.Tests;

/// <summary>A folder under %TEMP% that is deleted when the test ends.</summary>
internal sealed class TempFolder : IDisposable
{
    public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"files-test-{Guid.NewGuid():N}");

    public TempFolder() => Directory.CreateDirectory(Path);

    public string File(string relative, string contents = "")
    {
        string full = System.IO.Path.Combine(Path, relative);
        Directory.CreateDirectory(System.IO.Path.GetDirectoryName(full)!);
        System.IO.File.WriteAllText(full, contents);
        return full;
    }

    public void Dispose()
    {
        Microsoft.Data.Sqlite.SqliteConnection.ClearAllPools();
        foreach (string file in Directory.EnumerateFiles(Path, "*", SearchOption.AllDirectories))
        {
            System.IO.File.SetAttributes(file, FileAttributes.Normal);
        }
        Directory.Delete(Path, recursive: true);
    }
}
