use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::Arc,
    sync::mpsc,
    thread,
    time::Duration,
};

use shared_types::{
    CreatePairRequest, DeletePairRequest, RenamePairRequest, RestartSessionRequest,
    RouteMessageRequest, RuntimeSnapshot, SendInputRequest, SessionSnapshot, StartSessionRequest,
    StopSessionRequest,
};
use supervisor::{SupervisorConfig, SupervisorHandle};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

#[derive(Clone)]
struct DesktopDiagnostics {
    path: Arc<PathBuf>,
}

impl DesktopDiagnostics {
    fn new(runtime_dir: &Path) -> Result<Self, String> {
        fs::create_dir_all(runtime_dir).map_err(|error| error.to_string())?;
        Ok(Self {
            path: Arc::new(runtime_dir.join("desktop-events.jsonl")),
        })
    }

    fn log(&self, level: &str, event: &str, message: impl Into<String>) {
        let entry = serde_json::json!({
            "timestamp": shared_types::now_rfc3339(),
            "pid": std::process::id(),
            "level": level,
            "event": event,
            "message": message.into(),
        });

        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.path.as_ref())
        {
            let _ = writeln!(file, "{entry}");
        }
    }
}

struct DesktopState {
    supervisor: SupervisorHandle,
    diagnostics: DesktopDiagnostics,
}

#[derive(Debug, Clone)]
struct PendingSessionOutput {
    chunk: String,
    synthetic: bool,
    timestamp: String,
}

#[tauri::command]
fn bootstrap(state: State<'_, DesktopState>) -> Result<RuntimeSnapshot, String> {
    state
        .diagnostics
        .log("info", "bootstrap", "runtime snapshot requested");
    Ok(state.supervisor.snapshot())
}

