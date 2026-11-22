namespace Files.Services;

/// <summary>What the panel knows of kensaku for the key the host runs.</summary>
public enum KensakuState
{
    /// <summary>Not asked yet since the host took this key.</summary>
    Unknown,

    /// <summary>It answered the last search.</summary>
    Up,

    /// <summary>Content search is off, or kensaku is not built.</summary>
    Absent,

    /// <summary>The host is starting or restarting it.</summary>
    Starting,

    /// <summary>The last search failed, or another kensaku blocks this one.</summary>
    Failed,
}

/// <summary>
/// The panel's link to the bundled kensaku (issues #87, #97): each content search asks <see cref="KensakuHost"/> how
/// kensaku is, and only searches when it is up; otherwise it says why in words. Used from the UI thread.
/// </summary>
public sealed class KensakuLink(KensakuHost host)
{
    public const string StillStarting = "kensaku is starting";

    /// <summary>Said by a search whose key changed while it waited; the newer search says what is so.</summary>
    private const string SettingsChanged = "the content search settings changed";

    private KensakuKey? _key;

    public KensakuState State { get; private set; }

    /// <summary>Files whose contents match <paramref name="line"/>, or why there are none.</summary>
    public async Task<ContentResult> SearchAsync(string line, CancellationToken cancel)
    {
        KensakuKey? key = host.Key;
        if (key != _key)
        {
            _key = key;
            State = KensakuState.Unknown;
        }
        switch (host.State)
        {
            case KensakuHostState.Up:
                ContentResult result = await new Kensaku(KensakuHost.Port, host.DataFolder).SearchAsync(line, cancel);
                if (host.Key != key)
                {
                    return ContentResult.Unavailable(SettingsChanged);
                }
                State = result.Problem is null ? KensakuState.Up : KensakuState.Failed;
                return result;
            case KensakuHostState.StartingDaemon or KensakuHostState.StartingServe or KensakuHostState.Restarting:
                State = KensakuState.Starting;
                return ContentResult.Unavailable(StillStarting);
            case KensakuHostState.Blocked:
                State = KensakuState.Failed;
                return ContentResult.Unavailable(host.Problem ?? KensakuHost.AnotherKensaku);
            default:
                State = KensakuState.Absent;
                return ContentResult.Unavailable(host.Problem ?? Kensaku.NotRunning);
        }
    }
}
