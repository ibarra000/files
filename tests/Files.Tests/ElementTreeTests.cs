using System.Windows.Controls;
using System.Windows.Documents;
using Files.UI;

namespace Files.Tests;

public sealed class ElementTreeTests
{
    /// <summary>WPF elements live on an STA thread.</summary>
    private static void OnSta(Action test)
    {
        Exception? error = null;
        var thread = new Thread(() =>
        {
            try
            {
                test();
            }
            catch (Exception caught)
            {
                error = caught;
            }
        });
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        thread.Join();
        if (error is not null)
        {
            throw new InvalidOperationException("The test failed on its STA thread", error);
        }
    }

    [Fact]
    public void A_click_on_text_finds_its_row_without_failing()
    {
        // A result name is a TextBlock of Runs; a Run is not a Visual, so walking the visual tree from it threw.
        OnSta(() =>
        {
            var match = new Run("0704");
            var name = new TextBlock();
            name.Inlines.Add(new Run("AB-C-"));
            name.Inlines.Add(match);
            var row = new ListBoxItem { Content = name };

            Assert.Same(row, ElementTree.AncestorOf<ListBoxItem>(match));
            Assert.Same(name, ElementTree.AncestorOf<TextBlock>(match));
        });
    }

    [Fact]
    public void Nothing_found_or_nothing_given_is_null()
    {
        OnSta(() =>
        {
            var text = new TextBlock();
            Assert.Null(ElementTree.AncestorOf<ListBoxItem>(text));
            Assert.Null(ElementTree.AncestorOf<ListBoxItem>(null));
            Assert.Null(ElementTree.AncestorOf<ListBoxItem>("not an element"));
        });
    }
}
