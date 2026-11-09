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

    private async void UpdateAll_Click(object sender, RoutedEventArgs e)
    {
        var installed = (_status?.Regions ?? []).Where(r => r.Map is not null).ToList();
        if (installed.Count == 0)
        {
            return;
        }
        int extras = installed.Count(r => r.Extras is not null);
        int terrain = installed.Count(r => r.Terrain is not null);
        int overlays = installed.Count(r => r.Overlays is not null);
        string names = Format.List(installed.Select(r => r.Name));
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = "Update all regions?",
            Content = Text($"Offline Maps checks Geofabrik (download.geofabrik.de) for newer maps of {names}"
                + (extras > 0 ? ", newer stations and addresses where they are installed" : "")
                + (terrain > 0 ? ", newer terrain where it is installed" : "")
                + (overlays > 0 ? ", newer flood zones and public lands where they are installed" : "")
                + ", and downloads what has changed. Each map is about the size shown in Regions. The installed maps stay in use until each new one has been downloaded and checked."),
            PrimaryButtonText = "Update all",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        });
        if (answer != ContentDialogResult.Primary)
        {
            return;
        }
        foreach (var region in installed)
        {
            Enqueue(region.Id, UpdateTarget.Map);
            if (region.Extras is not null)
            {
                Enqueue(region.Id, UpdateTarget.Extras);
            }
            if (region.Terrain is not null)
            {
                Enqueue(region.Id, UpdateTarget.Terrain);
            }
            if (region.Overlays is not null)
            {
                Enqueue(region.Id, UpdateTarget.Overlays);
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
        await FinishAsync(message ?? "Cancelled. Search is prepared next time Offline Maps starts.");
    }

    /// <summary>Runs one job with the progress bar shown. Returns the result message, or null when cancelled or failed.</summary>
    private async Task<string?> RunAsync(Job? job, string title, Func<Task<string>> start)
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
            ShowError(job is null ? "Search could not be prepared" : $"{Format.Sentence(Subject(job))} could not be updated", ex.Message);
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
            _ => "Running",
        };
        if (progress.Cancelling)
        {
            text = "Cancelling";
        }
        bool determinate = progress.Total > 0 && progress.Phase is "Downloading" or "Preparing" && !progress.Cancelling;
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
