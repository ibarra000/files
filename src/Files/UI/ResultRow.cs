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
    LiveHint,
}

/// <summary>
/// One line in the panel: a search result, or on the home list a section
/// header, a favorite, a recently opened file, or "See all".
/// </summary>
public sealed record ResultRow(RowKind Kind, string Before, string Match, string After, string Detail, string Badge)
{
    private const string FileIcon = "\uE8A5";
    private const string FolderIcon = "\uE8B7";
    private const string SearchIcon = "\uE721";

    public Hit? Hit { get; init; }

    /// <summary>What a favorite or recently opened row stands for.</summary>
    public Favorite? Item { get; init; }

    /// <summary>A Segoe Fluent Icons glyph, or empty.</summary>
    public string Icon { get; init; } = "";

    /// <summary>A favorite file or folder that is no longer at its path.</summary>
    public bool Missing { get; init; }

    public bool IsHeader => Kind == RowKind.Header;

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

    public static ResultRow ForHit(Hit hit)
    {
        if (hit.MatchStart < 0)
        {
            return new ResultRow(RowKind.Hit, hit.Name, "", "", hit.Folder, hit.Volume) { Hit = hit };
        }
        int end = hit.MatchStart + hit.MatchLength;
        string before = hit.Name[..hit.MatchStart];
        string match = hit.Name[hit.MatchStart..end];
        return new ResultRow(RowKind.Hit, before, match, hit.Name[end..], hit.Folder, hit.Volume) { Hit = hit };
    }

    public static ResultRow ForHeader(string title) => new(RowKind.Header, title, "", "", "", "");

    public static ResultRow ForFavorite(Favorite favorite, bool missing)
    {
        if (favorite.Kind == FavoriteKind.Search)
        {
            return new ResultRow(RowKind.Favorite, favorite.Target, "", "", "Saved search", "") { Item = favorite, Icon = SearchIcon };
        }
        string icon = favorite.Kind == FavoriteKind.Folder ? FolderIcon : FileIcon;
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
        return new ResultRow(RowKind.Opened, NameOf(path), "", "", path, "") { Item = item, Icon = FileIcon };
    }

    public static ResultRow ForSeeAll(int count) => new(RowKind.SeeAll, $"See all {count} favorites", "", "", "", "");

    /// <summary>The row shown when nothing matched: Enter on it reads the volumes directly.</summary>
    public static ResultRow ForLiveHint()
    {
        return new ResultRow(RowKind.LiveHint, "Search the volumes directly", "", "", "Finds files added since the last read · Shift+Enter", "");
    }

    private static string NameOf(string path)
    {
        string name = System.IO.Path.GetFileName(path.TrimEnd('\\'));
        return name.Length > 0 ? name : path;
    }
}

/// <summary>The words on the panel's status line: only what needs attention, never a match count.</summary>
public static class StatusText
{
    public static string For(IReadOnlyList<VolumeState> volumes, string? queryProblem)
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
        if (state.ScannedSoFar is int files)
        {
            return $"Reading {name}… {files:N0} files";
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
