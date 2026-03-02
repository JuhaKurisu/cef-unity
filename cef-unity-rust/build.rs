fn main() {
    csbindgen::Builder::default()
        .input_extern_file("src/ffi.rs")
        .csharp_dll_name("cef_unity_rust")
        .csharp_namespace("CefUnity")
        .csharp_class_name("NativeMethods")
        .csharp_use_function_pointer(false)
        .generate_csharp_file("dotnet/NativeMethods.g.cs")
        .unwrap();
}
