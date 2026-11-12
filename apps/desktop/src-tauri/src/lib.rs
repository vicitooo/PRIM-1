use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    ffi::OsString,
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use shared_types::{
    AddRoomMemberRequest, BriefRoomMemberRequest, ChooseSessionWorkingDirectoryRequest,
    CreateRoomRequest, CreateSessionRequest, DeleteRoomRequest, DeleteSessionRequest,
    DeliverRoomMessageRequest, InviteRoomGuestRequest, ListRoomGuestsRequest, MoveRoomRequest,
    MoveSessionRequest, OperatorRouteMessageRequest, RevokeRoomGuestRequest, RoomGuest,
    PostRoomMessageRequest, ReadRoomFeedRequest, ReadRoomBriefRequest, RoomBriefSettings,
    UpdateRoomBriefRequest, RemoveRoomMemberRequest, RenameRoomRequest,
    RenameSessionRequest, RestartSessionRequest, RetryRoomDeliveryRequest, RoomDeliveryResult, RoomFeedPage, RoomPostResult,
    RoomExportReport, RoomId, RoomImportReport, RoomSourceSummary,
    RoomSnapshot, RoomFallbackSnapshot, SetRoomFallbacksRequest,
    RunEventIdentity, RunId, RuntimeSnapshot, SendInputRequest, SessionGeneration,
    SessionId, SessionSnapshot,
    SetRoomResumeRequest, SetSessionLinuxWorkingDirectoryRequest, SetSessionPermissionRequest,
    StartSessionRequest, StopSessionRequest, SessionLaunchPreview, TelemetrySnapshot,
    MemberPermissions, SetMemberPermissionsRequest,
};
use supervisor::{
    PersonalRepoStatus, RendererEventProjector, SupervisorConfig, SupervisorHandle, TaskFolderStatus,
    default_room_brief_template, validate_runtime_storage_paths,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::{DialogExt, MessageDialogButtons, MessageDialogKind};
use supervisor::output_credit::{OutputCredit, OutputCreditLedger, OutputCreditMetrics, RendererReset, TOTAL_ITEMS};
use supervisor::trial::{TRIAL_ADMISSION_HELD, TrialChallenge, TrialHealthReceipt, TrialStartup};
use supervisor::upgrade_flow::{UpgradeEnvironment, UpgradeHost, UpgradeStartup};
mod attachments;
mod georgi;
mod trial_desktop;
mod upgrade_launcher;
pub use trial_desktop::WEBVIEW_PROFILE_ENV;

const DESKTOP_INSTANCE_LOCK_FILE: &str = "desktop-instance.lock";
const DESKTOP_INSTANCE_ALREADY_RUNNING: &str = "PRIM-1 is already running for this user";
// The bridge queue bounds event count, not exact heap bytes: non-output event
// strings vary. SessionOutput ingress is bounded upstream to at most 12 KiB.
const UI_EVENT_QUEUE_CAPACITY: usize = 512;
// The renderer retains at most 512 output events. A 32 KiB emitted-event cap
// bounds retained terminal payload to roughly 16 MiB plus object overhead.
const MAX_UI_OUTPUT_BATCH_BYTES: usize = 32 * 1024;
const MAX_UI_OUTPUT_BATCH_AGE: Duration = Duration::from_millis(16);

#[derive(Debug)]
struct DesktopInstanceLock {
    _file: File,
}

impl DesktopInstanceLock {
    fn acquire(runtime_dir: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(runtime_dir).map_err(|error| {
            format!(
                "failed to create PRIM-1 runtime directory {}: {error}",
                runtime_dir.display()
            )
        })?;
        let path = runtime_dir.join(DESKTOP_INSTANCE_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|error| {
                format!(
                    "failed to open PRIM-1 desktop instance lock {}: {error}",
                    path.display()
                )
            })?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(DESKTOP_INSTANCE_ALREADY_RUNNING.into()),
            Err(TryLockError::Error(error)) => Err(format!(
                "failed to acquire PRIM-1 desktop instance lock {}: {error}",
                path.display()
            )),
        }
    }
}

#[derive(Clone)]
struct DesktopDiagnostics {
    path: Arc<OnceLock<PathBuf>>,
    writer: Arc<Mutex<()>>,
}

impl DesktopDiagnostics {
    fn uninitialized() -> Self {
        Self {
            path: Arc::new(OnceLock::new()),
            writer: Arc::new(Mutex::new(())),
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

        let _writer = self
            .writer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{entry}");
        }
    }
}

struct DesktopState {
    supervisor: SupervisorHandle,
    trial: Option<Arc<TrialStartup>>,
    diagnostics: DesktopDiagnostics,
    automation_mode: bool,
    shutdown_started: AtomicBool,
    shutdown_succeeded: AtomicBool,
    fullscreen_on_first_focus: AtomicBool,
    /// T-36: the main window's configuration, to rebuild it after its WebView2
    /// browser process exited. None in a held trial, which is never rebuilt.
    main_window_config: Option<tauri::utils::config::WindowConfig>,
    /// T-36: the main window is being rebuilt, so the moment without a window
    /// is not an exit request.
    ui_rebuilding: AtomicBool,
    /// T-36: the rebuilt window opens minimized when the old one was.
    rebuild_minimized: AtomicBool,
    renderer_recovery: Mutex<RendererRecoveryLedger>,
    _instance_lock: DesktopInstanceLock,
}

impl DesktopState {
    fn shutdown_once(&self, reason: &str) -> bool {
        if self
            .shutdown_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return self.shutdown_succeeded.load(Ordering::SeqCst);
        }
        self.run_shutdown(reason, false).is_ok()
    }

    /// A108: the same single shutdown for an upgrade close, strict about the
    /// final session-catalog write. It cannot vouch for a shutdown that some
    /// other path already started.
    fn shutdown_for_upgrade(&self) -> Result<(), String> {
        if self
            .shutdown_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err("PRIM-1 is already shutting down".into());
        }
        self.run_shutdown("upgrade", true)
    }

    /// A16: tao turns WM_ENDSESSION straight into `RunEvent::Exit`, with no
    /// close or exit request first, and Windows may end this process as soon
    /// as that returns. Unless an ordinary shutdown already ran, a session end
    /// gets the supervisor's `end_session_now`: the catalog checkpoint first,
    /// nothing waited for.
    fn end_session_unless_shut_down(&self) {
        if self
            .shutdown_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }
        self.diagnostics.log(
            "info",
            "session_end_started",
            "exit without a close or exit request: Windows is ending the session",
        );
        match self.supervisor.end_session_now() {
            Ok(()) => self.diagnostics.log(
                "info",
                "session_end_checkpoint",
                "running members saved for Resume on restart; agents told to end",
            ),
            Err(error) => self.diagnostics.log("error", "session_end_checkpoint_failed", format!("{error:#}")),
        }
    }

    fn run_shutdown(&self, reason: &str, strict: bool) -> Result<(), String> {
        self.diagnostics
            .log("info", "shutdown_started", format!("reason={reason}"));
        let outcome = if strict {
            self.supervisor.shutdown_for_upgrade()
        } else {
            self.supervisor.shutdown()
        };
        match outcome {
            Ok(()) => {
                self.shutdown_succeeded.store(true, Ordering::SeqCst);
                self.diagnostics
                    .log("info", "shutdown_complete", format!("reason={reason}"));
                Ok(())
            }
            Err(error) => {
                self.diagnostics.log(
                    "error",
                    "shutdown_failed",
                    format!("reason={reason}: {error:#}"),
                );
                Err(format!("{error:#}"))
            }
        }
    }
}

/// What the desktop does for a self-upgrade (A108): its own strict close,
/// process exit, and the stable launcher after the new build is shown.
struct DesktopUpgradeHost {
    app: AppHandle,
    diagnostics: DesktopDiagnostics,
    runtime_dir: PathBuf,
}

impl UpgradeHost for DesktopUpgradeHost {
    fn strict_shutdown(&self) -> anyhow::Result<()> {
        let state = self
            .app
            .try_state::<DesktopState>()
            .ok_or_else(|| anyhow::anyhow!("the desktop state is not available"))?;
        state.shutdown_for_upgrade().map_err(anyhow::Error::msg)
    }

    fn exit_for_upgrade(&self) {
        self.diagnostics.log(
            "info",
            "upgrade_exit",
            "clean shutdown recorded; exiting so the new build can start",
        );
        self.app.exit(0);
    }

    fn resume_consumed(&self, record: &supervisor::upgrade::UpgradeRecord) {
        let runtime_dir = self.runtime_dir.clone();
        let source = record.request.source.exe_path.clone();
        let candidate = record.request.candidate.exe_path.clone();
        let sha256 = record.request.candidate.exe_sha256.clone();
        let app = self.app.clone();
        let diagnostics = self.diagnostics.clone();
        let spawned = thread::Builder::new()
            .name("prim1-upgrade-launcher".into())
            .spawn(move || {
                let (level, message) = match upgrade_launcher::follow_new_build(
                    &runtime_dir,
                    &source,
                    &candidate,
                    &sha256,
                ) {
                    Ok(done) => (shared_types::LogLevel::Info, format!("Desktop launcher: {done}.")),
                    Err(error) => (
                        shared_types::LogLevel::Warn,
                        format!("Desktop launcher unchanged: {error}"),
                    ),
                };
                diagnostics.log("info", "upgrade_launcher", message.clone());
                emit_runtime_event(
                    &app,
                    &diagnostics,
                    shared_types::RuntimeEvent::SystemLog {
                        level,
                        message,
                        timestamp: shared_types::now_rfc3339(),
                    },
                );
            });
        if let Err(error) = spawned {
            self.diagnostics
                .log("warn", "upgrade_launcher_unchanged", error.to_string());
        }
    }
}

/// The WebView2 user data folder this process uses: the environment's
/// override, else Tauri's per-user application folder.
fn effective_webview_profile(app: &AppHandle) -> Result<PathBuf, String> {
    match std::env::var_os(WEBVIEW_PROFILE_ENV).filter(|value| !value.is_empty()) {
        Some(folder) => {
            let folder = PathBuf::from(folder);
            if folder.is_absolute() {
                Ok(folder)
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(folder))
                    .map_err(|error| error.to_string())
            }
        }
        None => app
            .path()
            .app_local_data_dir()
            .map_err(|error| error.to_string()),
    }
}

/// Not for a held trial. An unavailable upgrade store leaves PRIM-1 working.
fn install_upgrade(app: &AppHandle, diagnostics: &DesktopDiagnostics, startup: &StartupConfig) {
    let Some(state) = app.try_state::<DesktopState>() else {
        return;
    };
    let profile_dir = match effective_webview_profile(app) {
        Ok(profile_dir) => profile_dir,
        Err(error) => {
            diagnostics.log("warn", "upgrade_unavailable", format!("profile: {error}"));
            return;
        }
    };
    let host = Arc::new(DesktopUpgradeHost {
        app: app.clone(),
        diagnostics: diagnostics.clone(),
        runtime_dir: state.supervisor.runtime_dir().to_path_buf(),
    });
    let environment = UpgradeEnvironment {
        profile_dir,
        personal_repo_override: startup.personal_repo_override.clone(),
        startup: startup.upgrade_startup.clone(),
    };
    match state.supervisor.install_upgrade(environment, host) {
        Ok(()) => diagnostics.log(
            "info",
            "upgrade_ready",
            format!(
                "stage={:?}",
                state.supervisor.upgrade_status().map(|status| status.stage)
            ),
        ),
        Err(error) => diagnostics.log("info", "upgrade_unavailable", format!("{error:#}")),
    }
}

/// T-36 (Victor, room 3073: PC3's PRIM-1 window went black while its agents
/// kept working). WebView2 reports a failed process through ProcessFailed,
/// which neither Tauri nor wry handles, so a renderer or browser process that
/// died left the window black for good. The agents, rooms and their state live
/// in this process, not in the page, so reloading the page or rebuilding the
/// window recovers the UI without touching them; the page reattaches through
/// the same renderer epoch as a manual reload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererFailure {
    /// The WebView2 browser process exited: the WebView is closed for good.
    BrowserExited,
    /// The page's render process exited: the WebView survives a reload.
    RendererExited,
    /// The page's render process stopped responding but is alive.
    RendererUnresponsive,
    /// GPU, utility, frame and other helpers, which WebView2 restarts itself.
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RendererRecovery {
    Rebuild,
    Reload,
    /// Log only: the failure recovers by itself, or reloading would discard a
    /// page that may still answer.
    Wait,
    /// The recovery budget is spent, or the window cannot be rebuilt: log an
    /// error and leave the window as it is. Closing it still shuts down
    /// gracefully.
    GiveUp,
}

/// At most three recoveries in any ten minutes, so a renderer that dies on
/// every load cannot spin.
const RENDERER_RECOVERY_LIMIT: usize = 3;
const RENDERER_RECOVERY_WINDOW: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Default)]
struct RendererRecoveryLedger {
    recent: VecDeque<Instant>,
}

impl RendererRecoveryLedger {
    fn decide(&mut self, failure: RendererFailure, rebuild_allowed: bool, now: Instant) -> RendererRecovery {
        let action = match failure {
            RendererFailure::BrowserExited if rebuild_allowed => RendererRecovery::Rebuild,
            RendererFailure::BrowserExited => return RendererRecovery::GiveUp,
            RendererFailure::RendererExited => RendererRecovery::Reload,
            RendererFailure::RendererUnresponsive | RendererFailure::Other => return RendererRecovery::Wait,
        };
        while self.recent.front().is_some_and(|at| now.duration_since(*at) >= RENDERER_RECOVERY_WINDOW) {
            self.recent.pop_front();
        }
        if self.recent.len() >= RENDERER_RECOVERY_LIMIT {
            return RendererRecovery::GiveUp;
        }
        self.recent.push_back(now);
        action
    }
}

/// T-36: the window-less moment of a rebuild asks to exit with no code; an
/// explicit exit (a code) is never held back.
fn exit_request_is_ui_rebuild(code: Option<i32>, rebuilding: bool) -> bool {
    code.is_none() && rebuilding
}

#[cfg(windows)]
fn renderer_failure(kind: webview2_com::Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND) -> RendererFailure {
    use webview2_com::Microsoft::Web::WebView2::Win32::{
        COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED, COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED,
        COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE,
    };
    if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_BROWSER_PROCESS_EXITED {
        RendererFailure::BrowserExited
    } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_EXITED {
        RendererFailure::RendererExited
    } else if kind == COREWEBVIEW2_PROCESS_FAILED_KIND_RENDER_PROCESS_UNRESPONSIVE {
        RendererFailure::RendererUnresponsive
    } else {
        RendererFailure::Other
    }
}

/// T-36: watch the main WebView's processes. Called for every main window,
/// the first and each rebuilt one.
fn watch_renderer(window: &WebviewWindow, diagnostics: &DesktopDiagnostics) {
    #[cfg(windows)]
    {
        let handle = window.app_handle().clone();
        let log = diagnostics.clone();
        let watched = window.with_webview(move |platform| {
            use webview2_com::{Microsoft::Web::WebView2::Win32::COREWEBVIEW2_PROCESS_FAILED_KIND, ProcessFailedEventHandler};
            // SAFETY: called on the main thread with the live controller Tauri hands over.
            let registered = unsafe { platform.controller().CoreWebView2() }.and_then(|webview| {
                let handler = ProcessFailedEventHandler::create(Box::new(move |_, args| {
                    let Some(args) = args else { return Ok(()) };
                    let mut kind = COREWEBVIEW2_PROCESS_FAILED_KIND::default();
                    // SAFETY: `args` is the event's own argument object.
                    unsafe { args.ProcessFailedKind(&mut kind) }?;
                    on_renderer_failure(&handle, renderer_failure(kind));
                    Ok(())
                }));
                let mut token = 0i64;
                // SAFETY: as above; the handler lives as long as the WebView.
                unsafe { webview.add_ProcessFailed(&handler, &mut token) }
            });
            if let Err(error) = registered {
                log.log("error", "renderer_watch_failed", error.to_string());
            }
        });
        if let Err(error) = watched {
            diagnostics.log("error", "renderer_watch_failed", error.to_string());
        }
    }
    #[cfg(not(windows))]
    let _ = (window, diagnostics);
}

/// T-36: WebView2 raises ProcessFailed on the main thread. A reload runs
/// inside the event, as Microsoft's WebView2 sample does; a rebuild only posts
/// the window's destruction (`destroy` always goes through the event loop), so
/// a WebView is never torn down inside its own callback.
fn on_renderer_failure(handle: &AppHandle, failure: RendererFailure) {
    let Some(state) = handle.try_state::<DesktopState>() else { return };
    if state.shutdown_started.load(Ordering::SeqCst) {
        return;
    }
    let action = state.renderer_recovery.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
        .decide(failure, state.main_window_config.is_some(), Instant::now());
    let level = if action == RendererRecovery::GiveUp { "error" } else { "warn" };
    state.diagnostics.log(level, "renderer_failed", format!("{failure:?}; recovery {action:?}"));
    match action {
        RendererRecovery::Reload => reload_main_window(handle),
        RendererRecovery::Rebuild => begin_main_window_rebuild(handle),
        RendererRecovery::Wait | RendererRecovery::GiveUp => {}
    }
}

fn reload_main_window(handle: &AppHandle) {
    let Some(state) = handle.try_state::<DesktopState>() else { return };
    match handle.get_webview_window("main").map(|window| window.reload()) {
        Some(Ok(())) => state.diagnostics.log("info", "renderer_reloaded", "main page reloaded after its render process exited"),
        Some(Err(error)) => state.diagnostics.log("error", "renderer_recovery_failed", error.to_string()),
        None => state.diagnostics.log("error", "renderer_recovery_failed", "no main window to reload"),
    }
}

/// T-36: a WebView whose browser process exited cannot be revived; the window
/// is destroyed (no close request, so no shutdown) and rebuilt from
/// `RunEvent::WindowEvent` `Destroyed`, which Tauri delivers after releasing
/// the label "main" and without holding its window store.
fn begin_main_window_rebuild(handle: &AppHandle) {
    let Some(state) = handle.try_state::<DesktopState>() else { return };
    let Some(window) = handle.get_webview_window("main") else { return };
    if state.shutdown_started.load(Ordering::SeqCst)
        || state.ui_rebuilding.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err()
    {
        return;
    }
    state.rebuild_minimized.store(window.is_minimized().unwrap_or(false), Ordering::SeqCst);
    if let Err(error) = window.destroy() {
        state.ui_rebuilding.store(false, Ordering::SeqCst);
        state.diagnostics.log("error", "ui_rebuild_failed", error.to_string());
    }
}

fn finish_main_window_rebuild(handle: &AppHandle) {
    let Some(state) = handle.try_state::<DesktopState>() else { return };
    let minimized = state.rebuild_minimized.load(Ordering::SeqCst);
    let built = state.main_window_config.as_ref().ok_or_else(|| "no main window configuration".to_string())
        .and_then(|config| tauri::WebviewWindowBuilder::from_config(handle, config)
            .and_then(|builder| builder.build()).map_err(|error| error.to_string()))
        .and_then(|window| {
            watch_renderer(&window, &state.diagnostics);
            if minimized {
                state.fullscreen_on_first_focus.store(true, Ordering::SeqCst);
            }
            present_main_window(&window, minimized)
        });
    state.ui_rebuilding.store(false, Ordering::SeqCst);
    match built {
        Ok(()) => state.diagnostics.log("info", "ui_rebuilt",
            "main window rebuilt after its WebView2 browser process exited; agents and rooms untouched"),
        Err(error) => {
            state.diagnostics.log("error", "ui_rebuild_failed", error);
            // Without a window the operator has no control left: shut down as a
            // window close does, which records the running members for resume.
            let code = if state.shutdown_once("ui_rebuild_failed") { 0 } else { 1 };
            handle.exit(code);
        }
    }
}

fn close_main_window_gracefully(
    prevent_close: impl FnOnce(),
    shutdown: impl FnOnce() -> bool,
    request_exit: impl FnOnce(i32),
) {
    prevent_close();
    let exit_code = if shutdown() { 0 } else { 1 };
    request_exit(exit_code);
}

#[derive(Debug, Clone)]
struct PendingSessionOutput {
    identity: RunEventIdentity,
    session: String,
    chunk: String,
    synthetic: bool,
    timestamp: String,
    credit: Option<OutputCredit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiOutputGap {
    identity: RunEventIdentity,
    session: String,
    dropped_events: u64,
    observer_only: bool,
}

#[derive(Default)]
struct UiEventIngressMutable {
    terminal_streams: HashMap<RunOutputKey, RendererTerminalStream>,
    output_gaps: BTreeMap<RunOutputKey, UiOutputGap>,
    next_order: u64,
}

#[derive(Default)]
struct UiEventIngressState {
    mutable: Mutex<UiEventIngressMutable>,
}

impl UiEventIngressState {
    fn lock(&self) -> std::sync::MutexGuard<'_, UiEventIngressMutable> {
        self.mutable
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn take_all_output_gaps(&self) -> Vec<UiOutputGap> {
        let mut state = self.lock();
        std::mem::take(&mut state.output_gaps)
            .into_values()
            .collect()
    }

