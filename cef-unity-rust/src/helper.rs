use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;

use cef::args::Args;

fn main() {
    // Load CEF library from build output
    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let framework_path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    let framework_cstr =
        CString::new(framework_path.as_os_str().as_bytes()).expect("Invalid framework path");
    assert_eq!(
        cef::load_library(Some(unsafe { &*framework_cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );

    // Configure CEF API version (required before execute_process)
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);

    let args = Args::new();
    let exit_code = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
    std::process::exit(exit_code);
}
