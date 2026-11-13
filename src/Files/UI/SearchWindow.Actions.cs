using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>
/// The action menu (Tab or right-click on a result): every action for the row
/// with its shortcut. Typing filters it, ↑↓ move, Enter runs, Esc closes.
/// The search box keeps the keyboard throughout.
/// </summary>
public partial class SearchWindow
{
    private const double ActionMenuWidth = 300;

    private IReadOnlyList<RowAction> _actions = [];
    private string _actionFilter = "";
    private ResultRow? _actionRow;

    private void OpenActions()
    {
        if (SelectedRow is not ResultRow row)
        {
            return;
        }
        IReadOnlyList<RowAction> actions = RowActions.For(row, _app.Settings, _app.Favorites, Input.Text);
        if (actions.Count == 0)
        {
            return;
        }
        _actionRow = row;
        _actions = actions;
        _actionFilter = "";
        ShowActions();
        // Beside the row, at the right of the list, as on the design; the popup keeps itself on screen.
        ActionMenu.HorizontalOffset = Math.Max(0, Results.ActualWidth - ActionMenuWidth);
        ActionMenu.VerticalOffset = Results.ItemContainerGenerator.ContainerFromItem(row) is FrameworkElement item
            ? item.TranslatePoint(new Point(0, item.ActualHeight), Results).Y
            : 0;
        ActionMenu.IsOpen = true;
        UpdateStatus();
    }

    private void ShowActions()
    {
        IReadOnlyList<RowAction> shown = RowActions.Filter(_actions, _actionFilter);
        ActionList.ItemsSource = shown;
        ActionList.SelectedIndex = shown.Count > 0 ? 0 : -1;
        string name = _actionRow is null ? "" : _actionRow.Before + _actionRow.Match + _actionRow.After;
        ActionTitle.Text = _actionFilter.Length == 0 ? $"Actions for {name}" : $"Actions matching \"{_actionFilter}\"";
    }

    /// <summary>Keys while the menu is open. Returns false for keys that type, which <see cref="OnActionText"/> takes.</summary>
    private bool OnActionKey(Key key)
    {
        switch (key)
        {
            case Key.Escape:
            case Key.Tab:
                ActionMenu.IsOpen = false;
                return true;
            case Key.Down:
            case Key.Up:
                int count = ActionList.Items.Count;
                if (count > 0)
                {
                    ActionList.SelectedIndex = (ActionList.SelectedIndex + (key == Key.Down ? 1 : count - 1)) % count;
                    ActionList.ScrollIntoView(ActionList.SelectedItem);
                }
                return true;
            case Key.Enter:
                if (ActionList.SelectedItem is RowAction action)
                {
                    RunAction(action);
                }
                return true;
            case Key.Back:
                if (_actionFilter.Length > 0)
                {
                    _actionFilter = _actionFilter[..^1];
                    ShowActions();
                }
                return true;
            case Key.Delete:
            case Key.Left:
            case Key.Right:
            case Key.Home:
            case Key.End:
                return true;
            default:
                return false;
        }
    }

    /// <summary>Typing while the menu is open filters it instead of changing the search.</summary>
    private void OnActionText(object sender, TextCompositionEventArgs e)
    {
        if (!ActionMenu.IsOpen)
        {
            return;
        }
        e.Handled = true;
        if (e.Text.Any(char.IsControl))
        {
            return;
        }
        _actionFilter += e.Text;
        ShowActions();
    }

    private void OnActionClicked(object sender, MouseButtonEventArgs e)
    {
        if (ElementTree.AncestorOf<ListBoxItem>(e.OriginalSource)?.DataContext is RowAction action)
        {
            RunAction(action);
        }
    }

    private void OnActionMenuClosed(object? sender, EventArgs e) => UpdateStatus();

    /// <summary>Right-click selects the row under the mouse and opens its actions.</summary>
    private void OnResultRightClick(object sender, MouseButtonEventArgs e)
    {
        if (ElementTree.AncestorOf<ListBoxItem>(e.OriginalSource) is not { DataContext: ResultRow { IsHeader: false } row })
        {
            return;
        }
        Results.SelectedItem = row;
        OpenActions();
        e.Handled = true;
    }

    private void RunAction(RowAction action)
    {
        ActionMenu.IsOpen = false;
        if (_actionRow is not ResultRow row)
        {
            return;
        }
        switch (action.Kind)
        {
            case ActionKind.Open:
                Open(OpenWith.ForEnter(_app.Settings), row);
                break;
            case ActionKind.OpenDefault:
                Open(new OpenWith.Default(), row);
                break;
            case ActionKind.OpenWith when action.Viewer is Viewer viewer:
                Open(new OpenWith.Program(viewer), row);
                break;
            case ActionKind.Reveal:
                Reveal(row);
                break;
            case ActionKind.CopyPath:
                CopySelected(path: true, row);
                break;
            case ActionKind.CopyName:
                CopySelected(path: false, row);
                break;
            case ActionKind.Favorite:
                ToggleFavorite(FavoriteFor(row));
                break;
            case ActionKind.FavoriteFolder when row.Hit is Hit hit:
                ToggleFavorite(new Favorite(FavoriteKind.Folder, hit.Folder));
                break;
            case ActionKind.SaveSearch:
                ToggleFavorite(new Favorite(FavoriteKind.Search, Input.Text.Trim()));
                break;
            case ActionKind.RemoveRecent:
                RemoveRecent(row);
                break;
            case ActionKind.RunSearch when row.Item is Favorite saved:
                RunSearch(saved.Target);
                break;
        }
    }

    /// <summary>Takes a file off the Recent list (Del, or the action menu).</summary>
    private void RemoveRecent(ResultRow row)
    {
        if (row is not { Kind: RowKind.Opened, Path: string path })
        {
            return;
        }
        _app.RemoveOpened(path);
        ShowToast($"Removed {row.Before} from recent");
        if (Input.Text.Length == 0)
        {
            ShowHome();
        }
    }
}
