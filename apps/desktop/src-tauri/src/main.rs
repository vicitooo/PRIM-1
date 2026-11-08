// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // macOS: the crash-guard helper the desktop starts for itself. Runs alone,
    // before anything else, and never returns.
    #[cfg(target_os = "macos")]
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new(pty_host::ORPHAN_GUARD_ARGUMENT)) {
        pty_host::run_orphan_guard();
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-identify")) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
        match pane_mcp::run_identify_cli(&args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                pane_mcp::print_error_line(format!("PRIM-1 identify helper failed: {error:#}"));
                std::process::exit(2);
            }
        }
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-room")) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
        match pane_mcp::run_room_cli(&args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                pane_mcp::print_error_line(format!("PRIM-1 room CLI failed: {error:#}"));
                std::process::exit(2);
            }
        }
    }
    // A16: the operator entry for a same-user local process that is not a pane.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-operator")) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
        match pane_mcp::run_operator_cli(&args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                pane_mcp::print_error_line(format!("PRIM-1 operator entry failed: {error:#}"));
                std::process::exit(2);
            }
        }
    }
    // An invited outside agent's door: PRIM1_GUEST_TOKEN is its identity.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-guest")) {
        let args: Vec<std::ffi::OsString> = std::env::args_os().skip(2).collect();
        match pane_mcp::run_guest_cli(&args) {
            Ok(code) => std::process::exit(code),
            Err(error) => {
                pane_mcp::print_error_line(format!("PRIM-1 guest CLI failed: {error:#}"));
                std::process::exit(2);
            }
        }
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-guest-mcp")) {
        if std::env::args_os().count() != 2 {
            pane_mcp::print_error_line("guest MCP mode accepts no additional arguments");
            std::process::exit(2);
        }
        if let Err(error) = pane_mcp::run_guest_stdio() {
            pane_mcp::print_error_line(format!("guest MCP server failed: {error:#}"));
            std::process::exit(2);
        }
        return;
    }
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--prim1-pane-mcp")) {
        if std::env::args_os().count() != 2 {
            pane_mcp::print_error_line("pane MCP mode accepts no additional arguments");
            std::process::exit(2);
        }
        if let Err(error) = pane_mcp::run_stdio() {
            pane_mcp::print_error_line(format!("pane MCP server failed: {error:#}"));
            std::process::exit(2);
        }
        return;
    }

    // A16: an unknown `--prim1-…` mode (a typo, or a mode an older build
    // lacks) exits instead of starting the desktop, which over SSH would be
    // an invisible session-0 instance (Runtime Operator, 20 Oct).
    if let Some(mode) = std::env::args_os().nth(1).filter(|arg| arg.to_string_lossy().starts_with("--prim1-")) {
        pane_mcp::print_error_line(format!("unknown PRIM-1 mode {}; modes: --prim1-room, --prim1-operator, --prim1-guest, --prim1-guest-mcp, --prim1-pane-mcp, --prim1-identify",
            mode.to_string_lossy()));
        std::process::exit(2);
    }

    // macOS: a Finder launch gives only /usr/bin:/bin:/usr/sbin:/sbin, so the
    // CLIs and everything they run would be missing. Still single-threaded.
    #[cfg(target_os = "macos")]
    {
        let path = supervisor::macos_child_path(std::env::var_os("PATH").as_deref());
        unsafe { std::env::set_var("PATH", path) };
    }

    let startup = cli_master_wrapper_desktop_lib::load_startup_config().unwrap_or_else(|error| {
        eprintln!("startup configuration: {error}");
        std::process::exit(2);
    });

    // R9: WebView2 remote debugging stays off unless PRIM1_DEBUG_CDP_PORT
    // asked for it. Removing the switch otherwise is what makes "off" hold
    // when the launching shell already carries one. Still single-threaded.
    let browser_arguments_env = cli_master_wrapper_desktop_lib::WEBVIEW2_BROWSER_ARGUMENTS_ENV;
    match startup.webview_browser_arguments() {
        Some(args) => unsafe { std::env::set_var(browser_arguments_env, args) },
        None => unsafe { std::env::remove_var(browser_arguments_env) },
    }
    // The trial's validated private profile replaces any inherited WebView2
    // override. This runs before threads and before any WebView exists.
    // Normal startup deliberately retains its existing profile behavior.
    if let Some(profile) = startup.trial_webview_profile() {
        unsafe { std::env::set_var(cli_master_wrapper_desktop_lib::WEBVIEW_PROFILE_ENV, profile) };
    }
    // A108: the new build's one-time upgrade variables were read above; no
    // pane or later child inherits them. Still single-threaded.
    for name in [supervisor::upgrade_flow::ATTEMPT_ENV, supervisor::upgrade_flow::REQUEST_FILE_ENV] {
        unsafe { std::env::remove_var(name) };
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
        // Kept for the life of the process; telemetry inventories PRIM-1's
        // whole process tree through it.
        if let Err(error) = pty_host::register_app_job(job) {
            eprintln!("wrapper-root job: {error:#}");
            std::process::exit(2);
        }
    }
    cli_master_wrapper_desktop_lib::run(startup)
}
