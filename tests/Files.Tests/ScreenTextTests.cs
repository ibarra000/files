using System.Windows;
using Files.Services;

namespace Files.Tests;

public sealed class ScreenTextTests
{
    [Fact]
    public void The_whole_code_is_taken_even_when_the_app_splits_words_at_hyphens()
    {
        Assert.Equal("AB-C-0704_Page1.pdf", ScreenText.TokenAround("see AB-C-0704_Page1.pdf for details", "0704"));
    }

    [Fact]
    public void Surrounding_punctuation_is_dropped()
    {
        Assert.Equal("AB-C-0704", ScreenText.TokenAround("(drawing AB-C-0704).", "AB"));
        Assert.Equal("report.pdf", ScreenText.Clean("\"report.pdf\","));
        Assert.Null(ScreenText.Clean("..."));
    }

    [Fact]
    public void A_blank_word_gives_nothing()
    {
        Assert.Null(ScreenText.TokenAround("some line", "  "));
    }

    [Fact]
    public void The_word_under_the_pointer_wins_over_nearer_edges_of_others()
    {
        var words = new List<(string, Rect)>
        {
            ("left", new Rect(0, 0, 50, 20)),
            ("AB-C-0704", new Rect(60, 0, 90, 20)),
            ("right", new Rect(160, 0, 50, 20)),
        };
        Assert.Equal("AB-C-0704", ScreenText.Nearest(words, new Point(100, 10)));
    }

    [Fact]
    public void Off_any_word_the_closest_one_is_taken()
    {
        var words = new List<(string, Rect)>
        {
            ("far", new Rect(0, 0, 40, 20)),
            ("near", new Rect(200, 100, 40, 20)),
        };
        Assert.Equal("near", ScreenText.Nearest(words, new Point(190, 105)));
        Assert.Null(ScreenText.Nearest([], new Point(0, 0)));
    }
}
