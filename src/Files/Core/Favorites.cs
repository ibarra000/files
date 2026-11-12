using System.Text.Json;
using System.Text.Json.Serialization;

namespace Files.Core;

public enum FavoriteKind
{
    File,
    Folder,
    Search,
}

/// <summary>A favorite: a file or folder by its path, or a saved search by its text.</summary>
public sealed record Favorite(FavoriteKind Kind, string Target)
{
    public bool Matches(Favorite other)
    {
        return Kind == other.Kind && Target.Equals(other.Target, StringComparison.OrdinalIgnoreCase);
    }
}

/// <summary>The favorites list, newest first, kept in <c>favorites.json</c>.</summary>
public static class Favorites
{
    private static readonly JsonSerializerOptions Json = new()
    {
        WriteIndented = true,
        Encoder = System.Text.Encodings.Web.JavaScriptEncoder.UnsafeRelaxedJsonEscaping,
        Converters = { new JsonStringEnumConverter() },
    };

    public static bool Contains(IReadOnlyList<Favorite> favorites, Favorite favorite)
    {
        return favorites.Any(f => f.Matches(favorite));
    }

    /// <summary>Returns the list with <paramref name="favorite"/> removed if it was there, or added at the front.</summary>
    public static IReadOnlyList<Favorite> Toggle(IReadOnlyList<Favorite> favorites, Favorite favorite)
    {
        if (Contains(favorites, favorite))
        {
            return favorites.Where(f => !f.Matches(favorite)).ToList();
        }
        return favorites.Prepend(favorite).ToList();
    }

    /// <summary>Loads the list; a missing or unreadable file gives an empty one.</summary>
    public static IReadOnlyList<Favorite> Load(string path)
    {
        if (!File.Exists(path))
        {
            return [];
        }
        try
        {
            List<Favorite>? loaded = JsonSerializer.Deserialize<List<Favorite>>(File.ReadAllText(path), Json);
            return (loaded ?? []).Where(f => f?.Target is { Length: > 0 }).ToList();
        }
        catch (JsonException)
        {
            return [];
        }
    }

    public static void Save(string path, IReadOnlyList<Favorite> favorites)
    {
        AtomicFile.WriteAllText(path, JsonSerializer.Serialize(favorites, Json));
    }
}
