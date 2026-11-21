using Files.Core;

namespace Files.Services;

/// <summary>
/// Copying, moving, renaming and deleting files and folders for the panel. Nothing is ever overwritten:
/// a name already taken gets a number, as in "report (1).pdf". Each file is tried on its own, so one that
/// fails does not stop the rest.
/// </summary>
public static class FileOps
{
    private const int SharingViolation = unchecked((int)0x80070020);
    private const int LockViolation = unchecked((int)0x80070021);
    private const int FileExists = unchecked((int)0x80070050);
    private const int AlreadyExists = unchecked((int)0x800700B7);
    private const int NotSameDevice = unchecked((int)0x80070011);
    private const int BadNetPath = unchecked((int)0x80070035);
    private const int NetNameDeleted = unchecked((int)0x80070040);
    private const int BadNetName = unchecked((int)0x80070043);

    public static FileBatch Copy(IReadOnlyList<FileSnapshot> sources, string folder, IProgress<FileProgress>? progress = null) =>
        Run(FileOpKind.Copy, sources, folder, progress, source => CopyOne(source, folder));

    public static FileBatch Move(IReadOnlyList<FileSnapshot> sources, string folder, IProgress<FileProgress>? progress = null) =>
        Run(FileOpKind.Move, sources, folder, progress, source => MoveOne(source, folder));

    /// <summary>Gives a file or folder a new name in the same folder. A name already taken is refused, not numbered.</summary>
    public static FileBatch Rename(string path, string name)
    {
        string old = Path.GetFileName(path);
        if (name == old)
        {
            return new FileBatch(FileOpKind.Rename, []);
        }
        if (NameProblem(name) is string problem)
        {
            return new FileBatch(FileOpKind.Rename, [new FileResult(path, null, FileOutcome.Failed, problem)]);
        }
        string target = Path.Combine(Path.GetDirectoryName(path)!, name);
        FileResult result = Attempt(path, () =>
        {
            if (FileSnapshot.Take(path) is not FileSnapshot was)
            {
                return new FileResult(path, null, FileOutcome.NotFound);
            }
            // Only the case changing is the same file, which Windows would otherwise say is in the way.
            bool caseOnly = string.Equals(name, old, StringComparison.OrdinalIgnoreCase);
            if (!caseOnly && Exists(target))
            {
                return new FileResult(path, null, FileOutcome.Exists, $"{name} is already there") { Was = was };
            }
            MoveInPlace(path, target, was.IsFolder);
            return new FileResult(path, target, FileOutcome.Done) { Was = was };
        });
        return new FileBatch(FileOpKind.Rename, [result]);
    }

    /// <summary>
    /// Sends files and folders to the Recycle Bin, or deletes them for good. A network path has no
    /// Recycle Bin, and Windows would delete it for good without asking, so it is only deleted when
    /// <paramref name="permanently"/> says so.
    /// </summary>
    public static FileBatch Delete(IReadOnlyList<string> paths, bool permanently, IProgress<FileProgress>? progress = null)
    {
        var results = new List<FileResult>();
        foreach (string path in paths)
        {
            results.Add(Attempt(path, () => DeleteOne(path, permanently)));
            progress?.Report(new FileProgress(results.Count, paths.Count));
        }
        return new FileBatch(FileOpKind.Delete, results, permanently);
    }

    /// <summary>
    /// Takes back what <paramref name="batch"/> did: copies are deleted, moved and renamed files go back
    /// where they were, and deleted files come back from the Recycle Bin. Nothing is overwritten, and a
    /// copy that changed since is left alone.
    /// </summary>
    /// <param name="permanently">Delete copies for good rather than send them to the Recycle Bin.</param>
    public static FileBatch Undo(FileBatch batch, bool permanently = false, IProgress<FileProgress>? progress = null)
    {
        List<FileResult> done = batch.Done.ToList();
        var results = new List<FileResult>();
        foreach (FileResult result in done)
        {
            results.Add(Attempt(result.Target ?? result.Source, () => UndoOne(batch.Kind, result, permanently)));
            progress?.Report(new FileProgress(results.Count, done.Count));
        }
        FileOpKind kind = batch.Kind switch
        {
            FileOpKind.Copy => FileOpKind.Delete,
            FileOpKind.Delete => FileOpKind.Copy,
            _ => batch.Kind,
        };
        return new FileBatch(kind, results) { Undoes = batch.Kind };
    }

    /// <summary>The name itself if it is free in <paramref name="folder"/>, else the first of "name (1).ext", "name (2).ext", … that is.</summary>
    public static string FreeName(string folder, string name)
    {
        if (!Exists(Path.Combine(folder, name)))
        {
            return name;
        }
        string stem = Path.GetFileNameWithoutExtension(name);
        string extension = Path.GetExtension(name);
        for (int number = 1; ; number++)
        {
            string candidate = $"{stem} ({number}){extension}";
            if (!Exists(Path.Combine(folder, candidate)))
            {
                return candidate;
            }
        }
    }

