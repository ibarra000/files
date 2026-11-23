using System.Diagnostics;
using System.Runtime.InteropServices;
using Files.Core;
using Microsoft.Win32;

// Times search of the in-memory index on generated names, for the table in docs/BENCHMARK.md.
//
//   dotnet run -c Release --project tools/IndexBench
//       1M and 5M names: builds each index, then times the fixed queries below. Prints
//       the machine and a Markdown table; paste both into docs/BENCHMARK.md.
//
//   dotnet run -c Release --project tools/IndexBench -- --names 250000 --runs 20
//       Other sizes (comma-separated) or fewer timed runs per query.
//
//   dotnet run -c Release --project tools/IndexBench -- --write D:\bench --names 1000000
//       Writes the same names as empty files under a folder, for another tool to index
//       and be timed on the same queries. Takes a while and leaves the folder behind.
//
// The names are generated from a fixed seed, so every run searches the same names.

int at = Array.IndexOf(args, "--names");
int[] sizes = at >= 0 && at + 1 < args.Length ? args[at + 1].Split(',').Select(int.Parse).ToArray() : [1_000_000, 5_000_000];
at = Array.IndexOf(args, "--runs");
int runs = at >= 0 && at + 1 < args.Length ? int.Parse(args[at + 1]) : 50;
at = Array.IndexOf(args, "--write");
if (at >= 0 && at + 1 < args.Length)
{
    Names.Write(args[at + 1], sizes[0], Bench.Seed);
    return 0;
}

Console.WriteLine(Bench.Machine());
Console.WriteLine();
Console.WriteLine("| Names | Build | Memory | Query | First | p50 | p95 | p99 | Matched |");
Console.WriteLine("|---:|---:|---:|---|---:|---:|---:|---:|---:|");
foreach (int size in sizes)
{
    Bench.Run(size, runs);
}
return 0;

internal static class Bench
{
    public const int Seed = 20261121;

    /// <summary>What a search box sees: a short start of a word, a code from the middle of a name, a type filter, nothing, one letter.</summary>
    private static readonly (string Label, string Line)[] Queries =
    [
        ("short prefix", "inv"),
        ("mid-string", "0704_page"),
        ("extension filter", "ext:dwg 07"),
        ("no match", "qzxjvw"),
        ("common substring", "e"),
    ];

    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    public static void Run(int size, int runs)
    {
        long before = GC.GetTotalMemory(forceFullCollection: true);
        var clock = Stopwatch.StartNew();
        VolumeIndex index = Names.Build(size, Seed);
        TimeSpan build = clock.Elapsed;
        long memory = GC.GetTotalMemory(forceFullCollection: true) - before;

        foreach ((string label, string line) in Queries)
        {
            Query query = Query.Parse(line);
            clock.Restart();
            SearchResult first = Search(index, query);
            TimeSpan firstTook = clock.Elapsed;
            var times = new double[runs];
            for (int run = 0; run < runs; run++)
            {
                clock.Restart();
                Search(index, query);
                times[run] = clock.Elapsed.TotalMilliseconds;
            }
            Array.Sort(times);
            Console.WriteLine($"| {index.FileCount:N0} | {build.TotalSeconds:F1} s | {memory / 1048576.0:N0} MB | {label} `{line}` " +
                $"| {firstTook.TotalMilliseconds:F1} ms | {Percentile(times, 50):F1} ms | {Percentile(times, 95):F1} ms " +
                $"| {Percentile(times, 99):F1} ms | {first.Matched:N0} |");
        }
        GC.KeepAlive(index);
    }

    private static SearchResult Search(VolumeIndex index, Query query) =>
        Searcher.Search([("bench", index)], query, NothingHidden, CancellationToken.None);

    private static double Percentile(double[] sorted, int percent) =>
        sorted[Math.Min(sorted.Length - 1, (int)Math.Ceiling(percent / 100.0 * sorted.Length) - 1)];

    public static string Machine()
    {
        string cpu = Registry.GetValue(@"HKEY_LOCAL_MACHINE\HARDWARE\DESCRIPTION\System\CentralProcessor\0", "ProcessorNameString", null) as string ?? "unknown CPU";
        double ram = GC.GetGCMemoryInfo().TotalAvailableMemoryBytes / 1073741824.0;
        return $"{cpu.Trim()}, {Environment.ProcessorCount} logical cores, {ram:F0} GB RAM, Windows {Environment.OSVersion.Version}, " +
            $"{RuntimeInformation.FrameworkDescription}, {(Debugger.IsAttached ? "debugger attached" : "no debugger")}, " +
            $"{DateTime.Now:yyyy-MM-dd}";
    }
}

