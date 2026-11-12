using System.Windows;
using Files.UI;

namespace Files.Tests;

public sealed class BackdropTests
{
    [Fact]
    public void A_window_that_was_never_shown_is_left_alone()
    {
        // Issue #19: a settings change before the panel was first shown threw "Hwnd of zero is not valid".
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try
            {
                Backdrop.Apply(new Window(), dark: false);
            }
            catch (Exception caught)
            {
                error = caught;
            }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();

        Assert.Null(error);
    }
}
