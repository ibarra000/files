using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Input;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>Opening, showing, copying and dragging out the selected file.</summary>
public partial class SearchWindow
{
    private async void Open(OpenWith with, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string path)
        {
            ShowToast("Nothing to open");
            return;
        }
        string? problem;
        try
        {
            problem = await Task.Run(() => Opener.Open(path, with));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException)
        {
            problem = $"Could not open {path}: {error.Message}";
        }
        AfterOpening(row, problem);
    }

    private void Reveal(ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string path)
        {
            ShowToast("Nothing to show");
            return;
        }
        AfterOpening(row, Opener.Reveal(path));
    }

    /// <summary>Remembers an opened file and the search that found it, then hides if the settings say so.</summary>
    private void AfterOpening(ResultRow row, string? problem)
    {
        ShowToast(problem);
        if (problem is not null)
        {
            return;
        }
        if (row.Hit is Hit hit)
        {
            _app.RecordOpened(hit.Path, Input.Text);
        }
        else if (row.Item is { Kind: FavoriteKind.File } item)
        {
            _app.RecordOpened(item.Target, "");
        }
        _recall.Reset(_app.History);
        if (_app.Settings.HideAfterOpening)
        {
            HidePanel();
        }
    }

    private void CopySelected(bool path, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Path is not string full)
        {
            ShowToast("Nothing to copy");
            return;
        }
        string text = path ? full : Path.GetFileName(full.TrimEnd('\\'));
        try
        {
            Clipboard.SetText(text);
            ShowToast($"Copied {text}");
        }
        catch (COMException)
        {
            ShowToast("The clipboard is busy · try again");
        }
    }

    private void OnResultDoubleClick(object sender, MouseButtonEventArgs e)
    {
        // Over a name the source is a Run, not a FrameworkElement; the row it belongs to is what counts.
        if (ElementTree.AncestorOf<System.Windows.Controls.ListBoxItem>(e.OriginalSource)?.DataContext is ResultRow { IsHeader: false })
        {
            Accept();
        }
    }

    private void OnResultMouseDown(object sender, MouseButtonEventArgs e) => _dragStart = e.GetPosition(Results);

    /// <summary>Dragging a result out drops the file itself into Explorer, mail or any other app.</summary>
    private void OnResultMouseMove(object sender, MouseEventArgs e)
    {
        if (e.LeftButton != MouseButtonState.Pressed || _dragStart is not Point start || SelectedRow?.Path is not string path)
        {
            return;
        }
        Vector moved = e.GetPosition(Results) - start;
        bool farEnough = Math.Abs(moved.X) > SystemParameters.MinimumHorizontalDragDistance
            || Math.Abs(moved.Y) > SystemParameters.MinimumVerticalDragDistance;
        if (farEnough)
        {
            _dragStart = null;
            DragDrop.DoDragDrop(Results, new DataObject(DataFormats.FileDrop, new[] { path }), DragDropEffects.Copy);
        }
    }
}