    fn take_reportable_output_gaps(&self, credit: &OutputCreditLedger) -> Vec<UiOutputGap> {
        take_reportable_gaps(&mut self.lock().output_gaps, Some(credit), None)
    }
}

struct QueuedUiEvent {
    event: shared_types::RuntimeEvent,
    output_gaps_before: Vec<UiOutputGap>,
    order: u64,
    credit: Option<OutputCredit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiEventEnqueueResult {
    Enqueued,
    FilteredOutput,
    DroppedOutputFull,
    Disconnected,
}

#[derive(Clone)]
struct UiEventIngress {
    sender: mpsc::SyncSender<QueuedUiEvent>,
    output_sender: mpsc::SyncSender<QueuedUiEvent>,
    state: Arc<UiEventIngressState>,
    credit: Option<Arc<OutputCreditLedger>>,
    wake: Arc<(Mutex<()>, Condvar)>,
}

impl UiEventIngress {
    fn notify(&self) {
        let _guard = self.wake.0.lock().unwrap_or_else(|p| p.into_inner());
        self.wake.1.notify_one();
    }
    fn enqueue(&self, event: shared_types::RuntimeEvent) -> UiEventEnqueueResult {
        let mut state = self.state.lock();
        match event {
            shared_types::RuntimeEvent::SessionOutput {
                identity,
                session,
                chunk,
                synthetic,
                timestamp,
            } => {
                let credit = self.credit.as_ref().and_then(|ledger| ledger.take_published(identity));
                let run_key = RunOutputKey::from(identity);
                let observer_only = self.credit.is_some() && credit.is_none();
                let stream = state
                    .terminal_streams
                    .entry(run_key)
                    .or_insert_with(|| RendererTerminalStream::new(identity, session.clone()));
                let filtered = stream.push(identity, &chunk, synthetic, &timestamp);
                // A page reload starts a new renderer epoch with fresh xterm
                // panes: treat its first output like a gap so the recovery
                // prefix restores the run's tracked modes (review D9C-4).
                if let Some(credit) = &credit {
                    let epoch = credit.epoch.as_u128();
                    if stream.display_epoch.is_some_and(|previous| previous != epoch) {
                        stream.display_missing = true;
                    }
                    stream.display_epoch = Some(epoch);
                }
                let filtered = stream.prepare_display(filtered, observer_only);
                if filtered.is_empty() {
                    if let (Some(ledger), Some(credit)) = (&self.credit, &credit) { ledger.discard_native(credit); }
                    return UiEventEnqueueResult::FilteredOutput;
                }

                if observer_only {
                    // No uncredited payload enters either WebView queue. Keep
                    // one counter per live run, not a log/event per raw read.
                    record_output_gap(&mut state.output_gaps, run_key, identity, session);
                    state.output_gaps.get_mut(&run_key).unwrap().observer_only = true;
                    return UiEventEnqueueResult::DroppedOutputFull;
                }

                if let (Some(ledger), Some(credit)) = (&self.credit, &credit)
                    && !ledger.is_current(credit)
                {
                    ledger.discard_native(credit); // Explicit renderer reset; sanitizer still advanced.
                    state.terminal_streams.get_mut(&run_key).unwrap().display_missing = true;
                    return UiEventEnqueueResult::FilteredOutput;
                }

                let output_gaps_before = state.output_gaps.remove(&run_key).into_iter().collect();
                let queued = QueuedUiEvent {
                    order: state.next_order,
                    credit,
                    event: shared_types::RuntimeEvent::SessionOutput {
                        identity,
                        session: session.clone(),
                        chunk: filtered,
                        synthetic,
                        timestamp,
                    },
                    output_gaps_before,
                };
                if queued.credit.is_some() {
                    // Every queue item still owns one of <=TOTAL_ITEMS global
                    // reservations. No credit wait or pressure shedding here.
                    return match self.output_sender.try_send(queued) {
                        Ok(()) => { state.next_order += 1; self.notify(); UiEventEnqueueResult::Enqueued }
                        Err(mpsc::TrySendError::Disconnected(queued)) => {
                            if let (Some(ledger), Some(credit)) = (&self.credit, queued.credit) { ledger.discard_native(&credit); }
                            UiEventEnqueueResult::Disconnected
                        }
                        Err(mpsc::TrySendError::Full(_)) => panic!("reserved output queue capacity invariant violated"),
                    };
                }
                match self.sender.try_send(queued) {
                    Ok(()) => { state.next_order += 1; self.notify(); UiEventEnqueueResult::Enqueued },
                    Err(mpsc::TrySendError::Full(queued)) => {
                        restore_output_gaps(&mut state.output_gaps, queued.output_gaps_before);
                        record_output_gap(&mut state.output_gaps, run_key, identity, session);
                        UiEventEnqueueResult::DroppedOutputFull
                    }
                    Err(mpsc::TrySendError::Disconnected(queued)) => {
                        restore_output_gaps(&mut state.output_gaps, queued.output_gaps_before);
                        record_output_gap(&mut state.output_gaps, run_key, identity, session);
                        UiEventEnqueueResult::Disconnected
                    }
                }
            }
            event => {
                let mut closing = None;
                if let shared_types::RuntimeEvent::SessionState {
                    identity,
                    state:
                        shared_types::LifecycleState::Closed | shared_types::LifecycleState::Failed,
                    ..
                } = &event
                    && let Some(stream) = state
                        .terminal_streams
                        .remove(&RunOutputKey::from(*identity))
                {
                    closing = Some(RunOutputKey::from(*identity));
                    let _discarded_incomplete_control_tail = stream.finish();
                }

                let output_gaps_before = take_reportable_gaps(
                    &mut state.output_gaps, self.credit.as_deref(), closing,
                );
                let queued = QueuedUiEvent {
                    order: state.next_order,
                    credit: None,
                    event,
                    output_gaps_before,
                };
                // Preserve the existing bounded, lossless control lane. Some
                // callers still hold lifecycle locks during publication; this
                // pre-existing blocking debt is not a parser-credit wait.
                match self.sender.send(queued) {
                    Ok(()) => { state.next_order += 1; self.notify(); UiEventEnqueueResult::Enqueued },
                    Err(mpsc::SendError(queued)) => {
                        restore_output_gaps(&mut state.output_gaps, queued.output_gaps_before);
                        UiEventEnqueueResult::Disconnected
                    }
                }
            }
        }
    }
}

fn record_output_gap(
    gaps: &mut BTreeMap<RunOutputKey, UiOutputGap>,
    run_key: RunOutputKey,
    identity: RunEventIdentity,
    session: String,
) {
    let gap = gaps.entry(run_key).or_insert(UiOutputGap {
        identity,
        session: session.clone(),
        dropped_events: 0,
        observer_only: false,
    });
    gap.identity = identity;
    gap.session = session;
    gap.dropped_events = gap.dropped_events.saturating_add(1);
}

fn restore_output_gaps(gaps: &mut BTreeMap<RunOutputKey, UiOutputGap>, restored: Vec<UiOutputGap>) {
    for gap in restored {
        let run_key = RunOutputKey::from(gap.identity);
        let current = gaps.entry(run_key).or_insert(UiOutputGap {
            identity: gap.identity,
            session: gap.session.clone(),
            dropped_events: 0,
            observer_only: gap.observer_only,
        });
        if gap.identity.sequence >= current.identity.sequence {
            current.identity = gap.identity;
            current.session = gap.session;
        }
        current.dropped_events = current.dropped_events.saturating_add(gap.dropped_events);
        current.observer_only |= gap.observer_only;
    }
}

fn take_reportable_gaps(
    gaps: &mut BTreeMap<RunOutputKey, UiOutputGap>,
    credit: Option<&OutputCreditLedger>,
    closing: Option<RunOutputKey>,
) -> Vec<UiOutputGap> {
    let ready = gaps.iter().filter_map(|(key, gap)| {
        (!gap.observer_only || closing == Some(*key)
            || credit.is_some_and(|ledger| ledger.can_report_display_gap(gap.identity.into())))
            .then_some(*key)
    }).collect::<Vec<_>>();
    ready.into_iter().filter_map(|key| gaps.remove(&key)).collect()
}

fn ui_event_channel(
    capacity: usize,
) -> (
    UiEventIngress,
    UiEventReceiver,
    Arc<UiEventIngressState>,
) {
    assert!(capacity > 0, "UI event queue capacity must be positive");
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let (output_sender, output_receiver) = mpsc::sync_channel(TOTAL_ITEMS);
    let wake = Arc::new((Mutex::new(()), Condvar::new()));
    let state = Arc::new(UiEventIngressState::default());
    (
        UiEventIngress {
            sender,
            output_sender,
            state: state.clone(),
            credit: None,
            wake: wake.clone(),
        },
        UiEventReceiver { controls: receiver, outputs: output_receiver, wake,
            heads: std::cell::RefCell::new((None, None, 0)) },
        state,
    )
}

/// Two fixed-capacity lanes, one admission order. A control barrier cannot pass
/// preceding output; neither lane ever waits for a parser ACK. The existing
/// bounded control send policy remains unchanged (including its lock debt).
struct UiEventReceiver {
    controls: mpsc::Receiver<QueuedUiEvent>,
    outputs: mpsc::Receiver<QueuedUiEvent>,
    heads: std::cell::RefCell<(Option<QueuedUiEvent>, Option<QueuedUiEvent>, u64)>,
    wake: Arc<(Mutex<()>, Condvar)>,
}
impl UiEventReceiver {
    #[cfg(test)]
    fn try_iter(&self) -> impl Iterator<Item = QueuedUiEvent> + '_ {
        std::iter::from_fn(|| self.try_recv().ok())
    }
    fn try_recv(&self) -> Result<QueuedUiEvent, mpsc::TryRecvError> {
        let mut heads = self.heads.borrow_mut();
        let control_closed = if heads.0.is_none() { match self.controls.try_recv() {
            Ok(event) => { heads.0 = Some(event); false },
            Err(mpsc::TryRecvError::Disconnected) => true,
            Err(_) => false,
        } } else { false };
        let output_closed = if heads.1.is_none() { match self.outputs.try_recv() {
            Ok(event) => { heads.1 = Some(event); false },
            Err(mpsc::TryRecvError::Disconnected) => true,
            Err(_) => false,
        } } else { false };
        let next = if heads.0.as_ref().is_some_and(|e| e.order == heads.2) { heads.0.take() }
            else if heads.1.as_ref().is_some_and(|e| e.order == heads.2) { heads.1.take() }
            else { None };
        if let Some(event) = next { heads.2 += 1; return Ok(event); }
        Err(if control_closed && output_closed { mpsc::TryRecvError::Disconnected }
            else { mpsc::TryRecvError::Empty })
    }
    fn recv_timeout(&self, timeout: Duration) -> Result<QueuedUiEvent, mpsc::RecvTimeoutError> {
        let deadline = Instant::now() + timeout;
        let mut wake = self.wake.0.lock().unwrap_or_else(|p| p.into_inner());
        loop {
            match self.try_recv() {
                Ok(event) => return Ok(event),
                Err(mpsc::TryRecvError::Disconnected) => return Err(mpsc::RecvTimeoutError::Disconnected),
                Err(_) => {}
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(mpsc::RecvTimeoutError::Timeout); }
            wake = self.wake.1.wait_timeout(wake, remaining).unwrap_or_else(|p| p.into_inner()).0;
        }
    }
    #[cfg(test)]
    fn recv(&self) -> Result<QueuedUiEvent, mpsc::RecvError> {
        loop { match self.recv_timeout(Duration::from_secs(60)) {
            Ok(event) => return Ok(event),
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(mpsc::RecvError),
            Err(_) => {}
        } }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RunOutputKey {
    session_id: u128,
    run_id: u128,
}

impl From<RunEventIdentity> for RunOutputKey {
    fn from(identity: RunEventIdentity) -> Self {
        Self {
            session_id: identity.session_id.as_u128(),
            run_id: identity.run_id.as_u128(),
        }
    }
}

#[derive(Default)]
struct PendingOutputBatcher {
    pending: Option<PendingSessionOutput>,
    pending_since: Option<Instant>,
}

impl PendingOutputBatcher {
    fn push(&mut self, output: PendingSessionOutput, now: Instant) -> Vec<PendingSessionOutput> {
        let mut emitted = self.flush_expired(now);
        debug_assert!(
            output.chunk.len() <= MAX_UI_OUTPUT_BATCH_BYTES,
            "one logical SessionOutput must remain atomic; pty-host reads at most 4096 raw bytes (at most 12288 bytes after lossy UTF-8 expansion), below the UI batch cap"
        );
        if output.chunk.is_empty() {
            self.queue_one(output, now, &mut emitted);
            return emitted;
        }

        let must_flush = self.pending.as_ref().is_some_and(|buffer| {
            !pending_outputs_are_contiguous(buffer, &output)
                || buffer.chunk.len() + output.chunk.len() > MAX_UI_OUTPUT_BATCH_BYTES
        });
        if must_flush {
            self.flush_into(&mut emitted);
        }
        self.queue_one(output, now, &mut emitted);
        if self
            .pending
            .as_ref()
            .is_some_and(|buffer| buffer.chunk.len() == MAX_UI_OUTPUT_BATCH_BYTES)
        {
            self.flush_into(&mut emitted);
        }
        emitted
    }

    fn flush_expired(&mut self, now: Instant) -> Vec<PendingSessionOutput> {
        let expired = self
            .pending_since
            .is_some_and(|started| now.duration_since(started) >= MAX_UI_OUTPUT_BATCH_AGE);
        if expired { self.flush() } else { Vec::new() }
    }

    fn flush(&mut self) -> Vec<PendingSessionOutput> {
        self.pending_since = None;
        self.pending.take().into_iter().collect()
    }

    fn queue_one(
        &mut self,
        output: PendingSessionOutput,
        now: Instant,
        emitted: &mut Vec<PendingSessionOutput>,
    ) {
        let had_pending = self.pending.is_some();
        if let Some(displaced) = queue_pending_session_output(&mut self.pending, output) {
            emitted.push(displaced);
            self.pending_since = self.pending.as_ref().map(|_| now);
        } else if !had_pending && self.pending.is_some() {
            self.pending_since = Some(now);
        }
    }

    fn flush_into(&mut self, emitted: &mut Vec<PendingSessionOutput>) {
        emitted.extend(self.flush());
    }
}

#[derive(Default)]
struct TerminalOutputSanitizer {
    state: TerminalControlState,
}

#[derive(Clone, Copy, Default)]
enum TerminalControlState {
    #[default]
    Normal,
    Escape,
    String {
        bell_terminated: bool,
    },
    StringEscape {
        bell_terminated: bool,
    },
}

struct RendererTerminalStream {
    identity: RunEventIdentity,
    session: String,
    sanitizer: TerminalOutputSanitizer,
    last_synthetic: bool,
    last_timestamp: String,
    boundary: DisplayControlBoundary,
    modes: DisplayModeSnapshot,
    display_missing: bool,
    /// Renderer epoch (as u128) of the last credited output, so the first
    /// output to a reloaded page replays the tracked terminal modes.
    display_epoch: Option<u128>,
}

impl RendererTerminalStream {
    fn new(identity: RunEventIdentity, session: String) -> Self {
        Self {
            identity,
            session,
            sanitizer: TerminalOutputSanitizer::default(),
            last_synthetic: false,
            last_timestamp: String::new(),
            boundary: DisplayControlBoundary::default(),
            modes: DisplayModeSnapshot::default(),
            display_missing: false,
            display_epoch: None,
        }
    }

    fn push(
        &mut self,
        identity: RunEventIdentity,
        chunk: &str,
        synthetic: bool,
        timestamp: &str,
    ) -> String {
        debug_assert_eq!(
            RunOutputKey::from(self.identity),
            RunOutputKey::from(identity),
            "renderer terminal streams must not span run identities"
        );
        self.identity = identity;
        self.last_synthetic = synthetic;
        self.last_timestamp = timestamp.to_string();
        self.sanitizer.push(chunk)
    }

    fn finish(self) -> PendingSessionOutput {
        PendingSessionOutput {
            credit: None,
            identity: self.identity,
            session: self.session,
            chunk: self.sanitizer.finish(),
            synthetic: self.last_synthetic,
            timestamp: self.last_timestamp,
        }
    }

    fn prepare_display(&mut self, filtered: String, dropping: bool) -> String {
        if dropping || !self.display_missing {
            for character in filtered.chars() { self.boundary.observe(character, &mut self.modes); }
            if dropping && !filtered.is_empty() { self.display_missing = true; }
            return filtered;
        }
        let mut start = 0;
        if !matches!(self.boundary, DisplayControlBoundary::Normal) {
            for (offset, character) in filtered.char_indices() {
                self.boundary.observe(character, &mut self.modes);
                start = offset + character.len_utf8();
                if matches!(self.boundary, DisplayControlBoundary::Normal) { break; }
            }
        }
        let suffix = &filtered[start..];
        if suffix.is_empty() { return String::new(); }
        // Capture modes at the recovery boundary, before suffix commands can
        // change them. Replaying final-chunk modes here would reorder the TUI.
        let prefix = self.modes.recovery_prefix();
        for character in suffix.chars() { self.boundary.observe(character, &mut self.modes); }
        self.display_missing = false;
        // CAN cancels xterm's pre-gap partial sequence; SGR resets attributes.
        // Neither reconstructs cursor/screen state or lost text: keep the gap
        // warning and request a full redraw in the renderer.
        format!("{prefix}{suffix}")
    }
}

/// Constant state only. OSC/DCS/other strings have already been consumed by
/// TerminalOutputSanitizer; track the CSI/ESC sequences it passes to xterm.
#[derive(Clone, Copy, Default)]
enum DisplayControlBoundary { #[default] Normal, Escape, Intermediate, Csi(DisplayModeCsi) }
impl DisplayControlBoundary {
    fn observe(&mut self, character: char, modes: &mut DisplayModeSnapshot) {
        if matches!(character, '\u{18}' | '\u{1a}') { *self = Self::Normal; return; }
        if character == '\u{1b}' { *self = Self::Escape; return; }
        if character == '\u{009b}' { *self = Self::Csi(DisplayModeCsi::default()); return; }
        match *self {
            Self::Normal => {},
            Self::Escape => match character {
                '[' => *self = Self::Csi(DisplayModeCsi::default()),
                'c' => { modes.reset_full(); *self = Self::Normal; },
                '\u{20}'..='\u{2f}' => *self = Self::Intermediate,
                '\u{30}'..='\u{7e}' => *self = Self::Normal,
                _ => {},
            },
            Self::Intermediate if matches!(character, '\u{30}'..='\u{7e}') => *self = Self::Normal,
            Self::Csi(mut csi) => {
                if matches!(character, '\u{40}'..='\u{7e}') {
                    csi.finish_parameter();
                    if !csi.invalid && csi.private && !csi.soft_reset && matches!(character, 'h' | 'l') {
                        csi.selected.apply(modes, character == 'h');
                    } else if !csi.invalid && !csi.private && csi.soft_reset && character == 'p' {
                        modes.reset_soft();
                    }
                    *self = Self::Normal;
                } else {
                    csi.observe(character);
                    *self = Self::Csi(csi);
                }
            },
            _ => {},
        }
    }
}

/// Renderer state only, never permission/routing authority. These are the
/// reviewed input-affecting modes supported by installed xterm 6; unknown modes
/// stay untracked. Protocol/encoding aliases are last-wins, not independent bits.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
struct DisplayModeSnapshot {
    cursor_keys: Option<bool>,
    cursor_visible: Option<bool>,
    focus: Option<bool>,
    bracketed_paste: Option<bool>,
    alternate: Option<bool>,
    mouse_protocol: Option<u16>, // 0=off, 9/1000/1002/1003
    mouse_encoding: Option<u16>, // 0=default, 1006/1016
}
impl DisplayModeSnapshot {
    fn select(&mut self, parameter: u16) {
        match parameter {
            1 => self.cursor_keys = Some(true),
            25 => self.cursor_visible = Some(true),
            1004 => self.focus = Some(true),
            2004 => self.bracketed_paste = Some(true),
            47 | 1047 | 1049 => self.alternate = Some(true),
            9 | 1000 | 1002 | 1003 => self.mouse_protocol = Some(parameter),
            1006 | 1016 => self.mouse_encoding = Some(parameter),
            _ => {}, // xterm ignores 1001/1005/1015; do not invent support.
        }
    }
    fn apply(self, target: &mut Self, enabled: bool) {
        if self.cursor_keys.is_some() { target.cursor_keys = Some(enabled); }
        if self.cursor_visible.is_some() { target.cursor_visible = Some(enabled); }
        if self.focus.is_some() { target.focus = Some(enabled); }
        if self.bracketed_paste.is_some() { target.bracketed_paste = Some(enabled); }
        if self.alternate.is_some() { target.alternate = Some(enabled); }
        if let Some(mode) = self.mouse_protocol { target.mouse_protocol = Some(if enabled { mode } else { 0 }); }
        if let Some(mode) = self.mouse_encoding { target.mouse_encoding = Some(if enabled { mode } else { 0 }); }
    }
    fn reset_soft(&mut self) {
        self.cursor_keys = Some(false);
        self.cursor_visible = Some(true);
        self.focus = Some(false);
        self.bracketed_paste = Some(false);
        // xterm DECSTR does not reset its mouse service or active buffer.
    }
    fn reset_full(&mut self) {
        self.cursor_keys = Some(false);
        self.focus = Some(false);
        self.bracketed_paste = Some(false);
        self.alternate = Some(false);
        self.mouse_protocol = Some(0);
        self.mouse_encoding = Some(0);
        // Installed xterm RIS retains CoreService.isCursorHidden; unlike
        // DECSTR, it does not promise to show a previously hidden cursor.
    }
    fn recovery_prefix(self) -> String {
        let mut prefix = String::from("\x18\x1b[0m");
        // Canonical 47 selects the buffer without inventing 1049's saved
        // cursor, which cannot be reconstructed from omitted display output.
        for (mode, enabled) in [(47, self.alternate), (1, self.cursor_keys),
            (25, self.cursor_visible), (1004, self.focus), (2004, self.bracketed_paste)]
        {
            if let Some(enabled) = enabled { prefix.push_str(&format!("\x1b[?{mode}{}", if enabled { 'h' } else { 'l' })); }
        }
        for (selected, reset_mode) in [(self.mouse_protocol, 1000), (self.mouse_encoding, 1006)] {
            if let Some(mode) = selected {
                let parameter = if mode == 0 { reset_mode } else { mode };
                prefix.push_str(&format!("\x1b[?{parameter}{}", if mode == 0 { 'l' } else { 'h' }));
            }
        }
        // Seven controls of <=8 bytes + CAN/SGR + one carried ESC <=62 bytes.
        debug_assert!(prefix.len() + 1 <= supervisor::output_credit::DISPLAY_OVERHEAD);
        prefix
    }
}

/// Fixed CSI parameter accumulator. Like the supervisor's private-mode
/// scanner, changes commit only at a complete valid DECSET/DECRST final byte.
#[derive(Clone, Copy, Default)]
struct DisplayModeCsi {
    started: bool,
    private: bool,
    soft_reset: bool,
    invalid: bool,
    current: Option<u16>,
    selected: DisplayModeSnapshot,
}
impl DisplayModeCsi {
    fn finish_parameter(&mut self) {
        if let Some(parameter) = self.current.take() { self.selected.select(parameter); }
    }
    fn observe(&mut self, character: char) {
        if character <= '\u{1f}' || character == '\u{7f}' { return; }
        match character {
            '?' if !self.started => self.private = true,
            '0'..='9' if !self.soft_reset => {
                self.current = self.current.unwrap_or(0).checked_mul(10)
                    .and_then(|value| value.checked_add(character as u16 - '0' as u16));
                if self.current.is_none() { self.invalid = true; }
            },
            ';' if !self.soft_reset => self.finish_parameter(),
            '!' if !self.private && !self.soft_reset => self.soft_reset = true,
            _ => self.invalid = true,
        }
        self.started = true;
    }
}

impl TerminalOutputSanitizer {
    fn push(&mut self, input: &str) -> String {
        let mut output = String::with_capacity(input.len());
        for character in input.chars() {
            match self.state {
                TerminalControlState::Normal => match character {
                    '\u{1b}' => self.state = TerminalControlState::Escape,
                    '\u{009d}' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: true,
                        }
                    }
                    '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: false,
                        }
                    }
                    '\u{009c}' => {}
                    _ => output.push(character),
                },
                TerminalControlState::Escape => match character {
                    ']' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: true,
                        }
                    }
                    'P' | 'X' | '^' | '_' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: false,
                        }
                    }
                    '\u{009d}' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: true,
                        }
                    }
                    '\u{0090}' | '\u{0098}' | '\u{009e}' | '\u{009f}' => {
                        self.state = TerminalControlState::String {
                            bell_terminated: false,
                        }
                    }
                    '\u{009c}' => self.state = TerminalControlState::Normal,
                    '\u{1b}' => {}
                    _ => {
                        output.push('\u{1b}');
                        output.push(character);
                        self.state = TerminalControlState::Normal;
                    }
                },
                TerminalControlState::String { bell_terminated } => match character {
                    '\u{0007}' if bell_terminated => self.state = TerminalControlState::Normal,
                    '\u{009c}' => self.state = TerminalControlState::Normal,
                    '\u{1b}' => self.state = TerminalControlState::StringEscape { bell_terminated },
                    _ => {}
                },
                TerminalControlState::StringEscape { bell_terminated } => match character {
                    '\\' | '\u{009c}' => self.state = TerminalControlState::Normal,
                    '\u{0007}' if bell_terminated => self.state = TerminalControlState::Normal,
                    '\u{1b}' => {}
                    _ => self.state = TerminalControlState::String { bell_terminated },
                },
            }
        }
        output
    }

    fn finish(self) -> String {
        // A lone ESC is an incomplete control sequence. Emitting it later as a
        // second event would reuse the prior sequence and be rejected by the
        // renderer's sequence gate, so terminal close discards it atomically.
        String::new()
    }
}

