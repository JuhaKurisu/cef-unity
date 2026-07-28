using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>
    ///     SDL は macOS で NSUserDefaults 登録ドメインに AppleMomentumScrollSupported=NO を
    ///     登録し、OS のモメンタム (慣性) スクロールイベント生成を止める (ゲーム向け仕様)。
    ///     app ドメインは登録ドメインより優先されるため、YES を明示設定して
    ///     通常アプリと同じモメンタム配送を復元する。SDL 初期化前に呼ぶこと。
    /// </summary>
    public static class MacMomentumScrollSupport
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
            // 非 macOS では objc ランタイムが無く NativeLibrary.Load が例外を投げるため、
            // ここで抜ける (Windows で Program が起動できなくなるのを防ぐ)。
            if (!RuntimeInformation.IsOSPlatform(OSPlatform.OSX)) return;

            NativeLibrary.Load("/System/Library/Frameworks/Foundation.framework/Foundation");

            var userDefaultsClass = GetClass("NSUserDefaults");
            if (userDefaultsClass == IntPtr.Zero)
            {
                Console.Error.WriteLine("MacMomentumScrollSupport: NSUserDefaults class not found — momentum restore skipped");
                return;
            }

            var defaults = IntPtrMessage(userDefaultsClass, Selector("standardUserDefaults"));
            var key = IntPtrStringMessage(GetClass("NSString"), Selector("stringWithUTF8String:"), "AppleMomentumScrollSupported");
            VoidBoolKeyMessage(defaults, Selector("setBool:forKey:"), true, key);

            var resolvedValue = BoolKeyMessage(defaults, Selector("boolForKey:"), key);
            Console.WriteLine($"MacMomentumScrollSupport: AppleMomentumScrollSupported resolved={resolvedValue}");
        }
    }
}
