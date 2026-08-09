use std::{
    collections::{HashMap, HashSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader as StdBufReader, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, NaiveDate, Utc};
use control_plane::{DEFAULT_ENDPOINT, decode_request, encode_request, encode_response};
use parking_lot::{Mutex, RwLock};
use pty_host::{
    AgentLiveness, ConcretePtySession, PtyEvent, PtyEventHandler, PtyExitStatus, PtySession,
};
use shared_types::{
    AlertSeverity, ControlKey, ControlPlaneStatus, DeliverMessageRequest, DriverKind, EnvVar,
    EventCursor, EventFilter, HeartbeatSessionSummary, LaunchSpec, LifecycleState, LogLevel,
    MessageScope, PaneSignalType, RouteDeliveryPhase, RouteMessageRequest, RuntimeEvent,
    RuntimeSnapshot, SendInputRequest, SessionDefinition, SessionExitReason, SessionGeneration,
    SessionSnapshot, SidebandPhase, SidebandRequest, SidebandResponse, SidebandResponsePayload,
    SupervisorAlertType, WaitQuietRequest, WorkState, now_rfc3339,
};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

const CONTROL_PLANE_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const CONTROL_PLANE_PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(50);
const DEFAULT_REACTION_WINDOW_SECS: u64 = 12;
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 1800;
const DEFAULT_AUTO_RESTART_STALL_THRESHOLD_SECS: u64 = 600;
const AUTO_RESTART_WINDOW: Duration = Duration::from_secs(30 * 60);
const AUTO_RESTART_MAX_PER_WINDOW: usize = 3;
const DISPATCH_TEMPLATE_PATTERNS: [&str; 3] =
    ["pane_signal", "control-plane.ps1 -Action signal", "task_id"];
const RECENT_ROUTE_OVERLAP_WINDOW: Duration = Duration::from_secs(3);
const PAIR_NAME_MAX_LEN: usize = 48;
const RESERVED_PAIR_NAMES: [&str; 5] = ["main", "claude", "codex", "room", "operator"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmitBehavior {
    sequence: &'static str,
    delay: Duration,
    flatten_payload: bool,
    max_chunk_chars: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpLane {
    Lifecycle,
    SideEffect,
}

struct SidebandTimeouts;

impl SidebandTimeouts {
    fn lane(request: &SidebandRequest) -> OpLane {
        match request {
            SidebandRequest::CreatePair { .. } => OpLane::SideEffect,
            SidebandRequest::StartSession { .. }
            | SidebandRequest::StopSession { .. }
            | SidebandRequest::RestartSession { .. } => OpLane::Lifecycle,
            _ => OpLane::SideEffect,
        }
    }

    fn budget(request: &SidebandRequest) -> Duration {
        match request {
            SidebandRequest::Ping { .. } => Duration::from_secs(2),
            SidebandRequest::ListSessions { .. } => Duration::from_secs(2),
            SidebandRequest::CreatePair { .. } => Duration::from_secs(10),
            SidebandRequest::StartSession { .. } => Duration::from_secs(60),
            SidebandRequest::StopSession { .. } => Duration::from_secs(10),
            SidebandRequest::RestartSession { .. } => Duration::from_secs(70),
            SidebandRequest::WaitQuiet {
                timeout_seconds, ..
            } => Duration::from_secs((*timeout_seconds as u64).saturating_add(5)),
            SidebandRequest::EventsSince {
                max_wait_seconds, ..
            } => Duration::from_secs(max_wait_seconds.unwrap_or(0) as u64)
                .saturating_add(Duration::from_secs(5)),
            SidebandRequest::DeliverMessage { .. } => Duration::from_secs(20),
            SidebandRequest::SendInput { .. } => Duration::from_secs(20),
            SidebandRequest::SendKey { .. } => Duration::from_secs(20),
            SidebandRequest::RouteMessage { .. } => Duration::from_secs(30),
            SidebandRequest::PaneSignal { .. } => Duration::from_secs(5),
        }
    }
}

#[derive(Debug, Clone)]
struct RequestAckContext {
    request_id: String,
    action: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct DispatchReactionKey {
    request_id: String,
    session: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DispatchReactionOutcome {
    Reacted,
    Terminal,
}

struct PendingDispatchReaction {
    baseline: Instant,
    sender: mpsc::Sender<DispatchReactionOutcome>,
}

struct DispatchReactionWaiter {
    key: DispatchReactionKey,
    receiver: mpsc::Receiver<DispatchReactionOutcome>,
}

#[derive(Debug, Clone)]
struct AutoRestartOnStallConfig {
    allowed_sessions: HashSet<String>,
    threshold: Duration,
}

impl AutoRestartOnStallConfig {
    fn enabled_for(&self, session: &str) -> bool {
        self.allowed_sessions.contains(session)
    }
}

#[derive(Debug, Default)]
struct AutoRestartHistory {
    attempts: Vec<Instant>,
    disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRestartReservation {
    Reserved,
    CapReached,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StallAlertSnapshot {
    last_work_state: Option<WorkState>,
    last_session_state: Option<LifecycleState>,
}

struct StallDetector {
    generation: SessionGeneration,
    state: WorkState,
    entered_at: Instant,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeliveryWriteResult {
    bytes_written: usize,
    payload_part_count: u32,
}

struct RouteDeliveryEvent {
    request_id: String,
    route_id: String,
    from: String,
    logical_to: String,
    scope: MessageScope,
    recipient: Option<String>,
    recipient_index: u32,
    recipient_count: u32,
    payload_part_count: u32,
    phase: RouteDeliveryPhase,
    bytes_written: usize,
    error: Option<String>,
}

struct SidebandLifecycleEvent<'a> {
    request_id: &'a str,
    action: &'a str,
    session: Option<&'a str>,
    extra_args: &'a [String],
    phase: SidebandPhase,
    elapsed: Duration,
    error: Option<String>,
}

struct SupervisorAlertEvent {
    alert_type: SupervisorAlertType,
    request_id: Option<String>,
    session: Option<String>,
    action: Option<String>,
    last_work_state: Option<WorkState>,
    last_session_state: Option<LifecycleState>,
    message: String,
    severity: AlertSeverity,
}

struct PaneSignalRecord {
    token: String,
    task_id: String,
    signal_type: PaneSignalType,
    summary: String,
    artifact_paths: Vec<String>,
    commit_sha: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DispatchAttemptDecision {
    target_lifecycle_state_before: LifecycleState,
    target_work_state_before: Option<WorkState>,
    target_last_activity_at: Option<String>,
    last_route_from_target_at: Option<String>,
    last_route_from_target_instant: Option<Instant>,
    reason: Option<&'static str>,
}

impl DispatchAttemptDecision {
    fn overlap(&self) -> bool {
        self.reason.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneSignalWritePaths {
    signal_path: PathBuf,
    legacy_touch_path: PathBuf,
}

trait PtySpawner: Send + Sync {
    fn spawn(&self, spec: &LaunchSpec, handler: PtyEventHandler) -> Result<Box<dyn PtySession>>;
}

struct ConcretePtySpawner;

impl PtySpawner for ConcretePtySpawner {
    fn spawn(&self, spec: &LaunchSpec, handler: PtyEventHandler) -> Result<Box<dyn PtySession>> {
        Ok(Box::new(ConcretePtySession::spawn(spec, handler)?))
    }
}

trait MailboxFs: Send + Sync {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String>;
    fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()>;
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()>;
    fn remove_file(&self, path: &Path) -> std::io::Result<()>;
}

struct StdMailboxFs;

impl MailboxFs for StdMailboxFs {
    fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
        fs::read_to_string(path)
    }

    fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
        fs::write(path, contents)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        fs::rename(from, to)
    }

    fn remove_file(&self, path: &Path) -> std::io::Result<()> {
        fs::remove_file(path)
    }
}

// Exploration Finding 10 showed that very long routed messages could arrive
// truncated inside Claude's queued-message rendering even though the audit log
// still held the full routed_message payload. Keeping each Claude-targeted
// routed input below a conservative size is the least invasive mitigation.
const CLAUDE_ROUTED_MESSAGE_MAX_CHARS: usize = 500;
// Codex starts staging large payloads as "[Pasted Content N chars]" once the
// flattened routed input crosses its paste-detection threshold, so keep routed
// and deliver payloads comfortably below that boundary.
const CODEX_ROUTED_MESSAGE_MAX_CHARS: usize = 800;

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub working_root: PathBuf,
    pub runtime_dir: PathBuf,
    pub peer_slash_commands_allowed: bool,
    pub cross_pair_room_broadcast: bool,
    pub heartbeat_interval: Option<Duration>,
    pub auto_restart_on_stall_sessions: Option<Vec<String>>,
    pub auto_restart_stall_threshold: Option<Duration>,
    pub reaction_window: Option<Duration>,
}

struct AuditInner {
    path: PathBuf,
    active_date: NaiveDate,
}

struct AuditReadBatch {
    lines: Vec<AuditLine>,
    next_offset: u64,
    reached_eof: bool,
}

struct AuditLine {
    text: String,
    next_offset: u64,
}

struct AuditLog {
    dir: PathBuf,
    inner: Mutex<AuditInner>,
}

impl AuditLog {
    fn new(runtime_dir: &Path) -> Result<Self> {
        Self::new_at(runtime_dir, Utc::now())
    }

    fn new_at(runtime_dir: &Path, now: DateTime<Utc>) -> Result<Self> {
        let audit_dir = runtime_dir.join("audit");
        fs::create_dir_all(&audit_dir).context("failed to create audit directory")?;
        let active_date = now.date_naive();
        Ok(Self {
            dir: audit_dir.clone(),
            inner: Mutex::new(AuditInner {
                path: audit_dir.join(audit_file_name(active_date)),
                active_date,
            }),
        })
    }

    fn path(&self) -> PathBuf {
        self.inner.lock().path.clone()
    }

    fn active_file_name(&self) -> String {
        audit_file_name(self.inner.lock().active_date)
    }

    fn resolve_path(&self, audit_file: &str) -> PathBuf {
        self.dir.join(audit_file)
    }

    fn source_exists(&self, audit_file: &str) -> bool {
        self.resolve_path(audit_file).exists() || self.gzip_path_for(audit_file).exists()
    }

    fn open_reader(&self, audit_file: &str) -> Result<Option<Box<dyn Read + Send>>> {
        let plain = self.resolve_path(audit_file);
        if plain.exists() {
            let file = File::open(&plain)
                .with_context(|| format!("audit read error: failed to open {}", plain.display()))?;
            return Ok(Some(Box::new(file)));
        }

        let gz = self.gzip_path_for(audit_file);
        if gz.exists() {
            let file = File::open(&gz)
                .with_context(|| format!("audit read error: failed to open {}", gz.display()))?;
            return Ok(Some(Box::new(flate2::read::GzDecoder::new(file))));
        }

        Ok(None)
    }

    fn uncompressed_len(&self, audit_file: &str) -> Result<u64> {
        let plain = self.resolve_path(audit_file);
        match fs::metadata(&plain) {
            Ok(metadata) => return Ok(metadata.len()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("audit read error: failed to stat {}", plain.display())
                });
            }
        }

        let gz = self.gzip_path_for(audit_file);
        if !gz.exists() {
            return Ok(0);
        }

        let file = File::open(&gz)
            .with_context(|| format!("audit read error: failed to open {}", gz.display()))?;
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut sink = std::io::sink();
        std::io::copy(&mut decoder, &mut sink)
            .with_context(|| format!("audit read error: gz decode failed for {}", gz.display()))
    }

    fn read_lines_since(
        &self,
        audit_file: &str,
        byte_offset: u64,
        max_lines: usize,
    ) -> Result<AuditReadBatch> {
        if max_lines == 0 {
            return Ok(AuditReadBatch {
                lines: Vec::new(),
                next_offset: byte_offset,
                reached_eof: false,
            });
        }

        let Some(mut reader) = self.open_reader(audit_file)? else {
            return Ok(AuditReadBatch {
                lines: Vec::new(),
                next_offset: byte_offset,
                reached_eof: true,
            });
        };

        // Offsets are measured in the uncompressed JSONL stream. Gzip archives
        // cannot seek, so replay from a non-zero cursor decodes and discards.
        let mut remaining = byte_offset;
        let mut skipped = 0_u64;
        let mut discard = [0_u8; 8192];
        while remaining > 0 {
            let take = remaining.min(discard.len() as u64) as usize;
            let read = reader
                .read(&mut discard[..take])
                .with_context(|| format!("audit read error: failed to seek into {audit_file}"))?;
            if read == 0 {
                return Ok(AuditReadBatch {
                    lines: Vec::new(),
                    next_offset: skipped,
                    reached_eof: true,
                });
            }
            remaining -= read as u64;
            skipped += read as u64;
        }

        let mut reader = StdBufReader::new(reader);
        let mut offset = byte_offset;
        let mut lines = Vec::new();
        let mut line = Vec::new();

        loop {
            line.clear();
            let read = reader
                .read_until(b'\n', &mut line)
                .with_context(|| format!("audit read error: failed to read {audit_file}"))?;
            if read == 0 {
                return Ok(AuditReadBatch {
                    lines,
                    next_offset: offset,
                    reached_eof: true,
                });
            }

            offset += read as u64;
            let line_text = String::from_utf8(line.clone())
                .with_context(|| format!("audit read error: invalid UTF-8 in {audit_file}"))?;
            lines.push(AuditLine {
                text: line_text.trim_end_matches(['\r', '\n']).to_string(),
                next_offset: offset,
            });

            if lines.len() >= max_lines {
                return Ok(AuditReadBatch {
                    lines,
                    next_offset: offset,
                    reached_eof: false,
                });
            }
        }
    }

    fn append(&self, event: &RuntimeEvent) -> Result<()> {
        self.append_at(event, Utc::now())
    }

    fn append_at(&self, event: &RuntimeEvent, now: DateTime<Utc>) -> Result<()> {
        let today = now.date_naive();
        let rotated = {
            let mut inner = self.inner.lock();
            let rotated = if today != inner.active_date {
                inner.active_date = today;
                inner.path = self.dir.join(audit_file_name(today));
                true
            } else {
                false
            };

            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&inner.path)
                .with_context(|| format!("failed to open audit log at {}", inner.path.display()))?;
            let payload =
                serde_json::to_string(event).context("failed to serialize audit event")?;
            writeln!(file, "{payload}").context("failed to append audit event")?;
            rotated
        };

        if rotated {
            self.sweep_and_gzip_stale(today);
        }

        Ok(())
    }

    fn sweep_and_gzip_stale(&self, active_date: NaiveDate) {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) => {
                eprintln!(
                    "audit rotation: failed to scan stale logs in {}: {error}",
                    self.dir.display()
                );
                return;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let Some(date) = parse_audit_file_date(file_name) else {
                continue;
            };
            if date >= active_date {
                continue;
            }

            if let Err(error) = gzip_and_remove(&path) {
                eprintln!(
                    "audit rotation: gzip failed for {} - leaving plain file in place: {error:#}",
                    path.display()
                );
            }
        }
    }

    fn gzip_path_for(&self, audit_file: &str) -> PathBuf {
        self.dir.join(format!("{audit_file}.gz"))
    }
}

struct RunningSession {
    pty: Option<Box<dyn PtySession>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopIntentKind {
    Operator,
    Restart,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StopIntent {
    generation: SessionGeneration,
    kind: StopIntentKind,
}

struct QuiesceTimer {
    generation: SessionGeneration,
    handle: tokio::task::JoinHandle<()>,
}

struct SessionSlot {
    definition: SessionDefinition,
    state: LifecycleState,
    work_state: WorkState,
    work_state_observed: bool,
    work_detail: Option<String>,
    launch_banner_seen: bool,
    work_error_observations: HashMap<String, Vec<Instant>>,
    running: Option<RunningSession>,
    generation: SessionGeneration,
    stop_intent: Option<StopIntent>,
    process_id: Option<u32>,
    last_activity_at: Option<String>,
    last_real_output_at: Option<Instant>,
    last_route_from_session_at: Option<String>,
    last_route_from_session_instant: Option<Instant>,
    last_error: Option<String>,
    quiesce_timer: Option<QuiesceTimer>,
    stall_state_entered_at: Option<Instant>,
    stall_state_entered_timestamp: Option<String>,
    stall_detector: Option<StallDetector>,
}

impl SessionSlot {
    fn snapshot(&self) -> SessionSnapshot {
        SessionSnapshot {
            name: self.definition.name.clone(),
            title: self.definition.title.clone(),
            driver: self.definition.driver,
            lifecycle_state: self.state,
            working_dir: self.definition.working_dir.clone(),
            process_id: self.process_id,
            running: self.running.is_some(),
            last_activity_at: self.last_activity_at.clone(),
            last_error: self.last_error.clone(),
        }
    }

    fn title(&self) -> &str {
        &self.definition.title
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionExitClassification {
    exit_code: Option<i32>,
    signal: Option<i32>,
    success: bool,
    reason: SessionExitReason,
    requested: bool,
    last_error: Option<String>,
}

impl SessionExitClassification {
    fn requested(
        intent: StopIntent,
        exit_code: Option<i32>,
        signal: Option<i32>,
    ) -> SessionExitClassification {
        let reason = match intent.kind {
            StopIntentKind::Operator => SessionExitReason::OperatorStop,
            StopIntentKind::Restart => SessionExitReason::RestartStop,
        };

        SessionExitClassification {
            exit_code,
            signal,
            success: true,
            reason,
            requested: true,
            last_error: None,
        }
    }
}

fn classify_session_exit(
    status: Option<PtyExitStatus>,
    poll_error: Option<String>,
    stop_intent: Option<StopIntent>,
    fallback_reason: SessionExitReason,
    fallback_error: &str,
) -> SessionExitClassification {
    let exit_code = status
        .as_ref()
        .and_then(|status| exit_code_i32(status.exit_code));
    let signal = status
        .as_ref()
        .and_then(|status| parse_exit_signal(status.signal.as_deref()));

    if let Some(intent) = stop_intent {
        return SessionExitClassification::requested(intent, exit_code, signal);
    }

    if let Some(status) = status {
        let has_signal_indicator = status
            .signal
            .as_ref()
            .map(|signal| !signal.trim().is_empty())
            .unwrap_or(false);
        let success = status.success && !has_signal_indicator;
        let reason = if success {
            SessionExitReason::CleanExit
        } else {
            SessionExitReason::CrashExit
        };

        return SessionExitClassification {
            exit_code,
            signal,
            success,
            reason,
            requested: false,
            last_error: session_exit_last_error(reason, exit_code, signal, poll_error.as_deref()),
        };
    }

    SessionExitClassification {
        exit_code: None,
        signal: None,
        success: false,
        reason: fallback_reason,
        requested: false,
        last_error: session_exit_last_error(
            fallback_reason,
            None,
            None,
            poll_error.as_deref().or(Some(fallback_error)),
        ),
    }
}

fn classify_pty_error_exit(
    error: &str,
    stop_intent: Option<StopIntent>,
) -> SessionExitClassification {
    if let Some(intent) = stop_intent {
        return SessionExitClassification::requested(intent, None, None);
    }

    SessionExitClassification {
        exit_code: None,
        signal: None,
        success: false,
        reason: SessionExitReason::PtyError,
        requested: false,
        last_error: Some(error.to_string()),
    }
}

fn poll_pty_exit_status(
    running: Option<&RunningSession>,
) -> (Option<PtyExitStatus>, Option<String>) {
    let Some(pty) = running.and_then(|running| running.pty.as_ref()) else {
        return (None, None);
    };

    match pty.try_wait() {
        Ok(status) => (status, None),
        Err(error) => (None, Some(error.to_string())),
    }
}

fn exit_code_i32(exit_code: u32) -> Option<i32> {
    i32::try_from(exit_code).ok()
}

fn parse_exit_signal(signal: Option<&str>) -> Option<i32> {
    let raw = signal?.trim();
    if raw.is_empty() {
        return None;
    }
    if let Ok(value) = raw.parse::<i32>() {
        return Some(value);
    }

    let normalized = raw
        .trim_start_matches("SIG")
        .trim_start_matches("sig")
        .to_ascii_uppercase();
    match normalized.as_str() {
        "HUP" => Some(1),
        "INT" => Some(2),
        "QUIT" => Some(3),
        "ILL" => Some(4),
        "TRAP" => Some(5),
        "ABRT" | "IOT" => Some(6),
        "BUS" => Some(7),
        "FPE" => Some(8),
        "KILL" => Some(9),
        "USR1" => Some(10),
        "SEGV" => Some(11),
        "USR2" => Some(12),
        "PIPE" => Some(13),
        "ALRM" => Some(14),
        "TERM" => Some(15),
        _ => None,
    }
}

fn session_exit_last_error(
    reason: SessionExitReason,
    exit_code: Option<i32>,
    signal: Option<i32>,
    detail: Option<&str>,
) -> Option<String> {
    match reason {
        SessionExitReason::CrashExit => {
            if let Some(signal) = signal {
                Some(format!("process exited after signal {signal}"))
            } else if let Some(exit_code) = exit_code {
                Some(format!("process exited with code {exit_code}"))
            } else {
                Some("process exited unsuccessfully".into())
            }
        }
        SessionExitReason::PtyError | SessionExitReason::ProcessDisappeared => Some(
            detail
                .unwrap_or("process exit state unavailable")
                .to_string(),
        ),
        SessionExitReason::CleanExit
        | SessionExitReason::OperatorStop
        | SessionExitReason::RestartStop => None,
    }
}

fn session_exit_event(
    session: String,
    generation: SessionGeneration,
    process_id: Option<u32>,
    classification: &SessionExitClassification,
    timestamp: String,
) -> RuntimeEvent {
    RuntimeEvent::SessionExit {
        session,
        generation,
        process_id,
        exit_code: classification.exit_code,
        signal: classification.signal,
        success: classification.success,
        reason: classification.reason,
        requested: classification.requested,
        timestamp,
    }
}

fn named_claude_session(name: &str, title: &str, working_dir: &str) -> SessionDefinition {
    let mut definition = driver_claude::default_session(working_dir);
    definition.name = name.into();
    definition.title = title.into();
    definition
}

fn named_codex_session(name: &str, title: &str, working_dir: &str) -> SessionDefinition {
    let mut definition = driver_codex::default_session(working_dir);
    definition.name = name.into();
    definition.title = title.into();
    definition
}

fn default_session_definitions(working_dir: &str) -> Vec<SessionDefinition> {
    vec![
        driver_claude::default_session(working_dir),
        driver_codex::default_session(working_dir),
    ]
}

fn pair_slot_names(name: &str) -> (String, String) {
    (format!("{name}-claude"), format!("{name}-codex"))
}

/// Group sessions into "pairs" by naming convention.
///
/// - `claude` and `codex` (the protected main pair) -> `"main"`
/// - `<prefix>-claude` and `<prefix>-codex` -> `<prefix>`
/// - any other name -> the name itself (singleton pair, e.g. `operator`)
///
/// `"main"` is collision-safe because it is reserved by `validate_pair_name`.
fn pair_of(session_name: &str) -> &str {
    if session_name == "claude" || session_name == "codex" {
        return "main";
    }
    if let Some(stem) = session_name.strip_suffix("-claude")
        && !stem.is_empty()
    {
        return stem;
    }
    if let Some(stem) = session_name.strip_suffix("-codex")
        && !stem.is_empty()
    {
        return stem;
    }
    session_name
}

fn pair_title_stem(name: &str) -> String {
    let parts = name
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            let Some(first) = chars.next() else {
                return String::new();
            };
            let mut word = first.to_uppercase().collect::<String>();
            word.push_str(chars.as_str());
            word
        })
        .collect::<Vec<_>>();

    if parts.is_empty() {
        name.to_string()
    } else {
        parts.join(" ")
    }
}

fn pair_session_definitions(name: &str, working_dir: &str) -> [SessionDefinition; 2] {
    let title_stem = pair_title_stem(name);
    let (claude_name, codex_name) = pair_slot_names(name);
    [
        named_claude_session(&claude_name, &format!("{title_stem} · Claude"), working_dir),
        named_codex_session(&codex_name, &format!("{title_stem} · Codex"), working_dir),
    ]
}

fn validate_pair_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(anyhow!("pair name cannot be empty"));
    }
    if name.len() > PAIR_NAME_MAX_LEN {
        return Err(anyhow!(
            "pair name cannot exceed {PAIR_NAME_MAX_LEN} characters"
        ));
    }
    if RESERVED_PAIR_NAMES.contains(&name) {
        return Err(anyhow!("pair name '{name}' is reserved"));
    }
    if !name
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return Err(anyhow!(
            "pair name '{name}' may only contain letters, numbers, hyphens, and underscores"
        ));
    }

    Ok(())
}

fn ensure_pair_name_available(slots: &HashMap<String, SessionSlot>, name: &str) -> Result<()> {
    let (claude_name, codex_name) = pair_slot_names(name);
    if slots.contains_key(&claude_name) || slots.contains_key(&codex_name) {
        return Err(anyhow!("pair '{name}' already exists"));
    }

    Ok(())
}

fn renamed_closed_slot(slot: SessionSlot, definition: SessionDefinition) -> SessionSlot {
    SessionSlot {
        definition,
        state: LifecycleState::Closed,
        work_state: WorkState::Idle,
        work_state_observed: false,
        work_detail: None,
        launch_banner_seen: false,
        work_error_observations: HashMap::new(),
        running: None,
        generation: slot.generation,
        stop_intent: None,
        process_id: None,
        last_activity_at: slot.last_activity_at,
        last_real_output_at: None,
        last_route_from_session_at: slot.last_route_from_session_at,
        last_route_from_session_instant: slot.last_route_from_session_instant,
        last_error: slot.last_error,
        quiesce_timer: None,
        stall_state_entered_at: None,
        stall_state_entered_timestamp: None,
        stall_detector: None,
    }
}

fn closed_session_slot(definition: SessionDefinition) -> SessionSlot {
    SessionSlot {
        definition,
        state: LifecycleState::Closed,
        work_state: WorkState::Idle,
        work_state_observed: false,
        work_detail: None,
        launch_banner_seen: false,
        work_error_observations: HashMap::new(),
        running: None,
        generation: 0,
        stop_intent: None,
        process_id: None,
        last_activity_at: None,
        last_real_output_at: None,
        last_route_from_session_at: None,
        last_route_from_session_instant: None,
        last_error: None,
        quiesce_timer: None,
        stall_state_entered_at: None,
        stall_state_entered_timestamp: None,
        stall_detector: None,
    }
}

struct SupervisorInner {
    runtime_dir: PathBuf,
    audit: AuditLog,
    slots: Mutex<HashMap<String, SessionSlot>>,
    event_sink: RwLock<Option<EventSink>>,
    control_plane: RwLock<Option<ControlPlaneStatus>>,
    token_bindings: Mutex<HashMap<String, Option<String>>>,
    session_control_planes: Mutex<HashMap<String, ControlPlaneStatus>>,
    peer_slash_commands_allowed: bool,
    cross_pair_room_broadcast: bool,
    pty_spawner: RwLock<Arc<dyn PtySpawner>>,
    mailbox_fs: RwLock<Arc<dyn MailboxFs>>,
    background_runtime: Arc<tokio::runtime::Runtime>,
    events_seq: AtomicU64,
    events_watch: tokio::sync::watch::Sender<u64>,
    stale_event_drop_counts: Mutex<HashMap<(String, SessionGeneration), u64>>,
    stale_quiesce_drop_counts: Mutex<HashMap<(String, SessionGeneration), u64>>,
    dispatch_reactions: Mutex<HashMap<DispatchReactionKey, PendingDispatchReaction>>,
    started_at: Instant,
    heartbeat_interval: Duration,
    reaction_window: Duration,
    auto_restart_on_stall: AutoRestartOnStallConfig,
    auto_restart_history: Mutex<HashMap<String, AutoRestartHistory>>,
    #[cfg(test)]
    last_detached_worker: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventsSinceResult {
    events: Vec<RuntimeEvent>,
    next_cursor: EventCursor,
    gap_detected: bool,
    as_of: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AuditScan {
    events: Vec<RuntimeEvent>,
    next_offset: u64,
    reached_eof: bool,
    reached_limit: bool,
}

#[derive(Clone)]
pub struct SupervisorHandle {
    inner: Arc<SupervisorInner>,
}

impl SupervisorHandle {
    pub fn new(config: SupervisorConfig) -> Result<Self> {
        fs::create_dir_all(&config.runtime_dir).context("failed to create runtime directory")?;
        let audit = AuditLog::new(&config.runtime_dir)?;
        let background_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .context("failed to create supervisor background runtime")?;
        let (events_watch, _events_watch_rx) = tokio::sync::watch::channel(0_u64);
        let working_root = config.working_root.to_string_lossy().into_owned();
        let mut slots = HashMap::new();
        for definition in default_session_definitions(&working_root) {
            slots.insert(definition.name.clone(), closed_session_slot(definition));
        }
        let heartbeat_interval = config
            .heartbeat_interval
            .filter(|duration| !duration.is_zero())
            .unwrap_or_else(heartbeat_interval_from_env);
        let reaction_window = config
            .reaction_window
            .filter(|duration| !duration.is_zero())
            .unwrap_or_else(reaction_window_from_env);
        let auto_restart_on_stall = AutoRestartOnStallConfig {
            allowed_sessions: config
                .auto_restart_on_stall_sessions
                .unwrap_or_else(auto_restart_sessions_from_env)
                .into_iter()
                .map(|session| session.trim().to_string())
                .filter(|session| !session.is_empty())
                .collect(),
            threshold: config
                .auto_restart_stall_threshold
                .filter(|duration| !duration.is_zero())
                .unwrap_or_else(auto_restart_stall_threshold_from_env),
        };

        let handle = Self {
            inner: Arc::new(SupervisorInner {
                runtime_dir: config.runtime_dir,
                audit,
                slots: Mutex::new(slots),
                event_sink: RwLock::new(None),
                control_plane: RwLock::new(None),
                token_bindings: Mutex::new(HashMap::new()),
                session_control_planes: Mutex::new(HashMap::new()),
                peer_slash_commands_allowed: config.peer_slash_commands_allowed,
                cross_pair_room_broadcast: config.cross_pair_room_broadcast,
                pty_spawner: RwLock::new(Arc::new(ConcretePtySpawner)),
                mailbox_fs: RwLock::new(Arc::new(StdMailboxFs)),
                background_runtime: Arc::new(background_runtime),
                events_seq: AtomicU64::new(0),
                events_watch,
                stale_event_drop_counts: Mutex::new(HashMap::new()),
                stale_quiesce_drop_counts: Mutex::new(HashMap::new()),
                dispatch_reactions: Mutex::new(HashMap::new()),
                started_at: Instant::now(),
                heartbeat_interval,
                reaction_window,
                auto_restart_on_stall,
                auto_restart_history: Mutex::new(HashMap::new()),
                #[cfg(test)]
                last_detached_worker: Mutex::new(None),
            }),
        };
        handle.start_supervisor_heartbeat_task();
        Ok(handle)
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.inner.runtime_dir
    }

    pub fn audit_log_path(&self) -> PathBuf {
        self.inner.audit.path()
    }

    fn active_audit_file_name(&self) -> String {
        self.inner.audit.active_file_name()
    }

    fn audit_file_len(&self, audit_file: &str) -> Result<u64> {
        self.inner.audit.uncompressed_len(audit_file)
    }

    fn current_eof_cursor(&self) -> Result<EventCursor> {
        let audit_file = self.active_audit_file_name();
        let byte_offset = self.audit_file_len(&audit_file)?;
        Ok(EventCursor {
            audit_file,
            byte_offset,
        })
    }

    fn current_active_start_cursor(&self) -> EventCursor {
        EventCursor {
            audit_file: self.active_audit_file_name(),
            byte_offset: 0,
        }
    }

    fn start_supervisor_heartbeat_task(&self) {
        let interval = self.inner.heartbeat_interval;
        let weak = Arc::downgrade(&self.inner);
        self.inner.background_runtime.spawn(async move {
            loop {
                tokio::time::sleep(interval).await;
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                SupervisorHandle { inner }.emit_supervisor_heartbeat();
            }
        });
    }

    fn heartbeat_session_summaries(&self) -> Vec<HeartbeatSessionSummary> {
        let slots = self.inner.slots.lock();
        let mut summaries = slots
            .values()
            .map(|slot| HeartbeatSessionSummary {
                name: slot.definition.name.clone(),
                lifecycle_state: slot.state,
                work_state: slot.work_state_observed.then_some(slot.work_state),
                process_id: slot.process_id,
                last_activity_at: slot.last_activity_at.clone(),
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.name.cmp(&right.name));
        summaries
    }

    fn emit_supervisor_heartbeat(&self) {
        self.refresh_session_liveness();
        self.emit(RuntimeEvent::SupervisorHeartbeat {
            wrapper_pid: std::process::id(),
            uptime_secs: self.inner.started_at.elapsed().as_secs(),
            sessions: self.heartbeat_session_summaries(),
            timestamp: now_rfc3339(),
        });
    }

    fn normalize_events_filter(filter: Option<EventFilter>) -> EventFilter {
        filter.unwrap_or_default()
    }

    fn validate_events_filter(&self, filter: &EventFilter) -> Result<()> {
        for kind in &filter.include_kinds {
            if kind == EventFilter::ALL_KINDS {
                continue;
            }

            let known = matches!(
                kind.as_str(),
                "session_output"
                    | "session_state"
                    | "session_exit"
                    | "session_work_state"
                    | "supervisor_heartbeat"
                    | "supervisor_alert"
                    | "dispatch_template_warning"
                    | "pair_created"
                    | "pair_renamed"
                    | "pair_deleted"
                    | "routed_message"
                    | "route_delivery"
                    | "dispatch_attempt"
                    | "pane_signal"
                    | "system_log"
                    | "control_plane_ready"
                    | "sideband_request_lifecycle"
                    | "request_ack"
                    | "request_ack_timeout"
                    | "dispatch_no_reaction"
            );
            if !known {
                return Err(anyhow!("unknown event kind: '{kind}'"));
            }
        }

        if !filter.include_sessions.is_empty() {
            let configured_sessions = {
                let slots = self.inner.slots.lock();
                slots.keys().cloned().collect::<Vec<_>>()
            };
            for session in &filter.include_sessions {
                if !configured_sessions
                    .iter()
                    .any(|candidate| candidate == session)
                {
                    return Err(anyhow!("unknown session: '{session}'"));
                }
            }
        }

        for scope in &filter.include_scopes {
            let known = matches!(scope.as_str(), "direct" | "room" | "system" | "private");
            if !known {
                return Err(anyhow!("unknown routed_message scope: '{scope}'"));
            }
        }

        Ok(())
    }

    fn resolve_events_cursor(&self, cursor: Option<EventCursor>) -> Result<(EventCursor, bool)> {
        let Some(cursor) = cursor else {
            return Ok((self.current_eof_cursor()?, false));
        };

        if !is_valid_audit_filename(&cursor.audit_file) {
            return Err(anyhow!(
                "cursor audit_file not recognized: {}",
                cursor.audit_file
            ));
        }

        if self.inner.audit.source_exists(&cursor.audit_file) {
            return Ok((cursor, false));
        }

        Ok((self.current_active_start_cursor(), true))
    }

    fn events_request_error(
        &self,
        message: impl Into<String>,
        echoed_cursor: serde_json::Value,
    ) -> SidebandResponse {
        SidebandResponse {
            ok: false,
            message: message.into(),
            snapshot: Some(self.snapshot()),
            timed_out: false,
            payload: Some(SidebandResponsePayload::EventsSinceError { echoed_cursor }),
            request_id: None,
        }
    }

    fn read_events_since(
        &self,
        cursor: Option<EventCursor>,
        filter: &EventFilter,
        max_events: usize,
    ) -> Result<EventsSinceResult> {
        let (resolved_cursor, gap_detected) = self.resolve_events_cursor(cursor)?;
        let active_file = self.active_audit_file_name();
        let current_active_eof = self.current_eof_cursor()?;

        let current_len = self.audit_file_len(&resolved_cursor.audit_file)?;
        if resolved_cursor.byte_offset > current_len {
            return Ok(EventsSinceResult {
                events: Vec::new(),
                next_cursor: current_active_eof,
                gap_detected,
                as_of: now_rfc3339(),
            });
        }

        let mut events = Vec::new();
        let mut current_file = resolved_cursor.audit_file.clone();
        let mut current_offset = resolved_cursor.byte_offset;

        loop {
            let remaining = max_events.saturating_sub(events.len());
            let scan = self.scan_audit_file(&current_file, current_offset, filter, remaining)?;
            current_offset = scan.next_offset;
            events.extend(scan.events);

            if scan.reached_limit || events.len() >= max_events {
                return Ok(EventsSinceResult {
                    events,
                    next_cursor: EventCursor {
                        audit_file: current_file,
                        byte_offset: current_offset,
                    },
                    gap_detected,
                    as_of: now_rfc3339(),
                });
            }

            if scan.reached_eof && current_file != active_file {
                current_file = active_file.clone();
                current_offset = 0;
                continue;
            }

            return Ok(EventsSinceResult {
                events,
                next_cursor: EventCursor {
                    audit_file: current_file,
                    byte_offset: current_offset,
                },
                gap_detected,
                as_of: now_rfc3339(),
            });
        }
    }

    async fn wait_for_events(
        &self,
        cursor: Option<EventCursor>,
        filter: EventFilter,
        max_events: usize,
        max_wait: Duration,
    ) -> Result<EventsSinceResult> {
        let mut receiver = self.inner.events_watch.subscribe();
        let mut observed_seq = *receiver.borrow_and_update();
        let mut result = self.read_events_since(cursor, &filter, max_events)?;
        let mut gap_detected = result.gap_detected;

        if !result.events.is_empty() || max_wait.is_zero() {
            result.gap_detected = gap_detected;
            return Ok(result);
        }

        let deadline = tokio::time::Instant::now() + max_wait;
        let mut next_cursor = result.next_cursor.clone();

        loop {
            let now = tokio::time::Instant::now();
            if now >= deadline {
                result.gap_detected = gap_detected;
                return Ok(result);
            }

            let remaining = deadline.saturating_duration_since(now);
            let changed = tokio::time::timeout(remaining, receiver.changed()).await;
            match changed {
                Ok(Ok(())) => {
                    observed_seq = *receiver.borrow_and_update();
                    result =
                        self.read_events_since(Some(next_cursor.clone()), &filter, max_events)?;
                    gap_detected |= result.gap_detected;
                    result.gap_detected = gap_detected;
                    next_cursor = result.next_cursor.clone();

                    if !result.events.is_empty() {
                        return Ok(result);
                    }
                }
                Ok(Err(_)) | Err(_) => {
                    let current_seq = *receiver.borrow();
                    if current_seq != observed_seq {
                        observed_seq = *receiver.borrow_and_update();
                        result =
                            self.read_events_since(Some(next_cursor.clone()), &filter, max_events)?;
                        gap_detected |= result.gap_detected;
                        result.gap_detected = gap_detected;
                        next_cursor = result.next_cursor.clone();
                        if !result.events.is_empty() {
                            return Ok(result);
                        }
                        continue;
                    }

                    result.gap_detected = gap_detected;
                    return Ok(result);
                }
            }
        }
    }

    fn scan_audit_file(
        &self,
        audit_file: &str,
        byte_offset: u64,
        filter: &EventFilter,
        max_events: usize,
    ) -> Result<AuditScan> {
        if max_events == 0 {
            return Ok(AuditScan {
                events: Vec::new(),
                next_offset: byte_offset,
                reached_eof: false,
                reached_limit: true,
            });
        }

        let mut events = Vec::new();
        let mut next_offset = byte_offset;

        loop {
            let batch = self
                .inner
                .audit
                .read_lines_since(audit_file, next_offset, 1024)?;
            if batch.lines.is_empty() {
                return Ok(AuditScan {
                    events,
                    next_offset: batch.next_offset,
                    reached_eof: batch.reached_eof,
                    reached_limit: false,
                });
            }

            for line in batch.lines {
                let event: RuntimeEvent = serde_json::from_str(&line.text)
                    .with_context(|| format!("audit read error: invalid JSON in {audit_file}"))?;
                next_offset = line.next_offset;
                if event_matches_filter(&event, filter) {
                    events.push(event);
                }

                if events.len() >= max_events {
                    return Ok(AuditScan {
                        events,
                        next_offset,
                        reached_eof: false,
                        reached_limit: true,
                    });
                }
            }

            next_offset = batch.next_offset;
            if batch.reached_eof {
                return Ok(AuditScan {
                    events,
                    next_offset,
                    reached_eof: true,
                    reached_limit: false,
                });
            }
        }
    }

    fn bump_session_generation(&self, name: &str) -> Result<SessionGeneration> {
        let mut slots = self.inner.slots.lock();
        let slot = slots
            .get_mut(name)
            .with_context(|| format!("unknown session '{name}'"))?;
        cancel_quiesce_timer_locked(slot);
        slot.generation = slot.generation.wrapping_add(1);
        Ok(slot.generation)
    }

    #[cfg(test)]
    fn current_generation(&self, name: &str) -> Option<SessionGeneration> {
        self.inner
            .slots
            .lock()
            .get(name)
            .map(|slot| slot.generation)
    }

    pub fn set_event_sink<F>(&self, sink: F)
    where
        F: Fn(RuntimeEvent) + Send + Sync + 'static,
    {
        *self.inner.event_sink.write() = Some(Arc::new(sink));
    }

    fn emit_sideband_lifecycle(
        &self,
        request_id: &str,
        action: &str,
        session: Option<&str>,
        extra_args: &[String],
        phase: SidebandPhase,
        elapsed: Duration,
    ) {
        self.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
            request_id,
            action,
            session,
            extra_args,
            phase,
            elapsed,
            error: None,
        });
    }

    fn emit_sideband_lifecycle_with_error(&self, event: SidebandLifecycleEvent<'_>) {
        self.emit(RuntimeEvent::SidebandRequestLifecycle {
            request_id: event.request_id.to_string(),
            action: event.action.to_string(),
            session: event.session.map(ToOwned::to_owned),
            extra_args: event.extra_args.to_vec(),
            phase: event.phase,
            error: event.error,
            elapsed_ms: event.elapsed.as_millis() as u64,
            timestamp: now_rfc3339(),
        });
    }

    fn emit_request_ack(&self, context: &RequestAckContext, session: &str, bytes_written: usize) {
        self.emit(RuntimeEvent::RequestAck {
            request_id: context.request_id.clone(),
            session: session.to_string(),
            action: context.action.clone(),
            bytes_written,
            timestamp: now_rfc3339(),
        });
    }

    fn emit_dispatch_no_reaction(&self, context: &RequestAckContext, session: &str, detail: &str) {
        let (last_work_state, last_session_state) = self.session_state_for_alert(session);
        self.emit(RuntimeEvent::DispatchNoReaction {
            request_id: context.request_id.clone(),
            session: session.to_string(),
            action: context.action.clone(),
            timestamp: now_rfc3339(),
        });
        self.emit_supervisor_alert(SupervisorAlertEvent {
            alert_type: SupervisorAlertType::DispatchNoReaction,
            request_id: Some(context.request_id.clone()),
            session: Some(session.to_string()),
            action: Some(context.action.clone()),
            last_work_state,
            last_session_state,
            message: format!(
                "Dispatch {} to {} produced no live pane reaction: {}",
                context.action, session, detail
            ),
            severity: AlertSeverity::Critical,
        });
    }

    fn register_dispatch_reaction(
        &self,
        context: &RequestAckContext,
        session: &str,
    ) -> DispatchReactionWaiter {
        let (sender, receiver) = mpsc::channel();
        let key = DispatchReactionKey {
            request_id: context.request_id.clone(),
            session: session.to_string(),
        };
        self.inner.dispatch_reactions.lock().insert(
            key.clone(),
            PendingDispatchReaction {
                baseline: Instant::now(),
                sender,
            },
        );
        DispatchReactionWaiter { key, receiver }
    }

    fn cancel_dispatch_reaction(&self, waiter: &DispatchReactionWaiter) {
        self.inner.dispatch_reactions.lock().remove(&waiter.key);
    }

    fn resolve_dispatch_reactions_for_session(
        &self,
        session: &str,
        observed_at: Instant,
        outcome: DispatchReactionOutcome,
    ) {
        let matches = {
            let mut reactions = self.inner.dispatch_reactions.lock();
            let keys = reactions
                .iter()
                .filter(|(key, pending)| key.session == session && observed_at >= pending.baseline)
                .map(|(key, _)| key.clone())
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| reactions.remove(&key))
                .collect::<Vec<_>>()
        };

        for pending in matches {
            let _ = pending.sender.send(outcome);
        }
    }

    fn wait_for_dispatch_reaction(
        &self,
        waiter: DispatchReactionWaiter,
        context: &RequestAckContext,
        session: &str,
        bytes_written: usize,
    ) -> Result<()> {
        match waiter.receiver.recv_timeout(self.inner.reaction_window) {
            Ok(DispatchReactionOutcome::Reacted) => {
                self.mark_dispatch_reacted(context, session, bytes_written)?;
                Ok(())
            }
            Ok(DispatchReactionOutcome::Terminal) => {
                self.emit_dispatch_no_reaction(context, session, "terminal output observed");
                Err(anyhow!(
                    "dispatch no reaction from session '{}': terminal output observed",
                    session
                ))
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                self.cancel_dispatch_reaction(&waiter);
                self.emit_dispatch_no_reaction(
                    context,
                    session,
                    &format!(
                        "no output, work-state transition, or routed_message within {}ms",
                        self.inner.reaction_window.as_millis()
                    ),
                );
                Err(anyhow!(
                    "dispatch no reaction from session '{}' within {}ms",
                    session,
                    self.inner.reaction_window.as_millis()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.emit_dispatch_no_reaction(context, session, "reaction channel disconnected");
                Err(anyhow!(
                    "dispatch no reaction from session '{}': reaction channel disconnected",
                    session
                ))
            }
        }
    }

    fn mark_dispatch_reacted(
        &self,
        context: &RequestAckContext,
        session: &str,
        bytes_written: usize,
    ) -> Result<()> {
        let state_event = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(session)
                .with_context(|| format!("unknown session '{session}'"))?;
            cancel_quiesce_timer_locked(slot);
            slot.state = LifecycleState::Busy;
            slot.last_activity_at = Some(now_rfc3339());
            RuntimeEvent::SessionState {
                session: session.to_string(),
                state: LifecycleState::Busy,
                reason: "dispatch reaction observed".into(),
                timestamp: now_rfc3339(),
            }
        };
        self.emit(state_event);
        self.emit_request_ack(context, session, bytes_written);
        Ok(())
    }

    fn session_state_for_alert(
        &self,
        session: &str,
    ) -> (Option<WorkState>, Option<LifecycleState>) {
        let slots = self.inner.slots.lock();
        let Some(slot) = slots.get(session) else {
            return (None, None);
        };

        (
            slot.work_state_observed.then_some(slot.work_state),
            Some(slot.state),
        )
    }

    fn emit_supervisor_alert(&self, alert: SupervisorAlertEvent) {
        self.emit(RuntimeEvent::SupervisorAlert {
            alert_type: alert.alert_type,
            request_id: alert.request_id,
            session: alert.session,
            action: alert.action,
            last_work_state: alert.last_work_state,
            last_session_state: alert.last_session_state,
            message: alert.message,
            severity: alert.severity,
            timestamp: now_rfc3339(),
        });
    }

    #[cfg(test)]
    fn emit_request_ack_timeout(
        &self,
        context: &RequestAckContext,
        session: &str,
        elapsed: Duration,
    ) {
        let (last_work_state, last_session_state) = self.session_state_for_alert(session);
        self.emit(RuntimeEvent::RequestAckTimeout {
            request_id: context.request_id.clone(),
            session: session.to_string(),
            action: context.action.clone(),
            elapsed_ms: elapsed.as_millis() as u64,
            timestamp: now_rfc3339(),
        });
        self.emit_supervisor_alert(SupervisorAlertEvent {
            alert_type: SupervisorAlertType::AckTimeout,
            request_id: Some(context.request_id.clone()),
            session: Some(session.to_string()),
            action: Some(context.action.clone()),
            last_work_state,
            last_session_state,
            message: format!(
                "Dispatch {} to {} didn't ACK in {}s; last work_state={}",
                context.action,
                session,
                elapsed.as_secs(),
                work_state_alert_label(last_work_state)
            ),
            severity: AlertSeverity::Warn,
        });
    }

    fn emit_route_delivery(&self, event: RouteDeliveryEvent) {
        self.emit(RuntimeEvent::RouteDelivery {
            request_id: event.request_id,
            route_id: event.route_id,
            from: event.from,
            logical_to: event.logical_to,
            scope: event.scope,
            recipient: event.recipient,
            recipient_index: event.recipient_index,
            recipient_count: event.recipient_count,
            payload_part_count: event.payload_part_count,
            phase: event.phase,
            bytes_written: event.bytes_written,
            error: event.error,
            timestamp: now_rfc3339(),
        });
    }

    fn emit_dispatch_attempt(
        &self,
        request_id: &str,
        action: &str,
        from: &str,
        target_session: &str,
    ) -> Result<DispatchAttemptDecision> {
        self.refresh_session_liveness();
        let decision = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get(target_session)
                .with_context(|| format!("unknown session '{target_session}'"))?;
            let target_work_state_before = slot.work_state_observed.then_some(slot.work_state);
            let last_route_from_target_instant = slot.last_route_from_session_instant;
            let mut decision = DispatchAttemptDecision {
                target_lifecycle_state_before: slot.state,
                target_work_state_before,
                target_last_activity_at: slot.last_activity_at.clone(),
                last_route_from_target_at: slot.last_route_from_session_at.clone(),
                last_route_from_target_instant,
                reason: None,
            };
            decision.reason = dispatch_overlap_reason(&decision);
            decision
        };

        self.emit(RuntimeEvent::DispatchAttempt {
            request_id: request_id.to_string(),
            action: action.to_string(),
            from: from.to_string(),
            target_session: target_session.to_string(),
            target_lifecycle_state_before: decision.target_lifecycle_state_before,
            target_work_state_before: decision.target_work_state_before,
            target_last_activity_at: decision.target_last_activity_at.clone(),
            last_route_from_target_at: decision.last_route_from_target_at.clone(),
            overlap: decision.overlap(),
            reason: decision.reason.map(ToOwned::to_owned),
            timestamp: now_rfc3339(),
        });

        Ok(decision)
    }

    fn emit_dispatch_template_warning_if_needed(
        &self,
        request_id: &str,
        session: &str,
        content: &str,
    ) {
        let (detected_patterns, missing_patterns) = dispatch_template_pattern_match(content);
        if !detected_patterns.is_empty() {
            return;
        }

        self.emit(RuntimeEvent::DispatchTemplateWarning {
            request_id: request_id.to_string(),
            session: session.to_string(),
            detected_patterns,
            missing_patterns,
            severity: AlertSeverity::Info,
            timestamp: now_rfc3339(),
        });
    }

    fn record_route_from_session(&self, session: &str) {
        let timestamp = now_rfc3339();
        let now = Instant::now();
        let mut slots = self.inner.slots.lock();
        if let Some(slot) = slots.get_mut(session) {
            slot.last_route_from_session_at = Some(timestamp);
            slot.last_route_from_session_instant = Some(now);
        }
        drop(slots);
        self.resolve_dispatch_reactions_for_session(session, now, DispatchReactionOutcome::Reacted);
    }

    fn record_pane_signal(
        &self,
        request_id: &str,
        signal: PaneSignalRecord,
    ) -> Result<PaneSignalWritePaths> {
        let task_id = normalized_required_field("task_id", signal.task_id)?;
        let summary = normalized_required_field("summary", signal.summary)?;
        let session = self.resolve_pane_signal_session(&signal.token)?;
        let now = Utc::now();
        let timestamp = now.to_rfc3339();
        let signal_type_name = pane_signal_type_name(signal.signal_type);
        let safe_task_id = pane_signal_filename_component(&task_id);
        let signal_path = self.runtime_dir().join("signals").join(format!(
            "{}__{}__{}.json",
            safe_task_id,
            signal_type_name,
            pane_signal_file_timestamp(now)
        ));
        let legacy_touch_path = self
            .runtime_dir()
            .join("dispatch-triggers")
            .join(format!("{safe_task_id}.{signal_type_name}"));
        let event = RuntimeEvent::PaneSignal {
            request_id: request_id.to_string(),
            session,
            task_id,
            signal_type: signal.signal_type,
            summary,
            artifact_paths: signal.artifact_paths,
            commit_sha: signal.commit_sha,
            timestamp,
        };
        let payload = format!("{}\n", serde_json::to_string_pretty(&event)?);

        write_atomic_bytes(&signal_path, payload.as_bytes()).with_context(|| {
            format!(
                "failed to write canonical pane signal {}",
                signal_path.display()
            )
        })?;

        if let Err(error) = write_empty_touch_file(&legacy_touch_path) {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Warn,
                message: format!(
                    "pane_signal legacy touch-file write failed for {}: {error}",
                    legacy_touch_path.display()
                ),
                timestamp: now_rfc3339(),
            });
        }

        self.emit(event);

        Ok(PaneSignalWritePaths {
            signal_path,
            legacy_touch_path,
        })
    }

    fn arm_quiesce_timer(
        &self,
        session_name: &str,
        driver: DriverKind,
        generation: SessionGeneration,
        armed_at: Instant,
    ) {
        let Some(threshold) = quiesce_threshold(driver) else {
            return;
        };

        let handle = self.clone();
        let session_name_owned = session_name.to_string();
        let task = self.inner.background_runtime.spawn(async move {
            tokio::time::sleep(threshold).await;
            handle.fire_quiesce_timer(session_name_owned, generation, armed_at, threshold);
        });

        let mut slots = self.inner.slots.lock();
        if let Some(slot) = slots.get_mut(session_name) {
            cancel_quiesce_timer_locked(slot);
            slot.quiesce_timer = Some(QuiesceTimer {
                generation,
                handle: task,
            });
        } else {
            task.abort();
        }
    }

    fn fire_quiesce_timer(
        &self,
        session_name: String,
        armed_generation: SessionGeneration,
        armed_at: Instant,
        threshold: Duration,
    ) {
        let (events, stale_drop) = {
            let mut slots = self.inner.slots.lock();
            let Some(slot) = slots.get_mut(&session_name) else {
                return;
            };
            let timer = slot.quiesce_timer.take();
            match timer {
                Some(timer) if timer.generation == armed_generation => {
                    if slot.generation == armed_generation
                        && slot.state == LifecycleState::Ready
                        && slot.last_real_output_at == Some(armed_at)
                    {
                        slot.state = LifecycleState::Idle;
                        slot.last_activity_at = Some(now_rfc3339());
                        let mut events = vec![RuntimeEvent::SessionState {
                            session: session_name.clone(),
                            state: LifecycleState::Idle,
                            reason: format!("quiesce timeout {}s", threshold.as_secs()),
                            timestamp: now_rfc3339(),
                        }];
                        if let Some(event) =
                            transition_work_state_locked(&session_name, slot, WorkState::Idle, None)
                        {
                            events.push(event);
                        }
                        (events, false)
                    } else {
                        (Vec::new(), true)
                    }
                }
                Some(timer) => {
                    slot.quiesce_timer = Some(timer);
                    (Vec::new(), true)
                }
                None => (Vec::new(), true),
            }
        };

        if stale_drop {
            self.note_stale_quiesce_drop(&session_name, armed_generation);
        }
        for event in events {
            self.emit(event);
        }
    }

    fn note_stale_quiesce_drop(&self, session_name: &str, generation: SessionGeneration) {
        let counter = {
            let mut counts = self.inner.stale_quiesce_drop_counts.lock();
            let count = counts
                .entry((session_name.to_string(), generation))
                .or_insert(0);
            *count += 1;
            *count
        };

        if counter == 1 || counter % 10 == 0 {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: format!(
                    "Dropped stale quiesce timer for session '{session_name}' generation {generation} (count={counter})"
                ),
                timestamp: now_rfc3339(),
            });
        }
    }

    fn handle_session_work_state_side_effects(&self, session_name: &str, state: WorkState) {
        let is_stall_state = matches!(state, WorkState::Blocked | WorkState::ErrorLoop);
        let now = Instant::now();
        let timestamp = now_rfc3339();
        let enabled = self.inner.auto_restart_on_stall.enabled_for(session_name);
        let threshold = self.inner.auto_restart_on_stall.threshold;
        let weak = Arc::downgrade(&self.inner);

        let mut slots = self.inner.slots.lock();
        let Some(slot) = slots.get_mut(session_name) else {
            return;
        };
        cancel_stall_detector_locked(slot);

        if !is_stall_state {
            slot.stall_state_entered_at = None;
            slot.stall_state_entered_timestamp = None;
            return;
        }

        slot.stall_state_entered_at = Some(now);
        slot.stall_state_entered_timestamp = Some(timestamp);

        if !enabled {
            return;
        }

        let session = session_name.to_string();
        let generation = slot.generation;
        let task_session = session.clone();
        let task = self.inner.background_runtime.spawn(async move {
            tokio::time::sleep(threshold).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            SupervisorHandle { inner }
                .fire_stall_detector(task_session, generation, state, now)
                .await;
        });
        slot.stall_detector = Some(StallDetector {
            generation,
            state,
            entered_at: now,
            handle: task,
        });
    }

    async fn fire_stall_detector(
        &self,
        session: String,
        generation: SessionGeneration,
        state: WorkState,
        entered_at: Instant,
    ) {
        let Some(stall) = self.current_stall_snapshot(&session, generation, state, entered_at)
        else {
            return;
        };

        match self.reserve_auto_restart_attempt(&session) {
            AutoRestartReservation::Reserved => {
                self.emit_supervisor_alert(SupervisorAlertEvent {
                    alert_type: SupervisorAlertType::SessionStallDetected,
                    request_id: None,
                    session: Some(session.clone()),
                    action: Some("restart_session".into()),
                    last_work_state: stall.last_work_state,
                    last_session_state: stall.last_session_state,
                    message: format!(
                        "Session {} stayed in {} for {}s; issuing auto-restart",
                        session,
                        work_state_alert_label(stall.last_work_state),
                        self.inner.auto_restart_on_stall.threshold.as_secs()
                    ),
                    severity: AlertSeverity::Critical,
                });

                let restart_handle = self.clone();
                let restart_session = session.clone();
                let join = tokio::task::spawn_blocking(move || {
                    restart_handle.restart_session_at(&restart_session, generation)
                })
                .await;
                match join {
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => self.emit_supervisor_alert(SupervisorAlertEvent {
                        alert_type: SupervisorAlertType::OperatorAttention,
                        request_id: None,
                        session: Some(session),
                        action: Some("restart_session".into()),
                        last_work_state: stall.last_work_state,
                        last_session_state: stall.last_session_state,
                        message: format!("Auto-restart failed: {error}"),
                        severity: AlertSeverity::Critical,
                    }),
                    Err(error) => self.emit_supervisor_alert(SupervisorAlertEvent {
                        alert_type: SupervisorAlertType::OperatorAttention,
                        request_id: None,
                        session: Some(session),
                        action: Some("restart_session".into()),
                        last_work_state: stall.last_work_state,
                        last_session_state: stall.last_session_state,
                        message: format!("Auto-restart worker join failed: {error}"),
                        severity: AlertSeverity::Critical,
                    }),
                }
            }
            AutoRestartReservation::CapReached => {
                self.emit_supervisor_alert(SupervisorAlertEvent {
                    alert_type: SupervisorAlertType::SessionStallDetected,
                    request_id: None,
                    session: Some(session),
                    action: Some("restart_session".into()),
                    last_work_state: stall.last_work_state,
                    last_session_state: stall.last_session_state,
                    message: "Auto-restart cap reached: 3 restarts in 30 minutes; manual operator intervention required".to_string(),
                    severity: AlertSeverity::Critical,
                });
            }
            AutoRestartReservation::Disabled => {}
        }
    }

    fn current_stall_snapshot(
        &self,
        session: &str,
        generation: SessionGeneration,
        state: WorkState,
        entered_at: Instant,
    ) -> Option<StallAlertSnapshot> {
        let mut slots = self.inner.slots.lock();
        let slot = slots.get_mut(session)?;
        let detector_matches = slot
            .stall_detector
            .as_ref()
            .map(|detector| {
                detector.generation == generation
                    && detector.state == state
                    && detector.entered_at == entered_at
            })
            .unwrap_or(false);
        if !detector_matches
            || slot.generation != generation
            || slot.work_state != state
            || !matches!(slot.work_state, WorkState::Blocked | WorkState::ErrorLoop)
            || slot.state != LifecycleState::Ready
            || slot.running.is_none()
            || matches!(
                slot.stop_intent,
                Some(StopIntent {
                    kind: StopIntentKind::Operator,
                    ..
                })
            )
        {
            return None;
        }

        slot.stall_detector.take();
        Some(StallAlertSnapshot {
            last_work_state: slot.work_state_observed.then_some(slot.work_state),
            last_session_state: Some(slot.state),
        })
    }

    fn reserve_auto_restart_attempt(&self, session: &str) -> AutoRestartReservation {
        if !self.inner.auto_restart_on_stall.enabled_for(session) {
            return AutoRestartReservation::Disabled;
        }

        let now = Instant::now();
        let mut history_by_session = self.inner.auto_restart_history.lock();
        let history = history_by_session.entry(session.to_string()).or_default();
        if history.disabled {
            return AutoRestartReservation::Disabled;
        }

        history
            .attempts
            .retain(|attempt| now.duration_since(*attempt) <= AUTO_RESTART_WINDOW);
        if history.attempts.len() >= AUTO_RESTART_MAX_PER_WINDOW {
            history.disabled = true;
            return AutoRestartReservation::CapReached;
        }

        history.attempts.push(now);
        AutoRestartReservation::Reserved
    }

    #[cfg(test)]
    fn set_pty_spawner_for_tests(&self, spawner: Arc<dyn PtySpawner>) {
        *self.inner.pty_spawner.write() = spawner;
    }

    #[cfg(test)]
    fn set_mailbox_fs_for_tests(&self, mailbox_fs: Arc<dyn MailboxFs>) {
        *self.inner.mailbox_fs.write() = mailbox_fs;
    }

    #[cfg(test)]
    fn set_last_detached_worker_receiver(&self, receiver: std::sync::mpsc::Receiver<()>) {
        *self.inner.last_detached_worker.lock() = Some(receiver);
    }

    #[cfg(test)]
    fn test_wait_for_last_worker(&self, timeout: Duration) -> bool {
        self.inner
            .last_detached_worker
            .lock()
            .take()
            .and_then(|receiver| receiver.recv_timeout(timeout).ok())
            .is_some()
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.refresh_session_liveness();
        let mut sessions = self
            .inner
            .slots
            .lock()
            .values()
            .map(SessionSlot::snapshot)
            .collect::<Vec<_>>();
        sessions.sort_by(|left, right| left.name.cmp(&right.name));

        RuntimeSnapshot {
            sessions,
            control_plane: self.inner.control_plane.read().clone(),
            runtime_dir: self.runtime_dir().display().to_string(),
            audit_log_path: self.audit_log_path().display().to_string(),
            generated_at: now_rfc3339(),
        }
    }

    pub fn start_session(&self, name: &str, extra_args: Vec<String>) -> Result<SessionSnapshot> {
        validate_extra_args(&extra_args)?;
        self.refresh_session_liveness();
        let expected = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }
            cancel_quiesce_timer_locked(slot);
            slot.generation = slot.generation.wrapping_add(1);
            slot.generation
        };
        self.start_session_at(name, expected, extra_args)
    }

    pub fn stop_session(&self, name: &str) -> Result<SessionSnapshot> {
        let expected = self.bump_session_generation(name)?;
        self.stop_session_at(name, expected)
    }

    pub fn restart_session(&self, name: &str) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let expected_stop = self.bump_session_generation(name)?;
        self.restart_session_at(name, expected_stop)
    }

    pub fn shutdown(&self) -> Result<()> {
        let ptys = {
            let mut slots = self.inner.slots.lock();
            let mut ptys = Vec::new();
            for (name, slot) in slots.iter_mut() {
                cancel_quiesce_timer_locked(slot);
                slot.state = LifecycleState::Closed;
                slot.process_id = None;
                slot.last_activity_at = Some(now_rfc3339());
                slot.last_real_output_at = None;
                reset_work_state_locked(slot);
                if let Some(mut running) = slot.running.take()
                    && let Some(pty) = running.pty.take()
                {
                    ptys.push((name.clone(), pty));
                }
            }
            ptys
        };

        for (name, pty) in ptys {
            if let Err(error) = pty.kill() {
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!("shutdown: failed to kill session '{name}': {error:#}"),
                    timestamp: now_rfc3339(),
                });
            }
            drop(pty);
        }

        Ok(())
    }

    pub fn create_pair(&self, name: &str) -> Result<Vec<SessionSnapshot>> {
        let snapshots = {
            let mut slots = self.inner.slots.lock();
            validate_pair_name(name)?;
            ensure_pair_name_available(&slots, name)?;

            let working_dir = slots
                .get("claude")
                .or_else(|| slots.get("codex"))
                .map(|slot| slot.definition.working_dir.clone())
                .ok_or_else(|| anyhow!("main pair is missing from supervisor state"))?;
            let [claude_definition, codex_definition] =
                pair_session_definitions(name, &working_dir);

            let claude_slot = closed_session_slot(claude_definition);
            let codex_slot = closed_session_slot(codex_definition);
            let mut snapshots = vec![claude_slot.snapshot(), codex_slot.snapshot()];
            snapshots.sort_by(|left, right| left.name.cmp(&right.name));

            slots.insert(claude_slot.definition.name.clone(), claude_slot);
            slots.insert(codex_slot.definition.name.clone(), codex_slot);
            snapshots
        };

        self.emit(RuntimeEvent::PairCreated {
            name: name.to_string(),
            timestamp: now_rfc3339(),
        });

        Ok(snapshots)
    }

    pub fn rename_pair(&self, old_name: &str, new_name: &str) -> Result<Vec<SessionSnapshot>> {
        self.refresh_session_liveness();
        if old_name == "main" {
            return Err(anyhow!("cannot rename the main pair"));
        }

        let snapshots = {
            let mut slots = self.inner.slots.lock();
            validate_pair_name(new_name)?;
            ensure_pair_name_available(&slots, new_name)?;

            let (old_claude_name, old_codex_name) = pair_slot_names(old_name);
            let old_claude = slots
                .get(&old_claude_name)
                .ok_or_else(|| anyhow!("pair '{old_name}' not found"))?;
            let old_codex = slots
                .get(&old_codex_name)
                .ok_or_else(|| anyhow!("pair '{old_name}' not found"))?;

            if old_claude.state != LifecycleState::Closed
                || old_claude.running.is_some()
                || old_codex.state != LifecycleState::Closed
                || old_codex.running.is_some()
            {
                return Err(anyhow!(
                    "pair '{old_name}' has running sessions; stop both panes first"
                ));
            }

            let working_dir = old_claude.definition.working_dir.clone();
            let [new_claude_definition, new_codex_definition] =
                pair_session_definitions(new_name, &working_dir);

            let old_claude_slot = slots
                .remove(&old_claude_name)
                .expect("validated pair slot disappeared during rename");
            let old_codex_slot = slots
                .remove(&old_codex_name)
                .expect("validated pair slot disappeared during rename");

            let new_claude_slot = renamed_closed_slot(old_claude_slot, new_claude_definition);
            let new_codex_slot = renamed_closed_slot(old_codex_slot, new_codex_definition);
            let mut snapshots = vec![new_claude_slot.snapshot(), new_codex_slot.snapshot()];
            snapshots.sort_by(|left, right| left.name.cmp(&right.name));

            slots.insert(new_claude_slot.definition.name.clone(), new_claude_slot);
            slots.insert(new_codex_slot.definition.name.clone(), new_codex_slot);
            snapshots
        };

        self.emit(RuntimeEvent::PairRenamed {
            old_name: old_name.to_string(),
            new_name: new_name.to_string(),
            timestamp: now_rfc3339(),
        });

        Ok(snapshots)
    }

    pub fn delete_pair(&self, name: &str) -> Result<()> {
        self.refresh_session_liveness();
        if name == "main" {
            return Err(anyhow!("cannot delete the main pair"));
        }

        let (claude_name, codex_name) = pair_slot_names(name);
        let sessions_to_stop = {
            let slots = self.inner.slots.lock();
            let claude_slot = slots
                .get(&claude_name)
                .ok_or_else(|| anyhow!("pair '{name}' not found"))?;
            let codex_slot = slots
                .get(&codex_name)
                .ok_or_else(|| anyhow!("pair '{name}' not found"))?;

            let mut sessions = Vec::new();
            if claude_slot.running.is_some() {
                sessions.push((claude_name.clone(), claude_slot.generation));
            }
            if codex_slot.running.is_some() {
                sessions.push((codex_name.clone(), codex_slot.generation));
            }
            sessions
        };

        for (session_name, generation) in sessions_to_stop {
            self.stop_session_at(&session_name, generation)?;
        }

        {
            let mut slots = self.inner.slots.lock();
            let claude_slot = slots
                .get(&claude_name)
                .ok_or_else(|| anyhow!("pair '{name}' not found"))?;
            let codex_slot = slots
                .get(&codex_name)
                .ok_or_else(|| anyhow!("pair '{name}' not found"))?;
            if claude_slot.running.is_some() || codex_slot.running.is_some() {
                return Err(anyhow!(
                    "pair '{name}' has running sessions; stop both panes first"
                ));
            }

            slots.remove(&claude_name);
            slots.remove(&codex_name);
        }

        self.emit(RuntimeEvent::PairDeleted {
            name: name.to_string(),
            timestamp: now_rfc3339(),
        });

        Ok(())
    }

    fn stop_session_at(&self, name: &str, expected: SessionGeneration) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let (pty, stop_intent, process_id) = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.generation != expected {
                return Err(anyhow!(
                    "stop_session_at superseded: expected gen {expected}, current {}",
                    slot.generation
                ));
            }

            cancel_quiesce_timer_locked(slot);
            let pty = slot.running.take().and_then(|running| running.pty);
            let process_id = slot.process_id;
            let stop_intent = pty.as_ref().map(|_| StopIntent {
                generation: expected,
                kind: if slot.state == LifecycleState::Restarting {
                    StopIntentKind::Restart
                } else {
                    StopIntentKind::Operator
                },
            });
            slot.stop_intent = stop_intent;
            slot.state = LifecycleState::Closed;
            slot.process_id = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            reset_work_state_locked(slot);
            (pty, stop_intent, process_id)
        };

        let mut exit_status = None;
        let mut exit_poll_error = None;
        if let Some(pty) = pty {
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let kill_error = pty.kill().err().map(|error| error.to_string());
                let (status, poll_error) = match pty.try_wait() {
                    Ok(status) => (status, None),
                    Err(error) => (None, Some(error.to_string())),
                };
                let _ = tx.send((kill_error, status, poll_error));
            });
            match rx.recv_timeout(Duration::from_secs(5)) {
                Ok((kill_error, status, poll_error)) => {
                    exit_status = status;
                    exit_poll_error = kill_error.or(poll_error);
                }
                Err(_) => {
                    exit_poll_error =
                        Some("pty.kill() did not return within 5s; exit status unavailable".into());
                    self.emit(RuntimeEvent::SystemLog {
                        level: LogLevel::Warn,
                        message: format!(
                            "pty.kill() for session '{name}' (gen {expected}) did not return within 5s; proceeding (resource may be leaked)"
                        ),
                        timestamp: now_rfc3339(),
                    });
                }
            }
        }

        let exit_classification = stop_intent.map(|intent| {
            classify_session_exit(
                exit_status,
                exit_poll_error,
                Some(intent),
                SessionExitReason::ProcessDisappeared,
                "requested stop completed without exit status",
            )
        });

        let snapshot = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .expect("session disappeared during stop_session_at");
            if slot.generation != expected {
                return Err(anyhow!(
                    "stop_session_at superseded mid-op: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
            if let Some(classification) = &exit_classification {
                slot.last_error = classification.last_error.clone();
                if slot.stop_intent.map(|intent| intent.generation) == Some(expected) {
                    slot.stop_intent = None;
                }
            }
            slot.snapshot()
        };

        let timestamp = now_rfc3339();
        if let Some(classification) = &exit_classification {
            self.emit(session_exit_event(
                snapshot.name.clone(),
                expected,
                process_id,
                classification,
                timestamp.clone(),
            ));
        }

        {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get(name)
                .expect("session disappeared during stop_session_at");
            if slot.generation != expected {
                return Err(anyhow!(
                    "stop_session_at superseded mid-op: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
        }

        self.emit(RuntimeEvent::SessionState {
            session: snapshot.name.clone(),
            state: snapshot.lifecycle_state,
            reason: "session stopped".into(),
            timestamp,
        });
        Ok(snapshot)
    }

    fn start_session_at(
        &self,
        name: &str,
        expected: SessionGeneration,
        extra_args: Vec<String>,
    ) -> Result<SessionSnapshot> {
        validate_extra_args(&extra_args)?;
        self.refresh_session_liveness();
        let definition = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.generation != expected {
                return Err(anyhow!(
                    "start_session_at superseded: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }

            cancel_quiesce_timer_locked(slot);
            slot.state = LifecycleState::Starting;
            slot.last_error = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            reset_work_state_locked(slot);
            let snapshot = slot.snapshot();
            let mut definition = slot.definition.clone();
            definition.args.extend(extra_args.clone());
            drop(slots);

            self.emit(RuntimeEvent::SessionState {
                session: snapshot.name,
                state: LifecycleState::Starting,
                reason: "launch requested".into(),
                timestamp: now_rfc3339(),
            });
            definition
        };

        let definition = self.prepare_definition_for_spawn(&definition)?;
        let spec = build_launch_spec(&definition);
        let session_name = definition.name.clone();
        let handle = self.clone();
        let handler: PtyEventHandler = Arc::new(move |event| {
            handle.handle_pty_event(&session_name, expected, event);
        });

        match self.inner.pty_spawner.read().clone().spawn(&spec, handler) {
            Ok(pty) => {
                let snapshot = {
                    let mut slots = self.inner.slots.lock();
                    let slot = slots
                        .get_mut(name)
                        .expect("session disappeared during start_session_at success");
                    if slot.generation != expected {
                        let _ = pty.kill();
                        return Err(anyhow!(
                            "start_session_at superseded mid-spawn: expected gen {expected}, current {}",
                            slot.generation
                        ));
                    }

                    slot.process_id = pty.process_id();
                    slot.running = Some(RunningSession { pty: Some(pty) });
                    slot.state = LifecycleState::Ready;
                    slot.last_activity_at = Some(now_rfc3339());
                    slot.last_real_output_at = None;
                    reset_work_state_locked(slot);
                    slot.snapshot()
                };
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: format!("Started {} session", snapshot.title),
                    timestamp: now_rfc3339(),
                });
                self.emit(RuntimeEvent::SessionState {
                    session: snapshot.name.clone(),
                    state: snapshot.lifecycle_state,
                    reason: "session ready".into(),
                    timestamp: now_rfc3339(),
                });
                Ok(snapshot)
            }
            Err(error) => {
                let mut slots = self.inner.slots.lock();
                let slot = slots
                    .get_mut(name)
                    .expect("session disappeared during start_session_at failure");
                if slot.generation == expected {
                    slot.running = None;
                    slot.process_id = None;
                    slot.state = LifecycleState::Failed;
                    slot.last_error = Some(error.to_string());
                    slot.last_real_output_at = None;
                    reset_work_state_locked(slot);
                    let snapshot = slot.snapshot();
                    drop(slots);
                    self.emit(RuntimeEvent::SystemLog {
                        level: LogLevel::Error,
                        message: format!("Failed to start {}: {error}", snapshot.title),
                        timestamp: now_rfc3339(),
                    });
                    self.emit(RuntimeEvent::SessionState {
                        session: snapshot.name.clone(),
                        state: snapshot.lifecycle_state,
                        reason: "spawn failed".into(),
                        timestamp: now_rfc3339(),
                    });
                }
                Err(error)
            }
        }
    }

    fn restart_session_at(
        &self,
        name: &str,
        expected_stop: SessionGeneration,
    ) -> Result<SessionSnapshot> {
        {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.generation != expected_stop {
                return Err(anyhow!(
                    "restart_session_at superseded: expected gen {expected_stop}, current {}",
                    slot.generation
                ));
            }
            cancel_quiesce_timer_locked(slot);
            slot.state = LifecycleState::Restarting;
            slot.last_activity_at = Some(now_rfc3339());
            reset_work_state_locked(slot);
            let snapshot = slot.snapshot();
            drop(slots);
            self.emit(RuntimeEvent::SessionState {
                session: snapshot.name,
                state: LifecycleState::Restarting,
                reason: "restart requested".into(),
                timestamp: now_rfc3339(),
            });
        }

        self.stop_session_at(name, expected_stop)?;
        let expected_start = self.bump_session_generation(name)?;
        self.start_session_at(name, expected_start, Vec::new())
    }

    fn send_input_with_bytes(&self, request: SendInputRequest) -> Result<(SessionSnapshot, usize)> {
        self.refresh_session_liveness();
        let (snapshot, bytes_written) = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(&request.name)
                .with_context(|| format!("unknown session '{}'", request.name))?;
            let running = slot
                .running
                .as_ref()
                .ok_or_else(|| anyhow!("session '{}' is not running", request.name))?;
            let bytes_written = running
                .pty
                .as_ref()
                .ok_or_else(|| anyhow!("session '{}' transport is not available", request.name))?
                .send_input(&request.input)?;
            (slot.snapshot(), bytes_written)
        };

        Ok((snapshot, bytes_written))
    }

    pub fn send_input(&self, request: SendInputRequest) -> Result<SessionSnapshot> {
        let request_id = Uuid::new_v4().to_string();
        self.emit_dispatch_attempt(&request_id, "send_input", "operator", &request.name)?;
        self.send_input_with_bytes(request)
            .map(|(snapshot, _bytes_written)| snapshot)
    }

    fn send_input_with_request_ack(
        &self,
        request: SendInputRequest,
        ack_context: &RequestAckContext,
        from: &str,
        require_idle: bool,
    ) -> Result<SessionSnapshot> {
        let session_name = request.name.clone();
        let decision =
            self.emit_dispatch_attempt(&ack_context.request_id, "send_input", from, &session_name)?;
        if require_idle && decision.overlap() {
            return Err(anyhow!(
                "dispatch aborted: target {}",
                decision.reason.unwrap_or("overlap")
            ));
        }
        let waiter = self.register_dispatch_reaction(ack_context, &session_name);
        let (snapshot, bytes_written) = match self.send_input_with_bytes(request) {
            Ok(result) => result,
            Err(error) => {
                self.cancel_dispatch_reaction(&waiter);
                return Err(error);
            }
        };
        self.wait_for_dispatch_reaction(waiter, ack_context, &snapshot.name, bytes_written)?;
        Ok(snapshot)
    }

    pub fn send_control_key(&self, name: &str, key: ControlKey) -> Result<SessionSnapshot> {
        let request_id = Uuid::new_v4().to_string();
        self.emit_dispatch_attempt(&request_id, "send_key", "operator", name)?;
        let (snapshot, _bytes_written) = self.send_input_with_bytes(SendInputRequest {
            name: name.into(),
            input: control_key_sequence(key).into(),
        })?;
        Ok(snapshot)
    }

    fn send_control_key_with_request_ack(
        &self,
        name: &str,
        key: ControlKey,
        ack_context: &RequestAckContext,
        from: &str,
        require_idle: bool,
    ) -> Result<SessionSnapshot> {
        let decision =
            self.emit_dispatch_attempt(&ack_context.request_id, "send_key", from, name)?;
        if require_idle && decision.overlap() {
            return Err(anyhow!(
                "dispatch aborted: target {}",
                decision.reason.unwrap_or("overlap")
            ));
        }
        let waiter = self.register_dispatch_reaction(ack_context, name);
        let (snapshot, bytes_written) = match self.send_input_with_bytes(SendInputRequest {
            name: name.into(),
            input: control_key_sequence(key).into(),
        }) {
            Ok(result) => result,
            Err(error) => {
                self.cancel_dispatch_reaction(&waiter);
                return Err(error);
            }
        };
        self.wait_for_dispatch_reaction(waiter, ack_context, &snapshot.name, bytes_written)?;
        Ok(snapshot)
    }

    pub fn route_message(&self, request: RouteMessageRequest) -> Result<RuntimeSnapshot> {
        self.route_message_inner(request, None, false)
    }

    fn route_message_with_request_ack(
        &self,
        request: RouteMessageRequest,
        ack_context: &RequestAckContext,
        require_idle: bool,
    ) -> Result<RuntimeSnapshot> {
        self.route_message_inner(request, Some(ack_context), require_idle)
    }

    fn route_message_inner(
        &self,
        request: RouteMessageRequest,
        ack_context: Option<&RequestAckContext>,
        require_idle: bool,
    ) -> Result<RuntimeSnapshot> {
        self.refresh_session_liveness();
        let route_id = Uuid::new_v4();
        let route_id_string = route_id.to_string();
        let request_id = ack_context
            .map(|context| context.request_id.clone())
            .unwrap_or_else(|| route_id_string.clone());
        let sender_filter = if request.scope == MessageScope::Room
            && !self.inner.slots.lock().contains_key(&request.from)
        {
            None
        } else {
            Some(request.from.as_str())
        };
        let recipients = self.resolve_recipients(&request.to, request.scope, sender_filter);
        let recipient_count = recipients.len() as u32;
        let mut attempts = Vec::new();
        for recipient in &recipients {
            let decision =
                self.emit_dispatch_attempt(&request_id, "route_message", &request.from, recipient)?;
            attempts.push((recipient.clone(), decision));
        }
        if require_idle
            && let Some((recipient, decision)) =
                attempts.iter().find(|(_, decision)| decision.overlap())
        {
            return Err(anyhow!(
                "dispatch aborted: target {} {}",
                recipient,
                decision.reason.unwrap_or("overlap")
            ));
        }

        self.emit_route_delivery(RouteDeliveryEvent {
            request_id: request_id.clone(),
            route_id: route_id_string.clone(),
            from: request.from.clone(),
            logical_to: request.to.clone(),
            scope: request.scope,
            recipient: None,
            recipient_index: 0,
            recipient_count,
            payload_part_count: 0,
            phase: RouteDeliveryPhase::Resolved,
            bytes_written: 0,
            error: None,
        });
        if recipients.is_empty() {
            return Err(anyhow!(
                "no running recipients available for '{}'",
                request.to
            ));
        }

        self.record_route_from_session(&request.from);
        self.emit(RuntimeEvent::RoutedMessage {
            id: route_id,
            from: request.from.clone(),
            to: request.to.clone(),
            scope: request.scope,
            content: request.content.clone(),
            timestamp: now_rfc3339(),
        });

        let mut failures = Vec::new();
        for (recipient_index, recipient) in recipients.into_iter().enumerate() {
            let submit_behavior = match self.submit_behavior_for_session(&recipient) {
                Ok(submit_behavior) => submit_behavior,
                Err(error) => {
                    let error = error.to_string();
                    self.emit_route_delivery(RouteDeliveryEvent {
                        request_id: request_id.clone(),
                        route_id: route_id_string.clone(),
                        from: request.from.clone(),
                        logical_to: request.to.clone(),
                        scope: request.scope,
                        recipient: Some(recipient.clone()),
                        recipient_index: recipient_index as u32,
                        recipient_count,
                        payload_part_count: 0,
                        phase: RouteDeliveryPhase::Failed,
                        bytes_written: 0,
                        error: Some(error.clone()),
                    });
                    failures.push((recipient, error));
                    continue;
                }
            };
            let payloads = routed_message_payloads(&request, submit_behavior);
            let payload_part_count = payloads.len() as u32;
            let waiter =
                ack_context.map(|context| self.register_dispatch_reaction(context, &recipient));
            match self.deliver_prepared_payloads(&recipient, &payloads, submit_behavior) {
                Ok(delivery) => {
                    self.emit_route_delivery(RouteDeliveryEvent {
                        request_id: request_id.clone(),
                        route_id: route_id_string.clone(),
                        from: request.from.clone(),
                        logical_to: request.to.clone(),
                        scope: request.scope,
                        recipient: Some(recipient.clone()),
                        recipient_index: recipient_index as u32,
                        recipient_count,
                        payload_part_count: delivery.payload_part_count,
                        phase: RouteDeliveryPhase::Written,
                        bytes_written: delivery.bytes_written,
                        error: None,
                    });
                    if let (Some(context), Some(waiter)) = (ack_context, waiter)
                        && let Err(error) = self.wait_for_dispatch_reaction(
                            waiter,
                            context,
                            &recipient,
                            delivery.bytes_written,
                        )
                    {
                        let error = error.to_string();
                        self.emit_route_delivery(RouteDeliveryEvent {
                            request_id: request_id.clone(),
                            route_id: route_id_string.clone(),
                            from: request.from.clone(),
                            logical_to: request.to.clone(),
                            scope: request.scope,
                            recipient: Some(recipient.clone()),
                            recipient_index: recipient_index as u32,
                            recipient_count,
                            payload_part_count: delivery.payload_part_count,
                            phase: RouteDeliveryPhase::Failed,
                            bytes_written: delivery.bytes_written,
                            error: Some(error.clone()),
                        });
                        failures.push((recipient, error));
                    }
                }
                Err(error) => {
                    if let Some(waiter) = waiter.as_ref() {
                        self.cancel_dispatch_reaction(waiter);
                    }
                    let error = error.to_string();
                    self.emit_route_delivery(RouteDeliveryEvent {
                        request_id: request_id.clone(),
                        route_id: route_id_string.clone(),
                        from: request.from.clone(),
                        logical_to: request.to.clone(),
                        scope: request.scope,
                        recipient: Some(recipient.clone()),
                        recipient_index: recipient_index as u32,
                        recipient_count,
                        payload_part_count,
                        phase: RouteDeliveryPhase::Failed,
                        bytes_written: 0,
                        error: Some(error.clone()),
                    });
                    failures.push((recipient, error));
                }
            }
        }

        if !failures.is_empty() {
            let failed_recipients = failures
                .iter()
                .map(|(recipient, error)| format!("{recipient}: {error}"))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(anyhow!(
                "route delivery failed for recipient(s): {failed_recipients}"
            ));
        }

        self.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: format!("Routed message from {} to {}", request.from, request.to),
            timestamp: now_rfc3339(),
        });

        Ok(self.snapshot())
    }

    pub fn deliver_message(&self, request: DeliverMessageRequest) -> Result<SessionSnapshot> {
        self.deliver_message_inner(request, None, "operator", false)
    }

    fn deliver_message_with_request_ack(
        &self,
        request: DeliverMessageRequest,
        ack_context: &RequestAckContext,
        from: &str,
        require_idle: bool,
    ) -> Result<SessionSnapshot> {
        self.deliver_message_inner(request, Some(ack_context), from, require_idle)
    }

    fn deliver_message_inner(
        &self,
        request: DeliverMessageRequest,
        ack_context: Option<&RequestAckContext>,
        from: &str,
        require_idle: bool,
    ) -> Result<SessionSnapshot> {
        let request_id = ack_context
            .map(|context| context.request_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let decision =
            self.emit_dispatch_attempt(&request_id, "deliver_message", from, &request.name)?;
        if require_idle && decision.overlap() {
            return Err(anyhow!(
                "dispatch aborted: target {}",
                decision.reason.unwrap_or("overlap")
            ));
        }
        self.emit_dispatch_template_warning_if_needed(&request_id, &request.name, &request.content);

        let (_, submit_behavior, payloads) =
            self.prepare_delivery_for_session(&request.name, &request.content)?;

        let waiter =
            ack_context.map(|context| self.register_dispatch_reaction(context, &request.name));
        let delivery =
            match self.deliver_prepared_payloads(&request.name, &payloads, submit_behavior) {
                Ok(delivery) => delivery,
                Err(error) => {
                    if let Some(waiter) = waiter.as_ref() {
                        self.cancel_dispatch_reaction(waiter);
                    }
                    return Err(error);
                }
            };
        if let (Some(context), Some(waiter)) = (ack_context, waiter) {
            self.wait_for_dispatch_reaction(
                waiter,
                context,
                &request.name,
                delivery.bytes_written,
            )?;
        }

        let snapshot = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get(&request.name)
                .with_context(|| format!("unknown session '{}'", request.name))?;
            slot.snapshot()
        };

        Ok(snapshot)
    }

    pub fn resize_session(&self, name: &str, cols: u16, rows: u16) -> Result<()> {
        self.refresh_session_liveness();
        let slots = self.inner.slots.lock();
        let slot = slots
            .get(name)
            .with_context(|| format!("unknown session '{name}'"))?;
        let running = slot
            .running
            .as_ref()
            .ok_or_else(|| anyhow!("session '{name}' is not running"))?;
        running
            .pty
            .as_ref()
            .ok_or_else(|| anyhow!("session '{name}' transport is not available"))?
            .resize(cols, rows)?;
        Ok(())
    }

    pub fn start_control_plane(&self) -> Result<ControlPlaneStatus> {
        if let Some(existing) = self.inner.control_plane.read().clone() {
            return Ok(existing);
        }

        let info_path = self.runtime_dir().join("control-plane.json");
        if let Some(existing_status) = load_existing_control_plane_status(&info_path)?
            && probe_control_plane_owner(&existing_status, CONTROL_PLANE_PROBE_TIMEOUT)?
        {
            return Err(anyhow!(
                "another wrapper instance is already holding the control plane at {}. Close the other instance before starting a new one.",
                existing_status.endpoint
            ));
        }

        let endpoint = control_plane_endpoint();
        let status = ControlPlaneStatus {
            transport: control_plane_transport().into(),
            endpoint: endpoint.clone(),
            token: Uuid::new_v4().to_string(),
            info_path: info_path.display().to_string(),
        };

        fs::write(&info_path, serde_json::to_string_pretty(&status)?)
            .context("failed to persist control plane info file")?;

        *self.inner.control_plane.write() = Some(status.clone());
        self.inner
            .token_bindings
            .lock()
            .insert(status.token.clone(), None);
        self.emit(RuntimeEvent::ControlPlaneReady {
            endpoint: status.endpoint.clone(),
            transport: status.transport.clone(),
            info_path: status.info_path.clone(),
            timestamp: now_rfc3339(),
        });

        spawn_control_plane_thread(self.clone(), status.clone());
        spawn_sideband_mailbox_thread(self.clone());

        Ok(status)
    }

    fn resolve_recipients(
        &self,
        to: &str,
        scope: MessageScope,
        sender: Option<&str>,
    ) -> Vec<String> {
        let slots = self.inner.slots.lock();
        match scope {
            MessageScope::Room => {
                let cross_pair = self.inner.cross_pair_room_broadcast;
                let sender_pair = sender.map(pair_of);
                slots
                    .iter()
                    .filter(|(name, slot)| {
                        if slot.running.is_none() {
                            return false;
                        }
                        if Some(name.as_str()) == sender {
                            return false;
                        }
                        if cross_pair {
                            return true;
                        }
                        // Pair-scoped room delivery. Unknown sender keeps the
                        // old broadcast behavior for supervisor-originated room messages.
                        match sender_pair {
                            Some(pair) => pair_of(name) == pair,
                            None => true,
                        }
                    })
                    .map(|(name, _)| name.clone())
                    .collect()
            }
            _ => slots
                .get(to)
                .and_then(|slot| {
                    slot.running
                        .as_ref()
                        .map(|_| vec![slot.definition.name.clone()])
                })
                .unwrap_or_default(),
        }
    }

    fn submit_behavior_for_session(&self, name: &str) -> Result<SubmitBehavior> {
        let slots = self.inner.slots.lock();
        let slot = slots
            .get(name)
            .with_context(|| format!("unknown session '{name}'"))?;
        Ok(routed_message_submit_behavior(slot.definition.driver))
    }

    fn refresh_session_liveness(&self) {
        let mut lifecycle_events = Vec::new();
        let mut log_events = Vec::new();
        let mut exit_events = Vec::new();
        let mut terminal_reactions = Vec::new();

        {
            let mut slots = self.inner.slots.lock();
            for (session_name, slot) in slots.iter_mut() {
                if slot.running.is_none() {
                    continue;
                }

                let mut prune_reason: Option<(LifecycleState, SessionExitReason, String)> = None;
                if let Some(process_id) = slot.process_id
                    && !process_id_is_running(process_id)
                {
                    prune_reason = Some((
                        LifecycleState::Closed,
                        SessionExitReason::ProcessDisappeared,
                        "process no longer running".into(),
                    ));
                }

                if prune_reason.is_none() {
                    let agent_liveness = slot
                        .running
                        .as_ref()
                        .and_then(|running| running.pty.as_ref())
                        .map(|pty| pty.agent_alive(slot.definition.driver));
                    match agent_liveness {
                        Some(Ok(AgentLiveness::Exited { .. })) => {
                            prune_reason = Some((
                                LifecycleState::Closed,
                                SessionExitReason::ProcessDisappeared,
                                "agent process no longer running".into(),
                            ));
                        }
                        Some(Ok(AgentLiveness::NotYetObserved(_)))
                            if slot.work_state_observed && slot.work_state == WorkState::Exited =>
                        {
                            prune_reason = Some((
                                LifecycleState::Failed,
                                SessionExitReason::CrashExit,
                                "agent terminal signature observed before an agent process was found"
                                    .into(),
                            ));
                        }
                        Some(Err(error)) => {
                            log_events.push(RuntimeEvent::SystemLog {
                                level: LogLevel::Warn,
                                message: format!(
                                    "{} agent liveness check failed: {error}",
                                    slot.title()
                                ),
                                timestamp: now_rfc3339(),
                            });
                        }
                        _ => {}
                    }
                }

                let Some((closed_state, fallback_reason, fallback_error)) = prune_reason else {
                    continue;
                };

                let timestamp = now_rfc3339();
                let process_id_for_event = slot.process_id;
                let (exit_status, poll_error) = poll_pty_exit_status(slot.running.as_ref());
                let classification = classify_session_exit(
                    exit_status,
                    poll_error,
                    slot.stop_intent.take(),
                    fallback_reason,
                    &fallback_error,
                );
                slot.running = None;
                slot.process_id = None;
                slot.state = closed_state;
                slot.last_activity_at = Some(timestamp.clone());
                slot.last_error = classification.last_error.clone();
                cancel_quiesce_timer_locked(slot);
                reset_work_state_locked(slot);

                log_events.push(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!(
                        "{} {} (wrapper pid {:?}); pruning stale session state",
                        slot.title(),
                        fallback_error,
                        process_id_for_event
                    ),
                    timestamp: timestamp.clone(),
                });
                exit_events.push(session_exit_event(
                    session_name.clone(),
                    slot.generation,
                    process_id_for_event,
                    &classification,
                    timestamp.clone(),
                ));
                lifecycle_events.push(RuntimeEvent::SessionState {
                    session: session_name.clone(),
                    state: closed_state,
                    reason: fallback_error,
                    timestamp,
                });
                terminal_reactions.push((session_name.clone(), Instant::now()));
            }
        }

        for (session, observed_at) in terminal_reactions {
            self.resolve_dispatch_reactions_for_session(
                &session,
                observed_at,
                DispatchReactionOutcome::Terminal,
            );
        }
        for event in log_events {
            self.emit(event);
        }
        for event in exit_events {
            self.emit(event);
        }
        for event in lifecycle_events {
            self.emit(event);
        }
    }

    fn handle_pty_event(
        &self,
        session_name: &str,
        event_generation: SessionGeneration,
        event: PtyEvent,
    ) {
        let should_log_stale_event = {
            let slots = self.inner.slots.lock();
            let current = slots.get(session_name).map(|slot| slot.generation);
            current != Some(event_generation)
        };

        if should_log_stale_event {
            let counter = {
                let mut counts = self.inner.stale_event_drop_counts.lock();
                let key = (session_name.to_string(), event_generation);
                let count = counts.entry(key).or_insert(0);
                *count += 1;
                *count
            };

            if counter == 1 || counter % 10 == 0 {
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: format!(
                        "Dropped stale PTY event for session '{session_name}' generation {event_generation} (count={counter})"
                    ),
                    timestamp: now_rfc3339(),
                });
            }
            return;
        }

        match event {
            PtyEvent::Output(chunk) => {
                let has_real_content = chunk_has_real_content(&chunk);
                let observed_at = Instant::now();
                let real_output_at = has_real_content.then_some(observed_at);
                let (
                    transitioned_to_ready,
                    quiesce_arm,
                    work_state_event,
                    reaction_outcome,
                    needs_liveness_refresh,
                ) = {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        let transitioned = slot.state != LifecycleState::Ready;
                        if slot.state != LifecycleState::Ready {
                            slot.state = LifecycleState::Ready;
                            slot.last_activity_at = Some(now_rfc3339());
                        }
                        if has_real_content
                            && let Some(pty) = slot
                                .running
                                .as_ref()
                                .and_then(|running| running.pty.as_ref().map(|pty| pty.as_ref()))
                        {
                            pty.note_real_output(slot.definition.driver);
                        }

                        let classification =
                            classify_work_state_for_driver(slot.definition.driver, &chunk);
                        let mut terminal_signature = false;
                        let mut first_launch_banner = false;
                        let work_state_event = classification.and_then(|(state, detail)| {
                            if state == WorkState::Exited {
                                terminal_signature = true;
                                if detail.as_deref() == Some("launch_banner")
                                    && !slot.launch_banner_seen
                                {
                                    slot.launch_banner_seen = true;
                                    first_launch_banner = true;
                                    return None;
                                }
                            }
                            transition_work_state_locked(session_name, slot, state, detail)
                        });

                        let terminal_reaction = terminal_signature && !first_launch_banner;
                        let quiesce_arm = if terminal_reaction {
                            cancel_quiesce_timer_locked(slot);
                            None
                        } else {
                            real_output_at.map(|armed_at| {
                                slot.last_real_output_at = Some(armed_at);
                                cancel_quiesce_timer_locked(slot);
                                (slot.definition.driver, slot.generation, armed_at)
                            })
                        };
                        let reaction_outcome = if terminal_reaction {
                            Some(DispatchReactionOutcome::Terminal)
                        } else if has_real_content || work_state_event.is_some() {
                            Some(DispatchReactionOutcome::Reacted)
                        } else {
                            None
                        };
                        (
                            transitioned,
                            quiesce_arm,
                            work_state_event,
                            reaction_outcome,
                            terminal_reaction,
                        )
                    } else {
                        (false, None, None, None, false)
                    }
                };

                if let Some((driver, generation, armed_at)) = quiesce_arm {
                    self.arm_quiesce_timer(session_name, driver, generation, armed_at);
                }

                if transitioned_to_ready {
                    self.emit(RuntimeEvent::SessionState {
                        session: session_name.into(),
                        state: LifecycleState::Ready,
                        reason: "session emitted output".into(),
                        timestamp: now_rfc3339(),
                    });
                }

                if let Some(event) = work_state_event {
                    self.emit(event);
                }

                self.emit(RuntimeEvent::SessionOutput {
                    session: session_name.into(),
                    chunk,
                    synthetic: false,
                    timestamp: now_rfc3339(),
                });

                if let Some(outcome) = reaction_outcome {
                    self.resolve_dispatch_reactions_for_session(session_name, observed_at, outcome);
                }

                if needs_liveness_refresh {
                    self.refresh_session_liveness();
                }
            }
            PtyEvent::Closed => {
                let timestamp = now_rfc3339();
                let observed_at = Instant::now();
                let mut exit_event = None;
                {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        let process_id = slot.process_id;
                        let (exit_status, poll_error) = poll_pty_exit_status(slot.running.as_ref());
                        let classification = classify_session_exit(
                            exit_status,
                            poll_error,
                            slot.stop_intent.take(),
                            SessionExitReason::ProcessDisappeared,
                            "session output closed before exit status was available",
                        );
                        slot.running = None;
                        slot.process_id = None;
                        slot.state = LifecycleState::Closed;
                        slot.last_activity_at = Some(timestamp.clone());
                        slot.last_error = classification.last_error.clone();
                        slot.last_real_output_at = None;
                        cancel_quiesce_timer_locked(slot);
                        reset_work_state_locked(slot);
                        exit_event = Some(session_exit_event(
                            session_name.into(),
                            event_generation,
                            process_id,
                            &classification,
                            timestamp.clone(),
                        ));
                    }
                }
                self.resolve_dispatch_reactions_for_session(
                    session_name,
                    observed_at,
                    DispatchReactionOutcome::Terminal,
                );
                if let Some(event) = exit_event {
                    self.emit(event);
                }
                self.emit(RuntimeEvent::SessionState {
                    session: session_name.into(),
                    state: LifecycleState::Closed,
                    reason: "session output closed".into(),
                    timestamp,
                });
            }
            PtyEvent::Error(error) => {
                let timestamp = now_rfc3339();
                let observed_at = Instant::now();
                let mut exit_event = None;
                {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        let process_id = slot.process_id;
                        let classification =
                            classify_pty_error_exit(&error, slot.stop_intent.take());
                        slot.state = LifecycleState::Failed;
                        slot.last_error = classification.last_error.clone();
                        slot.running = None;
                        slot.process_id = None;
                        slot.last_activity_at = Some(timestamp.clone());
                        slot.last_real_output_at = None;
                        cancel_quiesce_timer_locked(slot);
                        reset_work_state_locked(slot);
                        exit_event = Some(session_exit_event(
                            session_name.into(),
                            event_generation,
                            process_id,
                            &classification,
                            timestamp.clone(),
                        ));
                    }
                }
                self.resolve_dispatch_reactions_for_session(
                    session_name,
                    observed_at,
                    DispatchReactionOutcome::Terminal,
                );
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Error,
                    message: format!("{session_name} PTY error: {error}"),
                    timestamp: timestamp.clone(),
                });
                if let Some(event) = exit_event {
                    self.emit(event);
                }
                self.emit(RuntimeEvent::SessionState {
                    session: session_name.into(),
                    state: LifecycleState::Failed,
                    reason: "PTY error".into(),
                    timestamp,
                });
            }
        }
    }

    fn apply_sideband_request(&self, request: SidebandRequest) -> SidebandResponse {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("failed to create wait_quiet runtime");
        runtime.block_on(self.apply_sideband_request_async(request))
    }

    fn validate_sideband_request(&self, token: &str) -> Option<SidebandResponse> {
        let Some(_) = self.inner.control_plane.read().clone() else {
            return Some(SidebandResponse {
                ok: false,
                message: "control plane not ready".into(),
                snapshot: None,
                timed_out: false,
                payload: None,
                request_id: None,
            });
        };

        if !self.inner.token_bindings.lock().contains_key(token) {
            return Some(SidebandResponse {
                ok: false,
                message: "invalid control plane token".into(),
                snapshot: None,
                timed_out: false,
                payload: None,
                request_id: None,
            });
        }

        None
    }

    fn sideband_response_from_outcome(&self, outcome: Result<String>) -> SidebandResponse {
        match outcome {
            Ok(message) => SidebandResponse {
                ok: true,
                message,
                snapshot: Some(self.snapshot()),
                timed_out: false,
                payload: None,
                request_id: None,
            },
            Err(error) => SidebandResponse {
                ok: false,
                message: error.to_string(),
                snapshot: Some(self.snapshot()),
                timed_out: false,
                payload: None,
                request_id: None,
            },
        }
    }

    fn apply_lifecycle_request_at(
        &self,
        request: SidebandRequest,
        expected_generation: Option<SessionGeneration>,
    ) -> SidebandResponse {
        if let Some(response) = self.validate_sideband_request(request.token()) {
            return response;
        }

        let outcome = match (request, expected_generation) {
            (
                SidebandRequest::StartSession {
                    name, extra_args, ..
                },
                Some(expected),
            ) => self
                .start_session_at(&name, expected, extra_args)
                .map(|_| format!("started {name}")),
            (SidebandRequest::StopSession { name, .. }, Some(expected)) => self
                .stop_session_at(&name, expected)
                .map(|_| format!("stopped {name}")),
            (SidebandRequest::RestartSession { name, .. }, Some(expected)) => self
                .restart_session_at(&name, expected)
                .map(|_| format!("restarted {name}")),
            (other, _) => return self.apply_sideband_request(other),
        };

        self.sideband_response_from_outcome(outcome)
    }

    async fn apply_side_effect_request_async(
        &self,
        request: SidebandRequest,
        request_id: &str,
        ack_context: Option<RequestAckContext>,
    ) -> SidebandResponse {
        if let Some(response) = self.validate_sideband_request(request.token()) {
            return response;
        }

        let outcome = match request {
            SidebandRequest::Ping { .. } => Ok("pong".into()),
            SidebandRequest::ListSessions { .. } => Ok("sessions listed".into()),
            SidebandRequest::WaitQuiet {
                name,
                quiet_seconds,
                timeout_seconds,
                ..
            } => {
                return self
                    .wait_quiet(WaitQuietRequest {
                        name,
                        quiet_seconds,
                        timeout_seconds,
                    })
                    .await;
            }
            SidebandRequest::EventsSince {
                cursor,
                max_events,
                max_wait_seconds,
                filter,
                ..
            } => {
                let filter = Self::normalize_events_filter(filter);
                let echoed_cursor = cursor
                    .as_ref()
                    .map(|value| serde_json::to_value(value).unwrap_or(serde_json::Value::Null))
                    .unwrap_or(serde_json::Value::Null);
                if let Err(error) = self.validate_events_filter(&filter) {
                    return self.events_request_error(error.to_string(), echoed_cursor);
                }

                let max_events = max_events.unwrap_or(500).clamp(1, 5000) as usize;
                let max_wait_seconds = max_wait_seconds.unwrap_or(0);
                if max_wait_seconds > 60 {
                    return self.events_request_error(
                        "max_wait_seconds > 60 not supported in V1",
                        echoed_cursor,
                    );
                }

                return match self
                    .wait_for_events(
                        cursor,
                        filter,
                        max_events,
                        Duration::from_secs(max_wait_seconds as u64),
                    )
                    .await
                {
                    Ok(result) => SidebandResponse {
                        ok: true,
                        message: format!("returned {} events", result.events.len()),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: Some(SidebandResponsePayload::EventsSince {
                            events: result.events,
                            next_cursor: result.next_cursor,
                            gap_detected: result.gap_detected,
                            as_of: result.as_of,
                        }),
                        request_id: None,
                    },
                    Err(error) => {
                        let message = if error.to_string().starts_with("audit read error:") {
                            error.to_string()
                        } else {
                            format!("audit read error: {error}")
                        };
                        self.events_request_error(message, echoed_cursor)
                    }
                };
            }
            SidebandRequest::DeliverMessage {
                token,
                name,
                content,
                require_idle,
            } => {
                tokio::task::yield_now().await;
                self.validate_deliver_message_token(&token, &name)
                    .and_then(|_| {
                        let from = self.dispatch_actor_for_token(&token)?;
                        let request = DeliverMessageRequest { name, content };
                        match ack_context.as_ref() {
                            Some(context) => self.deliver_message_with_request_ack(
                                request,
                                context,
                                &from,
                                require_idle,
                            ),
                            None => self.deliver_message_inner(request, None, &from, require_idle),
                        }
                    })
                    .map(|_| "delivered".into())
            }
            SidebandRequest::SendInput {
                token,
                name,
                input,
                require_idle,
            } => {
                tokio::task::yield_now().await;
                self.validate_session_action_token(&token, &name)
                    .and_then(|_| {
                        let from = self.dispatch_actor_for_token(&token)?;
                        let request = SendInputRequest { name, input };
                        match ack_context.as_ref() {
                            Some(context) => self.send_input_with_request_ack(
                                request,
                                context,
                                &from,
                                require_idle,
                            ),
                            None => self.send_input(request),
                        }
                    })
                    .map(|_| "input sent".into())
            }
            SidebandRequest::SendKey {
                token,
                name,
                key,
                require_idle,
            } => {
                tokio::task::yield_now().await;
                self.validate_session_action_token(&token, &name)
                    .and_then(|_| match ack_context.as_ref() {
                        Some(context) => {
                            let from = self.dispatch_actor_for_token(&token)?;
                            self.send_control_key_with_request_ack(
                                &name,
                                key,
                                context,
                                &from,
                                require_idle,
                            )
                        }
                        None => self.send_control_key(&name, key),
                    })
                    .map(|_| format!("key {:?} sent", key))
            }
            SidebandRequest::RouteMessage {
                token,
                mut request,
                require_idle,
            } => {
                tokio::task::yield_now().await;
                self.resolve_route_sender_identity(&token)
                    .map(|bound| {
                        if let Some(name) = bound {
                            request.from = name;
                        }
                        request
                    })
                    .and_then(|req| match ack_context.as_ref() {
                        Some(context) => {
                            self.route_message_with_request_ack(req, context, require_idle)
                        }
                        None => self.route_message_inner(req, None, require_idle),
                    })
                    .map(|_| "message routed".into())
            }
            SidebandRequest::PaneSignal {
                token,
                task_id,
                signal_type,
                summary,
                artifact_paths,
                commit_sha,
            } => {
                let result = self.record_pane_signal(
                    request_id,
                    PaneSignalRecord {
                        token,
                        task_id,
                        signal_type,
                        summary,
                        artifact_paths,
                        commit_sha,
                    },
                );
                return match result {
                    Ok(paths) => SidebandResponse {
                        ok: true,
                        message: format!("pane signal recorded at {}", paths.signal_path.display()),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: Some(SidebandResponsePayload::PaneSignal {
                            signal_path: paths.signal_path.display().to_string(),
                            legacy_touch_path: paths.legacy_touch_path.display().to_string(),
                        }),
                        request_id: None,
                    },
                    Err(error) => SidebandResponse {
                        ok: false,
                        message: error.to_string(),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: None,
                        request_id: None,
                    },
                };
            }
            SidebandRequest::CreatePair { token, name } => {
                tokio::task::yield_now().await;
                self.validate_master_token(&token)
                    .and_then(|_| self.create_pair(&name))
                    .map(|snapshots| format!("created pair '{name}' ({} slots)", snapshots.len()))
            }
            lifecycle => {
                return SidebandResponse {
                    ok: false,
                    message: format!(
                        "side-effect dispatcher received unexpected lifecycle request '{}'",
                        action_label_for(&lifecycle)
                    ),
                    snapshot: Some(self.snapshot()),
                    timed_out: false,
                    payload: None,
                    request_id: None,
                };
            }
        };

        self.sideband_response_from_outcome(outcome)
    }

    async fn apply_sideband_request_async(&self, request: SidebandRequest) -> SidebandResponse {
        let request_id = Uuid::new_v4().to_string();
        let action = action_label_for(&request).to_string();
        let session = session_name_of(&request).map(str::to_string);
        let extra_args = start_session_extra_args_of(&request).to_vec();
        let budget = SidebandTimeouts::budget(&request);
        let started = Instant::now();

        self.emit_sideband_lifecycle(
            &request_id,
            &action,
            session.as_deref(),
            &extra_args,
            SidebandPhase::Started,
            Duration::ZERO,
        );

        let mut response = match SidebandTimeouts::lane(&request) {
            OpLane::Lifecycle => {
                self.run_detached_with_timeout_async(
                    request.clone(),
                    budget,
                    &request_id,
                    &action,
                    session.as_deref(),
                    &extra_args,
                )
                .await
            }
            OpLane::SideEffect => {
                self.run_inline_with_timeout_async(
                    request.clone(),
                    budget,
                    &request_id,
                    &action,
                    session.as_deref(),
                    &extra_args,
                )
                .await
            }
        };

        response.request_id = Some(request_id.clone());

        if !response.timed_out {
            let phase = if response.ok {
                SidebandPhase::Completed
            } else {
                SidebandPhase::Failed
            };
            let error = (!response.ok).then(|| response.message.clone());
            self.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
                request_id: &request_id,
                action: &action,
                session: session.as_deref(),
                extra_args: &extra_args,
                phase,
                elapsed: started.elapsed(),
                error,
            });
        }

        response
    }

    async fn run_detached_with_timeout_async(
        &self,
        request: SidebandRequest,
        budget: Duration,
        request_id: &str,
        action: &str,
        session: Option<&str>,
        extra_args: &[String],
    ) -> SidebandResponse {
        if let Err(error) = validate_extra_args(start_session_extra_args_of(&request)) {
            return self.sideband_response_from_outcome(Err(error));
        }

        let expected_generation = match session {
            Some(name) => match self.bump_session_generation(name) {
                Ok(expected) => Some(expected),
                Err(error) => return self.sideband_response_from_outcome(Err(error)),
            },
            None => None,
        };

        let handle = self.clone();
        let lifecycle_request = request.clone();
        let mut worker = tokio::task::spawn_blocking(move || {
            handle.apply_lifecycle_request_at(lifecycle_request, expected_generation)
        });
        let slow_warn_at = budget / 2;
        let started = Instant::now();
        let mut warned = false;
        let slow_warning = tokio::time::sleep(slow_warn_at);
        tokio::pin!(slow_warning);
        let deadline = tokio::time::sleep(budget);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                join_result = &mut worker => {
                    let elapsed = started.elapsed();
                    if !warned && elapsed >= slow_warn_at {
                        self.emit_sideband_lifecycle(
                            request_id,
                            action,
                            session,
                            extra_args,
                            SidebandPhase::SlowWarning,
                            elapsed,
                        );
                    }

                    return match join_result {
                        Ok(response) => response,
                        Err(join_error) => SidebandResponse {
                            ok: false,
                            message: format!("worker join failed: {join_error}"),
                            snapshot: Some(self.snapshot()),
                            timed_out: false,
                            payload: None,
                            request_id: None,
                        },
                    };
                }
                _ = &mut slow_warning, if !warned => {
                    warned = true;
                    self.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        extra_args,
                        SidebandPhase::SlowWarning,
                        started.elapsed(),
                    );
                }
                _ = &mut deadline => {
                    let elapsed = started.elapsed();
                    let message = format!(
                        "lifecycle op '{action}' timed out after {}ms",
                        elapsed.as_millis()
                    );
                    self.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
                        request_id,
                        action,
                        session,
                        extra_args,
                        phase: SidebandPhase::TimedOut,
                        elapsed,
                        error: Some(message.clone()),
                    });
                    return SidebandResponse {
                        ok: false,
                        message,
                        snapshot: Some(self.snapshot()),
                        timed_out: true,
                        payload: None,
                        request_id: None,
                    };
                }
            }
        }
    }

    async fn run_inline_with_timeout_async(
        &self,
        request: SidebandRequest,
        budget: Duration,
        request_id: &str,
        action: &str,
        session: Option<&str>,
        extra_args: &[String],
    ) -> SidebandResponse {
        let slow_warn_at = budget / 2;
        let started = Instant::now();
        let mut warned = false;
        let ack_context = request_ack_context_for(request_id, action, &request);
        let operation = self.apply_side_effect_request_async(request, request_id, ack_context);
        tokio::pin!(operation);
        let slow_warning = tokio::time::sleep(slow_warn_at);
        tokio::pin!(slow_warning);
        let deadline = tokio::time::sleep(budget);
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                response = &mut operation => {
                    let elapsed = started.elapsed();
                    if !warned && elapsed >= slow_warn_at {
                        self.emit_sideband_lifecycle(
                            request_id,
                            action,
                            session,
                            extra_args,
                            SidebandPhase::SlowWarning,
                            elapsed,
                        );
                    }
                    return response;
                }
                _ = &mut slow_warning, if !warned => {
                    warned = true;
                    self.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        extra_args,
                        SidebandPhase::SlowWarning,
                        started.elapsed(),
                    );
                }
                _ = &mut deadline => {
                    let message = format!(
                        "side-effect op '{action}' timed out after {}ms (boundary pre-PTY-write unless documented otherwise)",
                        budget.as_millis()
                    );
                    self.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
                        request_id,
                        action,
                        session,
                        extra_args,
                        phase: SidebandPhase::TimedOut,
                        elapsed: budget,
                        error: Some(message.clone()),
                    });
                    return SidebandResponse {
                        ok: false,
                        message,
                        snapshot: Some(self.snapshot()),
                        timed_out: true,
                        payload: None,
                        request_id: None,
                    };
                }
            }
        }
    }

    fn emit(&self, event: RuntimeEvent) {
        if let RuntimeEvent::SessionWorkState { session, state, .. } = &event {
            self.handle_session_work_state_side_effects(session, *state);
        }

        let appended = match self.inner.audit.append(&event) {
            Ok(()) => true,
            Err(error) => {
                eprintln!("audit log failure: {error}");
                false
            }
        };

        if appended {
            let next = self.inner.events_seq.fetch_add(1, Ordering::SeqCst) + 1;
            self.inner.events_watch.send_replace(next);
        }

        if let Some(sink) = self.inner.event_sink.read().as_ref() {
            sink(event);
        }
    }
}

