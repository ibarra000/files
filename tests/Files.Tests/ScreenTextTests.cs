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
    public void The_token_where_the_word_starts_wins_over_one_that_merely_contains_it()
    {
        Assert.Equal("AB-C", ScreenText.TokenAround("TAB AB-C", "AB"));
    }

    [Fact]
    public void A_leading_dot_is_part_of_the_name_but_a_full_stop_is_not()
    {
        Assert.Equal(".gitignore", ScreenText.Clean(".gitignore"));
        Assert.Equal("plan.pdf", ScreenText.Clean("plan.pdf."));
    }

    [Fact]
    public void A_blank_word_gives_nothing()
    {
        Assert.Null(ScreenText.TokenAround("some line", "  "));
    }

    [Fact]
    public void The_word_under_the_pointer_wins_over_nearer_edges_of_others()
    {
        var words = new List<ReadWord>
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
        var words = new List<ReadWord>
        {
            ("far", new Rect(0, 0, 40, 20)),
            ("near", new Rect(200, 100, 40, 20)),
        };
        Assert.Equal("near", ScreenText.Nearest(words, new Point(190, 105)));
        Assert.Null(ScreenText.Nearest([], new Point(0, 0)));
    }

    [Fact]
    public void Words_are_put_back_into_lines_top_to_bottom_and_left_to_right()
    {
        var words = new List<ReadWord>
        {
            new("world", new Rect(70, 2, 50, 20), 0.9f),
            new("second", new Rect(0, 40, 60, 20), 0.5f),
            new("hello", new Rect(0, 0, 60, 22), 0.7f),
            new("line", new Rect(70, 41, 40, 18), 0.3f),
        };

        IReadOnlyList<Core.ScreenTextLine> lines = ScreenText.Lines(words);

        Assert.Equal(["hello world", "second line"], lines.Select(line => line.Text));
        Assert.Equal(0.7f, lines[0].Confidence);
        Assert.Equal(0.3f, lines[1].Confidence);
        Assert.Null(ScreenText.Lines([("plain", new Rect(0, 0, 10, 10))])[0].Confidence);
    }

    [Fact]
    public void An_area_is_the_box_between_the_two_corners_dragged_kept_on_the_screen()
    {
        var screen = new System.Drawing.Rectangle(-1920, 0, 3840, 1080);

        Assert.Equal(new System.Drawing.Rectangle(100, 200, 300, 50), ScreenText.Area(new(400, 250), new(100, 200), screen));
        Assert.Equal(new System.Drawing.Rectangle(-1920, 1000, 20, 80), ScreenText.Area(new(-2000, 1000), new(-1900, 1200), screen));
        Assert.Null(ScreenText.Area(new(10, 10), new(12, 40), screen));
    }
}
