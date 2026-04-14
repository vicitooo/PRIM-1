use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
};

use anyhow::{Context, Result, anyhow};
use chrono::Utc;
use control_plane::{DEFAULT_ENDPOINT, decode_request, encode_response};
use parking_lot::{Mutex, RwLock};
use pty_host::{PtyEvent, PtyEventHandler, PtySession};
use shared_types::{
    ControlPlaneStatus, DriverKind, LifecycleState, LogLevel, MessageScope, RouteMessageRequest,
    RuntimeEvent, RuntimeSnapshot, SendInputRequest, SessionDefinition, SessionSnapshot,
    SidebandRequest, SidebandResponse, now_rfc3339,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader},
};
use uuid::Uuid;

type EventSink = Arc<dyn Fn(RuntimeEvent) + Send + Sync>;

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
    pty: PtySession,
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
                    slot.running = Some(RunningSession { pty });
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
        let pty = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(name)
                .with_context(|| format!("unknown session '{name}'"))?;
            slot.state = LifecycleState::Closed;
            slot.process_id = None;
            slot.last_activity_at = Some(now_rfc3339());
            slot.running.take().map(|running| running.pty)
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
        let snapshot = {
            let mut slots = self.inner.slots.lock();
            let slot = slots
                .get_mut(&request.name)
                .with_context(|| format!("unknown session '{}'", request.name))?;
            let running = slot
                .running
                .as_ref()
                .ok_or_else(|| anyhow!("session '{}' is not running", request.name))?;
            running.pty.send_input(&request.input)?;
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

    pub fn route_message(&self, request: RouteMessageRequest) -> Result<RuntimeSnapshot> {
        let recipients = self.resolve_recipients(&request.to, request.scope);
        if recipients.is_empty() {
            return Err(anyhow!("no running recipients available for '{}'", request.to));
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
            let synthetic = format!(
                "\r\n\x1b[38;5;179m[{} -> {} / {}]\x1b[0m {}\r\n",
                request.from,
                recipient,
                scope_label(request.scope),
                request.content
            );
            self.emit(RuntimeEvent::SessionOutput {
                session: recipient.clone(),
                chunk: synthetic,
                synthetic: true,
                timestamp: now_rfc3339(),
            });

            let payload = format!(
                "\n[{} message from {}]\n{}\n\n",
                scope_label(request.scope),
                request.from,
                request.content
            );
            self.send_input(SendInputRequest {
                name: recipient,
                input: payload,
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
        let slots = self.inner.slots.lock();
        let slot = slots
            .get(name)
            .with_context(|| format!("unknown session '{name}'"))?;
        let running = slot
            .running
            .as_ref()
            .ok_or_else(|| anyhow!("session '{name}' is not running"))?;
        running.pty.resize(cols, rows)?;
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
                .and_then(|slot| slot.running.as_ref().map(|_| vec![slot.definition.name.clone()]))
                .unwrap_or_default(),
        }
    }

    fn handle_pty_event(&self, session_name: &str, event: PtyEvent) {
        match event {
            PtyEvent::Output(chunk) => {
                {
                    let mut slots = self.inner.slots.lock();
                    if let Some(slot) = slots.get_mut(session_name) {
                        slot.state = LifecycleState::Ready;
                        slot.last_activity_at = Some(now_rfc3339());
                    }
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
            SidebandRequest::StartSession { name, .. } => self
                .start_session(&name)
                .map(|_| format!("started {name}")),
            SidebandRequest::StopSession { name, .. } => {
                self.stop_session(&name).map(|_| format!("stopped {name}"))
            }
            SidebandRequest::RestartSession { name, .. } => self
                .restart_session(&name)
                .map(|_| format!("restarted {name}")),
            SidebandRequest::SendInput { name, input, .. } => self
                .send_input(SendInputRequest { name, input })
                .map(|_| "input sent".into()),
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

fn spawn_control_plane_thread(handle: SupervisorHandle, status: ControlPlaneStatus) {
    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
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
async fn run_windows_pipe_server(handle: SupervisorHandle, status: ControlPlaneStatus) -> Result<()> {
    use tokio::net::windows::named_pipe::ServerOptions;

    loop {
        let server = ServerOptions::new()
            .create(&status.endpoint)
            .with_context(|| format!("failed to create named pipe {}", status.endpoint))?;
        server.connect().await.context("failed to connect named pipe")?;
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_sideband_stream(handle_clone, server).await {
                eprintln!("named pipe connection failed: {error}");
            }
        });
    }
}

#[cfg(unix)]
async fn run_unix_socket_server(handle: SupervisorHandle, status: ControlPlaneStatus) -> Result<()> {
    use tokio::net::UnixListener;

    let _ = fs::remove_file(&status.endpoint);
    let listener = UnixListener::bind(&status.endpoint)
        .with_context(|| format!("failed to bind unix socket {}", status.endpoint))?;

    loop {
        let (stream, _) = listener.accept().await.context("failed to accept unix socket")?;
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
    write_half.flush().await.context("failed to flush sideband response")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::MessageScope;

    #[test]
    fn snapshot_contains_default_sessions() {
        let root = std::env::temp_dir().join(format!("cli-master-wrapper-test-{}", Uuid::new_v4()));
        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
        })
        .unwrap();

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
        let root = std::env::temp_dir().join(format!("cli-master-wrapper-test-{}", Uuid::new_v4()));
        let supervisor = SupervisorHandle::new(SupervisorConfig {
            working_root: root.clone(),
            runtime_dir: root.join("runtime"),
        })
        .unwrap();

        let recipients = supervisor.resolve_recipients("room", MessageScope::Room);
        assert!(recipients.is_empty());
    }
}
