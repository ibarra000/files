using System.Collections.Concurrent;
using System.Collections.Immutable;
using System.Diagnostics;
using Files.Core;
using Files.Data;

namespace Files.Services;

/// <summary>What the app knows about one volume right now.</summary>
/// <param name="Index">The last good index, kept while a rescan runs or fails.</param>
/// <param name="ScannedSoFar">Files read by the scan in progress, if any.</param>
/// <param name="Problem">Why the last scan failed, in words, or null.</param>
public sealed record VolumeState(Volume Volume, VolumeIndex? Index, int? ScannedSoFar, string? Problem);

/// <summary>
/// Keeps every volume's index in memory: loads the shards at start-up, then
/// rescans each volume when it is due, at most two at a time.
/// </summary>
public sealed class Indexer : IDisposable
{
    public const int MaxConcurrentScans = 2;
    private const int SqliteConstraint = 19;
    private static readonly TimeSpan Tick = TimeSpan.FromMinutes(1);
    private static readonly TimeSpan RetryAfterFailure = TimeSpan.FromMinutes(5);
    private static readonly TimeSpan ProgressInterval = TimeSpan.FromMilliseconds(250);

    private readonly Catalog _catalog;
    private readonly string _shardFolder;
    private readonly Func<Settings> _settings;
    private readonly SemaphoreSlim _scanSlots = new(MaxConcurrentScans);
    private readonly ConcurrentDictionary<long, CancellationTokenSource> _running = new();
    private readonly ConcurrentDictionary<long, DateTime> _retryAt = new();
    private readonly object _gate = new();
    private readonly object _shardGate = new();

    // Volumes removed since start, so loading at start-up cannot bring one back. Guarded by _gate.
    private readonly HashSet<long> _removed = [];
    private ImmutableDictionary<long, VolumeState> _states = ImmutableDictionary<long, VolumeState>.Empty;
    private Timer? _timer;

    public Indexer(Catalog catalog, string shardFolder, Func<Settings> settings)
    {
        _catalog = catalog;
        _shardFolder = shardFolder;
        _settings = settings;
    }

    /// <summary>Raised on a worker thread whenever a volume's state changes.</summary>
    public event Action? Changed;

    public IReadOnlyList<VolumeState> Volumes =>
        _states.Values.OrderBy(s => s.Volume.Name, StringComparer.CurrentCultureIgnoreCase).ToList();

    public bool IsScanning => !_running.IsEmpty;

    public void Start()
    {
        Task.Run(LoadAll);
    }

    public void RescanAll()
    {
        foreach (VolumeState state in _states.Values)
        {
            Scan(state.Volume);
        }
    }

    public void Rescan(long id)
    {
        if (_states.TryGetValue(id, out VolumeState? state))
        {
            Scan(state.Volume);
        }
    }

    /// <summary>Adds a volume and starts reading it. Returns a problem in words, or null.</summary>
    public string? Add(string name, string folder)
    {
        if (name.Trim().Length == 0 || folder.Trim().Length == 0)
        {
            return "A volume needs a name and a folder.";
        }
        Volume volume;
        try
        {
            volume = _catalog.Add(name, folder);
        }
        catch (Microsoft.Data.Sqlite.SqliteException error) when (error.SqliteErrorCode == SqliteConstraint)
        {
            return "That folder is already a volume.";
        }
        catch (Microsoft.Data.Sqlite.SqliteException error)
        {
            return $"The volume list could not be changed: {error.Message}";
        }
        catch (Exception error) when (error is ArgumentException or NotSupportedException or IOException)
        {
            return $"That is not a folder path: {error.Message}";
        }
        Put(new VolumeState(volume, null, null, null));
        Scan(volume);
        return null;
    }