impl SupervisorHandle {
    fn prepare_definition_for_spawn(
        &self,
        definition: &SessionDefinition,
    ) -> Result<SessionDefinition> {
        let mut prepared = definition.clone();

        if let Some(status) = self.ensure_session_control_plane_status(&definition.name)? {
            prepared.env.push(EnvVar {
                key: "PRIM1_PANE_IDENTITY".into(),
                value: definition.name.clone(),
            });
            prepared.env.push(EnvVar {
                key: "PRIM1_PANE_CREDENTIALS".into(),
                value: status.info_path,
            });
        }

        Ok(prepared)
    }

    fn ensure_session_control_plane_status(
        &self,
        session_name: &str,
    ) -> Result<Option<ControlPlaneStatus>> {
        let Some(control_plane) = self.inner.control_plane.read().clone() else {
            return Ok(None);
        };

        let info_path = self
            .runtime_dir()
            .join(format!("control-plane-{session_name}.json"));
        let info_path_text = info_path.display().to_string();

        let status = {
            let mut statuses = self.inner.session_control_planes.lock();
            statuses
                .entry(session_name.to_string())
                .or_insert_with(|| {
                    let status = ControlPlaneStatus {
                        transport: control_plane.transport.clone(),
                        endpoint: control_plane.endpoint.clone(),
                        token: Uuid::new_v4().to_string(),
                        info_path: info_path_text.clone(),
                    };
                    self.inner
                        .token_bindings
                        .lock()
                        .insert(status.token.clone(), Some(session_name.to_string()));
                    status
                })
                .clone()
        };

        fs::write(&info_path, serde_json::to_string_pretty(&status)?).with_context(|| {
            format!(
                "failed to persist session control plane info file {}",
                info_path.display()
            )
        })?;

        Ok(Some(status))
    }

