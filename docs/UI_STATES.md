# UI states

The panel and Settings have no explicit state machine: their state is a handful of fields and what is on screen. This page names the states those fields make, what moves between them, and the rule each one keeps. A change to the UI should keep these rules. A new state belongs here.

## Panel (`UI/SearchWindow.*.cs`)

| State | Fields | Entered by | Left by | Rule |
|---|---|---|---|---|
| **Hidden** | `IsVisible` false | Esc on an empty box, a click elsewhere (`HideOnBlur`), Alt+F4, opening a file (`HideAfterOpening`), opening Settings | The hotkey, the tray, a second launch | Hiding stops a live search, the typing pause, the indexed search and preview reads, and closes both menus. Nothing fills the list or reads a file while hidden. Showing restores a minimized panel. |
| **Home** | box empty or spaces (`IsHome`), `_showAllFavorites` | Showing with an empty box, Esc on a search, deleting the text | Typing, a paste, recall | Favorites, then Recent. Chips are hidden, and Ctrl+1–5 do nothing. Del removes a Recent row, and the next row takes its place. |
| **Recall** | `_recall.IsActive` | Up on the top row (what was typed is held as a draft) | Typing, Esc, opening, Down past the newest search | Up on the top row steps to older searches and Down to newer ones; Down past the newest brings back the draft and leaves Recall, so the next Down moves into the results. The draft is never added to history. |
| **Typing pause** | `_typing` running | A keystroke | 150 ms, Enter | The chips, count and footer follow at once. |
| **Searching (indexed)** | `_searchCancel`, `_searching`, `_epoch` | The pause ends, a paste, recall, a chip that changes the query, an indexer change | Results, a newer search, Home | Only the newest search (`_epoch`) may fill the list. A failure says so in the footer. Results for the same search keep the scroll and selection. |
| **Invalid search** | `_queryProblem` | A query that does not parse | Changing it | Enter does nothing. The footer says what is wrong and offers only Esc. |
| **Enter waiting** | `_accepting` | Enter while a search is still running | That search's results | It opens the best match of the search it waited for. If Esc, typing or hiding moved on, nothing opens. A second Enter is ignored. |
| **Live running** | `_live`, `_liveCancel` | Shift+Enter, or Enter with no hits | It reads everything, Esc stops it, typing, hiding, a new search | Before a hit: no "↵ Open", the count says "Searching…", and Esc stops it. Refreshes every 250 ms keep the scroll, and the preview is not read again. |
| **Live finished** | `_live` and no `_liveCancel`, `_liveStopped` | The live search ends | Typing, Shift+Enter, a chip or volume | Read everything with no hits: Enter does nothing, and the footer offers Esc. Stopped: Shift+Enter searches on. |

**Action menu** (`ActionMenu.IsOpen`, `_actionRow`, `_actionFilter`):
- Opened by Tab or a right-click on a row. Typing filters it, ↑↓ move, Enter or a click runs an action.
- The shortcuts it lists (Del, Ctrl+D, Ctrl+C, Ctrl+X, Ctrl+V, F2, Ctrl+Z, Ctrl+Shift+C, Ctrl+↵) run for its row and close it. Other chords do nothing while it is open.
- It closes on Esc, on Tab, when the panel deactivates, moves or hides, or when its row (matched by path) leaves the list.

**File operations**: see the next section.

**Volume menu** (`_scopeMenu`):
- Shown only with more than one volume. Opened by its button or Alt+↓.
- The keyboard moves in it. Choosing a volume searches again: a live search again if one was shown.
- When it closes, the box has the keyboard back.

**Preview** (`PreviewShown`, `_previewTimer`, `_previewCancel`, `_previewedPath`):
- Ctrl+P toggles it. It reads only the selection, 120 ms after it settles, and only while the panel is visible.
- A list refreshed under the same selection does not read the file again.
- A read cut short by a newer selection says nothing.

**Footer** (`_toast`, `_rescanShownUntil`): a message shows for 4 s. "Scanning…" shows for at least a second after F5. The key caps follow the states above.

## File operations (`UI/SearchWindow.FileOps.cs`)

On search results only; the home list keeps Del for Recent and Ctrl+C for the path. Ctrl+C copies the file whenever no text is selected in the box. The action menu's items always act on their row.

**Key owner** (`_keyOwner`, a `KeyOwner`): who has Del, Shift+Del, F2, Ctrl+X and Ctrl+V (with files on the clipboard).

| State | Entered by | Left by | Rule |
|---|---|---|---|
| **Box** | Typing, any caret or selection change in the box, the list falling back to its first row because the selected file is gone, hiding | Going to the results | The keys edit the search. Del never touches a file: at the end of the text with nothing selected it does nothing in the box, so the footer says "Press ↓ to choose the file, then Del". Shift+Del with no text selected acts on the highlighted result, after asking, since the box has nothing to cut; this is how a lone result, highlighted without going to it, is deleted (`FileKeys.ActsOnFile`). |
| **Results** | ↑ ↓ PgUp PgDn, a click or right-click on a row, a rename closing that began here | Any of the ways into Box, or F2 into Renaming | The keys act on the selected result. After a delete that worked the row beside it is selected, so the keys stay here. |
| **Renaming** | F2 or the menu's Rename opening the rename box | The box closing | The rename box has every key. On closing, the keys go back to whoever had them before (`RenameSession.ReturnTo`). |

