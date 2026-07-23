fn main() {
    // cc はファイル監視 (rerun-if-changed) を一切出力しない (rerun-if-env-changed のみ)。
    // かつ rerun-if 系を 1 つでも出すと cargo のデフォルト全ファイル監視が無効になるため、
    // src/ をディレクトリごと明示監視する (.m/.c の再コンパイルと lib.rs 変更での
    // csbindgen 再実行の両方をカバー。個別列挙は宣言漏れの温床なので避ける)。
    println!("cargo:rerun-if-changed=src");

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
        cc::Build::new().file("src/au_output.c").compile("au_output");
        cc::Build::new()
            .file("src/scroll_monitor.m")
            .flag("-fobjc-arc")
            .compile("scroll_monitor");
        println!("cargo:rustc-link-lib=framework=AudioUnit");
        println!("cargo:rustc-link-lib=framework=AudioToolbox");
        println!("cargo:rustc-link-lib=framework=CoreAudio");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=IOSurface");
        println!("cargo:rustc-link-lib=framework=CoreFoundation");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=dylib=objc");
        println!("cargo:rustc-link-lib=framework=AppKit");
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
