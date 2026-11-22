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
/// happened and how to undo it. The states these fields make are in docs/UI_STATES.md.
/// </summary>
public partial class SearchWindow
{
    // The rename box starts past the row's icon, over the name.
    private const double RenameIndent = 28;

    private FileBatch? _lastBatch;
    private DateTime _lastBatchAt;

    // An operation ended since the box was last typed in, so Ctrl+Z takes it back before the box's own undo.
    private bool _undoFilesFirst;

    // Who has Del, F2, Ctrl+X and Ctrl+V. Until the user goes to the results they belong to the search box,
    // so editing the search never touches a file.
    private KeyOwner _keyOwner = KeyOwner.Box;

    // What Ctrl+C or Ctrl+X put on the clipboard, so a paste can skip what changed since.
    private ClipboardMark? _clipboardMark;

    // Batches are numbered so a progress report from one that ended never covers the next; null when none runs.
    private int _batchCount;
    private int? _runningBatch;

    // A delete for good the footer asked about, which only the same key on the same file confirms.
    private DeleteAsk? _deleteAsked;

    // Each press of Del is numbered; hiding counts too. A press whose drive answers after a newer one is dropped.
    private int _deleteCount;

    private RenameSession? _rename;

    // The file the next results select: where a rename or move put it, or the row beside a deleted one.
    private string? _selectNext;

    /// <summary>The files this panel put on the clipboard, as they were then, and the clipboard's number just after.</summary>
    private sealed record ClipboardMark(uint Version, IReadOnlyList<FileSnapshot> Files);

