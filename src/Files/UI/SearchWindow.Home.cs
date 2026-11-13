using System.Windows.Controls;
using Files.Core;

namespace Files.UI;

/// <summary>The empty-box list (favorites and recently opened), favorites, and recalling past searches.</summary>
public partial class SearchWindow
{
    private readonly Recall _recall = new();
    private bool _recalling;
    private bool _showAllFavorites;
    private IReadOnlySet<string> _missing = new HashSet<string>();

    /// <param name="keepSelection">Stay on the same item (after a favorite changes) rather than the top row.</param>
    private void ShowHome(bool keepSelection = false)
    {
        // A search still running must not replace this list when it finishes.
        int epoch = ++_epoch;
        _result = null;
        _queryProblem = null;
        if (_app.Favorites.Count == 0)
        {
            _showAllFavorites = false;
        }
        ShowHomeRows(keepSelection ? SelectedRow?.Item : null);
        UpdateStatus();
        _ = MarkMissingAsync(epoch);
    }

    /// <summary>Lists the home rows, selecting <paramref name="select"/> if it is still there, else the top row.</summary>
    private void ShowHomeRows(Favorite? select)
    {
        // Hidden, for instance, while the screen is shared.
        IReadOnlyList<string> opened = _app.Settings.ShowRecentlyOpened ? _app.Opened : [];
        List<ResultRow> rows = HomeRows.Build(_app.Favorites, opened, _missing, _showAllFavorites);
        Results.ItemsSource = rows;
        int found = -1;
        if (select is not null)
        {
            found = rows.FindIndex(row => row.Item is not null && row.Item.Matches(select));
        }
        if (found < 0)
        {
            found = NextSelectable(0, 1) ?? -1;
        }
        Results.SelectedIndex = found;
    }

    /// <summary>Greys out favorites that are gone. Checked off the UI thread: an unreachable share can take seconds.</summary>
    private async Task MarkMissingAsync(int epoch)
    {
        IReadOnlyList<Favorite> favorites = _app.Favorites;
        IReadOnlySet<string> missing = await Task.Run(() => HomeRows.FindMissing(favorites));
        if (epoch != _epoch || missing.SetEquals(_missing))
        {
            return;
        }
        _missing = missing;
        ShowHomeRows(SelectedRow?.Item);
    }

    private ResultRow? SelectedRow => Results.SelectedItem as ResultRow;

    /// <summary>The first row at or after <paramref name="from"/>, stepping by <paramref name="step"/>, that is not a section header.</summary>
    private int? NextSelectable(int from, int step)
    {
        for (int i = from; i >= 0 && i < Results.Items.Count; i += step)
        {
            if (Results.Items[i] is ResultRow { IsHeader: false })
            {
                return i;
            }
        }
        return null;
    }

    /// <summary>What Ctrl+D adds or removes: the selected file or home item, else the search in the box.</summary>
    private Favorite? FavoriteFor(ResultRow? row)
    {
        if (row is not null && RowActions.FavoriteOf(row) is Favorite favorite)
        {
            return favorite;
        }
        string search = Input.Text.Trim();
        return search.Length > 0 ? new Favorite(FavoriteKind.Search, search) : null;
    }

    private void ToggleFavorite(Favorite? favorite)
    {
        if (favorite is null)
        {
            ShowToast("Select a file, or type a search, to add it to favorites");
            return;
        }
        bool added = _app.ToggleFavorite(favorite);
        string name = favorite.Kind == FavoriteKind.Search ? $"\"{favorite.Target}\"" : Path.GetFileName(favorite.Target.TrimEnd('\\'));
        ShowToast(added ? $"Added {name} to favorites" : $"Removed {name} from favorites");
        if (Input.Text.Length == 0)
        {
            ShowHome(keepSelection: true);
        }
    }

    /// <summary>Up recalls past searches while the box is empty or showing one, and the top row is selected.</summary>
    private bool CanRecall()
    {
        bool boxFree = Input.Text.Length == 0 || _recall.IsActive;
        int top = NextSelectable(0, 1) ?? -1;
        return boxFree && Results.SelectedIndex <= top;
    }

    private void ShowRecalled(string? search)
    {
        if (search is null)
        {
            // Already at the oldest search.
            return;
        }
        _recalling = true;
        Input.Text = search;
        Input.CaretIndex = search.Length;
        _recalling = false;
    }

    /// <summary>Shows the panel searching for <paramref name="search"/>, or with <paramref name="problem"/> when there is nothing to search.</summary>
    public void ShowWithSearch(string? search, string? problem)
    {
        ShowPanel();
        if (search is null)
        {
            ShowToast(problem);
            return;
        }
        RunSearch(search);
        Input.SelectAll();
        if (problem is not null)
        {
            // Something worth knowing about how it was read, such as a fallback to Windows OCR.
            ShowToast(problem);
        }
    }

    /// <summary>Puts a saved search in the box and runs it.</summary>
    private void RunSearch(string search)
    {
        if (Input.Text == search)
        {
            _ = SearchAsync();
            return;
        }
        _pasted = true;
        Input.Text = search;
        Input.CaretIndex = search.Length;
    }
}
