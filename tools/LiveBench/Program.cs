using System.Collections.Concurrent;
using System.Diagnostics;
using Files.Core;

// Times live search on a real volume, to compare against other tools, tune the worker counts,
// and find out why a search on one share stops moving.
//
//   dotnet run -c Release --project tools/LiveBench -- <root> <term>
//       The live search the app runs: prints each second how many folders were read and
//       matches found, then when the first match came and how long the whole read took.
//
//   dotnet run -c Release --project tools/LiveBench -- <root> <term> --trace [--workers 32]
//       The same walk, saying what it waits on: how long the checks before it take, each
//       second the folders being listed and for how long, and at the end (or on Ctrl+C)
//       the slowest listings and the failures by kind. Send the whole output.
//
//   dotnet run -c Release --project tools/LiveBench -- <root> <term> --workers 1,8,32
//       The bare walk with each worker count in turn, to see where listing more folders
//       at once stops helping, or makes the server slower. A second run of the same count
//       is faster (the server caches).

if (args.Length < 2)
{
    Console.Error.WriteLine("usage: LiveBench <root> <term> [--trace] [--workers 1,8,32]");
    return 2;
}
string root = args[0];
string term = args[1];
int at = Array.IndexOf(args, "--workers");
int[] workerCounts = at >= 0 && at + 1 < args.Length ? args[at + 1].Split(',').Select(int.Parse).ToArray() : [];
if (args.Contains("--trace"))
{
    Bench.Trace(root, term, workerCounts.FirstOrDefault(LiveSearch.NetworkWorkers));
    return 0;
}
if (workerCounts.Length > 0)
{
    foreach (int workers in workerCounts)
    {
        Bench.Walk(root, term, workers);
    }
    return 0;
}
await Bench.LiveAsync(root, term);
return 0;

internal static class Bench
{
    private static readonly TimeSpan Every = TimeSpan.FromSeconds(1);
    private const int Slowest = 20;
    private const int InFlightShown = 10;
    private const int ExamplesPerFailure = 3;

    public static async Task LiveAsync(string root, string term)
    {
        var live = new LiveSearch([("bench", root)], Query.Parse(term), new HashSet<string>(), skipHidden: true);
        var clock = Stopwatch.StartNew();
        TimeSpan? firstMatch = null;
        Task run = Task.Run(() => live.RunAsync(CancellationToken.None));
        while (!run.IsCompleted)
        {
            await Task.WhenAny(run, Task.Delay(Every));
            int matched = live.Snapshot().Matched;
            firstMatch ??= matched > 0 ? clock.Elapsed : null;
            string waiting = live.Waiting is var (path, waited) ? $"  waiting {waited.TotalSeconds:F0}s on {path}" : "";
            Console.WriteLine($"{clock.Elapsed.TotalSeconds,6:F1}s  {live.FoldersRead,10:N0} folders  {matched,8:N0} matches  {live.Reading}{waiting}");
        }
        await run;
        Console.WriteLine($"first match {(firstMatch is TimeSpan first ? $"{first.TotalSeconds:F2}s" : "none")} · done {clock.Elapsed.TotalSeconds:F2}s · {live.Skipped.Describe() ?? "nothing skipped"}");
        foreach (string problem in live.Problems)
        {
            Console.WriteLine(problem);
        }
    }

    public static void Walk(string root, string term, int workers)
    {
        string needle = Names.Fold(term);
        int matched = 0;
        var progress = new WalkProgress();
        var clock = Stopwatch.StartNew();
        Task walk = Task.Factory.StartNew(() => Walker.Walk(root, skipHidden: true, CancellationToken.None, (_, files) =>
        {
            List<string> kept = files.Where(file => Names.Fold(file).Contains(needle, StringComparison.Ordinal)).ToList();
            Interlocked.Add(ref matched, kept.Count);
            return kept;
        }, (_, _) => { }, new LiveWalkOptions(workers, [], progress)), TaskCreationOptions.LongRunning);
        while (!walk.Wait(Every))
        {
            Console.WriteLine($"    {workers,3} workers {clock.Elapsed.TotalSeconds,6:F1}s  {progress.Folders,10:N0} folders  {progress.InFlight.Count} listing");
        }
        if (walk.Exception?.GetBaseException() is Exception error)
        {
            Console.WriteLine($"{workers,3} workers: {error.GetType().Name} 0x{error.HResult:X8}: {error.Message}");
        }
        double seconds = clock.Elapsed.TotalSeconds;
        Console.WriteLine($"{workers,3} workers: {progress.Folders:N0} folders in {seconds:F2}s ({progress.Folders / seconds:N0}/s), {matched:N0} matches");
    }

