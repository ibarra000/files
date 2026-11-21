using System.Windows;
using System.Windows.Controls;
using System.Windows.Controls.Primitives;
using System.Windows.Media;
using System.Windows.Media.Effects;

// ThemeMode (the Fluent theme, which the app uses) is marked experimental, as in Files.csproj.
#pragma warning disable WPF0001

namespace Files.Tests;

/// <summary>
/// The popups' shadows take their colour and strength from the theme, and leave room for the blur
/// on every side: a popup is a window as big as its content and cuts off what a shadow draws outside it.
/// </summary>
public sealed class ThemeStylesTests
{
    private static ResourceDictionary Load(string name) =>
        (ResourceDictionary)Application.LoadComponent(new Uri($"/files;component/UI/{name}", UriKind.Relative));

    /// <summary>
    /// One STA thread for every test here, holding the Application: Settings' styles find the panel's
    /// (fonts, buttons) through the application's resources, as they do in the app.
    /// </summary>
    private static readonly Lazy<System.Windows.Threading.Dispatcher> Ui = new(() =>
    {
        System.Windows.Threading.Dispatcher? dispatcher = null;
        using var ready = new ManualResetEventSlim();
        var thread = new Thread(() =>
        {
            _ = System.IO.Packaging.PackUriHelper.UriSchemePack; // registers pack:// for LoadComponent
            _ = Application.Current ?? new Application { ShutdownMode = ShutdownMode.OnExplicitShutdown };
            dispatcher = System.Windows.Threading.Dispatcher.CurrentDispatcher;
            ready.Set();
            System.Windows.Threading.Dispatcher.Run();
        })
        { IsBackground = true };
        thread.SetApartmentState(ApartmentState.STA);
        thread.Start();
        ready.Wait();
        return dispatcher!;
    });

    /// <summary>Runs <paramref name="test"/> with the panel styles in <paramref name="theme"/>, as App.ApplyTheme sets them.</summary>
    private static void InTheme(string theme, Action<ResourceDictionary> test)
    {
        Ui.Value.Invoke(() =>
        {
            ResourceDictionary app = Application.Current.Resources;
            app.MergedDictionaries.Clear();
            app.MergedDictionaries.Add(Load($"PanelTheme.{theme}.xaml"));
            app.MergedDictionaries.Add(Load("PanelStyles.xaml"));
            var settings = new ResourceDictionary();
            settings.MergedDictionaries.Add(Load("SettingsStyles.xaml"));
            test(settings);
        });
    }

    private static IEnumerable<DependencyObject> Descendants(DependencyObject root)
    {
        for (int i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            DependencyObject child = VisualTreeHelper.GetChild(root, i);
            yield return child;
            foreach (DependencyObject below in Descendants(child))
            {
                yield return below;
            }
        }
    }

    private static void AssertRoomFor(DropShadowEffect shadow, Thickness room)
    {
        // The blur spreads its radius every way, moved by its depth (cast straight down).
        Assert.True(room.Left >= shadow.BlurRadius - 1 && room.Right >= shadow.BlurRadius - 1, $"sides {room}");
        Assert.True(room.Top >= shadow.BlurRadius - shadow.ShadowDepth - 1, $"top {room}");
        Assert.True(room.Bottom >= shadow.BlurRadius + shadow.ShadowDepth - 1, $"bottom {room}");
    }

    [Theory]
    [InlineData("Light")]
    [InlineData("Dark")]
    public void The_volume_menu_casts_the_theme_shadow_with_room_for_it(string theme)
    {
        InTheme(theme, _ =>
        {
            ResourceDictionary resources = Application.Current.Resources;
            var menu = new ContextMenu { Style = (Style)resources["PanelMenu"] };
            menu.Items.Add(new MenuItem { Header = "All volumes", Style = (Style)resources["PanelMenuItem"], IsCheckable = true });
            menu.ApplyTemplate();
            menu.Measure(new Size(400, 400));

            DropShadowEffect shadow = Descendants(menu).OfType<UIElement>().Select(e => e.Effect).OfType<DropShadowEffect>().Single();
            Assert.Equal((double)resources["ShadowOpacity"], shadow.Opacity);
            Grid card = Descendants(menu).OfType<Grid>().First();
            AssertRoomFor(shadow, card.Margin);
        });
    }

    [Theory]
    [InlineData("Light")]
    [InlineData("Dark")]
    public void A_settings_dropdown_casts_the_theme_shadow_with_room_for_it(string theme)
    {
        InTheme(theme, settings =>
        {
            ResourceDictionary resources = Application.Current.Resources;
            var box = new ComboBox { ItemsSource = new[] { "One", "Two" } };
            var window = new Window { Resources = settings, Content = box };
            box.ApplyTemplate();
            var popup = (Popup)box.Template.FindName("PART_Popup", box);
            var room = (Grid)popup.Child;
            var shadow = (DropShadowEffect)room.Children.OfType<Border>().First().Effect;

            Assert.Equal((double)resources["ShadowOpacity"], shadow.Opacity);
            AssertRoomFor(shadow, room.Margin);
            // A Popup already places its child's margin outside the field, so moving it back by the room as well
            // put the list over the field (seen on screen).
            Assert.Equal(0, popup.HorizontalOffset);
        });
    }

    [Theory]
    [InlineData("Light")]
    [InlineData("Dark")]
    public void The_result_list_keeps_its_scroll_bar_beside_the_rows(string theme)
    {
        InTheme(theme, _ =>
        {
            // The Fluent theme (App.ThemeMode) lays a ScrollViewer's bar over its content, over the rows' right edge (#77).
            Application.Current.ThemeMode = theme == "Dark" ? ThemeMode.Dark : ThemeMode.Light;
            try
            {
                ResourceDictionary resources = Application.Current.Resources;
                var list = new ListBox { Style = (Style)resources["PanelList"], ItemContainerStyle = (Style)resources["ResultItem"] };
                for (int i = 0; i < 40; i++)
                {
                    list.Items.Add($"result {i}");
                }
                var window = new Window { Content = list, Width = 400, Height = 300 };
                list.Measure(new Size(400, 300));
                list.Arrange(new Rect(0, 0, 400, 300));
                list.UpdateLayout();

                ScrollBar bar = Descendants(list).OfType<ScrollBar>().Single(b => b.Orientation == Orientation.Vertical);
                Assert.Equal(Visibility.Visible, bar.Visibility);
                double barLeft = bar.TransformToAncestor(list).Transform(new Point(0, 0)).X;
                ListBoxItem row = Descendants(list).OfType<ListBoxItem>().First();
                double rowRight = row.TransformToAncestor(list).Transform(new Point(row.ActualWidth, 0)).X;
                Assert.True(rowRight <= barLeft, $"row ends at {rowRight}, bar starts at {barLeft}");
            }
            finally
            {
                Application.Current.ThemeMode = ThemeMode.None;
            }
        });
    }

    [Fact]
    public void Both_themes_define_the_same_keys()
    {
        InTheme("Light", _ =>
        {
            var light = Load("PanelTheme.Light.xaml").Keys.Cast<object>().Select(k => k.ToString()).Order().ToList();
            var dark = Load("PanelTheme.Dark.xaml").Keys.Cast<object>().Select(k => k.ToString()).Order().ToList();
            Assert.Equal(light, dark);
        });
    }
}
