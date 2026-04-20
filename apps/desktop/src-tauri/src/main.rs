// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
static WRAPPER_JOB: OnceLock<pty_host::ProcessJob> = OnceLock::new();

fn main() {
    if let Ok(port) = std::env::var("PRIM1_CDP_PORT") {
        let args = format!("--remote-debugging-port={port} --remote-allow-origins=*");
        unsafe {
            std::env::set_var("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", args);
        }
    }
    #[cfg(windows)]
    {
        let job = match pty_host::ProcessJob::new() {
            Ok(job) => job,
            Err(error) => {
                eprintln!("wrapper-root job: create failed: {error:#}");
                std::process::exit(2);
            }
        };
        if let Err(error) = job.assign_current_process() {
            eprintln!("wrapper-root job: assign_current_process failed: {error:#}");
            std::process::exit(2);
        }
        if WRAPPER_JOB.set(job).is_err() {
            eprintln!("wrapper-root job: already initialized");
            std::process::exit(2);
        }
    }
    cli_master_wrapper_desktop_lib::run()
}
