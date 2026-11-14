using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.App;

/// <summary>
/// Region downloads, updates and removal. Work is queued and runs one item at a time; the core
/// downloads, verifies and activates, the installed maps stay in use throughout, and a cancelled
/// download resumes next time.
/// </summary>
public sealed partial class MainWindow
{
    /// <summary>One queued download or update.</summary>
    internal sealed record Job(string RegionId, UpdateTarget Target);

    private readonly List<Job> _queue = [];
    private Task? _update;
    private Job? _current;
    private IReadOnlyList<CatalogRegion> _catalog = [];
    private RegionsWindow? _regionsWindow;

    /// <summary>Raised when the installed regions, the queue or the progress change.</summary>
    internal event Action? RegionsChanged;

    internal IReadOnlyList<CatalogRegion> Catalog => _catalog;

    internal Status? CurrentStatus => _status;

    internal Job? CurrentJob => _current;

    internal string ProgressText { get; private set; } = "";

    internal bool IsQueued(string regionId, UpdateTarget target) => _queue.Contains(new Job(regionId, target));

    internal bool IsBusy => _update is not null;

    internal string RegionName(string id) => _catalog.FirstOrDefault(r => r.Id == id)?.Name ?? id;

    private void Regions_Click(object sender, RoutedEventArgs e) => OpenRegions();

    internal void OpenRegions()
    {
        if (_regionsWindow is null)
        {
            _regionsWindow = new RegionsWindow(this);
            _regionsWindow.Closed += (_, _) => _regionsWindow = null;
        }
        _regionsWindow.Activate();
    }

    /// <summary>Adds a download or update to the queue and starts the queue if it is idle.</summary>
    internal void Enqueue(string regionId, UpdateTarget target)
    {
        var job = new Job(regionId, target);
        if (_current == job || _queue.Contains(job))
        {
            return;
        }
        _queue.Add(job);
        RegionsChanged?.Invoke();
        _ = RunQueueAsync();
    }

    /// <summary>What a check for updates found for one installed item.</summary>
    internal sealed record Check(bool Newer, string RemoteDate, ulong Bytes);

    private readonly Dictionary<Job, Check> _checks = [];
    private readonly Dictionary<string, DateTime> _checkedAt = [];

    /// <summary>The last check's result for a region's map, extras, terrain or overlays, if checked.</summary>
    internal Check? CheckFor(string regionId, UpdateTarget target) => _checks.GetValueOrDefault(new Job(regionId, target));

    internal DateTime? CheckedAt(string regionId) => _checkedAt.TryGetValue(regionId, out var at) ? at : null;

    /// <summary>The parts of a region that are installed.</summary>
    private static IEnumerable<UpdateTarget> InstalledTargets(RegionStatus region)
    {
        if (region.Map is not null)
        {
            yield return UpdateTarget.Map;
        }
        if (region.Extras is not null)
        {
            yield return UpdateTarget.Extras;
        }
        if (region.Overlays is not null)
        {
            yield return UpdateTarget.Overlays;
        }
        if (region.Terrain is not null)
        {
            yield return UpdateTarget.Terrain;
        }
    }