#[tauri::command]
fn start_session(
    state: State<'_, DesktopState>,
    request: StartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "start_session",
        format!("requested {}", request.name),
    );
    state
        .supervisor
        .start_session(&request.name, request.extra_args)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "start_session_failed",
                format!("{}: {}", request.name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn stop_session(
    state: State<'_, DesktopState>,
    request: StopSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "stop_session",
        format!("requested {}", request.name),
    );
    state
        .supervisor
        .stop_session(&request.name)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "stop_session_failed",
                format!("{}: {}", request.name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn restart_session(
    state: State<'_, DesktopState>,
    request: RestartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "restart_session",
        format!("requested {}", request.name),
    );
    state
        .supervisor
        .restart_session(&request.name)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "restart_session_failed",
                format!("{}: {}", request.name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn create_pair(
    state: State<'_, DesktopState>,
    request: CreatePairRequest,
) -> Result<Vec<SessionSnapshot>, String> {
    state
        .diagnostics
        .log("info", "create_pair", format!("requested {}", request.name));
    state
        .supervisor
        .create_pair(&request.name)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "create_pair_failed",
                format!("{}: {}", request.name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn rename_pair(
    state: State<'_, DesktopState>,
    request: RenamePairRequest,
) -> Result<Vec<SessionSnapshot>, String> {
    state.diagnostics.log(
        "info",
        "rename_pair",
        format!("requested {} -> {}", request.old_name, request.new_name),
    );
    state
        .supervisor
        .rename_pair(&request.old_name, &request.new_name)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "rename_pair_failed",
                format!("{} -> {}: {}", request.old_name, request.new_name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn delete_pair(state: State<'_, DesktopState>, request: DeletePairRequest) -> Result<(), String> {
    state
        .diagnostics
        .log("info", "delete_pair", format!("requested {}", request.name));
    state
        .supervisor
        .delete_pair(&request.name)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "delete_pair_failed",
                format!("{}: {}", request.name, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn send_input(
    state: State<'_, DesktopState>,
    request: SendInputRequest,
) -> Result<SessionSnapshot, String> {
    state.supervisor.send_input(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "send_input_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn route_message(
    state: State<'_, DesktopState>,
    request: RouteMessageRequest,
) -> Result<RuntimeSnapshot, String> {
    state.diagnostics.log(
        "info",
        "route_message",
        format!(
            "from={} to={} scope={:?} content_len={}",
            request.from,
            request.to,
            request.scope,
            request.content.len()
        ),
    );
    state.supervisor.route_message(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "route_message_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn resize_session(
    state: State<'_, DesktopState>,
    name: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .supervisor
        .resize_session(&name, cols, rows)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "resize_session_failed",
                format!("{} {}x{}: {}", name, cols, rows, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn toggle_fullscreen(window: WebviewWindow) -> Result<(), String> {
    let is_fullscreen = window.is_fullscreen().map_err(|error| error.to_string())?;
    window
        .set_fullscreen(!is_fullscreen)
        .map_err(|error| error.to_string())
}

fn resolve_project_root() -> Result<PathBuf, String> {
    let canonical = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .canonicalize()
        .map_err(|error| format!("failed to resolve project root: {error}"))?;

    Ok(normalize_path_for_child_processes(canonical))
}

fn resolve_agent_working_root(project_root: &Path) -> Result<PathBuf, String> {
    let personal_root = project_root.parent().ok_or_else(|| {
        format!(
            "failed to resolve personal repo root from {}",
            project_root.display()
        )
    })?;

    Ok(normalize_path_for_child_processes(
        personal_root.to_path_buf(),
    ))
}

fn peer_slash_commands_allowed_from_env() -> bool {
    std::env::var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            normalized == "1" || normalized == "true"
        })
        .unwrap_or(false)
}

fn cross_pair_room_broadcast_from_env() -> bool {
    std::env::var("PRIM1_CROSS_PAIR_ROOM_BROADCAST")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            normalized == "1" || normalized == "true"
        })
        .unwrap_or(false)
}

fn normalize_path_for_child_processes(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        const VERBATIM_PREFIX: &str = r"\\?\";
        const VERBATIM_UNC_PREFIX: &str = r"\\?\UNC\";

        let path_text = path.to_string_lossy();
        if let Some(rest) = path_text.strip_prefix(VERBATIM_UNC_PREFIX) {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = path_text.strip_prefix(VERBATIM_PREFIX) {
            return PathBuf::from(rest);
        }
    }

    path
}

fn install_panic_hook(diagnostics: DesktopDiagnostics) {
    let previous = panic::take_hook();
    panic::set_hook(Box::new(move |panic_info| {
        diagnostics.log("error", "panic", panic_info.to_string());
        previous(panic_info);
    }));
}

fn init_supervisor(
    app: &AppHandle,
    project_root: PathBuf,
    diagnostics: &DesktopDiagnostics,
) -> Result<SupervisorHandle, String> {
    let agent_working_root = resolve_agent_working_root(&project_root)?;
    let runtime_dir = project_root.join(".runtime");
    let peer_slash_commands_allowed = peer_slash_commands_allowed_from_env();
    let cross_pair_room_broadcast = cross_pair_room_broadcast_from_env();
    diagnostics.log(
        "info",
        "supervisor_init",
        format!(
            "runtime_dir={} agent_working_root={} peer_slash_commands_allowed={} cross_pair_room_broadcast={}",
            runtime_dir.display(),
            agent_working_root.display(),
            peer_slash_commands_allowed,
            cross_pair_room_broadcast
        ),
    );
    let supervisor = SupervisorHandle::new(SupervisorConfig {
        working_root: agent_working_root,
        runtime_dir,
        peer_slash_commands_allowed,
        cross_pair_room_broadcast,
    })
    .map_err(|error| error.to_string())?;

    let ui_event_tx = start_ui_event_bridge(app.clone(), diagnostics.clone());
    supervisor.set_event_sink(move |event| {
        let _ = ui_event_tx.send(event);
    });
    let control_plane = supervisor.start_control_plane().map_err(|error| {
        diagnostics.log("error", "control_plane_start_failed", error.to_string());
        error.to_string()
    })?;
    diagnostics.log(
        "info",
        "control_plane_ready",
        format!("endpoint={}", control_plane.endpoint),
    );

    Ok(supervisor)
}

fn start_ui_event_bridge(
    app: AppHandle,
    diagnostics: DesktopDiagnostics,
) -> mpsc::Sender<shared_types::RuntimeEvent> {
    let (tx, rx) = mpsc::channel::<shared_types::RuntimeEvent>();

    thread::spawn(move || {
        let mut pending = HashMap::<String, PendingSessionOutput>::new();

        loop {
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(event) => match event {
                    shared_types::RuntimeEvent::SessionOutput {
                        session,
                        chunk,
                        synthetic,
                        timestamp,
                    } => {
                        let sanitized = sanitize_terminal_output_for_ui(&chunk);
                        if sanitized.is_empty() {
                            continue;
                        }

                        pending
                            .entry(session)
                            .and_modify(|buffer| {
                                buffer.chunk.push_str(&sanitized);
                                buffer.synthetic &= synthetic;
                                buffer.timestamp = timestamp.clone();
                            })
                            .or_insert(PendingSessionOutput {
                                chunk: sanitized,
                                synthetic,
                                timestamp,
                            });
                    }
                    event => {
                        flush_pending_session_output(&app, &diagnostics, &mut pending);
                        emit_runtime_event(&app, &diagnostics, event);
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    flush_pending_session_output(&app, &diagnostics, &mut pending);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    flush_pending_session_output(&app, &diagnostics, &mut pending);
                    break;
                }
            }
        }
    });

    tx
}

fn flush_pending_session_output(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    pending: &mut HashMap<String, PendingSessionOutput>,
) {
    let drained = pending.drain().collect::<Vec<_>>();
    for (session, buffer) in drained {
        emit_runtime_event(
            app,
            diagnostics,
            shared_types::RuntimeEvent::SessionOutput {
                session,
                chunk: buffer.chunk,
                synthetic: buffer.synthetic,
                timestamp: buffer.timestamp,
            },
        );
    }
}

fn emit_runtime_event(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    event: shared_types::RuntimeEvent,
) {
    if let Err(error) = app.emit("runtime://event", event) {
        diagnostics.log("error", "ui_emit_failed", error.to_string());
    }
}

/// Emit a boot-time diagnostic naming the exact frontend asset this release EXE has
/// bundled. Makes deployment skew (stale embedded dist vs on-disk dist) visible in
/// desktop-events.jsonl without needing to attach CDP. Rule: when WebView behavior
/// contradicts on-disk source, first check which bundle the live page actually loaded.
fn log_frontend_bundle_id(diagnostics: &DesktopDiagnostics) {
    const INDEX_HTML: &str = include_str!("../../dist/index.html");
    let main_script = INDEX_HTML
        .split("<script")
        .filter_map(|s| s.split("src=\"").nth(1))
        .filter_map(|s| s.split('"').next())
        .find(|s| s.ends_with(".js"))
        .unwrap_or("<no js script found in index.html>");
    diagnostics.log(
        "info",
        "frontend_bundle",
        format!("main_script={main_script}"),
    );
}

fn sanitize_terminal_output_for_ui(chunk: &str) -> String {
    strip_osc_sequences(chunk)
}

fn strip_osc_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b']' {
            index += 2;
            while index < bytes.len() {
                if bytes[index] == 0x07 {
                    index += 1;
                    break;
                }
                if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\' {
                    index += 2;
                    break;
                }
                index += 1;
            }
            continue;
        }

        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let project_root = resolve_project_root().expect("failed to resolve project root");
    let runtime_dir = project_root.join(".runtime");
    let diagnostics =
        DesktopDiagnostics::new(&runtime_dir).expect("failed to initialize diagnostics");
    install_panic_hook(diagnostics.clone());
    diagnostics.log("info", "process_start", "desktop process booting");
    let setup_diagnostics = diagnostics.clone();
    let exit_diagnostics = diagnostics.clone();
    let setup_project_root = project_root.clone();

    tauri::Builder::default()
        .setup(move |app| {
            setup_diagnostics.log("info", "tauri_setup", "starting setup");
            log_frontend_bundle_id(&setup_diagnostics);
            let supervisor =
                init_supervisor(app.handle(), setup_project_root.clone(), &setup_diagnostics)?;
            app.manage(DesktopState {
                supervisor,
                diagnostics: setup_diagnostics.clone(),
            });
            let main_window = app
                .get_webview_window("main")
                .ok_or_else(|| "missing main window".to_string())?;
            main_window
                .set_fullscreen(true)
                .map_err(|error| error.to_string())?;
            setup_diagnostics.log("info", "window_ready", "main window fullscreen applied");
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                let state = window.app_handle().state::<DesktopState>();
                state.diagnostics.log(
                    "info",
                    "window_close",
                    "shutting down supervisor before close",
                );
                if let Err(error) = state.supervisor.shutdown() {
                    state
                        .diagnostics
                        .log("error", "shutdown_failed", format!("{error:#}"));
                }
                state
                    .diagnostics
                    .log("info", "window_close", "supervisor shutdown complete");
            }
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            start_session,
            stop_session,
            restart_session,
            create_pair,
            rename_pair,
            delete_pair,
            send_input,
            route_message,
            resize_session,
            toggle_fullscreen
        ])
        .run(tauri::generate_context!())
        .unwrap_or_else(|error| {
            exit_diagnostics.log(
                "error",
                "process_exit",
                format!("tauri run failed: {error}"),
            );
            panic!("error while running tauri application: {error}");
        });

    diagnostics.log("info", "process_exit", "tauri run exited cleanly");
}

#[cfg(test)]
mod tests {
    use super::{
        cross_pair_room_broadcast_from_env, normalize_path_for_child_processes,
        peer_slash_commands_allowed_from_env, resolve_agent_working_root,
        sanitize_terminal_output_for_ui, strip_osc_sequences,
    };
    use std::{path::PathBuf, sync::Mutex};

    static PEER_SLASH_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn strip_osc_sequences_removes_bell_terminated_title_updates() {
        let input = "hello\x1b]0;spinner title\x07world";
        assert_eq!(strip_osc_sequences(input), "helloworld");
    }

    #[test]
    fn strip_osc_sequences_removes_st_terminated_sequences() {
        let input = "a\x1b]0;another title\x1b\\b";
        assert_eq!(strip_osc_sequences(input), "ab");
    }

    #[test]
    fn sanitize_terminal_output_for_ui_keeps_non_osc_content() {
        let input = "\x1b[31mwarn\x1b[0m\x1b]0;ignored\x07";
        assert_eq!(
            sanitize_terminal_output_for_ui(input),
            "\x1b[31mwarn\x1b[0m"
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_for_child_processes_strips_verbatim_drive_prefix() {
        let path = PathBuf::from(r"\\?\.");

        assert_eq!(
            normalize_path_for_child_processes(path),
            PathBuf::from(r".")
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_for_child_processes_strips_verbatim_unc_prefix() {
        let path = PathBuf::from(r"\\?\UNC\server\share\CLI-master-wrapper");

        assert_eq!(
            normalize_path_for_child_processes(path),
            PathBuf::from(r"\\server\share\CLI-master-wrapper")
        );
    }

    #[cfg(windows)]
    #[test]
    fn resolve_agent_working_root_returns_parent_repo_root() {
        let path = PathBuf::from(r".");

        assert_eq!(
            resolve_agent_working_root(&path).unwrap(),
            PathBuf::from(r"<workspace>")
        );
    }

    #[test]
    fn peer_slash_commands_allowed_defaults_to_false() {
        let _guard = PEER_SLASH_ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("PRIM1_PEER_SLASH_COMMANDS_ALLOWED");
        unsafe {
            std::env::remove_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED");
        }

        assert!(!peer_slash_commands_allowed_from_env());

        if let Some(value) = previous {
            unsafe {
                std::env::set_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED", value);
            }
        }
    }

    #[test]
    fn peer_slash_commands_allowed_accepts_one_and_true() {
        let _guard = PEER_SLASH_ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("PRIM1_PEER_SLASH_COMMANDS_ALLOWED");

        unsafe {
            std::env::set_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED", "1");
        }
        assert!(peer_slash_commands_allowed_from_env());

        unsafe {
            std::env::set_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED", "true");
        }
        assert!(peer_slash_commands_allowed_from_env());

        match previous {
            Some(value) => unsafe {
                std::env::set_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED", value);
            },
            None => unsafe {
                std::env::remove_var("PRIM1_PEER_SLASH_COMMANDS_ALLOWED");
            },
        }
    }

    #[test]
    fn cross_pair_room_broadcast_defaults_to_false() {
        let _guard = PEER_SLASH_ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("PRIM1_CROSS_PAIR_ROOM_BROADCAST");
        unsafe {
            std::env::remove_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST");
        }

        assert!(!cross_pair_room_broadcast_from_env());

        if let Some(value) = previous {
            unsafe {
                std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", value);
            }
        }
    }

    #[test]
    fn cross_pair_room_broadcast_accepts_one_and_true() {
        let _guard = PEER_SLASH_ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("PRIM1_CROSS_PAIR_ROOM_BROADCAST");

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "1");
        }
        assert!(cross_pair_room_broadcast_from_env());

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "true");
        }
        assert!(cross_pair_room_broadcast_from_env());

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "TRUE");
        }
        assert!(cross_pair_room_broadcast_from_env());

        match previous {
            Some(value) => unsafe {
                std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", value);
            },
            None => unsafe {
                std::env::remove_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST");
            },
        }
    }

    #[test]
    fn cross_pair_room_broadcast_rejects_garbage() {
        let _guard = PEER_SLASH_ENV_LOCK.lock().unwrap();
        let previous = std::env::var_os("PRIM1_CROSS_PAIR_ROOM_BROADCAST");

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "yes");
        }
        assert!(!cross_pair_room_broadcast_from_env());

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "0");
        }
        assert!(!cross_pair_room_broadcast_from_env());

        unsafe {
            std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", "false");
        }
        assert!(!cross_pair_room_broadcast_from_env());

        match previous {
            Some(value) => unsafe {
                std::env::set_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST", value);
            },
            None => unsafe {
                std::env::remove_var("PRIM1_CROSS_PAIR_ROOM_BROADCAST");
            },
        }
    }

    #[cfg(not(windows))]
    #[test]
    fn normalize_path_for_child_processes_leaves_non_windows_paths_unchanged() {
        let path = PathBuf::from("/workspace/cli-master-wrapper");

        assert_eq!(
            normalize_path_for_child_processes(path.clone()),
            PathBuf::from("/workspace/cli-master-wrapper")
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn resolve_agent_working_root_returns_parent_repo_root() {
        let path = PathBuf::from("/workspace/cli-master-wrapper");

        assert_eq!(
            resolve_agent_working_root(&path).unwrap(),
            PathBuf::from("/workspace")
        );
    }
}
