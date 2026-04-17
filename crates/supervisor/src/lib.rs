use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use control_plane::{DEFAULT_ENDPOINT, decode_request, encode_request, encode_response};
use parking_lot::{Mutex, RwLock};
use pty_host::{ConcretePtySession, PtyEvent, PtyEventHandler, PtySession};
use shared_types::{
    ControlKey, ControlPlaneStatus, DeliverMessageRequest, DriverKind, EnvVar, LaunchSpec,
    LifecycleState, LogLevel, MessageScope, RouteMessageRequest, RuntimeEvent, RuntimeSnapshot,
    SendInputRequest, SessionDefinition, SessionGeneration, SessionSnapshot, SidebandPhase,
    SidebandRequest, SidebandResponse, SidebandResponsePayload, WaitQuietRequest, now_rfc3339,
};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

const CONTROL_PLANE_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const CONTROL_PLANE_PROBE_RETRY_INTERVAL: Duration = Duration::from_millis(50);

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
            SidebandRequest::StartSession { .. } => Duration::from_secs(60),
            SidebandRequest::StopSession { .. } => Duration::from_secs(10),
            SidebandRequest::RestartSession { .. } => Duration::from_secs(70),
            SidebandRequest::WaitQuiet {
                timeout_seconds, ..
            } => Duration::from_secs((*timeout_seconds as u64).saturating_add(5)),
            SidebandRequest::DeliverMessage { .. } => Duration::from_secs(10),
            SidebandRequest::SendInput { .. } => Duration::from_secs(5),
            SidebandRequest::SendKey { .. } => Duration::from_secs(5),
            SidebandRequest::RouteMessage { .. } => Duration::from_secs(15),
        }
    }
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

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub working_root: PathBuf,
    pub runtime_dir: PathBuf,
    pub peer_slash_commands_allowed: bool,
}

struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    fn new(runtime_dir: &Path) -> Result<Self> {
        let audit_dir = runtime_dir.join("audit");
        fs::create_dir_all(&audit_dir).context("failed to create audit directory")?;
        let file_name = format!("{}.jsonl", Utc::now().format("%Y-%m-%d"));
        Ok(Self {
            path: audit_dir.join(file_name),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn append(&self, event: &RuntimeEvent) -> Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("failed to open audit log at {}", self.path.display()))?;
        let payload = serde_json::to_string(event).context("failed to serialize audit event")?;
        writeln!(file, "{payload}").context("failed to append audit event")?;
        Ok(())
    }
}

struct RunningSession {
    pty: Option<Box<dyn PtySession>>,
    generation: SessionGeneration,
}

