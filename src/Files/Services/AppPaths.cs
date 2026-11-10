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
    public static string Catalog => Path.Combine(Local, "catalog.db");
    public static string Shards => Path.Combine(Local, "volumes");
    public static string PdfCache => Path.Combine(Local, "pdf");
    public static string Updates => Path.Combine(Local, "updates");
}
