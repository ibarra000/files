using Files.Core;

namespace Files.Tests;

public sealed class ScreenTextHistoryTests
{
    private static ScreenTextEntry Entry(string text, int minute = 0) =>
        new(new DateTime(2026, 11, 21, 9, minute, 0, DateTimeKind.Utc), "Drawing.pdf - Viewer", [new ScreenTextLine(text, 0.9f)]);

    [Fact]
    public void Reads_are_kept_newest_first_up_to_the_cap()
    {
        IReadOnlyList<ScreenTextEntry> history = [];
        for (int i = 0; i < ScreenTextHistory.MaxEntries + 3; i++)
        {
            history = ScreenTextHistory.Record(history, Entry($"read {i}", i % 60));
        }

        Assert.Equal(ScreenTextHistory.MaxEntries, history.Count);
        Assert.Equal($"read {ScreenTextHistory.MaxEntries + 2}", history[0].Text);
    }

    [Fact]
    public void Reading_the_same_text_again_moves_it_to_the_top_instead_of_repeating_it()
    {
        IReadOnlyList<ScreenTextEntry> history = ScreenTextHistory.Record([], Entry("AB-C-0704"));
        history = ScreenTextHistory.Record(history, Entry("other", 1));
        history = ScreenTextHistory.Record(history, Entry("AB-C-0704", 2));

        Assert.Equal(["AB-C-0704", "other"], history.Select(entry => entry.Text));
    }

    [Fact]
    public void The_history_survives_a_restart_and_an_unreadable_file_is_empty()
    {
        using var temp = new TempFolder();
        string file = Path.Combine(temp.Path, "screen-text.json");
        ScreenTextEntry entry = Entry("AB-C-0704");

        ScreenTextHistory.Save(file, [entry]);
        ScreenTextEntry loaded = Assert.Single(ScreenTextHistory.Load(file));

        Assert.Equal(entry.TimeUtc, loaded.TimeUtc);
        Assert.Equal(entry.Source, loaded.Source);
        Assert.Equal(entry.Lines, loaded.Lines);
        File.WriteAllText(file, "{ not json");
        Assert.Empty(ScreenTextHistory.Load(file));
    }

    [Fact]
    public void Text_with_nothing_in_it_is_not_kept()
    {
        Assert.Empty(ScreenTextHistory.Record([], Entry("   ")));
    }

    [Fact]
    public void A_filter_finds_reads_by_their_text_or_their_window()
    {
        IReadOnlyList<ScreenTextEntry> history = [Entry("AB-C-0704"), Entry("Invoice 2026", 1) with { Source = "Outlook" }];

        Assert.Equal(["Invoice 2026"], ScreenTextHistory.Filter(history, "outlook").Select(entry => entry.Text));
        Assert.Equal(["AB-C-0704"], ScreenTextHistory.Filter(history, "0704").Select(entry => entry.Text));
        Assert.Equal(2, ScreenTextHistory.Filter(history, " ").Count);
    }

    [Fact]
    public void A_line_the_engine_is_unsure_of_is_marked()
    {
        Assert.True(new ScreenTextLine("ab-c-07O4", 0.4f).IsUnsure);
        Assert.False(new ScreenTextLine("AB-C-0704", 0.95f).IsUnsure);
        Assert.False(new ScreenTextLine("from Windows OCR", null).IsUnsure);
    }
}
