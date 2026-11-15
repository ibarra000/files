using Files.Core;

namespace Files.Tests;

public sealed class HistoryTests
{
    [Fact]
    public void A_repeat_moves_to_the_front_instead_of_duplicating()
    {
        IReadOnlyList<string> history = History.Record(["beta", "alpha"], "ALPHA");
        Assert.Equal(["ALPHA", "beta"], history);
    }

    [Fact]
    public void Removing_an_entry_ignores_case_and_keeps_the_order()
    {
        Assert.Equal(["gamma", "alpha"], History.Remove(["gamma", "BETA", "alpha"], "beta"));
    }

    [Fact]
    public void Removing_an_entry_that_is_not_there_changes_nothing()
    {
        IReadOnlyList<string> history = ["alpha"];
        Assert.Same(history, History.Remove(history, "beta"));
    }

    [Fact]
    public void A_longer_search_is_its_own_entry()
    {
        Assert.Equal(["AB-C-0704", "AB-C"], History.Record(["AB-C"], "AB-C-0704"));
    }

    [Fact]
    public void Blank_entries_are_not_recorded_and_the_oldest_fall_off_past_50()
    {
        IReadOnlyList<string> history = [];
        Assert.Same(history, History.Record(history, "   "));
        for (int i = 0; i < 60; i++)
        {
            history = History.Record(history, $"s{i:D3}");
        }
        Assert.Equal(50, history.Count);
        Assert.Equal("s059", history[0]);
        Assert.Equal("s010", history[^1]);
    }

    [Fact]
    public void History_survives_a_save_and_load()
    {
        using var temp = new TempFolder();
        string path = Path.Combine(temp.Path, "history.txt");
        History.Save(path, ["one", "two"]);
        Assert.Equal(["one", "two"], History.Load(path));
    }

    [Fact]
    public void Recall_steps_back_to_the_oldest_and_starts_over_when_reset()
    {
        var recall = new Recall();
        recall.Reset(["newest", "older", "oldest"]);

        Assert.False(recall.IsActive);
        Assert.Equal("newest", recall.Older(""));
        Assert.Equal("older", recall.Older(""));
        Assert.Equal("oldest", recall.Older(""));
        Assert.Null(recall.Older(""));
        Assert.True(recall.IsActive);
        recall.Reset(["newest"]);
        Assert.False(recall.IsActive);
    }

    [Fact]
    public void Recall_starts_over_after_a_reset()
    {
        var recall = new Recall();
        recall.Reset(["a", "b"]);
        recall.Older("");
        recall.Older("");

        recall.Reset(["c", "a", "b"]);

        Assert.False(recall.IsActive);
        Assert.Equal("c", recall.Older(""));
    }

    [Fact]
    public void Recall_with_no_history_does_nothing()
    {
        var recall = new Recall();
        recall.Reset([]);
        Assert.Null(recall.Older("typed"));
        Assert.Null(recall.Newer());
        Assert.False(recall.IsActive);
    }

    [Fact]
    public void Down_steps_back_through_newer_searches_to_what_was_typed()
    {
        IReadOnlyList<string> history = ["motor", "bracket", "gear"];
        var recall = new Recall();
        recall.Reset(history);

        Assert.Equal("motor", recall.Older("pla"));
        Assert.Equal("bracket", recall.Older("motor"));
        Assert.Equal("motor", recall.Newer());
        Assert.Equal("pla", recall.Newer());

        Assert.False(recall.IsActive);
        Assert.Null(recall.Newer());
        Assert.Equal(["motor", "bracket", "gear"], history);
    }

    [Fact]
    public void Up_at_the_oldest_search_stays_there()
    {
        var recall = new Recall();
        recall.Reset(["newer", "oldest"]);
        recall.Older("draft");
        recall.Older("newer");

        Assert.Null(recall.Older("oldest"));
        Assert.Equal("newer", recall.Newer());
        Assert.Equal("draft", recall.Newer());
    }

    [Fact]
    public void Stepping_again_after_coming_back_keeps_the_new_draft()
    {
        var recall = new Recall();
        recall.Reset(["motor"]);
        recall.Older("first");
        recall.Newer();

        recall.Older("second");

        Assert.Equal("second", recall.Newer());
    }

    [Fact]
    public void A_reset_forgets_the_draft()
    {
        var recall = new Recall();
        recall.Reset(["motor"]);
        recall.Older("typed but never run");

        recall.Reset(["motor"]);

        Assert.False(recall.IsActive);
        Assert.Null(recall.Newer());
    }

    [Fact]
    public void A_past_search_the_same_as_what_is_typed_is_passed_over_both_ways()
    {
        var recall = new Recall();
        recall.Reset(["motor", "bracket"]);

        Assert.Equal("bracket", recall.Older("motor"));
        Assert.Equal("motor", recall.Newer());
        Assert.False(recall.IsActive);
    }
}
