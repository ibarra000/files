using Files.Services;

namespace Files.Tests;

public sealed class HotkeySlotTests
{
    /// <summary>Stands in for RegisterHotKey: one combination per id, and a combination can be held only once.</summary>
    private sealed class FakeWindows
    {
        public Dictionary<int, (uint Modifiers, uint Key)> Held { get; } = [];
        public HashSet<(uint, uint)> TakenByOthers { get; } = [];

        public bool Register(int id, uint modifiers, uint key)
        {
            uint plain = modifiers & ~0x4000u;
            if (TakenByOthers.Contains((plain, key)) || Held.Values.Any(held => (held.Modifiers & ~0x4000u) == plain && held.Key == key))
            {
                return false;
            }
            Held[id] = (modifiers, key);
            return true;
        }

        public bool Unregister(int id) => Held.Remove(id);
    }

    private static (HotkeySlot Slot, FakeWindows Windows) NewSlot()
    {
        var windows = new FakeWindows();
        return (new HotkeySlot(windows.Register, windows.Unregister), windows);
    }

    [Fact]
    public void A_free_combination_replaces_the_old_one()
    {
        var (slot, windows) = NewSlot();
        Assert.Null(slot.Register("Ctrl+Shift+Space"));
        Assert.Null(slot.Register("Alt+Space"));
        Assert.Equal("Alt+Space", slot.Active);
        Assert.Single(windows.Held);
    }

    [Fact]
    public void A_taken_combination_keeps_the_old_one_working()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");
        windows.TakenByOthers.Add((0x1 /* Alt */, 0x20 /* Space */));

        string? problem = slot.Register("Alt+Space");

        Assert.Equal("Alt+Space is already used by another program", problem);
        Assert.Equal("Ctrl+Shift+Space", slot.Active);
        Assert.Single(windows.Held);
    }

    [Fact]
    public void The_same_combination_again_changes_nothing()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");
        Assert.Null(slot.Register("Ctrl+Shift+Space"));
        Assert.Single(windows.Held);
    }

    [Fact]
    public void The_same_keys_written_another_way_are_the_same_shortcut()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");
        Assert.Null(slot.Register("Shift+Ctrl+Space"));
        Assert.Single(windows.Held);
    }

    [Fact]
    public void A_combination_that_failed_at_start_is_tried_again()
    {
        var (slot, windows) = NewSlot();
        windows.TakenByOthers.Add((0x2 | 0x4 /* Ctrl+Shift */, 0x20));
        Assert.NotNull(slot.Register("Ctrl+Shift+Space"));
        Assert.Null(slot.Active);

        windows.TakenByOthers.Clear();

        Assert.Null(slot.Register("Ctrl+Shift+Space"));
        Assert.Equal("Ctrl+Shift+Space", slot.Active);
    }

    [Fact]
    public void Off_and_text_that_is_not_a_shortcut()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");
        Assert.Equal("\"Space\" is not a shortcut", slot.Register("Space"));
        Assert.Equal("Ctrl+Shift+Space", slot.Active);

        Assert.Null(slot.Register(Hotkey.Off));
        Assert.Null(slot.Active);
        Assert.Empty(windows.Held);
    }

    [Fact]
    public void Suspended_while_a_shortcut_is_recorded_then_resumed_with_the_new_one()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");

        slot.Suspend();
        Assert.Empty(windows.Held);

        (string kept, string? problem) = slot.Resume("Alt+Space");
        Assert.Null(problem);
        Assert.Equal("Alt+Space", kept);
        Assert.Equal("Alt+Space", slot.Active);
    }

    [Fact]
    public void Resuming_with_a_taken_combination_brings_the_old_one_back()
    {
        var (slot, windows) = NewSlot();
        slot.Register("Ctrl+Shift+Space");
        slot.Suspend();
        windows.TakenByOthers.Add((0x1, 0x20));

        (string kept, string? problem) = slot.Resume("Alt+Space");

        Assert.NotNull(problem);
        Assert.Equal("Ctrl+Shift+Space", kept);
        Assert.Equal("Ctrl+Shift+Space", slot.Active);
    }
}