/// A108: the current self-upgrade attempt for the banner, if any.
#[tauri::command]
fn upgrade_status(state: State<'_, DesktopState>) -> Option<shared_types::UpgradeStatus> {
    state.supervisor.upgrade_status()
}

/// The window is shown: the new build resumes its recorded members once.
/// Returns the status naming them; the restart resume leaves them alone.
#[tauri::command]
fn begin_upgrade_resume(state: State<'_, DesktopState>) -> Option<shared_types::UpgradeStatus> {
    state.supervisor.begin_upgrade_resume()
}

/// Operator Cancel during the upgrade notice.
#[tauri::command]
async fn cancel_upgrade(state: State<'_, DesktopState>, attempt_id: RunId) -> Result<(), String> {
    state.diagnostics.log("info", "cancel_upgrade", attempt_id.to_string());
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.cancel_upgrade(attempt_id))
        .await
        .map_err(|error| format!("upgrade worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
}

/// Operator-only, after the confirm gate: a terminal note on a held attempt.
#[tauri::command]
async fn abandon_held_upgrade(state: State<'_, DesktopState>, attempt_id: RunId) -> Result<(), String> {
    state.diagnostics.log("warn", "abandon_held_upgrade", attempt_id.to_string());
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.abandon_held_upgrade(attempt_id))
        .await
        .map_err(|error| format!("upgrade worker failed: {error}"))?
        .map_err(|error| format!("{error:#}"))
}

/// Whether any room composer holds unsent text; an upgrade waits while it does.
#[tauri::command]
fn report_room_drafts(state: State<'_, DesktopState>, open: bool) {
    state.supervisor.report_operator_room_drafts(open);
}

#[tauri::command]
fn automation_mode(state: State<'_, DesktopState>) -> bool {
    state.automation_mode
}

#[tauri::command]
fn trial_status(window: WebviewWindow, state: State<'_, DesktopState>) -> Result<Option<trial_desktop::TrialStatus>, String> {
    if window.label() != "main" { return Err("trial status is available only to the main WebView".into()); }
    Ok(state.trial.as_ref().map(|trial| trial_desktop::TrialStatus {
        challenge: trial.challenge(), admission: "held", backend_initialized: true,
    }))
}

#[tauri::command]
async fn trial_renderer_acknowledged(
    window: WebviewWindow, state: State<'_, DesktopState>, challenge: TrialChallenge,
) -> Result<TrialHealthReceipt, String> {
    let label = window.label().to_owned();
    let trial = state.trial.clone().ok_or("this desktop is not a held trial")?;
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || trial.record_renderer_acknowledged(&supervisor, &label, &challenge))
        .await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())
}

#[tauri::command]
fn bootstrap(state: State<'_, DesktopState>) -> Result<RuntimeSnapshot, String> {
    state
        .diagnostics
        .log("info", "bootstrap", "runtime snapshot requested");
    Ok(state.supervisor.snapshot())
}

/// Metadata discovery never launches or attaches a provider conversation.
#[tauri::command]
async fn discover_local_sessions(state: State<'_, DesktopState>) -> Result<supervisor::session_inventory::Inventory, String> {
    static BUSY: AtomicBool = AtomicBool::new(false);
    if BUSY.swap(true, Ordering::AcqRel) {
        return Err("A local conversation scan is already running".into());
    }
    struct Reset;
    impl Drop for Reset { fn drop(&mut self) { BUSY.store(false, Ordering::Release); } }
    let reset = Reset;
    let personal_repo = state.supervisor.personal_repo_status();
    tauri::async_runtime::spawn_blocking(move || {
        let _reset = reset;
        supervisor::session_inventory::discover_personal_repo(&personal_repo)
    }).await.map_err(|error| error.to_string())?
}

/// Operator-only metadata binding. The supervisor always retains a recovery
/// hold; this command never launches or resumes a provider conversation.
#[tauri::command]
async fn attach_local_session(
    window: WebviewWindow,
    state: State<'_, DesktopState>,
    request: supervisor::session_inventory::AttachLocalSessionRequest,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Local conversations can be attached only from the main WebView".into());
    }
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.attach_local_session(request))
        .await.map_err(|error| error.to_string())?.map_err(|error| error.to_string())
}

#[tauri::command]
fn begin_output_renderer(state: State<'_, DesktopState>) -> RendererReset {
    state.supervisor.output_credit().begin_renderer()
}

#[tauri::command]
async fn output_renderer_ready(state: State<'_, DesktopState>, epoch: String) -> Result<(), String> {
    let epoch = epoch.parse().map_err(|_| "invalid renderer epoch".to_string())?;
    let ledger = state.supervisor.output_credit();
    tauri::async_runtime::spawn_blocking(move || ledger.renderer_ready(epoch, Duration::from_secs(5)))
        .await.map_err(|error| error.to_string())?.map_err(str::to_string)
}

#[tauri::command]
async fn acknowledge_output(state: State<'_, DesktopState>, credit: OutputCredit) -> Result<usize, String> {
    // No slots, lifecycle, publisher, UI ingress, or event emission.
    state.supervisor.output_credit().acknowledge(&credit).map_err(str::to_string)
}

#[tauri::command]
async fn output_credit_metrics(state: State<'_, DesktopState>) -> Result<OutputCreditMetrics, String> {
    Ok(state.supervisor.output_credit().metrics())
}

#[tauri::command]
async fn discard_output(state: State<'_, DesktopState>, credit: OutputCredit, reason: String) -> Result<(), String> {
    if !matches!(reason.as_str(), "stale_epoch" | "stale_run" | "pane_disposed" | "write_failed" | "bootstrap_overflow" | "unattached_overflow") {
        return Err("unknown output disposal reason".into());
    }
    if credit.tickets.len() > TOTAL_ITEMS { return Err("too many output tickets".into()); }
    let dropped = state.supervisor.output_credit().discard(&credit);
    if dropped.items > 0 {
        state.diagnostics.log("warn", "display_output_discarded", format!("reason={reason} items={} bytes={} (not parsed)", dropped.items, dropped.bytes));
    }
    Ok(())
}

#[tauri::command]
async fn preview_session_launch(
    state: State<'_, DesktopState>,
    request: StartSessionRequest,
) -> Result<SessionLaunchPreview, String> {
    let supervisor = state.supervisor.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.preview_session_launch(request)).await {
        Ok(result) => result.map_err(|error| format!("{error:#}")),
        Err(error) => Err(format!("launch preview worker failed: {error}")),
    }
}

#[tauri::command]
fn start_session(
    state: State<'_, DesktopState>,
    request: StartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "start_session",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .start_session_with_launch_choices(
            request.session_id, request.fresh, request.search, request.launch_choices,
        )
        .map_err(|error| {
            let detail = format!("{error:#}");
            state.diagnostics.log(
                "error",
                "start_session_failed",
                format!("{}: {detail}", request.session_id),
            );
            detail
        })
}

/// Boot resume has native once-only eligibility; ordinary Start never promotes
/// an interrupted task. No caller-supplied launch choices cross this boundary.
#[tauri::command]
async fn resume_session_after_restart(
    state: State<'_, DesktopState>,
    session_id: SessionId,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log("info", "resume_session_after_restart", format!("requested {session_id}"));
    let supervisor = state.supervisor.clone();
    let outcome = match tauri::async_runtime::spawn_blocking(move || supervisor.resume_session_after_restart(session_id)).await {
        Ok(result) => result.map_err(|error| format!("{error:#}")),
        Err(error) => Err(format!("restart resume worker failed: {error}")),
    };
    outcome.map_err(|error| {
        state.diagnostics.log("error", "restart_resume_failed", format!("{session_id}: {error}"));
        error
    })
}

/// "Resume here" on a member copied from the live PRIM-1 (A106 move).
#[tauri::command]
async fn resume_held_conversation(
    state: State<'_, DesktopState>,
    request: StopSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log("info", "resume_held_conversation", format!("requested {}", request.session_id));
    // The Codex rollout search and the catalog write stay off the UI thread.
    let supervisor = state.supervisor.clone();
    let session_id = request.session_id;
    let outcome = match tauri::async_runtime::spawn_blocking(move || supervisor.resume_held_conversation(session_id)).await {
        Ok(result) => result.map_err(|error| format!("{error:#}")),
        Err(error) => Err(format!("resume worker failed: {error}")),
    };
    outcome.map_err(|error| {
        state.diagnostics.log("error", "resume_held_conversation_failed", format!("{session_id}: {error}"));
        error
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
        format!("requested {}", request.session_id),
    );
    // Admission stays on this ordered (main-thread) command, so a later Start,
    // Restart or Delete sees the declared Stop. Only the bounded termination
    // wait, up to the kill timeout, moves off the UI thread; the final state
    // arrives as runtime events and a late failure as a system log event.
    let (ticket, admitted) = state
        .supervisor
        .begin_stop_session(request.session_id)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "stop_session_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })?;
    // A dedicated thread, so a dispatch failure is observable: the admitted
    // Stop is then finished here rather than left declared forever.
    let pending = Arc::new(Mutex::new(Some(ticket)));
    let worker_ticket = pending.clone();
    let supervisor = state.supervisor.clone();
    let dispatched = thread::Builder::new()
        .name("prim1-stop-finish".into())
        .spawn(move || {
            if let Some(ticket) = worker_ticket.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
                supervisor.finish_stop_session_detached(ticket);
            }
        });
    if let Err(error) = dispatched {
        state.diagnostics.log(
            "warn",
            "stop_session_dispatch_failed",
            format!("{}: {error}; finishing on the command thread", request.session_id),
        );
        if let Some(ticket) = pending.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take() {
            return state
                .supervisor
                .finish_stop_session(ticket)
                .map_err(|error| error.to_string());
        }
    }
    Ok(admitted)
}

#[tauri::command]
fn restart_session(
    state: State<'_, DesktopState>,
    request: RestartSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "restart_session",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .restart_session_by_id(request.session_id)
        .map_err(|error| {
            let detail = format!("{error:#}");
            state.diagnostics.log(
                "error",
                "restart_session_failed",
                format!("{}: {detail}", request.session_id),
            );
            detail
        })
}

#[tauri::command]
fn create_session(
    state: State<'_, DesktopState>,
    request: CreateSessionRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "create_session",
        format!(
            "driver={:?} permission_profile={:?}",
            request.driver, request.permission_profile
        ),
    );
    state.supervisor.create_session(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "create_session_failed", error.to_string());
        error.to_string()
    })
}

/// Prime/WSL is parked in v2 unless `PRIM1_ENABLE_PRIME=1`; the UI hides it.
#[tauri::command]
fn prime_enabled() -> bool {
    supervisor::prime_enabled()
}

#[tauri::command]
fn prime_default_working_directory(state: State<'_, DesktopState>) -> Result<String, String> {
    state
        .diagnostics
        .log("info", "prime_default_working_directory", "requested");
    state
        .supervisor
        .prime_default_working_directory()
        .map_err(|error| {
            let detail = format!("{error:#}");
            state.diagnostics.log(
                "error",
                "prime_default_working_directory_failed",
                detail.clone(),
            );
            detail
        })
}

#[tauri::command]
fn rename_session(
    state: State<'_, DesktopState>,
    request: RenameSessionRequest,
) -> Result<(), String> {
    state.diagnostics.log(
        "info",
        "rename_session",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .rename_session(request.session_id, &request.label)
        .map(|_| ())
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "rename_session_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn set_session_permission_profile(
    state: State<'_, DesktopState>,
    request: SetSessionPermissionRequest,
) -> Result<(), String> {
    state.diagnostics.log(
        "info",
        "set_session_permission_profile",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .set_permission_profile(request.session_id, request.permission_profile)
        .map(|_| ())
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "set_session_permission_profile_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

/// Operator-only (A109, A110): the member's roam, maintain and manage grants. No pane
/// sideband request can change them.
#[tauri::command]
fn set_member_permissions(
    state: State<'_, DesktopState>,
    request: SetMemberPermissionsRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "set_member_permissions",
        format!("{} roam={} maintain={} manage={}", request.session_id, request.roam, request.maintain, request.manage),
    );
    let permissions = MemberPermissions { roam: request.roam, maintain: request.maintain, manage: request.manage };
    state
        .supervisor
        .set_member_permissions(request.session_id, permissions)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "set_member_permissions_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn move_session(
    state: State<'_, DesktopState>,
    request: MoveSessionRequest,
) -> Result<RuntimeSnapshot, String> {
    state
        .supervisor
        .move_session(request.session_id, request.new_index)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "move_session_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn delete_session(
    state: State<'_, DesktopState>,
    request: DeleteSessionRequest,
) -> Result<(), String> {
    state.diagnostics.log(
        "info",
        "delete_session",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .delete_session(request.session_id)
        .map_err(|error| {
            let message = error.to_string();
            // A16 D4 (PC3, 20 Oct): right after a stop, Close retries these
            // two transient refusals (main.ts deleteSessionOnceClosed); each
            // attempt is a wait, not a failure, and was read as one.
            let (level, event) = if delete_refusal_is_transient(&message) {
                ("info", "delete_session_waiting")
            } else {
                ("error", "delete_session_failed")
            };
            state.diagnostics.log(level, event, format!("{}: {}", request.session_id, message));
            message
        })
}

/// The supervisor's refusals that Close retries (the same two phrases as
/// main.ts deleteSessionOnceClosed): the stop is still finishing, or the
/// stopped session's event stream is still draining.
fn delete_refusal_is_transient(message: &str) -> bool {
    message.contains("must be fully closed") || message.contains("still draining")
}

#[tauri::command]
fn default_room_brief() -> String {
    default_room_brief_template()
}

#[tauri::command]
async fn create_room(
    state: State<'_, DesktopState>,
    request: CreateRoomRequest,
) -> Result<RoomSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.create_room(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "create_room_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room creation worker failed: {error}");
            diagnostics.log("error", "create_room_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn rename_room(
    state: State<'_, DesktopState>,
    request: RenameRoomRequest,
) -> Result<RoomSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.rename_room(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "rename_room_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room rename worker failed: {error}");
            diagnostics.log("error", "rename_room_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn set_room_resume_on_restart(
    state: State<'_, DesktopState>,
    request: SetRoomResumeRequest,
) -> Result<RoomSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || {
        supervisor.set_room_resume_on_restart(request)
    })
    .await
    {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "set_room_resume_on_restart_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room restart setting worker failed: {error}");
            diagnostics.log("error", "set_room_resume_on_restart_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn read_room_brief(
    state: State<'_, DesktopState>,
    request: ReadRoomBriefRequest,
) -> Result<RoomBriefSettings, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.read_room_brief(request)).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => {
            let message = format!("room brief read worker failed: {error}");
            diagnostics.log("error", "read_room_brief_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn update_room_brief(
    state: State<'_, DesktopState>,
    request: UpdateRoomBriefRequest,
) -> Result<RoomBriefSettings, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.update_room_brief(request)).await {
        Ok(Ok(settings)) => Ok(settings),
        Ok(Err(error)) => {
            diagnostics.log("error", "update_room_brief_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room brief settings worker failed: {error}");
            diagnostics.log("error", "update_room_brief_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn get_room_fallbacks(
    state: State<'_, DesktopState>,
    room_id: RoomId,
) -> Result<RoomFallbackSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.get_room_fallbacks(room_id)).await {
        Ok(result) => result.map_err(|error| error.to_string()),
        Err(error) => {
            let message = format!("room fallbacks read worker failed: {error}");
            diagnostics.log("error", "get_room_fallbacks_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn set_room_fallbacks(
    state: State<'_, DesktopState>,
    request: SetRoomFallbacksRequest,
) -> Result<RoomFallbackSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.set_room_fallbacks(request)).await {
        Ok(Ok(snapshot)) => Ok(snapshot),
        Ok(Err(error)) => {
            diagnostics.log("error", "set_room_fallbacks_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room fallbacks save worker failed: {error}");
            diagnostics.log("error", "set_room_fallbacks_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn move_room(
    state: State<'_, DesktopState>,
    request: MoveRoomRequest,
) -> Result<RuntimeSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.move_room(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "move_room_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room move worker failed: {error}");
            diagnostics.log("error", "move_room_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn add_room_member(
    state: State<'_, DesktopState>,
    request: AddRoomMemberRequest,
) -> Result<RoomSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.add_room_member(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "add_room_member_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room member addition worker failed: {error}");
            diagnostics.log("error", "add_room_member_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn remove_room_member(
    state: State<'_, DesktopState>,
    request: RemoveRoomMemberRequest,
) -> Result<RoomSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.remove_room_member(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "remove_room_member_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room member removal worker failed: {error}");
            diagnostics.log("error", "remove_room_member_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn delete_room(state: State<'_, DesktopState>, request: DeleteRoomRequest) -> Result<(), String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.delete_room(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "delete_room_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room deletion worker failed: {error}");
            diagnostics.log("error", "delete_room_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
async fn read_room_feed(
    state: State<'_, DesktopState>,
    request: ReadRoomFeedRequest,
) -> Result<RoomFeedPage, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.read_room_feed(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "read_room_feed_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room feed read worker failed: {error}");
            diagnostics.log("error", "read_room_feed_failed", &message);
            Err(message)
        }
    }
}

#[derive(serde::Serialize)]
struct RoomRequestError {
    kind: &'static str,
    message: String,
}

#[tauri::command]
async fn post_room_message(
    state: State<'_, DesktopState>,
    request: PostRoomMessageRequest,
) -> Result<RoomPostResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.post_room_message(request)).await {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "post_room_message_failed", error.to_string());
            Err(RoomRequestError { kind: supervisor::room_request_error_kind(&error),
                message: error.to_string() })
        }
        Err(error) => {
            let message = format!("room post worker failed: {error}");
            diagnostics.log("error", "post_room_message_failed", &message);
            Err(RoomRequestError { kind: "worker_failed", message })
        }
    }
}

