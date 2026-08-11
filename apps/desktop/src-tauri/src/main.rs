// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
static WRAPPER_JOB: OnceLock<pty_host::ProcessJob> = OnceLock::new();

fn main() {
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-pane-mcp")) {
        if std::env::args_os().count() != 2 {
            eprintln!("pane MCP mode accepts no additional arguments");
            std::process::exit(2);
        }
        if let Err(error) = pane_mcp::run_stdio() {
            eprintln!("pane MCP server failed: {error:#}");
            std::process::exit(2);
        }
        return;
    }

    let startup = cli_master_wrapper_desktop_lib::load_startup_config().unwrap_or_else(|error| {
        eprintln!("startup configuration: {error}");
        std::process::exit(2);
    });

    if let Some(port) = startup.cdp_port() {
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
    cli_master_wrapper_desktop_lib::run(startup)
}
