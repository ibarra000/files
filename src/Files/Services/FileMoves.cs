namespace Files.Services;

/// <summary>
/// Moving one file or folder to an exact new path, never overwriting. On the same drive or share it is a
/// rename. To another one it is a copy, checked against the original, and only then is the original deleted.
/// </summary>
internal static class FileMoves
{
    private const int NotSameDevice = unchecked((int)0x80070011);

    /// <summary>Moves to <paramref name="target"/>; file system errors are left to the caller, which reports them per file.</summary>
    public static FileResult MoveTo(string source, string target, bool isFolder, bool caseOnly = false)
    {
        if (FileOps.SameVolume(source, target))
        {
            try
            {
                Rename(source, target, isFolder, caseOnly);
                return new FileResult(source, target, FileOutcome.Done);
            }
            catch (IOException error) when (error.HResult == NotSameDevice)
            {
                // A drive mounted in a folder: the same root, but another volume. Copy it instead.
            }
        }
        return MoveAcross(source, target, isFolder);
    }

    /// <summary>Renames in place, without overwriting.</summary>
    /// <param name="caseOnly">Only the case changes; a folder goes through a name of its own on the way, as some file systems refuse that rename.</param>
    public static void Rename(string source, string target, bool isFolder, bool caseOnly)
    {
        if (!isFolder)
        {
            File.Move(source, target, overwrite: false);
            return;
        }
        if (!caseOnly)
        {
            Directory.Move(source, target);
            return;
        }
        string step = Path.Combine(Path.GetDirectoryName(source)!, $"{Path.GetFileName(source)}.renaming-{Guid.NewGuid():N}");
        Directory.Move(source, step);
        try
        {
            Directory.Move(step, target);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            Directory.Move(step, source);
            throw;
        }
    }

    /// <summary>
    /// Copies to another drive, checks the copy, then deletes the original. A folder holding links is not
    /// moved (they could not come along), and one that changed while it was copied stays where it was.
    /// </summary>
    internal static FileResult MoveAcross(string source, string target, bool isFolder)
    {
        return isFolder ? MoveFolderAcross(source, target) : MoveFileAcross(source, target);
    }

    private static FileResult MoveFileAcross(string source, string target)
    {
        File.Copy(source, target, overwrite: false);
        // The copy is new (nothing was overwritten), so it is this move's own to delete again.
        if (new FileInfo(target).Length != new FileInfo(source).Length)
        {
            File.Delete(target);
            return new FileResult(source, null, FileOutcome.Failed, $"The copy of {Path.GetFileName(source)} came out a different size, so it was not moved");
        }
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
        return new FileResult(source, target, FileOutcome.Done);
    }

    private static FileResult MoveFolderAcross(string source, string target)
    {
        string name = Path.GetFileName(source);
        if (FileTree.HasLinks(source))
        {
            return new FileResult(source, null, FileOutcome.Failed, $"{name} holds linked folders, so it was not moved to another drive");
        }
        List<TreeEntry> before = FileTree.Read(source);
        FileTree.CreateExactly(target);
        FileTree.CopyInto(source, target);
        if (!FileTree.Same(before, FileTree.Read(source), dates: true))
        {
            FileTree.RemoveCopy(target);
            return new FileResult(source, null, FileOutcome.Changed, $"{name} changed while it was copied, so it was left where it was");
        }
        if (!FileTree.Same(before, FileTree.Read(target), dates: false))
        {
            FileTree.RemoveCopy(target);
            return new FileResult(source, null, FileOutcome.Failed, $"The copy of {name} did not match it, so it was not moved");
        }
        try
        {
            Directory.Delete(source, recursive: true);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException)
        {
            return new FileResult(source, target, FileOutcome.CopiedOnly,
                $"{name} was copied to {target}, but some of the original could not be deleted: {error.Message}");
        }
        return new FileResult(source, target, FileOutcome.Done);
    }
}