#[tauri::command]
async fn brief_room_member(
    state: State<'_, DesktopState>,
    request: BriefRoomMemberRequest,
) -> Result<RoomDeliveryResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.brief_room_member(request)).await
    {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "brief_room_member_failed", error.to_string());
            Err(RoomRequestError { kind: supervisor::room_request_error_kind(&error), message: error.to_string() })
        }
        Err(error) => {
            let message = format!("room brief worker failed: {error}");
            diagnostics.log("error", "brief_room_member_failed", &message);
            Err(RoomRequestError { kind: "worker_failed", message })
        }
    }
}

/// v3 W3 (b): the operator's own disposition of a message for one recipient.
#[tauri::command]
async fn dispose_room_message(
    state: State<'_, DesktopState>,
    request: shared_types::DisposeRoomMessageRequest,
) -> Result<shared_types::RoomAckResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.dispose_room_message_as_operator(request))
        .await.map_err(|error| RoomRequestError { kind: "worker_failed", message: format!("room disposition worker failed: {error}") })?
        .map_err(|error| RoomRequestError { kind: supervisor::room_request_error_kind(&error),
            message: error.to_string() })
}

/// v3 W3 (D6): the operator withdraws a message.
#[tauri::command]
async fn cancel_room_message(
    state: State<'_, DesktopState>,
    request: shared_types::CancelRoomMessageRequest,
) -> Result<shared_types::RoomCancelResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.cancel_room_message_as_operator(request))
        .await.map_err(|error| RoomRequestError { kind: "worker_failed", message: format!("room withdrawal worker failed: {error}") })?
        .map_err(|error| RoomRequestError { kind: supervisor::room_request_error_kind(&error),
            message: error.to_string() })
}

#[tauri::command]
async fn retry_room_delivery(
    state: State<'_, DesktopState>,
    request: RetryRoomDeliveryRequest,
) -> Result<RoomDeliveryResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.retry_room_delivery(request))
        .await.map_err(|error| RoomRequestError { kind: "worker_failed", message: format!("room retry worker failed: {error}") })?
        .map_err(|error| RoomRequestError { kind: supervisor::room_request_error_kind(&error),
            message: error.to_string() })
}

#[tauri::command]
async fn deliver_room_message(
    state: State<'_, DesktopState>,
    request: DeliverRoomMessageRequest,
) -> Result<RoomDeliveryResult, RoomRequestError> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.deliver_room_message(request))
        .await
    {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "deliver_room_message_failed", error.to_string());
            Err(RoomRequestError { kind: supervisor::room_request_error_kind(&error),
                message: error.to_string() })
        }
        Err(error) => {
            let message = format!("room delivery worker failed: {error}");
            diagnostics.log("error", "deliver_room_message_failed", &message);
            Err(RoomRequestError { kind: "worker_failed", message })
        }
    }
}

/// Shown once after an invitation: the token, the one block to paste into an
/// agent, and the MCP config and script inside it. Nothing here is logged;
/// revoking the guest is the only way to retire it.
#[derive(serde::Serialize)]
struct RoomGuestInvitationView {
    guest: RoomGuest,
    guest_id: String,
    token: String,
    mcp_config_json: String,
    cli_example: String,
    agent_invitation: String,
}

#[tauri::command]
async fn invite_room_guest(
    state: State<'_, DesktopState>,
    request: InviteRoomGuestRequest,
) -> Result<RoomGuestInvitationView, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    let invitation = match tauri::async_runtime::spawn_blocking(move || supervisor.invite_room_guest(request)).await {
        Ok(Ok(invitation)) => invitation,
        Ok(Err(error)) => {
            diagnostics.log("error", "invite_room_guest_failed", error.to_string());
            return Err(error.to_string());
        }
        Err(error) => {
            let message = format!("guest invitation worker failed: {error}");
            diagnostics.log("error", "invite_room_guest_failed", &message);
            return Err(message);
        }
    };
    let output = state.supervisor.guest_invitation_output(&invitation).map_err(|error| error.to_string())?;
    Ok(RoomGuestInvitationView {
        guest_id: invitation.guest.guest_id.to_string(),
        mcp_config_json: output.mcp_config_json,
        cli_example: output.cli_example,
        agent_invitation: output.agent_invitation,
        guest: invitation.guest,
        token: invitation.token,
    })
}

#[tauri::command]
async fn revoke_room_guest(state: State<'_, DesktopState>, request: RevokeRoomGuestRequest) -> Result<(), String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.revoke_room_guest(request)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => {
            diagnostics.log("error", "revoke_room_guest_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("guest revocation worker failed: {error}");
            diagnostics.log("error", "revoke_room_guest_failed", &message);
            Err(message)
        }
    }
}

#[tauri::command]
fn list_room_guests(state: State<'_, DesktopState>, request: ListRoomGuestsRequest) -> Result<Vec<RoomGuest>, String> {
    state.supervisor.list_room_guests(request).map_err(|error| error.to_string())
}

/// The folder a picker opens at: the current one, or its nearest existing
/// parent when it was removed. None leaves the dialog at the system default.
fn picker_start(current: Option<&str>) -> Option<PathBuf> {
    let current = current?;
    // Trim only to detect a blank value; the path keeps its exact characters.
    if current.trim().is_empty() {
        return None;
    }
    Path::new(current).ancestors().find(|candidate| candidate.is_dir()).map(Path::to_path_buf)
}

fn pick_directory(app: &AppHandle, title: &str, start: Option<PathBuf>) -> Result<Option<PathBuf>, String> {
    let mut dialog = app.dialog().file().set_title(title);
    if let Some(start) = start {
        dialog = dialog.set_directory(start);
    }
    dialog
        .blocking_pick_folder()
        .map(|path| {
            path.into_path().map_err(|error| {
                format!("selected directory is not a local filesystem path: {error}")
            })
        })
        .transpose()
}

#[tauri::command]
fn choose_workspace_directory(
    app: AppHandle,
    state: State<'_, DesktopState>,
) -> Result<Option<RuntimeSnapshot>, String> {
    let start = picker_start(Some(&state.supervisor.snapshot().workspace_preference));
    let Some(path) = pick_directory(&app, "Choose PRIM-1 workspace", start)? else {
        return Ok(None);
    };
    state
        .supervisor
        .set_workspace_preference(&path)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "choose_workspace_directory_failed",
                error.to_string(),
            );
            error.to_string()
        })?;
    Ok(Some(state.supervisor.snapshot()))
}

#[tauri::command]
fn choose_session_working_directory(
    app: AppHandle,
    state: State<'_, DesktopState>,
    request: ChooseSessionWorkingDirectoryRequest,
) -> Result<Option<SessionSnapshot>, String> {
    let current = state.supervisor.snapshot().sessions.into_iter()
        .find(|session| session.session_id == request.session_id)
        .map(|session| session.working_dir);
    let Some(path) = pick_directory(&app, "Choose session working directory", picker_start(current.as_deref()))? else {
        return Ok(None);
    };
    state
        .supervisor
        .set_session_working_directory(request.session_id, &path)
        .map(Some)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "choose_session_working_directory_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn set_session_linux_working_directory(
    state: State<'_, DesktopState>,
    request: SetSessionLinuxWorkingDirectoryRequest,
) -> Result<SessionSnapshot, String> {
    state.diagnostics.log(
        "info",
        "set_session_linux_working_directory",
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .set_session_linux_working_directory(request.session_id, &request.linux_working_directory)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "set_session_linux_working_directory_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

/// Settings → Task folder: the selected folder and whether it is open.
#[tauri::command]
fn get_task_folder(state: State<'_, DesktopState>) -> TaskFolderStatus {
    state.supervisor.task_folder_status()
}

/// Opens and locks the new folder before the choice is saved; any failure
/// keeps the current folder. Nothing is copied or moved between folders.
#[tauri::command]
async fn set_task_folder(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<TaskFolderStatus, String> {
    let supervisor = state.supervisor.clone();
    let result = tauri::async_runtime::spawn_blocking(move || supervisor.select_task_folder(&path))
        .await
        .map_err(|error| format!("task folder worker failed: {error}"))?;
    match &result {
        Ok(status) => state.diagnostics.log(
            "info",
            "task_folder_selected",
            format!("folder={}", status.folder.as_deref().unwrap_or("-")),
        ),
        Err(error) => state.diagnostics.log("warn", "task_folder_selection_failed", error.clone()),
    }
    result
}

/// Settings → Task folder → Browse…: the native folder picker only; the
/// choice is then made through `set_task_folder`. A held trial refuses
/// before the picker opens.
#[tauri::command]
fn choose_task_folder(app: AppHandle, state: State<'_, DesktopState>) -> Result<Option<String>, String> {
    if state.supervisor.trial_admission_held() {
        return Err(TRIAL_ADMISSION_HELD.into());
    }
    let start = picker_start(state.supervisor.task_folder_status().folder.as_deref());
    Ok(pick_directory(&app, "Choose the task folder", start)?.map(|path| path.display().to_string()))
}

/// Settings → Personal repository: where the session register is written,
/// where it comes from, and why it is off when it is.
#[tauri::command]
fn get_personal_repo(state: State<'_, DesktopState>) -> PersonalRepoStatus {
    state.supervisor.personal_repo_status()
}

/// Saves the choice and moves the register's writes there; the folder must
/// exist. Refused while PRIM1_PERSONAL_REPO decides. Nothing is moved.
#[tauri::command]
async fn set_personal_repo(
    state: State<'_, DesktopState>,
    path: String,
) -> Result<PersonalRepoStatus, String> {
    let supervisor = state.supervisor.clone();
    let result = tauri::async_runtime::spawn_blocking(move || supervisor.select_personal_repo(&path))
        .await
        .map_err(|error| format!("personal repository worker failed: {error}"))?;
    match &result {
        Ok(status) => state.diagnostics.log(
            "info",
            "personal_repo_selected",
            format!("folder={}", status.folder.as_deref().unwrap_or("-")),
        ),
        Err(error) => state.diagnostics.log("warn", "personal_repo_selection_failed", error.clone()),
    }
    result
}

/// Settings → Personal repository → Browse…: the native folder picker only;
/// the choice is then made through `set_personal_repo`. A held trial refuses
/// before the picker opens.
#[tauri::command]
fn choose_personal_repo(app: AppHandle, state: State<'_, DesktopState>) -> Result<Option<String>, String> {
    if state.supervisor.trial_admission_held() {
        return Err(TRIAL_ADMISSION_HELD.into());
    }
    let start = picker_start(state.supervisor.personal_repo_status().folder.as_deref());
    Ok(pick_directory(&app, "Choose the personal repository", start)?.map(|path| path.display().to_string()))
}

/// A native OK/Cancel question for every confirmation gate. The dialog
/// plugin replaces `window.confirm` with an async invoke whose Promise is
/// always truthy, so the renderer awaits this instead. Cancel, a closed
/// dialog and any failure all mean no. The dialog waits off the main thread.
#[tauri::command]
async fn confirm_action(app: AppHandle, message: String, ok_label: String) -> Result<bool, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut dialog = app.dialog().message(message).title("PRIM-1").kind(MessageDialogKind::Warning)
            .buttons(MessageDialogButtons::OkCancelCustom(ok_label, "Cancel".into()));
        if let Some(window) = app.get_webview_window("main") {
            dialog = dialog.parent(&window);
        }
        dialog.blocking_show()
    })
    .await
    .map_err(|error| format!("confirmation dialog failed: {error}"))
}

