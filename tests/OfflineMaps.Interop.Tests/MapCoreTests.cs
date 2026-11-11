using Google.Protobuf;
using OfflineMaps.Interop;
using OfflineMaps.Interop.Messages;
using Xunit;

namespace OfflineMaps.Interop.Tests;

public sealed class TempStore : IDisposable
{
    public string Path { get; } = System.IO.Path.Combine(System.IO.Path.GetTempPath(), $"offline-maps-interop-{Guid.NewGuid():N}");

    public void Dispose()
    {
        try
        {
            Directory.Delete(Path, recursive: true);
        }
        catch (DirectoryNotFoundException)
        {
        }
    }
}

public class MapCoreTests
{
    [Fact]
    public void AbiVersionMatches()
    {
        MapCore.EnsureCompatible();
        Assert.Equal(MapCore.ExpectedAbiVersion, NativeMethods.om_abi_version());
    }

    [Fact]
    public void EmptyStoreHasNoMapAndRefusesSearch()
    {
        using var store = new TempStore();
        using var core = MapCore.Open(store.Path);
        var status = core.GetStatus();
        Assert.Empty(status.Regions);
        Assert.Null(status.Coverage);
        Assert.Equal(System.IO.Path.GetFullPath(store.Path), System.IO.Path.GetFullPath(status.StoreDirectory));

        var error = Assert.Throws<MapCoreException>(() => core.Search("springfield", 39.8, -89.65));
        Assert.Equal(ErrorKind.Unavailable, error.Kind);
        Assert.Empty(core.Tile(14, 4203, 6089));
        Assert.Empty(core.Tile(99, 0, 0));
        Assert.Empty(core.TerrainTile(12, 1046, 1520));
        Assert.Equal(ErrorKind.NotFound, Assert.Throws<MapCoreException>(() => core.Update(UpdateTarget.Terrain, "us-il")).Kind);
        var catalog = core.Catalog();
        Assert.Equal(100, catalog.Count);
        Assert.Contains(catalog, r => r.Id == "mx" && r.Name == "All of Mexico");
        Assert.Contains(catalog, r => r.Id == "us-il" && r.Name == "Illinois");
        core.RemoveRegion("us-in");
        Assert.Equal(ErrorKind.InvalidArgument, Assert.Throws<MapCoreException>(() => core.RemoveRegion("us-zz")).Kind);
        Assert.Contains("FeatureCollection", core.StationsGeoJson());
        Assert.False(core.Progress().Active);
        core.Cancel();
    }

    [Fact]
    public void SavedPlacesRoundTripWithUnicode()
    {
        using var store = new TempStore();
        using (var core = MapCore.Open(store.Path))
        {
            var place = core.AddPlace("Café Shelter — east door", "Bring ID ✓", 40.15, -88.96);
            Assert.Equal(1ul, place.Id);
            core.UpdatePlace(place.Id, "Shelter", "Bring ID ✓");
            var invalid = Assert.Throws<MapCoreException>(() => core.AddPlace("", "", 40, -89));
            Assert.Equal(ErrorKind.InvalidArgument, invalid.Kind);
            var outside = Assert.Throws<MapCoreException>(() => core.AddPlace("x", "", 91, 0));
            Assert.Equal(ErrorKind.InvalidArgument, outside.Kind);
        }
        using (var core = MapCore.Open(store.Path))
        {
            var places = core.GetStatus().Places;
            Assert.Single(places);
            Assert.Equal("Shelter", places[0].Name);
            Assert.Equal("Bring ID ✓", places[0].Note);
            core.RemovePlace(1);
            Assert.Equal(ErrorKind.NotFound, Assert.Throws<MapCoreException>(() => core.RemovePlace(1)).Kind);
        }
    }

    [Fact]
    public void RelativeStoreIsATypedCreationError()
    {
        var error = Assert.Throws<MapCoreException>(() => MapCore.Open("relative-folder"));
        Assert.Equal(ErrorKind.InvalidArgument, error.Kind);
    }

    [Fact]
    public void UnreachableServerIsANetworkError()
    {
        using var store = new TempStore();
        using var core = MapCore.Open(store.Path, packageUrl: "http://127.0.0.1:9/package.mbtiles");
        var error = Assert.Throws<MapCoreException>(() => core.Update(UpdateTarget.Map, "us-il"));
        Assert.Equal(ErrorKind.Network, error.Kind);
        Assert.False(core.GetStatus().Updating);
    }

    [Fact]
    public unsafe void MalformedAndEmptyRequestsAreRejected()
    {
        using var store = new TempStore();
        using var core = MapCore.Open(store.Path);
        var handle = typeof(MapCore).GetField("_handle", System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance)!
            .GetValue(core) as CoreSafeHandle;
        byte[] garbage = [0xff, 0xff, 0xff, 0xff];
        fixed (byte* p = garbage)
        {
            Assert.Equal(NativeStatus.MalformedRequest, NativeMethods.om_call(handle!, p, (nuint)garbage.Length, out var result));
            Assert.True(result.IsInvalid);
        }
        var empty = Assert.Throws<MapCoreException>(() => core.Call(new Request()));
        Assert.Equal(ErrorKind.InvalidArgument, empty.Kind);
    }

    [Fact]
    public void MessagesRoundTripWithUnknownFields()
    {
        // A newer core may add fields; this shell must still read what it knows.
        var status = new Status { ContentVersion = "us-il:1:", SearchReady = true };
        // Field 99, length-delimited: tag (99 << 3) | 2 = 794, as the varint 0x9A 0x06.
        byte[] bytes = [.. status.ToByteArray(), 0x9A, 0x06, 3, (byte)'n', (byte)'e', (byte)'w'];
        var parsed = Status.Parser.ParseFrom(bytes);
        Assert.Equal("us-il:1:", parsed.ContentVersion);
        Assert.True(parsed.SearchReady);
        Assert.Equal(bytes.Length, parsed.ToByteArray().Length);
    }

    [Fact]
    public void ConcurrentCallsDoNotInterfere()
    {
        using var store = new TempStore();
        using var core = MapCore.Open(store.Path);
        Parallel.For(0, 64, i =>
        {
            core.AddPlace($"Place {i}", "", 40 + i / 100.0, -89);
            _ = core.Tile(10, 262, 389);
            _ = core.GetStatus();
        });
        Assert.Equal(64, core.GetStatus().Places.Count);
    }

    /// <summary>Uses a real store when OFFLINE_MAPS_TEST_STORE names one.</summary>
    [Fact]
    public void RealStoreWhenAvailable()
    {
        string? path = Environment.GetEnvironmentVariable("OFFLINE_MAPS_TEST_STORE");
        if (path is null)
        {
            return;
        }
        using var core = MapCore.Open(path);
        var status = core.GetStatus();
        var illinois = Assert.Single(status.Regions, r => r.Id == "us-il");
        Assert.NotNull(illinois.Map);
        Assert.True(status.SearchReady, status.MapProblem);
        Assert.True(core.Tile(14, 4203, 6089).Length > 1000);
        var results = core.Search("905 e main st carbondale", 39.8, -89.65);
        if (illinois.Extras is not null)
        {
            Assert.Equal("905 East Main Street", results[0].Name);
            Assert.Equal("address", results[0].Kind);
        }
        Assert.Equal("Springfield", core.Search("springfield", null, null, 1)[0].Name);
    }
}
