using System.Runtime.InteropServices;
using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>
/// Copying, cutting, pasting, renaming and deleting search results, and taking back the last of those.
/// The work runs off the UI thread (Services/FileOps.cs); the footer shows how far it has got, then what
/// happened and how to undo it.
/// </summary>
public partial class SearchWindow
{
    // The rename box starts past the row's icon, over the name.
    private const double RenameIndent = 28;

    private FileBatch? _lastBatch;
    private DateTime _lastBatchAt;

    // An operation ended since the box was last typed in, so Ctrl+Z takes it back before the box's own undo.
    private bool _undoFilesFirst;

    // The user went to the results (arrow keys, Page Up or Down, or a click on a row) since last typing.
    // Until then the file keys belong to the search box, so editing the search never touches a file.
    private bool _inResults;

    // What Ctrl+C or Ctrl+X put on the clipboard, as it was then, so a paste can skip what changed since.
    private IReadOnlyList<FileSnapshot> _copied = [];
    private bool _fileOpRunning;

    // A delete for good the footer asked about, which only the same key on the same file confirms.
    private DeleteAsk? _deleteAsked;
    private ResultRow? _renaming;

    // The file a rename or move put somewhere else, which the next results select.
    private string? _selectNext;

    private Hit? SelectedHit => SelectedRow?.Hit;

    /// <summary>The user selected text in the box, which Ctrl+C and Ctrl+X act on as in any text box.</summary>
    private bool BoxHasSelection => Input.SelectionLength > 0 && !_selectedOnShow;

    /// <summary>The last operation, while it can still be taken back: until the panel hides, and for two minutes at most.</summary>
    private FileBatch? UndoableBatch =>
        _lastBatch is { CanUndo: true } batch && FileKeys.CanStillUndo(_lastBatchAt, DateTime.UtcNow) ? batch : null;

    /// <summary>
    /// Ctrl+Z, and Ctrl+X, Ctrl+V, Delete and Shift+Delete once the user has gone to the results. Returns
    /// whether the key was used; otherwise the search box has it as usual.
    /// </summary>
    /// <param name="isRepeat">A key held down: Delete then does nothing, so holding it can never answer its own question.</param>
    private bool OnFileKey(ModifierKeys modifiers, Key key, bool isRepeat)
    {
        if (modifiers == ModifierKeys.Control && key == Key.Z)
        {
            bool undo = UndoableBatch is not null && (_undoFilesFirst || !Input.CanUndo);
            if (undo)
            {
                UndoLast();
            }
            return undo;
        }
        if (SelectedHit is null || !_inResults)
        {
            return false;
        }
        switch ((modifiers, key))
        {
            case (ModifierKeys.Control, Key.X) when !BoxHasSelection:
                PutOnClipboard(cut: true);
                return true;
            case (ModifierKeys.Control, Key.V) when ClipboardHasFiles():
                Paste();
                return true;
            case (ModifierKeys.None or ModifierKeys.Shift, Key.Delete):
                if (!isRepeat)
                {
                    DeleteSelected(permanently: modifiers == ModifierKeys.Shift);
                }
                return true;
            default:
                return false;
        }
    }

    private static bool ClipboardHasFiles()
    {
        try
        {
            return Clipboard.ContainsFileDropList();
        }
        catch (COMException)
        {
            // Another program has the clipboard open; Ctrl+V pastes text as usual.
            return false;
        }
    }

