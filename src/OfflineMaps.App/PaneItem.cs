using System.Globalization;
using Microsoft.UI.Xaml;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.App;

/// <summary>A row in the search pane: a search result or a saved place.</summary>
public sealed class PaneItem
{
    public required string Title { get; init; }

    public required string Caption { get; init; }

    public double Latitude { get; init; }

    public double Longitude { get; init; }

    /// <summary>The zoom to show the item at.</summary>
    public double Zoom { get; init; } = 16;

    /// <summary>Set for saved places.</summary>
    public ulong? PlaceId { get; init; }

    public Place? Place { get; init; }

    public Visibility HasCaption => Caption.Length > 0 ? Visibility.Visible : Visibility.Collapsed;

    public string AutomationName => Caption.Length > 0 ? $"{Title}. {Caption}" : Title;

    public static PaneItem FromResult(SearchResult r, bool kilometres)
    {
        var parts = new List<string> { Describe(r) };
        if (r.Near.Length > 0)
        {
            parts.Add($"near {r.Near}");
        }
        if (r.HasDistance)
        {
            parts.Add(Format.Distance(r.DistanceKm, kilometres));
        }
        if (r.Detail.Length > 0)
        {
            parts.Add(r.Detail);
        }
        string title = r.Reference.Length > 0 && !r.Name.Contains(r.Reference.Split('\n')[0], StringComparison.OrdinalIgnoreCase)
            ? $"{r.Name} ({r.Reference.Replace('\n', '/')})"
            : r.Name;
        double zoom = r.Kind switch
        {
            "place" => r.Category switch
            {
                "city" or "capital" or "state_capital" => 11,
                "town" => 12,
                "village" => 13,
                _ => 14,
            },
            "address" => 18,
            "water" => 13,
            _ => 16,
        };
        return new PaneItem { Title = title, Caption = string.Join(" · ", parts), Latitude = r.Latitude, Longitude = r.Longitude, Zoom = zoom };
    }

    public static PaneItem FromPlace(Place p) => new()
    {
        Title = p.Name,
        Caption = string.Join(" · ", new[] { p.Note, p.Covered ? "" : "Outside the installed map" }.Where(s => s.Length > 0)),
        Latitude = p.Latitude,
        Longitude = p.Longitude,
        PlaceId = p.Id,
        Place = p,
    };

    /// <summary>"Town", "Street", "Fuel station", "Hospital" and so on.</summary>
    private static string Describe(SearchResult r) => r.Kind switch
    {
        "street" => "Street",
        "address" => "Address",
        "place" => r.Category switch
        {
            "state_capital" => "State capital",
            "capital" => "Capital",
            _ => Format.Sentence(r.Category),
        },
        _ => r.Category switch
        {
            "fuel" => "Fuel station",
            "charging_station" => "Charging station",
            "" => Format.Sentence(r.Kind),
            _ => Format.Sentence(r.Category),
        },
    };
}

internal static class Format
{
    public static string Sentence(string text)
    {
        text = text.Replace('_', ' ');
        return text.Length == 0 ? text : char.ToUpperInvariant(text[0]) + text[1..];
    }

    public static string Distance(double km, bool kilometres)
    {
        double value = kilometres ? km : km * 0.621371;
        string unit = kilometres ? "km" : "mi";
        return value < 10
            ? string.Create(CultureInfo.CurrentCulture, $"{value:0.0} {unit}")
            : string.Create(CultureInfo.CurrentCulture, $"{value:0} {unit}");
    }

    public static string Coordinates(double latitude, double longitude) =>
        string.Create(CultureInfo.InvariantCulture, $"{latitude:0.00000}, {longitude:0.00000}");

    public static string Megabytes(ulong bytes) =>
        string.Create(CultureInfo.CurrentCulture, $"{bytes / 1_000_000.0:0} MB");

    /// <summary>"2026-11-08" as a long date in the person's format.</summary>
    public static string Date(string iso) =>
        DateTime.TryParseExact(iso.Length >= 10 ? iso[..10] : iso, "yyyy-MM-dd", CultureInfo.InvariantCulture, DateTimeStyles.None, out var date)
            ? date.ToString("d MMMM yyyy", CultureInfo.CurrentCulture)
            : iso;
}
