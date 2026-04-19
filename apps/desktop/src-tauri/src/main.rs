// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if let Ok(port) = std::env::var("PRIM1_CDP_PORT") {
        let args = format!("--remote-debugging-port={port} --remote-allow-origins=*");
        unsafe {
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
        }
    }
    cli_master_wrapper_desktop_lib::run()
}
