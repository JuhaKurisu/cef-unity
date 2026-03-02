fn main() {
    cef_unity_rust::load_cef_auto();
    let args = cef::args::Args::new();
    std::process::exit(cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut()));
}
