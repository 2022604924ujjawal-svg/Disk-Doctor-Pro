// Windows release builds use the GUI subsystem. Development builds retain a
// console so native diagnostics remain visible.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Some(exit_code) = mangodisk_platform::run_startup_helper_mode(std::env::args_os()) {
        std::process::exit(exit_code);
    }
    #[cfg(windows)]
    if let Some(exit_code) =
        mangodisk_platform::run_system_settings_helper_mode(std::env::args_os())
    {
        std::process::exit(exit_code);
    }
    mangodisk_lib::run();
}
