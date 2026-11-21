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
- The shortcuts it lists (Del, Ctrl+D, Ctrl+C, Ctrl+Shift+C, Ctrl+↵) run for its row and close it. Other chords do nothing while it is open.
- It closes on Esc, on Tab, when the panel deactivates, moves or hides, or when its row (matched by path) leaves the list.

**Volume menu** (`_scopeMenu`):
- Shown only with more than one volume. Opened by its button or Alt+↓.
- The keyboard moves in it. Choosing a volume searches again: a live search again if one was shown.
- When it closes, the box has the keyboard back.

**Preview** (`PreviewShown`, `_previewTimer`, `_previewCancel`, `_previewedPath`):
- Ctrl+P toggles it. It reads only the selection, 120 ms after it settles, and only while the panel is visible.
- A list refreshed under the same selection does not read the file again.
- A read cut short by a newer selection says nothing.

**Footer** (`_toast`, `_rescanShownUntil`): a message shows for 4 s. "Scanning…" shows for at least a second after F5. The key caps follow the states above.

## Settings (`UI/SettingsWindow.xaml.cs`)

| State | Fields | Rule |
|---|---|---|
| **Loading** | `_loading` | Filling the controls never saves. |
| **Volume form: Add / Edit** | `_editing` | Edit… or a double-click on a row fills the form. Saving clears it, and so does removing that volume. Cancel goes back to Add. |
| **Saving a volume** | `_saving` | Save and Cancel are off until it finishes. Only the form that was saved is cleared. |
| **Recording a shortcut** | `App._hotkeysSuspended` | All three shortcuts are let go while a shortcut box has focus. Leaving it takes them back, or the old ones if a new one cannot be had. No two can be the same keys. Alt+F4, Alt+Tab, Ctrl+Esc and Win+L are refused. A problem found on close is shown by the tray icon. |
| **Patterns refused** | `_refusedRemovals` | The patterns that worked stay in use. Leaving the box does not try the refused ones again. Closing says once that they were not saved. |
| **Program row not valid** | `_viewerProblem`, `_closeWarned` | It is not saved. The first close stops so it can be fixed. |
| **Update: checking / offered / installing** | `App._checking`, `App.UpdateOffer`, `App._installing` | One check at a time. A new update folder drops the old offer. Install is on only with an offer. |

## App (`App.xaml.cs`)

- **Theme.** `Settings.Theme` and `_appliedDark`. With System, a Windows switch is followed at once. The panel colors (`UI/PanelTheme.*.xaml`) are swapped app-wide, so every window, popup and the tray menu follow.
- **Quitting.** `IsQuitting` is one-way. Once set, nothing registers a shortcut, and Alt+F4 on the panel no longer only hides it.

## Screen text (`App.xaml.cs`, `UI/AreaWindow.cs`, `UI/ScreenTextWindow.xaml.cs`)

**The read gate** (`App._screenRead`, a `ScreenReadState`; `App._areaPick`). Both reads go through `ReadScreenAsync`.

| State | Entered by | Left by | Rule |
|---|---|---|---|
| **Idle** | Start, the end of any read | A screen-text shortcut | The window title is taken at the press. When the panel is in front, it is taken once the panel hides. |
| **Picking** | The area shortcut | A drag (Reading), Esc, a right-click, a click without a drag, the area shortcut again (Idle) | The other screen-text shortcut is ignored. |
| **Reading** | The text-under-the-mouse shortcut, or the end of a drag | The text is read, or reading fails (Idle) | Every screen-text shortcut is ignored, so a slow app is read once. A failure becomes a note. Nothing is shown once quitting. |

**The area layer** (`AreaWindow`: `_from`, `_closing`, `_picked`):

| State | Fields | Entered by | Left by | Rule |
|---|---|---|---|---|
| **Shown** | `_from` null | Opening, a lost mouse capture | A left button down (Dragging), Esc, a right-click, `Cancel` | The layer never takes focus, so Esc is polled every 30 ms, not received; it is not swallowed, and the window behind sees it too. A keyboard hook is not worth it here. |
| **Dragging** | `_from` set, mouse captured | A left button down | The button up (Done), the capture lost (Shown), Esc, a right-click | The box shows the drag. Losing the capture forgets the drag and hides the box. |
| **Done** | `_picked` holds the area | The button up | (closed) | The area is in physical pixels. A click without a drag gives null. |
| **Cancelled** | `_picked` holds null | Esc, a right-click, `Cancel`, quitting | (closed) | It closes once (`_closing`), however many come together. |

A change of screens (DPI or display settings) covers every screen again. The display handler is static, so closing removes it.

**History** (`App.ScreenTexts`, saved as `screen-text.json`):
- Kept only while `Settings.RememberSearches` is on. Paused, a read is still searched or copied, and the history window says it is paused. It follows Settings as it changes.
- The newest read comes first. The same text read again moves up. At most 50 are kept.
- Clear empties it and saves the empty list.

**After reading** (`Settings.AfterReading`): Search opens the panel with the first line. Copy puts the text on the clipboard and never opens the panel; what it says, even "nothing read", goes to the tray. Text of several lines is always copied.

**The three shortcuts** (`_hotkey`, `_ocrHotkey`, `_areaHotkey`, with `HotkeyProblem`, `OcrHotkeyProblem`, `AreaHotkeyProblem`):
- Each is registered, suspended while a shortcut box records, or failed with its problem kept. A failed one is tried again whenever settings are applied.
- No two are the same keys (`Settings.WithShortcutsApart`). Of two that clash, the one just changed gives way, and only that one.
- Quitting disposes all three.
