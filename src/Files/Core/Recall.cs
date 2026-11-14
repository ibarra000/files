namespace Files.Core;

/// <summary>
/// Steps back through past searches in the search box, as a terminal does with
/// past commands: Up on the top row goes back. Down moves into what a recalled
/// search found, and Esc clears the box.
/// </summary>
public sealed class Recall
{
    private IReadOnlyList<string> _history = [];
    private int _index = -1;

    public bool IsActive => _index >= 0;

    /// <summary>Starts over with this history; the next <see cref="Older"/> returns the newest entry.</summary>
    public void Reset(IReadOnlyList<string> history)
    {
        _history = history;
        _index = -1;
    }

    /// <summary>The next older search, or null when there is none.</summary>
    public string? Older()
    {
        if (_index + 1 >= _history.Count)
        {
            return null;
        }
        _index++;
        return _history[_index];
    }
}
