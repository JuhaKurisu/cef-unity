fn main() {
    // Linux: libcef.so は実行ファイルと同じディレクトリに配置されるため、
    // $ORIGIN を RPATH に入れて LD_LIBRARY_PATH なしで解決させる。
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");

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
