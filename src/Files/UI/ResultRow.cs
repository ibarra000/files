using Files.Core;
using Files.Data;
using Files.Services;

namespace Files.UI;

public enum RowKind
{
    Hit,
    Header,
    Favorite,
    Opened,
    SeeAll,
}

/// <summary>
/// One line in the panel: a search result, or on the home list a section
/// header, a favorite, a recently opened file, or "See all".
/// </summary>
public sealed record ResultRow(RowKind Kind, string Before, string Match, string After, string Detail, string Badge)
{
    private const string SearchIcon = "\uE721";

    public Hit? Hit { get; init; }

    /// <summary>What a favorite or recently opened row stands for.</summary>
    public Favorite? Item { get; init; }

    /// <summary>A Segoe Fluent Icons glyph, or empty.</summary>
    public string Icon { get; init; } = "";

    /// <summary>A favorite file or folder that is no longer at its path.</summary>
    public bool Missing { get; init; }

    public bool IsHeader => Kind == RowKind.Header;

    /// <summary>Shown on hover: the full path, since the second line may shorten it or be hidden.</summary>
    public string? ToolTipText => Path ?? (Detail.Length > 0 ? Detail : null);

    /// <summary>The file or folder the row opens, if it opens one.</summary>
    public string? Path
    {
        get
        {
            if (Hit is not null)
            {
                return Hit.Path;
            }
            return Item is { Kind: not FavoriteKind.Search } item ? item.Target : null;
        }
    }

    /// <param name="showVolume">Name the volume, which only tells results apart when there are several.</param>
    public static ResultRow ForHit(Hit hit, bool showVolume)
    {
        string badge = showVolume ? hit.Volume : "";
        string icon = hit.IsFolder ? FileIcons.Folder : FileIcons.Glyph(hit.Name);
        if (hit.MatchStart < 0)
        {
            return new ResultRow(RowKind.Hit, hit.Name, "", "", hit.Folder, badge) { Hit = hit, Icon = icon };
        }
        int end = hit.MatchStart + hit.MatchLength;
        string before = hit.Name[..hit.MatchStart];
        string match = hit.Name[hit.MatchStart..end];
        return new ResultRow(RowKind.Hit, before, match, hit.Name[end..], hit.Folder, badge) { Hit = hit, Icon = icon };
    }

    public static ResultRow ForHeader(string title) => new(RowKind.Header, title, "", "", "", "");

    public static ResultRow ForFavorite(Favorite favorite, bool missing)
    {
        if (favorite.Kind == FavoriteKind.Search)
        {
            return new ResultRow(RowKind.Favorite, favorite.Target, "", "", "Saved search", "") { Item = favorite, Icon = SearchIcon };
        }
        string icon = favorite.Kind == FavoriteKind.Folder ? FileIcons.Folder : FileIcons.Glyph(favorite.Target);
        return new ResultRow(RowKind.Favorite, NameOf(favorite.Target), "", "", FolderOf(favorite.Target), missing ? "Not found" : "")
        {
            Item = favorite,
            Icon = icon,
            Missing = missing,
        };
    }

    public static ResultRow ForOpened(string path)
    {
        var item = new Favorite(FavoriteKind.File, path);
        return new ResultRow(RowKind.Opened, NameOf(path), "", "", FolderOf(path), "") { Item = item, Icon = FileIcons.Glyph(path) };
    }

    public static ResultRow ForSeeAll(int count) => new(RowKind.SeeAll, $"See all {count} favorites", "", "", "", "");

    /// <summary>The folder holding <paramref name="path"/>, as a result's second line shows it; the path itself for a drive or share root.</summary>
    private static string FolderOf(string path) => System.IO.Path.GetDirectoryName(path.TrimEnd('\\')) ?? path;

    private static string NameOf(string path)
    {
        string name = System.IO.Path.GetFileName(path.TrimEnd('\\'));
        return name.Length > 0 ? name : path;
    }
}

/// <summary>A key cap and what it does, shown in the footer.</summary>
public sealed record KeyHint(string Keys, string Label);

/// <summary>The words in the panel's footer and header.</summary>
public static class StatusText
{
    /// <param name="rescanRequested">F5 was just pressed: say "Scanning…" even before a volume reports, so a fast read still shows.</param>
    public static string For(IReadOnlyList<VolumeState> volumes, string? queryProblem, bool rescanRequested = false)
    {
        if (volumes.Count == 0)
        {
            return "No volumes yet · press Ctrl+, to add a folder";
        }
        var parts = new List<string>();
        if (queryProblem is not null)
        {
            parts.Add($"Invalid search · {queryProblem}");
        }
        // Which volume is being read, and how far, is on the Volumes page in Settings.
        if (rescanRequested || volumes.Any(state => state.ScannedSoFar is not null))
        {
            parts.Add("Scanning…");
        }
        foreach (VolumeState state in volumes)
        {
            string? words = VolumeWords(state);
            if (words is not null)
            {
                parts.Add(words);
            }
        }
        return string.Join(" · ", parts);
    }

