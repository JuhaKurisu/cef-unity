fn main() {
    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/cef_app_inject.m")
            .flag("-fobjc-arc")
            .compile("cef_app_inject");
        println!("cargo:rustc-link-lib=framework=AppKit");
        println!("cargo:rustc-link-lib=framework=IOSurface");
    }
}
