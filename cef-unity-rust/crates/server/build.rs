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
        println!("cargo:rustc-link-lib=gbm");
        println!("cargo:rustc-link-lib=EGL");
        println!("cargo:rustc-link-lib=GLESv2");
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
