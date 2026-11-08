using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

namespace OfflineMaps.App;

/// <summary>Settings - Offline Maps: one per app, non-modal; changes apply immediately.</summary>
public sealed partial class SettingsWindow : Window
{
    private readonly MainWindow _main;
    private readonly AppSettings _settings = App.Current.Settings;
    private bool _loading = true;

    public SettingsWindow(MainWindow main)
    {
        _main = main;
        InitializeComponent();
        App.Current.Themes.Attach(this);
        App.Current.Accent.Attach(Root);
        ThemeControl.Service = App.Current.Themes;
        AccentControl.Service = App.Current.Accent;
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico"));
        DistanceChoice.SelectedIndex = _settings.Kilometres ? 1 : 0;
        FolderBox.Text = AppPaths.StoreDir;
        _loading = false;
        Root.Loaded += (_, _) =>
        {
            double scale = Root.XamlRoot.RasterizationScale;
            var area = DisplayArea.GetFromWindowId(main.AppWindow.Id, DisplayAreaFallback.Primary).WorkArea;
            int width = Math.Min((int)(720 * scale), area.Width);
            int height = Math.Min((int)(720 * scale), area.Height);
            AppWindow.MoveAndResize(new RectInt32(area.X + (area.Width - width) / 2, area.Y + (area.Height - height) / 2, width, height));
            if (AppWindow.Presenter is OverlappedPresenter presenter)
            {
                presenter.PreferredMinimumWidth = Math.Min((int)(560 * scale), area.Width);
                presenter.PreferredMinimumHeight = Math.Min((int)(480 * scale), area.Height);
            }
        };
        Closed += (_, _) =>
        {
            App.Current.Themes.Detach(this);
            App.Current.Accent.Detach(Root);
        };
    }

    private void DistanceChoice_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_loading)
        {
            return;
        }
        _settings.Distances = DistanceChoice.SelectedIndex == 1 ? "kilometres" : "miles";
        _settings.Save();
        _main.OnUnitsChanged();
    }

    private void OpenFolder_Click(object sender, RoutedEventArgs e)
    {
        try
        {
            Directory.CreateDirectory(AppPaths.StoreDir);
            System.Diagnostics.Process.Start(new System.Diagnostics.ProcessStartInfo("explorer.exe", $"\"{AppPaths.StoreDir}\"") { UseShellExecute = true });
        }
        catch (Exception ex)
        {
            AppLog.Write($"The map folder could not be opened: {ex.Message}");
        }
    }
}
