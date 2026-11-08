using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.App;

/// <summary>
/// Map and extras downloads. The core downloads, verifies and activates; the current map stays
/// in use throughout, and a cancelled download resumes next time.
/// </summary>
public sealed partial class MainWindow
{
    private Task? _update;
    private UpdateTarget _updateTarget;

    private async void UpdateMap_Click(object sender, RoutedEventArgs e)
    {
        bool hasMap = _status?.Map is not null;
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = hasMap ? "Update the Illinois map?" : "Download the Illinois map?",
            Content = Text(hasMap
                ? "Offline Maps checks Geofabrik (download.geofabrik.de) for a newer Illinois map and downloads it if there is one, about 360 MB as of November 2026. The current map stays in use until the new one has been downloaded and checked."
                : "Offline Maps downloads the Illinois map from Geofabrik (download.geofabrik.de), about 360 MB as of November 2026, and prepares it for offline search. After that, no connection is needed."),
            PrimaryButtonText = hasMap ? "Update map" : "Download map",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        });
        if (answer == ContentDialogResult.Primary)
        {
            await RunUpdateAsync(UpdateTarget.Map);
        }
    }

    private async void UpdateExtras_Click(object sender, RoutedEventArgs e)
    {
        bool installed = _status?.Extras is not null;
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = installed ? "Update stations and addresses?" : "Download stations and addresses?",
            Content = Text("Fuel stations, charging stations and street addresses are read from Geofabrik's full Illinois data file, about 360 MB as of November 2026. Only what is read from it (about 35 MB) is kept. The map itself is not changed."),
            PrimaryButtonText = installed ? "Update" : "Download",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        });
        if (answer == ContentDialogResult.Primary)
        {
            await RunUpdateAsync(UpdateTarget.Extras);
        }
    }

    private void CancelUpdate_Click(object sender, RoutedEventArgs e)
    {
        if (_update is not null)
        {
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
            _settings.ExtrasOfferClosed = true;
            _settings.Save();
        }
    }

    private Task PrepareSearchAsync() => RunAsync(UpdateTarget.Map, prepareSearch: true);

    private Task RunUpdateAsync(UpdateTarget target) => RunAsync(target, prepareSearch: false);

    private async Task RunAsync(UpdateTarget target, bool prepareSearch)
    {
        if (_core is not { } core || _update is not null)
        {
            return;
        }
        _updateTarget = target;
        string subject = target == UpdateTarget.Map ? "map" : "stations and addresses";
        ErrorBar.IsOpen = false;
        NoMapBar.IsOpen = false;
        ExtrasBar.IsOpen = false;
        UpdateBar.Title = prepareSearch ? "Preparing search" : target == UpdateTarget.Map ? "Updating the map" : "Updating stations and addresses";
        UpdateBar.Message = "Running";
        UpdateProgress.IsIndeterminate = true;
        CancelButton.IsEnabled = true;
        CancelButton.Content = prepareSearch ? "Cancel" : "Cancel download";
        UpdateBar.IsOpen = true;
        CancelUpdateItem.IsEnabled = true;
        UpdateMapItem.IsEnabled = false;
        UpdateExtrasItem.IsEnabled = false;
        _progressTimer.Start();
        try
        {
            if (prepareSearch)
            {
                var work = Task.Run(core.PrepareSearch);
                _update = work;
                await work;
                StatusText.Text = "Search is ready";
            }
            else
            {
                var work = Task.Run(() => core.Update(target));
                _update = work;
                var result = await work;
                StatusText.Text = result.UpToDate
                    ? $"The {subject} {(target == UpdateTarget.Map ? "is" : "are")} already up to date"
                    : $"Completed: {subject} installed";
            }
        }
        catch (MapCoreException ex) when (ex.Kind == ErrorKind.Cancelled)
        {
            StatusText.Text = prepareSearch
                ? "Cancelled. Search is prepared next time Offline Maps starts."
                : "Cancelled. What was downloaded is kept, and the download continues from there next time.";
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"Update of {subject} failed: {ex.Kind} {ex.Message}");
            ShowError(prepareSearch ? "Search could not be prepared" : $"The {subject} could not be updated", ex.Message);
        }
        finally
        {
            _progressTimer.Stop();
            _update = null;
            UpdateBar.IsOpen = false;
            CancelUpdateItem.IsEnabled = false;
        }
        string message = StatusText.Text;
        await RefreshStatusAsync();
        StatusText.Text = message;
        if (SearchBox.Text.Trim().Length > 0)
        {
            await RunSearchAsync();
        }
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
            "Preparing" when _updateTarget == UpdateTarget.Extras => "Reading stations and addresses",
            "Preparing" => "Preparing search",
            "Activating" => "Installing",
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
        UpdateBar.Message = text;
        StatusText.Text = $"{UpdateBar.Title}: {char.ToLowerInvariant(text[0])}{text[1..]}";
    }
}
