fn main() {
    // cc はファイル監視を出力しないため、src/ をディレクトリごと明示監視する。
    // これが無いと .c / .m の変更が検知されず、古い .o がリンクされ続ける
    // (client/build.rs と同じ理由。実際にこの罠を踏んだ)。
    println!("cargo:rerun-if-changed=src");

    // Linux: libcef.so は実行ファイルと同じディレクトリに配置されるため、
    // $ORIGIN を RPATH に入れて LD_LIBRARY_PATH なしで解決させる。
    #[cfg(target_os = "linux")]
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

        // dmabuf プール (EGL / GBM / GLES)。macOS の iosurface_pool.m に相当する。
        cc::Build::new()
            .file("src/dmabuf_pool.c")
            .compile("dmabuf_pool");
        // 診断専用: dmabuf を Vulkan で読むプローブ。
        cc::Build::new()
            .file("src/dmabuf_vulkan_probe.c")
            .compile("dmabuf_vulkan_probe");
        println!("cargo:rustc-link-lib=vulkan");
        println!("cargo:rustc-link-lib=gbm");
        // EGL と GLESv2 は soname で指定する。cef クレート (151+) が CEF 配布物の
        // ディレクトリをリンクパスに入れるため、-lGLESv2 だと同梱の ANGLE 版
        // libGLESv2.so (gl* シンボルを export しない) を掴んで undefined symbol になる。
        // libGLESv2.so.2 / libEGL.so.1 という名前は CEF 配布物に存在しないので、
        // システム (mesa) 側が確実に選ばれる。
        println!("cargo:rustc-link-arg=-l:libEGL.so.1");
        println!("cargo:rustc-link-arg=-l:libGLESv2.so.2");
        println!("cargo:rustc-link-arg=-lvulkan");
    }

    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/cef_app_inject.m")
            .flag("-fobjc-arc")
            .compile("cef_app_inject");
        cc::Build::new()
            .file("src/mach_iosurface.c")
            .compile("mach_iosurface");
        cc::Build::new()
            .file("src/iosurface_pool.m")
            .flag("-fobjc-arc")
            .compile("iosurface_pool");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=IOSurface");
        println!("cargo:rustc-link-lib=framework=Metal");
    }
}