    /// <summary>An open rename box: its row, who has the keys once it closes, and whether Enter is being applied.</summary>
    private sealed record RenameSession(ResultRow Row, KeyOwner ReturnTo, bool Applying = false);

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
        if (key == Key.Delete && modifiers is ModifierKeys.None or ModifierKeys.Shift)
        {
            return OnDeleteKey(permanently: modifiers == ModifierKeys.Shift, isRepeat);
        }
        if (SelectedHit is null || _keyOwner != KeyOwner.Results)
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
            default:
                return false;
        }
    }

    /// <summary>Del or Shift+Del: deletes the selected result, hints how to, or leaves the key to the box (FileKeys.ActsOnFile).</summary>
    private bool OnDeleteKey(bool permanently, bool isRepeat)
    {
        // A selection made when the panel showed is deleted by Del like any other, so only a bare caret counts as at the end.
        bool caretAtEnd = Input.SelectionLength == 0 && Input.CaretIndex == Input.Text.Length;
        switch (FileKeys.ActsOnFile(_keyOwner, permanently, BoxHasSelection, caretAtEnd, hasHit: SelectedHit is not null))
        {
            case DeleteTarget.File:
                if (!isRepeat)
                {
                    DeleteSelected(permanently);
                }
                return true;
            case DeleteTarget.Hint:
                if (!isRepeat)
                {
                    ShowToast("Press ↓ to choose the file, then Del");
                }
                return true;
            default:
                return false;
        }
    }

    /// <summary>The user chose a row: the file keys go to the results. A click ends a rename as Esc does, unless it is being applied.</summary>
    private void GoToResults()
    {
        if (_rename is { Applying: false })
        {
            EndRename();
        }
        if (_rename is null)
        {
            _keyOwner = KeyOwner.Results;
        }
    }

    /// <summary>The user is editing the search again, or the row they chose has gone: the file keys go back to the box.</summary>
    private void BackToBox()
    {
        if (_keyOwner == KeyOwner.Results)
        {
            _keyOwner = KeyOwner.Box;
        }
        // A Del still waiting on its drive was meant for the result the user has now left, or for a search they are changing.
        _deleteCount++;
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
        if (!SetClipboard([snapshot], cut))
        {
            ShowToast("The clipboard is busy · try again");
            return;
        }
        ShowToast(cut
            ? $"Cut {hit.Name} · {RowActions.PasteKeys.Replace(' ', '+')} on another result moves it there"
            : $"Copied {hit.Name} · paste it in a folder, or its path as text");
    }

    private bool SetClipboard(IReadOnlyList<FileSnapshot> files, bool cut)
    {
        try
        {
            Clipboard.SetDataObject(FileClipboard.Make(files.Select(file => file.Path).ToList(), cut), copy: true);
        }
        catch (COMException)
        {
            return false;
        }
        _clipboardMark = new ClipboardMark(FileClipboard.Version, files);
        return true;
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
        // Only what this panel put there, with nothing put on the clipboard since, has snapshots to check against.
        ClipboardMark? mark = _clipboardMark is { } current && current.Version == FileClipboard.Version ? current : null;
        FileBatch? batch = await RunFileOpAsync(cut ? "Moving" : "Copying", progress =>
        {
            // Files copied here are checked against how they were then; files from elsewhere are taken as they are.
            IReadOnlyList<FileSnapshot> sources = mark is not null
                ? mark.Files
                : paths.Select(path => FileSnapshot.Take(path) ?? new FileSnapshot(path, false, 0, default)).ToList();
            return cut ? FileOps.Move(sources, folder, progress) : FileOps.Copy(sources, folder, progress);
        });
        if (cut && batch is not null && mark is not null)
        {
            AfterCutPaste(batch, mark);
        }
    }

    /// <summary>
    /// Cut files paste once, as in Explorer: what moved leaves the clipboard and what did not stays for
    /// another try. Only the panel's own cut is changed, and only if nothing was put on the clipboard since.
    /// </summary>
    private void AfterCutPaste(FileBatch batch, ClipboardMark mark)
    {
        if (mark.Version != FileClipboard.Version)
        {
            return;
        }
        IReadOnlyList<string> left = batch.LeftBehind;
        if (left.Count == mark.Files.Count)
        {
            return;
        }
        if (left.Count == 0)
        {
            ClearClipboard();
            return;
        }
        List<FileSnapshot> stay = mark.Files.Where(file => left.Contains(file.Path, StringComparer.OrdinalIgnoreCase)).ToList();
        if (stay.Count == 0 || !SetClipboard(stay, cut: true))
        {
            // The clipboard still lists the moved files too; pasting them again finds them gone and says so.
            _clipboardMark = null;
        }
    }

    private void ClearClipboard()
    {
        _clipboardMark = null;
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
        // The press answers the question, not the moment a slow drive says whether it has a Recycle Bin.
        DateTime pressedAt = DateTime.UtcNow;
        int press = ++_deleteCount;
        row ??= SelectedRow;
        if (row?.Hit is not Hit hit)
        {
            ShowToast("Nothing to delete");
            return;
        }
        string path = hit.Path;
        string? beside = BesideOf(row);
        // Off the UI thread: following a link or asking about a drive can wait on the network.
        bool hasBin = await Task.Run(() => RecycleBin.HasBin(path));
        if (press != _deleteCount)
        {
            // A newer press, or hiding, came while waiting: only the newest press counts.
            return;
        }
        bool forGood = permanently || !hasBin;
        DeleteAnswer answer = forGood ? FileKeys.Answer(_deleteAsked, path, permanently, pressedAt) : DeleteAnswer.Confirm;
        if (answer == DeleteAnswer.Ignore)
        {
            return;
        }
        if (answer == DeleteAnswer.Ask)
        {
            string where = hasBin ? "" : $" on {Path.GetPathRoot(path)}, which has no Recycle Bin";
            string again = permanently ? "Shift+Del" : "Del";
            ShowToast($"Permanently delete {hit.Name}{where}? It cannot be undone · {again} again to delete");
            // Armed only once the question shows (ShowToast drops any earlier one), so a press made before cannot confirm it.
            _deleteAsked = new DeleteAsk(path, permanently, DateTime.UtcNow);
            return;
        }
        _deleteAsked = null;
        await RunFileOpAsync("Deleting", progress => FileOps.Delete([path], forGood, progress), selectAfter: beside);
    }

    /// <summary>The result below <paramref name="row"/>, or above it at the end of the list, which a delete leaves selected.</summary>
    private string? BesideOf(ResultRow row)
    {
        int at = Results.Items.IndexOf(row);
        if (at < 0)
        {
            return null;
        }
        int? beside = NextSelectable(at + 1, 1) ?? NextSelectable(at - 1, -1);
        return beside is int index && Results.Items[index] is ResultRow { Hit: Hit hit } ? hit.Path : null;
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
    /// <param name="selectAfter">What a delete leaves selected.</param>
    private async Task<FileBatch?> RunFileOpAsync(string verb, Func<IProgress<FileProgress>, FileBatch> work, string? selectAfter = null)
    {
        if (_runningBatch is not null)
        {
            ShowToast("Still working on the last one · try again when it ends");
            return null;
        }
        int id = ++_batchCount;
        _runningBatch = id;
        var progress = new Progress<FileProgress>(step =>
        {
            // A report that arrives after its batch ended must not cover what happened, or the next batch.
            if (_runningBatch == id && step.Total > 1)
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
            _runningBatch = null;
        }
        AfterFileOp(batch, selectAfter);
        return batch;
    }

    private void AfterFileOp(FileBatch batch, string? selectAfter)
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
        if (!IsVisible || IsHome)
        {
            return;
        }
        // A delete that failed leaves its file listed and selected, so the next Del never lands on another file.
        bool deleted = batch.Kind == FileOpKind.Delete && batch.Done.Any();
        _selectNext = batch.MovedTo ?? (deleted ? selectAfter : null);
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
        if (_rename is not null)
        {
            return;
        }
        Results.ScrollIntoView(row);
        Results.UpdateLayout();
        if (!PlaceRenameBox(row))
        {
            return;
        }
        RenameBox.Text = hit.Name;
        RenameBox.Visibility = Visibility.Visible;
        _rename = new RenameSession(row, _keyOwner);
        _keyOwner = KeyOwner.Renaming;
        RenameBox.UpdateLayout();
        RenameBox.Focus();
        int stem = Path.GetFileNameWithoutExtension(hit.Name).Length;
        RenameBox.Select(0, hit.IsFolder || stem == 0 ? hit.Name.Length : stem);
        ShowToast("Enter renames · Esc cancels");
    }

    /// <summary>Lays the rename box over the row's name. Returns false if the row has no place on screen.</summary>
    private bool PlaceRenameBox(ResultRow row)
    {
        if (Results.ItemContainerGenerator.ContainerFromItem(row) is not FrameworkElement item)
        {
            return false;
        }
        Point at = item.TranslatePoint(new Point(RenameIndent, 0), RenameLayer);
        Canvas.SetLeft(RenameBox, at.X);
        Canvas.SetTop(RenameBox, at.Y);
        RenameBox.Width = Math.Max(0, item.ActualWidth - RenameIndent);
        return true;
    }

    /// <summary>The list scrolled, changed size or was rebuilt: the rename box follows its row.</summary>
    private void FollowRenameRow()
    {
        if (_rename is not null && !PlaceRenameBox(_rename.Row))
        {
            // Scrolled out of sight: the box waits above the list, where the layer clips it, with what was typed.
            Canvas.SetTop(RenameBox, -RenameBox.ActualHeight - 1);
        }
    }

    private void OnResultsLayoutChanged(object sender, RoutedEventArgs e) => FollowRenameRow();

    /// <summary>The list was built again: an open rename moves onto its row in the new list, or ends if the row has gone.</summary>
    private void KeepRenameFor(List<ResultRow> rows)
    {
        if (_rename?.Row.Hit is not Hit renaming)
        {
            return;
        }
        ResultRow? listed = rows.Find(row => row.Hit is not null && string.Equals(row.Hit.Path, renaming.Path, StringComparison.OrdinalIgnoreCase));
        if (listed is null)
        {
            EndRename();
            return;
        }
        _rename = _rename with { Row = listed };
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
        if (_rename is not { Applying: false, Row.Hit: Hit hit })
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
        _rename = _rename with { Applying = true };
        FileBatch? batch = await RunFileOpAsync("Renaming", _ => FileOps.Rename(path, name));
        if (_rename?.Row.Hit is not Hit open || !string.Equals(open.Path, path, StringComparison.OrdinalIgnoreCase))
        {
            // Ended meanwhile: by Esc, by hiding, or by results that no longer list the old name.
            return;
        }
        if (batch is not null && batch.Done.Any())
        {
            EndRename();
            return;
        }
        // Still working, a failure or a name taken: the footer says why, and the box keeps the name for another try.
        _rename = _rename with { Applying = false };
        RenameBox.Focus();
    }

    private void EndRename()
    {
        if (_rename is not RenameSession session)
        {
            return;
        }
        _rename = null;
        _keyOwner = session.ReturnTo;
        RenameBox.Visibility = Visibility.Collapsed;
        Input.Focus();
    }

    /// <summary>A click elsewhere cancels the rename, as Esc does, unless it is being applied.</summary>
    private void OnRenameLostFocus(object sender, KeyboardFocusChangedEventArgs e)
    {
        if (_rename is { Applying: false })
        {
            EndRename();
        }
    }
}
