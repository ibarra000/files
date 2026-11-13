using System.Windows;
using System.Windows.Media;
using System.Windows.Controls;
using System.Windows.Media.Animation;
using System.Windows.Shapes;
using Files.Core;
using Files.Services;

namespace Files.UI;

/// <summary>The footer: what needs attention, the key hint, the placeholder, and the turning icon while volumes are read.</summary>
public partial class SearchWindow
{
    // Below these widths the footer keeps fewer key hints, so the status still has room.
    private const double AllHintsWidth = 760;
    private const double SomeHintsWidth = 600;
    private const int SomeHints = 3;

    private IReadOnlyList<KeyHint> _shownHints = [];

    private void UpdateStatus()
    {
        IReadOnlyList<VolumeState> volumes = _app.Indexer.Volumes;
        long files = volumes.Sum(state => (long)(state.Index?.FileCount ?? 0));
        Placeholder.Text = StatusText.Placeholder(files);
        ShowHints(StatusText.Hints(HasNoHits, liveRunning: _liveCancel is not null));

        bool rescanRequested = DateTime.UtcNow < _rescanShownUntil;
        string attention = StatusText.For(volumes, _queryProblem, rescanRequested);
        StatusDot.SetResourceReference(Shape.FillProperty, attention.Length == 0 ? "Ok" : "Warn");
        // The glyph turns only beside the words "Scanning…", not beside a message or a live search.
        bool scanning = false;
        if (_toast is not null)
        {
            Status.Text = _toast;
        }
        else if (_live is LiveSearch live)
        {
            Status.Text = StatusText.ForLive(_result, live.FoldersRead, running: _liveCancel is not null, _liveStopped, live.IsCapped, live.Problems);
        }
        else
        {
            DateTime? newest = volumes.Max(state => state.Volume.LastScannedAt);
            Status.Text = attention.Length > 0 ? attention : StatusText.Indexed(files, newest);
            scanning = rescanRequested || volumes.Any(state => state.ScannedSoFar is not null);
        }
        SetSpinning(IsVisible && scanning);
    }

    /// <summary>The key caps on the right of the footer, as many as fit the panel's width.</summary>
    private void ShowHints(IReadOnlyList<KeyHint> hints)
    {
        int fit = ActualWidth >= AllHintsWidth ? hints.Count : ActualWidth >= SomeHintsWidth ? SomeHints : 1;
        IReadOnlyList<KeyHint> shown = hints.Take(fit).ToList();
        if (shown.SequenceEqual(_shownHints))
        {
            return;
        }
        _shownHints = shown;
        Hints.Children.Clear();
        foreach (KeyHint hint in shown)
        {
            var cap = new Border { Style = (Style)FindResource("Kbd") };
            cap.Child = new TextBlock { Text = hint.Keys, Style = (Style)FindResource("KbdText") };
            var label = new TextBlock { Text = hint.Label, FontSize = 12, Margin = new Thickness(0, 0, 14, 0), VerticalAlignment = VerticalAlignment.Center };
            label.SetResourceReference(TextBlock.ForegroundProperty, "TextDim");
            Hints.Children.Add(cap);
            Hints.Children.Add(label);
        }
    }

    /// <summary>F5: reads every volume again and shows it for at least a moment, so a fast read is still seen.</summary>
    private void Rescan()
    {
        _app.Indexer.RescanAll();
        _rescanShownUntil = DateTime.UtcNow + MinScanShown;
        _rescanTimer.Stop();
        _rescanTimer.Start();
        // A message such as "Copied …" would hide it.
        ShowToast(null);
    }

    /// <summary>Turns the glyph beside "Scanning…", or stops and hides it. Still, not turning, when Windows animations are off.</summary>
    private void SetSpinning(bool spinning)
    {
        bool shown = Spinner.Visibility == Visibility.Visible;
        if (spinning == shown)
        {
            return;
        }
        Spinner.Visibility = spinning ? Visibility.Visible : Visibility.Collapsed;
        StatusDot.Visibility = spinning ? Visibility.Collapsed : Visibility.Visible;
        DoubleAnimation? turn = spinning && SystemParameters.ClientAreaAnimation
            ? new DoubleAnimation(0, 360, SpinDuration) { RepeatBehavior = RepeatBehavior.Forever }
            : null;
        SpinnerTurn.BeginAnimation(RotateTransform.AngleProperty, turn);
    }

    /// <summary>A search is on screen and found nothing (the home list has no result).</summary>
    private bool HasNoHits => _result is { Hits.Count: 0 };

    private void ShowToast(string? message)
    {
        _toastTimer.Stop();
        _toast = message;
        if (message is not null)
        {
            _toastTimer.Start();
        }
        UpdateStatus();
    }
}
