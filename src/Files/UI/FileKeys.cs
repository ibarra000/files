namespace Files.UI;

/// <summary>What has Del, F2, Ctrl+X and Ctrl+V in the panel (docs/UI_STATES.md).</summary>
public enum KeyOwner
{
    /// <summary>The search box: the keys edit the search, and never touch a file.</summary>
    Box,

    /// <summary>The selected result: the user went to it with the arrows, Page Up or Down, or the mouse.</summary>
    Results,

    /// <summary>The rename box over a row's name.</summary>
    Renaming,
}

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

/// <summary>Where a press of Del or Shift+Del goes.</summary>
public enum DeleteTarget
{
    /// <summary>The search box edits its text as usual.</summary>
    Box,

    /// <summary>The selected result is deleted, after asking when it is for good.</summary>
    File,

    /// <summary>Del at the end of the box, where it does nothing: the footer says how to delete the file.</summary>
    Hint,
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

    /// <summary>
    /// Del acts on a file only from the results. Shift+Del also acts from the box when no text is selected
    /// there, since it has nothing to cut: a lone result is highlighted without the user going to it.
    /// </summary>
    public static DeleteTarget ActsOnFile(KeyOwner owner, bool shift, bool boxHasSelection, bool caretAtEnd, bool hasHit)
    {
        if (!hasHit || owner == KeyOwner.Renaming)
        {
            return DeleteTarget.Box;
        }
        if (owner == KeyOwner.Results || (shift && !boxHasSelection))
        {
            return DeleteTarget.File;
        }
        return !shift && caretAtEnd && !boxHasSelection ? DeleteTarget.Hint : DeleteTarget.Box;
    }

    public static bool CanStillUndo(DateTime doneAt, DateTime now) => now - doneAt <= UndoFor;
}
