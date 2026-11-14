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
    public void Text_under_the_mouse_is_cleaned_and_one_left_empty_says_why()
    {
        TextCleanup cleanup = Compile(@"_page\d+", @"^DRAFT$");

        Assert.Equal(new ScreenRead("AB-C-0704.pdf", "note"), ScreenText.WithRemovals(new ScreenRead("(AB-C-0704_Page1.pdf)", "note"), cleanup));
        ScreenRead empty = ScreenText.WithRemovals(new ScreenRead("DRAFT", null), cleanup);
        Assert.Null(empty.Text);
        Assert.Equal(ScreenText.NothingLeftNote, empty.Problem);
        Assert.Equal(new ScreenRead(null, "No text"), ScreenText.WithRemovals(new ScreenRead(null, "No text"), cleanup));
    }
}
