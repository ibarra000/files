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
    private const string Breadcrumb = " \u203A ";

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
        string folder = FolderText(hit.Folder, hit.Index.Root);
        string badge = showVolume ? hit.Volume : "";
        string icon = FileKinds.Glyph(hit.Name);
        if (hit.MatchStart < 0)
        {
            return new ResultRow(RowKind.Hit, hit.Name, "", "", folder, badge) { Hit = hit, Icon = icon };
        }
        int end = hit.MatchStart + hit.MatchLength;
        string before = hit.Name[..hit.MatchStart];
        string match = hit.Name[hit.MatchStart..end];
        return new ResultRow(RowKind.Hit, before, match, hit.Name[end..], folder, badge) { Hit = hit, Icon = icon };
    }

    /// <summary>
    /// The folder within its volume as a breadcrumb ("2024 › site A"): the volume's own
    /// path is the same on every result, so it is left out. A file at the root shows the root.
    /// </summary>
    public static string FolderText(string folder, string root)
    {
        string trimmedRoot = root.TrimEnd('\\');
        bool inside = folder.StartsWith(trimmedRoot, StringComparison.OrdinalIgnoreCase)
            && (folder.Length == trimmedRoot.Length || folder[trimmedRoot.Length] == '\\');
        if (!inside)
        {
            return folder;
        }
        string[] parts = folder[trimmedRoot.Length..].Split('\\', StringSplitOptions.RemoveEmptyEntries);
        return parts.Length == 0 ? root : string.Join(Breadcrumb, parts);
    }

    public static ResultRow ForHeader(string title) => new(RowKind.Header, title, "", "", "", "");

    public static ResultRow ForFavorite(Favorite favorite, bool missing)
    {
        if (favorite.Kind == FavoriteKind.Search)
        {
            return new ResultRow(RowKind.Favorite, favorite.Target, "", "", "Saved search", "") { Item = favorite, Icon = SearchIcon };
        }
        string icon = favorite.Kind == FavoriteKind.Folder ? FileKinds.Folder : FileKinds.Glyph(favorite.Target);
        return new ResultRow(RowKind.Favorite, NameOf(favorite.Target), "", "", favorite.Target, missing ? "Not found" : "")
        {
            Item = favorite,
            Icon = icon,
            Missing = missing,
        };
    }

    public static ResultRow ForOpened(string path)
    {
        var item = new Favorite(FavoriteKind.File, path);
        return new ResultRow(RowKind.Opened, NameOf(path), "", "", path, "") { Item = item, Icon = FileKinds.Glyph(path) };
    }

    public static ResultRow ForSeeAll(int count) => new(RowKind.SeeAll, $"See all {count} favorites", "", "", "", "");

    private static string NameOf(string path)
    {
        string name = System.IO.Path.GetFileName(path.TrimEnd('\\'));
        return name.Length > 0 ? name : path;
    }
}

/// <summary>The words on the panel's status line: only what needs attention, never a match count.</summary>
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

    /// <summary>The key hint at the bottom right: a search that found nothing offers to read the volumes directly.</summary>
    public static string Hint(bool noHits, bool liveRunning)
    {
        return noHits && !liveRunning ? "Shift+Enter to search live" : "Enter to open";
    }

    /// <summary>The status line while a live search runs, or after it ends.</summary>
    public static string ForLive(SearchResult? result, int foldersRead, bool running, bool stopped, bool capped, IReadOnlyList<string> problems)
    {
        int found = result?.Matched ?? 0;
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
