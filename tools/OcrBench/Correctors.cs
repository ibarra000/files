using Files.Core;

namespace OcrBench;

/// <summary>Turns what text recognition read into what is searched for.</summary>
public interface ICorrector
{
    string Name { get; }

    Task<string> CorrectAsync(string read);
}

/// <summary>The characters text recognition mixes up, in groups that look alike.</summary>
public static class Confusion
{
    private static readonly string[] Groups = ["0OoDQØø", "1Il|i", "5Ss", "8B", "2Zz", "6Gb"];

    /// <summary>The characters <paramref name="c"/> may really be, other than itself.</summary>
    public static string Alternatives(char c)
    {
        string? group = Groups.FirstOrDefault(g => g.Contains(c));
        return group is null ? "" : group.Replace(c.ToString(), "");
    }
}

/// <summary>Searches for what was read, as the app does today.</summary>
public sealed class AsRead : ICorrector
{
    public string Name => "as read";

    public Task<string> CorrectAsync(string read) => Task.FromResult(read);
}

/// <summary>
/// No model: if what was read finds nothing in the index, tries the look-alike characters,
/// fewest changes first, and takes the first variant the index has. The index is the context:
/// it knows which codes exist, which no language model can.
/// </summary>
public sealed class IndexSwap(VolumeIndex index) : ICorrector
{
    private const int MaxVariants = 512;
    private const int MaxChanges = 3;
    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    public string Name => "look-alike swap, checked in the index";

    public Task<string> CorrectAsync(string read) => Task.FromResult(Correct(read));

    private string Correct(string read)
    {
        if (read.Length == 0 || Finds(read))
        {
            return read;
        }
        int[] positions = Enumerable.Range(0, read.Length).Where(i => Confusion.Alternatives(read[i]).Length > 0).ToArray();
        // Search ignores case, so O and o are one try.
        var tried = new HashSet<string>(StringComparer.OrdinalIgnoreCase) { read };
        for (int changes = 1; changes <= Math.Min(MaxChanges, positions.Length); changes++)
        {
            foreach (int[] chosen in Combinations(positions, changes))
            {
                foreach (string variant in Variants(read, chosen))
                {
                    if (!tried.Add(variant))
                    {
                        continue;
                    }
                    if (tried.Count > MaxVariants)
                    {
                        return read;
                    }
                    if (Finds(variant))
                    {
                        return variant;
                    }
                }
            }
        }
        return read;
    }

    private bool Finds(string term)
    {
        Query query = Query.Parse(term);
        return query.IsValid && index.Search("bench", query, NothingHidden, CancellationToken.None).Hits.Any(hit => !hit.Inherited);
    }

    private static IEnumerable<int[]> Combinations(int[] items, int count, int from = 0)
    {
        if (count == 0)
        {
            yield return [];
            yield break;
        }
        for (int i = from; i <= items.Length - count; i++)
        {
            foreach (int[] rest in Combinations(items, count - 1, i + 1))
            {
                yield return [items[i], .. rest];
            }
        }
    }

    private static IEnumerable<string> Variants(string read, int[] positions, int at = 0)
    {
        if (at == positions.Length)
        {
            yield return read;
            yield break;
        }
        foreach (char alternative in Confusion.Alternatives(read[positions[at]]))
        {
            char[] chars = read.ToCharArray();
            chars[positions[at]] = alternative;
            foreach (string variant in Variants(new string(chars), positions, at + 1))
            {
                yield return variant;
            }
        }
    }
}

/// <summary>A model proposes; the index decides. Keeps the model's answer only if the index has it, else falls back.</summary>
public sealed class Checked(ICorrector model, ICorrector fallback, VolumeIndex index) : ICorrector
{
    private static readonly IReadOnlySet<string> NothingHidden = new HashSet<string>();

    public string Name => $"{model.Name}, checked in the index";

    /// <summary>The model whose answers are checked; its time is counted with this one's.</summary>
    public string ModelName => model.Name;

    public async Task<string> CorrectAsync(string read)
    {
        string proposed = await model.CorrectAsync(read);
        Query query = Query.Parse(proposed);
        bool found = query.IsValid && index.Search("bench", query, NothingHidden, CancellationToken.None).Hits.Any(hit => !hit.Inherited);
        return found ? proposed : await fallback.CorrectAsync(read);
    }
}