    fn validate_session_action_token(&self, token: &str, target_session: &str) -> Result<()> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        if self.inner.peer_slash_commands_allowed {
            return Ok(());
        }

        if let Some(bound_session) = binding
            && bound_session != target_session
        {
            return Err(anyhow!(
                "peer slash commands are disabled by this wrapper's policy (PRIM1_PEER_SLASH_COMMANDS_ALLOWED=0)"
            ));
        }

        Ok(())
    }

    fn validate_master_token(&self, token: &str) -> Result<()> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        if binding.is_some() {
            return Err(anyhow!(
                "create_pair: pane-bound tokens are not authorised; master token required"
            ));
        }

        Ok(())
    }

    fn resolve_route_sender_identity(&self, token: &str) -> Result<Option<String>> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        Ok(binding)
    }

    fn dispatch_actor_for_token(&self, token: &str) -> Result<String> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        Ok(binding.unwrap_or_else(|| "operator".into()))
    }

    fn resolve_pane_signal_session(&self, token: &str) -> Result<String> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        Ok(binding.unwrap_or_else(|| "supervisor".into()))
    }

    fn validate_deliver_message_token(&self, token: &str, target_session: &str) -> Result<()> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        if let Some(bound_session) = binding
            && bound_session != target_session
        {
            return Err(anyhow!(
                "deliver_message: pane-bound token cannot target other sessions; use route_message instead"
            ));
        }

        Ok(())
    }

    fn prepare_delivery_for_session(
        &self,
        session_name: &str,
        content: &str,
    ) -> Result<(SessionSnapshot, SubmitBehavior, Vec<String>)> {
        if content.trim_start().starts_with('/') {
            return Err(anyhow!(
                "deliver_message: slash commands are not supported; use send_input"
            ));
        }

        self.refresh_session_liveness();
        let slots = self.inner.slots.lock();
        let slot = slots
            .get(session_name)
            .with_context(|| format!("unknown session '{}'", session_name))?;
        if slot.running.is_none() {
            return Err(anyhow!("session '{}' is not running", session_name));
        }

        let submit_behavior = routed_message_submit_behavior(slot.definition.driver);
        let prepared = prepare_direct_message(slot.definition.driver, content);
        let single_payload = prepared.into_iter().next().unwrap_or_default();
        let chunks = split_routed_message_content(&single_payload, submit_behavior.max_chunk_chars);
        let payloads = if chunks.is_empty() {
            vec![String::new()]
        } else {
            chunks
        };
        Ok((slot.snapshot(), submit_behavior, payloads))
    }

    async fn wait_quiet(&self, request: WaitQuietRequest) -> SidebandResponse {
        self.refresh_session_liveness();
        let quiet_window = Duration::from_secs(request.quiet_seconds as u64);
        let timeout = Duration::from_secs(request.timeout_seconds as u64);
        let wait_started_at = Instant::now();

        loop {
            self.refresh_session_liveness();
            let status = {
                let slots = self.inner.slots.lock();
                let Some(slot) = slots.get(&request.name) else {
                    return SidebandResponse {
                        ok: false,
                        message: format!("unknown session '{}'", request.name),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: None,
                        request_id: None,
                    };
                };
                if slot.running.is_none() {
                    return SidebandResponse {
                        ok: false,
                        message: format!("session '{}' is not running", request.name),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: None,
                        request_id: None,
                    };
                }

                let last_real_output_at = slot.last_real_output_at.unwrap_or(wait_started_at);
                Instant::now()
                    .saturating_duration_since(last_real_output_at)
                    .as_millis() as u64
            };

            if status >= quiet_window.as_millis() as u64 {
                return SidebandResponse {
                    ok: true,
                    message: "session is quiet".into(),
                    snapshot: Some(self.snapshot()),
                    timed_out: false,
                    payload: Some(SidebandResponsePayload::WaitQuiet {
                        quiet_duration_ms: status,
                    }),
                    request_id: None,
                };
            }

            if Instant::now().saturating_duration_since(wait_started_at) >= timeout {
                return SidebandResponse {
                    ok: false,
                    message: "wait_quiet timed out".into(),
                    snapshot: Some(self.snapshot()),
                    timed_out: false,
                    payload: Some(SidebandResponsePayload::WaitQuietTimeout {
                        last_output_age_ms: status,
                    }),
                    request_id: None,
                };
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    fn deliver_prepared_payloads(
        &self,
        session_name: &str,
        payloads: &[String],
        submit_behavior: SubmitBehavior,
    ) -> Result<DeliveryWriteResult> {
        let mut total_bytes_written = 0;
        for payload in payloads {
            let (_snapshot, bytes_written) = self.send_input_with_bytes(SendInputRequest {
                name: session_name.into(),
                input: payload.clone(),
            })?;
            total_bytes_written += bytes_written;
            if !submit_behavior.delay.is_zero() {
                thread::sleep(submit_behavior.delay);
            }
            let (_snapshot, bytes_written) = self.send_input_with_bytes(SendInputRequest {
                name: session_name.into(),
                input: submit_behavior.sequence.into(),
            })?;
            total_bytes_written += bytes_written;
        }

        Ok(DeliveryWriteResult {
            bytes_written: total_bytes_written,
            payload_part_count: payloads.len() as u32,
        })
    }
}

#[cfg(windows)]
fn process_id_is_running(process_id: u32) -> bool {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, WAIT_TIMEOUT},
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, WaitForSingleObject},
    };
    const SYNCHRONIZE_ACCESS: u32 = 0x0010_0000;

    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE_ACCESS,
            0,
            process_id,
        );
        if handle.is_null() {
            return false;
        }

        let wait_result = WaitForSingleObject(handle, 0);
        let _ = CloseHandle(handle);
        wait_result == WAIT_TIMEOUT
    }
}

