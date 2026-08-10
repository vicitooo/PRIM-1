use std::{
    collections::HashMap,
    ffi::OsString,
    fs::OpenOptions,
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::mpsc,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use shared_types::{
    CreatePairRequest, DeletePairRequest, RenamePairRequest, RestartSessionRequest,
    RouteMessageRequest, RuntimeSnapshot, SendInputRequest, SessionSnapshot, StartSessionRequest,
    StopSessionRequest,
};
use supervisor::{
    RendererEventProjector, RendererOutputRedactor, SupervisorConfig, SupervisorHandle,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};

#[derive(Clone)]
struct DesktopDiagnostics {
    path: Arc<OnceLock<PathBuf>>,
}

impl DesktopDiagnostics {
    fn uninitialized() -> Self {
        Self {
            path: Arc::new(OnceLock::new()),
        }
    }

    fn initialize(&self, runtime_dir: &Path) -> Result<(), String> {
        self.path
            .set(runtime_dir.join("desktop-events.jsonl"))
            .map_err(|_| "desktop diagnostics already initialized".to_string())
    }

    fn log(&self, level: &str, event: &str, message: impl Into<String>) {
        let entry = serde_json::json!({
            "timestamp": shared_types::now_rfc3339(),
            "pid": std::process::id(),
            "level": level,
            "event": event,
            "message": message.into(),
        });

        let Some(path) = self.path.get() else {
            eprintln!("{level} {event}: {}", entry["message"]);
            return;
        };

        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{entry}");
        }
    }
}

struct DesktopState {
    supervisor: SupervisorHandle,
    diagnostics: DesktopDiagnostics,
    shutdown_started: AtomicBool,
}

impl DesktopState {
    fn shutdown_once(&self, reason: &str) {
        if self
            .shutdown_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.diagnostics
            .log("info", "shutdown_started", format!("reason={reason}"));
        match self.supervisor.shutdown() {
            Ok(()) => self
                .diagnostics
                .log("info", "shutdown_complete", format!("reason={reason}")),
            Err(error) => self.diagnostics.log(
                "error",
                "shutdown_failed",
                format!("reason={reason}: {error:#}"),
            ),
        }
    }
}

#[derive(Debug, Clone)]
struct PendingSessionOutput {
    chunk: String,
    synthetic: bool,
    timestamp: String,
}

#[derive(Default)]
struct TerminalOutputSanitizer {
    state: TerminalControlState,
}

#[derive(Default)]
enum TerminalControlState {
    #[default]
    Normal,
    Escape,
    Osc,
    OscEscape,
}

struct RendererTerminalStream {
    sanitizer: TerminalOutputSanitizer,
    redactor: RendererOutputRedactor,
    last_synthetic: bool,
    last_timestamp: String,
}

impl RendererTerminalStream {
    fn new(redactor: RendererOutputRedactor) -> Self {
        Self {
            sanitizer: TerminalOutputSanitizer::default(),
            redactor,
            last_synthetic: false,
            last_timestamp: String::new(),
        }
    }

    fn push(&mut self, chunk: &str, synthetic: bool, timestamp: &str) -> String {
        self.last_synthetic = synthetic;
        self.last_timestamp = timestamp.to_string();
        let sanitized = self.sanitizer.push(chunk);
        self.redactor.push(&sanitized)
    }

    fn finish(mut self) -> PendingSessionOutput {
        let sanitized_tail = self.sanitizer.finish();
        let mut chunk = self.redactor.push(&sanitized_tail);
        chunk.push_str(&self.redactor.finish());
        PendingSessionOutput {
            chunk,
            synthetic: self.last_synthetic,
            timestamp: self.last_timestamp,
        }
    }
}