    /// <summary>Why Windows would refuse <paramref name="name"/> as a file name, in words, or null.</summary>
    public static string? NameProblem(string name)
    {
        if (name.Trim().Length == 0)
        {
            return "A name cannot be empty";
        }
        if (name is "." or "..")
        {
            return $"{name} cannot be a name";
        }
        if (name.IndexOfAny(Path.GetInvalidFileNameChars()) >= 0)
        {
            return "A name cannot contain \\ / : * ? \" < > |";
        }
        if (name.EndsWith('.') || name.EndsWith(' '))
        {
            return "A name cannot end with a dot or a space";
        }
        return null;
    }

    /// <summary>Whether two paths are on the same drive or share, where a move is a rename.</summary>
    public static bool SameVolume(string path, string other) =>
        string.Equals(Path.GetPathRoot(path), Path.GetPathRoot(other), StringComparison.OrdinalIgnoreCase);

    /// <summary>A UNC path or a mapped network drive, which has no Recycle Bin.</summary>
    public static bool IsOnNetwork(string path) => Walker.IsOnNetwork(Path.GetPathRoot(path) ?? path);

    private static FileBatch Run(FileOpKind kind, IReadOnlyList<FileSnapshot> sources, string folder,
        IProgress<FileProgress>? progress, Func<FileSnapshot, FileResult> act)
    {
        var results = new List<FileResult>();
        bool folderThere = Directory.Exists(folder);
        foreach (FileSnapshot source in sources)
        {
            FileResult result;
            if (!folderThere)
            {
                result = new FileResult(source.Path, null, FileOutcome.NotFound, $"{folder} is not there any more");
            }
            else if (source.Problem() is FileOutcome problem)
            {
                result = new FileResult(source.Path, null, problem);
            }
            else
            {
                result = Attempt(source.Path, () => act(source));
            }
            results.Add(result with { Was = source });
            progress?.Report(new FileProgress(results.Count, sources.Count));
        }
        return new FileBatch(kind, results);
    }

    private static FileResult CopyOne(FileSnapshot source, string folder)
    {
        if (source.IsFolder && IsWithin(folder, source.Path))
        {
            return new FileResult(source.Path, null, FileOutcome.Failed, $"{Path.GetFileName(source.Path)} cannot go inside itself");
        }
        string target = Path.Combine(folder, FreeName(folder, Path.GetFileName(source.Path)));
        if (source.IsFolder)
        {
            CopyFolder(source.Path, target);
        }
        else
        {
            File.Copy(source.Path, target, overwrite: false);
        }
        return new FileResult(source.Path, target, FileOutcome.Done);
    }

    private static FileResult MoveOne(FileSnapshot source, string folder)
    {
        string name = Path.GetFileName(source.Path);
        if (SamePath(Path.GetDirectoryName(source.Path), folder))
        {
            return new FileResult(source.Path, null, FileOutcome.Exists, $"{name} is in that folder already");
        }
        if (source.IsFolder && IsWithin(folder, source.Path))
        {
            return new FileResult(source.Path, null, FileOutcome.Failed, $"{name} cannot go inside itself");
        }
        string target = Path.Combine(folder, FreeName(folder, name));
        MoveTo(source.Path, target, source.IsFolder);
        return new FileResult(source.Path, target, FileOutcome.Done);
    }

    private static FileResult DeleteOne(string path, bool permanently)
    {
        // Checked before the path is touched: an unreachable share can take a while to answer.
        if (!permanently && IsOnNetwork(path))
        {
            return new FileResult(path, null, FileOutcome.Failed, $"{Path.GetFileName(path)} is on the network, which has no Recycle Bin");
        }
        if (FileSnapshot.Take(path) is not FileSnapshot was)
        {
            return new FileResult(path, null, FileOutcome.NotFound);
        }
        Remove(path, was.IsFolder, permanently);
        return new FileResult(path, null, FileOutcome.Done) { Was = was };
    }

    private static FileResult UndoOne(FileOpKind kind, FileResult done, bool permanently) => kind switch
    {
        FileOpKind.Copy => RemoveCopy(done, permanently),
        FileOpKind.Delete => Restore(done),
        _ => PutBack(done),
    };

    private static FileResult RemoveCopy(FileResult done, bool permanently)
    {
        string copy = done.Target!;
        if (FileSnapshot.Take(copy) is not FileSnapshot now)
        {
            return new FileResult(copy, null, FileOutcome.NotFound);
        }
        // A copy keeps its original's size and date; if either moved, someone has worked on it since.
        if (done.Was is { IsFolder: false } was && (now.Size != was.Size || now.WrittenUtc != was.WrittenUtc))
        {
            return new FileResult(copy, null, FileOutcome.Changed, $"{Path.GetFileName(copy)} changed since it was copied, so it was kept");
        }
        // A copy on the network has no Recycle Bin to go to; it is only the copy just made.
        Remove(copy, now.IsFolder, permanently || IsOnNetwork(copy));
        return new FileResult(copy, null, FileOutcome.Done) { Was = now };
    }