    /// <summary>Stops searching a volume and deletes its file list. Returns a problem in words, or null.</summary>
    public string? Remove(long id)
    {
        if (_running.TryGetValue(id, out CancellationTokenSource? scan))
        {
            scan.Cancel();
        }
        if (!_states.TryGetValue(id, out VolumeState? state))
        {
            return null;
        }
        // Held while a scan checks for its volume and writes its file list, so one removed meanwhile leaves no file behind.
        lock (_shardGate)
        {
            try
            {
                _catalog.Remove(id);
            }
            catch (Microsoft.Data.Sqlite.SqliteException error)
            {
                // The read was stopped above; it must not stay "Reading…" until the next one.
                Update(id, current => current with { ScannedSoFar = null });
                return $"The volume list could not be changed: {error.Message}";
            }
            lock (_gate)
            {
                _states = _states.Remove(id);
                _removed.Add(id);
            }
            DeleteShard(state.Volume);
        }
        Changed?.Invoke();
        return null;
    }

    public void Dispose()
    {
        _timer?.Dispose();
        foreach (CancellationTokenSource scan in _running.Values)
        {
            scan.Cancel();
        }
    }

    private void LoadAll()
    {
        try
        {
            foreach (Volume volume in _catalog.All())
            {
                LoadShard(volume);
            }
        }
        catch (Microsoft.Data.Sqlite.SqliteException error)
        {
            Trace.WriteLine($"volume list not read: {error.Message}");
        }
        finally
        {
            // Scheduled even if loading failed, so volumes are still read from the network.
            _timer = new Timer(_ => ScanDue(), null, TimeSpan.Zero, Tick);
        }
    }

    private void LoadShard(Volume stored)
    {
        if (_states.ContainsKey(stored.Id))
        {
            // Added while start-up was loading, and already being read.
            return;
        }
        Volume volume = stored;
        if (volume.Status == VolumeStatus.Scanning)
        {
            // Left over from a run that ended mid-scan.
            _catalog.SetStatus(volume.Id, VolumeStatus.Offline, volume.LastError);
            volume = volume with { Status = VolumeStatus.Offline };
        }
        VolumeIndex? index = null;
        string? problem = null;
        try
        {
            index = Shard.Load(Shard.PathFor(_shardFolder, volume.Uid), volume.Uid, volume.Root);
        }
        catch (Exception error) when (error is Microsoft.Data.Sqlite.SqliteException or IOException or InvalidCastException)
        {
            problem = $"The saved file list could not be read, so it will be read again: {error.Message}";
        }
        // A volume added while start-up was loading already has a newer state; keep it.
        PutIfAbsent(new VolumeState(volume, index, null, problem));
    }

    private void ScanDue()
    {
        int hours = _settings().RescanHours;
        DateTime now = DateTime.UtcNow;
        foreach (VolumeState state in _states.Values)
        {
            if (_retryAt.TryGetValue(state.Volume.Id, out DateTime retry) && retry > now)
            {
                continue;
            }
            bool neverRead = state.Index is null;
            bool due = hours > 0 && (state.Volume.LastScannedAt is not DateTime last || now - last >= TimeSpan.FromHours(hours));
            if (neverRead || due)
            {
                Scan(state.Volume);
            }
        }
    }

    private void Scan(Volume volume)
    {
        var cancel = new CancellationTokenSource();
        if (!_running.TryAdd(volume.Id, cancel))
        {
            return;
        }
        Task.Run(async () =>
        {
            try
            {
                await _scanSlots.WaitAsync(cancel.Token);
                try
                {
                    RunScan(volume, cancel.Token);
                }
                finally
                {
                    _scanSlots.Release();
                }
            }
            catch (OperationCanceledException)
            {
                // Removed or shutting down; a volume still listed must not stay "Reading…".
                Update(volume.Id, state => state with { ScannedSoFar = null });
            }
            catch (Exception error)
            {
                // Nothing awaits this task, so whatever went wrong is shown on the volume instead.
                _retryAt[volume.Id] = DateTime.UtcNow + RetryAfterFailure;
                Update(volume.Id, state => state with { ScannedSoFar = null, Problem = $"Reading stopped: {error.Message}" });
            }
            finally
            {
                // The token source is not disposed: Remove or Dispose may still cancel it.
                _running.TryRemove(volume.Id, out _);
                Changed?.Invoke();
            }
        });
    }

