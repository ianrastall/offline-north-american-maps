using Google.Protobuf;
using OfflineMaps.Interop.Messages;

namespace OfflineMaps.Interop;

/// <summary>An expected failure reported by the core, with a message for the person using the app.</summary>
public sealed class MapCoreException(ErrorKind kind, string message) : Exception(message)
{
    public ErrorKind Kind { get; } = kind;
}

/// <summary>
/// One open map store. Thread-safe: calls may overlap (tile reads continue while an update
/// downloads). Calls block; the app runs them off the UI thread.
/// </summary>
public sealed class MapCore : IDisposable
{
    /// <summary>The ABI version this assembly was written against (crates/offline-maps-ffi ABI_VERSION).</summary>
    public const uint ExpectedAbiVersion = 1;

    private readonly CoreSafeHandle _handle;
    private long _nextRequestId;

    private MapCore(CoreSafeHandle handle) => _handle = handle;

    /// <summary>Fails clearly when offline_maps_ffi.dll is missing or was built for a different ABI.</summary>
    public static void EnsureCompatible()
    {
        uint actual;
        try
        {
            actual = NativeMethods.om_abi_version();
        }
        catch (Exception e) when (e is DllNotFoundException or BadImageFormatException or EntryPointNotFoundException)
        {
            throw new MapCoreException(ErrorKind.Internal, $"The map core (offline_maps_ffi.dll) could not be loaded: {e.Message}");
        }
        if (actual != ExpectedAbiVersion)
        {
            throw new MapCoreException(ErrorKind.Internal,
                $"The map core has ABI version {actual}, but this app needs {ExpectedAbiVersion}. Reinstall Offline Maps.");
        }
    }

    /// <summary>Opens the store in <paramref name="storeDirectory"/> (created if missing).</summary>
    public static unsafe MapCore Open(string storeDirectory, string? packageUrl = null, string? extrasUrl = null, string? overlaysUrl = null)
    {
        EnsureCompatible();
        byte[] config = new Config
        {
            StoreDirectory = storeDirectory,
            PackageUrl = packageUrl ?? "",
            ExtrasUrl = extrasUrl ?? "",
            OverlaysUrl = overlaysUrl ?? "",
        }.ToByteArray();
        NativeStatus status;
        CoreSafeHandle handle;
        ResultSafeHandle error;
        fixed (byte* p = config)
        {
            status = NativeMethods.om_create(p, (nuint)config.Length, out handle, out error);
        }
        using (error)
        {
            if (status == NativeStatus.Ok)
            {
                return new MapCore(handle);
            }
            handle.Dispose();
            if (status == NativeStatus.CreateFailed && !error.IsInvalid)
            {
                var response = Response.Parser.ParseFrom(error.CopyBytes());
                throw new MapCoreException(response.Error?.Kind ?? ErrorKind.Internal, response.Error?.Message ?? "The map core could not start.");
            }
            throw new MapCoreException(ErrorKind.Internal, $"The map core could not start ({status}).");
        }
    }

    /// <summary>Sends a request and returns the response; a typed failure is thrown as <see cref="MapCoreException"/>.</summary>
    public unsafe Response Call(Request request)
    {
        request.RequestId = (ulong)Interlocked.Increment(ref _nextRequestId);
        byte[] bytes = request.ToByteArray();
        NativeStatus status;
        ResultSafeHandle result;
        fixed (byte* p = bytes)
        {
            status = NativeMethods.om_call(_handle, p, (nuint)bytes.Length, out result);
        }
        using (result)
        {
            Check(status);
            var response = Response.Parser.ParseFrom(result.CopyBytes());
            if (response.RequestId != request.RequestId)
            {
                throw new MapCoreException(ErrorKind.Internal, "The map core answered a different request.");
            }
            if (response.Error is { } error)
            {
                throw new MapCoreException(error.Kind, error.Message);
            }
            return response;
        }
    }

    public Status GetStatus() => Call(new Request { GetStatus = new GetStatus() }).Status;