#[cfg(unix)]
fn process_id_is_running(process_id: u32) -> bool {
    unsafe { libc::kill(process_id as i32, 0) == 0 }
}

fn spawn_control_plane_thread(handle: SupervisorHandle, status: ControlPlaneStatus) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("failed to create control plane runtime: {error}");
                return;
            }
        };

        #[cfg(windows)]
        let result = runtime.block_on(run_windows_pipe_server(handle, status));

        #[cfg(unix)]
        let result = runtime.block_on(run_unix_socket_server(handle, status));

        if let Err(error) = result {
            eprintln!("control plane failed: {error}");
        }
    });
}

fn spawn_sideband_mailbox_thread(handle: SupervisorHandle) {
    let runtime_dir = handle.runtime_dir().to_path_buf();
    thread::spawn(move || {
        let sideband_dir = runtime_dir.join("sideband");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");

        if let Err(error) = fs::create_dir_all(&inbox_dir) {
            eprintln!("failed to create sideband inbox: {error}");
            return;
        }
        if let Err(error) = fs::create_dir_all(&outbox_dir) {
            eprintln!("failed to create sideband outbox: {error}");
            return;
        }
        if let Err(error) = fs::create_dir_all(&processed_dir) {
            eprintln!("failed to create sideband archive: {error}");
            return;
        }

        loop {
            let mut requests = match fs::read_dir(&inbox_dir) {
                Ok(entries) => entries
                    .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                    .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
                    .collect::<Vec<_>>(),
                Err(error) => {
                    eprintln!("failed to read sideband inbox: {error}");
                    thread::sleep(Duration::from_millis(200));
                    continue;
                }
            };

            requests.sort();

            for request_path in requests {
                if let Err(error) = process_sideband_mailbox_file(
                    &handle,
                    &request_path,
                    &outbox_dir,
                    &processed_dir,
                ) {
                    eprintln!(
                        "failed to process sideband mailbox request {}: {error}",
                        request_path.display()
                    );
                }
            }

            thread::sleep(Duration::from_millis(100));
        }
    });
}

fn process_sideband_mailbox_file(
    handle: &SupervisorHandle,
    request_path: &Path,
    outbox_dir: &Path,
    processed_dir: &Path,
) -> Result<()> {
    let mailbox_fs = handle.inner.mailbox_fs.read().clone();
    let raw = mailbox_fs
        .read_to_string(request_path)
        .with_context(|| format!("failed to read mailbox request {}", request_path.display()))?;
    let request = match decode_request(raw.trim()) {
        Ok(request) => request,
        Err(error) => {
            let request_is_fresh = fs::metadata(request_path)
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|modified| modified.elapsed().ok())
                .map(|elapsed| elapsed < Duration::from_millis(500))
                .unwrap_or(false);

            if request_is_fresh {
                return Ok(());
            }

            let response = SidebandResponse {
                ok: false,
                message: format!("invalid sideband payload: {error}"),
                snapshot: Some(handle.snapshot()),
                timed_out: false,
                payload: None,
                request_id: None,
            };
            return write_and_archive_response(
                handle,
                request_path,
                outbox_dir,
                processed_dir,
                &response,
            );
        }
    };

    let request_id = Uuid::new_v4().to_string();
    let action = action_label_for(&request);
    let session = session_name_of(&request);
    let extra_args = start_session_extra_args_of(&request).to_vec();
    let budget = SidebandTimeouts::budget(&request);
    let started = Instant::now();

    handle.emit_sideband_lifecycle(
        &request_id,
        action,
        session,
        &extra_args,
        SidebandPhase::Started,
        Duration::ZERO,
    );

    let mut response = match SidebandTimeouts::lane(&request) {
        OpLane::Lifecycle => run_detached_with_timeout(
            handle,
            request.clone(),
            budget,
            &request_id,
            action,
            session,
            &extra_args,
        ),
        OpLane::SideEffect => run_inline_with_timeout(
            handle,
            request.clone(),
            budget,
            &request_id,
            action,
            session,
            &extra_args,
        ),
    };

    response.request_id = Some(request_id.clone());

    if !response.timed_out {
        let phase = if response.ok {
            SidebandPhase::Completed
        } else {
            SidebandPhase::Failed
        };
        let error = (!response.ok).then(|| response.message.clone());
        handle.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
            request_id: &request_id,
            action,
            session,
            extra_args: &extra_args,
            phase,
            elapsed: started.elapsed(),
            error,
        });
    }

    write_and_archive_response(handle, request_path, outbox_dir, processed_dir, &response)
}

fn run_detached_with_timeout(
    handle: &SupervisorHandle,
    request: SidebandRequest,
    budget: Duration,
    request_id: &str,
    action: &str,
    session: Option<&str>,
    extra_args: &[String],
) -> SidebandResponse {
    if let Err(error) = validate_extra_args(start_session_extra_args_of(&request)) {
        return handle.sideband_response_from_outcome(Err(error));
    }

    let expected_generation = match session {
        Some(name) => match handle.bump_session_generation(name) {
            Ok(expected) => Some(expected),
            Err(error) => return handle.sideband_response_from_outcome(Err(error)),
        },
        None => None,
    };

    let (tx, rx) = std::sync::mpsc::channel();
    #[cfg(test)]
    let (done_tx, done_rx) = std::sync::mpsc::channel();

    #[cfg(test)]
    handle.set_last_detached_worker_receiver(done_rx);

    let worker_handle = handle.clone();
    thread::spawn(move || {
        let response = worker_handle.apply_lifecycle_request_at(request, expected_generation);
        let _ = tx.send(response);
        #[cfg(test)]
        let _ = done_tx.send(());
    });

    let slow_warn_at = budget / 2;
    let started = Instant::now();
    let mut warned = false;

    loop {
        let elapsed = started.elapsed();
        let remaining = budget.saturating_sub(elapsed);
        let tick = remaining.min(Duration::from_millis(500));
        match rx.recv_timeout(tick) {
            Ok(response) => return response,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let elapsed = started.elapsed();
                if !warned && elapsed >= slow_warn_at {
                    warned = true;
                    handle.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        extra_args,
                        SidebandPhase::SlowWarning,
                        elapsed,
                    );
                }
                if elapsed >= budget {
                    let message = format!(
                        "lifecycle op '{action}' timed out after {}ms",
                        elapsed.as_millis()
                    );
                    handle.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
                        request_id,
                        action,
                        session,
                        extra_args,
                        phase: SidebandPhase::TimedOut,
                        elapsed,
                        error: Some(message.clone()),
                    });
                    return SidebandResponse {
                        ok: false,
                        message,
                        snapshot: Some(handle.snapshot()),
                        timed_out: true,
                        payload: None,
                        request_id: None,
                    };
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return SidebandResponse {
                    ok: false,
                    message: "lifecycle worker panicked".into(),
                    snapshot: Some(handle.snapshot()),
                    timed_out: false,
                    payload: None,
                    request_id: None,
                };
            }
        }
    }
}

fn run_inline_with_timeout(
    handle: &SupervisorHandle,
    request: SidebandRequest,
    budget: Duration,
    request_id: &str,
    action: &str,
    session: Option<&str>,
    extra_args: &[String],
) -> SidebandResponse {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("failed to build inline side-effect runtime");
    let started = Instant::now();
    let slow_warn_at = budget / 2;
    let handle_clone = handle.clone();
    let request_clone = request.clone();
    let ack_context = request_ack_context_for(request_id, action, &request);
    let result = runtime.block_on(async move {
        tokio::time::timeout(
            budget,
            handle_clone.apply_side_effect_request_async(request_clone, request_id, ack_context),
        )
        .await
    });

    let elapsed = started.elapsed();
    if elapsed >= slow_warn_at {
        handle.emit_sideband_lifecycle(
            request_id,
            action,
            session,
            extra_args,
            SidebandPhase::SlowWarning,
            elapsed,
        );
    }

    match result {
        Ok(response) => response,
        Err(_) => {
            let message = format!(
                "side-effect op '{action}' timed out after {}ms (boundary pre-PTY-write unless documented otherwise)",
                budget.as_millis()
            );
            handle.emit_sideband_lifecycle_with_error(SidebandLifecycleEvent {
                request_id,
                action,
                session,
                extra_args,
                phase: SidebandPhase::TimedOut,
                elapsed: budget,
                error: Some(message.clone()),
            });
            SidebandResponse {
                ok: false,
                message,
                snapshot: Some(handle.snapshot()),
                timed_out: true,
                payload: None,
                request_id: None,
            }
        }
    }
}

fn write_and_archive_response(
    handle: &SupervisorHandle,
    request_path: &Path,
    outbox_dir: &Path,
    processed_dir: &Path,
    response: &SidebandResponse,
) -> Result<()> {
    let mailbox_fs = handle.inner.mailbox_fs.read().clone();
    let file_name = request_path.file_name().with_context(|| {
        format!(
            "mailbox request missing file name: {}",
            request_path.display()
        )
    })?;
    let response_path = outbox_dir.join(file_name);
    let temp_response_path = outbox_dir.join(format!("{}.tmp", file_name.to_string_lossy()));
    let payload = format!("{}\n", encode_response(response)?);
    let archived_request_path = processed_dir.join(file_name);

    let mut last_error = None;
    for _attempt in 0..3 {
        let result: Result<()> = (|| {
            mailbox_fs
                .write(&temp_response_path, payload.as_bytes())
                .with_context(|| {
                    format!(
                        "failed to write mailbox response {}",
                        temp_response_path.display()
                    )
                })?;
            mailbox_fs
                .rename(&temp_response_path, &response_path)
                .with_context(|| {
                    format!(
                        "failed to publish mailbox response {}",
                        response_path.display()
                    )
                })?;
            if archived_request_path.exists() {
                mailbox_fs
                    .remove_file(&archived_request_path)
                    .with_context(|| {
                        format!(
                            "failed to clear archived mailbox request {}",
                            archived_request_path.display()
                        )
                    })?;
            }
            mailbox_fs
                .rename(request_path, &archived_request_path)
                .with_context(|| {
                    format!(
                        "failed to archive mailbox request {}",
                        request_path.display()
                    )
                })?;
            Ok(())
        })();

        match result {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = Some(error);
                let _ = mailbox_fs.remove_file(&temp_response_path);
                thread::sleep(Duration::from_millis(50));
            }
        }
    }

    let error = last_error.expect("archive retries should record an error");
    let poison_dir = request_path
        .parent()
        .with_context(|| format!("request path missing parent: {}", request_path.display()))?
        .parent()
        .with_context(|| format!("sideband inbox missing parent: {}", request_path.display()))?
        .join("poison");
    fs::create_dir_all(&poison_dir)
        .with_context(|| format!("failed to create poison directory {}", poison_dir.display()))?;
    let poison_path = poison_dir.join(format!(
        "{}-{}",
        now_rfc3339().replace(':', "-"),
        file_name.to_string_lossy()
    ));
    mailbox_fs
        .rename(request_path, &poison_path)
        .with_context(|| {
            format!(
                "failed to poison mailbox request {}",
                request_path.display()
            )
        })?;
    fs::write(poison_path.with_extension("error"), format!("{error:#}\n"))
        .with_context(|| format!("failed to write poison error {}", poison_path.display()))?;
    Err(error)
}

fn build_launch_spec(definition: &SessionDefinition) -> shared_types::LaunchSpec {
    match definition.driver {
        DriverKind::Claude => driver_claude::launch_spec(definition),
        DriverKind::Codex => driver_codex::launch_spec(definition),
        DriverKind::GenericTerminal => driver_generic_terminal::launch_spec(definition),
    }
}

fn scope_label(scope: MessageScope) -> &'static str {
    match scope {
        MessageScope::Direct => "Direct",
        MessageScope::Room => "Room",
        MessageScope::System => "System",
        MessageScope::Private => "Private",
    }
}

fn cancel_quiesce_timer_locked(slot: &mut SessionSlot) {
    if let Some(timer) = slot.quiesce_timer.take() {
        timer.handle.abort();
    }
}

fn cancel_stall_detector_locked(slot: &mut SessionSlot) {
    if let Some(detector) = slot.stall_detector.take() {
        detector.handle.abort();
    }
}

fn reset_work_state_locked(slot: &mut SessionSlot) {
    cancel_stall_detector_locked(slot);
    slot.work_state = WorkState::Idle;
    slot.work_state_observed = false;
    slot.work_detail = None;
    slot.launch_banner_seen = false;
    slot.work_error_observations.clear();
    slot.stall_state_entered_at = None;
    slot.stall_state_entered_timestamp = None;
}

fn classify_work_state_for_driver(
    driver: DriverKind,
    chunk: &str,
) -> Option<(WorkState, Option<String>)> {
    match driver {
        DriverKind::Claude => driver_claude::classify_work_state(chunk),
        DriverKind::Codex => driver_codex::classify_work_state(chunk),
        DriverKind::GenericTerminal => None,
    }
}

fn transition_work_state_locked(
    session_name: &str,
    slot: &mut SessionSlot,
    mut state: WorkState,
    detail: Option<String>,
) -> Option<RuntimeEvent> {
    if state == WorkState::Blocked {
        if let Some(key) = detail.as_deref() {
            let now = Instant::now();
            let observations = slot
                .work_error_observations
                .entry(key.to_string())
                .or_default();
            observations.retain(|seen_at| now.duration_since(*seen_at) <= Duration::from_secs(60));
            observations.push(now);
            if observations.len() > 2 {
                state = WorkState::ErrorLoop;
            }
        }
    } else if state != WorkState::ErrorLoop {
        slot.work_error_observations.clear();
    }

    if slot.work_state == state {
        slot.work_state_observed = true;
        slot.work_detail = detail;
        return None;
    }

    let previous_state = Some(slot.work_state);
    slot.work_state = state;
    slot.work_state_observed = true;
    slot.work_detail = detail.clone();
    Some(RuntimeEvent::SessionWorkState {
        session: session_name.to_string(),
        state,
        detail,
        previous_state,
        timestamp: now_rfc3339(),
    })
}

