using System.Reflection;
using System.Runtime.InteropServices;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;

namespace OfflineMaps.App;

/// <summary>
/// The map window: lifecycle, window placement and commands. The map surface is in
/// MainWindow.Map.cs, search and saved places in MainWindow.Places.cs and downloads in
/// MainWindow.Updates.cs.
/// </summary>
public sealed partial class MainWindow : Window
{
    private readonly AppSettings _settings = App.Current.Settings;
    private readonly DispatcherQueueTimer _searchTimer;
    private readonly DispatcherQueueTimer _progressTimer;
    private MapCore? _core;
    private Status? _status;
    private SettingsWindow? _settingsWindow;
    private bool _placed;
    private bool _closing;
    private bool _allowClose;

    public MainWindow()
    {
        InitializeComponent();
        App.Current.Themes.Attach(this);
        App.Current.Accent.Attach(Root);
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        Title = "Offline Maps";

        _searchTimer = DispatcherQueue.CreateTimer();
        _searchTimer.Interval = TimeSpan.FromMilliseconds(200);
        _searchTimer.IsRepeating = false;
        _searchTimer.Tick += (_, _) => _ = RunSearchAsync();
        _progressTimer = DispatcherQueue.CreateTimer();
        _progressTimer.Interval = TimeSpan.FromMilliseconds(250);
        _progressTimer.Tick += (_, _) => ShowProgress();

        PaneColumn.Width = new GridLength(_settings.SearchPaneWidth);
        SetPaneVisible(_settings.SearchPaneVisible);
        // A closed InfoBar keeps its margin in the layout; collapse it while closed.
        foreach (var bar in new[] { NoMapBar, UpdateBar, ErrorBar, ExtrasBar })
        {
            bar.IsOpen = false;
            bar.Visibility = Visibility.Collapsed;
            bar.RegisterPropertyChangedCallback(InfoBar.IsOpenProperty, (d, _) =>
            {
                var b = (InfoBar)d;
                b.Visibility = b.IsOpen ? Visibility.Visible : Visibility.Collapsed;
            });
        }

        Root.Loaded += OnLoaded;
        App.Current.Themes.Changed += PushTheme;
        App.Current.Accent.Changed += PushTheme;
        AppWindow.Closing += OnClosing;
        AppWindow.Changed += (_, _) => RememberPlacement();
        Closed += (_, _) =>
        {
            App.Current.Themes.Changed -= PushTheme;
            App.Current.Accent.Changed -= PushTheme;
            _settingsWindow?.Close();
            _regionsWindow?.Close();
            App.Current.Accent.Detach(Root);
            App.Current.Themes.Detach(this);
        };
    }

