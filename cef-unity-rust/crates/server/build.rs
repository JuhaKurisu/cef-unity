fn main() {
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
