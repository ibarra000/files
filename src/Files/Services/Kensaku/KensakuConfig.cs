using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Files.Data;

namespace Files.Services;

/// <summary>
/// The <c>config.json</c> the bundled kensaku-daemon reads: every field kensaku has no default for, the roots of the
/// volumes Files reads in the background (live-only ones are left to live search), and OCR as set. Pure, so the same
/// volumes and setting always give the same text, and the daemon is restarted only when that text changes.
/// </summary>
public static class KensakuConfig
{
    // The version kensaku 1.0.18 writes; an older one makes it migrate and rewrite the file.
    private const int ConfigVersion = 7;

    // kensaku's own limit for one search.
    private const int MaxResults = 50;

    // Folders nobody searches inside, as kensaku's defaults; exact names.
    private static readonly string[] ExcludedDirs =
    [
        ".git", ".svn", ".hg", "node_modules", ".cache", "__pycache__", ".venv", ".vs",
        "$RECYCLE.BIN", "System Volume Information",
    ];

    // Binaries, credentials, databases and lock files, as kensaku's defaults.
    private static readonly string[] ExcludedExtensions =
    [
        "exe", "dll", "so", "dylib", "bin", "o", "a", "lib", "class", "pyc", "pyo",
        "pem", "key", "pfx", "p12", "crt", "cer",
        "db", "sqlite", "sqlite3", "lock", "lockb", "min.js", "min.css", "map",
    ];

    private static readonly JsonSerializerOptions Json = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        WriteIndented = true,
    };

    private sealed record Config(
        IReadOnlyList<string> WatchedDirs,
        IReadOnlyList<string> ExcludedDirs,
        IReadOnlyList<string> ExcludedExtensions,
        string Shortcut,
        int MaxResults,
        bool IsSetupComplete,
        bool OcrEnabled,
        int ConfigVersion);

    /// <summary>The config for these volumes, with OCR on or off.</summary>
    public static string Text(IEnumerable<Volume> volumes, bool ocr)
    {
        List<string> roots = volumes
            .Where(volume => !volume.LiveOnly)
            .Select(volume => volume.Root)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Order(StringComparer.OrdinalIgnoreCase)
            .ToList();
        // No shortcut: the daemon registers none, and kensaku's own app is not shipped.
        var config = new Config(roots, ExcludedDirs, ExcludedExtensions, "", MaxResults, IsSetupComplete: true, ocr, ConfigVersion);
        return JsonSerializer.Serialize(config, Json);
    }

    /// <summary>A short name for a config text, to tell whether it changed.</summary>
    public static string Hash(string text) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(text)))[..16];
}
