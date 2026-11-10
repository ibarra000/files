namespace Files.Core;

public static class AtomicFile
{
    /// <summary>Writes through a temporary file and a rename, so readers never see half a file.</summary>
    public static void WriteAllText(string path, string text)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(Path.GetFullPath(path))!);
        string temporary = $"{path}.{Environment.ProcessId}.tmp";
        File.WriteAllText(temporary, text);
        File.Move(temporary, path, overwrite: true);
    }
}
