pub mod ffi;

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// dylib 自身のディレクトリを返す。
fn dylib_dir() -> PathBuf {
    let info = dl_info().expect("dladdr failed");
    PathBuf::from(info).parent().unwrap().to_path_buf()
}

/// dladdr で dylib のパスを取得する。
fn dl_info() -> Option<String> {
    unsafe extern "C" {
        fn dladdr(addr: *const u8, info: *mut DlInfo) -> i32;
    }
    #[repr(C)]
    struct DlInfo {
        dli_fname: *const std::ffi::c_char,
        dli_fbase: *const u8,
        dli_sname: *const std::ffi::c_char,
        dli_saddr: *const u8,
    }
    let mut info: DlInfo = unsafe { std::mem::zeroed() };
    let ret = unsafe { dladdr(dylib_dir as *const u8, &mut info) };
    if ret == 0 || info.dli_fname.is_null() {
        return None;
    }
    let cstr = unsafe { std::ffi::CStr::from_ptr(info.dli_fname) };
    Some(cstr.to_str().ok()?.to_string())
}

/// CEFライブラリをロードしてAPIバージョンを設定する。
/// plugin_dir からフレームワークのパスを解決する。
pub fn load_cef(plugin_dir: &Path) {
    let framework_path =
        plugin_dir.join("Chromium Embedded Framework.framework/Chromium Embedded Framework");
    load_cef_from_path(&framework_path);
}

/// ヘルパープロセス用: get_cef_dir() でフレームワークを探してロードする。
pub fn load_cef_auto() {
    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let framework_path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    load_cef_from_path(&framework_path);
}

fn load_cef_from_path(framework_path: &Path) {
    let cstr = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
}
