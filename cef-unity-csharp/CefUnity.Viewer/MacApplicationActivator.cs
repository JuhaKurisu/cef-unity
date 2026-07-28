using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     CLI 起動 (バンドルなし) の SDL アプリは自動でアクティブにならず、
    ///     macOS は非アクティブアプリへモメンタムスクロールイベントを配送しない
    ///     (録画実測: phase 4/5/6 が皆無)。起動時に自己アクティベートして
    ///     通常アプリと同じイベント配送にする。
    /// </summary>
    public static class MacApplicationActivator
    {
        private const string LibraryObjC = "/usr/lib/libobjc.A.dylib";

        [DllImport(LibraryObjC, EntryPoint = "objc_getClass")]
        private static extern IntPtr GetClass([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "sel_registerName")]
        private static extern IntPtr Selector([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern void VoidMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern void VoidBoolMessage(IntPtr receiver, IntPtr selector, [MarshalAs(UnmanagedType.I1)] bool argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern void VoidMessageWithArg(IntPtr receiver, IntPtr selector, IntPtr argument);

        public static void ActivateCurrentApplication()
        {
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.OSX)) return;
            try
            {
                var application = IntPtrMessage(GetClass("NSApplication"), Selector("sharedApplication"));
                if (application != IntPtr.Zero)
                {
                    VoidBoolMessage(application, Selector("activateIgnoringOtherApps:"), true);
                    // arrangeInFront: は sender 引数を取る。nil (IntPtr.Zero) を明示的に渡す。
                    VoidMessageWithArg(application, Selector("arrangeInFront:"), IntPtr.Zero);
                }
            }
            catch
            {
                // Silently fail if Obj-C interop doesn't work
            }
        }
    }
}
