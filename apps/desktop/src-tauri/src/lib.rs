use std::path::PathBuf;

use shared_types::{
    RestartSessionRequest, RouteMessageRequest, RuntimeSnapshot, SendInputRequest, SessionSnapshot,
    StartSessionRequest, StopSessionRequest,
};
use supervisor::{SupervisorConfig, SupervisorHandle};
use tauri::{AppHandle, Emitter, Manager, State};

struct DesktopState {
    supervisor: SupervisorHandle,
}

#[tauri::command]
fn bootstrap(state: State<'_, DesktopState>) -> Result<RuntimeSnapshot, String> {
    Ok(state.supervisor.snapshot())
}

#[tauri::command]
fn start_session(
    state: State<'_, DesktopState>,
    request: StartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state
        .supervisor
        .start_session(&request.name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn stop_session(
    state: State<'_, DesktopState>,
    request: StopSessionRequest,
) -> Result<SessionSnapshot, String> {
    state
        .supervisor
        .stop_session(&request.name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn restart_session(
    state: State<'_, DesktopState>,
    request: RestartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state
        .supervisor
        .restart_session(&request.name)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn send_input(
    state: State<'_, DesktopState>,
    request: SendInputRequest,
) -> Result<SessionSnapshot, String> {
    state
        .supervisor
        .send_input(request)
        .map_err(|error| error.to_string())
}

#[tauri::command]
fn route_message(
    state: State<'_, DesktopState>,
    request: RouteMessageRequest,
) -> Result<RuntimeSnapshot, String> {
    state
        .supervisor
        .route_message(request)
        .map_err(|error| error.to_string())
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
        .map_err(|error| error.to_string())
}

fn init_supervisor(app: &AppHandle) -> Result<SupervisorHandle, String> {
    let project_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("..")
        .canonicalize()
        .map_err(|error| format!("failed to resolve project root: {error}"))?;
    let runtime_dir = project_root.join(".runtime");
    let supervisor = SupervisorHandle::new(SupervisorConfig {
        working_root: project_root,
        runtime_dir,
    })
    .map_err(|error| error.to_string())?;

    let handle = app.clone();
    supervisor.set_event_sink(move |event| {
        let _ = handle.emit("runtime://event", event);
    });
    supervisor
        .start_control_plane()
        .map_err(|error| error.to_string())?;

    Ok(supervisor)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            let supervisor = init_supervisor(&app.handle())?;
            app.manage(DesktopState { supervisor });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            bootstrap,
            start_session,
            stop_session,
            restart_session,
            send_input,
            route_message,
            resize_session
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
