// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
use std::sync::OnceLock;

#[cfg(windows)]
static WRAPPER_JOB: OnceLock<pty_host::ProcessJob> = OnceLock::new();

fn load_dotenv_from_project_root() {
    // CARGO_MANIFEST_DIR is the path to apps/desktop/src-tauri at build time.
    // Walk up 3 levels to reach the PRIM-1 repo root. The exe is built locally
    // and runs on the same machine, so this resolves correctly at runtime.
    // Loading .env before any env_var read lets operators configure
    // PRIM1_AGENT_WORKING_ROOT et al. in a gitignored file at the repo root.
    // dotenvy requires quoted values when they contain spaces (e.g.
    // PRIM1_AGENT_WORKING_ROOT="<workspace>"). Missing file is fine —
    // resolve_agent_working_root falls back to project_root.parent().
    let project_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..");
    let _ = dotenvy::from_path(project_root.join(".env"));
}

fn main() {
    load_dotenv_from_project_root();

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