    /// <summary>
    /// Asks the servers whether they have anything newer than what is installed for these regions,
    /// downloading nothing. Returns the items with a newer version, or null when the check failed.
    /// </summary>
    internal async Task<List<Job>?> CheckForUpdatesAsync(IReadOnlyList<RegionStatus> regions)
    {
        if (_core is not { } core || _update is not null)
        {
            return null;
        }
        var jobs = regions.SelectMany(r => InstalledTargets(r).Select(t => new Job(r.Id, t))).ToList();
        var newer = new List<Job>();
        UpdateAllItem.IsEnabled = false;
        ProgressText = "Checking for a newer version";
        try
        {
            foreach (var job in jobs)
            {
                _current = job;
                StatusText.Text = $"Checking for updates: {Subject(job)}";
                RegionsChanged?.Invoke();
                var work = Task.Run(() => core.Update(job.Target, job.RegionId, checkOnly: true));
                _update = work;
                var result = await work;
                _checks[job] = new Check(result.Available, result.RemoteDate, result.RemoteBytes);
                if (result.Available)
                {
                    newer.Add(job);
                }
            }
            foreach (var region in regions)
            {
                _checkedAt[region.Id] = DateTime.Now;
            }
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"Checking for updates failed: {ex.Kind} {ex.Message}");
            ShowError("Updates could not be checked", ex.Message);
            return null;
        }
        finally
        {
            _update = null;
            _current = null;
            ProgressText = "";
            UpdateAllItem.IsEnabled = _status?.Coverage is not null;
            RegionsChanged?.Invoke();
        }
        StatusText.Text = newer.Count switch
        {
            0 => "Everything installed is the newest version available",
            1 => "1 update available",
            int n => $"{n} updates available",
        };
        return newer;
    }

    /// <summary>"Illinois map", "Illinois stations and addresses" and so on, for lists.</summary>
    private string Item(Job job) => job.Target switch
    {
        UpdateTarget.Map => $"{RegionName(job.RegionId)} map",
        UpdateTarget.Terrain => $"{RegionName(job.RegionId)} terrain",
        UpdateTarget.Overlays => $"{RegionName(job.RegionId)} flood zones and public lands",
        _ => $"{RegionName(job.RegionId)} stations and addresses",
    };

    /// <summary>The date of what is installed for a job's item, for comparison with a newer one.</summary>
    private string InstalledDate(Job job)
    {
        var region = _status?.Regions.FirstOrDefault(r => r.Id == job.RegionId);
        return job.Target switch
        {
            UpdateTarget.Map => region?.Map?.DataDate ?? "",
            UpdateTarget.Extras => region?.Extras?.DataDate ?? "",
            UpdateTarget.Terrain => region?.Terrain?.DownloadedAt ?? "",
            _ => "",
        };
    }

    /// <summary>The download size of a newer item: as the server reported it, or the catalog's estimate.</summary>
    internal ulong UpdateBytes(Job job)
    {
        if (_checks.GetValueOrDefault(job) is { Bytes: > 0 } check)
        {
            return check.Bytes;
        }
        var region = _catalog.FirstOrDefault(c => c.Id == job.RegionId);
        return job.Target switch
        {
            UpdateTarget.Map => region?.PackageBytes ?? 0,
            UpdateTarget.Extras => region?.ExtrasBytes ?? 0,
            UpdateTarget.Terrain => region?.TerrainBytes ?? 0,
            _ => region?.OverlayBytes ?? 0,
        };
    }

    private async void CheckUpdates_Click(object sender, RoutedEventArgs e)
    {
        var installed = (_status?.Regions ?? []).Where(r => r.Map is not null).ToList();
        if (installed.Count == 0)
        {
            return;
        }
        var newer = await CheckForUpdatesAsync(installed);
        if (newer is null)
        {
            return;
        }
        if (newer.Count == 0)
        {
            await ShowDialogAsync(new ContentDialog
            {
                Title = "Everything is up to date",
                Content = Text($"The installed maps and data for {Format.List(installed.Select(r => r.Name))} are the newest versions available. Nothing was downloaded."),
                CloseButtonText = "OK",
                DefaultButton = ContentDialogButton.Close,
            });
            return;
        }
        var lines = newer.Select(job =>
        {
            string installedDate = InstalledDate(job);
            string remote = _checks[job].RemoteDate;
            string dates = remote.Length > 0
                ? $"version of {Format.Date(remote)}" + (installedDate.Length > 0 ? $" (installed: {Format.Date(installedDate)})" : "")
                : "newer version";
            return $"{Item(job)}: {dates}, about {Format.Megabytes(UpdateBytes(job))}";
        });
        ulong total = (ulong)newer.Sum(j => (decimal)UpdateBytes(j));
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = newer.Count == 1 ? "1 update available" : $"{newer.Count} updates available",
            Content = Text(string.Join(Environment.NewLine, lines)
                + Environment.NewLine + Environment.NewLine
                + "Geofabrik rebuilds its OpenStreetMap files every day, so a newer map usually adds only recent edits. The installed maps keep working without a connection whether or not they are updated."),
            PrimaryButtonText = $"Download {Format.Megabytes(total)}",
            CloseButtonText = "Not now",
            DefaultButton = ContentDialogButton.Close,
        });
        if (answer == ContentDialogResult.Primary)
        {
            foreach (var job in newer)
            {
                Enqueue(job.RegionId, job.Target);
            }
        }
    }

    /// <summary>Offers stations and addresses for every installed region that has none.</summary>
    private async void MissingExtras_Click(object sender, RoutedEventArgs e)
    {
        var missing = (_status?.Regions ?? []).Where(r => r.Map is not null && r.Extras is null).ToList();
        if (missing.Count == 0)
        {
            // Installed before outdoor places: rebuild them.
            missing = (_status?.Regions ?? []).Where(r => r.Extras is { Outdoor: false }).ToList();
        }
        if (missing.Count == 0)
        {
            return;
        }
        ulong bytes = (ulong)missing.Sum(r => (decimal)(_catalog.FirstOrDefault(c => c.Id == r.Id)?.ExtrasBytes ?? 0));
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = "Download stations and addresses?",
            Content = Text($"Fuel and charging stations, street addresses and outdoor places for {Format.List(missing.Select(r => r.Name))} are read from Geofabrik's full data files, about {Format.Megabytes(bytes)} in all as of November 2026. Only what is read from them is kept. The maps themselves are not changed."),
            PrimaryButtonText = "Download",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        });
        if (answer == ContentDialogResult.Primary)
        {
            foreach (var region in missing)
            {
                Enqueue(region.Id, UpdateTarget.Extras);
            }
        }
    }

    /// <summary>Asks, then removes a region's map, stations and addresses.</summary>
    internal async Task RemoveRegionAsync(string regionId, XamlRoot root)
    {
        if (_core is not { } core || _update is not null)
        {
            return;
        }
        string name = RegionName(regionId);
        var dialog = new ContentDialog
        {
            Title = $"Remove {name}?",
            Content = Text($"The {name} map, its search, its stations and addresses, its terrain and its flood zones and public lands are deleted from this computer. Saved places are kept. Downloading the region again needs a connection."),
            PrimaryButtonText = $"Remove {name}",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
            XamlRoot = root,
        };
        App.Current.Themes.Attach(dialog);
        ContentDialogResult answer;
        try
        {
            answer = await dialog.ShowAsync();
        }
        finally
        {
            App.Current.Themes.Detach(dialog);
        }
        if (answer != ContentDialogResult.Primary)
        {
            return;
        }
        if (_update is not null)
        {
            return;
        }
        _queue.RemoveAll(j => j.RegionId == regionId);
        var work = Task.Run(() => core.RemoveRegion(regionId));
        _update = work;
        CancelUpdateItem.IsEnabled = false;
        UpdateAllItem.IsEnabled = false;
        StatusText.Text = $"Removing {name}";
        RegionsChanged?.Invoke();
        try
        {
            await work;
            StatusText.Text = $"Removed {name}";
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"Removing {regionId} failed: {ex.Kind} {ex.Message}");
            ShowError($"{name} could not be removed", ex.Message);
        }
        finally
        {
            _update = null;
        }
        string message = StatusText.Text;
        await RefreshStatusAsync();
        StatusText.Text = message;
        if (SearchBox.Text.Trim().Length > 0)
        {
            await RunSearchAsync();
        }
        RegionsChanged?.Invoke();
        _ = RunQueueAsync();
    }

    private void CancelUpdate_Click(object sender, RoutedEventArgs e)
    {
        if (_update is not null)
        {
            _queue.Clear();
            _core?.Cancel();
            CancelButton.IsEnabled = false;
            CancelUpdateItem.IsEnabled = false;
            ShowProgress();
        }
    }

    private void ExtrasBar_Closed(InfoBar sender, InfoBarClosedEventArgs args)
    {
        if (args.Reason == InfoBarCloseReason.CloseButton)
        {
            bool missing = (_status?.Regions ?? []).Any(r => r.Map is not null && r.Extras is null);
            if (missing)
            {
                _settings.ExtrasOfferClosed = true;
            }
            else
            {
                _settings.OutdoorOfferClosed = true;
            }
            _settings.Save();
        }
    }

    private string Subject(Job job) => job.Target switch
    {
        UpdateTarget.Map => $"the {RegionName(job.RegionId)} map",
        UpdateTarget.Terrain => $"the {RegionName(job.RegionId)} terrain",
        UpdateTarget.Overlays => $"the {RegionName(job.RegionId)} flood zones and public lands",
        _ => $"{RegionName(job.RegionId)} stations and addresses",
    };

    /// <summary>Runs queued jobs until the queue is empty.</summary>
    private async Task RunQueueAsync()
    {
        if (_core is null || _update is not null || _queue.Count == 0)
        {
            return;
        }
        var messages = new List<string>();
        bool cancelled = false;
        while (_queue.Count > 0 && _core is { } core)
        {
            var job = _queue[0];
            _queue.RemoveAt(0);
            string subject = Subject(job);
            string? message = await RunAsync(job, $"Updating {subject}", () => Task.Run(() =>
            {
                var result = core.Update(job.Target, job.RegionId);
                return result.UpToDate ? $"{Format.Sentence(subject)} {(job.Target is UpdateTarget.Extras or UpdateTarget.Overlays ? "are" : "is")} already up to date" : $"Completed: {subject} installed";
            }));
            if (message is null)
            {
                cancelled = true;
                break;
            }
            // Downloaded, or found to be current: either way it is now the newest version.
            if (_checks.ContainsKey(job))
            {
                _checks[job] = new Check(false, "", 0);
            }
            messages.Add(message);
            await RefreshStatusAsync();
        }
        await FinishAsync(cancelled
            ? "Cancelled. What was downloaded is kept, and the download continues from there next time."
            : messages.Count == 1 ? messages[0] : $"Completed {messages.Count} downloads and updates");
    }

    private async Task PrepareSearchAsync()
    {
        if (_core is not { } core || _update is not null)
        {
            return;
        }
        string? message = await RunAsync(null, "Preparing search", () => Task.Run(() =>
        {
            core.PrepareSearch();
            return "Search is ready";
        }));
        await FinishAsync(message ?? "Cancelled. Search is prepared next time Offline North American Maps starts.");
    }

    /// <summary>Runs one job with the progress bar shown. Returns the result message, or null when cancelled or failed.</summary>
    private async Task<string?> RunAsync(Job? job, string title, Func<Task<string>> start, string? failure = null)
    {
        _current = job;
        ErrorBar.IsOpen = false;
        NoMapBar.IsOpen = false;
        ExtrasBar.IsOpen = false;
        UpdateBar.Title = title;
        UpdateBar.Message = "Running";
        ProgressText = "Running";
        UpdateProgress.IsIndeterminate = true;
        CancelButton.IsEnabled = true;
        CancelButton.Content = job is null ? "Cancel" : "Cancel download";
        UpdateBar.IsOpen = true;
        CancelUpdateItem.IsEnabled = true;
        UpdateAllItem.IsEnabled = false;
        RegionsChanged?.Invoke();
        _progressTimer.Start();
        try
        {
            var work = start();
            _update = work;
            return await work;
        }
        catch (MapCoreException ex) when (ex.Kind == ErrorKind.Cancelled)
        {
            return null;
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"{title} failed: {ex.Kind} {ex.Message}");
            _queue.Clear();
            ShowError(failure ?? (job is null ? "Search could not be prepared" : $"{Format.Sentence(Subject(job))} could not be updated"), ex.Message);
            return null;
        }
        finally
        {
            _progressTimer.Stop();
            _update = null;
            _current = null;
            ProgressText = "";
        }
    }

    private async Task FinishAsync(string message)
    {
        UpdateBar.IsOpen = false;
        CancelUpdateItem.IsEnabled = false;
        bool failed = ErrorBar.IsOpen;
        await RefreshStatusAsync();
        if (!failed)
        {
            StatusText.Text = message;
        }
        if (SearchBox.Text.Trim().Length > 0)
        {
            await RunSearchAsync();
        }
        RegionsChanged?.Invoke();
    }

    /// <summary>Reads the core's progress (polled, not pushed) into the progress bar and status bar.</summary>
    private void ShowProgress()
    {
        if (_core is not { } core)
        {
            return;
        }
        ProgressEvent progress;
        try
        {
            progress = core.Progress();
        }
        catch (MapCoreException)
        {
            return;
        }
        if (!progress.Active)
        {
            return;
        }
        string text = progress.Phase switch
        {
            "Checking" => "Checking for a newer version",
            "Downloading" when progress.Total > 0 => $"Downloading: {Format.Megabytes(progress.Done)} of {Format.Megabytes(progress.Total)}",
            "Downloading" => $"Downloading: {Format.Megabytes(progress.Done)}",
            "Verifying" => "Checking the download",
            "Preparing" when progress.Target == UpdateTarget.Extras => "Reading stations and addresses",
            "Preparing" => "Preparing search",
            "Activating" => "Installing",
            "Removing" => "Removing",
            "Moving" when progress.Total > 0 => $"Copied {Format.Megabytes(progress.Done)} of {Format.Megabytes(progress.Total)}",
            "Moving" => "Starting",
            _ => "Running",
        };
        if (progress.Cancelling)
        {
            text = "Cancelling";
        }
        bool determinate = progress.Total > 0 && progress.Phase is "Downloading" or "Preparing" or "Moving" && !progress.Cancelling;
        UpdateProgress.IsIndeterminate = !determinate;
        if (determinate)
        {
            UpdateProgress.Value = 100.0 * progress.Done / progress.Total;
        }
        if (_current is null && progress.RegionName.Length > 0)
        {
            UpdateBar.Title = $"Preparing search for {progress.RegionName}";
        }
        string queued = _queue.Count switch
        {
            0 => "",
            1 => " · 1 more queued",
            int n => $" · {n} more queued",
        };
        UpdateBar.Message = text + queued;
        ProgressText = text;
        StatusText.Text = $"{UpdateBar.Title}: {char.ToLowerInvariant(text[0])}{text[1..]}{queued}";
        RegionsChanged?.Invoke();
    }
}
