using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>objc_msgSend ベースの最小 Metal/CoreAnimation 呼び出し (MetalFrameRenderer 専用)。</summary>
    internal static class MetalNative
    {
        private const string LibraryObjC = "/usr/lib/libobjc.A.dylib";
        private const string LibraryMetal = "/System/Library/Frameworks/Metal.framework/Metal";

        [StructLayout(LayoutKind.Sequential)]
        internal struct CGSize
        {
            public double Width;
            public double Height;
        }

        [DllImport(LibraryObjC, EntryPoint = "sel_registerName")]
        internal static extern IntPtr Selector([MarshalAs(UnmanagedType.LPStr)] string name);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern IntPtr IntPtrMessage(IntPtr receiver, IntPtr selector, IntPtr argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector, IntPtr argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidMessage(IntPtr receiver, IntPtr selector, IntPtr argument1, IntPtr argument2);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidBoolMessage(IntPtr receiver, IntPtr selector, [MarshalAs(UnmanagedType.I1)] bool argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidCGSizeMessage(IntPtr receiver, IntPtr selector, CGSize argument);

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPush")]
        internal static extern IntPtr AutoreleasePoolPush();

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPop")]
        internal static extern void AutoreleasePoolPop(IntPtr pool);

        [DllImport(LibraryMetal)]
        internal static extern IntPtr MTLCreateSystemDefaultDevice();
    }
}