impl TerminalOutputSanitizer {
    fn push(&mut self, input: &str) -> String {
        let mut output = Vec::with_capacity(input.len());
        for byte in input.bytes() {
            match self.state {
                TerminalControlState::Normal if byte == 0x1b => {
                    self.state = TerminalControlState::Escape;
                }
                TerminalControlState::Normal => output.push(byte),
                TerminalControlState::Escape if byte == b']' => {
                    self.state = TerminalControlState::Osc;
                }
                TerminalControlState::Escape if byte == 0x1b => {
                    output.push(0x1b);
                }
                TerminalControlState::Escape => {
                    output.extend([0x1b, byte]);
                    self.state = TerminalControlState::Normal;
                }
                TerminalControlState::Osc if byte == 0x07 => {
                    self.state = TerminalControlState::Normal;
                }
                TerminalControlState::Osc if byte == 0x1b => {
                    self.state = TerminalControlState::OscEscape;
                }
                TerminalControlState::Osc => {}
                TerminalControlState::OscEscape if byte == b'\\' => {
                    self.state = TerminalControlState::Normal;
                }
                TerminalControlState::OscEscape if byte == 0x1b => {}
                TerminalControlState::OscEscape => {
                    self.state = TerminalControlState::Osc;
                }
            }
        }
        String::from_utf8_lossy(&output).into_owned()
    }

    fn finish(mut self) -> String {
        if matches!(self.state, TerminalControlState::Escape) {
            self.state = TerminalControlState::Normal;
            "\x1b".into()
        } else {
            String::new()
        }
    }
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

#[derive(Clone)]
pub struct StartupConfig {
    agent_working_root: PathBuf,
    environment_source: Option<PathBuf>,
    cdp_port: Option<u16>,
}

impl StartupConfig {
    pub fn cdp_port(&self) -> Option<u16> {
        self.cdp_port
    }
}

pub fn load_startup_config() -> Result<StartupConfig, String> {
    let startup_cwd = std::env::current_dir()
        .map_err(|error| format!("failed to resolve startup working directory: {error}"))?;
    let executable_dir = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));

    let environment_source = resolve_environment_source(
        &startup_cwd,
        executable_dir.as_deref(),
        std::env::var_os("PRIM1_ENV_FILE"),
    )?;
    if let Some(source) = environment_source.as_ref() {
        dotenvy::from_path(source).map_err(|error| {
            format!(
                "failed to load runtime environment {}: {error}",
                source.display()
            )
        })?;
    }

    let agent_working_root = resolve_agent_working_root_from(
        std::env::var_os("PRIM1_AGENT_WORKING_ROOT"),
        &startup_cwd,
    )?;
    let cdp_port = parse_cdp_port(std::env::var_os("PRIM1_CDP_PORT"))?;

    Ok(StartupConfig {
        agent_working_root,
        environment_source,
        cdp_port,
    })
}

fn non_empty_path(value: Option<OsString>) -> Option<PathBuf> {
    value.and_then(|value| {
        (!value.to_string_lossy().trim().is_empty()).then(|| PathBuf::from(value))
    })
}

fn resolve_environment_source(
    startup_cwd: &Path,
    executable_dir: Option<&Path>,
    explicit: Option<OsString>,
) -> Result<Option<PathBuf>, String> {
    if let Some(explicit) = non_empty_path(explicit) {
        let explicit = resolve_against(&explicit, startup_cwd);
        if !explicit.is_file() {
            return Err(format!(
                "explicit PRIM1_ENV_FILE is not a file: {}",
                explicit.display()
            ));
        }
        return Ok(Some(explicit));
    }

    let mut candidates = Vec::new();
    for directory in [Some(startup_cwd), executable_dir].into_iter().flatten() {
        let candidate = directory.join(".env");
        if !candidates.contains(&candidate) {
            candidates.push(candidate);
        }
    }
    Ok(candidates.into_iter().find(|candidate| candidate.is_file()))
}

