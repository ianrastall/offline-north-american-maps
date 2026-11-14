using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Windows.Storage.Pickers;
using Windows.Graphics;

namespace OfflineMaps.App;

/// <summary>Settings - Offline North American Maps: one per app, non-modal; changes apply immediately.</summary>
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
        ShowFolder();
        _main.MapFolderChanged += ShowFolder;
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
            _main.MapFolderChanged -= ShowFolder;
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

    private void ShowFolder()
    {
        FolderBox.Text = AppPaths.StoreDir;
        if (AppPaths.StoreOverride is not null)
        {
            ChangeFolderButton.IsEnabled = false;
            FolderCaption.Text = "Maps, stations and addresses, and saved places. Set with --store when Offline North American Maps was started, so it cannot be changed here.";
        }
    }

    /// <summary>Chooses a new map folder, then moves the maps there or uses the folder as it is.</summary>
    private async void ChangeFolder_Click(object sender, RoutedEventArgs e)
    {
        if (_main.IsBusy)
        {
            await ShowDialogAsync(new ContentDialog
            {
                Title = "The map folder cannot be changed now",
                Content = Text("A download, update or move is running. Change the folder when it has finished."),
                CloseButtonText = "Close",
            });
            return;
        }
        string? chosen;
        try
        {
            var picker = new FolderPicker(AppWindow.Id) { SuggestedStartLocation = PickerLocationId.ComputerFolder };
            chosen = (await picker.PickSingleFolderAsync())?.Path;
        }
        catch (Exception ex)
        {
            AppLog.Write($"The folder picker failed: {ex.Message}");
            return;
        }
        if (string.IsNullOrEmpty(chosen) || AppPaths.SameFolder(chosen, AppPaths.StoreDir) || _main.IsBusy)
        {
            return;
        }
        string folder = Path.GetFullPath(chosen);
        bool canMove = _main.HasCore;
        var dialog = new ContentDialog
        {
            Title = "Change the map folder?",
            Content = Text(canMove
                ? $"Move maps: the maps, stations and addresses, and saved places in {AppPaths.StoreDir} move to {folder}. On another drive they are copied first, which can take several minutes; the maps are not shown until the move finishes. Other files in either folder are left alone.\n\nUse without moving: only maps already in {folder} are shown. The current ones stay where they are."
                : $"Use {folder} as the map folder? Only maps already there are shown."),
            PrimaryButtonText = canMove ? "Move maps" : "Use folder",
            SecondaryButtonText = canMove ? "Use without moving" : "",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
        };
        var answer = await ShowDialogAsync(dialog);
        if (answer == ContentDialogResult.None)
        {
            return;
        }
        ChangeFolderButton.IsEnabled = false;
        try
        {
            await _main.ChangeMapFolderAsync(folder, move: canMove && answer == ContentDialogResult.Primary);
        }
        finally
        {
            ChangeFolderButton.IsEnabled = AppPaths.StoreOverride is null;
            ShowFolder();
        }
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

    private static TextBlock Text(string text) => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        IsTextSelectionEnabled = true,
        Style = (Style)Application.Current.Resources["JupiterBodyTextStyle"],
    };

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
