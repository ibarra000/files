using Files.Core;
using Files.Services;

namespace Files.Tests;

public sealed class TextCleanupTests
{
    private static TextCleanup Compile(params string[] patterns)
    {
        (TextCleanup? cleanup, IReadOnlyList<string> problems) = TextCleanup.Compile(patterns);
        Assert.Empty(problems);
        return cleanup!;
    }

    [Fact]
    public void A_pattern_is_removed_wherever_it_matches_ignoring_case()
    {
        Assert.Equal(new CleanedText("AB-C-0704.pdf", null), Compile(@"_page\d+").Apply("AB-C-0704_Page1.pdf"));
    }

    [Fact]
    public void Patterns_apply_in_order_and_the_spaces_they_leave_are_tidied()
    {
        TextCleanup cleanup = Compile(@"^DWG\s*", @"\s*Rev\s*[A-Z]$");
        Assert.Equal("1234-56", cleanup.Apply("DWG 1234-56 Rev B").Text);
    }

    [Fact]
    public void Blank_lines_are_not_patterns_and_no_patterns_changes_nothing()
    {
        (TextCleanup? cleanup, IReadOnlyList<string> problems) = TextCleanup.Compile(["", "   "]);
        Assert.Empty(problems);
        Assert.Equal("  as is ", cleanup!.Apply("  as is ").Text);
    }

    [Fact]
    public void An_invalid_pattern_is_named_and_nothing_is_compiled()
    {
        (TextCleanup? cleanup, IReadOnlyList<string> problems) = TextCleanup.Compile(["ok", "(unclosed"]);
        Assert.Null(cleanup);
        Assert.StartsWith("Pattern 2 \"(unclosed\" is not valid:", Assert.Single(problems));
    }

    [Fact]
    public void A_pattern_that_takes_too_long_keeps_the_text_and_says_so()
    {
        // Catastrophic backtracking on a long run of a's that does not end in "b".
        TextCleanup cleanup = Compile(@"^(a+)+b");
        CleanedText cleaned = cleanup.Apply(new string('a', 40) + "!");
        Assert.Equal(new string('a', 40) + "!", cleaned.Text);
        Assert.Contains("took too long", cleaned.Note);
    }

    [Fact]
    public void A_line_with_an_arrow_replaces_its_matches()
    {
        // OCR reads a zero between digits as the letter O.
        Assert.Equal("PN 1024 Owner", Compile(@"(?<=\d)O(?=\d) => 0").Apply("PN 1O24 Owner").Text);
    }

    [Fact]
    public void A_replacement_can_use_the_groups_it_matched()
    {
        Assert.Equal("AB-1234", Compile(@"([A-Z]{2})\s*(\d+) => $1-$2").Apply("AB 1234").Text);
    }

    [Fact]
    public void An_arrow_with_nothing_after_it_removes()
    {
        Assert.Equal("1234", Compile("REV =>").Apply("1234 REV").Text);
    }

    [Fact]
    public void An_arrow_without_spaces_is_part_of_the_pattern()
    {
        Assert.Equal("a b", Compile("x=>y").Apply("a x=>y b").Text);
    }

    [Fact]
    public void An_invalid_pattern_before_an_arrow_is_named_by_its_line()
    {
        (TextCleanup? cleanup, IReadOnlyList<string> problems) = TextCleanup.Compile(["ok", "(unclosed => x"]);
        Assert.Null(cleanup);
        Assert.StartsWith("Pattern 2 \"(unclosed\" is not valid:", Assert.Single(problems));
    }

    [Fact]
    public void Removals_and_replacements_apply_in_their_order()
    {
        TextCleanup cleanup = Compile(@"^DWG\s*", "O => 0", @"\s*Rev\s*[A-Z]$");
        Assert.Equal("1004-56", cleanup.Apply("DWG 1OO4-56 Rev B").Text);
    }

    [Fact]
    public void A_replacement_that_takes_too_long_keeps_the_text_and_says_so()
    {
        TextCleanup cleanup = Compile(@"^(a+)+b => c");
        CleanedText cleaned = cleanup.Apply(new string('a', 40) + "!");
        Assert.Equal(new string('a', 40) + "!", cleaned.Text);
        Assert.Contains("took too long", cleaned.Note);
    }

    [Fact]
    public void Text_under_the_mouse_is_cleaned_and_one_left_empty_says_why()
    {
        TextCleanup cleanup = Compile(@"_page\d+", @"^DRAFT$");

        Assert.Equal(new ScreenRead("AB-C-0704.pdf", "note"), ScreenText.WithRemovals(new ScreenRead("(AB-C-0704_Page1.pdf)", "note"), cleanup));
        ScreenRead empty = ScreenText.WithRemovals(new ScreenRead("DRAFT", null), cleanup);
        Assert.Null(empty.Text);
        Assert.Equal(ScreenText.NothingLeftNote, empty.Problem);
        Assert.Equal(new ScreenRead(null, "No text"), ScreenText.WithRemovals(new ScreenRead(null, "No text"), cleanup));
        // Why reading was weaker is still said when nothing is left.
        Assert.Equal(new ScreenRead(null, $"fell back · {ScreenText.NothingLeftNote}"), ScreenText.WithRemovals(new ScreenRead("DRAFT", "fell back"), cleanup));
    }
}
