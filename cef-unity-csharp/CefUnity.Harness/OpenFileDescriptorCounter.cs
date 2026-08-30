using System.IO;
using System.Runtime.InteropServices;

namespace CefUnity.Harness
{
    /// <summary>
    ///     開いている file descriptor の数を数える (Linux 専用)。
    ///
    ///     dmabuf の fd 転送は漏れても即座には壊れず、時間をかけて枯渇するだけなので
    ///     気づきにくい。lifecycle が macOS で mach_ports を数えているのと同じ位置で
    ///     これを出し、横ばいかどうかで機械的に判定できるようにする。
    /// </summary>
    public static class OpenFileDescriptorCounter
    {
        /// <summary>Linux で開いている file descriptor の数。数えられない環境では -1。</summary>
        public static int Count()
        {
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.Linux))
            {
                return -1;
            }
            try
            {
                return Directory.GetFileSystemEntries("/proc/self/fd").Length;
            }
            catch (DirectoryNotFoundException)
            {
                return -1;
            }
            catch (IOException)
            {
                return -1;
            }
        }
    }
}
