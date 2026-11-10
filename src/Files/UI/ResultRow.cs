using Files.Core;
using Files.Data;
using Files.Services;

namespace Files.UI;

/// <summary>One line in the panel: a file, or a recent search when the box is empty.</summary>
public sealed record ResultRow(Hit? Hit, string? RecentSearch, string Before, string Match, string After, string Detail, string Badge)
{
    public static ResultRow ForHit(Hit hit)
    {
        if (hit.MatchStart < 0)
        {
            return new ResultRow(hit, null, hit.Name, "", "", hit.Folder, hit.Volume);
        }
        int end = hit.MatchStart + hit.MatchLength;
        return new ResultRow(hit, null, hit.Name[..hit.MatchStart], hit.Name[hit.MatchStart..end], hit.Name[end..], hit.Folder, hit.Volume);
    }

    public static ResultRow ForRecent(string search) => new(null, search, search, "", "", "Recent search", "");
}

/// <summary>The words on the panel's status line.</summary>
public static class StatusText
{
    public static string For(IReadOnlyList<VolumeState> volumes, SearchResult? result, string? queryProblem)
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
        else if (result is not null)
        {
            parts.Add(result.Matched == 1 ? "1 match" : $"{result.Matched:N0} matches");
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
