using Files.UI;

namespace Files.Tests;

public sealed class FileKeysTests
{
    private static readonly DateTime Asked = new(2026, 1, 1, 9, 0, 0, DateTimeKind.Utc);
    private static readonly DeleteAsk ShiftDelete = new(@"C:\v\report.pdf", Permanently: true, Asked);

    [Fact]
    public void A_held_key_never_confirms_the_question_it_asked()
    {
        Assert.Equal(DeleteAnswer.Ignore, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: true, Asked.AddMilliseconds(30)));
        Assert.Equal(DeleteAnswer.Ignore, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: true, Asked.AddMilliseconds(299)));
    }

    [Fact]
    public void The_same_key_on_the_same_file_a_moment_later_confirms()
    {
        Assert.Equal(DeleteAnswer.Confirm, FileKeys.Answer(ShiftDelete, @"C:\V\REPORT.pdf", permanently: true, Asked.AddMilliseconds(600)));
    }

    [Fact]
    public void A_press_made_before_the_question_showed_never_confirms_it()
    {
        // Two quick presses on a slow share: the second was pressed before the first one's question appeared.
        Assert.Equal(DeleteAnswer.Ignore, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: true, Asked.AddSeconds(-1)));
    }

    [Fact]
    public void The_window_counts_from_the_press_not_from_when_the_drive_answered()
    {
        // Pressed inside the window; the drive took longer to say whether it has a Recycle Bin.
        Assert.Equal(DeleteAnswer.Confirm, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: true, Asked.AddSeconds(3.9)));
    }

    [Fact]
    public void Another_key_another_file_or_a_late_press_asks_again()
    {
        DateTime soon = Asked.AddSeconds(1);
        Assert.Equal(DeleteAnswer.Ask, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: false, soon));
        Assert.Equal(DeleteAnswer.Ask, FileKeys.Answer(ShiftDelete, @"C:\v\plan.pdf", permanently: true, soon));
        Assert.Equal(DeleteAnswer.Ask, FileKeys.Answer(ShiftDelete, @"C:\v\report.pdf", permanently: true, Asked.AddSeconds(10)));
        Assert.Equal(DeleteAnswer.Ask, FileKeys.Answer(null, @"C:\v\report.pdf", permanently: true, soon));
    }

    [Fact]
    public void An_operation_can_be_undone_for_two_minutes()
    {
        Assert.True(FileKeys.CanStillUndo(Asked, Asked.AddMinutes(1)));
        Assert.False(FileKeys.CanStillUndo(Asked, Asked.AddMinutes(3)));
    }
}
