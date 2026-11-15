using System.Diagnostics;
using Files.Core;

// Times live search on a real volume, to compare against other tools and to tune the worker counts.
//
//   dotnet run -c Release --project tools/LiveBench -- <root> <term>
//       The live search the app runs: prints each second how many folders were read and
//       matches found, then when the first match came and how long the whole read took.
//
//   dotnet run -c Release --project tools/LiveBench -- <root> <term> --workers 16,32,64
//       The bare walk with each worker count in turn, to see where listing more folders
//       at once stops helping. A second run of the same count is faster (the server caches).

if (args.Length < 2)
{
    Console.Error.WriteLine("usage: LiveBench <root> <term> [--workers 16,32,64]");
    return 2;
}
string root = args[0];
string term = args[1];
int at = Array.IndexOf(args, "--workers");
if (at >= 0 && at + 1 < args.Length)
{
    foreach (int workers in args[at + 1].Split(',').Select(int.Parse))
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
            Console.WriteLine($"{clock.Elapsed.TotalSeconds,6:F1}s  {live.FoldersRead,10:N0} folders  {matched,8:N0} matches  {live.Reading}");
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
        Walker.Walk(root, skipHidden: true, CancellationToken.None, (_, files) =>
        {
            List<string> kept = files.Where(file => Names.Fold(file).Contains(needle, StringComparison.Ordinal)).ToList();
            Interlocked.Add(ref matched, kept.Count);
            return kept;
        }, (_, _) => { }, new LiveWalkOptions(workers, [], progress));
        double seconds = clock.Elapsed.TotalSeconds;
        Console.WriteLine($"{workers,3} workers: {progress.Folders:N0} folders in {seconds:F2}s ({progress.Folders / seconds:N0}/s), {matched:N0} matches");
    }
}
