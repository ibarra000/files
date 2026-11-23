using System.Text.Json.Nodes;
using Files.Data;

namespace Files.Services;

/// <summary>What kensaku should read: the roots of the volumes Files reads in the background, and OCR on or off.</summary>
public sealed record KensakuWanted(IReadOnlyList<string> WatchedDirs, bool Ocr)
{
    public bool Equals(KensakuWanted? other) =>
        other is not null && Ocr == other.Ocr && WatchedDirs.SequenceEqual(other.WatchedDirs, StringComparer.OrdinalIgnoreCase);

    public override int GetHashCode() => HashCode.Combine(Ocr, WatchedDirs.Count);
}

/// <summary>One call to send to the kensaku daemon.</summary>
public sealed record KensakuCall(string Method, JsonObject Parameters);

/// <summary>
/// Works out what to tell the kensaku daemon (issue #97). Pure, so the same state always gives the same calls:
/// <c>setup</c> the first time, <c>patch_config</c> with what changed after that, and the OCR pack's download
/// whenever OCR is on as kensaku starts or OCR is turned on (kensaku fetches only what is missing).
/// </summary>
public static class KensakuSetup
{
    public const string NoVolumes = "No indexed volumes to read inside";

    /// <summary>The roots of the volumes read in the background, sorted; live-only volumes are left to live search.</summary>
    public static KensakuWanted Wanted(IEnumerable<Volume> volumes, bool ocr)
    {
        List<string> roots = volumes
            .Where(volume => !volume.LiveOnly)
            .Select(volume => volume.Root)
            .Distinct(StringComparer.OrdinalIgnoreCase)
            .Order(StringComparer.OrdinalIgnoreCase)
            .ToList();
        return new KensakuWanted(roots, ocr);
    }

    /// <param name="setupComplete">What kensaku's <c>health</c> said; false for a new data folder.</param>
    /// <param name="applied">What was sent since this daemon started, or null when nothing was.</param>
    public static IReadOnlyList<KensakuCall> Calls(bool setupComplete, KensakuWanted? applied, KensakuWanted wanted)
    {
        var calls = new List<KensakuCall>();
        if (!setupComplete)
        {
            // setup takes no OCR switch, and kensaku's default is on.
            calls.Add(new KensakuCall("setup", new JsonObject { ["watched_dirs"] = Dirs(wanted) }));
            calls.Add(Patch(new JsonObject { ["ocr_enabled"] = wanted.Ocr }));
        }
        else if (applied is null)
        {
            // A daemon set up by an earlier run: the volumes may have changed since. The same roots start no crawl.
            calls.Add(Patch(new JsonObject { ["watched_dirs"] = Dirs(wanted), ["ocr_enabled"] = wanted.Ocr }));
        }
        else
        {
            var patch = new JsonObject();
            if (!applied.WatchedDirs.SequenceEqual(wanted.WatchedDirs, StringComparer.OrdinalIgnoreCase))
            {
                patch["watched_dirs"] = Dirs(wanted);
            }
            if (applied.Ocr != wanted.Ocr)
            {
                patch["ocr_enabled"] = wanted.Ocr;
            }
            if (patch.Count > 0)
            {
                calls.Add(Patch(patch));
            }
        }
        if (wanted.Ocr && applied?.Ocr != true)
        {
            calls.Add(new KensakuCall("pack_download", new JsonObject { ["pack"] = "ocr" }));
        }
        return calls;
    }

    private static KensakuCall Patch(JsonObject patch) => new("patch_config", new JsonObject { ["patch"] = patch });

    private static JsonArray Dirs(KensakuWanted wanted) => new(wanted.WatchedDirs.Select(dir => (JsonNode?)JsonValue.Create(dir)).ToArray());
}
