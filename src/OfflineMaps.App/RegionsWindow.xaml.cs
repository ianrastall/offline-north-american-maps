using System.Collections.ObjectModel;
using System.ComponentModel;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using OfflineMaps.Interop.Messages;
using Windows.Graphics;

namespace OfflineMaps.App;

/// <summary>A row in the region list; its caption follows the region's state.</summary>
public sealed class RegionItem : INotifyPropertyChanged
{
    private string _caption = "";

    public required CatalogRegion Region { get; init; }

    public string Id => Region.Id;

    public string Name => Region.Name;

    public string Caption
    {
        get => _caption;
        set
        {
            if (_caption != value)
            {
                _caption = value;
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(Caption)));
                PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(AutomationName)));
            }
        }
    }

    public string AutomationName => $"{Name}. {Caption}";

    public event PropertyChangedEventHandler? PropertyChanged;
}

/// <summary>
/// Regions - Offline Maps: one per app, non-modal. Lists every region that can be downloaded;
/// downloads are queued in the map window, which shows their progress.
/// </summary>
public sealed partial class RegionsWindow : Window
{
    private readonly MainWindow _main;
    private readonly List<RegionItem> _all;
    private readonly ObservableCollection<RegionItem> _shown = [];

    public RegionsWindow(MainWindow main)
    {
        _main = main;
        InitializeComponent();
        App.Current.Themes.Attach(this);
        App.Current.Accent.Attach(Root);
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        _all = main.Catalog.Select(r => new RegionItem { Region = r }).ToList();
        RegionList.ItemsSource = _shown;
        ApplyFilter();
        Refresh();
        main.RegionsChanged += Refresh;
        Root.Loaded += (_, _) =>
        {
            double scale = Root.XamlRoot.RasterizationScale;
            var area = DisplayArea.GetFromWindowId(main.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
            int width = Math.Min((int)(960 * scale), area.Width);
            int height = Math.Min((int)(720 * scale), area.Height);
            AppWindow.MoveAndResize(new RectInt32(area.X + (area.Width - width) / 2, area.Y + (area.Height - height) / 2, width, height));
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.PreferredMinimumWidth = Math.Min((int)(800 * scale), area.Width);
                presenter.PreferredMinimumHeight = Math.Min((int)(560 * scale), area.Height);
            }
            // Start on the first installed region, so its state is in view.
            var installed = _main.CurrentStatus?.Regions.FirstOrDefault(r => r.Map is not null);
            RegionList.SelectedItem = _shown.FirstOrDefault(i => i.Id == installed?.Id);
            if (RegionList.SelectedItem is { } item)
            {
                RegionList.ScrollIntoView(item);
            }
            FilterBox.Focus(FocusState.Programmatic);
        };
        Closed += (_, _) =>
        {
            main.RegionsChanged -= Refresh;
            App.Current.Themes.Detach(this);
            App.Current.Accent.Detach(Root);
        };
    }

    private RegionStatus? Installed(string id) => _main.CurrentStatus?.Regions.FirstOrDefault(r => r.Id == id);

    /// <summary>The one-line state of a region, as shown in its row.</summary>
    private string Describe(RegionItem item)
    {
        var parts = new List<string>();
        var job = _main.CurrentJob;
        if (job?.RegionId == item.Id && _main.ProgressText.Length > 0)
        {
            parts.Add(job.Target switch
            {
                UpdateTarget.Map => $"Map: {_main.ProgressText}",
                UpdateTarget.Terrain => $"Terrain: {_main.ProgressText}",
                UpdateTarget.Overlays => $"Flood zones and public lands: {_main.ProgressText}",
                _ => $"Stations and addresses: {_main.ProgressText}",
            });
        }
        else if (_main.IsQueued(item.Id, UpdateTarget.Map) || _main.IsQueued(item.Id, UpdateTarget.Extras) || _main.IsQueued(item.Id, UpdateTarget.Terrain) || _main.IsQueued(item.Id, UpdateTarget.Overlays))
        {
            parts.Add("Queued");
        }
        var installed = Installed(item.Id);
        if (installed?.Map is { } map)
        {
            parts.Add(map.DataDate.Length > 0 ? $"Installed, data of {Format.Date(map.DataDate)}" : "Installed");
            parts.Add(installed.Extras is null ? "no stations and addresses" : "with stations and addresses");
            if (installed.Terrain is not null)
            {
                parts.Add("terrain");
            }
            if (installed.Overlays is not null)
            {
                parts.Add("flood zones and public lands");
            }
        }
        else if (installed is { Problem.Length: > 0 })
        {
            parts.Add("Cannot be opened");
        }
        else if (parts.Count == 0)
        {
            parts.Add($"About {Format.Megabytes(item.Region.PackageBytes)}");
        }
        return string.Join(" · ", parts);
    }

