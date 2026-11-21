using System.Runtime.InteropServices;

namespace Files.Services;

/// <summary>One file or folder under a folder, by its path relative to that folder. A folder has size -1 and no date.</summary>
internal sealed record TreeEntry(string Relative, long Size, DateTime WrittenUtc);

/// <summary>
/// Copying a folder so that a failure removes only what this copy made, and comparing folders file by
/// file, so a copy is only deleted (by undo, or as the original of a move) when it is known to be whole.
/// </summary>
internal static class FileTree
{
    private const int AlreadyExists = 183;

    /// <summary>Whether anything under <paramref name="folder"/> is a link (a junction, a symbolic link or a mount point).</summary>
    public static bool HasLinks(string folder)
    {
        foreach (FileSystemInfo entry in new DirectoryInfo(folder).EnumerateFileSystemInfos("*", Recursive()))
        {
            if (entry.Attributes.HasFlag(FileAttributes.ReparsePoint))
            {
                return true;
            }
        }
        return false;
    }

    /// <summary>Every file and folder under <paramref name="folder"/>, links not followed, in a fixed order.</summary>
    public static List<TreeEntry> Read(string folder)
    {
        var entries = new List<TreeEntry>();
        foreach (FileSystemInfo entry in new DirectoryInfo(folder).EnumerateFileSystemInfos("*", Recursive()))
        {
            string relative = Path.GetRelativePath(folder, entry.FullName);
            entries.Add(entry is FileInfo file
                ? new TreeEntry(relative, file.Length, file.LastWriteTimeUtc)
                : new TreeEntry(relative, -1, default));
        }
        entries.Sort((a, b) => string.Compare(a.Relative, b.Relative, StringComparison.OrdinalIgnoreCase));
        return entries;
    }

    /// <summary>The same names and sizes, and with <paramref name="dates"/> the same dates (a copy keeps its original's).</summary>
    public static bool Same(IReadOnlyList<TreeEntry> a, IReadOnlyList<TreeEntry> b, bool dates)
    {
        if (a.Count != b.Count)
        {
            return false;
        }
        for (int i = 0; i < a.Count; i++)
        {
            bool same = string.Equals(a[i].Relative, b[i].Relative, StringComparison.OrdinalIgnoreCase)
                && a[i].Size == b[i].Size
                && (!dates || a[i].WrittenUtc == b[i].WrittenUtc);
            if (!same)
            {
                return false;
            }
        }
        return true;
    }

    /// <summary>
    /// Makes a new folder named <paramref name="name"/> in <paramref name="parent"/>, or "name (1)", "name (2)", …
    /// if taken. Windows makes it only if nothing is there, so an existing folder is never taken as this one.
    /// </summary>
    public static string CreateFresh(string parent, string name)
    {
        string stem = Path.GetFileNameWithoutExtension(name);
        string extension = Path.GetExtension(name);
        for (int number = 0; ; number++)
        {
            string candidate = Path.Combine(parent, number == 0 ? name : $"{stem} ({number}){extension}");
            if (!File.Exists(candidate) && TryCreate(candidate))
            {
                return candidate;
            }
        }
    }

    /// <summary>Makes exactly this folder, failing with "already exists" if anything is there.</summary>
    public static void CreateExactly(string path)
    {
        if (File.Exists(path) || !TryCreate(path))
        {
            throw new IOException($"{path} already exists", unchecked((int)0x800700B7));
        }
    }

    /// <summary>
    /// Copies what is in <paramref name="source"/> into <paramref name="target"/>, a folder this copy just made.
    /// Linked folders are not followed. If any of it fails, what this copy made is removed, the target
    /// with it, and the error passed on.
    /// </summary>
    public static void CopyInto(string source, string target)
    {
        var made = new List<string> { target };
        try
        {
            CopyTree(source, target, made);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            RemoveMade(made);
            throw;
        }
    }

    /// <summary>Removes a folder this run made and fully copied: everything in it is the run's own.</summary>
    public static void RemoveCopy(string target)
    {
        try
        {
            Directory.Delete(target, recursive: true);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // What is left is a copy, never the original; the caller reports the original as kept.
        }
    }

    private static void CopyTree(string source, string target, List<string> made)
    {
        foreach (FileInfo file in new DirectoryInfo(source).EnumerateFiles())
        {
            string to = Path.Combine(target, file.Name);
            file.CopyTo(to, overwrite: false);
            made.Add(to);
        }
        foreach (DirectoryInfo folder in new DirectoryInfo(source).EnumerateDirectories())
        {
            // Not followed, as a scan does not follow one: it could lead back up into this folder.
            if (folder.Attributes.HasFlag(FileAttributes.ReparsePoint))
            {
                continue;
            }
            string to = Path.Combine(target, folder.Name);
            CreateExactly(to);
            made.Add(to);
            CopyTree(folder.FullName, to, made);
        }
    }

    /// <summary>Deletes what a failed copy made, newest first, and nothing else: a folder only if it is empty by then.</summary>
    private static void RemoveMade(List<string> made)
    {
        for (int i = made.Count - 1; i >= 0; i--)
        {
            try
            {
                if (File.Exists(made[i]))
                {
                    File.Delete(made[i]);
                }
                else
                {
                    Directory.Delete(made[i], recursive: false);
                }
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                // Left in place: the first error is the one to report, and a part copy still overwrote nothing.
            }
        }
    }

    private static EnumerationOptions Recursive() => new()
    {
        RecurseSubdirectories = true,
        AttributesToSkip = 0,
        IgnoreInaccessible = false,
    };

    private static bool TryCreate(string path)
    {
        if (CreateDirectoryW(path, IntPtr.Zero))
        {
            return true;
        }
        int error = Marshal.GetLastPInvokeError();
        if (error == AlreadyExists)
        {
            return false;
        }
        throw new IOException($"{path} could not be made", Marshal.GetHRForLastWin32Error());
    }

    // Directory.CreateDirectory succeeds on a folder that already exists; this fails instead.
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern bool CreateDirectoryW(string path, IntPtr security);
}
