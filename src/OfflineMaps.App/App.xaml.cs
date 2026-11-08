using Jupiter.UI;
using Microsoft.UI.Xaml;

namespace OfflineMaps.App;

public partial class App : Application
{
    private MainWindow? _window;

    public App()
    {
        InitializeComponent();
        UnhandledException += (_, e) => AppLog.Write($"Unhandled exception: {e.Exception}");
    }

    public static new App Current => (App)Application.Current;

    public AppSettings Settings { get; private set; } = null!;

    public ThemeService Themes { get; private set; } = null!;

    public AccentService Accent { get; private set; } = null!;

    protected override void OnLaunched(LaunchActivatedEventArgs args)
    {
        Directory.CreateDirectory(AppPaths.SettingsDir);
        AppLog.Open(AppPaths.SettingsDir);
        Settings = AppSettings.Load(AppPaths.SettingsDir);
        Themes = new ThemeService(Settings);
        Accent = new AccentService(Settings);
        _window = new MainWindow();
        _window.Activate();
        Program.ListenForShowRequests(_window);
    }
}
