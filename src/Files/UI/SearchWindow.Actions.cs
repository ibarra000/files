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
        string? undo = _lastBatch is { CanUndo: true } batch ? FileOpText.UndoLabel(batch) : null;
        IReadOnlyList<RowAction> actions = RowActions.For(row, _app.Settings, _app.Favorites, Input.Text, ClipboardHasFiles(), undo);
        if (actions.Count == 0)
        {
            return;
        }
        _actionRow = row;
        _actions = actions;
        _actionFilter = "";
        ShowActions();
        // Beside the row, at the right of the list, as on the design; the popup keeps itself on screen.
        // The offsets place the card itself: the Popup already puts its child's margin (the shadow's room) outside them.
        ActionMenu.HorizontalOffset = Math.Max(0, Results.ActualWidth - ActionMenuWidth);
        ActionMenu.VerticalOffset = Results.ItemContainerGenerator.ContainerFromItem(row) is FrameworkElement item
            ? item.TranslatePoint(new Point(0, item.ActualHeight), Results).Y
            : 0;
        ActionMenu.IsOpen = true;
        UpdateStatus();
    }

    /// <summary>The list changed under an open menu: it stays only while its row is still listed.</summary>
    private void KeepActionsFor(IReadOnlyList<ResultRow> rows)
    {
        // The same file or folder, though its row was rebuilt (a favorite found missing, a live search moving on).
        if (ActionMenu.IsOpen && (_actionRow is null || !rows.Any(row => SameItem(row, _actionRow))))
        {
            ActionMenu.IsOpen = false;
        }
    }

    private static bool SameItem(ResultRow row, ResultRow other) =>
        row.Path is string path ? row.Kind == other.Kind && path == other.Path : row.Equals(other);

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
            default:
                RunFileAction(action.Kind, row);
                break;
        }
    }

    private void RunFileAction(ActionKind kind, ResultRow row)
    {
        switch (kind)
        {
            case ActionKind.Copy:
                PutOnClipboard(cut: false, row);
                break;
            case ActionKind.Cut:
                PutOnClipboard(cut: true, row);
                break;
            case ActionKind.Paste:
                Paste(row);
                break;
            case ActionKind.Rename:
                StartRename(row);
                break;
            case ActionKind.Delete:
                DeleteSelected(permanently: false, row);
                break;
            case ActionKind.Undo:
                UndoLast();
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
        int at = Results.SelectedIndex;
        _app.RemoveOpened(path);
        ShowToast($"Removed {row.Before} from recent");
        if (IsHome)
        {
            ShowHome();
            // The next one down takes its place, so Delete can be pressed again.
            int target = Math.Clamp(at, 0, Math.Max(0, Results.Items.Count - 1));
            if ((NextSelectable(target, 1) ?? NextSelectable(target, -1)) is int next)
            {
                Results.SelectedIndex = next;
            }
        }
    }
}