struct SessionSlot {
    definition: SessionDefinition,
    state: LifecycleState,
    running: Option<RunningSession>,
    generation: SessionGeneration,
    process_id: Option<u32>,
    last_activity_at: Option<String>,
    last_real_output_at: Option<Instant>,
    last_error: Option<String>,
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

struct SupervisorInner {
    runtime_dir: PathBuf,
    audit: AuditLog,
    slots: Mutex<HashMap<String, SessionSlot>>,
    event_sink: RwLock<Option<EventSink>>,
    control_plane: RwLock<Option<ControlPlaneStatus>>,
    token_bindings: Mutex<HashMap<String, Option<String>>>,
    session_control_planes: Mutex<HashMap<String, ControlPlaneStatus>>,
    peer_slash_commands_allowed: bool,
    pty_spawner: RwLock<Arc<dyn PtySpawner>>,
    mailbox_fs: RwLock<Arc<dyn MailboxFs>>,
    stale_event_drop_counts: Mutex<HashMap<(String, SessionGeneration), u64>>,
    #[cfg(test)]
    last_detached_worker: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
}

#[derive(Clone)]
pub struct SupervisorHandle {
    inner: Arc<SupervisorInner>,
}

impl SupervisorHandle {
    pub fn new(config: SupervisorConfig) -> Result<Self> {
        fs::create_dir_all(&config.runtime_dir).context("failed to create runtime directory")?;
        let audit = AuditLog::new(&config.runtime_dir)?;
        let working_root = config.working_root.to_string_lossy().into_owned();
        let mut slots = HashMap::new();

        let claude = driver_claude::default_session(&working_root);
        slots.insert(
            claude.name.clone(),
            SessionSlot {
                definition: claude,
                state: LifecycleState::Closed,
                running: None,
                generation: 0,
                process_id: None,
                last_activity_at: None,
                last_real_output_at: None,
                last_error: None,
            },
        );

        let codex = driver_codex::default_session(&working_root);
        slots.insert(
            codex.name.clone(),
            SessionSlot {
                definition: codex,
                state: LifecycleState::Closed,
                running: None,
                generation: 0,
                process_id: None,
                last_activity_at: None,
                last_real_output_at: None,
                last_error: None,
            },
        );

        Ok(Self {
            inner: Arc::new(SupervisorInner {
                runtime_dir: config.runtime_dir,
                audit,
                slots: Mutex::new(slots),
                event_sink: RwLock::new(None),
                control_plane: RwLock::new(None),
                token_bindings: Mutex::new(HashMap::new()),
                session_control_planes: Mutex::new(HashMap::new()),
                peer_slash_commands_allowed: config.peer_slash_commands_allowed,
                pty_spawner: RwLock::new(Arc::new(ConcretePtySpawner)),
                mailbox_fs: RwLock::new(Arc::new(StdMailboxFs)),
                stale_event_drop_counts: Mutex::new(HashMap::new()),
                #[cfg(test)]
                last_detached_worker: Mutex::new(None),
            }),
        })
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.inner.runtime_dir
    }

    pub fn audit_log_path(&self) -> &Path {
        self.inner.audit.path()
    }

