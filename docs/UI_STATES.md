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

## Volume phases (`Services/Indexer.cs`)

`Indexer.PhaseOf(state)` names each volume's phase once. `StatusText.Describe`, `Stale` and `MenuLine` and `Indexer.NextScan` all read it. Fields: `VolumeState.ScannedSoFar`, `Queued` and `Index`, `Volume.Status` and `LiveOnly`, and `_retryAt`.

| Phase | Fields | Entered by | Left by | `NextScan` |
|---|---|---|---|---|
| **LiveOnly** | `LiveOnly` | Adding or editing it as live-only | Editing it | Never (null) |
| **Queued** | `Queued` | `Scan`: due, F5, or added | Its read starts, or is stopped | Now |
| **Reading** | `ScannedSoFar` set | The read starts | It ends: Online or Offline | Now |
| **NeverRead** | `Index` null | Added, moved, or its saved list could not be loaded | A read that succeeds | Now, or at its retry |
| **Online** | `Status` Online | A read that succeeds | The next read | `RescanHours` after the last read; when 0, only at the retry of a read that stopped |
| **Offline** | `Status` Offline | A read that fails | A read that succeeds | At its retry, 5 min on, even when `RescanHours` is 0; now if offline when the app last closed |
| **Interrupted** | `Status` Scanning | Start-up, when the app closed during a read | Its read | Now |

Rules:
- Only the catalog says Scanning during a read. In memory the volume keeps what its last read found, so Scanning in memory means the app closed mid-read. It is not marked offline.
- An Offline volume keeps its stale mark while it is Queued or Reading, so the mark does not blink at each retry.
- The stale mark names the list's date, not its age, so it does not change until a read succeeds.
- A read that fails in any phase waits for its retry (`_retryAt`), cleared by a read that succeeds.

**Stale marks** (`SearchWindow._shownStale`): keyed by volume id, and a result finds its volume by the list it came from, since two volumes may share a name. None after a live search. A change in the marks redraws the rows on screen.

## Search line (`Core/Query.cs`)

`Query.Parse` turns the box into `Alternatives`, `Extensions`, `ExcludedExtensions`, `Excluded`, `PathTerms` and `Problem`. It never throws. Each token is a word, an operator, or a problem. A quoted token is always a word.

| State | Fields | Entered by | Rule |
|---|---|---|---|
| **Valid** | `Problem` null, one or more `Alternatives` | A line with something to search for | It is searched. Operators narrow it; words between `\|` are its alternatives. |
| **Problem** | `Problem` set | A malformed operator, or nothing to search for | Enter does nothing; the panel is in **Invalid search** and the footer shows the message. The first problem on the line is the one shown. |
| **Lone `!term`** | one unquoted token that starts with `!` | `!readme` and nothing else | It is a word: it finds names holding `!readme`. With any other token, `!term` excludes. |

| Problem | Line |
|---|---|
| `ext: needs a type, like ext:pdf` | `ext:` with no type, or a type that is too long or holds a character no extension has (`ext:p<f`, `ext:!`) |
| `path: needs a folder name, like path:2026` | `path:` with nothing after it but slashes |
| `! needs a word after it, like !draft` | a `!` on its own |
| `nothing to search for` | only stars, quotes, filters or exclusions (`ext:pdf !draft`) |
| `contains a character that cannot be searched` | a NUL in the line |

Typing an operator in steps (`ext:` before `pdf`) passes through **Problem** for a moment. That is the same as any other invalid search.

## Indexed vs live search (`Core/VolumeIndex.cs`, `Core/LiveSearch.cs`)

Both read the same `Query` and rank with the same `RankKey`. They differ in where the names come from and in what they count.

| | Indexed | Live |
|---|---|---|
| **Names** | The saved list in memory | Read from the share now. Each snapshot builds a small index of what was kept and searches it the indexed way. |
| **Filter** | `NameFilter` (hidden types, `ext:`, `!term`) and `path:` on every match | The same filter in `Keep`, before a name is kept. A hidden or filtered name never takes room or adds to the count. |
| **Room** | `MaxResults` (300) shown, every match counted | `MaxKept` (100,000) kept. Past it, matches are only counted, and `IsCapped` is set. |
| **Files in a matching folder** | Every one counted, up to 64 listed per folder | Up to 64 kept per folder, so the count can be lower than indexed |
| **Matching folders** | Every folder whose own name matches | Up to 1,000 per volume, noted as the walk passes them |
| **A typed folder path** | Not special | Read first, only for a plain line (`IsPlain`: one phrase, no operators) |

Rule: a name that indexed search would leave out is never kept by live search.

Ties in rank are cut by the file's place in the index. A live snapshot builds its index in path order (`Walker.Merge` sorts folders and files), so the rows near the 300th place stay put between refreshes. They change only when a file that sorts earlier is found.

## Open counts (`Core/OpenCounts.cs`, `App.OpenCounts`)

`App.OpenCounts` maps a path, ignoring case, to `OpenCount(Count, LastUtc)`. It is saved to `opened-counts.txt` after each change. With **Rank opened files higher** on, a search ranks opened files first among matches of the same kind.

| State | Entered by | Left by | Rule |
|---|---|---|---|
| **Not counted** | The path was never opened, was removed, or fell off | Opening it from the panel with the setting on | It ranks by its match alone. |
| **Counted** | Opening from the panel (`Record`: count + 1, time now) | Del on its Recent row, Clear recent, or falling off | It ranks above matches of the same kind with fewer opens, then above those opened less recently. |
| **Fell off** | More than 500 counted | Opening it again | The file that ranks lowest goes first: fewest opens, then opened longest ago. Eviction and ranking use the same order (`OpenCounts.Compare`). Once 500 files have two or more opens, a file opened once falls off at once. |

**Load:**
- A damaged line is skipped.
- Lines that differ only in case are one file: their counts add up, and the later time is kept.
- A time in the future is read as now, so a clock set back cannot keep a file on top for good.

Turning the setting off stops counting and ranking, but keeps the counts. Changing it runs the search again.

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
