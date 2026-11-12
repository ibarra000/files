using Files.Core;

namespace Files.UI;

/// <summary>What the panel lists while the search box is empty: favorites, then recently opened files.</summary>
public static class HomeRows
{
    public const int FavoritesShown = 5;

    /// <param name="missing">Paths of favorites found to be gone, compared ignoring case.</param>
    /// <param name="showAll">List every favorite, for "See all", and nothing else.</param>
    public static List<ResultRow> Build(IReadOnlyList<Favorite> favorites, IReadOnlyList<string> opened, IReadOnlySet<string> missing, bool showAll)
    {
        var rows = new List<ResultRow>();
        if (favorites.Count > 0)
        {
            rows.Add(ResultRow.ForHeader(showAll ? "All favorites" : "Favorites"));
            int shown = showAll ? favorites.Count : Math.Min(favorites.Count, FavoritesShown);
            foreach (Favorite favorite in favorites.Take(shown))
            {
                bool isMissing = favorite.Kind != FavoriteKind.Search && missing.Contains(favorite.Target);
                rows.Add(ResultRow.ForFavorite(favorite, isMissing));
            }
            if (shown < favorites.Count)
            {
                rows.Add(ResultRow.ForSeeAll(favorites.Count));
            }
        }
        if (showAll)
        {
            return rows;
        }

        // A favorite file is already listed above.
        List<string> recent = opened
            .Where(path => !Favorites.Contains(favorites, new Favorite(FavoriteKind.File, path)))
            .ToList();
        if (recent.Count > 0)
        {
            rows.Add(ResultRow.ForHeader("Recently opened"));
            rows.AddRange(recent.Select(ResultRow.ForOpened));
        }
        return rows;
    }

    /// <summary>The paths of file and folder favorites that no longer exist. Slow on an unreachable share, so call it off the UI thread.</summary>
    public static IReadOnlySet<string> FindMissing(IReadOnlyList<Favorite> favorites)
    {
        var missing = new HashSet<string>(StringComparer.OrdinalIgnoreCase);
        foreach (Favorite favorite in favorites)
        {
            if (favorite.Kind != FavoriteKind.Search && !Path.Exists(favorite.Target))
            {
                missing.Add(favorite.Target);
            }
        }
        return missing;
    }
}