    /// <summary>Puts the file itself on the clipboard: Explorer pastes the file, a text box its path.</summary>
    private async void PutOnClipboard(bool cut, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Hit is not Hit hit)
        {
            ShowToast("Nothing to copy");
            return;
        }
        string path = hit.Path;
        // Off the UI thread: a share that went away can take a while to say so.
        FileSnapshot? snapshot = await Task.Run(() => FileSnapshot.Take(path));
        if (snapshot is null)
        {
            ShowToast($"{hit.Name} is not there any more");
            return;
        }
        try
        {
            Clipboard.SetDataObject(FileClipboard.Make([path], cut), copy: true);
        }
        catch (COMException)
        {
            ShowToast("The clipboard is busy · try again");
            return;
        }
        _copied = [snapshot];
        ShowToast(cut
            ? $"Cut {hit.Name} · {RowActions.PasteKeys.Replace(' ', '+')} on another result moves it there"
            : $"Copied {hit.Name} · paste it in a folder, or its path as text");
    }

    /// <summary>Pastes files from the clipboard into the selected folder, or the folder holding the selected file.</summary>
    private async void Paste(ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Hit is not Hit hit)
        {
            ShowToast("Select a result to paste beside");
            return;
        }
        string folder = hit.IsFolder ? hit.Path : hit.Folder;
        IDataObject? data;
        try
        {
            data = Clipboard.GetDataObject();
        }
        catch (COMException)
        {
            ShowToast("The clipboard is busy · try again");
            return;
        }
        IReadOnlyList<string> paths = FileClipboard.Files(data);
        if (paths.Count == 0)
        {
            ShowToast("No files on the clipboard");
            return;
        }
        bool cut = FileClipboard.IsCut(data);
        IReadOnlyList<FileSnapshot> copied = _copied;
        bool copiedHere = copied.Select(source => source.Path).SequenceEqual(paths, StringComparer.OrdinalIgnoreCase);
        FileBatch? batch = await RunFileOpAsync(cut ? "Moving" : "Copying", progress =>
        {
            // Files copied here are checked against how they were then; files from elsewhere are taken as they are.
            IReadOnlyList<FileSnapshot> sources = copiedHere
                ? copied
                : paths.Select(path => FileSnapshot.Take(path) ?? new FileSnapshot(path, false, 0, default)).ToList();
            return cut ? FileOps.Move(sources, folder, progress) : FileOps.Copy(sources, folder, progress);
        });
        if (cut && batch is not null && batch.Done.Any())
        {
            // Cut files paste once, as in Explorer: they are no longer where the clipboard says.
            _copied = [];
            ClearClipboard();
        }
    }

    private static void ClearClipboard()
    {
        try
        {
            Clipboard.Clear();
        }
        catch (COMException)
        {
            // A second paste then finds the files gone and says so.
        }
    }

    /// <summary>
    /// Sends the selected result to the Recycle Bin. A delete for good (Shift+Del, or a drive with no
    /// Recycle Bin: a share, a removable or subst drive, or a folder linked to one) is asked about first in
    /// the footer, and only the same key on the same file, pressed again a moment later, confirms it.
    /// </summary>
    private async void DeleteSelected(bool permanently, ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Hit is not Hit hit)
        {
            ShowToast("Nothing to delete");
            return;
        }
        string path = hit.Path;
        // Off the UI thread: following a link or asking about a drive can wait on the network.
        bool hasBin = await Task.Run(() => RecycleBin.HasBin(path));
        bool forGood = permanently || !hasBin;
        DateTime now = DateTime.UtcNow;
        DeleteAnswer answer = forGood ? FileKeys.Answer(_deleteAsked, path, permanently, now) : DeleteAnswer.Confirm;
        if (answer == DeleteAnswer.Ignore)
        {
            return;
        }
        if (answer == DeleteAnswer.Ask)
        {
            _deleteAsked = new DeleteAsk(path, permanently, now);
            string where = hasBin ? "" : $" on {Path.GetPathRoot(path)}, which has no Recycle Bin";
            string again = permanently ? "Shift+Del" : "Del";
            ShowToast($"Permanently delete {hit.Name}{where}? It cannot be undone · {again} again to delete");
            return;
        }
        _deleteAsked = null;
        await RunFileOpAsync("Deleting", progress => FileOps.Delete([path], forGood, progress));
    }

    private async void UndoLast()
    {
        if (UndoableBatch is not FileBatch batch)
        {
            ShowToast("Nothing to undo");
            return;
        }
        await RunFileOpAsync("Undoing", progress => FileOps.Undo(batch, progress: progress));
    }

    /// <summary>
    /// Runs an operation off the UI thread with its progress in the footer, then notes what changed for
    /// the search and shows the results again. Returns null if it could not start or failed as a whole.
    /// </summary>
    private async Task<FileBatch?> RunFileOpAsync(string verb, Func<IProgress<FileProgress>, FileBatch> work)
    {
        if (_fileOpRunning)
        {
            ShowToast("Still working on the last one · try again when it ends");
            return null;
        }
        _fileOpRunning = true;
        var progress = new Progress<FileProgress>(step =>
        {
            // A report that arrives after the end must not cover what happened.
            if (_fileOpRunning && step.Total > 1)
            {
                ShowToast(FileOpText.Progress(verb, step));
            }
        });
        FileBatch batch;
        try
        {
            batch = await Task.Run(() => work(progress));
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or InvalidOperationException or COMException)
        {
            ShowToast($"That did not work: {error.Message}");
            return null;
        }
        finally
        {
            _fileOpRunning = false;
        }
        AfterFileOp(batch);
        return batch;
    }

    private void AfterFileOp(FileBatch batch)
    {
        ShowToast(FileOpText.Done(batch, RowActions.UndoKeys.Replace(' ', '+')));
        (IReadOnlyList<string> removed, IReadOnlyList<NotedPath> added) = batch.Changes();
        if (removed.Count == 0 && added.Count == 0)
        {
            // Nothing changed: what was there to undo still is.
            return;
        }
        _app.Indexer.Note(removed, added);
        if (batch.Done.Any())
        {
            // Hiding ends the chance to undo, also when it hid while this ran (a shell question takes the focus).
            bool keep = batch.CanUndo && IsVisible;
            _lastBatch = keep ? batch : null;
            _lastBatchAt = DateTime.UtcNow;
            _undoFilesFirst = keep;
        }
        if (batch.Kind is FileOpKind.Rename or FileOpKind.Move && batch.Done.First().Target is string moved)
        {
            _selectNext = moved;
        }
        if (!IsVisible || IsHome)
        {
            return;
        }
        // The selected file may be another one now: the preview reads it again.
        _previewedPath = null;
        if (_live is null)
        {
            _ = SearchAsync();
        }
        else if (_result is SearchResult shown)
        {
            // A live search's own list is not searched again; what is gone leaves it.
            ShowResults(shown);
        }
    }

    /// <summary>F2: a box over the row's name, with the name selected up to its extension, as in Explorer.</summary>
    private void StartRename(ResultRow? row = null)
    {
        row ??= SelectedRow;
        if (row?.Hit is not Hit hit)
        {
            ShowToast("Nothing to rename");
            return;
        }
        Results.ScrollIntoView(row);
        Results.UpdateLayout();
        if (Results.ItemContainerGenerator.ContainerFromItem(row) is not FrameworkElement item)
        {
            return;
        }
        Point at = item.TranslatePoint(new Point(RenameIndent, 0), RenameLayer);
        Canvas.SetLeft(RenameBox, at.X);
        Canvas.SetTop(RenameBox, at.Y);
        RenameBox.Width = Math.Max(0, item.ActualWidth - RenameIndent);
        RenameBox.Text = hit.Name;
        RenameBox.Visibility = Visibility.Visible;
        _renaming = row;
        RenameBox.UpdateLayout();
        RenameBox.Focus();
        int stem = Path.GetFileNameWithoutExtension(hit.Name).Length;
        RenameBox.Select(0, hit.IsFolder || stem == 0 ? hit.Name.Length : stem);
        ShowToast("Enter renames · Esc cancels");
    }

    /// <summary>Keys while the rename box is open: Enter and Esc are its own, the rest type in it.</summary>
    private bool OnRenameKey(Key key)
    {
        switch (key)
        {
            case Key.Enter:
                ApplyRename();
                return true;
            case Key.Escape:
                EndRename();
                ShowToast(null);
                return true;
            default:
                return false;
        }
    }

    private async void ApplyRename()
    {
        if (_renaming?.Hit is not Hit hit)
        {
            return;
        }
        string name = RenameBox.Text;
        if (name == hit.Name)
        {
            EndRename();
            return;
        }
        if (FileOps.NameProblem(name) is string problem)
        {
            ShowToast(problem);
            return;
        }
        string path = hit.Path;
        FileBatch? batch = await RunFileOpAsync("Renaming", _ => FileOps.Rename(path, name));
        if (batch is { Results: [{ Outcome: FileOutcome.Exists }] })
        {
            // The footer says the name is taken; the box stays for another one.
            RenameBox.Focus();
            return;
        }
        EndRename();
    }

    private void EndRename()
    {
        if (_renaming is null)
        {
            return;
        }
        _renaming = null;
        RenameBox.Visibility = Visibility.Collapsed;
        Input.Focus();
    }

    /// <summary>A click elsewhere cancels the rename, as Esc does, unless it is being applied.</summary>
    private void OnRenameLostFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        if (!_fileOpRunning)
        {
            EndRename();
        }
    }
}