    fn bump_session_generation(&self, name: &str) -> Result<SessionGeneration> {
        let mut slots = self.inner.slots.lock();
        let slot = slots
            .get_mut(name)
            .with_context(|| format!("unknown session '{name}'"))?;
        slot.generation = slot.generation.wrapping_add(1);
        Ok(slot.generation)
    }

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
        phase: SidebandPhase,
        elapsed: Duration,
    ) {
        self.emit(RuntimeEvent::SidebandRequestLifecycle {
            request_id: request_id.to_string(),
            action: action.to_string(),
            session: session.map(ToOwned::to_owned),
            phase,
            elapsed_ms: elapsed.as_millis() as u64,
            timestamp: now_rfc3339(),
        });
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

    pub fn start_session(&self, name: &str) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let expected = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }
            slot.generation = slot.generation.wrapping_add(1);
            slot.generation
        };
        self.start_session_at(name, expected)
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

    fn stop_session_at(&self, name: &str, expected: SessionGeneration) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let pty = {
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

            slot.state = LifecycleState::Closed;
            slot.process_id = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            slot.running.take().and_then(|running| running.pty)
        };

        if let Some(pty) = pty {
            let (tx, rx) = std::sync::mpsc::channel();
            thread::spawn(move || {
                let _ = pty.kill();
                let _ = tx.send(());
            });
            if rx.recv_timeout(Duration::from_secs(5)).is_err() {
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!(
                        "pty.kill() for session '{name}' (gen {expected}) did not return within 5s; proceeding (resource may be leaked)"
                    ),
                    timestamp: now_rfc3339(),
                });
            }
        }

        let snapshot = {
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
            slot.snapshot()
        };

        self.emit(RuntimeEvent::SessionState {
            session: snapshot.name.clone(),
            state: snapshot.lifecycle_state,
            reason: "session stopped".into(),
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    fn start_session_at(&self, name: &str, expected: SessionGeneration) -> Result<SessionSnapshot> {
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

            slot.state = LifecycleState::Starting;
            slot.last_error = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.last_real_output_at = None;
            let snapshot = slot.snapshot();
            let definition = slot.definition.clone();
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
                    slot.running = Some(RunningSession {
                        pty: Some(pty),
                        generation: expected,
                    });
                    slot.state = LifecycleState::Ready;
                    slot.last_activity_at = Some(now_rfc3339());
                    slot.last_real_output_at = None;
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
            slot.state = LifecycleState::Restarting;
            slot.last_activity_at = Some(now_rfc3339());
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
        self.start_session_at(name, expected_start)
    }

    pub fn send_input(&self, request: SendInputRequest) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let snapshot = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(&request.name)
                .with_context(|| format!("unknown session '{}'", request.name))?;
            let running = slot
                .running
                .as_ref()
                .ok_or_else(|| anyhow!("session '{}' is not running", request.name))?;
            running
                .pty
                .as_ref()
                .ok_or_else(|| anyhow!("session '{}' transport is not available", request.name))?
                .send_input(&request.input)?;
            slot.state = LifecycleState::Busy;
            slot.last_activity_at = Some(now_rfc3339());
            slot.snapshot()
        };

        self.emit(RuntimeEvent::SessionState {
            session: snapshot.name.clone(),
            state: snapshot.lifecycle_state,
            reason: "input forwarded".into(),
            timestamp: now_rfc3339(),
        });

        Ok(snapshot)
    }

    pub fn send_control_key(&self, name: &str, key: ControlKey) -> Result<SessionSnapshot> {
        self.send_input(SendInputRequest {
            name: name.into(),
            input: control_key_sequence(key).into(),
        })
    }

    pub fn route_message(&self, request: RouteMessageRequest) -> Result<RuntimeSnapshot> {
        self.refresh_session_liveness();
        let recipients =
            self.resolve_recipients(&request.to, request.scope, Some(request.from.as_str()));
        if recipients.is_empty() {
            return Err(anyhow!(
                "no running recipients available for '{}'",
                request.to
            ));
        }

        let route_id = Uuid::new_v4();
        self.emit(RuntimeEvent::RoutedMessage {
            id: route_id,
            from: request.from.clone(),
            to: request.to.clone(),
            scope: request.scope,
            content: request.content.clone(),
            timestamp: now_rfc3339(),
        });

        for recipient in recipients {
            let submit_behavior = self.submit_behavior_for_session(&recipient)?;
            let payloads = routed_message_payloads(&request, submit_behavior);
            self.deliver_prepared_payloads(&recipient, &payloads, submit_behavior)?;
        }

        self.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: format!("Routed message from {} to {}", request.from, request.to),
            timestamp: now_rfc3339(),
        });

        Ok(self.snapshot())
    }

    pub fn deliver_message(&self, request: DeliverMessageRequest) -> Result<SessionSnapshot> {
        let (snapshot, submit_behavior, payloads) =
            self.prepare_delivery_for_session(&request.name, &request.content)?;

        self.deliver_prepared_payloads(&request.name, &payloads, submit_behavior)?;
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
        if let Some(existing_status) = load_existing_control_plane_status(&info_path)? {
            if probe_control_plane_owner(&existing_status, CONTROL_PLANE_PROBE_TIMEOUT)? {
                return Err(anyhow!(
                    "another wrapper instance is already holding the control plane at {}. Close the other instance before starting a new one.",
                    existing_status.endpoint
                ));
            }
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
            MessageScope::Room => slots
                .iter()
                .filter_map(|(name, slot)| {
                    (slot.running.is_some() && Some(name.as_str()) != sender).then(|| name.clone())
                })
                .collect(),
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

        {
            let mut slots = self.inner.slots.lock();
            for (session_name, slot) in slots.iter_mut() {
                if slot.running.is_none() {
                    continue;
                }

                let Some(process_id) = slot.process_id else {
                    continue;
                };

                if process_id_is_running(process_id) {
                    continue;
                }

                let timestamp = now_rfc3339();
                slot.running = None;
                slot.process_id = None;
                slot.state = LifecycleState::Closed;
                slot.last_activity_at = Some(timestamp.clone());
                slot.last_error = None;

                log_events.push(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!(
                        "{} process {} is no longer running; pruning stale session state",
                        slot.title(),
                        process_id
                    ),
                    timestamp: timestamp.clone(),
                });
                lifecycle_events.push(RuntimeEvent::SessionState {
                    session: session_name.clone(),
                    state: LifecycleState::Closed,
                    reason: "process no longer running".into(),
                    timestamp,
                });
            }
        }

        for event in log_events {
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
                let transitioned_to_ready = {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        let transitioned = slot.state != LifecycleState::Ready;
                        if slot.state != LifecycleState::Ready {
                            slot.state = LifecycleState::Ready;
                            slot.last_activity_at = Some(now_rfc3339());
                        }
                        if has_real_content {
                            slot.last_real_output_at = Some(Instant::now());
                        }
                        transitioned
                    } else {
                        false
                    }
                };

                if transitioned_to_ready {
                    self.emit(RuntimeEvent::SessionState {
                        session: session_name.into(),
                        state: LifecycleState::Ready,
                        reason: "session emitted output".into(),
                        timestamp: now_rfc3339(),
                    });
                }

                self.emit(RuntimeEvent::SessionOutput {
                    session: session_name.into(),
                    chunk,
                    synthetic: false,
                    timestamp: now_rfc3339(),
                });
            }
            PtyEvent::Closed => {
                {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        slot.running = None;
                        slot.process_id = None;
                        slot.state = LifecycleState::Closed;
                        slot.last_activity_at = Some(now_rfc3339());
                        slot.last_real_output_at = None;
                    }
                }
                self.emit(RuntimeEvent::SessionState {
                    session: session_name.into(),
                    state: LifecycleState::Closed,
                    reason: "session output closed".into(),
                    timestamp: now_rfc3339(),
                });
            }
            PtyEvent::Error(error) => {
                {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        slot.state = LifecycleState::Failed;
                        slot.last_error = Some(error.clone());
                        slot.running = None;
                        slot.process_id = None;
                        slot.last_activity_at = Some(now_rfc3339());
                        slot.last_real_output_at = None;
                    }
                }
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Error,
                    message: format!("{session_name} PTY error: {error}"),
                    timestamp: now_rfc3339(),
                });
                self.emit(RuntimeEvent::SessionState {
                    session: session_name.into(),
                    state: LifecycleState::Failed,
                    reason: "PTY error".into(),
                    timestamp: now_rfc3339(),
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
            });
        };

        if !self.inner.token_bindings.lock().contains_key(token) {
            return Some(SidebandResponse {
                ok: false,
                message: "invalid control plane token".into(),
                snapshot: None,
                timed_out: false,
                payload: None,
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
            },
            Err(error) => SidebandResponse {
                ok: false,
                message: error.to_string(),
                snapshot: Some(self.snapshot()),
                timed_out: false,
                payload: None,
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
            (SidebandRequest::StartSession { name, .. }, Some(expected)) => self
                .start_session_at(&name, expected)
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

    async fn apply_side_effect_request_async(&self, request: SidebandRequest) -> SidebandResponse {
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
            SidebandRequest::DeliverMessage {
                token,
                name,
                content,
            } => {
                tokio::task::yield_now().await;
                self.validate_deliver_message_token(&token, &name)
                    .and_then(|_| self.deliver_message(DeliverMessageRequest { name, content }))
                    .map(|_| "delivered".into())
            }
            SidebandRequest::SendInput { token, name, input } => {
                tokio::task::yield_now().await;
                self.validate_session_action_token(&token, &name)
                    .and_then(|_| self.send_input(SendInputRequest { name, input }))
                    .map(|_| "input sent".into())
            }
            SidebandRequest::SendKey { token, name, key } => {
                tokio::task::yield_now().await;
                self.validate_session_action_token(&token, &name)
                    .and_then(|_| self.send_control_key(&name, key))
                    .map(|_| format!("key {:?} sent", key))
            }
            SidebandRequest::RouteMessage { request, .. } => {
                tokio::task::yield_now().await;
                self.route_message(request).map(|_| "message routed".into())
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
                };
            }
        };

        self.sideband_response_from_outcome(outcome)
    }

    async fn apply_sideband_request_async(&self, request: SidebandRequest) -> SidebandResponse {
        let request_id = Uuid::new_v4().to_string();
        let action = action_label_for(&request).to_string();
        let session = session_name_of(&request).map(str::to_string);
        let budget = SidebandTimeouts::budget(&request);
        let started = Instant::now();

        self.emit_sideband_lifecycle(
            &request_id,
            &action,
            session.as_deref(),
            SidebandPhase::Started,
            Duration::ZERO,
        );

        let response = match SidebandTimeouts::lane(&request) {
            OpLane::Lifecycle => {
                self.run_detached_with_timeout_async(
                    request.clone(),
                    budget,
                    &request_id,
                    &action,
                    session.as_deref(),
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
                )
                .await
            }
        };

        if !response.timed_out {
            let phase = if response.ok {
                SidebandPhase::Completed
            } else {
                SidebandPhase::Failed
            };
            self.emit_sideband_lifecycle(
                &request_id,
                &action,
                session.as_deref(),
                phase,
                started.elapsed(),
            );
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
    ) -> SidebandResponse {
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
                        },
                    };
                }
                _ = &mut slow_warning, if !warned => {
                    warned = true;
                    self.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        SidebandPhase::SlowWarning,
                        started.elapsed(),
                    );
                }
                _ = &mut deadline => {
                    let elapsed = started.elapsed();
                    self.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        SidebandPhase::TimedOut,
                        elapsed,
                    );
                    return SidebandResponse {
                        ok: false,
                        message: format!(
                            "lifecycle op '{action}' timed out after {}ms",
                            elapsed.as_millis()
                        ),
                        snapshot: Some(self.snapshot()),
                        timed_out: true,
                        payload: None,
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
    ) -> SidebandResponse {
        let slow_warn_at = budget / 2;
        let started = Instant::now();
        let mut warned = false;
        let operation = self.apply_side_effect_request_async(request);
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
                        SidebandPhase::SlowWarning,
                        started.elapsed(),
                    );
                }
                _ = &mut deadline => {
                    self.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        SidebandPhase::TimedOut,
                        budget,
                    );
                    return SidebandResponse {
                        ok: false,
                        message: format!(
                            "side-effect op '{action}' timed out after {}ms (boundary pre-PTY-write unless documented otherwise)",
                            budget.as_millis()
                        ),
                        snapshot: Some(self.snapshot()),
                        timed_out: true,
                        payload: None,
                    };
                }
            }
        }
    }

    fn emit(&self, event: RuntimeEvent) {
        if let Err(error) = self.inner.audit.append(&event) {
            eprintln!("audit log failure: {error}");
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

        if let Some(bound_session) = binding {
            if bound_session != target_session {
                return Err(anyhow!(
                    "peer slash commands are disabled by this wrapper's policy (PRIM1_PEER_SLASH_COMMANDS_ALLOWED=0)"
                ));
            }
        }

        Ok(())
    }

    fn validate_deliver_message_token(&self, token: &str, target_session: &str) -> Result<()> {
        let binding = self
            .inner
            .token_bindings
            .lock()
            .get(token)
            .cloned()
            .ok_or_else(|| anyhow!("invalid control plane token"))?;

        if let Some(bound_session) = binding {
            if bound_session != target_session {
                return Err(anyhow!(
                    "deliver_message: pane-bound token cannot target other sessions; use route_message instead"
                ));
            }
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
        let payloads = prepare_direct_message(slot.definition.driver, content);
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
                    };
                };
                if slot.running.is_none() {
                    return SidebandResponse {
                        ok: false,
                        message: format!("session '{}' is not running", request.name),
                        snapshot: Some(self.snapshot()),
                        timed_out: false,
                        payload: None,
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
    ) -> Result<()> {
        for payload in payloads {
            self.send_input(SendInputRequest {
                name: session_name.into(),
                input: payload.clone(),
            })?;
            if !submit_behavior.delay.is_zero() {
                thread::sleep(submit_behavior.delay);
            }
            self.send_input(SendInputRequest {
                name: session_name.into(),
                input: submit_behavior.sequence.into(),
            })?;
        }

        Ok(())
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
    let budget = SidebandTimeouts::budget(&request);
    let started = Instant::now();

    handle.emit_sideband_lifecycle(
        &request_id,
        action,
        session,
        SidebandPhase::Started,
        Duration::ZERO,
    );

    let response = match SidebandTimeouts::lane(&request) {
        OpLane::Lifecycle => run_detached_with_timeout(
            handle,
            request.clone(),
            budget,
            &request_id,
            action,
            session,
        ),
        OpLane::SideEffect => run_inline_with_timeout(
            handle,
            request.clone(),
            budget,
            &request_id,
            action,
            session,
        ),
    };

    if !response.timed_out {
        let phase = if response.ok {
            SidebandPhase::Completed
        } else {
            SidebandPhase::Failed
        };
        handle.emit_sideband_lifecycle(&request_id, action, session, phase, started.elapsed());
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
) -> SidebandResponse {
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
                        SidebandPhase::SlowWarning,
                        elapsed,
                    );
                }
                if elapsed >= budget {
                    handle.emit_sideband_lifecycle(
                        request_id,
                        action,
                        session,
                        SidebandPhase::TimedOut,
                        elapsed,
                    );
                    return SidebandResponse {
                        ok: false,
                        message: format!(
                            "lifecycle op '{action}' timed out after {}ms",
                            elapsed.as_millis()
                        ),
                        snapshot: Some(handle.snapshot()),
                        timed_out: true,
                        payload: None,
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
) -> SidebandResponse {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("failed to build inline side-effect runtime");
    let started = Instant::now();
    let slow_warn_at = budget / 2;
    let handle_clone = handle.clone();
    let request_clone = request.clone();
    let result = runtime.block_on(async move {
        tokio::time::timeout(
            budget,
            handle_clone.apply_side_effect_request_async(request_clone),
        )
        .await
    });

    let elapsed = started.elapsed();
    if elapsed >= slow_warn_at {
        handle.emit_sideband_lifecycle(
            request_id,
            action,
            session,
            SidebandPhase::SlowWarning,
            elapsed,
        );
    }

    match result {
        Ok(response) => response,
        Err(_) => {
            handle.emit_sideband_lifecycle(
                request_id,
                action,
                session,
                SidebandPhase::TimedOut,
                budget,
            );
            SidebandResponse {
                ok: false,
                message: format!(
                    "side-effect op '{action}' timed out after {}ms (boundary pre-PTY-write unless documented otherwise)",
                    budget.as_millis()
                ),
                snapshot: Some(handle.snapshot()),
                timed_out: true,
                payload: None,
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

fn action_label_for(request: &SidebandRequest) -> &'static str {
    match request {
        SidebandRequest::Ping { .. } => "ping",
        SidebandRequest::ListSessions { .. } => "list_sessions",
        SidebandRequest::StartSession { .. } => "start_session",
        SidebandRequest::StopSession { .. } => "stop_session",
        SidebandRequest::RestartSession { .. } => "restart_session",
        SidebandRequest::DeliverMessage { .. } => "deliver_message",
        SidebandRequest::WaitQuiet { .. } => "wait_quiet",
        SidebandRequest::SendInput { .. } => "send_input",
        SidebandRequest::SendKey { .. } => "send_key",
        SidebandRequest::RouteMessage { .. } => "route_message",
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
        SidebandRequest::Ping { .. } | SidebandRequest::ListSessions { .. } => None,
    }
}

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
            max_chunk_chars: None,
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
    let mut char_count = 0;

    for (index, ch) in content.char_indices() {
        if char_count == max_chunk_chars {
            break;
        }
        if ch.is_whitespace() {
            last_whitespace_index = Some(index);
        }
        char_count += 1;
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
        send_input_count: Arc<AtomicUsize>,
        kill_count: Arc<AtomicUsize>,
        kill_behavior: MockKillBehavior,
    }

    impl PtySessionTrait for MockPtySession {
        fn send_input(&self, _input: &str) -> Result<()> {
            self.send_input_count.fetch_add(1, Ordering::SeqCst);
            Ok(())
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
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            self.process_id
        }
    }

    fn mock_pty_session(
        process_id: Option<u32>,
        kill_behavior: MockKillBehavior,
    ) -> (Box<dyn PtySessionTrait>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let send_input_count = Arc::new(AtomicUsize::new(0));
        let kill_count = Arc::new(AtomicUsize::new(0));
        (
            Box::new(MockPtySession {
                process_id,
                send_input_count: send_input_count.clone(),
                kill_count: kill_count.clone(),
                kill_behavior,
            }) as Box<dyn PtySessionTrait>,
            send_input_count,
            kill_count,
        )
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
        })
        .unwrap()
    }

    fn install_stale_running_session(supervisor: &SupervisorHandle, name: &str) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.running = Some(RunningSession {
            pty: None,
            generation: slot.generation,
        });
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
        slot.running = Some(RunningSession {
            pty: None,
            generation: slot.generation,
        });
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
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.definition.driver = driver;
        slot.running = Some(RunningSession {
            pty: Some(pty),
            generation: slot.generation,
        });
        slot.process_id = None;
        slot.state = LifecycleState::Busy;
        slot.last_real_output_at = None;
    }

    fn session_token(supervisor: &SupervisorHandle, name: &str) -> String {
        supervisor.start_control_plane().unwrap();
        supervisor
            .ensure_session_control_plane_status(name)
            .unwrap()
            .unwrap()
            .token
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

        assert_eq!(names, vec!["claude".to_string(), "codex".to_string()]);
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
    fn direct_targets_ignore_sender_filter() {
        let supervisor = test_supervisor();
        install_stale_running_session(&supervisor, "claude");

        let recipients =
            supervisor.resolve_recipients("claude", MessageScope::Direct, Some("claude"));

        assert_eq!(recipients, vec!["claude".to_string()]);
    }

    #[test]
    fn routed_message_payload_ends_with_terminal_submit() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "victor".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "tell me a joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Claude),
        );

        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from victor]"));
        assert!(payload.contains("tell me a joke"));
    }

    #[test]
    fn codex_payload_is_single_line() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "victor".into(),
                to: "codex".into(),
                scope: MessageScope::Direct,
                content: "tell me\na joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Codex),
        );

        assert!(!payload.contains('\n'));
        assert_eq!(payload, "[Direct message from victor] tell me a joke");
    }

    #[test]
    fn claude_payload_stays_multiline_even_with_delayed_submit() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "victor".into(),
                to: "claude".into(),
                scope: MessageScope::Direct,
                content: "tell me\na joke".into(),
            },
            routed_message_submit_behavior(DriverKind::Claude),
        );

        assert!(payload.starts_with('\n'));
        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from victor]"));
        assert!(payload.contains("tell me\na joke"));
    }

    #[test]
    fn generic_terminal_payload_uses_multiline_prompt_shape() {
        let payload = routed_message_payload(
            &RouteMessageRequest {
                from: "victor".into(),
                to: "terminal".into(),
                scope: MessageScope::Direct,
                content: "hello".into(),
            },
            routed_message_submit_behavior(DriverKind::GenericTerminal),
        );

        assert!(payload.starts_with('\n'));
        assert!(payload.ends_with('\n'));
        assert!(payload.contains("[Direct message from victor]"));
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
                max_chunk_chars: None,
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
            slot.running.as_mut().unwrap().generation = 2;
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
        );
        assert!(response.timed_out);

        let snapshot = supervisor.start_session("claude").unwrap();
        assert_eq!(snapshot.lifecycle_state, LifecycleState::Ready);
        assert!(supervisor.test_wait_for_last_worker(Duration::from_secs(1)));
        assert_eq!(supervisor.current_generation("claude"), Some(2));
        assert!(old_kill_count.load(Ordering::SeqCst) >= 1);

        let slots = supervisor.inner.slots.lock();
        let slot = slots.get("claude").unwrap();
        assert_eq!(slot.state, LifecycleState::Ready);
        assert_eq!(slot.running.as_ref().unwrap().generation, 2);
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
            },
            Duration::from_millis(50),
            "req-start",
            "start_session",
            Some("claude"),
        );
        assert!(response.timed_out);

        let snapshot = supervisor.start_session("claude").unwrap();
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
        assert_eq!(slot.running.as_ref().unwrap().generation, 2);
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
    fn route_message_requires_running_recipient() {
        let supervisor = test_supervisor();

        let error = supervisor
            .route_message(RouteMessageRequest {
                from: "victor".into(),
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
