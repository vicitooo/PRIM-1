use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{BufReader as StdBufReader, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, NaiveDate, Utc};
use control_plane::{DEFAULT_ENDPOINT, MAX_FRAME_BYTES, decode_request, encode_response};
use parking_lot::{Condvar, Mutex, RwLock};
#[cfg(windows)]
use pty_host::PinnedProcess;
use pty_host::{
    AgentLiveness, ConcretePtySession, PtyEvent, PtyEventHandler, PtyExitStatus, PtySession,
    PtyWriteError,
};
use serde::{Deserialize, Serialize};
use shared_types::{
    AddRoomMemberRequest, AlertSeverity, ControlKey, ControlPlaneSnapshot, ControlPlaneStatus,
    CreateRoomRequest, DeleteRoomRequest, DeliverRoomMessageRequest, DriverKind, EnvVar,
    HeartbeatSessionSummary, LaunchSpec, LifecycleState, LogLevel, MessageScope, MoveRoomRequest,
    OperatorRouteMessageRequest, PostRoomMessageRequest, ROOM_EVENT_SCHEMA_VERSION,
    ReadRoomFeedRequest, RemoveRoomMemberRequest, RenameRoomRequest, RoomDeliveryFailure,
    RoomDeliveryResult, RoomDeliveryStatus, RoomFeedItem, RoomFeedPage, RoomId,
    RoomMembershipAction, RoomMessageSender, RoomPostResult, RoomRecipientSelection, RoomSnapshot,
    RouteDeliveryPhase, RunEventIdentity, RuntimeEvent, RuntimeSnapshot, SendInputRequest,
    SessionDefinition, SessionExitReason, SessionGeneration, SessionId, SessionSnapshot,
    SidebandRequest, SidebandResponse, SidebandResponsePayload, SupervisorAlertType,
    WaitQuietRequest, WorkState, now_rfc3339,
};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

mod rooms;

use rooms::{
    PersistedRoomV1, ROOM_CATALOG_FILE_NAME, ROOM_MAX_COUNT, ROOM_MEMBER_MAX_COUNT, RoomCatalogV1,
    RoomRuntime, RoomState, default_room_label, validate_room_label,
};

type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

#[derive(Debug, Clone)]
struct RouteMessageRequest {
    from: String,
    to: String,
    scope: MessageScope,
    content: String,
}

const SIDEBAND_FRAME_MAX_BYTES: usize = MAX_FRAME_BYTES;
const SIDEBAND_FRAME_READ_TIMEOUT: Duration = Duration::from_secs(30);
const SIDEBAND_RESPONSE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const SIDEBAND_MAX_CONNECTIONS: usize = 64;
const SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS: u32 = 60;
const SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS: u32 = 300;
const MESSAGE_BODY_MAX_BYTES: usize = 1024 * 1024;
const BRACKETED_PASTE_START: &str = "\x1b[200~";
const BRACKETED_PASTE_END: &str = "\x1b[201~";
const BRACKETED_PASTE_SUBMIT_DELAY: Duration = Duration::from_secs(1);
const TERMINAL_MODE_CONTROL_MAX_CHARS: u16 = 128;
const DEFAULT_HEARTBEAT_INTERVAL_SECS: u64 = 1800;
const DEFAULT_AUTO_RESTART_STALL_THRESHOLD_SECS: u64 = 600;
const SESSION_STOP_KILL_TIMEOUT: Duration = Duration::from_secs(5);
const AUTO_RESTART_WINDOW: Duration = Duration::from_secs(30 * 60);
const AUTO_RESTART_MAX_PER_WINDOW: usize = 3;
const RECENT_ROUTE_OVERLAP_WINDOW: Duration = Duration::from_secs(3);
const SESSION_LABEL_MAX_CHARS: usize = 128;
const SESSION_CATALOG_SCHEMA_VERSION: u32 = 1;
const SESSION_CATALOG_FILE_NAME: &str = "session-catalog-v1.json";
#[cfg(windows)]
const PANE_MCP_SERVER_NAME: &str = "prim1_pane";
#[cfg(windows)]
const PANE_MCP_MODE_ARGUMENT: &str = "--prim1-pane-mcp";

#[cfg(windows)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneMcpClient {
    Claude,
    Codex,
}

#[cfg(windows)]
impl PaneMcpClient {
    fn for_driver(driver: DriverKind) -> Option<Self> {
        match driver {
            DriverKind::Claude => Some(Self::Claude),
            DriverKind::Codex => Some(Self::Codex),
            // Grok's TUI has no session-scoped plugin seam; Prime cannot use
            // Windows Job identity; Generic Terminal has no model tool host.
            DriverKind::Grok | DriverKind::Prime | DriverKind::GenericTerminal => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageFraming {
    BracketedPaste,
    RawSingleLine,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmitBehavior {
    sequence: &'static str,
    framing: MessageFraming,
    submit_delay: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunInputSafety {
    Raw,
    RoutedSubmit,
    BracketedPasteEnabled,
}

impl From<SubmitBehavior> for RunInputSafety {
    fn from(behavior: SubmitBehavior) -> Self {
        match behavior.framing {
            MessageFraming::BracketedPaste => Self::BracketedPasteEnabled,
            MessageFraming::RawSingleLine => Self::Raw,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BracketedPasteMode {
    Unknown,
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RunBinding {
    session_id: Uuid,
    run_id: Uuid,
    generation: SessionGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TerminalStringKind {
    Osc,
    Dcs,
    Sos,
    Pm,
    Apc,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum TerminalModeParserState {
    #[default]
    Ground,
    Escape,
    Csi(DecPrivateModeParser),
    CsiDiscard,
    String {
        kind: TerminalStringKind,
        chars_seen: u16,
    },
    StringDiscard {
        kind: TerminalStringKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecPrivateModeParser {
    chars_seen: u16,
    private_marker: bool,
    current_parameter: Option<u32>,
    contains_bracketed_paste: bool,
    invalid: bool,
}

impl DecPrivateModeParser {
    fn new() -> Self {
        Self {
            chars_seen: 0,
            private_marker: false,
            current_parameter: None,
            contains_bracketed_paste: false,
            invalid: false,
        }
    }

    fn finish_parameter(&mut self) {
        if self.current_parameter.take() == Some(2004) {
            self.contains_bracketed_paste = true;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BracketedPasteRunState {
    binding: Option<RunBinding>,
    mode: BracketedPasteMode,
    parser: TerminalModeParserState,
}

impl Default for BracketedPasteRunState {
    fn default() -> Self {
        Self {
            binding: None,
            mode: BracketedPasteMode::Unknown,
            parser: TerminalModeParserState::Ground,
        }
    }
}

impl BracketedPasteRunState {
    fn begin_run(&mut self, binding: RunBinding) {
        self.binding = Some(binding);
        self.mode = BracketedPasteMode::Unknown;
        self.parser = TerminalModeParserState::Ground;
    }

    fn mode_for(self, binding: RunBinding) -> BracketedPasteMode {
        if self.binding == Some(binding) {
            self.mode
        } else {
            BracketedPasteMode::Unknown
        }
    }

    fn observe_output(&mut self, binding: RunBinding, chunk: &str) {
        if self.binding != Some(binding) {
            return;
        }
        for character in chunk.chars() {
            self.observe_character(character);
        }
    }

    fn observe_character(&mut self, character: char) {
        if self.observe_global_transition(character) {
            return;
        }

        match self.parser {
            TerminalModeParserState::Ground => self.observe_ground(character),
            TerminalModeParserState::Escape => self.observe_escape(character),
            TerminalModeParserState::Csi(mut csi) => {
                csi.chars_seen = csi.chars_seen.saturating_add(1);
                if csi.chars_seen > TERMINAL_MODE_CONTROL_MAX_CHARS {
                    self.mode = BracketedPasteMode::Unknown;
                    self.parser = TerminalModeParserState::CsiDiscard;
                    self.observe_csi_discard(character);
                    return;
                }

                if Self::is_c0_executable_or_del(character) {
                    self.parser = TerminalModeParserState::Csi(csi);
                    return;
                }

                match character {
                    '\u{001b}' => {
                        self.parser = TerminalModeParserState::Escape;
                        return;
                    }
                    '?' if csi.chars_seen == 1 => csi.private_marker = true,
                    '0'..='9' => {
                        let digit = character as u32 - '0' as u32;
                        csi.current_parameter = match csi.current_parameter {
                            Some(value) => value
                                .checked_mul(10)
                                .and_then(|value| value.checked_add(digit)),
                            None => Some(digit),
                        };
                        if csi.current_parameter.is_none() {
                            csi.invalid = true;
                        }
                    }
                    ';' => csi.finish_parameter(),
                    'h' | 'l' => {
                        csi.finish_parameter();
                        if csi.private_marker && csi.contains_bracketed_paste && !csi.invalid {
                            self.mode = if character == 'h' {
                                BracketedPasteMode::Enabled
                            } else {
                                BracketedPasteMode::Disabled
                            };
                        }
                        self.parser = TerminalModeParserState::Ground;
                        return;
                    }
                    '\u{40}'..='\u{7e}' => {
                        self.parser = TerminalModeParserState::Ground;
                        return;
                    }
                    '\u{20}'..='\u{3f}' => csi.invalid = true,
                    _ => {
                        self.parser = TerminalModeParserState::Ground;
                        return;
                    }
                }
                self.parser = TerminalModeParserState::Csi(csi);
            }
            TerminalModeParserState::CsiDiscard => self.observe_csi_discard(character),
            TerminalModeParserState::String { kind, chars_seen } => {
                self.observe_string(character, kind, Some(chars_seen));
            }
            TerminalModeParserState::StringDiscard { kind } => {
                self.observe_string(character, kind, None);
            }
        }
    }

    fn observe_global_transition(&mut self, character: char) -> bool {
        self.parser = match character {
            '\u{0018}'
            | '\u{001a}'
            | '\u{0080}'..='\u{008f}'
            | '\u{0091}'..='\u{0097}'
            | '\u{0099}'..='\u{009a}'
            | '\u{009c}' => TerminalModeParserState::Ground,
            '\u{009b}' => TerminalModeParserState::Csi(DecPrivateModeParser::new()),
            '\u{009d}' => Self::terminal_string(TerminalStringKind::Osc),
            '\u{0090}' => Self::terminal_string(TerminalStringKind::Dcs),
            '\u{0098}' => Self::terminal_string(TerminalStringKind::Sos),
            '\u{009e}' => Self::terminal_string(TerminalStringKind::Pm),
            '\u{009f}' => Self::terminal_string(TerminalStringKind::Apc),
            _ => return false,
        };
        true
    }

    fn observe_ground(&mut self, character: char) {
        self.parser = match character {
            '\u{001b}' => TerminalModeParserState::Escape,
            _ => TerminalModeParserState::Ground,
        };
    }

    fn observe_escape(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            return;
        }
        self.parser = match character {
            '[' => TerminalModeParserState::Csi(DecPrivateModeParser::new()),
            ']' => Self::terminal_string(TerminalStringKind::Osc),
            'P' => Self::terminal_string(TerminalStringKind::Dcs),
            'X' => Self::terminal_string(TerminalStringKind::Sos),
            '^' => Self::terminal_string(TerminalStringKind::Pm),
            '_' => Self::terminal_string(TerminalStringKind::Apc),
            '\u{001b}' => TerminalModeParserState::Escape,
            _ => TerminalModeParserState::Ground,
        };
    }

    fn observe_csi_discard(&mut self, character: char) {
        if Self::is_c0_executable_or_del(character) {
            return;
        }
        self.parser = match character {
            '\u{001b}' => TerminalModeParserState::Escape,
            '\u{40}'..='\u{7e}' => TerminalModeParserState::Ground,
            _ => TerminalModeParserState::CsiDiscard,
        };
    }

    fn observe_string(
        &mut self,
        character: char,
        kind: TerminalStringKind,
        chars_seen: Option<u16>,
    ) {
        match character {
            '\u{0007}' if kind == TerminalStringKind::Osc => {
                self.parser = TerminalModeParserState::Ground;
            }
            '\u{001b}' => self.parser = TerminalModeParserState::Escape,
            _ => match chars_seen {
                Some(chars_seen) => {
                    let chars_seen = chars_seen.saturating_add(1);
                    if chars_seen > TERMINAL_MODE_CONTROL_MAX_CHARS {
                        self.parser = TerminalModeParserState::StringDiscard { kind };
                    } else {
                        self.parser = TerminalModeParserState::String { kind, chars_seen };
                    }
                }
                None => self.parser = TerminalModeParserState::StringDiscard { kind },
            },
        }
    }

    fn terminal_string(kind: TerminalStringKind) -> TerminalModeParserState {
        TerminalModeParserState::String {
            kind,
            chars_seen: 0,
        }
    }

    fn is_c0_executable_or_del(character: char) -> bool {
        matches!(
            character,
            '\u{0000}'..='\u{0017}'
                | '\u{0019}'
                | '\u{001c}'..='\u{001f}'
                | '\u{007f}'
        )
    }
}

struct SidebandTimeouts;

impl SidebandTimeouts {
    fn write_budget() -> Duration {
        Duration::from_secs(20)
    }
}

trait PaneProcess: Send + Sync {
    fn pid(&self) -> u32;
    fn is_alive(&self) -> Result<bool>;
    fn belongs_to(&self, pty: &dyn PtySession) -> Result<bool>;
}

#[cfg(windows)]
struct WindowsPaneProcess {
    process: PinnedProcess,
}

#[cfg(windows)]
impl WindowsPaneProcess {
    fn open(process_id: u32) -> Result<Self> {
        Ok(Self {
            process: PinnedProcess::open(process_id)?,
        })
    }
}

#[cfg(windows)]
impl PaneProcess for WindowsPaneProcess {
    fn pid(&self) -> u32 {
        self.process.pid()
    }

    fn is_alive(&self) -> Result<bool> {
        self.process.is_alive()
    }

    fn belongs_to(&self, pty: &dyn PtySession) -> Result<bool> {
        pty.contains_process(&self.process)
    }
}

#[derive(Clone)]
struct PaneCaller {
    session: String,
    session_id: Uuid,
    generation: SessionGeneration,
    run_id: Uuid,
    process: Arc<dyn PaneProcess>,
}

#[derive(Debug)]
struct AutoRestartOnStallConfig {
    allowed_sessions: RwLock<HashSet<SessionId>>,
    threshold: Duration,
}

impl AutoRestartOnStallConfig {
    fn enabled_for(&self, session_id: SessionId) -> bool {
        self.allowed_sessions.read().contains(&session_id)
    }
}

#[derive(Debug, Default)]
struct AutoRestartHistory {
    attempts: Vec<Instant>,
    disabled: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AutoRestartReservation {
    Reserved(Instant),
    CapReached,
    Disabled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StallAlertSnapshot {
    session_id: SessionId,
    last_work_state: Option<WorkState>,
    last_session_state: Option<LifecycleState>,
}

struct StallDetector {
    generation: SessionGeneration,
    run_id: Uuid,
    state: WorkState,
    entered_at: Instant,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DeliveryWriteResult {
    bytes_written: usize,
    payload_part_count: u32,
}

#[derive(Clone)]
struct RunWriteTarget {
    session: String,
    session_id: Uuid,
    run_id: Uuid,
    generation: SessionGeneration,
    pty: Arc<dyn PtySession>,
    input_gate: Arc<RunInputGate>,
}

struct PlannedDelivery {
    target: RunWriteTarget,
    submit_behavior: SubmitBehavior,
    payload: String,
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
    fn from_slot(slot: &SessionSlot) -> Self {
        let mut decision = Self {
            target_lifecycle_state_before: slot.state,
            target_work_state_before: slot.work_state_observed.then_some(slot.work_state),
            target_last_activity_at: slot.last_activity_at.clone(),
            last_route_from_target_at: slot.last_route_from_session_at.clone(),
            last_route_from_target_instant: slot.last_route_from_session_instant,
            reason: None,
        };
        decision.reason = dispatch_overlap_reason(&decision);
        decision
    }

    fn overlap(&self) -> bool {
        self.reason.is_some()
    }
}

trait PtySpawner: Send + Sync {
    fn spawn(&self, plan: &PreparedLaunch, handler: PtyEventHandler)
    -> Result<Box<dyn PtySession>>;
}

#[derive(Debug, Clone)]
struct PreparedLaunch {
    spec: LaunchSpec,
    wsl_scope: Option<WslRunScope>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WslRunScope {
    distro: String,
    unit: String,
}

trait WslControl: Send + Sync {
    fn reconcile_stale_scopes(&self) -> Result<()>;
    fn default_working_directory(&self) -> Result<String>;
    fn qualify_working_directory(&self, candidate: &str) -> Result<QualifiedWorkingDirectory>;
    fn revalidate_working_directory(&self, expected: &QualifiedWorkingDirectory) -> Result<()>;
    fn resolve_prime_executable(&self) -> Result<String>;
    fn wsl_executable(&self) -> Result<String>;
    fn confirm_scope_started(&self, scope: &WslRunScope) -> Result<()>;
    fn terminate_scope(&self, scope: &WslRunScope) -> Result<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedLaunchProgram {
    program: String,
    prefix_args: Vec<String>,
}

trait DriverExecutableResolver: Send + Sync {
    fn resolve(&self, driver: DriverKind) -> Result<ResolvedLaunchProgram>;
}

struct HostDriverExecutableResolver;

impl DriverExecutableResolver for HostDriverExecutableResolver {
    fn resolve(&self, driver: DriverKind) -> Result<ResolvedLaunchProgram> {
        resolve_driver_executable(driver)
    }
}

struct ConcretePtySpawner {
    wsl: Arc<dyn WslControl>,
}

impl PtySpawner for ConcretePtySpawner {
    fn spawn(
        &self,
        plan: &PreparedLaunch,
        handler: PtyEventHandler,
    ) -> Result<Box<dyn PtySession>> {
        let session = ConcretePtySession::spawn(&plan.spec, handler)?;
        let Some(scope) = plan.wsl_scope.clone() else {
            return Ok(Box::new(session));
        };

        // ConPTY requests a cursor-position report before the WSL payload runs.
        // The renderer cannot answer until the session is installed, so the
        // supervisor releases this one measured startup handshake itself.
        if let Err(error) = session.send_input("\u{1b}[1;1R") {
            let _ = self.wsl.terminate_scope(&scope);
            let _ = session.kill();
            return Err(anyhow!(error).context("failed to release Prime ConPTY startup handshake"));
        }
        if let Err(start_error) = self.wsl.confirm_scope_started(&scope) {
            let scope_cleanup = self.wsl.terminate_scope(&scope);
            let job_cleanup = session.kill();
            let mut error = format!("Prime WSL scope failed start-time binding: {start_error:#}");
            if let Err(cleanup_error) = scope_cleanup {
                error.push_str(&format!("; scope cleanup failed: {cleanup_error:#}"));
            }
            if let Err(cleanup_error) = job_cleanup {
                error.push_str(&format!("; Windows job cleanup failed: {cleanup_error:#}"));
            }
            return Err(anyhow!(error));
        }

        Ok(Box::new(WslScopedPtySession {
            inner: Box::new(session),
            control: Arc::clone(&self.wsl),
            scope,
        }))
    }
}

struct WslScopedPtySession {
    inner: Box<dyn PtySession>,
    control: Arc<dyn WslControl>,
    scope: WslRunScope,
}

impl PtySession for WslScopedPtySession {
    fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
        self.inner.send_input(input)
    }

    fn cancel_input_write(&self) -> Result<()> {
        self.inner.cancel_input_write()
    }

    fn resize(&self, cols: u16, rows: u16) -> Result<()> {
        self.inner.resize(cols, rows)
    }

    fn kill(&self) -> Result<()> {
        let scope_result = self.control.terminate_scope(&self.scope);
        let job_result = self.inner.kill();
        match (scope_result, job_result) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(scope), Ok(())) => Err(scope.context("failed to prove Prime WSL scope empty")),
            (Ok(()), Err(job)) => Err(job.context("failed to prove Prime Windows job empty")),
            (Err(scope), Err(job)) => Err(anyhow!(
                "failed to prove Prime WSL scope empty: {scope:#}; failed to prove Prime Windows job empty: {job:#}"
            )),
        }
    }

    fn try_wait(&self) -> Result<Option<PtyExitStatus>> {
        self.inner.try_wait()
    }

    fn process_id(&self) -> Option<u32> {
        self.inner.process_id()
    }

    fn contains_process_id(&self, _process_id: u32) -> Result<bool> {
        // Prime/WSL has no verified Windows Job -> Linux task identity bridge.
        // It is deliberately ineligible for the native pane sideband.
        Ok(false)
    }

    #[cfg(windows)]
    fn contains_process(&self, _process: &PinnedProcess) -> Result<bool> {
        Ok(false)
    }

    fn note_real_output(&self, _driver: DriverKind) {
        self.inner.note_real_output(DriverKind::GenericTerminal);
    }

    fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
        self.inner.agent_alive(DriverKind::GenericTerminal)
    }
}

impl Drop for WslScopedPtySession {
    fn drop(&mut self) {
        let _ = self.control.terminate_scope(&self.scope);
        let _ = self.inner.kill();
    }
}

const WSL_CONTROL_COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const WSL_SCOPE_START_TIMEOUT: Duration = Duration::from_secs(8);
const WSL_SCOPE_STOP_TIMEOUT: Duration = Duration::from_secs(5);

struct ConcreteWslControl {
    wsl_executable: String,
    runtime_dir: PathBuf,
}

struct WslCommandResult {
    success: bool,
    exit_code: u32,
    output: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LinuxDirectoryProbe {
    path: String,
    device: u64,
    inode: u64,
    directory: bool,
}

#[derive(Debug)]
struct WslScopeStatus {
    active_state: String,
    tasks_current: u64,
}

impl ConcreteWslControl {
    fn new(runtime_dir: PathBuf) -> Self {
        let system_root = std::env::var_os("SystemRoot")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        let candidate = system_root.join("System32").join("wsl.exe");
        let wsl_executable = fs::canonicalize(&candidate)
            .map(|path| child_process_path(&path))
            .unwrap_or(candidate)
            .to_string_lossy()
            .into_owned();
        Self {
            wsl_executable,
            runtime_dir,
        }
    }

    fn run_command(&self, linux_args: &[String], timeout: Duration) -> Result<WslCommandResult> {
        #[cfg(not(windows))]
        {
            let _ = (linux_args, timeout);
            return Err(anyhow!("Prime/WSL is supported only on Windows"));
        }

        #[cfg(windows)]
        {
            if !Path::new(&self.wsl_executable).is_file() {
                return Err(anyhow!(
                    "WSL executable is unavailable at {}",
                    self.wsl_executable
                ));
            }
            let mut args = vec![
                "-d".to_owned(),
                driver_prime::WSL_DISTRO.to_owned(),
                "--exec".to_owned(),
            ];
            args.extend(linux_args.iter().cloned());
            let working_dir = Path::new(&self.wsl_executable)
                .parent()
                .unwrap_or_else(|| Path::new(r"C:\Windows\System32"))
                .to_string_lossy()
                .into_owned();
            let spec = LaunchSpec {
                program: self.wsl_executable.clone(),
                args,
                working_dir,
                env: vec![EnvVar {
                    key: "WSLENV".into(),
                    value: String::new(),
                }],
                display_name: "PRIM-1 WSL control".into(),
            };
            let output = Arc::new(Mutex::new(String::new()));
            let captured = Arc::clone(&output);
            let reader_closed = Arc::new(AtomicBool::new(false));
            let closed = Arc::clone(&reader_closed);
            let handler: PtyEventHandler = Arc::new(move |event| match event {
                PtyEvent::Output(chunk) => captured.lock().push_str(&chunk),
                PtyEvent::Closed => closed.store(true, Ordering::Release),
                PtyEvent::Error(error) => {
                    let mut captured = captured.lock();
                    captured.push_str("\n[PTY error: ");
                    captured.push_str(&error);
                    captured.push_str("]\n");
                    closed.store(true, Ordering::Release);
                }
            });
            let session = ConcretePtySession::spawn(&spec, handler)
                .context("failed to start bounded WSL control command")?;
            session
                .send_input("\u{1b}[1;1R")
                .map_err(anyhow::Error::from)
                .context("failed to release WSL control ConPTY startup handshake")?;

            let deadline = Instant::now() + timeout;
            let status = loop {
                if let Some(status) = session.try_wait()? {
                    break status;
                }
                if Instant::now() >= deadline {
                    let cleanup = session.kill();
                    return match cleanup {
                        Ok(()) => Err(anyhow!("WSL control command timed out after {timeout:?}")),
                        Err(error) => Err(anyhow!(
                            "WSL control command timed out after {timeout:?}; Windows job cleanup failed: {error:#}"
                        )),
                    };
                }
                thread::sleep(Duration::from_millis(20));
            };

            session
                .kill()
                .context("failed to prove WSL control command Windows job empty")?;
            let drain_deadline = Instant::now() + Duration::from_secs(1);
            while !reader_closed.load(Ordering::Acquire) && Instant::now() < drain_deadline {
                thread::sleep(Duration::from_millis(10));
            }
            let output = strip_terminal_control_sequences(&output.lock())
                .trim()
                .to_owned();
            Ok(WslCommandResult {
                success: status.success,
                exit_code: status.exit_code,
                output,
            })
        }
    }

    fn run_checked(&self, linux_args: &[String], context: &str) -> Result<String> {
        let result = self.run_command(linux_args, WSL_CONTROL_COMMAND_TIMEOUT)?;
        if !result.success {
            return Err(anyhow!(
                "{context} failed with exit code {}{}",
                result.exit_code,
                bounded_command_output(&result.output)
            ));
        }
        Ok(result.output)
    }

    fn query_directory(&self, candidate: &str) -> Result<LinuxDirectoryProbe> {
        validate_linux_candidate(candidate)?;
        const SCRIPT: &str = "import json,os,stat,sys; p=os.path.realpath(sys.argv[1]); s=os.stat(p); print(json.dumps({'path':p,'device':s.st_dev,'inode':s.st_ino,'directory':stat.S_ISDIR(s.st_mode)},separators=(',',':')))";
        let output = self.run_checked(
            &[
                driver_prime::PYTHON.into(),
                "-c".into(),
                SCRIPT.into(),
                candidate.into(),
            ],
            "Linux working-directory qualification",
        )?;
        let line = output
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .ok_or_else(|| anyhow!("Linux directory probe returned no identity record"))?;
        let probe: LinuxDirectoryProbe = serde_json::from_str(line)
            .context("failed to decode Linux directory identity record")?;
        validate_linux_candidate(&probe.path)?;
        if !probe.directory {
            return Err(anyhow!(
                "selected Linux working directory is not a directory: {}",
                probe.path
            ));
        }
        Ok(probe)
    }

    fn runtime_path_in_wsl(&self) -> Result<String> {
        let output = self.run_checked(
            &[
                "/usr/bin/wslpath".into(),
                "-a".into(),
                "-u".into(),
                self.runtime_dir.to_string_lossy().into_owned(),
            ],
            "runtime-path translation into Ubuntu",
        )?;
        let translated = output
            .lines()
            .rev()
            .map(str::trim)
            .find(|line| line.starts_with('/'))
            .ok_or_else(|| anyhow!("wslpath returned no absolute Linux path"))?;
        Ok(translated.to_owned())
    }

    fn scope_status(&self, scope: &WslRunScope) -> Result<Option<WslScopeStatus>> {
        validate_wsl_scope(scope)?;
        let result = self.run_command(
            &[
                driver_prime::SYSTEMCTL.into(),
                "--user".into(),
                "show".into(),
                format!("{}.service", scope.unit),
                "--property=LoadState".into(),
                "--property=ActiveState".into(),
                "--property=TasksCurrent".into(),
                "--no-pager".into(),
            ],
            WSL_CONTROL_COMMAND_TIMEOUT,
        )?;
        if !result.success {
            let lower = result.output.to_ascii_lowercase();
            if lower.contains("not found")
                || lower.contains("could not be found")
                || lower.contains("not loaded")
            {
                return Ok(None);
            }
            return Err(anyhow!(
                "failed to inspect Prime scope '{}' (exit {}){}",
                scope.unit,
                result.exit_code,
                bounded_command_output(&result.output)
            ));
        }
        parse_wsl_scope_status(&result.output)
    }
}

fn parse_wsl_scope_status(output: &str) -> Result<Option<WslScopeStatus>> {
    let mut load_state = None;
    let mut active_state = None;
    let mut tasks_current = None;
    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "LoadState" => load_state = Some(value),
            "ActiveState" => active_state = Some(value),
            "TasksCurrent" => tasks_current = Some(value),
            _ => {}
        }
    }
    let load_state = load_state.ok_or_else(|| anyhow!("Prime scope status omitted LoadState"))?;
    if load_state == "not-found" {
        return Ok(None);
    }
    let active_state = active_state
        .ok_or_else(|| anyhow!("Prime scope status omitted ActiveState"))?
        .to_owned();
    let tasks_current = match tasks_current {
        None | Some("") | Some("[not set]") => 0,
        Some(value) => value
            .parse::<u64>()
            .with_context(|| format!("invalid Prime scope TasksCurrent value '{value}'"))?,
    };
    Ok(Some(WslScopeStatus {
        active_state,
        tasks_current,
    }))
}

impl WslControl for ConcreteWslControl {
    fn reconcile_stale_scopes(&self) -> Result<()> {
        #[cfg(not(windows))]
        return Ok(());

        #[cfg(windows)]
        {
            if !Path::new(&self.wsl_executable).is_file() {
                return Ok(());
            }
            let result = self.run_command(
                &[
                    driver_prime::SYSTEMCTL.into(),
                    "--user".into(),
                    "list-units".into(),
                    "--all".into(),
                    "--plain".into(),
                    "--full".into(),
                    "--no-legend".into(),
                    "--no-pager".into(),
                    format!("{}*.service", driver_prime::SCOPE_UNIT_PREFIX),
                ],
                WSL_CONTROL_COMMAND_TIMEOUT,
            )?;
            if !result.success {
                let lower = result.output.to_ascii_lowercase();
                if lower.contains("no distribution") || lower.contains("distribution was not found")
                {
                    return Ok(());
                }
                return Err(anyhow!(
                    "failed to enumerate stale Prime scopes (exit {}){}",
                    result.exit_code,
                    bounded_command_output(&result.output)
                ));
            }
            for service in result
                .output
                .lines()
                .filter_map(|line| line.split_whitespace().next())
            {
                let Some(unit) = service.strip_suffix(".service") else {
                    continue;
                };
                if !valid_wsl_unit(unit) {
                    continue;
                }
                self.terminate_scope(&WslRunScope {
                    distro: driver_prime::WSL_DISTRO.into(),
                    unit: unit.into(),
                })?;
            }
            Ok(())
        }
    }

    fn default_working_directory(&self) -> Result<String> {
        const SCRIPT: &str = "import json,os,pwd; print(json.dumps(os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir)))";
        let output = self.run_checked(
            &[driver_prime::PYTHON.into(), "-c".into(), SCRIPT.into()],
            "Prime home-directory discovery",
        )?;
        let value = output
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('"'))
            .ok_or_else(|| anyhow!("Prime home-directory probe returned no path"))?;
        let path: String =
            serde_json::from_str(value).context("failed to decode Prime home-directory path")?;
        validate_linux_candidate(&path)?;
        Ok(path)
    }

    fn qualify_working_directory(&self, candidate: &str) -> Result<QualifiedWorkingDirectory> {
        let selected = self.query_directory(candidate)?;
        let runtime = self.query_directory(&self.runtime_path_in_wsl()?)?;
        if linux_paths_overlap(&selected.path, &runtime.path) {
            return Err(anyhow!(
                "Linux working directory and runtime storage must be disjoint"
            ));
        }
        Ok(QualifiedWorkingDirectory {
            namespace: WorkingDirectoryNamespace::WslUbuntu,
            canonical_path: selected.path,
            identity: format!("wsl_ubuntu:{}:{}", selected.device, selected.inode),
        })
    }

    fn revalidate_working_directory(&self, expected: &QualifiedWorkingDirectory) -> Result<()> {
        if expected.namespace != WorkingDirectoryNamespace::WslUbuntu {
            return Err(anyhow!("Prime session has a non-WSL working directory"));
        }
        let actual = self.qualify_working_directory(&expected.canonical_path)?;
        if actual != *expected {
            return Err(anyhow!(
                "Prime Linux working directory changed since it was selected; choose it again (expected identity {}, actual identity {})",
                expected.identity,
                actual.identity
            ));
        }
        Ok(())
    }

    fn resolve_prime_executable(&self) -> Result<String> {
        const SCRIPT: &str = "import json,os,pwd,shutil; h=pwd.getpwuid(os.getuid()).pw_dir; p=shutil.which('prime-agent') or os.path.join(h,'.npm-global','bin','prime-agent'); print(json.dumps(os.path.realpath(p) if os.path.isfile(p) and os.access(p,os.X_OK) else None))";
        let output = self.run_checked(
            &[driver_prime::PYTHON.into(), "-c".into(), SCRIPT.into()],
            "Prime executable discovery",
        )?;
        let value = output
            .lines()
            .rev()
            .find(|line| matches!(line.trim_start().chars().next(), Some('"' | 'n')))
            .ok_or_else(|| anyhow!("Prime executable probe returned no result"))?;
        let path: Option<String> =
            serde_json::from_str(value).context("failed to decode Prime executable path")?;
        let path = path.ok_or_else(|| anyhow!("prime-agent is not installed in Ubuntu"))?;
        validate_linux_candidate(&path)?;
        Ok(path)
    }

    fn wsl_executable(&self) -> Result<String> {
        if !Path::new(&self.wsl_executable).is_file() {
            return Err(anyhow!(
                "WSL executable is unavailable at {}",
                self.wsl_executable
            ));
        }
        Ok(self.wsl_executable.clone())
    }

    fn confirm_scope_started(&self, scope: &WslRunScope) -> Result<()> {
        validate_wsl_scope(scope)?;
        let deadline = Instant::now() + WSL_SCOPE_START_TIMEOUT;
        let mut last = None;
        while Instant::now() < deadline {
            match self.scope_status(scope) {
                Ok(Some(status))
                    if status.active_state == "active" && status.tasks_current >= 2 =>
                {
                    return Ok(());
                }
                Ok(status) => last = status.map(|value| format!("{value:?}")),
                Err(error) => last = Some(format!("{error:#}")),
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(anyhow!(
            "Prime scope '{}' did not become active with at least two tasks{}",
            scope.unit,
            last.map(|value| format!(": {value}")).unwrap_or_default()
        ))
    }

    fn terminate_scope(&self, scope: &WslRunScope) -> Result<()> {
        validate_wsl_scope(scope)?;
        let service = format!("{}.service", scope.unit);
        let _ = self.run_command(
            &[
                driver_prime::SYSTEMCTL.into(),
                "--user".into(),
                "stop".into(),
                service.clone(),
                "--no-block".into(),
            ],
            WSL_CONTROL_COMMAND_TIMEOUT,
        );
        let deadline = Instant::now() + WSL_SCOPE_STOP_TIMEOUT;
        let mut kill_sent = false;
        loop {
            match self.scope_status(scope) {
                Ok(None) => return Ok(()),
                Ok(Some(status))
                    if status.tasks_current == 0
                        && !matches!(status.active_state.as_str(), "active" | "activating") =>
                {
                    return Ok(());
                }
                Ok(Some(_)) | Err(_) if Instant::now() < deadline => {
                    if !kill_sent && Instant::now() + Duration::from_secs(2) >= deadline {
                        let _ = self.run_command(
                            &[
                                driver_prime::SYSTEMCTL.into(),
                                "--user".into(),
                                "kill".into(),
                                "--signal=KILL".into(),
                                "--kill-whom=all".into(),
                                service.clone(),
                            ],
                            WSL_CONTROL_COMMAND_TIMEOUT,
                        );
                        kill_sent = true;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                Ok(Some(status)) => {
                    return Err(anyhow!(
                        "Prime scope '{}' still owns {} task(s) in state '{}'",
                        scope.unit,
                        status.tasks_current,
                        status.active_state
                    ));
                }
                Err(error) => {
                    return Err(error.context(format!(
                        "failed to prove Prime scope '{}' empty",
                        scope.unit
                    )));
                }
            }
        }
    }
}

fn bounded_command_output(output: &str) -> String {
    if output.is_empty() {
        String::new()
    } else {
        let truncated = output.chars().take(512).collect::<String>();
        format!(": {truncated}")
    }
}

fn validate_linux_candidate(candidate: &str) -> Result<()> {
    if candidate.trim() != candidate
        || !candidate.starts_with('/')
        || candidate.chars().any(char::is_control)
    {
        return Err(anyhow!(
            "Prime Linux working directory must be a control-free absolute Ubuntu path"
        ));
    }
    Ok(())
}

fn linux_paths_overlap(left: &str, right: &str) -> bool {
    let left = left.trim_end_matches('/');
    let right = right.trim_end_matches('/');
    left == right
        || left
            .strip_prefix(right)
            .is_some_and(|rest| rest.starts_with('/'))
        || right
            .strip_prefix(left)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn valid_wsl_unit(unit: &str) -> bool {
    unit.strip_prefix(driver_prime::SCOPE_UNIT_PREFIX)
        .is_some_and(|suffix| {
            suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
}

fn validate_wsl_scope(scope: &WslRunScope) -> Result<()> {
    if scope.distro != driver_prime::WSL_DISTRO || !valid_wsl_unit(&scope.unit) {
        return Err(anyhow!("invalid supervisor-owned Prime WSL scope"));
    }
    Ok(())
}

fn parse_wsl_identity(identity: &str) -> Result<(u64, u64)> {
    let mut parts = identity.split(':');
    if parts.next() != Some("wsl_ubuntu") {
        return Err(anyhow!("invalid Prime Linux directory identity"));
    }
    let device = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| anyhow!("invalid Prime Linux directory device identity"))?;
    let inode = parts
        .next()
        .and_then(|part| part.parse::<u64>().ok())
        .ok_or_else(|| anyhow!("invalid Prime Linux directory inode identity"))?;
    if parts.next().is_some() {
        return Err(anyhow!("invalid Prime Linux directory identity"));
    }
    Ok((device, inode))
}

#[cfg(test)]
struct TestWslUnavailable;

#[cfg(test)]
impl WslControl for TestWslUnavailable {
    fn reconcile_stale_scopes(&self) -> Result<()> {
        Ok(())
    }
    fn default_working_directory(&self) -> Result<String> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn qualify_working_directory(&self, _candidate: &str) -> Result<QualifiedWorkingDirectory> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn revalidate_working_directory(&self, _expected: &QualifiedWorkingDirectory) -> Result<()> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn resolve_prime_executable(&self) -> Result<String> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn wsl_executable(&self) -> Result<String> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn confirm_scope_started(&self, _scope: &WslRunScope) -> Result<()> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
    fn terminate_scope(&self, _scope: &WslRunScope) -> Result<()> {
        Err(anyhow!("Prime/WSL test control is not configured"))
    }
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub working_root: PathBuf,
    pub runtime_dir: PathBuf,
    pub pane_mcp_executable: Option<PathBuf>,
    pub heartbeat_interval: Option<Duration>,
    pub auto_restart_on_stall_sessions: Option<Vec<SessionId>>,
    pub auto_restart_stall_threshold: Option<Duration>,
}

struct AuditInner {
    path: PathBuf,
    active_date: NaiveDate,
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
}

#[derive(Clone)]
struct RunningSession {
    pty: Option<Arc<dyn PtySession>>,
    input_gate: Arc<RunInputGate>,
}

impl RunningSession {
    fn new(pty: Option<Arc<dyn PtySession>>) -> Self {
        Self {
            pty,
            input_gate: Arc::new(RunInputGate::new()),
        }
    }

    fn close_input(&self) {
        self.input_gate.close();
    }
}

struct RunInputGate {
    state: Mutex<RunInputState>,
    ready: Condvar,
}

struct RunInputState {
    accepting: bool,
    active: bool,
    cancellation_barrier: bool,
    next_ticket: u64,
    queue: VecDeque<u64>,
}

struct RunInputPermit<'a> {
    gate: &'a RunInputGate,
    control: Option<&'a InputWriteControl>,
}

const INPUT_WRITE_PENDING: u8 = 0;
const INPUT_WRITE_ACTIVE: u8 = 1;
const INPUT_WRITE_CANCELLED_BEFORE_START: u8 = 2;
const INPUT_WRITE_CANCELLING: u8 = 3;
const INPUT_WRITE_FINISHED: u8 = 4;
const INPUT_WRITE_FINISHED_AFTER_CANCEL: u8 = 5;

struct InputWriteControl {
    state: AtomicU8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputCancelDisposition {
    BeforeStart,
    InFlight,
    Finished,
}

struct RunEventPublishState {
    next_sequence: Option<u64>,
    pending: BTreeMap<u64, RuntimeEvent>,
    draining: bool,
}

impl RunEventPublishState {
    fn new(next_sequence: u64) -> Self {
        Self {
            next_sequence: Some(next_sequence),
            pending: BTreeMap::new(),
            draining: false,
        }
    }
}

impl RunInputGate {
    fn new() -> Self {
        Self {
            state: Mutex::new(RunInputState {
                accepting: true,
                active: false,
                cancellation_barrier: false,
                next_ticket: 0,
                queue: VecDeque::new(),
            }),
            ready: Condvar::new(),
        }
    }

    fn begin_write<'a>(
        &'a self,
        control: Option<&'a InputWriteControl>,
    ) -> Result<RunInputPermit<'a>> {
        let mut state = self.state.lock();
        if !state.accepting {
            return Err(anyhow!("run is closed for input"));
        }
        let ticket = state.next_ticket;
        state.next_ticket = state
            .next_ticket
            .checked_add(1)
            .ok_or_else(|| anyhow!("run input ticket space exhausted"))?;
        state.queue.push_back(ticket);

        loop {
            if control.is_some_and(InputWriteControl::cancelled_before_start) {
                if let Some(position) = state.queue.iter().position(|queued| *queued == ticket) {
                    state.queue.remove(position);
                }
                self.ready.notify_all();
                return Err(anyhow!("input write cancelled before PTY input began"));
            }
            if !state.accepting {
                if let Some(position) = state.queue.iter().position(|queued| *queued == ticket) {
                    state.queue.remove(position);
                }
                self.ready.notify_all();
                return Err(anyhow!("run is closed for input"));
            }
            if !state.active && !state.cancellation_barrier && state.queue.front() == Some(&ticket)
            {
                state.queue.pop_front();
                state.active = true;
                return Ok(RunInputPermit {
                    gate: self,
                    control,
                });
            }
            self.ready.wait(&mut state);
        }
    }

    fn close(&self) {
        let mut state = self.state.lock();
        state.accepting = false;
        self.ready.notify_all();
    }

    fn is_accepting(&self) -> bool {
        self.state.lock().accepting
    }

    fn wake_waiters(&self) {
        self.ready.notify_all();
    }

    fn begin_cancellation_barrier(&self) {
        self.state.lock().cancellation_barrier = true;
    }

    fn end_cancellation_barrier(&self) {
        let mut state = self.state.lock();
        state.cancellation_barrier = false;
        self.ready.notify_all();
    }

    fn wait_until_idle_timeout(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while state.active || !state.queue.is_empty() {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            self.ready.wait_for(&mut state, deadline - now);
        }
        true
    }

    #[cfg(test)]
    fn queued_writes(&self) -> usize {
        self.state.lock().queue.len()
    }
}

struct TerminationAttempt {
    kill_error: Option<String>,
    exit_status: Option<PtyExitStatus>,
    exit_poll_error: Option<String>,
    input_idle: bool,
}

enum BoundedTerminationAttempt {
    Completed(TerminationAttempt),
    TimedOut(mpsc::Receiver<TerminationAttempt>),
}

fn begin_termination_attempt(
    pty: Arc<dyn PtySession>,
    input_gate: Arc<RunInputGate>,
    deadline: Instant,
) -> mpsc::Receiver<TerminationAttempt> {
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let kill_error = pty.kill().err().map(|error| error.to_string());
        let (exit_status, exit_poll_error) = match pty.try_wait() {
            Ok(status) => (status, None),
            Err(error) => (None, Some(error.to_string())),
        };
        let input_idle = kill_error.is_none()
            && input_gate
                .wait_until_idle_timeout(deadline.saturating_duration_since(Instant::now()));
        let _ = tx.send(TerminationAttempt {
            kill_error,
            exit_status,
            exit_poll_error,
            input_idle,
        });
    });
    rx
}

fn terminate_running_session_bounded(
    pty: Arc<dyn PtySession>,
    input_gate: Arc<RunInputGate>,
    timeout: Duration,
) -> BoundedTerminationAttempt {
    let deadline = Instant::now() + timeout;
    let rx = begin_termination_attempt(pty, input_gate, deadline);
    match rx.recv_timeout(timeout) {
        Ok(attempt) => BoundedTerminationAttempt::Completed(attempt),
        Err(_) => BoundedTerminationAttempt::TimedOut(rx),
    }
}

impl InputWriteControl {
    fn new() -> Self {
        Self {
            state: AtomicU8::new(INPUT_WRITE_PENDING),
        }
    }

    fn begin_pty_write(&self) -> Result<()> {
        match self.state.compare_exchange(
            INPUT_WRITE_PENDING,
            INPUT_WRITE_ACTIVE,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(()),
            Err(INPUT_WRITE_CANCELLED_BEFORE_START) => {
                Err(anyhow!("input write cancelled before PTY input began"))
            }
            Err(state) => Err(anyhow!(
                "input write entered an invalid cancellation state {state}"
            )),
        }
    }

    fn finish(&self) {
        loop {
            let state = self.state.load(Ordering::Acquire);
            let finished = match state {
                INPUT_WRITE_ACTIVE => INPUT_WRITE_FINISHED,
                INPUT_WRITE_CANCELLING => INPUT_WRITE_FINISHED_AFTER_CANCEL,
                INPUT_WRITE_CANCELLED_BEFORE_START => INPUT_WRITE_FINISHED_AFTER_CANCEL,
                INPUT_WRITE_FINISHED | INPUT_WRITE_FINISHED_AFTER_CANCEL => return,
                other => panic!("invalid input-write completion state {other}"),
            };
            if self
                .state
                .compare_exchange(state, finished, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn request_cancel(&self) -> InputCancelDisposition {
        loop {
            let state = self.state.load(Ordering::Acquire);
            match state {
                INPUT_WRITE_PENDING => {
                    if self
                        .state
                        .compare_exchange(
                            INPUT_WRITE_PENDING,
                            INPUT_WRITE_CANCELLED_BEFORE_START,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return InputCancelDisposition::BeforeStart;
                    }
                }
                INPUT_WRITE_ACTIVE => {
                    if self
                        .state
                        .compare_exchange(
                            INPUT_WRITE_ACTIVE,
                            INPUT_WRITE_CANCELLING,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return InputCancelDisposition::InFlight;
                    }
                }
                INPUT_WRITE_CANCELLED_BEFORE_START => {
                    return InputCancelDisposition::BeforeStart;
                }
                INPUT_WRITE_CANCELLING => return InputCancelDisposition::InFlight,
                INPUT_WRITE_FINISHED | INPUT_WRITE_FINISHED_AFTER_CANCEL => {
                    return InputCancelDisposition::Finished;
                }
                other => panic!("invalid input-write cancellation state {other}"),
            }
        }
    }

    fn cancelled_before_start(&self) -> bool {
        self.state.load(Ordering::Acquire) == INPUT_WRITE_CANCELLED_BEFORE_START
    }

    fn cancellation_was_requested(&self) -> bool {
        matches!(
            self.state.load(Ordering::Acquire),
            INPUT_WRITE_CANCELLING | INPUT_WRITE_FINISHED_AFTER_CANCEL
        )
    }
}

impl Drop for RunInputPermit<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock();
        if self
            .control
            .is_some_and(InputWriteControl::cancellation_was_requested)
        {
            state.cancellation_barrier = true;
        }
        state.active = false;
        self.gate.ready.notify_all();
    }
}

struct BackgroundRuntime(Option<tokio::runtime::Runtime>);

impl BackgroundRuntime {
    fn new(runtime: tokio::runtime::Runtime) -> Self {
        Self(Some(runtime))
    }

    fn spawn<Future>(&self, future: Future) -> tokio::task::JoinHandle<Future::Output>
    where
        Future: std::future::Future + Send + 'static,
        Future::Output: Send + 'static,
    {
        self.0
            .as_ref()
            .expect("background runtime unavailable during supervisor lifetime")
            .spawn(future)
    }
}

impl Drop for BackgroundRuntime {
    fn drop(&mut self) {
        let Some(runtime) = self.0.take() else {
            return;
        };
        if tokio::runtime::Handle::try_current().is_ok() {
            runtime.shutdown_background();
        } else {
            drop(runtime);
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LifecycleOperation {
    generation: SessionGeneration,
    kind: StopIntentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SpawnReservation {
    generation: SessionGeneration,
    run_id: Uuid,
}

enum RunRetirementCause {
    OutputClosed,
    PtyError(String),
    Liveness {
        final_state: LifecycleState,
        fallback_reason: SessionExitReason,
        detail: String,
    },
}

impl RunRetirementCause {
    fn final_state(&self) -> LifecycleState {
        match self {
            Self::OutputClosed => LifecycleState::Closed,
            Self::PtyError(_) => LifecycleState::Failed,
            Self::Liveness { final_state, .. } => *final_state,
        }
    }

    fn detail(&self) -> &str {
        match self {
            Self::OutputClosed => "session output closed",
            Self::PtyError(error) => error,
            Self::Liveness { detail, .. } => detail,
        }
    }
}

struct QuiesceTimer {
    generation: SessionGeneration,
    run_id: Uuid,
    handle: tokio::task::JoinHandle<()>,
}

struct SessionSlot {
    session_id: Uuid,
    definition: SessionDefinition,
    qualified_working_directory: Option<QualifiedWorkingDirectory>,
    state: LifecycleState,
    work_state: WorkState,
    work_state_observed: bool,
    work_detail: Option<String>,
    work_error_observations: HashMap<String, Vec<Instant>>,
    running: Option<RunningSession>,
    run_id: Option<Uuid>,
    last_run_id: Option<Uuid>,
    bracketed_paste: BracketedPasteRunState,
    run_event_sequence: u64,
    generation: SessionGeneration,
    spawn_in_flight: Option<SpawnReservation>,
    lifecycle_operation: Option<LifecycleOperation>,
    stop_intent: Option<StopIntent>,
    termination_uncertain: bool,
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
            session_id: self.session_id,
            alias: self.definition.alias.clone(),
            label: self.definition.label.clone(),
            driver: self.definition.driver,
            permission_profile: self.definition.permission_profile,
            lifecycle_state: self.state,
            working_dir: self.definition.working_dir.clone(),
            generation: self.generation,
            run_id: self.run_id,
            run_event_sequence: self.run_event_sequence,
            process_id: self.process_id,
            running: self.running.is_some(),
            last_activity_at: self.last_activity_at.clone(),
            last_error: self.last_error.clone(),
        }
    }

    fn title(&self) -> &str {
        &self.definition.label
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct QualifiedWorkingDirectory {
    #[serde(default)]
    namespace: WorkingDirectoryNamespace,
    canonical_path: String,
    identity: String,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum WorkingDirectoryNamespace {
    #[default]
    Windows,
    WslUbuntu,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct PersistedSessionV1 {
    session_id: SessionId,
    label: String,
    driver: DriverKind,
    working_directory: QualifiedWorkingDirectory,
    permission_profile: shared_types::PermissionProfile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SessionCatalogV1 {
    schema_version: u32,
    workspace_preference: QualifiedWorkingDirectory,
    sessions: Vec<PersistedSessionV1>,
}

impl SessionCatalogV1 {
    fn empty(workspace_preference: QualifiedWorkingDirectory) -> Self {
        Self {
            schema_version: SESSION_CATALOG_SCHEMA_VERSION,
            workspace_preference,
            sessions: Vec::new(),
        }
    }
}

#[derive(Default)]
struct SessionRegistry {
    by_id: HashMap<SessionId, SessionSlot>,
    order: Vec<SessionId>,
    alias_to_id: HashMap<String, SessionId>,
}

impl SessionRegistry {
    fn from_catalog(catalog: &SessionCatalogV1) -> Result<Self> {
        let mut registry = Self::default();
        for persisted in &catalog.sessions {
            if registry.by_id.contains_key(&persisted.session_id) {
                return Err(anyhow!(
                    "session catalog contains duplicate session id '{}'",
                    persisted.session_id
                ));
            }
            validate_session_label(&persisted.label)?;
            if matches!(
                persisted.driver,
                DriverKind::GenericTerminal | DriverKind::Prime
            ) && persisted.permission_profile == shared_types::PermissionProfile::Unsafe
            {
                return Err(anyhow!(
                    "{:?} session '{}' cannot use the unsafe permission profile",
                    persisted.driver,
                    persisted.session_id,
                ));
            }
            validate_driver_working_directory_pair(persisted.driver, &persisted.working_directory)?;
            let definition = SessionDefinition {
                session_id: persisted.session_id,
                alias: session_alias(persisted.session_id),
                label: persisted.label.clone(),
                driver: persisted.driver,
                working_dir: persisted.working_directory.canonical_path.clone(),
                permission_profile: persisted.permission_profile,
            };
            let mut slot = closed_session_slot(definition);
            slot.qualified_working_directory = Some(persisted.working_directory.clone());
            registry.insert(slot)?;
        }
        Ok(registry)
    }

    fn insert(&mut self, slot: SessionSlot) -> Result<()> {
        self.validate_insert(&slot)?;
        self.insert_prevalidated(slot);
        Ok(())
    }

    fn validate_insert(&self, slot: &SessionSlot) -> Result<()> {
        let session_id = slot.session_id;
        if self.by_id.contains_key(&session_id) {
            return Err(anyhow!("duplicate session id '{session_id}'"));
        }
        let alias = &slot.definition.alias;
        if self.alias_to_id.contains_key(alias) {
            return Err(anyhow!("duplicate internal session alias '{alias}'"));
        }
        Ok(())
    }

    fn insert_prevalidated(&mut self, slot: SessionSlot) {
        debug_assert!(self.validate_insert(&slot).is_ok());
        let session_id = slot.session_id;
        let alias = slot.definition.alias.clone();
        self.alias_to_id.insert(alias, session_id);
        self.order.push(session_id);
        self.by_id.insert(session_id, slot);
    }

    fn get_by_id(&self, session_id: SessionId) -> Option<&SessionSlot> {
        self.by_id.get(&session_id)
    }

    fn get_by_id_mut(&mut self, session_id: SessionId) -> Option<&mut SessionSlot> {
        self.by_id.get_mut(&session_id)
    }

    fn get_by_alias(&self, alias: &str) -> Option<&SessionSlot> {
        self.alias_to_id
            .get(alias)
            .and_then(|session_id| self.by_id.get(session_id))
    }

    fn remove(&mut self, session_id: SessionId) -> Option<SessionSlot> {
        let slot = self.by_id.remove(&session_id)?;
        self.alias_to_id.remove(&slot.definition.alias);
        self.order.retain(|candidate| *candidate != session_id);
        Some(slot)
    }

    fn ordered_slots(&self) -> impl Iterator<Item = &SessionSlot> {
        self.order
            .iter()
            .filter_map(|session_id| self.by_id.get(session_id))
    }

    fn ordered_snapshots(&self) -> Vec<SessionSnapshot> {
        self.ordered_slots().map(SessionSlot::snapshot).collect()
    }

    #[cfg(test)]
    fn get(&self, alias_or_legacy_label: &str) -> Option<&SessionSlot> {
        if let Some(slot) = self.get_by_alias(alias_or_legacy_label) {
            return Some(slot);
        }
        let mut matches = self.ordered_slots().filter(|slot| {
            slot.definition
                .label
                .eq_ignore_ascii_case(alias_or_legacy_label)
        });
        let only = matches.next()?;
        matches.next().is_none().then_some(only)
    }

    #[cfg(test)]
    fn get_mut(&mut self, alias_or_legacy_label: &str) -> Option<&mut SessionSlot> {
        let session_id = if let Some(session_id) = self.alias_to_id.get(alias_or_legacy_label) {
            *session_id
        } else {
            let mut matches = self.order.iter().copied().filter(|session_id| {
                self.by_id.get(session_id).is_some_and(|slot| {
                    slot.definition
                        .label
                        .eq_ignore_ascii_case(alias_or_legacy_label)
                })
            });
            let only = matches.next()?;
            if matches.next().is_some() {
                return None;
            }
            only
        };
        self.by_id.get_mut(&session_id)
    }

    #[cfg(test)]
    fn values(&self) -> impl Iterator<Item = &SessionSlot> {
        self.ordered_slots()
    }
}

fn session_alias(session_id: SessionId) -> String {
    format!("session-{session_id}")
}

fn validate_session_label(label: &str) -> Result<()> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("session label cannot be empty"));
    }
    if trimmed != label {
        return Err(anyhow!(
            "session label cannot contain leading or trailing whitespace"
        ));
    }
    if trimmed.chars().count() > SESSION_LABEL_MAX_CHARS {
        return Err(anyhow!(
            "session label cannot exceed {SESSION_LABEL_MAX_CHARS} characters"
        ));
    }
    if trimmed.chars().any(|character| character.is_control()) {
        return Err(anyhow!("session label cannot contain control characters"));
    }
    Ok(())
}

fn ensure_closed_for_definition_edit(slot: &SessionSlot) -> Result<()> {
    if slot.state != LifecycleState::Closed
        || slot.running.is_some()
        || slot.run_id.is_some()
        || slot.spawn_in_flight.is_some()
        || slot.lifecycle_operation.is_some()
        || slot.stop_intent.is_some()
        || slot.termination_uncertain
    {
        return Err(anyhow!(
            "session '{}' must be fully closed before this operation",
            slot.session_id
        ));
    }
    Ok(())
}

fn next_run_event_identity(slot: &mut SessionSlot, run_id: Uuid) -> RunEventIdentity {
    slot.run_event_sequence = slot
        .run_event_sequence
        .checked_add(1)
        .expect("run event capacity must be reserved before slot mutation");
    RunEventIdentity {
        session_id: slot.session_id,
        run_id,
        generation: slot.generation,
        sequence: slot.run_event_sequence,
    }
}

fn ensure_run_event_capacity(slot: &SessionSlot, required: u64) -> Result<()> {
    slot.run_event_sequence
        .checked_add(required)
        .map(|_| ())
        .ok_or_else(|| {
            anyhow!(
                "run event sequence exhausted for '{}'",
                slot.definition.alias
            )
        })
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
    identity: RunEventIdentity,
    process_id: Option<u32>,
    classification: &SessionExitClassification,
    timestamp: String,
) -> RuntimeEvent {
    RuntimeEvent::SessionExit {
        identity,
        session,
        process_id,
        exit_code: classification.exit_code,
        signal: classification.signal,
        success: classification.success,
        reason: classification.reason,
        requested: classification.requested,
        timestamp,
    }
}

fn closed_session_slot(definition: SessionDefinition) -> SessionSlot {
    let session_id = definition.session_id;
    SessionSlot {
        session_id,
        definition,
        qualified_working_directory: None,
        state: LifecycleState::Closed,
        work_state: WorkState::Idle,
        work_state_observed: false,
        work_detail: None,
        work_error_observations: HashMap::new(),
        running: None,
        run_id: None,
        last_run_id: None,
        bracketed_paste: BracketedPasteRunState::default(),
        run_event_sequence: 0,
        generation: 0,
        spawn_in_flight: None,
        lifecycle_operation: None,
        stop_intent: None,
        termination_uncertain: false,
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

fn run_write_target_from_slot(name: &str, slot: &SessionSlot) -> Result<RunWriteTarget> {
    let running = slot
        .running
        .as_ref()
        .ok_or_else(|| anyhow!("session '{name}' is not running"))?;
    let pty = running
        .pty
        .as_ref()
        .cloned()
        .ok_or_else(|| anyhow!("session '{name}' transport is not available"))?;
    let run_id = slot
        .run_id
        .ok_or_else(|| anyhow!("session '{name}' has no active run identity"))?;
    Ok(RunWriteTarget {
        session: name.to_string(),
        session_id: slot.session_id,
        run_id,
        generation: slot.generation,
        pty,
        input_gate: running.input_gate.clone(),
    })
}

fn run_binding_from_target(target: &RunWriteTarget) -> RunBinding {
    RunBinding {
        session_id: target.session_id,
        run_id: target.run_id,
        generation: target.generation,
    }
}

fn ensure_run_input_safety_locked(
    slot: &SessionSlot,
    target: &RunWriteTarget,
    safety: RunInputSafety,
) -> Result<()> {
    if safety == RunInputSafety::Raw {
        return Ok(());
    }

    if slot.work_state_observed
        && matches!(
            slot.work_state,
            WorkState::Blocked | WorkState::ErrorLoop | WorkState::Exited
        )
    {
        let detail = slot
            .work_detail
            .as_deref()
            .map(|detail| format!(" ({detail})"))
            .unwrap_or_default();
        return Err(anyhow!(
            "session '{}' work state is {}{detail}; routed/delivered framing is blocked; use raw terminal input to resolve the prompt",
            target.session,
            work_state_alert_label(Some(slot.work_state)),
        ));
    }

    if safety == RunInputSafety::RoutedSubmit {
        return Ok(());
    }

    match slot
        .bracketed_paste
        .mode_for(run_binding_from_target(target))
    {
        BracketedPasteMode::Enabled => Ok(()),
        BracketedPasteMode::Unknown => Err(anyhow!(
            "session '{}' bracketed-paste mode is unknown for the active run; routed/delivered framing is blocked",
            target.session
        )),
        BracketedPasteMode::Disabled => Err(anyhow!(
            "session '{}' bracketed-paste mode is disabled for the active run; routed/delivered framing is blocked",
            target.session
        )),
    }
}

struct SupervisorInner {
    runtime_dir: PathBuf,
    pane_mcp_executable: Option<PathBuf>,
    catalog: Mutex<SessionCatalogV1>,
    rooms: Mutex<RoomState>,
    audit: AuditLog,
    slots: Mutex<SessionRegistry>,
    event_sink: RwLock<Option<EventSink>>,
    control_plane: RwLock<Option<ControlPlaneStatus>>,
    control_plane_lifecycle: Mutex<()>,
    shutdown_lifecycle: Mutex<()>,
    shutdown_started: AtomicBool,
    #[cfg(windows)]
    control_plane_listener: Mutex<Option<ControlPlaneListener>>,
    pty_spawner: RwLock<Arc<dyn PtySpawner>>,
    executable_resolver: RwLock<Arc<dyn DriverExecutableResolver>>,
    wsl_control: RwLock<Arc<dyn WslControl>>,
    wsl_reconciliation_error: Mutex<Option<String>>,
    background_runtime: BackgroundRuntime,
    events_seq: AtomicU64,
    events_watch: tokio::sync::watch::Sender<u64>,
    run_event_publish: Mutex<HashMap<Uuid, RunEventPublishState>>,
    room_event_publish: Mutex<()>,
    stale_event_drop_counts: Mutex<HashMap<(SessionId, SessionGeneration, Uuid), u64>>,
    stale_quiesce_drop_counts: Mutex<HashMap<(SessionId, SessionGeneration), u64>>,
    started_at: Instant,
    heartbeat_interval: Duration,
    sideband_write_timeout: Mutex<Duration>,
    stop_kill_timeout: Mutex<Duration>,
    auto_restart_on_stall: AutoRestartOnStallConfig,
    auto_restart_history: Mutex<HashMap<SessionId, AutoRestartHistory>>,
    #[cfg(test)]
    fail_next_catalog_write: AtomicBool,
    #[cfg(test)]
    fail_next_room_catalog_write: AtomicBool,
    #[cfg(test)]
    pty_event_before_commit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    run_input_before_commit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    work_state_before_side_effect: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    stall_before_reservation: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    stall_after_reservation: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    sideband_after_initial_authorization: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    control_plane_after_prepare: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    room_delivery_after_preflight: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    room_event_after_append: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

struct RoomDeliveryLease {
    inner: Arc<SupervisorInner>,
    room_id: RoomId,
}

impl Drop for RoomDeliveryLease {
    fn drop(&mut self) {
        let mut rooms = self.inner.rooms.lock();
        let Some(room) = rooms.get_mut(self.room_id) else {
            debug_assert!(
                false,
                "in-flight room disappeared before its delivery lease"
            );
            return;
        };
        debug_assert!(room.in_flight_deliveries > 0);
        room.in_flight_deliveries = room.in_flight_deliveries.saturating_sub(1);
    }
}

#[derive(Clone)]
pub struct SupervisorHandle {
    inner: Arc<SupervisorInner>,
}

#[derive(Clone)]
pub struct RendererEventProjector {
    inner: Weak<SupervisorInner>,
}

impl RendererEventProjector {
    pub fn project(&self, event: RuntimeEvent) -> RuntimeEvent {
        if self.inner.upgrade().is_none() {
            RuntimeEvent::SystemLog {
                level: LogLevel::Warn,
                message: "runtime event unavailable after supervisor shutdown".into(),
                timestamp: now_rfc3339(),
            }
        } else {
            event
        }
    }
}

fn audit_event_projection(event: &RuntimeEvent, _inner: &SupervisorInner) -> Option<RuntimeEvent> {
    let event = match event {
        RuntimeEvent::SessionOutput { .. } => return None,
        RuntimeEvent::RoutedMessage {
            id,
            from,
            to,
            scope,
            timestamp,
            ..
        } => RuntimeEvent::RoutedMessage {
            id: *id,
            from: from.clone(),
            to: to.clone(),
            scope: *scope,
            content: "[content omitted]".into(),
            timestamp: timestamp.clone(),
        },
        RuntimeEvent::SessionWorkState {
            identity,
            session,
            state,
            previous_state,
            timestamp,
            ..
        } => RuntimeEvent::SessionWorkState {
            identity: *identity,
            session: session.clone(),
            state: *state,
            detail: None,
            previous_state: *previous_state,
            timestamp: timestamp.clone(),
        },
        RuntimeEvent::SessionCreated {
            schema_version,
            session,
            timestamp,
        } => {
            let mut session = session.clone();
            session.working_dir = "[path omitted]".into();
            RuntimeEvent::SessionCreated {
                schema_version: *schema_version,
                session,
                timestamp: timestamp.clone(),
            }
        }
        RuntimeEvent::SessionWorkingDirectoryChanged {
            schema_version,
            session_id,
            timestamp,
            ..
        } => RuntimeEvent::SessionWorkingDirectoryChanged {
            schema_version: *schema_version,
            session_id: *session_id,
            old_working_dir: "[path omitted]".into(),
            new_working_dir: "[path omitted]".into(),
            timestamp: timestamp.clone(),
        },
        RuntimeEvent::RoomFeedEvent { feed_event } => {
            let mut feed_event = feed_event.clone();
            if let RoomFeedItem::Message { content, .. } = &mut feed_event.item {
                *content = "[content omitted]".into();
            }
            RuntimeEvent::RoomFeedEvent { feed_event }
        }
        event => event.clone(),
    };

    Some(event)
}

fn metadata_is_link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }

    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    #[cfg(not(windows))]
    false
}

fn ensure_existing_path_chain_has_no_reparse_points(path: &Path) -> Result<()> {
    let mut cursor = Some(path);
    while let Some(candidate) = cursor {
        match fs::symlink_metadata(candidate) {
            Ok(metadata) if metadata_is_link_or_reparse(&metadata) => {
                return Err(anyhow!(
                    "runtime storage path must not contain a symlink or reparse point: {}",
                    candidate.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect runtime storage path component {}",
                        candidate.display()
                    )
                });
            }
        }
        cursor = candidate.parent();
    }
    Ok(())
}

fn canonical_destination_without_reparse_points(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .context("failed to resolve the current directory for runtime storage")?
            .join(path)
    };
    ensure_existing_path_chain_has_no_reparse_points(&absolute)?;

    let mut cursor = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        match fs::metadata(cursor) {
            Ok(metadata) => {
                if !metadata.is_dir() {
                    return Err(anyhow!(
                        "runtime storage ancestor is not a directory: {}",
                        cursor.display()
                    ));
                }
                let mut destination = fs::canonicalize(cursor).with_context(|| {
                    format!(
                        "failed to canonicalize runtime storage ancestor {}",
                        cursor.display()
                    )
                })?;
                for component in missing.iter().rev() {
                    destination.push(component);
                }
                return Ok(destination);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let component = cursor.file_name().ok_or_else(|| {
                    anyhow!(
                        "runtime storage destination has no existing directory ancestor: {}",
                        absolute.display()
                    )
                })?;
                missing.push(component.to_os_string());
                cursor = cursor.parent().ok_or_else(|| {
                    anyhow!(
                        "runtime storage destination has no parent: {}",
                        absolute.display()
                    )
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to inspect runtime storage destination {}",
                        cursor.display()
                    )
                });
            }
        }
    }
}

fn comparable_storage_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_ascii_lowercase())
    }

    #[cfg(not(windows))]
    path.to_path_buf()
}

fn child_process_path(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let path = path.to_string_lossy();
        if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = path.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
        PathBuf::from(path.as_ref())
    }

    #[cfg(not(windows))]
    path.to_path_buf()
}

/// Resolves the runtime destination without following symlinks/reparse points and
/// proves that it is disjoint from the harness working tree. This function is
/// intentionally side-effect free so callers can run it before creating locks,
/// files, or directories beneath the destination.
pub fn validate_runtime_storage_paths(runtime_dir: &Path, working_root: &Path) -> Result<PathBuf> {
    let runtime_destination = canonical_destination_without_reparse_points(runtime_dir)?;
    let working_root = fs::canonicalize(working_root).with_context(|| {
        format!(
            "failed to canonicalize agent working root {}",
            working_root.display()
        )
    })?;
    if !working_root.is_dir() {
        return Err(anyhow!(
            "agent working root is not a directory: {}",
            working_root.display()
        ));
    }

    let runtime_comparable = comparable_storage_path(&runtime_destination);
    let working_comparable = comparable_storage_path(&working_root);
    if runtime_comparable.starts_with(&working_comparable)
        || working_comparable.starts_with(&runtime_comparable)
    {
        return Err(anyhow!(
            "runtime storage and agent working root must be disjoint (runtime={}, working_root={})",
            runtime_destination.display(),
            working_root.display()
        ));
    }

    Ok(runtime_destination)
}

struct DirectoryLease {
    _handle: File,
    identity: String,
}

enum SessionDirectoryLease {
    Windows { _lease: DirectoryLease },
    WslUbuntu,
}

#[cfg(windows)]
fn open_directory_lease(path: &Path) -> Result<DirectoryLease> {
    use std::mem::zeroed;
    use std::os::windows::{fs::OpenOptionsExt as _, io::AsRawHandle as _};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle,
    };

    let handle = OpenOptions::new()
        .access_mode(FILE_READ_ATTRIBUTES)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
        .open(path)
        .with_context(|| format!("failed to lease working directory {}", path.display()))?;
    let mut information: BY_HANDLE_FILE_INFORMATION = unsafe { zeroed() };
    if unsafe { GetFileInformationByHandle(handle.as_raw_handle().cast(), &mut information) } == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to query working-directory identity {}",
                path.display()
            )
        });
    }
    let file_index =
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow);
    Ok(DirectoryLease {
        _handle: handle,
        identity: format!("windows:{}:{file_index}", information.dwVolumeSerialNumber),
    })
}

#[cfg(unix)]
fn open_directory_lease(path: &Path) -> Result<DirectoryLease> {
    use std::os::unix::fs::MetadataExt as _;

    let handle = File::open(path)
        .with_context(|| format!("failed to lease working directory {}", path.display()))?;
    let metadata = handle
        .metadata()
        .with_context(|| format!("failed to inspect working directory {}", path.display()))?;
    Ok(DirectoryLease {
        _handle: handle,
        identity: format!("unix:{}:{}", metadata.dev(), metadata.ino()),
    })
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    let left = comparable_storage_path(left);
    let right = comparable_storage_path(right);
    left.starts_with(&right) || right.starts_with(&left)
}

fn qualify_working_directory(
    candidate: &Path,
    runtime_dir: &Path,
) -> Result<(QualifiedWorkingDirectory, DirectoryLease)> {
    if !candidate.is_absolute() {
        return Err(anyhow!(
            "working directory must be an absolute path: {}",
            candidate.display()
        ));
    }
    let canonical = fs::canonicalize(candidate).with_context(|| {
        format!(
            "failed to resolve selected working directory {}",
            candidate.display()
        )
    })?;
    if !canonical.is_dir() {
        return Err(anyhow!(
            "selected working directory is not a directory: {}",
            canonical.display()
        ));
    }
    if paths_overlap(&canonical, runtime_dir) {
        return Err(anyhow!(
            "working directory and runtime storage must be disjoint (working_directory={}, runtime={})",
            canonical.display(),
            runtime_dir.display()
        ));
    }

    let lease = open_directory_lease(&canonical)?;
    let confirmed = fs::canonicalize(&canonical).with_context(|| {
        format!(
            "working directory changed while it was being qualified: {}",
            canonical.display()
        )
    })?;
    if comparable_storage_path(&canonical) != comparable_storage_path(&confirmed) {
        return Err(anyhow!(
            "working directory changed while it was being qualified (expected={}, resolved={})",
            canonical.display(),
            confirmed.display()
        ));
    }

    Ok((
        QualifiedWorkingDirectory {
            namespace: WorkingDirectoryNamespace::Windows,
            canonical_path: child_process_path(&canonical)
                .to_string_lossy()
                .into_owned(),
            identity: lease.identity.clone(),
        },
        lease,
    ))
}

fn revalidate_qualified_working_directory(
    session_id: SessionId,
    expected: &QualifiedWorkingDirectory,
    runtime_dir: &Path,
) -> Result<DirectoryLease> {
    if expected.namespace != WorkingDirectoryNamespace::Windows {
        return Err(anyhow!(
            "session '{session_id}' working directory is not a Windows directory"
        ));
    }
    revalidate_qualified_directory(
        &format!("session '{session_id}' working directory"),
        expected,
        runtime_dir,
    )
}

fn revalidate_qualified_directory(
    subject: &str,
    expected: &QualifiedWorkingDirectory,
    runtime_dir: &Path,
) -> Result<DirectoryLease> {
    let (actual, lease) =
        qualify_working_directory(Path::new(&expected.canonical_path), runtime_dir)?;
    if actual != *expected {
        return Err(anyhow!(
            "{subject} changed since it was selected; choose it again (expected identity {}, actual identity {})",
            expected.identity,
            actual.identity
        ));
    }
    Ok(lease)
}

fn session_catalog_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(SESSION_CATALOG_FILE_NAME)
}

fn room_catalog_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join(ROOM_CATALOG_FILE_NAME)
}

fn read_session_catalog(path: &Path) -> Result<SessionCatalogV1> {
    let file = File::open(path)
        .with_context(|| format!("failed to open session catalog {}", path.display()))?;
    let catalog: SessionCatalogV1 = serde_json::from_reader(StdBufReader::new(file))
        .with_context(|| format!("failed to parse session catalog {}", path.display()))?;
    if catalog.schema_version != SESSION_CATALOG_SCHEMA_VERSION {
        return Err(anyhow!(
            "unsupported session catalog schema version {} (expected {})",
            catalog.schema_version,
            SESSION_CATALOG_SCHEMA_VERSION
        ));
    }
    validate_persisted_qualified_directory("workspace preference", &catalog.workspace_preference)?;
    if catalog.workspace_preference.namespace != WorkingDirectoryNamespace::Windows {
        return Err(anyhow!(
            "workspace preference must use the Windows working-directory namespace"
        ));
    }
    let mut seen = HashSet::new();
    for session in &catalog.sessions {
        if !seen.insert(session.session_id) {
            return Err(anyhow!(
                "session catalog contains duplicate session id '{}'",
                session.session_id
            ));
        }
        validate_session_label(&session.label)?;
        validate_persisted_qualified_directory(
            &format!("session '{}' working directory", session.session_id),
            &session.working_directory,
        )?;
        validate_driver_working_directory_pair(session.driver, &session.working_directory)?;
    }
    Ok(catalog)
}

fn read_room_catalog(path: &Path) -> Result<RoomCatalogV1> {
    let file = File::open(path)
        .with_context(|| format!("failed to open room catalog {}", path.display()))?;
    serde_json::from_reader(StdBufReader::new(file))
        .with_context(|| format!("failed to parse room catalog {}", path.display()))
}

fn validate_persisted_qualified_directory(
    subject: &str,
    directory: &QualifiedWorkingDirectory,
) -> Result<()> {
    let valid_absolute_path = match directory.namespace {
        WorkingDirectoryNamespace::Windows => Path::new(&directory.canonical_path).is_absolute(),
        WorkingDirectoryNamespace::WslUbuntu => directory.canonical_path.starts_with('/'),
    };
    if directory.canonical_path.trim() != directory.canonical_path
        || directory.canonical_path.contains('\0')
        || !valid_absolute_path
    {
        return Err(anyhow!("{subject} has an invalid absolute canonical path"));
    }
    if directory.identity.trim().is_empty() {
        return Err(anyhow!("{subject} has a blank persisted identity"));
    }
    if directory.namespace == WorkingDirectoryNamespace::WslUbuntu {
        validate_linux_candidate(&directory.canonical_path)
            .with_context(|| format!("{subject} has an invalid Ubuntu path"))?;
        parse_wsl_identity(&directory.identity)
            .with_context(|| format!("{subject} has an invalid Ubuntu identity"))?;
    }
    Ok(())
}

fn validate_driver_working_directory_pair(
    driver: DriverKind,
    directory: &QualifiedWorkingDirectory,
) -> Result<()> {
    let valid = match driver {
        DriverKind::Prime => directory.namespace == WorkingDirectoryNamespace::WslUbuntu,
        DriverKind::Claude | DriverKind::Codex | DriverKind::Grok | DriverKind::GenericTerminal => {
            directory.namespace == WorkingDirectoryNamespace::Windows
        }
    };
    if valid {
        Ok(())
    } else {
        Err(anyhow!(
            "driver {driver:?} cannot use the persisted {:?} working-directory namespace",
            directory.namespace
        ))
    }
}

fn persist_session_catalog(runtime_dir: &Path, catalog: &SessionCatalogV1) -> Result<()> {
    if catalog.schema_version != SESSION_CATALOG_SCHEMA_VERSION {
        return Err(anyhow!("refusing to persist an unsupported catalog schema"));
    }
    persist_catalog(
        runtime_dir,
        SESSION_CATALOG_FILE_NAME,
        "session catalog",
        catalog,
    )
}

fn persist_room_catalog(runtime_dir: &Path, catalog: &RoomCatalogV1) -> Result<()> {
    if catalog.schema_version != rooms::ROOM_CATALOG_SCHEMA_VERSION {
        return Err(anyhow!(
            "refusing to persist an unsupported room catalog schema"
        ));
    }
    persist_catalog(runtime_dir, ROOM_CATALOG_FILE_NAME, "room catalog", catalog)
}

fn persist_catalog<T: Serialize>(
    runtime_dir: &Path,
    file_name: &str,
    subject: &str,
    catalog: &T,
) -> Result<()> {
    let destination = runtime_dir.join(file_name);
    let temporary = runtime_dir.join(format!(".{file_name}.{}.tmp", Uuid::new_v4()));
    let write_result = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .with_context(|| {
                format!(
                    "failed to create temporary {subject} {}",
                    temporary.display()
                )
            })?;
        restrict_path_to_current_user(&temporary, false)?;
        serde_json::to_writer_pretty(&mut file, catalog)
            .with_context(|| format!("failed to serialize {subject}"))?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_all()?;
        drop(file);

        replace_catalog_file(&temporary, &destination, subject)?;
        // The temporary file is already private. Rename/ReplaceFile is the commit
        // point, so no fallible work may follow it or disk and memory could diverge.
        Ok(())
    })();
    if write_result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(windows)]
fn replace_catalog_file(temporary: &Path, destination: &Path, subject: &str) -> Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{REPLACEFILE_WRITE_THROUGH, ReplaceFileW};

    if !destination.exists() {
        return fs::rename(temporary, destination).with_context(|| {
            format!(
                "failed to install initial {subject} {}",
                destination.display()
            )
        });
    }
    let destination_wide = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let temporary_wide = temporary
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    if unsafe {
        ReplaceFileW(
            destination_wide.as_ptr(),
            temporary_wide.as_ptr(),
            std::ptr::null(),
            REPLACEFILE_WRITE_THROUGH,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to atomically replace {subject} {}",
                destination.display()
            )
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn replace_catalog_file(temporary: &Path, destination: &Path, subject: &str) -> Result<()> {
    fs::rename(temporary, destination).with_context(|| {
        format!(
            "failed to atomically replace {subject} {}",
            destination.display()
        )
    })
}

fn remove_legacy_disk_mailbox(runtime_dir: &Path) -> Result<()> {
    let path = runtime_dir.join("sideband");
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect legacy disk mailbox: {}", path.display())
            });
        }
    };

    if metadata_is_link_or_reparse(&metadata) || !metadata.is_dir() {
        fs::remove_file(&path).or_else(|file_error| {
            fs::remove_dir(&path).map_err(|directory_error| {
                std::io::Error::other(format!(
                    "file removal failed: {file_error}; directory removal failed: {directory_error}"
                ))
            })
        })
    } else {
        fs::remove_dir_all(&path)
    }
    .with_context(|| format!("failed to remove legacy disk mailbox: {}", path.display()))
}

fn remove_legacy_control_plane_files(runtime_dir: &Path) -> Result<()> {
    for entry in fs::read_dir(runtime_dir).with_context(|| {
        format!(
            "failed to inspect runtime directory {}",
            runtime_dir.display()
        )
    })? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let is_legacy = file_name == "control-plane.json"
            || (file_name.starts_with("control-plane-") && file_name.ends_with(".json"));
        if !is_legacy {
            continue;
        }

        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata_is_link_or_reparse(&metadata) {
            fs::remove_file(&path)
                .or_else(|file_error| {
                    fs::remove_dir(&path).map_err(|directory_error| {
                        std::io::Error::other(format!(
                            "symlink file removal failed: {file_error}; symlink directory removal failed: {directory_error}"
                        ))
                    })
                })
                .with_context(|| {
                    format!(
                        "failed to remove legacy control-plane symlink without following it: {}",
                        path.display()
                    )
                })?;
        } else if metadata.is_file() {
            fs::remove_file(&path)?;
        } else if metadata.is_dir() {
            return Err(anyhow!(
                "refusing to recursively remove legacy control-plane directory {}",
                path.display()
            ));
        } else {
            return Err(anyhow!(
                "unsupported legacy control-plane artifact {}",
                path.display()
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
struct LocalSecurityDescriptor(windows_sys::Win32::Security::PSECURITY_DESCRIPTOR);

#[cfg(windows)]
impl LocalSecurityDescriptor {
    fn as_ptr(&self) -> windows_sys::Win32::Security::PSECURITY_DESCRIPTOR {
        self.0
    }
}

#[cfg(windows)]
impl Drop for LocalSecurityDescriptor {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::LocalFree(self.0);
        }
    }
}

#[cfg(windows)]
fn current_user_sid_string() -> Result<String> {
    use std::{mem, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    struct TokenHandle(HANDLE);
    impl Drop for TokenHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    let mut token = ptr::null_mut();
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to open current process token");
    }
    let token = TokenHandle(token);

    let mut required = 0_u32;
    unsafe {
        GetTokenInformation(token.0, TokenUser, ptr::null_mut(), 0, &mut required);
    }
    if required == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to measure current process token user");
    }
    let word_count = (required as usize).div_ceil(mem::size_of::<usize>());
    let mut token_user_buffer = vec![0_usize; word_count];
    if unsafe {
        GetTokenInformation(
            token.0,
            TokenUser,
            token_user_buffer.as_mut_ptr().cast(),
            required,
            &mut required,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error())
            .context("failed to read current process token user");
    }
    let token_user = unsafe { &*(token_user_buffer.as_ptr().cast::<TOKEN_USER>()) };

    let mut sid_text = ptr::null_mut();
    if unsafe {
        windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW(
            token_user.User.Sid,
            &mut sid_text,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error()).context("failed to render current user SID");
    }
    let sid_text_guard = LocalSecurityDescriptor(sid_text.cast());
    let sid_len = (0..)
        .take_while(|index| unsafe { *sid_text.add(*index) } != 0)
        .count();
    let sid = String::from_utf16(unsafe { std::slice::from_raw_parts(sid_text, sid_len) })
        .context("current user SID was not valid UTF-16")?;
    drop(sid_text_guard);
    Ok(sid)
}

#[cfg(windows)]
fn current_user_only_security_descriptor(
    ace_flags: &str,
    access_rights: &str,
) -> Result<LocalSecurityDescriptor> {
    use std::ptr;
    use windows_sys::Win32::Security::{
        Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
        PSECURITY_DESCRIPTOR,
    };

    let sddl = format!(
        "D:P(A;{ace_flags};{access_rights};;;{})",
        current_user_sid_string()?
    );
    let sddl_wide = sddl
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    let converted = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl_wide.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    };
    if converted == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to build current-user-only security descriptor");
    }
    Ok(LocalSecurityDescriptor(descriptor))
}

#[cfg(windows)]
fn restrict_path_to_current_user(path: &Path, is_directory: bool) -> Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Security::{
        DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
    };

    let descriptor =
        current_user_only_security_descriptor(if is_directory { "OICI" } else { "" }, "FA")?;
    let path_wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let applied = unsafe {
        SetFileSecurityW(
            path_wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor.as_ptr(),
        )
    };
    if applied == 0 {
        return Err(std::io::Error::last_os_error()).with_context(|| {
            format!(
                "failed to apply current-user-only permissions to {}",
                path.display()
            )
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn restrict_path_to_current_user(path: &Path, is_directory: bool) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mode = if is_directory { 0o700 } else { 0o600 };
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(mode);
    fs::set_permissions(path, permissions).with_context(|| {
        format!(
            "failed to apply current-user-only permissions to {}",
            path.display()
        )
    })
}

impl SupervisorHandle {
    pub fn new(mut config: SupervisorConfig) -> Result<Self> {
        #[cfg(windows)]
        if let Some(executable) = config.pane_mcp_executable.as_deref() {
            validate_pane_mcp_executable(executable)?;
        }
        let expected_runtime =
            validate_runtime_storage_paths(&config.runtime_dir, &config.working_root)?;
        fs::create_dir_all(&expected_runtime).context("failed to create runtime directory")?;
        let resolved_runtime =
            validate_runtime_storage_paths(&expected_runtime, &config.working_root)?;
        if comparable_storage_path(&expected_runtime) != comparable_storage_path(&resolved_runtime)
        {
            return Err(anyhow!(
                "runtime storage destination changed while it was being prepared (expected={}, resolved={})",
                expected_runtime.display(),
                resolved_runtime.display()
            ));
        }
        config.runtime_dir = resolved_runtime;
        restrict_path_to_current_user(&config.runtime_dir, true)?;
        remove_legacy_disk_mailbox(&config.runtime_dir)?;
        remove_legacy_control_plane_files(&config.runtime_dir)?;
        let audit = AuditLog::new(&config.runtime_dir)?;
        let background_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_time()
            .build()
            .context("failed to create supervisor background runtime")?;
        let (events_watch, _events_watch_rx) = tokio::sync::watch::channel(0_u64);
        let catalog_path = session_catalog_path(&config.runtime_dir);
        let catalog = if catalog_path.exists() {
            read_session_catalog(&catalog_path)?
        } else {
            let (workspace_preference, _lease) =
                qualify_working_directory(&config.working_root, &config.runtime_dir)?;
            let catalog = SessionCatalogV1::empty(workspace_preference);
            persist_session_catalog(&config.runtime_dir, &catalog)?;
            catalog
        };
        #[cfg(test)]
        let wsl_control: Arc<dyn WslControl> = Arc::new(TestWslUnavailable);
        #[cfg(not(test))]
        let wsl_control: Arc<dyn WslControl> =
            Arc::new(ConcreteWslControl::new(config.runtime_dir.clone()));
        let wsl_reconciliation_error = wsl_control
            .reconcile_stale_scopes()
            .err()
            .map(|error| format!("{error:#}"));

        let mut slots = SessionRegistry::from_catalog(&catalog)?;
        for slot in slots.by_id.values_mut() {
            let availability = slot
                .qualified_working_directory
                .as_ref()
                .ok_or_else(|| anyhow!("qualified working-directory metadata is missing"))
                .and_then(|qualified| match qualified.namespace {
                    WorkingDirectoryNamespace::Windows => revalidate_qualified_working_directory(
                        slot.session_id,
                        qualified,
                        &config.runtime_dir,
                    )
                    .map(|_lease| ()),
                    WorkingDirectoryNamespace::WslUbuntu => {
                        wsl_control.revalidate_working_directory(qualified)
                    }
                });
            if let Err(error) = availability {
                slot.last_error = Some(format!("working directory unavailable: {error:#}"));
            }
        }
        let run_event_publish = slots
            .ordered_slots()
            .map(|slot| {
                (
                    slot.session_id,
                    RunEventPublishState::new(
                        slot.run_event_sequence
                            .checked_add(1)
                            .expect("new session run-event sequence must have capacity"),
                    ),
                )
            })
            .collect();
        let room_catalog_path = room_catalog_path(&config.runtime_dir);
        let room_catalog = if room_catalog_path.exists() {
            read_room_catalog(&room_catalog_path)?
        } else {
            let catalog = RoomCatalogV1::empty();
            persist_room_catalog(&config.runtime_dir, &catalog)?;
            catalog
        };
        let known_sessions = slots.by_id.keys().copied().collect::<HashSet<_>>();
        let rooms = RoomState::from_catalog(room_catalog, &known_sessions)?;
        let heartbeat_interval = config
            .heartbeat_interval
            .filter(|duration| !duration.is_zero())
            .unwrap_or_else(heartbeat_interval_from_env);
        let auto_restart_on_stall = AutoRestartOnStallConfig {
            allowed_sessions: RwLock::new(
                config
                    .auto_restart_on_stall_sessions
                    .unwrap_or_else(auto_restart_sessions_from_env)
                    .into_iter()
                    .collect(),
            ),
            threshold: config
                .auto_restart_stall_threshold
                .filter(|duration| !duration.is_zero())
                .unwrap_or_else(auto_restart_stall_threshold_from_env),
        };

        let handle = Self {
            inner: Arc::new(SupervisorInner {
                runtime_dir: config.runtime_dir,
                pane_mcp_executable: config.pane_mcp_executable,
                catalog: Mutex::new(catalog),
                rooms: Mutex::new(rooms),
                audit,
                slots: Mutex::new(slots),
                event_sink: RwLock::new(None),
                control_plane: RwLock::new(None),
                control_plane_lifecycle: Mutex::new(()),
                shutdown_lifecycle: Mutex::new(()),
                shutdown_started: AtomicBool::new(false),
                #[cfg(windows)]
                control_plane_listener: Mutex::new(None),
                pty_spawner: RwLock::new(Arc::new(ConcretePtySpawner {
                    wsl: Arc::clone(&wsl_control),
                })),
                executable_resolver: RwLock::new(Arc::new(HostDriverExecutableResolver)),
                wsl_control: RwLock::new(wsl_control),
                wsl_reconciliation_error: Mutex::new(wsl_reconciliation_error),
                background_runtime: BackgroundRuntime::new(background_runtime),
                events_seq: AtomicU64::new(0),
                events_watch,
                run_event_publish: Mutex::new(run_event_publish),
                room_event_publish: Mutex::new(()),
                stale_event_drop_counts: Mutex::new(HashMap::new()),
                stale_quiesce_drop_counts: Mutex::new(HashMap::new()),
                started_at: Instant::now(),
                heartbeat_interval,
                sideband_write_timeout: Mutex::new(SidebandTimeouts::write_budget()),
                stop_kill_timeout: Mutex::new(SESSION_STOP_KILL_TIMEOUT),
                auto_restart_on_stall,
                auto_restart_history: Mutex::new(HashMap::new()),
                #[cfg(test)]
                fail_next_catalog_write: AtomicBool::new(false),
                #[cfg(test)]
                fail_next_room_catalog_write: AtomicBool::new(false),
                #[cfg(test)]
                pty_event_before_commit: Mutex::new(None),
                #[cfg(test)]
                run_input_before_commit: Mutex::new(None),
                #[cfg(test)]
                work_state_before_side_effect: Mutex::new(None),
                #[cfg(test)]
                stall_before_reservation: Mutex::new(None),
                #[cfg(test)]
                stall_after_reservation: Mutex::new(None),
                #[cfg(test)]
                sideband_after_initial_authorization: Mutex::new(None),
                #[cfg(test)]
                control_plane_after_prepare: Mutex::new(None),
                #[cfg(test)]
                room_delivery_after_preflight: Mutex::new(None),
                #[cfg(test)]
                room_event_after_append: Mutex::new(None),
            }),
        };
        handle.start_supervisor_heartbeat_task();
        Ok(handle)
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.inner.runtime_dir
    }

    pub fn renderer_event_projector(&self) -> RendererEventProjector {
        RendererEventProjector {
            inner: Arc::downgrade(&self.inner),
        }
    }

    pub fn audit_log_path(&self) -> PathBuf {
        self.inner.audit.path()
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
        slots
            .ordered_slots()
            .map(|slot| HeartbeatSessionSummary {
                name: slot.definition.alias.clone(),
                lifecycle_state: slot.state,
                work_state: slot.work_state_observed.then_some(slot.work_state),
                process_id: slot.process_id,
                last_activity_at: slot.last_activity_at.clone(),
            })
            .collect::<Vec<_>>()
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

    #[cfg(test)]
    fn bump_session_generation_for_tests(
        &self,
        session_id: SessionId,
    ) -> Result<SessionGeneration> {
        let mut slots = self.inner.slots.lock();
        self.ensure_active()?;
        let slot = slots
            .get_by_id_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
        ensure_run_event_capacity(slot, 2)?;
        let next_generation = slot
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("session generation exhausted for '{session_id}'"))?;
        if let Some(running) = slot.running.as_ref() {
            running.close_input();
        }
        slot.run_id = None;
        cancel_quiesce_timer_locked(slot);
        slot.generation = next_generation;
        Ok(slot.generation)
    }

    fn declare_stop_operation(
        &self,
        session_id: SessionId,
        expected_run: Option<(SessionGeneration, Option<Uuid>)>,
        kind: StopIntentKind,
    ) -> Result<(String, SessionGeneration)> {
        let mut slots = self.inner.slots.lock();
        self.ensure_active()?;
        let slot = slots
            .get_by_id_mut(session_id)
            .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
        if let Some(operation) = slot.lifecycle_operation {
            return Err(anyhow!(
                "session '{session_id}' already has a {:?} lifecycle operation in progress at generation {}",
                operation.kind,
                operation.generation
            ));
        }
        if slot.spawn_in_flight.is_some() && (slot.termination_uncertain || slot.running.is_some())
        {
            return Err(anyhow!(
                "session '{session_id}' rejected-spawn cleanup is still in progress"
            ));
        }
        if slot.termination_uncertain
            && !(kind == StopIntentKind::Operator && slot.running.is_some())
        {
            return Err(anyhow!(
                "session '{session_id}' has an unverified prior termination"
            ));
        }
        if let Some((expected_generation, expected_run_id)) = expected_run
            && (slot.generation != expected_generation || slot.run_id != expected_run_id)
        {
            return Err(anyhow!(
                "session '{session_id}' run changed before lifecycle declaration"
            ));
        }
        let alias = slot.definition.alias.clone();
        ensure_run_event_capacity(
            slot,
            if kind == StopIntentKind::Restart {
                5
            } else {
                2
            },
        )?;
        if kind == StopIntentKind::Restart {
            slot.generation
                .checked_add(2)
                .ok_or_else(|| anyhow!("session generation exhausted for '{session_id}'"))?;
        }
        let next_generation = slot
            .generation
            .checked_add(1)
            .ok_or_else(|| anyhow!("session generation exhausted for '{session_id}'"))?;
        if let Some(running) = slot.running.as_ref() {
            running.close_input();
        }
        slot.run_id = None;
        cancel_quiesce_timer_locked(slot);
        slot.generation = next_generation;
        if kind == StopIntentKind::Restart {
            slot.state = LifecycleState::Restarting;
        }
        slot.lifecycle_operation = Some(LifecycleOperation {
            generation: next_generation,
            kind,
        });
        Ok((alias, slot.generation))
    }

    fn ensure_active(&self) -> Result<()> {
        if self.inner.shutdown_started.load(Ordering::Acquire) {
            Err(anyhow!("supervisor has shut down"))
        } else {
            Ok(())
        }
    }

    #[cfg(test)]
    fn current_generation_for_tests(&self, session_id: SessionId) -> Option<SessionGeneration> {
        self.inner
            .slots
            .lock()
            .get_by_id(session_id)
            .map(|slot| slot.generation)
    }

    pub fn set_event_sink<F>(&self, sink: F)
    where
        F: Fn(RuntimeEvent) + Send + Sync + 'static,
    {
        *self.inner.event_sink.write() = Some(Arc::new(sink));
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
        self.emit_dispatch_attempt_from_current_slot(request_id, action, from, target_session)
    }

    fn emit_dispatch_attempt_from_current_slot(
        &self,
        request_id: &str,
        action: &str,
        from: &str,
        target_session: &str,
    ) -> Result<DispatchAttemptDecision> {
        let decision = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_alias(target_session)
                .with_context(|| format!("unknown session '{target_session}'"))?;
            DispatchAttemptDecision::from_slot(slot)
        };

        self.emit_dispatch_attempt_decision(request_id, action, from, target_session, decision)
    }

    fn emit_dispatch_attempt_for_session_id(
        &self,
        request_id: &str,
        action: &str,
        from: &str,
        session_id: SessionId,
    ) -> Result<DispatchAttemptDecision> {
        self.refresh_session_liveness();
        let (alias, decision) = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            (
                slot.definition.alias.clone(),
                DispatchAttemptDecision::from_slot(slot),
            )
        };
        self.emit_dispatch_attempt_decision(request_id, action, from, &alias, decision)
    }

    fn emit_dispatch_attempt_from_pane_caller(
        &self,
        request_id: &str,
        action: &str,
        caller: &PaneCaller,
    ) -> Result<DispatchAttemptDecision> {
        let decision = {
            let slots = self.inner.slots.lock();
            Self::validate_pane_caller_locked(caller, &slots)?;
            let slot = slots
                .get_by_id(caller.session_id)
                .ok_or_else(|| anyhow!("sideband caller run is stale"))?;
            DispatchAttemptDecision::from_slot(slot)
        };

        self.emit_dispatch_attempt_decision(
            request_id,
            action,
            &caller.session,
            &caller.session,
            decision,
        )
    }

    fn emit_dispatch_attempt_decision(
        &self,
        request_id: &str,
        action: &str,
        from: &str,
        target_session: &str,
        decision: DispatchAttemptDecision,
    ) -> Result<DispatchAttemptDecision> {
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

    fn arm_quiesce_timer(
        &self,
        session_id: SessionId,
        driver: DriverKind,
        generation: SessionGeneration,
        run_id: Uuid,
        armed_at: Instant,
    ) {
        let Some(threshold) = quiesce_threshold(driver) else {
            return;
        };

        let handle = self.clone();
        let task = self.inner.background_runtime.spawn(async move {
            tokio::time::sleep(threshold).await;
            handle.fire_quiesce_timer(session_id, generation, run_id, armed_at, threshold);
        });

        let mut slots = self.inner.slots.lock();
        if !self.inner.shutdown_started.load(Ordering::Acquire)
            && let Some(slot) = slots
                .get_by_id_mut(session_id)
                .filter(|slot| slot.generation == generation && slot.run_id == Some(run_id))
        {
            cancel_quiesce_timer_locked(slot);
            slot.quiesce_timer = Some(QuiesceTimer {
                generation,
                run_id,
                handle: task,
            });
        } else {
            task.abort();
        }
    }

    fn fire_quiesce_timer(
        &self,
        session_id: SessionId,
        armed_generation: SessionGeneration,
        armed_run_id: Uuid,
        armed_at: Instant,
        threshold: Duration,
    ) {
        let _shutdown = self.inner.shutdown_lifecycle.lock();
        if self.inner.shutdown_started.load(Ordering::Acquire) {
            return;
        }
        let (events, stale_drop) = {
            let mut slots = self.inner.slots.lock();
            let Some(slot) = slots.get_by_id_mut(session_id) else {
                return;
            };
            let session_alias = slot.definition.alias.clone();
            let timer = slot.quiesce_timer.take();
            match timer {
                Some(timer)
                    if timer.generation == armed_generation && timer.run_id == armed_run_id =>
                {
                    if slot.generation == armed_generation
                        && slot.run_id == Some(armed_run_id)
                        && slot.state == LifecycleState::Ready
                        && slot.last_real_output_at == Some(armed_at)
                    {
                        if ensure_run_event_capacity(slot, 2).is_err() {
                            (Vec::new(), true)
                        } else {
                            slot.state = LifecycleState::Idle;
                            slot.last_activity_at = Some(now_rfc3339());
                            let mut events = Vec::new();
                            if let Some(event) = transition_work_state_locked(
                                &session_alias,
                                slot,
                                armed_run_id,
                                WorkState::Idle,
                                None,
                            ) {
                                events.push(event);
                            }
                            let identity = next_run_event_identity(slot, armed_run_id);
                            events.push(RuntimeEvent::SessionState {
                                identity,
                                session: session_alias,
                                state: LifecycleState::Idle,
                                reason: format!("quiesce timeout {}s", threshold.as_secs()),
                                timestamp: now_rfc3339(),
                            });
                            (events, false)
                        }
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
            self.note_stale_quiesce_drop(session_id, armed_generation);
        }
        for event in events {
            if matches!(&event, RuntimeEvent::SessionWorkState { .. }) {
                self.emit_run_work_state(event, armed_generation, armed_run_id);
            } else {
                self.emit_run_event(event);
            }
        }
    }

    fn note_stale_quiesce_drop(&self, session_id: SessionId, generation: SessionGeneration) {
        let counter = {
            let mut counts = self.inner.stale_quiesce_drop_counts.lock();
            let count = counts.entry((session_id, generation)).or_insert(0);
            *count += 1;
            *count
        };

        if counter == 1 || counter % 10 == 0 {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Info,
                message: format!(
                    "Dropped stale quiesce timer for session '{session_id}' generation {generation} (count={counter})"
                ),
                timestamp: now_rfc3339(),
            });
        }
    }

    fn handle_session_work_state_side_effects(
        &self,
        session_id: SessionId,
        generation: SessionGeneration,
        run_id: Uuid,
        state: WorkState,
    ) {
        let is_stall_state = matches!(state, WorkState::Blocked | WorkState::ErrorLoop);
        let now = Instant::now();
        let timestamp = now_rfc3339();
        let enabled = self.inner.auto_restart_on_stall.enabled_for(session_id);
        let threshold = self.inner.auto_restart_on_stall.threshold;
        let weak = Arc::downgrade(&self.inner);

        let mut slots = self.inner.slots.lock();
        let Some(slot) = slots
            .get_by_id_mut(session_id)
            .filter(|slot| slot.generation == generation && slot.run_id == Some(run_id))
        else {
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

        let task = self.inner.background_runtime.spawn(async move {
            tokio::time::sleep(threshold).await;
            let Some(inner) = weak.upgrade() else {
                return;
            };
            SupervisorHandle { inner }
                .fire_stall_detector(session_id, generation, run_id, state, now)
                .await;
        });
        slot.stall_detector = Some(StallDetector {
            generation,
            run_id,
            state,
            entered_at: now,
            handle: task,
        });
    }

    async fn fire_stall_detector(
        &self,
        session_id: SessionId,
        generation: SessionGeneration,
        run_id: Uuid,
        state: WorkState,
        entered_at: Instant,
    ) {
        #[cfg(test)]
        if let Some(hook) = self.inner.stall_before_reservation.lock().take() {
            hook();
        }

        let Some((stall, reservation)) =
            self.reserve_current_stall_attempt(session_id, generation, run_id, state, entered_at)
        else {
            return;
        };
        let session_alias = self
            .inner
            .slots
            .lock()
            .get_by_id(session_id)
            .map(|slot| slot.definition.alias.clone())
            .unwrap_or_else(|| session_alias(session_id));

        #[cfg(test)]
        if let Some(hook) = self.inner.stall_after_reservation.lock().take() {
            hook();
        }
        if !self.is_current_run(session_id, generation, run_id) {
            self.rollback_auto_restart_reservation(session_id, reservation);
            return;
        }

        match reservation {
            AutoRestartReservation::Reserved(reserved_at) => {
                let restart_handle = self.clone();
                let restart_session_id = stall.session_id;
                let join = tokio::task::spawn_blocking(move || {
                    restart_handle.restart_session_at(restart_session_id, generation, Some(run_id))
                })
                .await;
                match join {
                    Ok(Ok(_)) => self.emit_supervisor_alert(SupervisorAlertEvent {
                        alert_type: SupervisorAlertType::SessionStallDetected,
                        request_id: None,
                        session: Some(session_alias.clone()),
                        action: Some("restart_session".into()),
                        last_work_state: stall.last_work_state,
                        last_session_state: stall.last_session_state,
                        message: format!(
                            "Session {} run {} stayed in {} for {}s; auto-restart completed",
                            session_alias,
                            run_id,
                            work_state_alert_label(stall.last_work_state),
                            self.inner.auto_restart_on_stall.threshold.as_secs()
                        ),
                        severity: AlertSeverity::Critical,
                    }),
                    Ok(Err(error)) => {
                        if error.to_string().contains("superseded") {
                            self.rollback_auto_restart_reservation(
                                session_id,
                                AutoRestartReservation::Reserved(reserved_at),
                            );
                            return;
                        }
                        self.emit_supervisor_alert(SupervisorAlertEvent {
                            alert_type: SupervisorAlertType::OperatorAttention,
                            request_id: None,
                            session: Some(session_alias),
                            action: Some("restart_session".into()),
                            last_work_state: stall.last_work_state,
                            last_session_state: stall.last_session_state,
                            message: format!("Auto-restart failed for run {run_id}: {error}"),
                            severity: AlertSeverity::Critical,
                        });
                    }
                    Err(error) => self.emit_supervisor_alert(SupervisorAlertEvent {
                        alert_type: SupervisorAlertType::OperatorAttention,
                        request_id: None,
                        session: Some(session_alias),
                        action: Some("restart_session".into()),
                        last_work_state: stall.last_work_state,
                        last_session_state: stall.last_session_state,
                        message: format!(
                            "Auto-restart worker join failed for run {run_id}: {error}"
                        ),
                        severity: AlertSeverity::Critical,
                    }),
                }
            }
            AutoRestartReservation::CapReached => {
                self.emit_supervisor_alert(SupervisorAlertEvent {
                    alert_type: SupervisorAlertType::SessionStallDetected,
                    request_id: None,
                    session: Some(session_alias),
                    action: Some("restart_session".into()),
                    last_work_state: stall.last_work_state,
                    last_session_state: stall.last_session_state,
                    message: format!(
                        "Auto-restart cap reached for run {run_id}: 3 restarts in 30 minutes; manual operator intervention required"
                    ),
                    severity: AlertSeverity::Critical,
                });
            }
            AutoRestartReservation::Disabled => {}
        }
    }

    fn reserve_current_stall_attempt(
        &self,
        session_id: SessionId,
        generation: SessionGeneration,
        run_id: Uuid,
        state: WorkState,
        entered_at: Instant,
    ) -> Option<(StallAlertSnapshot, AutoRestartReservation)> {
        if !self.inner.auto_restart_on_stall.enabled_for(session_id) {
            return None;
        }
        let mut slots = self.inner.slots.lock();
        let slot = slots.get_by_id_mut(session_id)?;
        let detector_matches = slot
            .stall_detector
            .as_ref()
            .map(|detector| {
                detector.generation == generation
                    && detector.run_id == run_id
                    && detector.state == state
                    && detector.entered_at == entered_at
            })
            .unwrap_or(false);
        if !detector_matches
            || slot.generation != generation
            || slot.run_id != Some(run_id)
            || slot.work_state != state
            || !matches!(slot.work_state, WorkState::Blocked | WorkState::ErrorLoop)
            || slot.state != LifecycleState::Ready
            || slot.running.is_none()
            || slot.lifecycle_operation.is_some()
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

        let now = Instant::now();
        let reservation = {
            let mut history_by_session = self.inner.auto_restart_history.lock();
            let history = history_by_session.entry(session_id).or_default();
            if history.disabled {
                AutoRestartReservation::Disabled
            } else {
                history
                    .attempts
                    .retain(|attempt| now.duration_since(*attempt) <= AUTO_RESTART_WINDOW);
                if history.attempts.len() >= AUTO_RESTART_MAX_PER_WINDOW {
                    history.disabled = true;
                    AutoRestartReservation::CapReached
                } else {
                    history.attempts.push(now);
                    AutoRestartReservation::Reserved(now)
                }
            }
        };
        slot.stall_detector.take();
        Some((
            StallAlertSnapshot {
                session_id: slot.session_id,
                last_work_state: slot.work_state_observed.then_some(slot.work_state),
                last_session_state: Some(slot.state),
            },
            reservation,
        ))
    }

    fn rollback_auto_restart_reservation(
        &self,
        session_id: SessionId,
        reservation: AutoRestartReservation,
    ) {
        let mut history_by_session = self.inner.auto_restart_history.lock();
        let Some(history) = history_by_session.get_mut(&session_id) else {
            return;
        };
        match reservation {
            AutoRestartReservation::Reserved(reserved_at) => {
                history.attempts.retain(|attempt| *attempt != reserved_at);
            }
            AutoRestartReservation::CapReached => history.disabled = false,
            AutoRestartReservation::Disabled => {}
        }
        if history.attempts.is_empty() && !history.disabled {
            history_by_session.remove(&session_id);
        }
    }

    #[cfg(test)]
    fn set_pty_spawner_for_tests(&self, spawner: Arc<dyn PtySpawner>) {
        *self.inner.pty_spawner.write() = spawner;
    }

    #[cfg(test)]
    fn set_executable_resolver_for_tests(&self, resolver: Arc<dyn DriverExecutableResolver>) {
        *self.inner.executable_resolver.write() = resolver;
    }

    #[cfg(test)]
    fn set_wsl_control_for_tests(&self, control: Arc<dyn WslControl>) {
        *self.inner.wsl_control.write() = Arc::clone(&control);
        *self.inner.pty_spawner.write() = Arc::new(ConcretePtySpawner { wsl: control });
        *self.inner.wsl_reconciliation_error.lock() = None;
    }

    #[cfg(test)]
    fn set_pty_event_before_commit_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.pty_event_before_commit.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_work_state_before_side_effect_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.work_state_before_side_effect.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_stall_before_reservation_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.stall_before_reservation.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_stall_after_reservation_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.stall_after_reservation.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_sideband_write_timeout_for_tests(&self, timeout: Duration) {
        *self.inner.sideband_write_timeout.lock() = timeout;
    }

    #[cfg(test)]
    fn set_stop_kill_timeout_for_tests(&self, timeout: Duration) {
        *self.inner.stop_kill_timeout.lock() = timeout;
    }

    #[cfg(test)]
    fn fail_next_catalog_write_for_tests(&self) {
        self.inner
            .fail_next_catalog_write
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn fail_next_room_catalog_write_for_tests(&self) {
        self.inner
            .fail_next_room_catalog_write
            .store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn set_run_input_before_commit_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.run_input_before_commit.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_room_delivery_after_preflight_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.room_delivery_after_preflight.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn set_room_event_after_append_for_tests<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        *self.inner.room_event_after_append.lock() = Some(Box::new(hook));
    }

    #[cfg(test)]
    fn run_room_event_after_append_hook_for_tests(&self) {
        if let Some(hook) = self.inner.room_event_after_append.lock().take() {
            hook();
        }
    }

    pub fn snapshot(&self) -> RuntimeSnapshot {
        self.refresh_session_liveness();
        let sessions = self.inner.slots.lock().ordered_snapshots();
        let rooms = self.inner.rooms.lock().snapshots();
        let workspace_preference = self
            .inner
            .catalog
            .lock()
            .workspace_preference
            .canonical_path
            .clone();
        let control_plane = self
            .inner
            .control_plane
            .read()
            .as_ref()
            .map(ControlPlaneSnapshot::from);

        RuntimeSnapshot {
            sessions,
            rooms,
            workspace_preference,
            control_plane,
            runtime_dir: self.runtime_dir().display().to_string(),
            audit_log_path: self.audit_log_path().display().to_string(),
            generated_at: now_rfc3339(),
        }
    }

    pub fn start_session_by_id(&self, session_id: SessionId) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        self.refresh_session_liveness();
        let (initial_generation, definition, qualified_directory) = {
            let slots = self.inner.slots.lock();
            self.ensure_active()?;
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.lifecycle_operation.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a lifecycle operation in progress"
                ));
            }
            if slot.termination_uncertain {
                return Err(anyhow!(
                    "session '{session_id}' has an unverified prior termination and cannot be started"
                ));
            }
            if slot.spawn_in_flight.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a spawn in progress"
                ));
            }
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }
            let qualified_directory = slot
                .qualified_working_directory
                .as_ref()
                .ok_or_else(|| {
                    anyhow!("session '{session_id}' has no qualified working directory")
                })?
                .clone();
            (
                slot.generation,
                slot.definition.clone(),
                qualified_directory,
            )
        };
        let lease = self.revalidate_session_working_directory(
            session_id,
            definition.driver,
            &qualified_directory,
        )?;
        let plan = self.prepare_launch_spec_for_spawn(&definition, &qualified_directory)?;
        let expected = {
            let mut slots = self.inner.slots.lock();
            self.ensure_active()?;
            let slot = slots
                .get_by_id_mut(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.lifecycle_operation.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a lifecycle operation in progress"
                ));
            }
            if slot.termination_uncertain {
                return Err(anyhow!(
                    "session '{session_id}' has an unverified prior termination and cannot be started"
                ));
            }
            if slot.spawn_in_flight.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a spawn in progress"
                ));
            }
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }
            if slot.generation != initial_generation
                || slot.definition != definition
                || slot.qualified_working_directory.as_ref() != Some(&qualified_directory)
            {
                return Err(anyhow!(
                    "session '{session_id}' definition changed while launch was being prepared; retry"
                ));
            }
            ensure_run_event_capacity(slot, 1)?;
            cancel_quiesce_timer_locked(slot);
            slot.run_id = None;
            slot.generation = slot
                .generation
                .checked_add(1)
                .ok_or_else(|| anyhow!("session generation exhausted for '{session_id}'"))?;
            slot.state = LifecycleState::Starting;
            slot.generation
        };
        self.start_session_at(session_id, expected, lease, plan)
    }

    pub fn stop_session_by_id(&self, session_id: SessionId) -> Result<SessionSnapshot> {
        {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.state == LifecycleState::Closed
                && slot.running.is_none()
                && slot.run_id.is_none()
                && slot.spawn_in_flight.is_none()
                && slot.lifecycle_operation.is_none()
                && slot.stop_intent.is_none()
                && !slot.termination_uncertain
            {
                return Ok(slot.snapshot());
            }
        }
        let (_, expected) =
            self.declare_stop_operation(session_id, None, StopIntentKind::Operator)?;
        self.stop_session_at(session_id, expected)
    }

    pub fn restart_session_by_id(&self, session_id: SessionId) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let (expected_generation, expected_run_id) = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.lifecycle_operation.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a lifecycle operation in progress"
                ));
            }
            if slot.termination_uncertain {
                return Err(anyhow!(
                    "session '{session_id}' has an unverified prior termination and cannot be restarted"
                ));
            }
            if slot.spawn_in_flight.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a spawn in progress"
                ));
            }
            ensure_run_event_capacity(slot, 5)?;
            (slot.generation, slot.run_id)
        };
        self.restart_session_at(session_id, expected_generation, expected_run_id)
    }

    pub fn shutdown(&self) -> Result<()> {
        let _shutdown = self.inner.shutdown_lifecycle.lock();
        if self.inner.shutdown_started.swap(true, Ordering::AcqRel) {
            let slots = self.inner.slots.lock();
            let cleanup_pending = slots.ordered_slots().any(|slot| {
                slot.running.is_some()
                    || slot.spawn_in_flight.is_some()
                    || slot.lifecycle_operation.is_some()
                    || slot.termination_uncertain
            });
            drop(slots);
            #[cfg(windows)]
            let listener_cleanup_pending = self.inner.control_plane_listener.lock().is_some();
            #[cfg(not(windows))]
            let listener_cleanup_pending = false;
            if !cleanup_pending && !listener_cleanup_pending {
                return Ok(());
            }
        }

        #[cfg(windows)]
        let listener = {
            let _lifecycle = self.inner.control_plane_lifecycle.lock();
            self.inner.control_plane.write().take();
            self.inner.control_plane_listener.lock().take()
        };

        #[cfg(not(windows))]
        {
            let _lifecycle = self.inner.control_plane_lifecycle.lock();
            self.inner.control_plane.write().take();
        }

        #[cfg(windows)]
        if let Some(listener) = listener.as_ref() {
            listener.cancel();
        }

        let kill_timeout = *self.inner.stop_kill_timeout.lock();
        let deadline = Instant::now() + kill_timeout;
        let (termination_attempts, mut errors) = {
            let mut slots = self.inner.slots.lock();
            let mut attempts = Vec::new();
            let mut errors = Vec::new();
            for slot in slots.by_id.values_mut() {
                cancel_quiesce_timer_locked(slot);
                slot.run_id = None;
                slot.stop_intent = None;
                slot.last_activity_at = Some(now_rfc3339());
                slot.last_real_output_at = None;
                reset_work_state_locked(slot);

                if let Some(operation) = slot.lifecycle_operation {
                    slot.state = LifecycleState::Failed;
                    slot.termination_uncertain = true;
                    slot.last_error = Some(format!(
                        "shutdown could not overtake the in-flight {:?} lifecycle operation at generation {}",
                        operation.kind, operation.generation
                    ));
                    errors.push(format!(
                        "session '{}' still has an in-flight lifecycle operation",
                        slot.definition.alias
                    ));
                    continue;
                }

                if slot.spawn_in_flight.is_some() {
                    slot.generation = slot.generation.saturating_add(1);
                    slot.state = LifecycleState::Failed;
                    slot.termination_uncertain = true;
                    slot.last_error = Some(
                        "shutdown superseded a spawn that has not returned a process-scope termination receipt"
                            .into(),
                    );
                    errors.push(format!(
                        "session '{}' still has a spawn in flight",
                        slot.definition.alias
                    ));
                    continue;
                }

                let Some(running) = slot.running.as_ref().cloned() else {
                    if slot.termination_uncertain {
                        slot.state = LifecycleState::Failed;
                        errors.push(format!(
                            "session '{}' has no retained process-scope owner for re-proof",
                            slot.definition.alias
                        ));
                    } else {
                        slot.state = LifecycleState::Closed;
                        slot.process_id = None;
                    }
                    continue;
                };

                slot.generation = slot.generation.saturating_add(1);
                slot.lifecycle_operation = Some(LifecycleOperation {
                    generation: slot.generation,
                    kind: StopIntentKind::Operator,
                });
                slot.state = LifecycleState::Failed;
                slot.termination_uncertain = true;
                slot.last_error = Some("shutdown process-scope termination is in progress".into());
                running.close_input();
                if let Some(pty) = running.pty.as_ref().cloned() {
                    let receiver = begin_termination_attempt(
                        pty.clone(),
                        running.input_gate.clone(),
                        deadline,
                    );
                    attempts.push((
                        slot.session_id,
                        slot.generation,
                        slot.definition.alias.clone(),
                        pty,
                        receiver,
                    ));
                } else {
                    errors.push(format!(
                        "session '{}' has no retained PTY process-scope owner",
                        slot.definition.alias
                    ));
                }
            }
            (attempts, errors)
        };

        for (session_id, generation, alias, pty, receiver) in termination_attempts {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let attempt = receiver.recv_timeout(remaining).ok();
            let timed_out = attempt.is_none();
            let proved = attempt
                .as_ref()
                .is_some_and(|attempt| attempt.kill_error.is_none() && attempt.input_idle);
            let mut detail = match &attempt {
                Some(attempt) if attempt.kill_error.is_some() => {
                    format!("process-scope termination failed: {}", attempt.kill_error.as_deref().unwrap_or_default())
                }
                Some(_) if !proved => "process scope terminated but input writers did not drain before the shutdown deadline".into(),
                Some(_) => "shutdown process-scope termination proved".into(),
                None => format!(
                    "process-scope termination did not finish within {}ms",
                    kill_timeout.as_millis()
                ),
            };
            let committed_proof = {
                let mut slots = self.inner.slots.lock();
                if let Some(slot) = slots.get_by_id_mut(session_id)
                    && slot.generation == generation
                    && slot.lifecycle_operation
                        == Some(LifecycleOperation {
                            generation,
                            kind: StopIntentKind::Operator,
                        })
                {
                    let owns_same_pty = slot
                        .running
                        .as_ref()
                        .and_then(|running| running.pty.as_ref())
                        .is_some_and(|current| Arc::ptr_eq(current, &pty));
                    let committed_proof = proved && owns_same_pty;
                    if committed_proof {
                        slot.running = None;
                        slot.process_id = None;
                        slot.termination_uncertain = false;
                        slot.state = LifecycleState::Closed;
                        slot.last_error = None;
                    } else {
                        if proved && !owns_same_pty {
                            detail =
                                "process-scope owner changed before shutdown proof could commit"
                                    .into();
                        }
                        slot.termination_uncertain = true;
                        slot.state = LifecycleState::Failed;
                        slot.last_error = Some(detail.clone());
                    }
                    if !timed_out {
                        slot.lifecycle_operation = None;
                    }
                    committed_proof
                } else {
                    detail =
                        "shutdown termination reservation changed before proof could commit".into();
                    false
                }
            };
            if !committed_proof {
                errors.push(format!("session '{alias}': {detail}"));
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!("shutdown: {alias}: {detail}"),
                    timestamp: now_rfc3339(),
                });
            }
            if timed_out {
                self.schedule_late_termination_reproof(
                    session_id,
                    generation,
                    StopIntentKind::Operator,
                    pty,
                    receiver,
                    "shutdown",
                );
            }
        }

        #[cfg(windows)]
        {
            if let Some(listener) = listener {
                match listener.join_until(deadline) {
                    ControlPlaneJoinOutcome::Joined(Ok(())) => {}
                    ControlPlaneJoinOutcome::Joined(Err(error)) => {
                        errors.push(format!("control-plane listener shutdown failed: {error:#}"));
                    }
                    ControlPlaneJoinOutcome::TimedOut(listener) => {
                        let mut retained = self.inner.control_plane_listener.lock();
                        if retained.is_none() {
                            *retained = Some(listener);
                        } else {
                            errors.push(
                                "control-plane listener shutdown lost its exclusive retained handle"
                                    .into(),
                            );
                        }
                        errors.push(format!(
                            "control-plane listener did not stop within the {}ms shutdown deadline",
                            kill_timeout.as_millis()
                        ));
                    }
                }
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!("shutdown incomplete: {}", errors.join("; ")))
        }
    }

    fn persist_catalog_candidate(&self, candidate: &SessionCatalogV1) -> Result<()> {
        #[cfg(test)]
        if self
            .inner
            .fail_next_catalog_write
            .swap(false, Ordering::AcqRel)
        {
            return Err(anyhow!("injected session catalog persistence failure"));
        }
        persist_session_catalog(&self.inner.runtime_dir, candidate)
    }

    fn persist_room_catalog_candidate(&self, candidate: &RoomCatalogV1) -> Result<()> {
        #[cfg(test)]
        if self
            .inner
            .fail_next_room_catalog_write
            .swap(false, Ordering::AcqRel)
        {
            return Err(anyhow!("injected room catalog persistence failure"));
        }
        persist_room_catalog(&self.inner.runtime_dir, candidate)
    }

    fn ensure_wsl_reconciled(&self) -> Result<()> {
        if self.inner.wsl_reconciliation_error.lock().is_none() {
            return Ok(());
        }
        let control = self.inner.wsl_control.read().clone();
        match control.reconcile_stale_scopes() {
            Ok(()) => {
                *self.inner.wsl_reconciliation_error.lock() = None;
                Ok(())
            }
            Err(error) => {
                let message = format!("{error:#}");
                *self.inner.wsl_reconciliation_error.lock() = Some(message.clone());
                Err(anyhow!(
                    "Prime/WSL startup reconciliation is incomplete: {message}"
                ))
            }
        }
    }

    fn revalidate_session_working_directory(
        &self,
        session_id: SessionId,
        driver: DriverKind,
        expected: &QualifiedWorkingDirectory,
    ) -> Result<SessionDirectoryLease> {
        validate_driver_working_directory_pair(driver, expected)?;
        match expected.namespace {
            WorkingDirectoryNamespace::Windows => revalidate_qualified_working_directory(
                session_id,
                expected,
                &self.inner.runtime_dir,
            )
            .map(|lease| SessionDirectoryLease::Windows { _lease: lease }),
            WorkingDirectoryNamespace::WslUbuntu => {
                self.ensure_wsl_reconciled()?;
                self.inner
                    .wsl_control
                    .read()
                    .revalidate_working_directory(expected)?;
                Ok(SessionDirectoryLease::WslUbuntu)
            }
        }
    }

    pub fn set_workspace_preference(&self, path: &Path) -> Result<String> {
        self.ensure_active()?;
        let (qualified, _lease) = qualify_working_directory(path, &self.inner.runtime_dir)?;
        let mut catalog = self.inner.catalog.lock();
        let mut candidate = catalog.clone();
        candidate.workspace_preference = qualified.clone();
        self.persist_catalog_candidate(&candidate)?;
        *catalog = candidate;
        Ok(qualified.canonical_path)
    }

    pub fn prime_default_working_directory(&self) -> Result<String> {
        self.ensure_active()?;
        self.ensure_wsl_reconciled()?;
        let control = self.inner.wsl_control.read().clone();
        let candidate = control.default_working_directory()?;
        Ok(control
            .qualify_working_directory(&candidate)?
            .canonical_path)
    }

    pub fn create_session(
        &self,
        request: shared_types::CreateSessionRequest,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let label = request.label.unwrap_or_else(|| match request.driver {
            DriverKind::Claude => "Claude".into(),
            DriverKind::Codex => "Codex".into(),
            DriverKind::Grok => "Grok".into(),
            DriverKind::Prime => "Prime".into(),
            DriverKind::GenericTerminal => "Terminal".into(),
        });
        validate_session_label(&label)?;
        if matches!(
            request.driver,
            DriverKind::GenericTerminal | DriverKind::Prime
        ) && request.permission_profile == shared_types::PermissionProfile::Unsafe
        {
            return Err(anyhow!(
                "{:?} sessions do not support the unsafe permission profile",
                request.driver
            ));
        }

        let workspace_preference = self.inner.catalog.lock().workspace_preference.clone();
        let qualified_directory = if request.driver == DriverKind::Prime {
            self.ensure_wsl_reconciled()?;
            let control = self.inner.wsl_control.read().clone();
            let candidate = match request.linux_working_directory.as_deref() {
                Some(path) if !path.trim().is_empty() => path.to_owned(),
                Some(_) => {
                    return Err(anyhow!("Prime Linux working directory must not be blank"));
                }
                None => control.default_working_directory()?,
            };
            control.qualify_working_directory(&candidate)?
        } else {
            if request.linux_working_directory.is_some() {
                return Err(anyhow!(
                    "linux_working_directory is supported only for Prime sessions"
                ));
            }
            let _lease = revalidate_qualified_directory(
                "workspace preference",
                &workspace_preference,
                &self.inner.runtime_dir,
            )?;
            workspace_preference.clone()
        };
        let session_id = Uuid::new_v4();
        let definition = SessionDefinition {
            session_id,
            alias: session_alias(session_id),
            label: label.clone(),
            driver: request.driver,
            working_dir: qualified_directory.canonical_path.clone(),
            permission_profile: request.permission_profile,
        };
        let mut slot = closed_session_slot(definition);
        slot.qualified_working_directory = Some(qualified_directory.clone());
        let snapshot = slot.snapshot();

        {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            if request.driver != DriverKind::Prime
                && catalog.workspace_preference != workspace_preference
            {
                return Err(anyhow!(
                    "workspace preference changed while the session was being created; retry"
                ));
            }
            slots.validate_insert(&slot)?;
            let mut candidate = catalog.clone();
            candidate.sessions.push(PersistedSessionV1 {
                session_id,
                label,
                driver: request.driver,
                working_directory: qualified_directory,
                permission_profile: request.permission_profile,
            });
            self.persist_catalog_candidate(&candidate)?;
            slots.insert_prevalidated(slot);
            self.inner
                .run_event_publish
                .lock()
                .insert(session_id, RunEventPublishState::new(1));
            *catalog = candidate;
        }

        self.emit(RuntimeEvent::SessionCreated {
            schema_version: 1,
            session: snapshot.clone(),
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn rename_session(&self, session_id: SessionId, label: &str) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        validate_session_label(label)?;
        let (snapshot, old_label) = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let old_label = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?
                .definition
                .label
                .clone();
            let mut candidate = catalog.clone();
            let persisted = candidate
                .sessions
                .iter_mut()
                .find(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?;
            persisted.label = label.to_string();
            self.persist_catalog_candidate(&candidate)?;
            let slot = slots
                .get_by_id_mut(session_id)
                .expect("validated session disappeared during rename");
            slot.definition.label = label.to_string();
            let snapshot = slot.snapshot();
            *catalog = candidate;
            (snapshot, old_label)
        };
        self.emit(RuntimeEvent::SessionRenamed {
            schema_version: 1,
            session_id,
            old_label,
            new_label: label.to_string(),
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn set_session_working_directory(
        &self,
        session_id: SessionId,
        path: &Path,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let (qualified, _lease) = qualify_working_directory(path, &self.inner.runtime_dir)?;
        let (snapshot, old_working_dir) = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            ensure_closed_for_definition_edit(slot)?;
            if slot.definition.driver == DriverKind::Prime {
                return Err(anyhow!(
                    "Prime sessions require an Ubuntu Linux working directory"
                ));
            }
            let old_working_dir = slot.definition.working_dir.clone();
            let mut candidate = catalog.clone();
            let persisted = candidate
                .sessions
                .iter_mut()
                .find(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?;
            persisted.working_directory = qualified.clone();
            self.persist_catalog_candidate(&candidate)?;
            let slot = slots
                .get_by_id_mut(session_id)
                .expect("validated session disappeared during cwd update");
            slot.definition.working_dir = qualified.canonical_path.clone();
            slot.qualified_working_directory = Some(qualified.clone());
            let snapshot = slot.snapshot();
            *catalog = candidate;
            (snapshot, old_working_dir)
        };
        self.emit(RuntimeEvent::SessionWorkingDirectoryChanged {
            schema_version: 1,
            session_id,
            old_working_dir,
            new_working_dir: qualified.canonical_path,
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn set_session_linux_working_directory(
        &self,
        session_id: SessionId,
        path: &str,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            ensure_closed_for_definition_edit(slot)?;
            if slot.definition.driver != DriverKind::Prime {
                return Err(anyhow!(
                    "Linux working directories are supported only for Prime sessions"
                ));
            }
        }
        self.ensure_wsl_reconciled()?;
        let qualified = self
            .inner
            .wsl_control
            .read()
            .qualify_working_directory(path)?;
        let (snapshot, old_working_dir) = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            ensure_closed_for_definition_edit(slot)?;
            if slot.definition.driver != DriverKind::Prime {
                return Err(anyhow!(
                    "Linux working directories are supported only for Prime sessions"
                ));
            }
            let old_working_dir = slot.definition.working_dir.clone();
            let mut candidate = catalog.clone();
            let persisted = candidate
                .sessions
                .iter_mut()
                .find(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?;
            persisted.working_directory = qualified.clone();
            self.persist_catalog_candidate(&candidate)?;
            let slot = slots
                .get_by_id_mut(session_id)
                .expect("validated session disappeared during Linux cwd update");
            slot.definition.working_dir = qualified.canonical_path.clone();
            slot.qualified_working_directory = Some(qualified.clone());
            let snapshot = slot.snapshot();
            *catalog = candidate;
            (snapshot, old_working_dir)
        };
        self.emit(RuntimeEvent::SessionWorkingDirectoryChanged {
            schema_version: 1,
            session_id,
            old_working_dir,
            new_working_dir: qualified.canonical_path,
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn set_permission_profile(
        &self,
        session_id: SessionId,
        permission_profile: shared_types::PermissionProfile,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let (snapshot, old_profile) = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            ensure_closed_for_definition_edit(slot)?;
            if matches!(
                slot.definition.driver,
                DriverKind::GenericTerminal | DriverKind::Prime
            ) && permission_profile == shared_types::PermissionProfile::Unsafe
            {
                return Err(anyhow!(
                    "{:?} sessions do not support the unsafe permission profile",
                    slot.definition.driver
                ));
            }
            let old_profile = slot.definition.permission_profile;
            let mut candidate = catalog.clone();
            let persisted = candidate
                .sessions
                .iter_mut()
                .find(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?;
            persisted.permission_profile = permission_profile;
            self.persist_catalog_candidate(&candidate)?;
            let slot = slots
                .get_by_id_mut(session_id)
                .expect("validated session disappeared during permission update");
            slot.definition.permission_profile = permission_profile;
            let snapshot = slot.snapshot();
            *catalog = candidate;
            (snapshot, old_profile)
        };
        self.emit(RuntimeEvent::SessionPermissionChanged {
            schema_version: 1,
            session_id,
            old_profile,
            new_profile: permission_profile,
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn move_session(&self, session_id: SessionId, new_index: usize) -> Result<RuntimeSnapshot> {
        self.ensure_active()?;
        let old_index = {
            let mut slots = self.inner.slots.lock();
            let mut catalog = self.inner.catalog.lock();
            let old_index = slots
                .order
                .iter()
                .position(|candidate| *candidate == session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if new_index >= slots.order.len() {
                return Err(anyhow!(
                    "session index {new_index} is out of bounds for {} sessions",
                    slots.order.len()
                ));
            }
            if old_index == new_index {
                drop(catalog);
                drop(slots);
                return Ok(self.snapshot());
            }
            let mut candidate = catalog.clone();
            let persisted = candidate.sessions.remove(old_index);
            candidate.sessions.insert(new_index, persisted);
            self.persist_catalog_candidate(&candidate)?;
            let moved = slots.order.remove(old_index);
            slots.order.insert(new_index, moved);
            *catalog = candidate;
            old_index
        };
        self.emit(RuntimeEvent::SessionMoved {
            schema_version: 1,
            session_id,
            old_index,
            new_index,
            timestamp: now_rfc3339(),
        });
        Ok(self.snapshot())
    }

    pub fn delete_session(&self, session_id: SessionId) -> Result<()> {
        self.ensure_active()?;
        self.refresh_session_liveness();
        let label = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            ensure_closed_for_definition_edit(slot)?;
            let next_sequence = slot
                .run_event_sequence
                .checked_add(1)
                .ok_or_else(|| anyhow!("session run-event sequence is exhausted"))?;
            {
                let streams = self.inner.run_event_publish.lock();
                let stream = streams
                    .get(&session_id)
                    .ok_or_else(|| anyhow!("session event stream is missing"))?;
                if stream.draining
                    || !stream.pending.is_empty()
                    || stream.next_sequence != Some(next_sequence)
                {
                    return Err(anyhow!(
                        "session event stream is still draining; retry deletion"
                    ));
                }
            }
            let rooms = self.inner.rooms.lock();
            if let Some(room_id) = rooms.room_by_session.get(&session_id) {
                return Err(anyhow!(
                    "session '{session_id}' belongs to room '{room_id}'; remove it from the room before deletion"
                ));
            }
            let label = slot.definition.label.clone();
            let mut catalog = self.inner.catalog.lock();
            let mut candidate = catalog.clone();
            let index = candidate
                .sessions
                .iter()
                .position(|session| session.session_id == session_id)
                .ok_or_else(|| anyhow!("session '{session_id}' is missing from the catalog"))?;
            candidate.sessions.remove(index);
            self.persist_catalog_candidate(&candidate)?;
            slots
                .remove(session_id)
                .expect("validated session disappeared during deletion");
            self.inner.run_event_publish.lock().remove(&session_id);
            *catalog = candidate;
            label
        };
        self.emit(RuntimeEvent::SessionDeleted {
            schema_version: 1,
            session_id,
            label,
            timestamp: now_rfc3339(),
        });
        Ok(())
    }

    pub fn create_room(&self, request: CreateRoomRequest) -> Result<RoomSnapshot> {
        self.ensure_active()?;
        let mut unique_members = HashSet::new();
        if request.member_ids.len() < 2 {
            return Err(anyhow!("room creation requires at least two sessions"));
        }
        if request.member_ids.len() > ROOM_MEMBER_MAX_COUNT {
            return Err(anyhow!(
                "room creation cannot exceed {ROOM_MEMBER_MAX_COUNT} sessions"
            ));
        }
        for session_id in &request.member_ids {
            if !unique_members.insert(*session_id) {
                return Err(anyhow!(
                    "room creation contains duplicate session '{session_id}'"
                ));
            }
        }

        let _room_event_publish = self.inner.room_event_publish.lock();
        let (snapshot, feed_events) = {
            let slots = self.inner.slots.lock();
            for session_id in &request.member_ids {
                if slots.get_by_id(*session_id).is_none() {
                    return Err(anyhow!("unknown session id '{session_id}'"));
                }
            }
            let mut rooms = self.inner.rooms.lock();
            if rooms.order.len() >= ROOM_MAX_COUNT {
                return Err(anyhow!("room limit of {ROOM_MAX_COUNT} has been reached"));
            }
            for session_id in &request.member_ids {
                if let Some(room_id) = rooms.room_by_session.get(session_id) {
                    return Err(anyhow!(
                        "session '{session_id}' already belongs to room '{room_id}'"
                    ));
                }
            }
            let label = request
                .label
                .unwrap_or_else(|| default_room_label(rooms.order.len()));
            validate_room_label(&label)?;
            let mut room_id = Uuid::new_v4();
            while rooms.by_id.contains_key(&room_id) {
                room_id = Uuid::new_v4();
            }
            let definition = PersistedRoomV1 {
                room_id,
                label,
                member_ids: request.member_ids.clone(),
                membership_revision: 1,
            };
            let mut runtime = RoomRuntime::from_persisted(definition.clone());
            let mut feed_events = Vec::with_capacity(definition.member_ids.len());
            for session_id in &definition.member_ids {
                feed_events.push(runtime.append(RoomFeedItem::Membership {
                    action: RoomMembershipAction::Joined,
                    session_id: *session_id,
                    membership_revision: definition.membership_revision,
                })?);
            }
            let snapshot = runtime.snapshot();
            let mut candidate = rooms.catalog.clone();
            candidate.rooms.push(definition);
            self.persist_room_catalog_candidate(&candidate)?;
            for session_id in &request.member_ids {
                rooms.room_by_session.insert(*session_id, room_id);
            }
            rooms.order.push(room_id);
            rooms.by_id.insert(room_id, runtime);
            rooms.catalog = candidate;
            (snapshot, feed_events)
        };

        #[cfg(test)]
        self.run_room_event_after_append_hook_for_tests();
        self.emit(RuntimeEvent::RoomCreated {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room: snapshot.clone(),
            timestamp: now_rfc3339(),
        });
        for feed_event in feed_events {
            self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
        }
        Ok(snapshot)
    }

    pub fn rename_room(&self, request: RenameRoomRequest) -> Result<RoomSnapshot> {
        self.ensure_active()?;
        validate_room_label(&request.label)?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let (snapshot, old_label) = {
            let mut rooms = self.inner.rooms.lock();
            let old_label = rooms
                .get(request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?
                .definition
                .label
                .clone();
            let mut candidate = rooms.catalog.clone();
            candidate
                .rooms
                .iter_mut()
                .find(|room| room.room_id == request.room_id)
                .expect("validated room missing from persisted catalog")
                .label = request.label.clone();
            self.persist_room_catalog_candidate(&candidate)?;
            let room = rooms
                .get_mut(request.room_id)
                .expect("validated room disappeared during rename");
            room.definition.label = request.label.clone();
            let snapshot = room.snapshot();
            rooms.catalog = candidate;
            (snapshot, old_label)
        };
        self.emit(RuntimeEvent::RoomRenamed {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: request.room_id,
            old_label,
            new_label: request.label,
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn move_room(&self, request: MoveRoomRequest) -> Result<RuntimeSnapshot> {
        self.ensure_active()?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let old_index = {
            let mut rooms = self.inner.rooms.lock();
            let old_index = rooms
                .order
                .iter()
                .position(|room_id| *room_id == request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            if request.new_index >= rooms.order.len() {
                return Err(anyhow!(
                    "room index {} is out of bounds for {} rooms",
                    request.new_index,
                    rooms.order.len()
                ));
            }
            if old_index == request.new_index {
                drop(rooms);
                return Ok(self.snapshot());
            }
            let mut candidate = rooms.catalog.clone();
            let persisted = candidate.rooms.remove(old_index);
            candidate.rooms.insert(request.new_index, persisted);
            self.persist_room_catalog_candidate(&candidate)?;
            let moved = rooms.order.remove(old_index);
            rooms.order.insert(request.new_index, moved);
            rooms.catalog = candidate;
            old_index
        };
        self.emit(RuntimeEvent::RoomMoved {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: request.room_id,
            old_index,
            new_index: request.new_index,
            timestamp: now_rfc3339(),
        });
        Ok(self.snapshot())
    }

    pub fn add_room_member(&self, request: AddRoomMemberRequest) -> Result<RoomSnapshot> {
        self.ensure_active()?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let (snapshot, feed_event, revision) = {
            let slots = self.inner.slots.lock();
            if slots.get_by_id(request.session_id).is_none() {
                return Err(anyhow!("unknown session id '{}'", request.session_id));
            }
            let mut rooms = self.inner.rooms.lock();
            if let Some(room_id) = rooms.room_by_session.get(&request.session_id) {
                return Err(anyhow!(
                    "session '{}' already belongs to room '{room_id}'",
                    request.session_id
                ));
            }
            let room = rooms
                .get(request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            if room.definition.member_ids.len() >= ROOM_MEMBER_MAX_COUNT {
                return Err(anyhow!(
                    "room '{}' has reached the {ROOM_MEMBER_MAX_COUNT} member limit",
                    request.room_id
                ));
            }
            let revision = room
                .definition
                .membership_revision
                .checked_add(1)
                .ok_or_else(|| anyhow!("room membership revision exhausted"))?;
            let join_floor = room.next_sequence.saturating_sub(1);
            let prepared = room.prepare_append(RoomFeedItem::Membership {
                action: RoomMembershipAction::Joined,
                session_id: request.session_id,
                membership_revision: revision,
            })?;
            let mut candidate = rooms.catalog.clone();
            let persisted = candidate
                .rooms
                .iter_mut()
                .find(|room| room.room_id == request.room_id)
                .expect("validated room missing from persisted catalog");
            persisted.member_ids.push(request.session_id);
            persisted.membership_revision = revision;
            self.persist_room_catalog_candidate(&candidate)?;
            let room = rooms
                .get_mut(request.room_id)
                .expect("validated room disappeared during membership update");
            room.definition.member_ids.push(request.session_id);
            room.definition.membership_revision = revision;
            room.join_floor_by_session
                .insert(request.session_id, join_floor);
            let feed_event = room.commit_append(prepared);
            let snapshot = room.snapshot();
            rooms
                .room_by_session
                .insert(request.session_id, request.room_id);
            rooms.catalog = candidate;
            (snapshot, feed_event, revision)
        };
        #[cfg(test)]
        self.run_room_event_after_append_hook_for_tests();
        self.emit(RuntimeEvent::RoomMemberAdded {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: request.room_id,
            session_id: request.session_id,
            membership_revision: revision,
            timestamp: now_rfc3339(),
        });
        self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
        Ok(snapshot)
    }

    pub fn remove_room_member(&self, request: RemoveRoomMemberRequest) -> Result<RoomSnapshot> {
        self.ensure_active()?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let (snapshot, feed_event, revision) = {
            let mut rooms = self.inner.rooms.lock();
            let room = rooms
                .get(request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            if !room.definition.member_ids.contains(&request.session_id) {
                return Err(anyhow!(
                    "session '{}' is not a member of room '{}'",
                    request.session_id,
                    request.room_id
                ));
            }
            let revision = room
                .definition
                .membership_revision
                .checked_add(1)
                .ok_or_else(|| anyhow!("room membership revision exhausted"))?;
            let prepared = room.prepare_append(RoomFeedItem::Membership {
                action: RoomMembershipAction::Removed,
                session_id: request.session_id,
                membership_revision: revision,
            })?;
            let mut candidate = rooms.catalog.clone();
            let persisted = candidate
                .rooms
                .iter_mut()
                .find(|room| room.room_id == request.room_id)
                .expect("validated room missing from persisted catalog");
            persisted
                .member_ids
                .retain(|session_id| *session_id != request.session_id);
            persisted.membership_revision = revision;
            self.persist_room_catalog_candidate(&candidate)?;
            let room = rooms
                .get_mut(request.room_id)
                .expect("validated room disappeared during membership update");
            room.definition
                .member_ids
                .retain(|session_id| *session_id != request.session_id);
            room.definition.membership_revision = revision;
            room.join_floor_by_session.remove(&request.session_id);
            let feed_event = room.commit_append(prepared);
            let snapshot = room.snapshot();
            rooms.room_by_session.remove(&request.session_id);
            rooms.catalog = candidate;
            (snapshot, feed_event, revision)
        };
        #[cfg(test)]
        self.run_room_event_after_append_hook_for_tests();
        self.emit(RuntimeEvent::RoomMemberRemoved {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: request.room_id,
            session_id: request.session_id,
            membership_revision: revision,
            timestamp: now_rfc3339(),
        });
        self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
        Ok(snapshot)
    }

    pub fn delete_room(&self, request: DeleteRoomRequest) -> Result<()> {
        self.ensure_active()?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let label = {
            let mut rooms = self.inner.rooms.lock();
            let room = rooms
                .get(request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            if room.in_flight_deliveries != 0 {
                return Err(anyhow!(
                    "room '{}' has a delivery in progress",
                    request.room_id
                ));
            }
            let label = room.definition.label.clone();
            let members = room.definition.member_ids.clone();
            let mut candidate = rooms.catalog.clone();
            candidate
                .rooms
                .retain(|room| room.room_id != request.room_id);
            self.persist_room_catalog_candidate(&candidate)?;
            rooms.by_id.remove(&request.room_id);
            rooms.order.retain(|room_id| *room_id != request.room_id);
            for session_id in members {
                rooms.room_by_session.remove(&session_id);
            }
            rooms.catalog = candidate;
            label
        };
        self.emit(RuntimeEvent::RoomDeleted {
            schema_version: ROOM_EVENT_SCHEMA_VERSION,
            room_id: request.room_id,
            label,
            timestamp: now_rfc3339(),
        });
        Ok(())
    }

    pub fn read_room_feed(&self, request: ReadRoomFeedRequest) -> Result<RoomFeedPage> {
        self.ensure_active()?;
        let rooms = self.inner.rooms.lock();
        rooms
            .get(request.room_id)
            .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?
            .read(request.cursor, 0)
    }

    pub fn post_room_message(&self, request: PostRoomMessageRequest) -> Result<RoomPostResult> {
        self.post_room_message_as(
            request.room_id,
            RoomMessageSender::Operator {},
            request.content,
        )
    }

    pub fn deliver_room_message(
        &self,
        request: DeliverRoomMessageRequest,
    ) -> Result<RoomDeliveryResult> {
        self.ensure_active()?;
        validate_message_body(&request.content)?;
        let (membership_revision, room_label, recipient_ids) = {
            let rooms = self.inner.rooms.lock();
            let room = rooms
                .get(request.room_id)
                .ok_or_else(|| anyhow!("unknown room id '{}'", request.room_id))?;
            if room.definition.member_ids.len() < 2 {
                return Err(anyhow!(
                    "room '{}' is dormant; room delivery requires at least two members",
                    request.room_id
                ));
            }
            let recipient_ids = match request.recipients {
                RoomRecipientSelection::One { session_id } => {
                    if !room.definition.member_ids.contains(&session_id) {
                        return Err(anyhow!(
                            "session '{session_id}' is not a member of room '{}'",
                            request.room_id
                        ));
                    }
                    vec![session_id]
                }
                RoomRecipientSelection::All {} => room.definition.member_ids.clone(),
            };
            (
                room.definition.membership_revision,
                room.definition.label.clone(),
                recipient_ids,
            )
        };

        self.refresh_session_liveness();
        let mut delivery_plan = Vec::with_capacity(recipient_ids.len());
        for recipient_id in &recipient_ids {
            let (target, behavior, payload) = self
                .delivery_target_for_session_id(*recipient_id)
                .and_then(|(target, behavior)| {
                    if matches!(
                        self.inner
                            .slots
                            .lock()
                            .get_by_id(*recipient_id)
                            .map(|slot| slot.definition.driver),
                        Some(DriverKind::Prime)
                    ) {
                        return Err(anyhow!(
                            "Prime/WSL room delivery is not admitted; use its visible raw terminal input"
                        ));
                    }
                    validate_message_framing(&request.content, behavior)?;
                    let route = RouteMessageRequest {
                        from: "operator".into(),
                        to: room_label.clone(),
                        scope: MessageScope::Room,
                        content: request.content.clone(),
                    };
                    Ok((target, behavior, routed_message_payload(&route, behavior)))
                })
                .map_err(|error| {
                    anyhow!(
                        "room delivery preflight failed for recipient '{recipient_id}': {error}"
                    )
                })?;
            delivery_plan.push((
                *recipient_id,
                PlannedDelivery {
                    target,
                    submit_behavior: behavior,
                    payload,
                },
            ));
        }

        #[cfg(test)]
        if let Some(hook) = self.inner.room_delivery_after_preflight.lock().take() {
            hook();
        }

        let message_id = Uuid::new_v4();
        let (message_cursor, _delivery_lease) = {
            let _room_event_publish = self.inner.room_event_publish.lock();
            let (message_event, pending_events) = {
                let mut rooms = self.inner.rooms.lock();
                let room = rooms.get_mut(request.room_id).ok_or_else(|| {
                    anyhow!("room '{}' was deleted before delivery", request.room_id)
                })?;
                if room.definition.membership_revision != membership_revision {
                    return Err(anyhow!(
                        "room '{}' membership changed during delivery preflight; retry",
                        request.room_id
                    ));
                }
                if room.in_flight_deliveries == usize::MAX {
                    return Err(anyhow!("room delivery counter exhausted"));
                }
                room.ensure_sequence_capacity(1 + recipient_ids.len() * 2)?;
                let message_event = room.append(RoomFeedItem::Message {
                    message_id,
                    sender: RoomMessageSender::Operator {},
                    content: request.content,
                    recipient_ids: recipient_ids.clone(),
                    membership_revision,
                })?;
                let mut pending_events = Vec::with_capacity(delivery_plan.len());
                for (recipient_id, delivery) in &delivery_plan {
                    pending_events.push(room.append(RoomFeedItem::Delivery {
                        message_id,
                        recipient_id: *recipient_id,
                        status: RoomDeliveryStatus::Pending,
                        bytes_written: 0,
                        error: None,
                        run_id: Some(delivery.target.run_id),
                        generation: Some(delivery.target.generation),
                    })?);
                }
                room.in_flight_deliveries += 1;
                (message_event, pending_events)
            };
            let delivery_lease = RoomDeliveryLease {
                inner: self.inner.clone(),
                room_id: request.room_id,
            };
            let message_cursor = message_event.cursor;
            #[cfg(test)]
            self.run_room_event_after_append_hook_for_tests();
            self.emit(RuntimeEvent::RoomFeedEvent {
                feed_event: message_event,
            });
            for feed_event in pending_events {
                self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
            }
            (message_cursor, delivery_lease)
        };

        let mut failures = Vec::new();
        let mut written_count = 0usize;
        for (recipient_id, delivery) in delivery_plan {
            let (status, bytes_written, error) = match self.deliver_prepared_payload(
                &delivery.target,
                &delivery.payload,
                delivery.submit_behavior,
            ) {
                Ok(result) => {
                    written_count += 1;
                    (RoomDeliveryStatus::Written, result.bytes_written, None)
                }
                Err(error) => {
                    let bytes_written = error
                        .downcast_ref::<PtyWriteError>()
                        .map(PtyWriteError::bytes_written)
                        .unwrap_or(0);
                    let error = bounded_room_delivery_error(&error.to_string());
                    failures.push(RoomDeliveryFailure {
                        recipient_id,
                        bytes_written,
                        error: error.clone(),
                    });
                    (RoomDeliveryStatus::Failed, bytes_written, Some(error))
                }
            };
            {
                let _room_event_publish = self.inner.room_event_publish.lock();
                let feed_event = {
                    let mut rooms = self.inner.rooms.lock();
                    let room = rooms
                        .get_mut(request.room_id)
                        .expect("in-flight room cannot be deleted");
                    room.append(RoomFeedItem::Delivery {
                        message_id,
                        recipient_id,
                        status,
                        bytes_written,
                        error,
                        run_id: Some(delivery.target.run_id),
                        generation: Some(delivery.target.generation),
                    })
                    .expect("room delivery reserved feed sequence capacity")
                };
                #[cfg(test)]
                self.run_room_event_after_append_hook_for_tests();
                self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
            }
        }
        Ok(RoomDeliveryResult {
            room_id: request.room_id,
            message_id,
            cursor: message_cursor,
            recipient_count: recipient_ids.len(),
            written_count,
            failures,
        })
    }

    fn post_room_message_as(
        &self,
        room_id: RoomId,
        sender: RoomMessageSender,
        content: String,
    ) -> Result<RoomPostResult> {
        self.ensure_active()?;
        validate_message_body(&content)?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let message_id = Uuid::new_v4();
        let feed_event = {
            let mut rooms = self.inner.rooms.lock();
            let room = rooms
                .get_mut(room_id)
                .ok_or_else(|| anyhow!("unknown room id '{room_id}'"))?;
            let membership_revision = room.definition.membership_revision;
            room.append(RoomFeedItem::Message {
                message_id,
                sender,
                content,
                recipient_ids: Vec::new(),
                membership_revision,
            })?
        };
        let result = RoomPostResult {
            room_id,
            message_id,
            cursor: feed_event.cursor,
        };
        #[cfg(test)]
        self.run_room_event_after_append_hook_for_tests();
        self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
        Ok(result)
    }

    fn schedule_late_termination_reproof(
        &self,
        session_id: SessionId,
        generation: SessionGeneration,
        kind: StopIntentKind,
        pty: Arc<dyn PtySession>,
        receiver: mpsc::Receiver<TerminationAttempt>,
        context: &'static str,
    ) {
        let supervisor = self.clone();
        thread::spawn(move || {
            let late_result = receiver.recv();
            let detail = match late_result {
                Ok(attempt) if attempt.kill_error.is_none() && attempt.input_idle => format!(
                    "{context} termination returned after its deadline; an explicit reserved re-proof is required"
                ),
                Ok(attempt) => format!(
                    "{context} termination returned after its deadline without a usable proof: {}",
                    attempt
                        .kill_error
                        .or(attempt.exit_poll_error)
                        .unwrap_or_else(|| "input writers did not drain".into())
                ),
                Err(_) => format!(
                    "{context} termination worker ended without returning a proof; an explicit reserved re-proof is required"
                ),
            };
            let reconciled = {
                let mut slots = supervisor.inner.slots.lock();
                slots.get_by_id_mut(session_id).is_some_and(|slot| {
                    let owns_same_pty = slot
                        .running
                        .as_ref()
                        .and_then(|running| running.pty.as_ref())
                        .is_some_and(|current| Arc::ptr_eq(current, &pty));
                    if slot.generation != generation
                        || slot.lifecycle_operation != Some(LifecycleOperation { generation, kind })
                        || !owns_same_pty
                    {
                        return false;
                    }
                    slot.lifecycle_operation = None;
                    slot.state = LifecycleState::Failed;
                    slot.termination_uncertain = true;
                    slot.last_error = Some(detail.clone());
                    true
                })
            };
            if reconciled {
                supervisor.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!("{}: {detail}", session_alias(session_id)),
                    timestamp: now_rfc3339(),
                });
            }
        });
    }

    fn schedule_late_rejected_spawn_reproof(
        &self,
        session_id: SessionId,
        reservation: SpawnReservation,
        pty: Arc<dyn PtySession>,
        receiver: mpsc::Receiver<TerminationAttempt>,
    ) {
        let supervisor = self.clone();
        thread::spawn(move || {
            let late_result = receiver.recv();
            let detail = match late_result {
                Ok(attempt) if attempt.kill_error.is_none() && attempt.input_idle =>
                    "rejected-spawn termination returned after its deadline; an explicit reserved re-proof is required".to_string(),
                Ok(attempt) => format!(
                    "rejected-spawn termination returned after its deadline without a usable proof: {}",
                    attempt
                        .kill_error
                        .or(attempt.exit_poll_error)
                        .unwrap_or_else(|| "input writers did not drain".into())
                ),
                Err(_) => "rejected-spawn termination worker ended without returning a proof; an explicit reserved re-proof is required".into(),
            };
            let reconciled = {
                let mut slots = supervisor.inner.slots.lock();
                slots.get_by_id_mut(session_id).is_some_and(|slot| {
                    let owns_same_pty = slot
                        .running
                        .as_ref()
                        .and_then(|running| running.pty.as_ref())
                        .is_some_and(|current| Arc::ptr_eq(current, &pty));
                    if slot.spawn_in_flight != Some(reservation) || !owns_same_pty {
                        return false;
                    }
                    slot.spawn_in_flight = None;
                    if slot
                        .lifecycle_operation
                        .is_some_and(|operation| operation.generation == reservation.generation)
                    {
                        slot.lifecycle_operation = None;
                    }
                    slot.state = LifecycleState::Failed;
                    slot.termination_uncertain = true;
                    slot.last_error = Some(detail.clone());
                    true
                })
            };
            if reconciled {
                supervisor.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!("{}: {detail}", session_alias(session_id)),
                    timestamp: now_rfc3339(),
                });
            }
        });
    }

    fn stop_session_at(
        &self,
        session_id: SessionId,
        expected: SessionGeneration,
    ) -> Result<SessionSnapshot> {
        let (alias, pty, input_gate, stop_intent, process_id, spawn_in_flight, lifecycle_kind) = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id_mut(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            let alias = slot.definition.alias.clone();
            if slot.generation != expected {
                return Err(anyhow!(
                    "stop_session_at superseded: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
            let operation = slot.lifecycle_operation.ok_or_else(|| {
                anyhow!(
                    "stop_session_at has no declared lifecycle operation for session '{session_id}'"
                )
            })?;
            if operation.generation != expected {
                return Err(anyhow!(
                    "stop_session_at lifecycle declaration mismatch: expected gen {expected}, declared {}",
                    operation.generation
                ));
            }
            ensure_run_event_capacity(slot, 2)?;

            cancel_quiesce_timer_locked(slot);
            let running = slot.running.as_ref().cloned();
            if let Some(running) = &running {
                running.close_input();
            }
            let pty = running
                .as_ref()
                .and_then(|running| running.pty.as_ref().cloned());
            let input_gate = running.map(|running| running.input_gate);
            slot.run_id = None;
            let process_id = slot.process_id;
            let stop_intent = pty.as_ref().map(|_| StopIntent {
                generation: expected,
                kind: operation.kind,
            });
            slot.stop_intent = stop_intent;
            let spawn_in_flight = slot.spawn_in_flight.is_some();
            slot.termination_uncertain = pty.is_some() || spawn_in_flight;
            slot.state = if matches!(
                stop_intent,
                Some(StopIntent {
                    kind: StopIntentKind::Restart,
                    ..
                })
            ) {
                LifecycleState::Restarting
            } else if pty.is_some() || spawn_in_flight {
                LifecycleState::Failed
            } else {
                LifecycleState::Closed
            };
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            reset_work_state_locked(slot);
            (
                alias,
                pty,
                input_gate,
                stop_intent,
                process_id,
                spawn_in_flight,
                operation.kind,
            )
        };

        let mut exit_status = None;
        let mut exit_poll_error = None;
        let mut termination_proven = pty.is_none() && process_id.is_none() && !spawn_in_flight;
        let mut termination_attempt_timed_out = false;
        let mut late_termination = None;
        if let (Some(pty), Some(input_gate)) = (pty.as_ref(), input_gate) {
            let kill_timeout = *self.inner.stop_kill_timeout.lock();
            match terminate_running_session_bounded(pty.clone(), input_gate, kill_timeout) {
                BoundedTerminationAttempt::Completed(attempt) => {
                    termination_proven = attempt.kill_error.is_none() && attempt.input_idle;
                    exit_status = attempt.exit_status;
                    exit_poll_error = attempt.kill_error.or(attempt.exit_poll_error);
                    if !attempt.input_idle && exit_poll_error.is_none() {
                        exit_poll_error = Some(format!(
                            "PTY input writers did not drain within {}ms after process-scope termination",
                            kill_timeout.as_millis()
                        ));
                    }
                }
                BoundedTerminationAttempt::TimedOut(receiver) => {
                    termination_attempt_timed_out = true;
                    late_termination = Some(receiver);
                    exit_poll_error = Some(format!(
                        "pty.kill() did not return within {}ms; process-scope termination is unproved",
                        kill_timeout.as_millis()
                    ));
                    self.emit(RuntimeEvent::SystemLog {
                        level: LogLevel::Warn,
                        message: format!(
                            "pty.kill() for session '{alias}' (gen {expected}) did not return within {}ms; retained ownership and blocked lifecycle mutation",
                            kill_timeout.as_millis()
                        ),
                        timestamp: now_rfc3339(),
                    });
                }
            }
        }

        let exit_classification = stop_intent.map(|intent| {
            classify_session_exit(
                exit_status,
                exit_poll_error.clone(),
                Some(intent),
                SessionExitReason::ProcessDisappeared,
                "requested stop completed without exit status",
            )
        });

        let (snapshot, state_identity, exit_identity) = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id_mut(session_id)
                .ok_or_else(|| anyhow!("session disappeared during stop_session_at"))?;
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
            if termination_proven && let Some(expected_pty) = pty.as_ref() {
                let owns_expected_pty = slot
                    .running
                    .as_ref()
                    .and_then(|running| running.pty.as_ref())
                    .is_some_and(|current| Arc::ptr_eq(current, expected_pty));
                if !owns_expected_pty {
                    termination_proven = false;
                    exit_poll_error = Some(
                        "process-scope owner changed before termination proof could commit".into(),
                    );
                }
            }
            if !termination_attempt_timed_out
                && slot.lifecycle_operation.is_some_and(|operation| {
                    operation.generation == expected
                        && (operation.kind == StopIntentKind::Operator || !termination_proven)
                })
            {
                slot.lifecycle_operation = None;
            }
            slot.termination_uncertain = !termination_proven;
            if termination_proven {
                slot.running = None;
                slot.process_id = None;
                slot.state = LifecycleState::Closed;
            } else {
                slot.state = LifecycleState::Failed;
                slot.last_error = Some(match exit_poll_error.as_deref() {
                    Some(detail) => format!(
                        "process termination could not be proved; session definition and process-scope owner are retained: {detail}"
                    ),
                    None => "process termination could not be proved; session definition and process-scope owner are retained".into(),
                });
            }
            let state_identity = slot
                .last_run_id
                .map(|run_id| next_run_event_identity(slot, run_id));
            let exit_identity = if termination_proven && exit_classification.is_some() {
                slot.last_run_id
                    .map(|run_id| next_run_event_identity(slot, run_id))
            } else {
                None
            };
            (slot.snapshot(), state_identity, exit_identity)
        };

        let timestamp = now_rfc3339();
        if let Some(identity) = state_identity {
            self.emit_run_event(RuntimeEvent::SessionState {
                identity,
                session: snapshot.alias.clone(),
                state: snapshot.lifecycle_state,
                reason: if termination_proven {
                    "session stopped".into()
                } else {
                    "process termination could not be proved".into()
                },
                timestamp: timestamp.clone(),
            });
        }
        if let (Some(classification), Some(identity)) = (&exit_classification, exit_identity) {
            self.emit_run_event(session_exit_event(
                snapshot.alias.clone(),
                identity,
                process_id,
                classification,
                timestamp.clone(),
            ));
        }

        {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("session disappeared during stop_session_at"))?;
            if slot.generation != expected {
                return Err(anyhow!(
                    "stop_session_at superseded mid-op: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
        };

        if let (Some(receiver), Some(pty)) = (late_termination, pty) {
            self.schedule_late_termination_reproof(
                session_id,
                expected,
                lifecycle_kind,
                pty,
                receiver,
                "stop",
            );
        }

        Ok(snapshot)
    }

    fn start_session_at(
        &self,
        session_id: SessionId,
        expected: SessionGeneration,
        directory_lease: SessionDirectoryLease,
        plan: PreparedLaunch,
    ) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let (run_id, starting_event) = {
            let mut slots = self.inner.slots.lock();
            self.ensure_active()?;
            let slot = slots
                .get_by_id_mut(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.generation != expected {
                return Err(anyhow!(
                    "start_session_at superseded: expected gen {expected}, current {}",
                    slot.generation
                ));
            }
            if slot.termination_uncertain {
                return Err(anyhow!(
                    "session '{session_id}' has an unverified prior termination and cannot be started"
                ));
            }
            if slot.spawn_in_flight.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a spawn in progress"
                ));
            }
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }

            ensure_run_event_capacity(slot, 1)?;
            cancel_quiesce_timer_locked(slot);
            slot.state = LifecycleState::Starting;
            slot.last_error = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            reset_work_state_locked(slot);
            let run_id = Uuid::new_v4();
            slot.bracketed_paste.begin_run(RunBinding {
                session_id: slot.session_id,
                run_id,
                generation: slot.generation,
            });
            slot.run_id = Some(run_id);
            slot.last_run_id = Some(run_id);
            slot.spawn_in_flight = Some(SpawnReservation {
                generation: expected,
                run_id,
            });
            let snapshot = slot.snapshot();
            let identity = next_run_event_identity(slot, run_id);
            let starting_event = RuntimeEvent::SessionState {
                identity,
                session: snapshot.alias,
                state: LifecycleState::Starting,
                reason: "launch requested".into(),
                timestamp: now_rfc3339(),
            };
            (run_id, starting_event)
        };
        self.emit_run_event(starting_event);

        let handle = self.clone();
        let handler: PtyEventHandler = Arc::new(move |event| {
            handle.handle_pty_event_by_id(session_id, expected, run_id, event);
        });

        let spawn_result = self.inner.pty_spawner.read().clone().spawn(&plan, handler);
        drop(directory_lease);
        match spawn_result {
            Ok(pty) => {
                let pty: Arc<dyn PtySession> = Arc::from(pty);
                let installation = {
                    let mut slots = self.inner.slots.lock();
                    match slots.get_by_id_mut(session_id) {
                        None => Err(anyhow!(
                            "session '{session_id}' was removed while its PTY was spawning"
                        )),
                        Some(slot) => {
                            if self.inner.shutdown_started.load(Ordering::Acquire) {
                                Err(anyhow!("supervisor has shut down"))
                            } else if slot.generation != expected {
                                Err(anyhow!(
                                    "start_session_at superseded mid-spawn: expected gen {expected}, current {}",
                                    slot.generation
                                ))
                            } else if slot.run_id != Some(run_id) {
                                Err(anyhow!(
                                    "start_session_at superseded by another run identity"
                                ))
                            } else if slot.spawn_in_flight
                                != Some(SpawnReservation {
                                    generation: expected,
                                    run_id,
                                })
                            {
                                Err(anyhow!("start_session_at lost its spawn reservation"))
                            } else if let Err(error) = ensure_run_event_capacity(slot, 1) {
                                Err(error)
                            } else {
                                slot.spawn_in_flight = None;
                                slot.process_id = pty.process_id();
                                slot.running = Some(RunningSession::new(Some(pty.clone())));
                                slot.state = LifecycleState::Ready;
                                slot.last_activity_at = Some(now_rfc3339());
                                slot.last_real_output_at = None;
                                reset_work_state_locked(slot);
                                let identity = next_run_event_identity(slot, run_id);
                                Ok((slot.snapshot(), identity))
                            }
                        }
                    }
                };
                let (snapshot, identity) = match installation {
                    Ok(result) => result,
                    Err(error) => {
                        let process_id = pty.process_id();
                        let reservation = SpawnReservation {
                            generation: expected,
                            run_id,
                        };
                        let rejected_running = RunningSession::new(Some(pty.clone()));
                        rejected_running.close_input();
                        let input_gate = rejected_running.input_gate.clone();
                        {
                            let mut slots = self.inner.slots.lock();
                            if let Some(slot) = slots.get_by_id_mut(session_id) {
                                slot.running = Some(rejected_running);
                                slot.process_id = process_id;
                                slot.run_id = None;
                                slot.state = LifecycleState::Failed;
                                slot.termination_uncertain = true;
                                slot.last_error = Some(
                                    "rejected spawned process-scope termination is in progress"
                                        .into(),
                                );
                            }
                        }
                        let kill_timeout = *self.inner.stop_kill_timeout.lock();
                        let (mut cleanup_proven, cleanup_error, late_termination) =
                            match terminate_running_session_bounded(
                                pty.clone(),
                                input_gate,
                                kill_timeout,
                            ) {
                                BoundedTerminationAttempt::Completed(attempt) => {
                                    let proved = attempt.kill_error.is_none() && attempt.input_idle;
                                    let error = attempt
                                        .kill_error
                                        .or(attempt.exit_poll_error)
                                        .or_else(|| {
                                            (!attempt.input_idle).then(|| {
                                                format!(
                                                    "PTY input writers did not drain within {}ms",
                                                    kill_timeout.as_millis()
                                                )
                                            })
                                        });
                                    (proved, error, None)
                                }
                                BoundedTerminationAttempt::TimedOut(receiver) => (
                                    false,
                                    Some(format!(
                                        "process-scope termination did not finish within {}ms",
                                        kill_timeout.as_millis()
                                    )),
                                    Some(receiver),
                                ),
                            };
                        let terminal_event = {
                            let mut slots = self.inner.slots.lock();
                            slots.get_by_id_mut(session_id).and_then(|slot| {
                                if late_termination.is_none()
                                    && slot.spawn_in_flight == Some(reservation)
                                {
                                    slot.spawn_in_flight = None;
                                }
                                if late_termination.is_none()
                                    && slot.lifecycle_operation.is_some_and(|operation| {
                                        operation.generation == expected
                                    })
                                {
                                    slot.lifecycle_operation = None;
                                }
                                slot.stop_intent = None;
                                slot.run_id = None;
                                slot.last_activity_at = Some(now_rfc3339());
                                slot.last_real_output_at = None;
                                reset_work_state_locked(slot);
                                if cleanup_proven {
                                    let owns_same_pty = slot
                                        .running
                                        .as_ref()
                                        .and_then(|running| running.pty.as_ref())
                                        .is_some_and(|current| Arc::ptr_eq(current, &pty));
                                    if !owns_same_pty {
                                        cleanup_proven = false;
                                        slot.state = LifecycleState::Failed;
                                        slot.termination_uncertain = true;
                                        slot.last_error = Some(
                                            "rejected spawned process-scope owner changed during termination proof"
                                                .into(),
                                        );
                                    } else {
                                        slot.running = None;
                                        slot.process_id = None;
                                        slot.state = LifecycleState::Closed;
                                        slot.termination_uncertain = false;
                                        slot.last_error = None;
                                    }
                                } else {
                                    slot.state = LifecycleState::Failed;
                                    slot.termination_uncertain = true;
                                    slot.last_error = Some(format!(
                                        "rejected spawned process scope could not be terminated; ownership retained: {}",
                                        cleanup_error.as_deref().unwrap_or("termination proof unavailable")
                                    ));
                                }
                                let identity = slot.last_run_id.and_then(|event_run_id| {
                                    ensure_run_event_capacity(slot, 1)
                                        .ok()
                                        .map(|_| next_run_event_identity(slot, event_run_id))
                                });
                                identity.map(|identity| RuntimeEvent::SessionState {
                                    identity,
                                    session: slot.definition.alias.clone(),
                                    state: slot.state,
                                    reason: if cleanup_proven {
                                        "rejected spawned process scope terminated".into()
                                    } else {
                                        "rejected spawned process-scope termination failed".into()
                                    },
                                    timestamp: now_rfc3339(),
                                })
                            })
                        };
                        if let Some(event) = terminal_event {
                            self.emit_run_event(event);
                        }
                        if let Some(receiver) = late_termination {
                            self.schedule_late_rejected_spawn_reproof(
                                session_id,
                                reservation,
                                pty,
                                receiver,
                            );
                        }
                        return if cleanup_proven {
                            Err(error)
                        } else {
                            Err(anyhow!(
                                "{error}; failed to prove rejected spawned PTY termination: {}",
                                cleanup_error
                                    .as_deref()
                                    .unwrap_or("unknown cleanup failure")
                            ))
                        };
                    }
                };
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: format!("Started {} session", snapshot.label),
                    timestamp: now_rfc3339(),
                });
                self.emit_run_event(RuntimeEvent::SessionState {
                    identity,
                    session: snapshot.alias.clone(),
                    state: snapshot.lifecycle_state,
                    reason: "session ready".into(),
                    timestamp: now_rfc3339(),
                });
                Ok(snapshot)
            }
            Err(error) => {
                let terminal = {
                    let mut slots = self.inner.slots.lock();
                    let Some(slot) = slots.get_by_id_mut(session_id) else {
                        return Err(error);
                    };
                    if slot.spawn_in_flight
                        == Some(SpawnReservation {
                            generation: expected,
                            run_id,
                        })
                    {
                        slot.spawn_in_flight = None;
                    }
                    slot.running = None;
                    slot.run_id = None;
                    slot.process_id = None;
                    slot.termination_uncertain = false;
                    let superseded = self.inner.shutdown_started.load(Ordering::Acquire)
                        || slot.session_id != session_id
                        || slot.generation != expected;
                    slot.state = if superseded {
                        LifecycleState::Closed
                    } else {
                        LifecycleState::Failed
                    };
                    slot.last_error = (!superseded).then(|| format!("{error:#}"));
                    slot.last_real_output_at = None;
                    reset_work_state_locked(slot);
                    let identity = ensure_run_event_capacity(slot, 1)
                        .ok()
                        .map(|_| next_run_event_identity(slot, run_id));
                    if slot
                        .lifecycle_operation
                        .is_some_and(|operation| operation.generation == expected)
                    {
                        slot.lifecycle_operation = None;
                    }
                    (slot.snapshot(), identity, superseded)
                };
                let (snapshot, identity, superseded) = terminal;
                self.emit(RuntimeEvent::SystemLog {
                    level: if superseded {
                        LogLevel::Info
                    } else {
                        LogLevel::Error
                    },
                    message: if superseded {
                        format!(
                            "Discarded failed spawn for superseded {} session: {error:#}",
                            snapshot.label
                        )
                    } else {
                        format!("Failed to start {}: {error:#}", snapshot.label)
                    },
                    timestamp: now_rfc3339(),
                });
                if let Some(identity) = identity {
                    self.emit_run_event(RuntimeEvent::SessionState {
                        identity,
                        session: snapshot.alias.clone(),
                        state: snapshot.lifecycle_state,
                        reason: if superseded {
                            "superseded spawn failed before creating a process scope".into()
                        } else {
                            "spawn failed".into()
                        },
                        timestamp: now_rfc3339(),
                    });
                }
                Err(error)
            }
        }
    }

    fn restart_session_at(
        &self,
        session_id: SessionId,
        expected_generation: SessionGeneration,
        expected_run_id: Option<Uuid>,
    ) -> Result<SessionSnapshot> {
        let (definition, qualified_directory) = {
            let slots = self.inner.slots.lock();
            self.ensure_active()?;
            let slot = slots
                .get_by_id(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.generation != expected_generation || slot.run_id != expected_run_id {
                return Err(anyhow!(
                    "restart_session_at superseded before launch preparation"
                ));
            }
            if slot.lifecycle_operation.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a lifecycle operation in progress"
                ));
            }
            if slot.termination_uncertain {
                return Err(anyhow!(
                    "session '{session_id}' has an unverified prior termination and cannot be restarted"
                ));
            }
            if slot.spawn_in_flight.is_some() {
                return Err(anyhow!(
                    "session '{session_id}' already has a spawn in progress"
                ));
            }
            let qualified_directory = slot
                .qualified_working_directory
                .as_ref()
                .ok_or_else(|| {
                    anyhow!("session '{session_id}' has no qualified working directory")
                })?
                .clone();
            (slot.definition.clone(), qualified_directory)
        };
        let directory_lease = self.revalidate_session_working_directory(
            session_id,
            definition.driver,
            &qualified_directory,
        )?;
        let plan = self.prepare_launch_spec_for_spawn(&definition, &qualified_directory)?;
        let (_, expected_stop) = self.declare_stop_operation(
            session_id,
            Some((expected_generation, expected_run_id)),
            StopIntentKind::Restart,
        )?;
        let restarting_event = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id_mut(session_id)
                .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
            if slot.generation != expected_stop {
                return Err(anyhow!(
                    "restart_session_at superseded: expected gen {expected_stop}, current {}",
                    slot.generation
                ));
            }
            if slot.lifecycle_operation
                != Some(LifecycleOperation {
                    generation: expected_stop,
                    kind: StopIntentKind::Restart,
                })
            {
                return Err(anyhow!(
                    "restart_session_at lifecycle declaration changed before stop"
                ));
            }
            slot.last_activity_at = Some(now_rfc3339());
            reset_work_state_locked(slot);
            let snapshot = slot.snapshot();
            slot.last_run_id.map(|run_id| RuntimeEvent::SessionState {
                identity: next_run_event_identity(slot, run_id),
                session: snapshot.alias,
                state: LifecycleState::Restarting,
                reason: "restart requested".into(),
                timestamp: now_rfc3339(),
            })
        };
        if let Some(event) = restarting_event {
            self.emit_run_event(event);
        }

        let stopped = self.stop_session_at(session_id, expected_stop)?;
        if stopped.lifecycle_state != LifecycleState::Closed {
            return Err(anyhow!(
                "session '{session_id}' could not be restarted because prior process-scope termination was not proved"
            ));
        }
        let expected_start_result = {
            let mut slots = self.inner.slots.lock();
            let admission = (|| -> Result<SessionGeneration> {
                self.ensure_active()?;
                let slot = slots
                    .get_by_id_mut(session_id)
                    .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
                if slot.lifecycle_operation
                    != Some(LifecycleOperation {
                        generation: expected_stop,
                        kind: StopIntentKind::Restart,
                    })
                {
                    return Err(anyhow!(
                        "restart_session_at lifecycle declaration changed after stop"
                    ));
                }
                if slot.definition.driver != definition.driver
                    || slot.definition.working_dir != definition.working_dir
                    || slot.definition.permission_profile != definition.permission_profile
                    || slot.qualified_working_directory.as_ref() != Some(&qualified_directory)
                {
                    return Err(anyhow!(
                        "session '{session_id}' definition changed during restart; retry"
                    ));
                }
                ensure_run_event_capacity(slot, 2)?;
                slot.generation = slot
                    .generation
                    .checked_add(1)
                    .ok_or_else(|| anyhow!("session generation exhausted for '{session_id}'"))?;
                slot.state = LifecycleState::Starting;
                slot.lifecycle_operation = Some(LifecycleOperation {
                    generation: slot.generation,
                    kind: StopIntentKind::Restart,
                });
                Ok(slot.generation)
            })();
            if let Err(error) = &admission
                && let Some(slot) = slots.get_by_id_mut(session_id)
                && slot.lifecycle_operation
                    == Some(LifecycleOperation {
                        generation: expected_stop,
                        kind: StopIntentKind::Restart,
                    })
            {
                slot.lifecycle_operation = None;
                if slot.running.is_none() && slot.spawn_in_flight.is_none() {
                    slot.state = LifecycleState::Closed;
                    slot.termination_uncertain = false;
                    slot.run_id = None;
                    slot.process_id = None;
                    slot.stop_intent = None;
                    slot.last_error = Some(format!("restart did not begin: {error:#}"));
                }
            }
            admission
        };
        let expected_start = expected_start_result?;
        let result = self.start_session_at(session_id, expected_start, directory_lease, plan);
        let shutdown_crossed_restart = self.inner.shutdown_started.load(Ordering::Acquire);
        let mut slots = self.inner.slots.lock();
        if let Some(slot) = slots.get_by_id_mut(session_id)
            && slot.lifecycle_operation
                == Some(LifecycleOperation {
                    generation: expected_start,
                    kind: StopIntentKind::Restart,
                })
        {
            slot.lifecycle_operation = None;
            if shutdown_crossed_restart {
                if slot.running.is_none() && slot.spawn_in_flight.is_none() {
                    slot.state = LifecycleState::Closed;
                    slot.termination_uncertain = false;
                    slot.run_id = None;
                    slot.process_id = None;
                    slot.stop_intent = None;
                } else {
                    slot.state = LifecycleState::Failed;
                    slot.termination_uncertain = true;
                    slot.last_error = Some(
                        "supervisor shutdown crossed restart; retained process-scope ownership"
                            .into(),
                    );
                }
            }
        }
        drop(slots);
        if shutdown_crossed_restart {
            result
                .map(|_| ())
                .context("supervisor shut down during restart")?;
            return Err(anyhow!("supervisor has shut down"));
        }
        result
    }

    fn validate_run_input_target_locked(
        target: &RunWriteTarget,
        caller: Option<&PaneCaller>,
        safety: RunInputSafety,
        slots: &SessionRegistry,
    ) -> Result<()> {
        if let Some(caller) = caller {
            Self::validate_pane_target_locked(caller, &target.session, slots)?;
        }
        let slot = slots
            .get_by_id(target.session_id)
            .with_context(|| format!("unknown session '{}'", target.session))?;
        if slot.definition.alias != target.session
            || slot.generation != target.generation
            || slot.run_id != Some(target.run_id)
        {
            return Err(anyhow!(
                "session '{}' run identity changed before input",
                target.session
            ));
        }
        ensure_run_input_safety_locked(slot, target, safety)?;
        let running = slot
            .running
            .as_ref()
            .ok_or_else(|| anyhow!("session '{}' is not running", target.session))?;
        let current_pty = running
            .pty
            .as_ref()
            .ok_or_else(|| anyhow!("session '{}' transport is not available", target.session))?;
        if !target.input_gate.is_accepting()
            || !Arc::ptr_eq(&running.input_gate, &target.input_gate)
            || !Arc::ptr_eq(current_pty, &target.pty)
        {
            return Err(anyhow!(
                "session '{}' run changed before input",
                target.session
            ));
        }
        Ok(())
    }

    fn begin_run_input_write<'a>(
        &self,
        target: &'a RunWriteTarget,
        caller: Option<&PaneCaller>,
        safety: RunInputSafety,
        control: Option<&'a InputWriteControl>,
    ) -> Result<RunInputPermit<'a>> {
        #[cfg(test)]
        if let Some(hook) = self.inner.run_input_before_commit.lock().take() {
            hook();
        }
        let writer = target.input_gate.begin_write(control)?;
        let slots = self.inner.slots.lock();
        Self::validate_run_input_target_locked(target, caller, safety, &slots)?;
        drop(slots);
        if let Some(control) = control {
            control.begin_pty_write()?;
        }
        Ok(writer)
    }

    fn write_full_pty_input(pty: &dyn PtySession, input: &str) -> Result<usize> {
        let bytes_written = pty.send_input(input)?;
        if bytes_written != input.len() {
            return Err(PtyWriteError::new(
                bytes_written,
                format!(
                    "PTY reported a successful short write: accepted {bytes_written} of {} bytes",
                    input.len()
                ),
            )
            .into());
        }
        Ok(bytes_written)
    }

    fn delivery_error_after_progress(error: anyhow::Error, prior_bytes: usize) -> anyhow::Error {
        let current_bytes = error
            .downcast_ref::<PtyWriteError>()
            .map(PtyWriteError::bytes_written)
            .unwrap_or(0);
        PtyWriteError::new(prior_bytes.saturating_add(current_bytes), error.to_string()).into()
    }

    fn write_run_input(
        &self,
        target: &RunWriteTarget,
        caller: Option<&PaneCaller>,
        input: &str,
        safety: RunInputSafety,
        control: Option<&InputWriteControl>,
    ) -> Result<usize> {
        let _writer = self.begin_run_input_write(target, caller, safety, control)?;
        let write_result = Self::write_full_pty_input(target.pty.as_ref(), input);
        if let Some(control) = control {
            control.finish();
        }
        write_result
    }

    fn write_bracketed_submission(
        &self,
        target: &RunWriteTarget,
        content: &str,
        submit_behavior: SubmitBehavior,
    ) -> Result<usize> {
        let safety = RunInputSafety::BracketedPasteEnabled;
        let _writer = self.begin_run_input_write(target, None, safety, None)?;
        let framed_content = frame_message_payload(content, MessageFraming::BracketedPaste);
        let content_bytes = Self::write_full_pty_input(target.pty.as_ref(), &framed_content)?;
        thread::sleep(submit_behavior.submit_delay);

        let slots = self.inner.slots.lock();
        if let Err(error) = Self::validate_run_input_target_locked(
            target,
            None,
            RunInputSafety::RoutedSubmit,
            &slots,
        ) {
            return Err(Self::delivery_error_after_progress(error, content_bytes));
        }
        drop(slots);

        Self::write_full_pty_input(target.pty.as_ref(), submit_behavior.sequence)
            .map(|submit_bytes| content_bytes + submit_bytes)
            .map_err(|error| Self::delivery_error_after_progress(error, content_bytes))
    }

    fn send_input_with_bytes(&self, request: SendInputRequest) -> Result<(SessionSnapshot, usize)> {
        self.refresh_session_liveness();
        let (snapshot, target) = {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(request.session_id)
                .ok_or_else(|| anyhow!("unknown session id '{}'", request.session_id))?;
            (
                slot.snapshot(),
                run_write_target_from_slot(&slot.definition.alias, slot)?,
            )
        };
        let bytes_written =
            self.write_run_input(&target, None, &request.input, RunInputSafety::Raw, None)?;

        Ok((snapshot, bytes_written))
    }

    pub fn send_input(&self, request: SendInputRequest) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let request_id = Uuid::new_v4().to_string();
        self.emit_dispatch_attempt_for_session_id(
            &request_id,
            "send_input",
            "operator",
            request.session_id,
        )?;
        self.send_input_with_bytes(request)
            .map(|(snapshot, _bytes_written)| snapshot)
    }

    pub fn send_control_key_by_id(
        &self,
        session_id: SessionId,
        key: ControlKey,
    ) -> Result<SessionSnapshot> {
        self.ensure_active()?;
        let request_id = Uuid::new_v4().to_string();
        self.emit_dispatch_attempt_for_session_id(&request_id, "send_key", "operator", session_id)?;
        let (snapshot, _bytes_written) = self.send_input_with_bytes(SendInputRequest {
            session_id,
            input: control_key_sequence(key).into(),
        })?;
        Ok(snapshot)
    }

    pub fn route_operator_message(
        &self,
        request: OperatorRouteMessageRequest,
    ) -> Result<RuntimeSnapshot> {
        self.ensure_active()?;
        validate_message_body(&request.content)?;
        {
            let slots = self.inner.slots.lock();
            let slot = slots
                .get_by_id(request.recipient_id)
                .ok_or_else(|| anyhow!("unknown session id '{}'", request.recipient_id))?;
            if slot.definition.driver == DriverKind::Prime {
                return Err(anyhow!(
                    "Prime/WSL routed delivery is not admitted in this release; use the visible raw terminal input"
                ));
            }
        }
        self.refresh_session_liveness();
        let recipient = {
            let slots = self.inner.slots.lock();
            slots
                .get_by_id(request.recipient_id)
                .ok_or_else(|| anyhow!("unknown session id '{}'", request.recipient_id))?
                .definition
                .alias
                .clone()
        };
        let route = RouteMessageRequest {
            from: "operator".into(),
            to: recipient.clone(),
            scope: MessageScope::Direct,
            content: request.content,
        };
        let (target, behavior) = self
            .delivery_target_for_session_id(request.recipient_id)
            .and_then(|(target, behavior)| {
                validate_message_framing(&route.content, behavior)?;
                Ok((target, behavior))
            })
            .map_err(|error| {
                anyhow!("route preflight failed for recipient '{recipient}': {error}")
            })?;
        let payload = routed_message_payload(&route, behavior);
        let delivery_plan = vec![PlannedDelivery {
            target,
            submit_behavior: behavior,
            payload,
        }];

        self.execute_route(route, delivery_plan)
    }

    fn execute_route(
        &self,
        request: RouteMessageRequest,
        delivery_plan: Vec<PlannedDelivery>,
    ) -> Result<RuntimeSnapshot> {
        let route_id = Uuid::new_v4();
        let route_id_string = route_id.to_string();
        let request_id = route_id_string.clone();
        let recipient_count = delivery_plan.len() as u32;

        for delivery in &delivery_plan {
            self.emit_dispatch_attempt(
                &request_id,
                "route_message",
                &request.from,
                &delivery.target.session,
            )?;
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

        self.emit(RuntimeEvent::RoutedMessage {
            id: route_id,
            from: request.from.clone(),
            to: request.to.clone(),
            scope: request.scope,
            content: request.content.clone(),
            timestamp: now_rfc3339(),
        });

        let mut failures = Vec::new();
        for (recipient_index, delivery) in delivery_plan.into_iter().enumerate() {
            let recipient = delivery.target.session.clone();
            let payload_part_count = 1;
            match self.deliver_prepared_payload(
                &delivery.target,
                &delivery.payload,
                delivery.submit_behavior,
            ) {
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
                }
                Err(error) => {
                    let bytes_written = error
                        .downcast_ref::<PtyWriteError>()
                        .map(PtyWriteError::bytes_written)
                        .unwrap_or(0);
                    let error = error.to_string();
                    let failure_summary = if bytes_written > 0 {
                        format!(
                            "{error} after {bytes_written} bytes were accepted by the PTY; content may be partial"
                        )
                    } else {
                        error.clone()
                    };
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
                        bytes_written,
                        error: Some(error.clone()),
                    });
                    failures.push((recipient, failure_summary));
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

    pub fn resize_session_by_id(&self, session_id: SessionId, cols: u16, rows: u16) -> Result<()> {
        self.ensure_active()?;
        self.refresh_session_liveness();
        let slots = self.inner.slots.lock();
        let slot = slots
            .get_by_id(session_id)
            .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
        let running = slot
            .running
            .as_ref()
            .ok_or_else(|| anyhow!("session '{session_id}' is not running"))?;
        running
            .pty
            .as_ref()
            .ok_or_else(|| anyhow!("session '{session_id}' transport is not available"))?
            .resize(cols, rows)?;
        Ok(())
    }

    pub fn start_control_plane(&self) -> Result<ControlPlaneStatus> {
        self.ensure_active()?;
        self.start_control_plane_at(None)
    }

    fn start_control_plane_at(
        &self,
        endpoint_override: Option<String>,
    ) -> Result<ControlPlaneStatus> {
        let _shutdown = self.inner.shutdown_lifecycle.lock();
        let _lifecycle = self.inner.control_plane_lifecycle.lock();
        self.ensure_active()?;
        if let Some(existing) = self.inner.control_plane.read().clone() {
            return Ok(existing);
        }

        #[cfg(not(windows))]
        {
            let _ = endpoint_override;
            Err(anyhow!(
                "external pane control is unavailable on this platform; use the desktop UI"
            ))
        }

        #[cfg(windows)]
        {
            let endpoint = endpoint_override.unwrap_or_else(control_plane_endpoint);
            let status = ControlPlaneStatus {
                transport: control_plane_transport().into(),
                endpoint,
            };

            let prepared = prepare_control_plane_thread(self.clone(), status.clone())?;
            #[cfg(test)]
            if let Some(hook) = self.inner.control_plane_after_prepare.lock().take() {
                hook();
            }
            self.ensure_active()?;
            let listener = prepared.activate()?;
            *self.inner.control_plane_listener.lock() = Some(listener);
            *self.inner.control_plane.write() = Some(status.clone());
            self.emit(RuntimeEvent::ControlPlaneReady {
                endpoint: status.endpoint.clone(),
                transport: status.transport.clone(),
                timestamp: now_rfc3339(),
            });
            Ok(status)
        }
    }

    fn delivery_target_for_session_id(
        &self,
        session_id: SessionId,
    ) -> Result<(RunWriteTarget, SubmitBehavior)> {
        let slots = self.inner.slots.lock();
        let slot = slots
            .get_by_id(session_id)
            .ok_or_else(|| anyhow!("unknown session id '{session_id}'"))?;
        let behavior = routed_message_submit_behavior(slot.definition.driver);
        let target = run_write_target_from_slot(&slot.definition.alias, slot)?;
        ensure_run_input_safety_locked(slot, &target, RunInputSafety::from(behavior))?;
        Ok((target, behavior))
    }

    fn retire_session_run(
        &self,
        session_id: SessionId,
        event_generation: SessionGeneration,
        event_run_id: Uuid,
        cause: RunRetirementCause,
    ) {
        let (session_alias, process_id, pty, input_gate, stop_intent) = {
            let mut slots = self.inner.slots.lock();
            let Some(slot) = slots.get_by_id_mut(session_id).filter(|slot| {
                slot.generation == event_generation
                    && slot.run_id == Some(event_run_id)
                    && slot.lifecycle_operation.is_none()
                    && slot.spawn_in_flight.is_none()
                    && !slot.termination_uncertain
            }) else {
                return;
            };
            if ensure_run_event_capacity(slot, 2).is_err() {
                return;
            }
            let Some(running) = slot.running.as_ref().cloned() else {
                return;
            };
            let Some(pty) = running.pty.as_ref().cloned() else {
                return;
            };
            running.close_input();
            slot.run_id = None;
            slot.lifecycle_operation = Some(LifecycleOperation {
                generation: event_generation,
                kind: StopIntentKind::Operator,
            });
            slot.state = LifecycleState::Failed;
            slot.termination_uncertain = true;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            slot.last_error = Some(format!(
                "{}; process-scope termination proof is in progress",
                cause.detail()
            ));
            cancel_quiesce_timer_locked(slot);
            reset_work_state_locked(slot);
            (
                slot.definition.alias.clone(),
                slot.process_id,
                pty,
                running.input_gate,
                slot.stop_intent.take(),
            )
        };

        if let RunRetirementCause::PtyError(error) = &cause {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Error,
                message: format!("{session_id} PTY error: {error}"),
                timestamp: now_rfc3339(),
            });
        }

        let kill_timeout = *self.inner.stop_kill_timeout.lock();
        let (
            mut termination_proven,
            exit_status,
            exit_poll_error,
            failure_detail,
            timed_out,
            late_termination,
        ) = match terminate_running_session_bounded(pty.clone(), input_gate, kill_timeout) {
            BoundedTerminationAttempt::Completed(attempt) => {
                let termination_proven = attempt.kill_error.is_none() && attempt.input_idle;
                let failure_detail = if let Some(error) = &attempt.kill_error {
                    Some(format!("process-scope termination failed: {error}"))
                } else if !attempt.input_idle {
                    Some(format!(
                        "PTY input writers did not drain within {}ms after process-scope termination",
                        kill_timeout.as_millis()
                    ))
                } else {
                    None
                };
                (
                    termination_proven,
                    attempt.exit_status,
                    attempt.exit_poll_error,
                    failure_detail,
                    false,
                    None,
                )
            }
            BoundedTerminationAttempt::TimedOut(receiver) => (
                false,
                None,
                None,
                Some(format!(
                    "process-scope termination did not finish within {}ms",
                    kill_timeout.as_millis()
                )),
                true,
                Some(receiver),
            ),
        };

        let classification = if termination_proven {
            Some(match &cause {
                RunRetirementCause::OutputClosed => classify_session_exit(
                    exit_status,
                    exit_poll_error,
                    stop_intent,
                    SessionExitReason::ProcessDisappeared,
                    "session output closed before exit status was available",
                ),
                RunRetirementCause::PtyError(error) => classify_pty_error_exit(error, stop_intent),
                RunRetirementCause::Liveness {
                    fallback_reason,
                    detail,
                    ..
                } => classify_session_exit(
                    exit_status,
                    exit_poll_error,
                    stop_intent,
                    *fallback_reason,
                    detail,
                ),
            })
        } else {
            None
        };

        let terminal_events = {
            let mut slots = self.inner.slots.lock();
            let Some(slot) = slots.get_by_id_mut(session_id).filter(|slot| {
                slot.generation == event_generation
                    && slot.run_id.is_none()
                    && slot.lifecycle_operation
                        == Some(LifecycleOperation {
                            generation: event_generation,
                            kind: StopIntentKind::Operator,
                        })
            }) else {
                return;
            };
            let owns_same_pty = slot
                .running
                .as_ref()
                .and_then(|running| running.pty.as_ref())
                .is_some_and(|current| Arc::ptr_eq(current, &pty));
            termination_proven &= owns_same_pty;
            if !timed_out {
                slot.lifecycle_operation = None;
            }
            let timestamp = now_rfc3339();
            slot.last_activity_at = Some(timestamp.clone());
            if termination_proven {
                slot.running = None;
                slot.process_id = None;
                slot.termination_uncertain = false;
                slot.state = cause.final_state();
                slot.last_error = classification
                    .as_ref()
                    .and_then(|classification| classification.last_error.clone());
            } else {
                slot.state = LifecycleState::Failed;
                slot.termination_uncertain = true;
                slot.last_error = Some(format!(
                    "{}; process termination could not be proved and the process-scope owner is retained: {}",
                    cause.detail(),
                    failure_detail
                        .as_deref()
                        .unwrap_or("process-scope ownership changed during cleanup")
                ));
            }
            let exit_event = if termination_proven {
                classification.as_ref().map(|classification| {
                    session_exit_event(
                        session_alias.clone(),
                        next_run_event_identity(slot, event_run_id),
                        process_id,
                        classification,
                        timestamp.clone(),
                    )
                })
            } else {
                None
            };
            let state_identity = next_run_event_identity(slot, event_run_id);
            let state_event = RuntimeEvent::SessionState {
                identity: state_identity,
                session: session_alias.clone(),
                state: slot.state,
                reason: if termination_proven {
                    cause.detail().into()
                } else {
                    "process termination could not be proved".into()
                },
                timestamp,
            };
            (exit_event, state_event)
        };

        if !termination_proven {
            self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Error,
                message: format!(
                    "{session_alias}: {}; retained the process-scope owner",
                    failure_detail
                        .as_deref()
                        .unwrap_or("termination proof failed")
                ),
                timestamp: now_rfc3339(),
            });
        }
        if let Some(exit_event) = terminal_events.0 {
            self.emit_run_event(exit_event);
        }
        self.emit_run_event(terminal_events.1);
        if let Some(receiver) = late_termination {
            self.schedule_late_termination_reproof(
                session_id,
                event_generation,
                StopIntentKind::Operator,
                pty,
                receiver,
                "terminal retirement",
            );
        }
    }

    fn refresh_session_liveness(&self) {
        let _shutdown = self.inner.shutdown_lifecycle.lock();
        if self.inner.shutdown_started.load(Ordering::Acquire) {
            return;
        }
        let mut retirements = Vec::new();
        let mut log_events = Vec::new();
        {
            let slots = self.inner.slots.lock();
            for slot in slots.by_id.values() {
                let Some(run_id) = slot.run_id else {
                    continue;
                };
                if slot.running.is_none()
                    || slot.lifecycle_operation.is_some()
                    || slot.spawn_in_flight.is_some()
                    || slot.termination_uncertain
                {
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
                log_events.push(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!(
                        "{} {} (wrapper pid {:?}); proving process-scope retirement",
                        slot.title(),
                        fallback_error,
                        slot.process_id
                    ),
                    timestamp: now_rfc3339(),
                });
                retirements.push((
                    slot.session_id,
                    slot.generation,
                    run_id,
                    RunRetirementCause::Liveness {
                        final_state: closed_state,
                        fallback_reason,
                        detail: fallback_error,
                    },
                ));
            }
        }

        for event in log_events {
            self.emit(event);
        }
        for (session_id, generation, run_id, cause) in retirements {
            self.retire_session_run(session_id, generation, run_id, cause);
        }
    }

    #[cfg(test)]
    fn handle_pty_event(
        &self,
        alias_or_label: &str,
        event_generation: SessionGeneration,
        event_run_id: Uuid,
        event: PtyEvent,
    ) {
        let Some(session_id) = self
            .inner
            .slots
            .lock()
            .get(alias_or_label)
            .map(|slot| slot.session_id)
        else {
            return;
        };
        self.handle_pty_event_by_id(session_id, event_generation, event_run_id, event);
    }

    fn handle_pty_event_by_id(
        &self,
        session_id: SessionId,
        event_generation: SessionGeneration,
        event_run_id: Uuid,
        event: PtyEvent,
    ) {
        let _shutdown = self.inner.shutdown_lifecycle.lock();
        if self.inner.shutdown_started.load(Ordering::Acquire) {
            return;
        }
        let should_log_stale_event = {
            let slots = self.inner.slots.lock();
            let current = slots
                .get_by_id(session_id)
                .map(|slot| (slot.generation, slot.run_id));
            current != Some((event_generation, Some(event_run_id)))
        };

        if should_log_stale_event {
            let counter = {
                let mut counts = self.inner.stale_event_drop_counts.lock();
                let key = (session_id, event_generation, event_run_id);
                let count = counts.entry(key).or_insert(0);
                *count += 1;
                *count
            };

            if counter == 1 || counter % 10 == 0 {
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: format!(
                        "Dropped stale PTY event for session '{session_id}' generation {event_generation} run {event_run_id} (count={counter})"
                    ),
                    timestamp: now_rfc3339(),
                });
            }
            return;
        }

        #[cfg(test)]
        if let Some(hook) = self.inner.pty_event_before_commit.lock().take() {
            hook();
        }

        match event {
            PtyEvent::Output(chunk) => {
                let has_real_content = chunk_has_real_content(&chunk);
                let observed_at = Instant::now();
                let real_output_at = has_real_content.then_some(observed_at);
                let outcome = {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_by_id_mut(session_id).filter(|slot| {
                        slot.generation == event_generation && slot.run_id == Some(event_run_id)
                    }) {
                        let session_alias = slot.definition.alias.clone();
                        let binding = RunBinding {
                            session_id: slot.session_id,
                            run_id: event_run_id,
                            generation: event_generation,
                        };
                        slot.bracketed_paste.observe_output(binding, &chunk);
                        let spawn_is_unconfirmed = slot.spawn_in_flight
                            == Some(SpawnReservation {
                                generation: event_generation,
                                run_id: event_run_id,
                            });
                        let required_events = if spawn_is_unconfirmed { 1 } else { 3 };
                        if ensure_run_event_capacity(slot, required_events).is_err() {
                            return;
                        }
                        let transitioned =
                            !spawn_is_unconfirmed && slot.state != LifecycleState::Ready;
                        if transitioned {
                            slot.state = LifecycleState::Ready;
                            slot.last_activity_at = Some(now_rfc3339());
                        }
                        if !spawn_is_unconfirmed
                            && has_real_content
                            && let Some(pty) = slot
                                .running
                                .as_ref()
                                .and_then(|running| running.pty.as_ref().map(|pty| pty.as_ref()))
                        {
                            pty.note_real_output(slot.definition.driver);
                        }

                        let classification = if spawn_is_unconfirmed {
                            None
                        } else {
                            classify_work_state_for_driver(slot.definition.driver, &chunk)
                        };
                        let work_state_event = classification.and_then(|(state, detail)| {
                            transition_work_state_locked(
                                &session_alias,
                                slot,
                                event_run_id,
                                state,
                                detail,
                            )
                        });

                        let quiesce_arm =
                            real_output_at
                                .filter(|_| !spawn_is_unconfirmed)
                                .map(|armed_at| {
                                    slot.last_real_output_at = Some(armed_at);
                                    cancel_quiesce_timer_locked(slot);
                                    (
                                        slot.definition.driver,
                                        slot.generation,
                                        event_run_id,
                                        armed_at,
                                    )
                                });
                        let ready_identity =
                            transitioned.then(|| next_run_event_identity(slot, event_run_id));
                        let output_identity = next_run_event_identity(slot, event_run_id);
                        Some((
                            session_alias,
                            ready_identity,
                            output_identity,
                            quiesce_arm,
                            work_state_event,
                        ))
                    } else {
                        None
                    }
                };
                let Some((
                    session_alias,
                    ready_identity,
                    output_identity,
                    quiesce_arm,
                    work_state_event,
                )) = outcome
                else {
                    return;
                };

                if let Some((driver, generation, run_id, armed_at)) = quiesce_arm {
                    self.arm_quiesce_timer(session_id, driver, generation, run_id, armed_at);
                }

                if let Some(event) = work_state_event {
                    self.emit_run_work_state(event, event_generation, event_run_id);
                }

                if let Some(identity) = ready_identity {
                    self.emit_run_event(RuntimeEvent::SessionState {
                        identity,
                        session: session_alias.clone(),
                        state: LifecycleState::Ready,
                        reason: "session emitted output".into(),
                        timestamp: now_rfc3339(),
                    });
                }

                self.emit_run_event(RuntimeEvent::SessionOutput {
                    identity: output_identity,
                    session: session_alias,
                    chunk,
                    synthetic: false,
                    timestamp: now_rfc3339(),
                });
            }
            PtyEvent::Closed => {
                self.retire_session_run(
                    session_id,
                    event_generation,
                    event_run_id,
                    RunRetirementCause::OutputClosed,
                );
            }
            PtyEvent::Error(error) => {
                self.retire_session_run(
                    session_id,
                    event_generation,
                    event_run_id,
                    RunRetirementCause::PtyError(error),
                );
            }
        }
    }

    fn resolve_pane_process_locked(
        process: &dyn PaneProcess,
        slots: &SessionRegistry,
    ) -> Result<(String, Uuid, SessionGeneration, Uuid)> {
        if !process.is_alive()? {
            return Err(anyhow!(
                "sideband caller process {} has exited",
                process.pid()
            ));
        }

        let mut matches = Vec::new();
        let mut query_failed = false;
        for (session_id, slot) in &slots.by_id {
            let Some(running) = slot.running.as_ref() else {
                continue;
            };
            let Some(pty) = running.pty.as_deref() else {
                query_failed = true;
                continue;
            };
            match process.belongs_to(pty) {
                Ok(true) => {
                    let Some(run_id) = slot.run_id else {
                        query_failed = true;
                        continue;
                    };
                    matches.push((
                        slot.definition.alias.clone(),
                        *session_id,
                        slot.generation,
                        run_id,
                    ));
                }
                Ok(false) => {}
                Err(_) => query_failed = true,
            }
        }

        if query_failed {
            return Err(anyhow!(
                "sideband caller process {} membership could not be verified",
                process.pid()
            ));
        }
        match matches.as_slice() {
            [(session, session_id, generation, run_id)] => {
                Ok((session.clone(), *session_id, *generation, *run_id))
            }
            [] => Err(anyhow!(
                "sideband caller process {} is not owned by a live pane",
                process.pid()
            )),
            _ => Err(anyhow!(
                "sideband caller process {} belongs to multiple live panes",
                process.pid()
            )),
        }
    }

    fn resolve_pane_caller(&self, process: Arc<dyn PaneProcess>) -> Result<PaneCaller> {
        let (session, session_id, generation, run_id) = {
            let slots = self.inner.slots.lock();
            Self::resolve_pane_process_locked(process.as_ref(), &slots)?
        };
        Ok(PaneCaller {
            session,
            session_id,
            generation,
            run_id,
            process,
        })
    }

    fn validate_pane_caller_locked(caller: &PaneCaller, slots: &SessionRegistry) -> Result<()> {
        let Some(bound_slot) = slots.get_by_id(caller.session_id) else {
            return Err(anyhow!("sideband caller run is stale"));
        };
        if bound_slot.definition.alias != caller.session
            || bound_slot.generation != caller.generation
            || bound_slot.run_id != Some(caller.run_id)
        {
            return Err(anyhow!("sideband caller run is stale"));
        }
        let (session, session_id, generation, run_id) =
            Self::resolve_pane_process_locked(caller.process.as_ref(), slots)?;
        if session != caller.session
            || session_id != caller.session_id
            || generation != caller.generation
            || run_id != caller.run_id
        {
            return Err(anyhow!("sideband caller run is stale"));
        }
        Ok(())
    }

    fn validate_pane_target_locked(
        caller: &PaneCaller,
        target: &str,
        slots: &SessionRegistry,
    ) -> Result<()> {
        if target != caller.session {
            return Err(anyhow!(
                "sideband request may target only the calling session"
            ));
        }
        Self::validate_pane_caller_locked(caller, slots)
    }

    fn pane_room_binding_locked(
        caller: &PaneCaller,
        slots: &SessionRegistry,
        rooms: &RoomState,
    ) -> Result<(RoomId, u64)> {
        Self::validate_pane_caller_locked(caller, slots)?;
        let room_id = *rooms
            .room_by_session
            .get(&caller.session_id)
            .ok_or_else(|| anyhow!("sideband caller is not a room member"))?;
        let room = rooms
            .get(room_id)
            .ok_or_else(|| anyhow!("sideband caller room is unavailable"))?;
        let floor = *room
            .join_floor_by_session
            .get(&caller.session_id)
            .ok_or_else(|| anyhow!("sideband caller room membership is unavailable"))?;
        Ok((room_id, floor))
    }

    fn authorize_sideband_request(
        &self,
        caller: &PaneCaller,
        request: &SidebandRequest,
    ) -> Result<()> {
        let slots = self.inner.slots.lock();
        match request {
            SidebandRequest::Ping {} => Self::validate_pane_caller_locked(caller, &slots),
            SidebandRequest::WaitQuiet {
                name,
                quiet_seconds,
                timeout_seconds,
            } => {
                Self::validate_pane_target_locked(caller, name, &slots)?;
                validate_wait_quiet_bounds(*quiet_seconds, *timeout_seconds)
            }
            SidebandRequest::SendInput { name, .. } | SidebandRequest::SendKey { name, .. } => {
                Self::validate_pane_target_locked(caller, name, &slots)
            }
            SidebandRequest::RoomRead { .. } => {
                let rooms = self.inner.rooms.lock();
                Self::pane_room_binding_locked(caller, &slots, &rooms).map(|_| ())
            }
            SidebandRequest::RoomPost { content } => {
                let rooms = self.inner.rooms.lock();
                Self::pane_room_binding_locked(caller, &slots, &rooms)?;
                validate_message_body(content)
            }
        }
    }

    fn read_room_feed_from_pane(
        &self,
        caller: &PaneCaller,
        cursor: Option<shared_types::RoomFeedCursor>,
    ) -> Result<RoomFeedPage> {
        let slots = self.inner.slots.lock();
        let rooms = self.inner.rooms.lock();
        let (room_id, floor) = Self::pane_room_binding_locked(caller, &slots, &rooms)?;
        rooms
            .get(room_id)
            .expect("validated pane room disappeared while room state was locked")
            .read(cursor, floor)
    }

    fn post_room_message_from_pane(
        &self,
        caller: &PaneCaller,
        content: String,
    ) -> Result<RoomPostResult> {
        validate_message_body(&content)?;
        let _room_event_publish = self.inner.room_event_publish.lock();
        let message_id = Uuid::new_v4();
        let feed_event = {
            let slots = self.inner.slots.lock();
            let mut rooms = self.inner.rooms.lock();
            let (room_id, _floor) = Self::pane_room_binding_locked(caller, &slots, &rooms)?;
            let room = rooms
                .get_mut(room_id)
                .expect("validated pane room disappeared while room state was locked");
            let membership_revision = room.definition.membership_revision;
            room.append(RoomFeedItem::Message {
                message_id,
                sender: RoomMessageSender::Session {
                    session_id: caller.session_id,
                },
                content,
                recipient_ids: Vec::new(),
                membership_revision,
            })?
        };
        let result = RoomPostResult {
            room_id: feed_event.room_id,
            message_id,
            cursor: feed_event.cursor,
        };
        #[cfg(test)]
        self.run_room_event_after_append_hook_for_tests();
        self.emit(RuntimeEvent::RoomFeedEvent { feed_event });
        Ok(result)
    }

    fn pane_input_target(&self, caller: &PaneCaller, name: &str) -> Result<RunWriteTarget> {
        {
            let slots = self.inner.slots.lock();
            Self::validate_pane_target_locked(caller, name, &slots)?;
            let slot = slots
                .get_by_id(caller.session_id)
                .with_context(|| format!("unknown session '{name}'"))?;
            run_write_target_from_slot(name, slot)
        }
    }

    fn send_pane_input(&self, caller: &PaneCaller, name: &str, input: &str) -> Result<usize> {
        let target = self.pane_input_target(caller, name)?;
        self.write_run_input(&target, Some(caller), input, RunInputSafety::Raw, None)
    }

    fn terminate_timed_out_pane_run(&self, caller: &PaneCaller) -> Result<()> {
        let (_, expected_stop) = self
            .declare_stop_operation(
                caller.session_id,
                Some((caller.generation, Some(caller.run_id))),
                StopIntentKind::Operator,
            )
            .map_err(|error| {
                if error.to_string().contains("run changed") {
                    anyhow!("sideband caller run is stale")
                } else {
                    error
                }
            })?;
        self.stop_session_at(caller.session_id, expected_stop)?;
        Ok(())
    }

    #[cfg(test)]
    fn apply_sideband_request(
        &self,
        caller: &PaneCaller,
        request: SidebandRequest,
    ) -> SidebandResponse {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .expect("failed to create sideband test runtime");
        runtime.block_on(self.apply_sideband_request_async(caller, request))
    }

    fn sideband_response(outcome: Result<String>) -> SidebandResponse {
        match outcome {
            Ok(message) => SidebandResponse {
                ok: true,
                message,
                timed_out: false,
                payload: None,
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(error.to_string()),
        }
    }

    fn rejected_sideband_response(message: impl Into<String>) -> SidebandResponse {
        SidebandResponse {
            ok: false,
            message: message.into(),
            timed_out: false,
            payload: None,
            request_id: None,
        }
    }

    async fn wait_quiet_from_pane(
        &self,
        caller: &PaneCaller,
        request: WaitQuietRequest,
    ) -> SidebandResponse {
        let quiet_window = Duration::from_secs(request.quiet_seconds as u64);
        let timeout = Duration::from_secs(request.timeout_seconds as u64);
        let wait_started_at = Instant::now();

        loop {
            let last_output_age_ms = {
                let slots = self.inner.slots.lock();
                if let Err(error) = Self::validate_pane_target_locked(caller, &request.name, &slots)
                {
                    return Self::rejected_sideband_response(error.to_string());
                }
                let slot = slots
                    .get_by_id(caller.session_id)
                    .expect("validated pane target disappeared while slots were locked");
                let last_real_output_at = slot.last_real_output_at.unwrap_or(wait_started_at);
                Instant::now()
                    .saturating_duration_since(last_real_output_at)
                    .as_millis() as u64
            };

            if last_output_age_ms >= quiet_window.as_millis() as u64 {
                return SidebandResponse {
                    ok: true,
                    message: "session is quiet".into(),
                    timed_out: false,
                    payload: Some(SidebandResponsePayload::WaitQuiet {
                        quiet_duration_ms: last_output_age_ms,
                    }),
                    request_id: None,
                };
            }

            if Instant::now().saturating_duration_since(wait_started_at) >= timeout {
                return SidebandResponse {
                    ok: false,
                    message: "wait_quiet timed out".into(),
                    timed_out: true,
                    payload: Some(SidebandResponsePayload::WaitQuietTimeout { last_output_age_ms }),
                    request_id: None,
                };
            }

            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn apply_authorized_sideband_request(
        &self,
        caller: &PaneCaller,
        request: SidebandRequest,
    ) -> SidebandResponse {
        match request {
            SidebandRequest::Ping {} => Self::sideband_response(Ok("pong".into())),
            SidebandRequest::WaitQuiet {
                name,
                quiet_seconds,
                timeout_seconds,
            } => {
                self.wait_quiet_from_pane(
                    caller,
                    WaitQuietRequest {
                        name,
                        quiet_seconds,
                        timeout_seconds,
                    },
                )
                .await
            }
            SidebandRequest::SendInput { name, input } => Self::sideband_response(
                self.send_pane_input(caller, &name, &input)
                    .map(|_| "input sent".into()),
            ),
            SidebandRequest::SendKey { name, key } => Self::sideband_response(
                self.send_pane_input(caller, &name, control_key_sequence(key))
                    .map(|_| format!("key {:?} sent", key)),
            ),
            SidebandRequest::RoomRead { cursor } => {
                match self.read_room_feed_from_pane(caller, cursor) {
                    Ok(page) => SidebandResponse {
                        ok: true,
                        message: "room feed read".into(),
                        timed_out: false,
                        payload: Some(SidebandResponsePayload::RoomFeed { page }),
                        request_id: None,
                    },
                    Err(error) => Self::rejected_sideband_response(error.to_string()),
                }
            }
            SidebandRequest::RoomPost { content } => {
                match self.post_room_message_from_pane(caller, content) {
                    Ok(result) => SidebandResponse {
                        ok: true,
                        message: "room message posted".into(),
                        timed_out: false,
                        payload: Some(SidebandResponsePayload::RoomPost { result }),
                        request_id: None,
                    },
                    Err(error) => Self::rejected_sideband_response(error.to_string()),
                }
            }
        }
    }

    async fn apply_sideband_write_with_timeout(
        &self,
        caller: PaneCaller,
        name: String,
        input: String,
        success_message: String,
        action: &'static str,
    ) -> SidebandResponse {
        let target = match self.pane_input_target(&caller, &name) {
            Ok(target) => target,
            Err(error) => return Self::rejected_sideband_response(error.to_string()),
        };
        let pty = target.pty.clone();
        let input_gate = target.input_gate.clone();
        let control = Arc::new(InputWriteControl::new());
        let writer_handle = self.clone();
        let writer_caller = caller.clone();
        let writer_target = target.clone();
        let writer_control = control.clone();
        let mut writer = tokio::task::spawn_blocking(move || {
            writer_handle
                .write_run_input(
                    &writer_target,
                    Some(&writer_caller),
                    &input,
                    RunInputSafety::Raw,
                    Some(&writer_control),
                )
                .map(|_| success_message)
        });
        let budget = *self.inner.sideband_write_timeout.lock();
        match tokio::time::timeout(budget, &mut writer).await {
            Ok(Ok(outcome)) => Self::sideband_response(outcome),
            Ok(Err(error)) => Self::rejected_sideband_response(format!(
                "sideband {action} worker failed: {error}"
            )),
            Err(_) => {
                let disposition = control.request_cancel();
                let owns_cancellation_barrier = disposition == InputCancelDisposition::InFlight;
                if owns_cancellation_barrier {
                    input_gate.begin_cancellation_barrier();
                }
                input_gate.wake_waiters();
                let cancellation_deadline = Instant::now() + Duration::from_secs(5);
                let mut cancellation_attempts = 0_u32;
                let mut cancellation_failure = None;

                loop {
                    if disposition == InputCancelDisposition::InFlight {
                        cancellation_attempts += 1;
                        let cancel_pty = pty.clone();
                        let cancellation =
                            tokio::task::spawn_blocking(move || cancel_pty.cancel_input_write());
                        match tokio::time::timeout(Duration::from_millis(250), cancellation).await {
                            Ok(Ok(Ok(()))) => {}
                            Ok(Ok(Err(error))) => {
                                cancellation_failure =
                                    Some(format!("isolated input cancellation failed: {error:#}"));
                                break;
                            }
                            Ok(Err(error)) => {
                                cancellation_failure = Some(format!(
                                    "isolated input cancellation worker failed: {error}"
                                ));
                                break;
                            }
                            Err(_) => {
                                cancellation_failure =
                                    Some("isolated input cancellation exceeded 250ms".into());
                                break;
                            }
                        }
                    }

                    let remaining = cancellation_deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    let poll = remaining.min(Duration::from_millis(50));
                    if let Ok(outcome) = tokio::time::timeout(poll, &mut writer).await {
                        let detail = match disposition {
                            InputCancelDisposition::BeforeStart => {
                                "write was cancelled before PTY input began".to_string()
                            }
                            InputCancelDisposition::InFlight => format!(
                                "isolated PTY input cancellation completed after {cancellation_attempts} attempt(s); run preserved"
                            ),
                            InputCancelDisposition::Finished => {
                                "writer completed as the deadline elapsed".to_string()
                            }
                        };
                        if owns_cancellation_barrier {
                            input_gate.end_cancellation_barrier();
                        }
                        return Self::sideband_response_after_write_deadline(
                            action, budget, detail, outcome,
                        );
                    }
                }

                let terminate_handle = self.clone();
                let terminate_caller = caller.clone();
                let termination = tokio::task::spawn_blocking(move || {
                    terminate_handle.terminate_timed_out_pane_run(&terminate_caller)
                });
                let termination_detail =
                    match tokio::time::timeout(Duration::from_secs(5), termination).await {
                        Ok(Ok(Ok(()))) => {
                            "exact run terminated as cancellation fallback".to_string()
                        }
                        Ok(Ok(Err(error))) => format!("run termination failed: {error:#}"),
                        Ok(Err(error)) => format!("run termination worker failed: {error}"),
                        Err(_) => "run termination exceeded 5s".to_string(),
                    };
                let cancellation_detail = cancellation_failure.unwrap_or_else(|| match disposition {
                    InputCancelDisposition::BeforeStart => {
                        "cancelled queued writer did not stop within 5s".into()
                    }
                    InputCancelDisposition::InFlight => format!(
                        "isolated input cancellation did not stop the writer after {cancellation_attempts} attempt(s)"
                    ),
                    InputCancelDisposition::Finished => {
                        "finished writer did not join within 5s".into()
                    }
                });
                match tokio::time::timeout(Duration::from_secs(5), &mut writer).await {
                    Ok(outcome) => Self::sideband_response_after_write_deadline(
                        action,
                        budget,
                        format!("{cancellation_detail}; {termination_detail}"),
                        outcome,
                    ),
                    Err(_) => SidebandResponse {
                        ok: false,
                        message: format!(
                            "sideband action '{action}' timed out after {}ms; {cancellation_detail}; {termination_detail}; writer did not stop within 5s of fallback termination",
                            budget.as_millis()
                        ),
                        timed_out: true,
                        payload: None,
                        request_id: None,
                    },
                }
            }
        }
    }

    fn sideband_response_after_write_deadline(
        action: &str,
        budget: Duration,
        cancellation_detail: String,
        outcome: std::result::Result<Result<String>, tokio::task::JoinError>,
    ) -> SidebandResponse {
        match outcome {
            Ok(Ok(message)) => SidebandResponse {
                ok: true,
                message: format!(
                    "{message}; completed after the {}ms sideband deadline before cancellation took effect; {cancellation_detail}",
                    budget.as_millis()
                ),
                timed_out: true,
                payload: None,
                request_id: None,
            },
            Ok(Err(error)) => {
                let bytes_written = error
                    .downcast_ref::<PtyWriteError>()
                    .map(PtyWriteError::bytes_written)
                    .unwrap_or(0);
                let partial_detail = if bytes_written > 0 {
                    format!(
                        "; {bytes_written} bytes reached the PTY before cancellation and content may be partial"
                    )
                } else {
                    String::new()
                };
                SidebandResponse {
                    ok: false,
                    message: format!(
                        "sideband action '{action}' timed out after {}ms; {cancellation_detail}; writer stopped: {error:#}{partial_detail}",
                        budget.as_millis()
                    ),
                    timed_out: true,
                    payload: None,
                    request_id: None,
                }
            }
            Err(error) => SidebandResponse {
                ok: false,
                message: format!(
                    "sideband action '{action}' timed out after {}ms; {cancellation_detail}; writer worker failed: {error}",
                    budget.as_millis()
                ),
                timed_out: true,
                payload: None,
                request_id: None,
            },
        }
    }

    async fn apply_sideband_request_async(
        &self,
        caller: &PaneCaller,
        request: SidebandRequest,
    ) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }

        #[cfg(test)]
        if let Some(hook) = self
            .inner
            .sideband_after_initial_authorization
            .lock()
            .take()
        {
            hook();
        }

        if matches!(
            request,
            SidebandRequest::RoomRead { .. } | SidebandRequest::RoomPost { .. }
        ) {
            return self
                .apply_authorized_sideband_request(caller, request)
                .await;
        }

        let request_id = Uuid::new_v4().to_string();
        let action = match &request {
            SidebandRequest::Ping {} => "ping",
            SidebandRequest::WaitQuiet { .. } => "wait_quiet",
            SidebandRequest::SendInput { .. } => "send_input",
            SidebandRequest::SendKey { .. } => "send_key",
            SidebandRequest::RoomRead { .. } | SidebandRequest::RoomPost { .. } => {
                unreachable!("room sideband requests return before dispatch metadata")
            }
        };
        if let Err(error) = self.emit_dispatch_attempt_from_pane_caller(&request_id, action, caller)
        {
            return Self::rejected_sideband_response(error.to_string());
        }
        let mut response = match request {
            SidebandRequest::SendInput { name, input } => {
                self.apply_sideband_write_with_timeout(
                    caller.clone(),
                    name,
                    input,
                    "input sent".into(),
                    "send_input",
                )
                .await
            }
            SidebandRequest::SendKey { name, key } => {
                self.apply_sideband_write_with_timeout(
                    caller.clone(),
                    name,
                    control_key_sequence(key).into(),
                    format!("key {:?} sent", key),
                    "send_key",
                )
                .await
            }
            request @ (SidebandRequest::Ping {} | SidebandRequest::WaitQuiet { .. }) => {
                self.apply_authorized_sideband_request(caller, request)
                    .await
            }
            SidebandRequest::RoomRead { .. } | SidebandRequest::RoomPost { .. } => {
                unreachable!("room sideband requests return before PTY dispatch")
            }
        };
        response.request_id = Some(request_id);
        response
    }

    fn emit_run_work_state(
        &self,
        event: RuntimeEvent,
        generation: SessionGeneration,
        run_id: Uuid,
    ) {
        let RuntimeEvent::SessionWorkState {
            identity, state, ..
        } = &event
        else {
            debug_assert!(false, "emit_run_work_state requires SessionWorkState");
            return;
        };

        #[cfg(test)]
        if let Some(hook) = self.inner.work_state_before_side_effect.lock().take() {
            hook();
        }

        let is_current = self
            .inner
            .slots
            .lock()
            .get_by_id(identity.session_id)
            .map(|slot| (slot.generation, slot.run_id))
            == Some((generation, Some(run_id)));
        if is_current {
            self.handle_session_work_state_side_effects(
                identity.session_id,
                generation,
                run_id,
                *state,
            );
        }
        self.emit_run_event(event);
    }

    fn is_current_run(
        &self,
        session_id: SessionId,
        generation: SessionGeneration,
        run_id: Uuid,
    ) -> bool {
        self.inner
            .slots
            .lock()
            .get_by_id(session_id)
            .map(|slot| (slot.generation, slot.run_id))
            == Some((generation, Some(run_id)))
    }

    fn emit_run_event(&self, event: RuntimeEvent) {
        let identity = match &event {
            RuntimeEvent::SessionState { identity, .. }
            | RuntimeEvent::SessionOutput { identity, .. }
            | RuntimeEvent::SessionExit { identity, .. }
            | RuntimeEvent::SessionWorkState { identity, .. } => *identity,
            _ => {
                debug_assert!(false, "emit_run_event requires a run-derived event");
                self.emit(event);
                return;
            }
        };

        let should_drain = {
            let mut streams = self.inner.run_event_publish.lock();
            let Some(stream) = streams.get_mut(&identity.session_id) else {
                eprintln!(
                    "run event for unknown or retired session stream {} sequence {} was dropped",
                    identity.session_id, identity.sequence
                );
                return;
            };
            let Some(next_sequence) = stream.next_sequence else {
                eprintln!(
                    "run event stream {} is exhausted; dropping sequence {}",
                    identity.session_id, identity.sequence
                );
                return;
            };
            if identity.sequence < next_sequence {
                eprintln!(
                    "duplicate run event for stream {}: sequence {} < next {}",
                    identity.session_id, identity.sequence, next_sequence
                );
                return;
            }
            if stream.pending.insert(identity.sequence, event).is_some() {
                eprintln!(
                    "duplicate pending run event for stream {} sequence {}",
                    identity.session_id, identity.sequence
                );
                return;
            }
            if stream.draining {
                false
            } else {
                stream.draining = true;
                true
            }
        };
        if !should_drain {
            return;
        }

        loop {
            let next_event = {
                let mut streams = self.inner.run_event_publish.lock();
                let Some(stream) = streams.get_mut(&identity.session_id) else {
                    return;
                };
                let Some(next_sequence) = stream.next_sequence else {
                    stream.draining = false;
                    return;
                };
                match stream.pending.remove(&next_sequence) {
                    Some(event) => {
                        stream.next_sequence = next_sequence.checked_add(1);
                        Some(event)
                    }
                    None => {
                        stream.draining = false;
                        None
                    }
                }
            };
            let Some(event) = next_event else {
                return;
            };
            self.emit(event);
        }
    }

    fn emit(&self, event: RuntimeEvent) {
        let appended = match audit_event_projection(&event, &self.inner) {
            Some(audit_event) => match self.inner.audit.append(&audit_event) {
                Ok(()) => true,
                Err(error) => {
                    eprintln!("audit log failure: {error}");
                    false
                }
            },
            None => false,
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
    fn prepare_launch_spec_for_spawn(
        &self,
        definition: &SessionDefinition,
        qualified_directory: &QualifiedWorkingDirectory,
    ) -> Result<PreparedLaunch> {
        validate_driver_working_directory_pair(definition.driver, qualified_directory)?;
        let (mut spec, wsl_scope) = if definition.driver == DriverKind::Prime {
            self.ensure_wsl_reconciled()?;
            let (expected_device, expected_inode) =
                parse_wsl_identity(&qualified_directory.identity)?;
            let control = self.inner.wsl_control.read().clone();
            let prime_executable = control.resolve_prime_executable()?;
            let wsl_executable = control.wsl_executable()?;
            let scope = WslRunScope {
                distro: driver_prime::WSL_DISTRO.into(),
                unit: format!(
                    "{}{}",
                    driver_prime::SCOPE_UNIT_PREFIX,
                    Uuid::new_v4().simple()
                ),
            };
            let spec = driver_prime::launch_spec(
                definition,
                &wsl_executable,
                &prime_executable,
                &scope.unit,
                expected_device,
                expected_inode,
            )?;
            (spec, Some(scope))
        } else {
            let resolved = self
                .inner
                .executable_resolver
                .read()
                .resolve(definition.driver)?;
            (build_launch_spec(definition, &resolved)?, None)
        };

        // Prime is intentionally excluded: native Windows Job membership is
        // the pane-sideband authority, and WSL has no equivalent verified
        // caller-identity bridge in this release.
        if definition.driver != DriverKind::Prime
            && let Some(status) = self.inner.control_plane.read().clone()
        {
            spec.env.push(EnvVar {
                key: "PRIM1_PANE_IDENTITY".into(),
                value: definition.alias.clone(),
            });
            spec.env.push(EnvVar {
                key: "PRIM1_CONTROL_PLANE_ENDPOINT".into(),
                value: status.endpoint,
            });
            spec.env.push(EnvVar {
                key: "PRIM1_CONTROL_PLANE_TRANSPORT".into(),
                value: status.transport,
            });
            #[cfg(windows)]
            {
                let server = PinnedProcess::open(std::process::id())
                    .context("failed to identify the control-plane server process")?;
                spec.env.push(EnvVar {
                    key: "PRIM1_CONTROL_PLANE_SERVER_PID".into(),
                    value: server.pid().to_string(),
                });
                spec.env.push(EnvVar {
                    key: "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME".into(),
                    value: server.creation_time_filetime().to_string(),
                });
                if let (Some(executable), Some(client)) = (
                    self.inner.pane_mcp_executable.as_deref(),
                    PaneMcpClient::for_driver(definition.driver),
                ) {
                    augment_launch_spec_with_pane_mcp(&mut spec, client, executable)?;
                }
            }
        }

        Ok(PreparedLaunch { spec, wsl_scope })
    }

    fn deliver_prepared_payload(
        &self,
        target: &RunWriteTarget,
        payload: &str,
        submit_behavior: SubmitBehavior,
    ) -> Result<DeliveryWriteResult> {
        let bytes_written = match submit_behavior.framing {
            MessageFraming::BracketedPaste => {
                self.write_bracketed_submission(target, payload, submit_behavior)?
            }
            MessageFraming::RawSingleLine => {
                let mut input = frame_message_payload(payload, submit_behavior.framing);
                input.push_str(submit_behavior.sequence);
                self.write_run_input(target, None, &input, RunInputSafety::Raw, None)?
            }
        };

        Ok(DeliveryWriteResult {
            bytes_written,
            payload_part_count: 1,
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

#[cfg(windows)]
struct ControlPlaneListener {
    shutdown: tokio::sync::watch::Sender<bool>,
    thread: Option<thread::JoinHandle<Result<()>>>,
}

#[cfg(windows)]
enum ControlPlaneJoinOutcome {
    Joined(Result<()>),
    TimedOut(ControlPlaneListener),
}

#[cfg(windows)]
impl ControlPlaneListener {
    fn cancel(&self) {
        self.shutdown.send_replace(true);
    }

    fn join(mut self) -> Result<()> {
        let Some(thread) = self.thread.take() else {
            return Ok(());
        };
        thread
            .join()
            .map_err(|_| anyhow!("control plane listener thread panicked"))?
    }

    fn join_until(self, deadline: Instant) -> ControlPlaneJoinOutcome {
        loop {
            let Some(listener_thread) = self.thread.as_ref() else {
                return ControlPlaneJoinOutcome::Joined(Ok(()));
            };
            if listener_thread.is_finished() {
                return ControlPlaneJoinOutcome::Joined(self.join());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return ControlPlaneJoinOutcome::TimedOut(self);
            }
            thread::sleep(remaining.min(Duration::from_millis(5)));
        }
    }

    fn stop(self) -> Result<()> {
        self.cancel();
        self.join()
    }
}

#[cfg(windows)]
struct PreparedControlPlaneListener {
    activation: mpsc::SyncSender<()>,
    listener: ControlPlaneListener,
}

#[cfg(windows)]
impl PreparedControlPlaneListener {
    fn activate(self) -> Result<ControlPlaneListener> {
        let Self {
            activation,
            listener,
        } = self;
        if activation.send(()).is_err() {
            let _ = listener.stop();
            return Err(anyhow!("control plane listener stopped before activation"));
        }
        Ok(listener)
    }
}

#[cfg(windows)]
fn prepare_control_plane_thread(
    handle: SupervisorHandle,
    status: ControlPlaneStatus,
) -> Result<PreparedControlPlaneListener> {
    let (ready_tx, ready_rx) = mpsc::sync_channel::<Result<(), String>>(1);
    let (activation_tx, activation_rx) = mpsc::sync_channel::<()>(1);
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let thread = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                let _ = ready_tx.send(Err(format!(
                    "failed to create control plane runtime: {error}"
                )));
                eprintln!("failed to create control plane runtime: {error}");
                return Err(error).context("failed to create control plane runtime");
            }
        };

        let result = runtime.block_on(run_windows_pipe_server(
            handle,
            status,
            ready_tx,
            activation_rx,
            shutdown_rx,
        ));

        if let Err(error) = &result {
            eprintln!("control plane failed: {error}");
        }
        result
    });

    let listener = ControlPlaneListener {
        shutdown: shutdown_tx,
        thread: Some(thread),
    };

    let startup = match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(anyhow!(error)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            Err(anyhow!("timed out preparing control plane listener"))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(anyhow!("control plane listener stopped during startup"))
        }
    };
    if let Err(error) = startup {
        drop(activation_tx);
        let _ = listener.stop();
        return Err(error);
    }

    Ok(PreparedControlPlaneListener {
        activation: activation_tx,
        listener,
    })
}

fn build_launch_spec(
    definition: &SessionDefinition,
    resolved: &ResolvedLaunchProgram,
) -> Result<LaunchSpec> {
    match definition.driver {
        DriverKind::Claude => driver_claude::launch_spec(definition, &resolved.program),
        DriverKind::Codex => {
            driver_codex::launch_spec(definition, &resolved.program, &resolved.prefix_args)
        }
        DriverKind::Grok => driver_grok::launch_spec(definition, &resolved.program),
        DriverKind::Prime => {
            return Err(anyhow!(
                "Prime launch specifications require a qualified WSL scope"
            ));
        }
        DriverKind::GenericTerminal => {
            let mut spec = driver_generic_terminal::launch_spec(definition, &resolved.program)?;
            spec.args.extend(resolved.prefix_args.clone());
            return Ok(spec);
        }
    }
    .map_err(anyhow::Error::from)
}

#[cfg(windows)]
fn validate_pane_mcp_executable(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err(anyhow!(
            "pane MCP executable must be an absolute path: {}",
            path.display()
        ));
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("failed to inspect pane MCP executable {}", path.display()))?;
    if metadata_is_link_or_reparse(&metadata) || !metadata.is_file() {
        return Err(anyhow!(
            "pane MCP executable must be a regular non-reparse file: {}",
            path.display()
        ));
    }
    Ok(())
}

#[cfg(windows)]
fn augment_launch_spec_with_pane_mcp(
    spec: &mut LaunchSpec,
    client: PaneMcpClient,
    pane_mcp_executable: &Path,
) -> Result<()> {
    validate_pane_mcp_executable(pane_mcp_executable)?;
    let executable = child_process_path(pane_mcp_executable)
        .into_os_string()
        .into_string()
        .map_err(|_| anyhow!("pane MCP executable path is not valid Unicode"))?;

    match client {
        PaneMcpClient::Claude => {
            let mut servers = serde_json::Map::new();
            servers.insert(
                PANE_MCP_SERVER_NAME.into(),
                serde_json::json!({
                    "type": "stdio",
                    "command": executable,
                    "args": [PANE_MCP_MODE_ARGUMENT],
                }),
            );
            let config = serde_json::to_string(&serde_json::json!({
                "mcpServers": servers,
            }))
            .context("failed to encode Claude pane MCP configuration")?;
            spec.args.extend(["--mcp-config".into(), config]);
        }
        PaneMcpClient::Codex => {
            let command = serde_json::to_string(&executable)
                .context("failed to encode Codex pane MCP command")?;
            let arguments = serde_json::to_string(&[PANE_MCP_MODE_ARGUMENT])
                .context("failed to encode Codex pane MCP arguments")?;
            spec.args.extend([
                "-c".into(),
                format!("mcp_servers.{PANE_MCP_SERVER_NAME}.command={command}"),
                "-c".into(),
                format!("mcp_servers.{PANE_MCP_SERVER_NAME}.args={arguments}"),
                "-c".into(),
                format!("mcp_servers.{PANE_MCP_SERVER_NAME}.required=true"),
            ]);
        }
    }
    Ok(())
}

fn resolve_driver_executable(driver: DriverKind) -> Result<ResolvedLaunchProgram> {
    match driver {
        DriverKind::Claude => Ok(ResolvedLaunchProgram {
            program: find_direct_executable(&[if cfg!(windows) {
                "claude.exe"
            } else {
                "claude"
            }])?,
            prefix_args: Vec::new(),
        }),
        DriverKind::Codex => resolve_codex_executable(),
        DriverKind::Grok => Ok(ResolvedLaunchProgram {
            program: find_direct_executable(&[if cfg!(windows) { "grok.exe" } else { "grok" }])?,
            prefix_args: Vec::new(),
        }),
        DriverKind::Prime => Err(anyhow!(
            "Prime executable resolution is owned by the Ubuntu WSL controller"
        )),
        DriverKind::GenericTerminal => Ok(ResolvedLaunchProgram {
            program: find_direct_executable(if cfg!(windows) {
                &["powershell.exe", "pwsh.exe"]
            } else {
                &["bash"]
            })?,
            prefix_args: Vec::new(),
        }),
    }
}

fn find_direct_executable(names: &[&str]) -> Result<String> {
    let search_path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
    find_direct_executable_on_path(names, &search_path)
}

fn find_direct_executable_on_path(names: &[&str], search_path: &std::ffi::OsStr) -> Result<String> {
    // `names` is an ordered preference list. Search every PATH directory for
    // the preferred executable before considering a fallback. On Windows this
    // keeps the built-in powershell.exe ahead of Store/App Execution Alias
    // pwsh.exe entries that cannot be launched in the session job.
    for name in names {
        for directory in std::env::split_paths(search_path) {
            let candidate = directory.join(name);
            let Ok(metadata) = fs::metadata(&candidate) else {
                continue;
            };
            if !metadata.is_file() {
                continue;
            }
            let canonical = fs::canonicalize(&candidate)
                .with_context(|| format!("failed to qualify executable {}", candidate.display()))?;
            return Ok(child_process_path(&canonical)
                .to_string_lossy()
                .into_owned());
        }
    }
    Err(anyhow!(
        "no supported direct executable found on PATH (looked for {})",
        names.join(", ")
    ))
}

fn resolve_codex_executable() -> Result<ResolvedLaunchProgram> {
    let direct_name = if cfg!(windows) { "codex.exe" } else { "codex" };
    if let Ok(program) = find_direct_executable(&[direct_name]) {
        return Ok(ResolvedLaunchProgram {
            program,
            prefix_args: Vec::new(),
        });
    }

    #[cfg(windows)]
    {
        let search_path = std::env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
        for directory in std::env::split_paths(&search_path) {
            let shim = directory.join("codex.cmd");
            if !shim.is_file() {
                continue;
            }
            let script = directory
                .join("node_modules")
                .join("@openai")
                .join("codex")
                .join("bin")
                .join("codex.js");
            if !script.is_file() {
                continue;
            }
            let node = find_direct_executable(&["node.exe"])?;
            let script = fs::canonicalize(&script)
                .with_context(|| format!("failed to qualify Codex script {}", script.display()))?;
            return Ok(ResolvedLaunchProgram {
                program: node,
                prefix_args: vec![child_process_path(&script).to_string_lossy().into_owned()],
            });
        }
    }

    Err(anyhow!(
        "Codex requires a direct codex executable or node.exe plus the canonical npm codex.js"
    ))
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
        DriverKind::Grok => driver_grok::classify_work_state(chunk),
        DriverKind::Prime => None,
        DriverKind::GenericTerminal => None,
    }
}

fn transition_work_state_locked(
    session_name: &str,
    slot: &mut SessionSlot,
    run_id: Uuid,
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
    let identity = next_run_event_identity(slot, run_id);
    Some(RuntimeEvent::SessionWorkState {
        identity,
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
        // Grok Build 1.0.0 continuously redraws its full-screen TUI at a
        // variable cadence. Silence is therefore not an honest idle signal;
        // its measured semantic markers own work-state transitions instead.
        DriverKind::Grok => return None,
        DriverKind::Prime => return None,
        DriverKind::GenericTerminal => Duration::from_secs(5),
    };

    (threshold >= Duration::from_secs(1) && threshold <= Duration::from_secs(30))
        .then_some(threshold)
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

fn heartbeat_interval_from_env() -> Duration {
    positive_duration_from_env(
        "PRIM1_HEARTBEAT_INTERVAL_SECS",
        DEFAULT_HEARTBEAT_INTERVAL_SECS,
    )
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

fn auto_restart_sessions_from_env() -> Vec<SessionId> {
    std::env::var("PRIM1_AUTO_RESTART_ON_STALL")
        .unwrap_or_default()
        .split(',')
        .filter_map(|session| session.trim().parse::<SessionId>().ok())
        .collect()
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

fn routed_message_payload(request: &RouteMessageRequest, behavior: SubmitBehavior) -> String {
    let header = routed_message_header(request.scope, &request.from);
    if request.content.is_empty() {
        header
    } else if behavior.framing == MessageFraming::RawSingleLine {
        format!("{header} {}", request.content)
    } else {
        format!("{header}\n{}", request.content)
    }
}

fn routed_message_submit_behavior(driver: DriverKind) -> SubmitBehavior {
    match driver {
        DriverKind::Claude | DriverKind::Codex | DriverKind::Grok | DriverKind::Prime => {
            SubmitBehavior {
                sequence: "\r",
                framing: MessageFraming::BracketedPaste,
                submit_delay: BRACKETED_PASTE_SUBMIT_DELAY,
            }
        }
        DriverKind::GenericTerminal => SubmitBehavior {
            sequence: "\r",
            framing: MessageFraming::RawSingleLine,
            submit_delay: Duration::ZERO,
        },
    }
}

fn routed_message_header(scope: MessageScope, sender: &str) -> String {
    format!("[{} message from {}]", scope_label(scope), sender)
}

fn validate_message_body(content: &str) -> Result<()> {
    if content.len() > MESSAGE_BODY_MAX_BYTES {
        return Err(anyhow!(
            "message body is {} bytes; maximum is {} bytes",
            content.len(),
            MESSAGE_BODY_MAX_BYTES
        ));
    }
    if let Some((offset, ch)) = content
        .char_indices()
        .find(|(_, ch)| ch.is_control() && !matches!(*ch, '\t' | '\n' | '\r'))
    {
        return Err(anyhow!(
            "message body contains disallowed control character U+{:04X} at byte offset {offset}",
            ch as u32
        ));
    }
    Ok(())
}

fn bounded_room_delivery_error(error: &str) -> String {
    const MAX_CHARS: usize = 4096;
    let mut chars = error.chars();
    let bounded = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}…")
    } else {
        bounded
    }
}

fn validate_message_framing(content: &str, behavior: SubmitBehavior) -> Result<()> {
    if behavior.framing == MessageFraming::RawSingleLine && content.chars().any(char::is_control) {
        return Err(anyhow!(
            "generic terminal delivery supports only printable single-line message bodies"
        ));
    }
    Ok(())
}

fn frame_message_payload(content: &str, framing: MessageFraming) -> String {
    let framing_bytes = match framing {
        MessageFraming::BracketedPaste => BRACKETED_PASTE_START.len() + BRACKETED_PASTE_END.len(),
        MessageFraming::RawSingleLine => 0,
    };
    let mut input = String::with_capacity(content.len() + framing_bytes);
    if framing == MessageFraming::BracketedPaste {
        input.push_str(BRACKETED_PASTE_START);
    }
    input.push_str(content);
    if framing == MessageFraming::BracketedPaste {
        input.push_str(BRACKETED_PASTE_END);
    }
    input
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

fn validate_wait_quiet_bounds(quiet_seconds: u32, timeout_seconds: u32) -> Result<()> {
    if !(1..=SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS).contains(&quiet_seconds) {
        return Err(anyhow!(
            "wait_quiet quiet_seconds must be between 1 and {SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS}"
        ));
    }
    if !(1..=SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS).contains(&timeout_seconds) {
        return Err(anyhow!(
            "wait_quiet timeout_seconds must be between 1 and {SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS}"
        ));
    }
    if quiet_seconds > timeout_seconds {
        return Err(anyhow!(
            "wait_quiet quiet_seconds must not exceed timeout_seconds"
        ));
    }
    Ok(())
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
        format!(
            "{DEFAULT_ENDPOINT}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        )
    }

    #[cfg(not(windows))]
    {
        format!(
            "{DEFAULT_ENDPOINT}-{}-{}.sock",
            std::process::id(),
            Uuid::new_v4()
        )
    }
}

#[cfg(windows)]
fn create_windows_pipe_server(
    endpoint: &str,
    first_instance: bool,
) -> Result<tokio::net::windows::named_pipe::NamedPipeServer> {
    use std::{mem, ptr};
    use tokio::net::windows::named_pipe::ServerOptions;
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;

    let descriptor = current_user_only_security_descriptor("", "GA")?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.as_ptr(),
        bInheritHandle: 0,
    };
    let mut options = ServerOptions::new();
    options
        .reject_remote_clients(true)
        .first_pipe_instance(first_instance);
    unsafe {
        options.create_with_security_attributes_raw(endpoint, ptr::from_mut(&mut attributes).cast())
    }
    .with_context(|| format!("failed to create named pipe {endpoint}"))
}

#[cfg(windows)]
async fn run_windows_pipe_server(
    handle: SupervisorHandle,
    status: ControlPlaneStatus,
    ready: mpsc::SyncSender<Result<(), String>>,
    activation: mpsc::Receiver<()>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let first_server = match create_windows_pipe_server(&status.endpoint, true) {
        Ok(server) => server,
        Err(error) => {
            let message = error.to_string();
            let _ = ready.send(Err(message.clone()));
            return Err(anyhow!(message));
        }
    };
    ready
        .send(Ok(()))
        .map_err(|_| anyhow!("control plane startup receiver disconnected"))?;
    if activation.recv().is_err() {
        return Ok(());
    }
    if *shutdown.borrow() {
        return Ok(());
    }

    let connection_slots = Arc::new(Semaphore::new(SIDEBAND_MAX_CONNECTIONS));
    let mut prepared_server = Some(first_server);
    loop {
        let server = match prepared_server.take() {
            Some(server) => server,
            None => create_windows_pipe_server(&status.endpoint, false)?,
        };
        tokio::select! {
            changed = shutdown.changed() => {
                match changed {
                    Ok(()) if *shutdown.borrow() => return Ok(()),
                    Ok(()) => continue,
                    Err(_) => return Ok(()),
                }
            }
            connected = server.connect() => {
                connected.context("failed to connect named pipe")?;
            }
        }
        let Some(connection_permit) = reserve_sideband_connection(&connection_slots) else {
            continue;
        };
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            let _connection_permit = connection_permit;
            if let Err(error) = handle_windows_sideband_connection(handle_clone, server).await {
                eprintln!("named pipe connection failed: {error}");
            }
        });
    }
}

fn reserve_sideband_connection(slots: &Arc<Semaphore>) -> Option<OwnedSemaphorePermit> {
    slots.clone().try_acquire_owned().ok()
}

#[cfg(windows)]
fn pane_process_from_windows_pipe(
    server: &tokio::net::windows::named_pipe::NamedPipeServer,
) -> Result<Arc<dyn PaneProcess>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeClientProcessId};

    let mut process_id = 0_u32;
    let ok =
        unsafe { GetNamedPipeClientProcessId(server.as_raw_handle() as HANDLE, &mut process_id) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error())
            .context("failed to identify named-pipe client process");
    }
    Ok(Arc::new(WindowsPaneProcess::open(process_id)?))
}

#[cfg(windows)]
async fn handle_windows_sideband_connection(
    handle: SupervisorHandle,
    mut server: tokio::net::windows::named_pipe::NamedPipeServer,
) -> Result<()> {
    const ACCESS_DENIED: &str = "sideband access denied";
    let caller = match pane_process_from_windows_pipe(&server)
        .and_then(|process| handle.resolve_pane_caller(process))
    {
        Ok(caller) => caller,
        Err(error) => {
            eprintln!("rejected unaffiliated sideband client: {error:#}");
            let response = SupervisorHandle::rejected_sideband_response(ACCESS_DENIED);
            let payload = format!("{}\n", encode_response(&response)?);
            write_sideband_response(
                &mut server,
                payload.as_bytes(),
                SIDEBAND_RESPONSE_WRITE_TIMEOUT,
            )
            .await?;
            return Ok(());
        }
    };

    handle_sideband_stream(handle, caller, server).await
}

async fn handle_sideband_stream<Stream>(
    handle: SupervisorHandle,
    caller: PaneCaller,
    stream: Stream,
) -> Result<()>
where
    Stream: tokio::io::AsyncRead + AsyncWrite + Unpin,
{
    let (read_half, mut write_half) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let frame = read_sideband_frame(
        &mut reader,
        SIDEBAND_FRAME_MAX_BYTES,
        SIDEBAND_FRAME_READ_TIMEOUT,
    )
    .await?;
    let request = decode_request(&frame).context("invalid sideband payload")?;
    let response = handle.apply_sideband_request_async(&caller, request).await;
    let payload = format!("{}\n", encode_response(&response)?);
    write_sideband_response(
        &mut write_half,
        payload.as_bytes(),
        SIDEBAND_RESPONSE_WRITE_TIMEOUT,
    )
    .await?;
    Ok(())
}

async fn write_sideband_response<Writer>(
    writer: &mut Writer,
    payload: &[u8],
    timeout: Duration,
) -> Result<()>
where
    Writer: AsyncWrite + Unpin,
{
    tokio::time::timeout(timeout, async {
        writer
            .write_all(payload)
            .await
            .context("failed to write sideband response")?;
        writer
            .flush()
            .await
            .context("failed to flush sideband response")?;
        Ok::<(), anyhow::Error>(())
    })
    .await
    .map_err(|_| anyhow!("sideband response write timed out"))?
}

async fn read_sideband_frame<Reader>(
    reader: &mut Reader,
    max_bytes: usize,
    timeout: Duration,
) -> Result<String>
where
    Reader: AsyncBufRead + Unpin,
{
    tokio::time::timeout(timeout, async {
        let mut frame = Vec::new();
        loop {
            let available = reader
                .fill_buf()
                .await
                .context("failed to read sideband request")?;
            if available.is_empty() {
                return Err(anyhow!("unterminated sideband request"));
            }

            let consumed = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map(|index| index + 1)
                .unwrap_or(available.len());
            if frame.len().saturating_add(consumed) > max_bytes {
                return Err(anyhow!(
                    "sideband request exceeds {max_bytes}-byte frame limit"
                ));
            }

            frame.extend_from_slice(&available[..consumed]);
            reader.consume(consumed);
            if frame.last() == Some(&b'\n') {
                frame.pop();
                if frame.last() == Some(&b'\r') {
                    frame.pop();
                }
                return String::from_utf8(frame)
                    .map_err(|_| anyhow!("invalid UTF-8 sideband request"));
            }
        }
    })
    .await
    .map_err(|_| anyhow!("sideband request timed out before newline"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use control_plane::{decode_response, encode_request};
    use parking_lot::Condvar;
    use pty_host::PtySession as PtySessionTrait;
    use shared_types::{MessageScope, SidebandRequest};
    use std::{
        collections::{BTreeSet, VecDeque},
        io::Read,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    #[cfg(windows)]
    fn security_descriptor_sddl_for_handle(
        handle: windows_sys::Win32::Foundation::HANDLE,
    ) -> String {
        use std::ptr;
        use windows_sys::Win32::Security::{
            Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
            DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR,
        };

        let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                handle,
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        assert_eq!(result, 0, "GetSecurityInfo failed");
        security_descriptor_to_sddl(LocalSecurityDescriptor(descriptor))
    }

    #[cfg(windows)]
    fn security_descriptor_to_sddl(descriptor: LocalSecurityDescriptor) -> String {
        use std::ptr;
        use windows_sys::Win32::Security::{
            Authorization::{
                ConvertSecurityDescriptorToStringSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION,
        };

        let mut text = ptr::null_mut();
        let mut len = 0_u32;
        let converted = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor.as_ptr(),
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &mut text,
                &mut len,
            )
        };
        assert_ne!(converted, 0, "failed to convert security descriptor");
        let text_guard = LocalSecurityDescriptor(text.cast());
        let sddl = String::from_utf16(unsafe {
            std::slice::from_raw_parts(text, len.saturating_sub(1) as usize)
        })
        .unwrap();
        drop(text_guard);
        sddl
    }

    #[cfg(windows)]
    fn assert_current_user_only_sddl(sddl: &str, current_sid: &str) {
        assert!(sddl.starts_with("D:P"), "DACL is not protected: {sddl}");
        assert!(sddl.contains(current_sid), "current SID missing: {sddl}");
        assert_eq!(sddl.matches("(A;").count(), 1, "unexpected ACE: {sddl}");
        for broad_sid in [";;;WD)", ";;;BU)", ";;;AU)", ";;;IU)", ";;;BA)"] {
            assert!(!sddl.contains(broad_sid), "broad ACE in DACL: {sddl}");
        }
    }

    #[derive(Clone, Copy)]
    enum MockKillBehavior {
        Immediate,
        Sleep(Duration),
        Error(&'static str),
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

    struct GatedKillPtySession {
        process_id: u32,
        kill_entered: Mutex<Option<mpsc::SyncSender<()>>>,
        kill_release: Mutex<mpsc::Receiver<()>>,
    }

    struct FirstKillFailsPtySession {
        process_id: u32,
        kill_count: Arc<AtomicUsize>,
    }

    struct RecordingPtySession {
        process_id: u32,
        inputs: Arc<Mutex<Vec<String>>>,
    }

    struct FirstWriteSignalPtySession {
        process_id: u32,
        inputs: Arc<Mutex<Vec<String>>>,
        write_times: Arc<Mutex<Vec<Instant>>>,
        first_write: Mutex<Option<mpsc::SyncSender<()>>>,
    }

    struct FirstWriteSignalFixture {
        pty: Box<dyn PtySessionTrait>,
        inputs: Arc<Mutex<Vec<String>>>,
        write_times: Arc<Mutex<Vec<Instant>>>,
    }

    struct FirstWriteBlockingPtySession {
        process_id: u32,
        inputs: Arc<Mutex<Vec<String>>>,
        calls: Arc<AtomicUsize>,
        first_entered: mpsc::SyncSender<()>,
        release: Arc<(Mutex<bool>, Condvar)>,
    }

    struct TimedOutWritePtySession {
        process_id: u32,
        entered: Mutex<Option<mpsc::SyncSender<()>>>,
        release: Arc<(Mutex<bool>, Condvar)>,
        writes_started: Arc<AtomicUsize>,
        writes_finished: Arc<AtomicUsize>,
        inputs: Arc<Mutex<Vec<String>>>,
        cancel_supported: bool,
        cancel_count: Arc<AtomicUsize>,
        kill_count: Arc<AtomicUsize>,
        first_write_error_bytes: usize,
    }

    struct DelayedCancelPtySession {
        process_id: u32,
        calls: AtomicUsize,
        inputs: Arc<Mutex<Vec<String>>>,
        first_entered: mpsc::SyncSender<()>,
        first_release: Arc<(Mutex<bool>, Condvar)>,
        second_entered: Mutex<Option<mpsc::SyncSender<()>>>,
        cancel_entered: Mutex<Option<mpsc::SyncSender<()>>>,
        cancel_release: Arc<(Mutex<bool>, Condvar)>,
    }

    impl PtySessionTrait for TimedOutWritePtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            let call = self.writes_started.fetch_add(1, Ordering::SeqCst);
            self.inputs.lock().push(input.to_string());
            if call > 0 {
                self.writes_finished.fetch_add(1, Ordering::SeqCst);
                return Ok(input.len());
            }
            if let Some(entered) = self.entered.lock().take() {
                let _ = entered.send(());
            }
            let (released, ready) = &*self.release;
            let mut released = released.lock();
            while !*released {
                ready.wait(&mut released);
            }
            self.writes_finished.fetch_add(1, Ordering::SeqCst);
            Err(PtyWriteError::new(
                self.first_write_error_bytes,
                "timed-out PTY write interrupted",
            ))
        }

        fn cancel_input_write(&self) -> Result<()> {
            self.cancel_count.fetch_add(1, Ordering::SeqCst);
            if !self.cancel_supported {
                return Err(anyhow!("isolated cancellation unsupported by test PTY"));
            }
            let (released, ready) = &*self.release;
            *released.lock() = true;
            ready.notify_all();
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            self.kill_count.fetch_add(1, Ordering::SeqCst);
            let (released, ready) = &*self.release;
            *released.lock() = true;
            ready.notify_all();
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    impl PtySessionTrait for DelayedCancelPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.inputs.lock().push(input.to_string());
            if call == 0 {
                self.first_entered.send(()).unwrap();
                let (released, ready) = &*self.first_release;
                let mut released = released.lock();
                while !*released {
                    ready.wait(&mut released);
                }
            } else if let Some(entered) = self.second_entered.lock().take() {
                entered.send(()).unwrap();
            }
            Ok(input.len())
        }

        fn cancel_input_write(&self) -> Result<()> {
            if let Some(entered) = self.cancel_entered.lock().take() {
                entered.send(()).unwrap();
            }
            let (released, ready) = &*self.cancel_release;
            let mut released = released.lock();
            while !*released {
                ready.wait(&mut released);
            }
            Ok(())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            for release in [&self.first_release, &self.cancel_release] {
                let (released, ready) = &**release;
                *released.lock() = true;
                ready.notify_all();
            }
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    impl PtySessionTrait for FirstWriteBlockingPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            self.inputs.lock().push(input.to_string());
            if call == 0 {
                self.first_entered.send(()).unwrap();
                let (released, ready) = &*self.release;
                let mut released = released.lock();
                while !*released {
                    ready.wait(&mut released);
                }
            }
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            let (released, ready) = &*self.release;
            *released.lock() = true;
            ready.notify_all();
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    impl PtySessionTrait for RecordingPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            self.inputs.lock().push(input.to_string());
            Ok(input.len())
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
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    impl PtySessionTrait for FirstWriteSignalPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            self.inputs.lock().push(input.to_string());
            self.write_times.lock().push(Instant::now());
            if let Some(first_write) = self.first_write.lock().take() {
                first_write.send(()).unwrap();
            }
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    fn recording_pty_session(
        process_id: u32,
    ) -> (Box<dyn PtySessionTrait>, Arc<Mutex<Vec<String>>>) {
        let inputs = Arc::new(Mutex::new(Vec::new()));
        (
            Box::new(RecordingPtySession {
                process_id,
                inputs: inputs.clone(),
            }),
            inputs,
        )
    }

    fn first_write_signal_pty_session(
        process_id: u32,
        first_write: mpsc::SyncSender<()>,
    ) -> FirstWriteSignalFixture {
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let write_times = Arc::new(Mutex::new(Vec::new()));
        FirstWriteSignalFixture {
            pty: Box::new(FirstWriteSignalPtySession {
                process_id,
                inputs: inputs.clone(),
                write_times: write_times.clone(),
                first_write: Mutex::new(Some(first_write)),
            }),
            inputs,
            write_times,
        }
    }

    struct TestPaneProcess {
        process_id: u32,
        alive: AtomicBool,
        member_process_ids: Mutex<HashSet<u32>>,
        failing_process_ids: Mutex<HashSet<u32>>,
        queried_process_ids: Mutex<Vec<u32>>,
    }

    impl TestPaneProcess {
        fn new(process_id: u32, member_process_ids: impl IntoIterator<Item = u32>) -> Arc<Self> {
            Arc::new(Self {
                process_id,
                alive: AtomicBool::new(true),
                member_process_ids: Mutex::new(member_process_ids.into_iter().collect()),
                failing_process_ids: Mutex::new(HashSet::new()),
                queried_process_ids: Mutex::new(Vec::new()),
            })
        }

        fn fail_membership_for(&self, process_id: u32) {
            self.failing_process_ids.lock().insert(process_id);
        }

        fn queried_process_ids(&self) -> Vec<u32> {
            let mut queried = self.queried_process_ids.lock().clone();
            queried.sort_unstable();
            queried
        }

        fn exit(&self) {
            self.alive.store(false, Ordering::SeqCst);
        }
    }

    impl PaneProcess for TestPaneProcess {
        fn pid(&self) -> u32 {
            self.process_id
        }

        fn is_alive(&self) -> Result<bool> {
            Ok(self.alive.load(Ordering::SeqCst))
        }

        fn belongs_to(&self, pty: &dyn PtySession) -> Result<bool> {
            let process_id = pty
                .process_id()
                .ok_or_else(|| anyhow!("test PTY has no process identity"))?;
            self.queried_process_ids.lock().push(process_id);
            if self.failing_process_ids.lock().contains(&process_id) {
                return Err(anyhow!("injected membership query failure"));
            }
            Ok(self.member_process_ids.lock().contains(&process_id))
        }
    }

    impl PtySessionTrait for MockPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
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
            match self.kill_behavior {
                MockKillBehavior::Immediate => Ok(()),
                MockKillBehavior::Sleep(duration) => {
                    thread::sleep(duration);
                    Ok(())
                }
                MockKillBehavior::Error(message) => Err(anyhow!(message)),
            }
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

    impl PtySessionTrait for GatedKillPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            if let Some(entered) = self.kill_entered.lock().take() {
                entered.send(()).context("failed to announce gated kill")?;
            }
            self.kill_release
                .lock()
                .recv()
                .context("gated kill was not released")?;
            Ok(())
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    impl PtySessionTrait for FirstKillFailsPtySession {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            if self.kill_count.fetch_add(1, Ordering::SeqCst) == 0 {
                Err(anyhow!("injected first process-scope termination failure"))
            } else {
                Ok(())
            }
        }

        fn try_wait(&self) -> Result<Option<PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn agent_alive(&self, _driver: DriverKind) -> Result<AgentLiveness> {
            Ok(AgentLiveness::Alive(Vec::new()))
        }
    }

    struct FailingPtySession {
        send_input_count: Arc<AtomicUsize>,
        bytes_written: usize,
        error_message: String,
    }

    struct ShortOkPtySession {
        bytes_written: usize,
    }

    impl PtySessionTrait for ShortOkPtySession {
        fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
            Ok(self.bytes_written)
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

    impl PtySessionTrait for FailingPtySession {
        fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
            self.send_input_count.fetch_add(1, Ordering::SeqCst);
            Err(PtyWriteError::new(
                self.bytes_written,
                self.error_message.clone(),
            ))
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

    fn failing_pty_session(error_message: &str) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>) {
        write_failing_pty_session(0, error_message)
    }

    fn write_failing_pty_session(
        bytes_written: usize,
        error_message: &str,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>) {
        let send_input_count = Arc::new(AtomicUsize::new(0));
        (
            Box::new(FailingPtySession {
                send_input_count: send_input_count.clone(),
                bytes_written,
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
            _plan: &PreparedLaunch,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.sessions
                .lock()
                .pop_front()
                .ok_or_else(|| anyhow!("no queued PTY sessions"))
        }
    }

    struct OutputThenFailPtySpawner;

    impl PtySpawner for OutputThenFailPtySpawner {
        fn spawn(
            &self,
            _plan: &PreparedLaunch,
            handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            handler(PtyEvent::Output("Working before admission".into()));
            Err(anyhow!("injected post-output spawn validation failure"))
        }
    }

    struct TestExecutableResolver;

    impl DriverExecutableResolver for TestExecutableResolver {
        fn resolve(&self, _driver: DriverKind) -> Result<ResolvedLaunchProgram> {
            Ok(ResolvedLaunchProgram {
                program: fs::canonicalize(std::env::current_exe()?)?
                    .to_string_lossy()
                    .into_owned(),
                prefix_args: Vec::new(),
            })
        }
    }

    struct FailingExecutableResolver;

    impl DriverExecutableResolver for FailingExecutableResolver {
        fn resolve(&self, driver: DriverKind) -> Result<ResolvedLaunchProgram> {
            Err(anyhow!("injected unresolved {driver:?} executable"))
        }
    }

    struct FixedExecutableResolver(ResolvedLaunchProgram);

    impl DriverExecutableResolver for FixedExecutableResolver {
        fn resolve(&self, _driver: DriverKind) -> Result<ResolvedLaunchProgram> {
            Ok(self.0.clone())
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
            plan: &PreparedLaunch,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.specs.lock().push(plan.spec.clone());
            self.sessions
                .lock()
                .pop_front()
                .ok_or_else(|| anyhow!("no queued PTY sessions"))
        }
    }

    struct GatedPtySpawner {
        session: Mutex<Option<Box<dyn PtySessionTrait>>>,
        entered: mpsc::SyncSender<()>,
        release: Mutex<mpsc::Receiver<()>>,
    }

    impl PtySpawner for GatedPtySpawner {
        fn spawn(
            &self,
            _plan: &PreparedLaunch,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.entered
                .send(())
                .context("failed to announce gated spawn")?;
            self.release
                .lock()
                .recv()
                .context("gated spawn was not released")?;
            self.session
                .lock()
                .take()
                .ok_or_else(|| anyhow!("gated PTY session already consumed"))
        }
    }

    fn gated_pty_spawner(
        session: Box<dyn PtySessionTrait>,
    ) -> (GatedPtySpawner, mpsc::Receiver<()>, mpsc::SyncSender<()>) {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        (
            GatedPtySpawner {
                session: Mutex::new(Some(session)),
                entered: entered_tx,
                release: Mutex::new(release_rx),
            },
            entered_rx,
            release_tx,
        )
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

    fn test_supervisor() -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!("cli-master-wrapper-test-{}", Uuid::new_v4()));
        test_supervisor_with_root(root)
    }

    fn empty_test_supervisor() -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!("prim1-empty-supervisor-{}", Uuid::new_v4()));
        empty_test_supervisor_with_root(root)
    }

    fn test_session_id(supervisor: &SupervisorHandle, name: &str) -> SessionId {
        supervisor
            .inner
            .slots
            .lock()
            .get(name)
            .unwrap_or_else(|| panic!("missing test session '{name}'"))
            .session_id
    }

    fn test_session_alias(supervisor: &SupervisorHandle, label: &str) -> String {
        supervisor
            .inner
            .slots
            .lock()
            .get(label)
            .unwrap_or_else(|| panic!("missing test session '{label}'"))
            .definition
            .alias
            .clone()
    }

    fn start_test_session(
        supervisor: &SupervisorHandle,
        unique_label: &str,
    ) -> Result<SessionSnapshot> {
        supervisor.start_session_by_id(test_session_id(supervisor, unique_label))
    }

    fn stop_test_session(
        supervisor: &SupervisorHandle,
        unique_label: &str,
    ) -> Result<SessionSnapshot> {
        supervisor.stop_session_by_id(test_session_id(supervisor, unique_label))
    }

    fn restart_test_session(
        supervisor: &SupervisorHandle,
        unique_label: &str,
    ) -> Result<SessionSnapshot> {
        supervisor.restart_session_by_id(test_session_id(supervisor, unique_label))
    }

    fn test_supervisor_with_root(root: PathBuf) -> SupervisorHandle {
        let supervisor = empty_test_supervisor_with_root(root);
        install_standard_test_sessions(&supervisor);
        supervisor
    }

    fn empty_test_supervisor_with_root(root: PathBuf) -> SupervisorHandle {
        let working_root = root.join("work");
        fs::create_dir_all(&working_root).expect("create test working root");
        let supervisor = SupervisorHandle::new(test_supervisor_config(&root))
            .expect("create empty test supervisor");
        supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
        supervisor
    }

    fn test_supervisor_config(root: &Path) -> SupervisorConfig {
        SupervisorConfig {
            working_root: root.join("work"),
            runtime_dir: root.join("runtime"),
            pane_mcp_executable: None,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
        }
    }

    fn install_standard_test_sessions(supervisor: &SupervisorHandle) {
        assert!(
            supervisor.snapshot().sessions.is_empty(),
            "the standard test fixture requires a fresh zero-session catalog"
        );
        supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("claude".into()),
                driver: DriverKind::Claude,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: None,
            })
            .expect("create Claude test session");
        supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("codex".into()),
                driver: DriverKind::Codex,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: None,
            })
            .expect("create Codex test session");
    }

    fn create_test_session(
        supervisor: &SupervisorHandle,
        label: &str,
        driver: DriverKind,
        permission_profile: shared_types::PermissionProfile,
    ) -> SessionSnapshot {
        supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some(label.into()),
                driver,
                permission_profile,
                linux_working_directory: None,
            })
            .expect("create test session")
    }

    fn route_operator_to_test_session(
        supervisor: &SupervisorHandle,
        label: &str,
        content: impl Into<String>,
    ) -> Result<RuntimeSnapshot> {
        supervisor.route_operator_message(OperatorRouteMessageRequest {
            recipient_id: test_session_id(supervisor, label),
            content: content.into(),
        })
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
        let working_root = root.join("work");
        fs::create_dir_all(&working_root).expect("create defaults test working root");
        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root,
            runtime_dir: root.join("runtime"),
            pane_mcp_executable: None,
            heartbeat_interval,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: auto_restart_threshold,
        })
        .unwrap();
        install_standard_test_sessions(&supervisor);
        supervisor.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
        let allowed = auto_restart_sessions
            .unwrap_or_default()
            .into_iter()
            .map(|name| test_session_id(&supervisor, &name))
            .collect();
        *supervisor
            .inner
            .auto_restart_on_stall
            .allowed_sessions
            .write() = allowed;
        supervisor
    }

    fn install_stale_running_session(supervisor: &SupervisorHandle, name: &str) {
        let (pty, _, _) = mock_pty_session(Some(u32::MAX), MockKillBehavior::Immediate);
        install_mock_running_session_with_process_id(
            supervisor,
            name,
            DriverKind::Codex,
            Some(u32::MAX),
            pty,
        );
    }

    fn install_synthetic_running_session(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
    ) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.definition.driver = driver;
        slot.running = Some(RunningSession::new(None));
        let run_id = Uuid::new_v4();
        set_test_bracketed_paste_mode(slot, run_id, BracketedPasteMode::Unknown);
        slot.run_id = Some(run_id);
        slot.last_run_id = Some(run_id);
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
        install_mock_running_session_with_process_id_and_mode(
            supervisor,
            name,
            driver,
            process_id,
            pty,
            BracketedPasteMode::Unknown,
        );
    }

    fn install_mock_running_session_with_bracketed_paste_enabled(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
        pty: Box<dyn PtySessionTrait>,
    ) {
        install_mock_running_session_with_process_id_and_bracketed_paste_enabled(
            supervisor, name, driver, None, pty,
        );
    }

    fn install_mock_running_session_with_process_id_and_bracketed_paste_enabled(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
        process_id: Option<u32>,
        pty: Box<dyn PtySessionTrait>,
    ) {
        install_mock_running_session_with_process_id_and_mode(
            supervisor,
            name,
            driver,
            process_id,
            pty,
            BracketedPasteMode::Enabled,
        );
    }

    fn install_mock_running_session_with_process_id_and_mode(
        supervisor: &SupervisorHandle,
        name: &str,
        driver: DriverKind,
        process_id: Option<u32>,
        pty: Box<dyn PtySessionTrait>,
        bracketed_paste_mode: BracketedPasteMode,
    ) {
        let session_id = test_session_id(supervisor, name);
        install_mock_running_session_by_id_with_mode(
            supervisor,
            session_id,
            driver,
            process_id,
            pty,
            bracketed_paste_mode,
        );
    }

    fn install_mock_running_session_by_id_with_mode(
        supervisor: &SupervisorHandle,
        session_id: SessionId,
        driver: DriverKind,
        process_id: Option<u32>,
        pty: Box<dyn PtySessionTrait>,
        bracketed_paste_mode: BracketedPasteMode,
    ) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(session_id).unwrap();
        slot.definition.driver = driver;
        slot.running = Some(RunningSession::new(Some(Arc::from(pty))));
        let run_id = Uuid::new_v4();
        set_test_bracketed_paste_mode(slot, run_id, bracketed_paste_mode);
        slot.run_id = Some(run_id);
        slot.last_run_id = Some(run_id);
        slot.process_id = process_id;
        slot.state = LifecycleState::Busy;
        slot.last_real_output_at = None;
    }

    fn set_test_bracketed_paste_mode(
        slot: &mut SessionSlot,
        run_id: Uuid,
        mode: BracketedPasteMode,
    ) {
        let binding = RunBinding {
            session_id: slot.session_id,
            run_id,
            generation: slot.generation,
        };
        slot.bracketed_paste.begin_run(binding);
        let control = match mode {
            BracketedPasteMode::Unknown => return,
            BracketedPasteMode::Enabled => "\x1b[?2004h",
            BracketedPasteMode::Disabled => "\x1b[?2004l",
        };
        slot.bracketed_paste.observe_output(binding, control);
    }

    fn current_run_id(supervisor: &SupervisorHandle, name: &str) -> Uuid {
        supervisor
            .inner
            .slots
            .lock()
            .get(name)
            .and_then(|slot| slot.run_id)
            .expect("test session must have an active run identity")
    }

    fn handle_current_pty_event(
        supervisor: &SupervisorHandle,
        name: &str,
        generation: SessionGeneration,
        event: PtyEvent,
    ) {
        supervisor.handle_pty_event(name, generation, current_run_id(supervisor, name), event);
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

    fn arm_test_quiesce_timer(supervisor: &SupervisorHandle, name: &str) {
        let generation = supervisor
            .current_generation_for_tests(test_session_id(supervisor, name))
            .unwrap();
        let run_id = current_run_id(supervisor, name);
        let handle = supervisor
            .inner
            .background_runtime
            .spawn(std::future::pending::<()>());
        let mut slots = supervisor.inner.slots.lock();
        slots.get_mut(name).unwrap().quiesce_timer = Some(QuiesceTimer {
            generation,
            run_id,
            handle,
        });
    }

    #[derive(Debug, PartialEq, Eq)]
    struct SlotMutationProbe {
        session_id: Uuid,
        definition: SessionDefinition,
        state: LifecycleState,
        work_state: WorkState,
        work_state_observed: bool,
        work_detail: Option<String>,
        work_error_observations: HashMap<String, Vec<Instant>>,
        running: bool,
        running_has_pty: bool,
        run_id: Option<Uuid>,
        last_run_id: Option<Uuid>,
        bracketed_paste: BracketedPasteRunState,
        run_event_sequence: u64,
        generation: SessionGeneration,
        spawn_in_flight: Option<SpawnReservation>,
        lifecycle_operation: Option<LifecycleOperation>,
        stop_intent: Option<StopIntent>,
        termination_uncertain: bool,
        process_id: Option<u32>,
        last_activity_at: Option<String>,
        last_real_output_at: Option<Instant>,
        last_route_from_session_at: Option<String>,
        last_route_from_session_instant: Option<Instant>,
        last_error: Option<String>,
        quiesce_timer: Option<(SessionGeneration, Uuid, bool)>,
        stall_state_entered_at: Option<Instant>,
        stall_state_entered_timestamp: Option<String>,
        stall_detector: Option<(SessionGeneration, Uuid, WorkState, Instant, bool)>,
    }

    fn slots_mutation_probe(supervisor: &SupervisorHandle) -> Vec<SlotMutationProbe> {
        let slots = supervisor.inner.slots.lock();
        let mut probes = slots
            .values()
            .map(|slot| SlotMutationProbe {
                session_id: slot.session_id,
                definition: slot.definition.clone(),
                state: slot.state,
                work_state: slot.work_state,
                work_state_observed: slot.work_state_observed,
                work_detail: slot.work_detail.clone(),
                work_error_observations: slot.work_error_observations.clone(),
                running: slot.running.is_some(),
                running_has_pty: slot
                    .running
                    .as_ref()
                    .and_then(|running| running.pty.as_ref())
                    .is_some(),
                run_id: slot.run_id,
                last_run_id: slot.last_run_id,
                bracketed_paste: slot.bracketed_paste,
                run_event_sequence: slot.run_event_sequence,
                generation: slot.generation,
                spawn_in_flight: slot.spawn_in_flight,
                lifecycle_operation: slot.lifecycle_operation,
                stop_intent: slot.stop_intent,
                termination_uncertain: slot.termination_uncertain,
                process_id: slot.process_id,
                last_activity_at: slot.last_activity_at.clone(),
                last_real_output_at: slot.last_real_output_at,
                last_route_from_session_at: slot.last_route_from_session_at.clone(),
                last_route_from_session_instant: slot.last_route_from_session_instant,
                last_error: slot.last_error.clone(),
                quiesce_timer: slot
                    .quiesce_timer
                    .as_ref()
                    .map(|timer| (timer.generation, timer.run_id, timer.handle.is_finished())),
                stall_state_entered_at: slot.stall_state_entered_at,
                stall_state_entered_timestamp: slot.stall_state_entered_timestamp.clone(),
                stall_detector: slot.stall_detector.as_ref().map(|detector| {
                    (
                        detector.generation,
                        detector.run_id,
                        detector.state,
                        detector.entered_at,
                        detector.handle.is_finished(),
                    )
                }),
            })
            .collect::<Vec<_>>();
        probes.sort_by(|left, right| left.definition.alias.cmp(&right.definition.alias));
        probes
    }

    fn slot_mutation_probe(supervisor: &SupervisorHandle, name: &str) -> SlotMutationProbe {
        slots_mutation_probe(supervisor)
            .into_iter()
            .find(|probe| {
                probe.definition.alias == name || probe.definition.label.eq_ignore_ascii_case(name)
            })
            .unwrap_or_else(|| panic!("missing mutation probe for session '{name}'"))
    }

    fn runtime_file_manifest(supervisor: &SupervisorHandle) -> Vec<(String, Vec<u8>)> {
        fn collect(root: &Path, current: &Path, files: &mut Vec<(String, Vec<u8>)>) {
            let mut entries = fs::read_dir(current)
                .unwrap()
                .map(|entry| entry.unwrap())
                .collect::<Vec<_>>();
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let path = entry.path();
                let file_type = entry.file_type().unwrap();
                if file_type.is_dir() {
                    collect(root, &path, files);
                } else if file_type.is_file() {
                    files.push((
                        path.strip_prefix(root)
                            .unwrap()
                            .to_string_lossy()
                            .replace('\\', "/"),
                        fs::read(path).unwrap(),
                    ));
                }
            }
        }

        let mut files = Vec::new();
        collect(
            supervisor.runtime_dir(),
            supervisor.runtime_dir(),
            &mut files,
        );
        files
    }

    fn make_test_event(label: &str) -> RuntimeEvent {
        RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: label.into(),
            timestamp: "2026-04-20T00:00:00Z".into(),
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
    fn durable_audit_omits_terminal_and_message_content_but_keeps_route_metadata() {
        let supervisor = test_supervisor();
        let audit_baseline = fs::read_to_string(supervisor.audit_log_path())
            .unwrap_or_default()
            .lines()
            .count();
        let session_id = Uuid::new_v4();
        let run_id = Uuid::new_v4();
        let output_sentinel = "PRIM1_OUTPUT_SECRET_7f44d1";
        let route_sentinel = "PRIM1_ROUTE_SECRET_3bc29a";
        let work_sentinel = "PRIM1_WORK_SECRET_ee810c";
        let identity = |sequence| RunEventIdentity {
            session_id,
            run_id,
            generation: 7,
            sequence,
        };

        supervisor.emit(RuntimeEvent::SessionOutput {
            identity: identity(1),
            session: "claude".into(),
            chunk: output_sentinel.into(),
            synthetic: false,
            timestamp: now_rfc3339(),
        });
        let route_id = Uuid::new_v4();
        supervisor.emit(RuntimeEvent::RoutedMessage {
            id: route_id,
            from: "operator".into(),
            to: "claude".into(),
            scope: MessageScope::Direct,
            content: route_sentinel.into(),
            timestamp: now_rfc3339(),
        });
        supervisor.emit(RuntimeEvent::SessionWorkState {
            identity: identity(2),
            session: "claude".into(),
            state: WorkState::Thinking,
            detail: Some(work_sentinel.into()),
            previous_state: Some(WorkState::Idle),
            timestamp: now_rfc3339(),
        });
        supervisor
            .shutdown()
            .expect("shutdown audit privacy fixture");

        let audit_text = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        let audit_events = audit_text
            .lines()
            .skip(audit_baseline)
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(audit_events.len(), 2, "SessionOutput must not be durable");
        let route = audit_events
            .iter()
            .find(|event| event["event"] == "routed_message")
            .expect("route metadata must remain durable");
        assert_eq!(route["id"], route_id.to_string());
        assert_eq!(route["from"], "operator");
        assert_eq!(route["to"], "claude");
        assert_eq!(route["content"], "[content omitted]");
        let work = audit_events
            .iter()
            .find(|event| event["event"] == "session_work_state")
            .expect("work-state metadata must remain durable");
        assert!(work.get("detail").is_none() || work["detail"].is_null());

        for (path, bytes) in runtime_file_manifest(&supervisor) {
            let content = String::from_utf8_lossy(&bytes);
            for sentinel in [output_sentinel, route_sentinel, work_sentinel] {
                assert!(
                    !content.contains(sentinel),
                    "durable runtime file {path} leaked sentinel {sentinel}"
                );
            }
        }
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
                session.label
            );
            assert_eq!(session.lifecycle_state, LifecycleState::Closed);
            assert_eq!(session.process_id, None);
        }
    }

    #[test]
    fn shutdown_invalidates_in_flight_spawn_and_rejects_future_starts() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (spawner, entered, release) = gated_pty_spawner(pty);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let start_supervisor = supervisor.clone();
        let start_thread = thread::spawn(move || start_test_session(&start_supervisor, "claude"));
        entered
            .recv_timeout(Duration::from_secs(2))
            .expect("start never reached the gated PTY spawner");

        let shutdown_error = supervisor
            .shutdown()
            .expect_err("shutdown must report the still in-flight spawn");
        assert!(
            shutdown_error.to_string().contains("spawn in flight"),
            "unexpected shutdown error: {shutdown_error:#}"
        );
        release.send(()).unwrap();

        let error = start_thread
            .join()
            .expect("start thread panicked")
            .unwrap_err();
        assert_eq!(error.to_string(), "supervisor has shut down");
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);

        let claude = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.label == "claude")
            .unwrap();
        assert!(!claude.running);
        assert_eq!(claude.lifecycle_state, LifecycleState::Closed);
        assert_eq!(claude.process_id, None);
        assert!(!events.lock().iter().any(|event| match event {
            RuntimeEvent::SessionState {
                state: LifecycleState::Ready,
                ..
            } => true,
            RuntimeEvent::SystemLog { message, .. } => message.starts_with("Started "),
            _ => false,
        }));

        let error = start_test_session(&supervisor, "claude").unwrap_err();
        assert_eq!(error.to_string(), "supervisor has shut down");
        supervisor.shutdown().unwrap();
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
    fn routed_payload_preserves_multiline_whitespace_for_harness_drivers() {
        let request = RouteMessageRequest {
            from: "operator".into(),
            to: "codex".into(),
            scope: MessageScope::Direct,
            content: "  tell  me\r\n\ta joke 👩‍💻  ".into(),
        };
        let expected = "[Direct message from operator]\n  tell  me\r\n\ta joke 👩‍💻  ";

        assert_eq!(
            routed_message_payload(&request, routed_message_submit_behavior(DriverKind::Claude)),
            expected
        );
        assert_eq!(
            routed_message_payload(&request, routed_message_submit_behavior(DriverKind::Codex)),
            expected
        );
        assert_eq!(
            routed_message_payload(&request, routed_message_submit_behavior(DriverKind::Grok)),
            expected
        );
    }

    #[test]
    fn generic_terminal_route_stays_single_line() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "operator".into(),
                to: "terminal".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            },
            routed_message_submit_behavior(DriverKind::GenericTerminal),
        );

        assert_eq!(payload, "[Direct message from operator] hello");
    }

    #[test]
    fn harness_message_frame_is_content_faithful_before_observed_submit() {
        let content = "  alpha\r\n\tbeta 👩‍💻  ";
        for driver in [DriverKind::Claude, DriverKind::Codex, DriverKind::Grok] {
            let behavior = routed_message_submit_behavior(driver);
            validate_message_body(content).unwrap();
            validate_message_framing(content, behavior).unwrap();
            assert_eq!(
                frame_message_payload(content, behavior.framing),
                format!("{BRACKETED_PASTE_START}{content}{BRACKETED_PASTE_END}")
            );
            assert_eq!(behavior.sequence, "\r");
        }
    }

    #[test]
    fn message_validation_is_bounded_and_fail_closed() {
        validate_message_body(&"x".repeat(MESSAGE_BODY_MAX_BYTES)).unwrap();

        let too_large = "x".repeat(MESSAGE_BODY_MAX_BYTES + 1);
        assert_eq!(
            validate_message_body(&too_large).unwrap_err().to_string(),
            format!(
                "message body is {} bytes; maximum is {} bytes",
                MESSAGE_BODY_MAX_BYTES + 1,
                MESSAGE_BODY_MAX_BYTES
            )
        );
        let multibyte_too_large = "é".repeat(MESSAGE_BODY_MAX_BYTES / 2 + 1);
        assert!(multibyte_too_large.chars().count() < MESSAGE_BODY_MAX_BYTES);
        assert_eq!(multibyte_too_large.len(), MESSAGE_BODY_MAX_BYTES + 2);
        assert!(
            validate_message_body(&multibyte_too_large)
                .unwrap_err()
                .to_string()
                .contains(&format!("maximum is {MESSAGE_BODY_MAX_BYTES} bytes"))
        );
        assert_eq!(
            validate_message_body("before\x1b[201~after")
                .unwrap_err()
                .to_string(),
            "message body contains disallowed control character U+001B at byte offset 6"
        );
        assert_eq!(
            validate_message_body("before\0after")
                .unwrap_err()
                .to_string(),
            "message body contains disallowed control character U+0000 at byte offset 6"
        );
        assert_eq!(
            validate_message_body("before\x03after")
                .unwrap_err()
                .to_string(),
            "message body contains disallowed control character U+0003 at byte offset 6"
        );
        assert_eq!(
            validate_message_body("before\x7fafter")
                .unwrap_err()
                .to_string(),
            "message body contains disallowed control character U+007F at byte offset 6"
        );
        assert_eq!(
            validate_message_body("before\u{009b}after")
                .unwrap_err()
                .to_string(),
            "message body contains disallowed control character U+009B at byte offset 6"
        );
    }

    #[test]
    fn generic_terminal_rejects_multiline_delivery() {
        let behavior = routed_message_submit_behavior(DriverKind::GenericTerminal);
        assert_eq!(
            validate_message_framing("first\nsecond", behavior)
                .unwrap_err()
                .to_string(),
            "generic terminal delivery supports only printable single-line message bodies"
        );
        assert_eq!(
            validate_message_framing("first\tsecond", behavior)
                .unwrap_err()
                .to_string(),
            "generic terminal delivery supports only printable single-line message bodies"
        );
        assert_eq!(
            validate_message_framing("first\u{009b}second", behavior)
                .unwrap_err()
                .to_string(),
            "generic terminal delivery supports only printable single-line message bodies"
        );
        assert_eq!(frame_message_payload("hello", behavior.framing), "hello");
        assert_eq!(behavior.sequence, "\r");
    }

    #[test]
    fn operator_route_writes_one_exact_frame_then_observed_submit() {
        let content = format!("  {}\r\n\tUnicode 👩‍💻 e\u{301} 終  ", "segment ".repeat(200));
        assert!(content.len() > 800);

        for (name, driver) in [("claude", DriverKind::Claude), ("codex", DriverKind::Codex)] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_bracketed_paste_enabled(
                &supervisor,
                name,
                driver,
                pty,
            );

            route_operator_to_test_session(&supervisor, name, content.clone()).unwrap();
            let expected_payload = format!("[Direct message from operator]\n{content}");

            assert_eq!(
                inputs.lock().as_slice(),
                &[
                    format!("{BRACKETED_PASTE_START}{expected_payload}{BRACKETED_PASTE_END}"),
                    "\r".to_string(),
                ],
                "{name} delivery did not preserve the paste/submit boundary"
            );
        }
    }

    #[test]
    fn bracketed_delivery_waits_for_compatibility_delay_and_keeps_submit_ahead_of_queued_input() {
        let supervisor = test_supervisor();
        let (first_write_tx, first_write_rx) = mpsc::sync_channel(1);
        let FirstWriteSignalFixture {
            pty,
            inputs,
            write_times,
        } = first_write_signal_pty_session(std::process::id(), first_write_tx);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            pty,
            BracketedPasteMode::Enabled,
        );

        let delivery_supervisor = supervisor.clone();
        let delivery = thread::spawn(move || {
            route_operator_to_test_session(&delivery_supervisor, "claude", "paste before submit")
        });
        first_write_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("paste frame did not reach the PTY");
        assert_eq!(
            inputs.lock().as_slice(),
            &[format!(
                "{BRACKETED_PASTE_START}[Direct message from operator]\npaste before submit{BRACKETED_PASTE_END}"
            )]
        );

        let queued_session_id = test_session_id(&supervisor, "claude");
        let queued_supervisor = supervisor.clone();
        let queued = thread::spawn(move || {
            queued_supervisor.send_input(SendInputRequest {
                session_id: queued_session_id,
                input: "queued raw input".into(),
            })
        });
        let input_gate = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .running
            .as_ref()
            .unwrap()
            .input_gate
            .clone();
        let queued_deadline = Instant::now() + Duration::from_secs(1);
        while input_gate.queued_writes() < 1 && Instant::now() < queued_deadline {
            thread::yield_now();
        }
        assert_eq!(input_gate.queued_writes(), 1);
        thread::sleep(BRACKETED_PASTE_SUBMIT_DELAY / 2);
        assert_eq!(
            inputs.lock().len(),
            1,
            "submit ran before the compatibility delay elapsed"
        );
        delivery.join().unwrap().unwrap();
        queued.join().unwrap().unwrap();

        assert_eq!(
            inputs.lock().as_slice(),
            &[
                format!(
                    "{BRACKETED_PASTE_START}[Direct message from operator]\npaste before submit{BRACKETED_PASTE_END}"
                ),
                "\r".to_string(),
                "queued raw input".to_string(),
            ]
        );
        let write_times = write_times.lock();
        assert!(
            write_times[1].duration_since(write_times[0]) >= BRACKETED_PASTE_SUBMIT_DELAY,
            "submit was not delayed for the measured compatibility interval"
        );
    }

    #[test]
    fn bracketed_delivery_submits_after_terminal_disables_paste_mode() {
        let supervisor = test_supervisor();
        let (first_write_tx, first_write_rx) = mpsc::sync_channel(1);
        let FirstWriteSignalFixture {
            pty,
            inputs,
            write_times,
        } = first_write_signal_pty_session(std::process::id(), first_write_tx);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
            BracketedPasteMode::Enabled,
        );

        let delivery_supervisor = supervisor.clone();
        let delivery = thread::spawn(move || {
            route_operator_to_test_session(
                &delivery_supervisor,
                "codex",
                "submit after completed paste",
            )
        });
        first_write_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("paste frame did not reach the PTY");
        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("\x1b[?2004l".into()),
        );

        delivery.join().unwrap().unwrap();
        assert_eq!(
            inputs.lock().as_slice(),
            &[
                format!(
                    "{BRACKETED_PASTE_START}[Direct message from operator]\nsubmit after completed paste{BRACKETED_PASTE_END}"
                ),
                "\r".to_string(),
            ],
            "a terminal may legitimately disable paste mode after consuming the completed frame"
        );
        let write_times = write_times.lock();
        assert!(
            write_times[1].duration_since(write_times[0]) >= BRACKETED_PASTE_SUBMIT_DELAY,
            "Codex submit ran before its measured compatibility interval elapsed"
        );
    }

    #[test]
    fn bracketed_delivery_revalidates_unsafe_work_state_before_submit() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (first_write_tx, first_write_rx) = mpsc::sync_channel(1);
        let FirstWriteSignalFixture {
            pty,
            inputs,
            write_times: _write_times,
        } = first_write_signal_pty_session(std::process::id(), first_write_tx);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
            BracketedPasteMode::Enabled,
        );

        let delivery_supervisor = supervisor.clone();
        let delivery = thread::spawn(move || {
            route_operator_to_test_session(
                &delivery_supervisor,
                "codex",
                "do not submit into a blocked prompt",
            )
        });
        first_write_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("paste frame did not reach the PTY");
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.work_state = WorkState::Blocked;
            slot.work_state_observed = true;
            slot.work_detail = Some("trust_prompt".into());
        }

        let error = delivery.join().unwrap().unwrap_err();
        assert!(
            error.to_string().contains("work state is blocked"),
            "{error:#}"
        );
        assert_eq!(
            inputs.lock().len(),
            1,
            "submit reached a driver-observed blocked prompt"
        );
        let committed = inputs.lock()[0].len();
        assert!(events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                bytes_written,
                ..
            } if *bytes_written == committed
        )));
    }

    #[test]
    fn routed_bracketed_submission_never_enters_a_replacement_run_after_paste() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (first_write_tx, first_write_rx) = mpsc::sync_channel(1);
        let FirstWriteSignalFixture {
            pty,
            inputs: original_inputs,
            write_times: _write_times,
        } = first_write_signal_pty_session(std::process::id(), first_write_tx);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            pty,
            BracketedPasteMode::Enabled,
        );

        let route_supervisor = supervisor.clone();
        let route = thread::spawn(move || {
            route_operator_to_test_session(
                &route_supervisor,
                "claude",
                "must not submit into a replacement run",
            )
        });
        first_write_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("paste frame did not reach the original PTY");

        let (replacement_pty, replacement_inputs) = recording_pty_session(std::process::id());
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("claude").unwrap();
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement_pty))));
            let replacement_run_id = Uuid::new_v4();
            set_test_bracketed_paste_mode(slot, replacement_run_id, BracketedPasteMode::Enabled);
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
        }

        let error = route.join().unwrap().unwrap_err();
        assert!(
            error
                .to_string()
                .contains("run identity changed before input"),
            "{error:#}"
        );
        assert_eq!(
            original_inputs.lock().len(),
            1,
            "Enter reached the retired run"
        );
        assert!(replacement_inputs.lock().is_empty());
        let paste_bytes = original_inputs.lock()[0].len();
        let deliveries = route_delivery_events(&events);
        assert!(deliveries.iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                bytes_written,
                error: Some(error),
                ..
            } if *bytes_written == paste_bytes
                && error.contains("run identity changed before input")
        )));
        assert!(!deliveries.iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Written,
                ..
            }
        )));
    }

    #[test]
    fn oversized_delivery_writes_nothing() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            pty,
        );

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: test_session_id(&supervisor, "codex"),
                content: "x".repeat(MESSAGE_BODY_MAX_BYTES + 1),
            })
            .unwrap_err();

        assert!(error.to_string().contains("maximum is 1048576 bytes"));
        assert!(inputs.lock().is_empty());
    }

    #[test]
    fn one_mib_delivery_is_one_exact_paste_then_submit_for_each_harness_driver() {
        let prefix = "  \r\n\tUnicode 👩‍💻 e\u{301} 終  ";
        let content = format!(
            "{prefix}{}",
            "x".repeat(MESSAGE_BODY_MAX_BYTES - prefix.len())
        );
        assert_eq!(content.len(), MESSAGE_BODY_MAX_BYTES);

        for (name, driver) in [("claude", DriverKind::Claude), ("codex", DriverKind::Codex)] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_bracketed_paste_enabled(
                &supervisor,
                name,
                driver,
                pty,
            );

            route_operator_to_test_session(&supervisor, name, content.clone()).unwrap();

            let inputs = inputs.lock();
            assert_eq!(inputs.len(), 2);
            let written = &inputs[0];
            assert!(written.starts_with(BRACKETED_PASTE_START));
            assert!(written.ends_with(BRACKETED_PASTE_END));
            let expected_payload = format!("[Direct message from operator]\n{content}");
            assert_eq!(
                &written[BRACKETED_PASTE_START.len()..written.len() - BRACKETED_PASTE_END.len()],
                expected_payload
            );
            assert_eq!(inputs[1], "\r");
        }
    }

    #[test]
    fn operator_route_writes_exact_provenance_paste_then_submit_at_required_sizes() {
        let prefix = "  \r\n\tUnicode 👩‍💻 e\u{301} 終  ";
        for size in [1024, 64 * 1024, MESSAGE_BODY_MAX_BYTES] {
            let content = format!("{prefix}{}", "x".repeat(size - prefix.len()));
            assert_eq!(content.len(), size);

            for driver in [DriverKind::Claude, DriverKind::Codex] {
                let supervisor = test_supervisor();
                let (pty, inputs) = recording_pty_session(std::process::id());
                install_mock_running_session_with_bracketed_paste_enabled(
                    &supervisor,
                    "codex",
                    driver,
                    pty,
                );
                let session_id = test_session_id(&supervisor, "codex");

                supervisor
                    .route_operator_message(OperatorRouteMessageRequest {
                        recipient_id: session_id,
                        content: content.clone(),
                    })
                    .unwrap();

                let expected_payload = format!("[Direct message from operator]\n{content}");
                assert_eq!(
                    inputs.lock().as_slice(),
                    &[
                        format!("{BRACKETED_PASTE_START}{expected_payload}{BRACKETED_PASTE_END}"),
                        "\r".to_string(),
                    ],
                    "{driver:?} route of {size} source bytes did not preserve the paste/submit boundary"
                );
            }
        }
    }

    #[test]
    fn generic_operator_route_is_one_printable_single_line_write() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session(&supervisor, "codex", DriverKind::GenericTerminal, pty);
        let session_id = test_session_id(&supervisor, "codex");

        supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: session_id,
                content: "printable only".into(),
            })
            .unwrap();

        assert_eq!(
            inputs.lock().as_slice(),
            &["[Direct message from operator] printable only\r".to_string()]
        );
    }

    #[test]
    fn operator_route_rejects_oversize_and_control_bodies_before_writing() {
        for content in [
            "x".repeat(MESSAGE_BODY_MAX_BYTES + 1),
            "before\u{1b}[201~after".to_string(),
            "before\0after".to_string(),
            "before\u{009b}after".to_string(),
        ] {
            let supervisor = test_supervisor();
            let events = capture_runtime_events(&supervisor);
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_bracketed_paste_enabled(
                &supervisor,
                "codex",
                DriverKind::Codex,
                pty,
            );
            let session_id = test_session_id(&supervisor, "codex");

            supervisor
                .route_operator_message(OperatorRouteMessageRequest {
                    recipient_id: session_id,
                    content,
                })
                .unwrap_err();

            assert!(inputs.lock().is_empty());
            assert!(events.lock().is_empty());
        }
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
        let bracketed = SubmitBehavior {
            sequence: "\r",
            framing: MessageFraming::BracketedPaste,
            submit_delay: BRACKETED_PASTE_SUBMIT_DELAY,
        };
        assert_eq!(
            routed_message_submit_behavior(DriverKind::Claude),
            bracketed
        );
        assert_eq!(routed_message_submit_behavior(DriverKind::Codex), bracketed);
        assert_eq!(routed_message_submit_behavior(DriverKind::Grok), bracketed);
        assert_eq!(
            routed_message_submit_behavior(DriverKind::GenericTerminal),
            SubmitBehavior {
                sequence: "\r",
                framing: MessageFraming::RawSingleLine,
                submit_delay: Duration::ZERO,
            }
        );
    }

    #[test]
    fn idle_emitted_after_quiesce_threshold() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Output("Working".into()));

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            if events.lock().iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::SessionState { session, state, .. }
                        if session == &codex_alias && *state == LifecycleState::Idle
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
    fn grok_repaints_use_semantic_work_state_without_lifecycle_quiescence() {
        assert_eq!(quiesce_threshold(DriverKind::Grok), None);
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Grok);
        let grok_alias = test_session_alias(&supervisor, "codex");
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        for chunk in [
            "◆ Thinking…",
            "measured full-screen repaint",
            "Worked for 6.3s",
            "another measured full-screen repaint",
        ] {
            handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Output(chunk.into()));
        }

        assert!(
            !events.lock().iter().any(|event| matches!(
                event,
                RuntimeEvent::SessionState { session, state, .. }
                    if session == &grok_alias && *state == LifecycleState::Idle
            )),
            "Grok's variable repaint cadence must not produce lifecycle Idle"
        );
        assert_eq!(
            events
                .lock()
                .iter()
                .filter(|event| matches!(
                    event,
                    RuntimeEvent::SessionState { session, state, .. }
                        if session == &grok_alias && *state == LifecycleState::Ready
                ))
                .count(),
            1,
            "only the initial Busy -> Ready lifecycle transition is valid"
        );
        let work_states = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SessionWorkState { session, state, .. } if session == &grok_alias => {
                    Some(*state)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(work_states, [WorkState::Thinking, WorkState::Idle]);
        assert!(
            supervisor
                .inner
                .slots
                .lock()
                .get("codex")
                .unwrap()
                .quiesce_timer
                .is_none(),
            "Grok output must never arm a lifecycle quiesce timer"
        );
    }

    #[test]
    fn idle_cancelled_on_new_output() {
        let supervisor = test_supervisor();
        install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Output("Working".into()));
        thread::sleep(Duration::from_millis(1200));
        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("Still working".into()),
        );
        thread::sleep(Duration::from_millis(1300));

        assert!(
            !events.lock().iter().any(|event| matches!(
                event,
                RuntimeEvent::SessionState { session, state, .. }
                    if session == &codex_alias && *state == LifecycleState::Idle
            )),
            "idle fired before the reset timer elapsed"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if events.lock().iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::SessionState { session, state, .. }
                        if session == &codex_alias && *state == LifecycleState::Idle
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
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = Arc::new(Mutex::new(Vec::<RuntimeEvent>::new()));
        let captured = events.clone();
        supervisor.set_event_sink(move |event| {
            captured.lock().push(event);
        });

        handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Output("Working".into()));
        let armed_at = {
            let slots = supervisor.inner.slots.lock();
            slots.get("codex").unwrap().last_real_output_at.unwrap()
        };
        let stale_run_id = current_run_id(&supervisor, "codex");
        supervisor
            .bump_session_generation_for_tests(test_session_id(&supervisor, "codex"))
            .unwrap();
        supervisor.fire_quiesce_timer(
            test_session_id(&supervisor, "codex"),
            0,
            stale_run_id,
            armed_at,
            Duration::from_secs(2),
        );
        thread::sleep(Duration::from_millis(50));

        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionState { session, state, .. }
                if session == &codex_alias && *state == LifecycleState::Idle
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
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("Working 12s".into()),
        );
        let armed_at = {
            let slots = supervisor.inner.slots.lock();
            slots.get("codex").unwrap().last_real_output_at.unwrap()
        };
        let stale_run_id = current_run_id(&supervisor, "codex");
        supervisor.fire_quiesce_timer(
            test_session_id(&supervisor, "codex"),
            0,
            stale_run_id,
            armed_at,
            Duration::from_secs(2),
        );

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
                assert_eq!(session, &codex_alias);
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
                assert_eq!(session, &codex_alias);
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
            handle_current_pty_event(
                &supervisor,
                "codex",
                0,
                PtyEvent::Output(format!("Working {idx}s")),
            );
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
            handle_current_pty_event(
                &supervisor,
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
        let codex_alias = test_session_alias(&supervisor, "codex");
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.process_id = Some(4242);
            slot.last_activity_at = Some("2026-05-18T00:00:00Z".into());
        }

        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SupervisorHeartbeat { sessions, .. }
                    if sessions.iter().any(|summary| {
                        summary.name == codex_alias
                            && summary.lifecycle_state == LifecycleState::Ready
                            && summary.work_state == Some(WorkState::Thinking)
                            && summary.process_id == Some(4242)
                            && summary.last_activity_at.as_deref()
                                == Some("2026-05-18T00:00:00Z")
                    })
            )
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
        assert!(heartbeats.iter().any(|(_, sessions)| {
            sessions.iter().any(|summary| {
                summary.name == codex_alias
                    && summary.lifecycle_state == LifecycleState::Ready
                    && summary.work_state == Some(WorkState::Thinking)
                    && summary.process_id == Some(4242)
                    && summary.last_activity_at.as_deref() == Some("2026-05-18T00:00:00Z")
            })
        }));
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

        handle_current_pty_event(&supervisor, "claude", 1, PtyEvent::Output("Working".into()));

        assert_eq!(
            supervisor.current_generation_for_tests(test_session_id(&supervisor, "claude")),
            Some(2)
        );
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
    fn run_event_sequence_exhaustion_rejects_before_pty_state_mutation() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
        );
        supervisor
            .inner
            .slots
            .lock()
            .get_mut("codex")
            .unwrap()
            .run_event_sequence = u64::MAX - 2;
        let events = capture_runtime_events(&supervisor);
        let slots_before = slots_mutation_probe(&supervisor);

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("real output".into()),
        );

        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert!(events.lock().is_empty());
        assert!(inputs.lock().is_empty());
    }

    #[test]
    fn run_event_publisher_restores_sequence_order_before_the_sink() {
        let supervisor = test_supervisor();
        let (session_id, run_id) = {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get("claude").unwrap();
            (slot.session_id, Uuid::new_v4())
        };
        let events = capture_runtime_events(&supervisor);
        let output_event = |sequence, chunk: &str| RuntimeEvent::SessionOutput {
            identity: RunEventIdentity {
                session_id,
                run_id,
                generation: 0,
                sequence,
            },
            session: "claude".into(),
            chunk: chunk.into(),
            synthetic: false,
            timestamp: now_rfc3339(),
        };

        supervisor.emit_run_event(output_event(2, "second"));
        assert!(
            events.lock().is_empty(),
            "a sequence gap must not publish a later event"
        );
        supervisor.emit_run_event(output_event(1, "first"));

        let observed = events
            .lock()
            .iter()
            .map(|event| match event {
                RuntimeEvent::SessionOutput {
                    identity, chunk, ..
                } => (identity.sequence, chunk.clone()),
                other => panic!("unexpected event: {other:?}"),
            })
            .collect::<Vec<_>>();
        assert_eq!(observed, vec![(1, "first".into()), (2, "second".into())]);
    }

    #[test]
    fn pty_event_revalidates_generation_at_branch_commit() {
        for event in [
            PtyEvent::Output("late output".into()),
            PtyEvent::Closed,
            PtyEvent::Error("late error".into()),
        ] {
            let supervisor = test_supervisor();
            let (old_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
            install_mock_running_session(&supervisor, "claude", DriverKind::Claude, old_pty);
            let old_run_id = current_run_id(&supervisor, "claude");
            let events = capture_runtime_events(&supervisor);
            let (hook_entered_tx, hook_entered_rx) = mpsc::sync_channel(1);
            let (hook_release_tx, hook_release_rx) = mpsc::sync_channel(1);
            supervisor.set_pty_event_before_commit_for_tests(move || {
                hook_entered_tx.send(()).unwrap();
                hook_release_rx.recv().unwrap();
            });

            let event_supervisor = supervisor.clone();
            let event_thread = thread::spawn(move || {
                event_supervisor.handle_pty_event("claude", 0, old_run_id, event)
            });
            hook_entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("PTY event never reached the branch-commit barrier");

            let (replacement, _) = recording_pty_session(std::process::id());
            {
                let mut slots = supervisor.inner.slots.lock();
                let slot = slots.get_mut("claude").unwrap();
                slot.generation = 1;
                slot.state = LifecycleState::Ready;
                slot.process_id = Some(std::process::id());
                slot.running = Some(RunningSession::new(Some(Arc::from(replacement))));
                let replacement_run_id = Uuid::new_v4();
                slot.run_id = Some(replacement_run_id);
                slot.last_run_id = Some(replacement_run_id);
            }
            let replacement_probe = slots_mutation_probe(&supervisor);
            hook_release_tx.send(()).unwrap();
            event_thread.join().unwrap();

            assert_eq!(slots_mutation_probe(&supervisor), replacement_probe);
            assert!(events.lock().is_empty());
        }
    }

    #[test]
    fn replaced_run_cannot_inherit_old_work_state_side_effects() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_secs(60)),
        );
        let (old_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, old_pty);
        let old_run_id = current_run_id(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);
        let (hook_entered_tx, hook_entered_rx) = mpsc::sync_channel(1);
        let (hook_release_tx, hook_release_rx) = mpsc::sync_channel(1);
        supervisor.set_work_state_before_side_effect_for_tests(move || {
            hook_entered_tx.send(()).unwrap();
            hook_release_rx.recv().unwrap();
        });

        let event_supervisor = supervisor.clone();
        let event_thread = thread::spawn(move || {
            event_supervisor.handle_pty_event(
                "codex",
                0,
                old_run_id,
                PtyEvent::Output("You've hit your usage limit.".into()),
            )
        });
        hook_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("work-state side effect barrier was not reached");

        let (replacement, _) = recording_pty_session(std::process::id());
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            cancel_quiesce_timer_locked(slot);
            if let Some(running) = slot.running.take() {
                running.close_input();
            }
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement))));
            let replacement_run_id = Uuid::new_v4();
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
            slot.state = LifecycleState::Busy;
            slot.process_id = Some(std::process::id());
            slot.last_activity_at = None;
            slot.last_real_output_at = None;
            slot.last_error = None;
            reset_work_state_locked(slot);
        }
        let replacement_probe = slots_mutation_probe(&supervisor);
        hook_release_tx.send(()).unwrap();
        event_thread.join().unwrap();

        assert_eq!(slots_mutation_probe(&supervisor), replacement_probe);
        let run_events = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::SessionState { identity, .. }
                | RuntimeEvent::SessionOutput { identity, .. }
                | RuntimeEvent::SessionWorkState { identity, .. } => Some(*identity),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(run_events.len(), 3);
        assert!(
            run_events
                .iter()
                .all(|identity| identity.run_id == old_run_id)
        );
        assert_eq!(
            run_events
                .iter()
                .map(|identity| identity.sequence)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn generation_exhaustion_does_not_close_or_mutate_the_live_run() {
        let supervisor = test_supervisor();
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        arm_test_quiesce_timer(&supervisor, "codex");
        {
            let mut slots = supervisor.inner.slots.lock();
            slots.get_mut("codex").unwrap().generation = SessionGeneration::MAX;
        }
        let gate = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .running
            .as_ref()
            .unwrap()
            .input_gate
            .clone();
        let before = slots_mutation_probe(&supervisor);

        let error = supervisor
            .bump_session_generation_for_tests(test_session_id(&supervisor, "codex"))
            .unwrap_err();

        assert!(error.to_string().contains("generation exhausted"));
        assert_eq!(slots_mutation_probe(&supervisor), before);
        assert!(gate.is_accepting());
    }

    #[test]
    fn post_shutdown_pty_callbacks_are_dropped_even_at_max_generation() {
        let supervisor = test_supervisor();
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, pty);
        {
            let mut slots = supervisor.inner.slots.lock();
            slots.get_mut("claude").unwrap().generation = SessionGeneration::MAX;
        }
        let old_run_id = current_run_id(&supervisor, "claude");
        let events = capture_runtime_events(&supervisor);
        supervisor.shutdown().unwrap();
        events.lock().clear();

        for event in [
            PtyEvent::Output("post-shutdown output".into()),
            PtyEvent::Closed,
            PtyEvent::Error("post-shutdown error".into()),
        ] {
            supervisor.handle_pty_event("claude", SessionGeneration::MAX, old_run_id, event);
        }

        let claude = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.label == "claude")
            .unwrap();
        assert_eq!(claude.lifecycle_state, LifecycleState::Closed);
        assert!(!claude.running);
        assert_eq!(claude.process_id, None);
        assert!(events.lock().is_empty());
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
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);

        handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Closed);

        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        match &exits[0] {
            RuntimeEvent::SessionExit {
                session,
                identity,
                process_id,
                exit_code,
                signal,
                success,
                reason,
                requested,
                timestamp,
            } => {
                assert_eq!(session, &codex_alias);
                assert_eq!(identity.generation, 0);
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
                        ..
                    } if session == &codex_alias
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
    fn failed_spawn_creates_no_control_plane_credentials() {
        let supervisor = test_supervisor();
        supervisor.start_control_plane().unwrap();
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(Vec::new())));

        let error = start_test_session(&supervisor, "claude").unwrap_err();

        assert!(error.to_string().contains("no queued PTY sessions"));
        assert!(
            fs::read_dir(supervisor.runtime_dir())
                .unwrap()
                .all(|entry| {
                    let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                    !name.starts_with("control-plane") || !name.ends_with(".json")
                })
        );
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

        handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Closed);

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

        let snapshot = stop_test_session(&supervisor, "codex").unwrap();

        assert_eq!(snapshot.lifecycle_state, LifecycleState::Closed);
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);
        let exits = session_exit_events(&events);
        assert_eq!(exits.len(), 1);
        assert!(matches!(
            &exits[0],
            RuntimeEvent::SessionExit {
                identity: RunEventIdentity { generation: 1, .. },
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
        let codex_alias = test_session_alias(&supervisor, "codex");

        let snapshot = restart_test_session(&supervisor, "codex").unwrap();

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
                } if session == &codex_alias && reason == "session ready"
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
        let codex_alias = test_session_alias(&supervisor, "codex");

        handle_current_pty_event(
            &supervisor,
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
                } if session == &codex_alias
            )
        });
        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SessionExit {
                    session,
                    reason: SessionExitReason::RestartStop,
                    ..
                } if session == &codex_alias
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
                } if session == &codex_alias && reason == "session ready"
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

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );
        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("Working 1s".into()),
        );
        thread::sleep(Duration::from_millis(150));

        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert!(supervisor_alert_events(&events).is_empty());
        assert!(session_exit_events(&events).is_empty());
    }

    #[test]
    fn stale_stall_detector_cannot_consume_replacement_restart_budget_or_alert() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_millis(25)),
        );
        let (old_pty, _, old_kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (replacement_pty, _, replacement_kill_count) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, old_pty);
        let events = capture_runtime_events(&supervisor);
        let replacement_supervisor = supervisor.clone();
        let (replaced_tx, replaced_rx) = mpsc::sync_channel(1);
        supervisor.set_stall_before_reservation_for_tests(move || {
            let mut slots = replacement_supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.generation = slot.generation.checked_add(1).unwrap();
            let replacement_run_id = Uuid::new_v4();
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement_pty))));
            slot.state = LifecycleState::Ready;
            slot.work_state = WorkState::Idle;
            slot.work_state_observed = true;
            slot.work_detail = None;
            slot.stall_detector = None;
            replaced_tx.send(()).unwrap();
        });

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );
        replaced_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stall detector never reached the pre-reservation barrier");
        thread::sleep(Duration::from_millis(50));

        assert_eq!(old_kill_count.load(Ordering::SeqCst), 0);
        assert_eq!(replacement_kill_count.load(Ordering::SeqCst), 0);
        assert!(supervisor_alert_events(&events).is_empty());
        assert!(session_exit_events(&events).is_empty());
        assert!(
            supervisor
                .inner
                .auto_restart_history
                .lock()
                .get(&test_session_id(&supervisor, "codex"))
                .is_none()
        );
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.generation, 1);
        assert_eq!(slot.lifecycle_state, LifecycleState::Ready);
        assert!(slot.running);
    }

    #[test]
    fn reserved_stall_restart_rolls_back_when_the_run_is_replaced_before_action() {
        let supervisor = test_supervisor_with_wrapper_defaults(
            None,
            Some(vec!["codex".into()]),
            Some(Duration::from_millis(25)),
        );
        let (old_pty, _, old_kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (replacement_pty, _, replacement_kill_count) =
            mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, old_pty);
        let events = capture_runtime_events(&supervisor);
        let replacement_supervisor = supervisor.clone();
        let (replaced_tx, replaced_rx) = mpsc::sync_channel(1);
        supervisor.set_stall_after_reservation_for_tests(move || {
            let mut slots = replacement_supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.generation = slot.generation.checked_add(1).unwrap();
            let replacement_run_id = Uuid::new_v4();
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement_pty))));
            slot.state = LifecycleState::Ready;
            slot.work_state = WorkState::Idle;
            slot.work_state_observed = true;
            slot.work_detail = None;
            slot.stall_detector = None;
            replaced_tx.send(()).unwrap();
        });

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output("You've hit your usage limit.".into()),
        );
        replaced_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stall action never reached the post-reservation barrier");
        thread::sleep(Duration::from_millis(50));

        assert_eq!(old_kill_count.load(Ordering::SeqCst), 0);
        assert_eq!(replacement_kill_count.load(Ordering::SeqCst), 0);
        assert!(supervisor_alert_events(&events).is_empty());
        assert!(session_exit_events(&events).is_empty());
        assert!(
            supervisor
                .inner
                .auto_restart_history
                .lock()
                .get(&test_session_id(&supervisor, "codex"))
                .is_none()
        );
        let slot = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .snapshot();
        assert_eq!(slot.generation, 1);
        assert_eq!(slot.lifecycle_state, LifecycleState::Ready);
        assert!(slot.running);
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
                test_session_id(&supervisor, "codex"),
                AutoRestartHistory {
                    attempts: vec![Instant::now(), Instant::now(), Instant::now()],
                    disabled: false,
                },
            );
        }
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session(&supervisor, "codex", DriverKind::Codex, pty);
        let events = capture_runtime_events(&supervisor);
        let codex_alias = test_session_alias(&supervisor, "codex");

        handle_current_pty_event(
            &supervisor,
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
                } if session == &codex_alias && message.contains("cap reached")
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
                .get(&test_session_id(&supervisor, "codex"))
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

        handle_current_pty_event(
            &supervisor,
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
            .find(|session| session.label == "codex")
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

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Error("pipe broke".into()),
        );

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
    fn unknown_route_target_rejects_without_emitting_delivery() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let missing_id = Uuid::new_v4();

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: missing_id,
                content: "hello".into(),
            })
            .unwrap_err();

        assert_eq!(
            error.to_string(),
            format!("unknown session id '{missing_id}'")
        );
        assert!(events.lock().is_empty());
    }

    #[test]
    fn one_recipient_write_failure_emits_failed_delivery_and_returns_error() {
        let supervisor = test_supervisor();
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);
        let (codex_pty, send_count) = failing_pty_session("synthetic route write failure");
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let codex_id = test_session_id(&supervisor, "codex");

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "hello".into(),
            })
            .unwrap_err();

        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        let error = error.to_string();
        assert!(error.contains(&codex_alias));
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
            } if recipient == &codex_alias && message.contains("synthetic route write failure")
        ));
    }

    #[test]
    fn partial_recipient_write_failure_reports_the_committed_prefix() {
        let supervisor = test_supervisor();
        let codex_alias = test_session_alias(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);
        let committed_prefix = 17;
        let (codex_pty, send_count) =
            write_failing_pty_session(committed_prefix, "synthetic partial PTY write failure");
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let codex_id = test_session_id(&supervisor, "codex");

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "hello".into(),
            })
            .unwrap_err();

        assert_eq!(send_count.load(Ordering::SeqCst), 1);
        let error = error.to_string();
        assert!(error.contains("synthetic partial PTY write failure"));
        assert!(error.contains("after 17 bytes were accepted by the PTY"));
        assert!(error.contains("content may be partial"));
        let deliveries = route_delivery_events(&events);
        assert!(matches!(
            deliveries.last().unwrap(),
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                recipient: Some(recipient),
                bytes_written,
                error: Some(message),
                ..
            } if recipient == &codex_alias
                && *bytes_written == committed_prefix
                && message.contains("synthetic partial PTY write failure")
        ));
    }

    #[test]
    fn successful_short_write_is_failed_closed_with_truthful_progress() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let accepted_prefix = 5;
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Box::new(ShortOkPtySession {
                bytes_written: accepted_prefix,
            }),
        );
        let codex_id = test_session_id(&supervisor, "codex");

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "hello".into(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("successful short write"));
        let deliveries = route_delivery_events(&events);
        assert!(deliveries.iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Failed,
                bytes_written,
                ..
            } if *bytes_written == accepted_prefix
        )));
        assert!(!deliveries.iter().any(|event| matches!(
            event,
            RuntimeEvent::RouteDelivery {
                phase: RouteDeliveryPhase::Written,
                ..
            }
        )));
    }

    #[test]
    fn route_never_writes_to_a_replacement_identity_at_the_same_generation() {
        let supervisor = test_supervisor();
        let codex_alias = test_session_alias(&supervisor, "codex");
        let (original_pty, original_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            original_pty,
        );
        let codex_id = test_session_id(&supervisor, "codex");

        let replacement_inputs = Arc::new(Mutex::new(Vec::<String>::new()));
        let replacement_inputs_for_sink = replacement_inputs.clone();
        let supervisor_for_sink = supervisor.clone();
        let replaced = Arc::new(AtomicBool::new(false));
        let replaced_for_sink = replaced.clone();
        supervisor.set_event_sink(move |event| {
            if matches!(
                event,
                RuntimeEvent::DispatchAttempt {
                    action,
                    target_session,
                    ..
                } if action == "route_message" && target_session == codex_alias
            ) && !replaced_for_sink.swap(true, Ordering::SeqCst)
            {
                let mut slots = supervisor_for_sink.inner.slots.lock();
                let slot = slots.get_mut("codex").unwrap();
                slot.running = Some(RunningSession::new(Some(Arc::new(RecordingPtySession {
                    process_id: std::process::id(),
                    inputs: replacement_inputs_for_sink.clone(),
                }))));
                let replacement_run_id = Uuid::new_v4();
                set_test_bracketed_paste_mode(
                    slot,
                    replacement_run_id,
                    BracketedPasteMode::Enabled,
                );
                slot.run_id = Some(replacement_run_id);
                slot.last_run_id = Some(replacement_run_id);
            }
        });

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "must not cross a run boundary".into(),
            })
            .unwrap_err();

        assert!(replaced.load(Ordering::SeqCst));
        assert!(
            error
                .to_string()
                .contains("run identity changed before input")
        );
        assert!(original_inputs.lock().is_empty());
        assert!(replacement_inputs.lock().is_empty());
    }

    #[test]
    fn route_revalidates_its_run_gate_after_lifecycle_wins() {
        let supervisor = test_supervisor();
        let process_id = std::process::id();
        let (old_pty, old_inputs) = recording_pty_session(process_id);
        install_mock_running_session_with_process_id_and_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(process_id),
            old_pty,
        );
        let (replacement_pty, replacement_inputs) = recording_pty_session(process_id);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![replacement_pty])));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        supervisor.set_run_input_before_commit_for_tests(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        let route_supervisor = supervisor.clone();
        let route_thread = thread::spawn(move || {
            route_operator_to_test_session(&route_supervisor, "codex", "must not cross lifecycle")
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("route never reached the run-input commit barrier");

        let replacement = restart_test_session(&supervisor, "codex").unwrap();
        assert_eq!(replacement.lifecycle_state, LifecycleState::Ready);
        release_tx.send(()).unwrap();

        let error = route_thread.join().unwrap().unwrap_err().to_string();
        assert!(error.contains("run changed before input") || error.contains("run is closed"));
        assert!(old_inputs.lock().is_empty());
        assert!(replacement_inputs.lock().is_empty());
    }

    #[test]
    fn operator_input_revalidates_its_run_gate_after_lifecycle_wins() {
        let supervisor = test_supervisor();
        let process_id = std::process::id();
        let (old_pty, old_inputs) = recording_pty_session(process_id);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(process_id),
            old_pty,
        );
        let (replacement_pty, replacement_inputs) = recording_pty_session(process_id);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![replacement_pty])));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        supervisor.set_run_input_before_commit_for_tests(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        let input_session_id = test_session_id(&supervisor, "codex");
        let input_supervisor = supervisor.clone();
        let input_thread = thread::spawn(move || {
            input_supervisor.send_input(SendInputRequest {
                session_id: input_session_id,
                input: "must not cross lifecycle".into(),
            })
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("operator input never reached the run-input commit barrier");

        let replacement = restart_test_session(&supervisor, "codex").unwrap();
        assert_eq!(replacement.lifecycle_state, LifecycleState::Ready);
        release_tx.send(()).unwrap();

        let error = input_thread.join().unwrap().unwrap_err().to_string();
        assert!(error.contains("run changed before input") || error.contains("run is closed"));
        assert!(old_inputs.lock().is_empty());
        assert!(replacement_inputs.lock().is_empty());
    }

    #[test]
    fn lifecycle_declaration_closes_old_run_input_before_stop_commit() {
        let supervisor = test_supervisor();
        let process_id = std::process::id();
        let (old_pty, old_inputs) = recording_pty_session(process_id);
        install_mock_running_session_with_process_id_and_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(process_id),
            old_pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7011, [process_id]))
            .unwrap();

        let session_id = test_session_id(&supervisor, "codex");
        let (_, declared_generation) = supervisor
            .declare_stop_operation(session_id, None, StopIntentKind::Operator)
            .unwrap();
        assert!(
            supervisor
                .inner
                .slots
                .lock()
                .get("codex")
                .unwrap()
                .running
                .is_some()
        );
        assert!(
            !supervisor
                .inner
                .slots
                .lock()
                .get("codex")
                .unwrap()
                .running
                .as_ref()
                .unwrap()
                .input_gate
                .is_accepting(),
            "lifecycle declaration must close the old run's input gate atomically"
        );

        let operator_error = supervisor
            .send_input(SendInputRequest {
                session_id: test_session_id(&supervisor, "codex"),
                input: "operator after declaration".into(),
            })
            .unwrap_err();
        assert!(
            operator_error
                .to_string()
                .contains("run is closed for input")
                || operator_error
                    .to_string()
                    .contains("has no active run identity"),
            "unexpected operator rejection: {operator_error:#}"
        );

        let route_error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: session_id,
                content: "route after declaration".into(),
            })
            .unwrap_err();
        let route_error_chain = format!("{route_error:#}");
        assert!(
            route_error_chain.contains("run is closed for input")
                || route_error_chain.contains("has no active run identity"),
            "unexpected route rejection: {route_error:#}"
        );

        let sideband = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "sideband after declaration".into(),
            },
        );
        assert!(!sideband.ok);
        assert_eq!(sideband.message, "sideband caller run is stale");
        assert!(old_inputs.lock().is_empty());

        supervisor
            .stop_session_at(session_id, declared_generation)
            .unwrap();
    }

    #[test]
    fn in_flight_stop_excludes_concurrent_start_restart_and_second_stop() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "codex");
        let (kill_entered_tx, kill_entered_rx) = mpsc::sync_channel(1);
        let (kill_release_tx, kill_release_rx) = mpsc::sync_channel(1);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(std::process::id()),
            Box::new(GatedKillPtySession {
                process_id: std::process::id(),
                kill_entered: Mutex::new(Some(kill_entered_tx)),
                kill_release: Mutex::new(kill_release_rx),
            }),
        );
        let (spawner, spawn_specs) =
            CapturingPtySpawner::new(Vec::<Box<dyn PtySessionTrait>>::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let stop_supervisor = supervisor.clone();
        let stop_thread = thread::spawn(move || stop_supervisor.stop_session_by_id(session_id));
        kill_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("stop did not reach gated process-scope termination");
        let in_flight_probe = slots_mutation_probe(&supervisor);

        let start_supervisor = supervisor.clone();
        let start = thread::spawn(move || start_supervisor.start_session_by_id(session_id));
        let restart_supervisor = supervisor.clone();
        let restart = thread::spawn(move || restart_supervisor.restart_session_by_id(session_id));
        let second_stop_supervisor = supervisor.clone();
        let second_stop =
            thread::spawn(move || second_stop_supervisor.stop_session_by_id(session_id));

        for error in [
            start.join().unwrap().unwrap_err(),
            restart.join().unwrap().unwrap_err(),
            second_stop.join().unwrap().unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("lifecycle operation in progress"),
                "unexpected concurrent lifecycle error: {error:#}"
            );
        }
        assert_eq!(slots_mutation_probe(&supervisor), in_flight_probe);
        assert!(
            spawn_specs.lock().is_empty(),
            "no replacement spawn may begin while stop proof is in flight"
        );

        kill_release_tx.send(()).unwrap();
        let stopped = stop_thread.join().unwrap().unwrap();
        assert_eq!(stopped.lifecycle_state, LifecycleState::Closed);
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(session_id).unwrap();
        assert!(slot.lifecycle_operation.is_none());
        assert!(!slot.termination_uncertain);
    }

    #[test]
    fn restart_crossing_terminal_shutdown_never_spawns_or_leaves_starting_state() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "codex");
        let (kill_entered_tx, kill_entered_rx) = mpsc::sync_channel(1);
        let (kill_release_tx, kill_release_rx) = mpsc::sync_channel(1);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(std::process::id()),
            Box::new(GatedKillPtySession {
                process_id: std::process::id(),
                kill_entered: Mutex::new(Some(kill_entered_tx)),
                kill_release: Mutex::new(kill_release_rx),
            }),
        );
        let (spawner, spawn_specs) =
            CapturingPtySpawner::new(Vec::<Box<dyn PtySessionTrait>>::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let restart_supervisor = supervisor.clone();
        let restart = thread::spawn(move || restart_supervisor.restart_session_by_id(session_id));
        kill_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("restart stop did not reach gated process-scope termination");

        let shutdown_error = supervisor
            .shutdown()
            .expect_err("shutdown must report the in-flight restart reservation");
        assert!(
            shutdown_error
                .to_string()
                .contains("in-flight lifecycle operation"),
            "unexpected shutdown error: {shutdown_error:#}"
        );

        kill_release_tx.send(()).unwrap();
        let restart_error = restart.join().unwrap().unwrap_err();
        assert!(
            format!("{restart_error:#}").contains("supervisor has shut down"),
            "unexpected restart error: {restart_error:#}"
        );
        assert!(
            spawn_specs.lock().is_empty(),
            "a restart crossing terminal shutdown must not spawn a replacement"
        );

        let snapshot = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(snapshot.lifecycle_state, LifecycleState::Closed);
        assert!(!snapshot.running);
        assert!(snapshot.run_id.is_none());
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(session_id).unwrap();
        assert!(slot.lifecycle_operation.is_none());
        assert!(slot.spawn_in_flight.is_none());
        assert!(!slot.termination_uncertain);
        drop(slots);

        supervisor
            .shutdown()
            .expect("repeated shutdown must accept the proved closed state");
    }

    #[test]
    fn restart_reserves_all_run_events_before_stopping_the_old_process_scope() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "codex");
        let (pty, _, kill_count) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
        );
        supervisor
            .inner
            .slots
            .lock()
            .get_by_id_mut(session_id)
            .unwrap()
            .run_event_sequence = u64::MAX - 4;
        let before = slots_mutation_probe(&supervisor);

        let error = supervisor
            .restart_session_by_id(session_id)
            .expect_err("restart without full event capacity must fail before declaration");
        assert!(
            error.to_string().contains("run event sequence exhausted"),
            "unexpected restart error: {error:#}"
        );
        assert_eq!(slots_mutation_probe(&supervisor), before);
        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn post_spawn_event_exhaustion_terminates_the_returned_process_scope() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "claude");
        let (pty, _, kill_count) =
            mock_pty_session(Some(std::process::id()), MockKillBehavior::Immediate);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));
        supervisor
            .inner
            .slots
            .lock()
            .get_by_id_mut(session_id)
            .unwrap()
            .run_event_sequence = u64::MAX - 1;

        let error = supervisor
            .start_session_by_id(session_id)
            .expect_err("ready-event exhaustion must reject the spawned PTY");
        assert!(
            error.to_string().contains("run event sequence exhausted"),
            "unexpected start error: {error:#}"
        );
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(session_id).unwrap();
        assert_eq!(slot.state, LifecycleState::Closed);
        assert!(slot.running.is_none());
        assert!(slot.spawn_in_flight.is_none());
        assert!(!slot.termination_uncertain);
    }

    #[test]
    fn spawn_error_cleanup_cannot_be_skipped_by_terminal_event_exhaustion() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "claude");
        let (spawner, spawn_specs) =
            CapturingPtySpawner::new(Vec::<Box<dyn PtySessionTrait>>::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        supervisor
            .inner
            .slots
            .lock()
            .get_by_id_mut(session_id)
            .unwrap()
            .run_event_sequence = u64::MAX - 1;

        let error = supervisor
            .start_session_by_id(session_id)
            .expect_err("injected spawn failure must remain visible");
        assert!(
            error.to_string().contains("no queued PTY sessions"),
            "unexpected spawn error: {error:#}"
        );
        assert_eq!(spawn_specs.lock().len(), 1);
        let slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id(session_id).unwrap();
        assert_eq!(slot.state, LifecycleState::Failed);
        assert!(slot.running.is_none());
        assert!(slot.spawn_in_flight.is_none());
        assert!(!slot.termination_uncertain);
    }

    #[test]
    fn output_before_spawn_validation_never_declares_the_run_ready_or_working() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "claude");
        let events = capture_runtime_events(&supervisor);
        supervisor.set_pty_spawner_for_tests(Arc::new(OutputThenFailPtySpawner));

        let error = supervisor
            .start_session_by_id(session_id)
            .expect_err("post-output validation failure must reject the run");
        assert!(
            error
                .to_string()
                .contains("injected post-output spawn validation failure")
        );
        let snapshot = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(snapshot.lifecycle_state, LifecycleState::Failed);
        assert!(!snapshot.running);

        let events = events.lock();
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionOutput { identity, chunk, .. }
                if identity.session_id == session_id && chunk == "Working before admission"
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionState {
                identity,
                state: LifecycleState::Ready,
                ..
            } if identity.session_id == session_id
        )));
        assert!(!events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState { identity, .. }
                if identity.session_id == session_id
        )));
    }

    #[test]
    fn run_input_gate_serializes_submissions_in_arrival_order() {
        let supervisor = test_supervisor();
        let inputs = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (first_entered_tx, first_entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(std::process::id()),
            Box::new(FirstWriteBlockingPtySession {
                process_id: std::process::id(),
                inputs: inputs.clone(),
                calls: calls.clone(),
                first_entered: first_entered_tx,
                release: release.clone(),
            }),
        );
        let input_gate = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .running
            .as_ref()
            .unwrap()
            .input_gate
            .clone();

        let session_id = test_session_id(&supervisor, "codex");
        let first_supervisor = supervisor.clone();
        let first = thread::spawn(move || {
            first_supervisor.send_input(SendInputRequest {
                session_id,
                input: "first".into(),
            })
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first input never reached the PTY");

        let second_supervisor = supervisor.clone();
        let second = thread::spawn(move || {
            second_supervisor.send_input(SendInputRequest {
                session_id,
                input: "second".into(),
            })
        });
        let second_deadline = Instant::now() + Duration::from_secs(2);
        while input_gate.queued_writes() < 1 && Instant::now() < second_deadline {
            thread::yield_now();
        }
        assert_eq!(input_gate.queued_writes(), 1);

        let third_supervisor = supervisor.clone();
        let third = thread::spawn(move || {
            third_supervisor.send_input(SendInputRequest {
                session_id,
                input: "third".into(),
            })
        });
        let third_deadline = Instant::now() + Duration::from_secs(2);
        while input_gate.queued_writes() < 2 && Instant::now() < third_deadline {
            thread::yield_now();
        }
        assert_eq!(input_gate.queued_writes(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let (released, ready) = &*release;
        *released.lock() = true;
        ready.notify_all();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        third.join().unwrap().unwrap();

        assert_eq!(calls.load(Ordering::SeqCst), 3);
        assert_eq!(inputs.lock().as_slice(), &["first", "second", "third"]);
    }

    #[test]
    fn route_message_requires_running_recipient() {
        let supervisor = test_supervisor();
        let claude_id = test_session_id(&supervisor, "claude");
        let claude_alias = test_session_alias(&supervisor, "claude");

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: claude_id,
                content: "hello".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&format!("session '{claude_alias}' is not running"))
        );
    }

    #[test]
    fn send_control_key_rejects_non_running_session() {
        let supervisor = test_supervisor();
        let claude_id = test_session_id(&supervisor, "claude");
        let claude_alias = test_session_alias(&supervisor, "claude");

        let error = supervisor
            .send_control_key_by_id(claude_id, ControlKey::Enter)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&format!("session '{claude_alias}' is not running"))
        );
    }

    #[test]
    fn agent_liveness_prunes_dead_inner_child_while_wrapper_pid_lives() {
        let supervisor = test_supervisor();
        let codex_alias = test_session_alias(&supervisor, "codex");
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
        install_mock_running_session_with_process_id_and_bracketed_paste_enabled(
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
            .find(|session| session.label == "codex")
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
            } if session == &codex_alias && *pid == wrapper_pid
        )));

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: test_session_id(&supervisor, "codex"),
                content: "hello".into(),
            })
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains(&format!("session '{codex_alias}' is not running"))
        );
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
            .find(|session| session.label == "codex")
            .unwrap();
        assert!(codex.running);
        assert_eq!(codex.process_id, Some(wrapper_pid));
    }

    #[test]
    fn terminal_text_cannot_close_session_without_process_evidence() {
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

        for text in [
            "codex: command not found",
            "[process exited with code 1]",
            "Codex CLI v0.147.0",
            "Codex CLI v0.147.0",
            "Codex CLI v0.147.0",
        ] {
            handle_current_pty_event(&supervisor, "codex", 0, PtyEvent::Output(text.into()));
        }

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.label == "codex")
            .unwrap();
        assert!(codex.running);
        assert_eq!(codex.lifecycle_state, LifecycleState::Ready);
        assert!(!work_state_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState {
                state: WorkState::Exited,
                ..
            }
        )));
        assert!(session_exit_events(&events).is_empty());
    }

    #[test]
    fn snapshot_prunes_exited_running_session() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "codex");

        let snapshot = supervisor.snapshot();
        let codex = snapshot
            .sessions
            .into_iter()
            .find(|session| session.label == "codex")
            .unwrap();

        assert!(!codex.running);
        assert_eq!(codex.lifecycle_state, LifecycleState::Closed);
        assert_eq!(codex.process_id, None);
    }

    #[test]
    fn send_input_rejects_exited_session_after_liveness_refresh() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "codex");
        let codex_alias = test_session_alias(&supervisor, "codex");

        let error = supervisor
            .send_input(SendInputRequest {
                session_id: test_session_id(&supervisor, "codex"),
                input: "hello".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&format!("session '{codex_alias}' is not running"))
        );
    }

    #[test]
    fn pane_caller_resolution_requires_one_fully_verified_live_job() {
        struct Case {
            members: Vec<u32>,
            failing_job: Option<u32>,
            missing_pty: bool,
            expected_error: &'static str,
            expected_queries: Vec<u32>,
        }

        let cases = [
            Case {
                members: Vec::new(),
                failing_job: None,
                missing_pty: false,
                expected_error: "is not owned by a live pane",
                expected_queries: vec![101, 102],
            },
            Case {
                members: vec![101, 102],
                failing_job: None,
                missing_pty: false,
                expected_error: "belongs to multiple live panes",
                expected_queries: vec![101, 102],
            },
            Case {
                members: vec![101],
                failing_job: Some(102),
                missing_pty: false,
                expected_error: "membership could not be verified",
                expected_queries: vec![101, 102],
            },
            Case {
                members: vec![101],
                failing_job: None,
                missing_pty: true,
                expected_error: "membership could not be verified",
                expected_queries: vec![101],
            },
        ];

        for case in cases {
            let supervisor = test_supervisor();
            let (claude_pty, _) = recording_pty_session(101);
            install_mock_running_session_with_process_id(
                &supervisor,
                "claude",
                DriverKind::Claude,
                Some(101),
                claude_pty,
            );
            if case.missing_pty {
                install_synthetic_running_session(&supervisor, "codex", DriverKind::Codex);
            } else {
                let (codex_pty, _) = recording_pty_session(102);
                install_mock_running_session_with_process_id(
                    &supervisor,
                    "codex",
                    DriverKind::Codex,
                    Some(102),
                    codex_pty,
                );
            }

            let process = TestPaneProcess::new(7001, case.members);
            if let Some(process_id) = case.failing_job {
                process.fail_membership_for(process_id);
            }
            let error = supervisor
                .resolve_pane_caller(process.clone())
                .err()
                .expect("ambiguous or unverifiable caller must be rejected");

            assert!(
                error.to_string().contains(case.expected_error),
                "unexpected error: {error:#}"
            );
            assert_eq!(process.queried_process_ids(), case.expected_queries);
        }
    }

    #[test]
    fn pane_caller_resolution_pins_exact_session_and_generation() {
        let supervisor = test_supervisor();
        let (claude_pty, _) = recording_pty_session(101);
        let (codex_pty, _) = recording_pty_session(102);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            claude_pty,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            codex_pty,
        );
        let process = TestPaneProcess::new(7002, [101]);

        let caller = supervisor.resolve_pane_caller(process.clone()).unwrap();

        assert_eq!(caller.session, test_session_alias(&supervisor, "claude"));
        assert_eq!(caller.generation, 0);
        assert_eq!(process.queried_process_ids(), vec![101, 102]);
    }

    #[test]
    fn exited_process_is_rejected_before_any_job_query() {
        let supervisor = test_supervisor();
        let (claude_pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            claude_pty,
        );
        let process = TestPaneProcess::new(7003, [101]);
        process.exit();

        let error = supervisor
            .resolve_pane_caller(process.clone())
            .err()
            .expect("exited process must be rejected");

        assert!(error.to_string().contains("has exited"));
        assert!(process.queried_process_ids().is_empty());
    }

    #[test]
    fn peer_sideband_actions_reject_before_events_files_or_slot_mutation() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, claude_inputs) = recording_pty_session(101);
        let (codex_pty, codex_inputs) = recording_pty_session(102);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            claude_pty,
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(102),
            codex_pty,
        );
        arm_test_quiesce_timer(&supervisor, "codex");
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7004, [101]))
            .unwrap();
        let slots_before = slots_mutation_probe(&supervisor);
        let files_before = runtime_file_manifest(&supervisor);

        for request in [
            SidebandRequest::SendInput {
                name: "codex".into(),
                input: "blocked".into(),
            },
            SidebandRequest::SendKey {
                name: "codex".into(),
                key: ControlKey::Enter,
            },
            SidebandRequest::WaitQuiet {
                name: "codex".into(),
                quiet_seconds: 1,
                timeout_seconds: 1,
            },
        ] {
            let response = supervisor.apply_sideband_request(&caller, request);
            assert!(!response.ok);
            assert_eq!(
                response.message,
                "sideband request may target only the calling session"
            );
            assert!(response.request_id.is_none());
        }

        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(runtime_file_manifest(&supervisor), files_before);
        assert!(events.lock().is_empty());
        assert!(claude_inputs.lock().is_empty());
        assert!(codex_inputs.lock().is_empty());
    }

    #[test]
    fn pane_sideband_allows_only_its_live_run_input_key_and_ping() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (claude_pty, inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            claude_pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7005, [101]))
            .unwrap();

        let ping = supervisor.apply_sideband_request(&caller, SidebandRequest::Ping {});
        let input = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "hello".into(),
            },
        );
        let key = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendKey {
                name: caller.session.clone(),
                key: ControlKey::Enter,
            },
        );

        assert!(ping.ok);
        assert_eq!(ping.message, "pong");
        assert!(input.ok);
        assert!(key.ok);
        assert!(ping.request_id.is_some());
        assert!(input.request_id.is_some());
        assert!(key.request_id.is_some());
        assert_eq!(&*inputs.lock(), &["hello".to_string(), "\r".to_string()]);
        let caller_alias = caller.session.clone();
        let attempts = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::DispatchAttempt {
                    action,
                    from,
                    target_session,
                    ..
                } => Some((action.clone(), from.clone(), target_session.clone())),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            attempts,
            vec![
                ("ping".into(), caller_alias.clone(), caller_alias.clone(),),
                (
                    "send_input".into(),
                    caller_alias.clone(),
                    caller_alias.clone(),
                ),
                (
                    "send_key".into(),
                    caller_alias.clone(),
                    caller_alias.clone()
                ),
            ]
        );
        let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        assert!(audit.contains("\"event\":\"dispatch_attempt\""));
        assert!(audit.contains(&format!("\"from\":\"{caller_alias}\"")));
        assert!(!audit.contains("hello"));
    }

    #[test]
    fn pane_sideband_revalidates_caller_before_dispatch_metadata_or_ping() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (old_pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            old_pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7020, [101]))
            .unwrap();
        let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));

        let request_supervisor = supervisor.clone();
        let request_thread = thread::spawn(move || {
            request_supervisor.apply_sideband_request(&caller, SidebandRequest::Ping {})
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("sideband request did not reach the authorization barrier");
        let (replacement_pty, replacement_inputs) = recording_pty_session(101);
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("claude").unwrap();
            slot.session_id = Uuid::new_v4();
            let replacement_run_id = Uuid::new_v4();
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement_pty))));
        }
        let slots_after_replacement = slots_mutation_probe(&supervisor);
        release_tx.send(()).unwrap();

        let response = request_thread.join().unwrap();
        assert!(!response.ok);
        assert_eq!(response.message, "sideband caller run is stale");
        assert!(response.request_id.is_none());
        assert!(replacement_inputs.lock().is_empty());
        assert_eq!(slots_mutation_probe(&supervisor), slots_after_replacement);
        assert!(events.lock().is_empty());
        assert_eq!(
            fs::read(supervisor.audit_log_path()).unwrap_or_default(),
            audit_before
        );
    }

    #[test]
    fn pane_wait_quiet_is_server_bounded_and_rejects_before_dispatch() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let (pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
        );
        {
            let mut slots = supervisor.inner.slots.lock();
            slots.get_mut("claude").unwrap().last_real_output_at =
                Some(Instant::now() - Duration::from_secs(61));
        }
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7021, [101]))
            .unwrap();

        let at_limit = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::WaitQuiet {
                name: caller.session.clone(),
                quiet_seconds: SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS,
                timeout_seconds: SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS,
            },
        );
        assert!(at_limit.ok, "maximum bounded wait should be admitted");
        events.lock().clear();
        let audit_before_invalid = fs::read(supervisor.audit_log_path()).unwrap_or_default();
        let slots_before_invalid = slots_mutation_probe(&supervisor);

        let invalid_cases = [
            (
                0,
                1,
                format!(
                    "wait_quiet quiet_seconds must be between 1 and {SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS}"
                ),
            ),
            (
                SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS + 1,
                SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS,
                format!(
                    "wait_quiet quiet_seconds must be between 1 and {SIDEBAND_WAIT_QUIET_MAX_QUIET_SECONDS}"
                ),
            ),
            (
                1,
                SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS + 1,
                format!(
                    "wait_quiet timeout_seconds must be between 1 and {SIDEBAND_WAIT_QUIET_MAX_TIMEOUT_SECONDS}"
                ),
            ),
            (
                2,
                1,
                "wait_quiet quiet_seconds must not exceed timeout_seconds".into(),
            ),
        ];
        for (quiet_seconds, timeout_seconds, expected_error) in invalid_cases {
            let response = supervisor.apply_sideband_request(
                &caller,
                SidebandRequest::WaitQuiet {
                    name: caller.session.clone(),
                    quiet_seconds,
                    timeout_seconds,
                },
            );
            assert!(!response.ok);
            assert_eq!(response.message, expected_error);
            assert!(response.request_id.is_none());
        }

        assert!(events.lock().is_empty());
        assert_eq!(slots_mutation_probe(&supervisor), slots_before_invalid);
        assert_eq!(
            fs::read(supervisor.audit_log_path()).unwrap_or_default(),
            audit_before_invalid
        );
    }

    #[test]
    fn pane_sideband_revalidates_generation_immediately_before_pty_write() {
        let supervisor = test_supervisor();
        let (old_pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            old_pty,
        );
        let process = TestPaneProcess::new(7006, [101]);
        let caller = supervisor.resolve_pane_caller(process).unwrap();
        let (replacement_pty, replacement_inputs) = recording_pty_session(101);
        {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("claude").unwrap();
            slot.generation = slot.generation.checked_add(1).unwrap();
            slot.running = Some(RunningSession::new(Some(Arc::from(replacement_pty))));
            let replacement_run_id = Uuid::new_v4();
            slot.run_id = Some(replacement_run_id);
            slot.last_run_id = Some(replacement_run_id);
        }
        let slots_before = slots_mutation_probe(&supervisor);

        let response = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "must not reach replacement".into(),
            },
        );

        assert!(!response.ok);
        assert_eq!(response.message, "sideband caller run is stale");
        assert!(response.request_id.is_none());
        assert!(replacement_inputs.lock().is_empty());
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
    }

    #[test]
    fn pane_sideband_revalidates_its_run_gate_after_lifecycle_wins() {
        let supervisor = test_supervisor();
        let (old_pty, old_inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            old_pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7010, [101]))
            .unwrap();
        let (replacement_pty, replacement_inputs) = recording_pty_session(102);
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![replacement_pty])));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        supervisor.set_run_input_before_commit_for_tests(move || {
            entered_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        let input_supervisor = supervisor.clone();
        let input_thread = thread::spawn(move || {
            input_supervisor.apply_sideband_request(
                &caller,
                SidebandRequest::SendInput {
                    name: caller.session.clone(),
                    input: "must not cross lifecycle".into(),
                },
            )
        });
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("sideband input never reached the run-input commit barrier");

        let replacement = restart_test_session(&supervisor, "claude").unwrap();
        assert_eq!(replacement.lifecycle_state, LifecycleState::Ready);
        release_tx.send(()).unwrap();

        let response = input_thread.join().unwrap();
        assert!(!response.ok);
        assert!(old_inputs.lock().is_empty());
        assert!(replacement_inputs.lock().is_empty());
    }

    #[test]
    fn pane_sideband_revalidates_process_liveness_immediately_before_pty_write() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
        );
        let process = TestPaneProcess::new(7007, [101]);
        let caller = supervisor.resolve_pane_caller(process.clone()).unwrap();
        process.exit();
        let slots_before = slots_mutation_probe(&supervisor);

        let response = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "must not be written".into(),
            },
        );

        assert!(!response.ok);
        assert!(response.message.contains("has exited"));
        assert!(response.request_id.is_none());
        assert!(inputs.lock().is_empty());
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
    }

    #[test]
    fn wait_quiet_revalidates_caller_while_waiting() {
        let supervisor = test_supervisor();
        let (pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
        );
        supervisor
            .inner
            .slots
            .lock()
            .get_mut("claude")
            .unwrap()
            .last_real_output_at = Some(Instant::now());
        let process = TestPaneProcess::new(7008, [101]);
        let caller = supervisor.resolve_pane_caller(process.clone()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();

        let response = runtime.block_on(async {
            let handle = supervisor.clone();
            let wait_caller = caller.clone();
            let task = tokio::spawn(async move {
                handle
                    .apply_sideband_request_async(
                        &wait_caller,
                        SidebandRequest::WaitQuiet {
                            name: wait_caller.session.clone(),
                            quiet_seconds: 1,
                            timeout_seconds: 2,
                        },
                    )
                    .await
            });
            tokio::time::sleep(Duration::from_millis(25)).await;
            process.exit();
            task.await.unwrap()
        });

        assert!(!response.ok);
        assert!(!response.timed_out);
        assert!(response.message.contains("has exited"));
    }

    #[test]
    fn sideband_write_timeout_falls_back_to_terminating_only_the_exact_run() {
        let supervisor = test_supervisor();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let writes_started = Arc::new(AtomicUsize::new(0));
        let writes_finished = Arc::new(AtomicUsize::new(0));
        let timed_out_inputs = Arc::new(Mutex::new(Vec::new()));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let kill_count = Arc::new(AtomicUsize::new(0));
        let rescue_fired = Arc::new(AtomicBool::new(false));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            Box::new(TimedOutWritePtySession {
                process_id: 101,
                entered: Mutex::new(Some(entered_tx)),
                release: release.clone(),
                writes_started: writes_started.clone(),
                writes_finished: writes_finished.clone(),
                inputs: timed_out_inputs.clone(),
                cancel_supported: false,
                cancel_count: cancel_count.clone(),
                kill_count: kill_count.clone(),
                first_write_error_bytes: 0,
            }),
        );
        let (codex_pty, codex_inputs) = recording_pty_session(102);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            codex_pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7012, [101]))
            .unwrap();
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_millis(50));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap();
        let started_at = Instant::now();
        let rescue_release = release.clone();
        let rescue_flag = rescue_fired.clone();
        let (disarm_rescue_tx, disarm_rescue_rx) = mpsc::channel();
        let rescue = thread::spawn(move || {
            if disarm_rescue_rx
                .recv_timeout(Duration::from_secs(3))
                .is_ok()
            {
                return;
            }
            let (released, ready) = &*rescue_release;
            let mut released = released.lock();
            if !*released {
                rescue_flag.store(true, Ordering::SeqCst);
                *released = true;
                ready.notify_all();
            }
        });

        let response = runtime.block_on(async {
            let request_supervisor = supervisor.clone();
            let request_caller = caller.clone();
            let request = tokio::spawn(async move {
                request_supervisor
                    .apply_sideband_request_async(
                        &request_caller,
                        SidebandRequest::SendInput {
                            name: request_caller.session.clone(),
                            input: "must time out".into(),
                        },
                    )
                    .await
            });
            tokio::task::spawn_blocking(move || {
                entered_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("sideband write never entered the blocking PTY")
            })
            .await
            .unwrap();

            supervisor
                .send_input(SendInputRequest {
                    session_id: test_session_id(&supervisor, "codex"),
                    input: "other run remains writable".into(),
                })
                .expect("unrelated run was blocked by the timed-out write");
            request.await.unwrap()
        });
        let _ = disarm_rescue_tx.send(());

        assert!(!response.ok);
        assert!(response.timed_out);
        assert!(response.message.contains("timed out after 50ms"));
        assert!(
            response
                .message
                .contains("isolated input cancellation failed"),
            "{}",
            response.message
        );
        assert!(
            response
                .message
                .contains("exact run terminated as cancellation fallback"),
            "{}",
            response.message
        );
        assert!(started_at.elapsed() < Duration::from_secs(2));
        assert_eq!(writes_started.load(Ordering::SeqCst), 1);
        assert_eq!(writes_finished.load(Ordering::SeqCst), 1);
        assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);
        rescue.join().unwrap();
        assert!(!rescue_fired.load(Ordering::SeqCst));
        assert_eq!(
            &*codex_inputs.lock(),
            &["other run remains writable".to_string()]
        );

        let claude = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .snapshot();
        assert!(!claude.running);
        assert_eq!(claude.lifecycle_state, LifecycleState::Closed);
        assert_eq!(claude.run_id, None);
        let final_started = writes_started.load(Ordering::SeqCst);
        let final_finished = writes_finished.load(Ordering::SeqCst);
        thread::sleep(Duration::from_millis(100));
        assert_eq!(writes_started.load(Ordering::SeqCst), final_started);
        assert_eq!(writes_finished.load(Ordering::SeqCst), final_finished);
    }

    #[test]
    fn sideband_write_timeout_cancels_input_and_preserves_the_live_conversation() {
        let supervisor = test_supervisor();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        let writes_started = Arc::new(AtomicUsize::new(0));
        let writes_finished = Arc::new(AtomicUsize::new(0));
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let cancel_count = Arc::new(AtomicUsize::new(0));
        let kill_count = Arc::new(AtomicUsize::new(0));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            Box::new(TimedOutWritePtySession {
                process_id: 101,
                entered: Mutex::new(Some(entered_tx)),
                release,
                writes_started: writes_started.clone(),
                writes_finished: writes_finished.clone(),
                inputs: inputs.clone(),
                cancel_supported: true,
                cancel_count: cancel_count.clone(),
                kill_count: kill_count.clone(),
                first_write_error_bytes: 4,
            }),
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7013, [101]))
            .unwrap();
        let original_snapshot = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .snapshot();
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_millis(50));
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap();

        let response = runtime.block_on(async {
            let request_supervisor = supervisor.clone();
            let request_caller = caller.clone();
            let request = tokio::spawn(async move {
                request_supervisor
                    .apply_sideband_request_async(
                        &request_caller,
                        SidebandRequest::SendInput {
                            name: request_caller.session.clone(),
                            input: "partial timed write".into(),
                        },
                    )
                    .await
            });
            tokio::task::spawn_blocking(move || {
                entered_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("sideband write never entered the blocking PTY")
            })
            .await
            .unwrap();
            request.await.unwrap()
        });

        assert!(!response.ok);
        assert!(response.timed_out);
        assert!(
            response.message.contains("run preserved"),
            "{}",
            response.message
        );
        assert!(
            response
                .message
                .contains("4 bytes reached the PTY before cancellation"),
            "{}",
            response.message
        );
        assert_eq!(cancel_count.load(Ordering::SeqCst), 1);
        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert_eq!(writes_started.load(Ordering::SeqCst), 1);
        assert_eq!(writes_finished.load(Ordering::SeqCst), 1);
        let after_timeout = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .snapshot();
        assert_eq!(after_timeout.session_id, original_snapshot.session_id);
        assert_eq!(after_timeout.run_id, original_snapshot.run_id);
        assert_eq!(after_timeout.generation, original_snapshot.generation);
        assert!(after_timeout.running);

        let second = runtime.block_on(supervisor.apply_sideband_request_async(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "conversation continues".into(),
            },
        ));
        assert!(second.ok, "{}", second.message);
        assert!(!second.timed_out);
        assert_eq!(writes_started.load(Ordering::SeqCst), 2);
        assert_eq!(writes_finished.load(Ordering::SeqCst), 2);
        assert_eq!(kill_count.load(Ordering::SeqCst), 0);
        assert_eq!(
            &*inputs.lock(),
            &[
                "partial timed write".to_string(),
                "conversation continues".to_string()
            ]
        );
    }

    #[test]
    fn timeout_cancellation_barrier_prevents_a_late_cancel_from_hitting_the_next_write() {
        let supervisor = test_supervisor();
        let (first_entered_tx, first_entered_rx) = mpsc::sync_channel(1);
        let (second_entered_tx, second_entered_rx) = mpsc::sync_channel(1);
        let (cancel_entered_tx, cancel_entered_rx) = mpsc::sync_channel(1);
        let first_release = Arc::new((Mutex::new(false), Condvar::new()));
        let cancel_release = Arc::new((Mutex::new(false), Condvar::new()));
        let inputs = Arc::new(Mutex::new(Vec::new()));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            Box::new(DelayedCancelPtySession {
                process_id: 101,
                calls: AtomicUsize::new(0),
                inputs: inputs.clone(),
                first_entered: first_entered_tx,
                first_release: first_release.clone(),
                second_entered: Mutex::new(Some(second_entered_tx)),
                cancel_entered: Mutex::new(Some(cancel_entered_tx)),
                cancel_release: cancel_release.clone(),
            }),
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7014, [101]))
            .unwrap();
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_millis(50));

        let timed_supervisor = supervisor.clone();
        let timed_caller = caller.clone();
        let timed_request = thread::spawn(move || {
            timed_supervisor.apply_sideband_request(
                &timed_caller,
                SidebandRequest::SendInput {
                    name: timed_caller.session.clone(),
                    input: "first completes late".into(),
                },
            )
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first write did not enter");
        cancel_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("isolated cancellation did not enter");

        let successor_session_id = test_session_id(&supervisor, "claude");
        let successor_supervisor = supervisor.clone();
        let successor = thread::spawn(move || {
            successor_supervisor.send_input(SendInputRequest {
                session_id: successor_session_id,
                input: "successor write".into(),
            })
        });
        {
            let (released, ready) = &*first_release;
            *released.lock() = true;
            ready.notify_all();
        }
        assert!(
            second_entered_rx
                .recv_timeout(Duration::from_millis(150))
                .is_err(),
            "successor entered PTY input before the late cancellation completed"
        );

        {
            let (released, ready) = &*cancel_release;
            *released.lock() = true;
            ready.notify_all();
        }
        let timed_response = timed_request.join().unwrap();
        successor.join().unwrap().unwrap();
        second_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("successor did not resume after cancellation barrier cleared");

        assert!(timed_response.ok, "{}", timed_response.message);
        assert!(timed_response.timed_out);
        assert!(
            timed_response.message.contains("completed after the 50ms"),
            "{}",
            timed_response.message
        );
        assert_eq!(
            &*inputs.lock(),
            &[
                "first completes late".to_string(),
                "successor write".to_string()
            ]
        );
    }

    #[test]
    fn queued_timeout_cannot_clear_an_older_inflight_cancellation_barrier() {
        let supervisor = test_supervisor();
        let (first_entered_tx, first_entered_rx) = mpsc::sync_channel(1);
        let (successor_entered_tx, successor_entered_rx) = mpsc::sync_channel(1);
        let (cancel_entered_tx, cancel_entered_rx) = mpsc::sync_channel(1);
        let first_release = Arc::new((Mutex::new(false), Condvar::new()));
        let cancel_release = Arc::new((Mutex::new(false), Condvar::new()));
        let inputs = Arc::new(Mutex::new(Vec::new()));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            Box::new(DelayedCancelPtySession {
                process_id: 101,
                calls: AtomicUsize::new(0),
                inputs: inputs.clone(),
                first_entered: first_entered_tx,
                first_release: first_release.clone(),
                second_entered: Mutex::new(Some(successor_entered_tx)),
                cancel_entered: Mutex::new(Some(cancel_entered_tx)),
                cancel_release: cancel_release.clone(),
            }),
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7016, [101]))
            .unwrap();
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_millis(25));
        let input_gate = supervisor
            .inner
            .slots
            .lock()
            .get("claude")
            .unwrap()
            .running
            .as_ref()
            .unwrap()
            .input_gate
            .clone();

        let first_supervisor = supervisor.clone();
        let first_caller = caller.clone();
        let first = thread::spawn(move || {
            first_supervisor.apply_sideband_request(
                &first_caller,
                SidebandRequest::SendInput {
                    name: first_caller.session.clone(),
                    input: "inflight A".into(),
                },
            )
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("inflight writer A did not enter");
        cancel_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("A cancellation did not enter");

        let queued_supervisor = supervisor.clone();
        let queued_caller = caller.clone();
        let queued = thread::spawn(move || {
            queued_supervisor.apply_sideband_request(
                &queued_caller,
                SidebandRequest::SendInput {
                    name: queued_caller.session.clone(),
                    input: "queued timeout B".into(),
                },
            )
        });
        let queued_deadline = Instant::now() + Duration::from_secs(2);
        while input_gate.queued_writes() < 1 && Instant::now() < queued_deadline {
            thread::yield_now();
        }
        assert_eq!(input_gate.queued_writes(), 1);

        {
            let (released, ready) = &*first_release;
            *released.lock() = true;
            ready.notify_all();
        }
        let queued_response = queued.join().unwrap();
        assert!(!queued_response.ok);
        assert!(queued_response.timed_out);
        assert!(
            queued_response
                .message
                .contains("cancelled before PTY input began"),
            "{}",
            queued_response.message
        );

        let successor_session_id = test_session_id(&supervisor, "claude");
        let successor_supervisor = supervisor.clone();
        let successor = thread::spawn(move || {
            successor_supervisor.send_input(SendInputRequest {
                session_id: successor_session_id,
                input: "successor C".into(),
            })
        });
        assert!(
            successor_entered_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "queued timeout B cleared A's still-owned cancellation barrier"
        );

        {
            let (released, ready) = &*cancel_release;
            *released.lock() = true;
            ready.notify_all();
        }
        let first_response = first.join().unwrap();
        assert!(first_response.ok);
        assert!(first_response.timed_out);
        successor.join().unwrap().unwrap();
        successor_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("successor C did not enter after A released its barrier");
        assert_eq!(
            &*inputs.lock(),
            &["inflight A".to_string(), "successor C".to_string()]
        );
    }

    #[test]
    fn queued_sideband_timeout_never_enters_the_pty_or_cancels_the_active_writer() {
        let supervisor = test_supervisor();
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (first_entered_tx, first_entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            None,
            Box::new(FirstWriteBlockingPtySession {
                process_id: 101,
                inputs: inputs.clone(),
                calls: calls.clone(),
                first_entered: first_entered_tx,
                release: release.clone(),
            }),
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7015, [101]))
            .unwrap();
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_millis(50));

        let active_session_id = test_session_id(&supervisor, "claude");
        let active_supervisor = supervisor.clone();
        let active_write = thread::spawn(move || {
            active_supervisor.send_input(SendInputRequest {
                session_id: active_session_id,
                input: "active operator write".into(),
            })
        });
        first_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("active writer did not enter the PTY");

        let response = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "must never reach PTY".into(),
            },
        );
        assert!(!response.ok);
        assert!(response.timed_out);
        assert!(
            response
                .message
                .contains("cancelled before PTY input began"),
            "{}",
            response.message
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(&*inputs.lock(), &["active operator write".to_string()]);

        {
            let (released, ready) = &*release;
            *released.lock() = true;
            ready.notify_all();
        }
        active_write.join().unwrap().unwrap();

        let continued = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "conversation still writable".into(),
            },
        );
        assert!(continued.ok, "{}", continued.message);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            &*inputs.lock(),
            &[
                "active operator write".to_string(),
                "conversation still writable".to_string()
            ]
        );
    }

    #[test]
    fn sideband_stream_rejects_malformed_payload_without_mutation() {
        let supervisor = test_supervisor();
        let (pty, _) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7009, [101]))
            .unwrap();
        let slots_before = slots_mutation_probe(&supervisor);
        let files_before = runtime_file_manifest(&supervisor);
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        let error = runtime.block_on(async {
            let (mut client, server) = tokio::io::duplex(4096);
            let handle = supervisor.clone();
            let server_task =
                tokio::spawn(async move { handle_sideband_stream(handle, caller, server).await });
            client.write_all(b"{not json}\n").await.unwrap();
            client.flush().await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), server_task)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
        });

        assert!(error.to_string().contains("invalid sideband payload"));
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(runtime_file_manifest(&supervisor), files_before);
    }

    #[test]
    fn sideband_frame_reader_enforces_boundaries_and_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            let mut exact_frame = b"{}".to_vec();
            exact_frame.resize(SIDEBAND_FRAME_MAX_BYTES - 1, b' ');
            exact_frame.push(b'\n');
            let mut exact_reader = BufReader::new(exact_frame.as_slice());
            let decoded = read_sideband_frame(
                &mut exact_reader,
                SIDEBAND_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .unwrap();
            assert_eq!(decoded.len(), SIDEBAND_FRAME_MAX_BYTES - 1);

            let mut oversized_frame = b"{}".to_vec();
            oversized_frame.resize(SIDEBAND_FRAME_MAX_BYTES, b' ');
            oversized_frame.extend_from_slice(b"X\n");
            let mut oversized_reader = BufReader::new(oversized_frame.as_slice());
            let error = read_sideband_frame(
                &mut oversized_reader,
                SIDEBAND_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                format!("sideband request exceeds {SIDEBAND_FRAME_MAX_BYTES}-byte frame limit")
            );

            let mut unterminated_reader = BufReader::new(&b"{}"[..]);
            let error = read_sideband_frame(
                &mut unterminated_reader,
                SIDEBAND_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "unterminated sideband request");

            let invalid_utf8 = [0xff, b'\n'];
            let mut invalid_utf8_reader = BufReader::new(invalid_utf8.as_slice());
            let error = read_sideband_frame(
                &mut invalid_utf8_reader,
                SIDEBAND_FRAME_MAX_BYTES,
                Duration::from_secs(1),
            )
            .await
            .unwrap_err();
            assert_eq!(error.to_string(), "invalid UTF-8 sideband request");

            let (mut incomplete_client, incomplete_server) = tokio::io::duplex(64);
            incomplete_client.write_all(b"{").await.unwrap();
            let mut incomplete_reader = BufReader::new(incomplete_server);
            let error = read_sideband_frame(
                &mut incomplete_reader,
                SIDEBAND_FRAME_MAX_BYTES,
                Duration::from_millis(20),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error.to_string(),
                "sideband request timed out before newline"
            );
        });
    }

    #[test]
    fn sideband_stream_admits_required_payload_sizes() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7010, [101]))
            .unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        runtime.block_on(async {
            for content in [
                "x".repeat(1024),
                "x".repeat(64 * 1024),
                "x".repeat(1024 * 1024),
                "\0".repeat(1024 * 1024),
            ] {
                let request_payload = format!(
                    "{}\n",
                    encode_request(&SidebandRequest::SendInput {
                        name: caller.session.clone(),
                        input: content.clone(),
                    })
                    .unwrap()
                );
                assert!(request_payload.len() <= SIDEBAND_FRAME_MAX_BYTES);

                let (client, server) = tokio::io::duplex(16 * 1024);
                let handle = supervisor.clone();
                let request_caller = caller.clone();
                let server_task = tokio::spawn(async move {
                    handle_sideband_stream(handle, request_caller, server).await
                });
                let (read_half, mut write_half) = tokio::io::split(client);
                let writer = tokio::spawn(async move {
                    write_half
                        .write_all(request_payload.as_bytes())
                        .await
                        .unwrap();
                    write_half.flush().await.unwrap();
                });
                let mut response_reader = BufReader::new(read_half);
                let mut response_line = String::new();
                response_reader.read_line(&mut response_line).await.unwrap();
                writer.await.unwrap();
                server_task.await.unwrap().unwrap();

                let response = decode_response(response_line.trim()).unwrap();
                assert!(response.ok, "unexpected response: {response:?}");
                assert_eq!(inputs.lock().pop(), Some(content));
            }
        });
    }

    #[test]
    fn sideband_response_write_has_a_deadline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .unwrap();

        let error = runtime.block_on(async {
            let (_reader, writer) = tokio::io::duplex(1);
            let (_read_half, mut write_half) = tokio::io::split(writer);
            write_sideband_response(
                &mut write_half,
                b"response larger than the unread one-byte pipe",
                Duration::from_millis(20),
            )
            .await
            .unwrap_err()
        });

        assert_eq!(error.to_string(), "sideband response write timed out");
    }

    #[test]
    fn sideband_connection_limit_refuses_excess_and_recovers_capacity() {
        let slots = Arc::new(Semaphore::new(SIDEBAND_MAX_CONNECTIONS));
        let mut permits = (0..SIDEBAND_MAX_CONNECTIONS)
            .map(|_| reserve_sideband_connection(&slots).expect("capacity unexpectedly exhausted"))
            .collect::<Vec<_>>();

        assert!(reserve_sideband_connection(&slots).is_none());
        permits.pop();
        assert!(reserve_sideband_connection(&slots).is_some());
    }

    #[test]
    fn supervisor_startup_removes_legacy_bearer_artifacts_only() {
        let root = tempfile::tempdir().expect("create legacy cleanup root");
        let runtime_dir = root.path().join("runtime");
        let legacy_mailbox = runtime_dir.join("sideband");
        fs::create_dir_all(&legacy_mailbox).expect("create legacy mailbox");
        fs::write(legacy_mailbox.join("master-token"), b"legacy bearer")
            .expect("write legacy mailbox bearer");
        fs::write(
            runtime_dir.join("control-plane.json"),
            b"legacy master bearer",
        )
        .expect("write legacy master control-plane file");
        fs::write(
            runtime_dir.join("control-plane-claude.json"),
            b"legacy pane bearer",
        )
        .expect("write legacy pane control-plane file");
        fs::write(runtime_dir.join("keep.txt"), b"unrelated")
            .expect("write unrelated runtime file");

        let supervisor = test_supervisor_with_root(root.path().to_path_buf());

        assert!(!legacy_mailbox.exists());
        assert!(!runtime_dir.join("control-plane.json").exists());
        assert!(!runtime_dir.join("control-plane-claude.json").exists());
        assert_eq!(
            fs::read(runtime_dir.join("keep.txt")).expect("read unrelated runtime file"),
            b"unrelated"
        );
        drop(supervisor);
    }

    #[test]
    fn supervisor_refuses_to_recursively_delete_a_legacy_named_directory() {
        let root = tempfile::tempdir().expect("create legacy directory cleanup root");
        let runtime_dir = root.path().join("runtime");
        let working_root = root.path().join("work");
        fs::create_dir_all(&working_root).expect("create cleanup test working root");
        let legacy_directory = runtime_dir.join("control-plane.json");
        fs::create_dir_all(&legacy_directory).expect("create legacy-named directory");
        let sentinel = legacy_directory.join("must-remain.txt");
        fs::write(&sentinel, b"not a legacy credential file").expect("write directory sentinel");

        let result = SupervisorHandle::new(SupervisorConfig {
            working_root,
            runtime_dir,
            pane_mcp_executable: None,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
        });
        let error = match result {
            Ok(_) => panic!("startup must reject a legacy-named directory"),
            Err(error) => error,
        };

        assert!(
            error
                .to_string()
                .contains("refusing to recursively remove legacy control-plane directory"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&sentinel).expect("directory sentinel must remain"),
            b"not a legacy credential file"
        );
    }

    #[test]
    fn legacy_control_plane_symlink_is_removed_without_following_its_target() {
        let root = tempfile::tempdir().expect("create legacy symlink cleanup root");
        let runtime_dir = root.path().join("runtime");
        let working_root = root.path().join("work");
        fs::create_dir_all(&working_root).expect("create symlink test working root");
        fs::create_dir_all(&runtime_dir).expect("create runtime directory");
        let target = root.path().join("outside-target");
        fs::create_dir_all(&target).expect("create symlink target directory");
        let sentinel = target.join("must-remain.txt");
        fs::write(&sentinel, b"target sentinel").expect("write symlink target");
        let link = runtime_dir.join("control-plane-pane.json");
        #[cfg(windows)]
        create_windows_junction(&link, &target);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("create Unix legacy credential symlink");

        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root,
            runtime_dir,
            pane_mcp_executable: None,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
        })
        .expect("startup should remove the symlink itself");

        assert!(!link.exists());
        assert_eq!(
            fs::read(&sentinel).expect("symlink target must remain"),
            b"target sentinel"
        );
        drop(supervisor);
    }

    #[cfg(windows)]
    #[test]
    fn supervisor_rejects_a_runtime_junction_before_touching_its_target() {
        let root = tempfile::tempdir().expect("create runtime-junction test root");
        let working_root = root.path().join("work");
        let junction_target = root.path().join("outside-runtime-target");
        fs::create_dir_all(&working_root).expect("create junction test working root");
        fs::create_dir_all(&junction_target).expect("create runtime junction target");
        let sentinel = junction_target.join("must-remain.txt");
        fs::write(&sentinel, b"runtime target sentinel").expect("write runtime sentinel");
        let target_sddl_before = windows_path_sddl(&junction_target);
        let runtime_junction = root.path().join("runtime");
        create_windows_junction(&runtime_junction, &junction_target);

        let result = SupervisorHandle::new(SupervisorConfig {
            working_root,
            runtime_dir: runtime_junction.clone(),
            pane_mcp_executable: None,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
        });
        let error = match result {
            Ok(_) => panic!("startup must reject a runtime-directory junction"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("symlink or reparse point"),
            "{error:#}"
        );
        assert_eq!(
            fs::read(&sentinel).expect("runtime target sentinel must remain"),
            b"runtime target sentinel"
        );
        assert_eq!(windows_path_sddl(&junction_target), target_sddl_before);
        assert!(!junction_target.join("audit.jsonl").exists());
        assert!(!junction_target.join("desktop-instance.lock").exists());
        fs::remove_dir(&runtime_junction).expect("remove runtime test junction");
    }

    #[cfg(windows)]
    #[test]
    fn legacy_sideband_junction_is_unlinked_without_touching_its_target() {
        let root = tempfile::tempdir().expect("create sideband-junction test root");
        let runtime_dir = root.path().join("runtime");
        let working_root = root.path().join("work");
        let target = root.path().join("outside-sideband-target");
        fs::create_dir_all(&runtime_dir).expect("create safe runtime directory");
        fs::create_dir_all(&working_root).expect("create sideband test working root");
        fs::create_dir_all(&target).expect("create sideband junction target");
        let sentinel = target.join("must-remain.txt");
        fs::write(&sentinel, b"sideband target sentinel").expect("write sideband sentinel");
        let target_sddl_before = windows_path_sddl(&target);
        let sideband_junction = runtime_dir.join("sideband");
        create_windows_junction(&sideband_junction, &target);

        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root,
            runtime_dir,
            pane_mcp_executable: None,
            heartbeat_interval: None,
            auto_restart_on_stall_sessions: None,
            auto_restart_stall_threshold: None,
        })
        .expect("startup should unlink the legacy sideband junction itself");

        assert!(!sideband_junction.exists());
        assert_eq!(
            fs::read(&sentinel).expect("sideband target sentinel must remain"),
            b"sideband target sentinel"
        );
        assert_eq!(windows_path_sddl(&target), target_sddl_before);
        drop(supervisor);
    }

    #[cfg(windows)]
    struct KillWindowsChild(std::process::Child);

    #[cfg(windows)]
    impl Drop for KillWindowsChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(windows)]
    struct WindowsJobBackedPty {
        job: pty_host::ProcessJob,
        process_id: u32,
    }

    #[cfg(windows)]
    impl PtySessionTrait for WindowsJobBackedPty {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult {
            Ok(input.len())
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            self.job.terminate()
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn contains_process(&self, process: &pty_host::PinnedProcess) -> Result<bool> {
            self.job.contains_process(process)
        }
    }

    #[cfg(windows)]
    struct BlockingWindowsJobPty {
        job: pty_host::ProcessJob,
        process_id: u32,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Arc<(Mutex<bool>, Condvar)>,
        killed: Arc<AtomicBool>,
    }

    #[cfg(windows)]
    struct NonReleasingBlockingWindowsJobPty {
        job: pty_host::ProcessJob,
        process_id: u32,
        entered: Mutex<Option<mpsc::Sender<()>>>,
        release: Arc<(Mutex<bool>, Condvar)>,
        kill_count: Arc<AtomicUsize>,
    }

    #[cfg(windows)]
    impl PtySessionTrait for BlockingWindowsJobPty {
        fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
            if let Some(entered) = self.entered.lock().take() {
                let _ = entered.send(());
            }
            let (released, signal) = &*self.release;
            let mut released = released.lock();
            while !*released {
                signal.wait(&mut released);
            }
            Err(PtyWriteError::new(
                0,
                "blocking PTY write interrupted by shutdown",
            ))
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            self.killed.store(true, Ordering::SeqCst);
            let (released, signal) = &*self.release;
            *released.lock() = true;
            signal.notify_all();
            self.job.terminate()
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn contains_process(&self, process: &pty_host::PinnedProcess) -> Result<bool> {
            self.job.contains_process(process)
        }
    }

    #[cfg(windows)]
    impl PtySessionTrait for NonReleasingBlockingWindowsJobPty {
        fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
            if let Some(entered) = self.entered.lock().take() {
                let _ = entered.send(());
            }
            let (released, signal) = &*self.release;
            let mut released = released.lock();
            while !*released {
                signal.wait(&mut released);
            }
            Err(PtyWriteError::new(
                0,
                "blocking PTY write released after bounded shutdown",
            ))
        }

        fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
            Ok(())
        }

        fn kill(&self) -> Result<()> {
            if self.kill_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(anyhow!(
                    "injected process-scope termination failure without writer release"
                ));
            }
            self.job.terminate()
        }

        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            Some(self.process_id)
        }

        fn contains_process(&self, process: &pty_host::PinnedProcess) -> Result<bool> {
            self.job.contains_process(process)
        }
    }

    #[cfg(windows)]
    fn windows_test_pipe_endpoint(label: &str) -> String {
        format!(
            r"\\.\pipe\prim1-{label}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        )
    }

    #[cfg(windows)]
    fn windows_pipe_name(endpoint: &str) -> &str {
        endpoint
            .strip_prefix(r"\\.\pipe\")
            .expect("test endpoint must be a local Windows pipe")
    }

    #[cfg(windows)]
    fn powershell_single_quoted(value: &str) -> String {
        value.replace('\'', "''")
    }

    #[cfg(windows)]
    fn spawn_pipe_client_that_waits(endpoint: &str) -> KillWindowsChild {
        let script = format!(
            "$client = [System.IO.Pipes.NamedPipeClientStream]::new('.', '{}', [System.IO.Pipes.PipeDirection]::InOut); \
             $client.Connect(5000); \
             Start-Sleep -Seconds 60",
            powershell_single_quoted(windows_pipe_name(endpoint)),
        );
        std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(KillWindowsChild)
            .expect("spawn real Windows pipe client")
    }

    #[cfg(windows)]
    #[test]
    fn windows_named_pipe_reports_kernel_client_process_id_and_exit() {
        let endpoint = windows_test_pipe_endpoint("caller-pid");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("build pipe test runtime");
        let server = runtime.block_on(async {
            create_windows_pipe_server(&endpoint, true).expect("create real named-pipe server")
        });
        let mut child = spawn_pipe_client_that_waits(&endpoint);
        runtime.block_on(async {
            tokio::time::timeout(Duration::from_secs(5), server.connect())
                .await
                .expect("real pipe client did not connect")
                .expect("connect real pipe client");
        });

        let process = pane_process_from_windows_pipe(&server)
            .expect("derive caller from GetNamedPipeClientProcessId");
        assert_eq!(process.pid(), child.0.id());
        assert!(process.is_alive().expect("query live pipe caller"));

        child.0.kill().expect("kill real pipe client");
        child.0.wait().expect("wait for real pipe client exit");
        assert!(!process.is_alive().expect("query exited pipe caller"));

        let supervisor = test_supervisor();
        let error = supervisor
            .resolve_pane_caller(process)
            .err()
            .expect("exited pipe caller must be rejected");
        assert!(error.to_string().contains("has exited"), "{error:#}");
    }

    #[cfg(windows)]
    #[test]
    fn unaffiliated_named_pipe_client_is_rejected_before_sending_a_frame() {
        let supervisor = test_supervisor();
        let endpoint = windows_test_pipe_endpoint("unaffiliated");
        let status = supervisor
            .start_control_plane_at(Some(endpoint))
            .expect("start real control plane");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .expect("build pipe client runtime");

        let response = runtime.block_on(async {
            let client = tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&status.endpoint)
                .expect("connect unaffiliated client");
            let mut reader = BufReader::new(client);
            let mut response_line = String::new();
            tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut response_line))
                .await
                .expect("server waited for a request frame before rejecting caller")
                .expect("read access-denied response");
            decode_response(response_line.trim()).expect("decode access-denied response")
        });

        assert!(!response.ok);
        assert_eq!(response.message, "sideband access denied");
        supervisor.shutdown().expect("stop real control plane");
    }

    #[cfg(windows)]
    #[test]
    fn job_assigned_named_pipe_child_is_accepted_end_to_end() {
        use std::io::Write as _;

        let root = tempfile::tempdir().expect("create assigned-client test root");
        let supervisor = test_supervisor_with_root(root.path().to_path_buf());
        let endpoint = windows_test_pipe_endpoint("assigned");
        let status = supervisor
            .start_control_plane_at(Some(endpoint))
            .expect("start real control plane");
        let response_path = root.path().join("assigned-response.json");
        let request = encode_request(&SidebandRequest::Ping {}).expect("encode ping request");
        let script = format!(
            "[void][Console]::In.ReadLine(); \
             $client = [System.IO.Pipes.NamedPipeClientStream]::new('.', '{}', [System.IO.Pipes.PipeDirection]::InOut); \
             $client.Connect(5000); \
             $writer = [System.IO.StreamWriter]::new($client); \
             $writer.AutoFlush = $true; \
             $writer.WriteLine('{}'); \
             $reader = [System.IO.StreamReader]::new($client); \
             $response = $reader.ReadLine(); \
             [System.IO.File]::WriteAllText('{}', $response)",
            powershell_single_quoted(windows_pipe_name(&status.endpoint)),
            powershell_single_quoted(&request),
            powershell_single_quoted(&response_path.display().to_string()),
        );
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(KillWindowsChild)
            .expect("spawn assigned pipe child");
        let process_id = child.0.id();
        let mut release = child.0.stdin.take().expect("capture assigned child stdin");
        let job = pty_host::ProcessJob::new().expect("create assigned child job");
        let pinned_child = PinnedProcess::open(process_id).expect("pin assigned pipe child");
        job.assign_process(&pinned_child)
            .expect("assign pipe child to session job");
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(process_id),
            Box::new(WindowsJobBackedPty { job, process_id }),
        );
        release
            .write_all(b"connect\n")
            .expect("release assigned pipe child");
        release.flush().expect("flush assigned child gate");
        drop(release);

        let deadline = Instant::now() + Duration::from_secs(8);
        let response_text = loop {
            match fs::read_to_string(&response_path) {
                Ok(response) if !response.is_empty() => break response,
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(20));
                }
                Ok(_) | Err(_) => panic!("assigned pipe child did not receive a response"),
            }
        };
        let response = decode_response(&response_text).expect("decode assigned child response");
        assert!(response.ok, "assigned child was rejected: {response:?}");
        assert_eq!(response.message, "pong");

        supervisor
            .shutdown()
            .expect("stop assigned-client supervisor");
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_interrupts_authorized_blocking_pipe_write_before_listener_join() {
        use std::io::Write as _;

        let root = tempfile::tempdir().expect("create blocking-write test root");
        let supervisor = test_supervisor_with_root(root.path().to_path_buf());
        let endpoint = windows_test_pipe_endpoint("blocking-write");
        let status = supervisor
            .start_control_plane_at(Some(endpoint.clone()))
            .expect("start blocking-write control plane");
        let claude_alias = test_session_alias(&supervisor, "claude");
        let request = encode_request(&SidebandRequest::SendInput {
            name: claude_alias,
            input: "force blocking PTY write".into(),
        })
        .expect("encode blocking input request");
        let script = format!(
            "[void][Console]::In.ReadLine(); \
             $client = [System.IO.Pipes.NamedPipeClientStream]::new('.', '{}', [System.IO.Pipes.PipeDirection]::InOut); \
             $client.Connect(5000); \
             $writer = [System.IO.StreamWriter]::new($client); \
             $writer.AutoFlush = $true; \
             $writer.WriteLine('{}'); \
             $reader = [System.IO.StreamReader]::new($client); \
             [void]$reader.ReadLine()",
            powershell_single_quoted(windows_pipe_name(&status.endpoint)),
            powershell_single_quoted(&request),
        );
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(KillWindowsChild)
            .expect("spawn blocking pipe child");
        let process_id = child.0.id();
        let mut release_child = child.0.stdin.take().expect("capture child gate");
        let job = pty_host::ProcessJob::new().expect("create blocking child job");
        let pinned_child = PinnedProcess::open(process_id).expect("pin blocking pipe child");
        job.assign_process(&pinned_child)
            .expect("assign blocking child to session job");
        let (entered_tx, entered_rx) = mpsc::channel();
        let release_write = Arc::new((Mutex::new(false), Condvar::new()));
        let killed = Arc::new(AtomicBool::new(false));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(process_id),
            Box::new(BlockingWindowsJobPty {
                job,
                process_id,
                entered: Mutex::new(Some(entered_tx)),
                release: release_write.clone(),
                killed: killed.clone(),
            }),
        );
        release_child
            .write_all(b"connect\n")
            .expect("release blocking pipe child");
        release_child.flush().expect("flush child gate");
        drop(release_child);
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("authorized sideband request never entered the blocking PTY write");

        let shutdown_supervisor = supervisor.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let shutdown = thread::spawn(move || {
            let _ = done_tx.send(shutdown_supervisor.shutdown());
        });
        let shutdown_result = done_rx
            .recv_timeout(Duration::from_secs(3))
            .unwrap_or_else(|_| {
                let (released, signal) = &*release_write;
                *released.lock() = true;
                signal.notify_all();
                panic!("shutdown did not interrupt the blocked PTY write before listener join")
            });
        shutdown_result.expect("shutdown after blocking write failed");
        shutdown.join().expect("shutdown thread panicked");
        assert!(killed.load(Ordering::SeqCst));

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("build replacement pipe runtime");
        runtime.block_on(async {
            let replacement = create_windows_pipe_server(&endpoint, true)
                .expect("control-plane endpoint was not reusable after shutdown");
            drop(replacement);
        });
    }

    #[cfg(windows)]
    #[test]
    fn shutdown_retains_an_unjoined_listener_when_a_pipe_writer_cannot_be_interrupted() {
        use std::io::Write as _;

        let root = tempfile::tempdir().expect("create bounded-listener test root");
        let supervisor = test_supervisor_with_root(root.path().to_path_buf());
        supervisor.set_sideband_write_timeout_for_tests(Duration::from_secs(60));
        supervisor.set_stop_kill_timeout_for_tests(Duration::from_millis(50));
        let session_id = test_session_id(&supervisor, "claude");
        let endpoint = windows_test_pipe_endpoint("bounded-listener-stop");
        let status = supervisor
            .start_control_plane_at(Some(endpoint.clone()))
            .expect("start bounded-listener control plane");
        let claude_alias = test_session_alias(&supervisor, "claude");
        let request = encode_request(&SidebandRequest::SendInput {
            name: claude_alias,
            input: "hold the listener runtime open".into(),
        })
        .expect("encode blocking input request");
        let script = format!(
            "[void][Console]::In.ReadLine(); \
             $client = [System.IO.Pipes.NamedPipeClientStream]::new('.', '{}', [System.IO.Pipes.PipeDirection]::InOut); \
             $client.Connect(5000); \
             $writer = [System.IO.StreamWriter]::new($client); \
             $writer.AutoFlush = $true; \
             $writer.WriteLine('{}'); \
             $reader = [System.IO.StreamReader]::new($client); \
             [void]$reader.ReadLine()",
            powershell_single_quoted(windows_pipe_name(&status.endpoint)),
            powershell_single_quoted(&request),
        );
        let mut child = std::process::Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(KillWindowsChild)
            .expect("spawn bounded-listener pipe child");
        let process_id = child.0.id();
        let mut release_child = child.0.stdin.take().expect("capture child gate");
        let job = pty_host::ProcessJob::new().expect("create blocking child job");
        let pinned_child = PinnedProcess::open(process_id).expect("pin blocking pipe child");
        job.assign_process(&pinned_child)
            .expect("assign blocking child to session job");
        let (entered_tx, entered_rx) = mpsc::channel();
        let release_write = Arc::new((Mutex::new(false), Condvar::new()));
        let kill_count = Arc::new(AtomicUsize::new(0));
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(process_id),
            Box::new(NonReleasingBlockingWindowsJobPty {
                job,
                process_id,
                entered: Mutex::new(Some(entered_tx)),
                release: release_write.clone(),
                kill_count: kill_count.clone(),
            }),
        );
        release_child
            .write_all(b"connect\n")
            .expect("release blocking pipe child");
        release_child.flush().expect("flush child gate");
        drop(release_child);
        entered_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("authorized request never entered the non-releasing writer");

        let started = Instant::now();
        let error = supervisor
            .shutdown()
            .expect_err("unproved PTY cleanup and listener join must be reported");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "shutdown exceeded its bounded listener deadline: {:?}",
            started.elapsed()
        );
        assert!(
            error
                .to_string()
                .contains("control-plane listener did not stop"),
            "unexpected bounded shutdown error: {error:#}"
        );
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);
        assert!(
            supervisor.inner.control_plane_listener.lock().is_some(),
            "a timed-out listener join must retain the exact join handle for retry"
        );

        let (released, signal) = &*release_write;
        *released.lock() = true;
        signal.notify_all();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let retry_ready = {
                let listener_finished = supervisor
                    .inner
                    .control_plane_listener
                    .lock()
                    .as_ref()
                    .and_then(|listener| listener.thread.as_ref())
                    .is_some_and(thread::JoinHandle::is_finished);
                let lifecycle_clear = supervisor
                    .inner
                    .slots
                    .lock()
                    .get_by_id(session_id)
                    .unwrap()
                    .lifecycle_operation
                    .is_none();
                listener_finished && lifecycle_clear
            };
            if retry_ready {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "released listener or termination worker did not finish"
            );
            thread::sleep(Duration::from_millis(10));
        }

        supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(2));
        supervisor
            .shutdown()
            .expect("repeated shutdown must join the retained listener and prove termination");
        assert_eq!(kill_count.load(Ordering::SeqCst), 2);
        assert!(supervisor.inner.control_plane_listener.lock().is_none());
        drop(pinned_child);
    }

    #[cfg(windows)]
    #[test]
    fn tokenless_control_plane_start_is_idempotent_and_shutdown_joins_listener() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let endpoint = windows_test_pipe_endpoint("lifecycle");
        let first = supervisor
            .start_control_plane_at(Some(endpoint.clone()))
            .expect("start tokenless control plane");
        let second = supervisor
            .start_control_plane_at(Some(windows_test_pipe_endpoint("ignored")))
            .expect("repeat tokenless control-plane startup");

        assert_eq!(first, second);
        assert_eq!(
            events
                .lock()
                .iter()
                .filter(|event| matches!(event, RuntimeEvent::ControlPlaneReady { .. }))
                .count(),
            1
        );
        assert!(
            fs::read_dir(supervisor.runtime_dir())
                .expect("read tokenless runtime directory")
                .filter_map(|entry| entry.ok())
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with("control-plane")),
            "tokenless startup must not recreate bearer files"
        );

        supervisor.shutdown().expect("join control-plane listener");
        assert!(supervisor.snapshot().control_plane.is_none());

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("build replacement pipe runtime");
        runtime.block_on(async {
            let replacement = create_windows_pipe_server(&endpoint, true)
                .expect("listener handle survived shutdown join");
            drop(replacement);
        });

        let error = supervisor
            .start_control_plane_at(Some(endpoint))
            .expect_err("terminal supervisor must not restart its listener");
        assert_eq!(error.to_string(), "supervisor has shut down");
        supervisor
            .shutdown()
            .expect("repeated shutdown is idempotent");
    }

    #[cfg(windows)]
    #[test]
    fn control_plane_start_and_shutdown_share_one_terminal_lifecycle_order() {
        let supervisor = test_supervisor();
        let events = capture_runtime_events(&supervisor);
        let endpoint = windows_test_pipe_endpoint("start-shutdown-order");
        let (prepared_tx, prepared_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        *supervisor.inner.control_plane_after_prepare.lock() = Some(Box::new(move || {
            prepared_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        }));

        let start_supervisor = supervisor.clone();
        let start_endpoint = endpoint.clone();
        let start =
            thread::spawn(move || start_supervisor.start_control_plane_at(Some(start_endpoint)));
        prepared_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("control-plane start did not reach the prepared barrier");
        assert!(
            supervisor.inner.shutdown_lifecycle.try_lock().is_none(),
            "start must hold the terminal lifecycle lock through listener preparation"
        );

        let shutdown_supervisor = supervisor.clone();
        let (shutdown_invoked_tx, shutdown_invoked_rx) = mpsc::sync_channel(1);
        let (shutdown_done_tx, shutdown_done_rx) = mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            shutdown_invoked_tx.send(()).unwrap();
            shutdown_done_tx
                .send(shutdown_supervisor.shutdown())
                .unwrap();
        });
        shutdown_invoked_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("shutdown thread did not start");
        assert!(
            shutdown_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "shutdown must not overtake an admitted control-plane start"
        );
        assert!(!supervisor.inner.shutdown_started.load(Ordering::Acquire));

        release_tx.send(()).unwrap();
        let started = start.join().expect("control-plane start thread panicked");
        assert!(started.is_ok(), "admitted start should linearize first");
        shutdown_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("shutdown did not complete after start released")
            .expect("shutdown failed");
        shutdown.join().expect("shutdown thread panicked");

        assert!(supervisor.snapshot().control_plane.is_none());
        assert_eq!(
            events
                .lock()
                .iter()
                .filter(|event| matches!(event, RuntimeEvent::ControlPlaneReady { .. }))
                .count(),
            1
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("build endpoint reuse runtime");
        runtime.block_on(async {
            let replacement = create_windows_pipe_server(&endpoint, true)
                .expect("ordered shutdown must release the prepared endpoint");
            drop(replacement);
        });
    }

    #[cfg(windows)]
    #[test]
    fn direct_executable_resolution_honors_name_preference_before_path_order() {
        let root = tempfile::tempdir().expect("create executable-resolution fixture");
        let early = root.path().join("early");
        let late = root.path().join("late");
        fs::create_dir_all(&early).unwrap();
        fs::create_dir_all(&late).unwrap();
        fs::write(early.join("pwsh.exe"), b"fixture").unwrap();
        fs::write(late.join("powershell.exe"), b"fixture").unwrap();
        let search_path = std::env::join_paths([&early, &late]).unwrap();

        let resolved = find_direct_executable_on_path(
            &["powershell.exe", "pwsh.exe"],
            search_path.as_os_str(),
        )
        .expect("resolve preferred executable");
        let expected = child_process_path(
            &fs::canonicalize(late.join("powershell.exe")).expect("qualify preferred fixture"),
        )
        .to_path_buf();

        assert_eq!(PathBuf::from(resolved), expected);
    }

    #[cfg(windows)]
    #[test]
    fn production_generic_terminal_starts_the_preferred_direct_powershell() {
        let root = tempfile::tempdir().expect("create production terminal fixture");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        supervisor.set_executable_resolver_for_tests(Arc::new(HostDriverExecutableResolver));
        let events = capture_runtime_events(&supervisor);
        let session = create_test_session(
            &supervisor,
            "Production terminal",
            DriverKind::GenericTerminal,
            shared_types::PermissionProfile::Normal,
        );

        let started = supervisor
            .start_session_by_id(session.session_id)
            .expect("spawn the production-resolved Generic terminal");
        assert_eq!(started.lifecycle_state, LifecycleState::Ready);
        supervisor
            .send_input(SendInputRequest {
                session_id: session.session_id,
                input: "\u{1b}[1;1RWrite-Output ([string]::Concat('PRIM1_','GENERIC_','READY'))\r"
                    .into(),
            })
            .expect("release ConPTY startup and run the terminal probe");

        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let output = events
                .lock()
                .iter()
                .filter_map(|event| match event {
                    RuntimeEvent::SessionOutput {
                        identity, chunk, ..
                    } if identity.session_id == session.session_id => Some(chunk.as_str()),
                    _ => None,
                })
                .collect::<String>();
            if output.contains("PRIM1_GENERIC_READY") {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "production Generic terminal did not execute the probe; output: {output:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }

        supervisor
            .shutdown()
            .expect("terminate the production Generic terminal");
    }

    #[cfg(windows)]
    #[test]
    fn control_plane_refuses_first_instance_squatting() {
        let endpoint = windows_test_pipe_endpoint("squatting");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("build squatter runtime");
        let squatter = runtime.block_on(async {
            create_windows_pipe_server(&endpoint, false).expect("create preexisting pipe squatter")
        });
        let supervisor = test_supervisor();

        let error = supervisor
            .start_control_plane_at(Some(endpoint.clone()))
            .expect_err("first-instance startup must reject a preexisting pipe");
        assert!(
            error.to_string().contains("failed to create named pipe"),
            "unexpected squatting error: {error:#}"
        );
        assert!(supervisor.snapshot().control_plane.is_none());

        drop(squatter);
        supervisor
            .start_control_plane_at(Some(endpoint))
            .expect("start after squatter releases endpoint");
        supervisor.shutdown().expect("stop post-squatter listener");
    }

    #[cfg(windows)]
    #[test]
    fn production_spawn_identity_authenticates_the_real_powershell_client_to_the_rust_listener() {
        let root = tempfile::tempdir().expect("create cross-boundary identity test root");
        let supervisor = test_supervisor_with_root(root.path().to_path_buf());
        let events = capture_runtime_events(&supervisor);
        let endpoint = windows_test_pipe_endpoint("rust-powershell-identity");
        let status = supervisor
            .start_control_plane_at(Some(endpoint))
            .expect("start real Rust control-plane listener");
        let control_plane_script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join("scripts")
            .join("control-plane.ps1");
        assert!(
            control_plane_script.is_file(),
            "actual control-plane.ps1 was not found at {}",
            control_plane_script.display()
        );

        let rust_server = pty_host::PinnedProcess::open(std::process::id())
            .expect("pin Rust control-plane server process");
        let expected_identity_receipt = format!(
            "PRIM1_SERVER_IDENTITY={}|{}|{}",
            rust_server.pid(),
            rust_server.creation_time_filetime(),
            status.endpoint
        );
        let child_command = format!(
            "$ErrorActionPreference = 'Stop'; \
             Write-Output ('PRIM1_SERVER_IDENTITY=' + $env:PRIM1_CONTROL_PLANE_SERVER_PID + '|' + $env:PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME + '|' + $env:PRIM1_CONTROL_PLANE_ENDPOINT); \
             & '{}' -Action ping -Quiet -PassThruJson; \
             exit $LASTEXITCODE",
            powershell_single_quoted(&control_plane_script.display().to_string()),
        );
        let powershell = find_direct_executable(&["powershell.exe"])
            .expect("resolve a concrete PowerShell executable");
        supervisor.set_executable_resolver_for_tests(Arc::new(FixedExecutableResolver(
            ResolvedLaunchProgram {
                program: powershell,
                prefix_args: vec![
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-ExecutionPolicy".into(),
                    "Bypass".into(),
                    "-Command".into(),
                    child_command,
                ],
            },
        )));
        let (session_id, session_alias) = {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get("claude").expect("test Claude slot");
            (slot.session_id, slot.definition.alias.clone())
        };
        {
            let mut slots = supervisor.inner.slots.lock();
            let definition = &mut slots
                .get_mut("claude")
                .expect("default claude slot")
                .definition;
            definition.label = "Rust-to-PowerShell identity receipt".into();
            definition.driver = DriverKind::GenericTerminal;
        }

        let started = supervisor
            .start_session_by_id(session_id)
            .expect("spawn PowerShell through the production PTY path");
        assert_eq!(started.lifecycle_state, LifecycleState::Ready);
        wait_for_event_count(&events, 1, |event| {
            matches!(
                event,
                RuntimeEvent::SessionOutput { session, chunk, .. }
                    if session == &session_alias && chunk.contains("\u{1b}[6n")
            )
        });
        supervisor
            .send_input(SendInputRequest {
                session_id,
                input: "\u{1b}[1;1R".into(),
            })
            .expect("release the real ConPTY cursor handshake");

        let deadline = Instant::now() + Duration::from_secs(15);
        let observed_output = loop {
            let output = events
                .lock()
                .iter()
                .filter_map(|event| match event {
                    RuntimeEvent::SessionOutput { session, chunk, .. }
                        if session == &session_alias =>
                    {
                        Some(chunk.as_str())
                    }
                    _ => None,
                })
                .collect::<String>();
            if output.contains(&expected_identity_receipt)
                && output.contains("\"ok\":true")
                && output.contains("\"message\":\"pong\"")
            {
                break output;
            }
            assert!(
                Instant::now() < deadline,
                "production-injected identity did not authenticate scripts/control-plane.ps1 to the real Rust listener; output: {output:?}"
            );
            thread::sleep(Duration::from_millis(20));
        };

        assert!(
            observed_output.contains(&expected_identity_receipt),
            "child did not receive the Rust-generated server identity: {observed_output:?}"
        );
        assert!(
            observed_output.contains("pong"),
            "authenticated PowerShell client did not reach the listener response: {observed_output:?}"
        );
        supervisor
            .shutdown()
            .expect("stop cross-boundary identity supervisor");
    }

    #[cfg(windows)]
    #[test]
    fn control_plane_pipe_dacl_is_protected_and_current_user_only() {
        use std::os::windows::io::AsRawHandle as _;

        let endpoint = windows_test_pipe_endpoint("private-dacl");
        let current_sid = current_user_sid_string().expect("resolve current test SID");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .expect("build private-pipe runtime");
        let sddl = runtime.block_on(async {
            let server =
                create_windows_pipe_server(&endpoint, true).expect("create private named pipe");
            security_descriptor_sddl_for_handle(server.as_raw_handle() as _)
        });

        assert_current_user_only_sddl(&sddl, &current_sid);
    }

    fn bracketed_paste_test_binding() -> RunBinding {
        RunBinding {
            session_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            generation: 7,
        }
    }

    fn bracketed_paste_tracker(binding: RunBinding) -> BracketedPasteRunState {
        let mut tracker = BracketedPasteRunState::default();
        tracker.begin_run(binding);
        tracker
    }

    fn valid_character_boundaries(value: &str) -> Vec<usize> {
        value
            .char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(value.len()))
            .collect()
    }

    #[test]
    fn bracketed_paste_parser_accepts_every_split_combined_parameters_and_c1_csi() {
        for (sequence, expected) in [
            ("\x1b[?1;2004;25h", BracketedPasteMode::Enabled),
            ("\x1b[?25;2004l", BracketedPasteMode::Disabled),
            ("\u{009b}?1;2004;25h", BracketedPasteMode::Enabled),
            ("\u{009b}?2004l", BracketedPasteMode::Disabled),
        ] {
            for split in valid_character_boundaries(sequence) {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, &sequence[..split]);
                tracker.observe_output(binding, &sequence[split..]);
                assert_eq!(
                    tracker.mode_for(binding),
                    expected,
                    "sequence {sequence:?} failed at split {split}"
                );
            }
        }

        let binding = bracketed_paste_test_binding();
        let mut tracker = bracketed_paste_tracker(binding);
        tracker.observe_output(binding, "\x1b[?2004h\x1b[?2004l\u{009b}?2004h");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "the last complete DECSET/DECRST must win"
        );
    }

    #[test]
    fn bracketed_paste_parser_matches_xterm_string_exits_at_every_character_boundary() {
        let fixtures = [
            (
                "\x1b]hidden\x1b[?2004l\x07",
                BracketedPasteMode::Disabled,
                "OSC ESC exits into ESCAPE before DECRST and BEL",
            ),
            (
                "\x1b]hidden\x1b[?2004l\x1b\\",
                BracketedPasteMode::Disabled,
                "OSC ESC exits into ESCAPE before DECRST and ST",
            ),
            (
                "\x1bPhidden\x1b[?2004l\x1b\\",
                BracketedPasteMode::Disabled,
                "DCS ESC exits into ESCAPE before DECRST",
            ),
            (
                "\x1bXhidden\x1b[?2004l\x1b\\",
                BracketedPasteMode::Disabled,
                "SOS ESC enters ESCAPE",
            ),
            (
                "\x1b^hidden\x1b[?2004l\x1b\\",
                BracketedPasteMode::Disabled,
                "PM ESC enters ESCAPE",
            ),
            (
                "\x1b_hidden\x1b[?2004l\x1b\\",
                BracketedPasteMode::Disabled,
                "APC ESC enters ESCAPE",
            ),
            (
                "\u{009d}hidden\u{009b}?2004l\x07",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 OSC before BEL",
            ),
            (
                "\u{009d}hidden\u{009b}?2004l\u{009c}",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 OSC before ST",
            ),
            (
                "\u{0090}hidden\u{009b}?2004l\u{009c}",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 DCS",
            ),
            (
                "\u{0098}hidden\u{009b}?2004l\u{009c}",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 SOS",
            ),
            (
                "\u{009e}hidden\u{009b}?2004l\u{009c}",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 PM",
            ),
            (
                "\u{009f}hidden\u{009b}?2004l\u{009c}",
                BracketedPasteMode::Disabled,
                "C1 CSI globally exits C1 APC",
            ),
        ];

        for (fixture, expected, label) in fixtures {
            for split in valid_character_boundaries(fixture) {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, "\x1b[?2004h");
                tracker.observe_output(binding, &fixture[..split]);
                tracker.observe_output(binding, &fixture[split..]);
                assert_eq!(
                    tracker.mode_for(binding),
                    expected,
                    "{label} diverged at split {split}"
                );
            }
        }

        let binding = bracketed_paste_test_binding();
        let mut tracker = bracketed_paste_tracker(binding);
        tracker.observe_output(binding, "\x1b[?2004h");
        tracker.observe_output(binding, "\x1b]hidden\x1bnot-a-CSI\x07");
        assert_eq!(tracker.mode_for(binding), BracketedPasteMode::Enabled);
        tracker.observe_output(binding, "\x1b[?2004l");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Disabled,
            "a real DECRST after OSC's ESC exit was not observed"
        );
    }

    #[test]
    fn bracketed_paste_parser_matches_xterm_c0_del_and_cancel_transitions() {
        let ignored_controls = ('\u{0000}'..='\u{0017}')
            .chain(std::iter::once('\u{0019}'))
            .chain('\u{001c}'..='\u{001f}')
            .chain(std::iter::once('\u{007f}'));
        for ignored in ignored_controls {
            let binding = bracketed_paste_test_binding();
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(binding, &format!("\x1b[?2004{ignored}l"));
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Disabled,
                "CSI did not remain active across {ignored:?}"
            );

            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(binding, &format!("\x1b{ignored}[?2004l"));
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Disabled,
                "ESCAPE did not remain active across {ignored:?}"
            );

            let overflow = format!(
                "\x1b[?{}",
                "2".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 8)
            );
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(binding, &overflow);
            assert_eq!(tracker.parser, TerminalModeParserState::CsiDiscard);
            let mut encoded = [0; 4];
            tracker.observe_output(binding, ignored.encode_utf8(&mut encoded));
            assert_eq!(
                tracker.parser,
                TerminalModeParserState::CsiDiscard,
                "CSI discard did not remain active across {ignored:?}"
            );
            tracker.observe_output(binding, "h");
            assert_eq!(tracker.parser, TerminalModeParserState::Ground);
            assert_eq!(tracker.mode_for(binding), BracketedPasteMode::Unknown);
        }

        for cancel in ['\u{0018}', '\u{001a}'] {
            let binding = bracketed_paste_test_binding();
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(binding, &format!("\x1b[?2004{cancel}l"));
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Enabled,
                "{cancel:?} did not cancel the partial CSI"
            );
            assert_eq!(tracker.parser, TerminalModeParserState::Ground);

            tracker.observe_output(binding, &format!("\x1bPopaque{cancel}\x1b[?2004l"));
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Disabled,
                "{cancel:?} did not cancel DCS before the following real DECRST"
            );
        }
    }

    #[test]
    fn every_relevant_c1_control_is_global_from_every_parser_state() {
        let overflow = format!(
            "\x1b[?{}",
            "2".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 8)
        );
        let overlong_osc = format!(
            "\x1b]{}",
            "x".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 1)
        );
        let starting_states = [
            ("ground", String::new()),
            ("escape", "\x1b".to_string()),
            ("csi", "\x1b[?20".to_string()),
            ("csi-discard", overflow),
            ("osc", "\x1b]text".to_string()),
            ("dcs", "\x1bPtext".to_string()),
            ("sos", "\x1bXtext".to_string()),
            ("pm", "\x1b^text".to_string()),
            ("apc", "\x1b_text".to_string()),
            ("string-discard", overlong_osc),
        ];
        let global_transitions = [
            (
                '\u{009b}',
                TerminalModeParserState::Csi(DecPrivateModeParser::new()),
                "CSI",
            ),
            (
                '\u{009d}',
                BracketedPasteRunState::terminal_string(TerminalStringKind::Osc),
                "OSC",
            ),
            (
                '\u{0090}',
                BracketedPasteRunState::terminal_string(TerminalStringKind::Dcs),
                "DCS",
            ),
            (
                '\u{0098}',
                BracketedPasteRunState::terminal_string(TerminalStringKind::Sos),
                "SOS",
            ),
            (
                '\u{009e}',
                BracketedPasteRunState::terminal_string(TerminalStringKind::Pm),
                "PM",
            ),
            (
                '\u{009f}',
                BracketedPasteRunState::terminal_string(TerminalStringKind::Apc),
                "APC",
            ),
            ('\u{009c}', TerminalModeParserState::Ground, "ST"),
        ];
        let global_executables = ('\u{0080}'..='\u{008f}')
            .chain('\u{0091}'..='\u{0097}')
            .chain('\u{0099}'..='\u{009a}')
            .collect::<Vec<_>>();

        for (state_label, prefix) in starting_states {
            for (control, expected, control_label) in global_transitions {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, &prefix);
                let mut encoded = [0; 4];
                tracker.observe_output(binding, control.encode_utf8(&mut encoded));
                assert_eq!(
                    tracker.parser, expected,
                    "C1 {control_label} was not global from {state_label}"
                );
            }
            for control in &global_executables {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, &prefix);
                let mut encoded = [0; 4];
                tracker.observe_output(binding, control.encode_utf8(&mut encoded));
                assert_eq!(
                    tracker.parser,
                    TerminalModeParserState::Ground,
                    "executable C1 {control:?} was not global from {state_label}"
                );
            }
            for cancel in ['\u{0018}', '\u{001a}'] {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, &prefix);
                let mut encoded = [0; 4];
                tracker.observe_output(binding, cancel.encode_utf8(&mut encoded));
                assert_eq!(
                    tracker.parser,
                    TerminalModeParserState::Ground,
                    "{cancel:?} was not global from {state_label}"
                );
            }
        }
    }

    #[test]
    fn dcs_escape_enters_escape_and_dispatches_real_csi_at_every_boundary() {
        let sequence = "\x1bPhidden\x1b[?2004h\x1b\\";
        for split in valid_character_boundaries(sequence) {
            let binding = bracketed_paste_test_binding();
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004l");
            tracker.observe_output(binding, &sequence[..split]);
            tracker.observe_output(binding, &sequence[split..]);
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Enabled,
                "DCS ESC did not enter ESCAPE at split {split}"
            );
        }
    }

    #[test]
    fn normal_string_terminators_preserve_mode_at_every_character_boundary() {
        for (sequence, label) in [
            ("\x1b]hidden\x07", "OSC BEL"),
            ("\x1b]hidden\x1b\\", "OSC ST"),
            ("\x1bPhidden\x1b\\", "DCS ST"),
            ("\x1bXhidden\x1b\\", "SOS ST"),
            ("\x1b^hidden\x1b\\", "PM ST"),
            ("\x1b_hidden\x1b\\", "APC ST"),
            ("\u{009d}hidden\x07", "C1 OSC BEL"),
            ("\u{009d}hidden\u{009c}", "C1 OSC ST"),
            ("\u{0090}hidden\u{009c}", "C1 DCS ST"),
            ("\u{0098}hidden\u{009c}", "C1 SOS ST"),
            ("\u{009e}hidden\u{009c}", "C1 PM ST"),
            ("\u{009f}hidden\u{009c}", "C1 APC ST"),
        ] {
            for split in valid_character_boundaries(sequence) {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, "\x1b[?2004h");
                tracker.observe_output(binding, &sequence[..split]);
                tracker.observe_output(binding, &sequence[split..]);
                assert_eq!(
                    tracker.mode_for(binding),
                    BracketedPasteMode::Enabled,
                    "{label} changed mode at split {split}"
                );
                assert_eq!(tracker.parser, TerminalModeParserState::Ground);
            }
        }
    }

    #[test]
    fn overflow_discard_and_real_decset_recover_at_every_string_and_character_boundary() {
        let fixtures = [
            ("\x1b]hidden\x07", "OSC BEL"),
            ("\x1b]hidden\x1b\\", "OSC ST"),
            ("\x1bPhidden\x1b\\", "DCS"),
            ("\x1bXhidden\x1b\\", "SOS"),
            ("\x1b^hidden\x1b\\", "PM"),
            ("\x1b_hidden\x1b\\", "APC"),
            ("\u{009d}hidden\x07", "C1 OSC/BEL"),
            ("\u{009d}hidden\u{009c}", "C1 OSC/ST"),
            ("\u{0090}hidden\u{009c}", "C1 DCS/ST"),
            ("\u{0098}hidden\u{009c}", "C1 SOS/ST"),
            ("\u{009e}hidden\u{009c}", "C1 PM/ST"),
            ("\u{009f}hidden\u{009c}", "C1 APC/ST"),
        ];
        let overflow = format!(
            "\x1b[?{}",
            "2".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 8)
        );
        let recovery = "\x1b[?2004h";

        for (fixture, label) in fixtures {
            let discarded_sequence = format!("{overflow}{fixture}");
            for discard_split in valid_character_boundaries(&discarded_sequence) {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, "\x1b[?2004h");
                tracker.observe_output(binding, &discarded_sequence[..discard_split]);
                tracker.observe_output(binding, &discarded_sequence[discard_split..]);
                assert_eq!(
                    tracker.mode_for(binding),
                    BracketedPasteMode::Unknown,
                    "{label} escaped overflow discard at split {discard_split}"
                );

                let discarded = tracker;
                for recovery_split in valid_character_boundaries(recovery) {
                    let mut recovering = discarded;
                    recovering.observe_output(binding, &recovery[..recovery_split]);
                    recovering.observe_output(binding, &recovery[recovery_split..]);
                    assert_eq!(
                        recovering.mode_for(binding),
                        BracketedPasteMode::Enabled,
                        "{label} failed recovery at discard split {discard_split}, DECSET split {recovery_split}"
                    );
                }
            }
        }
    }

    #[test]
    fn bracketed_paste_parser_fails_unknown_on_csi_overflow_and_recovers_after_a_real_final() {
        let binding = bracketed_paste_test_binding();
        let mut tracker = bracketed_paste_tracker(binding);
        tracker.observe_output(binding, "\x1b[?2004h");
        assert_eq!(tracker.mode_for(binding), BracketedPasteMode::Enabled);

        let overlong = format!(
            "\x1b[?{}h",
            "2".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 8)
        );
        for character in overlong.chars() {
            let mut encoded = [0; 4];
            tracker.observe_output(binding, character.encode_utf8(&mut encoded));
        }
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Unknown,
            "overlong partial control must fail closed"
        );

        tracker.observe_output(binding, "\x1b[?2004h");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "parser did not recover after consuming the overlong CSI final"
        );
        tracker.observe_output(binding, "\x1b[?2004:1l");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "malformed subparameter lookalike must not toggle mode"
        );
        tracker.observe_output(binding, "\x1b[?2004l");
        assert_eq!(tracker.mode_for(binding), BracketedPasteMode::Disabled);

        tracker.observe_output(binding, "\x1b[?2004h");
        tracker.observe_output(
            binding,
            &format!(
                "\x1b]{}",
                "unterminated".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS))
            ),
        );
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "bounded terminal-string discard must preserve the last observed mode"
        );
        tracker.observe_output(binding, "still discarded\x07");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "terminal-string discard changed mode while waiting for BEL"
        );
        tracker.observe_output(binding, "\x1b[?2004l");
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Disabled,
            "a real DECRST after terminal-string discard was not observed"
        );
    }

    #[test]
    fn terminal_string_discard_preserves_mode_and_real_transitions_at_every_boundary() {
        let payload = format!(
            "{}[?2004l",
            "x".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 1)
        );
        let fixtures = [
            (format!("\x1b]{payload}\x07"), "OSC BEL"),
            (format!("\x1b]{payload}\x1b\\"), "OSC ST"),
            (format!("\x1bP{payload}\x1b\\"), "DCS ST"),
            (format!("\x1bX{payload}\x1b\\"), "SOS ST"),
            (format!("\x1b^{payload}\x1b\\"), "PM ST"),
            (format!("\x1b_{payload}\x1b\\"), "APC ST"),
            (format!("\u{009d}{payload}\u{009c}"), "C1 OSC ST"),
            (format!("\u{0090}{payload}\u{009c}"), "C1 DCS ST"),
            (format!("\u{0098}{payload}\u{009c}"), "C1 SOS ST"),
            (format!("\u{009e}{payload}\u{009c}"), "C1 PM ST"),
            (format!("\u{009f}{payload}\u{009c}"), "C1 APC ST"),
        ];

        for (sequence, label) in fixtures {
            for split in valid_character_boundaries(&sequence) {
                let binding = bracketed_paste_test_binding();
                let mut tracker = bracketed_paste_tracker(binding);
                tracker.observe_output(binding, "\x1b[?2004h");
                tracker.observe_output(binding, &sequence[..split]);
                tracker.observe_output(binding, &sequence[split..]);
                assert_eq!(
                    tracker.mode_for(binding),
                    BracketedPasteMode::Enabled,
                    "{label} discard changed mode at split {split}"
                );
                assert_eq!(
                    tracker.parser,
                    TerminalModeParserState::Ground,
                    "{label} discard did not terminate at split {split}"
                );

                tracker.observe_output(binding, "\x1b[?2004l");
                assert_eq!(
                    tracker.mode_for(binding),
                    BracketedPasteMode::Disabled,
                    "{label} made the retained mode sticky at split {split}"
                );
            }
        }
    }

    #[test]
    fn terminal_string_discard_honors_xterm_global_exits() {
        let overflow = "x".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 1);
        for sequence in [
            format!("\x1b]{overflow}\x1b[?2004l"),
            format!("\x1bP{overflow}\u{009b}?2004l"),
            format!("\x1bX{overflow}\u{0018}\x1b[?2004l"),
            format!("\x1b^{overflow}\u{001a}\x1b[?2004l"),
        ] {
            let binding = bracketed_paste_test_binding();
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(binding, &sequence);
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Disabled,
                "discard ignored a real xterm global exit followed by DECRST"
            );
        }
    }

    #[test]
    fn prompt_ready_mode_survives_bounded_terminal_metadata() {
        let binding = bracketed_paste_test_binding();
        let mut tracker = bracketed_paste_tracker(binding);
        tracker.observe_output(
            binding,
            "\x1b[?25h\x1b[?25l\x1b[?2004h\x1b[?1004h\x1b[?2031h",
        );
        tracker.observe_output(
            binding,
            &format!(
                "\x1b]{}\x07",
                "terminal metadata ".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) / 4 + 1)
            ),
        );
        assert_eq!(
            tracker.mode_for(binding),
            BracketedPasteMode::Enabled,
            "prompt-ready DECSET was lost while discarding bounded terminal metadata"
        );
    }

    #[test]
    fn csi_overflow_discard_routes_every_c1_string_opener_without_parsing_lookalikes() {
        for (opener, label) in [
            ('\u{009d}', "OSC"),
            ('\u{0090}', "DCS"),
            ('\u{0098}', "SOS"),
            ('\u{009e}', "PM"),
            ('\u{009f}', "APC"),
        ] {
            let binding = bracketed_paste_test_binding();
            let mut tracker = bracketed_paste_tracker(binding);
            tracker.observe_output(binding, "\x1b[?2004h");
            tracker.observe_output(
                binding,
                &format!(
                    "\x1b[?{}",
                    "2".repeat(usize::from(TERMINAL_MODE_CONTROL_MAX_CHARS) + 8)
                ),
            );
            assert_eq!(tracker.mode_for(binding), BracketedPasteMode::Unknown);

            tracker.observe_output(binding, &format!("{opener}hidden[?2004h\u{009c}"));
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Unknown,
                "{label} lookalike escaped CSI overflow discard"
            );
            tracker.observe_output(binding, "\x1b[?2004h");
            assert_eq!(
                tracker.mode_for(binding),
                BracketedPasteMode::Enabled,
                "real DECSET after {label} ST did not recover"
            );
        }
    }

    #[test]
    fn bracketed_paste_state_resets_and_rejects_every_stale_run_binding_component() {
        let old = bracketed_paste_test_binding();
        let mut tracker = bracketed_paste_tracker(old);
        tracker.observe_output(old, "\x1b[?2004h");
        assert_eq!(tracker.mode_for(old), BracketedPasteMode::Enabled);

        for stale in [
            RunBinding {
                session_id: Uuid::new_v4(),
                ..old
            },
            RunBinding {
                run_id: Uuid::new_v4(),
                ..old
            },
            RunBinding {
                generation: old.generation + 1,
                ..old
            },
        ] {
            tracker.observe_output(stale, "\x1b[?2004l");
            assert_eq!(
                tracker.mode_for(old),
                BracketedPasteMode::Enabled,
                "stale binding mutated the active run"
            );
        }

        let successor = RunBinding {
            session_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            generation: old.generation,
        };
        tracker.begin_run(successor);
        assert_eq!(
            tracker.mode_for(successor),
            BracketedPasteMode::Unknown,
            "new run inherited predecessor mode"
        );
        tracker.observe_output(old, "\x1b[?2004h");
        assert_eq!(tracker.mode_for(successor), BracketedPasteMode::Unknown);
        tracker.observe_output(successor, "\x1b[?2004l");
        assert_eq!(tracker.mode_for(successor), BracketedPasteMode::Disabled);
    }

    #[test]
    fn raw_pty_output_updates_only_the_exact_active_run_bracketed_paste_tracker() {
        let supervisor = test_supervisor();
        let (pty, _inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
            BracketedPasteMode::Unknown,
        );
        let (generation, run_id, binding) = {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get("codex").unwrap();
            let run_id = slot.run_id.unwrap();
            (
                slot.generation,
                run_id,
                RunBinding {
                    session_id: slot.session_id,
                    run_id,
                    generation: slot.generation,
                },
            )
        };

        supervisor.handle_pty_event(
            "codex",
            generation,
            run_id,
            PtyEvent::Output("\x1b[?20".into()),
        );
        supervisor.handle_pty_event("codex", generation, run_id, PtyEvent::Output("04h".into()));
        assert_eq!(
            supervisor
                .inner
                .slots
                .lock()
                .get("codex")
                .unwrap()
                .bracketed_paste
                .mode_for(binding),
            BracketedPasteMode::Enabled
        );

        supervisor.handle_pty_event(
            "codex",
            generation + 1,
            run_id,
            PtyEvent::Output("\x1b[?2004l".into()),
        );
        assert_eq!(
            supervisor
                .inner
                .slots
                .lock()
                .get("codex")
                .unwrap()
                .bracketed_paste
                .mode_for(binding),
            BracketedPasteMode::Enabled,
            "stale PTY output changed the exact active run tracker"
        );
    }

    #[test]
    fn raw_mode_scanner_runs_even_when_output_event_sequence_is_exhausted() {
        let supervisor = test_supervisor();
        let (pty, _inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            pty,
            BracketedPasteMode::Unknown,
        );
        let (generation, run_id, binding) = {
            let mut slots = supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            slot.run_event_sequence = u64::MAX - 2;
            let run_id = slot.run_id.unwrap();
            (
                slot.generation,
                run_id,
                RunBinding {
                    session_id: slot.session_id,
                    run_id,
                    generation: slot.generation,
                },
            )
        };

        supervisor.handle_pty_event(
            "codex",
            generation,
            run_id,
            PtyEvent::Output("\x1b[?2004h".into()),
        );

        let slots = supervisor.inner.slots.lock();
        let slot = slots.get("codex").unwrap();
        assert_eq!(
            slot.bracketed_paste.mode_for(binding),
            BracketedPasteMode::Enabled
        );
        assert_eq!(slot.run_event_sequence, u64::MAX - 2);
    }

    #[test]
    fn production_start_binds_a_fresh_unknown_bracketed_paste_state_before_spawn() {
        let supervisor = test_supervisor();
        let (pty, _inputs) = recording_pty_session(std::process::id());
        supervisor.set_pty_spawner_for_tests(Arc::new(QueuePtySpawner::new(vec![pty])));

        let started = start_test_session(&supervisor, "codex").unwrap();

        let slots = supervisor.inner.slots.lock();
        let slot = slots.get("codex").unwrap();
        let binding = RunBinding {
            session_id: slot.session_id,
            run_id: slot.run_id.unwrap(),
            generation: slot.generation,
        };
        assert_eq!(started.run_id, Some(binding.run_id));
        assert_eq!(slot.bracketed_paste.binding, Some(binding));
        assert_eq!(
            slot.bracketed_paste.mode_for(binding),
            BracketedPasteMode::Unknown
        );
    }

    #[test]
    fn direct_operator_route_rejects_unknown_and_disabled_before_message_side_effects() {
        for mode in [BracketedPasteMode::Unknown, BracketedPasteMode::Disabled] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_process_id_and_mode(
                &supervisor,
                "codex",
                DriverKind::Codex,
                None,
                pty,
                mode,
            );
            let before = slots_mutation_probe(&supervisor);
            let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();
            let events = capture_runtime_events(&supervisor);
            let session_id = test_session_id(&supervisor, "codex");

            let error = supervisor
                .route_operator_message(OperatorRouteMessageRequest {
                    recipient_id: session_id,
                    content: "must remain atomic".into(),
                })
                .unwrap_err();

            assert!(
                error.to_string().contains(match mode {
                    BracketedPasteMode::Unknown => "bracketed-paste mode is unknown",
                    BracketedPasteMode::Disabled => "bracketed-paste mode is disabled",
                    BracketedPasteMode::Enabled => unreachable!(),
                }),
                "{error:#}"
            );
            assert_eq!(slots_mutation_probe(&supervisor), before);
            assert!(inputs.lock().is_empty());
            assert!(events.lock().is_empty());
            assert_eq!(
                fs::read(supervisor.audit_log_path()).unwrap_or_default(),
                audit_before
            );
        }
    }

    #[test]
    fn mode_rejection_preserves_unrelated_liveness_reconciliation_without_message_side_effects() {
        let supervisor = test_supervisor();
        let claude_alias = test_session_alias(&supervisor, "claude");
        let codex_id = test_session_id(&supervisor, "codex");
        let (stale_pty, stale_inputs) = recording_pty_session(u32::MAX);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(u32::MAX),
            stale_pty,
            BracketedPasteMode::Unknown,
        );
        let (target_pty, target_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            target_pty,
            BracketedPasteMode::Unknown,
        );
        let target_before = slot_mutation_probe(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "must fail before routing".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("bracketed-paste mode is unknown"),
            "{error:#}"
        );
        assert_eq!(slot_mutation_probe(&supervisor, "codex"), target_before);
        assert!(stale_inputs.lock().is_empty());
        assert!(target_inputs.lock().is_empty());
        let events = events.lock();
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit { session, .. } if session == &claude_alias
        )));
        assert!(!events.iter().any(|event| match event {
            RuntimeEvent::DispatchAttempt { action, .. } => action == "route_message",
            RuntimeEvent::RoutedMessage { .. } | RuntimeEvent::RouteDelivery { .. } => true,
            _ => false,
        }));
        drop(events);
        let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        let audited_events = audit
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            audited_events
                .iter()
                .any(|event| event["event"] == "session_exit")
        );
        assert!(!audited_events.iter().any(|event| matches!(
            event["event"].as_str(),
            Some("dispatch_attempt" | "routed_message" | "route_delivery")
        )));
    }

    #[test]
    fn addressed_stale_run_reconciles_before_rejecting_without_message_side_effects() {
        let supervisor = test_supervisor();
        let codex_id = test_session_id(&supervisor, "codex");
        let codex_alias = test_session_alias(&supervisor, "codex");
        let (stale_pty, stale_inputs) = recording_pty_session(u32::MAX);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(u32::MAX),
            stale_pty,
            BracketedPasteMode::Unknown,
        );
        let bracketed_before = supervisor
            .inner
            .slots
            .lock()
            .get("codex")
            .unwrap()
            .bracketed_paste;
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: codex_id,
                content: "stale target".into(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("is not running"), "{error:#}");
        assert!(stale_inputs.lock().is_empty());
        let slot = supervisor.inner.slots.lock();
        let target = slot.get("codex").unwrap();
        assert!(target.running.is_none());
        assert_eq!(target.bracketed_paste, bracketed_before);
        drop(slot);
        let events = events.lock();
        assert!(events.iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit { session, .. } if session == &codex_alias
        )));
        assert!(!events.iter().any(|event| match event {
            RuntimeEvent::DispatchAttempt { action, .. } => action == "route_message",
            RuntimeEvent::RoutedMessage { .. } | RuntimeEvent::RouteDelivery { .. } => true,
            _ => false,
        }));
        drop(events);
        let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        let audited_events = audit
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert!(
            audited_events
                .iter()
                .any(|event| event["event"] == "session_exit")
        );
        assert!(!audited_events.iter().any(|event| matches!(
            event["event"].as_str(),
            Some("dispatch_attempt" | "routed_message" | "route_delivery")
        )));
    }

    #[test]
    fn operator_route_mode_flip_after_preflight_is_revalidated_before_zero_write() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            pty,
        );
        let hook_supervisor = supervisor.clone();
        let hook_ran = Arc::new(AtomicBool::new(false));
        let hook_ran_in_callback = hook_ran.clone();
        supervisor.set_run_input_before_commit_for_tests(move || {
            hook_ran_in_callback.store(true, Ordering::SeqCst);
            let mut slots = hook_supervisor.inner.slots.lock();
            let slot = slots.get_mut("codex").unwrap();
            let binding = RunBinding {
                session_id: slot.session_id,
                run_id: slot.run_id.unwrap(),
                generation: slot.generation,
            };
            slot.bracketed_paste.observe_output(binding, "\x1b[?2004l");
        });
        let session_id = test_session_id(&supervisor, "codex");

        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: session_id,
                content: "mode flips after route preflight".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("bracketed-paste mode is disabled")
        );
        assert!(hook_ran.load(Ordering::SeqCst));
        assert!(inputs.lock().is_empty());
    }

    #[test]
    fn raw_operator_input_remains_available_when_bracketed_paste_is_unknown_or_disabled() {
        for mode in [BracketedPasteMode::Unknown, BracketedPasteMode::Disabled] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_process_id_and_mode(
                &supervisor,
                "codex",
                DriverKind::Codex,
                None,
                pty,
                mode,
            );

            supervisor
                .send_input(SendInputRequest {
                    session_id: test_session_id(&supervisor, "codex"),
                    input: "raw operator typing".into(),
                })
                .unwrap();

            assert_eq!(inputs.lock().as_slice(), &["raw operator typing"]);
        }
    }

    #[test]
    fn raw_sideband_input_remains_available_when_bracketed_paste_is_disabled() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
            BracketedPasteMode::Disabled,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(7017, [101]))
            .unwrap();

        let response = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::SendInput {
                name: caller.session.clone(),
                input: "raw sideband typing".into(),
            },
        );

        assert!(response.ok, "{}", response.message);
        assert_eq!(inputs.lock().as_slice(), &["raw sideband typing"]);
    }

    #[test]
    fn observed_unsafe_work_state_rejects_synthetic_delivery_before_message_side_effects() {
        for (work_state, detail) in [
            (WorkState::Blocked, "workspace_trust"),
            (WorkState::ErrorLoop, "rate_limit"),
        ] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_bracketed_paste_enabled(
                &supervisor,
                "codex",
                DriverKind::Codex,
                pty,
            );
            set_session_dispatch_state(
                &supervisor,
                "codex",
                LifecycleState::Ready,
                Some(work_state),
            );
            supervisor
                .inner
                .slots
                .lock()
                .get_mut("codex")
                .unwrap()
                .work_detail = Some(detail.into());
            let before = slots_mutation_probe(&supervisor);
            let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();
            let events = capture_runtime_events(&supervisor);
            let session_id = test_session_id(&supervisor, "codex");

            let error = supervisor
                .route_operator_message(OperatorRouteMessageRequest {
                    recipient_id: session_id,
                    content: "must not enter a modal prompt".into(),
                })
                .unwrap_err();

            assert!(
                error.to_string().contains(&format!(
                    "work state is {} ({detail})",
                    work_state_alert_label(Some(work_state))
                )),
                "{error:#}"
            );
            assert!(
                error
                    .to_string()
                    .contains("use raw terminal input to resolve the prompt"),
                "{error:#}"
            );
            assert_eq!(slots_mutation_probe(&supervisor), before);
            assert!(inputs.lock().is_empty());
            assert!(events.lock().is_empty());
            assert_eq!(
                fs::read(supervisor.audit_log_path()).unwrap_or_default(),
                audit_before
            );
        }
    }

    #[test]
    fn codex_workspace_trust_output_blocks_the_next_route_before_any_write() {
        let supervisor = test_supervisor();
        let codex_alias = test_session_alias(&supervisor, "codex");
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            pty,
        );
        let events = capture_runtime_events(&supervisor);

        handle_current_pty_event(
            &supervisor,
            "codex",
            0,
            PtyEvent::Output(
                "Do you trust the contents of this directory?\n› 1. Yes, continue\n  2. No, exit\nPress enter to continue"
                    .into(),
            ),
        );

        assert!(work_state_events(&events).iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionWorkState {
                session,
                state: WorkState::Blocked,
                detail: Some(detail),
                ..
            } if session == &codex_alias && detail == "workspace_trust"
        )));
        events.lock().clear();
        let before = slots_mutation_probe(&supervisor);
        let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();

        let session_id = test_session_id(&supervisor, "codex");
        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: session_id,
                content: "do not type into trust UI".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("work state is blocked (workspace_trust)"),
            "{error:#}"
        );
        assert_eq!(slots_mutation_probe(&supervisor), before);
        assert!(inputs.lock().is_empty());
        assert!(events.lock().is_empty());
        assert_eq!(
            fs::read(supervisor.audit_log_path()).unwrap_or_default(),
            audit_before
        );
    }

    #[test]
    fn missing_catalog_persists_zero_sessions_and_explicit_empty_reopens_empty() {
        let root = tempfile::tempdir().expect("create zero-session catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        assert!(supervisor.snapshot().sessions.is_empty());

        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let first_bytes = fs::read(&catalog_path).expect("read initial empty catalog");
        let first_catalog: serde_json::Value =
            serde_json::from_slice(&first_bytes).expect("parse initial empty catalog");
        assert_eq!(first_catalog["schema_version"], 1);
        assert_eq!(first_catalog["sessions"], serde_json::json!([]));
        drop(supervisor);

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path()))
            .expect("reopen explicit empty catalog");
        assert!(reopened.snapshot().sessions.is_empty());
        assert_eq!(
            fs::read(&catalog_path).expect("read reopened empty catalog"),
            first_bytes,
            "an explicit empty catalog must not be reseeded or rewritten"
        );
    }

    #[test]
    fn catalog_crud_preserves_ids_order_duplicate_labels_and_closed_only_fields() {
        let root = tempfile::tempdir().expect("create catalog CRUD root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let workspace = root.path().join("chosen workspace");
        let alternate = root.path().join("alternate workspace");
        fs::create_dir_all(&workspace).unwrap();
        fs::create_dir_all(&alternate).unwrap();
        let persisted_workspace = supervisor
            .set_workspace_preference(&workspace)
            .expect("persist workspace preference");
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let claude = create_test_session(
            &supervisor,
            "Twin",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let codex = create_test_session(
            &supervisor,
            "Twin",
            DriverKind::Codex,
            shared_types::PermissionProfile::Normal,
        );
        assert_ne!(claude.session_id, codex.session_id);
        assert_ne!(claude.alias, codex.alias);
        assert_eq!(
            claude.label, codex.label,
            "duplicate labels are presentation-only"
        );

        let (live_pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        install_mock_running_session_by_id_with_mode(
            &supervisor,
            codex.session_id,
            DriverKind::Codex,
            None,
            live_pty,
            BracketedPasteMode::Unknown,
        );
        let renamed = supervisor
            .rename_session(codex.session_id, "Renamed while live")
            .expect("rename a live session");
        assert!(renamed.running);
        assert_eq!(renamed.label, "Renamed while live");
        supervisor
            .stop_session_by_id(codex.session_id)
            .expect("close live session before changing launch fields");

        let updated = supervisor
            .set_permission_profile(codex.session_id, shared_types::PermissionProfile::Unsafe)
            .expect("set explicit unsafe profile while closed");
        assert_eq!(
            updated.permission_profile,
            shared_types::PermissionProfile::Unsafe
        );
        let updated = supervisor
            .set_session_working_directory(codex.session_id, &alternate)
            .expect("set qualified session working directory while closed");
        assert_eq!(
            updated.working_dir,
            child_process_path(&fs::canonicalize(&alternate).unwrap())
                .to_string_lossy()
                .into_owned()
        );
        supervisor
            .move_session(codex.session_id, 0)
            .expect("move session to first tab");
        supervisor
            .delete_session(claude.session_id)
            .expect("delete the other closed session");

        let before_reopen = supervisor.snapshot();
        assert_eq!(before_reopen.workspace_preference, persisted_workspace);
        assert_eq!(before_reopen.sessions.len(), 1);
        assert_eq!(before_reopen.sessions[0].session_id, codex.session_id);
        assert!(
            specs.lock().is_empty(),
            "catalog CRUD must never spawn a PTY"
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_value: serde_json::Value =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        let persisted = catalog_value["sessions"][0].as_object().unwrap();
        assert_eq!(
            persisted.keys().cloned().collect::<BTreeSet<_>>(),
            BTreeSet::from([
                "driver".to_string(),
                "label".to_string(),
                "permission_profile".to_string(),
                "session_id".to_string(),
                "working_directory".to_string(),
            ])
        );
        assert!(!catalog_value.to_string().contains("run_id"));
        assert!(!catalog_value.to_string().contains("process_id"));
        assert!(!catalog_value.to_string().contains("argv"));
        assert!(!catalog_value.to_string().contains("env"));
        drop(supervisor);

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path()))
            .expect("reopen persisted CRUD catalog");
        let after_reopen = reopened.snapshot();
        assert_eq!(after_reopen.workspace_preference, persisted_workspace);
        assert_eq!(after_reopen.sessions.len(), 1);
        let restored = &after_reopen.sessions[0];
        assert_eq!(restored.session_id, codex.session_id);
        assert_eq!(restored.alias, codex.alias);
        assert_eq!(restored.label, "Renamed while live");
        assert_eq!(restored.driver, DriverKind::Codex);
        assert_eq!(
            restored.permission_profile,
            shared_types::PermissionProfile::Unsafe
        );
        assert_eq!(restored.lifecycle_state, LifecycleState::Closed);
        assert!(!restored.running);
        assert_eq!(restored.run_id, None);
    }

    #[test]
    fn grok_session_persists_and_uses_the_native_permission_contract() {
        let root = tempfile::tempdir().expect("create Grok catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let session = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: None,
                driver: DriverKind::Grok,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: None,
            })
            .expect("create native Grok session");
        assert_eq!(session.label, "Grok");
        let session = supervisor
            .set_permission_profile(session.session_id, shared_types::PermissionProfile::Unsafe)
            .expect("select measured Grok unsafe profile");
        assert_eq!(
            session.permission_profile,
            shared_types::PermissionProfile::Unsafe
        );

        supervisor
            .start_session_by_id(session.session_id)
            .expect("start captured Grok session");
        let specs = specs.lock();
        assert_eq!(specs.len(), 1);
        assert_eq!(
            specs[0].args,
            vec![
                "--permission-mode",
                "bypassPermissions",
                "--cwd",
                session.working_dir.as_str(),
            ]
        );
        drop(specs);
        supervisor
            .stop_session_by_id(session.session_id)
            .expect("stop captured Grok session");
        drop(supervisor);

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path()))
            .expect("reopen catalog containing Grok");
        let restored = &reopened.snapshot().sessions[0];
        assert_eq!(restored.session_id, session.session_id);
        assert_eq!(restored.driver, DriverKind::Grok);
        assert_eq!(
            restored.permission_profile,
            shared_types::PermissionProfile::Unsafe
        );
        assert_eq!(restored.lifecycle_state, LifecycleState::Closed);
    }

    #[test]
    fn corrupt_or_unknown_catalog_fails_visibly_without_rewrite() {
        for (name, replacement, expected_error) in [
            ("corrupt", b"{ definitely not JSON".to_vec(), "failed to parse session catalog"),
            (
                "unknown-version",
                br#"{"schema_version":99,"workspace_preference":{"canonical_path":"C:\\placeholder","identity":"placeholder"},"sessions":[]}"#.to_vec(),
                "unsupported session catalog schema version 99",
            ),
        ] {
            let root = tempfile::tempdir().expect("create invalid-catalog root");
            let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
            let catalog_path = session_catalog_path(supervisor.runtime_dir());
            drop(supervisor);
            fs::write(&catalog_path, &replacement).expect("install invalid catalog fixture");

            let error = SupervisorHandle::new(test_supervisor_config(root.path()))
                .err()
                .expect("invalid catalog must fail startup");
            assert!(
                format!("{error:#}").contains(expected_error),
                "{name}: {error:#}"
            );
            assert_eq!(
                fs::read(&catalog_path).unwrap(),
                replacement,
                "{name}: startup failure must not rewrite the catalog"
            );
        }
    }

    #[test]
    fn pre_prime_v1_catalog_without_directory_namespace_loads_without_rewrite() {
        let root = tempfile::tempdir().expect("create pre-Prime catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Pre-Prime Claude",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let mut catalog: serde_json::Value =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        catalog["workspace_preference"]
            .as_object_mut()
            .unwrap()
            .remove("namespace");
        for persisted in catalog["sessions"].as_array_mut().unwrap() {
            persisted["working_directory"]
                .as_object_mut()
                .unwrap()
                .remove("namespace");
        }
        let legacy = serde_json::to_vec_pretty(&catalog).unwrap();
        drop(supervisor);
        fs::write(&catalog_path, &legacy).unwrap();

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path()))
            .expect("load pre-Prime schema-v1 catalog");
        let restored = reopened
            .snapshot()
            .sessions
            .into_iter()
            .find(|candidate| candidate.session_id == session.session_id)
            .expect("restore legacy session identity");
        assert_eq!(restored.driver, DriverKind::Claude);
        assert_eq!(restored.lifecycle_state, LifecycleState::Closed);
        assert_eq!(fs::read(&catalog_path).unwrap(), legacy);
    }

    #[test]
    fn catalog_rejects_relative_workspace_and_blank_qualified_identity_without_rewrite() {
        let root = tempfile::tempdir().expect("create structural-catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Structural validation",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let valid: serde_json::Value =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        drop(supervisor);

        let mut relative_workspace = valid.clone();
        relative_workspace["workspace_preference"]["canonical_path"] =
            serde_json::json!("relative/workspace");
        let relative_bytes = serde_json::to_vec_pretty(&relative_workspace).unwrap();
        fs::write(&catalog_path, &relative_bytes).unwrap();
        let error = SupervisorHandle::new(test_supervisor_config(root.path()))
            .err()
            .expect("relative workspace preference must fail startup");
        assert!(format!("{error:#}").contains("workspace preference has an invalid absolute"));
        assert_eq!(fs::read(&catalog_path).unwrap(), relative_bytes);

        let mut blank_identity = valid;
        let persisted = blank_identity["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|candidate| candidate["session_id"] == session.session_id.to_string())
            .unwrap();
        persisted["working_directory"]["identity"] = serde_json::json!("   ");
        let blank_bytes = serde_json::to_vec_pretty(&blank_identity).unwrap();
        fs::write(&catalog_path, &blank_bytes).unwrap();
        let error = SupervisorHandle::new(test_supervisor_config(root.path()))
            .err()
            .expect("blank session directory identity must fail startup");
        assert!(format!("{error:#}").contains("blank persisted identity"));
        assert_eq!(fs::read(&catalog_path).unwrap(), blank_bytes);

        let mut unknown_namespace: serde_json::Value =
            serde_json::from_slice(&blank_bytes).unwrap();
        let persisted = unknown_namespace["sessions"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|candidate| candidate["session_id"] == session.session_id.to_string())
            .unwrap();
        persisted["working_directory"]["identity"] = serde_json::json!("windows:1:1");
        persisted["working_directory"]["namespace"] = serde_json::json!("future_kernel");
        let unknown_bytes = serde_json::to_vec_pretty(&unknown_namespace).unwrap();
        fs::write(&catalog_path, &unknown_bytes).unwrap();
        let error = SupervisorHandle::new(test_supervisor_config(root.path()))
            .err()
            .expect("unknown working-directory namespace must fail startup");
        assert!(format!("{error:#}").contains("failed to parse session catalog"));
        assert_eq!(fs::read(&catalog_path).unwrap(), unknown_bytes);
    }

    #[test]
    fn catalog_rejects_invalid_prime_namespace_identity_and_permission_without_rewrite() {
        let root = tempfile::tempdir().expect("create Prime structural-catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Prime structural validation",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let valid: serde_json::Value =
            serde_json::from_slice(&fs::read(&catalog_path).unwrap()).unwrap();
        drop(supervisor);

        for (name, mutate, expected) in [
            (
                "workspace-wsl-namespace",
                0_u8,
                "workspace preference must use the Windows",
            ),
            ("Prime-control-path", 1, "invalid Ubuntu path"),
            ("Prime-bad-identity", 2, "invalid Ubuntu identity"),
            ("Prime-unsafe", 3, "cannot use the unsafe permission"),
        ] {
            let mut candidate = valid.clone();
            if mutate == 0 {
                candidate["workspace_preference"] = serde_json::json!({
                    "namespace": "wsl_ubuntu",
                    "canonical_path": "/home/test",
                    "identity": "wsl_ubuntu:7:9"
                });
            } else {
                let persisted = candidate["sessions"]
                    .as_array_mut()
                    .unwrap()
                    .iter_mut()
                    .find(|entry| entry["session_id"] == session.session_id.to_string())
                    .unwrap();
                persisted["driver"] = serde_json::json!("prime");
                persisted["working_directory"] = serde_json::json!({
                    "namespace": "wsl_ubuntu",
                    "canonical_path": if mutate == 1 { "/home/test\u{0007}child" } else { "/home/test" },
                    "identity": if mutate == 2 { "wsl_ubuntu:not-a-device:9" } else { "wsl_ubuntu:7:9" }
                });
                if mutate == 3 {
                    persisted["permission_profile"] = serde_json::json!("unsafe");
                }
            }
            let bytes = serde_json::to_vec_pretty(&candidate).unwrap();
            fs::write(&catalog_path, &bytes).unwrap();
            let error = SupervisorHandle::new(test_supervisor_config(root.path()))
                .err()
                .expect("invalid Prime catalog must fail startup");
            assert!(format!("{error:#}").contains(expected), "{name}: {error:#}");
            assert_eq!(fs::read(&catalog_path).unwrap(), bytes, "{name}");
        }
    }

    #[test]
    fn replaced_workspace_preference_rejects_create_without_catalog_event_or_session_mutation() {
        let root = tempfile::tempdir().expect("create workspace-identity root");
        let selected = root.path().join("selected workspace");
        fs::create_dir_all(&selected).unwrap();
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        supervisor.set_workspace_preference(&selected).unwrap();
        fs::rename(&selected, root.path().join("original selected workspace")).unwrap();
        fs::create_dir_all(&selected).unwrap();
        let slots_before = slots_mutation_probe(&supervisor);
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_before = fs::read(&catalog_path).unwrap();
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Must reselect".into()),
                driver: DriverKind::Claude,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: None,
            })
            .unwrap_err();

        assert!(
            format!("{error:#}").contains("workspace preference changed since it was selected")
        );
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
        assert!(events.lock().is_empty());
    }

    #[test]
    fn catalog_persistence_failure_is_zero_memory_file_event_and_spawn_mutation() {
        let supervisor = empty_test_supervisor();
        let session = create_test_session(
            &supervisor,
            "Before",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_before = fs::read(&catalog_path).unwrap();
        let slots_before = slots_mutation_probe(&supervisor);
        let events = capture_runtime_events(&supervisor);
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        supervisor.fail_next_catalog_write_for_tests();

        let error = supervisor
            .rename_session(session.session_id, "After")
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected session catalog persistence failure")
        );
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
        assert!(events.lock().is_empty());
        assert!(specs.lock().is_empty());
    }

    #[test]
    fn room_catalog_crud_reopens_exact_ids_order_membership_without_feed_content_or_spawn() {
        let root = tempfile::tempdir().expect("create room catalog root");
        let supervisor = test_supervisor_with_root(root.path().to_path_buf());
        let grok = create_test_session(
            &supervisor,
            "grok",
            DriverKind::Grok,
            shared_types::PermissionProfile::Normal,
        );
        let terminal = create_test_session(
            &supervisor,
            "terminal",
            DriverKind::GenericTerminal,
            shared_types::PermissionProfile::Normal,
        );
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let first = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Twin".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let second = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Twin".into()),
                member_ids: vec![grok.session_id, terminal.session_id],
            })
            .unwrap();
        assert_ne!(first.room_id, second.room_id);
        supervisor
            .rename_room(RenameRoomRequest {
                room_id: second.room_id,
                label: "Renamed".into(),
            })
            .unwrap();
        supervisor
            .move_room(MoveRoomRequest {
                room_id: second.room_id,
                new_index: 0,
            })
            .unwrap();
        let sentinel = "ROOM-CONTENT-MUST-NOT-PERSIST-λ";
        supervisor
            .post_room_message(PostRoomMessageRequest {
                room_id: first.room_id,
                content: sentinel.into(),
            })
            .unwrap();
        let before = supervisor.snapshot();
        assert_eq!(
            before
                .rooms
                .iter()
                .map(|room| room.room_id)
                .collect::<Vec<_>>(),
            vec![second.room_id, first.room_id]
        );
        assert!(specs.lock().is_empty());
        let catalog_path = room_catalog_path(supervisor.runtime_dir());
        let catalog_bytes = fs::read(&catalog_path).unwrap();
        assert!(!String::from_utf8_lossy(&catalog_bytes).contains(sentinel));
        let old_epochs = before
            .rooms
            .iter()
            .map(|room| (room.room_id, room.feed_epoch))
            .collect::<HashMap<_, _>>();
        drop(supervisor);

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path())).unwrap();
        let after = reopened.snapshot();
        assert_eq!(
            after
                .rooms
                .iter()
                .map(|room| room.room_id)
                .collect::<Vec<_>>(),
            vec![second.room_id, first.room_id]
        );
        assert_eq!(after.rooms[0].label, "Renamed");
        for room in &after.rooms {
            assert_eq!(room.feed_oldest_sequence, 1);
            assert_eq!(room.feed_next_sequence, 1);
            assert_ne!(room.feed_epoch, old_epochs[&room.room_id]);
        }
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_bytes);
    }

    #[test]
    fn room_catalog_persistence_failure_has_zero_memory_file_event_or_spawn_mutation() {
        let supervisor = test_supervisor();
        let before = supervisor.snapshot();
        let path = room_catalog_path(supervisor.runtime_dir());
        let bytes_before = fs::read(&path).unwrap();
        let events = capture_runtime_events(&supervisor);
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        supervisor.fail_next_room_catalog_write_for_tests();

        let error = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Must not exist".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap_err();

        assert!(error.to_string().contains("injected room catalog"));
        assert_eq!(supervisor.snapshot().rooms, before.rooms);
        assert_eq!(fs::read(path).unwrap(), bytes_before);
        assert!(events.lock().is_empty());
        assert!(specs.lock().is_empty());
    }

    #[test]
    fn corrupt_or_unknown_room_catalog_fails_visibly_without_rewrite() {
        for (name, replacement, expected_error) in [
            (
                "corrupt",
                b"{ definitely not room JSON".to_vec(),
                "failed to parse room catalog",
            ),
            (
                "unknown-version",
                br#"{"schema_version":99,"rooms":[]}"#.to_vec(),
                "unsupported room catalog schema version 99",
            ),
        ] {
            let root = tempfile::tempdir().expect("create invalid-room-catalog root");
            let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
            let catalog_path = room_catalog_path(supervisor.runtime_dir());
            drop(supervisor);
            fs::write(&catalog_path, &replacement).expect("install invalid room catalog");

            let error = SupervisorHandle::new(test_supervisor_config(root.path()))
                .err()
                .expect("invalid room catalog must fail startup");
            assert!(
                format!("{error:#}").contains(expected_error),
                "{name}: {error:#}"
            );
            assert_eq!(
                fs::read(&catalog_path).unwrap(),
                replacement,
                "{name}: startup failure must not rewrite the room catalog"
            );
        }
    }

    #[test]
    fn concurrent_room_posts_publish_cursor_order_without_a_reload_race() {
        let supervisor = test_supervisor();
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Ordered feed".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let events = capture_runtime_events(&supervisor);
        let (hook_entered_tx, hook_entered_rx) = mpsc::sync_channel(0);
        let (hook_release_tx, hook_release_rx) = mpsc::sync_channel(0);
        supervisor.set_room_event_after_append_for_tests(move || {
            hook_entered_tx.send(()).unwrap();
            hook_release_rx.recv().unwrap();
        });

        let first_supervisor = supervisor.clone();
        let first = thread::spawn(move || {
            first_supervisor
                .post_room_message(PostRoomMessageRequest {
                    room_id: room.room_id,
                    content: "first".into(),
                })
                .unwrap()
        });
        hook_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first post did not reach the publish barrier");

        let second_supervisor = supervisor.clone();
        let (second_started_tx, second_started_rx) = mpsc::sync_channel(0);
        let (second_done_tx, second_done_rx) = mpsc::sync_channel(1);
        let second = thread::spawn(move || {
            second_started_tx.send(()).unwrap();
            let result = second_supervisor.post_room_message(PostRoomMessageRequest {
                room_id: room.room_id,
                content: "second".into(),
            });
            second_done_tx.send(result).unwrap();
        });
        second_started_rx.recv().unwrap();
        assert!(
            second_done_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err(),
            "a later post bypassed the earlier append-to-publish interval"
        );

        hook_release_tx.send(()).unwrap();
        first.join().unwrap();
        second_done_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("second post did not finish")
            .unwrap();
        second.join().unwrap();

        let published = events
            .lock()
            .iter()
            .filter_map(|event| match event {
                RuntimeEvent::RoomFeedEvent { feed_event } => match &feed_event.item {
                    RoomFeedItem::Message { content, .. } => {
                        Some((feed_event.cursor.sequence, content.clone()))
                    }
                    _ => None,
                },
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(published, vec![(3, "first".into()), (4, "second".into())]);
    }

    #[test]
    fn session_deletion_fails_closed_while_room_membership_exists() {
        let supervisor = test_supervisor();
        let claude = test_session_id(&supervisor, "claude");
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: None,
                member_ids: vec![claude, test_session_id(&supervisor, "codex")],
            })
            .unwrap();
        let catalog_before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();

        let error = supervisor.delete_session(claude).unwrap_err();

        assert!(error.to_string().contains("remove it from the room"));
        assert_eq!(
            fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
            catalog_before
        );
        assert!(
            supervisor
                .snapshot()
                .sessions
                .iter()
                .any(|session| session.session_id == claude)
        );
        supervisor
            .remove_room_member(RemoveRoomMemberRequest {
                room_id: room.room_id,
                session_id: claude,
            })
            .unwrap();
        supervisor.delete_session(claude).unwrap();
    }

    #[test]
    fn feed_only_room_post_writes_no_pty_and_redacts_durable_content() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(std::process::id());
        let (codex_pty, codex_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Feed only".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let sentinel = "FEED-ONLY-PRIVATE-λ-🚀";

        let result = supervisor
            .post_room_message(PostRoomMessageRequest {
                room_id: room.room_id,
                content: sentinel.into(),
            })
            .unwrap();
        let page = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room.room_id,
                cursor: None,
            })
            .unwrap();

        assert_eq!(
            result.message_id,
            match &page.events.last().unwrap().item {
                RoomFeedItem::Message {
                    message_id,
                    content,
                    recipient_ids,
                    ..
                } => {
                    assert_eq!(content, sentinel);
                    assert!(recipient_ids.is_empty());
                    *message_id
                }
                item => panic!("unexpected feed item: {item:?}"),
            }
        );
        assert!(claude_inputs.lock().is_empty());
        assert!(codex_inputs.lock().is_empty());
        let audit = fs::read_to_string(supervisor.audit_log_path()).unwrap();
        assert!(!audit.contains(sentinel));
        assert!(audit.contains("[content omitted]"));
    }

    #[test]
    fn same_label_rooms_isolate_feed_and_pty_delivery_by_room_id() {
        let supervisor = test_supervisor();
        let second_claude = create_test_session(
            &supervisor,
            "second-claude",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let second_codex = create_test_session(
            &supervisor,
            "second-codex",
            DriverKind::Codex,
            shared_types::PermissionProfile::Normal,
        );
        let (first_claude_pty, first_claude_inputs) = recording_pty_session(101);
        let (first_codex_pty, first_codex_inputs) = recording_pty_session(102);
        let (second_claude_pty, second_claude_inputs) = recording_pty_session(103);
        let (second_codex_pty, second_codex_inputs) = recording_pty_session(104);
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            first_claude_pty,
        );
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            first_codex_pty,
        );
        install_mock_running_session_by_id_with_mode(
            &supervisor,
            second_claude.session_id,
            DriverKind::Claude,
            None,
            second_claude_pty,
            BracketedPasteMode::Enabled,
        );
        install_mock_running_session_by_id_with_mode(
            &supervisor,
            second_codex.session_id,
            DriverKind::Codex,
            None,
            second_codex_pty,
            BracketedPasteMode::Enabled,
        );
        let first = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Twin".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let second = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Twin".into()),
                member_ids: vec![second_claude.session_id, second_codex.session_id],
            })
            .unwrap();
        let second_before = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: second.room_id,
                cursor: None,
            })
            .unwrap();
        let sentinel = "FIRST-ROOM-ONLY";

        let result = supervisor
            .deliver_room_message(DeliverRoomMessageRequest {
                room_id: first.room_id,
                recipients: RoomRecipientSelection::All {},
                content: sentinel.into(),
            })
            .unwrap();

        assert_eq!(result.written_count, 2);
        assert_eq!(first_claude_inputs.lock().len(), 2);
        assert_eq!(first_codex_inputs.lock().len(), 2);
        assert!(second_claude_inputs.lock().is_empty());
        assert!(second_codex_inputs.lock().is_empty());
        assert_eq!(
            supervisor
                .read_room_feed(ReadRoomFeedRequest {
                    room_id: second.room_id,
                    cursor: None,
                })
                .unwrap(),
            second_before
        );
        let first_page = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: first.room_id,
                cursor: None,
            })
            .unwrap();
        assert!(first_page.events.iter().any(|event| matches!(
            &event.item,
            RoomFeedItem::Message { content, .. } if content == sentinel
        )));
    }

    #[test]
    fn room_delivery_lease_releases_if_event_publication_unwinds() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(105);
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Unwind".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        supervisor.set_event_sink(|event| {
            if matches!(event, RuntimeEvent::RoomFeedEvent { .. }) {
                panic!("synthetic room event sink panic");
            }
        });

        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = supervisor.deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::One {
                    session_id: test_session_id(&supervisor, "claude"),
                },
                content: "must not strand the lease".into(),
            });
        }));

        assert!(unwind.is_err());
        assert!(claude_inputs.lock().is_empty());
        supervisor.set_event_sink(|_| {});
        supervisor
            .delete_room(DeleteRoomRequest {
                room_id: room.room_id,
            })
            .expect("a panicking event sink must not strand the room delivery lease");
    }

    #[test]
    fn room_send_all_preflights_every_run_then_records_exact_per_recipient_receipts() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(std::process::id());
        let (codex_pty, codex_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Exact room".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let content = "  λ room line one\r\nline two 🚀  \n";

        let result = supervisor
            .deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::All {},
                content: content.into(),
            })
            .unwrap();

        assert_eq!(result.recipient_count, 2);
        assert_eq!(result.written_count, 2);
        assert!(result.failures.is_empty());
        let payload = format!("[Room message from operator]\n{content}");
        let framed = frame_message_payload(&payload, MessageFraming::BracketedPaste);
        for inputs in [claude_inputs, codex_inputs] {
            assert_eq!(inputs.lock().as_slice(), &[framed.clone(), "\r".into()]);
        }
        let page = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room.room_id,
                cursor: None,
            })
            .unwrap();
        assert!(page.events.iter().any(|event| matches!(
            &event.item,
            RoomFeedItem::Message { message_id, content: stored, recipient_ids, .. }
                if *message_id == result.message_id && stored == content && recipient_ids.len() == 2
        )));
        assert_eq!(
            page.events
                .iter()
                .filter(|event| matches!(
                    event.item,
                    RoomFeedItem::Delivery {
                        status: RoomDeliveryStatus::Written,
                        ..
                    }
                ))
                .count(),
            2
        );
    }

    #[test]
    fn room_delivery_membership_revision_change_after_preflight_is_zero_message_write() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(std::process::id());
        let (codex_pty, codex_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let claude = test_session_id(&supervisor, "claude");
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Revision pin".into()),
                member_ids: vec![claude, test_session_id(&supervisor, "codex")],
            })
            .unwrap();
        let hook_supervisor = supervisor.clone();
        supervisor.set_room_delivery_after_preflight_for_tests(move || {
            hook_supervisor
                .remove_room_member(RemoveRoomMemberRequest {
                    room_id: room.room_id,
                    session_id: claude,
                })
                .unwrap();
        });

        let error = supervisor
            .deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::All {},
                content: "must not write".into(),
            })
            .unwrap_err();

        assert!(error.to_string().contains("membership changed"));
        assert!(claude_inputs.lock().is_empty());
        assert!(codex_inputs.lock().is_empty());
        let page = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room.room_id,
                cursor: None,
            })
            .unwrap();
        assert!(!page.events.iter().any(|event| matches!(
            &event.item,
            RoomFeedItem::Message { content, .. } if content == "must not write"
        )));
    }

    #[test]
    fn room_send_all_rejects_one_unknown_mode_before_every_message_side_effect() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(std::process::id());
        let (codex_pty, codex_inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "codex",
            DriverKind::Codex,
            None,
            codex_pty,
            BracketedPasteMode::Unknown,
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Atomic".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let before = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room.room_id,
                cursor: None,
            })
            .unwrap();
        let audit_before = fs::read(supervisor.audit_log_path()).unwrap();
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::All {},
                content: "atomically rejected".into(),
            })
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("bracketed-paste mode is unknown")
        );
        assert!(claude_inputs.lock().is_empty());
        assert!(codex_inputs.lock().is_empty());
        assert!(events.lock().is_empty());
        assert_eq!(
            supervisor
                .read_room_feed(ReadRoomFeedRequest {
                    room_id: room.room_id,
                    cursor: None,
                })
                .unwrap(),
            before
        );
        assert_eq!(fs::read(supervisor.audit_log_path()).unwrap(), audit_before);
    }

    #[test]
    fn room_send_all_returns_truthful_partial_result_without_retry() {
        let supervisor = test_supervisor();
        let (claude_pty, claude_inputs) = recording_pty_session(std::process::id());
        let (codex_pty, codex_send_count) =
            write_failing_pty_session(23, "synthetic room partial failure");
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            claude_pty,
        );
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            codex_pty,
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Partial".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();

        let result = supervisor
            .deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::All {},
                content: "partial truth".into(),
            })
            .unwrap();

        assert_eq!(result.recipient_count, 2);
        assert_eq!(result.written_count, 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].bytes_written, 23);
        assert!(
            result.failures[0]
                .error
                .contains("synthetic room partial failure")
        );
        assert_eq!(codex_send_count.load(Ordering::SeqCst), 1);
        assert_eq!(claude_inputs.lock().len(), 2);
        let page = supervisor
            .read_room_feed(ReadRoomFeedRequest {
                room_id: room.room_id,
                cursor: None,
            })
            .unwrap();
        assert!(page.events.iter().any(|event| matches!(
            &event.item,
            RoomFeedItem::Delivery {
                status: RoomDeliveryStatus::Failed,
                bytes_written: 23,
                error: Some(error),
                ..
            } if error.contains("synthetic room partial failure")
        )));
    }

    #[test]
    fn room_deletion_waits_for_the_exact_in_flight_delivery() {
        let supervisor = test_supervisor();
        let inputs = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let (write_entered_tx, write_entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new((Mutex::new(false), Condvar::new()));
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Box::new(FirstWriteBlockingPtySession {
                process_id: std::process::id(),
                inputs,
                calls,
                first_entered: write_entered_tx,
                release: release.clone(),
            }),
        );
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("In flight".into()),
                member_ids: vec![
                    test_session_id(&supervisor, "claude"),
                    test_session_id(&supervisor, "codex"),
                ],
            })
            .unwrap();
        let recipient_id = test_session_id(&supervisor, "claude");

        let delivery_supervisor = supervisor.clone();
        let delivery = thread::spawn(move || {
            delivery_supervisor.deliver_room_message(DeliverRoomMessageRequest {
                room_id: room.room_id,
                recipients: RoomRecipientSelection::One {
                    session_id: recipient_id,
                },
                content: "hold deletion".into(),
            })
        });
        write_entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("delivery did not enter its first PTY write");

        let error = supervisor
            .delete_room(DeleteRoomRequest {
                room_id: room.room_id,
            })
            .unwrap_err();
        assert!(error.to_string().contains("delivery in progress"));
        assert!(
            supervisor
                .snapshot()
                .rooms
                .iter()
                .any(|candidate| candidate.room_id == room.room_id)
        );

        let (released, ready) = &*release;
        *released.lock() = true;
        ready.notify_all();
        delivery.join().unwrap().unwrap();
        supervisor
            .delete_room(DeleteRoomRequest {
                room_id: room.room_id,
            })
            .unwrap();
    }

    #[test]
    fn pane_room_read_and_post_are_kernel_derived_future_only_and_revoked_on_removal() {
        let supervisor = test_supervisor();
        let grok = create_test_session(
            &supervisor,
            "grok",
            DriverKind::Grok,
            shared_types::PermissionProfile::Normal,
        );
        let claude = test_session_id(&supervisor, "claude");
        let room = supervisor
            .create_room(CreateRoomRequest {
                label: Some("Pane room".into()),
                member_ids: vec![test_session_id(&supervisor, "codex"), grok.session_id],
            })
            .unwrap();
        let before_join = supervisor
            .post_room_message(PostRoomMessageRequest {
                room_id: room.room_id,
                content: "before-join-private".into(),
            })
            .unwrap();
        supervisor
            .remove_room_member(RemoveRoomMemberRequest {
                room_id: room.room_id,
                session_id: grok.session_id,
            })
            .unwrap();
        supervisor
            .add_room_member(AddRoomMemberRequest {
                room_id: room.room_id,
                session_id: claude,
            })
            .unwrap();
        let (pty, _inputs) = recording_pty_session(101);
        install_mock_running_session_with_process_id_and_mode(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(101),
            pty,
            BracketedPasteMode::Unknown,
        );
        let caller = supervisor
            .resolve_pane_caller(TestPaneProcess::new(9001, [101]))
            .unwrap();

        let posted = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::RoomPost {
                content: "after-join-visible".into(),
            },
        );
        assert!(posted.ok, "{}", posted.message);
        assert!(posted.request_id.is_none());
        let Some(SidebandResponsePayload::RoomPost { result: pane_post }) = posted.payload else {
            panic!("missing room post payload")
        };
        let read =
            supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { cursor: None });
        assert!(read.ok, "{}", read.message);
        let SidebandResponsePayload::RoomFeed { page } = read.payload.unwrap() else {
            panic!("missing room feed payload")
        };
        let encoded = serde_json::to_string(&page).unwrap();
        assert!(!encoded.contains("before-join-private"));
        assert!(encoded.contains("after-join-visible"));
        assert!(page.events.iter().any(|event| matches!(
            event.item,
            RoomFeedItem::Message {
                sender: RoomMessageSender::Session { session_id },
                ..
            } if session_id == claude
        )));

        for sequence in [0, before_join.cursor.sequence] {
            let explicit = supervisor.apply_sideband_request(
                &caller,
                SidebandRequest::RoomRead {
                    cursor: Some(shared_types::RoomFeedCursor {
                        epoch: before_join.cursor.epoch,
                        sequence,
                    }),
                },
            );
            assert!(explicit.ok, "{}", explicit.message);
            let Some(SidebandResponsePayload::RoomFeed { page }) = explicit.payload else {
                panic!("missing explicit-cursor room feed payload")
            };
            let encoded = serde_json::to_string(&page).unwrap();
            assert!(
                !encoded.contains("before-join-private"),
                "an explicit pre-join cursor bypassed the membership join floor"
            );
            assert!(encoded.contains("after-join-visible"));
        }

        {
            let mut rooms = supervisor.inner.rooms.lock();
            let runtime = rooms.get_mut(room.room_id).unwrap();
            for index in 0..513 {
                runtime
                    .append(RoomFeedItem::Message {
                        message_id: Uuid::new_v4(),
                        sender: RoomMessageSender::Operator {},
                        content: format!("eviction-{index}"),
                        recipient_ids: Vec::new(),
                        membership_revision: runtime.definition.membership_revision,
                    })
                    .unwrap();
            }
        }
        let evicted = supervisor.apply_sideband_request(
            &caller,
            SidebandRequest::RoomRead {
                cursor: Some(pane_post.cursor),
            },
        );
        assert!(evicted.ok, "{}", evicted.message);
        let Some(SidebandResponsePayload::RoomFeed { page }) = evicted.payload else {
            panic!("missing evicted room feed payload")
        };
        assert_eq!(
            page.gap.as_ref().map(|gap| gap.reason),
            Some(shared_types::RoomFeedGapReason::Evicted),
            "the sideband read path must surface authoritative feed eviction"
        );
        let gap = page.gap.unwrap();
        assert_eq!(gap.from_sequence, Some(pane_post.cursor.sequence + 1));
        assert!(gap.through_sequence.unwrap() > pane_post.cursor.sequence);

        supervisor
            .remove_room_member(RemoveRoomMemberRequest {
                room_id: room.room_id,
                session_id: claude,
            })
            .unwrap();
        let events = capture_runtime_events(&supervisor);
        let audit_before = fs::read(supervisor.audit_log_path()).unwrap();
        let denied =
            supervisor.apply_sideband_request(&caller, SidebandRequest::RoomRead { cursor: None });
        assert!(!denied.ok);
        assert!(denied.request_id.is_none());
        assert!(events.lock().is_empty());
        assert_eq!(fs::read(supervisor.audit_log_path()).unwrap(), audit_before);
    }

    #[test]
    fn missing_and_replaced_persisted_working_directories_load_unavailable_without_spawn_or_rewrite()
     {
        let root = tempfile::tempdir().expect("create unavailable-cwd root");
        let missing = root.path().join("missing selected cwd");
        let replaced = root.path().join("replaced selected cwd");
        fs::create_dir_all(&missing).unwrap();
        fs::create_dir_all(&replaced).unwrap();
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let missing_session = create_test_session(
            &supervisor,
            "Missing cwd",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let replaced_session = create_test_session(
            &supervisor,
            "Replaced cwd",
            DriverKind::Codex,
            shared_types::PermissionProfile::Normal,
        );
        supervisor
            .set_session_working_directory(missing_session.session_id, &missing)
            .unwrap();
        supervisor
            .set_session_working_directory(replaced_session.session_id, &replaced)
            .unwrap();
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_before = fs::read(&catalog_path).unwrap();
        drop(supervisor);

        fs::rename(&missing, root.path().join("missing cwd moved away")).unwrap();
        fs::rename(&replaced, root.path().join("original replaced cwd")).unwrap();
        fs::create_dir_all(&replaced).unwrap();

        let reopened = SupervisorHandle::new(test_supervisor_config(root.path()))
            .expect("load catalog with unavailable session directories");
        reopened.set_executable_resolver_for_tests(Arc::new(TestExecutableResolver));
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        reopened.set_pty_spawner_for_tests(Arc::new(spawner));
        for session_id in [missing_session.session_id, replaced_session.session_id] {
            let snapshot = reopened
                .snapshot()
                .sessions
                .into_iter()
                .find(|session| session.session_id == session_id)
                .unwrap();
            assert_eq!(snapshot.lifecycle_state, LifecycleState::Closed);
            assert!(!snapshot.running);
            assert!(
                snapshot
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("working directory unavailable")),
                "missing visible unavailable state: {snapshot:?}"
            );
            let before = slots_mutation_probe(&reopened);
            let error = reopened.start_session_by_id(session_id).unwrap_err();
            assert!(
                format!("{error:#}").contains("working directory"),
                "{error:#}"
            );
            assert_eq!(slots_mutation_probe(&reopened), before);
        }
        assert!(specs.lock().is_empty());
        assert_eq!(
            fs::read(&catalog_path).unwrap(),
            catalog_before,
            "availability probing must not rewrite the user's catalog"
        );
    }

    #[test]
    fn workspace_and_session_directory_selection_reject_bidirectional_runtime_overlap() {
        let root = tempfile::tempdir().expect("create runtime-overlap root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Runtime overlap",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let runtime = supervisor.runtime_dir().to_path_buf();
        let catalog_path = session_catalog_path(&runtime);
        let catalog_before = fs::read(&catalog_path).unwrap();
        let slots_before = slots_mutation_probe(&supervisor);
        let events = capture_runtime_events(&supervisor);

        for overlapping in [&runtime, root.path()] {
            let workspace_error = supervisor
                .set_workspace_preference(overlapping)
                .unwrap_err();
            assert!(
                workspace_error.to_string().contains("must be disjoint"),
                "{workspace_error:#}"
            );
            let session_error = supervisor
                .set_session_working_directory(session.session_id, overlapping)
                .unwrap_err();
            assert!(
                session_error.to_string().contains("must be disjoint"),
                "{session_error:#}"
            );
        }

        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
        assert!(events.lock().is_empty());
    }

    #[test]
    fn durable_catalog_events_redact_personal_working_directory_paths() {
        let root = tempfile::tempdir().expect("create audit-redaction root");
        let selected = root.path().join("private customer workspace");
        fs::create_dir_all(&selected).unwrap();
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Private workspace",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        supervisor
            .set_session_working_directory(session.session_id, &selected)
            .unwrap();

        let events = fs::read_to_string(supervisor.audit_log_path())
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).unwrap())
            .collect::<Vec<_>>();
        let created = events
            .iter()
            .find(|event| event["event"] == "session_created")
            .expect("durable SessionCreated metadata");
        assert_eq!(created["session"]["working_dir"], "[path omitted]");
        let changed = events
            .iter()
            .find(|event| event["event"] == "session_working_directory_changed")
            .expect("durable working-directory change metadata");
        assert_eq!(changed["old_working_dir"], "[path omitted]");
        assert_eq!(changed["new_working_dir"], "[path omitted]");
    }

    #[test]
    fn delete_retains_definition_until_event_publisher_is_quiescent_and_gap_free() {
        let supervisor = empty_test_supervisor();
        let session = create_test_session(
            &supervisor,
            "Delete only after drain",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_before = fs::read(&catalog_path).unwrap();
        let events = capture_runtime_events(&supervisor);

        {
            let mut streams = supervisor.inner.run_event_publish.lock();
            streams.get_mut(&session.session_id).unwrap().draining = true;
        }
        assert!(
            supervisor
                .delete_session(session.session_id)
                .unwrap_err()
                .to_string()
                .contains("still draining")
        );
        supervisor
            .inner
            .run_event_publish
            .lock()
            .get_mut(&session.session_id)
            .unwrap()
            .draining = false;

        {
            let mut streams = supervisor.inner.run_event_publish.lock();
            let stream = streams.get_mut(&session.session_id).unwrap();
            stream.pending.insert(
                1,
                RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: "pending fixture".into(),
                    timestamp: now_rfc3339(),
                },
            );
        }
        assert!(
            supervisor
                .delete_session(session.session_id)
                .unwrap_err()
                .to_string()
                .contains("still draining")
        );
        supervisor
            .inner
            .run_event_publish
            .lock()
            .get_mut(&session.session_id)
            .unwrap()
            .pending
            .clear();

        supervisor
            .inner
            .run_event_publish
            .lock()
            .get_mut(&session.session_id)
            .unwrap()
            .next_sequence = None;
        assert!(
            supervisor
                .delete_session(session.session_id)
                .unwrap_err()
                .to_string()
                .contains("still draining")
        );
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
        assert!(
            supervisor
                .snapshot()
                .sessions
                .iter()
                .any(|candidate| candidate.session_id == session.session_id)
        );
        assert!(
            !events
                .lock()
                .iter()
                .any(|event| matches!(event, RuntimeEvent::SessionDeleted { .. }))
        );

        supervisor
            .inner
            .run_event_publish
            .lock()
            .get_mut(&session.session_id)
            .unwrap()
            .next_sequence = Some(1);
        supervisor
            .delete_session(session.session_id)
            .expect("delete after publisher becomes quiescent");
        assert!(
            !supervisor
                .snapshot()
                .sessions
                .iter()
                .any(|candidate| candidate.session_id == session.session_id)
        );
        assert!(events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionDeleted { session_id, .. }
                if *session_id == session.session_id
        )));
    }

    #[test]
    fn unresolved_driver_binary_is_zero_spawn_slot_catalog_and_event_mutation() {
        let supervisor = empty_test_supervisor();
        let session = create_test_session(
            &supervisor,
            "Unresolved driver",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        supervisor.set_executable_resolver_for_tests(Arc::new(FailingExecutableResolver));
        let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        let slots_before = slots_mutation_probe(&supervisor);
        let catalog_path = session_catalog_path(supervisor.runtime_dir());
        let catalog_before = fs::read(&catalog_path).unwrap();
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .start_session_by_id(session.session_id)
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("injected unresolved Claude executable")
        );
        assert_eq!(slots_mutation_probe(&supervisor), slots_before);
        assert_eq!(fs::read(&catalog_path).unwrap(), catalog_before);
        assert!(specs.lock().is_empty());
        assert!(events.lock().is_empty());
    }

    #[test]
    fn labels_and_metacharacter_working_directories_never_enter_shell_syntax() {
        let root = tempfile::tempdir().expect("create metacharacter launch root");
        let metachar_cwd = root.path().join("workspace & harmless");
        fs::create_dir_all(&metachar_cwd).unwrap();
        let sentinel = metachar_cwd.join("prim1-shell-injection-sentinel.txt");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let session = create_test_session(
            &supervisor,
            "Claude & echo owned>prim1-shell-injection-sentinel.txt",
            DriverKind::Claude,
            shared_types::PermissionProfile::Normal,
        );
        supervisor
            .set_session_working_directory(session.session_id, &metachar_cwd)
            .unwrap();
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (spawner, specs) = CapturingPtySpawner::new(vec![pty]);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        supervisor
            .start_session_by_id(session.session_id)
            .expect("prepare direct executable launch");

        let specs = specs.lock();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert!(Path::new(&spec.program).is_absolute());
        assert!(!matches!(
            Path::new(&spec.program)
                .extension()
                .and_then(|extension| extension.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("cmd" | "bat" | "ps1")
        ));
        assert_eq!(
            spec.working_dir,
            child_process_path(&fs::canonicalize(&metachar_cwd).unwrap()).to_string_lossy()
        );
        assert!(
            !spec
                .args
                .iter()
                .any(|argument| argument.contains("echo owned"))
        );
        assert!(!sentinel.exists());
    }

    #[test]
    fn failed_or_timed_out_process_scope_termination_is_never_closed_deletable_or_restartable() {
        for (name, kill_behavior, exit_status, timeout) in [
            (
                "kill-error-with-root-exit",
                MockKillBehavior::Error("injected Job termination failure"),
                Some(pty_exit_status(0, None, true)),
                SESSION_STOP_KILL_TIMEOUT,
            ),
            (
                "timeout-with-dead-root-pid",
                MockKillBehavior::Sleep(Duration::from_millis(100)),
                None,
                Duration::from_millis(10),
            ),
        ] {
            let supervisor = empty_test_supervisor();
            let session = create_test_session(
                &supervisor,
                name,
                DriverKind::Claude,
                shared_types::PermissionProfile::Normal,
            );
            let (pty, _, _) =
                mock_pty_session_with_exit_status(Some(u32::MAX), exit_status, kill_behavior);
            install_mock_running_session_by_id_with_mode(
                &supervisor,
                session.session_id,
                DriverKind::Claude,
                Some(u32::MAX),
                pty,
                BracketedPasteMode::Unknown,
            );
            supervisor.set_stop_kill_timeout_for_tests(timeout);
            let events = capture_runtime_events(&supervisor);
            let stopped = supervisor
                .stop_session_by_id(session.session_id)
                .expect("uncertain stop still returns its truthful Failed snapshot");
            assert_eq!(stopped.lifecycle_state, LifecycleState::Failed, "{name}");
            assert!(
                stopped.running,
                "{name}: uncertain termination must retain the exact process-scope owner"
            );
            assert!(
                stopped
                    .last_error
                    .as_deref()
                    .is_some_and(|error| error.contains("termination could not be proved")),
                "{name}: {stopped:?}"
            );
            assert!(
                supervisor
                    .inner
                    .slots
                    .lock()
                    .get_by_id(session.session_id)
                    .unwrap()
                    .termination_uncertain,
                "{name}"
            );
            assert!(
                !events
                    .lock()
                    .iter()
                    .any(|event| matches!(event, RuntimeEvent::SessionExit { .. })),
                "{name}"
            );
            assert!(
                !events.lock().iter().any(|event| matches!(
                    event,
                    RuntimeEvent::SessionState {
                        state: LifecycleState::Closed,
                        ..
                    }
                )),
                "{name}"
            );

            let (spawner, specs) = CapturingPtySpawner::new(Vec::new());
            supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
            let protected_state = slots_mutation_probe(&supervisor);
            assert!(
                supervisor.start_session_by_id(session.session_id).is_err(),
                "{name}"
            );
            assert!(
                supervisor
                    .restart_session_by_id(session.session_id)
                    .is_err(),
                "{name}"
            );
            assert!(
                supervisor.delete_session(session.session_id).is_err(),
                "{name}"
            );
            assert_eq!(slots_mutation_probe(&supervisor), protected_state, "{name}");
            assert!(specs.lock().is_empty(), "{name}");
            assert!(
                supervisor
                    .snapshot()
                    .sessions
                    .iter()
                    .any(|candidate| candidate.session_id == session.session_id)
            );
        }
    }

    #[test]
    fn terminal_retirement_retains_owner_until_reserved_reproof_succeeds() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "codex");
        let kill_count = Arc::new(AtomicUsize::new(0));
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(std::process::id()),
            Box::new(FirstKillFailsPtySession {
                process_id: std::process::id(),
                kill_count: kill_count.clone(),
            }),
        );
        let generation = supervisor.current_generation_for_tests(session_id).unwrap();
        let run_id = current_run_id(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);

        supervisor.handle_pty_event_by_id(session_id, generation, run_id, PtyEvent::Closed);

        let failed = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(failed.lifecycle_state, LifecycleState::Failed);
        assert!(failed.running);
        assert_eq!(failed.process_id, Some(std::process::id()));
        assert!(
            supervisor
                .inner
                .slots
                .lock()
                .get_by_id(session_id)
                .unwrap()
                .termination_uncertain
        );
        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit { .. }
                | RuntimeEvent::SessionState {
                    state: LifecycleState::Closed,
                    ..
                }
        )));

        let before_late_eof = slot_mutation_probe(&supervisor, "codex");
        supervisor.handle_pty_event_by_id(session_id, generation, run_id, PtyEvent::Closed);
        assert_eq!(slot_mutation_probe(&supervisor, "codex"), before_late_eof);

        let stopped = supervisor
            .stop_session_by_id(session_id)
            .expect("explicit reserved re-proof should retry the retained owner");
        assert_eq!(stopped.lifecycle_state, LifecycleState::Closed);
        assert!(!stopped.running);
        assert_eq!(kill_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn timed_out_stop_late_completion_and_eof_require_a_new_reserved_reproof() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "codex");
        let (kill_entered_tx, kill_entered_rx) = mpsc::sync_channel(1);
        let (kill_release_tx, kill_release_rx) = mpsc::sync_channel(2);
        install_mock_running_session_with_process_id(
            &supervisor,
            "codex",
            DriverKind::Codex,
            Some(std::process::id()),
            Box::new(GatedKillPtySession {
                process_id: std::process::id(),
                kill_entered: Mutex::new(Some(kill_entered_tx)),
                kill_release: Mutex::new(kill_release_rx),
            }),
        );
        let generation = supervisor.current_generation_for_tests(session_id).unwrap();
        let run_id = current_run_id(&supervisor, "codex");
        let events = capture_runtime_events(&supervisor);
        supervisor.set_stop_kill_timeout_for_tests(Duration::from_millis(20));

        let timed_out = supervisor.stop_session_by_id(session_id).unwrap();
        assert_eq!(timed_out.lifecycle_state, LifecycleState::Failed);
        assert!(timed_out.running);
        kill_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("termination attempt did not enter the gated kill");
        assert!(
            supervisor
                .inner
                .slots
                .lock()
                .get_by_id(session_id)
                .unwrap()
                .lifecycle_operation
                .is_some(),
            "a still-running timed-out termination attempt must retain its reservation"
        );

        supervisor.handle_pty_event_by_id(session_id, generation, run_id, PtyEvent::Closed);
        assert_eq!(
            supervisor
                .snapshot()
                .sessions
                .into_iter()
                .find(|session| session.session_id == session_id)
                .unwrap()
                .lifecycle_state,
            LifecycleState::Failed
        );

        kill_release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let operation_cleared = supervisor
                .inner
                .slots
                .lock()
                .get_by_id(session_id)
                .unwrap()
                .lifecycle_operation
                .is_none();
            if operation_cleared {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "late termination did not reconcile"
            );
            thread::sleep(Duration::from_millis(5));
        }
        let after_late_completion = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(
            after_late_completion.lifecycle_state,
            LifecycleState::Failed
        );
        assert!(after_late_completion.running);
        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionExit { .. }
                | RuntimeEvent::SessionState {
                    state: LifecycleState::Closed,
                    ..
                }
        )));

        supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(1));
        kill_release_tx.send(()).unwrap();
        let reproved = supervisor.stop_session_by_id(session_id).unwrap();
        assert_eq!(reproved.lifecycle_state, LifecycleState::Closed);
        assert!(!reproved.running);
    }

    #[test]
    fn rejected_spawn_cleanup_timeout_retains_owner_until_reserved_reproof() {
        let supervisor = test_supervisor();
        let session_id = test_session_id(&supervisor, "claude");
        let (kill_entered_tx, kill_entered_rx) = mpsc::sync_channel(1);
        let (kill_release_tx, kill_release_rx) = mpsc::sync_channel(2);
        let (spawner, spawn_entered, spawn_release) =
            gated_pty_spawner(Box::new(GatedKillPtySession {
                process_id: std::process::id(),
                kill_entered: Mutex::new(Some(kill_entered_tx)),
                kill_release: Mutex::new(kill_release_rx),
            }));
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));
        supervisor.set_stop_kill_timeout_for_tests(Duration::from_millis(20));

        let start_supervisor = supervisor.clone();
        let start = thread::spawn(move || start_supervisor.start_session_by_id(session_id));
        spawn_entered
            .recv_timeout(Duration::from_secs(1))
            .expect("spawn did not reach its gate");
        let (spawn_generation, spawn_run_id) = {
            let slots = supervisor.inner.slots.lock();
            let slot = slots.get_by_id(session_id).unwrap();
            (slot.generation, slot.run_id.unwrap())
        };

        let stopped = supervisor.stop_session_by_id(session_id).unwrap();
        assert_eq!(stopped.lifecycle_state, LifecycleState::Failed);
        spawn_release.send(()).unwrap();
        kill_entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("rejected spawn cleanup did not enter kill");
        let start_error = start.join().unwrap().unwrap_err();
        assert!(
            start_error
                .to_string()
                .contains("failed to prove rejected spawned PTY termination"),
            "unexpected rejected-spawn error: {start_error:#}"
        );
        let retained = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.session_id == session_id)
            .unwrap();
        assert_eq!(retained.lifecycle_state, LifecycleState::Failed);
        assert!(retained.running);
        assert_eq!(retained.process_id, Some(std::process::id()));
        let overlapping_reproof = supervisor.stop_session_by_id(session_id).unwrap_err();
        assert!(
            overlapping_reproof
                .to_string()
                .contains("rejected-spawn cleanup is still in progress"),
            "unexpected overlapping cleanup error: {overlapping_reproof:#}"
        );

        supervisor.handle_pty_event_by_id(
            session_id,
            spawn_generation,
            spawn_run_id,
            PtyEvent::Closed,
        );
        assert_eq!(
            supervisor
                .snapshot()
                .sessions
                .into_iter()
                .find(|session| session.session_id == session_id)
                .unwrap()
                .lifecycle_state,
            LifecycleState::Failed
        );

        kill_release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let spawn_reconciled = supervisor
                .inner
                .slots
                .lock()
                .get_by_id(session_id)
                .unwrap()
                .spawn_in_flight
                .is_none();
            if spawn_reconciled {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "late spawn cleanup did not reconcile"
            );
            thread::sleep(Duration::from_millis(5));
        }
        supervisor.set_stop_kill_timeout_for_tests(Duration::from_secs(1));
        kill_release_tx.send(()).unwrap();
        let reproved = supervisor.stop_session_by_id(session_id).unwrap();
        assert_eq!(reproved.lifecycle_state, LifecycleState::Closed);
        assert!(!reproved.running);
    }

    #[test]
    fn shutdown_error_retains_failed_process_scope_and_never_reports_closed() {
        let supervisor = test_supervisor();
        let (pty, _, kill_count) = mock_pty_session(
            Some(std::process::id()),
            MockKillBehavior::Error("injected shutdown Job termination failure"),
        );
        install_mock_running_session_with_process_id(
            &supervisor,
            "claude",
            DriverKind::Claude,
            Some(std::process::id()),
            pty,
        );

        let error = supervisor.shutdown().unwrap_err();
        assert!(
            error.to_string().contains("termination failed"),
            "{error:#}"
        );
        let retained = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.label == "claude")
            .unwrap();
        assert_eq!(retained.lifecycle_state, LifecycleState::Failed);
        assert!(retained.running);
        assert_eq!(retained.process_id, Some(std::process::id()));
        assert_eq!(kill_count.load(Ordering::SeqCst), 1);

        let retry_error = supervisor.shutdown().unwrap_err();
        assert!(retry_error.to_string().contains("termination failed"));
        assert_eq!(kill_count.load(Ordering::SeqCst), 2);
        let retained = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|session| session.label == "claude")
            .unwrap();
        assert_eq!(retained.lifecycle_state, LifecycleState::Failed);
        assert!(retained.running);
    }

    #[test]
    fn normal_profile_approval_modals_block_real_routes_without_route_side_effects() {
        for (label, driver, prompt, detail) in [
            (
                "claude",
                DriverKind::Claude,
                "Do you want to proceed?\n❯ 1. Yes\n  2. Yes, and don't ask again\n  3. No\nEsc to cancel · Tab to amend",
                "approval_prompt",
            ),
            (
                "codex",
                DriverKind::Codex,
                "Would you like to run the following command?\n› 1. Yes, proceed (y)\n  2. Yes, and don't ask again\n  3. No, and tell Codex what to do differently (esc)",
                "approval_prompt",
            ),
        ] {
            let supervisor = test_supervisor();
            let (pty, inputs) = recording_pty_session(std::process::id());
            install_mock_running_session_with_bracketed_paste_enabled(
                &supervisor,
                label,
                driver,
                pty,
            );
            let events = capture_runtime_events(&supervisor);
            handle_current_pty_event(&supervisor, label, 0, PtyEvent::Output(prompt.into()));
            assert!(work_state_events(&events).iter().any(|event| matches!(
                event,
                RuntimeEvent::SessionWorkState {
                    state: WorkState::Blocked,
                    detail: Some(observed),
                    ..
                } if observed == detail
            )));
            events.lock().clear();
            let before = slots_mutation_probe(&supervisor);
            let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();

            let error = supervisor
                .route_operator_message(OperatorRouteMessageRequest {
                    recipient_id: test_session_id(&supervisor, label),
                    content: "must not enter an approval modal".into(),
                })
                .unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("work state is blocked (approval_prompt)"),
                "{label}: {error:#}"
            );
            assert_eq!(slots_mutation_probe(&supervisor), before, "{label}");
            assert!(inputs.lock().is_empty(), "{label}");
            assert!(events.lock().is_empty(), "{label}");
            assert_eq!(
                fs::read(supervisor.audit_log_path()).unwrap_or_default(),
                audit_before,
                "{label}"
            );
        }
    }

    #[test]
    fn raw_operator_input_remains_available_for_blocked_work_state() {
        let supervisor = test_supervisor();
        let (pty, inputs) = recording_pty_session(std::process::id());
        install_mock_running_session_with_bracketed_paste_enabled(
            &supervisor,
            "codex",
            DriverKind::Codex,
            pty,
        );
        set_session_dispatch_state(
            &supervisor,
            "codex",
            LifecycleState::Ready,
            Some(WorkState::Blocked),
        );

        supervisor
            .send_input(SendInputRequest {
                session_id: test_session_id(&supervisor, "codex"),
                input: "1".into(),
            })
            .unwrap();

        assert_eq!(inputs.lock().as_slice(), &["1"]);
    }

    struct FakeWslControl {
        reconciliations: AtomicUsize,
        qualifications: AtomicUsize,
        fail_reconciliation: AtomicBool,
        revalidation_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
        confirmed: Mutex<Vec<String>>,
        terminated: Mutex<Vec<String>>,
    }

    impl FakeWslControl {
        fn new() -> Self {
            Self {
                reconciliations: AtomicUsize::new(0),
                qualifications: AtomicUsize::new(0),
                fail_reconciliation: AtomicBool::new(false),
                revalidation_hook: Mutex::new(None),
                confirmed: Mutex::new(Vec::new()),
                terminated: Mutex::new(Vec::new()),
            }
        }
    }

    impl WslControl for FakeWslControl {
        fn reconcile_stale_scopes(&self) -> Result<()> {
            self.reconciliations.fetch_add(1, Ordering::SeqCst);
            if self.fail_reconciliation.load(Ordering::Acquire) {
                return Err(anyhow!("injected stale Prime scope cleanup failure"));
            }
            Ok(())
        }

        fn default_working_directory(&self) -> Result<String> {
            Ok("/home/test".into())
        }

        fn qualify_working_directory(&self, candidate: &str) -> Result<QualifiedWorkingDirectory> {
            self.qualifications.fetch_add(1, Ordering::SeqCst);
            validate_linux_candidate(candidate)?;
            let canonical_path = if candidate == "/renderer/../proposal" {
                "/backend-qualified".into()
            } else {
                candidate.into()
            };
            Ok(QualifiedWorkingDirectory {
                namespace: WorkingDirectoryNamespace::WslUbuntu,
                canonical_path,
                identity: "wsl_ubuntu:7:9".into(),
            })
        }

        fn revalidate_working_directory(&self, expected: &QualifiedWorkingDirectory) -> Result<()> {
            if let Some(hook) = self.revalidation_hook.lock().clone() {
                hook();
            }
            validate_driver_working_directory_pair(DriverKind::Prime, expected)
        }

        fn resolve_prime_executable(&self) -> Result<String> {
            Ok("/home/test/.npm-global/bin/prime-agent".into())
        }

        fn wsl_executable(&self) -> Result<String> {
            Ok(r"C:\Windows\System32\wsl.exe".into())
        }

        fn confirm_scope_started(&self, scope: &WslRunScope) -> Result<()> {
            validate_wsl_scope(scope)?;
            self.confirmed.lock().push(scope.unit.clone());
            Ok(())
        }

        fn terminate_scope(&self, scope: &WslRunScope) -> Result<()> {
            validate_wsl_scope(scope)?;
            self.terminated.lock().push(scope.unit.clone());
            Ok(())
        }
    }

    struct CapturingPreparedPtySpawner {
        session: Mutex<Option<Box<dyn PtySessionTrait>>>,
        plans: Arc<Mutex<Vec<PreparedLaunch>>>,
    }

    impl CapturingPreparedPtySpawner {
        fn new(session: Box<dyn PtySessionTrait>) -> (Self, Arc<Mutex<Vec<PreparedLaunch>>>) {
            let plans = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    session: Mutex::new(Some(session)),
                    plans: Arc::clone(&plans),
                },
                plans,
            )
        }
    }

    impl PtySpawner for CapturingPreparedPtySpawner {
        fn spawn(
            &self,
            plan: &PreparedLaunch,
            _handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            self.plans.lock().push(plan.clone());
            self.session
                .lock()
                .take()
                .ok_or_else(|| anyhow!("capturing Prime PTY already consumed"))
        }
    }

    struct MissingScopePtySpawner {
        control: Arc<dyn WslControl>,
    }

    impl PtySpawner for MissingScopePtySpawner {
        fn spawn(
            &self,
            plan: &PreparedLaunch,
            handler: PtyEventHandler,
        ) -> Result<Box<dyn PtySessionTrait>> {
            let mut missing = plan.clone();
            let systemd_run = missing
                .spec
                .args
                .iter_mut()
                .find(|argument| argument.as_str() == driver_prime::SYSTEMD_RUN)
                .ok_or_else(|| anyhow!("Prime launch plan omitted systemd-run"))?;
            *systemd_run = "/usr/bin/prim1-deliberately-missing-systemd-run".into();
            ConcretePtySpawner {
                wsl: Arc::clone(&self.control),
            }
            .spawn(&missing, handler)
        }
    }

    #[test]
    fn prime_catalog_and_launch_plan_are_typed_shell_free_and_sideband_free() {
        let root = tempfile::tempdir().expect("create Prime catalog root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let control = Arc::new(FakeWslControl::new());
        supervisor.set_wsl_control_for_tests(control.clone());
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (spawner, plans) = CapturingPreparedPtySpawner::new(pty);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let created = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime & echo pwned".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some("/home/test/work & (qa)".into()),
            })
            .expect("create typed Prime session");
        assert_eq!(created.working_dir, "/home/test/work & (qa)");
        let started = supervisor
            .start_session_by_id(created.session_id)
            .expect("prepare Prime launch");
        assert_eq!(started.lifecycle_state, LifecycleState::Ready);

        let plans = plans.lock();
        assert_eq!(plans.len(), 1);
        let plan = &plans[0];
        let scope = plan.wsl_scope.as_ref().expect("Prime plan owns WSL scope");
        assert!(valid_wsl_unit(&scope.unit));
        assert_eq!(plan.spec.program, r"C:\Windows\System32\wsl.exe");
        assert_eq!(plan.spec.env.len(), 1);
        assert_eq!(plan.spec.env[0].key, "WSLENV");
        assert!(
            !plan
                .spec
                .env
                .iter()
                .any(|entry| entry.key.starts_with("PRIM1_"))
        );
        assert!(plan.spec.args.windows(4).any(|window| {
            window
                == [
                    "7",
                    "9",
                    "/home/test/work & (qa)",
                    "/home/test/.npm-global/bin/prime-agent",
                ]
        }));
        assert!(
            !plan
                .spec
                .args
                .iter()
                .any(|argument| argument.contains("echo pwned"))
        );
        drop(plans);

        let catalog: serde_json::Value = serde_json::from_slice(
            &fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog["sessions"][0]["working_directory"]["namespace"],
            "wsl_ubuntu"
        );
        assert_eq!(
            catalog["sessions"][0]["working_directory"]["identity"],
            "wsl_ubuntu:7:9"
        );
        assert!(control.reconciliations.load(Ordering::SeqCst) <= 1);
    }

    #[test]
    fn prime_rejects_unsafe_and_native_linux_directory_crossing_before_mutation() {
        let supervisor = test_supervisor();
        let control = Arc::new(FakeWslControl::new());
        supervisor.set_wsl_control_for_tests(control.clone());
        let before = slots_mutation_probe(&supervisor);
        let catalog_before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();

        assert!(
            supervisor
                .create_session(shared_types::CreateSessionRequest {
                    label: None,
                    driver: DriverKind::Prime,
                    permission_profile: shared_types::PermissionProfile::Unsafe,
                    linux_working_directory: Some("/home/test".into()),
                })
                .is_err()
        );
        assert!(
            supervisor
                .create_session(shared_types::CreateSessionRequest {
                    label: None,
                    driver: DriverKind::Claude,
                    permission_profile: shared_types::PermissionProfile::Normal,
                    linux_working_directory: Some("/home/test".into()),
                })
                .is_err()
        );
        assert_eq!(slots_mutation_probe(&supervisor), before);
        assert_eq!(
            fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
            catalog_before
        );
        assert!(
            supervisor
                .set_session_linux_working_directory(
                    test_session_id(&supervisor, "claude"),
                    "/home/probe-must-not-run",
                )
                .is_err()
        );
        assert_eq!(control.qualifications.load(Ordering::SeqCst), 0);

        let prime = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Route denied".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some("/home/test".into()),
            })
            .unwrap();
        let events = capture_runtime_events(&supervisor);
        let before = slots_mutation_probe(&supervisor);
        let audit_before = fs::read(supervisor.audit_log_path()).unwrap_or_default();
        let error = supervisor
            .route_operator_message(OperatorRouteMessageRequest {
                recipient_id: prime.session_id,
                content: "must remain raw-only".into(),
            })
            .unwrap_err();
        assert!(error.to_string().contains("not admitted"));
        assert_eq!(slots_mutation_probe(&supervisor), before);
        assert!(events.lock().is_empty());
        assert_eq!(
            fs::read(supervisor.audit_log_path()).unwrap_or_default(),
            audit_before
        );
    }

    #[test]
    fn prime_uses_only_backend_qualified_linux_path_and_blocks_on_stale_scope_failure() {
        let supervisor = empty_test_supervisor();
        let control = Arc::new(FakeWslControl::new());
        supervisor.set_wsl_control_for_tests(control.clone());
        *supervisor.inner.wsl_reconciliation_error.lock() = Some("retry required".into());
        control.fail_reconciliation.store(true, Ordering::Release);
        let before = slots_mutation_probe(&supervisor);
        let error = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime qualified".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some("/renderer/../proposal".into()),
            })
            .unwrap_err();
        assert!(format!("{error:#}").contains("stale Prime scope cleanup failure"));
        assert_eq!(slots_mutation_probe(&supervisor), before);

        control.fail_reconciliation.store(false, Ordering::Release);
        let session = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime qualified".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some("/renderer/../proposal".into()),
            })
            .expect("create only after stale-scope reconciliation succeeds");
        assert_eq!(session.working_dir, "/backend-qualified");
        assert_eq!(control.qualifications.load(Ordering::SeqCst), 1);
        let catalog: serde_json::Value = serde_json::from_slice(
            &fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog["sessions"][0]["working_directory"]["canonical_path"],
            "/backend-qualified"
        );
    }

    #[test]
    fn prime_scope_status_parser_is_keyed_and_treats_not_found_as_absent() {
        let active =
            parse_wsl_scope_status("TasksCurrent=2\nLoadState=loaded\nActiveState=active\n")
                .expect("parse keyed active scope")
                .expect("active scope is present");
        assert_eq!(active.active_state, "active");
        assert_eq!(active.tasks_current, 2);

        assert!(
            parse_wsl_scope_status(
                "ActiveState=inactive\nTasksCurrent=[not set]\nLoadState=not-found\n",
            )
            .expect("parse collected transient scope")
            .is_none()
        );
        assert!(parse_wsl_scope_status("ActiveState=active\nTasksCurrent=2\n").is_err());
        assert!(
            parse_wsl_scope_status("LoadState=loaded\nActiveState=active\nTasksCurrent=unknown\n",)
                .is_err()
        );
    }

    #[test]
    fn prime_default_working_directory_is_backend_qualified_before_display() {
        let supervisor = empty_test_supervisor();
        let control = Arc::new(FakeWslControl::new());
        supervisor.set_wsl_control_for_tests(control.clone());
        *supervisor.inner.wsl_reconciliation_error.lock() = Some("retry required".into());

        assert_eq!(
            supervisor.prime_default_working_directory().unwrap(),
            "/home/test"
        );
        assert_eq!(control.reconciliations.load(Ordering::SeqCst), 1);
        assert_eq!(control.qualifications.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn prime_start_revalidation_releases_registry_and_rejects_a_concurrent_cwd_change() {
        let supervisor = empty_test_supervisor();
        let control = Arc::new(FakeWslControl::new());
        supervisor.set_wsl_control_for_tests(control.clone());
        let session = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime concurrent start".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some("/home/test".into()),
            })
            .unwrap();
        let (pty, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let (spawner, plans) = CapturingPreparedPtySpawner::new(pty);
        supervisor.set_pty_spawner_for_tests(Arc::new(spawner));

        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Arc::new(Mutex::new(release_rx));
        *control.revalidation_hook.lock() = Some(Arc::new(move || {
            entered_tx.send(()).unwrap();
            release_rx.lock().recv().unwrap();
        }));

        let starting = supervisor.clone();
        let thread = thread::spawn(move || starting.start_session_by_id(session.session_id));
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("Prime start never entered WSL cwd revalidation");
        let slots = supervisor.inner.slots.try_lock();
        assert!(
            slots.is_some(),
            "Prime WSL revalidation held the global session registry lock"
        );
        drop(slots);
        supervisor
            .set_session_linux_working_directory(session.session_id, "/home/changed")
            .expect("concurrent closed-session cwd update");
        release_tx.send(()).unwrap();
        let error = thread
            .join()
            .unwrap()
            .expect_err("stale Prime launch preparation must not spawn");
        assert!(
            error
                .to_string()
                .contains("definition changed while launch was being prepared"),
            "{error:#}"
        );
        assert!(plans.lock().is_empty());
        let current = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|candidate| candidate.session_id == session.session_id)
            .unwrap();
        assert_eq!(current.lifecycle_state, LifecycleState::Closed);
        assert_eq!(current.working_dir, "/home/changed");
    }

    #[test]
    fn wsl_scoped_pty_denies_native_membership_and_terminates_both_scopes() {
        let control = Arc::new(FakeWslControl::new());
        let (inner, _, _) = mock_pty_session(None, MockKillBehavior::Immediate);
        let session = WslScopedPtySession {
            inner,
            control: control.clone(),
            scope: WslRunScope {
                distro: driver_prime::WSL_DISTRO.into(),
                unit: "prim1-session-11111111111111111111111111111111".into(),
            },
        };
        assert!(!session.contains_process_id(std::process::id()).unwrap());
        #[cfg(windows)]
        {
            let process = PinnedProcess::open(std::process::id()).unwrap();
            assert!(!session.contains_process(&process).unwrap());
        }
        session
            .kill()
            .expect("prove both Prime ownership scopes empty");
        assert_eq!(control.terminated.lock().len(), 1);
    }

    #[cfg(windows)]
    #[test]
    fn pane_mcp_launch_augmentation_is_driver_scoped_and_authority_free() {
        let root = tempfile::tempdir().expect("create pane MCP launch root");
        let executable = root.path().join("PRIM & one's pane helper.exe");
        fs::write(&executable, b"fixture").expect("write pane MCP executable fixture");
        let expected_executable = child_process_path(&executable)
            .to_str()
            .expect("encode pane MCP executable path")
            .to_owned();

        let base_spec = |driver| LaunchSpec {
            program: executable.display().to_string(),
            args: vec![format!("{driver:?}-base")],
            working_dir: root.path().display().to_string(),
            env: Vec::new(),
            display_name: format!("{driver:?}"),
        };

        let mut claude = base_spec(DriverKind::Claude);
        augment_launch_spec_with_pane_mcp(&mut claude, PaneMcpClient::Claude, &executable)
            .expect("augment Claude pane MCP launch");
        let config_index = claude
            .args
            .iter()
            .position(|argument| argument == "--mcp-config")
            .expect("Claude launch must carry an MCP config");
        let claude_config: serde_json::Value =
            serde_json::from_str(&claude.args[config_index + 1]).expect("decode Claude MCP config");
        assert_eq!(
            claude_config["mcpServers"][PANE_MCP_SERVER_NAME]["command"],
            expected_executable
        );
        assert_eq!(
            claude_config["mcpServers"][PANE_MCP_SERVER_NAME]["args"],
            serde_json::json!([PANE_MCP_MODE_ARGUMENT])
        );

        let mut codex = base_spec(DriverKind::Codex);
        augment_launch_spec_with_pane_mcp(&mut codex, PaneMcpClient::Codex, &executable)
            .expect("augment Codex pane MCP launch");
        let codex_arguments = codex.args.join("\n");
        assert!(codex_arguments.contains(&format!(
            "mcp_servers.{PANE_MCP_SERVER_NAME}.command={}",
            serde_json::to_string(&expected_executable).unwrap()
        )));
        assert!(codex_arguments.contains(&format!(
            "mcp_servers.{PANE_MCP_SERVER_NAME}.args=[\"{PANE_MCP_MODE_ARGUMENT}\"]"
        )));
        assert!(
            codex_arguments.contains(&format!("mcp_servers.{PANE_MCP_SERVER_NAME}.required=true"))
        );

        for payload in [claude.args.join("\n"), codex_arguments] {
            let lower = payload.to_ascii_lowercase();
            for forbidden in ["token", "credential", "room_id", "sender", "from="] {
                assert!(
                    !lower.contains(forbidden),
                    "pane MCP launch config leaked authority field {forbidden:?}: {payload}"
                );
            }
        }

        for driver in [
            DriverKind::Grok,
            DriverKind::GenericTerminal,
            DriverKind::Prime,
        ] {
            assert_eq!(PaneMcpClient::for_driver(driver), None);
        }
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Ubuntu WSL with systemd and Prime Agent"]
    fn real_wsl_controller_qualifies_identity_and_resolves_prime() {
        let root = tempfile::tempdir().expect("create real WSL controller runtime");
        let control = ConcreteWslControl::new(root.path().to_path_buf());
        control
            .reconcile_stale_scopes()
            .expect("reconcile stale Prime scopes");
        let home = control
            .default_working_directory()
            .expect("discover Ubuntu home");
        let qualified = control
            .qualify_working_directory(&home)
            .expect("qualify Ubuntu home");
        assert_eq!(qualified.namespace, WorkingDirectoryNamespace::WslUbuntu);
        parse_wsl_identity(&qualified.identity).expect("parse real Linux identity");
        control
            .revalidate_working_directory(&qualified)
            .expect("revalidate exact Linux identity");
        let executable = control
            .resolve_prime_executable()
            .expect("resolve installed Prime Agent");
        assert!(executable.starts_with('/'));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Ubuntu WSL with systemd"]
    fn real_startup_reconciliation_reaps_a_seeded_stale_prime_scope() {
        let root = tempfile::tempdir().expect("create stale-scope runtime");
        let control = ConcreteWslControl::new(root.path().to_path_buf());
        control
            .reconcile_stale_scopes()
            .expect("begin from an empty Prime scope set");
        let scope = WslRunScope {
            distro: driver_prime::WSL_DISTRO.into(),
            unit: format!(
                "{}{}",
                driver_prime::SCOPE_UNIT_PREFIX,
                Uuid::new_v4().simple()
            ),
        };
        control
            .run_checked(
                &[
                    driver_prime::SYSTEMD_RUN.into(),
                    "--user".into(),
                    format!("--unit={}.service", scope.unit),
                    "--property=KillMode=control-group".into(),
                    "--property=Type=exec".into(),
                    "--property=TimeoutStopSec=2s".into(),
                    "--collect".into(),
                    "--quiet".into(),
                    driver_prime::PYTHON.into(),
                    "-c".into(),
                    "import signal,time; signal.signal(signal.SIGHUP,signal.SIG_IGN); time.sleep(120)".into(),
                ],
                "seed stale Prime scope",
            )
            .expect("seed stale Prime scope without a live desktop owner");
        let seeded = control
            .scope_status(&scope)
            .expect("inspect seeded stale scope")
            .expect("seeded stale scope must exist");
        assert_eq!(seeded.active_state, "active");
        assert!(seeded.tasks_current >= 1);

        control
            .reconcile_stale_scopes()
            .expect("startup reconciliation must reap every exact-prefix stale scope");
        assert!(
            control.scope_status(&scope).unwrap().is_none(),
            "stale Prime scope survived startup reconciliation"
        );
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Ubuntu WSL with systemd and Prime Agent"]
    fn real_missing_prime_scope_never_reaches_ready_or_vacuous_success() {
        let root = tempfile::tempdir().expect("create missing-scope supervisor root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let control: Arc<dyn WslControl> = Arc::new(ConcreteWslControl::new(
            supervisor.runtime_dir().to_path_buf(),
        ));
        control
            .reconcile_stale_scopes()
            .expect("begin missing-scope test reconciled");
        supervisor.set_wsl_control_for_tests(Arc::clone(&control));
        supervisor.set_pty_spawner_for_tests(Arc::new(MissingScopePtySpawner {
            control: Arc::clone(&control),
        }));
        let home = control.default_working_directory().unwrap();
        let session = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime missing scope".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: Some(home),
            })
            .unwrap();
        let events = capture_runtime_events(&supervisor);

        let error = supervisor
            .start_session_by_id(session.session_id)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("failed start-time binding"),
            "{error:#}"
        );
        let snapshot = supervisor
            .snapshot()
            .sessions
            .into_iter()
            .find(|candidate| candidate.session_id == session.session_id)
            .unwrap();
        assert!(!snapshot.running);
        assert_ne!(snapshot.lifecycle_state, LifecycleState::Ready);
        assert!(!events.lock().iter().any(|event| matches!(
            event,
            RuntimeEvent::SessionState {
                identity,
                state: LifecycleState::Ready,
                ..
            } if identity.session_id == session.session_id
        )));
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Ubuntu WSL with systemd and Prime Agent"]
    fn real_prime_wsl_scope_starts_resizes_outputs_and_stops_empty() {
        let root = tempfile::tempdir().expect("create real Prime supervisor root");
        let supervisor = empty_test_supervisor_with_root(root.path().to_path_buf());
        let control = Arc::new(ConcreteWslControl::new(
            supervisor.runtime_dir().to_path_buf(),
        ));
        control
            .reconcile_stale_scopes()
            .expect("reconcile stale Prime scopes before test");
        supervisor.set_wsl_control_for_tests(control.clone());
        let events = capture_runtime_events(&supervisor);

        let session = supervisor
            .create_session(shared_types::CreateSessionRequest {
                label: Some("Prime integration".into()),
                driver: DriverKind::Prime,
                permission_profile: shared_types::PermissionProfile::Normal,
                linux_working_directory: None,
            })
            .expect("create real Prime session");
        let started = supervisor
            .start_session_by_id(session.session_id)
            .expect("start real Prime session in owned WSL scope");
        assert_eq!(started.lifecycle_state, LifecycleState::Ready);
        assert!(started.process_id.is_some());
        supervisor
            .resize_session_by_id(session.session_id, 100, 36)
            .expect("resize Prime ConPTY bridge");

        let output_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < output_deadline
            && !events.lock().iter().any(|event| {
                matches!(
                    event,
                    RuntimeEvent::SessionOutput { identity, .. }
                        if identity.session_id == session.session_id
                )
            })
        {
            thread::sleep(Duration::from_millis(50));
        }
        assert!(events.lock().iter().any(|event| {
            matches!(
                event,
                RuntimeEvent::SessionOutput { identity, .. }
                    if identity.session_id == session.session_id
            )
        }));

        let stopped = supervisor
            .stop_session_by_id(session.session_id)
            .expect("stop and prove both Prime scopes empty");
        assert_eq!(stopped.lifecycle_state, LifecycleState::Closed);
        assert!(!stopped.running);
        let remaining = control
            .run_checked(
                &[
                    driver_prime::SYSTEMCTL.into(),
                    "--user".into(),
                    "list-units".into(),
                    "--all".into(),
                    "--plain".into(),
                    "--no-legend".into(),
                    "--no-pager".into(),
                    format!("{}*.service", driver_prime::SCOPE_UNIT_PREFIX),
                ],
                "post-stop Prime scope enumeration",
            )
            .expect("enumerate Prime scopes after stop");
        assert!(
            !remaining.contains(driver_prime::SCOPE_UNIT_PREFIX),
            "Prime scope remained after stop: {remaining}"
        );
        supervisor
            .shutdown()
            .expect("shutdown clean test supervisor");
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires Ubuntu WSL with systemd"]
    fn real_prime_guard_propagates_resize_and_reaps_detached_linux_descendant_after_windows_death()
    {
        const PAYLOAD: &str = "import os,signal,subprocess,time; signal.signal(signal.SIGHUP,signal.SIG_IGN); signal.signal(signal.SIGWINCH,lambda s,f: print('SIZE %d %d'%os.get_terminal_size(),flush=True)); subprocess.Popen(['/usr/bin/python3','-c','import signal,time; signal.signal(signal.SIGHUP,signal.SIG_IGN); time.sleep(120)'],start_new_session=True); print('READY',flush=True); time.sleep(120)";

        let root = tempfile::tempdir().expect("create adversarial WSL runtime");
        let control = ConcreteWslControl::new(root.path().to_path_buf());
        control
            .reconcile_stale_scopes()
            .expect("clear stale scopes before adversarial run");
        let home = control.default_working_directory().unwrap();
        let qualified = control.qualify_working_directory(&home).unwrap();
        let (device, inode) = parse_wsl_identity(&qualified.identity).unwrap();
        let scope = WslRunScope {
            distro: driver_prime::WSL_DISTRO.into(),
            unit: format!(
                "{}{}",
                driver_prime::SCOPE_UNIT_PREFIX,
                Uuid::new_v4().simple()
            ),
        };
        let definition = SessionDefinition {
            session_id: Uuid::new_v4(),
            alias: "prime-adversarial".into(),
            label: "Prime adversarial lifecycle".into(),
            driver: DriverKind::Prime,
            working_dir: qualified.canonical_path,
            permission_profile: shared_types::PermissionProfile::Normal,
        };
        let mut spec = driver_prime::launch_spec(
            &definition,
            &control.wsl_executable().unwrap(),
            &control.resolve_prime_executable().unwrap(),
            &scope.unit,
            device,
            inode,
        )
        .unwrap();
        let guard_index = spec
            .args
            .iter()
            .position(|argument| argument == driver_prime::PRIME_GUARD)
            .expect("find immutable guard argv");
        spec.args.truncate(guard_index + 4);
        spec.args
            .extend([driver_prime::PYTHON.into(), "-c".into(), PAYLOAD.into()]);

        let output = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&output);
        let session = ConcretePtySession::spawn(
            &spec,
            Arc::new(move |event| {
                if let PtyEvent::Output(chunk) = event {
                    captured.lock().push_str(&chunk);
                }
            }),
        )
        .expect("spawn adversarial Prime WSL bridge");
        session
            .send_input("\u{1b}[1;1R")
            .expect("release adversarial ConPTY startup handshake");
        control
            .confirm_scope_started(&scope)
            .expect("bind adversarial scope before proof");

        let ready_deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < ready_deadline && !output.lock().contains("READY") {
            thread::sleep(Duration::from_millis(50));
        }
        let ready = output.lock().contains("READY");
        session.resize(91, 37).expect("resize double-PTY path");
        let resize_deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < resize_deadline && !output.lock().contains("SIZE 91 37") {
            thread::sleep(Duration::from_millis(50));
        }
        let resize_observed = output.lock().contains("SIZE 91 37");

        // Simulate abrupt loss of the exact Windows WSL client/job without
        // first asking systemd to stop the Linux scope.
        session
            .kill()
            .expect("terminate and prove adversarial Windows job empty");
        let reap_deadline = Instant::now() + Duration::from_secs(8);
        let linux_empty = loop {
            match control.scope_status(&scope) {
                Ok(None) => break true,
                Ok(Some(status))
                    if status.tasks_current == 0
                        && !matches!(status.active_state.as_str(), "active" | "activating") =>
                {
                    break true;
                }
                _ if Instant::now() < reap_deadline => {
                    thread::sleep(Duration::from_millis(100));
                }
                _ => break false,
            }
        };
        if !linux_empty {
            let _ = control.terminate_scope(&scope);
        }
        assert!(
            ready,
            "adversarial Linux payload never reached READY: {}",
            output.lock()
        );
        assert!(
            resize_observed,
            "Linux payload did not observe 91x37 through the double PTY: {}",
            output.lock()
        );
        assert!(
            linux_empty,
            "detached Linux task survived abrupt Windows job death"
        );
    }
}
