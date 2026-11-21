using Files.Services;

namespace Files.UI;

/// <summary>What the footer says while files are copied, moved, renamed or deleted, and afterwards.</summary>
public static class FileOpText
{
    /// <summary>"Moving 3 of 12…".</summary>
    public static string Progress(string verb, FileProgress step) => $"{verb} {step.Done:N0} of {step.Total:N0}…";

    /// <summary>The action menu's line for taking back <paramref name="batch"/>.</summary>
    public static string UndoLabel(FileBatch batch) => $"Undo {Noun(batch.Kind)}";

    /// <summary>What happened, then what went wrong, then how to undo it.</summary>
    /// <param name="undoKeys">The keys that take it back, as a sentence writes them.</param>
    public static string Done(FileBatch batch, string undoKeys = "Ctrl+Z")
    {
        List<FileResult> done = batch.Done.ToList();
        List<FileResult> failed = batch.Results.Where(result => result.Outcome != FileOutcome.Done).ToList();
        if (batch.Results.Count == 0)
        {
            return "Nothing changed";
        }
        if (done.Count == 0 && failed.Count == 1)
        {
            return Problem(failed[0]);
        }
        var parts = new List<string>();
        if (done.Count > 0)
        {
            parts.Add(What(batch, done));
        }
        parts.AddRange(failed
            .GroupBy(result => result.Outcome)
            .Select(group => $"{group.Count():N0} {Short(group.Key)}"));
        if (batch.CanUndo)
        {
            parts.Add($"{undoKeys} to undo");
        }
        return string.Join(" · ", parts);
    }

    private static string What(FileBatch batch, List<FileResult> done)
    {
        string one = Path.GetFileName(done[0].Target ?? done[0].Source);
        string items = done.Count == 1 ? one : $"{done.Count:N0} items";
        if (batch.Undoes is FileOpKind undone)
        {
            return $"Undid the {Noun(undone)} of {(done.Count == 1 ? Path.GetFileName(done[0].Source) : items)}";
        }
        return batch.Kind switch
        {
            FileOpKind.Copy => $"Copied {items}",
            FileOpKind.Move => $"Moved {items}",
            FileOpKind.Rename => $"Renamed to {one}",
            FileOpKind.Delete when batch.Permanent => $"Deleted {items}",
            _ => $"Moved {items} to the Recycle Bin",
        };
    }

    /// <summary>One file's problem as a sentence.</summary>
    private static string Problem(FileResult result)
    {
        string name = Path.GetFileName(result.Source);
        return result.Outcome switch
        {
            FileOutcome.Changed => result.Message ?? $"{name} changed since it was copied, so it was left alone",
            FileOutcome.Exists => result.Message ?? $"{name} is already there",
            FileOutcome.Locked => $"{name} is open in another program",
            FileOutcome.Denied => $"Not allowed to change {name}",
            FileOutcome.NotFound => result.Message ?? $"{name} was not found, or its share cannot be reached",
            _ => result.Message ?? $"{name} could not be changed",
        };
    }

    private static string Short(FileOutcome outcome) => outcome switch
    {
        FileOutcome.Changed => "changed since copied",
        FileOutcome.Exists => "already there",
        FileOutcome.Locked => "open in another program",
        FileOutcome.Denied => "not allowed",
        FileOutcome.NotFound => "not found",
        _ => "failed",
    };

    private static string Noun(FileOpKind kind) => kind switch
    {
        FileOpKind.Copy => "copy",
        FileOpKind.Move => "move",
        FileOpKind.Rename => "rename",
        _ => "delete",
    };
}
