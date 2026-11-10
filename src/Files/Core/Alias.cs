namespace Files.Core;

/// <summary>A short name that expands to a longer search when it is the whole line.</summary>
public sealed record Alias(string Name, string Code, string Note = "")
{
    /// <summary>Why this alias cannot be added next to <paramref name="existing"/>, or null when it can.</summary>
    public string? Problem(IEnumerable<Alias> existing)
    {
        string name = Name.Trim();
        if (name.Length == 0)
        {
            return "an alias needs a name";
        }
        if (!name.All(char.IsAscii))
        {
            return "an alias name must be plain letters, digits or symbols";
        }
        if (name.Any(char.IsWhiteSpace))
        {
            return "an alias name cannot contain spaces";
        }
        if (existing.Any(other => other.Name.Equals(name, StringComparison.OrdinalIgnoreCase)))
        {
            return "that name is already an alias";
        }
        if (Code.Trim().Length == 0)
        {
            return "an alias needs the search it expands to";
        }
        string? queryProblem = Query.Parse(Code).Problem;
        return queryProblem is null ? null : $"the expansion is not a valid search: {queryProblem}";
    }

    /// <summary>The alias whose name is the whole line, ignoring case, if any.</summary>
    public static Alias? Resolve(IEnumerable<Alias> aliases, string line)
    {
        string trimmed = line.Trim();
        return aliases.FirstOrDefault(alias => alias.Name.Equals(trimmed, StringComparison.OrdinalIgnoreCase));
    }
}
