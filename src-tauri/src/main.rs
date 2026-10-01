fn main() {
    nienudno_browser_lib::load_cef_library(false).expect("Nie można załadować biblioteki CEF.");
    use cef::ImplCommandLine;

    let args = cef::args::Args::new();
    let command_line = args
        .as_cmd_line()
        .expect("Nie można odczytać argumentów procesu CEF.");
    let process_type = cef::CefString::from("type");
    if command_line.has_switch(Some(&process_type)) != 0 {
        let result = cef::execute_process(Some(args.as_main_args()), None, std::ptr::null_mut());
        std::process::exit(result.max(0));
    }
    nienudno_browser_lib::run();
}
