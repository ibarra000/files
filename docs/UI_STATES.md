# UI states

The panel and Settings have no explicit state machine: their state is a handful of fields and what is on screen. This page names the states those fields make, what moves between them, and the rule each one keeps. A change to the UI should keep these rules. A new state belongs here.

## Panel (`UI/SearchWindow.*.cs`)

| State | Fields | Entered by | Left by | Rule |
|---|---|---|---|---|
| **Hidden** | `IsVisible` false | Esc on an empty box, a click elsewhere (`HideOnBlur`), Alt+F4, opening a file (`HideAfterOpening`), opening Settings | The hotkey, the tray, a second launch | Hiding stops a live search, the typing pause, the indexed search, a content search and preview reads, and closes both menus. Nothing fills the list or reads a file while hidden. Showing restores a minimized panel. |
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

## Content search (`UI/SearchWindow.Contents.cs`, `Services/Kensaku/`)

**The slot** (`_contentCancel`, `_contents`, `_contentsFor`, `_contentProblem`, `_epoch`):

| State | Fields | Entered by | Left by | Rule |
|---|---|---|---|---|
| **Idle** | `_contentCancel` null | Content search off, the panel hidden, a search answered or cancelled | A search's names on screen | Nothing is asked of kensaku while the panel is hidden. |
| **Asking** | `_contentCancel` | The names of a search are on screen | Its answer, a newer search, hiding | The search takes its token once and disposes its own source; stopping only cancels. Only the newest search (`_epoch`), for what is still in the box, on a visible panel, may fill the list. |
| **Shown** | `_contents` for `_contentsFor` | An answer | The next answer | Rows are drawn under Contents only for the line they were found for. An answer for a new line redraws the list, even when its matches are the same as the last line's. A problem is said once, until it changes. |

Turning content search on or off searches again. A search no file can match (a type filter none of the typed extensions fits) shows no contents without asking kensaku.

**The host** (`KensakuHost`, held by the app; fields `_wanted`, `_stop`, `_run`, `_changed`, `State`, `Problem`):
- What it runs for is `KensakuWanted`: the roots of the volumes that are not live-only, and the OCR setting. The app calls `Apply` on every volume or settings change.
- While a run is going, a different `KensakuWanted` only wakes it (`_changed`). It waits 2 s for more changes, then sends the daemon what changed (`KensakuSetup.Calls`). The daemon is not restarted.
- Content search off, no indexed volume or no kensaku stops the run. State changes made by a stopped run are dropped, so an old run never overwrites a new one.

| State | Entered by | Left by | Rule |
|---|---|---|---|
| **Stopped** | Content search off; quit; on without kensaku (`Problem` says "run tools/fetch_kensaku.ps1") or without a volume that is not live-only ("No indexed volumes to read inside") | Content search on with kensaku and a volume | Nothing runs. |
| **Starting** | A run starts, after a 2 s settle | Setup sent (Up); the daemon exiting or not answering a ping for 10 s (Restarting); a refusal (Failed) | A daemon left by an earlier run of the app is shut down first. Then `daemon run` starts, is pinged every 250 ms, and gets `setup` (a new data folder) or `patch_config`. |
| **Up** | The daemon answers and has the volumes | The daemon exiting or a call failing (Restarting), off, quit | The panel searches only now. Changes go to the running daemon. |
| **Restarting** | The daemon stopped or did not answer | The wait passing (Starting), off, quit | The daemon is shut down or ended. Waits 5 s, 30 s, then 2 min each time; a run that reached Up starts again at 5 s. |
| **Failed** | The pipe refuses this app (`forbidden`), or kensaku.exe cannot be started | Content search off and on, quit | Says why. Starting again would not help, so nothing is retried. |

Stopping (off, quit) asks the daemon to `shutdown` over the pipe, waits up to 2 s, then ends what is left. Quit waits at most 3 s for this.

**The panel** asks the host's state before each search (`Kensaku.SearchAsync`) and calls `search` only while it is Up:

| Host | The panel says |
|---|---|
| Starting, Restarting | "kensaku is starting" |
| Stopped, Failed | The host's problem, or "kensaku is not running" |
| Up, but the search failed | "kensaku did not answer in time" (800 ms), "kensaku is still setting up" (`not_setup`, `no_index`), "kensaku is not running" (the pipe broke; the host sees the daemon exit and restarts it), or kensaku's own message |

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