fn resolve_against(path: &Path, base: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

fn resolve_agent_working_root_from(
    env_root: Option<OsString>,
    startup_cwd: &Path,
) -> Result<PathBuf, String> {
    let selected = non_empty_path(env_root)
        .map(|path| resolve_against(&path, startup_cwd))
        .unwrap_or_else(|| startup_cwd.to_path_buf());
    if !selected.is_dir() {
        return Err(format!(
            "agent working root is not a directory: {}",
            selected.display()
        ));
    }
    let canonical = selected.canonicalize().map_err(|error| {
        format!(
            "failed to canonicalize agent working root {}: {error}",
            selected.display()
        )
    })?;
    Ok(normalize_path_for_child_processes(canonical))
}

fn parse_cdp_port(value: Option<OsString>) -> Result<Option<u16>, String> {
    let Some(value) = value.filter(|value| !value.to_string_lossy().trim().is_empty()) else {
        return Ok(None);
    };
    let text = value.to_string_lossy();
    let port = text
        .parse::<u16>()
        .map_err(|_| "PRIM1_CDP_PORT must be a non-zero TCP port".to_string())?;
    if port == 0 {
        return Err("PRIM1_CDP_PORT must be a non-zero TCP port".into());
    }
    Ok(Some(port))
}

fn resolve_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|path| path.join("runtime"))
        .map_err(|error| format!("failed to resolve per-user application data: {error}"))
}

fn ensure_runtime_is_outside_working_root(
    runtime_dir: &Path,
    working_root: &Path,
) -> Result<(), String> {
    let runtime = path_for_overlap_check(runtime_dir);
    let working = path_for_overlap_check(working_root);
    if runtime.starts_with(&working) || working.starts_with(&runtime) {
        return Err(format!(
            "runtime storage and agent working root must be disjoint (runtime={}, working_root={})",
            runtime_dir.display(),
            working_root.display()
        ));
    }
    Ok(())
}

fn path_for_overlap_check(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_ascii_lowercase())
    }

    #[cfg(not(windows))]
    path.to_path_buf()
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
    runtime_dir: PathBuf,
    agent_working_root: PathBuf,
    diagnostics: &DesktopDiagnostics,
) -> Result<SupervisorHandle, String> {
    ensure_runtime_is_outside_working_root(&runtime_dir, &agent_working_root)?;
    let cross_pair_room_broadcast = cross_pair_room_broadcast_from_env();
    let supervisor = SupervisorHandle::new(SupervisorConfig {
        working_root: agent_working_root,
        runtime_dir,
        cross_pair_room_broadcast,
        heartbeat_interval: None,
        auto_restart_on_stall_sessions: None,
        auto_restart_stall_threshold: None,
        reaction_window: None,
    })
    .map_err(|error| error.to_string())?;
    diagnostics.initialize(supervisor.runtime_dir())?;
    diagnostics.log(
        "info",
        "supervisor_init",
        format!("storage=per_user_app_data cross_pair_room_broadcast={cross_pair_room_broadcast}"),
    );

    let ui_event_tx = start_ui_event_bridge(
        app.clone(),
        diagnostics.clone(),
        supervisor.renderer_event_projector(),
    );
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
    projector: RendererEventProjector,
) -> mpsc::Sender<shared_types::RuntimeEvent> {
    let (tx, rx) = mpsc::channel::<shared_types::RuntimeEvent>();

    thread::spawn(move || {
        let mut pending = HashMap::<String, PendingSessionOutput>::new();
        let mut terminal_streams = HashMap::<String, RendererTerminalStream>::new();

        loop {
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(event) => match event {
                    shared_types::RuntimeEvent::SessionOutput {
                        session,
                        chunk,
                        synthetic,
                        timestamp,
                    } => {
                        let filtered = terminal_streams
                            .entry(session.clone())
                            .or_insert_with(|| {
                                RendererTerminalStream::new(projector.output_redactor())
                            })
                            .push(&chunk, synthetic, &timestamp);
                        if filtered.is_empty() {
                            continue;
                        }

                        queue_pending_session_output(
                            &mut pending,
                            session,
                            PendingSessionOutput {
                                chunk: filtered,
                                synthetic,
                                timestamp,
                            },
                        );
                    }
                    event => {
                        if let shared_types::RuntimeEvent::SessionState {
                            session,
                            state:
                                shared_types::LifecycleState::Closed
                                | shared_types::LifecycleState::Failed,
                            ..
                        } = &event
                            && let Some(stream) = terminal_streams.remove(session)
                        {
                            queue_pending_session_output(
                                &mut pending,
                                session.clone(),
                                stream.finish(),
                            );
                        }
                        flush_pending_session_output(&app, &diagnostics, &mut pending);
                        emit_runtime_event(&app, &diagnostics, projector.project(event));
                    }
                },
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    flush_pending_session_output(&app, &diagnostics, &mut pending);
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    for (session, stream) in terminal_streams.drain() {
                        queue_pending_session_output(&mut pending, session, stream.finish());
                    }
                    flush_pending_session_output(&app, &diagnostics, &mut pending);
                    break;
                }
            }
        }
    });

    tx
}

