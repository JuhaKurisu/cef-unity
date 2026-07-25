using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     SDL は macOS で NSUserDefaults 登録ドメインに AppleMomentumScrollSupported=NO を
    ///     登録し、OS のモメンタム (慣性) スクロールイベント生成を止める (ゲーム向け仕様)。
    ///     app ドメインは登録ドメインより優先されるため、YES を明示設定して
    ///     通常アプリと同じモメンタム配送を復元する。SDL 初期化前に呼ぶこと。
    /// </summary>
    internal static class MacMomentumScrollSupport
    {
        private const string LibraryObjC = "/usr/lib/libobjc.A.dylib";

        [DllImport(LibraryObjC, EntryPoint = "objc_getClass")]
        private static extern IntPtr GetClass([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "sel_registerName")]
        private static extern IntPtr Selector([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern IntPtr IntPtrStringMessage(IntPtr receiver, IntPtr selector, [MarshalAs(UnmanagedType.LPStr)] string argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        private static extern void VoidBoolKeyMessage(IntPtr receiver, IntPtr selector, [MarshalAs(UnmanagedType.I1)] bool value, IntPtr key);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        [return: MarshalAs(UnmanagedType.I1)]
        private static extern bool BoolKeyMessage(IntPtr receiver, IntPtr selector, IntPtr key);

        public static void Enable()
        {
            var defaults = IntPtrMessage(GetClass("NSUserDefaults"), Selector("standardUserDefaults"));
            var key = IntPtrStringMessage(GetClass("NSString"), Selector("stringWithUTF8String:"), "AppleMomentumScrollSupported");
            VoidBoolKeyMessage(defaults, Selector("setBool:forKey:"), true, key);
        }
    }
}
