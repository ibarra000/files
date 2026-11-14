using System.Diagnostics;
using System.Globalization;
using System.Text;
using Files.Core;
using Files.Services;
using OcrBench;

// Usage: dotnet run -c Release --project tools/OcrBench -- [--codes N] [--llm model.gguf]... [--out folder]
// Reads the same synthetic crops with Windows OCR and PaddleOCR, corrects what was read in
// several ways, and reports how often a search for the result opens the right file.

int codes = 80;
var models = new List<string>();
string output = Path.Combine("artifacts", "ocrbench");
for (int i = 0; i < args.Length; i++)
{
    switch (args[i])
    {
        case "--codes": codes = int.Parse(args[++i], CultureInfo.InvariantCulture); break;
        case "--llm": models.Add(args[++i]); break;
        case "--out": output = args[++i]; break;
        default: throw new ArgumentException($"unknown argument {args[i]}");
    }
}
Directory.CreateDirectory(output);

IReadOnlyList<Sample> samples = Dataset.Build(codes, variantsPerCode: 4);
VolumeIndex index = Dataset.Index(samples, distractors: 200_000);
Console.WriteLine($"{samples.Count} crops of {codes} codes; index of {index.FileCount:N0} names");

var asRead = new AsRead();
var swap = new IndexSwap(index);
var correctors = new List<ICorrector> { asRead, swap };
var loaded = new List<IDisposable>();
foreach (string model in models)
{
    var stopwatch = Stopwatch.StartNew();
    var llm = new LlmCorrector(model);
    loaded.Add(llm);
    Console.WriteLine($"loaded {llm.Name} in {stopwatch.Elapsed.TotalSeconds:N1} s ({new FileInfo(model).Length / 1e6:N0} MB)");
    correctors.Add(llm);
    correctors.Add(new Checked(llm, swap, index));
}

using var paddle = new PaddleTextReader(Path.Combine(AppContext.BaseDirectory, "models", "v5"));
var engines = new (string Name, ITextReader Reader)[] { ("Windows OCR", new WindowsTextReader()), ("PaddleOCR", paddle) };
var rows = new List<Row>();
foreach (var (engineName, reader) in engines)
{
    // The first read loads PaddleOCR's models; it is not counted.
    await reader.ReadWordsAsync(Dataset.Render(samples[0], reader.Scale).Png, CancellationToken.None);
    foreach (Sample sample in samples)
    {
        var (png, pointer) = Dataset.Render(sample, reader.Scale);
        var clock = Stopwatch.StartNew();
        var words = await reader.ReadWordsAsync(png, CancellationToken.None);
        double ocrMs = clock.Elapsed.TotalMilliseconds;
        string read = ScreenText.Nearest(words, pointer) ?? "";
        var timeOf = new Dictionary<string, double>();
        foreach (ICorrector corrector in correctors)
        {
            clock.Restart();
            string searched = (await corrector.CorrectAsync(read)).Trim();
            // A checked model reuses the model's answer, so the model's own time is counted with it.
            double fixMs = clock.Elapsed.TotalMilliseconds + (corrector is Checked check ? timeOf[check.ModelName] : 0);
            timeOf[corrector.Name] = fixMs;
            var (top1, top5) = Opens(index, searched, sample.Code);
            rows.Add(new Row(engineName, corrector.Name, sample, read, searched, searched.Equals(sample.Code, StringComparison.OrdinalIgnoreCase), top1, top5, ocrMs, fixMs));
        }
    }
    Console.WriteLine($"{engineName}: done");
}
loaded.ForEach(model => model.Dispose());

Report.Write(output, rows, samples, index, models);
Console.WriteLine($"wrote {Path.GetFullPath(Path.Combine(output, "report.md"))}");

// Whether a search for what was read lists the code's file first (what Enter opens), or in the first five.
static (bool Top1, bool Top5) Opens(VolumeIndex index, string searched, string code)
{
    Query query = Query.Parse(searched);
    if (!query.IsValid)
    {
        return (false, false);
    }
    var hits = Searcher.Search([("bench", index)], query, new HashSet<string>(), CancellationToken.None).Hits;
    var names = hits.Take(5).Select(hit => Path.GetFileNameWithoutExtension(hit.Name)).ToList();
    return (names.Count > 0 && names[0].Equals(code, StringComparison.OrdinalIgnoreCase),
        names.Contains(code, StringComparer.OrdinalIgnoreCase));
}

public sealed record Row(string Engine, string Corrector, Sample Sample, string Read, string Searched, bool Exact, bool Top1, bool Top5, double OcrMs, double FixMs);

