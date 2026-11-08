using System.Text.Json;
using System.Text.Json.Serialization;

namespace OfflineMaps.App;

/// <summary>
/// What the shell remembers: appearance, units, the window and the last map view. Maps and
/// saved places belong to the core. Kept in settings.json in the settings folder.
/// </summary>
public sealed class AppSettings : Jupiter.UI.IAccentPreferences, Jupiter.UI.IThemePreferences
{
    public const int CurrentVersion = 1;

    private string _file = "";

    public int Version { get; set; }

    public string? Theme { get; set; }

    public bool UseCustomAccent { get; set; }

    public string? CustomAccent { get; set; }

    /// <summary>"miles" (the default) or "kilometres".</summary>
    public string Distances { get; set; } = "miles";

    public bool SearchPaneVisible { get; set; } = true;

    public double SearchPaneWidth { get; set; } = 320;

    /// <summary>The offer to download stations and addresses was closed.</summary>
    public bool ExtrasOfferClosed { get; set; }

    /// <summary>Hillshading from installed terrain.</summary>
    public bool TerrainShading { get; set; } = true;

    /// <summary>Contour lines from installed terrain, in feet or metres with the distance unit.</summary>
    public bool ContourLines { get; set; } = true;

    public double? ViewLatitude { get; set; }

    public double? ViewLongitude { get; set; }

    public double? ViewZoom { get; set; }

    public int WindowX { get; set; } = int.MinValue;

    public int WindowY { get; set; } = int.MinValue;

    public int WindowWidth { get; set; }

    public int WindowHeight { get; set; }

    public bool WindowMaximized { get; set; }

    [JsonIgnore]
    public bool Kilometres => Distances == "kilometres";

    public static AppSettings Load(string directory)
    {
        string file = Path.Combine(directory, "settings.json");
        AppSettings settings = new();
        try
        {
            if (File.Exists(file))
            {
                settings = JsonSerializer.Deserialize(File.ReadAllText(file), SettingsJson.Default.AppSettings) ?? new();
            }
        }
        catch (Exception e) when (e is IOException or JsonException or UnauthorizedAccessException)
        {
            AppLog.Write($"Settings could not be read and were reset: {e.Message}");
            settings = new();
        }
        settings._file = file;
        settings.Version = CurrentVersion;
        settings.SearchPaneWidth = Math.Clamp(settings.SearchPaneWidth, 240, 900);
        if (settings.Distances is not ("miles" or "kilometres"))
        {
            settings.Distances = "miles";
        }
        return settings;
    }

    /// <summary>Writes through a temporary file, so a failed write leaves the old settings whole.</summary>
    public void Save()
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_file)!);
            string temporary = _file + ".tmp";
            File.WriteAllText(temporary, JsonSerializer.Serialize(this, SettingsJson.Default.AppSettings));
            File.Move(temporary, _file, overwrite: true);
        }
        catch (Exception e) when (e is IOException or UnauthorizedAccessException)
        {
            AppLog.Write($"Settings could not be saved: {e.Message}");
        }
    }
}

[JsonSourceGenerationOptions(WriteIndented = true)]
[JsonSerializable(typeof(AppSettings))]
internal sealed partial class SettingsJson : JsonSerializerContext;
