namespace Files.UI;

/// <summary>What a press of Del or Shift+Del does when the footer may have asked about deleting for good.</summary>
public enum DeleteAnswer
{
    /// <summary>Ask in the footer; the same key again confirms.</summary>
    Ask,

    /// <summary>This press answers the question: delete.</summary>
    Confirm,

    /// <summary>Too soon after asking to be a second press (a key held down, or a bounce): nothing happens.</summary>
    Ignore,
}

/// <summary>A delete for good the footer asked about: which file, which key (Shift+Del or Del), and when.</summary>
public sealed record DeleteAsk(string Path, bool Permanently, DateTime AskedAt);

/// <summary>The timing rules for the panel's file keys.</summary>
public static class FileKeys
{
    /// <summary>The least time between the press that asks and the one that confirms, so a held key never confirms.</summary>
    public static readonly TimeSpan ConfirmGap = TimeSpan.FromMilliseconds(300);

    /// <summary>How long the question stands: as long as the footer shows it.</summary>
    public static readonly TimeSpan ConfirmWindow = TimeSpan.FromSeconds(4);

    /// <summary>How long Ctrl+Z and the menu can still take the last operation back, if the panel stays open.</summary>
    public static readonly TimeSpan UndoFor = TimeSpan.FromMinutes(2);

    /// <summary>Only the same key on the same file, between the gap and the end of the window, confirms.</summary>
    public static DeleteAnswer Answer(DeleteAsk? asked, string path, bool permanently, DateTime now)
    {
        bool same = asked is not null && asked.Permanently == permanently
            && string.Equals(asked.Path, path, StringComparison.OrdinalIgnoreCase);
        if (!same)
        {
            return DeleteAnswer.Ask;
        }
        TimeSpan since = now - asked!.AskedAt;
        if (since < ConfirmGap)
        {
            return DeleteAnswer.Ignore;
        }
        return since <= ConfirmWindow ? DeleteAnswer.Confirm : DeleteAnswer.Ask;
    }

    public static bool CanStillUndo(DateTime doneAt, DateTime now) => now - doneAt <= UndoFor;
}
