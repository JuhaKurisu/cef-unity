fn main() {
    // Linux: libcef.so は実行ファイルと同じディレクトリに配置されるため、
    // $ORIGIN を RPATH に入れて LD_LIBRARY_PATH なしで解決させる。
    #[cfg(target_os = "linux")]
    println!("cargo:rustc-link-arg=-Wl,-rpath,$ORIGIN");
}
