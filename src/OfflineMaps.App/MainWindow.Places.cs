using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.App;

/// <summary>Search and saved places in the pane, and the place dialogs.</summary>
public sealed partial class MainWindow
{
    private int _searchGeneration;

    private void SearchBox_TextChanged(AutoSuggestBox sender, AutoSuggestBoxTextChangedEventArgs args)
    {
        if (args.Reason != AutoSuggestionBoxTextChangeReason.UserInput)
        {
            return;
        }
        _searchTimer.Stop();
        if (sender.Text.Trim().Length == 0)
        {
            _searchGeneration++;
            ShowPlaces();
            return;
        }
        _searchTimer.Start();
    }

    private void SearchBox_QuerySubmitted(AutoSuggestBox sender, AutoSuggestBoxQuerySubmittedEventArgs args)
    {
        _searchTimer.Stop();
        _ = RunSearchAsync(showFirst: true);
    }

    /// <summary>Searches near the map's centre; a later search makes an earlier result stale.</summary>
    private async Task RunSearchAsync(bool showFirst = false)
    {
        string query = SearchBox.Text.Trim();
        if (_core is not { } core || query.Length == 0)
        {
            return;
        }
        int generation = ++_searchGeneration;
        bool known = _shownMap is not null;
        double latitude = _viewLatitude, longitude = _viewLongitude;
        bool kilometres = _settings.Kilometres;
        try
        {
            var results = await Task.Run(() => core.Search(query, known ? latitude : null, known ? longitude : null, 50));
            if (generation != _searchGeneration)
            {
                return;
            }
            var items = results.Select(r => PaneItem.FromResult(r, kilometres)).ToList();
            ListHeading.Text = items.Count == 1 ? "1 result" : $"{items.Count} results";
            Results.ItemsSource = items;
            ListEmpty.Text = items.Count == 0
                ? "Nothing found. Search finds towns, streets, landmarks, transit and water; with stations and addresses installed, also fuel and charging stations, shelters, drinking water, campgrounds, toilets and house numbers (\"905 e main st carbondale\"). Parks and forests are found by their roads and features, not their area names."
                : "";
            if (showFirst && items.Count > 0)
            {
                Results.SelectedIndex = 0;
            }
        }
        catch (MapCoreException ex)
        {
            if (generation == _searchGeneration)
            {
                Results.ItemsSource = null;
                ListHeading.Text = "Results";
                ListEmpty.Text = ex.Message;
            }
        }
    }

    /// <summary>The saved places, shown when the search box is empty.</summary>
    private void ShowPlaces()
    {
        if (SearchBox.Text.Trim().Length > 0)
        {
            return;
        }
        var items = (_status?.Places ?? []).Select(PaneItem.FromPlace).ToList();
        ListHeading.Text = "Saved places";
        Results.ItemsSource = items;
        ListEmpty.Text = items.Count == 0
            ? "No saved places. Use Save location to keep a place, such as home or a shelter, for later."
            : "";
        UpdatePlaceCommands();
    }

