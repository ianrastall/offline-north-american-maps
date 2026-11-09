using System.Text.Json;
using System.Text.Json.Nodes;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.Web.WebView2.Core;
using OfflineMaps.Interop;
using Windows.Storage.Streams;
using Windows.UI;
using Windows.UI.ViewManagement;

namespace OfflineMaps.App;

/// <summary>
/// The map surface: MapLibre in WebView2, loaded from the app folder as https://map.local/.
/// Tiles and stations are answered from the Rust core; every other origin is refused, so the
/// map never reaches the network.
/// </summary>
public sealed partial class MainWindow
{
    private const string Host = "map.local";
    private static readonly UISettings UiSettings = new();
    private CoreWebView2Environment? _environment;
    private bool _pageReady;
    /// <summary>The content version of the installed maps and extras the page shows.</summary>
    private string? _shownMap;
    private double _viewLatitude;
    private double _viewLongitude;
    private double _viewZoom;

    private async Task StartMapViewAsync()
    {
        try
        {
            _environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, AppPaths.WebViewDataDir, new CoreWebView2EnvironmentOptions());
            await MapView.EnsureCoreWebView2Async(_environment);
        }
        catch (Exception ex)
        {
            AppLog.Write($"WebView2 could not start: {ex}");
            MapMessage.Text = $"The map view could not start. The Microsoft Edge WebView2 Runtime may be missing; reinstall Offline US Maps to add it. ({ex.Message})";
            return;
        }
        var web = MapView.CoreWebView2;
        web.Settings.AreDevToolsEnabled = false;
        web.Settings.AreDefaultContextMenusEnabled = false;
        web.Settings.AreBrowserAcceleratorKeysEnabled = false;
        web.Settings.IsStatusBarEnabled = false;
        web.Settings.IsZoomControlEnabled = false;
        web.Settings.IsGeneralAutofillEnabled = false;
        web.Settings.IsPasswordAutosaveEnabled = false;
        // Every request passes through OnWebResourceRequested, including those from MapLibre's
        // web worker: map.local is answered there (no folder mapping, which bypasses the
        // handler), and every other origin is refused.
        web.AddWebResourceRequestedFilter("*", CoreWebView2WebResourceContext.All, CoreWebView2WebResourceRequestSourceKinds.All);
        web.WebResourceRequested += OnWebResourceRequested;
        web.NavigationStarting += (_, e) =>
        {
            if (!IsLocal(e.Uri))
            {
                e.Cancel = true;
            }
        };
        web.NewWindowRequested += (_, e) => e.Handled = true;
        web.PermissionRequested += (_, e) => e.State = CoreWebView2PermissionState.Deny;
        web.WebMessageReceived += OnWebMessage;
        web.ProcessFailed += (_, e) =>
        {
            AppLog.Write($"The map view stopped: {e.ProcessFailedKind}");
            _pageReady = false;
            _shownMap = null;
            MapView.Visibility = Visibility.Collapsed;
            MapMessage.Text = "The map view stopped unexpectedly. Reopening it…";
            MapMessage.Visibility = Visibility.Visible;
            web.Reload();
        };
        web.NavigationCompleted += (_, e) =>
        {
            if (!e.IsSuccess)
            {
                MapMessage.Text = $"The map page could not be opened ({e.WebErrorStatus}).";
            }
        };
        web.Navigate($"https://{Host}/index.html");
    }

    private static bool IsLocal(string uri) =>
        Uri.TryCreate(uri, UriKind.Absolute, out var parsed) && parsed.Scheme == Uri.UriSchemeHttps && parsed.Host == Host;

    private async void OnWebResourceRequested(CoreWebView2 sender, CoreWebView2WebResourceRequestedEventArgs e)
    {
        string uri = e.Request.Uri;
        if (!IsLocal(uri))
        {
            if (!uri.StartsWith("blob:", StringComparison.Ordinal) && !uri.StartsWith("data:", StringComparison.Ordinal))
            {
                e.Response = _environment!.CreateWebResourceResponse(null, 403, "Offline", "");
            }
            return;
        }
        string path = Uri.UnescapeDataString(new Uri(uri).AbsolutePath);
        bool tile = path.StartsWith("/tiles/", StringComparison.Ordinal);
        bool terrain = path.StartsWith("/terrain/", StringComparison.Ordinal);
        bool overlay = path.StartsWith("/overlays/", StringComparison.Ordinal);
        bool overview = path.StartsWith("/overview/", StringComparison.Ordinal);
        bool stations = path == "/extras/stations.geojson";
        var deferral = e.GetDeferral();
        try
        {
            var core = _core;
            byte[] body;
            string type;
            int status = 200;
            if (!tile && !stations && !terrain && !overlay && !overview)
            {
                (body, type, status) = await Task.Run(() => ReadWebFile(path));
            }
            else if (core is null)
            {
                body = [];
                type = "";
            }
            else if (tile)
            {
                string[] parts = path["/tiles/".Length..].Replace(".pbf", "", StringComparison.Ordinal).Split('/');
                body = parts.Length == 3 && int.TryParse(parts[0], out int z) && int.TryParse(parts[1], out int x) && int.TryParse(parts[2], out int y)
                    ? await Task.Run(() => core.Tile(z, x, y))
                    : [];
                type = "application/x-protobuf";
            }
            else if (overview)
            {
                string[] parts = path["/overview/".Length..].Replace(".pbf", "", StringComparison.Ordinal).Split('/');
                body = parts.Length == 3 && int.TryParse(parts[0], out int z) && int.TryParse(parts[1], out int x) && int.TryParse(parts[2], out int y)
                    ? await Task.Run(() => core.OverviewTile(z, x, y))
                    : [];
                type = "application/x-protobuf";
            }
            else if (overlay)
            {
                string[] parts = path["/overlays/".Length..].Replace(".pbf", "", StringComparison.Ordinal).Split('/');
                body = parts.Length == 3 && int.TryParse(parts[0], out int z) && int.TryParse(parts[1], out int x) && int.TryParse(parts[2], out int y)
                    ? await Task.Run(() => core.OverlayTile(z, x, y))
                    : [];
                type = "application/x-protobuf";
            }
            else if (terrain)
            {
                string[] parts = path["/terrain/".Length..].Replace(".webp", "", StringComparison.Ordinal).Split('/');
                body = parts.Length == 3 && int.TryParse(parts[0], out int z) && int.TryParse(parts[1], out int x) && int.TryParse(parts[2], out int y)
                    ? await Task.Run(() => core.TerrainTile(z, x, y))
                    : [];
                type = "image/webp";
            }
            else
            {
                string json = await Task.Run(core.StationsGeoJson);
                body = System.Text.Encoding.UTF8.GetBytes(json);
                type = "application/geo+json";
            }
            if (status == 404)
            {
                e.Response = _environment!.CreateWebResourceResponse(null, 404, "Not Found", "");
            }
            else if (body.Length == 0)
            {
                e.Response = _environment!.CreateWebResourceResponse(null, 204, "No Content", "");
            }
            else
            {
                var stream = new InMemoryRandomAccessStream();
                using (var writer = new DataWriter(stream.GetOutputStreamAt(0)))
                {
                    writer.WriteBytes(body);
                    await writer.StoreAsync();
                    await writer.FlushAsync();
                    writer.DetachStream();
                }
                stream.Seek(0);
                string cache = tile || stations || terrain || overlay ? "no-store" : "no-cache";
                e.Response = _environment!.CreateWebResourceResponse(stream, 200, "OK", $"Content-Type: {type}\r\nCache-Control: {cache}");
            }
        }
        catch (Exception ex)
        {
            AppLog.Write($"Map data request {path} failed: {ex.Message}");
            e.Response = _environment!.CreateWebResourceResponse(null, 500, "Error", "");
        }
        finally
        {
            deferral.Complete();
        }
    }

    /// <summary>A file from the app's Web folder; paths outside it are not found.</summary>
    private static (byte[] Body, string Type, int Status) ReadWebFile(string path)
    {
        string root = Path.GetFullPath(AppPaths.WebDir) + Path.DirectorySeparatorChar;
        string file = Path.GetFullPath(Path.Combine(root, path.TrimStart('/').Replace('/', Path.DirectorySeparatorChar)));
        if (!file.StartsWith(root, StringComparison.OrdinalIgnoreCase) || !File.Exists(file))
        {
            return ([], "", 404);
        }
        string type = Path.GetExtension(file).ToLowerInvariant() switch
        {
            ".html" => "text/html; charset=utf-8",
            ".js" or ".mjs" => "text/javascript; charset=utf-8",
            ".css" => "text/css; charset=utf-8",
            ".json" => "application/json",
            ".png" => "image/png",
            ".pbf" => "application/x-protobuf",
            _ => "application/octet-stream",
        };
        return (File.ReadAllBytes(file), type, 200);
    }

    private void OnWebMessage(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs args)
    {
        if (!IsLocal(args.Source))
        {
            return;
        }
        try
        {
            using var document = JsonDocument.Parse(args.WebMessageAsJson);
            var message = document.RootElement;
            double Number(string name) => message.GetProperty(name).GetDouble();
            switch (message.GetProperty("kind").GetString())
            {
                case "ready":
                    _pageReady = true;
                    _shownMap = null;
                    SyncMap();
                    break;
                case "view":
                    _viewLatitude = Number("latitude");
                    _viewLongitude = Number("longitude");
                    _viewZoom = Number("zoom");
                    ViewText.Text = $"{Format.Coordinates(_viewLatitude, _viewLongitude)} · zoom {_viewZoom:0.0}";
                    break;
                case "contextmenu":
                    ShowMapMenu(Number("latitude"), Number("longitude"), Number("x"), Number("y"));
                    break;
                case "place":
                    SelectPlace(message.GetProperty("id").GetUInt64());
                    break;
                case "key":
                    HandleMapKey(message.GetProperty("key").GetString() ?? "");
                    break;
                case "error":
                    AppLog.Write($"Map: {message.GetProperty("message").GetString()}");
                    break;
            }
        }
        catch (Exception ex) when (ex is JsonException or KeyNotFoundException or InvalidOperationException or FormatException)
        {
            AppLog.Write($"Unreadable message from the map: {ex.Message}");
        }
    }

    /// <summary>Shortcuts pressed while the map has focus (the web view does not pass them to XAML).</summary>
    private void HandleMapKey(string key)
    {
        switch (key)
        {
            case "Ctrl+F": FocusSearch(); break;
            case "Ctrl+,": OpenSettings(); break;
            case "Ctrl+B": SetPaneVisible(!_settings.SearchPaneVisible); break;
            case "Ctrl+D": SavePlace_Click(this, new RoutedEventArgs()); break;
            case "Ctrl+Shift+C": CopyCoordinates_Click(this, new RoutedEventArgs()); break;
            case "Ctrl+0": Post(new JsonObject { ["kind"] = "fit" }); break;
            case "F11": ToggleFullScreen(); break;
        }
    }

    private void ShowMapMenu(double latitude, double longitude, double x, double y)
    {
        var menu = new MenuFlyout();
        var save = new MenuFlyoutItem { Text = "Save location here…" };
        save.Click += async (_, _) => await AddPlaceAsync(latitude, longitude, "");
        var copy = new MenuFlyoutItem { Text = "Copy coordinates" };
        copy.Click += (_, _) => CopyText(Format.Coordinates(latitude, longitude));
        menu.Items.Add(save);
        menu.Items.Add(copy);
        menu.ShowAt(MapView, new FlyoutShowOptions { Position = new Windows.Foundation.Point(x, y) });
    }

    /// <summary>
    /// Shows newly installed or removed regions and extras, or the first maps once the page is
    /// ready. The page sees one map covering the box around all installed regions.
    /// </summary>
    private void SyncMap()
    {
        if (!_pageReady || _status is not { } status || (status.Coverage is null && !AppPaths.HasOverview))
        {
            return;
        }
        // With nothing installed, the overview of the 48 contiguous states.
        var b = status.Coverage ?? new OfflineMaps.Interop.Messages.Bounds { West = -125, South = 24, East = -66.5, North = 49.5 };
        if (_shownMap == status.ContentVersion)
        {
            PostPlaces();
            return;
        }
        var message = new JsonObject
        {
            ["kind"] = _shownMap is null ? "init" : "reload",
            ["metadata"] = new JsonObject
            {
                ["bounds"] = new JsonObject { ["west"] = b.West, ["south"] = b.South, ["east"] = b.East, ["north"] = b.North },
                ["minZoom"] = status.Coverage is null ? 0 : status.MinZoom,
                ["maxZoom"] = status.Coverage is null ? 14 : status.MaxZoom,
                ["regions"] = new JsonArray(status.Regions.Where(r => r.Map is not null).Select(r => (JsonNode)r.Id).ToArray()),
                ["overview"] = AppPaths.HasOverview,
            },
            ["theme"] = App.Current.Themes.IsDark ? "dark" : "light",
            ["colors"] = Colors(),
            ["animate"] = UiSettings.AnimationsEnabled,
            ["units"] = _settings.Kilometres ? "metric" : "imperial",
            ["terrain"] = status.TerrainCoverage is { } t
                ? new JsonObject { ["bounds"] = new JsonObject { ["west"] = t.West, ["south"] = t.South, ["east"] = t.East, ["north"] = t.North } }
                : null,
            ["outdoor"] = _settings.OutdoorPlaces,
            ["overlays"] = status.OverlayCoverage is { } o
                ? new JsonObject { ["bounds"] = new JsonObject { ["west"] = o.West, ["south"] = o.South, ["east"] = o.East, ["north"] = o.North } }
                : null,
            ["flood"] = _settings.FloodZones,
            ["lands"] = _settings.PublicLands,
            ["shading"] = _settings.TerrainShading,
            ["contours"] = _settings.ContourLines,
        };
        if (_settings.ViewLatitude is double lat && _settings.ViewLongitude is double lon && _settings.ViewZoom is double zoom
            && lat >= b.South && lat <= b.North && lon >= b.West && lon <= b.East)
        {
            message["view"] = new JsonObject { ["latitude"] = lat, ["longitude"] = lon, ["zoom"] = zoom };
            (_viewLatitude, _viewLongitude, _viewZoom) = (lat, lon, zoom);
        }
        else
        {
            (_viewLatitude, _viewLongitude, _viewZoom) = ((b.South + b.North) / 2, (b.West + b.East) / 2, 7);
        }
        Post(message);
        _shownMap = status.ContentVersion;
        MapView.Visibility = Visibility.Visible;
        MapMessage.Visibility = Visibility.Collapsed;
        PostPlaces();
        UpdatePlaceCommands();
    }

    private void PostPlaces()
    {
        if (_status is null)
        {
            return;
        }
        var items = new JsonArray();
        foreach (var p in _status.Places)
        {
            items.Add(new JsonObject
            {
                ["id"] = p.Id,
                ["name"] = p.Name,
                ["latitude"] = p.Latitude,
                ["longitude"] = p.Longitude,
                ["covered"] = p.Covered,
            });
        }
        Post(new JsonObject { ["kind"] = "places", ["items"] = items });
    }

    private void PostTerrain() => Post(new JsonObject
    {
        ["kind"] = "terrain",
        ["shading"] = _settings.TerrainShading,
        ["contours"] = _settings.ContourLines,
    });

    private void PostUnits() => Post(new JsonObject { ["kind"] = "units", ["units"] = _settings.Kilometres ? "metric" : "imperial" });

    private void Post(JsonObject message)
    {
        if (_pageReady && MapView.CoreWebView2 is { } web)
        {
            web.PostWebMessageAsJson(message.ToJsonString());
        }
    }

    private void RememberView()
    {
        if (_shownMap is not null)
        {
            (_settings.ViewLatitude, _settings.ViewLongitude, _settings.ViewZoom) = (_viewLatitude, _viewLongitude, _viewZoom);
        }
    }

    /// <summary>Jupiter.UI colors for the map's own controls and markers, in the current theme.</summary>
    private static JsonObject Colors()
    {
        var theme = App.Current.Themes.Resolved;
        string Token(string name)
        {
            try
            {
                var color = (Color)Jupiter.UI.JupiterTheme.Find($"Jupiter{name}Color", theme);
                return Hex(color);
            }
            catch (Exception ex) when (ex is InvalidCastException or KeyNotFoundException)
            {
                return App.Current.Themes.IsDark ? "#E8E4DC" : "#1C1B19";
            }
        }
        var accent = App.Current.Accent.Current;
        double luminance = (accent.R * 0.299 + accent.G * 0.587 + accent.B * 0.114) / 255;
        return new JsonObject
        {
            ["window"] = Token("Window"),
            ["surface"] = Token("Surface"),
            ["separator"] = Token("Separator"),
            ["text"] = Token("PrimaryText"),
            ["secondary"] = Token("SecondaryText"),
            ["accent"] = Hex(accent),
            ["accent-text"] = luminance > 0.55 ? "#000000" : "#FFFFFF",
        };
    }

    private static string Hex(Color c) => $"#{c.R:X2}{c.G:X2}{c.B:X2}";

    private void PushTheme()
    {
        Post(new JsonObject
        {
            ["kind"] = "theme",
            ["theme"] = App.Current.Themes.IsDark ? "dark" : "light",
            ["colors"] = Colors(),
            ["animate"] = UiSettings.AnimationsEnabled,
        });
    }

    private void ZoomIn_Click(object sender, RoutedEventArgs e) => Post(new JsonObject { ["kind"] = "zoom", ["delta"] = 1 });

    private void ZoomOut_Click(object sender, RoutedEventArgs e) => Post(new JsonObject { ["kind"] = "zoom", ["delta"] = -1 });

    private void Fit_Click(object sender, RoutedEventArgs e) => Post(new JsonObject { ["kind"] = "fit" });

    private void ShowOnMap(PaneItem item)
    {
        Post(new JsonObject
        {
            ["kind"] = "show",
            ["latitude"] = item.Latitude,
            ["longitude"] = item.Longitude,
            ["zoom"] = item.Zoom,
            ["label"] = item.Title,
        });
    }
}
