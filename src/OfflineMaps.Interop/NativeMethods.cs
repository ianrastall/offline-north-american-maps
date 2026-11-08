using System.Runtime.CompilerServices;
using System.Runtime.InteropServices;

// The app directory only (and System32 for the OS): a stray copy elsewhere is never loaded.
[assembly: DefaultDllImportSearchPaths(DllImportSearchPath.AssemblyDirectory | DllImportSearchPath.System32)]
[assembly: DisableRuntimeMarshalling]

namespace OfflineMaps.Interop;

/// <summary>Declarations for the Rust C ABI in crates/offline-maps-ffi (ABI version 1).</summary>
internal static unsafe partial class NativeMethods
{
    private const string Library = "offline_maps_ffi";

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial uint om_abi_version();

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_create(byte* config, nuint configLength, out CoreSafeHandle core, out ResultSafeHandle error);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_call(CoreSafeHandle core, byte* request, nuint requestLength, out ResultSafeHandle result);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_tile(CoreSafeHandle core, uint z, uint x, uint y, out ResultSafeHandle result);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_progress(CoreSafeHandle core, out ResultSafeHandle result);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_cancel(CoreSafeHandle core);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial NativeStatus om_result_bytes(ResultSafeHandle result, out byte* bytes, out nuint length);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial void om_result_free(nint result);

    [LibraryImport(Library)]
    [UnmanagedCallConv(CallConvs = [typeof(CallConvCdecl)])]
    public static partial void om_destroy(nint core);
}

/// <summary>Transport-level outcomes of an ABI call. Domain failures arrive as Response.Error.</summary>
public enum NativeStatus : uint
{
    Ok = 0,
    InvalidArgument = 1,
    MalformedRequest = 2,
    Panic = 3,
    Unusable = 4,
    CreateFailed = 5,
}

internal sealed class CoreSafeHandle() : SafeHandle(nint.Zero, ownsHandle: true)
{
    public override bool IsInvalid => handle == nint.Zero;

    // The handle's reference count keeps this from running while any call, including a
    // running update, is still inside the native core.
    protected override bool ReleaseHandle()
    {
        NativeMethods.om_destroy(handle);
        return true;
    }
}

internal sealed class ResultSafeHandle() : SafeHandle(nint.Zero, ownsHandle: true)
{
    public override bool IsInvalid => handle == nint.Zero;

    protected override bool ReleaseHandle()
    {
        NativeMethods.om_result_free(handle);
        return true;
    }

    /// <summary>Copies the result bytes into managed memory before the handle is freed.</summary>
    public unsafe byte[] CopyBytes()
    {
        var status = NativeMethods.om_result_bytes(this, out var ptr, out var length);
        if (status != NativeStatus.Ok)
        {
            throw new MapCoreException(Messages.ErrorKind.Internal, $"Reading a result from the map core failed ({status}).");
        }
        if (length > 64 * 1024 * 1024)
        {
            throw new MapCoreException(Messages.ErrorKind.Internal, "A result from the map core was too large.");
        }
        return length == 0 ? [] : new ReadOnlySpan<byte>(ptr, checked((int)length)).ToArray();
    }
}
