use std::{
    collections::{BTreeMap, HashMap},
    ffi::OsString,
    fs::{File, OpenOptions, TryLockError},
    io::Write,
    panic,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use shared_types::{
    AddRoomMemberRequest, ChooseSessionWorkingDirectoryRequest, CreateRoomRequest,
    CreateSessionRequest, DeleteRoomRequest, DeleteSessionRequest, DeliverRoomMessageRequest,
    MoveRoomRequest, MoveSessionRequest, OperatorRouteMessageRequest, PostRoomMessageRequest,
    ReadRoomFeedRequest, RemoveRoomMemberRequest, RenameRoomRequest, RenameSessionRequest,
    RestartSessionRequest, RoomDeliveryResult, RoomFeedPage, RoomPostResult, RoomSnapshot,
    RunEventIdentity, RuntimeSnapshot, SendInputRequest, SessionId, SessionSnapshot,
    SetSessionLinuxWorkingDirectoryRequest, SetSessionPermissionRequest, StartSessionRequest,
    StopSessionRequest,
};
use supervisor::{
    RendererEventProjector, SupervisorConfig, SupervisorHandle, validate_runtime_storage_paths,
};
use tauri::{AppHandle, Emitter, Manager, State, WebviewWindow};
use tauri_plugin_dialog::DialogExt;

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
    diagnostics: DesktopDiagnostics,
    automation_mode: bool,
    shutdown_started: AtomicBool,
    shutdown_succeeded: AtomicBool,
    fullscreen_on_first_focus: AtomicBool,
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
        self.diagnostics
            .log("info", "shutdown_started", format!("reason={reason}"));
        match self.supervisor.shutdown() {
            Ok(()) => {
                self.shutdown_succeeded.store(true, Ordering::SeqCst);
                self.diagnostics
                    .log("info", "shutdown_complete", format!("reason={reason}"));
                true
            }
            Err(error) => {
                self.diagnostics.log(
                    "error",
                    "shutdown_failed",
                    format!("reason={reason}: {error:#}"),
                );
                false
            }
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UiOutputGap {
    identity: RunEventIdentity,
    session: String,
    dropped_events: u64,
}

#[derive(Default)]
struct UiEventIngressMutable {
    terminal_streams: HashMap<RunOutputKey, RendererTerminalStream>,
    output_gaps: BTreeMap<RunOutputKey, UiOutputGap>,
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
}

struct QueuedUiEvent {
    event: shared_types::RuntimeEvent,
    output_gaps_before: Vec<UiOutputGap>,
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
    state: Arc<UiEventIngressState>,
}

impl UiEventIngress {
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
                let run_key = RunOutputKey::from(identity);
                let filtered = state
                    .terminal_streams
                    .entry(run_key)
                    .or_insert_with(|| RendererTerminalStream::new(identity, session.clone()))
                    .push(identity, &chunk, synthetic, &timestamp);
                if filtered.is_empty() {
                    return UiEventEnqueueResult::FilteredOutput;
                }

                let output_gaps_before = state.output_gaps.remove(&run_key).into_iter().collect();
                let queued = QueuedUiEvent {
                    event: shared_types::RuntimeEvent::SessionOutput {
                        identity,
                        session: session.clone(),
                        chunk: filtered,
                        synthetic,
                        timestamp,
                    },
                    output_gaps_before,
                };
                match self.sender.try_send(queued) {
                    Ok(()) => UiEventEnqueueResult::Enqueued,
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
                    let _discarded_incomplete_control_tail = stream.finish();
                }

                let output_gaps_before = std::mem::take(&mut state.output_gaps)
                    .into_values()
                    .collect::<Vec<_>>();
                let queued = QueuedUiEvent {
                    event,
                    output_gaps_before,
                };
                // Lifecycle/control events are lossless. Supervisor mutations
                // release their state locks before publication, so bounded
                // backpressure can delay a publisher without leaving renderer
                // state stale or holding a lifecycle mutation lock.
                match self.sender.send(queued) {
                    Ok(()) => UiEventEnqueueResult::Enqueued,
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
        });
        if gap.identity.sequence >= current.identity.sequence {
            current.identity = gap.identity;
            current.session = gap.session;
        }
        current.dropped_events = current.dropped_events.saturating_add(gap.dropped_events);
    }
}