**Delete ask** (`_deleteAsked`, `_deleteCount`): a delete for good (Shift+Del, or Del where there is no Recycle Bin) is asked about in the footer.
- Each press is numbered and timed when pressed, before waiting to learn whether the drive has a Recycle Bin. A press whose drive answers after a newer press, after the keys went back to the box or the box changed, or after hiding, is dropped.
- The question is armed only once its toast shows. Any other toast drops it (`ShowToast`), and so does hiding.
- Only the same key on the same file, pressed between 300 ms and 4 s after the question showed, confirms it. A press made before it showed, or a held key (auto-repeat), does nothing.

**Undo slot** (`_lastBatch`, `_lastBatchAt`, `_undoFilesFirst`):
- Filled by an operation that did something and can be taken back, while the panel is visible. Emptied by hiding, and after 2 minutes.
- Ctrl+Z and the menu's "Undo …" take it back. Ctrl+Z does so when nothing was typed since the operation, or when the box has nothing to undo; otherwise the box undoes its typing. Hiding clears `_undoFilesFirst`.

**Rename session** (`_rename`, a `RenameSession(Row, ReturnTo, Applying)`):

| State | Entered by | Left by | Rule |
|---|---|---|---|
| **Open** | F2 or the menu's Rename | Enter with a new name that works, Esc, a click elsewhere, hiding, or results that no longer list its row | The box lies over its row's name. A refresh that still lists the row keeps it, and the box moves onto that row. A scroll or resize moves it too; scrolled out of sight it waits above the list with what was typed. |
| **Applying** | Enter with a changed name | The rename ending | Lost focus does not end it, and a second Enter does nothing. "Still working", a failure or a name taken goes back to Open, with the typed name kept. |

**Clipboard mark** (`_clipboardMark`): the files Ctrl+C or Ctrl+X put on the clipboard, as they were then, and the clipboard's sequence number just after (`FileClipboard.Version`).
- A paste checks the files against the mark only while the sequence number is unchanged; files from elsewhere are taken as they are. `Clipboard.IsCurrent` cannot tell, because the panel's copy is flushed so it outlives the app.
- After a cut is pasted, and only if the clipboard is still the mark's: the files that moved leave it, and the ones that did not (locked, changed, in the way) stay on it, cut, for another try. A folder copied but not all deleted (`CopiedOnly`) leaves it too, since pasting it again would copy it twice. When all moved, the clipboard is cleared. A cut from another program is left as it is.

**Batch progress** (`_runningBatch`, `_batchCount`): one operation runs at a time, and another is refused with a message. Each batch has a number, and only the running batch's progress reports reach the footer, so a late report never covers what happened or the next batch.

**After an operation** (`_selectNext`): the results are searched again, or a live search's list is shown again. A renamed or moved file stays selected; after a delete that worked, the row beside it, and after one that failed, the same file. Nothing is chosen while the panel is hidden, and hiding or typing clears `_selectNext`.

**Index note** (`Core/IndexNote.cs`, `Indexer._notes`): what the panel copied, moved, renamed or deleted in each volume since its last read.
- Added by every operation that changed something (`Indexer.Note`). A read of the volume drops what was noted before that read began. Removing the volume, or moving it to another folder, drops its note.
- The search adds what the note put in a volume. `ShowResults` leaves out what it removed, once, for indexed and live results alike.
- The note knows the renamed or moved path, not what is inside it: the contents of a renamed or moved folder disappear from search until the next scan.

## Settings (`UI/SettingsWindow.xaml.cs`)

| State | Fields | Rule |
|---|---|---|
| **Loading** | `_loading` | Filling the controls never saves. |
| **Volume form: Add / Edit** | `_editing` | Edit… or a double-click on a row fills the form. Saving clears it, and so does removing that volume. Cancel goes back to Add. |
| **Saving a volume** | `_saving` | Save and Cancel are off until it finishes. Only the form that was saved is cleared. |
| **Recording a shortcut** | `App._hotkeysSuspended` | Both shortcuts are let go while a shortcut box has focus. Leaving it takes them back, or the old ones if a new one cannot be had. The two can never be the same keys. Alt+F4, Alt+Tab, Ctrl+Esc and Win+L are refused. A problem found on close is shown by the tray icon. |
| **Patterns refused** | `_refusedRemovals` | The patterns that worked stay in use. Leaving the box does not try the refused ones again. Closing says once that they were not saved. |
| **Program row not valid** | `_viewerProblem`, `_closeWarned` | It is not saved. The first close stops so it can be fixed. |
| **Update: checking / offered / installing** | `App._checking`, `App.UpdateOffer`, `App._installing` | One check at a time. A new update folder drops the old offer. Install is on only with an offer. |

## App (`App.xaml.cs`)

- **Theme.** `Settings.Theme` and `_appliedDark`. With System, a Windows switch is followed at once. The panel colors (`UI/PanelTheme.*.xaml`) are swapped app-wide, so every window, popup and the tray menu follow.
- **Quitting.** `IsQuitting` is one-way. Once set, nothing registers a shortcut, and Alt+F4 on the panel no longer only hides it.
