fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("src/metal_texture.m")
            .flag("-fobjc-arc")
            .compile("metal_texture");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=IOSurface");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=dylib=objc");
    }

    for dest in [
        workspace_root.join("../cef-unity-csharp/Interop/NativeMethods.g.cs"),
        workspace_root.join("../cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs"),
    ] {
        csbindgen::Builder::default()
            .input_extern_file("src/lib.rs")
            .csharp_dll_name("cef_unity_rust")
            .csharp_namespace("CefUnity")
            .csharp_class_name("NativeMethods")
            .csharp_class_accessibility("public")
            .csharp_use_function_pointer(false)
            .generate_csharp_file(dest)
            .unwrap();
    }
}