fn dispatch_overlap_reason(decision: &DispatchAttemptDecision) -> Option<&'static str> {
    let recent_route_from_target = decision
        .last_route_from_target_instant
        .map(|last_route| last_route.elapsed() <= RECENT_ROUTE_OVERLAP_WINDOW)
        .unwrap_or(false);

    if recent_route_from_target
        && matches!(
            decision.target_work_state_before,
            Some(WorkState::Thinking | WorkState::ToolCall)
        )
    {
        return Some("recent_route_from_target");
    }

    match decision.target_work_state_before {
        Some(WorkState::Thinking) => Some("target_thinking"),
        Some(WorkState::ToolCall) => Some("target_tool_call"),
        Some(WorkState::Blocked) => Some("target_blocked"),
        Some(WorkState::ErrorLoop) => Some("target_error_loop"),
        Some(WorkState::Exited) => Some("target_exited"),
        Some(WorkState::Idle) => None,
        None if decision.target_lifecycle_state_before != LifecycleState::Ready => {
            Some("target_not_ready")
        }
        None => None,
    }
}

fn quiesce_threshold(driver: DriverKind) -> Option<Duration> {
    let threshold = match driver {
        DriverKind::Claude => Duration::from_secs(3),
        DriverKind::Codex => Duration::from_secs(2),
        DriverKind::GenericTerminal => Duration::from_secs(5),
    };

    (threshold >= Duration::from_secs(1) && threshold <= Duration::from_secs(30))
        .then_some(threshold)
}

fn event_kind(event: &RuntimeEvent) -> &'static str {
    match event {
        RuntimeEvent::SessionOutput { .. } => "session_output",
        RuntimeEvent::SessionState { .. } => "session_state",
        RuntimeEvent::SessionExit { .. } => "session_exit",
        RuntimeEvent::SessionWorkState { .. } => "session_work_state",
        RuntimeEvent::SupervisorHeartbeat { .. } => "supervisor_heartbeat",
        RuntimeEvent::SupervisorAlert { .. } => "supervisor_alert",
        RuntimeEvent::DispatchTemplateWarning { .. } => "dispatch_template_warning",
        RuntimeEvent::PairCreated { .. } => "pair_created",
        RuntimeEvent::PairRenamed { .. } => "pair_renamed",
        RuntimeEvent::PairDeleted { .. } => "pair_deleted",
        RuntimeEvent::RoutedMessage { .. } => "routed_message",
        RuntimeEvent::RouteDelivery { .. } => "route_delivery",
        RuntimeEvent::DispatchAttempt { .. } => "dispatch_attempt",
        RuntimeEvent::PaneSignal { .. } => "pane_signal",
        RuntimeEvent::SystemLog { .. } => "system_log",
        RuntimeEvent::ControlPlaneReady { .. } => "control_plane_ready",
        RuntimeEvent::SidebandRequestLifecycle { .. } => "sideband_request_lifecycle",
        RuntimeEvent::RequestAck { .. } => "request_ack",
        RuntimeEvent::RequestAckTimeout { .. } => "request_ack_timeout",
        RuntimeEvent::DispatchNoReaction { .. } => "dispatch_no_reaction",
    }
}

fn event_matches_filter(event: &RuntimeEvent, filter: &EventFilter) -> bool {
    if !filter.includes_kind(event_kind(event)) {
        return false;
    }

    if !filter.include_sessions.is_empty() {
        let session_match = match event {
            RuntimeEvent::SessionOutput { session, .. }
            | RuntimeEvent::SessionState { session, .. }
            | RuntimeEvent::SessionExit { session, .. }
            | RuntimeEvent::SessionWorkState { session, .. } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == session),
            RuntimeEvent::RoutedMessage { from, to, .. } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == from || candidate == to),
            RuntimeEvent::RouteDelivery {
                from,
                logical_to,
                recipient,
                ..
            } => filter.include_sessions.iter().any(|candidate| {
                candidate == from
                    || candidate == logical_to
                    || recipient
                        .as_ref()
                        .map(|value| candidate == value)
                        .unwrap_or(false)
            }),
            RuntimeEvent::DispatchAttempt {
                from,
                target_session,
                ..
            } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == from || candidate == target_session),
            RuntimeEvent::SidebandRequestLifecycle { session, .. } => session
                .as_ref()
                .map(|value| {
                    filter
                        .include_sessions
                        .iter()
                        .any(|candidate| candidate == value)
                })
                .unwrap_or(false),
            RuntimeEvent::RequestAck { session, .. }
            | RuntimeEvent::RequestAckTimeout { session, .. }
            | RuntimeEvent::DispatchNoReaction { session, .. } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == session),
            RuntimeEvent::PaneSignal { session, .. } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == session),
            RuntimeEvent::SupervisorHeartbeat { sessions, .. } => sessions.iter().any(|summary| {
                filter
                    .include_sessions
                    .iter()
                    .any(|candidate| candidate == &summary.name)
            }),
            RuntimeEvent::SupervisorAlert { session, .. } => session
                .as_ref()
                .map(|value| {
                    filter
                        .include_sessions
                        .iter()
                        .any(|candidate| candidate == value)
                })
                .unwrap_or(false),
            RuntimeEvent::DispatchTemplateWarning { session, .. } => filter
                .include_sessions
                .iter()
                .any(|candidate| candidate == session),
            RuntimeEvent::PairCreated { .. }
            | RuntimeEvent::PairRenamed { .. }
            | RuntimeEvent::PairDeleted { .. }
            | RuntimeEvent::SystemLog { .. }
            | RuntimeEvent::ControlPlaneReady { .. } => false,
        };

        if !session_match {
            return false;
        }
    }

    match event {
        RuntimeEvent::RoutedMessage { scope, .. } if !filter.include_scopes.is_empty() => filter
            .include_scopes
            .iter()
            .any(|candidate| candidate == message_scope_name(*scope)),
        RuntimeEvent::RouteDelivery { scope, .. } if !filter.include_scopes.is_empty() => filter
            .include_scopes
            .iter()
            .any(|candidate| candidate == message_scope_name(*scope)),
        _ => true,
    }
}

fn message_scope_name(scope: MessageScope) -> &'static str {
    match scope {
        MessageScope::Direct => "direct",
        MessageScope::Room => "room",
        MessageScope::System => "system",
        MessageScope::Private => "private",
    }
}

fn normalized_required_field(field: &str, value: String) -> Result<String> {
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(anyhow!("pane_signal requires non-empty {field}"));
    }
    Ok(value)
}

fn pane_signal_type_name(signal_type: PaneSignalType) -> &'static str {
    match signal_type {
        PaneSignalType::Done => "done",
        PaneSignalType::Blocked => "blocked",
        PaneSignalType::Yellow => "yellow",
        PaneSignalType::Heartbeat => "heartbeat",
        PaneSignalType::Progress => "progress",
    }
}

fn pane_signal_file_timestamp(now: DateTime<Utc>) -> String {
    now.format("%Y%m%dT%H%M%S%.9fZ").to_string()
}

fn pane_signal_filename_component(value: &str) -> String {
    let sanitized = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let trimmed = sanitized.trim_matches('.');
    if trimmed.is_empty() {
        "signal".into()
    } else {
        trimmed.into()
    }
}

fn write_atomic_bytes(path: &Path, contents: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| anyhow!("path has no UTF-8 filename: {}", path.display()))?;
    let temp_path = parent.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));

    let result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
            .with_context(|| format!("failed to create temp file {}", temp_path.display()))?;
        file.write_all(contents)
            .with_context(|| format!("failed to write temp file {}", temp_path.display()))?;
        file.sync_all()
            .with_context(|| format!("failed to sync temp file {}", temp_path.display()))?;
        drop(file);
        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "failed to rename temp file {} to {}",
                temp_path.display(),
                path.display()
            )
        })?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    result
}

fn write_empty_touch_file(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create directory {}", parent.display()))?;
    fs::write(path, b"").with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn audit_file_name(date: NaiveDate) -> String {
    format!("{date}.jsonl")
}

fn parse_audit_file_date(name: &str) -> Option<NaiveDate> {
    if !is_valid_audit_filename(name) {
        return None;
    }

    NaiveDate::parse_from_str(&name[..10], "%Y-%m-%d").ok()
}

fn gzip_path_for(path: &Path) -> Result<PathBuf> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            anyhow!(
                "audit rotate: path has no UTF-8 filename: {}",
                path.display()
            )
        })?;
    Ok(path.with_file_name(format!("{file_name}.gz")))
}

fn gzip_and_remove(path: &Path) -> Result<()> {
    use flate2::{Compression, write::GzEncoder};

    let gz_path = gzip_path_for(path)?;
    let mut input = StdBufReader::new(
        File::open(path)
            .with_context(|| format!("audit rotate: failed to open {}", path.display()))?,
    );
    let output = File::create(&gz_path)
        .with_context(|| format!("audit rotate: failed to create {}", gz_path.display()))?;
    let mut encoder = GzEncoder::new(output, Compression::default());
    std::io::copy(&mut input, &mut encoder)
        .with_context(|| format!("audit rotate: failed to gzip {}", path.display()))?;
    encoder
        .finish()
        .with_context(|| format!("audit rotate: failed to finish {}", gz_path.display()))?;
    fs::remove_file(path)
        .with_context(|| format!("audit rotate: failed to remove {}", path.display()))?;
    Ok(())
}

fn is_valid_audit_filename(name: &str) -> bool {
    name.len() == 16
        && name.ends_with(".jsonl")
        && name.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            10..=15 => true,
            _ => byte.is_ascii_digit(),
        })
        && name.as_bytes()[10..] == *b".jsonl"
}

fn action_label_for(request: &SidebandRequest) -> &'static str {
    match request {
        SidebandRequest::Ping { .. } => "ping",
        SidebandRequest::ListSessions { .. } => "list_sessions",
        SidebandRequest::CreatePair { .. } => "create_pair",
        SidebandRequest::StartSession { .. } => "start_session",
        SidebandRequest::StopSession { .. } => "stop_session",
        SidebandRequest::RestartSession { .. } => "restart_session",
        SidebandRequest::DeliverMessage { .. } => "deliver_message",
        SidebandRequest::WaitQuiet { .. } => "wait_quiet",
        SidebandRequest::EventsSince { .. } => "events_since",
        SidebandRequest::SendInput { .. } => "send_input",
        SidebandRequest::SendKey { .. } => "send_key",
        SidebandRequest::RouteMessage { .. } => "route_message",
        SidebandRequest::PaneSignal { .. } => "pane_signal",
    }
}

fn request_ack_context_for(
    request_id: &str,
    action: &str,
    request: &SidebandRequest,
) -> Option<RequestAckContext> {
    match request {
        SidebandRequest::DeliverMessage { .. }
        | SidebandRequest::SendInput { .. }
        | SidebandRequest::SendKey { .. }
        | SidebandRequest::RouteMessage { .. } => Some(RequestAckContext {
            request_id: request_id.to_string(),
            action: action.to_string(),
        }),
        SidebandRequest::Ping { .. }
        | SidebandRequest::ListSessions { .. }
        | SidebandRequest::CreatePair { .. }
        | SidebandRequest::StartSession { .. }
        | SidebandRequest::StopSession { .. }
        | SidebandRequest::RestartSession { .. }
        | SidebandRequest::WaitQuiet { .. }
        | SidebandRequest::EventsSince { .. }
        | SidebandRequest::PaneSignal { .. } => None,
    }
}

fn heartbeat_interval_from_env() -> Duration {
    positive_duration_from_env(
        "PRIM1_HEARTBEAT_INTERVAL_SECS",
        DEFAULT_HEARTBEAT_INTERVAL_SECS,
    )
}

fn reaction_window_from_env() -> Duration {
    positive_duration_from_env("PRIM1_REACTION_WINDOW_SECS", DEFAULT_REACTION_WINDOW_SECS)
}

fn auto_restart_stall_threshold_from_env() -> Duration {
    positive_duration_from_env(
        "PRIM1_AUTO_RESTART_STALL_THRESHOLD_SECS",
        DEFAULT_AUTO_RESTART_STALL_THRESHOLD_SECS,
    )
}

fn positive_duration_from_env(name: &str, default_seconds: u64) -> Duration {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(default_seconds))
}

fn auto_restart_sessions_from_env() -> Vec<String> {
    std::env::var("PRIM1_AUTO_RESTART_ON_STALL")
        .unwrap_or_default()
        .split(',')
        .map(|session| session.trim().to_string())
        .filter(|session| !session.is_empty())
        .collect()
}

fn dispatch_template_pattern_match(content: &str) -> (Vec<String>, Vec<String>) {
    let lower_content = content.to_ascii_lowercase();
    let mut detected = Vec::new();
    let mut missing = Vec::new();

    for pattern in DISPATCH_TEMPLATE_PATTERNS {
        if lower_content.contains(&pattern.to_ascii_lowercase()) {
            detected.push(pattern.to_string());
        } else {
            missing.push(pattern.to_string());
        }
    }

    (detected, missing)
}

fn work_state_alert_label(state: Option<WorkState>) -> &'static str {
    match state {
        Some(WorkState::Idle) => "idle",
        Some(WorkState::Thinking) => "thinking",
        Some(WorkState::ToolCall) => "tool_call",
        Some(WorkState::Blocked) => "blocked",
        Some(WorkState::ErrorLoop) => "error_loop",
        Some(WorkState::Exited) => "exited",
        None => "unknown",
    }
}

fn session_name_of(request: &SidebandRequest) -> Option<&str> {
    match request {
        SidebandRequest::StartSession { name, .. }
        | SidebandRequest::StopSession { name, .. }
        | SidebandRequest::RestartSession { name, .. }
        | SidebandRequest::DeliverMessage { name, .. }
        | SidebandRequest::WaitQuiet { name, .. }
        | SidebandRequest::SendInput { name, .. }
        | SidebandRequest::SendKey { name, .. } => Some(name.as_str()),
        SidebandRequest::RouteMessage { request, .. } => Some(request.to.as_str()),
        SidebandRequest::Ping { .. }
        | SidebandRequest::ListSessions { .. }
        | SidebandRequest::CreatePair { .. }
        | SidebandRequest::EventsSince { .. }
        | SidebandRequest::PaneSignal { .. } => None,
    }
}

fn start_session_extra_args_of(request: &SidebandRequest) -> &[String] {
    match request {
        SidebandRequest::StartSession { extra_args, .. } => extra_args.as_slice(),
        _ => &[],
    }
}

fn validate_extra_args(extra_args: &[String]) -> Result<()> {
    if let Some(index) = extra_args.iter().position(|arg| arg.trim().is_empty()) {
        return Err(anyhow!(
            "extra_args[{index}] must not be empty or whitespace-only"
        ));
    }

    Ok(())
}

#[cfg(test)]
fn routed_message_payload(request: &RouteMessageRequest, behavior: SubmitBehavior) -> String {
    routed_message_payloads(request, behavior)
        .into_iter()
        .next()
        .unwrap_or_default()
}

fn routed_message_payloads(request: &RouteMessageRequest, behavior: SubmitBehavior) -> Vec<String> {
    let message_content = prepare_direct_message(
        if behavior.flatten_payload {
            DriverKind::Codex
        } else {
            DriverKind::Claude
        },
        &request.content,
    )
    .into_iter()
    .next()
    .unwrap_or_default();
    let content_chunks = split_routed_message_content(&message_content, behavior.max_chunk_chars);
    let total_parts = content_chunks.len();

    content_chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let header =
                routed_message_header(request.scope, &request.from, index + 1, total_parts);
            if behavior.flatten_payload {
                if chunk.is_empty() {
                    header
                } else {
                    format!("{header} {chunk}")
                }
            } else {
                format!("\n{header}\n{chunk}\n")
            }
        })
        .collect()
}

fn routed_message_submit_behavior(driver: DriverKind) -> SubmitBehavior {
    match driver {
        DriverKind::Codex => SubmitBehavior {
            sequence: "\r",
            delay: Duration::from_millis(500),
            flatten_payload: true,
            max_chunk_chars: Some(CODEX_ROUTED_MESSAGE_MAX_CHARS),
        },
        DriverKind::Claude => SubmitBehavior {
            sequence: "\r",
            delay: Duration::from_millis(200),
            flatten_payload: false,
            max_chunk_chars: Some(CLAUDE_ROUTED_MESSAGE_MAX_CHARS),
        },
        DriverKind::GenericTerminal => SubmitBehavior {
            sequence: "\r",
            delay: Duration::ZERO,
            flatten_payload: false,
            max_chunk_chars: None,
        },
    }
}

fn prepare_direct_message(driver: DriverKind, content: &str) -> Vec<String> {
    match driver {
        DriverKind::Codex => vec![collapse_inline_content(content)],
        DriverKind::Claude | DriverKind::GenericTerminal => vec![content.to_string()],
    }
}

fn load_existing_control_plane_status(info_path: &Path) -> Result<Option<ControlPlaneStatus>> {
    if !info_path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(info_path).with_context(|| {
        format!(
            "failed to read existing control plane info file {}",
            info_path.display()
        )
    })?;
    let status = serde_json::from_str::<ControlPlaneStatus>(&raw).with_context(|| {
        format!(
            "failed to parse existing control plane info file {}",
            info_path.display()
        )
    })?;
    Ok(Some(status))
}

fn probe_control_plane_owner(status: &ControlPlaneStatus, timeout: Duration) -> Result<bool> {
    let payload = format!(
        "{}\n",
        encode_request(&SidebandRequest::Ping {
            token: status.token.clone(),
        })?
    );
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }

        if try_probe_control_plane_endpoint(&status.endpoint, &payload, remaining)? {
            return Ok(true);
        }

        let sleep_for = CONTROL_PLANE_PROBE_RETRY_INTERVAL
            .min(deadline.saturating_duration_since(Instant::now()));
        if sleep_for.is_zero() {
            return Ok(false);
        }

        thread::sleep(sleep_for);
    }
}

#[cfg(windows)]
fn try_probe_control_plane_endpoint(
    endpoint: &str,
    payload: &str,
    timeout: Duration,
) -> Result<bool> {
    use tokio::net::windows::named_pipe::ClientOptions;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("failed to create control-plane probe runtime")?;

    runtime.block_on(async {
        let client = match ClientOptions::new().open(endpoint) {
            Ok(client) => client,
            Err(_) => return Ok(false),
        };

        tokio::time::timeout(timeout, async move {
            let (read_half, mut write_half) = tokio::io::split(client);
            write_half
                .write_all(payload.as_bytes())
                .await
                .context("failed to write probe request")?;
            write_half
                .flush()
                .await
                .context("failed to flush probe request")?;

            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .await
                .context("failed to read probe response")?;
            Ok::<bool, anyhow::Error>(bytes > 0)
        })
        .await
        .map_err(|_| anyhow!("control-plane probe timed out"))?
    })
}

#[cfg(unix)]
fn try_probe_control_plane_endpoint(
    endpoint: &str,
    payload: &str,
    timeout: Duration,
) -> Result<bool> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
        .context("failed to create control-plane probe runtime")?;

    runtime.block_on(async {
        let client =
            match tokio::time::timeout(timeout, tokio::net::UnixStream::connect(endpoint)).await {
                Ok(Ok(client)) => client,
                Ok(Err(_)) | Err(_) => return Ok(false),
            };

        tokio::time::timeout(timeout, async move {
            let (read_half, mut write_half) = tokio::io::split(client);
            write_half
                .write_all(payload.as_bytes())
                .await
                .context("failed to write probe request")?;
            write_half
                .flush()
                .await
                .context("failed to flush probe request")?;

            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            let bytes = reader
                .read_line(&mut line)
                .await
                .context("failed to read probe response")?;
            Ok::<bool, anyhow::Error>(bytes > 0)
        })
        .await
        .map_err(|_| anyhow!("control-plane probe timed out"))?
    })
}

fn collapse_inline_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn routed_message_header(
    scope: MessageScope,
    sender: &str,
    part_number: usize,
    total_parts: usize,
) -> String {
    if total_parts <= 1 {
        return format!("[{} message from {}]", scope_label(scope), sender);
    }

    format!(
        "[{} message from {} | part {}/{}]",
        scope_label(scope),
        sender,
        part_number,
        total_parts
    )
}

fn split_routed_message_content(content: &str, max_chunk_chars: Option<usize>) -> Vec<String> {
    let Some(max_chunk_chars) = max_chunk_chars else {
        return vec![content.to_string()];
    };

    if content.chars().count() <= max_chunk_chars {
        return vec![content.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = content.trim();

    while remaining.chars().count() > max_chunk_chars {
        let split_index = split_point_within_limit(remaining, max_chunk_chars);
        let (chunk, tail) = remaining.split_at(split_index);
        let chunk = chunk.trim();
        if !chunk.is_empty() {
            chunks.push(chunk.to_string());
        }
        remaining = tail.trim_start();
    }

    if !remaining.is_empty() {
        chunks.push(remaining.to_string());
    }

    if chunks.is_empty() {
        vec![String::new()]
    } else {
        chunks
    }
}

fn split_point_within_limit(content: &str, max_chunk_chars: usize) -> usize {
    let mut last_whitespace_index = None;
    for (char_count, (index, ch)) in content.char_indices().enumerate() {
        if char_count == max_chunk_chars {
            break;
        }
        if ch.is_whitespace() {
            last_whitespace_index = Some(index);
        }
    }

    if let Some(index) = last_whitespace_index {
        return index;
    }

    content
        .char_indices()
        .nth(max_chunk_chars)
        .map(|(index, _)| index)
        .unwrap_or(content.len())
}

fn control_key_sequence(key: ControlKey) -> &'static str {
    match key {
        ControlKey::Enter => "\r",
        ControlKey::Up => "\x1b[A",
        ControlKey::Down => "\x1b[B",
        ControlKey::Right => "\x1b[C",
        ControlKey::Left => "\x1b[D",
        ControlKey::Tab => "\t",
        ControlKey::Esc => "\x1b",
        ControlKey::CtrlC => "\x03",
    }
}

fn chunk_has_real_content(chunk: &str) -> bool {
    if chunk.len() < 3 {
        return false;
    }

    let visible = strip_terminal_control_sequences(chunk);
    let trimmed = visible.trim();
    if trimmed.is_empty() {
        return false;
    }

    let mut chars = trimmed.chars();
    if let (Some(ch), None) = (chars.next(), chars.next()) {
        let codepoint = ch as u32;
        if (0x2800..=0x28ff).contains(&codepoint) {
            return false;
        }
    }

    true
}

fn strip_terminal_control_sequences(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index] == 0x1b {
            if index + 1 < bytes.len() && bytes[index + 1] == b']' {
                index += 2;
                while index < bytes.len() {
                    if bytes[index] == 0x07 {
                        index += 1;
                        break;
                    }
                    if bytes[index] == 0x1b && index + 1 < bytes.len() && bytes[index + 1] == b'\\'
                    {
                        index += 2;
                        break;
                    }
                    index += 1;
                }
                continue;
            }

            if index + 1 < bytes.len() && bytes[index + 1] == b'[' {
                index += 2;
                while index < bytes.len() {
                    let byte = bytes[index];
                    if (0x40..=0x7e).contains(&byte) {
                        index += 1;
                        break;
                    }
                    index += 1;
                }
                continue;
            }
        }

        output.push(bytes[index]);
        index += 1;
    }

    String::from_utf8_lossy(&output).replace('\r', "")
}

fn control_plane_transport() -> &'static str {
    #[cfg(windows)]
    {
        "named_pipe"
    }

    #[cfg(not(windows))]
    {
        "unix_socket"
    }
}

fn control_plane_endpoint() -> String {
    #[cfg(windows)]
    {
        format!("{DEFAULT_ENDPOINT}-{}", std::process::id())
    }

    #[cfg(not(windows))]
    {
        format!("{DEFAULT_ENDPOINT}-{}.sock", std::process::id())
    }
}

#[cfg(windows)]
async fn run_windows_pipe_server(
    handle: SupervisorHandle,
    status: ControlPlaneStatus,
) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    loop {
        let server = ServerOptions::new()
            .create(&status.endpoint)
            .with_context(|| format!("failed to create named pipe {}", status.endpoint))?;
        server
            .connect()
            .await
            .context("failed to connect named pipe")?;
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_sideband_stream(handle_clone, server).await {
                eprintln!("named pipe connection failed: {error}");
            }
        });
    }
}

#[cfg(unix)]
async fn run_unix_socket_server(
    handle: SupervisorHandle,
    status: ControlPlaneStatus,
) -> Result<()> {
    use tokio::net::UnixListener;

    let _ = fs::remove_file(&status.endpoint);
    let listener = UnixListener::bind(&status.endpoint)
        .with_context(|| format!("failed to bind unix socket {}", status.endpoint))?;

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept unix socket")?;
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_sideband_stream(handle_clone, stream).await {
                eprintln!("unix socket connection failed: {error}");
            }
        });
    }
}