    private void Results_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (Results.SelectedItem is PaneItem item)
        {
            ShowOnMap(item);
        }
        UpdatePlaceCommands();
    }

    private void Results_ItemClick(object sender, ItemClickEventArgs e)
    {
        // A click on the already selected row shows it again (after the map was moved).
        if (e.ClickedItem is PaneItem item && ReferenceEquals(item, Results.SelectedItem))
        {
            ShowOnMap(item);
        }
    }

    private void Results_RightTapped(object sender, RightTappedRoutedEventArgs e)
    {
        if ((e.OriginalSource as FrameworkElement)?.DataContext is not PaneItem item)
        {
            return;
        }
        Results.SelectedItem = item;
        var menu = new MenuFlyout();
        if (item.PlaceId is null)
        {
            var save = new MenuFlyoutItem { Text = "Save location…" };
            save.Click += SavePlace_Click;
            menu.Items.Add(save);
        }
        else
        {
            var edit = new MenuFlyoutItem { Text = "Edit place…" };
            edit.Click += EditPlace_Click;
            var remove = new MenuFlyoutItem { Text = "Remove place…" };
            remove.Click += RemovePlace_Click;
            menu.Items.Add(edit);
            menu.Items.Add(remove);
        }
        var copy = new MenuFlyoutItem { Text = "Copy coordinates" };
        copy.Click += (_, _) => CopyText(Format.Coordinates(item.Latitude, item.Longitude));
        menu.Items.Add(copy);
        menu.ShowAt((FrameworkElement)e.OriginalSource, e.GetPosition((FrameworkElement)e.OriginalSource));
        e.Handled = true;
    }

    private void UpdatePlaceCommands()
    {
        bool place = Results.SelectedItem is PaneItem { PlaceId: not null };
        bool usable = _status is not null && _status.PlacesProblem.Length == 0;
        EditPlaceItem.IsEnabled = place && usable;
        RemovePlaceItem.IsEnabled = place && usable;
        SavePlaceItem.IsEnabled = usable && (_shownMap is not null || Results.SelectedItem is PaneItem);
        SaveButton.IsEnabled = SavePlaceItem.IsEnabled;
        CopyCoordinatesItem.IsEnabled = _shownMap is not null || Results.SelectedItem is PaneItem;
    }

    private void SelectPlace(ulong id)
    {
        if (SearchBox.Text.Length > 0)
        {
            SearchBox.Text = "";
            ShowPlaces();
        }
        if (Results.ItemsSource is List<PaneItem> items && items.FirstOrDefault(i => i.PlaceId == id) is { } item)
        {
            Results.SelectedItem = item;
            Results.ScrollIntoView(item);
        }
    }

    /// <summary>Saves the selected search result, or the map's centre when nothing is selected.</summary>
    private async void SavePlace_Click(object sender, RoutedEventArgs e)
    {
        if (Results.SelectedItem is PaneItem { PlaceId: null } result)
        {
            await AddPlaceAsync(result.Latitude, result.Longitude, result.Title);
        }
        else if (_shownMap is not null)
        {
            await AddPlaceAsync(_viewLatitude, _viewLongitude, "");
        }
    }

    private async Task AddPlaceAsync(double latitude, double longitude, string name)
    {
        if (_core is not { } core)
        {
            return;
        }
        var input = await PlaceDialogAsync("Save location", "Save place", name, "", latitude, longitude);
        if (input is not { } value)
        {
            return;
        }
        var (newName, note) = value;
        try
        {
            var place = await Task.Run(() => core.AddPlace(newName, note, latitude, longitude));
            await RefreshStatusAsync();
            ShowPlaces();
            SelectPlace(place.Id);
            StatusText.Text = $"Saved {place.Name}";
        }
        catch (MapCoreException ex)
        {
            ShowError("The place could not be saved", ex.Message);
        }
    }

    private async void EditPlace_Click(object sender, RoutedEventArgs e)
    {
        if (_core is not { } core || Results.SelectedItem is not PaneItem { Place: { } place })
        {
            return;
        }
        var input = await PlaceDialogAsync("Edit place", "Save changes", place.Name, place.Note, place.Latitude, place.Longitude);
        if (input is not { } value)
        {
            return;
        }
        var (name, note) = value;
        try
        {
            await Task.Run(() => core.UpdatePlace(place.Id, name, note));
            await RefreshStatusAsync();
            ShowPlaces();
            SelectPlace(place.Id);
        }
        catch (MapCoreException ex)
        {
            ShowError("The place could not be changed", ex.Message);
        }
    }

    private async void RemovePlace_Click(object sender, RoutedEventArgs e)
    {
        if (_core is not { } core || Results.SelectedItem is not PaneItem { Place: { } place })
        {
            return;
        }
        var answer = await ShowDialogAsync(new ContentDialog
        {
            Title = $"Remove {place.Name}?",
            Content = Text("The saved place is removed from this computer. The map is not changed."),
            PrimaryButtonText = "Remove",
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Close,
        });
        if (answer != ContentDialogResult.Primary)
        {
            return;
        }
        try
        {
            await Task.Run(() => core.RemovePlace(place.Id));
            await RefreshStatusAsync();
            ShowPlaces();
            StatusText.Text = $"Removed {place.Name}";
        }
        catch (MapCoreException ex)
        {
            ShowError("The place could not be removed", ex.Message);
        }
    }

    private async void ResetPlaces_Click(object sender, RoutedEventArgs e)
    {
        if (_core is not { } core)
        {
            return;
        }
        try
        {
            await Task.Run(core.ResetPlaces);
            ErrorBar.IsOpen = false;
            await RefreshStatusAsync();
            ShowPlaces();
            StatusText.Text = "Started a new list of saved places; the unreadable file was kept beside it";
        }
        catch (MapCoreException ex)
        {
            ShowError("A new list could not be started", ex.Message);
        }
    }

    /// <summary>Asks for a name and note; null when cancelled.</summary>
    private async Task<(string Name, string Note)?> PlaceDialogAsync(string title, string action, string name, string note, double latitude, double longitude)
    {
        var nameBox = new TextBox { Header = "Name", Text = name, MaxLength = 120 };
        var noteBox = new TextBox
        {
            Header = "Note",
            Text = note,
            MaxLength = 2000,
            AcceptsReturn = true,
            TextWrapping = TextWrapping.Wrap,
            MinHeight = 96,
        };
        var panel = new StackPanel { Spacing = 16, MinWidth = 320 };
        panel.Children.Add(nameBox);
        panel.Children.Add(noteBox);
        panel.Children.Add(Text(Format.Coordinates(latitude, longitude), "JupiterCaptionTextStyle"));
        var dialog = new ContentDialog
        {
            Title = title,
            Content = panel,
            PrimaryButtonText = action,
            CloseButtonText = "Cancel",
            DefaultButton = ContentDialogButton.Primary,
            IsPrimaryButtonEnabled = name.Trim().Length > 0,
        };
        nameBox.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = nameBox.Text.Trim().Length > 0;
        dialog.Opened += (_, _) => nameBox.Focus(FocusState.Programmatic);
        return await ShowDialogAsync(dialog) == ContentDialogResult.Primary
            ? (nameBox.Text.Trim(), noteBox.Text.Trim())
            : null;
    }
}