    public IReadOnlyList<SearchResult> Search(string query, double? latitude, double? longitude, uint limit = 20)
    {
        var search = new Search { Query = query, Limit = limit };
        if (latitude is double lat && longitude is double lon)
        {
            search.Near = new Position { Latitude = lat, Longitude = lon };
        }
        return Call(new Request { Search = search }).Search.Results;
    }

    public Place AddPlace(string name, string note, double latitude, double longitude) =>
        Call(new Request
        {
            AddPlace = new AddPlace { Name = name, Note = note, Position = new Position { Latitude = latitude, Longitude = longitude } },
        }).Place;

    public Place UpdatePlace(ulong id, string name, string note) =>
        Call(new Request { UpdatePlace = new UpdatePlace { Id = id, Name = name, Note = note } }).Place;

    public void RemovePlace(ulong id) => Call(new Request { RemovePlace = new RemovePlace { Id = id } });

    public void ResetPlaces() => Call(new Request { ResetPlaces = new ResetPlaces() });

    /// <summary>Downloads and activates a region's newer map or extras. Blocks for the whole download.</summary>
    public UpdateResult Update(UpdateTarget target, string regionId) =>
        Call(new Request { Update = new Update { Target = target, RegionId = regionId } }).Update;

    /// <summary>Deletes a region's map, search index and extras. Saved places are kept.</summary>
    public void RemoveRegion(string regionId) => Call(new Request { RemoveRegion = new RemoveRegion { RegionId = regionId } });

    /// <summary>A terrain tile (Terrarium-encoded WebP), or an empty array where no installed terrain has one.</summary>
    public byte[] TerrainTile(int z, int x, int y)
    {
        if (z < 0 || x < 0 || y < 0)
        {
            return [];
        }
        return Call(new Request { GetTerrainTile = new GetTerrainTile { Z = (uint)z, X = (uint)x, Y = (uint)y } }).TerrainTile.Data.ToByteArray();
    }

    /// <summary>A decompressed overlay vector tile (flood zones, public lands), or an empty array.</summary>
    public byte[] OverlayTile(int z, int x, int y)
    {
        if (z < 0 || x < 0 || y < 0)
        {
            return [];
        }
        return Call(new Request { GetOverlayTile = new GetOverlayTile { Z = (uint)z, X = (uint)x, Y = (uint)y } }).OverlayTile.Data.ToByteArray();
    }

    /// <summary>The regions that can be downloaded, by name.</summary>
    public IReadOnlyList<CatalogRegion> Catalog() => Call(new Request { GetCatalog = new GetCatalog() }).Catalog.Regions;

    public void PrepareSearch() => Call(new Request { PrepareSearch = new PrepareSearch() });

    public string StationsGeoJson() => Call(new Request { GetStations = new GetStations() }).Stations.Geojson;

    /// <summary>The decompressed vector tile, or an empty array where there is none.</summary>
    public byte[] Tile(int z, int x, int y)
    {
        if (z < 0 || x < 0 || y < 0)
        {
            return [];
        }
        var status = NativeMethods.om_tile(_handle, (uint)z, (uint)x, (uint)y, out var result);
        using (result)
        {
            if (status == NativeStatus.InvalidArgument)
            {
                return [];
            }
            Check(status);
            return result.CopyBytes();
        }
    }

    public ProgressEvent Progress()
    {
        var status = NativeMethods.om_progress(_handle, out var result);
        using (result)
        {
            Check(status);
            return ProgressEvent.Parser.ParseFrom(result.CopyBytes());
        }
    }

    /// <summary>Asks a running update to stop; returns at once.</summary>
    public void Cancel() => Check(NativeMethods.om_cancel(_handle));

    private static void Check(NativeStatus status)
    {
        switch (status)
        {
            case NativeStatus.Ok:
                return;
            case NativeStatus.Panic:
            case NativeStatus.Unusable:
                throw new MapCoreException(ErrorKind.Internal, "The map core stopped after an internal error. Restart Offline Maps.");
            default:
                throw new MapCoreException(ErrorKind.Internal, $"The map core rejected a request ({status}).");
        }
    }

    public void Dispose() => _handle.Dispose();
}
