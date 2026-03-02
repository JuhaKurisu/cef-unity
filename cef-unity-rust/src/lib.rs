use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// CEFライブラリをロードしてAPIバージョンを設定する。CEFディレクトリを返す。
pub fn load_cef() -> PathBuf {
    let cef_dir = cef::sys::get_cef_dir().expect("CEF directory not found");
    let path = cef_dir.join(cef::sys::FRAMEWORK_PATH);
    let cstr = CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(
        cef::load_library(Some(unsafe { &*cstr.as_ptr().cast() })),
        1,
        "Failed to load CEF framework"
    );
    cef::api_hash(cef::sys::CEF_API_VERSION_LAST, 0);
    cef_dir
}
