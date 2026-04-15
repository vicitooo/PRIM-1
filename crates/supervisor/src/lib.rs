use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use control_plane::{DEFAULT_ENDPOINT, decode_request, encode_response};
use parking_lot::{Mutex, RwLock};
use pty_host::{PtyEvent, PtyEventHandler, PtySession};
use shared_types::{
    ControlKey, ControlPlaneStatus, DriverKind, LifecycleState, LogLevel, MessageScope,
    RouteMessageRequest, RuntimeEvent, RuntimeSnapshot, SendInputRequest, SessionDefinition,
    SessionSnapshot, SidebandRequest, SidebandResponse, now_rfc3339,
};
use tokio::io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use uuid::Uuid;

type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SubmitBehavior {
    sequence: &'static str,
    delay: Duration,
    flatten_payload: bool,
}

#[derive(Debug, Clone)]
pub struct SupervisorConfig {
    pub working_root: PathBuf,
    pub runtime_dir: PathBuf,
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
    pty: Option<PtySession>,
}

struct SessionSlot {
    definition: SessionDefinition,
    state: LifecycleState,
    running: Option<RunningSession>,
    process_id: Option<u32>,
    last_activity_at: Option<String>,
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
                process_id: None,
                last_activity_at: None,
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
                process_id: None,
                last_activity_at: None,
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
            }),
        })
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.inner.runtime_dir
    }

    pub fn audit_log_path(&self) -> &Path {
        self.inner.audit.path()
    }

    pub fn set_event_sink<F>(&self, sink: F)
    where
        F: Fn(RuntimeEvent) + Send + Sync + 'static,
    {
        *self.inner.event_sink.write() = Some(Arc::new(sink));
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
        let definition = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            if slot.running.is_some() {
                return Ok(slot.snapshot());
            }

            slot.state = LifecycleState::Starting;
            slot.last_error = None;
            slot.last_activity_at = Some(now_rfc3339());
            let snapshot = slot.snapshot();
            drop(slots);
            self.emit(RuntimeEvent::SessionState {
                session: snapshot.name.clone(),
                state: snapshot.lifecycle_state,
                reason: "launch requested".into(),
                timestamp: now_rfc3339(),
            });
            let slots = self.inner.slots.lock();
            slots
                .get(name)
                .expect("session disappeared after state update")
                .definition
                .clone()
        };

        let spec = build_launch_spec(&definition);
        let session_name = definition.name.clone();
        let handle = self.clone();
        let handler: PtyEventHandler = Arc::new(move |event| {
            handle.handle_pty_event(&session_name, event);
        });

        match PtySession::spawn(&spec, handler) {
            Ok(pty) => {
                let snapshot = {
                    let mut slots = self.inner.slots.lock();
                    let slot = slots
                        .get_mut(name)
                        .expect("session disappeared during spawn success");
                    slot.process_id = pty.process_id();
                    slot.running = Some(RunningSession { pty: Some(pty) });
                    slot.state = LifecycleState::Ready;
                    slot.last_activity_at = Some(now_rfc3339());
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
                let snapshot = {
                    let mut slots = self.inner.slots.lock();
                    let slot = slots
                        .get_mut(name)
                        .expect("session disappeared during spawn failure");
                    slot.running = None;
                    slot.process_id = None;
                    slot.state = LifecycleState::Failed;
                    slot.last_error = Some(error.to_string());
                    slot.snapshot()
                };
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
                Err(error)
            }
        }
    }

    pub fn stop_session(&self, name: &str) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        let pty = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            slot.state = LifecycleState::Closed;
            slot.process_id = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.running.take().and_then(|running| running.pty)
        };

        if let Some(pty) = pty {
            let _ = pty.kill();
        }

        let snapshot = self
            .inner
            .slots
            .lock()
            .get(name)
            .expect("session disappeared during stop")
            .snapshot();

        self.emit(RuntimeEvent::SessionState {
            session: snapshot.name.clone(),
            state: snapshot.lifecycle_state,
            reason: "session stopped".into(),
            timestamp: now_rfc3339(),
        });
        Ok(snapshot)
    }

    pub fn restart_session(&self, name: &str) -> Result<SessionSnapshot> {
        self.refresh_session_liveness();
        {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
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

        let _ = self.stop_session(name);
        self.start_session(name)
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
        let recipients = self.resolve_recipients(&request.to, request.scope);
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
            let payload = routed_message_payload(&request, submit_behavior);
            self.send_input(SendInputRequest {
                name: recipient.clone(),
                input: payload,
            })?;
            if !submit_behavior.delay.is_zero() {
                thread::sleep(submit_behavior.delay);
            }
            self.send_input(SendInputRequest {
                name: recipient,
                input: submit_behavior.sequence.into(),
            })?;
        }

        self.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: format!("Routed message from {} to {}", request.from, request.to),
            timestamp: now_rfc3339(),
        });

        Ok(self.snapshot())
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

        let endpoint = control_plane_endpoint();
        let status = ControlPlaneStatus {
            transport: control_plane_transport().into(),
            endpoint: endpoint.clone(),
            token: Uuid::new_v4().to_string(),
            info_path: self
                .runtime_dir()
                .join("control-plane.json")
                .display()
                .to_string(),
        };

        fs::write(
            self.runtime_dir().join("control-plane.json"),
            serde_json::to_string_pretty(&status)?,
        )
        .context("failed to persist control plane info file")?;

        *self.inner.control_plane.write() = Some(status.clone());
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

    fn resolve_recipients(&self, to: &str, scope: MessageScope) -> Vec<String> {
        let slots = self.inner.slots.lock();
        match scope {
            MessageScope::Room => slots
                .iter()
                .filter_map(|(name, slot)| slot.running.as_ref().map(|_| name.clone()))
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

    fn handle_pty_event(&self, session_name: &str, event: PtyEvent) {
        match event {
            PtyEvent::Output(chunk) => {
                let transitioned_to_ready = {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        if slot.state != LifecycleState::Ready {
                            slot.state = LifecycleState::Ready;
                            slot.last_activity_at = Some(now_rfc3339());
                            true
                        } else {
                            false
                        }
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
        let Some(control_plane) = self.inner.control_plane.read().clone() else {
            return SidebandResponse {
                ok: false,
                message: "control plane not ready".into(),
                snapshot: None,
            };
        };

        if request.token() != control_plane.token {
            return SidebandResponse {
                ok: false,
                message: "invalid control plane token".into(),
                snapshot: None,
            };
        }

        let outcome = match request {
            SidebandRequest::Ping { .. } => Ok("pong".into()),
            SidebandRequest::ListSessions { .. } => Ok("sessions listed".into()),
            SidebandRequest::StartSession { name, .. } => {
                self.start_session(&name).map(|_| format!("started {name}"))
            }
            SidebandRequest::StopSession { name, .. } => {
                self.stop_session(&name).map(|_| format!("stopped {name}"))
            }
            SidebandRequest::RestartSession { name, .. } => self
                .restart_session(&name)
                .map(|_| format!("restarted {name}")),
            SidebandRequest::SendInput { name, input, .. } => self
                .send_input(SendInputRequest { name, input })
                .map(|_| "input sent".into()),
            SidebandRequest::SendKey { name, key, .. } => self
                .send_control_key(&name, key)
                .map(|_| format!("key {:?} sent", key)),
            SidebandRequest::RouteMessage { request, .. } => {
                self.route_message(request).map(|_| "message routed".into())
            }
        };

        match outcome {
            Ok(message) => SidebandResponse {
                ok: true,
                message,
                snapshot: Some(self.snapshot()),
            },
            Err(error) => SidebandResponse {
                ok: false,
                message: error.to_string(),
                snapshot: Some(self.snapshot()),
            },
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
    let raw = fs::read_to_string(request_path)
        .with_context(|| format!("failed to read mailbox request {}", request_path.display()))?;
    let response = match decode_request(raw.trim()) {
        Ok(request) => handle.apply_sideband_request(request),
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

            SidebandResponse {
                ok: false,
                message: format!("invalid sideband payload: {error}"),
                snapshot: Some(handle.snapshot()),
            }
        }
    };

    let file_name = request_path.file_name().with_context(|| {
        format!(
            "mailbox request missing file name: {}",
            request_path.display()
        )
    })?;
    let response_path = outbox_dir.join(file_name);
    let temp_response_path = outbox_dir.join(format!("{}.tmp", file_name.to_string_lossy()));
    fs::write(
        &temp_response_path,
        format!("{}\n", encode_response(&response)?),
    )
    .with_context(|| {
        format!(
            "failed to write mailbox response {}",
            temp_response_path.display()
        )
    })?;
    fs::rename(&temp_response_path, &response_path).with_context(|| {
        format!(
            "failed to publish mailbox response {}",
            response_path.display()
        )
    })?;

    let archived_request_path = processed_dir.join(file_name);
    if archived_request_path.exists() {
        fs::remove_file(&archived_request_path).with_context(|| {
            format!(
                "failed to clear archived mailbox request {}",
                archived_request_path.display()
            )
        })?;
    }
    fs::rename(request_path, &archived_request_path).with_context(|| {
        format!(
            "failed to archive mailbox request {}",
            request_path.display()
        )
    })?;

    Ok(())
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

fn routed_message_payload(request: &RouteMessageRequest, behavior: SubmitBehavior) -> String {
    if !behavior.flatten_payload {
        return format!(
            "\n[{} message from {}]\n{}\n",
            scope_label(request.scope),
            request.from,
            request.content
        );
    }

    collapse_inline_content(&request.content)
}

fn routed_message_submit_behavior(driver: DriverKind) -> SubmitBehavior {
    match driver {
        DriverKind::Codex => SubmitBehavior {
            sequence: "\r",
            delay: Duration::from_millis(500),
            flatten_payload: true,
        },
        DriverKind::Claude => SubmitBehavior {
            sequence: "\r",
            delay: Duration::from_millis(200),
            flatten_payload: false,
        },
        DriverKind::GenericTerminal => SubmitBehavior {
            sequence: "\r",
            delay: Duration::ZERO,
            flatten_payload: false,
        },
    }
}

fn collapse_inline_content(content: &str) -> String {
    content.split_whitespace().collect::<Vec<_>>().join(" ")
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
    let response = handle.apply_sideband_request(request);
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
    use shared_types::{MessageScope, RouteMessageRequest, SidebandRequest};

    fn test_supervisor() -> SupervisorHandle {
        let root = std::env::temp_dir().join(format!("cli-master-wrapper-test-{}", Uuid::new_v4()));
        SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
        })
        .unwrap()
    }

    fn install_stale_running_session(supervisor: &SupervisorHandle, name: &str) {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_mut(name).unwrap();
        slot.running = Some(RunningSession { pty: None });
        slot.process_id = Some(u32::MAX);
        slot.state = LifecycleState::Busy;
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

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room);
        assert!(recipients.is_empty());
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
        assert_eq!(payload, "tell me a joke");
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
    fn routed_message_submit_behavior_is_driver_aware() {
        assert_eq!(
            routed_message_submit_behavior(DriverKind::Claude),
            SubmitBehavior {
                sequence: "\r",
                delay: Duration::from_millis(200),
                flatten_payload: false,
            }
        );
        assert_eq!(
            routed_message_submit_behavior(DriverKind::Codex),
            SubmitBehavior {
                sequence: "\r",
                delay: Duration::from_millis(500),
                flatten_payload: true,
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
