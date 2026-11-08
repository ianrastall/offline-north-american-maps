using System.Globalization;

namespace OfflineMaps.App;

/// <summary>A small append-only log (offline-maps.log in the settings folder) for problems worth keeping.</summary>
internal static class AppLog
{
    private const long MaxBytes = 1 << 20;
    private static readonly Lock Gate = new();
    private static string? s_file;

    public static void Open(string directory)
    {
        s_file = Path.Combine(directory, "offline-maps.log");
        try
        {
            if (File.Exists(s_file) && new FileInfo(s_file).Length > MaxBytes)
            {
                File.Move(s_file, s_file + ".old", overwrite: true);
            }
        }
        catch (IOException)
        {
        }
    }

    public static void Write(string message)
    {
        if (s_file is null)
        {
            return;
        }
        lock (Gate)
        {
            try
            {
                string time = DateTime.Now.ToString("yyyy-MM-dd HH:mm:ss", CultureInfo.InvariantCulture);
                File.AppendAllText(s_file, $"{time} {message}{Environment.NewLine}");
            }
            catch (Exception e) when (e is IOException or UnauthorizedAccessException)
            {
            }
        }
    }
}
