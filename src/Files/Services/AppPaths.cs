namespace Files.Services;

/// <summary>
/// Where things are kept. Settings and history roam with the profile; the
/// catalog, shards and caches stay on the local disk.
/// </summary>
public static class AppPaths
{
    public static string Roaming { get; } =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "files");

    public static string Local { get; } =
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "files");

    public static string Settings => Path.Combine(Roaming, "settings.json");
    public static string History => Path.Combine(Roaming, "history.txt");
    public static string Opened => Path.Combine(Roaming, "opened.txt");
    public static string Favorites => Path.Combine(Roaming, "favorites.json");
    public static string Catalog => Path.Combine(Local, "catalog.db");
    public static string Shards => Path.Combine(Local, "volumes");
    public static string Updates => Path.Combine(Local, "updates");
}
