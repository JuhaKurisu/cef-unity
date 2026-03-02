fn main() {
    for dest in [
        "../cef-unity-csharp/Interop/NativeMethods.g.cs",
        "../cef-unity-unityproject/Assets/CefUnity/Interop/NativeMethods.g.cs",
    ] {
        csbindgen::Builder::default()
            .input_extern_file("src/ffi.rs")
            .csharp_dll_name("cef_unity_rust")
            .csharp_namespace("CefUnity")
            .csharp_class_name("NativeMethods")
            .csharp_class_accessibility("public")
            .csharp_use_function_pointer(false)
            .generate_csharp_file(dest)
            .unwrap();
    }
}