public static class Report
{
    public static void Write(string folder, IReadOnlyList<Row> rows, IReadOnlyList<Sample> samples, VolumeIndex index, IReadOnlyList<string> models)
    {
        var csv = new StringBuilder("engine,corrector,id,code,font,size,dark,jpeg,read,searched,exact,top1,top5,ocr_ms,fix_ms\n");
        foreach (Row r in rows)
        {
            csv.AppendLine(string.Join(",", Quote(r.Engine), Quote(r.Corrector), r.Sample.Id, Quote(r.Sample.Code), Quote(r.Sample.Font), r.Sample.SizePx,
                r.Sample.Dark, r.Sample.Jpeg, Quote(r.Read), Quote(r.Searched), r.Exact, r.Top1, r.Top5,
                r.OcrMs.ToString("F1", CultureInfo.InvariantCulture), r.FixMs.ToString("F1", CultureInfo.InvariantCulture)));
        }
        File.WriteAllText(Path.Combine(folder, "results.csv"), csv.ToString());

        var md = new StringBuilder();
        md.AppendLine("# OCR correction benchmark (issue #62)");
        md.AppendLine();
        md.AppendLine($"{samples.Count} crops of {samples.Select(s => s.Code).Distinct().Count()} codes, each drawn at screen size in a line of text " +
            "(Segoe UI, Consolas, Arial, Courier New or Tahoma at 11, 13 or 15 px; light or dark; a third saved as JPEG at quality 35), " +
            $"then enlarged as the app enlarges its screen capture. The index holds {index.FileCount:N0} names, including a look-alike twin for a fifth of the codes.");
        md.AppendLine();
        md.AppendLine("**Opens it** = a search for the result lists the code's file first, so Enter opens it. **Top 5** = the file is among the first five results.");
        md.AppendLine();
        md.AppendLine("| Engine | Correction | Read exactly | Opens it | Top 5 | Correction time (mean / p95) |");
        md.AppendLine("|---|---|---:|---:|---:|---:|");
        foreach (var group in rows.GroupBy(r => (r.Engine, r.Corrector)))
        {
            md.AppendLine($"| {group.Key.Engine} | {group.Key.Corrector} | {Percent(group, r => r.Exact)} | {Percent(group, r => r.Top1)} | {Percent(group, r => r.Top5)} | " +
                $"{group.Average(r => r.FixMs):N1} / {P95(group.Select(r => r.FixMs)):N1} ms |");
        }
        md.AppendLine();
        md.AppendLine("## Opens it, by condition");
        md.AppendLine();
        md.AppendLine("| Engine | Correction | Clean | JPEG | 11 px | 13 px | 15 px | Light | Dark |");
        md.AppendLine("|---|---|---:|---:|---:|---:|---:|---:|---:|");
        foreach (var group in rows.GroupBy(r => (r.Engine, r.Corrector)))
        {
            md.AppendLine($"| {group.Key.Engine} | {group.Key.Corrector} | {Opens(group, r => !r.Sample.Jpeg)} | {Opens(group, r => r.Sample.Jpeg)} | " +
                $"{Opens(group, r => r.Sample.SizePx == 11)} | {Opens(group, r => r.Sample.SizePx == 13)} | {Opens(group, r => r.Sample.SizePx == 15)} | " +
                $"{Opens(group, r => !r.Sample.Dark)} | {Opens(group, r => r.Sample.Dark)} |");
        }
        md.AppendLine();
        md.AppendLine("## Recognition time");
        md.AppendLine();
        foreach (var engine in rows.GroupBy(r => r.Engine))
        {
            var perCrop = engine.GroupBy(r => r.Sample.Id).Select(g => g.First().OcrMs).ToList();
            md.AppendLine($"- {engine.Key}: {perCrop.Average():N0} ms mean, {P95(perCrop):N0} ms p95 per crop");
        }
        foreach (string model in models)
        {
            md.AppendLine($"- Model `{Path.GetFileName(model)}`: {new FileInfo(model).Length / 1e6:N0} MB on disk");
        }
        md.AppendLine();
        md.AppendLine("## Wrong after correction, first 25");
        md.AppendLine();
        md.AppendLine("| Engine | Correction | Code | Read | Searched |");
        md.AppendLine("|---|---|---|---|---|");
        foreach (Row r in rows.Where(r => !r.Top1 && r.Corrector != "as read").Take(25))
        {
            md.AppendLine($"| {r.Engine} | {r.Corrector} | `{r.Sample.Code}` | `{r.Read}` | `{r.Searched}` |");
        }
        File.WriteAllText(Path.Combine(folder, "report.md"), md.ToString());
    }

    private static string Percent(IEnumerable<Row> rows, Func<Row, bool> test)
    {
        var list = rows.ToList();
        return $"{100.0 * list.Count(test) / list.Count:N1}%";
    }

    private static string Opens(IEnumerable<Row> rows, Func<Row, bool> filter)
    {
        var list = rows.Where(filter).ToList();
        return list.Count == 0 ? "–" : $"{100.0 * list.Count(r => r.Top1) / list.Count:N0}%";
    }

    private static double P95(IEnumerable<double> values)
    {
        var sorted = values.Order().ToList();
        return sorted.Count == 0 ? 0 : sorted[(int)Math.Ceiling(0.95 * sorted.Count) - 1];
    }

    private static string Quote(string text) => "\"" + text.Replace("\"", "\"\"") + "\"";
}
