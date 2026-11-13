using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>The chips and the volume menu that narrow a search, and the result count beside the box.</summary>
public partial class SearchWindow
{
    private SearchFilter _filter = SearchFilter.All;
    private ContextMenu? _scopeMenu;

    /// <summary>The one volume searched, by name, or null for all of them; kept until the app closes.</summary>
    private string? _scope;

    /// <summary>How long the last search of the saved lists took; null for a live search.</summary>
    private TimeSpan? _searchTook;

    /// <summary>The volumes a search reads: all of them, or the one chosen in the volume menu.</summary>
    private IReadOnlyList<VolumeState> ScopedVolumes()
    {
        IReadOnlyList<VolumeState> all = _app.Indexer.Volumes;
        if (_scope is null || !all.Any(state => state.Volume.Name == _scope))
        {
            // A volume removed in Settings falls back to all of them.
            _scope = null;
            return all;
        }
        return all.Where(state => state.Volume.Name == _scope).ToList();
    }

    private void OnChipChecked(object sender, RoutedEventArgs e)
    {
        if (sender is not RadioButton { Tag: SearchFilter filter } || filter == _filter)
        {
            return;
        }
        // All, Files and Folders only choose among the results already found; Code and Documents change the search.
        bool queryChanged = ChangesQuery(_filter) || ChangesQuery(filter);
        _filter = filter;
        RunAgain(queryChanged);
    }

    private static bool ChangesQuery(SearchFilter filter) => filter is SearchFilter.Code or SearchFilter.Documents;

    /// <summary>Ctrl+1 to Ctrl+5 pick a chip.</summary>
    private void SetFilter(SearchFilter filter)
    {
        RadioButton? chip = Chips.Children.OfType<RadioButton>().FirstOrDefault(button => button.Tag is SearchFilter f && f == filter);
        if (chip is not null)
        {
            chip.IsChecked = true;
        }
    }

    private void OnScopeClick(object sender, RoutedEventArgs e)
    {
        var menu = new ContextMenu { PlacementTarget = ScopeButton, Placement = PlacementMode.Bottom };
        AddScopeItem(menu, "All volumes", null);
        foreach (VolumeState state in _app.Indexer.Volumes)
        {
            AddScopeItem(menu, state.Volume.Name, state.Volume.Name);
        }
        // Kept here, not as the button's context menu, so a right-click can never reopen an old one.
        _scopeMenu = menu;
        menu.IsOpen = true;
    }

    private void AddScopeItem(ContextMenu menu, string header, string? scope)
    {
        var item = new MenuItem { Header = header, IsCheckable = true, IsChecked = _scope == scope };
        item.Click += (_, _) =>
        {
            _scope = scope;
            RunAgain(queryChanged: true);
        };
        menu.Items.Add(item);
    }

    /// <summary>Repeats the search on screen with the new chip or volume, live if it was live.</summary>
    /// <param name="queryChanged">False when only the rows change: they are rebuilt from the result on screen,
    /// so a live search keeps the folders it has already read.</param>
    private void RunAgain(bool queryChanged)
    {
        UpdateStatus();
        if (IsHome)
        {
            return;
        }
        if (!queryChanged && _result is SearchResult result)
        {
            ShowResults(result);
            return;
        }
        _ = _live is not null ? StartLiveSearchAsync() : SearchAsync();
    }

    /// <summary>The chips and count show while searching; the volume menu only when there is more than one volume.</summary>
    private void UpdateHeader(IReadOnlyList<VolumeState> volumes)
    {
        bool searching = Input.Text.Trim().Length > 0;
        Chips.Visibility = searching ? Visibility.Visible : Visibility.Collapsed;
        ScopeButton.Visibility = volumes.Count > 1 ? Visibility.Visible : Visibility.Collapsed;
        ScopeLabel.Text = _scope ?? "All volumes";
        if (searching && _result is SearchResult result)
        {
            Count.Text = StatusText.Count(Shown(result), _live is null ? _searchTook : null);
            Count.Visibility = Visibility.Visible;
        }
        else
        {
            Count.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>How many results the chip lets through: files counted in full, folders as found (at most <see cref="Searcher.MaxFolders"/>).</summary>
    private int Shown(SearchResult result) => _filter switch
    {
        SearchFilter.Folders => result.Folders.Count,
        SearchFilter.All => result.Matched + result.Folders.Count,
        _ => result.Matched,
    };
}
