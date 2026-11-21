using Files.Core;

namespace Files.Services;

public enum FileOpKind
{
    Copy,
    Move,
    Rename,
    Delete,
}

/// <summary>What became of one file or folder in an operation.</summary>
public enum FileOutcome
{
    Done,

    /// <summary>Changed since it was copied or cut, so it was left alone.</summary>
    Changed,

    /// <summary>Something of that name is in the way, or it is where it was to go already.</summary>
    Exists,

    /// <summary>Open in another program.</summary>
    Locked,
    Denied,

    /// <summary>Gone, or on a share that cannot be reached.</summary>
    NotFound,
    Failed,
}

/// <summary>A file or folder as it was when it was copied or cut, so a paste can tell whether it changed since.</summary>
public sealed record FileSnapshot(string Path, bool IsFolder, long Size, DateTime WrittenUtc)
{
    /// <summary>What is at <paramref name="path"/> now, or null if nothing is (or it cannot be reached).</summary>
    public static FileSnapshot? Take(string path)
    {
        try
        {
            var file = new FileInfo(path);
            if (file.Exists)
            {
                return new FileSnapshot(path, false, file.Length, file.LastWriteTimeUtc);
            }
            var folder = new DirectoryInfo(path);
            return folder.Exists ? new FileSnapshot(path, true, 0, folder.LastWriteTimeUtc) : null;
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or ArgumentException or NotSupportedException)
        {
            return null;
        }
    }

    /// <summary>
    /// Why it should be left alone now, or null if it is as it was. A folder only has to still be there:
    /// its date moves whenever anything inside it changes.
    /// </summary>
    internal FileOutcome? Problem()
    {
        if (Take(Path) is not FileSnapshot now)
        {
            return FileOutcome.NotFound;
        }
        bool same = now.IsFolder == IsFolder && (IsFolder || (now.Size == Size && now.WrittenUtc == WrittenUtc));
        return same ? null : FileOutcome.Changed;
    }
}

/// <summary>One file or folder of an operation: where it was, where it went, and how it went.</summary>
public sealed record FileResult(string Source, string? Target, FileOutcome Outcome, string? Message = null)
{
    /// <summary>The source as it was before the operation, for undo to check against.</summary>
    public FileSnapshot? Was { get; init; }
}

/// <summary>How far an operation has got: <see cref="Done"/> of <see cref="Total"/> files and folders.</summary>
public readonly record struct FileProgress(int Done, int Total);

/// <summary>A finished operation, which can be taken back unless it deleted for good or was itself an undo.</summary>
/// <param name="Kind">What the operation did to the files. Undoing a copy deletes, and undoing a delete brings files back as a copy does.</param>
public sealed record FileBatch(FileOpKind Kind, IReadOnlyList<FileResult> Results, bool Permanent = false)
{
    /// <summary>The operation this one took back, or null.</summary>
    public FileOpKind? Undoes { get; init; }

    public IEnumerable<FileResult> Done => Results.Where(result => result.Outcome == FileOutcome.Done);

    public bool CanUndo => !Permanent && Undoes is null && Done.Any();

    /// <summary>The paths that are no longer where the saved lists have them, and the paths those lists do not have yet.</summary>
    public (IReadOnlyList<string> Removed, IReadOnlyList<NotedPath> Added) Changes()
    {
        List<string> removed = Kind == FileOpKind.Copy ? [] : Done.Select(result => result.Source).ToList();
        List<NotedPath> added = Done
            .Where(result => result.Target is not null)
            .Select(result => new NotedPath(result.Target!, result.Was?.IsFolder == true))
            .ToList();
        return (removed, added);
    }
}
