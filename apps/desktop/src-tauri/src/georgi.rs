//! Georgi themes: small window helpers for the renderer.

use shared_types::{SessionId, SessionSnapshot};
use std::path::PathBuf;
use tauri::State;

/// Give `to` the same working folder as `from` (room templates: a new agent
/// in a project works where the project's agents already work). The renderer
/// names two sessions, never a path: a folder still only ever enters PRIM-1
/// through the native picker, and the supervisor applies its usual checks
/// (the session must be stopped, the folder must exist).
#[tauri::command]
pub fn georgi_copy_session_folder(
    state: State<'_, crate::DesktopState>,
    from_session_id: SessionId,
    to_session_id: SessionId,
) -> Result<SessionSnapshot, String> {
    let folder = state
        .supervisor
        .snapshot()
        .sessions
        .into_iter()
        .find(|session| session.session_id == from_session_id)
        .map(|session| session.working_dir)
        .ok_or_else(|| "the source session no longer exists".to_string())?;
    state
        .supervisor
        .set_session_working_directory(to_session_id, &PathBuf::from(folder))
        .map_err(|error| error.to_string())
}

const LAYOUT_FILE: &str = "georgi-layout.json";
const LAYOUT_MAX_BYTES: u64 = 1024 * 1024;

fn layout_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    use tauri::Manager;
    app.path()
        .app_local_data_dir()
        .map(|dir| dir.join(LAYOUT_FILE))
        .map_err(|error| error.to_string())
}

/// A prepared Georgi layout waiting in PRIM-1's data folder (portfolios,
/// projects, room files to load), or None. Read only; the renderer asks the
/// operator before applying anything, through the ordinary room commands.
#[tauri::command]
pub fn georgi_take_layout(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let path = layout_path(&app)?;
    let Ok(meta) = std::fs::metadata(&path) else {
        return Ok(None);
    };
    if meta.len() > LAYOUT_MAX_BYTES {
        return Err(format!("{} is larger than 1 MiB", path.display()));
    }
    std::fs::read_to_string(&path).map(Some).map_err(|error| error.to_string())
}

/// Set the layout file aside once applied (or declined for good), so it is
/// never offered twice.
#[tauri::command]
pub fn georgi_finish_layout(app: tauri::AppHandle, outcome: String) -> Result<(), String> {
    let path = layout_path(&app)?;
    let tag: String = outcome.chars().filter(|c| c.is_ascii_alphanumeric()).take(16).collect();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let done = path.with_file_name(format!("georgi-layout.{tag}-{stamp}.json"));
    std::fs::rename(&path, &done).map_err(|error| error.to_string())
}

/// Flash PRIM-1's taskbar button when an agent asks the operator for
/// something (@Georgi) while the window is in the background. Windows stops
/// the flash when the window is focused.
#[tauri::command]
pub fn georgi_request_attention(window: tauri::WebviewWindow) -> Result<(), String> {
    if window.is_focused().unwrap_or(false) {
        return Ok(());
    }
    window
        .request_user_attention(Some(tauri::UserAttentionType::Informational))
        .map_err(|error| error.to_string())
}

/// How full each listed agent's context window is (Georgi chat gauge).
#[tauri::command]
pub async fn georgi_context_usage(
    state: State<'_, crate::DesktopState>,
    session_ids: Vec<SessionId>,
) -> Result<Vec<supervisor::ContextUsage>, String> {
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.context_usage(&session_ids))
        .await
        .map_err(|error| error.to_string())
}
