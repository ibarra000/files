namespace Files.Services;

/// <summary>
/// Where things are kept. Settings and history roam with the profile; the
/// catalog, shards and caches stay on the local disk.
/// </summary>
public static class AppPaths
{
    /// <summary>
    /// For development: a folder that holds everything instead, so a build can run, with its own
    /// settings and volumes, beside the installed app (see CONTRIBUTING). Null normally.
    /// </summary>
    public static string? Home { get; } = Environment.GetEnvironmentVariable("FILES_HOME") is { Length: > 0 } home ? Path.GetFullPath(home) : null;

    public static string Roaming { get; } = Home is not null
        ? Path.Combine(Home, "roaming")
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.ApplicationData), "files");

    public static string Local { get; } = Home is not null
        ? Path.Combine(Home, "local")
        : Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "files");

    /// <summary>Names the app's single-instance objects; a <see cref="Home"/> of its own is another instance.</summary>
    public static string InstanceSuffix { get; } = Home is null ? "" : "-" + Convert.ToHexString(
        System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(Home.ToUpperInvariant())))[..12];

    public static string Settings => Path.Combine(Roaming, "settings.json");
    public static string History => Path.Combine(Roaming, "history.txt");

    /// <summary>The last reads of the screen (Core/ScreenTextHistory.cs).</summary>
    public static string ScreenTexts => Path.Combine(Roaming, "screen-text.json");
    public static string Opened => Path.Combine(Roaming, "opened.txt");
    public static string Favorites => Path.Combine(Roaming, "favorites.json");
    public static string Catalog => Path.Combine(Local, "catalog.db");
    public static string Shards => Path.Combine(Local, "volumes");
    public static string Updates => Path.Combine(Local, "updates");
}
