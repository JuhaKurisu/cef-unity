pub mod ffi;

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

/// dylib 自身のディレクトリを返す。
pub fn dylib_dir() -> PathBuf {
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

/// plugin_dir から CEF フレームワークのディレクトリを探す。
/// 見つからなければ None を返す。
pub fn find_cef_framework(plugin_dir: &Path) -> Option<PathBuf> {
    let fw = plugin_dir.join("Chromium Embedded Framework.framework");
    if fw.exists() {
        return Some(fw);
    }
    // フォールバック: CEF_PATH 環境変数
    if let Ok(cef_path) = std::env::var("CEF_PATH") {
        let fw = PathBuf::from(&cef_path).join("Chromium Embedded Framework.framework");
        if fw.exists() {
            return Some(fw);
        }
    }
    None
}

/// CEFライブラリをロードしてAPIバージョンを設定する。
pub fn load_cef(framework_dir: &Path) {
    let framework_path = framework_dir.join("Chromium Embedded Framework");
    let cstr = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework from {}",
        framework_path.display()
    );
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
}

/// ヘルパープロセス用: get_cef_dir() でフレームワークを探してロードする。
pub fn load_cef_auto() {
    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let framework_path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    let cstr = CString::new(framework_path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
}
