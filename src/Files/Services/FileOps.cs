using Files.Core;

namespace Files.Services;

/// <summary>Sends a file or folder to the Recycle Bin and says whether it is there now (RecycleBin.Send); tests give their own.</summary>
internal delegate bool Recycler(string path, bool isFolder);

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
        DateTime started = DateTime.UtcNow;
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
            FileMoves.Rename(path, target, was.IsFolder, caseOnly);
            return new FileResult(path, target, FileOutcome.Done) { Was = was };
        });
        return new FileBatch(FileOpKind.Rename, [result]) { Started = started, Finished = DateTime.UtcNow };
    }

    /// <summary>
    /// Sends files and folders to the Recycle Bin, or deletes them for good. A path with no Recycle Bin
    /// (see <see cref="RecycleBin.HasBin"/>) is only deleted when <paramref name="permanently"/> says so.
    /// An item the shell deleted for good after all is reported so, and undo leaves it.
    /// </summary>
    public static FileBatch Delete(IReadOnlyList<string> paths, bool permanently, IProgress<FileProgress>? progress = null) =>
        Delete(paths, permanently, progress, RecycleBin.Send);

    internal static FileBatch Delete(IReadOnlyList<string> paths, bool permanently, IProgress<FileProgress>? progress, Recycler recycle)
    {
        DateTime started = DateTime.UtcNow;
        var results = new List<FileResult>();
        foreach (string path in paths)
        {
            results.Add(Attempt(path, () => DeleteOne(path, permanently, recycle)));
            progress?.Report(new FileProgress(results.Count, paths.Count));
        }
        return new FileBatch(FileOpKind.Delete, results, permanently) { Started = started, Finished = DateTime.UtcNow };
    }

    /// <summary>
    /// Takes back what <paramref name="batch"/> did: copies go to the Recycle Bin, moved and renamed files go
    /// back where they were, and deleted files come back from the Recycle Bin. Nothing is overwritten,
    /// nothing is deleted for good, and a file that changed since is left alone.
    /// </summary>
    public static FileBatch Undo(FileBatch batch, IProgress<FileProgress>? progress = null) => Undo(batch, progress, RecycleBin.Send);

    internal static FileBatch Undo(FileBatch batch, IProgress<FileProgress>? progress, Recycler recycle)
    {
        DateTime started = DateTime.UtcNow;
        List<FileResult> undoable = batch.Undoable.ToList();
        var results = new List<FileResult>();
        foreach (FileResult result in undoable)
        {
            results.Add(Attempt(result.Target ?? result.Source, () => UndoOne(batch, result, recycle)));
            progress?.Report(new FileProgress(results.Count, undoable.Count));
        }
        FileOpKind kind = batch.Kind switch
        {
            FileOpKind.Copy => FileOpKind.Delete,
            FileOpKind.Delete => FileOpKind.Copy,
            _ => batch.Kind,
        };
        return new FileBatch(kind, results) { Undoes = batch.Kind, Started = started, Finished = DateTime.UtcNow };
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

    private static FileBatch Run(FileOpKind kind, IReadOnlyList<FileSnapshot> sources, string folder,
        IProgress<FileProgress>? progress, Func<FileSnapshot, FileResult> act)
    {
        DateTime started = DateTime.UtcNow;
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
        return new FileBatch(kind, results) { Started = started, Finished = DateTime.UtcNow };
    }

    private static FileResult CopyOne(FileSnapshot source, string folder)
    {
        string name = Path.GetFileName(source.Path);
        if (!source.IsFolder)
        {
            string target = Path.Combine(folder, FreeName(folder, name));
            File.Copy(source.Path, target, overwrite: false);
            return new FileResult(source.Path, target, FileOutcome.Done);
        }
        if (IsWithin(folder, source.Path))
        {
            return new FileResult(source.Path, null, FileOutcome.Failed, $"{name} cannot go inside itself");
        }
        // Made here and only here, so a copy that fails removes nothing that was there before.
        string copy = FileTree.CreateFresh(folder, name);
        FileTree.CopyInto(source.Path, copy);
        return new FileResult(source.Path, copy, FileOutcome.Done) { CopiedTree = FileTree.Read(copy) };
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
        return FileMoves.MoveTo(source.Path, Path.Combine(folder, FreeName(folder, name)), source.IsFolder);
    }

    private static FileResult DeleteOne(string path, bool permanently, Recycler recycle)
    {
        // Checked before the path is read: an unreachable share can take a while to answer.
        if (!permanently && !RecycleBin.HasBin(path))
        {
            return new FileResult(path, null, FileOutcome.Failed, $"{Path.GetFileName(path)} is on a drive with no Recycle Bin");
        }
        if (FileSnapshot.Take(path) is not FileSnapshot was)
        {
            return new FileResult(path, null, FileOutcome.NotFound);
        }
        if (permanently)
        {
            DeleteForGood(path, was.IsFolder);
            return new FileResult(path, null, FileOutcome.Done) { Was = was };
        }
        bool landed = recycle(path, was.IsFolder);
        return new FileResult(path, null, FileOutcome.Done) { Was = was, InRecycleBin = landed };
    }

    private static FileResult UndoOne(FileBatch batch, FileResult done, Recycler recycle) => batch.Kind switch
    {
        FileOpKind.Copy => RemoveCopy(done, recycle),
        FileOpKind.Delete => Restore(batch, done),
        _ => PutBack(done),
    };

    /// <summary>Sends a copy to the Recycle Bin, only if nobody has worked on it since and it can go there.</summary>
    private static FileResult RemoveCopy(FileResult done, Recycler recycle)
    {
        string copy = done.Target!;
        string name = Path.GetFileName(copy);
        // Checked before the path is read, as for a delete; a copy is never deleted for good.
        if (!RecycleBin.HasBin(copy))
        {
            return new FileResult(copy, null, FileOutcome.Failed, $"{name} was kept: it is on a drive with no Recycle Bin");
        }
        if (FileSnapshot.Take(copy) is not FileSnapshot now)
        {
            return new FileResult(copy, null, FileOutcome.NotFound);
        }
        if (!StillAsCopied(done, now))
        {
            return new FileResult(copy, null, FileOutcome.Changed, $"{name} changed since it was copied, so it was kept");
        }
        bool landed = recycle(copy, now.IsFolder);
        return new FileResult(copy, null, FileOutcome.Done) { Was = now, InRecycleBin = landed };
    }

    /// <summary>A copied file keeps its original's size and date; a copied folder holds just what it held after copying.</summary>
    private static bool StillAsCopied(FileResult done, FileSnapshot now)
    {
        if (!now.IsFolder)
        {
            return done.Was is { IsFolder: false } was && now.Size == was.Size && now.WrittenUtc == was.WrittenUtc;
        }
        return done.CopiedTree is { } copied && !FileTree.HasLinks(now.Path) && FileTree.Same(FileTree.Read(now.Path), copied, dates: true);
    }

    /// <summary>Moves a file back, only if it is as it was moved (a folder only has to be there) and nothing took its place.</summary>
    private static FileResult PutBack(FileResult done)
    {
        string now = done.Target!;
        if (FileSnapshot.Take(now) is not FileSnapshot moved)
        {
            return new FileResult(now, null, FileOutcome.NotFound);
        }
        bool changed = !moved.IsFolder && (done.Was is not { } was || moved.Size != was.Size || moved.WrittenUtc != was.WrittenUtc);
        if (changed)
        {
            return new FileResult(now, null, FileOutcome.Changed, $"{Path.GetFileName(now)} changed since it was moved, so it was left there");
        }
        if (!SamePath(now, done.Source) && Exists(done.Source))
        {
            return new FileResult(now, null, FileOutcome.Exists, $"Something else is at {done.Source} now");
        }
        FileResult back = FileMoves.MoveTo(now, done.Source, moved.IsFolder, caseOnly: SamePath(now, done.Source));
        return back with { Was = moved };
    }

    /// <summary>Restores a deleted item: only one deleted from that path while this batch ran, and of the same size.</summary>
    private static FileResult Restore(FileBatch batch, FileResult done)
    {
        long? size = done.Was is { IsFolder: false } was ? was.Size : null;
        string? problem = RecycleBin.Restore(done.Source, batch.Started, batch.Finished, size);
        if (problem is not null)
        {
            return new FileResult(done.Source, null, FileOutcome.Failed, problem);
        }
        return new FileResult(done.Source, done.Source, FileOutcome.Done) { Was = done.Was };
    }

    private static void DeleteForGood(string path, bool isFolder)
    {
        if (isFolder)
        {
            Directory.Delete(path, recursive: true);
        }
        else
        {
            File.Delete(path);
        }
    }

    /// <summary>
    /// Runs one file's step, turning whatever it throws into an outcome, so one odd error neither stops the
    /// batch nor loses what was already done (which undo needs).
    /// </summary>
    private static FileResult Attempt(string path, Func<FileResult> act)
    {
        try
        {
            return act();
        }
        catch (Exception error)
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

    internal static bool Exists(string path) => File.Exists(path) || Directory.Exists(path);

    internal static bool SamePath(string? path, string other) =>
        path is not null && string.Equals(Path.TrimEndingDirectorySeparator(path), Path.TrimEndingDirectorySeparator(other), StringComparison.OrdinalIgnoreCase);

    /// <summary>Whether <paramref name="path"/> is <paramref name="folder"/> or somewhere inside it.</summary>
    private static bool IsWithin(string path, string folder) => IndexNote.IsUnder(folder, path);
}