/// <summary>Names like those on an engineering share: drawing codes, office files, code, scans, some accents and CJK.</summary>
internal static class Names
{
    private static readonly string[] Words =
    [
        "invoice", "report", "meeting", "notes", "budget", "proposal", "drawing", "assembly", "bracket", "housing",
        "schedule", "contract", "quote", "inspection", "summary", "draft", "final", "review", "spec", "layout",
        "Überprüfung", "résumé", "año", "Lieferschein", "東京", "設計図", "plan", "client", "site", "survey",
    ];

    private static readonly string[] DrawingTypes = ["dwg", "dxf", "pdf", "prt", "asm", "drw", "step", "plt"];
    private static readonly string[] OfficeTypes = ["docx", "xlsx", "pptx", "pdf", "msg", "txt", "csv"];
    private static readonly string[] CodeTypes = ["cs", "py", "js", "json", "md", "xml", "ps1"];
    private static readonly string[] MediaTypes = ["jpg", "png", "tif", "mp4"];

    public static VolumeIndex Build(int count, int seed)
    {
        var builder = new VolumeIndex.Builder(@"\\server\share");
        foreach ((string folder, List<string> files) in Folders(count, seed))
        {
            builder.AddFolder(folder, files);
        }
        return builder.Build();
    }

    /// <summary>The same names as empty files under <paramref name="root"/>; a name that repeats in a folder is written once.</summary>
    public static void Write(string root, int count, int seed)
    {
        int written = 0;
        foreach ((string folder, List<string> files) in Folders(count, seed))
        {
            string path = Directory.CreateDirectory(Path.Join(root, folder)).FullName;
            foreach (string name in files.Distinct(StringComparer.OrdinalIgnoreCase))
            {
                File.Create(Path.Join(path, name)).Dispose();
                written++;
            }
        }
        Console.WriteLine($"{written:N0} files written under {root}");
    }

    private static IEnumerable<(string Folder, List<string> Files)> Folders(int count, int seed)
    {
        var random = new Random(seed);
        int made = 0;
        for (int folder = 0; made < count; folder++)
        {
            int files = Math.Min(count - made, random.Next(20, 400));
            string path = $@"Projects\{2015 + folder % 12}\{Pick(random, Words)} {folder % 97:D2}\{Pick(random, Words)}_{folder}";
            yield return (path, Enumerable.Range(0, files).Select(_ => Name(random)).ToList());
            made += files;
        }
    }

    private static string Name(Random random) => random.Next(10) switch
    {
        < 4 => Drawing(random),
        < 7 => $"{Phrase(random)}{(random.Next(3) == 0 ? $" {2015 + random.Next(12)}-{random.Next(1, 13):D2}-{random.Next(1, 29):D2}" : "")}.{Pick(random, OfficeTypes)}",
        < 9 => $"{Pick(random, Words)}{(random.Next(2) == 0 ? "_" : "")}{Pick(random, Words)}{random.Next(100)}.{Pick(random, CodeTypes)}",
        _ => $"{(random.Next(2) == 0 ? "IMG_" : "Scan ")}{random.Next(10_000):D4}.{Pick(random, MediaTypes)}",
    };

    private static string Drawing(Random random)
    {
        string code = $"{(char)('A' + random.Next(26))}{(char)('A' + random.Next(26))}-{(char)('A' + random.Next(26))}-{random.Next(10_000):D4}";
        string suffix = random.Next(4) switch
        {
            0 => $"_Page{random.Next(1, 40)}",
            1 => $" Rev {(char)('A' + random.Next(8))}",
            2 => $"-{random.Next(100):D2}",
            _ => "",
        };
        return $"{code}{suffix}.{Pick(random, DrawingTypes)}";
    }

    private static string Phrase(Random random)
    {
        string separator = random.Next(3) switch { 0 => " ", 1 => "_", _ => "-" };
        return string.Join(separator, Enumerable.Range(0, random.Next(1, 5)).Select(_ => Pick(random, Words)));
    }

    private static string Pick(Random random, string[] from) => from[random.Next(from.Length)];
}
