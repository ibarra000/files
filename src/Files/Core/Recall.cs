namespace Files.Core;

/// <summary>
/// Steps through past searches in the search box, as a terminal does with
/// past commands: Up goes back, Down comes forward, and past the newest the
/// box is empty again.
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

    /// <summary>The next newer search, or "" after the newest, which also ends the recall.</summary>
    public string Newer()
    {
        if (_index <= 0)
        {
            _index = -1;
            return "";
        }
        _index--;
        return _history[_index];
    }
}