    private void Refresh()
    {
        foreach (var item in _all)
        {
            item.Caption = Describe(item);
        }
        var status = _main.CurrentStatus;
        int count = status?.Regions.Count(r => r.Map is not null) ?? 0;
        ulong bytes = 0;
        foreach (var r in status?.Regions ?? [])
        {
            bytes += r.Map?.Bytes ?? 0;
        }
        StatusText.Text = count == 0
            ? $"No regions installed · {_all.Count} available"
            : $"{count} of {_all.Count} regions installed · maps {Format.Megabytes(bytes)}";
        ShowDetail();
    }

    private void ApplyFilter()
    {
        string filter = FilterBox.Text.Trim();
        var selected = RegionList.SelectedItem;
        _shown.Clear();
        foreach (var item in _all.Where(i => filter.Length == 0 || i.Name.Contains(filter, StringComparison.CurrentCultureIgnoreCase)))
        {
            _shown.Add(item);
        }
        if (selected is RegionItem s && _shown.Contains(s))
        {
            RegionList.SelectedItem = s;
        }
    }

    private void FilterBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (args.Reason == AutoSuggestionBoxTextChangeReason.UserInput)
        {
            ApplyFilter();
        }
    }

    private void RegionList_SelectionChanged(object sender, SelectionChangedEventArgs e) => ShowDetail();

    private void ShowDetail()
    {
        if (RegionList.SelectedItem is not RegionItem item)
        {
            DetailHeading.Text = "Region";
            DetailText.Text = _all.Count == 0
                ? "The region list could not be read. Restart Offline Maps to try again."
                : "Choose a region to download, update or remove it.";
            Commands.Visibility = Visibility.Collapsed;
            SourceText.Text = "";
            return;
        }
        var r = item.Region;
        var installed = Installed(item.Id);
        bool hasMap = installed?.Map is not null;
        bool hasExtras = installed?.Extras is not null;
        bool busy = _main.IsBusy;
        bool mapQueued = _main.IsQueued(item.Id, UpdateTarget.Map) || _main.CurrentJob == new MainWindow.Job(item.Id, UpdateTarget.Map);
        bool extrasQueued = _main.IsQueued(item.Id, UpdateTarget.Extras) || _main.CurrentJob == new MainWindow.Job(item.Id, UpdateTarget.Extras);
        bool terrainQueued = _main.IsQueued(item.Id, UpdateTarget.Terrain) || _main.CurrentJob == new MainWindow.Job(item.Id, UpdateTarget.Terrain);
        bool hasTerrain = installed?.Terrain is not null;
        bool overlaysQueued = _main.IsQueued(item.Id, UpdateTarget.Overlays) || _main.CurrentJob == new MainWindow.Job(item.Id, UpdateTarget.Overlays);
        bool hasOverlays = installed?.Overlays is not null;
        DetailHeading.Text = r.Name;
        var lines = new List<string>();
        if (installed?.Map is { } map)
        {
            lines.Add(map.DataDate.Length > 0
                ? $"Map: data of {Format.Date(map.DataDate)}, {Format.Megabytes(map.Bytes)}."
                : $"Map: downloaded {Format.Date(map.DownloadedAt)}, {Format.Megabytes(map.Bytes)}.");
        }
        else
        {
            lines.Add($"Map: not installed. About {Format.Megabytes(r.PackageBytes)} to download, with offline search prepared from it.");
        }
        if (installed?.Extras is { } extras)
        {
            lines.Add($"Stations and addresses: data of {Format.Date(extras.DataDate)}; {extras.Fuel:N0} fuel stations, {extras.Charging:N0} charging stations, {extras.Addresses:N0} addresses"
                + (extras.Outdoor
                    ? $", {extras.Shelters:N0} shelters, {extras.DrinkingWater:N0} drinking water points, {extras.Campgrounds:N0} campgrounds, {extras.Toilets:N0} toilets."
                    : ". Update to add shelters, drinking water, campgrounds and toilets."));
        }
        else
        {
            lines.Add($"Stations and addresses: not installed. Read from a {Format.Megabytes(r.ExtrasBytes)} download; only what is read from it is kept.");
        }
        if (installed?.Overlays is { } overlays)
        {
            lines.Add($"Flood zones and public lands: {Format.Megabytes(overlays.Bytes)}; flood data effective {overlays.FloodEffective}, {overlays.PublicLands}. Shown with View > Flood zones and View > Public lands.");
        }
        else
        {
            lines.Add("Flood zones and public lands: not installed. FEMA flood hazard zones and USGS public lands, prepared for Offline US Maps and published with its releases.");
        }
        if (installed?.Terrain is { } terrain)
        {
            lines.Add($"Terrain: downloaded {Format.Date(terrain.DownloadedAt)}, {Format.Megabytes(terrain.Bytes)}. Shown as shading and contour lines (View menu).");
        }
        else
        {
            lines.Add($"Terrain: not installed. About {Format.Megabytes(r.TerrainBytes)} from Mapterhorn, for shading and contour lines; needs the map first.");
        }
        if (installed is { Problem.Length: > 0 })
        {
            lines.Add(installed.Problem);
        }
        if (item.Caption.StartsWith("Map:", StringComparison.Ordinal) || item.Caption.StartsWith("Stations", StringComparison.Ordinal) || item.Caption.StartsWith("Terrain:", StringComparison.Ordinal) || item.Caption.StartsWith("Flood", StringComparison.Ordinal) || item.Caption.StartsWith("Queued", StringComparison.Ordinal))
        {
            lines.Add(item.Caption.Split(" · ")[0] + ".");
        }
        DetailText.Text = string.Join(Environment.NewLine + Environment.NewLine, lines);
        Commands.Visibility = Visibility.Visible;
        Label(MapButton, hasMap ? "Update map" : "Download map");
        MapButton.IsEnabled = !mapQueued;
        Label(ExtrasButton, hasExtras ? "Update stations and addresses" : "Download stations and addresses");
        ExtrasButton.IsEnabled = (hasMap || mapQueued) && !extrasQueued;
        Label(OverlaysButton, hasOverlays ? "Update flood zones and public lands" : "Download flood zones and public lands");
        OverlaysButton.IsEnabled = !overlaysQueued;
        Label(TerrainButton, hasTerrain ? "Update terrain" : "Download terrain");
        TerrainButton.IsEnabled = (hasMap || mapQueued) && !terrainQueued && !overlaysQueued;
        Label(RemoveButton, $"Remove {r.Name}…");
        RemoveButton.Visibility = installed is null ? Visibility.Collapsed : Visibility.Visible;
        RemoveButton.IsEnabled = !busy && !mapQueued && !extrasQueued && !terrainQueued;
        SourceText.Text = $"Map, stations and addresses from Geofabrik ({r.SourcePage}); map data © OpenStreetMap contributors (ODbL). Terrain from Mapterhorn (download.mapterhorn.com): USGS 3DEP and Copernicus GLO-30. Sizes are as of November 2026.";
    }

    /// <summary>Sets a command's name as text that wraps in the narrow pane instead of being cut off.</summary>
    private static void Label(Button button, string text)
    {
        AutomationProperties.SetName(button, text);
        if (button.Content is TextBlock { } block)
        {
            block.Text = text;
            return;
        }
        button.Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, TextAlignment = TextAlignment.Center };
    }

    private void MapButton_Click(object sender, RoutedEventArgs e)
    {
        if (RegionList.SelectedItem is RegionItem item)
        {
            _main.Enqueue(item.Id, UpdateTarget.Map);
        }
    }

    private void ExtrasButton_Click(object sender, RoutedEventArgs e)
    {
        if (RegionList.SelectedItem is RegionItem item)
        {
            _main.Enqueue(item.Id, UpdateTarget.Extras);
        }
    }

    private void OverlaysButton_Click(object sender, RoutedEventArgs e)
    {
        if (RegionList.SelectedItem is RegionItem item)
        {
            _main.Enqueue(item.Id, UpdateTarget.Overlays);
        }
    }

    private void TerrainButton_Click(object sender, RoutedEventArgs e)
    {
        if (RegionList.SelectedItem is RegionItem item)
        {
            _main.Enqueue(item.Id, UpdateTarget.Terrain);
        }
    }

    private async void RemoveButton_Click(object sender, RoutedEventArgs e)
    {
        if (RegionList.SelectedItem is RegionItem item)
        {
            await _main.RemoveRegionAsync(item.Id, Root.XamlRoot);
        }
    }
}
