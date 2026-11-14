using System.Text.Json;
using Files.Core;
using Files.Data;

namespace Files.Services;

/// <summary>One volume an installer brings, from <c>volumes.json</c> beside files.exe.</summary>
public sealed record BundledVolume(string? Name, string? Folder, bool LiveOnly);

/// <summary>
/// Volumes built into the installer (<c>tools/make_msi.ps1 -Volumes</c>). Each is added
/// once on every PC; after that it belongs to the user, who may edit or remove it, and a
/// removed one is not brought back. Which ones were added is kept beside the catalog
/// (not in the roaming settings), because the volume list itself is per PC.
/// </summary>
public static class BundledVolumes
{
    public const string FileName = "volumes.json";

    private static readonly JsonSerializerOptions Json = new() { PropertyNameCaseInsensitive = true };

    /// <summary>Where the installer puts the list: beside files.exe.</summary>
    public static string DefaultPath => Path.Combine(AppContext.BaseDirectory, FileName);

    /// <summary>The roots of bundled volumes already added on this PC.</summary>
    public static string AddedPath => Path.Combine(AppPaths.Local, "bundled.txt");

    /// <summary>
    /// Adds the bundled volumes in <paramref name="bundlePath"/> not added before, as recorded in
    /// <paramref name="addedPath"/>. Returns what went wrong, in words; a missing file is not a problem.
    /// </summary>
    public static IReadOnlyList<string> Add(string bundlePath, string addedPath, Indexer indexer)
    {
        if (!File.Exists(bundlePath))
        {
            return [];
        }
        List<BundledVolume?> bundled;
        IReadOnlyList<string> added;
        try
        {
            bundled = JsonSerializer.Deserialize<List<BundledVolume?>>(File.ReadAllText(bundlePath), Json) ?? [];
            added = File.Exists(addedPath) ? File.ReadAllLines(addedPath) : [];
        }
        catch (Exception error) when (error is JsonException or IOException or UnauthorizedAccessException)
        {
            return [$"The bundled volumes could not be read: {error.Message}"];
        }

        var problems = new List<string>();
        var nowAdded = new List<string>(added);
        for (int at = 0; at < bundled.Count; at++)
        {
            string? problem = AddOne(bundled[at], added, nowAdded, indexer);
            if (problem is not null)
            {
                problems.Add($"Bundled volume {at + 1}: {problem}");
            }
        }
        if (nowAdded.Count > added.Count)
        {
            try
            {
                AtomicFile.WriteAllText(addedPath, string.Join(Environment.NewLine, nowAdded));
            }
            catch (Exception error) when (error is IOException or UnauthorizedAccessException)
            {
                problems.Add($"Which bundled volumes were added could not be saved: {error.Message}");
            }
        }
        return problems;
    }

    /// <summary>Adds one bundled volume unless it was added before; a folder that is already a volume counts as added.</summary>
    private static string? AddOne(BundledVolume? volume, IReadOnlyList<string> added, List<string> nowAdded, Indexer indexer)
    {
        if (volume is not { Name.Length: > 0, Folder.Length: > 0 })
        {
            return "a volume needs a name and a folder";
        }
        string root;
        try
        {
            root = VolumeRoot.Normalize(volume.Folder);
        }
        catch (Exception error) when (error is ArgumentException or NotSupportedException or IOException)
        {
            return $"{volume.Folder} is not a folder path: {error.Message}";
        }
        if (added.Contains(root, StringComparer.OrdinalIgnoreCase))
        {
            return null;
        }
        string? problem = indexer.Add(volume.Name, volume.Folder, volume.LiveOnly);
        if (problem is not null && problem != Indexer.AlreadyAVolume)
        {
            // Not recorded, so it is tried again at the next start.
            return problem;
        }
        nowAdded.Add(root);
        return null;
    }
}