    private void RunScan(Volume volume, CancellationToken cancel)
    {
        SetStatus(volume.Id, VolumeStatus.Scanning, null, problem: null, scannedSoFar: 0);
        // Reported from several reading threads at once: one report at a time, and the count never goes back.
        var progressGate = new object();
        DateTime lastReport = DateTime.MinValue;
        int lastFiles = 0;
        void Progress(int files)
        {
            lock (progressGate)
            {
                if (files <= lastFiles || DateTime.UtcNow - lastReport < ProgressInterval)
                {
                    return;
                }
                lastReport = DateTime.UtcNow;
                lastFiles = files;
                Update(volume.Id, state => state with { ScannedSoFar = files });
            }
        }

        try
        {
            WalkResult walk = Walker.Walk(volume.Root, _settings().HideSystemFiles, cancel, Progress);
            // Checked and written under the lock Remove holds, so a volume removed meanwhile never gets its file list back.
            lock (_shardGate)
            {
                if (!_states.ContainsKey(volume.Id))
                {
                    return;
                }
                Shard.Write(Shard.PathFor(_shardFolder, volume.Uid), volume.Uid, walk.Index);
            }
            _catalog.MarkScanned(volume.Id);
            _retryAt.TryRemove(volume.Id, out _);
            string? problem = null;
            if (walk.Truncated)
            {
                problem = $"Stopped after {walk.Index.FileCount:N0} files because the names no longer fit in memory; add its subfolders as separate volumes";
            }
            else if (walk.Unreadable > 0)
            {
                problem = $"{walk.Unreadable:N0} folders could not be read";
            }
            SetStatus(volume.Id, VolumeStatus.Online, null, problem, scannedSoFar: null, walk.Index);
        }
        catch (Exception error) when (error is IOException or UnauthorizedAccessException or Microsoft.Data.Sqlite.SqliteException)
        {
            _retryAt[volume.Id] = DateTime.UtcNow + RetryAfterFailure;
            SetStatus(volume.Id, VolumeStatus.Offline, Classify(volume.Root, error), error.Message, scannedSoFar: null);
        }
    }

    private void SetStatus(long id, VolumeStatus status, VolumeError? error, string? problem, int? scannedSoFar, VolumeIndex? index = null)
    {
        if (!_states.ContainsKey(id))
        {
            return;
        }
        _catalog.SetStatus(id, status, error);
        Volume? fresh = _catalog.All().FirstOrDefault(v => v.Id == id);
        if (fresh is null)
        {
            return;
        }
        Update(id, state => new VolumeState(fresh, index ?? state.Index, scannedSoFar, problem));
    }

    private void Put(VolumeState state)
    {
        lock (_gate)
        {
            _states = _states.SetItem(state.Volume.Id, state);
        }
        Changed?.Invoke();
    }

    private void PutIfAbsent(VolumeState state)
    {
        lock (_gate)
        {
            // Not brought back by loading at start-up after it was removed.
            if (_states.ContainsKey(state.Volume.Id) || _removed.Contains(state.Volume.Id))
            {
                return;
            }
            _states = _states.Add(state.Volume.Id, state);
        }
        Changed?.Invoke();
    }

    /// <summary>Changes a volume's state, unless it has been removed meanwhile.</summary>
    private void Update(long id, Func<VolumeState, VolumeState> change)
    {
        lock (_gate)
        {
            if (!_states.TryGetValue(id, out VolumeState? current))
            {
                return;
            }
            _states = _states.SetItem(id, change(current));
        }
        Changed?.Invoke();
    }

    private void DeleteShard(Volume volume)
    {
        try
        {
            File.Delete(Shard.PathFor(_shardFolder, volume.Uid));
        }
        catch (IOException)
        {
            // Open elsewhere; a stale shard is ignored because its uid no longer exists.
        }
    }

    internal static VolumeError? Classify(string root, Exception error)
    {
        const int BadNetPath = unchecked((int)0x80070035);
        const int BadNetName = unchecked((int)0x80070043);
        const int InvalidDrive = unchecked((int)0x8007000F);
        return error switch
        {
            UnauthorizedAccessException => VolumeError.PermissionDenied,
            DirectoryNotFoundException when !Directory.Exists(Path.GetPathRoot(root)) => VolumeError.DriveNotFound,
            DirectoryNotFoundException => VolumeError.PathNotFound,
            IOException { HResult: BadNetPath or BadNetName or InvalidDrive } => VolumeError.DriveNotFound,
            _ => null,
        };
    }
}