    /// <summary>The empty box says how much it searches.</summary>
    public static string Placeholder(long files) => files == 0 ? "Type to search" : $"Search {files:N0} files";

    /// <summary>The header beside the box: how many results and, for a search of the saved lists, how long it took.</summary>
    public static string Count(int results, TimeSpan? took)
    {
        if (results == 0)
        {
            return "No results";
        }
        string count = results == 1 ? "1 result" : $"{results:N0} results";
        return took is TimeSpan time ? $"{count} · {Math.Round(time.TotalMilliseconds):N0} ms" : count;
    }

    /// <summary>
    /// The key caps at the bottom right, most useful first (a narrow panel shows only the first few).
    /// A search that found nothing offers to read the volumes directly.
    /// </summary>
    /// <param name="invalid">The search cannot run; only clearing it helps.</param>
    /// <param name="liveFinished">A live search read everything; searching live again would find the same.</param>
    public static IReadOnlyList<KeyHint> Hints(bool noHits, bool liveRunning, bool invalid = false, bool liveFinished = false)
    {
        if (invalid || (noHits && liveFinished && !liveRunning))
        {
            return [new KeyHint("Esc", "Clear")];
        }
        KeyHint first = noHits && !liveRunning ? new KeyHint("Shift ↵", "Search live") : new KeyHint("↵", "Open");
        return [first, new KeyHint("↑↓", "Move"), new KeyHint("Ctrl ↵", "Folder"), new KeyHint("Ctrl C", "Copy path"), new KeyHint("Ctrl P", "Preview")];
    }

    /// <summary>The key caps while the action menu is open.</summary>
    public static IReadOnlyList<KeyHint> MenuHints { get; } = [new KeyHint("↵", "Run"), new KeyHint("Esc", "Back")];

    /// <summary>The footer when all is well: how much is searchable and how fresh it is.</summary>
    public static string Indexed(long files, DateTime? newestScan)
    {
        string count = files == 1 ? "1 file indexed" : $"{files:N0} files indexed";
        return newestScan is null ? count : $"{count} · updated {Age(newestScan)}";
    }

    /// <summary>The status line while a live search runs, or after it ends.</summary>
    /// <param name="found">The same count the header shows, folders included.</param>
    public static string ForLive(int found, int foldersRead, bool running, bool stopped, bool capped, IReadOnlyList<string> problems)
    {
        string progress = $"{(found == 1 ? "1 match" : $"{found:N0} matches")} in {foldersRead:N0} folders";
        if (capped)
        {
            progress += $" · ranking the first {LiveSearch.MaxKept:N0}, type more to narrow it";
        }
        string state;
        if (running)
        {
            state = $"Searching live · {progress} · Esc to stop";
        }
        else if (stopped)
        {
            state = $"Live search stopped · {progress}";
        }
        else
        {
            state = $"Live search · {progress}";
        }
        var parts = new List<string> { state };
        parts.AddRange(problems);
        return string.Join(" · ", parts);
    }

    /// <summary>What the status line says about one volume, or null when all is well.</summary>
    private static string? VolumeWords(VolumeState state)
    {
        string name = state.Volume.Name;
        if (state.ScannedSoFar is not null)
        {
            // Covered by the one "Scanning…" for all volumes.
            return null;
        }
        if (state.Index is null)
        {
            return state.Problem is null ? $"{name} not read yet" : $"{name} could not be read · {state.Problem}";
        }
        if (state.Volume.Status == VolumeStatus.Offline && state.Problem is not null)
        {
            return $"{name} is unreachable · using the list from {Age(state.Volume.LastScannedAt)}";
        }
        return null;
    }

    public static string Age(DateTime? utc)
    {
        if (utc is not DateTime when)
        {
            return "never";
        }
        TimeSpan ago = DateTime.UtcNow - when;
        if (ago.TotalMinutes < 1)
        {
            return "just now";
        }
        if (ago.TotalHours < 1)
        {
            return $"{(int)ago.TotalMinutes} min ago";
        }
        if (ago.TotalDays < 1)
        {
            return $"{(int)ago.TotalHours} h ago";
        }
        return when.ToLocalTime().ToString("d MMM");
    }
}
