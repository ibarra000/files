namespace Files.Core;

/// <summary>
/// Steps through past searches in the search box, as a terminal does with past commands: Up goes to an
/// older search and Down to a newer one. What was typed before the first Up is held as a draft, and Down
/// past the newest search brings it back. The draft is never added to the history.
/// </summary>
public sealed class Recall
{
    private IReadOnlyList<string> _history = [];
    private int _index = -1;
    private string _draft = "";

    /// <summary>Whether a past search is shown, rather than what was typed.</summary>
    public bool IsActive => _index >= 0;

    /// <summary>Starts over with this history and forgets the draft; the next <see cref="Older"/> returns the newest entry.</summary>
    public void Reset(IReadOnlyList<string> history)
    {
        _history = history;
        _index = -1;
        _draft = "";
    }

    /// <summary>The next older search, or null when there is none. The first step keeps <paramref name="current"/> as the draft.</summary>
    public string? Older(string current)
    {
        if (_index + 1 >= _history.Count)
        {
            return null;
        }
        if (_index < 0)
        {
            _draft = current;
            if (_history[0] == current && _history.Count > 1)
            {
                // The newest search is what is already typed: showing it again would look like a key that did nothing.
                _index++;
            }
        }
        _index++;
        return _history[_index];
    }

    /// <summary>The next newer search; past the newest, the draft, which ends the stepping. Null when not stepping.</summary>
    public string? Newer()
    {
        if (_index < 0)
        {
            return null;
        }
        _index--;
        if (_index == 0 && _history[0] == _draft)
        {
            // Skipped on the way up, so skipped on the way down.
            _index--;
        }
        return _index < 0 ? _draft : _history[_index];
    }
}