    /// <summary>The walk the app runs on a share, with everything it waits on printed.</summary>
    public static void Trace(string root, string term, int workers)
    {
        Console.WriteLine($"LiveBench trace · {Environment.OSVersion} · {Environment.ProcessorCount} processors · {workers} workers");
        Timed("network volume", () => (root.StartsWith(@"\\", StringComparison.Ordinal) || new DriveInfo(root).DriveType == DriveType.Network).ToString());
        Timed("exists", () => Directory.Exists(root).ToString());
        Timed("typed path exists", () => (Directory.Exists(Path.Join(root, term)) || File.Exists(Path.Join(root, term))).ToString());
        // Listed on its own beside the walk, to see whether the server sends the root's names bit by bit or holds them all back.
        _ = Task.Factory.StartNew(() => ListRoot(root), TaskCreationOptions.LongRunning);

        var listed = new ConcurrentQueue<FolderListed>();
        var progress = new WalkProgress { Listed = listed.Enqueue };
        string needle = Names.Fold(term);
        int matched = 0;
        using var cancel = new CancellationTokenSource();
        Console.CancelKeyPress += (_, e) =>
        {
            e.Cancel = true;
            cancel.Cancel();
        };
        var clock = Stopwatch.StartNew();
        Task walk = Task.Factory.StartNew(() => Walker.Walk(root, skipHidden: true, cancel.Token, (_, files) =>
        {
            List<string> kept = files.Where(file => Names.Fold(file).Contains(needle, StringComparison.Ordinal)).ToList();
            Interlocked.Add(ref matched, kept.Count);
            return kept;
        }, (_, _) => { }, new LiveWalkOptions(workers, TypedFolder(root, term), progress)), TaskCreationOptions.LongRunning);

        while (!walk.Wait(Every))
        {
            IReadOnlyList<(string Relative, TimeSpan Waited)> inFlight = progress.InFlight;
            Console.WriteLine($"{clock.Elapsed.TotalSeconds,6:F1}s  {progress.Folders,10:N0} folders  {Volatile.Read(ref matched),8:N0} matches  {inFlight.Count} listing  {progress.Skipped.Describe()}");
            foreach (var (relative, waited) in inFlight.Where(folder => folder.Waited >= Every).Take(InFlightShown))
            {
                Console.WriteLine($"          waiting {waited.TotalSeconds,6:F1}s on {Shown(relative)}");
            }
        }
        string outcome = walk.Exception?.GetBaseException() is Exception error ? $"{error.GetType().Name}: {error.Message}" : "finished";
        Console.WriteLine($"{outcome} after {clock.Elapsed.TotalSeconds:F2}s · {progress.Folders:N0} folders · {matched:N0} matches · {progress.Skipped.Describe() ?? "nothing skipped"}");
        Summarise(listed.ToList(), progress.InFlight);
    }

    private static void Summarise(List<FolderListed> listed, IReadOnlyList<(string Relative, TimeSpan Waited)> stillListing)
    {
        if (listed.Count > 0)
        {
            List<double> times = listed.Select(folder => folder.Took.TotalMilliseconds).Order().ToList();
            Console.WriteLine($"listings: {listed.Count:N0} · median {times[times.Count / 2]:F0} ms · 95% {times[times.Count * 95 / 100]:F0} ms · slowest {times[^1]:F0} ms");
        }
        Console.WriteLine($"slowest {Slowest}:");
        foreach (FolderListed folder in listed.OrderByDescending(folder => folder.Took).Take(Slowest))
        {
            string failed = folder.Error is null ? "" : $"  {folder.Error.GetType().Name}";
            Console.WriteLine($"  {folder.Took.TotalMilliseconds,8:F0} ms  {folder.Entries,8:N0} names  {Shown(folder.Relative)}{failed}");
        }
        foreach (var kind in listed.Where(folder => folder.Error is not null).GroupBy(folder => $"{folder.Error!.GetType().Name} 0x{folder.Error.HResult:X8}"))
        {
            Console.WriteLine($"failed {kind.Count():N0}× {kind.Key} · {kind.First().Error!.Message}");
            foreach (FolderListed folder in kind.Take(ExamplesPerFailure))
            {
                Console.WriteLine($"  {Shown(folder.Relative)}");
            }
        }
        foreach (var (relative, waited) in stillListing)
        {
            Console.WriteLine($"never answered ({waited.TotalSeconds:F0}s): {Shown(relative)}");
        }
    }

    /// <summary>The typed path as a folder to read first, as the app does: itself if a folder, its folder if a file.</summary>
    private static List<string> TypedFolder(string root, string term)
    {
        string path = Path.Join(root, term);
        return Directory.Exists(path) ? [term]
            : File.Exists(path) && Path.GetDirectoryName(term) is { Length: > 0 } parent ? [parent] : [];
    }

    /// <summary>Prints when the root's first name arrives, how many have come every 5 s, and how the listing ends.</summary>
    private static void ListRoot(string root)
    {
        var clock = Stopwatch.StartNew();
        TimeSpan report = TimeSpan.FromSeconds(5);
        int names = 0;
        try
        {
            foreach (string _ in Directory.EnumerateFileSystemEntries(root))
            {
                if (++names == 1)
                {
                    Console.WriteLine($"  root listing: first name after {clock.Elapsed.TotalSeconds:F1}s");
                }
                if (clock.Elapsed >= report)
                {
                    Console.WriteLine($"  root listing: {names:N0} names after {clock.Elapsed.TotalSeconds:F0}s");
                    report += TimeSpan.FromSeconds(5);
                }
            }
            Console.WriteLine($"  root listing: done, {names:N0} names in {clock.Elapsed.TotalSeconds:F1}s");
        }
        catch (Exception error)
        {
            Console.WriteLine($"  root listing: {error.GetType().Name} 0x{error.HResult:X8} after {clock.Elapsed.TotalSeconds:F0}s and {names:N0} names: {error.Message}");
        }
    }

    private static void Timed(string what, Func<string> check)
    {
        var clock = Stopwatch.StartNew();
        string result;
        try
        {
            result = check();
        }
        catch (Exception error)
        {
            result = $"{error.GetType().Name} 0x{error.HResult:X8}: {error.Message}";
        }
        Console.WriteLine($"  {what,-18} {clock.Elapsed.TotalMilliseconds,8:F0} ms  {result}");
    }

    private static string Shown(string relative) => relative.Length == 0 ? "(root)" : relative;
}