async fn handle_sideband_stream<Stream>(handle: SupervisorHandle, stream: Stream) -> Result<()>
where
    Stream: tokio::io::AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .context("failed to read sideband request")?;

    let request = decode_request(line.trim()).context("invalid sideband payload")?;
    let response = handle.apply_sideband_request_async(request).await;
    let payload = format!("{}\n", encode_response(&response)?);
    write_half
        .write_all(payload.as_bytes())
        .await
        .context("failed to write sideband response")?;
    write_half
        .flush()
        .await
        .context("failed to flush sideband response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_plane::{decode_response, encode_request};
    use parking_lot::Condvar;
    use pty_host::PtySession as PtySessionTrait;
    use shared_types::{MessageScope, RouteMessageRequest, SidebandRequest};
    use std::{
        collections::VecDeque,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    #[derive(Clone, Copy)]
    enum MockKillBehavior {
        Immediate,
        Sleep(Duration),
    }

    struct MockPtySession {
        process_id: Option<u32>,
        exit_status: Option<pty_host::PtyExitStatus>,
        send_input_count: Arc<AtomicUsize>,
        kill_count: Arc<AtomicUsize>,
        kill_behavior: MockKillBehavior,
        agent_liveness: Arc<Mutex<AgentLiveness>>,
        on_send: Option<Arc<dyn Fn() + Send + Sync>>,
    }

    impl PtySessionTrait for MockPtySession {
        fn send_input(&self, input: &str) -> Result<usize> {
            self.send_input_count.fetch_add(1, Ordering::SeqCst);
            if let Some(on_send) = self.on_send.clone() {
                thread::spawn(move || on_send());
            }
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            self.kill_count.fetch_add(1, Ordering::SeqCst);
            if let MockKillBehavior::Sleep(duration) = self.kill_behavior {
                thread::sleep(duration);
            }
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(self.exit_status.clone())
        }

        fn process_id(&self) -> Option<u32> {
            self.process_id
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(self.agent_liveness.lock().clone())
        }
    }

    struct FailingPtySession {
        send_input_count: Arc<AtomicUsize>,
        error_message: String,
    }

    impl PtySessionTrait for FailingPtySession {
        fn send_input(&self, _input: &str) -> Result<usize> {
            self.send_input_count.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!(self.error_message.clone()))
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    fn mock_pty_session(
        process_id: Option<u32>,
        kill_behavior: MockKillBehavior,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        mock_pty_session_with_exit_status(process_id, None, kill_behavior)
    }

    fn mock_pty_session_with_exit_status(
        process_id: Option<u32>,
        exit_status: Option<pty_host::PtyExitStatus>,
        kill_behavior: MockKillBehavior,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        mock_pty_session_full(
            process_id,
            exit_status,
            kill_behavior,
            AgentLiveness::NotYetObserved(Vec::new()),
            None,
        )
    }

    fn mock_pty_session_full(
        process_id: Option<u32>,
        exit_status: Option<pty_host::PtyExitStatus>,
        kill_behavior: MockKillBehavior,
        agent_liveness: AgentLiveness,
        on_send: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let send_input_count = Arc::new(AtomicUsize::new(0));
        let kill_count = Arc::new(AtomicUsize::new(0));
        (
            Box::new(MockPtySession {
                process_id,
                exit_status,
                send_input_count: send_input_count.clone(),
                kill_count: kill_count.clone(),
                kill_behavior,
                agent_liveness: Arc::new(Mutex::new(agent_liveness)),
                on_send,
            }) as Box<dyn PtySessionTrait>,
            send_input_count,
            kill_count,
        )
    }

    fn reacting_pty_session(
        supervisor: &SupervisorHandle,
        session: &str,
        chunk: &str,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let handle = supervisor.clone();
        let session = session.to_string();
        let chunk = chunk.to_string();
        mock_pty_session_full(
            None,
            None,
            MockKillBehavior::Immediate,
            AgentLiveness::Alive(Vec::new()),
            Some(Arc::new(move || {
                handle.handle_pty_event(&session, 0, PtyEvent::Output(chunk.clone()));
            })),
        )
    }

    fn failing_pty_session(error_message: &str) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>) {
        let send_input_count = Arc::new(AtomicUsize::new(0));
        (
            Box::new(FailingPtySession {
                send_input_count: send_input_count.clone(),
                error_message: error_message.into(),
            }) as Box<dyn PtySessionTrait>,
            send_input_count,
        )
    }

    fn pty_exit_status(
        exit_code: u32,
        signal: Option<&str>,
        success: bool,
    ) -> pty_host::PtyExitStatus {
        pty_host::PtyExitStatus {
            exit_code,
            signal: signal.map(ToOwned::to_owned),
            success,
        }
    }

    struct QueuePtySpawner {
        sessions: Mutex<VecDeque<Box<dyn PtySessionTrait>>>,
    }

    impl QueuePtySpawner {
        fn new(sessions: Vec<Box<dyn PtySessionTrait>>) -> Self {
            Self {
                sessions: Mutex::new(sessions.into()),
            }
        }
    }

    impl PtySpawner for QueuePtySpawner {
        fn spawn(
            &self,
            _spec: &LaunchSpec,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.sessions
                .lock()
                .pop_front()
                .ok_or_else(|| anyhow!("no queued PTY sessions"))
        }
    }

    struct CapturingPtySpawner {
        sessions: Mutex<VecDeque<Box<dyn PtySessionTrait>>>,
        specs: Arc<Mutex<Vec<LaunchSpec>>>,
    }

    impl CapturingPtySpawner {
        fn new(sessions: Vec<Box<dyn PtySessionTrait>>) -> (Self, Arc<Mutex<Vec<LaunchSpec>>>) {
            let specs = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    sessions: Mutex::new(sessions.into()),
                    specs: specs.clone(),
                },
                specs,
            )
        }
    }

    impl PtySpawner for CapturingPtySpawner {
        fn spawn(
            &self,
            spec: &LaunchSpec,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.specs.lock().push(spec.clone());
            self.sessions
                .lock()
                .pop_front()
                .ok_or_else(|| anyhow!("no queued PTY sessions"))
        }
    }

    struct StagedPtySpawner {
        release_gate: Arc<(Mutex<bool>, Condvar)>,
        stage_first_spawn: AtomicBool,
        sessions: Mutex<VecDeque<Box<dyn PtySessionTrait>>>,
    }

    impl StagedPtySpawner {
        fn new(
            release_gate: Arc<(Mutex<bool>, Condvar)>,
            sessions: Vec<Box<dyn PtySessionTrait>>,
        ) -> Self {
            Self {
                release_gate,
                stage_first_spawn: AtomicBool::new(true),
                sessions: Mutex::new(sessions.into()),
            }
        }
    }

    impl PtySpawner for StagedPtySpawner {
        fn spawn(
            &self,
            _spec: &LaunchSpec,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            let session = self
                .sessions
                .lock()
                .pop_front()
                .ok_or_else(|| anyhow!("no staged PTY sessions"))?;

            if self.stage_first_spawn.swap(false, Ordering::SeqCst) {
                let (lock, cvar) = &*self.release_gate;
                let mut released = lock.lock();
                while !*released {
                    cvar.wait(&mut released);
                }
            }

            Ok(session)
        }
    }

    struct FailingRenameMailboxFs {
        rename_failures_remaining: AtomicUsize,
    }

    impl FailingRenameMailboxFs {
        fn new(rename_failures: usize) -> Self {
            Self {
                rename_failures_remaining: AtomicUsize::new(rename_failures),
            }
        }
    }

    impl MailboxFs for FailingRenameMailboxFs {
        fn read_to_string(&self, path: &Path) -> std::io::Result<String> {
            fs::read_to_string(path)
        }

        fn write(&self, path: &Path, contents: &[u8]) -> std::io::Result<()> {
            fs::write(path, contents)
        }

        fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
            if self.rename_failures_remaining.load(Ordering::SeqCst) > 0 {
                self.rename_failures_remaining
                    .fetch_sub(1, Ordering::SeqCst);
                return Err(std::io::Error::other("synthetic rename failure"));
            }
            fs::rename(from, to)
        }

        fn remove_file(&self, path: &Path) -> std::io::Result<()> {
            fs::remove_file(path)
        }
    }

    fn test_supervisor() -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!("cli-master-wrapper-test-{}", Uuid::new_v4()));
        test_supervisor_with_root(root)
    }

    fn test_supervisor_with_root(root: PathBuf) -> SupervisorHandle {
        SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
            peer_slash_commands_allowed: false,
            cross_pair_room_broadcast: false,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
            reaction_window: Some(Duration::from_millis(200)),
        })
        .unwrap()
    }

    fn test_supervisor_with_peer_slash_commands_allowed(allowed: bool) -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!(
            "cli-master-wrapper-peer-policy-test-{}",
            Uuid::new_v4()
        ));
        SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
            peer_slash_commands_allowed: allowed,
            cross_pair_room_broadcast: false,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
            reaction_window: Some(Duration::from_millis(200)),
        })
        .unwrap()
    }

    fn test_supervisor_with_cross_pair_room_broadcast(enabled: bool) -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!(
            "cli-master-wrapper-cross-pair-test-{}",
            Uuid::new_v4()
        ));
        SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
            peer_slash_commands_allowed: false,
            cross_pair_room_broadcast: enabled,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
            reaction_window: Some(Duration::from_millis(200)),
        })
        .unwrap()
    }

    fn test_supervisor_with_wrapper_defaults(
        heartbeat_interval: Option<Duration>,
        auto_restart_sessions: Option<Vec<String>>,
        auto_restart_threshold: Option<Duration>,
    ) -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!(
            "cli-master-wrapper-defaults-test-{}",
            Uuid::new_v4()
        ));
        SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
            peer_slash_commands_allowed: false,
            cross_pair_room_broadcast: false,
            heartbeat_interval,
            auto_restart_on_stall_sessions: auto_restart_sessions,
            auto_restart_stall_threshold: auto_restart_threshold,
            reaction_window: Some(Duration::from_millis(200)),
        })
        .unwrap()
    }

    fn install_stale_running_session(supervisor: &SupervisorHandle, name: &str) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.running = Some(RunningSession { pty: None });
        slot.process_id = Some(u32::MAX);
        slot.state = LifecycleState::Busy;
        slot.last_real_output_at = None;
    }

    fn install_synthetic_running_session(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
    ) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.definition.driver = driver;
        slot.running = Some(RunningSession { pty: None });
        slot.process_id = None;
        slot.state = LifecycleState::Busy;
        slot.last_real_output_at = None;
    }

    fn install_mock_running_session(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
        pty: Box<dyn PtySessionTrait>,
    ) {
        install_mock_running_session_with_process_id(supervisor, name, driver, None, pty);
    }

    fn install_mock_running_session_with_process_id(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
        process_id: Option<u32>,
        pty: Box<dyn PtySessionTrait>,
    ) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.definition.driver = driver;
        slot.running = Some(RunningSession { pty: Some(pty) });
        slot.process_id = process_id;
        slot.state = LifecycleState::Busy;
        slot.last_real_output_at = None;
    }

    fn capture_runtime_events(supervisor: &SupervisorHandle) -> Arc<Mutex<Vec<RuntimeEvent>>> {
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });
        events
    }

    fn route_delivery_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::RouteDelivery { .. }))
            .cloned()
            .collect()
    }

    fn dispatch_attempt_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::DispatchAttempt { .. }))
            .cloned()
            .collect()
    }

    fn request_ack_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::RequestAck { .. }))
            .cloned()
            .collect()
    }

    fn dispatch_no_reaction_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::DispatchNoReaction { .. }))
            .cloned()
            .collect()
    }

    fn set_session_dispatch_state(
        supervisor: &SupervisorHandle,
        name: &str,
        lifecycle_state: LifecycleState,
        work_state: Option<WorkState>,
    ) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.state = lifecycle_state;
        slot.work_state = work_state.unwrap_or(WorkState::Idle);
        slot.work_state_observed = work_state.is_some();
    }

    fn mark_recent_route_from_session(supervisor: &SupervisorHandle, name: &str) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.last_route_from_session_at = Some(now_rfc3339());
        slot.last_route_from_session_instant = Some(Instant::now());
    }

    fn pane_signal_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::PaneSignal { .. }))
            .cloned()
            .collect()
    }

    fn work_state_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::SessionWorkState { .. }))
            .cloned()
            .collect()
    }

    fn session_exit_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::SessionExit { .. }))
            .cloned()
            .collect()
    }

    fn supervisor_alert_events(events: &Arc<Mutex<Vec<RuntimeEvent>>>) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::SupervisorAlert { .. }))
            .cloned()
            .collect()
    }

    fn dispatch_template_warning_events(
        events: &Arc<Mutex<Vec<RuntimeEvent>>>,
    ) -> Vec<RuntimeEvent> {
        events
            .lock()
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::DispatchTemplateWarning { .. }))
            .cloned()
            .collect()
    }

    fn wait_for_event_count<F>(events: &Arc<Mutex<Vec<RuntimeEvent>>>, expected: usize, matches: F)
    where
        F: Fn(&RuntimeEvent) -> bool,
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let count = events.lock().iter().filter(|event| matches(event)).count();
            if count >= expected {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {expected} matching event(s); saw {count}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn wait_until<F>(timeout: Duration, condition: F)
    where
        F: Fn() -> bool,
    {
        let deadline = Instant::now() + timeout;
        loop {
            if condition() {
                return;
            }
            assert!(Instant::now() < deadline, "timed out waiting for condition");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn session_token(supervisor: &SupervisorHandle, name: &str) -> String {
        supervisor.start_control_plane().unwrap();
        supervisor
            .ensure_session_control_plane_status(name)
            .unwrap()
            .unwrap()
            .token
    }

    fn current_audit_file(supervisor: &SupervisorHandle) -> String {
        supervisor.active_audit_file_name()
    }

    fn append_audit_event(
        supervisor: &SupervisorHandle,
        audit_file: &str,
        event: &RuntimeEvent,
    ) -> PathBuf {
        let path = supervisor.runtime_dir().join("audit").join(audit_file);
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .unwrap();
        writeln!(file, "{}", serde_json::to_string(event).unwrap()).unwrap();
        path
    }

    fn make_test_event(label: &str) -> RuntimeEvent {
        RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: label.into(),
            timestamp: "2026-04-20T00:00:00Z".into(),
        }
    }

    fn unwrap_events_since(
        response: SidebandResponse,
    ) -> (Vec<RuntimeEvent>, EventCursor, bool, String) {
        assert!(
            response.ok,
            "events_since response failed: {}",
            response.message
        );
        match response.payload {
            Some(SidebandResponsePayload::EventsSince {
                events,
                next_cursor,
                gap_detected,
                as_of,
            }) => (events, next_cursor, gap_detected, as_of),
            other => panic!("unexpected events_since payload: {other:?}"),
        }
    }

    #[test]
    fn audit_log_rotates_at_utc_midnight_with_gzip() {
        use chrono::TimeZone;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let day_one = Utc.with_ymd_and_hms(2026, 4, 19, 23, 59, 30).unwrap();
        let day_two = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 30).unwrap();
        let audit = AuditLog::new_at(temp_dir.path(), day_one).expect("new audit");

        audit
            .append_at(&make_test_event("pre-midnight"), day_one)
            .expect("append pre");
        audit
            .append_at(&make_test_event("post-midnight"), day_two)
            .expect("append post");

        let audit_dir = temp_dir.path().join("audit");
        let gz = audit_dir.join("2026-04-19.jsonl.gz");
        let plain_today = audit_dir.join("2026-04-20.jsonl");
        let plain_yesterday = audit_dir.join("2026-04-19.jsonl");

        assert!(gz.exists(), "rotated archive should exist");
        assert!(plain_today.exists(), "today's file should exist");
        assert!(
            !plain_yesterday.exists(),
            "yesterday's plain file should be removed after gzip"
        );

        let file = File::open(gz).expect("open gz");
        let mut decoder = flate2::read::GzDecoder::new(file);
        let mut text = String::new();
        decoder.read_to_string(&mut text).expect("decode gz");
        assert!(text.contains("pre-midnight"));
        assert!(!text.contains("post-midnight"));
    }

    #[test]
    fn audit_rotation_keeps_existing_archive_when_plain_file_is_absent() {
        use chrono::TimeZone;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let day_one = Utc.with_ymd_and_hms(2026, 4, 19, 23, 59, 30).unwrap();
        let day_two = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 30).unwrap();
        let audit = AuditLog::new_at(temp_dir.path(), day_one).expect("new audit");
        let audit_dir = temp_dir.path().join("audit");
        fs::create_dir_all(&audit_dir).unwrap();

        let gz = audit_dir.join("2026-04-19.jsonl.gz");
        fs::write(&gz, b"existing archive").unwrap();

        audit
            .append_at(&make_test_event("post-midnight"), day_two)
            .expect("append post");

        assert_eq!(fs::read(&gz).unwrap(), b"existing archive");
        assert!(audit_dir.join("2026-04-20.jsonl").exists());
    }

    #[test]
    fn audit_sweep_cleans_up_accumulated_stale_plain_files() {
        use chrono::TimeZone;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let day_nineteen = Utc.with_ymd_and_hms(2026, 4, 19, 12, 0, 0).unwrap();
        let day_twenty = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 30).unwrap();
        let audit = AuditLog::new_at(temp_dir.path(), day_nineteen).expect("new audit");
        let audit_dir = temp_dir.path().join("audit");

        for day in ["2026-04-17", "2026-04-18", "2026-04-19"] {
            fs::write(
                audit_dir.join(format!("{day}.jsonl")),
                format!(
                    "{}\n",
                    serde_json::to_string(&make_test_event(day)).unwrap()
                ),
            )
            .unwrap();
        }

        audit
            .append_at(&make_test_event("post-midnight"), day_twenty)
            .expect("append post");

        for day in ["2026-04-17", "2026-04-18", "2026-04-19"] {
            assert!(
                audit_dir.join(format!("{day}.jsonl.gz")).exists(),
                "{day} archive missing"
            );
            assert!(
                !audit_dir.join(format!("{day}.jsonl")).exists(),
                "{day} plain file should have been removed"
            );
        }
        assert!(audit_dir.join("2026-04-20.jsonl").exists());
    }

    #[test]
    fn events_since_spans_rotated_archive_and_active_file() {
        use chrono::TimeZone;

        let day_one = Utc.with_ymd_and_hms(2026, 4, 19, 12, 0, 0).unwrap();
        let day_two = Utc.with_ymd_and_hms(2026, 4, 20, 0, 0, 30).unwrap();
        let supervisor = test_supervisor();
        {
            let mut inner = supervisor.inner.audit.inner.lock();
            inner.active_date = day_one.date_naive();
            inner.path = supervisor
                .runtime_dir()
                .join("audit")
                .join(audit_file_name(day_one.date_naive()));
        }

        let event_a = make_test_event("A");
        let event_b = make_test_event("B");
        let event_c = make_test_event("C");
        let event_d = make_test_event("D");
        let event_e = make_test_event("E");
        supervisor.inner.audit.append_at(&event_a, day_one).unwrap();
        supervisor.inner.audit.append_at(&event_b, day_one).unwrap();
        supervisor.inner.audit.append_at(&event_c, day_one).unwrap();
        let cursor_after_a = serde_json::to_string(&event_a).unwrap().len() as u64 + 1;

        supervisor.inner.audit.append_at(&event_d, day_two).unwrap();
        supervisor.inner.audit.append_at(&event_e, day_two).unwrap();

        let result = supervisor
            .read_events_since(
                Some(EventCursor {
                    audit_file: audit_file_name(day_one.date_naive()),
                    byte_offset: cursor_after_a,
                }),
                &EventFilter {
                    include_kinds: vec!["system_log".into()],
                    include_sessions: Vec::new(),
                    include_scopes: Vec::new(),
                },
                10,
            )
            .unwrap();
        let messages = result
            .events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SystemLog { message, .. } => Some(message.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(messages, vec!["B", "C", "D", "E"]);
        assert_eq!(
            result.next_cursor.audit_file,
            audit_file_name(day_two.date_naive())
        );
        assert!(!result.gap_detected);
    }

    #[test]
    fn audit_append_is_single_writer_under_concurrent_load() {
        use chrono::TimeZone;

        let temp_dir = tempfile::tempdir().expect("tempdir");
        let now = Utc.with_ymd_and_hms(2026, 4, 20, 12, 0, 0).unwrap();
        let audit = Arc::new(AuditLog::new_at(temp_dir.path(), now).expect("new audit"));
        let thread_count = 16_usize;
        let events_per_thread = 100_usize;
        let mut handles = Vec::new();

        for thread_index in 0..thread_count {
            let audit = Arc::clone(&audit);
            handles.push(thread::spawn(move || {
                for event_index in 0..events_per_thread {
                    let label = format!("t{thread_index}-e{event_index}");
                    audit
                        .append_at(&make_test_event(&label), now)
                        .expect("append");
                }
            }));
        }
        for handle in handles {
            handle.join().expect("join");
        }

        let content = fs::read_to_string(audit.path()).expect("read active file");
        let lines = content.lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), thread_count * events_per_thread);
        for line in lines {
            serde_json::from_str::<RuntimeEvent>(line)
                .unwrap_or_else(|error| panic!("line did not parse: {error}\n{line}"));
        }
    }

    #[test]
    fn snapshot_contains_default_sessions() {
        let supervisor = test_supervisor();

        let snapshot = supervisor.snapshot();
        let names = snapshot
            .sessions
            .iter()
            .map(|session| session.name.clone())
            .collect::<Vec<_>>();

        assert_eq!(names, vec!["claude".to_string(), "codex".to_string(),]);
    }

    #[test]
    fn create_pair_inserts_two_closed_slots_and_emits_audit() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = supervisor.current_eof_cursor().unwrap();

        let snapshots = supervisor.create_pair("foo").unwrap();

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].name, "foo-claude");
        assert_eq!(snapshots[1].name, "foo-codex");
        assert!(snapshots.iter().all(|snapshot| {
            snapshot.lifecycle_state == LifecycleState::Closed && !snapshot.running
        }));

        let routed = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token,
            cursor: Some(cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(EventFilter {
                include_kinds: vec!["pair_created".into()],
                include_sessions: Vec::new(),
                include_scopes: Vec::new(),
            }),
        });
        let (events, _, _, _) = unwrap_events_since(routed);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            RuntimeEvent::PairCreated { name, .. } if name == "foo"
        ));
    }

    #[test]
    fn create_pair_rejects_name_collision() {
        let supervisor = test_supervisor();
        {
            let mut slots = supervisor.inner.slots.lock();
            let working_dir = slots.get("claude").unwrap().definition.working_dir.clone();
            let [foo_claude_definition, _] = pair_session_definitions("foo", &working_dir);
            slots.insert(
                foo_claude_definition.name.clone(),
                closed_session_slot(foo_claude_definition),
            );
        }
        let (foo_claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(
            &supervisor,
            "foo-claude",
            DriverKind::Claude,
            foo_claude_pty,
        );
        let initial_count = supervisor.snapshot().sessions.len();

        let error = supervisor.create_pair("foo").unwrap_err();

        assert!(error.to_string().contains("pair 'foo' already exists"));
        assert_eq!(supervisor.snapshot().sessions.len(), initial_count);
    }

    #[test]
    fn start_session_passes_extra_args_to_spawned_launch_spec() {
        let supervisor = test_supervisor();
        let (pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let snapshot = supervisor
            .start_session("claude", vec!["--resume".into(), "abc-123".into()])
            .unwrap();

        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        let specs = specs.lock();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            &specs[0].args[specs[0].args.len() - 2..],
            &["--resume".to_string(), "abc-123".to_string()]
        );
        assert!(
            supervisor
                .inner
                .slots
                .lock()
                .get("claude")
                .unwrap()
                .definition
                .args
                .is_empty(),
            "registered SessionDefinition must remain pristine"
        );
    }

    #[test]
    fn start_session_without_extra_args_keeps_baseline_launch_spec() {
        let supervisor = test_supervisor();
        let baseline_definition = {
            supervisor
                .inner
                .slots
                .lock()
                .get("claude")
                .unwrap()
                .definition
                .clone()
        };
        let expected = build_launch_spec(&baseline_definition);
        let (pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let snapshot = supervisor.start_session("claude", Vec::new()).unwrap();

        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        let specs = specs.lock();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].program, expected.program);
        assert_eq!(specs[0].args, expected.args);
    }

    #[test]
    fn start_session_rejects_whitespace_only_extra_args() {
        let supervisor = test_supervisor();

        let error = supervisor
            .start_session("claude", vec!["--resume".into(), "   ".into()])
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("extra_args[1] must not be empty or whitespace-only")
        );
    }

    #[test]
    fn sideband_start_session_audit_records_extra_args() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let (pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        let (spawner, _) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let response = supervisor.apply_sideband_request(SidebandRequest::StartSession {
            token: status.token,
            name: "claude".into(),
            extra_args: vec!["--resume".into(), "abc-123".into()],
        });

        assert!(response.ok, "start response failed: {}", response.message);
        let raw = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        let started = raw
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| {
                event["event"] == "sideband_request_lifecycle"
                    && event["action"] == "start_session"
                    && event["phase"] == "started"
            })
            .expect("start_session lifecycle event not found");

        assert_eq!(
            started["extra_args"],
            serde_json::json!(["--resume", "abc-123"])
        );
    }

    #[test]
    fn sideband_start_session_audit_omits_empty_extra_args() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let (pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        let (spawner, _) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let response = supervisor.apply_sideband_request(SidebandRequest::StartSession {
            token: status.token,
            name: "claude".into(),
            extra_args: Vec::new(),
        });

        assert!(response.ok, "start response failed: {}", response.message);
        let raw = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        let started = raw
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .find(|event| {
                event["event"] == "sideband_request_lifecycle"
                    && event["action"] == "start_session"
                    && event["phase"] == "started"
            })
            .expect("start_session lifecycle event not found");

        assert!(started.get("extra_args").is_none());
    }

    #[test]
    fn create_pair_rejects_invalid_names() {
        let supervisor = test_supervisor();
        let invalid_names = vec![
            String::new(),
            "main".to_string(),
            "claude".to_string(),
            "codex".to_string(),
            "room".to_string(),
            "operator".to_string(),
            "with space".to_string(),
            "with/slash".to_string(),
            "with.dot".to_string(),
            "x".repeat(PAIR_NAME_MAX_LEN + 1),
        ];

        for invalid in invalid_names {
            let error = supervisor.create_pair(&invalid).unwrap_err();
            assert!(!error.to_string().is_empty());
            assert_eq!(supervisor.snapshot().sessions.len(), 2);
        }
    }

    #[test]
    fn rename_pair_refuses_when_running() {
        let supervisor = test_supervisor();
        supervisor.create_pair("foo").unwrap();
        let (foo_claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(
            &supervisor,
            "foo-claude",
            DriverKind::Claude,
            foo_claude_pty,
        );

        let error = supervisor.rename_pair("foo", "bar").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("pair 'foo' has running sessions; stop both panes first")
        );
        let snapshot_names = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .map(|session| session.name)
            .collect::<Vec<_>>();
        assert!(snapshot_names.contains(&"foo-claude".to_string()));
        assert!(snapshot_names.contains(&"foo-codex".to_string()));
        assert!(!snapshot_names.contains(&"bar-claude".to_string()));
        assert!(!snapshot_names.contains(&"bar-codex".to_string()));
    }

    #[test]
    fn rename_pair_moves_both_slots_when_stopped() {
        let supervisor = test_supervisor();
        supervisor.create_pair("foo").unwrap();
        let foo_claude_generation = supervisor.bump_session_generation("foo-claude").unwrap();
        let foo_codex_generation = supervisor.bump_session_generation("foo-codex").unwrap();

        let snapshots = supervisor.rename_pair("foo", "bar").unwrap();

        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].name, "bar-claude");
        assert_eq!(snapshots[1].name, "bar-codex");
        assert_eq!(
            supervisor.current_generation("bar-claude"),
            Some(foo_claude_generation)
        );
        assert_eq!(
            supervisor.current_generation("bar-codex"),
            Some(foo_codex_generation)
        );
        assert_eq!(supervisor.current_generation("foo-claude"), None);
        assert_eq!(supervisor.current_generation("foo-codex"), None);
    }

    #[test]
    fn delete_pair_stops_running_panes_and_removes_slots() {
        let supervisor = test_supervisor();
        supervisor.create_pair("foo").unwrap();
        let (foo_claude_pty, _, foo_claude_kill_count) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        let (foo_codex_pty, _, foo_codex_kill_count) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(
            &supervisor,
            "foo-claude",
            DriverKind::Claude,
            foo_claude_pty,
        );
        install_mock_running_session(&supervisor, "foo-codex", DriverKind::Codex, foo_codex_pty);

        supervisor.delete_pair("foo").unwrap();

        assert_eq!(foo_claude_kill_count.load(Ordering::SeqCst), 1);
        assert_eq!(foo_codex_kill_count.load(Ordering::SeqCst), 1);
        let snapshot_names = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .map(|session| session.name)
            .collect::<Vec<_>>();
        assert!(!snapshot_names.contains(&"foo-claude".to_string()));
        assert!(!snapshot_names.contains(&"foo-codex".to_string()));
    }

    #[test]
    fn delete_pair_refuses_main() {
        let supervisor = test_supervisor();

        let error = supervisor.delete_pair("main").unwrap_err();

        assert!(error.to_string().contains("cannot delete the main pair"));
    }

    #[test]
    fn shutdown_kills_and_clears_all_running_sessions() {
        let supervisor = test_supervisor();
        let (claude_pty, _, claude_kill_count) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, _, codex_kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);

        supervisor.shutdown().unwrap();

        assert_eq!(claude_kill_count.load(Ordering::SeqCst), 1);
        assert_eq!(codex_kill_count.load(Ordering::SeqCst), 1);
        let snapshot = supervisor.snapshot();
        for session in snapshot.sessions {
            assert!(
                !session.running,
                "{} must not be running after shutdown",
                session.name
            );
            assert_eq!(session.lifecycle_state, LifecycleState::Closed);
            assert_eq!(session.process_id, None);
        }
    }

    #[test]
    fn shutdown_releases_slots_lock_before_slow_kills() {
        let supervisor = test_supervisor();
        let (slow_pty, _, kill_count) =
            mock_pty_session(None, MockKillBehavior::Sleep(Duration::from_millis(500)));
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, slow_pty);

        let shutdown_supervisor = supervisor.clone();
        let shutdown_thread = thread::spawn(move || shutdown_supervisor.shutdown().unwrap());

        let deadline = Instant::now() + Duration::from_secs(1);
        while kill_count.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);

        let lock_started_at = Instant::now();
        let slots = supervisor.inner.slots.lock();
        let elapsed = lock_started_at.elapsed();
        assert!(
            elapsed < Duration::from_millis(100),
            "slots lock was held across slow kill for {elapsed:?}"
        );
        drop(slots);

        shutdown_thread.join().unwrap();
    }

    #[test]
    fn pair_of_main_pair_panes() {
        assert_eq!(pair_of("claude"), "main");
        assert_eq!(pair_of("codex"), "main");
    }

    #[test]
    fn pair_of_named_pair_panes() {
        assert_eq!(pair_of("FrontendQA-claude"), "FrontendQA");
        assert_eq!(pair_of("FrontendQA-codex"), "FrontendQA");
        assert_eq!(pair_of("foo-bar-claude"), "foo-bar");
        assert_eq!(pair_of("foo_bar-codex"), "foo_bar");
    }

    #[test]
    fn pair_of_singleton_for_non_convention_names() {
        assert_eq!(pair_of("operator"), "operator");
        assert_eq!(pair_of("supervisor"), "supervisor");
    }

    #[test]
    fn pair_of_empty_string_is_singleton() {
        assert_eq!(pair_of(""), "");
    }

    #[test]
    fn pair_of_pathological_inputs_do_not_collapse_to_main() {
        assert_eq!(pair_of("claude-claude"), "claude");
        assert_eq!(pair_of("claude-codex"), "claude");
        assert_eq!(pair_of("-claude"), "-claude");
        assert_eq!(pair_of("-codex"), "-codex");
    }

    #[test]
    fn pair_of_main_pair_name_is_reserved_against_user_pairs() {
        assert_eq!(pair_of("claude"), "main");
        assert_eq!(pair_of("codex"), "main");

        assert_eq!(pair_of("default-claude"), "default");
        assert_eq!(pair_of("default-codex"), "default");
        assert_ne!(pair_of("default-claude"), pair_of("claude"));

        assert!(validate_pair_name("main").is_err());
        assert_eq!(pair_of("main-claude"), "main");
        assert_eq!(pair_of("main-codex"), "main");
    }

    #[test]
    fn room_targets_only_running_sessions() {
        let supervisor = test_supervisor();

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room, Some("claude"));
        assert!(recipients.is_empty());
    }

    #[test]
    fn room_targets_exclude_claude_sender() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "codex");

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room, Some("claude"));

        assert_eq!(recipients, vec!["codex".to_string()]);
    }

    #[test]
    fn room_targets_exclude_codex_sender() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "codex");

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room, Some("codex"));

        assert_eq!(recipients, vec!["claude".to_string()]);
    }

    #[test]
    fn room_targets_isolate_cross_pair_when_flag_off() {
        let supervisor = test_supervisor();
        supervisor.create_pair("FrontendQA").unwrap();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "codex");
        install_stale_running_session(&supervisor, "FrontendQA-claude");
        install_stale_running_session(&supervisor, "FrontendQA-codex");

        let recipients =
            supervisor.resolve_recipients("room", MessageScope::Room, Some("FrontendQA-claude"));

        assert_eq!(recipients, vec!["FrontendQA-codex".to_string()]);
    }

    #[test]
    fn room_targets_main_pair_unaffected_by_isolation() {
        let supervisor = test_supervisor();
        supervisor.create_pair("FrontendQA").unwrap();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "codex");
        install_stale_running_session(&supervisor, "FrontendQA-claude");
        install_stale_running_session(&supervisor, "FrontendQA-codex");

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room, Some("claude"));

        assert_eq!(recipients, vec!["codex".to_string()]);
    }

    #[test]
    fn room_targets_broadcast_all_panes_when_flag_on() {
        let supervisor = test_supervisor_with_cross_pair_room_broadcast(true);
        supervisor.create_pair("FrontendQA").unwrap();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "codex");
        install_stale_running_session(&supervisor, "FrontendQA-claude");
        install_stale_running_session(&supervisor, "FrontendQA-codex");

        let mut recipients =
            supervisor.resolve_recipients("room", MessageScope::Room, Some("FrontendQA-claude"));
        recipients.sort();

        assert_eq!(
            recipients,
            vec![
                "FrontendQA-codex".to_string(),
                "claude".to_string(),
                "codex".to_string(),
            ]
        );
    }

    #[test]
    fn room_targets_sender_none_broadcasts_to_all_running() {
        let supervisor = test_supervisor();
        supervisor.create_pair("FrontendQA").unwrap();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "FrontendQA-codex");

        let mut recipients = supervisor.resolve_recipients("room", MessageScope::Room, None);
        recipients.sort();

        assert_eq!(
            recipients,
            vec!["FrontendQA-codex".to_string(), "claude".to_string()]
        );
    }

    #[test]
    fn direct_targets_ignore_sender_filter() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "claude");

        let recipients =
            supervisor.resolve_recipients("claude", MessageScope::Direct, Some("claude"));

        assert_eq!(recipients, vec!["claude".to_string()]);
    }

    #[test]
    fn direct_targets_cross_pair_unchanged_by_flag() {
        let supervisor = test_supervisor();
        supervisor.create_pair("FrontendQA").unwrap();
        install_stale_running_session(&supervisor, "claude");
        install_stale_running_session(&supervisor, "FrontendQA-codex");

        let recipients =
            supervisor.resolve_recipients("FrontendQA-codex", MessageScope::Direct, Some("claude"));

        assert_eq!(recipients, vec!["FrontendQA-codex".to_string()]);
    }

    #[test]
    fn routed_message_payload_ends_with_terminal_submit() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "operator".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "tell me a joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Claude),
        );

        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from operator]"));
        assert!(payload.contains("tell me a joke"));
    }

    #[test]
    fn codex_payload_is_single_line() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "operator".into(),
                to: "codex".into(),
                scope: MessageScope::Direct,
                content: "tell me\na joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Codex),
        );

        assert!(!payload.contains('\n'));
        assert_eq!(payload, "[Direct message from operator] tell me a joke");
    }

    #[test]
    fn claude_payload_stays_multiline_even_with_delayed_submit() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "operator".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "tell me\na joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Claude),
        );

        assert!(payload.starts_with('\n'));
        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from operator]"));
        assert!(payload.contains("tell me\na joke"));
    }

    #[test]
    fn generic_terminal_payload_uses_multiline_prompt_shape() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "operator".into(),
                to: "terminal".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            },
            routed_message_submit_behavior(DriverKind::GenericTerminal),
        );

        assert!(payload.starts_with('\n'));
        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from operator]"));
        assert!(payload.contains("hello"));
    }

    #[test]
    fn collapse_inline_content_reduces_whitespace() {
        assert_eq!(
            collapse_inline_content("  tell   me \n a\tjoke  "),
            "tell me a joke"
        );
    }

    #[test]
    fn prepare_direct_message_claude_single_line_preserves_content() {
        assert_eq!(
            prepare_direct_message(DriverKind::Claude, "tell me a joke"),
            vec!["tell me a joke".to_string()]
        );
    }

    #[test]
    fn prepare_direct_message_claude_multiline_no_chunking_under_limit() {
        assert_eq!(
            prepare_direct_message(DriverKind::Claude, "tell me\na joke"),
            vec!["tell me\na joke".to_string()]
        );
    }

    #[test]
    fn prepare_direct_message_codex_flattens_newlines_to_spaces() {
        assert_eq!(
            prepare_direct_message(DriverKind::Codex, "tell me\na joke"),
            vec!["tell me a joke".to_string()]
        );
    }

    #[test]
    fn prepare_direct_message_codex_flattens_carriage_returns() {
        assert_eq!(
            prepare_direct_message(DriverKind::Codex, "tell\rme\r\na joke"),
            vec!["tell me a joke".to_string()]
        );
    }

    #[test]
    fn long_claude_payloads_are_chunked_with_part_headers() {
        let long_content = (0..120)
            .map(|index| format!("segment-{index:03}"))
            .collect::<Vec<_>>()
            .join(" ");
        let payloads = routed_message_payloads(
            &RouteMessageRequest {
                from: "codex".into(),
                to: "claude".into(),
                scope: MessageScope::Room,
                content: long_content.clone(),
            },
            routed_message_submit_behavior(DriverKind::Claude),
        );

        assert!(payloads.len() > 1);
        assert!(payloads[0].contains("[Room message from codex | part 1/"));
        assert!(payloads.last().unwrap().contains("| part "));
        let reassembled = payloads
            .iter()
            .map(|payload| {
                payload
                    .lines()
                    .skip(2)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(collapse_inline_content(&reassembled), long_content);
    }

    #[test]
    fn long_codex_payloads_are_chunked_with_part_headers() {
        let long_content = (0..120)
            .map(|index| format!("segment-{index:03}"))
            .collect::<Vec<_>>()
            .join(" ");
        let payloads = routed_message_payloads(
            &RouteMessageRequest {
                from: "codex".into(),
                to: "codex".into(),
                scope: MessageScope::Room,
                content: long_content.clone(),
            },
            routed_message_submit_behavior(DriverKind::Codex),
        );

        assert!(payloads.len() > 1);
        assert!(payloads[0].starts_with("[Room message from codex | part 1/"));
        assert!(payloads.last().unwrap().contains("| part "));
        assert!(payloads.iter().all(|payload| !payload.contains('\n')));
        let reassembled = payloads
            .iter()
            .map(|payload| {
                payload
                    .split_once("] ")
                    .map(|(_, content)| content)
                    .unwrap_or("")
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(collapse_inline_content(&reassembled), long_content);
    }

    #[test]
    fn codex_payload_keeps_flattened_provenance_marker() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "claude".into(),
                to: "codex".into(),
                scope: MessageScope::Room,
                content: "status update".into(),
            },
            routed_message_submit_behavior(DriverKind::Codex),
        );

        assert_eq!(payload, "[Room message from claude] status update");
    }

    #[test]
    fn control_key_sequences_match_terminal_expectations() {
        assert_eq!(control_key_sequence(ControlKey::Enter), "\r");
        assert_eq!(control_key_sequence(ControlKey::Up), "\x1b[A");
        assert_eq!(control_key_sequence(ControlKey::Down), "\x1b[B");
        assert_eq!(control_key_sequence(ControlKey::Left), "\x1b[D");
        assert_eq!(control_key_sequence(ControlKey::Right), "\x1b[C");
        assert_eq!(control_key_sequence(ControlKey::Tab), "\t");
        assert_eq!(control_key_sequence(ControlKey::Esc), "\x1b");
        assert_eq!(control_key_sequence(ControlKey::CtrlC), "\x03");
    }

    #[test]
    fn classifier_ignores_pure_ansi_escape() {
        assert!(!chunk_has_real_content("\x1b[2K\x1b[1G"));
    }

    #[test]
    fn classifier_ignores_title_bar_update() {
        assert!(!chunk_has_real_content("\x1b]0;claude\x07"));
    }

    #[test]
    fn classifier_ignores_braille_spinner_char() {
        assert!(!chunk_has_real_content("\x1b[?25l⠙\x1b[?25h"));
    }

    #[test]
    fn classifier_treats_multichar_text_as_real_content() {
        assert!(chunk_has_real_content("Working"));
    }

    #[test]
    fn routed_message_submit_behavior_is_driver_aware() {
        assert_eq!(
            routed_message_submit_behavior(DriverKind::Claude),
            SubmitBehavior {
                sequence: "\r",
                delay: Duration::from_millis(200),
                flatten_payload: false,
                max_chunk_chars: Some(CLAUDE_ROUTED_MESSAGE_MAX_CHARS),
            }
        );
        assert_eq!(
            routed_message_submit_behavior(DriverKind::Codex),
            SubmitBehavior {
                sequence: "\r",
                delay: Duration::from_millis(500),
                flatten_payload: true,
                max_chunk_chars: Some(CODEX_ROUTED_MESSAGE_MAX_CHARS),
            }
        );
    }

    #[test]
    fn start_control_plane_persists_status_file_and_snapshot() {
        let supervisor = test_supervisor();

        let status = supervisor.start_control_plane().unwrap();
        let persisted: ControlPlaneStatus = serde_json::from_str(
            &fs::read_to_string(supervisor.runtime_dir().join("control-plane.json")).unwrap(),
        )
        .unwrap();
        let snapshot = supervisor.snapshot();

        assert_eq!(persisted.endpoint, status.endpoint);
        assert_eq!(persisted.token, status.token);
        assert_eq!(
            snapshot
                .control_plane
                .as_ref()
                .map(|item| item.endpoint.as_str()),
            Some(status.endpoint.as_str())
        );
    }

    #[test]
    fn start_control_plane_is_idempotent() {
        let supervisor = test_supervisor();

        let first = supervisor.start_control_plane().unwrap();
        let second = supervisor.start_control_plane().unwrap();

        assert_eq!(first.endpoint, second.endpoint);
        assert_eq!(first.token, second.token);
    }

    fn wait_for_live_probe(status: &ControlPlaneStatus) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if probe_control_plane_owner(status, Duration::from_millis(250)).unwrap() {
                return;
            }

            assert!(
                Instant::now() < deadline,
                "timed out waiting for control-plane probe response at {}",
                status.endpoint
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn start_control_plane_refuses_to_overwrite_live_owner() {
        let root = std::env::temp_dir().join(format!(
            "cli-master-wrapper-live-owner-test-{}",
            Uuid::new_v4()
        ));
        let first = test_supervisor_with_root(root.clone());
        let first_status = first.start_control_plane().unwrap();
        wait_for_live_probe(&first_status);
        let control_plane_path = first.runtime_dir().join("control-plane.json");
        let before = fs::read_to_string(&control_plane_path).unwrap();

        let second = test_supervisor_with_root(root);
        let error = second.start_control_plane().unwrap_err();

        assert!(
            error
                .to_string()
                .contains("another wrapper instance is already holding the control plane"),
            "unexpected error: {error}"
        );
        assert_eq!(fs::read_to_string(&control_plane_path).unwrap(), before);
    }

    #[test]
    fn start_control_plane_overwrites_dead_owner_file() {
        let root = std::env::temp_dir().join(format!(
            "cli-master-wrapper-dead-owner-test-{}",
            Uuid::new_v4()
        ));
        let supervisor = test_supervisor_with_root(root);
        let info_path = supervisor.runtime_dir().join("control-plane.json");
        fs::create_dir_all(supervisor.runtime_dir()).unwrap();
        let stale = ControlPlaneStatus {
            transport: control_plane_transport().into(),
            endpoint: format!("{DEFAULT_ENDPOINT}-dead-{}", Uuid::new_v4()),
            token: "stale-token".into(),
            info_path: info_path.display().to_string(),
        };
        fs::write(&info_path, serde_json::to_string_pretty(&stale).unwrap()).unwrap();

        let status = supervisor.start_control_plane().unwrap();
        let persisted: ControlPlaneStatus =
            serde_json::from_str(&fs::read_to_string(&info_path).unwrap()).unwrap();

        assert_eq!(persisted.endpoint, status.endpoint);
        assert_eq!(persisted.token, status.token);
        assert_ne!(persisted.endpoint, stale.endpoint);
        assert_ne!(persisted.token, stale.token);
    }

    #[test]
    fn mailbox_request_processing_writes_response_and_archives_request() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let sideband_dir = supervisor.runtime_dir().join("sideband-test-success");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&outbox_dir).unwrap();
        fs::create_dir_all(&processed_dir).unwrap();

        let request_path = inbox_dir.join("ping.json");
        fs::write(
            &request_path,
            encode_request(&SidebandRequest::Ping {
                token: status.token.clone(),
            })
            .unwrap(),
        )
        .unwrap();

        process_sideband_mailbox_file(&supervisor, &request_path, &outbox_dir, &processed_dir)
            .unwrap();

        let response =
            decode_response(&fs::read_to_string(outbox_dir.join("ping.json")).unwrap()).unwrap();
        assert!(response.ok);
        assert_eq!(response.message, "pong");
        assert!(processed_dir.join("ping.json").exists());
        assert!(!request_path.exists());
    }

    #[test]
    fn mailbox_request_processing_returns_error_for_invalid_payload() {
        let supervisor = test_supervisor();
        let sideband_dir = supervisor.runtime_dir().join("sideband-test-invalid");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&outbox_dir).unwrap();
        fs::create_dir_all(&processed_dir).unwrap();

        let request_path = inbox_dir.join("invalid.json");
        fs::write(&request_path, "{not json}").unwrap();
        thread::sleep(Duration::from_millis(600));

        process_sideband_mailbox_file(&supervisor, &request_path, &outbox_dir, &processed_dir)
            .unwrap();

        let response =
            decode_response(&fs::read_to_string(outbox_dir.join("invalid.json")).unwrap()).unwrap();
        assert!(!response.ok);
        assert!(response.message.contains("invalid sideband payload"));
    }

    #[test]
    fn mailbox_request_processing_accepts_bom_prefixed_payload() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let sideband_dir = supervisor.runtime_dir().join("sideband-test-bom");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&outbox_dir).unwrap();
        fs::create_dir_all(&processed_dir).unwrap();

        let request_path = inbox_dir.join("ping.json");
        let raw = format!(
            "\u{feff}{}",
            encode_request(&SidebandRequest::Ping {
                token: status.token.clone(),
            })
            .unwrap()
        );
        fs::write(&request_path, raw).unwrap();

        process_sideband_mailbox_file(&supervisor, &request_path, &outbox_dir, &processed_dir)
            .unwrap();

        let response =
            decode_response(&fs::read_to_string(outbox_dir.join("ping.json")).unwrap()).unwrap();
        assert!(response.ok);
        assert_eq!(response.message, "pong");
    }

    #[test]
    fn apply_sideband_request_rejects_invalid_token() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::Ping {
            token: format!("{}-wrong", status.token),
        });

        assert!(!response.ok);
        assert_eq!(response.message, "invalid control plane token");
    }

    #[test]
    fn failed_sideband_lifecycle_event_carries_error_message() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        let response = supervisor.apply_sideband_request(SidebandRequest::Ping {
            token: format!("{}-wrong", status.token),
        });

        assert!(!response.ok);
        let request_id = response.request_id.as_deref().unwrap();
        let failed = events
            .lock()
            .iter()
            .find_map(|event| match event {
                RuntimeEvent::SidebandRequestLifecycle {
                    request_id: event_request_id,
                    phase: SidebandPhase::Failed,
                    error,
                    ..
                } => Some((event_request_id.clone(), error.clone())),
                _ => None,
            })
            .expect("failed lifecycle event");

        assert_eq!(failed.0, request_id);
        assert_eq!(failed.1.as_deref(), Some("invalid control plane token"));
    }

    #[test]
    fn pane_bound_send_input_rejects_peer_target_when_lockdown_is_on() {
        let supervisor = test_supervisor_with_peer_slash_commands_allowed(false);
        let claude_token = session_token(&supervisor, "claude");

        let error = supervisor
            .validate_session_action_token(&claude_token, "codex")
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "peer slash commands are disabled by this wrapper's policy (PRIM1_PEER_SLASH_COMMANDS_ALLOWED=0)"
        );
    }

    #[test]
    fn pane_bound_send_input_allows_same_session_when_lockdown_is_on() {
        let supervisor = test_supervisor_with_peer_slash_commands_allowed(false);
        let claude_token = session_token(&supervisor, "claude");

        supervisor
            .validate_session_action_token(&claude_token, "claude")
            .unwrap();
    }

    #[test]
    fn master_token_bypasses_peer_lockdown() {
        let supervisor = test_supervisor_with_peer_slash_commands_allowed(false);
        let status = supervisor.start_control_plane().unwrap();

        supervisor
            .validate_session_action_token(&status.token, "codex")
            .unwrap();
    }

    #[test]
    fn create_pair_request_inserts_closed_slots() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::CreatePair {
            token: status.token.clone(),
            name: "frontend-qa".into(),
        });

        assert!(response.ok, "response: {:?}", response);
        let snapshot = response.snapshot.expect("snapshot present");
        let by_name: HashMap<&str, &SessionSnapshot> = snapshot
            .sessions
            .iter()
            .map(|session| (session.name.as_str(), session))
            .collect();

        let new_claude = by_name
            .get("frontend-qa-claude")
            .expect("frontend-qa-claude slot inserted");
        let new_codex = by_name
            .get("frontend-qa-codex")
            .expect("frontend-qa-codex slot inserted");

        assert_eq!(new_claude.lifecycle_state, LifecycleState::Closed);
        assert!(!new_claude.running);
        assert_eq!(new_codex.lifecycle_state, LifecycleState::Closed);
        assert!(!new_codex.running);
        assert!(by_name.contains_key("claude"));
        assert!(by_name.contains_key("codex"));
    }

    #[test]
    fn create_pair_rejects_pane_bound_token() {
        let supervisor = test_supervisor();
        let _status = supervisor.start_control_plane().unwrap();
        let claude_token = session_token(&supervisor, "claude");

        let response = supervisor.apply_sideband_request(SidebandRequest::CreatePair {
            token: claude_token,
            name: "frontend-qa".into(),
        });

        assert!(!response.ok);
        assert!(
            response.message.contains("master token required"),
            "unexpected message: {}",
            response.message
        );
    }

    #[test]
    fn create_pair_rejects_invalid_token() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::CreatePair {
            token: format!("{}-wrong", status.token),
            name: "frontend-qa".into(),
        });

        assert!(!response.ok);
        assert_eq!(response.message, "invalid control plane token");
    }

    #[test]
    fn create_pair_rejects_reserved_name() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::CreatePair {
            token: status.token,
            name: "main".into(),
        });

        assert!(!response.ok);
        assert!(
            response.message.to_lowercase().contains("reserved")
                || response.message.to_lowercase().contains("name"),
            "unexpected message: {}",
            response.message
        );
    }

    #[test]
    fn deliver_message_rejects_slash_command() {
        let supervisor = test_supervisor();

        let error = supervisor
            .deliver_message(DeliverMessageRequest {
                name: "claude".into(),
                content: "/compact".into(),
            })
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "deliver_message: slash commands are not supported; use send_input"
        );
    }

    #[test]
    fn deliver_message_rejects_slash_command_with_leading_whitespace() {
        let supervisor = test_supervisor();

        let error = supervisor
            .deliver_message(DeliverMessageRequest {
                name: "claude".into(),
                content: "  /compact".into(),
            })
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            "deliver_message: slash commands are not supported; use send_input"
        );
    }

    #[test]
    fn deliver_message_pane_bound_token_rejects_other_session() {
        let supervisor = test_supervisor();
        let claude_token = session_token(&supervisor, "claude");

        let response = supervisor.apply_sideband_request(SidebandRequest::DeliverMessage {
            token: claude_token,
            name: "codex".into(),
            content: "status update".into(),
            require_idle: false,
        });

        assert!(!response.ok);
        assert_eq!(
            response.message,
            "deliver_message: pane-bound token cannot target other sessions; use route_message instead"
        );
    }

    #[test]
    fn deliver_message_pane_bound_token_accepts_own_session() {
        let supervisor = test_supervisor();
        let claude_token = session_token(&supervisor, "claude");
        install_stale_running_session(&supervisor, "claude");

        supervisor
            .validate_deliver_message_token(&claude_token, "claude")
            .unwrap();
    }

    #[test]
    fn deliver_message_master_token_accepts_any_session() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        install_stale_running_session(&supervisor, "codex");

        supervisor
            .validate_deliver_message_token(&status.token, "codex")
            .unwrap();
    }

    #[test]
    fn pane_signal_pane_bound_token_records_bound_session_and_files() {
        let supervisor = test_supervisor();
        let _status = supervisor.start_control_plane().unwrap();
        let codex_token = session_token(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);

        let response = supervisor.apply_sideband_request(SidebandRequest::PaneSignal {
            token: codex_token,
            task_id: "task-418".into(),
            signal_type: PaneSignalType::Done,
            summary: "signal complete".into(),
            artifact_paths: vec!["evidence/task-418.md".into()],
            commit_sha: Some("abc123".into()),
        });

        assert!(response.ok, "response: {:?}", response);
        let request_id = response.request_id.clone().expect("request id");
        let (signal_path, legacy_touch_path) = match response.payload {
            Some(SidebandResponsePayload::PaneSignal {
                signal_path,
                legacy_touch_path,
            }) => (PathBuf::from(signal_path), PathBuf::from(legacy_touch_path)),
            other => panic!("unexpected response payload: {other:?}"),
        };
        assert!(signal_path.exists(), "missing {}", signal_path.display());
        assert!(
            signal_path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("task-418__done__")
        );
        assert!(
            legacy_touch_path.exists(),
            "missing {}",
            legacy_touch_path.display()
        );
        assert_eq!(
            legacy_touch_path.file_name().unwrap().to_string_lossy(),
            "task-418.done"
        );

        let signals = pane_signal_events(&events);
        assert_eq!(signals.len(), 1);
        match &signals[0] {
            RuntimeEvent::PaneSignal {
                request_id: event_request_id,
                session,
                task_id,
                signal_type,
                summary,
                artifact_paths,
                commit_sha,
                ..
            } => {
                assert_eq!(event_request_id, &request_id);
                assert_eq!(session, "codex");
                assert_eq!(task_id, "task-418");
                assert_eq!(*signal_type, PaneSignalType::Done);
                assert_eq!(summary, "signal complete");
                assert_eq!(artifact_paths, &vec!["evidence/task-418.md".to_string()]);
                assert_eq!(commit_sha.as_deref(), Some("abc123"));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let persisted: RuntimeEvent =
            serde_json::from_str(&fs::read_to_string(signal_path).unwrap()).unwrap();
        assert_eq!(persisted, signals[0]);
    }

    #[test]
    fn pane_signal_master_token_records_supervisor_session() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);

        let response = supervisor.apply_sideband_request(SidebandRequest::PaneSignal {
            token: status.token,
            task_id: "smoke".into(),
            signal_type: PaneSignalType::Heartbeat,
            summary: "still alive".into(),
            artifact_paths: Vec::new(),
            commit_sha: None,
        });

        assert!(response.ok, "response: {:?}", response);
        let signals = pane_signal_events(&events);
        assert_eq!(signals.len(), 1);
        assert!(matches!(
            &signals[0],
            RuntimeEvent::PaneSignal {
                session,
                signal_type: PaneSignalType::Heartbeat,
                ..
            } if session == "supervisor"
        ));
    }

    #[test]
    fn pane_signal_rejects_invalid_token() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::PaneSignal {
            token: format!("{}-wrong", status.token),
            task_id: "task-418".into(),
            signal_type: PaneSignalType::Blocked,
            summary: "blocked".into(),
            artifact_paths: Vec::new(),
            commit_sha: None,
        });

        assert!(!response.ok);
        assert_eq!(response.message, "invalid control plane token");
    }

    #[test]
    fn deliver_message_claude_delivers_multi_line_intact() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "claude", DriverKind::Claude);

        let (snapshot, submit_behavior, payloads) = supervisor
            .prepare_delivery_for_session("claude", "line one\nline two")
            .unwrap();

        assert_eq!(snapshot.name, "claude");
        assert_eq!(
            submit_behavior,
            routed_message_submit_behavior(DriverKind::Claude)
        );
        assert_eq!(payloads, vec!["line one\nline two".to_string()]);
    }

    #[test]
    fn deliver_message_codex_delivers_flattened_single_line() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);

        let (snapshot, submit_behavior, payloads) = supervisor
            .prepare_delivery_for_session("codex", "line one\nline two")
            .unwrap();

        assert_eq!(snapshot.name, "codex");
        assert_eq!(
            submit_behavior,
            routed_message_submit_behavior(DriverKind::Codex)
        );
        assert_eq!(payloads, vec!["line one line two".to_string()]);
    }

    #[test]
    fn deliver_message_codex_chunks_large_flattened_content() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let long_content = (0..120)
            .map(|index| format!("segment-{index:03}"))
            .collect::<Vec<_>>()
            .join(" ");

        let (_, submit_behavior, payloads) = supervisor
            .prepare_delivery_for_session("codex", &long_content)
            .unwrap();

        assert_eq!(
            submit_behavior,
            routed_message_submit_behavior(DriverKind::Codex)
        );
        assert!(payloads.len() > 1);
        assert!(
            payloads
                .iter()
                .all(|payload| payload.chars().count() <= CODEX_ROUTED_MESSAGE_MAX_CHARS)
        );
        assert_eq!(payloads.join(" "), long_content);
    }

    #[test]
    fn events_since_returns_emitted_events() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = supervisor.current_eof_cursor().unwrap();

        supervisor.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: "first".into(),
            timestamp: now_rfc3339(),
        });
        supervisor.emit(RuntimeEvent::SessionState {
            session: "claude".into(),
            state: LifecycleState::Ready,
            reason: "ready".into(),
            timestamp: now_rfc3339(),
        });

        let response = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token,
            cursor: Some(cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(EventFilter {
                include_kinds: vec!["system_log".into(), "session_state".into()],
                include_sessions: Vec::new(),
                include_scopes: Vec::new(),
            }),
        });

        let (events, next_cursor, gap_detected, _) = unwrap_events_since(response);

        assert_eq!(events.len(), 2);
        assert_eq!(event_kind(&events[0]), "system_log");
        assert_eq!(event_kind(&events[1]), "session_state");
        assert_eq!(next_cursor.audit_file, current_audit_file(&supervisor));
        assert!(next_cursor.byte_offset > 0);
        assert!(!gap_detected);
    }

    #[test]
    fn events_filter_accepts_script_default_signal_kinds() {
        let supervisor = test_supervisor();
        let filter = EventFilter {
            include_kinds: vec![
                "routed_message".into(),
                "route_delivery".into(),
                "dispatch_attempt".into(),
                "pane_signal".into(),
                "session_state".into(),
                "session_exit".into(),
                "session_work_state".into(),
                "system_log".into(),
                "sideband_request_lifecycle".into(),
                "request_ack".into(),
                "request_ack_timeout".into(),
            ],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };

        supervisor.validate_events_filter(&filter).unwrap();
    }

    #[test]
    fn events_since_respects_max_events() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = supervisor.current_eof_cursor().unwrap();

        for index in 0..25 {
            supervisor.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: format!("event-{index}"),
                timestamp: now_rfc3339(),
            });
        }

        let filter = EventFilter {
            include_kinds: vec!["system_log".into()],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };

        let first = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token.clone(),
            cursor: Some(cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(filter.clone()),
        });
        let (first_events, next_cursor, _, _) = unwrap_events_since(first);
        assert_eq!(first_events.len(), 10);

        let second = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token,
            cursor: Some(next_cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(filter),
        });
        let (second_events, _, _, _) = unwrap_events_since(second);
        assert_eq!(second_events.len(), 10);
    }

    #[test]
    fn events_since_long_poll_wakes_on_emit() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = supervisor.current_eof_cursor().unwrap();
        let filter = EventFilter {
            include_kinds: vec!["system_log".into()],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };
        let handle = supervisor.clone();
        let (tx, rx) = std::sync::mpsc::channel();

        thread::spawn(move || {
            let started = Instant::now();
            let response = handle.apply_sideband_request(SidebandRequest::EventsSince {
                token: status.token,
                cursor: Some(cursor),
                max_events: Some(10),
                max_wait_seconds: Some(5),
                filter: Some(filter),
            });
            tx.send((started.elapsed(), response)).unwrap();
        });

        thread::sleep(Duration::from_millis(1000));
        supervisor.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: "wake".into(),
            timestamp: now_rfc3339(),
        });

        let (elapsed, response) = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let (events, _, _, _) = unwrap_events_since(response);

        assert!(elapsed >= Duration::from_millis(900));
        assert!(elapsed < Duration::from_secs(3));
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            RuntimeEvent::SystemLog { message, .. } if message == "wake"
        ));
    }

    #[test]
    fn events_since_handles_cross_restart_file_advance() {
        let supervisor = test_supervisor();
        let active_file = current_audit_file(&supervisor);
        let prior_file = if active_file == "2026-04-18.jsonl" {
            "2026-04-17.jsonl".to_string()
        } else {
            "2026-04-18.jsonl".to_string()
        };
        let filter = EventFilter {
            include_kinds: vec!["system_log".into()],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };

        append_audit_event(
            &supervisor,
            &prior_file,
            &RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: "old-1".into(),
                timestamp: now_rfc3339(),
            },
        );
        let old_path = append_audit_event(
            &supervisor,
            &prior_file,
            &RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: "old-2".into(),
                timestamp: now_rfc3339(),
            },
        );
        append_audit_event(
            &supervisor,
            &active_file,
            &RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: "new-1".into(),
                timestamp: now_rfc3339(),
            },
        );

        let full = supervisor
            .read_events_since(
                Some(EventCursor {
                    audit_file: prior_file.clone(),
                    byte_offset: 0,
                }),
                &filter,
                10,
            )
            .unwrap();

        let messages = full
            .events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SystemLog { message, .. } => Some(message.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(messages, vec!["old-1", "old-2", "new-1"]);
        assert_eq!(full.next_cursor.audit_file, active_file);

        let limited = supervisor
            .read_events_since(
                Some(EventCursor {
                    audit_file: prior_file.clone(),
                    byte_offset: 0,
                }),
                &filter,
                1,
            )
            .unwrap();

        assert_eq!(limited.events.len(), 1);
        assert_eq!(limited.next_cursor.audit_file, prior_file);
        assert!(limited.next_cursor.byte_offset < fs::metadata(old_path).unwrap().len());
    }

    #[test]
    fn events_since_gap_detected() {
        let supervisor = test_supervisor();
        let active_file = current_audit_file(&supervisor);
        let filter = EventFilter {
            include_kinds: vec!["system_log".into()],
            include_sessions: Vec::new(),
            include_scopes: Vec::new(),
        };

        append_audit_event(
            &supervisor,
            &active_file,
            &RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: "current".into(),
                timestamp: now_rfc3339(),
            },
        );

        let result = supervisor
            .read_events_since(
                Some(EventCursor {
                    audit_file: "2026-04-01.jsonl".into(),
                    byte_offset: 0,
                }),
                &filter,
                10,
            )
            .unwrap();

        assert!(result.gap_detected);
        assert_eq!(result.next_cursor.audit_file, active_file);
        assert!(matches!(
            &result.events[0],
            RuntimeEvent::SystemLog { message, .. } if message == "current"
        ));
    }

    #[test]
    fn events_since_error_payload_echoes_cursor() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = EventCursor {
            audit_file: current_audit_file(&supervisor),
            byte_offset: 0,
        };

        let response = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token,
            cursor: Some(cursor.clone()),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(EventFilter {
                include_kinds: vec!["bogus".into()],
                include_sessions: Vec::new(),
                include_scopes: Vec::new(),
            }),
        });

        assert!(!response.ok);
        match response.payload {
            Some(SidebandResponsePayload::EventsSinceError { echoed_cursor }) => {
                assert_eq!(echoed_cursor, serde_json::to_value(cursor).unwrap());
            }
            other => panic!("unexpected error payload: {other:?}"),
        }
    }

    #[test]
    fn idle_emitted_after_quiesce_threshold() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working".into()));

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if events.lock().iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::SessionState { session, state, .. }
                        if session == "codex" && *state == LifecycleState::Idle
                )
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "idle event was not emitted in time"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn idle_cancelled_on_new_output() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working".into()));
        thread::sleep(Duration::from_millis(1200));
        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Still working".into()));
        thread::sleep(Duration::from_millis(1300));

        assert!(
            !events.lock().iter().any(|event| matches!(
                event,
                RuntimeEvent::SessionState { session, state, .. }
                    if session == "codex" && *state == LifecycleState::Idle
            )),
            "idle fired before the reset timer elapsed"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if events.lock().iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::SessionState { session, state, .. }
                        if session == "codex" && *state == LifecycleState::Idle
                )
            }) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "idle event was not emitted after reset"
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn idle_suppressed_on_stale_generation() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working".into()));
        let armed_at = {
            let slots = supervisor.inner.slots.lock();
            slots.get("codex").unwrap().last_real_output_at.unwrap()
        };
        supervisor.bump_session_generation("codex").unwrap();
        supervisor.fire_quiesce_timer("codex".into(), 0, armed_at, Duration::from_secs(2));
        thread::sleep(Duration::from_millis(50));

        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionState { session, state, .. }
                if session == "codex" && *state == LifecycleState::Idle
        )));
        assert!(events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SystemLog { message, .. }
                if message.contains("Dropped stale quiesce timer")
        )));
    }

    #[test]
    fn work_state_transitions_idle_thinking_idle() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working 12s".into()));
        let armed_at = {
            let slots = supervisor.inner.slots.lock();
            slots.get("codex").unwrap().last_real_output_at.unwrap()
        };
        supervisor.fire_quiesce_timer("codex".into(), 0, armed_at, Duration::from_secs(2));

        let work_events = work_state_events(&events);
        assert_eq!(work_events.len(), 2);
        match &work_events[0] {
            RuntimeEvent::SessionWorkState {
                session,
                state,
                previous_state,
                detail,
                ..
            } => {
                assert_eq!(session, "codex");
                assert_eq!(*state, WorkState::Thinking);
                assert_eq!(*previous_state, Some(WorkState::Idle));
                assert_eq!(detail.as_deref(), Some("Working 12s"));
            }
            other => panic!("unexpected event: {other:?}"),
        }
        match &work_events[1] {
            RuntimeEvent::SessionWorkState {
                session,
                state,
                previous_state,
                detail,
                ..
            } => {
                assert_eq!(session, "codex");
                assert_eq!(*state, WorkState::Idle);
                assert_eq!(*previous_state, Some(WorkState::Thinking));
                assert_eq!(detail, &None);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn work_state_identical_thinking_chunks_emit_once() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = capture_runtime_events(&supervisor);

        for idx in 0..5 {
            supervisor.handle_pty_event("codex", 0, PtyEvent::Output(format!("Working {idx}s")));
        }

        let work_events = work_state_events(&events);
        assert_eq!(work_events.len(), 1);
        assert!(matches!(
            &work_events[0],
            RuntimeEvent::SessionWorkState {
                state: WorkState::Thinking,
                ..
            }
        ));
    }

    #[test]
    fn work_state_repeated_blocked_hint_escalates_to_error_loop() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let events = capture_runtime_events(&supervisor);

        for _ in 0..3 {
            supervisor.handle_pty_event(
                "codex",
                0,
                PtyEvent::Output("You've hit your usage limit.".into()),
            );
        }

        let work_events = work_state_events(&events);
        assert_eq!(work_events.len(), 2);
        assert!(matches!(
            &work_events[0],
            RuntimeEvent::SessionWorkState {
                state: WorkState::Blocked,
                detail: Some(detail),
                previous_state: Some(WorkState::Idle),
                ..
            } if detail == "usage_limit"
        ));
        assert!(matches!(
            &work_events[1],
            RuntimeEvent::SessionWorkState {
                state: WorkState::ErrorLoop,
                detail: Some(detail),
                previous_state: Some(WorkState::Blocked),
                ..
            } if detail == "usage_limit"
        ));
    }

    #[test]
    fn terminal_work_state_suppresses_quiesce_and_blocks_require_idle_dispatch() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("PS C:\\Projects\\PRIM-1>".into()),
        );

        {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get("codex").unwrap();
            assert_eq!(slot.work_state, WorkState::Exited);
            assert!(slot.work_state_observed);
            assert!(slot.quiesce_timer.is_none());
        }
        assert!(work_state_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState {
                session,
                state: WorkState::Exited,
                detail: Some(detail),
                ..
            } if session == "codex" && detail == "shell_prompt"
        )));
        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionState {
                session,
                state: LifecycleState::Idle,
                ..
            } if session == "codex"
        )));

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: true,
        });

        assert!(!response.ok);
        assert_eq!(response.message, "dispatch aborted: target target_exited");
    }

    #[test]
    fn supervisor_heartbeat_fires_at_configured_interval_with_session_summary() {
        let supervisor =
            test_supervisor_with_wrapper_defaults(Some(Duration::from_millis(25)), None, None);
        let events = capture_runtime_events(&supervisor);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.process_id = Some(4242);
            slot.last_activity_at = Some("2026-05-18T00:00:00Z".into());
        }

        wait_for_event_count(&events, 1, |event| {
            matches!(event, RuntimeEvent::SupervisorHeartbeat { .. })
        });

        let heartbeats = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SupervisorHeartbeat {
                    wrapper_pid,
                    sessions,
                    ..
                } => Some((*wrapper_pid, sessions.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert!(!heartbeats.is_empty());
        assert_eq!(heartbeats[0].0, std::process::id());
        assert!(heartbeats[0].1.iter().any(|summary| {
            summary.name == "codex"
                && summary.lifecycle_state == LifecycleState::Ready
                && summary.work_state == Some(WorkState::Thinking)
                && summary.process_id == Some(4242)
                && summary.last_activity_at.as_deref() == Some("2026-05-18T00:00:00Z")
        }));
    }

    #[test]
    fn request_ack_timeout_co_emits_supervisor_alert() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Blocked),
        );
        let context = RequestAckContext {
            request_id: "req-timeout".into(),
            action: "deliver_message".into(),
        };

        supervisor.emit_request_ack_timeout(&context, "codex", Duration::from_secs(60));

        assert!(events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::RequestAckTimeout {
                request_id,
                session,
                action,
                elapsed_ms,
                ..
            } if request_id == "req-timeout"
                && session == "codex"
                && action == "deliver_message"
                && *elapsed_ms == 60000
        )));
        let alerts = supervisor_alert_events(&events);
        assert_eq!(alerts.len(), 1);
        assert!(matches!(
            &alerts[0],
            RuntimeEvent::SupervisorAlert {
                alert_type: SupervisorAlertType::AckTimeout,
                request_id: Some(request_id),
                session: Some(session),
                action: Some(action),
                last_work_state: Some(WorkState::Blocked),
                last_session_state: Some(LifecycleState::Ready),
                severity: AlertSeverity::Warn,
                message,
                ..
            } if request_id == "req-timeout"
                && session == "codex"
                && action == "deliver_message"
                && message.contains("didn't ACK in 60s")
        ));
    }

    #[test]
    fn deliver_message_with_completion_signal_text_emits_no_template_warning() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);

        let snapshot = supervisor
            .deliver_message(DeliverMessageRequest {
                name: "codex".into(),
                content: "Task must call pane_signal with task_id task-410 when done.".into(),
            })
            .unwrap();

        assert_eq!(snapshot.name, "codex");
        assert!(dispatch_template_warning_events(&events).is_empty());
    }

    #[test]
    fn deliver_message_without_completion_signal_text_emits_template_warning() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);

        supervisor
            .deliver_message(DeliverMessageRequest {
                name: "codex".into(),
                content: "Please inspect the repo and report back.".into(),
            })
            .unwrap();

        let warnings = dispatch_template_warning_events(&events);
        assert_eq!(warnings.len(), 1);
        let expected_missing = DISPATCH_TEMPLATE_PATTERNS
            .iter()
            .map(|pattern| pattern.to_string())
            .collect::<Vec<_>>();
        match &warnings[0] {
            RuntimeEvent::DispatchTemplateWarning {
                session,
                detected_patterns,
                missing_patterns,
                severity: AlertSeverity::Info,
                ..
            } => {
                assert_eq!(session, "codex");
                assert!(detected_patterns.is_empty());
                assert_eq!(missing_patterns, &expected_missing);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn wait_quiet_returns_after_no_real_content_for_n_sec() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "claude", DriverKind::Claude);
        let status = supervisor.start_control_plane().unwrap();
        let started = Instant::now();

        let response = supervisor.apply_sideband_request(SidebandRequest::WaitQuiet {
            token: status.token,
            name: "claude".into(),
            quiet_seconds: 1,
            timeout_seconds: 2,
        });

        assert!(started.elapsed() >= Duration::from_millis(900));
        assert!(response.ok);
        match response.payload {
            Some(SidebandResponsePayload::WaitQuiet { quiet_duration_ms }) => {
                assert!(quiet_duration_ms >= 1000);
            }
            other => panic!("unexpected wait_quiet payload: {other:?}"),
        }
    }

    #[test]
    fn wait_quiet_timeout_reports_last_output_age() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "claude", DriverKind::Claude);
        {
            let mut slots = supervisor.inner.slots.lock();
            slots.get_mut("claude").unwrap().last_real_output_at = Some(Instant::now());
        }
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::WaitQuiet {
            token: status.token,
            name: "claude".into(),
            quiet_seconds: 2,
            timeout_seconds: 1,
        });

        assert!(!response.ok);
        match response.payload {
            Some(SidebandResponsePayload::WaitQuietTimeout { last_output_age_ms }) => {
                assert!(last_output_age_ms < 2000);
            }
            other => panic!("unexpected wait_quiet timeout payload: {other:?}"),
        }
    }

    #[test]
    fn wait_quiet_real_content_resets_timer() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "claude", DriverKind::Claude);
        let status = supervisor.start_control_plane().unwrap();
        let refresher = supervisor.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            refresher.handle_pty_event("claude", 0, PtyEvent::Output("Working".into()));
        });
        let started = Instant::now();

        let response = supervisor.apply_sideband_request(SidebandRequest::WaitQuiet {
            token: status.token,
            name: "claude".into(),
            quiet_seconds: 1,
            timeout_seconds: 3,
        });

        assert!(response.ok);
        assert!(started.elapsed() >= Duration::from_millis(1100));
    }

    #[test]
    fn handle_pty_event_drops_stale_generation() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "claude", DriverKind::Claude);
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("claude").unwrap();
            slot.generation = 2;
            slot.state = LifecycleState::Busy;
        }
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        supervisor.handle_pty_event("claude", 1, PtyEvent::Output("Working".into()));

        assert_eq!(supervisor.current_generation("claude"), Some(2));
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .snapshot();
        assert_eq!(slot.lifecycle_state, LifecycleState::Busy);
        let events = events.lock();
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SystemLog { message, .. }
                if message.contains("Dropped stale PTY event")
        )));
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, RuntimeEvent::SessionOutput { .. }))
        );
    }

    #[test]
    fn pty_closed_clean_exit_emits_session_exit() {
        let supervisor = test_supervisor();
        let (pty, _, _) = mock_pty_session_with_exit_status(
            Some(1201),
            Some(pty_exit_status(0, None, true)),
            MockKillBehavior::Immediate,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(1201),
            pty,
        );
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event("codex", 0, PtyEvent::Closed);

        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        match &exits[0] {
            RuntimeEvent::SessionExit {
                session,
                generation,
                process_id,
                exit_code,
                signal,
                success,
                reason,
                requested,
                timestamp,
            } => {
                assert_eq!(session, "codex");
                assert_eq!(*generation, 0);
                assert_eq!(*process_id, Some(1201));
                assert_eq!(*exit_code, Some(0));
                assert_eq!(*signal, None);
                assert!(*success);
                assert_eq!(*reason, SessionExitReason::CleanExit);
                assert!(!requested);
                assert!(events.lock().iter().any(|event| matches!(
                    event,
                    RuntimeEvent::SessionState {
                        session,
                        state: LifecycleState::Closed,
                        reason,
                        timestamp: state_timestamp,
                    } if session == "codex"
                        && reason == "session output closed"
                        && state_timestamp == timestamp
                )));
            }
            other => panic!("unexpected event: {other:?}"),
        }

        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.last_error, None);
    }

    #[test]
    fn pty_closed_crash_exit_sets_last_error() {
        let supervisor = test_supervisor();
        let (pty, _, _) = mock_pty_session_with_exit_status(
            Some(1202),
            Some(pty_exit_status(1, None, false)),
            MockKillBehavior::Immediate,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(1202),
            pty,
        );
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event("codex", 0, PtyEvent::Closed);

        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                success: false,
                reason: SessionExitReason::CrashExit,
                requested: false,
                exit_code: Some(1),
                ..
            }
        ));
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.last_error, Some("process exited with code 1".into()));
    }

    #[test]
    fn operator_stop_emits_requested_session_exit_without_last_error() {
        let supervisor = test_supervisor();
        let process_id = std::process::id();
        let (pty, _, kill_count) = mock_pty_session_with_exit_status(
            Some(process_id),
            Some(pty_exit_status(1, Some("TERM"), false)),
            MockKillBehavior::Immediate,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(process_id),
            pty,
        );
        let events = capture_runtime_events(&supervisor);

        let snapshot = supervisor.stop_session("codex").unwrap();

        assert_eq!(snapshot.lifecycle_state, LifecycleState::Closed);
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);
        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                generation: 1,
                process_id: Some(id),
                reason: SessionExitReason::OperatorStop,
                requested: true,
                success: true,
                signal: Some(15),
                ..
            } if *id == process_id
        ));
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.last_error, None);
    }

    #[test]
    fn restart_emits_restart_stop_before_new_ready_state() {
        let supervisor = test_supervisor();
        let process_id = std::process::id();
        let (old_pty, _, _) = mock_pty_session_with_exit_status(
            Some(process_id),
            Some(pty_exit_status(0, None, true)),
            MockKillBehavior::Immediate,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(process_id),
            old_pty,
        );
        let (new_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![new_pty])));
        let events = capture_runtime_events(&supervisor);

        let snapshot = supervisor.restart_session("codex").unwrap();

        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                reason: SessionExitReason::RestartStop,
                requested: true,
                success: true,
                ..
            }
        ));
        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SessionState {
                    session,
                    state: LifecycleState::Ready,
                    reason,
                    ..
                } if session == "codex" && reason == "session ready"
            )
        });
    }

    #[test]
    fn auto_restart_on_stall_alerts_and_restarts_allowed_session() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_millis(25)),
        );
        let (old_pty, _, _) = mock_pty_session_with_exit_status(
            None,
            Some(pty_exit_status(0, None, true)),
            MockKillBehavior::Immediate,
        );
        let (new_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![new_pty])));
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, old_pty);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );

        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SupervisorAlert {
                    alert_type: SupervisorAlertType::SessionStallDetected,
                    session: Some(session),
                    severity: AlertSeverity::Critical,
                    ..
                } if session == "codex"
            )
        });
        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SessionExit {
                    session,
                    reason: SessionExitReason::RestartStop,
                    ..
                } if session == "codex"
            )
        });
        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SessionState {
                    session,
                    state: LifecycleState::Ready,
                    reason,
                    ..
                } if session == "codex" && reason == "session ready"
            )
        });
        assert_eq!(
            supervisor.inner.slots.lock().get("codex").unwrap().state,
            LifecycleState::Ready
        );
    }

    #[test]
    fn auto_restart_on_stall_clears_when_state_recovers_before_threshold() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_millis(100)),
        );
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );
        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working 1s".into()));
        thread::sleep(Duration::from_millis(150));

        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert!(supervisor_alert_events(&events).is_empty());
        assert!(session_exit_events(&events).is_empty());
    }

    #[test]
    fn auto_restart_on_stall_caps_fourth_restart_attempt() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_millis(25)),
        );
        {
            let mut history = supervisor.inner.auto_restart_history.lock();
            history.insert(
                "codex".into(),
                AutoRestartHistory {
                    attempts: vec![Instant::now(), Instant::now(), Instant::now()],
                    disabled: false,
                },
            );
        }
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );

        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SupervisorAlert {
                    alert_type: SupervisorAlertType::SessionStallDetected,
                    session: Some(session),
                    severity: AlertSeverity::Critical,
                    message,
                    ..
                } if session == "codex" && message.contains("cap reached")
            )
        });
        thread::sleep(Duration::from_millis(50));
        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert!(session_exit_events(&events).is_empty());
        assert!(
            supervisor
                .inner
                .auto_restart_history
                .lock()
                .get("codex")
                .unwrap()
                .disabled
        );
    }

    #[test]
    fn auto_restart_on_stall_disabled_by_default() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(Vec::new()),
            Some(Duration::from_millis(25)),
        );
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );
        thread::sleep(Duration::from_millis(80));

        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert!(supervisor_alert_events(&events).is_empty());
        assert!(session_exit_events(&events).is_empty());
    }

    #[test]
    fn liveness_pruning_without_exit_status_emits_process_disappeared() {
        let supervisor = test_supervisor();
        let (pty, _, _) =
            mock_pty_session_with_exit_status(Some(u32::MAX), None, MockKillBehavior::Immediate);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(u32::MAX),
            pty,
        );
        let events = capture_runtime_events(&supervisor);

        let snapshot = supervisor.snapshot();

        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.name == "codex")
            .unwrap();
        assert!(!codex.running);
        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                reason: SessionExitReason::ProcessDisappeared,
                requested: false,
                success: false,
                ..
            }
        ));
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.last_error, Some("process no longer running".into()));
    }

    #[test]
    fn pty_error_emits_session_exit_and_last_error() {
        let supervisor = test_supervisor();
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event("codex", 0, PtyEvent::Error("pipe broke".into()));

        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                reason: SessionExitReason::PtyError,
                requested: false,
                success: false,
                ..
            }
        ));
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.last_error, Some("pipe broke".into()));
    }

    #[test]
    fn sideband_lifecycle_events_emitted_for_mailbox_ping() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        let sideband_dir = supervisor.runtime_dir().join("sideband-test-lifecycle");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&outbox_dir).unwrap();
        fs::create_dir_all(&processed_dir).unwrap();

        let request_path = inbox_dir.join("ping.json");
        fs::write(
            &request_path,
            encode_request(&SidebandRequest::Ping {
                token: status.token.clone(),
            })
            .unwrap(),
        )
        .unwrap();

        process_sideband_mailbox_file(&supervisor, &request_path, &outbox_dir, &processed_dir)
            .unwrap();

        let lifecycle_events = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SidebandRequestLifecycle {
                    request_id,
                    action,
                    phase,
                    ..
                } => Some((request_id.clone(), action.clone(), *phase)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(lifecycle_events.len(), 2);
        assert_eq!(lifecycle_events[0].1, "ping");
        assert_eq!(lifecycle_events[0].2, SidebandPhase::Started);
        assert_eq!(lifecycle_events[1].2, SidebandPhase::Completed);
        assert_eq!(lifecycle_events[0].0, lifecycle_events[1].0);
    }

    #[test]
    fn sideband_lifecycle_events_emitted_for_pipe_ping() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        let response = runtime.block_on(async {
            let (client, server) = tokio::io::duplex(4096);
            let handle = supervisor.clone();
            let request_payload = format!(
                "{}\n",
                encode_request(&SidebandRequest::Ping {
                    token: status.token.clone(),
                })
                .unwrap()
            );

            let server_task = tokio::spawn(async move {
                handle_sideband_stream(handle, server).await.unwrap();
            });

            let (read_half, mut write_half) = tokio::io::split(client);
            write_half
                .write_all(request_payload.as_bytes())
                .await
                .unwrap();
            write_half.flush().await.unwrap();

            let mut reader = BufReader::new(read_half);
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            server_task.await.unwrap();

            decode_response(line.trim()).unwrap()
        });

        assert!(response.ok);
        assert_eq!(response.message, "pong");

        let lifecycle_events = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SidebandRequestLifecycle {
                    request_id,
                    action,
                    phase,
                    ..
                } => Some((request_id.clone(), action.clone(), *phase)),
                _ => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(lifecycle_events.len(), 2);
        assert_eq!(lifecycle_events[0].1, "ping");
        assert_eq!(lifecycle_events[0].2, SidebandPhase::Started);
        assert_eq!(lifecycle_events[1].2, SidebandPhase::Completed);
        assert_eq!(lifecycle_events[0].0, lifecycle_events[1].0);
    }

    #[test]
    fn sideband_send_input_emits_request_ack_and_response_request_id() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });
        let (pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "/fast".into(),
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        let response_request_id = response.request_id.as_deref().unwrap();

        let events = events.lock();
        let ack_events = events
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::RequestAck {
                    request_id,
                    session,
                    action,
                    bytes_written,
                    ..
                } => Some((
                    request_id.as_str(),
                    session.as_str(),
                    action.as_str(),
                    *bytes_written,
                )),
                _ => None,
            })
            .collect::<Vec<_>>();
        let timeout_count = events
            .iter()
            .filter(|event| matches!(event, RuntimeEvent::RequestAckTimeout { .. }))
            .count();

        assert_eq!(ack_events.len(), 1);
        assert_eq!(timeout_count, 0);
        assert_eq!(
            ack_events[0],
            (response_request_id, "codex", "send_input", 5)
        );
    }

    #[test]
    fn no_output_dispatch_emits_no_reaction_and_does_not_force_busy() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(!response.ok);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        assert!(request_ack_events(&events).is_empty());
        assert_eq!(dispatch_no_reaction_events(&events).len(), 1);
        assert!(
            supervisor_alert_events(&events)
                .iter()
                .any(|event| matches!(
                    event,
                    RuntimeEvent::SupervisorAlert {
                        alert_type: SupervisorAlertType::DispatchNoReaction,
                        request_id: Some(request_id),
                        session: Some(session),
                        action: Some(action),
                        severity: AlertSeverity::Critical,
                        ..
                    } if request_id == response.request_id.as_deref().unwrap()
                        && session == "codex"
                        && action == "send_input"
                ))
        );
        let slots = supervisor.inner.slots.lock();
        assert_eq!(slots.get("codex").unwrap().state, LifecycleState::Ready);
    }

    #[test]
    fn request_ack_is_emitted_only_after_reaction_output() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        let events = events.lock();
        let output_index = events
            .iter()
            .position(|event| matches!(event, RuntimeEvent::SessionOutput { .. }))
            .unwrap();
        let ack_index = events
            .iter()
            .position(|event| matches!(event, RuntimeEvent::RequestAck { .. }))
            .unwrap();
        assert!(output_index < ack_index);
    }

    #[test]
    fn terminal_output_is_negative_reaction_not_ack() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let handle = supervisor.clone();
        let (pty, send_count, _) = mock_pty_session_full(
            None,
            None,
            MockKillBehavior::Immediate,
            AgentLiveness::Alive(Vec::new()),
            Some(Arc::new(move || {
                handle.handle_pty_event(
                    "codex",
                    0,
                    PtyEvent::Output("PS C:\\Projects\\PRIM-1>".into()),
                );
            })),
        );
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(!response.ok);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        assert!(request_ack_events(&events).is_empty());
        assert_eq!(dispatch_no_reaction_events(&events).len(), 1);
        assert!(work_state_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState {
                session,
                state: WorkState::Exited,
                detail: Some(detail),
                ..
            } if session == "codex" && detail == "shell_prompt"
        )));
    }

    #[test]
    fn output_before_dispatch_baseline_does_not_resolve_reaction() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );
        supervisor.handle_pty_event("codex", 0, PtyEvent::Output("Working 1s".into()));

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(!response.ok);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        assert!(request_ack_events(&events).is_empty());
        assert_eq!(dispatch_no_reaction_events(&events).len(), 1);
    }

    #[test]
    fn route_fails_when_any_recipient_does_not_react() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, claude_send_count, _) =
            reacting_pty_session(&supervisor, "claude", "Thinking...");
        let (codex_pty, codex_send_count, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
        set_session_dispatch_state(
            &supervisor,
            "claude",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: status.token,
            request: RouteMessageRequest {
                from: "operator".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "status".into(),
            },
            require_idle: false,
        });

        assert!(!response.ok);
        assert!(claude_send_count.load(Ordering::SeqCst) > 0);
        assert!(codex_send_count.load(Ordering::SeqCst) > 0);
        assert_eq!(request_ack_events(&events).len(), 1);
        assert_eq!(dispatch_no_reaction_events(&events).len(), 1);
        assert!(route_delivery_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                recipient: Some(recipient),
                error: Some(error),
                ..
            } if recipient == "codex" && error.contains("dispatch no reaction")
        )));
    }

    #[test]
    fn dispatch_attempt_idle_ready_proceeds_without_overlap() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        let attempts = dispatch_attempt_events(&events);
        assert_eq!(attempts.len(), 1);
        assert!(matches!(
            &attempts[0],
            RuntimeEvent::DispatchAttempt {
                request_id,
                action,
                from,
                target_session,
                target_lifecycle_state_before,
                target_work_state_before,
                overlap,
                reason,
                ..
            } if request_id == response.request_id.as_deref().unwrap()
                && action == "send_input"
                && from == "operator"
                && target_session == "codex"
                && *target_lifecycle_state_before == LifecycleState::Ready
                && *target_work_state_before == Some(WorkState::Idle)
                && !overlap
                && reason.is_none()
        ));
    }

    #[test]
    fn dispatch_attempt_require_idle_aborts_thinking_without_ack_or_write() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: true,
        });

        assert!(!response.ok);
        assert_eq!(response.message, "dispatch aborted: target target_thinking");
        assert_eq!(send_count.load(Ordering::SeqCst), 0);
        assert!(request_ack_events(&events).is_empty());
        let attempts = dispatch_attempt_events(&events);
        assert_eq!(attempts.len(), 1);
        assert!(matches!(
            &attempts[0],
            RuntimeEvent::DispatchAttempt {
                overlap: true,
                reason: Some(reason),
                target_work_state_before: Some(WorkState::Thinking),
                ..
            } if reason == "target_thinking"
        ));
    }

    #[test]
    fn dispatch_attempt_allow_busy_semantics_proceed_when_thinking() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        assert_eq!(request_ack_events(&events).len(), 1);
        let attempts = dispatch_attempt_events(&events);
        assert_eq!(attempts.len(), 1);
        assert!(matches!(
            &attempts[0],
            RuntimeEvent::DispatchAttempt {
                overlap: true,
                reason: Some(reason),
                ..
            } if reason == "target_thinking"
        ));
    }

    #[test]
    fn dispatch_attempt_default_mode_proceeds_when_thinking() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );

        let request_json = serde_json::json!({
            "kind": "send_input",
            "token": status.token,
            "name": "codex",
            "input": "hello"
        });
        let request: SidebandRequest = serde_json::from_value(request_json).unwrap();
        let response = supervisor.apply_sideband_request(request);

        assert!(response.ok, "got: {}", response.message);
        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        assert_eq!(request_ack_events(&events).len(), 1);
        assert!(matches!(
            &dispatch_attempt_events(&events)[0],
            RuntimeEvent::DispatchAttempt {
                overlap: true,
                reason: Some(reason),
                ..
            } if reason == "target_thinking"
        ));
    }

    #[test]
    fn dispatch_attempt_recent_route_reason_takes_precedence() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (pty, send_count, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );
        mark_recent_route_from_session(&supervisor, "codex");

        let response = supervisor.apply_sideband_request(SidebandRequest::SendInput {
            token: status.token,
            name: "codex".into(),
            input: "hello".into(),
            require_idle: true,
        });

        assert!(!response.ok);
        assert_eq!(
            response.message,
            "dispatch aborted: target recent_route_from_target"
        );
        assert_eq!(send_count.load(Ordering::SeqCst), 0);
        let attempts = dispatch_attempt_events(&events);
        assert_eq!(attempts.len(), 1);
        assert!(matches!(
            &attempts[0],
            RuntimeEvent::DispatchAttempt {
                overlap: true,
                reason: Some(reason),
                last_route_from_target_at: Some(_),
                ..
            } if reason == "recent_route_from_target"
        ));
    }

    #[test]
    fn detached_stop_late_events_filtered_by_generation() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let (old_pty, _, old_kill_count) =
            mock_pty_session(None, MockKillBehavior::Sleep(Duration::from_millis(200)));
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, old_pty);
        let (new_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![new_pty])));

        let response = run_detached_with_timeout(
            &supervisor,
            SidebandRequest::StopSession {
                token: status.token.clone(),
                name: "claude".into(),
            },
            Duration::from_millis(50),
            "req-stop",
            "stop_session",
            Some("claude"),
            &[],
        );
        assert!(response.timed_out);
        wait_until(Duration::from_secs(1), || {
            let slots = supervisor.inner.slots.lock();
            slots
                .get("claude")
                .map(|slot| slot.running.is_none())
                .unwrap_or(false)
        });

        let snapshot = supervisor.start_session("claude", Vec::new()).unwrap();
        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        assert!(supervisor.test_wait_for_last_worker(Duration::from_secs(1)));
        assert_eq!(supervisor.current_generation("claude"), Some(2));
        assert!(old_kill_count.load(Ordering::SeqCst) >= 1);

        let slots = supervisor.inner.slots.lock();
        let slot = slots.get("claude").unwrap();
        assert_eq!(slot.state, LifecycleState::Ready);
        assert_eq!(slot.generation, 2);
    }

    #[test]
    fn detached_start_orphan_pty_killed_on_generation_mismatch() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let (first_pty, _, first_kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (second_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        supervisor.set_pty_spawner_for_tests(Arc::new(StagedPtySpawner::new(
            gate.clone(),
            vec![first_pty, second_pty],
        )));

        let response = run_detached_with_timeout(
            &supervisor,
            SidebandRequest::StartSession {
                token: status.token.clone(),
                name: "claude".into(),
                extra_args: Vec::new(),
            },
            Duration::from_millis(50),
            "req-start",
            "start_session",
            Some("claude"),
            &[],
        );
        assert!(response.timed_out);

        let snapshot = supervisor.start_session("claude", Vec::new()).unwrap();
        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        {
            let (lock, cvar) = &*gate;
            let mut released = lock.lock();
            *released = true;
            cvar.notify_all();
        }

        assert!(supervisor.test_wait_for_last_worker(Duration::from_secs(1)));
        assert_eq!(first_kill_count.load(Ordering::SeqCst), 1);
        assert_eq!(supervisor.current_generation("claude"), Some(2));

        let slots = supervisor.inner.slots.lock();
        let slot = slots.get("claude").unwrap();
        assert_eq!(slot.state, LifecycleState::Ready);
        assert_eq!(slot.generation, 2);
    }

    #[test]
    fn mailbox_poison_queue_catches_rename_failure() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        supervisor.set_mailbox_fs_for_tests(Arc::new(FailingRenameMailboxFs::new(3)));
        let sideband_dir = supervisor.runtime_dir().join("sideband-test-poison");
        let inbox_dir = sideband_dir.join("inbox");
        let outbox_dir = sideband_dir.join("outbox");
        let processed_dir = sideband_dir.join("processed");
        fs::create_dir_all(&inbox_dir).unwrap();
        fs::create_dir_all(&outbox_dir).unwrap();
        fs::create_dir_all(&processed_dir).unwrap();

        let request_path = inbox_dir.join("ping.json");
        fs::write(
            &request_path,
            encode_request(&SidebandRequest::Ping {
                token: status.token.clone(),
            })
            .unwrap(),
        )
        .unwrap();

        let error =
            process_sideband_mailbox_file(&supervisor, &request_path, &outbox_dir, &processed_dir)
                .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("failed to publish mailbox response")
        );

        let poison_dir = sideband_dir.join("poison");
        let poison_entries = fs::read_dir(&poison_dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .collect::<Vec<_>>();
        assert!(
            poison_entries
                .iter()
                .any(|path| path.extension().and_then(|ext| ext.to_str()) != Some("error"))
        );
        assert!(
            poison_entries
                .iter()
                .any(|path| path.extension().and_then(|ext| ext.to_str()) == Some("error"))
        );
    }

    #[test]
    fn pane_bound_send_key_allows_peer_target_when_lockdown_is_off() {
        let supervisor = test_supervisor_with_peer_slash_commands_allowed(true);
        let claude_token = session_token(&supervisor, "claude");

        supervisor
            .validate_session_action_token(&claude_token, "codex")
            .unwrap();
    }

    #[test]
    fn prepare_definition_for_spawn_injects_pane_credentials_env() {
        let supervisor = test_supervisor();
        supervisor.start_control_plane().unwrap();
        let definition = {
            let slots = supervisor.inner.slots.lock();
            slots.get("claude").unwrap().definition.clone()
        };

        let prepared = supervisor
            .prepare_definition_for_spawn(&definition)
            .unwrap();
        let credentials = supervisor.runtime_dir().join("control-plane-claude.json");

        assert!(
            prepared
                .env
                .iter()
                .any(|env| { env.key == "PRIM1_PANE_IDENTITY" && env.value == "claude" })
        );
        assert!(prepared.env.iter().any(|env| {
            env.key == "PRIM1_PANE_CREDENTIALS" && env.value == credentials.display().to_string()
        }));
        assert!(credentials.exists());
    }

    #[test]
    fn apply_sideband_list_sessions_returns_snapshot() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();

        let response = supervisor.apply_sideband_request(SidebandRequest::ListSessions {
            token: status.token,
        });

        assert!(response.ok);
        assert_eq!(response.message, "sessions listed");
        assert_eq!(response.snapshot.unwrap().sessions.len(), 2);
    }

    #[test]
    fn route_message_overrides_from_with_bound_session_identity() {
        let supervisor = test_supervisor();
        let claude_token = session_token(&supervisor, "claude");
        let cursor = supervisor.current_eof_cursor().unwrap();
        let (claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: claude_token.clone(),
            request: RouteMessageRequest {
                from: "spoofed-name".into(),
                to: "codex".into(),
                scope: MessageScope::Direct,
                content: "hi".into(),
            },
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);

        let routed = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: claude_token,
            cursor: Some(cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(EventFilter {
                include_kinds: vec!["routed_message".into()],
                include_sessions: Vec::new(),
                include_scopes: Vec::new(),
            }),
        });
        let (events, _, _, _) = unwrap_events_since(routed);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            RuntimeEvent::RoutedMessage {
                from,
                to,
                scope,
                content,
                ..
            } if from == "claude"
                && to == "codex"
                && *scope == MessageScope::Direct
                && content == "hi"
        ));
    }

    #[test]
    fn route_message_preserves_user_from_for_root_token() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let cursor = supervisor.current_eof_cursor().unwrap();
        let (claude_pty, _, _) = reacting_pty_session(&supervisor, "claude", "Thinking...");
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: status.token.clone(),
            request: RouteMessageRequest {
                from: "operator".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "hi".into(),
            },
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);

        let routed = supervisor.apply_sideband_request(SidebandRequest::EventsSince {
            token: status.token,
            cursor: Some(cursor),
            max_events: Some(10),
            max_wait_seconds: Some(0),
            filter: Some(EventFilter {
                include_kinds: vec!["routed_message".into()],
                include_sessions: Vec::new(),
                include_scopes: Vec::new(),
            }),
        });
        let (events, _, _, _) = unwrap_events_since(routed);

        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            RuntimeEvent::RoutedMessage {
                from,
                to,
                scope,
                content,
                ..
            } if from == "operator"
                && to == "claude"
                && *scope == MessageScope::Direct
                && content == "hi"
        ));
    }

    #[test]
    fn sideband_direct_route_emits_resolved_and_written_delivery() {
        let supervisor = test_supervisor();
        let claude_token = session_token(&supervisor, "claude");
        let events = capture_runtime_events(&supervisor);
        let (codex_pty, _, _) = reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
        let long_content = (0..120)
            .map(|index| format!("segment-{index:03}"))
            .collect::<Vec<_>>()
            .join(" ");

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: claude_token,
            request: RouteMessageRequest {
                from: "spoofed".into(),
                to: "codex".into(),
                scope: MessageScope::Direct,
                content: long_content,
            },
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        let response_request_id = response.request_id.as_deref().unwrap();
        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 2);

        let route_id = match &deliveries[0] {
            RuntimeEvent::RouteDelivery {
                request_id,
                route_id,
                from,
                logical_to,
                scope,
                recipient,
                recipient_index,
                recipient_count,
                payload_part_count,
                phase,
                bytes_written,
                error,
                ..
            } => {
                assert_eq!(request_id, response_request_id);
                assert_eq!(from, "claude");
                assert_eq!(logical_to, "codex");
                assert_eq!(*scope, MessageScope::Direct);
                assert_eq!(recipient, &None);
                assert_eq!(*recipient_index, 0);
                assert_eq!(*recipient_count, 1);
                assert_eq!(*payload_part_count, 0);
                assert_eq!(*phase, RouteDeliveryPhase::Resolved);
                assert_eq!(*bytes_written, 0);
                assert_eq!(error, &None);
                route_id.clone()
            }
            other => panic!("unexpected event: {other:?}"),
        };

        match &deliveries[1] {
            RuntimeEvent::RouteDelivery {
                request_id,
                route_id: written_route_id,
                from,
                logical_to,
                scope,
                recipient,
                recipient_index,
                recipient_count,
                payload_part_count,
                phase,
                bytes_written,
                error,
                ..
            } => {
                assert_eq!(request_id, response_request_id);
                assert_eq!(written_route_id, &route_id);
                assert_eq!(from, "claude");
                assert_eq!(logical_to, "codex");
                assert_eq!(*scope, MessageScope::Direct);
                assert_eq!(recipient.as_deref(), Some("codex"));
                assert_eq!(*recipient_index, 0);
                assert_eq!(*recipient_count, 1);
                assert!(*payload_part_count > 1);
                assert_eq!(*phase, RouteDeliveryPhase::Written);
                assert!(*bytes_written > 0);
                assert_eq!(error, &None);
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn operator_room_route_emits_two_pane_delivery_receipts() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);

        supervisor
            .route_message(RouteMessageRequest {
                from: "operator".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "status".into(),
            })
            .unwrap();

        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 3);
        assert!(matches!(
            &deliveries[0],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Resolved,
                recipient: None,
                recipient_count: 2,
                ..
            }
        ));

        let mut written_recipients = deliveries
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::RouteDelivery {
                    phase: RouteDeliveryPhase::Written,
                    recipient: Some(recipient),
                    recipient_count,
                    payload_part_count,
                    bytes_written,
                    error,
                    ..
                } => {
                    assert_eq!(*recipient_count, 2);
                    assert_eq!(*payload_part_count, 1);
                    assert!(*bytes_written > 0);
                    assert_eq!(error, &None);
                    Some(recipient.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        written_recipients.sort();

        assert_eq!(
            written_recipients,
            vec!["claude".to_string(), "codex".to_string()]
        );
    }

    #[test]
    fn route_message_default_emits_dispatch_attempt_per_recipient_and_proceeds() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, claude_send_count, _) =
            reacting_pty_session(&supervisor, "claude", "Thinking...");
        let (codex_pty, codex_send_count, _) =
            reacting_pty_session(&supervisor, "codex", "Working 1s");
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
        set_session_dispatch_state(
            &supervisor,
            "claude",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: status.token,
            request: RouteMessageRequest {
                from: "operator".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "status".into(),
            },
            require_idle: false,
        });

        assert!(response.ok, "got: {}", response.message);
        assert!(claude_send_count.load(Ordering::SeqCst) > 0);
        assert!(codex_send_count.load(Ordering::SeqCst) > 0);
        let attempts = dispatch_attempt_events(&events);
        assert_eq!(attempts.len(), 2);
        assert!(attempts.iter().any(|event| matches!(
            event,
            RuntimeEvent::DispatchAttempt {
                target_session,
                overlap: false,
                reason: None,
                ..
            } if target_session == "claude"
        )));
        assert!(attempts.iter().any(|event| matches!(
            event,
            RuntimeEvent::DispatchAttempt {
                target_session,
                overlap: true,
                reason: Some(reason),
                ..
            } if target_session == "codex" && reason == "target_thinking"
        )));
        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 3);
    }

    #[test]
    fn route_message_require_idle_aborts_entire_route_before_delivery_events_or_writes() {
        let supervisor = test_supervisor();
        let status = supervisor.start_control_plane().unwrap();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, claude_send_count, _) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, codex_send_count, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
        set_session_dispatch_state(
            &supervisor,
            "claude",
            LifecycleState::Ready,
            Some(WorkState::Idle),
        );
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Thinking),
        );

        let response = supervisor.apply_sideband_request(SidebandRequest::RouteMessage {
            token: status.token,
            request: RouteMessageRequest {
                from: "operator".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "status".into(),
            },
            require_idle: true,
        });

        assert!(!response.ok);
        assert!(response.message.contains("codex"));
        assert!(response.message.contains("target_thinking"));
        assert_eq!(claude_send_count.load(Ordering::SeqCst), 0);
        assert_eq!(codex_send_count.load(Ordering::SeqCst), 0);
        assert_eq!(dispatch_attempt_events(&events).len(), 2);
        assert!(route_delivery_events(&events).is_empty());
        assert!(request_ack_events(&events).is_empty());
    }

    #[test]
    fn cross_pair_room_route_emits_delivery_for_each_resolved_recipient() {
        let supervisor = test_supervisor_with_cross_pair_room_broadcast(true);
        supervisor.create_pair("FrontendQA").unwrap();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (frontend_claude_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (frontend_codex_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);
        install_mock_running_session(
            &supervisor,
            "FrontendQA-claude",
            DriverKind::Claude,
            frontend_claude_pty,
        );
        install_mock_running_session(
            &supervisor,
            "FrontendQA-codex",
            DriverKind::Codex,
            frontend_codex_pty,
        );

        supervisor
            .route_message(RouteMessageRequest {
                from: "claude".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "status".into(),
            })
            .unwrap();

        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 4);
        assert!(matches!(
            &deliveries[0],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Resolved,
                recipient_count: 3,
                ..
            }
        ));

        let mut written_recipients = deliveries
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::RouteDelivery {
                    phase: RouteDeliveryPhase::Written,
                    recipient: Some(recipient),
                    recipient_count,
                    ..
                } => {
                    assert_eq!(*recipient_count, 3);
                    Some(recipient.clone())
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        written_recipients.sort();

        assert_eq!(
            written_recipients,
            vec![
                "FrontendQA-claude".to_string(),
                "FrontendQA-codex".to_string(),
                "codex".to_string(),
            ]
        );
    }

    #[test]
    fn no_recipient_route_still_emits_resolved_delivery() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .route_message(RouteMessageRequest {
                from: "operator".into(),
                to: "missing".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("no running recipients available for 'missing'")
        );
        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 1);
        assert!(matches!(
            &deliveries[0],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Resolved,
                recipient: None,
                recipient_count: 0,
                payload_part_count: 0,
                bytes_written: 0,
                ..
            }
        ));
    }

    #[test]
    fn one_recipient_write_failure_emits_failed_delivery_and_returns_error() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (codex_pty, send_count) = failing_pty_session("synthetic route write failure");
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);

        let error = supervisor
            .route_message(RouteMessageRequest {
                from: "claude".into(),
                to: "codex".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            })
            .unwrap_err();

        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        let error = error.to_string();
        assert!(error.contains("codex"));
        assert!(error.contains("synthetic route write failure"));

        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 2);
        assert!(matches!(
            &deliveries[0],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Resolved,
                recipient_count: 1,
                ..
            }
        ));
        assert!(matches!(
            &deliveries[1],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                recipient: Some(recipient),
                recipient_count: 1,
                payload_part_count: 1,
                bytes_written: 0,
                error: Some(message),
                ..
            } if recipient == "codex" && message.contains("synthetic route write failure")
        ));
    }

    #[test]
    fn mixed_room_delivery_failure_preserves_success_receipt_and_returns_error() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, claude_send_count, _) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        let (codex_pty, codex_send_count) = failing_pty_session("synthetic route write failure");
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, claude_pty);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, codex_pty);

        let error = supervisor
            .route_message(RouteMessageRequest {
                from: "operator".into(),
                to: "room".into(),
                scope: MessageScope::Room,
                content: "hello".into(),
            })
            .unwrap_err();

        assert_eq!(claude_send_count.load(Ordering::SeqCst), 2);
        assert_eq!(codex_send_count.load(Ordering::SeqCst), 1);
        let error = error.to_string();
        assert!(error.contains("codex"));
        assert!(error.contains("synthetic route write failure"));

        let deliveries = route_delivery_events(&events);
        assert_eq!(deliveries.len(), 3);
        assert!(matches!(
            &deliveries[0],
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Resolved,
                recipient_count: 2,
                ..
            }
        ));
        assert!(deliveries.iter().any(|event| {
            matches!(
                event,
                RuntimeEvent::RouteDelivery {
                    phase: RouteDeliveryPhase::Written,
                    recipient: Some(recipient),
                    recipient_count: 2,
                    ..
                } if recipient == "claude"
            )
        }));
        assert!(deliveries.iter().any(|event| {
            matches!(
                event,
                RuntimeEvent::RouteDelivery {
                    phase: RouteDeliveryPhase::Failed,
                    recipient: Some(recipient),
                    recipient_count: 2,
                    bytes_written: 0,
                    error: Some(message),
                    ..
                } if recipient == "codex" && message.contains("synthetic route write failure")
            )
        }));
    }

    #[test]
    fn route_message_requires_running_recipient() {
        let supervisor = test_supervisor();

        let error = supervisor
            .route_message(RouteMessageRequest {
                from: "operator".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("no running recipients available for 'claude'")
        );
    }

    #[test]
    fn send_control_key_rejects_non_running_session() {
        let supervisor = test_supervisor();

        let error = supervisor
            .send_control_key("claude", ControlKey::Enter)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("session 'claude' is not running")
        );
    }

    #[test]
    fn agent_liveness_prunes_dead_inner_child_while_wrapper_pid_lives() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let wrapper_pid = std::process::id();
        let (pty, _, _) = mock_pty_session_full(
            Some(wrapper_pid),
            None,
            MockKillBehavior::Immediate,
            AgentLiveness::Exited {
                last_agent_pids: vec![4242],
                live_processes: vec![pty_host::ProcessIdentity {
                    process_id: wrapper_pid,
                    image_name: Some("cmd.exe".into()),
                }],
            },
            None,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(wrapper_pid),
            pty,
        );

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.name == "codex")
            .unwrap();
        assert!(!codex.running);
        assert_eq!(codex.lifecycle_state, LifecycleState::Closed);
        assert_eq!(codex.process_id, None);
        assert!(session_exit_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit {
                session,
                process_id: Some(pid),
                reason: SessionExitReason::ProcessDisappeared,
                ..
            } if session == "codex" && *pid == wrapper_pid
        )));

        let error = supervisor
            .deliver_message(DeliverMessageRequest {
                name: "codex".into(),
                content: "hello".into(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("session 'codex' is not running"));
    }

    #[test]
    fn delayed_agent_start_does_not_prune_cmd_only_window() {
        let supervisor = test_supervisor();
        let wrapper_pid = std::process::id();
        let (pty, _, _) = mock_pty_session_full(
            Some(wrapper_pid),
            None,
            MockKillBehavior::Immediate,
            AgentLiveness::NotYetObserved(vec![pty_host::ProcessIdentity {
                process_id: wrapper_pid,
                image_name: Some("cmd.exe".into()),
            }]),
            None,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(wrapper_pid),
            pty,
        );

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.name == "codex")
            .unwrap();
        assert!(codex.running);
        assert_eq!(codex.process_id, Some(wrapper_pid));
    }

    #[test]
    fn wrapper_only_command_not_found_terminal_output_closes_session() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let wrapper_pid = std::process::id();
        let (pty, _, _) = mock_pty_session_full(
            Some(wrapper_pid),
            None,
            MockKillBehavior::Immediate,
            AgentLiveness::NotYetObserved(vec![pty_host::ProcessIdentity {
                process_id: wrapper_pid,
                image_name: Some("cmd.exe".into()),
            }]),
            None,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(wrapper_pid),
            pty,
        );

        supervisor.handle_pty_event(
            "codex",
            0,
            PtyEvent::Output("codex: command not found".into()),
        );

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.name == "codex")
            .unwrap();
        assert!(!codex.running);
        assert_eq!(codex.lifecycle_state, LifecycleState::Failed);
        assert!(work_state_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState {
                session,
                state: WorkState::Exited,
                detail: Some(detail),
                ..
            } if session == "codex" && detail == "command_not_found"
        )));
        assert!(session_exit_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit {
                session,
                reason: SessionExitReason::CrashExit,
                ..
            } if session == "codex"
        )));
    }

    #[test]
    fn snapshot_prunes_exited_running_session() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "codex");

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.name == "codex")
            .unwrap();

        assert!(!codex.running);
        assert_eq!(codex.lifecycle_state, LifecycleState::Closed);
        assert_eq!(codex.process_id, None);
    }

    #[test]
    fn send_input_rejects_exited_session_after_liveness_refresh() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "codex");

        let error = supervisor
            .send_input(SendInputRequest {
                name: "codex".into(),
                input: "hello".into(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("session 'codex' is not running"));
    }
}