    private async void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (_placed)
        {
            return;
        }
        Place();
        _placed = true;
        try
        {
            _core = await Task.Run(() => MapCore.Open(AppPaths.StoreDir, AppPaths.PackageUrl, AppPaths.ExtrasUrl));
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"The map core could not start: {ex.Message}");
            MapMessage.Text = $"Offline Maps could not open its map folder ({AppPaths.StoreDir}). {ex.Message}";
            StatusText.Text = "Not started";
            return;
        }
        try
        {
            var opened = _core;
            _catalog = await Task.Run(opened.Catalog);
        }
        catch (MapCoreException ex)
        {
            AppLog.Write($"The region list could not be read: {ex.Message}");
        }
        await RefreshStatusAsync();
        ShowPlaces();
        await StartMapViewAsync();
        if (_status is { SearchReady: false } && _status.Regions.Any(r => r.Map is not null))
        {
            await PrepareSearchAsync();
        }
    }

    /// <summary>First run: 1200 × 800, centred; later runs restore the last valid placement.</summary>
    private void Place()
    {
        var area = DisplayArea.GetFromWindowId(AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
        double scale = Root.XamlRoot.RasterizationScale;
        int minWidth = Math.Min((int)(800 * scale), area.Width);
        int minHeight = Math.Min((int)(560 * scale), area.Height);
        int width = Math.Clamp(_settings.WindowWidth > 0 ? _settings.WindowWidth : (int)(1200 * scale), minWidth, area.Width);
        int height = Math.Clamp(_settings.WindowHeight > 0 ? _settings.WindowHeight : (int)(800 * scale), minHeight, area.Height);
        int x = _settings.WindowX == int.MinValue ? area.X + (area.Width - width) / 2 : Math.Clamp(_settings.WindowX, area.X, area.X + area.Width - width);
        int y = _settings.WindowY == int.MinValue ? area.Y + (area.Height - height) / 2 : Math.Clamp(_settings.WindowY, area.Y, area.Y + area.Height - height);
        AppWindow.MoveAndResize(new RectInt32(x, y, width, height));
        if (AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.PreferredMinimumWidth = minWidth;
            presenter.PreferredMinimumHeight = minHeight;
            if (_settings.WindowMaximized)
            {
                presenter.Maximize();
            }
        }
    }

    private void RememberPlacement()
    {
        if (!_placed || AppWindow.Presenter is not OverlappedPresenter presenter)
        {
            return;
        }
        _settings.WindowMaximized = presenter.State == OverlappedPresenterState.Maximized;
        if (presenter.State == OverlappedPresenterState.Restored)
        {
            _settings.WindowWidth = AppWindow.Size.Width;
            _settings.WindowHeight = AppWindow.Size.Height;
            _settings.WindowX = AppWindow.Position.X;
            _settings.WindowY = AppWindow.Position.Y;
        }
    }

    public void ShowAndActivate()
    {
        if (AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized } presenter)
        {
            presenter.Restore();
        }
        AppWindow.Show();
        Activate();
        SetForegroundWindow(WinRT.Interop.WindowNative.GetWindowHandle(this));
    }

    /// <summary>Reads the store's state and brings the window's messages, menus and status up to date.</summary>
    private async Task RefreshStatusAsync()
    {
        if (_core is null)
        {
            return;
        }
        try
        {
            var core = _core;
            _status = await Task.Run(core.GetStatus);
        }
        catch (MapCoreException ex)
        {
            ShowError("The map state could not be read", ex.Message);
            return;
        }
        var status = _status;
        bool hasMap = status.Coverage is not null;
        bool busy = _update is not null;
        var missingExtras = status.Regions.Where(r => r.Map is not null && r.Extras is null).Select(r => r.Name).ToList();
        NoMapBar.IsOpen = !hasMap && !busy && _queue.Count == 0;
        ExtrasBar.IsOpen = missingExtras.Count > 0 && !busy && !_settings.ExtrasOfferClosed;
        ExtrasBar.Title = $"Stations and addresses are not installed for {Format.List(missingExtras)}";
        UpdateAllItem.IsEnabled = !busy && hasMap;
        MapInfoItem.IsEnabled = hasMap;
        bool hasTerrain = status.TerrainCoverage is not null;
        ShadingItem.IsEnabled = hasTerrain;
        ContoursItem.IsEnabled = hasTerrain;
        ShadingItem.IsChecked = _settings.TerrainShading;
        ContoursItem.IsChecked = _settings.ContourLines;
        foreach (var item in new[] { ZoomInItem, ZoomOutItem, FitItem })
        {
            item.IsEnabled = hasMap;
        }
        if (status.MapProblem.Length > 0)
        {
            ShowError("An installed map has a problem", $"{status.MapProblem} Download the region again in Regions to replace it, or remove it.", "Regions…", Regions_Click);
        }
        else if (status.PlacesProblem.Length > 0)
        {
            ShowError("Saved places could not be read", status.PlacesProblem, "Start a new list", ResetPlaces_Click);
        }
        if (!hasMap)
        {
            MapMessage.Text = status.MapProblem.Length > 0
                ? "The installed map cannot be shown."
                : "No map is installed yet.";
            MapMessage.Visibility = Visibility.Visible;
            MapView.Visibility = Visibility.Collapsed;
        }
        if (!busy)
        {
            StatusText.Text = Summary(status);
        }
        UpdatePlaceCommands();
        SyncMap();
        RegionsChanged?.Invoke();
    }

    private static string Summary(Status status)
    {
        var maps = status.Regions.Where(r => r.Map is not null).ToList();
        if (maps.Count == 0)
        {
            return "No map installed";
        }
        string text;
        if (maps.Count == 1)
        {
            var map = maps[0].Map;
            text = map.DataDate.Length > 0
                ? $"{map.RegionName} map, data of {Format.Date(map.DataDate)}"
                : $"{map.RegionName} map, downloaded {Format.Date(map.DownloadedAt)}";
            if (maps[0].Extras is { } extras)
            {
                text += $" · stations and addresses, data of {Format.Date(extras.DataDate)}";
            }
        }
        else
        {
            var dates = maps.Select(r => r.Map.DataDate).Where(d => d.Length > 0).Order().ToList();
            string names = maps.Count <= 3 ? Format.List(maps.Select(r => r.Name)) : $"{maps.Count} regions";
            text = dates.Count == 0 ? $"{names} maps"
                : dates[0] == dates[^1] ? $"{names} maps, data of {Format.Date(dates[0])}"
                : $"{names} maps, data from {Format.Date(dates[0])} to {Format.Date(dates[^1])}";
            int extras = maps.Count(r => r.Extras is not null);
            if (extras > 0)
            {
                text += extras == maps.Count ? " · stations and addresses" : $" · stations and addresses for {extras} of {maps.Count}";
            }
        }
        return status.SearchReady ? text : text + " · search not ready";
    }

    private void ShowError(string title, string message, string? action = null, RoutedEventHandler? onAction = null)
    {
        ErrorBar.Title = title;
        ErrorBar.Message = message;
        if (action is not null && onAction is not null)
        {
            var button = new Button { Content = action };
            button.Click += onAction;
            ErrorBar.ActionButton = button;
        }
        else
        {
            ErrorBar.ActionButton = null;
        }
        ErrorBar.IsOpen = true;
    }

    private async Task<ContentDialogResult> ShowDialogAsync(ContentDialog dialog)
    {
        dialog.XamlRoot = Root.XamlRoot;
        App.Current.Themes.Attach(dialog);
        try
        {
            return await dialog.ShowAsync();
        }
        finally
        {
            App.Current.Themes.Detach(dialog);
        }
    }

    private static TextBlock Text(string text, string style = "JupiterBodyTextStyle") => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        IsTextSelectionEnabled = true,
        Style = (Style)Application.Current.Resources[style],
    };

    private void SetPaneVisible(bool visible)
    {
        _settings.SearchPaneVisible = visible;
        PaneToggle.IsChecked = visible;
        Pane.Visibility = visible ? Visibility.Visible : Visibility.Collapsed;
        PaneSplitter.Visibility = Pane.Visibility;
        PaneColumn.MinWidth = visible ? (double)Application.Current.Resources["JupiterPaneMinWidth"] : 0;
        PaneColumn.Width = visible ? new GridLength(_settings.SearchPaneWidth) : new GridLength(0);
    }

    private void PaneSplitter_ManipulationCompleted(object sender, Microsoft.UI.Xaml.Input.ManipulationCompletedRoutedEventArgs e)
    {
        _settings.SearchPaneWidth = Math.Clamp(PaneColumn.ActualWidth, 240, 900);
    }

    private void Pane_Click(object sender, RoutedEventArgs e) => SetPaneVisible(PaneToggle.IsChecked);

    private void Settings_Click(object sender, RoutedEventArgs e) => OpenSettings();

    private void OpenSettings()
    {
        if (_settingsWindow is null)
        {
            _settingsWindow = new SettingsWindow(this);
            _settingsWindow.Closed += (_, _) => _settingsWindow = null;
        }
        _settingsWindow.Activate();
    }

    /// <summary>Called by the settings window when distance units change.</summary>
    internal void OnUnitsChanged()
    {
        PostUnits();
        if (SearchBox.Text.Trim().Length > 0)
        {
            _ = RunSearchAsync();
        }
    }

    private void Exit_Click(object sender, RoutedEventArgs e) => Close();

    private void Terrain_Click(object sender, RoutedEventArgs e)
    {
        _settings.TerrainShading = ShadingItem.IsChecked;
        _settings.ContourLines = ContoursItem.IsChecked;
        _settings.Save();
        PostTerrain();
    }

    private void Find_Click(object sender, RoutedEventArgs e) => FocusSearch();

    private void FocusSearch()
    {
        if (!_settings.SearchPaneVisible)
        {
            SetPaneVisible(true);
        }
        SearchBox.Focus(FocusState.Keyboard);
    }

    private void CopyCoordinates_Click(object sender, RoutedEventArgs e)
    {
        var (latitude, longitude) = Results.SelectedItem is PaneItem item ? (item.Latitude, item.Longitude) : (_viewLatitude, _viewLongitude);
        CopyText(Format.Coordinates(latitude, longitude));
    }

    private void CopyText(string text)
    {
        var package = new DataPackage();
        package.SetText(text);
        Clipboard.SetContent(package);
        StatusText.Text = $"Copied {text}";
    }

    private void FullScreen_Click(object sender, RoutedEventArgs e) => ToggleFullScreen();

    private void ToggleFullScreen()
    {
        AppWindow.SetPresenter(AppWindow.Presenter.Kind == AppWindowPresenterKind.FullScreen
            ? AppWindowPresenterKind.Overlapped
            : AppWindowPresenterKind.FullScreen);
    }

    private async void MapInfo_Click(object sender, RoutedEventArgs e)
    {
        if (_status is not { } status || status.Coverage is null)
        {
            return;
        }
        var panel = new StackPanel { Spacing = 16 };
        void Row(string label, string value)
        {
            var row = new StackPanel { Spacing = 4 };
            row.Children.Add(Text(label, "JupiterCaptionTextStyle"));
            row.Children.Add(Text(value));
            panel.Children.Add(row);
        }
        foreach (var region in status.Regions)
        {
            var lines = new List<string>();
            if (region.Map is { } map)
            {
                lines.Add(map.DataDate.Length > 0
                    ? $"Map data of {Format.Date(map.DataDate)} (date of the package on the server), downloaded {Format.Date(map.DownloadedAt)}"
                    : $"Map downloaded {Format.Date(map.DownloadedAt)}; the server reported no data date");
                lines.Add($"{Format.Megabytes(map.Bytes)}, {map.Tiles:N0} tiles, zoom {map.MinZoom}–{map.MaxZoom}, from {map.SourceUrl}");
            }
            lines.Add(region.Extras is { } extras
                ? $"Stations and addresses: data of {Format.Date(extras.DataDate)}, downloaded {Format.Date(extras.DownloadedAt)}: {extras.Fuel:N0} fuel stations, {extras.Charging:N0} charging stations, {extras.Addresses:N0} addresses"
                : "Stations and addresses: not installed");
            lines.Add(region.Terrain is { } terrain
                ? $"Terrain: downloaded {Format.Date(terrain.DownloadedAt)}, {Format.Megabytes(terrain.Bytes)}, {terrain.Tiles:N0} tiles to zoom {terrain.MaxZoom}"
                : "Terrain: not installed");
            if (region.Problem.Length > 0)
            {
                lines.Add(region.Problem);
            }
            Row(region.Name, string.Join(Environment.NewLine, lines));
        }
        Row("Map folder", status.StoreDirectory);
        Row("Attribution", "Map data © OpenStreetMap contributors, available under the Open Database License (ODbL). Map package and extracts by Geofabrik."
            + (status.TerrainCoverage is null ? "" : " Terrain from Mapterhorn (mapterhorn.com/attribution): USGS 3D Elevation Program (public domain) and Copernicus GLO-30, © DLR e.V. 2010–2014 and © Airbus Defence and Space GmbH 2014–2018, provided under COPERNICUS by the European Union and ESA."));
        panel.Children.Add(Text("Offline maps show roads as mapped on the data date. They do not show current closures, detours, traffic or emergency conditions.", "JupiterCaptionTextStyle"));
        await ShowDialogAsync(new ContentDialog
        {
            Title = "Map information",
            Content = new ScrollViewer { Content = panel },
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Close,
        });
    }

    private async void About_Click(object sender, RoutedEventArgs e)
    {
        string version = Assembly.GetExecutingAssembly().GetCustomAttribute<AssemblyInformationalVersionAttribute>()?.InformationalVersion ?? "unknown";
        var panel = new StackPanel { Spacing = 8 };
        panel.Children.Add(Text("Part of Jupiter"));
        panel.Children.Add(Text($"Version {version}"));
        panel.Children.Add(Text($"Map core ABI {MapCore.ExpectedAbiVersion} · Windows App SDK, .NET {Environment.Version}", "JupiterCaptionTextStyle"));
        panel.Children.Add(Text("License: not yet chosen. Map data © OpenStreetMap contributors (ODbL). Third-party components are listed in THIRD-PARTY-NOTICES.txt in the program folder.", "JupiterCaptionTextStyle"));
        var dialog = new ContentDialog
        {
            Title = "Offline Maps",
            Content = panel,
            PrimaryButtonText = "Third-party notices",
            CloseButtonText = "Close",
            DefaultButton = ContentDialogButton.Close,
        };
        if (await ShowDialogAsync(dialog) == ContentDialogResult.Primary)
        {
            string notices = await File.ReadAllTextAsync(Path.Combine(AppContext.BaseDirectory, "THIRD-PARTY-NOTICES.txt"));
            await ShowDialogAsync(new ContentDialog
            {
                Title = "Third-party notices",
                Content = new ScrollViewer { Content = Text(notices, "JupiterMonoCaptionTextStyle"), MaxHeight = 520 },
                CloseButtonText = "Close",
            });
        }
    }

    private void OnClosing(AppWindow sender, AppWindowClosingEventArgs args)
    {
        if (_allowClose)
        {
            return;
        }
        args.Cancel = true;
        _ = CloseSafelyAsync();
    }

    /// <summary>
    /// Shutdown: stop a running download (its partial file is kept for resuming), save the
    /// view and settings, close the web view, then release the core.
    /// </summary>
    private async Task CloseSafelyAsync()
    {
        if (_closing)
        {
            return;
        }
        _closing = true;
        try
        {
            if (_update is { } update)
            {
                var answer = await ShowDialogAsync(new ContentDialog
                {
                    Title = "Stop the download and close?",
                    Content = Text("What has been downloaded is kept, and the download continues from there next time."),
                    PrimaryButtonText = "Stop and close",
                    CloseButtonText = "Cancel",
                    DefaultButton = ContentDialogButton.Primary,
                });
                if (answer != ContentDialogResult.Primary)
                {
                    _closing = false;
                    return;
                }
                _core?.Cancel();
                try
                {
                    await update;
                }
                catch (Exception ex) when (ex is MapCoreException or OperationCanceledException)
                {
                }
            }
            RememberPlacement();
            RememberView();
            _settings.Save();
            MapView.Close();
            _core?.Dispose();
            _core = null;
        }
        catch (Exception ex)
        {
            AppLog.Write($"Closing failed: {ex}");
        }
        _allowClose = true;
        Close();
    }

    [LibraryImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static partial bool SetForegroundWindow(nint hwnd);
}
