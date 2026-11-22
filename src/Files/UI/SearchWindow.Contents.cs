using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>Files found by what is inside them, from kensaku, listed after the name results once they come back.</summary>
public partial class SearchWindow
{
    private CancellationTokenSource? _contentCancel;

    /// <summary>The content matches for <see cref="_contentsFor"/>, the line they were searched for.</summary>
    private IReadOnlyList<ContentMatch>? _contents;
    private string? _contentsFor;

    /// <summary>Why the last content search gave nothing, said once until it changes.</summary>
    private string? _contentProblem;

    /// <summary>Whether the bundled kensaku is up, asked before each content search.</summary>
    private readonly KensakuLink _kensaku;

    /// <summary>The content matches for what is in the box now, or none (also when content search was turned off, or Folders is chosen).</summary>
    private IReadOnlyList<ContentMatch>? ContentsShown =>
        _app.Settings.ContentSearch && _filter != SearchFilter.Folders && _contentsFor == Input.Text ? _contents : null;

    /// <summary>Stops a content search in flight, when the search changes or the panel hides. The search disposes its own source.</summary>
    private void StopContentSearch()
    {
        _contentCancel?.Cancel();
        _contentCancel = null;
    }

    /// <summary>Asks kensaku for <paramref name="line"/> after the name results are on screen; a newer search cancels it.</summary>
    private async Task SearchContentsAsync(string line, int epoch)
    {
        StopContentSearch();
        Settings settings = _app.Settings;
        if (!settings.ContentSearch || !IsVisible)
        {
            return;
        }
        var cancel = new CancellationTokenSource();
        // Taken once: StopContentSearch may let go of the source while this waits.
        CancellationToken token = cancel.Token;
        _contentCancel = cancel;
        ContentResult result;
        try
        {
            result = await _kensaku.SearchAsync(line, token);
        }
        catch (Exception) when (token.IsCancellationRequested)
        {
            return;
        }
        finally
        {
            if (_contentCancel == cancel)
            {
                _contentCancel = null;
            }
            cancel.Dispose();
        }
        if (epoch != _epoch || Input.Text != line || !IsVisible)
        {
            return;
        }
        ShowContents(line, result);
    }

    private void ShowContents(string line, ContentResult result)
    {
        // Rows drawn for this line before its contents came have none, so they are redrawn even when the matches
        // are the same as the last line's; and also when matches shown before are gone now.
        bool changed = !(_contents ?? []).Select(match => match.Path).SequenceEqual(result.Matches.Select(match => match.Path));
        bool redraw = _contentsFor != line || changed;
        _contents = result.Matches;
        _contentsFor = line;
        if (result.Problem != _contentProblem)
        {
            _contentProblem = result.Problem;
            if (result.Problem is not null)
            {
                ShowToast($"Content search unavailable · {result.Problem}");
            }
        }
        if (_live is null && _result is SearchResult shown && redraw)
        {
            ShowResults(shown);
        }
    }
}
