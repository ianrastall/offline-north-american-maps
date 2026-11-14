namespace OfflineMaps.App;

/// <summary>
/// Where Offline North American Maps keeps things. Settings are under %LOCALAPPDATA%\Jupiter\OfflineMaps, and maps
/// and saved places are in its Maps folder unless another map folder is chosen in Settings. Both are outside the
/// install folder, so they survive upgrades and uninstalling.
/// </summary>
internal static class AppPaths
{
    /// <summary>Settings, the log and the WebView2 profile.</summary>
    public static string SettingsDir { get; } = Option("--settings") ??
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Jupiter", "OfflineMaps");

    /// <summary>The map folder when none is chosen in Settings.</summary>
    public static string DefaultStoreDir { get; } = Path.Combine(SettingsDir, "Maps");

    /// <summary>A map folder given with --store; while set, Settings cannot change it.</summary>
    public static string? StoreOverride { get; } = Option("--store");

    /// <summary>
    /// Map packages, stations and addresses, and saved places (owned by the Rust core): --store, else the
    /// folder chosen in Settings (set at startup and when it changes), else the default.
    /// </summary>
    public static string StoreDir { get; set; } = StoreOverride ?? DefaultStoreDir;

    /// <summary>Whether two folder paths name the same folder, ignoring case and a trailing separator.</summary>
    public static bool SameFolder(string a, string b) =>
        string.Equals(Path.TrimEndingDirectorySeparator(Path.GetFullPath(a)), Path.TrimEndingDirectorySeparator(Path.GetFullPath(b)), StringComparison.OrdinalIgnoreCase);

    public static string WebViewDataDir { get; } = Path.Combine(SettingsDir, "WebView2");

    public static string WebDir { get; } = Path.Combine(AppContext.BaseDirectory, "Web");

    /// <summary>The built-in whole-country overview, drawn under the installed regions.</summary>
    public static string OverviewPath { get; } = Path.Combine(AppContext.BaseDirectory, "Overview", "us-overview.mbtiles");

    public static bool HasOverview { get; } = File.Exists(OverviewPath);

    /// <summary>
    /// Download addresses for testing against a local server (OFFLINE_MAPS_PACKAGE_URL and
    /// OFFLINE_MAPS_EXTRAS_URL); unset means Geofabrik.
    /// </summary>
    public static string? PackageUrl { get; } = Environment.GetEnvironmentVariable("OFFLINE_MAPS_PACKAGE_URL");

    public static string? ExtrasUrl { get; } = Environment.GetEnvironmentVariable("OFFLINE_MAPS_EXTRAS_URL");

    /// <summary>Where overlay files are published (OFFLINE_MAPS_OVERLAYS_URL); unset means the GitHub releases.</summary>
    public static string? OverlaysUrl { get; } = Environment.GetEnvironmentVariable("OFFLINE_MAPS_OVERLAYS_URL");

    private static string? Option(string name)
    {
        string[] args = Environment.GetCommandLineArgs();
        int at = Array.IndexOf(args, name);
        return at >= 1 && at + 1 < args.Length ? Path.GetFullPath(args[at + 1]) : null;
    }
}