/// Rooms → Save…: the native save dialog only; `export_room` then writes.
/// A held trial refuses before the dialog opens.
#[tauri::command]
fn choose_room_save_path(app: AppHandle, state: State<'_, DesktopState>, label: String) -> Result<Option<String>, String> {
    if state.supervisor.trial_admission_held() {
        return Err(TRIAL_ADMISSION_HELD.into());
    }
    let stem: String = label.chars().map(|c| if c.is_control() || r#"<>:"/\|?*"#.contains(c) { '-' } else { c })
        .take(100).collect::<String>().trim_matches([' ', '.']).to_owned();
    let Some(path) = app.dialog().file().set_title("Save room").add_filter("PRIM-1 room", &["prim1room"])
        .set_file_name(format!("{}.prim1room", if stem.is_empty() { "room" } else { &stem }))
        .blocking_save_file() else { return Ok(None) };
    let path = path.into_path().map_err(|error| format!("selected file is not a local filesystem path: {error}"))?;
    let path = if path.extension().is_some_and(|extension| extension.eq_ignore_ascii_case("prim1room")) { path } else {
        let mut name = path.file_name().unwrap_or_default().to_os_string();
        name.push(".prim1room");
        path.with_file_name(name)
    };
    Ok(Some(path.display().to_string()))
}

/// Rooms → Load room…: the native open dialog only, for a room file or a
/// copied runtime's room catalog; `inspect_room_source` and `import_room` act.
#[tauri::command]
fn choose_room_source(app: AppHandle, state: State<'_, DesktopState>) -> Result<Option<String>, String> {
    if state.supervisor.trial_admission_held() {
        return Err(TRIAL_ADMISSION_HELD.into());
    }
    app.dialog().file().set_title("Load room")
        .add_filter("PRIM-1 room (*.prim1room)", &["prim1room"])
        .add_filter("PRIM-1 runtime copy (room-catalog-v1.json)", &["json"])
        .blocking_pick_file()
        .map(|path| path.into_path().map(|path| path.display().to_string())
            .map_err(|error| format!("selected file is not a local filesystem path: {error}")))
        .transpose()
}

#[tauri::command]
async fn export_room(state: State<'_, DesktopState>, room_id: RoomId, path: String) -> Result<RoomExportReport, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    let result = tauri::async_runtime::spawn_blocking(move || supervisor.export_room(room_id, Path::new(&path)))
        .await
        .map_err(|error| format!("room save worker failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()));
    match &result {
        Ok(report) => diagnostics.log("info", "room_exported",
            format!("room={room_id} records={} bytes={} paused_ms={}", report.records, report.bytes, report.paused_ms)),
        Err(error) => diagnostics.log("error", "export_room_failed", error.clone()),
    }
    result
}

/// The rooms a file or runtime copy offers, read only, for the confirm step.
#[tauri::command]
async fn inspect_room_source(state: State<'_, DesktopState>, path: String) -> Result<Vec<RoomSourceSummary>, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    let result = tauri::async_runtime::spawn_blocking(move || supervisor.inspect_room_source(Path::new(&path)))
        .await
        .map_err(|error| format!("room inspect worker failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()));
    if let Err(error) = &result {
        diagnostics.log("warn", "inspect_room_source_failed", error.clone());
    }
    result
}

/// Creates a new room with new members; nothing starts.
#[tauri::command]
async fn import_room(
    state: State<'_, DesktopState>,
    path: String,
    source_room_id: Option<RoomId>,
) -> Result<RoomImportReport, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    let result = tauri::async_runtime::spawn_blocking(move || supervisor.import_room(Path::new(&path), source_room_id))
        .await
        .map_err(|error| format!("room load worker failed: {error}"))
        .and_then(|result| result.map_err(|error| error.to_string()));
    match &result {
        Ok(report) => diagnostics.log("info", "room_imported",
            format!("room={} members={} warnings={}", report.room.room_id, report.members.len(), report.warnings.len())),
        Err(error) => diagnostics.log("error", "import_room_failed", error.clone()),
    }
    result
}

/// The read-only Tasks panel: GraphQL `{data, errors}` from a schema with no
/// mutation type. Agents change tasks through their pane `tasks` tool.
#[tauri::command]
async fn tasks_query(
    state: State<'_, DesktopState>,
    query: String,
    variables: Option<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let supervisor = state.supervisor.clone();
    tauri::async_runtime::spawn_blocking(move || supervisor.read_tasks(&query, variables.as_ref(), None))
        .await
        .map_err(|error| format!("task query worker failed: {error}"))
}

/// D9 step 5: admits the input and returns at once; the run's worker writes
/// it in order, so this sync command never holds the UI thread on a PTY write.
#[tauri::command]
fn send_input(state: State<'_, DesktopState>, request: SendInputRequest) -> Result<(), String> {
    state
        .supervisor
        .enqueue_operator_input(request)
        .map_err(|error| {
            state
                .diagnostics
                .log("error", "send_input_failed", error.to_string());
            error.to_string()
        })
}

#[tauri::command]
async fn route_message(
    state: State<'_, DesktopState>,
    request: OperatorRouteMessageRequest,
) -> Result<RuntimeSnapshot, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    diagnostics.log(
        "info",
        "route_message",
        format!(
            "recipient={} content_len={}",
            request.recipient_id,
            request.content.len()
        ),
    );
    match tauri::async_runtime::spawn_blocking(move || supervisor.route_operator_message(request))
        .await
    {
        Ok(Ok(snapshot)) => Ok(snapshot),
        Ok(Err(error)) => {
            diagnostics.log("error", "route_message_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("route worker failed: {error}");
            diagnostics.log("error", "route_message_failed", &message);
            Err(message)
        }
    }
}

/// Polled every 2 s while the telemetry line or page is on screen. Process
/// queries take milliseconds, so they run off the main thread.
#[tauri::command]
async fn telemetry_snapshot(state: State<'_, DesktopState>) -> Result<TelemetrySnapshot, String> {
    let supervisor = state.supervisor.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.telemetry_snapshot()).await {
        Ok(result) => result.map_err(|error| format!("{error:#}")),
        Err(error) => Err(format!("telemetry worker failed: {error}")),
    }
}

#[tauri::command]
fn resize_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    expected_run_id: RunId,
    expected_generation: SessionGeneration,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .supervisor
        .resize_session_for_run(session_id, expected_run_id, expected_generation, cols, rows)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "resize_session_failed",
                format!("{} {}x{}: {}", session_id, cols, rows, error),
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
    runtime_dir_override: Option<PathBuf>,
    cdp_port: Option<u16>,
    start_minimized: bool,
    trial: Option<Arc<TrialStartup>>,
    /// A108: the new build's one-time upgrade variables (both or neither).
    upgrade_startup: Result<Option<UpgradeStartup>, String>,
    /// `PRIM1_PERSONAL_REPO` as set (QA's `off` included), for a later upgrade.
    personal_repo_override: Option<String>,
}

impl StartupConfig {
    pub fn trial_webview_profile(&self) -> Option<&Path> {
        self.trial.as_ref().map(|trial| trial.webview_profile_dir())
    }

    pub fn cdp_port(&self) -> Option<u16> {
        self.cdp_port
    }

    /// The value this process gives [`WEBVIEW2_BROWSER_ARGUMENTS_ENV`] before
    /// its WebView exists. `None` means the variable must be removed: PRIM-1
    /// owns that switch, so a value inherited from the launching shell (every
    /// v1 pane exports one) can never turn remote debugging on.
    pub fn webview_browser_arguments(&self) -> Option<String> {
        self.cdp_port.map(cdp_browser_arguments)
    }
}

/// The process-wide switch WebView2 reads when the WebView is created. Panes
/// never inherit it (see `pty-host`).
pub const WEBVIEW2_BROWSER_ARGUMENTS_ENV: &str = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";

/// R9: remote debugging is an explicit, temporary QA mode, off by default.
/// Only this variable turns it on. v1's `PRIM1_CDP_PORT` is ignored on
/// purpose: the live v1 app exports it into every pane it hosts.
pub const DEBUG_CDP_PORT_ENV: &str = "PRIM1_DEBUG_CDP_PORT";

pub fn cdp_browser_arguments(port: u16) -> String {
    format!("--remote-debugging-port={port} --remote-debugging-address=127.0.0.1")
}

fn cdp_port_from_environment(
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<u16>, String> {
    parse_cdp_port(lookup(DEBUG_CDP_PORT_ENV))
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
    let runtime_dir_override =
        resolve_runtime_dir_override(std::env::var_os("PRIM1_RUNTIME_DIR"), &startup_cwd);
    let cdp_port = cdp_port_from_environment(|name| std::env::var_os(name))?;
    let start_minimized = parse_start_minimized(std::env::var_os("PRIM1_START_MINIMIZED"))?;
    let trial = trial_desktop::parse_settings(|name| std::env::var_os(name))?
        .map(|settings| TrialStartup::prepare(settings.attempt_id, &settings.root, settings.health_window)
            .map(Arc::new).map_err(|error| error.to_string()))
        .transpose()?;
    // An invalid pair is shown after startup and ignored: ordinary start,
    // nothing replayed.
    let upgrade_startup = supervisor::upgrade_flow::parse_upgrade_startup(
        std::env::var_os(supervisor::upgrade_flow::ATTEMPT_ENV),
        std::env::var_os(supervisor::upgrade_flow::REQUEST_FILE_ENV),
    );
    let personal_repo_override = std::env::var("PRIM1_PERSONAL_REPO")
        .ok()
        .filter(|value| !value.is_empty());

    Ok(StartupConfig {
        agent_working_root,
        environment_source,
        runtime_dir_override,
        cdp_port,
        start_minimized,
        trial,
        upgrade_startup,
        personal_repo_override,
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

fn resolve_runtime_dir_override(value: Option<OsString>, startup_cwd: &Path) -> Option<PathBuf> {
    non_empty_path(value).map(|path| resolve_against(&path, startup_cwd))
}

fn parse_cdp_port(value: Option<OsString>) -> Result<Option<u16>, String> {
    let Some(value) = value.filter(|value| !value.to_string_lossy().trim().is_empty()) else {
        return Ok(None);
    };
    let text = value.to_string_lossy();
    let invalid = || format!("{DEBUG_CDP_PORT_ENV} must be a non-zero TCP port");
    let port = text.parse::<u16>().map_err(|_| invalid())?;
    if port == 0 {
        return Err(invalid());
    }
    Ok(Some(port))
}

fn parse_start_minimized(value: Option<OsString>) -> Result<bool, String> {
    let Some(value) = value.filter(|value| !value.to_string_lossy().trim().is_empty()) else {
        return Ok(false);
    };
    match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "1" | "true" => Ok(true),
        "0" | "false" => Ok(false),
        _ => Err("PRIM1_START_MINIMIZED must be one of: 1, true, 0, false".into()),
    }
}

/// A window started minimized gets its deferred fullscreen only once the
/// operator has restored it: focused and no longer minimized.
fn operator_restored(focused: bool, minimized: bool) -> bool {
    focused && !minimized
}

fn present_main_window(window: &WebviewWindow, start_minimized: bool) -> Result<(), String> {
    // Show through Tauri so its window layer (tao) records the window as
    // visible. The previous raw Win32 `ShowWindow` bypassed that state: tao
    // still believed the window was hidden and re-hid it (SW_HIDE) whenever it
    // re-applied window state — every exit from fullscreen made the window
    // vanish. `hwnd()` is also not always available inside the setup hook,
    // which could panic the whole launch. With `focus: false` in the window
    // config this show does not activate the window.
    window.show().map_err(|error| error.to_string())?;
    if start_minimized {
        window.minimize().map_err(|error| error.to_string())?;
    } else {
        window
            .set_fullscreen(true)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn default_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|path| path.join("runtime"))
        .map_err(|error| format!("failed to resolve per-user application data: {error}"))
}

/// v3 W7 (Claude PRIM-1): what the person sees when this build stopped before
/// writing. The previous build restarts without its members, so it says so.
fn startup_refusal_text(reason: &str, restarted: bool) -> String {
    const MEMBERS: &str = "Your members are stopped: start each one again; each resumes its own conversation.";
    if restarted {
        format!("PRIM-1 could not start the new version and restarted your previous version. {MEMBERS}\n\nWhy: {reason}")
    } else {
        format!("PRIM-1 could not start the new version. Start your previous version again. {MEMBERS}\n\nWhy: {reason}")
    }
}

/// v3 W7: the reason the person sees; this happens before any window exists.
#[cfg(windows)]
fn show_startup_refusal(text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MB_ICONWARNING, MB_OK, MessageBoxW};
    let wide = |value: &str| value.encode_utf16().chain(std::iter::once(0)).collect::<Vec<u16>>();
    let (body, title) = (wide(text), wide("PRIM-1"));
    // SAFETY: NUL-terminated UTF-16 strings that outlive the call; no owner window.
    unsafe { MessageBoxW(std::ptr::null_mut(), body.as_ptr(), title.as_ptr(), MB_OK | MB_ICONWARNING); }
}

#[cfg(not(windows))]
fn show_startup_refusal(_text: &str) {}

fn validate_and_lock_desktop_runtime(
    requested_runtime_dir: &Path,
    working_root: &Path,
) -> Result<(PathBuf, DesktopInstanceLock), String> {
    let runtime_dir = validate_runtime_storage_paths(requested_runtime_dir, working_root)
        .map_err(|error| error.to_string())?;
    let instance_lock = DesktopInstanceLock::acquire(&runtime_dir)?;
    Ok((runtime_dir, instance_lock))
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
    trial: Option<&TrialStartup>,
) -> Result<SupervisorHandle, String> {
    let pane_mcp_executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate the PRIM-1 pane MCP executable: {error}"))?;
    let config = SupervisorConfig {
        working_root: agent_working_root,
        runtime_dir,
        pane_mcp_executable: Some(pane_mcp_executable),
        room_brief_on_join: true,
        heartbeat_interval: None,
        auto_restart_on_stall_sessions: None,
        auto_restart_stall_threshold: None,
        task_folder: None,
    };
    let supervisor = match trial {
        Some(trial) => SupervisorHandle::new_with_startup_admission(config, trial.admission()),
        None => SupervisorHandle::new(config),
    }
    .map_err(|error| error.to_string())?;
    diagnostics.initialize(supervisor.runtime_dir())?;
    diagnostics.log(
        "info",
        "supervisor_init",
        "storage=per_user_app_data session_authority=session_id",
    );

    let output_credit = supervisor.output_credit();
    output_credit.enable();
    let ui_event_tx = start_ui_event_bridge(
        app.clone(),
        diagnostics.clone(),
        supervisor.renderer_event_projector(),
        output_credit,
    );
    supervisor.set_event_sink(move |event| {
        let _ = ui_event_tx.enqueue(event);
    });
    let personal_repo = supervisor.personal_repo_status();
    diagnostics.log(
        if personal_repo.status == supervisor::PersonalRepoHealth::Error { "warn" } else { "info" },
        "personal_repo",
        format!(
            "source={:?} status={:?} folder={}{}",
            personal_repo.source,
            personal_repo.status,
            personal_repo.folder.as_deref().unwrap_or("-"),
            personal_repo.message.map(|message| format!(" message={message}")).unwrap_or_default()
        ),
    );
    // The Settings choice, else the default, opened before any pane can reach
    // the control plane. A held trial resolves the folder but never opens it.
    let tasks = supervisor.open_task_folder_from_setting();
    diagnostics.log(
        if tasks.error.is_none() { "info" } else { "warn" },
        "task_folder",
        format!(
            "source={:?} status={:?}{}",
            tasks.source,
            tasks.status,
            tasks.error.map(|error| format!(" error={error}")).unwrap_or_default()
        ),
    );
    if trial.is_none() {
        let control_plane = supervisor.start_control_plane().map_err(|error| {
            diagnostics.log("error", "control_plane_start_failed", error.to_string());
            error.to_string()
        })?;
        diagnostics.log("info", "control_plane_ready", format!("endpoint={}", control_plane.endpoint));
        // A106: room messages still waiting for a member before the restart.
        let resumed = supervisor.resume_room_wake_queue();
        diagnostics.log("info", "room_wake_queue_resumed", format!("queued={resumed}"));
    }

    Ok(supervisor)
}

fn start_ui_event_bridge(
    app: AppHandle,
    diagnostics: DesktopDiagnostics,
    projector: RendererEventProjector,
    credit: Arc<OutputCreditLedger>,
) -> UiEventIngress {
    let (mut ingress, rx, ingress_state) = ui_event_channel(UI_EVENT_QUEUE_CAPACITY);
    ingress.credit = Some(credit.clone());

    thread::spawn(move || {
        let mut batcher = PendingOutputBatcher::default();

        loop {
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(queued) => {
                    let now = Instant::now();
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush_expired(now), &credit);
                    if !queued.output_gaps_before.is_empty() {
                        emit_pending_session_outputs(&app, &diagnostics, batcher.flush(), &credit);
                        emit_ui_output_gaps(&app, &diagnostics, queued.output_gaps_before);
                    }
                    let event = queued.event;
                    match event {
                        shared_types::RuntimeEvent::SessionOutput {
                            identity,
                            session,
                            chunk,
                            synthetic,
                            timestamp,
                        } => {
                            emit_pending_session_outputs(
                                &app,
                                &diagnostics,
                                batcher.push(
                                    PendingSessionOutput {
                                        credit: queued.credit,
                                        identity,
                                        session,
                                        chunk,
                                        synthetic,
                                        timestamp,
                                    },
                                    now,
                                ),
                                &credit,
                            );
                        }
                        event => {
                            emit_pending_session_outputs(&app, &diagnostics, batcher.flush(), &credit);
                            emit_runtime_event(&app, &diagnostics, projector.project(event));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush(), &credit);
                    emit_ui_output_gaps(&app, &diagnostics, ingress_state.take_reportable_output_gaps(&credit));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush(), &credit);
                    emit_ui_output_gaps(&app, &diagnostics, ingress_state.take_all_output_gaps());
                    break;
                }
            }
        }
    });

    ingress
}

fn ui_output_gap_message(gap: &UiOutputGap) -> String {
    if gap.observer_only {
        return format!(
            "UI output gap: omitted {} display event(s) for session '{}' run {} (generation {}, through sequence {}) while renderer credit was unavailable after 2 seconds without progress or the 5-second wait ceiling. Native observation and control-string parsing continued. Display is incomplete; repaint cannot restore lost history or guarantee screen parity.",
            gap.dropped_events, gap.session, gap.identity.run_id, gap.identity.generation, gap.identity.sequence,
        );
    }
    format!(
        "UI output gap: dropped {} sanitized display event(s) for session '{}' run {} (latest generation {}, sequence {}) under bounded-queue pressure. Terminal control parsing remained synchronized; visible output is incomplete.",
        gap.dropped_events,
        gap.session,
        gap.identity.run_id,
        gap.identity.generation,
        gap.identity.sequence,
    )
}

fn ui_output_gap_runtime_event(gap: &UiOutputGap) -> shared_types::RuntimeEvent {
    shared_types::RuntimeEvent::SystemLog {
        level: shared_types::LogLevel::Warn,
        message: ui_output_gap_message(gap),
        timestamp: shared_types::now_rfc3339(),
    }
}

fn emit_ui_output_gaps(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    gaps: impl IntoIterator<Item = UiOutputGap>,
) {
    for gap in gaps {
        let event = ui_output_gap_runtime_event(&gap);
        if let shared_types::RuntimeEvent::SystemLog { message, .. } = &event {
            diagnostics.log("warn", "ui_output_bridge_gap", message.clone());
        }
        emit_runtime_event(app, diagnostics, event);
        // Structured companion: lets the renderer heal the pane (full-repaint
        // resync) instead of leaving a diff-painted terminal truncated.
        emit_runtime_event(
            app,
            diagnostics,
            shared_types::RuntimeEvent::UiOutputGap {
                identity: gap.identity,
                session: gap.session.clone(),
                dropped_events: gap.dropped_events,
                timestamp: shared_types::now_rfc3339(),
            },
        );
    }
}

fn queue_pending_session_output(
    pending: &mut Option<PendingSessionOutput>,
    output: PendingSessionOutput,
) -> Option<PendingSessionOutput> {
    match pending.take() {
        Some(mut buffer) if pending_outputs_are_contiguous(&buffer, &output) => {
            buffer.chunk.push_str(&output.chunk);
            buffer.synthetic &= output.synthetic;
            buffer.timestamp = output.timestamp;
            buffer.identity = output.identity;
            if let (Some(buffer_credit), Some(next_credit)) = (&mut buffer.credit, output.credit) {
                buffer_credit.tickets.extend(next_credit.tickets);
            }
            *pending = Some(buffer);
            None
        }
        displaced => {
            if !output.chunk.is_empty() {
                *pending = Some(output);
            }
            displaced
        }
    }
}

fn pending_outputs_are_contiguous(
    buffered: &PendingSessionOutput,
    next: &PendingSessionOutput,
) -> bool {
    buffered.session == next.session
        && match (&buffered.credit, &next.credit) {
            (None, None) => true,
            (Some(a), Some(b)) => a.epoch == b.epoch && a.run == b.run,
            _ => false,
        }
        && RunOutputKey::from(buffered.identity) == RunOutputKey::from(next.identity)
        && buffered.identity.generation == next.identity.generation
        && buffered.identity.sequence.checked_add(1) == Some(next.identity.sequence)
}

fn emit_pending_session_output(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    buffer: PendingSessionOutput,
    ledger: &OutputCreditLedger,
) {
    if let Some(credit) = &buffer.credit
        && !ledger.dispatch(credit)
    {
        ledger.discard_native(credit);
        return;
    }
    #[derive(serde::Serialize, Clone)]
    struct OutputEnvelope {
        #[serde(flatten)]
        event: shared_types::RuntimeEvent,
        #[serde(skip_serializing_if = "Option::is_none")]
        output_credit: Option<OutputCredit>,
    }
    let envelope = OutputEnvelope {
        event: shared_types::RuntimeEvent::SessionOutput {
            identity: buffer.identity,
            session: buffer.session,
            chunk: buffer.chunk,
            synthetic: buffer.synthetic,
            timestamp: buffer.timestamp,
        },
        output_credit: buffer.credit,
    };
    if let Err(error) = app.emit("runtime://event", &envelope) {
        if let Some(credit) = &envelope.output_credit { ledger.discard_native(credit); }
        diagnostics.log("error", "display_output_emit_failed", format!("{error}; output discarded, not parsed; reload resets display"));
        // A successful later control emission must not hide a lost display
        // batch. This is an explicit gap, never a synthetic parser ACK.
        emit_runtime_event(app, diagnostics, shared_types::RuntimeEvent::SystemLog {
            level: shared_types::LogLevel::Error,
            message: "Terminal display output could not be delivered and was discarded without parsing. The display is incomplete; reload resets it but cannot recover lost scrollback.".into(),
            timestamp: shared_types::now_rfc3339(),
        });
    }
}

fn emit_pending_session_outputs(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    outputs: impl IntoIterator<Item = PendingSessionOutput>,
    ledger: &OutputCreditLedger,
) {
    for buffer in outputs {
        emit_pending_session_output(app, diagnostics, buffer, ledger);
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

/// A16: Windows ends processes from the highest shutdown level down. 0x380,
/// in the applications' "shut down first" range (0x300-0x3FF) and above the
/// 0x280 default that the agent CLIs and their console hosts keep, ends
/// PRIM-1 before its agents, so their runs are still alive when the
/// session-end checkpoint records them. Not 0x3FF: that top value is left to
/// programs that must go before everything else.
#[cfg(windows)]
const PRIM1_SHUTDOWN_LEVEL: u32 = 0x380;
/// `SHUTDOWN_NORETRY` (WindowsProgramming): a slow end is cut short, never
/// answered with a retry dialog that holds up the OS.
#[cfg(windows)]
const SHUTDOWN_NORETRY: u32 = 0x1;

#[cfg(windows)]
fn set_shutdown_level(diagnostics: &DesktopDiagnostics) {
    use windows_sys::Win32::System::Threading::SetProcessShutdownParameters;
    if unsafe { SetProcessShutdownParameters(PRIM1_SHUTDOWN_LEVEL, SHUTDOWN_NORETRY) } == 0 {
        diagnostics.log("warn", "shutdown_level", format!(
            "SetProcessShutdownParameters(0x{PRIM1_SHUTDOWN_LEVEL:x}) failed: {}", std::io::Error::last_os_error()));
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run(startup: StartupConfig) {
    let diagnostics = DesktopDiagnostics::uninitialized();
    let setup_diagnostics = diagnostics.clone();
    let setup_startup = startup.clone();
    let mut context = tauri::generate_context!();
    let trial_window = startup.trial.as_ref().map(|_| trial_desktop::take_trial_main_config(context.config_mut()))
        .transpose().unwrap_or_else(|error| panic!("trial window configuration: {error}"));
    // T-36: a held trial's window is never rebuilt (its renderer acknowledgement
    // belongs to the first WebView).
    let main_window_config = startup.trial.is_none()
        .then(|| context.config().app.windows.iter().find(|window| window.label == "main").cloned())
        .flatten();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let requested_runtime_dir = match setup_startup.trial.as_ref() {
                Some(trial) => {
                    let local = app.path().local_data_dir().map_err(|error| error.to_string())?;
                    let root = trial.runtime_dir().parent().ok_or("trial runtime has no parent")?;
                    trial_desktop::validate_known_local_root(root, &local)?;
                    trial.runtime_dir().to_path_buf()
                },
                None => match &setup_startup.runtime_dir_override {
                    Some(runtime_dir) => runtime_dir.clone(),
                    None => default_runtime_dir(app.handle())?,
                },
            };
            let (runtime_dir, instance_lock) = validate_and_lock_desktop_runtime(
                &requested_runtime_dir,
                &setup_startup.agent_working_root,
            )?;
            // v3 W7: before anything writes, keep a verified copy of a runtime an
            // earlier build wrote; a v2 build cannot read room history v3 writes.
            // A refusal is shown, and the build this one replaced starts again.
            let instance_lock = match supervisor::runtime_snapshot::ensure_pre_v3_snapshot_now(&runtime_dir) {
                Ok(_) => instance_lock,
                Err(error) => {
                    let reason = format!("{error:#}");
                    setup_diagnostics.log("error", "runtime_snapshot_refused", &reason);
                    drop(instance_lock);
                    let restarted = supervisor::runtime_snapshot::restart_replaced_build(&runtime_dir);
                    if let Some(source) = &restarted {
                        setup_diagnostics.log("info", "replaced_build_restarted", &source.display().to_string());
                    }
                    let text = startup_refusal_text(&reason, restarted.is_some());
                    // Test instances start minimized and must never take focus.
                    if !setup_startup.start_minimized { show_startup_refusal(&text); }
                    return Err(reason.into());
                }
            };
            // Supervisor construction remains the next stateful step. It hardens
            // the runtime ACL before audit or control-plane state is created.
            let supervisor = init_supervisor(
                app.handle(),
                runtime_dir,
                setup_startup.agent_working_root.clone(),
                &setup_diagnostics,
                setup_startup.trial.as_deref(),
            )?;
            install_panic_hook(setup_diagnostics.clone());
            #[cfg(windows)]
            set_shutdown_level(&setup_diagnostics);
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
            if let Some(trial) = &setup_startup.trial {
                trial.record_backend_initialized(&supervisor).map_err(|error| error.to_string())?;
            }
            app.manage(DesktopState {
                supervisor,
                trial: setup_startup.trial.clone(),
                diagnostics: setup_diagnostics.clone(),
                automation_mode: setup_startup.cdp_port.is_some(),
                shutdown_started: AtomicBool::new(false),
                shutdown_succeeded: AtomicBool::new(false),
                fullscreen_on_first_focus: AtomicBool::new(setup_startup.start_minimized),
                main_window_config,
                ui_rebuilding: AtomicBool::new(false),
                rebuild_minimized: AtomicBool::new(false),
                renderer_recovery: Mutex::new(RendererRecoveryLedger::default()),
                _instance_lock: instance_lock,
            });
            if setup_startup.trial.is_none() {
                install_upgrade(app.handle(), &setup_diagnostics, &setup_startup);
            }
            if let (Some(config), Some(trial)) = (&trial_window, &setup_startup.trial) {
                tauri::WebviewWindowBuilder::from_config(app, config)?
                    .data_directory(trial.webview_profile_dir().to_path_buf())
                    .build()?;
            }
            let main_window = app
                .get_webview_window("main")
                .ok_or_else(|| "missing main window".to_string())?;
            watch_renderer(&main_window, &setup_diagnostics);
            present_main_window(&main_window, setup_startup.start_minimized)?;
            if setup_startup.start_minimized {
                setup_diagnostics.log(
                    "info",
                    "window_ready",
                    "main window presented minimized without activation; fullscreen deferred until operator restore",
                );
            } else {
                setup_diagnostics.log("info", "window_ready", "main window presented fullscreen");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            attachments::save_room_attachment,
            attachments::read_room_attachment,
            georgi::georgi_request_attention,
            georgi::georgi_copy_session_folder,
            georgi::georgi_take_layout,
            georgi::georgi_finish_layout,
            georgi::georgi_context_usage,
            automation_mode,
            trial_status,
            trial_renderer_acknowledged,
            bootstrap,
            discover_local_sessions,
            attach_local_session,
            begin_output_renderer,
            output_renderer_ready,
            acknowledge_output,
            output_credit_metrics,
            discard_output,
            start_session,
            resume_session_after_restart,
            preview_session_launch,
            stop_session,
            resume_held_conversation,
            restart_session,
            choose_workspace_directory,
            create_session,
            prime_enabled,
            prime_default_working_directory,
            rename_session,
            choose_session_working_directory,
            set_session_linux_working_directory,
            get_task_folder,
            set_task_folder,
            choose_task_folder,
            get_personal_repo,
            set_personal_repo,
            choose_personal_repo,
            confirm_action,
            choose_room_save_path,
            choose_room_source,
            export_room,
            inspect_room_source,
            import_room,
            tasks_query,
            set_session_permission_profile,
            set_member_permissions,
            upgrade_status,
            begin_upgrade_resume,
            cancel_upgrade,
            abandon_held_upgrade,
            report_room_drafts,
            move_session,
            delete_session,
            create_room,
            default_room_brief,
            rename_room,
            set_room_resume_on_restart,
            read_room_brief,
            update_room_brief,
            get_room_fallbacks,
            set_room_fallbacks,
            move_room,
            add_room_member,
            remove_room_member,
            delete_room,
            read_room_feed,
            post_room_message,
            deliver_room_message,
            retry_room_delivery,
            dispose_room_message,
            cancel_room_message,
            brief_room_member,
            invite_room_guest,
            revoke_room_guest,
            list_room_guests,
            send_input,
            route_message,
            telemetry_snapshot,
            resize_session,
            toggle_fullscreen
        ])
        .build(context)
        .unwrap_or_else(|error| panic!("error while building tauri application: {error}"));

    app.run(|handle, event| match event {
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::CloseRequested { api, .. },
            ..
        } if label == "main" => {
            if let Some(state) = handle.try_state::<DesktopState>() {
                close_main_window_gracefully(
                    || api.prevent_close(),
                    || state.shutdown_once("main_window_close_requested"),
                    |exit_code| handle.exit(exit_code),
                );
            }
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Focused(true) | tauri::WindowEvent::Resized(_),
            ..
        } if label == "main" => {
            // The deferred fullscreen waits for the operator's restore: a
            // focus that reaches the window while it is still minimized (an
            // accessibility Invoke, a page focus request) is not one, and
            // tao activates the window to apply fullscreen (Claude PRIM-1,
            // 26a7f92 walk). A restore arrives as a focus or a resize.
            if let Some(state) = handle.try_state::<DesktopState>()
                && state.fullscreen_on_first_focus.load(Ordering::SeqCst)
                && let Some(window) = handle.get_webview_window("main")
                && operator_restored(
                    window.is_focused().unwrap_or(false),
                    window.is_minimized().unwrap_or(true),
                )
                && state
                    .fullscreen_on_first_focus
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                match window.set_fullscreen(true) {
                    Ok(()) => state.diagnostics.log(
                        "info",
                        "window_restored",
                        "main window restored fullscreen by operator restore",
                    ),
                    Err(error) => {
                        state
                            .diagnostics
                            .log("error", "window_restore_failed", error.to_string())
                    }
                }
            }
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } if label == "main" => {
            if handle.try_state::<DesktopState>().is_some_and(|state| state.ui_rebuilding.load(Ordering::SeqCst)) {
                finish_main_window_rebuild(handle);
            }
        }
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            if let Some(state) = handle.try_state::<DesktopState>() {
                if exit_request_is_ui_rebuild(code, state.ui_rebuilding.load(Ordering::SeqCst)) {
                    api.prevent_exit();
                    state.diagnostics.log("info", "ui_rebuild_exit_prevented", "no main window while it is rebuilt");
                } else {
                    state.shutdown_once("app_exit_requested");
                }
            }
        }
        tauri::RunEvent::Exit => {
            if let Some(state) = handle.try_state::<DesktopState>() {
                // Runs first in tauri's Exit, inside WM_ENDSESSION, before any
                // cleanup that could pump messages, so the second window's
                // WM_ENDSESSION (tao's "cannot move state from Destroyed"
                // panic) cannot arrive before the checkpoint is written.
                state.end_session_unless_shut_down();
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
    /// Claude PRIM-1, 26a7f92 walk (findings 2-3): a focus reaching a still
    /// minimized window is not the operator's restore, so it neither applies
    /// the deferred fullscreen nor spends it.
    #[test]
    fn the_deferred_fullscreen_waits_for_a_focused_restored_window() {
        assert!(super::operator_restored(true, false));
        assert!(!super::operator_restored(true, true), "focused while minimized");
        assert!(!super::operator_restored(false, false), "restored but another window has focus");
        assert!(!super::operator_restored(false, true));
    }

    /// Claude PRIM-1 (after the pre-write failure leg): the refusal says the
    /// previous version is back and that its members must be started again.
    #[test]
    fn the_startup_refusal_says_the_previous_version_restarted_and_the_members_are_stopped() {
        assert_eq!(super::startup_refusal_text("no room for the copy", true),
            "PRIM-1 could not start the new version and restarted your previous version. Your members are stopped: \
             start each one again; each resumes its own conversation.\n\nWhy: no room for the copy");
        let alone = super::startup_refusal_text("no room for the copy", false);
        assert!(alone.starts_with("PRIM-1 could not start the new version. Start your previous version again. \
            Your members are stopped:") && alone.ends_with("Why: no room for the copy"), "{alone}");
    }

    #[test]
    fn t36_a_failed_renderer_is_recovered_within_a_budget_and_a_dead_browser_rebuilds_the_window() {
        use super::{RendererFailure as F, RendererRecovery as R, RendererRecoveryLedger};
        let start = std::time::Instant::now();
        let mut ledger = RendererRecoveryLedger::default();
        // A dead browser process (PC3, 20 Oct) rebuilds the window; a dead page reloads.
        assert_eq!(ledger.decide(F::BrowserExited, true, start), R::Rebuild);
        assert_eq!(ledger.decide(F::RendererExited, true, start), R::Reload);
        // An unresponsive page and helper processes are logged only, and spend nothing.
        for _ in 0..10 {
            assert_eq!(ledger.decide(F::RendererUnresponsive, true, start), R::Wait);
            assert_eq!(ledger.decide(F::Other, true, start), R::Wait);
        }
        assert_eq!(ledger.decide(F::RendererExited, true, start), R::Reload, "the third recovery");
        // A renderer that dies on every load cannot spin: the fourth in ten minutes gives up.
        let minute = std::time::Duration::from_secs(60);
        assert_eq!(ledger.decide(F::RendererExited, true, start + minute), R::GiveUp);
        assert_eq!(ledger.decide(F::BrowserExited, true, start + 9 * minute), R::GiveUp);
        // Once the oldest recoveries are ten minutes old, the budget is back.
        assert_eq!(ledger.decide(F::BrowserExited, true, start + 10 * minute), R::Rebuild);
        // A held trial's window is never rebuilt.
        assert_eq!(RendererRecoveryLedger::default().decide(F::BrowserExited, false, start), R::GiveUp);
    }

    #[test]
    fn t36_only_the_window_less_moment_of_a_rebuild_is_kept_from_exiting() {
        assert!(super::exit_request_is_ui_rebuild(None, true));
        assert!(!super::exit_request_is_ui_rebuild(None, false), "closing the last window still exits");
        assert!(!super::exit_request_is_ui_rebuild(Some(0), true), "an explicit exit is never held back");
        assert!(!super::exit_request_is_ui_rebuild(Some(1), false));
    }

    #[test]
    fn folder_pickers_open_at_the_current_folder_or_its_nearest_existing_parent() {
        let root = unique_temp_dir("picker-start");
        let existing = root.join("tasks");
        std::fs::create_dir_all(&existing).expect("create tasks");
        let existing_text = existing.display().to_string();
        assert_eq!(super::picker_start(Some(&existing_text)), Some(existing.clone()));
        let removed = existing.join("gone").join("deeper").display().to_string();
        assert_eq!(super::picker_start(Some(&removed)), Some(existing));
        // A trailing non-breaking space is part of the folder name, not padding.
        let spaced = root.join("tasks\u{a0}");
        std::fs::create_dir_all(&spaced).expect("create spaced");
        let spaced_text = spaced.display().to_string();
        assert_eq!(super::picker_start(Some(&spaced_text)), Some(spaced));
        assert_eq!(super::picker_start(Some("  ")), None);
        assert_eq!(super::picker_start(None), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn guest_invitation_config_runs_this_executable_in_guest_mode_with_the_token() {
        let executable = std::path::PathBuf::from(r"C:\Program Files\PRIM-1 v2\O'Neil.exe");
        let runtime = std::path::PathBuf::from(r"C:\Users\x\AppData\Local\io.prim1.v2\runtime");
        let config: serde_json::Value =
            serde_json::from_str(&supervisor::guest_mcp_config_json(&executable, &runtime, "prim1g_abc")).unwrap();
        let server = &config["mcpServers"]["prim1_room"];
        assert_eq!(server["command"], r"C:\Program Files\PRIM-1 v2\O'Neil.exe");
        assert_eq!(server["args"], serde_json::json!(["--prim1-guest-mcp"]));
        assert_eq!(server["env"]["PRIM1_GUEST_TOKEN"], "prim1g_abc");
        assert_eq!(server["env"]["PRIM1_RUNTIME_DIR"], runtime.display().to_string());
        let cli = supervisor::guest_cli_example(&executable, &runtime, "prim1g_abc");
        assert!(cli.contains("$env:PRIM1_GUEST_TOKEN = 'prim1g_abc'"), "{cli}");
        assert!(cli.contains(r"& 'C:\Program Files\PRIM-1 v2\O''Neil.exe' --prim1-guest rooms"), "{cli}");
    }

    #[test]
    fn a16_a_delete_waiting_for_a_finishing_stop_is_not_logged_as_a_failure() {
        assert!(super::delete_refusal_is_transient(
            "session 'x' must be fully closed before this operation: a Stop lifecycle operation in progress (generation 2) is still finishing; try again in a moment"));
        assert!(super::delete_refusal_is_transient("session 'x' is still draining its event stream; retry deletion"));
        assert!(!super::delete_refusal_is_transient("session 'x' belongs to room 'y'; remove it from the room before deletion"));
    }

    #[test]
    fn a16_the_agent_invitation_is_one_block_an_agent_can_join_and_take_part_with() {
        let executable = std::path::PathBuf::from(r"C:\Program Files\PRIM-1 v2\O'Neil.exe");
        let runtime = std::path::PathBuf::from(r"C:\Users\x\AppData\Local\io.prim1.v2\runtime");
        let block = supervisor::guest_agent_invitation("PC3 Demo", "Grok helper", &executable, &runtime, "prim1g_abc");
        // Who, where and for how long.
        assert!(block.starts_with("You are invited into the PRIM-1 room \"PC3 Demo\" as the outside agent \"Grok helper\"."), "{block}");
        assert!(block.contains("has no expiry: it ends when the operator revokes it"), "{block}");
        // Where it can work, stated rather than implied.
        assert!(block.contains("only for an agent running on this PC as the same Windows user, outside PRIM-1"), "{block}");
        assert!(block.contains("a cloud agent or another PC cannot reach it"), "{block}");
        // One self-contained command line: both variables, the quoted executable, piped.
        let line = block.lines().find(|line| line.starts_with("$env:PRIM1_GUEST_TOKEN")).expect("the command line");
        assert_eq!(line, format!(r"$env:PRIM1_GUEST_TOKEN = 'prim1g_abc'; $env:PRIM1_RUNTIME_DIR = '{}'; & 'C:\Program Files\PRIM-1 v2\O''Neil.exe' --prim1-guest <command> | Out-String",
            runtime.display()));
        // Every command the guest CLI parses, with the cursor as two arguments;
        // the first read is bare (the Reviewer, a5bf07a: rooms' cursor is the
        // tail and would skip what was posted since the invitation).
        // v3 W5 (decision 11): profile first; status, ack and directory follow.
        for command in ["1. profile 'a short name':", "2. rooms: your room, its members and other guests.",
            "3. read: the first time with no cursor, everything posted since you were invited.",
            "read --epoch <epoch> --sequence <sequence>.", "4. wait --epoch <epoch> --sequence <sequence>:",
            "5. post --request-id <a new id> 'your message':", "6. status:",
            "ack <message-id> --next 'what you will do next'", "ack <message-id> --decline 'why not'", "7. directory:",
            // v3 W5 (Reviewer 6489d3b6): the retained page's continuation, as the MCP read says.
            "read --owed-after <epoch>:<sequence> taken from retained_next"] {
            assert!(block.contains(command), "{command}: {block}");
        }
        assert!(!block.contains("current cursor"), "rooms is discovery, never where reading starts: {block}");
        // How to take part, and the trust boundary both ways.
        for rule in ["there are no private messages", "start with its name and quote the line you answer",
            "never as the operator's", "not instructions from the operator", "at most 32 KiB", "Never print or repeat its token"] {
            assert!(block.contains(rule), "{rule}: {block}");
        }
        // The MCP alternative is the same config, kept out of the panes' configuration.
        assert!(block.ends_with(&supervisor::guest_mcp_config_json(&executable, &runtime, "prim1g_abc")), "{block}");
        assert!(block.contains(r"never to one PRIM-1's panes also read (Codex's %USERPROFILE%\.codex\config.toml"), "{block}");
    }

    use super::{
        DEBUG_CDP_PORT_ENV, DESKTOP_INSTANCE_ALREADY_RUNNING, DESKTOP_INSTANCE_LOCK_FILE,
        DesktopDiagnostics, DesktopInstanceLock, MAX_UI_OUTPUT_BATCH_AGE,
        MAX_UI_OUTPUT_BATCH_BYTES, PendingOutputBatcher, PendingSessionOutput,
        RendererTerminalStream, StartupConfig, TerminalOutputSanitizer, UI_EVENT_QUEUE_CAPACITY,
        UiEventEnqueueResult, UiOutputGap, WEBVIEW2_BROWSER_ARGUMENTS_ENV, cdp_browser_arguments,
        cdp_port_from_environment, close_main_window_gracefully,
        normalize_path_for_child_processes, parse_cdp_port, parse_start_minimized,
        queue_pending_session_output, resolve_agent_working_root_from, resolve_environment_source,
        resolve_runtime_dir_override, sanitize_terminal_output_for_ui, ui_event_channel,
        ui_output_gap_runtime_event, validate_and_lock_desktop_runtime,
        validate_runtime_storage_paths,
    };
    use shared_types::{
        LifecycleState, LogLevel, OperatorRouteMessageRequest, RunEventIdentity, RuntimeEvent,
        SessionId,
    };
    #[cfg(windows)]
    use std::path::Path;
    use std::{
        collections::HashMap,
        ffi::OsString,
        fs,
        path::PathBuf,
        sync::{Mutex, mpsc},
        thread,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    fn run_event_identity(
        session_id: &str,
        run_id: &str,
        generation: u64,
        sequence: u64,
    ) -> RunEventIdentity {
        serde_json::from_value(serde_json::json!({
            "session_id": session_id,
            "run_id": run_id,
            "generation": generation,
            "sequence": sequence,
        }))
        .expect("valid test run identity")
    }

    fn pending_output(
        identity: RunEventIdentity,
        session: &str,
        chunk: &str,
    ) -> PendingSessionOutput {
        PendingSessionOutput {
            identity,
            session: session.into(),
            chunk: chunk.into(),
            synthetic: false,
            timestamp: format!("sequence-{}", identity.sequence),
            credit: None,
        }
    }

    #[test]
    fn desktop_diagnostics_serializes_concurrent_jsonl_writes() {
        let runtime_dir = unique_temp_dir("desktop-diagnostics-concurrency");
        fs::create_dir_all(&runtime_dir).expect("create diagnostics runtime directory");
        let diagnostics = DesktopDiagnostics::uninitialized();
        diagnostics
            .initialize(&runtime_dir)
            .expect("initialize diagnostics");

        let writers = (0..8)
            .map(|writer| {
                let diagnostics = diagnostics.clone();
                thread::spawn(move || {
                    for sequence in 0..128 {
                        diagnostics.log(
                            "info",
                            "concurrent_test",
                            format!("writer={writer} sequence={sequence}"),
                        );
                    }
                })
            })
            .collect::<Vec<_>>();
        for writer in writers {
            writer.join().expect("diagnostics writer panicked");
        }

        let log = fs::read_to_string(runtime_dir.join("desktop-events.jsonl"))
            .expect("read diagnostics log");
        let lines = log.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 8 * 128);
        for line in lines {
            let entry: serde_json::Value =
                serde_json::from_str(line).expect("every diagnostic line must be valid JSON");
            assert_eq!(entry["event"], "concurrent_test");
        }

        fs::remove_dir_all(runtime_dir).expect("remove diagnostics runtime directory");
    }

    fn ui_output_event(sequence: u64) -> RuntimeEvent {
        ui_output_event_with_chunk(sequence, &format!("output-{sequence}"))
    }

    fn ui_output_event_with_chunk(sequence: u64, chunk: &str) -> RuntimeEvent {
        RuntimeEvent::SessionOutput {
            identity: run_event_identity(
                "11111111-1111-1111-1111-111111111111",
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                7,
                sequence,
            ),
            session: "claude".into(),
            chunk: chunk.into(),
            synthetic: false,
            timestamp: "test-time".into(),
        }
    }

    fn ui_lifecycle_event(sequence: u64) -> RuntimeEvent {
        RuntimeEvent::SessionState {
            identity: run_event_identity(
                "11111111-1111-1111-1111-111111111111",
                "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                7,
                sequence,
            ),
            session: "claude".into(),
            state: LifecycleState::Ready,
            reason: "test lifecycle".into(),
            timestamp: "test-time".into(),
        }
    }

    fn ui_system_message(event: &RuntimeEvent) -> &str {
        match event {
            RuntimeEvent::SystemLog { message, .. } => message,
            event => panic!("expected system-log event, got {event:?}"),
        }
    }

    fn ui_output_chunk(event: &RuntimeEvent) -> &str {
        match event {
            RuntimeEvent::SessionOutput { chunk, .. } => chunk,
            event => panic!("expected session-output event, got {event:?}"),
        }
    }

    #[test]
    fn main_window_close_prevents_destruction_before_shutdown_and_explicit_exit() {
        let steps = Mutex::new(Vec::new());

        close_main_window_gracefully(
            || steps.lock().unwrap().push("prevent_close"),
            || {
                steps.lock().unwrap().push("shutdown");
                true
            },
            |exit_code| {
                assert_eq!(exit_code, 0);
                steps.lock().unwrap().push("request_exit");
            },
        );

        assert_eq!(
            *steps.lock().unwrap(),
            ["prevent_close", "shutdown", "request_exit"]
        );
    }

    #[test]
    fn main_window_close_requests_nonzero_exit_after_shutdown_failure() {
        let exit_code = Mutex::new(None);

        close_main_window_gracefully(
            || {},
            || false,
            |code| *exit_code.lock().unwrap() = Some(code),
        );

        assert_eq!(*exit_code.lock().unwrap(), Some(1));
    }

    #[test]
    fn bounded_ui_event_queue_has_an_exact_512_event_limit() {
        let (ingress, receiver, state) = ui_event_channel(UI_EVENT_QUEUE_CAPACITY);

        for sequence in 1..=UI_EVENT_QUEUE_CAPACITY {
            assert_eq!(
                ingress.enqueue(ui_output_event(sequence as u64)),
                UiEventEnqueueResult::Enqueued
            );
        }
        assert_eq!(
            ingress.enqueue(ui_output_event((UI_EVENT_QUEUE_CAPACITY + 1) as u64)),
            UiEventEnqueueResult::DroppedOutputFull
        );

        let queued = receiver.try_iter().collect::<Vec<_>>();
        assert_eq!(queued.len(), UI_EVENT_QUEUE_CAPACITY);
        assert_eq!(ui_output_chunk(&queued[0].event), "output-1");
        assert_eq!(
            ui_output_chunk(&queued[UI_EVENT_QUEUE_CAPACITY - 1].event),
            format!("output-{UI_EVENT_QUEUE_CAPACITY}")
        );
        let gaps = state.take_all_output_gaps();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].dropped_events, 1);
        assert_eq!(
            gaps[0].identity.sequence,
            (UI_EVENT_QUEUE_CAPACITY + 1) as u64
        );
    }

    fn credited_ui_channel() -> (super::UiEventIngress, super::UiEventReceiver, std::sync::Arc<super::OutputCreditLedger>) {
        let ledger = std::sync::Arc::new(super::OutputCreditLedger::default());
        ledger.enable();
        let reset = ledger.begin_renderer();
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        let (mut ingress, receiver, _) = ui_event_channel(1);
        ingress.credit = Some(ledger.clone());
        if let RuntimeEvent::SessionOutput { identity, .. } = ui_output_event(1) {
            ledger.open_run(identity.into());
        }
        (ingress, receiver, ledger)
    }

    fn enqueue_credited(ingress: &super::UiEventIngress, ledger: &std::sync::Arc<super::OutputCreditLedger>, event: RuntimeEvent) -> UiEventEnqueueResult {
        if let RuntimeEvent::SessionOutput { identity, chunk, .. } = &event {
            match ledger.reserve((*identity).into(), chunk.len()) {
                supervisor::output_credit::Admission::Reserved(lease) => lease.publish(*identity),
                _ => panic!("fixture has reserved capacity"),
            }
        } else { panic!("output fixture"); }
        ingress.enqueue(event)
    }

    #[test]
    fn output_credit_reload_replays_tracked_modes_without_an_in_flight_credit() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        let first = ui_output_event_with_chunk(1, "\x1b[?1049h\x1b[?1hmenu");
        assert_eq!(enqueue_credited(&ingress, &ledger, first), UiEventEnqueueResult::Enqueued);
        let shown = receiver.try_recv().unwrap();
        assert_eq!(ui_output_chunk(&shown.event), "\x1b[?1049h\x1b[?1hmenu", "first page shows output as written");
        assert!(ledger.dispatch(shown.credit.as_ref().unwrap()));
        ledger.acknowledge(shown.credit.as_ref().unwrap()).unwrap();
        // Page reload: new epoch, nothing in flight.
        let reset = ledger.begin_renderer();
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        let next = ui_output_event_with_chunk(2, "after");
        assert_eq!(enqueue_credited(&ingress, &ledger, next), UiEventEnqueueResult::Enqueued);
        let replayed = receiver.try_recv().unwrap();
        let chunk = ui_output_chunk(&replayed.event);
        assert!(chunk.starts_with("\x18\x1b[0m") && chunk.contains("\x1b[?47h") && chunk.contains("\x1b[?1h"), "{chunk:?}");
        assert!(chunk.ends_with("after"));
        ledger.discard(replayed.credit.as_ref().unwrap());
    }

    #[test]
    fn output_credit_lanes_preserve_control_barriers_without_waiting_for_parser() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event(1)), UiEventEnqueueResult::Enqueued);
        assert_eq!(ingress.enqueue(RuntimeEvent::SystemLog {
            level: LogLevel::Info, message: "barrier".into(), timestamp: "now".into(),
        }), UiEventEnqueueResult::Enqueued);
        // The one-item control lane is full; the independently reserved output
        // lane still accepts the successor, with no display shedding or ACK.
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event(2)), UiEventEnqueueResult::Enqueued);
        let first = receiver.try_recv().unwrap();
        assert_eq!(ui_output_chunk(&first.event), "output-1");
        assert!(matches!(receiver.try_recv().unwrap().event, RuntimeEvent::SystemLog { message, .. } if message == "barrier"));
        let last = receiver.try_recv().unwrap();
        assert_eq!(ui_output_chunk(&last.event), "output-2");
        assert!(ledger.acknowledge(first.credit.as_ref().unwrap()).is_err(), "dequeuing is not parsing");
        ledger.discard(first.credit.as_ref().unwrap());
        ledger.discard(last.credit.as_ref().unwrap());
    }

    #[test]
    fn output_credit_batches_keep_every_ticket_until_one_actual_parser_ack() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        for sequence in 1..=supervisor::output_credit::RUN_ITEMS {
            assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(sequence as u64, "x")), UiEventEnqueueResult::Enqueued);
        }
        let mut batcher = PendingOutputBatcher::default();
        let now = Instant::now();
        for _ in 0..supervisor::output_credit::RUN_ITEMS {
            let queued = receiver.try_recv().unwrap();
            let RuntimeEvent::SessionOutput { identity, session, chunk, synthetic, timestamp } = queued.event else { panic!("output") };
            assert!(batcher.push(PendingSessionOutput { identity, session, chunk, synthetic, timestamp, credit: queued.credit }, now).is_empty());
        }
        let mut outputs = batcher.flush();
        assert_eq!(outputs.len(), 1);
        let output = outputs.pop().unwrap();
        let credit = output.credit.unwrap();
        assert_eq!(output.chunk.len(), supervisor::output_credit::RUN_ITEMS);
        assert_eq!(credit.tickets.len(), supervisor::output_credit::RUN_ITEMS);
        assert!(ledger.acknowledge(&credit).is_err());
        assert!(ledger.dispatch(&credit));
        assert_eq!(ledger.acknowledge(&credit).unwrap(), supervisor::output_credit::RUN_ITEMS);
        assert_eq!(ledger.acknowledge(&credit).unwrap(), 0);
    }

    #[test]
    fn output_credit_filtered_control_carry_is_disposed_without_parser_ack() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(1, "\x1b]0;hidden title")), UiEventEnqueueResult::FilteredOutput);
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(2, "\x07")), UiEventEnqueueResult::FilteredOutput);
        assert!(receiver.try_recv().is_err());
        let reset = ledger.begin_renderer();
        assert_eq!(reset.discarded.items, 0, "nothing was dispatched or parsed");
        ledger.renderer_ready(reset.epoch, Duration::ZERO).expect("no filtered native charge leaked");
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(3, "visible")), UiEventEnqueueResult::Enqueued);
        let output = receiver.try_recv().unwrap();
        // The first output after begin_renderer is the new page's first
        // display, so it carries the reload recovery prefix (CAN + SGR 0; no
        // modes were tracked here). The filtered carry still leaks nothing.
        assert_eq!(ui_output_chunk(&output.event), "\x18\x1b[0mvisible");
        ledger.discard(output.credit.as_ref().unwrap());
    }

    #[test]
    fn output_credit_observer_gap_is_coalesced_bounded_and_precedes_recovered_output() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        for sequence in 1..=supervisor::output_credit::RUN_ITEMS {
            enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(sequence as u64, "x"));
        }
        // Simulate the native observer path: no ticket, sanitizer still sees
        // every read. A credit-backed ingress must never use the legacy lane.
        for sequence in 65..=1064 {
            assert_eq!(ingress.enqueue(ui_output_event_with_chunk(sequence, "omitted")),
                UiEventEnqueueResult::DroppedOutputFull);
        }
        assert_eq!(ingress.state.lock().output_gaps.len(), 1);
        assert!(ingress.state.take_reportable_output_gaps(&ledger).is_empty());
        let first = receiver.try_recv().unwrap();
        assert!(ledger.dispatch(first.credit.as_ref().unwrap()));
        ledger.acknowledge(first.credit.as_ref().unwrap()).unwrap();
        assert_eq!(enqueue_credited(&ingress, &ledger, ui_output_event_with_chunk(1065, "resumed")),
            UiEventEnqueueResult::Enqueued);
        for _ in 1..supervisor::output_credit::RUN_ITEMS {
            let old = receiver.try_recv().unwrap();
            assert!(old.output_gaps_before.is_empty());
            ledger.discard(old.credit.as_ref().unwrap());
        }
        let resumed = receiver.try_recv().unwrap();
        assert_eq!(resumed.output_gaps_before.len(), 1);
        assert_eq!(resumed.output_gaps_before[0].dropped_events, 1000);
        assert!(resumed.output_gaps_before[0].observer_only);
        assert_eq!(ui_output_chunk(&resumed.event), "\x18\x1b[0mresumed");
        assert!(receiver.try_recv().is_err(), "no per-read observer notifications");
        assert!(ingress.state.take_all_output_gaps().is_empty());
        ledger.discard(resumed.credit.as_ref().unwrap());
    }

    #[test]
    fn output_credit_observer_control_fragments_do_not_leak_on_recovery() {
        for (before, omitted, resumed, expected) in [
            ("", "lost\x1b[31", "mnew", "new"),
            ("\x1b[31", "mlost", "new", "new"),
            ("", "lost\u{009b}31", "mnew", "new"),
            ("", "lost\x1b(", "Bnew", "new"),
            ("", "lost\x1b]title", "private\x07new", "new"),
            ("", "lost\u{009d}title", "private\u{009c}new", "new"),
            ("", "lost\x1bPpayload", "private\x1b\\new", "new"),
            ("", "lost\u{0090}payload", "private\u{009c}new", "new"),
            ("", "lost\x1b", "[31mnew", "\x1b[31mnew"),
        ] {
            let identity = match ui_output_event(1) { RuntimeEvent::SessionOutput { identity, .. } => identity, _ => unreachable!() };
            let mut stream = RendererTerminalStream::new(identity, "fixture".into());
            let clean = stream.push(identity, before, false, "before");
            assert_eq!(stream.prepare_display(clean, false), before);
            let dropped = stream.push(identity, omitted, false, "omitted");
            stream.prepare_display(dropped, true);
            let next = stream.push(identity, resumed, false, "resumed");
            assert_eq!(stream.prepare_display(next, false), format!("\x18\x1b[0m{expected}"), "{omitted:?}");
        }
    }

    #[test]
    fn output_credit_observer_gap_flushes_at_close_even_without_parser_progress() {
        let (ingress, receiver, ledger) = credited_ui_channel();
        let reset = ledger.begin_renderer(); // not ready, zero capacity
        let event = ui_output_event(1);
        let identity = match &event { RuntimeEvent::SessionOutput { identity, .. } => *identity, _ => unreachable!() };
        ingress.enqueue(event);
        assert!(ingress.state.take_reportable_output_gaps(&ledger).is_empty());
        let mut closed_identity = identity; closed_identity.sequence = 2;
        ingress.enqueue(RuntimeEvent::SessionState { identity: closed_identity, session: "claude".into(),
            state: LifecycleState::Closed, reason: "natural exit".into(), timestamp: "now".into() });
        let closed = receiver.try_recv().unwrap();
        assert_eq!(closed.output_gaps_before.len(), 1);
        assert_eq!(closed.output_gaps_before[0].identity, identity);
        assert!(matches!(closed.event, RuntimeEvent::SessionState { state: LifecycleState::Closed, .. }));
        assert!(ingress.state.lock().terminal_streams.is_empty());
        assert!(ingress.state.take_all_output_gaps().is_empty());
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
    }

    #[test]
    fn output_credit_observer_replays_known_modes_at_every_split_and_c1_boundary() {
        let identity = match ui_output_event(1) { RuntimeEvent::SessionOutput { identity, .. } => identity, _ => unreachable!() };
        for start in ["\x1b[", "\u{009b}"] {
            let omitted = format!("lost{start}?1;25;1004;2004;1049;1000;1002;1003;1006;1016h");
            let csi_prefix_len = "lost".len() + start.len();
            for split in omitted.char_indices().map(|(offset, _)| offset).chain([omitted.len()]) {
                let mut stream = RendererTerminalStream::new(identity, "fixture".into());
                let first = stream.push(identity, &omitted[..split], false, "first");
                stream.prepare_display(first, true);
                let rest = format!("{}new\x1b[?1l", &omitted[split..]);
                let filtered = stream.push(identity, &rest, false, "resume");
                let recovered = stream.prepare_display(filtered, false);
                // If the complete command fell in omitted output or its
                // incomplete prefix, its final state must precede fresh text.
                if split >= csi_prefix_len {
                    let text_at = recovered.find("new").unwrap();
                    assert!(recovered[..text_at].contains("\x1b[?1h"), "split={split} {recovered:?}");
                    assert!(recovered[..text_at].contains("\x1b[?47h"), "canonical buffer replay: {recovered:?}");
                    assert!(recovered[..text_at].contains("\x1b[?1003h"), "last protocol wins: {recovered:?}");
                    assert!(recovered[..text_at].contains("\x1b[?1016h"), "last encoding wins: {recovered:?}");
                }
                assert!(recovered.ends_with("new\x1b[?1l"));
                assert_eq!(stream.modes.cursor_keys, Some(false), "suffix updates future state, not its own prefix");
                assert_eq!(stream.modes.alternate, Some(true));
                assert_eq!(stream.modes.mouse_protocol, Some(1003));
                assert_eq!(stream.modes.mouse_encoding, Some(1016));
            }
        }
    }

    #[test]
    fn output_credit_mode_recovery_filters_strings_and_replays_alias_resets() {
        let identity = match ui_output_event(1) { RuntimeEvent::SessionOutput { identity, .. } => identity, _ => unreachable!() };
        let mut stream = RendererTerminalStream::new(identity, "fixture".into());
        let visible = stream.push(identity, "\x1b[?1;25;1004;2004;1049;1003;1016h", false, "before");
        stream.prepare_display(visible, false);
        let omitted = stream.push(identity,
            "lost\x1b[?1047;1000;1006;1;25;1004;2004l\x1b]title\x1b[?1h\x07\u{0090}private\x1b[?1003h\u{009c}\x1b[?1001;1005;1015h",
            false, "omitted");
        stream.prepare_display(omitted, true);
        let suffix = stream.push(identity, "restored", false, "resume");
        let recovered = stream.prepare_display(suffix, false);
        assert_eq!(recovered,
            "\x18\x1b[0m\x1b[?47l\x1b[?1l\x1b[?25l\x1b[?1004l\x1b[?2004l\x1b[?1000l\x1b[?1006lrestored");
        assert!(!recovered.contains("1049"), "do not replay unknown cursor-save side effects");
        assert!(recovered.len() - "restored".len() + 1 <= supervisor::output_credit::DISPLAY_OVERHEAD);
    }

    #[test]
    fn output_credit_mode_resets_and_malformed_controls_cannot_replay_stale_settings() {
        let identity = match ui_output_event(1) { RuntimeEvent::SessionOutput { identity, .. } => identity, _ => unreachable!() };
        for reset in ["\x1bc", "\x1b[!p", "\u{009b}!p"] {
            let mut stream = RendererTerminalStream::new(identity, "fixture".into());
            let omitted = stream.push(identity, &format!("lost\x1b[?1;1004;2004;1049;1003;1016h\x1b[?25l{reset}"), false, "omitted");
            stream.prepare_display(omitted, true);
            let suffix = stream.push(identity, "next", false, "resume");
            let recovered = stream.prepare_display(suffix, false);
            assert!(recovered.contains("\x1b[?1l") && recovered.contains("\x1b[?1004l") && recovered.contains("\x1b[?2004l"));
            if reset == "\x1bc" {
                assert!(recovered.contains("\x1b[?47l") && recovered.contains("\x1b[?1000l") && recovered.contains("\x1b[?1006l"));
                assert!(recovered.contains("\x1b[?25l"), "installed xterm RIS retains cursor visibility");
            } else {
                assert!(recovered.contains("\x1b[?47h") && recovered.contains("\x1b[?1003h") && recovered.contains("\x1b[?1016h"));
                assert!(recovered.contains("\x1b[?25h"), "DECSTR explicitly shows cursor");
            }
        }
        let mut stream = RendererTerminalStream::new(identity, "fixture".into());
        let omitted = stream.push(identity, "lost\x1b[?1;999999999999999999999h\x1b[?25\x18h\x1b[?1004:1h", false, "omitted");
        stream.prepare_display(omitted, true);
        assert_eq!(stream.modes, super::DisplayModeSnapshot::default(), "invalid or cancelled control must not establish mode state");
    }

    #[test]
    fn saturated_output_queue_preserves_survivor_order_and_inserts_a_per_run_gap() {
        let (ingress, receiver, _state) = ui_event_channel(2);
        assert_eq!(
            ingress.enqueue(ui_output_event(1)),
            UiEventEnqueueResult::Enqueued
        );
        assert_eq!(
            ingress.enqueue(ui_output_event(2)),
            UiEventEnqueueResult::Enqueued
        );
        assert_eq!(
            ingress.enqueue(ui_output_event(3)),
            UiEventEnqueueResult::DroppedOutputFull
        );

        let first = receiver.recv().expect("first queued event");
        assert!(first.output_gaps_before.is_empty());
        assert_eq!(ui_output_chunk(&first.event), "output-1");
        assert_eq!(
            ingress.enqueue(ui_output_event(4)),
            UiEventEnqueueResult::Enqueued
        );

        let second = receiver.recv().expect("second queued event");
        let fourth = receiver.recv().expect("post-gap queued event");
        assert_eq!(ui_output_chunk(&second.event), "output-2");
        assert!(second.output_gaps_before.is_empty());
        assert_eq!(
            fourth.output_gaps_before,
            vec![UiOutputGap {
                identity: run_event_identity(
                    "11111111-1111-1111-1111-111111111111",
                    "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                    7,
                    3,
                ),
                session: "claude".into(),
                dropped_events: 1,
                observer_only: false,
            }]
        );
        let gap_event = ui_output_gap_runtime_event(&fourth.output_gaps_before[0]);
        assert_eq!(
            ui_system_message(&gap_event),
            "UI output gap: dropped 1 sanitized display event(s) for session 'claude' run aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa (latest generation 7, sequence 3) under bounded-queue pressure. Terminal control parsing remained synchronized; visible output is incomplete."
        );
        assert!(matches!(
            gap_event,
            RuntimeEvent::SystemLog {
                level: LogLevel::Warn,
                ..
            }
        ));
        assert_eq!(ui_output_chunk(&fourth.event), "output-4");
    }

    #[test]
    fn full_output_queue_backpressures_but_delivers_concurrent_lifecycle_in_order() {
        let (ingress, receiver, state) = ui_event_channel(1);
        assert_eq!(
            ingress.enqueue(ui_output_event(1)),
            UiEventEnqueueResult::Enqueued
        );

        let concurrent_ingress = ingress.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (completed_tx, completed_rx) = mpsc::channel();
        let publisher = thread::spawn(move || {
            entered_tx.send(()).unwrap();
            completed_tx
                .send(concurrent_ingress.enqueue(ui_lifecycle_event(2)))
                .unwrap();
        });
        entered_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert!(
            completed_rx
                .recv_timeout(Duration::from_millis(25))
                .is_err(),
            "a full bounded queue must apply backpressure instead of dropping lifecycle"
        );

        let predecessor = receiver.recv().expect("queued output predecessor");
        assert_eq!(ui_output_chunk(&predecessor.event), "output-1");
        assert_eq!(
            completed_rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            UiEventEnqueueResult::Enqueued
        );
        let lifecycle = receiver.recv().expect("lossless lifecycle event");
        assert!(matches!(
            lifecycle.event,
            RuntimeEvent::SessionState {
                identity: RunEventIdentity { sequence: 2, .. },
                ..
            }
        ));
        assert!(lifecycle.output_gaps_before.is_empty());
        assert!(state.take_all_output_gaps().is_empty());
        publisher.join().unwrap();
    }

    #[test]
    fn dropped_split_osc_and_dcs_output_keeps_the_stateful_sanitizer_synchronized() {
        for (label, opener, continuation, hidden_tail) in [
            (
                "osc",
                "\x1b]title-secret",
                "-title-tail\x07after",
                "title-tail",
            ),
            (
                "dcs",
                "\x1bPdevice-secret",
                "-device-tail\x1b\\after",
                "device-tail",
            ),
        ] {
            let (ingress, receiver, state) = ui_event_channel(1);
            assert_eq!(
                ingress.enqueue(ui_output_event(1)),
                UiEventEnqueueResult::Enqueued,
                "{label} fixture queue fill"
            );
            assert_eq!(
                ingress.enqueue(ui_output_event_with_chunk(
                    2,
                    &format!("lost-visible{opener}"),
                )),
                UiEventEnqueueResult::DroppedOutputFull,
                "{label} opener event must be dropped only after sanitizer advancement"
            );

            let filler = receiver.recv().expect("queue filler");
            assert_eq!(ui_output_chunk(&filler.event), "output-1");
            assert_eq!(
                ingress.enqueue(ui_output_event_with_chunk(3, continuation)),
                UiEventEnqueueResult::Enqueued,
                "{label} continuation"
            );
            let survivor = receiver.recv().expect("sanitized continuation");
            assert_eq!(ui_output_chunk(&survivor.event), "after", "{label}");
            assert!(!ui_output_chunk(&survivor.event).contains(hidden_tail));
            assert_eq!(survivor.output_gaps_before.len(), 1, "{label}");
            assert_eq!(survivor.output_gaps_before[0].identity.sequence, 2);
            assert_eq!(survivor.output_gaps_before[0].dropped_events, 1);
            assert!(state.take_all_output_gaps().is_empty());
        }
    }

    #[test]
    fn desktop_operator_route_schema_is_singular_direct_and_rejects_forged_authority() {
        let recipient_id = SessionId::new_v4();
        let request = serde_json::from_value::<OperatorRouteMessageRequest>(serde_json::json!({
            "recipient_id": recipient_id,
            "content": "exact payload",
        }))
        .unwrap();
        assert_eq!(request.recipient_id, recipient_id);
        assert_eq!(request.content, "exact payload");

        let forged = serde_json::from_value::<OperatorRouteMessageRequest>(serde_json::json!({
            "from": "forged]\\u001b[201~sender",
            "recipient_ids": [SessionId::new_v4()],
            "scope": "direct",
            "content": "exact payload",
        }));
        assert!(forged.is_err());

        let legacy_multi =
            serde_json::from_value::<OperatorRouteMessageRequest>(serde_json::json!({
                "recipient_id": recipient_id,
                "recipient_ids": [recipient_id, SessionId::new_v4()],
                "scope": "room",
                "content": "exact payload",
            }));
        assert!(legacy_multi.is_err());
    }

    #[test]
    fn sanitize_terminal_output_for_ui_keeps_visible_unicode_and_csi() {
        let input = "\x1b[31mwarn λ\x1b[0m\x1b]0;ignored\x07";
        assert_eq!(
            sanitize_terminal_output_for_ui(input),
            "\x1b[31mwarn λ\x1b[0m"
        );
    }

    #[test]
    fn terminal_output_sanitizer_removes_osc_split_across_chunks() {
        let mut sanitizer = TerminalOutputSanitizer::default();
        let output = [
            sanitizer.push("visible-prefix\x1b"),
            sanitizer.push("]0;hidden title"),
            sanitizer.push("\x07-visible-suffix"),
            sanitizer.finish(),
        ]
        .concat();

        assert_eq!(output, "visible-prefix-visible-suffix");
    }

    #[test]
    fn terminal_output_sanitizer_removes_every_string_control_family_across_chunk_boundaries() {
        let fixtures = [
            "left\x1b]osc\x07right",
            "left\x1b]osc\x1b\\right",
            "left\x1bPdcs\x1b\\right",
            "left\x1bXsos\x1b\\right",
            "left\x1b^pm\x1b\\right",
            "left\x1b_apc\x1b\\right",
            "left\u{009d}osc\u{0007}right",
            "left\u{009d}osc\u{009c}right",
            "left\u{0090}dcs\u{009c}right",
            "left\u{0098}sos\u{009c}right",
            "left\u{009e}pm\u{009c}right",
            "left\u{009f}apc\u{009c}right",
        ];

        for fixture in fixtures {
            for split in (0..=fixture.len()).filter(|index| fixture.is_char_boundary(*index)) {
                let mut sanitizer = TerminalOutputSanitizer::default();
                let output = [
                    sanitizer.push(&fixture[..split]),
                    sanitizer.push(&fixture[split..]),
                    sanitizer.finish(),
                ]
                .concat();
                assert_eq!(
                    output, "leftright",
                    "wrong output at split {split}: {fixture:?}"
                );
            }
        }
    }

    #[test]
    fn renderer_terminal_stream_sanitizes_without_buffering_visible_output() {
        let first_identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            1,
            1,
        );
        let second_identity = RunEventIdentity {
            sequence: 2,
            ..first_identity
        };
        let mut stream = RendererTerminalStream::new(first_identity, "claude".into());
        assert_eq!(
            stream.push(first_identity, "first \x1b]hidden", false, "one"),
            "first "
        );
        assert_eq!(
            stream.push(second_identity, " title\x07second", true, "two"),
            "second"
        );

        let tail = stream.finish();
        assert_eq!(tail.chunk, "");
        assert!(tail.synthetic);
        assert_eq!(tail.timestamp, "two");
        assert_eq!(tail.identity, second_identity);
        assert_eq!(tail.session, "claude");
    }

    #[test]
    fn contiguous_output_for_one_run_coalesces_under_the_highest_sequence() {
        let sequence_two = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let sequence_three = RunEventIdentity {
            sequence: 3,
            ..sequence_two
        };
        let mut pending = None;

        assert!(
            queue_pending_session_output(
                &mut pending,
                pending_output(sequence_two, "claude", "A2"),
            )
            .is_none()
        );
        assert!(
            queue_pending_session_output(
                &mut pending,
                pending_output(sequence_three, "claude", "A3"),
            )
            .is_none()
        );

        let emitted = pending.take().expect("coalesced output");
        assert_eq!(emitted.identity, sequence_three);
        assert_eq!(emitted.chunk, "A2A3");
        assert_eq!(emitted.timestamp, "sequence-3");
    }

    #[test]
    fn incomplete_terminal_escape_is_discarded_without_reusing_its_sequence() {
        let identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let mut stream = RendererTerminalStream::new(identity, "claude".into());
        let visible = stream.push(identity, "visible\x1b", false, "sequence-2");
        let started = Instant::now();
        let mut batcher = PendingOutputBatcher::default();
        assert!(
            batcher
                .push(
                    PendingSessionOutput {
                        identity,
                        session: "claude".into(),
                        chunk: visible,
                        synthetic: false,
                        timestamp: "sequence-2".into(),
                        credit: None,
                    },
                    started,
                )
                .is_empty()
        );
        let mut emitted = batcher.flush_expired(started + MAX_UI_OUTPUT_BATCH_AGE);
        let discarded_tail = stream.finish();
        assert!(discarded_tail.chunk.is_empty());
        emitted.extend(batcher.push(discarded_tail, started + MAX_UI_OUTPUT_BATCH_AGE));
        emitted.extend(batcher.flush());

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].identity, identity);
        assert_eq!(emitted[0].chunk, "visible");
    }

    #[test]
    fn bumped_generation_terminal_state_finishes_prior_generation_stream_once() {
        let output_identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let terminal_state_identity = RunEventIdentity {
            generation: 8,
            sequence: 3,
            ..output_identity
        };

        for (raw_chunk, expected_chunk) in [
            ("visible-escape\x1b", "visible-escape"),
            ("visible-osc\x1b]discarded", "visible-osc"),
        ] {
            let mut streams = HashMap::new();
            let filtered = streams
                .entry(super::RunOutputKey::from(output_identity))
                .or_insert_with(|| RendererTerminalStream::new(output_identity, "claude".into()))
                .push(output_identity, raw_chunk, false, "sequence-2");
            let mut pending = None;
            assert!(
                queue_pending_session_output(
                    &mut pending,
                    PendingSessionOutput {
                        identity: output_identity,
                        session: "claude".into(),
                        chunk: filtered,
                        synthetic: false,
                        timestamp: "sequence-2".into(),
                        credit: None,
                    },
                )
                .is_none()
            );

            let stream = streams
                .remove(&super::RunOutputKey::from(terminal_state_identity))
                .expect("bumped generation must resolve the prior-generation stream");
            assert!(stream.finish().chunk.is_empty());
            assert!(streams.is_empty());
            assert!(
                streams
                    .remove(&super::RunOutputKey::from(terminal_state_identity))
                    .is_none(),
                "terminal stream must be finished exactly once"
            );

            let emitted = pending.take().expect("one sanitized output emission");
            assert_eq!(emitted.identity, output_identity);
            assert_eq!(emitted.chunk, expected_chunk);
            assert!(pending.is_none());
        }
    }

    #[test]
    fn new_run_or_generation_flushes_the_old_identity_before_buffering_the_new_one() {
        let old_run = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let new_run = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            8,
            1,
        );
        let mut pending = None;

        assert!(
            queue_pending_session_output(&mut pending, pending_output(old_run, "claude", "A2"),)
                .is_none()
        );
        let first =
            queue_pending_session_output(&mut pending, pending_output(new_run, "claude", "B1"))
                .expect("identity change must flush old output");
        let second = pending.take().expect("new output remains pending");

        assert_eq!((first.identity, first.chunk.as_str()), (old_run, "A2"));
        assert_eq!((second.identity, second.chunk.as_str()), (new_run, "B1"));
    }

    #[test]
    fn delayed_old_run_output_is_never_relabelled_as_the_current_run() {
        let old_sequence_two = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let old_sequence_three = RunEventIdentity {
            sequence: 3,
            ..old_sequence_two
        };
        let current_run = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb",
            8,
            1,
        );
        let mut pending = None;
        let mut emitted = Vec::new();

        assert!(
            queue_pending_session_output(
                &mut pending,
                pending_output(old_sequence_two, "claude", "A2"),
            )
            .is_none()
        );
        emitted.push(
            queue_pending_session_output(&mut pending, pending_output(current_run, "claude", "B1"))
                .expect("current run must flush old run"),
        );
        emitted.push(
            queue_pending_session_output(
                &mut pending,
                pending_output(old_sequence_three, "claude", "A3-delayed"),
            )
            .expect("delayed old run must flush current run"),
        );
        emitted.push(pending.take().expect("delayed output remains pending"));

        assert_eq!(
            emitted
                .iter()
                .map(|output| (output.identity, output.chunk.as_str()))
                .collect::<Vec<_>>(),
            vec![
                (old_sequence_two, "A2"),
                (current_run, "B1"),
                (old_sequence_three, "A3-delayed"),
            ]
        );
    }

    #[test]
    fn a_sequence_gap_is_not_coalesced() {
        let sequence_two = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            2,
        );
        let sequence_four = RunEventIdentity {
            sequence: 4,
            ..sequence_two
        };
        let mut pending = None;

        assert!(
            queue_pending_session_output(
                &mut pending,
                pending_output(sequence_two, "claude", "A2"),
            )
            .is_none()
        );
        let first = queue_pending_session_output(
            &mut pending,
            pending_output(sequence_four, "claude", "A4"),
        )
        .expect("sequence gap must flush rather than coalesce");

        assert_eq!(first.identity, sequence_two);
        assert_eq!(
            pending.expect("new sequence remains pending").identity,
            sequence_four
        );
    }

    #[test]
    fn continuous_output_flood_produces_multiple_byte_bounded_batches_without_idle() {
        let base_identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            1,
        );
        let now = Instant::now();
        let chunk = "x".repeat(4096);
        let mut batcher = PendingOutputBatcher::default();
        let mut emitted = Vec::new();

        for sequence in 1..=40 {
            emitted.extend(batcher.push(
                pending_output(
                    RunEventIdentity {
                        sequence,
                        ..base_identity
                    },
                    "claude",
                    &chunk,
                ),
                now,
            ));
        }
        emitted.extend(batcher.flush());

        assert!(
            emitted.len() >= 5,
            "a no-idle 160 KiB flood must not remain one unbounded batch"
        );
        assert!(
            emitted
                .iter()
                .all(|output| output.chunk.len() <= MAX_UI_OUTPUT_BATCH_BYTES),
            "every emitted terminal batch must respect the byte cap"
        );
        assert_eq!(
            emitted
                .iter()
                .map(|output| output.chunk.len())
                .sum::<usize>(),
            40 * chunk.len()
        );
        assert!(
            emitted
                .windows(2)
                .all(|pair| pair[0].identity.sequence <= pair[1].identity.sequence),
            "batch identities must remain monotonic"
        );
        assert_eq!(emitted.last().unwrap().identity.sequence, 40);
        assert_eq!(MAX_UI_OUTPUT_BATCH_BYTES * 512, 16 * 1024 * 1024);
    }

    #[test]
    fn continuous_output_flushes_when_batch_age_expires_without_an_idle_receive() {
        let first_identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            1,
        );
        let second_identity = RunEventIdentity {
            sequence: 2,
            ..first_identity
        };
        let started = Instant::now();
        let mut batcher = PendingOutputBatcher::default();

        assert!(
            batcher
                .push(pending_output(first_identity, "claude", "first"), started)
                .is_empty()
        );
        let emitted = batcher.push(
            pending_output(second_identity, "claude", "second"),
            started + MAX_UI_OUTPUT_BATCH_AGE,
        );

        assert_eq!(emitted.len(), 1);
        assert_eq!(emitted[0].identity, first_identity);
        assert_eq!(emitted[0].chunk, "first");
        let remaining = batcher.flush();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].identity, second_identity);
        assert_eq!(remaining[0].chunk, "second");
    }

    #[test]
    fn overflow_flushes_before_the_next_logical_event_without_splitting_its_sequence() {
        let base_identity = run_event_identity(
            "11111111-1111-1111-1111-111111111111",
            "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
            7,
            1,
        );
        let prefix_chunk = "a".repeat(4096);
        let final_chunk = "b".repeat(10 * 1024);
        let expected = [prefix_chunk.repeat(7), final_chunk.clone()].concat();
        let now = Instant::now();
        let mut batcher = PendingOutputBatcher::default();

        for sequence in 1..=7 {
            assert!(
                batcher
                    .push(
                        pending_output(
                            RunEventIdentity {
                                sequence,
                                ..base_identity
                            },
                            "claude",
                            &prefix_chunk,
                        ),
                        now,
                    )
                    .is_empty()
            );
        }
        let final_identity = RunEventIdentity {
            sequence: 8,
            ..base_identity
        };
        let mut emitted = batcher.push(pending_output(final_identity, "claude", &final_chunk), now);
        emitted.extend(batcher.flush());

        assert_eq!(emitted.len(), 2);
        assert_eq!(emitted[0].identity.sequence, 7);
        assert_eq!(emitted[0].chunk, prefix_chunk.repeat(7));
        assert_eq!(emitted[1].identity, final_identity);
        assert_eq!(emitted[1].chunk, final_chunk);
        assert!(
            emitted
                .iter()
                .all(|output| output.chunk.len() <= MAX_UI_OUTPUT_BATCH_BYTES)
        );

        // Model the renderer's strictly increasing sequence gate. If one
        // SessionOutput were split into two seq-8 emissions, its tail would be
        // rejected here.
        let mut highest_sequence = 0;
        let mut rendered = String::new();
        for output in &emitted {
            if output.identity.sequence > highest_sequence {
                highest_sequence = output.identity.sequence;
                rendered.push_str(&output.chunk);
            }
        }
        assert_eq!(
            rendered, expected,
            "all 38 KiB must survive the renderer sequence gate exactly once"
        );
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
    fn runtime_dir_override_is_absolute_and_ignores_blank_values() {
        let startup_cwd = unique_temp_dir("runtime-override-cwd");
        let relative = PathBuf::from("isolated runtime");
        let absolute = unique_temp_dir("runtime-override-absolute");

        assert_eq!(resolve_runtime_dir_override(None, &startup_cwd), None);
        assert_eq!(
            resolve_runtime_dir_override(Some(OsString::from("   ")), &startup_cwd),
            None
        );
        assert_eq!(
            resolve_runtime_dir_override(Some(relative.clone().into_os_string()), &startup_cwd),
            Some(startup_cwd.join(relative))
        );
        assert_eq!(
            resolve_runtime_dir_override(Some(absolute.clone().into_os_string()), &startup_cwd),
            Some(absolute)
        );
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
    fn release_webview_security_configuration_is_fail_closed() {
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["app"]["withGlobalTauri"], false);

        let csp = config["app"]["security"]["csp"]
            .as_str()
            .expect("release CSP must be configured");
        for directive in [
            "default-src 'self'",
            "script-src 'self'",
            "connect-src 'self' ipc: http://ipc.localhost",
            "object-src 'none'",
            "base-uri 'none'",
            "form-action 'none'",
            "frame-ancestors 'none'",
        ] {
            assert!(csp.contains(directive), "missing CSP directive {directive}");
        }
        assert!(!csp.contains("'unsafe-eval'"));

        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        assert_eq!(
            capability["permissions"],
            serde_json::json!([
                "core:event:allow-listen",
                "core:event:allow-unlisten",
                "core:window:allow-is-focused",
                "core:window:allow-is-fullscreen",
                "core:window:allow-is-minimized",
                "core:window:allow-is-maximized",
                "core:window:allow-minimize",
                "core:window:allow-toggle-maximize",
                "core:window:allow-internal-toggle-maximize",
                "core:window:allow-close",
                "core:window:allow-start-dragging",
                "core:window:allow-outer-size",
                "core:window:allow-outer-position",
                "core:window:allow-set-size",
                "core:window:allow-center",
                "core:window:allow-is-visible",
                "core:window:allow-show"
            ])
        );

        let manifest = include_str!("../Cargo.toml");
        assert!(!manifest.contains("features = [\"devtools\"]"));
    }

    #[test]
    fn cdp_browser_arguments_bind_loopback_without_wildcard_origins() {
        let arguments = cdp_browser_arguments(9222);
        assert_eq!(
            arguments,
            "--remote-debugging-port=9222 --remote-debugging-address=127.0.0.1"
        );
        assert!(!arguments.contains("remote-allow-origins"));
        assert!(!arguments.contains("0.0.0.0"));
    }

    #[test]
    fn only_the_explicit_debug_variable_turns_cdp_on() {
        let environment = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| OsString::from(*value))
            }
        };

        // A shell started from a live v1 pane carries both of these.
        let inherited_from_v1_pane = environment(&[
            ("PRIM1_CDP_PORT", "9222"),
            (
                "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS",
                "--remote-debugging-port=9222 --remote-debugging-address=127.0.0.1",
            ),
        ]);
        assert_eq!(cdp_port_from_environment(inherited_from_v1_pane), Ok(None));
        assert_eq!(cdp_port_from_environment(environment(&[])), Ok(None));
        assert_eq!(
            cdp_port_from_environment(environment(&[("PRIM1_DEBUG_CDP_PORT", "9333")])),
            Ok(Some(9333))
        );
        assert_eq!(
            cdp_port_from_environment(environment(&[("PRIM1_DEBUG_CDP_PORT", "0")])),
            Err(format!("{DEBUG_CDP_PORT_ENV} must be a non-zero TCP port"))
        );
    }

    #[test]
    fn webview_switch_is_removed_unless_debugging_was_requested() {
        let startup = |cdp_port| StartupConfig {
            agent_working_root: PathBuf::from("ignored"),
            environment_source: None,
            runtime_dir_override: None,
            cdp_port,
            start_minimized: false,
            trial: None,
            upgrade_startup: Ok(None),
            personal_repo_override: None,
        };
        assert_eq!(
            WEBVIEW2_BROWSER_ARGUMENTS_ENV,
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS"
        );
        assert_eq!(startup(None).webview_browser_arguments(), None);
        assert_eq!(
            startup(Some(9333)).webview_browser_arguments().as_deref(),
            Some("--remote-debugging-port=9333 --remote-debugging-address=127.0.0.1")
        );
    }

    #[test]
    fn v2_identity_is_disjoint_from_the_installed_v1_app() {
        // The identifier owns the default runtime dir and the WebView2
        // profile. Reverting it would make v2 share both with the live app.
        let config: serde_json::Value =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        assert_eq!(config["identifier"], "io.prim1.v2");
        assert_eq!(config["productName"], "PRIM-1 v2");
        assert_eq!(config["app"]["windows"][0]["title"], "PRIM-1 v2");
    }

    #[test]
    fn start_minimized_parser_is_strict_and_defaults_to_visible() {
        assert!(!parse_start_minimized(None).unwrap());
        assert!(!parse_start_minimized(Some(OsString::from("   "))).unwrap());
        assert!(!parse_start_minimized(Some(OsString::from("0"))).unwrap());
        assert!(!parse_start_minimized(Some(OsString::from("FALSE"))).unwrap());
        assert!(parse_start_minimized(Some(OsString::from("1"))).unwrap());
        assert!(parse_start_minimized(Some(OsString::from("true"))).unwrap());
        assert!(parse_start_minimized(Some(OsString::from("yes"))).is_err());
    }

    #[test]
    fn runtime_and_working_root_must_be_disjoint() {
        let root = unique_temp_dir("runtime-disjoint");
        let working_root = root.join("work");
        fs::create_dir_all(&working_root).unwrap();

        assert!(
            validate_runtime_storage_paths(&working_root.join("runtime"), &working_root).is_err()
        );
        assert!(validate_runtime_storage_paths(&root, &working_root).is_err());
        assert!(validate_runtime_storage_paths(&root.join("runtime"), &working_root).is_ok());

        fs::remove_dir_all(root).unwrap();
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

    #[cfg(windows)]
    fn create_windows_junction(link: &Path, target: &Path) {
        let output = std::process::Command::new("cmd.exe")
            .args(["/d", "/c", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("launch mklink junction command");
        assert!(
            output.status.success(),
            "failed to create junction {} -> {}: stdout={} stderr={}",
            link.display(),
            target.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    fn windows_path_sddl(path: &Path) -> String {
        let output = std::process::Command::new("powershell.exe")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "& { param([string]$TargetPath); (Get-Acl -LiteralPath $TargetPath).Sddl }",
                "-TargetPath",
            ])
            .arg(path)
            .output()
            .expect("query Windows path security descriptor");
        assert!(
            output.status.success(),
            "failed to read SDDL for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    #[cfg(windows)]
    #[test]
    fn desktop_rejects_runtime_junction_before_instance_lock_or_target_mutation() {
        let root = unique_temp_dir("runtime-junction-before-lock");
        let working_root = root.join("work");
        let junction_target = root.join("outside-runtime-target");
        fs::create_dir_all(&working_root).unwrap();
        fs::create_dir_all(&junction_target).unwrap();
        let sentinel = junction_target.join("must-remain.txt");
        fs::write(&sentinel, b"desktop runtime target sentinel").unwrap();
        let target_sddl_before = windows_path_sddl(&junction_target);
        let runtime_junction = root.join("runtime");
        create_windows_junction(&runtime_junction, &junction_target);

        let error =
            validate_and_lock_desktop_runtime(&runtime_junction, &working_root).unwrap_err();

        assert!(
            error.to_string().contains("symlink or reparse point"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&sentinel).unwrap(),
            b"desktop runtime target sentinel"
        );
        assert_eq!(windows_path_sddl(&junction_target), target_sddl_before);
        assert!(!junction_target.join(DESKTOP_INSTANCE_LOCK_FILE).exists());
        assert!(!junction_target.join("audit.jsonl").exists());

        fs::remove_dir(&runtime_junction).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn desktop_instance_lock_ignores_stale_file_and_reacquires_after_drop() {
        let runtime_dir = unique_temp_dir("instance-lock-stale");
        fs::create_dir_all(&runtime_dir).unwrap();
        let lock_path = runtime_dir.join(DESKTOP_INSTANCE_LOCK_FILE);
        fs::write(&lock_path, b"stale marker from a prior process").unwrap();

        let first = DesktopInstanceLock::acquire(&runtime_dir).unwrap();
        drop(first);
        let second = DesktopInstanceLock::acquire(&runtime_dir).unwrap();
        drop(second);

        assert_eq!(
            fs::read(&lock_path).unwrap(),
            b"stale marker from a prior process"
        );
        fs::remove_dir_all(runtime_dir).unwrap();
    }

    #[test]
    fn desktop_instance_lock_rejects_concurrent_holder_with_exact_error_then_reacquires() {
        let runtime_dir = unique_temp_dir("instance-lock-contention");
        let holder_runtime_dir = runtime_dir.clone();
        let (acquired_tx, acquired_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder = thread::spawn(move || {
            let _lock = DesktopInstanceLock::acquire(&holder_runtime_dir).unwrap();
            acquired_tx.send(()).unwrap();
            release_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("test did not release desktop instance lock holder");
        });

        acquired_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("desktop instance lock holder did not acquire in time");
        let error = DesktopInstanceLock::acquire(&runtime_dir).unwrap_err();
        assert_eq!(error, DESKTOP_INSTANCE_ALREADY_RUNNING);

        release_tx.send(()).unwrap();
        holder.join().unwrap();
        let reacquired = DesktopInstanceLock::acquire(&runtime_dir).unwrap();
        drop(reacquired);
        fs::remove_dir_all(runtime_dir).unwrap();
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