fn ui_event_channel(
    capacity: usize,
) -> (
    UiEventIngress,
    mpsc::Receiver<QueuedUiEvent>,
    Arc<UiEventIngressState>,
) {
    assert!(capacity > 0, "UI event queue capacity must be positive");
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let state = Arc::new(UiEventIngressState::default());
    (
        UiEventIngress {
            sender,
            state: state.clone(),
        },
        receiver,
        state,
    )
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
}

impl RendererTerminalStream {
    fn new(identity: RunEventIdentity, session: String) -> Self {
        Self {
            identity,
            session,
            sanitizer: TerminalOutputSanitizer::default(),
            last_synthetic: false,
            last_timestamp: String::new(),
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
            identity: self.identity,
            session: self.session,
            chunk: self.sanitizer.finish(),
            synthetic: self.last_synthetic,
            timestamp: self.last_timestamp,
        }
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

#[tauri::command]
fn automation_mode(state: State<'_, DesktopState>) -> bool {
    state.automation_mode
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
        format!("requested {}", request.session_id),
    );
    state
        .supervisor
        .start_session_by_id(request.session_id)
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
    state
        .supervisor
        .stop_session_by_id(request.session_id)
        .map_err(|error| {
            state.diagnostics.log(
                "error",
                "stop_session_failed",
                format!("{}: {}", request.session_id, error),
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
            state.diagnostics.log(
                "error",
                "delete_session_failed",
                format!("{}: {}", request.session_id, error),
            );
            error.to_string()
        })
}

#[tauri::command]
fn create_room(
    state: State<'_, DesktopState>,
    request: CreateRoomRequest,
) -> Result<RoomSnapshot, String> {
    state.supervisor.create_room(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "create_room_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn rename_room(
    state: State<'_, DesktopState>,
    request: RenameRoomRequest,
) -> Result<RoomSnapshot, String> {
    state.supervisor.rename_room(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "rename_room_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn move_room(
    state: State<'_, DesktopState>,
    request: MoveRoomRequest,
) -> Result<RuntimeSnapshot, String> {
    state.supervisor.move_room(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "move_room_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn add_room_member(
    state: State<'_, DesktopState>,
    request: AddRoomMemberRequest,
) -> Result<RoomSnapshot, String> {
    state.supervisor.add_room_member(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "add_room_member_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn remove_room_member(
    state: State<'_, DesktopState>,
    request: RemoveRoomMemberRequest,
) -> Result<RoomSnapshot, String> {
    state
        .supervisor
        .remove_room_member(request)
        .map_err(|error| {
            state
                .diagnostics
                .log("error", "remove_room_member_failed", error.to_string());
            error.to_string()
        })
}

#[tauri::command]
fn delete_room(state: State<'_, DesktopState>, request: DeleteRoomRequest) -> Result<(), String> {
    state.supervisor.delete_room(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "delete_room_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn read_room_feed(
    state: State<'_, DesktopState>,
    request: ReadRoomFeedRequest,
) -> Result<RoomFeedPage, String> {
    state.supervisor.read_room_feed(request).map_err(|error| {
        state
            .diagnostics
            .log("error", "read_room_feed_failed", error.to_string());
        error.to_string()
    })
}

#[tauri::command]
fn post_room_message(
    state: State<'_, DesktopState>,
    request: PostRoomMessageRequest,
) -> Result<RoomPostResult, String> {
    state
        .supervisor
        .post_room_message(request)
        .map_err(|error| {
            state
                .diagnostics
                .log("error", "post_room_message_failed", error.to_string());
            error.to_string()
        })
}

#[tauri::command]
async fn deliver_room_message(
    state: State<'_, DesktopState>,
    request: DeliverRoomMessageRequest,
) -> Result<RoomDeliveryResult, String> {
    let supervisor = state.supervisor.clone();
    let diagnostics = state.diagnostics.clone();
    match tauri::async_runtime::spawn_blocking(move || supervisor.deliver_room_message(request))
        .await
    {
        Ok(Ok(result)) => Ok(result),
        Ok(Err(error)) => {
            diagnostics.log("error", "deliver_room_message_failed", error.to_string());
            Err(error.to_string())
        }
        Err(error) => {
            let message = format!("room delivery worker failed: {error}");
            diagnostics.log("error", "deliver_room_message_failed", &message);
            Err(message)
        }
    }
}

fn pick_directory(app: &AppHandle, title: &str) -> Result<Option<PathBuf>, String> {
    app.dialog()
        .file()
        .set_title(title)
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
    let Some(path) = pick_directory(&app, "Choose PRIM-1 workspace")? else {
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
    let Some(path) = pick_directory(&app, "Choose session working directory")? else {
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

#[tauri::command]
fn resize_session(
    state: State<'_, DesktopState>,
    session_id: SessionId,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    state
        .supervisor
        .resize_session_by_id(session_id, cols, rows)
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
}

impl StartupConfig {
    pub fn cdp_port(&self) -> Option<u16> {
        self.cdp_port
    }
}

pub fn cdp_browser_arguments(port: u16) -> String {
    format!("--remote-debugging-port={port} --remote-debugging-address=127.0.0.1")
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
    let cdp_port = parse_cdp_port(std::env::var_os("PRIM1_CDP_PORT"))?;
    let start_minimized = parse_start_minimized(std::env::var_os("PRIM1_START_MINIMIZED"))?;

    Ok(StartupConfig {
        agent_working_root,
        environment_source,
        runtime_dir_override,
        cdp_port,
        start_minimized,
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
    let port = text
        .parse::<u16>()
        .map_err(|_| "PRIM1_CDP_PORT must be a non-zero TCP port".to_string())?;
    if port == 0 {
        return Err("PRIM1_CDP_PORT must be a non-zero TCP port".into());
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

fn present_main_window(window: &WebviewWindow, start_minimized: bool) -> Result<(), String> {
    if !start_minimized {
        window
            .set_fullscreen(true)
            .map_err(|error| error.to_string())?;
    }

    #[cfg(windows)]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SW_SHOW, SW_SHOWMINNOACTIVE, ShowWindow,
        };

        let hwnd = window.hwnd().map_err(|error| error.to_string())?;
        unsafe {
            let _ = ShowWindow(
                hwnd.0,
                if start_minimized {
                    SW_SHOWMINNOACTIVE
                } else {
                    SW_SHOW
                },
            );
        }
        Ok(())
    }

    #[cfg(not(windows))]
    {
        if start_minimized {
            window.minimize().map_err(|error| error.to_string())?;
            window.show().map_err(|error| error.to_string())
        } else {
            window.show().map_err(|error| error.to_string())?;
            window.set_focus().map_err(|error| error.to_string())
        }
    }
}

fn default_runtime_dir(app: &AppHandle) -> Result<PathBuf, String> {
    app.path()
        .app_local_data_dir()
        .map(|path| path.join("runtime"))
        .map_err(|error| format!("failed to resolve per-user application data: {error}"))
}

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
) -> Result<SupervisorHandle, String> {
    let pane_mcp_executable = std::env::current_exe()
        .map_err(|error| format!("failed to locate the PRIM-1 pane MCP executable: {error}"))?;
    let supervisor = SupervisorHandle::new(SupervisorConfig {
        working_root: agent_working_root,
        runtime_dir,
        pane_mcp_executable: Some(pane_mcp_executable),
        heartbeat_interval: None,
        auto_restart_on_stall_sessions: None,
        auto_restart_stall_threshold: None,
    })
    .map_err(|error| error.to_string())?;
    diagnostics.initialize(supervisor.runtime_dir())?;
    diagnostics.log(
        "info",
        "supervisor_init",
        "storage=per_user_app_data session_authority=session_id",
    );

    let ui_event_tx = start_ui_event_bridge(
        app.clone(),
        diagnostics.clone(),
        supervisor.renderer_event_projector(),
    );
    supervisor.set_event_sink(move |event| {
        let _ = ui_event_tx.enqueue(event);
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
) -> UiEventIngress {
    let (ingress, rx, ingress_state) = ui_event_channel(UI_EVENT_QUEUE_CAPACITY);

    thread::spawn(move || {
        let mut batcher = PendingOutputBatcher::default();

        loop {
            match rx.recv_timeout(Duration::from_millis(16)) {
                Ok(queued) => {
                    let now = Instant::now();
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush_expired(now));
                    if !queued.output_gaps_before.is_empty() {
                        emit_pending_session_outputs(&app, &diagnostics, batcher.flush());
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
                                        identity,
                                        session,
                                        chunk,
                                        synthetic,
                                        timestamp,
                                    },
                                    now,
                                ),
                            );
                        }
                        event => {
                            emit_pending_session_outputs(&app, &diagnostics, batcher.flush());
                            emit_runtime_event(&app, &diagnostics, projector.project(event));
                        }
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush());
                    emit_ui_output_gaps(&app, &diagnostics, ingress_state.take_all_output_gaps());
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    emit_pending_session_outputs(&app, &diagnostics, batcher.flush());
                    emit_ui_output_gaps(&app, &diagnostics, ingress_state.take_all_output_gaps());
                    break;
                }
            }
        }
    });

    ingress
}

fn ui_output_gap_message(gap: &UiOutputGap) -> String {
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
        && RunOutputKey::from(buffered.identity) == RunOutputKey::from(next.identity)
        && buffered.identity.generation == next.identity.generation
        && buffered.identity.sequence.checked_add(1) == Some(next.identity.sequence)
}

fn emit_pending_session_output(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    buffer: PendingSessionOutput,
) {
    emit_runtime_event(
        app,
        diagnostics,
        shared_types::RuntimeEvent::SessionOutput {
            identity: buffer.identity,
            session: buffer.session,
            chunk: buffer.chunk,
            synthetic: buffer.synthetic,
            timestamp: buffer.timestamp,
        },
    );
}

fn emit_pending_session_outputs(
    app: &AppHandle,
    diagnostics: &DesktopDiagnostics,
    outputs: impl IntoIterator<Item = PendingSessionOutput>,
) {
    for buffer in outputs {
        emit_pending_session_output(app, diagnostics, buffer);
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run(startup: StartupConfig) {
    let diagnostics = DesktopDiagnostics::uninitialized();
    let setup_diagnostics = diagnostics.clone();
    let setup_startup = startup.clone();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(move |app| {
            let requested_runtime_dir = match &setup_startup.runtime_dir_override {
                Some(runtime_dir) => runtime_dir.clone(),
                None => default_runtime_dir(app.handle())?,
            };
            let (runtime_dir, instance_lock) = validate_and_lock_desktop_runtime(
                &requested_runtime_dir,
                &setup_startup.agent_working_root,
            )?;
            // Supervisor construction remains the next stateful step. It hardens
            // the runtime ACL before audit or control-plane state is created.
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
                automation_mode: setup_startup.cdp_port.is_some(),
                shutdown_started: AtomicBool::new(false),
                shutdown_succeeded: AtomicBool::new(false),
                fullscreen_on_first_focus: AtomicBool::new(setup_startup.start_minimized),
                _instance_lock: instance_lock,
            });
            let main_window = app
                .get_webview_window("main")
                .ok_or_else(|| "missing main window".to_string())?;
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
            automation_mode,
            bootstrap,
            start_session,
            stop_session,
            restart_session,
            choose_workspace_directory,
            create_session,
            prime_default_working_directory,
            rename_session,
            choose_session_working_directory,
            set_session_linux_working_directory,
            set_session_permission_profile,
            move_session,
            delete_session,
            create_room,
            rename_room,
            move_room,
            add_room_member,
            remove_room_member,
            delete_room,
            read_room_feed,
            post_room_message,
            deliver_room_message,
            send_input,
            route_message,
            resize_session,
            toggle_fullscreen
        ])
        .build(tauri::generate_context!())
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
            event: tauri::WindowEvent::Focused(true),
            ..
        } if label == "main" => {
            if let Some(state) = handle.try_state::<DesktopState>()
                && state
                    .fullscreen_on_first_focus
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                && let Some(window) = handle.get_webview_window("main")
            {
                match window.set_fullscreen(true) {
                    Ok(()) => state.diagnostics.log(
                        "info",
                        "window_restored",
                        "main window restored fullscreen by operator focus",
                    ),
                    Err(error) => {
                        state
                            .diagnostics
                            .log("error", "window_restore_failed", error.to_string())
                    }
                }
            }
        }
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
        DESKTOP_INSTANCE_ALREADY_RUNNING, DESKTOP_INSTANCE_LOCK_FILE, DesktopDiagnostics,
        DesktopInstanceLock, MAX_UI_OUTPUT_BATCH_AGE, MAX_UI_OUTPUT_BATCH_BYTES,
        PendingOutputBatcher, PendingSessionOutput, RendererTerminalStream,
        TerminalOutputSanitizer, UI_EVENT_QUEUE_CAPACITY, UiEventEnqueueResult, UiOutputGap,
        cdp_browser_arguments, close_main_window_gracefully, normalize_path_for_child_processes,
        parse_cdp_port, parse_start_minimized, queue_pending_session_output,
        resolve_agent_working_root_from, resolve_environment_source, resolve_runtime_dir_override,
        sanitize_terminal_output_for_ui, ui_event_channel, ui_output_gap_runtime_event,
        validate_and_lock_desktop_runtime, validate_runtime_storage_paths,
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
                "core:window:allow-is-minimized"
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