    private static FileResult PutBack(FileResult done)
    {
        string now = done.Target!;
        if (FileSnapshot.Take(now) is not FileSnapshot moved)
        {
            return new FileResult(now, null, FileOutcome.NotFound);
        }
        if (!SamePath(now, done.Source) && Exists(done.Source))
        {
            return new FileResult(now, null, FileOutcome.Exists, $"Something else is at {done.Source} now");
        }
        MoveTo(now, done.Source, moved.IsFolder);
        return new FileResult(now, done.Source, FileOutcome.Done) { Was = moved };
    }

    private static FileResult Restore(FileResult done)
    {
        string? problem = RecycleBin.Restore(done.Source);
        if (problem is not null)
        {
            return new FileResult(done.Source, null, FileOutcome.Failed, problem);
        }
        return new FileResult(done.Source, done.Source, FileOutcome.Done) { Was = done.Was };
    }

    /// <summary>
    /// Moves without overwriting. On the same drive or share Windows renames it; to another one it is
    /// copied, and the original deleted once the copy is complete.
    /// </summary>
    private static void MoveTo(string source, string target, bool isFolder)
    {
        if (SameVolume(source, target))
        {
            try
            {
                MoveInPlace(source, target, isFolder);
                return;
            }
            catch (IOException error) when (error.HResult == NotSameDevice)
            {
                // A drive mounted in a folder: the same root, but another volume. Copy it instead.
            }
        }
        if (isFolder)
        {
            CopyFolder(source, target);
            Directory.Delete(source, recursive: true);
            return;
        }
        File.Copy(source, target, overwrite: false);
        try
        {
            File.Delete(source);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            // The original stays, so the copy goes: a move that half happened would leave two.
            File.Delete(target);
            throw;
        }
    }

    private static void MoveInPlace(string source, string target, bool isFolder)
    {
        if (isFolder)
        {
            Directory.Move(source, target);
        }
        else
        {
            File.Move(source, target, overwrite: false);
        }
    }

    /// <summary>Copies a folder to a new one; if any of it fails, what was copied is removed again and the error passed on.</summary>
    private static void CopyFolder(string source, string target)
    {
        try
        {
            CopyTree(source, target);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            try
            {
                Directory.Delete(target, recursive: true);
            }
            catch (Exception cleanup) when (cleanup is IOException or UnauthorizedAccessException)
            {
                // The first error is the one to report; a part copy left behind still never overwrote anything.
            }
            throw;
        }
    }

    private static void CopyTree(string source, string target)
    {
        Directory.CreateDirectory(target);
        foreach (string file in Directory.EnumerateFiles(source))
        {
            File.Copy(file, Path.Combine(target, Path.GetFileName(file)), overwrite: false);
        }
        foreach (var folder in new DirectoryInfo(source).EnumerateDirectories())
        {
            // A linked folder is not followed, as a scan does not follow one: it could lead back up into this one.
            if (!folder.Attributes.HasFlag(FileAttributes.ReparsePoint))
            {
                CopyTree(folder.FullName, Path.Combine(target, folder.Name));
            }
        }
    }

    private static void Remove(string path, bool isFolder, bool permanently)
    {
        if (!permanently)
        {
            RecycleBin.Send(path, isFolder);
        }
        else if (isFolder)
        {
            Directory.Delete(path, recursive: true);
        }
        else
        {
            File.Delete(path);
        }
    }

    /// <summary>Runs one file's step, turning what the file system throws into an outcome so the batch goes on.</summary>
    private static FileResult Attempt(string path, Func<FileResult> act)
    {
        try
        {
            return act();
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException
            or NotSupportedException or OperationCanceledException)
        {
            return new FileResult(path, null, OutcomeOf(error), error.Message);
        }
    }

    internal static FileOutcome OutcomeOf(Exception error) => error switch
    {
        UnauthorizedAccessException => FileOutcome.Denied,
        FileNotFoundException or DirectoryNotFoundException or DriveNotFoundException => FileOutcome.NotFound,
        IOException { HResult: SharingViolation or LockViolation } => FileOutcome.Locked,
        IOException { HResult: FileExists or AlreadyExists } => FileOutcome.Exists,
        IOException { HResult: BadNetPath or BadNetName or NetNameDeleted } => FileOutcome.NotFound,
        _ => FileOutcome.Failed,
    };

    private static bool Exists(string path) => File.Exists(path) || Directory.Exists(path);

    private static bool SamePath(string? path, string other) =>
        path is not null && string.Equals(Path.TrimEndingDirectorySeparator(path), Path.TrimEndingDirectorySeparator(other), StringComparison.OrdinalIgnoreCase);

    /// <summary>Whether <paramref name="path"/> is <paramref name="folder"/> or somewhere inside it.</summary>
    private static bool IsWithin(string path, string folder) => IndexNote.IsUnder(folder, path);
}
