using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.App;

/// <summary>
/// Changing the map folder (Settings > Map folder). The core moves its own maps and saved places and rolls back
/// on failure or cancel; the shell records the new folder before the originals are removed, then reopens the
/// core on the new folder.
/// </summary>
public sealed partial class MainWindow
{
    private string? _movingFrom;

    /// <summary>Raised after the map folder changes.</summary>
    internal event Action? MapFolderChanged;

    /// <summary>The core is open, so its maps can be moved.</summary>
    internal bool HasCore => _core is not null;

    /// <summary>
    /// Makes <paramref name="folder"/> the map folder. With <paramref name="move"/>, the maps and saved places
    /// move there first; if that fails or is cancelled they stay where they are and the folder is unchanged.
    /// </summary>
    internal async Task ChangeMapFolderAsync(string folder, bool move)
    {
        if (_update is not null || _closing || AppPaths.StoreOverride is not null)
        {
            return;
        }
        string from = AppPaths.StoreDir;
        string done = $"Map folder changed to {folder}";
        string? leftover = null;
        if (move && _core is { } core)
        {
            _movingFrom = from;
            string? message;
            try
            {
                message = await RunAsync(null, $"Moving the maps to {folder}", async () =>
                {
                    var result = await Task.Run(() => core.MoveStore(folder));
                    // Recorded before the originals are removed, so the maps are never left only in a
                    // folder the settings do not name.
                    UseFolder(folder);
                    if (result.Copied)
                    {
                        UpdateBar.Message = $"Removing the originals from {from}";
                        try
                        {
                            await Task.Run(core.RemoveMovedSource);
                        }
                        catch (MapCoreException ex)
                        {
                            AppLog.Write($"Removing the moved maps failed: {ex.Message}");
                            leftover = ex.Message;
                        }
                    }
                    return result.Bytes == 0 ? done : $"Moved the maps to {folder}";
                }, "The maps could not be moved");
            }
            finally
            {
                _movingFrom = null;
            }
            if (message is null)
            {
                await FinishAsync($"Cancelled. The maps are still in {from}.");
                return;
            }
            done = message;
        }
        else
        {
            UseFolder(folder);
        }
        await ReopenCoreAsync();
        await FinishAsync(done);
        ShowPlaces();
        if (leftover is not null)
        {
            ShowError("Some of the original maps could not be removed", $"{leftover} They can be deleted.");
        }
        MapFolderChanged?.Invoke();
        if (_status is { SearchReady: false } && _status.Regions.Any(r => r.Map is not null))
        {
            await PrepareSearchAsync();
        }
    }

    private void UseFolder(string folder)
    {
        AppPaths.StoreDir = folder;
        _settings.MapFolder = AppPaths.SameFolder(folder, AppPaths.DefaultStoreDir) ? null : folder;
        _settings.Save();
        AppLog.Write($"Map folder changed to {folder}");
    }

    /// <summary>Closes the core and opens one on the current map folder.</summary>
    private async Task ReopenCoreAsync()
    {
        var old = _core;
        _core = null;
        old?.Dispose();
        try
        {
            _core = await Task.Run(() => MapCore.Open(AppPaths.StoreDir, AppPaths.PackageUrl, AppPaths.ExtrasUrl, AppPaths.OverlaysUrl, AppPaths.OverviewPath));
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"The map core could not open {AppPaths.StoreDir}: {ex.Message}");
            ShowError("The map folder could not be opened", $"{AppPaths.StoreDir}: {ex.Message}");
            return;
        }
        if (_catalog.Count == 0)
        {
            try
            {
                var opened = _core;
                _catalog = await Task.Run(opened.Catalog);
            }
            catch (MapCoreException ex)
            {
                AppLog.Write($"The region list could not be read: {ex.Message}");
            }
        }
        // Tiles asked for while the maps were closed came back empty; reload them.
        if (_shownMap is not null)
        {
            _shownMap = "";
        }
        if (_environment is null)
        {
            await StartMapViewAsync();
        }
    }
}