fn queue_pending_session_output(
    pending: &mut HashMap<String, PendingSessionOutput>,
    session: String,
    output: PendingSessionOutput,
) {
    if output.chunk.is_empty() {
        return;
    }
    pending
        .entry(session)
        .and_modify(|buffer| {
            buffer.chunk.push_str(&output.chunk);
            buffer.synthetic &= output.synthetic;
            buffer.timestamp = output.timestamp.clone();
        })
        .or_insert(output);
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

#[cfg(test)]
fn sanitize_terminal_output_for_ui(chunk: &str) -> String {
    let mut sanitizer = TerminalOutputSanitizer::default();
    let mut output = sanitizer.push(chunk);
    output.push_str(&sanitizer.finish());
    output
}

#[cfg(test)]
fn strip_osc_sequences(input: &str) -> String {
    sanitize_terminal_output_for_ui(input)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run(startup: StartupConfig) {
    let diagnostics = DesktopDiagnostics::uninitialized();
    let setup_diagnostics = diagnostics.clone();
    let setup_startup = startup.clone();

    let app = tauri::Builder::default()
        .setup(move |app| {
            let runtime_dir = resolve_runtime_dir(app.handle())?;
            let supervisor = init_supervisor(
                app.handle(),
                runtime_dir,
                setup_startup.agent_working_root.clone(),
                &setup_diagnostics,
            )?;
            install_panic_hook(setup_diagnostics.clone());
            setup_diagnostics.log(
                "info",
                "process_start",
                format!(
                    "desktop process booting; environment_file_loaded={}",
                    setup_startup.environment_source.is_some()
                ),
            );
            setup_diagnostics.log("info", "tauri_setup", "starting setup");
            log_frontend_bundle_id(&setup_diagnostics);
            app.manage(DesktopState {
                supervisor,
                diagnostics: setup_diagnostics.clone(),
                shutdown_started: AtomicBool::new(false),
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
        .build(tauri::generate_context!())
        .unwrap_or_else(|error| panic!("error while building tauri application: {error}"));

    app.run(|handle, event| match event {
        tauri::RunEvent::ExitRequested { .. } => {
            if let Some(state) = handle.try_state::<DesktopState>() {
                state.shutdown_once("app_exit_requested");
            }
        }
        tauri::RunEvent::Exit => {
            if let Some(state) = handle.try_state::<DesktopState>() {
                state
                    .diagnostics
                    .log("info", "process_exit", "desktop process exiting");
            }
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::{
        RendererTerminalStream, TerminalOutputSanitizer, cross_pair_room_broadcast_from_env,
        ensure_runtime_is_outside_working_root, normalize_path_for_child_processes, parse_cdp_port,
        resolve_agent_working_root_from, resolve_environment_source,
        sanitize_terminal_output_for_ui, strip_osc_sequences,
    };
    use std::{
        ffi::OsString,
        fs,
        path::PathBuf,
        sync::Mutex,
        time::{SystemTime, UNIX_EPOCH},
    };
    use supervisor::{SupervisorConfig, SupervisorHandle};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

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

    #[test]
    fn terminal_output_sanitizer_removes_osc_split_across_chunks() {
        let mut sanitizer = TerminalOutputSanitizer::default();
        let output = [
            sanitizer.push("token-prefix\x1b"),
            sanitizer.push("]0;hidden title"),
            sanitizer.push("\x07-token-suffix"),
            sanitizer.finish(),
        ]
        .concat();

        assert_eq!(output, "token-prefix-token-suffix");
    }

    #[test]
    fn renderer_terminal_stream_redacts_after_osc_and_across_every_chunk_boundary() {
        let runtime_dir = unique_temp_dir("renderer-stream");
        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root: std::env::current_dir().unwrap(),
            runtime_dir,
            cross_pair_room_broadcast: false,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
            reaction_window: None,
        })
        .unwrap();
        let status = supervisor.start_control_plane().unwrap();
        let projector = supervisor.renderer_event_projector();

        for secret in [status.token.clone(), status.info_path.clone()] {
            let mut one_chunk = RendererTerminalStream::new(projector.output_redactor());
            let output = [
                one_chunk.push(&secret, false, "one"),
                one_chunk.finish().chunk,
            ]
            .concat();
            assert_eq!(output, "[redacted]");
            assert!(!output.contains(&secret));

            for split in secret
                .char_indices()
                .map(|(index, _)| index)
                .filter(|index| *index > 0)
            {
                let mut stream = RendererTerminalStream::new(projector.output_redactor());
                let output = [
                    stream.push(&secret[..split], false, "first"),
                    stream.push(&secret[split..], false, "second"),
                    stream.finish().chunk,
                ]
                .concat();
                assert_eq!(output, "[redacted]", "wrong output at split {split}");
                assert!(!output.contains(&secret), "secret leaked at split {split}");

                let mut ansi_stream = RendererTerminalStream::new(projector.output_redactor());
                let output = [
                    ansi_stream.push(
                        &format!("{}\x1b[31m{}", &secret[..split], &secret[split..]),
                        false,
                        "ansi",
                    ),
                    ansi_stream.finish().chunk,
                ]
                .concat();
                assert_eq!(
                    output, "[redacted]",
                    "ANSI-interleaved secret leaked at split {split}"
                );
            }
        }

        let split = status.token.len() / 2;
        let osc_interleaved = format!(
            "{}\x1b]0;hidden credential boundary\x07{}",
            &status.token[..split],
            &status.token[split..]
        );
        let mut osc_stream = RendererTerminalStream::new(projector.output_redactor());
        let output = [
            osc_stream.push(&osc_interleaved, false, "osc"),
            osc_stream.finish().chunk,
        ]
        .concat();
        assert_eq!(output, "[redacted]");
        assert!(!output.contains(&status.token));

        for control_wrapped in [
            format!("\x1b]0;{}\x07ordinary", status.token),
            format!("\x1bP{}\x1b\\ordinary", status.info_path),
        ] {
            let mut control_stream = RendererTerminalStream::new(projector.output_redactor());
            let output = [
                control_stream.push(&control_wrapped, false, "control"),
                control_stream.finish().chunk,
            ]
            .concat();
            assert!(!output.contains(&status.token));
            assert!(!output.contains(&status.info_path));
        }

        let mut delayed = RendererTerminalStream::new(projector.output_redactor());
        assert!(
            delayed
                .push(&status.token[..split], false, "before-revocation")
                .is_empty()
        );
        supervisor.shutdown().unwrap();
        let output = [
            delayed.push(&status.token[split..], false, "after-revocation"),
            delayed.finish().chunk,
        ]
        .concat();
        assert_eq!(output, "[redacted]");
        assert!(!output.contains(&status.token));
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_for_child_processes_strips_verbatim_drive_prefix() {
        let path = PathBuf::from(r"\\?\C:\Users\example\projects\prim1");

        assert_eq!(
            normalize_path_for_child_processes(path),
            PathBuf::from(r"C:\Users\example\projects\prim1")
        );
    }

    #[cfg(windows)]
    #[test]
    fn normalize_path_for_child_processes_strips_verbatim_unc_prefix() {
        let path = PathBuf::from(r"\\?\UNC\server\share\prim1");

        assert_eq!(
            normalize_path_for_child_processes(path),
            PathBuf::from(r"\\server\share\prim1")
        );
    }

    #[test]
    fn resolve_agent_working_root_uses_runtime_cwd_when_env_unset() {
        let current = std::env::current_dir().unwrap();
        let resolved = resolve_agent_working_root_from(None, &current).unwrap();

        assert_eq!(
            resolved,
            normalize_path_for_child_processes(current.canonicalize().unwrap())
        );
    }

    #[test]
    fn resolve_agent_working_root_uses_relative_env_var_when_set() {
        let current = unique_temp_dir("relative-working-root");
        let expected = current.join("relative child");
        fs::create_dir_all(&expected).unwrap();
        let resolved =
            resolve_agent_working_root_from(Some(OsString::from("relative child")), &current)
                .unwrap();

        assert_eq!(
            resolved,
            normalize_path_for_child_processes(expected.canonicalize().unwrap())
        );
        fs::remove_dir_all(current).unwrap();
    }

    #[test]
    fn resolve_agent_working_root_ignores_empty_env_var() {
        let current = std::env::current_dir().unwrap();
        let resolved =
            resolve_agent_working_root_from(Some(OsString::from("   ")), &current).unwrap();
        assert_eq!(
            resolved,
            normalize_path_for_child_processes(current.canonicalize().unwrap())
        );
    }

    #[test]
    fn resolve_agent_working_root_rejects_missing_directory() {
        let current = std::env::current_dir().unwrap();
        let missing = format!("missing-{}", unique_suffix());

        let error =
            resolve_agent_working_root_from(Some(OsString::from(missing)), &current).unwrap_err();

        assert!(error.contains("is not a directory"));
    }

    #[test]
    fn runtime_environment_precedence_is_explicit_then_cwd_then_executable() {
        let root = unique_temp_dir("environment-precedence");
        let cwd = root.join("cwd");
        let exe = root.join("exe");
        fs::create_dir_all(&cwd).unwrap();
        fs::create_dir_all(&exe).unwrap();
        fs::write(cwd.join(".env"), "SOURCE=cwd\n").unwrap();
        fs::write(exe.join(".env"), "SOURCE=exe\n").unwrap();
        let explicit = root.join("explicit.env");
        fs::write(&explicit, "SOURCE=explicit\n").unwrap();

        assert_eq!(
            resolve_environment_source(&cwd, Some(&exe), Some(explicit.clone().into_os_string()))
                .unwrap(),
            Some(explicit)
        );
        assert_eq!(
            resolve_environment_source(&cwd, Some(&exe), None).unwrap(),
            Some(cwd.join(".env"))
        );
        fs::remove_file(cwd.join(".env")).unwrap();
        assert_eq!(
            resolve_environment_source(&cwd, Some(&exe), None).unwrap(),
            Some(exe.join(".env"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_runtime_environment_must_be_a_file() {
        let cwd = std::env::current_dir().unwrap();
        let error = resolve_environment_source(
            &cwd,
            None,
            Some(OsString::from(format!("missing-{}.env", unique_suffix()))),
        )
        .unwrap_err();

        assert!(error.contains("explicit PRIM1_ENV_FILE is not a file"));
    }

    #[test]
    fn cdp_port_parser_rejects_flag_injection_and_zero() {
        assert_eq!(parse_cdp_port(None).unwrap(), None);
        assert_eq!(
            parse_cdp_port(Some(OsString::from("9222"))).unwrap(),
            Some(9222)
        );
        assert!(parse_cdp_port(Some(OsString::from("0"))).is_err());
        assert!(
            parse_cdp_port(Some(OsString::from(
                "9222 --remote-debugging-address=0.0.0.0"
            )))
            .is_err()
        );
    }

    #[test]
    fn runtime_and_working_root_must_be_disjoint() {
        let root = std::env::current_dir().unwrap();
        assert!(ensure_runtime_is_outside_working_root(&root.join("runtime"), &root).is_err());
        assert!(ensure_runtime_is_outside_working_root(&root, &root.join("runtime")).is_err());
        assert!(
            ensure_runtime_is_outside_working_root(&root.join("runtime"), &root.join("other"))
                .is_ok()
        );
    }

    fn unique_suffix() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("prim1-desktop-{label}-{}", unique_suffix()))
    }

    #[test]
    fn cross_pair_room_broadcast_defaults_to_false() {
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
        let _guard = ENV_LOCK.lock().unwrap();
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
}
