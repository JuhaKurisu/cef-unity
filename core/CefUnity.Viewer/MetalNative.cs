using System.Runtime.InteropServices;

namespace CefUnity.Viewer
{
    /// <summary>objc_msgSend ベースの最小 Metal/CoreAnimation 呼び出し (MetalFrameRenderer 専用)。</summary>
    internal static class MetalNative
    {
        private const string LibraryObjC = "/usr/lib/libobjc.A.dylib";
        private const string LibraryMetal = "/System/Library/Frameworks/Metal.framework/Metal";

        /// <summary>
        ///     CoreGraphics の CGSize (width, height の double ペア)。
        ///     arm64 ABI では HFA として d0/d1 に返るため、通常の P/Invoke で正しく受け取れる。
        /// </summary>
        [StructLayout(LayoutKind.Sequential)]
        internal struct CGSize
        {
            public double Width;
            public double Height;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct MTLOrigin
        {
            public nuint X;
            public nuint Y;
            public nuint Z;
        }

        [StructLayout(LayoutKind.Sequential)]
        internal struct MTLSize
        {
            public nuint Width;
            public nuint Height;
            public nuint Depth;
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
        internal static extern nuint NuintMessage(IntPtr receiver, IntPtr selector);

        /// <summary>CGSize を返すセレクター (drawableSize 等) の呼び出し。</summary>
        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern CGSize CGSizeMessage(IntPtr receiver, IntPtr selector);

        /// <summary>CGSize を引数に取るセレクター (setDrawableSize: 等) の呼び出し。</summary>
        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void VoidCGSizeMessage(IntPtr receiver, IntPtr selector, CGSize size);

        // copyFromTexture:sourceSlice:sourceLevel:sourceOrigin:sourceSize:toTexture:destinationSlice:destinationLevel:destinationOrigin:
        [DllImport(LibraryObjC, EntryPoint = "objc_msgSend")]
        internal static extern void CopyTextureRegion(
            IntPtr receiver, IntPtr selector,
            IntPtr sourceTexture, nuint sourceSlice, nuint sourceLevel, MTLOrigin sourceOrigin, MTLSize sourceSize,
            IntPtr destinationTexture, nuint destinationSlice, nuint destinationLevel, MTLOrigin destinationOrigin);

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPush")]
        internal static extern IntPtr AutoreleasePoolPush();

        [DllImport(LibraryObjC, EntryPoint = "objc_autoreleasePoolPop")]
        internal static extern void AutoreleasePoolPop(IntPtr pool);

        [DllImport(LibraryMetal)]
        internal static extern IntPtr MTLCreateSystemDefaultDevice();
    }
}
