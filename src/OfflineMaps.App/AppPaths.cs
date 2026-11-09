namespace OfflineMaps.App;

/// <summary>
/// Where Offline US Maps keeps things. Everything is under %LOCALAPPDATA%\Jupiter\OfflineMaps, outside
/// the install folder, so maps, saved places and settings survive upgrades and uninstalling.
/// </summary>
internal static class AppPaths
{
    /// <summary>Settings, the log and the WebView2 profile.</summary>
    public static string SettingsDir { get; } = Option("--settings") ??
        Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Jupiter", "OfflineMaps");

    /// <summary>Map packages, stations and addresses, and saved places (owned by the Rust core).</summary>
    public static string StoreDir { get; } = Option("--store") ?? Path.Combine(SettingsDir, "Maps");

    public static string WebViewDataDir { get; } = Path.Combine(SettingsDir, "WebView2");

    public static string WebDir { get; } = Path.Combine(AppContext.BaseDirectory, "Web");

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
