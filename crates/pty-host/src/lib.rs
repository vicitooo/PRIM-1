use std::{
    collections::HashSet,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shared_types::{DriverKind, EnvVar, LaunchSpec};

mod job;
#[cfg(windows)]
pub use job::PinnedProcess;
pub use job::ProcessJob;

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Output(String),
    Closed,
    Error(String),
}

pub type PtyEventHandler = Arc<dyn Fn(PtyEvent) + Send + Sync>;

#[derive(Default)]
struct Utf8StreamDecoder {
    pending: Vec<u8>,
}

impl Utf8StreamDecoder {
    fn push(&mut self, bytes: &[u8]) -> Option<String> {
        let mut input = Vec::with_capacity(self.pending.len() + bytes.len());
        input.append(&mut self.pending);
        input.extend_from_slice(bytes);

        let mut output = String::new();
        let mut remaining = input.as_slice();
        while !remaining.is_empty() {
            match std::str::from_utf8(remaining) {
                Ok(text) => {
                    output.push_str(text);
                    break;
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    if valid > 0 {
                        output.push_str(
                            std::str::from_utf8(&remaining[..valid])
                                .expect("UTF-8 validator returned an invalid prefix"),
                        );
                        remaining = &remaining[valid..];
                    }

                    match error.error_len() {
                        Some(invalid) => {
                            output.push('\u{fffd}');
                            remaining = &remaining[invalid..];
                        }
                        None => {
                            self.pending.extend_from_slice(remaining);
                            break;
                        }
                    }
                }
            }
        }

        (!output.is_empty()).then_some(output)
    }

    fn finish(&mut self) -> Option<String> {
        if self.pending.is_empty() {
            None
        } else {
            Some(String::from_utf8_lossy(&std::mem::take(&mut self.pending)).into_owned())
        }
    }
}

const LEGACY_PANE_CREDENTIAL_ENV: &str = "PRIM1_PANE_CREDENTIALS";
const INHERITED_CONTROL_ENV: [&str; 6] = [
    LEGACY_PANE_CREDENTIAL_ENV,
    "PRIM1_CONTROL_PLANE_ENDPOINT",
    "PRIM1_CONTROL_PLANE_TRANSPORT",
    "PRIM1_PANE_IDENTITY",
    "PRIM1_CONTROL_PLANE_SERVER_PID",
    "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME",
];

fn apply_launch_environment(command: &mut CommandBuilder, env: &[EnvVar]) -> Result<()> {
    for key in INHERITED_CONTROL_ENV {
        command.env_remove(key);
    }
    for env_var in env {
        if env_var.key.eq_ignore_ascii_case(LEGACY_PANE_CREDENTIAL_ENV) {
            return Err(anyhow!(
                "legacy pane credential environment is not permitted"
            ));
        }
        command.env(&env_var.key, &env_var.value);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PtyExitStatus {
    pub exit_code: u32,
    pub signal: Option<String>,
    pub success: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessIdentity {
    pub process_id: u32,
    pub image_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentLiveness {
    Alive(Vec<ProcessIdentity>),
    NotYetObserved(Vec<ProcessIdentity>),
    Exited {
        last_agent_pids: Vec<u32>,
        live_processes: Vec<ProcessIdentity>,
    },
}

#[derive(Debug)]
pub struct PtyWriteError {
    bytes_written: usize,
    message: String,
    source: Option<std::io::Error>,
}

impl PtyWriteError {
    pub fn new(bytes_written: usize, message: impl Into<String>) -> Self {
        Self {
            bytes_written,
            message: message.into(),
            source: None,
        }
    }

    fn from_io(bytes_written: usize, context: &str, source: std::io::Error) -> Self {
        Self {
            bytes_written,
            message: format!("{context}: {source}"),
            source: Some(source),
        }
    }

    pub fn bytes_written(&self) -> usize {
        self.bytes_written
    }
}

impl std::fmt::Display for PtyWriteError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PtyWriteError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_ref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}

impl PtyWriteError {
    pub fn io_error(&self) -> Option<&std::io::Error> {
        self.source.as_ref()
    }
}

pub type PtyWriteResult = std::result::Result<usize, PtyWriteError>;

fn write_pty_input(writer: &mut dyn Write, input: &[u8]) -> PtyWriteResult {
    let mut bytes_written = 0;
    while bytes_written < input.len() {
        match writer.write(&input[bytes_written..]) {
            Ok(0) => {
                return Err(PtyWriteError::from_io(
                    bytes_written,
                    "failed to write PTY input",
                    std::io::Error::from(std::io::ErrorKind::WriteZero),
                ));
            }
            Ok(count) => bytes_written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => {
                return Err(PtyWriteError::from_io(
                    bytes_written,
                    "failed to write PTY input",
                    error,
                ));
            }
        }
    }
    if let Err(error) = writer.flush() {
        return Err(PtyWriteError::from_io(
            bytes_written,
            "failed to flush PTY input",
            error,
        ));
    }
    Ok(bytes_written)
}

pub trait PtySession: Send + Sync {
    fn send_input(&self, input: &str) -> PtyWriteResult;
    /// Cancels an in-flight input write without closing the PTY or terminating
    /// its process tree. Implementations must fail closed when they cannot
    /// provide this distinction.
    fn cancel_input_write(&self) -> anyhow::Result<()> {
        Err(anyhow!(
            "PTY session does not support isolated input-write cancellation"
        ))
    }
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;
    /// Terminates the complete process scope owned by this PTY session.
    ///
    /// `Ok(())` is a proof-bearing result: every root and descendant in the
    /// session's owned process scope has terminated before the method returns.
    /// Killing only the immediate child, observing PTY EOF, or relying on
    /// handle-drop cleanup is not sufficient. On error the session object must
    /// remain usable for a later termination attempt; callers retain it until
    /// termination is proved.
    fn kill(&self) -> anyhow::Result<()>;
    /// Returns the immediate child status for diagnostics. This is not a
    /// substitute for the whole-scope proof supplied by `kill`.
    fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>>;
    fn process_id(&self) -> Option<u32>;
    /// Snapshot-only diagnostic. Never use a retained PID for authorization.
    fn contains_process_id(&self, _process_id: u32) -> anyhow::Result<bool> {
        Err(anyhow!(
            "PTY session does not expose process-job membership"
        ))
    }
    /// Authoritative Windows job-membership check using a pinned process handle.
    #[cfg(windows)]
    fn contains_process(&self, _process: &PinnedProcess) -> anyhow::Result<bool> {
        Err(anyhow!(
            "PTY session does not expose pinned-process job membership"
        ))
    }
    fn note_real_output(&self, _driver: DriverKind) {}
    fn agent_alive(&self, _driver: DriverKind) -> anyhow::Result<AgentLiveness> {
        Ok(AgentLiveness::NotYetObserved(Vec::new()))
    }
}

pub struct ConcretePtySession {
    master: Mutex<Option<Box<dyn MasterPty + Send>>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    #[cfg(windows)]
    writer_handle: usize,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_id: Option<u32>,
    agent_seen: Arc<AtomicBool>,
    agent_pids_seen: Arc<Mutex<HashSet<u32>>>,
    #[cfg(windows)]
    job: ProcessJob,
}

impl ConcretePtySession {
    pub fn spawn(spec: &LaunchSpec, handler: PtyEventHandler) -> anyhow::Result<Self> {
        Self::spawn_with_size(spec, None, handler)
    }

    /// Spawn at the pane's real size when known: a harness that paints (or
    /// replays) before the UI's first fit-resize otherwise wraps for the
    /// 120x40 default and reflows into scatter.
    pub fn spawn_with_size(
        spec: &LaunchSpec,
        size: Option<(u16, u16)>,
        handler: PtyEventHandler,
    ) -> anyhow::Result<Self> {
        let (cols, rows) = size.unwrap_or((120, 40));
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.working_dir);

        apply_launch_environment(&mut command, &spec.env)?;

        #[cfg(windows)]
        let job = ProcessJob::new().context("failed to create per-session process job")?;

        #[cfg(windows)]
        let child = pair
            .slave
            .spawn_command_in_job(command, job.raw_handle())
            .with_context(|| {
                format!(
                    "failed to spawn {} in per-session process job",
                    spec.program
                )
            })?;

        #[cfg(windows)]
        job.track_creation_assigned_process(
            child
                .as_raw_handle()
                .ok_or_else(|| anyhow!("spawned Windows child did not expose a process handle"))?,
        )
        .context("failed to pin the creation-assigned PTY root process")?;

        #[cfg(not(windows))]
        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to spawn {}", spec.program))?;
        drop(pair.slave);
        let process_id = child.process_id();

        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        #[cfg(windows)]
        let (writer, writer_handle) = pair
            .master
            .take_writer_with_raw_handle()
            .context("failed to take interruptible PTY writer")?;
        #[cfg(not(windows))]
        let writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;
        let child = Arc::new(Mutex::new(child));

        let reader_handler = Arc::clone(&handler);
        thread::spawn(move || {
            let mut reader = reader;
            let mut buffer = [0_u8; 4096];
            let mut decoder = Utf8StreamDecoder::default();

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        if let Some(output) = decoder.finish() {
                            reader_handler(PtyEvent::Output(output));
                        }
                        reader_handler(PtyEvent::Closed);
                        break;
                    }
                    Ok(size) => {
                        if let Some(output) = decoder.push(&buffer[..size]) {
                            reader_handler(PtyEvent::Output(output));
                        }
                    }
                    Err(error) => {
                        if let Some(output) = decoder.finish() {
                            reader_handler(PtyEvent::Output(output));
                        }
                        reader_handler(PtyEvent::Error(error.to_string()));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master: Mutex::new(Some(pair.master)),
            writer: Arc::new(Mutex::new(writer)),
            #[cfg(windows)]
            writer_handle: writer_handle as usize,
            child,
            process_id,
            agent_seen: Arc::new(AtomicBool::new(false)),
            agent_pids_seen: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(windows)]
            job,
        })
    }

    fn kill_immediate_child(&self) -> Result<()> {
        self.child
            .lock()
            .expect("pty child poisoned")
            .kill()
            .context("failed to kill PTY child")?;
        Ok(())
    }

    fn wait_for_child_exit(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            let wait_result = self.child.lock().expect("pty child poisoned").try_wait();
            match wait_result {
                Ok(Some(_)) => break,
                Ok(None) => thread::sleep(Duration::from_millis(10)),
                Err(_) => break,
            }
        }
    }

    fn close_master(&self) {
        self.master.lock().expect("pty master poisoned").take();
    }

    #[cfg(windows)]
    fn interrupt_writer(&self) -> Result<()> {
        use windows_sys::Win32::{Foundation::ERROR_NOT_FOUND, System::IO::CancelIoEx};

        // A zero handle is reserved for test doubles that interrupt on master
        // drop and never enter the Windows I/O subsystem.
        if self.writer_handle == 0 {
            return Ok(());
        }

        let cancelled = unsafe {
            CancelIoEx(
                self.writer_handle as windows_sys::Win32::Foundation::HANDLE,
                std::ptr::null(),
            )
        };
        if cancelled != 0 {
            return Ok(());
        }

        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_NOT_FOUND as i32) {
            return Ok(());
        }
        Err(error).context("CancelIoEx failed for PTY writer")
    }

    fn live_process_identities(&self) -> Result<Vec<ProcessIdentity>> {
        #[cfg(windows)]
        {
            self.job
                .live_process_ids()
                .map(|process_ids| process_identities_for_ids(&process_ids))
        }

        #[cfg(unix)]
        {
            let Some(process_id) = self.process_id else {
                return Ok(Vec::new());
            };
            Ok(process_group_process_identities(process_id))
        }
    }

    fn current_agent_processes(&self, driver: DriverKind) -> Result<Vec<ProcessIdentity>> {
        let live_processes = self.live_process_identities()?;
        if driver == DriverKind::GenericTerminal {
            return Ok(live_processes);
        }

        Ok(live_processes
            .into_iter()
            .filter(|process| {
                process
                    .image_name
                    .as_deref()
                    .map(|name| expected_agent_image_name(driver, name))
                    .unwrap_or(false)
            })
            .collect())
    }

    fn record_agent_processes(&self, processes: &[ProcessIdentity]) {
        if processes.is_empty() {
            return;
        }

        self.agent_seen.store(true, Ordering::SeqCst);
        let mut seen = self
            .agent_pids_seen
            .lock()
            .expect("agent pid cache poisoned");
        for process in processes {
            seen.insert(process.process_id);
        }
    }
}

impl PtySession for ConcretePtySession {
    fn send_input(&self, input: &str) -> PtyWriteResult {
        let mut writer = self.writer.lock().expect("pty writer poisoned");
        write_pty_input(writer.as_mut(), input.as_bytes())
    }

    fn cancel_input_write(&self) -> anyhow::Result<()> {
        #[cfg(windows)]
        {
            self.interrupt_writer()
        }

        #[cfg(not(windows))]
        {
            Err(anyhow!(
                "PTY session does not support isolated input-write cancellation"
            ))
        }
    }

    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master
            .lock()
            .expect("pty master poisoned")
            .as_ref()
            .context("PTY master is closed")?
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize PTY")?;
        Ok(())
    }

    fn kill(&self) -> anyhow::Result<()> {
        let mut first_error = None;

        // Cancel the borrowed writer handle before closing the PTY control end.
        // Neither operation takes the writer mutex: the thread being
        // interrupted can be holding it inside a synchronous WriteFile call.
        #[cfg(windows)]
        if let Err(error) = self.interrupt_writer() {
            first_error = Some(error);
        }
        self.close_master();

        #[cfg(windows)]
        if let Err(error) = self
            .job
            .terminate()
            .context("failed to terminate PTY process job")
        {
            first_error = Some(error);
        }

        if let Err(error) = self.kill_immediate_child() {
            first_error.get_or_insert(error);
        }

        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>> {
        let mut child = self.child.lock().expect("pty child poisoned");
        let status = child.try_wait().context("failed to poll PTY child")?;
        Ok(status.map(|status| PtyExitStatus {
            exit_code: status.exit_code(),
            signal: status.signal().map(ToOwned::to_owned),
            success: status.success(),
        }))
    }

    fn process_id(&self) -> Option<u32> {
        self.process_id
    }

    fn contains_process_id(&self, process_id: u32) -> anyhow::Result<bool> {
        #[cfg(windows)]
        {
            self.job.contains_process_id(process_id)
        }

        #[cfg(not(windows))]
        {
            let _ = process_id;
            Ok(false)
        }
    }

    #[cfg(windows)]
    fn contains_process(&self, process: &PinnedProcess) -> anyhow::Result<bool> {
        self.job.contains_process(process)
    }

    fn note_real_output(&self, driver: DriverKind) {
        if let Ok(processes) = self.current_agent_processes(driver) {
            self.record_agent_processes(&processes);
        }
    }

    fn agent_alive(&self, driver: DriverKind) -> anyhow::Result<AgentLiveness> {
        let live_processes = self.live_process_identities()?;
        let agent_processes = if driver == DriverKind::GenericTerminal {
            live_processes.clone()
        } else {
            live_processes
                .iter()
                .filter(|process| {
                    process
                        .image_name
                        .as_deref()
                        .map(|name| expected_agent_image_name(driver, name))
                        .unwrap_or(false)
                })
                .cloned()
                .collect::<Vec<_>>()
        };

        if !agent_processes.is_empty() {
            self.record_agent_processes(&agent_processes);
            return Ok(AgentLiveness::Alive(agent_processes));
        }

        if self.agent_seen.load(Ordering::SeqCst) {
            let mut last_agent_pids = self
                .agent_pids_seen
                .lock()
                .expect("agent pid cache poisoned")
                .iter()
                .copied()
                .collect::<Vec<_>>();
            last_agent_pids.sort_unstable();
            return Ok(AgentLiveness::Exited {
                last_agent_pids,
                live_processes,
            });
        }

        Ok(AgentLiveness::NotYetObserved(live_processes))
    }
}

impl Drop for ConcretePtySession {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = self.interrupt_writer();
        self.close_master();

        #[cfg(windows)]
        let _ = self.job.terminate();

        let _ = self.kill_immediate_child();
        self.wait_for_child_exit(Duration::from_millis(200));
    }
}

pub fn expected_agent_image_name(driver: DriverKind, image_name: &str) -> bool {
    let base = process_image_basename(image_name).to_ascii_lowercase();
    if matches!(
        base.as_str(),
        "cmd.exe"
            | "cmd"
            | "powershell.exe"
            | "powershell"
            | "pwsh.exe"
            | "pwsh"
            | "conhost.exe"
            | "conhost"
    ) {
        return false;
    }

    match driver {
        DriverKind::Codex => {
            matches!(base.as_str(), "node.exe" | "node")
                || (base.starts_with("codex") && (base.ends_with(".exe") || !base.contains('.')))
        }
        DriverKind::Claude => {
            matches!(base.as_str(), "node.exe" | "node")
                || (base.starts_with("claude") && (base.ends_with(".exe") || !base.contains('.')))
        }
        DriverKind::Grok => matches!(base.as_str(), "grok.exe" | "grok"),
        DriverKind::Prime => false,
        DriverKind::GenericTerminal => false,
    }
}

fn process_image_basename(image_name: &str) -> &str {
    Path::new(image_name)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(image_name)
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(image_name)
}

fn process_identities_for_ids(process_ids: &[u32]) -> Vec<ProcessIdentity> {
    process_ids
        .iter()
        .copied()
        .map(|process_id| ProcessIdentity {
            process_id,
            image_name: process_image_name(process_id),
        })
        .collect()
}

#[cfg(windows)]
fn process_image_name(process_id: u32) -> Option<String> {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{
            OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW,
        },
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
        if handle.is_null() {
            return None;
        }

        let mut buffer = vec![0_u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok =
            QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size);
        let _ = CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }

        Some(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

#[cfg(target_os = "linux")]
fn process_group_process_identities(process_id: u32) -> Vec<ProcessIdentity> {
    let Some(target_pgid) = linux_process_group_id(process_id) else {
        return process_identities_for_ids(&[process_id]);
    };

    let Ok(entries) = std::fs::read_dir("/proc") else {
        return process_identities_for_ids(&[process_id]);
    };

    let mut process_ids = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| linux_process_group_id(*pid) == Some(target_pgid))
        .collect::<Vec<_>>();
    process_ids.sort_unstable();
    process_identities_for_ids(&process_ids)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_group_process_identities(process_id: u32) -> Vec<ProcessIdentity> {
    process_identities_for_ids(&[process_id])
}

#[cfg(target_os = "linux")]
fn linux_process_group_id(process_id: u32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{process_id}/stat")).ok()?;
    let close_paren = stat.rfind(") ")?;
    let mut fields = stat[close_paren + 2..].split_whitespace();
    let _state = fields.next()?;
    let _ppid = fields.next()?;
    fields.next()?.parse::<i32>().ok()
}

#[cfg(unix)]
fn process_image_name(process_id: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        if let Ok(name) = std::fs::read_to_string(format!("/proc/{process_id}/comm")) {
            let trimmed = name.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
        if let Ok(path) = std::fs::read_link(format!("/proc/{process_id}/exe")) {
            return path.file_name()?.to_str().map(ToOwned::to_owned);
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode_utf8_chunks(chunks: &[&[u8]]) -> String {
        let mut decoder = Utf8StreamDecoder::default();
        let mut output = String::new();
        for chunk in chunks {
            if let Some(decoded) = decoder.push(chunk) {
                output.push_str(&decoded);
            }
        }
        if let Some(decoded) = decoder.finish() {
            output.push_str(&decoded);
        }
        output
    }

    #[test]
    fn utf8_output_decoder_preserves_controls_and_text_across_every_boundary() {
        let expected = "prefix\u{009b}?2004l λ 😀 漢字 \u{009d}title\u{009c}suffix";
        let bytes = expected.as_bytes();

        for split in 0..=bytes.len() {
            assert_eq!(
                decode_utf8_chunks(&[&bytes[..split], &bytes[split..]]),
                expected,
                "UTF-8 output changed at byte boundary {split}"
            );
        }

        let one_byte_chunks = bytes.iter().map(std::slice::from_ref).collect::<Vec<_>>();
        assert_eq!(decode_utf8_chunks(&one_byte_chunks), expected);
    }

    #[test]
    fn utf8_output_decoder_matches_stream_wide_lossy_decoding() {
        let bytes = b"valid\xf0\x9f\x92broken\xc2\x9bcontrol\xe2\x82";
        let expected = String::from_utf8_lossy(bytes);

        for split in 0..=bytes.len() {
            assert_eq!(
                decode_utf8_chunks(&[&bytes[..split], &bytes[split..]]),
                expected,
                "lossy UTF-8 output changed at byte boundary {split}"
            );
        }

        let one_byte_chunks = bytes.iter().map(std::slice::from_ref).collect::<Vec<_>>();
        assert_eq!(decode_utf8_chunks(&one_byte_chunks), expected);
    }
    use std::collections::VecDeque;

    enum WriteStep {
        Count(usize),
        Error(std::io::ErrorKind),
    }

    struct ScriptedWriter {
        steps: VecDeque<WriteStep>,
        flush_error: Option<std::io::ErrorKind>,
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            match self.steps.pop_front().expect("unexpected write call") {
                WriteStep::Count(count) => Ok(count),
                WriteStep::Error(kind) => Err(std::io::Error::from(kind)),
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            match self.flush_error.take() {
                Some(kind) => Err(std::io::Error::from(kind)),
                None => Ok(()),
            }
        }
    }

    #[test]
    fn write_progress_reports_partial_error_zero_write_flush_and_interrupt() {
        let mut partial = ScriptedWriter {
            steps: VecDeque::from([
                WriteStep::Count(2),
                WriteStep::Error(std::io::ErrorKind::BrokenPipe),
            ]),
            flush_error: None,
        };
        let error = write_pty_input(&mut partial, b"abcdef").unwrap_err();
        assert_eq!(error.bytes_written(), 2);
        assert_eq!(
            error.io_error().map(std::io::Error::kind),
            Some(std::io::ErrorKind::BrokenPipe)
        );

        let mut zero = ScriptedWriter {
            steps: VecDeque::from([WriteStep::Count(2), WriteStep::Count(0)]),
            flush_error: None,
        };
        let error = write_pty_input(&mut zero, b"abcdef").unwrap_err();
        assert_eq!(error.bytes_written(), 2);
        assert_eq!(
            error.io_error().map(std::io::Error::kind),
            Some(std::io::ErrorKind::WriteZero)
        );

        let mut flush = ScriptedWriter {
            steps: VecDeque::from([WriteStep::Count(6)]),
            flush_error: Some(std::io::ErrorKind::BrokenPipe),
        };
        let error = write_pty_input(&mut flush, b"abcdef").unwrap_err();
        assert_eq!(error.bytes_written(), 6);
        assert_eq!(
            error.io_error().map(std::io::Error::kind),
            Some(std::io::ErrorKind::BrokenPipe)
        );

        let mut interrupted = ScriptedWriter {
            steps: VecDeque::from([
                WriteStep::Error(std::io::ErrorKind::Interrupted),
                WriteStep::Count(6),
            ]),
            flush_error: None,
        };
        assert_eq!(write_pty_input(&mut interrupted, b"abcdef").unwrap(), 6);
    }

    #[test]
    fn launch_environment_scrubs_legacy_authority_before_current_values_are_applied() {
        let mut command = CommandBuilder::new("ignored");
        for key in INHERITED_CONTROL_ENV {
            command.env(key, "stale-parent-value");
        }
        let current = [
            EnvVar {
                key: "PRIM1_CONTROL_PLANE_ENDPOINT".into(),
                value: "current-endpoint".into(),
            },
            EnvVar {
                key: "PRIM1_CONTROL_PLANE_TRANSPORT".into(),
                value: "windows-named-pipe".into(),
            },
            EnvVar {
                key: "PRIM1_PANE_IDENTITY".into(),
                value: "current-pane".into(),
            },
            EnvVar {
                key: "PRIM1_CONTROL_PLANE_SERVER_PID".into(),
                value: "4242".into(),
            },
            EnvVar {
                key: "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME".into(),
                value: "987654321".into(),
            },
        ];

        apply_launch_environment(&mut command, &current).unwrap();

        assert_eq!(command.get_env(LEGACY_PANE_CREDENTIAL_ENV), None);
        assert_eq!(
            command.get_env("PRIM1_CONTROL_PLANE_ENDPOINT"),
            Some(std::ffi::OsStr::new("current-endpoint"))
        );
        assert_eq!(
            command.get_env("PRIM1_CONTROL_PLANE_TRANSPORT"),
            Some(std::ffi::OsStr::new("windows-named-pipe"))
        );
        assert_eq!(
            command.get_env("PRIM1_PANE_IDENTITY"),
            Some(std::ffi::OsStr::new("current-pane"))
        );
        assert_eq!(
            command.get_env("PRIM1_CONTROL_PLANE_SERVER_PID"),
            Some(std::ffi::OsStr::new("4242"))
        );
        assert_eq!(
            command.get_env("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME"),
            Some(std::ffi::OsStr::new("987654321"))
        );

        let error = apply_launch_environment(
            &mut command,
            &[EnvVar {
                key: "prim1_pane_credentials".into(),
                value: "forbidden".into(),
            }],
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "legacy pane credential environment is not permitted"
        );
    }

    struct MembershipUnsupportedPty;

    impl PtySession for MembershipUnsupportedPty {
        fn send_input(&self, _input: &str) -> PtyWriteResult {
            Ok(0)
        }

        fn resize(&self, _cols: u16, _rows: u16) -> anyhow::Result<()> {
            Ok(())
        }

        fn kill(&self) -> anyhow::Result<()> {
            Ok(())
        }

        fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>> {
            Ok(None)
        }

        fn process_id(&self) -> Option<u32> {
            None
        }
    }

    #[test]
    fn pty_membership_default_fails_closed_as_unsupported() {
        let error = MembershipUnsupportedPty
            .contains_process_id(42)
            .expect_err("default membership query must fail closed");
        assert_eq!(
            error.to_string(),
            "PTY session does not expose process-job membership"
        );
    }

    #[test]
    fn pty_input_write_cancellation_default_fails_closed_as_unsupported() {
        let error = MembershipUnsupportedPty
            .cancel_input_write()
            .expect_err("default input-write cancellation must fail closed");
        assert_eq!(
            error.to_string(),
            "PTY session does not support isolated input-write cancellation"
        );
    }

    #[cfg(windows)]
    #[test]
    fn pty_pinned_membership_default_fails_closed_as_unsupported() {
        let process = PinnedProcess::open(std::process::id()).expect("pin current test process");
        let error = MembershipUnsupportedPty
            .contains_process(&process)
            .expect_err("default pinned membership query must fail closed");
        assert_eq!(
            error.to_string(),
            "PTY session does not expose pinned-process job membership"
        );
    }

    #[test]
    fn expected_agent_image_name_identifies_agents_not_wrappers() {
        assert!(expected_agent_image_name(
            DriverKind::Codex,
            r"C:\Users\me\AppData\Roaming\npm\node.exe"
        ));
        assert!(expected_agent_image_name(
            DriverKind::Codex,
            r"C:\Program Files\Codex\codex.exe"
        ));
        assert!(expected_agent_image_name(
            DriverKind::Claude,
            r"C:\Program Files\Claude\claude.exe"
        ));
        assert!(expected_agent_image_name(
            DriverKind::Claude,
            r"C:\Program Files\Claude\claude-code.exe"
        ));
        assert!(expected_agent_image_name(
            DriverKind::Grok,
            r"C:\Users\me\.grok\bin\grok.exe"
        ));
        assert!(!expected_agent_image_name(
            DriverKind::Codex,
            r"C:\Windows\System32\cmd.exe"
        ));
        assert!(!expected_agent_image_name(
            DriverKind::Claude,
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!expected_agent_image_name(
            DriverKind::GenericTerminal,
            "node.exe"
        ));
        assert!(!expected_agent_image_name(
            DriverKind::Grok,
            r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"
        ));
        assert!(!expected_agent_image_name(
            DriverKind::Grok,
            r"C:\Users\me\.grok\bin\grok-helper.exe"
        ));
    }
}

#[cfg(all(test, windows))]
mod windows_real_process_tests {
    use super::*;
    use std::{
        collections::HashSet,
        fs,
        io::{Read, Write},
        os::windows::io::AsRawHandle,
        path::PathBuf,
        process::{Command, Stdio},
        sync::atomic::AtomicBool,
        sync::{Arc, Condvar, Mutex, mpsc},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };
    use windows_sys::Win32::Foundation::ERROR_OPERATION_ABORTED;

    const CREATE_TIME_JOB_ATTEMPTS: usize = 24;
    const INPUT_WRITE_CANCEL_HELPER_ENV: &str = "PRIM1_INPUT_WRITE_CANCEL_HELPER";
    const INPUT_WRITE_CANCEL_RELEASE_ENV: &str = "PRIM1_INPUT_WRITE_CANCEL_RELEASE";

    #[test]
    fn missing_working_directory_fails_instead_of_falling_back_to_user_profile() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_nanos();
        let missing = std::env::temp_dir().join(format!(
            "prim1-missing-working-directory-{}-{nonce}",
            std::process::id()
        ));
        assert!(!missing.exists(), "missing cwd fixture unexpectedly exists");

        let spec = LaunchSpec {
            program: "cmd.exe".to_owned(),
            args: vec!["/d".to_owned(), "/c".to_owned(), "exit 0".to_owned()],
            working_dir: missing.to_string_lossy().into_owned(),
            env: Vec::new(),
            display_name: "missing-working-directory".to_owned(),
        };

        let error = ConcretePtySession::spawn(&spec, Arc::new(|_| {}))
            .err()
            .expect("spawn with a missing cwd must fail closed");
        let error_text = format!("{error:#}");
        assert!(
            error_text.contains("failed to spawn cmd.exe in per-session process job"),
            "unexpected missing-cwd error: {error:#}"
        );
    }

    #[test]
    fn inherited_legacy_control_environment_is_absent_from_real_child() {
        let status = Command::new(std::env::current_exe().expect("locate pty-host test binary"))
            .args([
                "--exact",
                "windows_real_process_tests::legacy_control_environment_scrub_child_helper",
                "--nocapture",
            ])
            .env("PRIM1_ENV_SCRUB_HELPER", "1")
            .env("PRIM1_PANE_CREDENTIALS", "stale-credential")
            .env("PRIM1_CONTROL_PLANE_ENDPOINT", "stale-endpoint")
            .env("PRIM1_CONTROL_PLANE_TRANSPORT", "stale-transport")
            .env("PRIM1_PANE_IDENTITY", "stale-pane")
            .env("PRIM1_CONTROL_PLANE_SERVER_PID", "111")
            .env("PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME", "222")
            .status()
            .expect("run isolated environment scrub helper");
        assert!(status.success(), "isolated environment scrub helper failed");
    }

    #[test]
    fn legacy_control_environment_scrub_child_helper() {
        if std::env::var("PRIM1_ENV_SCRUB_HELPER").as_deref() != Ok("1") {
            return;
        }

        let batch = TempBatch::new(
            "@echo off\r\n\
             echo CREDENTIAL=[%PRIM1_PANE_CREDENTIALS%]\r\n\
             echo ENDPOINT=[%PRIM1_CONTROL_PLANE_ENDPOINT%]\r\n\
             echo TRANSPORT=[%PRIM1_CONTROL_PLANE_TRANSPORT%]\r\n\
             echo IDENTITY=[%PRIM1_PANE_IDENTITY%]\r\n\
             echo SERVER_PID=[%PRIM1_CONTROL_PLANE_SERVER_PID%]\r\n\
             echo SERVER_STARTED=[%PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME%]\r\n",
        );
        let spec = LaunchSpec {
            program: "cmd.exe".to_owned(),
            args: vec!["/d".to_owned(), "/c".to_owned(), batch.path_string()],
            working_dir: batch.working_dir_string(),
            env: vec![
                EnvVar {
                    key: "PRIM1_CONTROL_PLANE_ENDPOINT".into(),
                    value: "current-endpoint".into(),
                },
                EnvVar {
                    key: "PRIM1_CONTROL_PLANE_TRANSPORT".into(),
                    value: "current-transport".into(),
                },
                EnvVar {
                    key: "PRIM1_PANE_IDENTITY".into(),
                    value: "current-pane".into(),
                },
                EnvVar {
                    key: "PRIM1_CONTROL_PLANE_SERVER_PID".into(),
                    value: "4242".into(),
                },
                EnvVar {
                    key: "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME".into(),
                    value: "987654321".into(),
                },
            ],
            display_name: "environment-scrub-child".into(),
        };
        let output = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&output);
        let session = ConcretePtySession::spawn(
            &spec,
            Arc::new(move |event| {
                if let PtyEvent::Output(chunk) = event {
                    captured.lock().expect("capture output").push_str(&chunk);
                }
            }),
        )
        .expect("spawn real environment probe child");

        poll_until(Duration::from_secs(2), || {
            output
                .lock()
                .ok()
                .and_then(|output| output.contains("\u{1b}[6n").then_some(()))
        })
        .expect("ConPTY cursor query was not observed");
        session
            .send_input("\u{1b}[1;1R")
            .expect("release ConPTY cursor handshake");
        let observed = poll_until(Duration::from_secs(4), || {
            let output = output.lock().ok()?.clone();
            output
                .contains("SERVER_STARTED=[987654321]")
                .then_some(output)
        })
        .expect("environment probe output was not observed");

        assert!(observed.contains("CREDENTIAL=[]"), "{observed:?}");
        assert!(
            observed.contains("ENDPOINT=[current-endpoint]"),
            "{observed:?}"
        );
        assert!(
            observed.contains("TRANSPORT=[current-transport]"),
            "{observed:?}"
        );
        assert!(!observed.contains("stale-credential"), "{observed:?}");
        assert!(!observed.contains("stale-endpoint"), "{observed:?}");
        assert!(!observed.contains("stale-transport"), "{observed:?}");
        assert!(!observed.contains("stale-pane"), "{observed:?}");
        assert!(observed.contains("SERVER_PID=[4242]"), "{observed:?}");
        assert!(
            observed.contains("SERVER_STARTED=[987654321]"),
            "{observed:?}"
        );
        assert!(!observed.contains("SERVER_PID=[111]"), "{observed:?}");
        assert!(!observed.contains("SERVER_STARTED=[222]"), "{observed:?}");
        session.kill().expect("terminate environment probe child");
    }

    #[test]
    fn production_spawn_contains_immediate_descendants_at_process_creation() {
        let batch = TempBatch::new(
            "@echo off\r\n\
             ping.exe -n 30 127.0.0.1 >nul\r\n",
        );

        for attempt in 0..CREATE_TIME_JOB_ATTEMPTS {
            let spec = LaunchSpec {
                program: "cmd.exe".to_owned(),
                args: vec!["/d".to_owned(), "/c".to_owned(), batch.path_string()],
                working_dir: batch.working_dir_string(),
                env: Vec::new(),
                display_name: format!("create-time-job-attempt-{attempt}"),
            };
            let output = Arc::new(Mutex::new(String::new()));
            let captured_output = Arc::clone(&output);
            let session = ConcretePtySession::spawn(
                &spec,
                Arc::new(move |event| {
                    let mut output = captured_output
                        .lock()
                        .expect("capture output mutex poisoned");
                    match event {
                        PtyEvent::Output(chunk) => output.push_str(&chunk),
                        PtyEvent::Closed => output.push_str("<PTY closed>"),
                        PtyEvent::Error(error) => {
                            output.push_str("<PTY error: ");
                            output.push_str(&error);
                            output.push('>');
                        }
                    }
                }),
            )
            .unwrap_or_else(|error| {
                panic!("attempt {attempt}: production spawn failed: {error:#}")
            });
            let root_pid = session
                .process_id()
                .unwrap_or_else(|| panic!("attempt {attempt}: spawned PTY child had no PID"));

            poll_until(Duration::from_secs(2), || {
                output
                    .lock()
                    .ok()
                    .and_then(|output| output.contains("\u{1b}[6n").then_some(()))
            })
            .unwrap_or_else(|| panic!("attempt {attempt}: ConPTY cursor query was not observed"));
            session.send_input("\u{1b}[1;1R").unwrap_or_else(|error| {
                panic!("attempt {attempt}: cursor reply failed: {error:#}")
            });

            let process_ids = poll_until(Duration::from_secs(4), || {
                let process_ids = session.job.live_process_ids().ok()?;
                (process_ids.contains(&root_pid) && process_ids.iter().any(|pid| *pid != root_pid))
                    .then_some(process_ids)
            })
            .unwrap_or_else(|| {
                let captured_output = output
                    .lock()
                    .expect("capture output mutex poisoned")
                    .clone();
                panic!(
                    "attempt {attempt}: immediate descendant escaped or never appeared; job PIDs: {:?}; PTY output: {:?}",
                    session.job.live_process_ids(),
                    captured_output
                )
            });

            let descendant_pid = process_ids
                .iter()
                .copied()
                .find(|pid| *pid != root_pid)
                .expect("descendant established by poll predicate");
            let pinned_root = PinnedProcess::open(root_pid).unwrap_or_else(|error| {
                panic!("attempt {attempt}: failed to pin root {root_pid}: {error:#}")
            });
            let pinned_descendant = PinnedProcess::open(descendant_pid).unwrap_or_else(|error| {
                panic!("attempt {attempt}: failed to pin descendant {descendant_pid}: {error:#}")
            });
            let pinned_processes = process_ids
                .iter()
                .map(|pid| {
                    PinnedProcess::open(*pid).unwrap_or_else(|error| {
                        panic!("attempt {attempt}: failed to pin job process {pid}: {error:#}")
                    })
                })
                .collect::<Vec<_>>();

            assert!(
                session
                    .job
                    .contains_process(&pinned_root)
                    .expect("query pinned root membership"),
                "attempt {attempt}: root {root_pid} was not in its session job"
            );
            assert!(
                session
                    .job
                    .contains_process(&pinned_descendant)
                    .expect("query pinned descendant membership"),
                "attempt {attempt}: descendant {descendant_pid} was not in its session job"
            );
            assert_ne!(
                root_pid, descendant_pid,
                "attempt {attempt}: root and descendant must be distinct"
            );

            session.job.terminate().unwrap_or_else(|error| {
                panic!("attempt {attempt}: terminate job failed: {error:#}")
            });
            poll_until(Duration::from_secs(4), || {
                let job_empty = session
                    .job
                    .live_process_ids()
                    .map(|ids| ids.is_empty())
                    .unwrap_or(false);
                let all_exited = pinned_processes
                    .iter()
                    .all(|process| matches!(process.is_alive(), Ok(false)));
                (job_empty && all_exited).then_some(())
            })
            .unwrap_or_else(|| {
                panic!(
                    "attempt {attempt}: terminating the session job did not reap root {root_pid} and descendant {descendant_pid}; remaining job PIDs: {:?}",
                    session.job.live_process_ids()
                )
            });
        }
    }

    #[test]
    fn kill_closes_master_without_waiting_for_blocked_writer_mutex() {
        let gate = Arc::new((Mutex::new(WriteGate::default()), Condvar::new()));
        let child = Command::new("cmd.exe")
            .args(["/d", "/c", "ping -n 30 127.0.0.1 >nul"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child for blocked writer test");
        let process_id = child.id();
        let session = Arc::new(ConcretePtySession {
            master: Mutex::new(Some(Box::new(InterruptingMaster {
                gate: Arc::clone(&gate),
            }))),
            writer: Arc::new(Mutex::new(Box::new(BlockingWriter {
                gate: Arc::clone(&gate),
            }))),
            writer_handle: 0,
            child: Arc::new(Mutex::new(Box::new(child))),
            process_id: Some(process_id),
            agent_seen: Arc::new(AtomicBool::new(false)),
            agent_pids_seen: Arc::new(Mutex::new(HashSet::new())),
            job: ProcessJob::new().expect("create empty process job"),
        });

        let (result_tx, result_rx) = mpsc::channel();
        let writer_session = Arc::clone(&session);
        let writer_thread = std::thread::spawn(move || {
            result_tx
                .send(writer_session.send_input("input that remains blocked"))
                .expect("publish blocked write result");
        });
        wait_for_blocked_writer(&gate, Duration::from_secs(2));

        session.kill().expect("kill session with blocked writer");
        let write_result = result_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("blocked PTY write did not return after master close");
        assert!(
            write_result.is_err(),
            "interrupted PTY write should fail instead of reporting delivery"
        );
        writer_thread.join().expect("join blocked writer thread");
    }

    #[test]
    fn kill_cancels_a_blocked_real_conpty_write_and_reaps_the_job() {
        let output = Arc::new(Mutex::new(String::new()));
        let captured_output = Arc::clone(&output);
        let spec = LaunchSpec {
            program: "cmd.exe".to_owned(),
            args: vec![
                "/d".to_owned(),
                "/c".to_owned(),
                "ping -n 30 127.0.0.1 >nul".to_owned(),
            ],
            working_dir: std::env::temp_dir().to_string_lossy().into_owned(),
            env: Vec::new(),
            display_name: "blocked-real-conpty-write".to_owned(),
        };
        let session = Arc::new(
            ConcretePtySession::spawn(
                &spec,
                Arc::new(move |event| {
                    if let PtyEvent::Output(chunk) = event {
                        captured_output
                            .lock()
                            .expect("capture output mutex poisoned")
                            .push_str(&chunk);
                    }
                }),
            )
            .expect("spawn real ConPTY session"),
        );

        poll_until(Duration::from_secs(2), || {
            output
                .lock()
                .ok()
                .and_then(|output| output.contains("\u{1b}[6n").then_some(()))
        })
        .expect("ConPTY cursor query was not observed");
        session
            .send_input("\u{1b}[1;1R")
            .expect("release ConPTY cursor handshake");
        poll_until(Duration::from_secs(4), || {
            session
                .job
                .live_process_ids()
                .ok()
                .and_then(|process_ids| (process_ids.len() >= 2).then_some(()))
        })
        .expect("real ConPTY target did not start its child process");

        let payload = "x".repeat(64 * 1024 * 1024);
        let (started_tx, started_rx) = mpsc::channel();
        let (write_tx, write_rx) = mpsc::channel();
        let writer_session = Arc::clone(&session);
        let writer_thread = std::thread::spawn(move || {
            started_tx.send(()).expect("publish writer start");
            write_tx
                .send(writer_session.send_input(&payload))
                .expect("publish real ConPTY write result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("real ConPTY writer thread did not start");
        assert!(
            matches!(
                write_rx.recv_timeout(Duration::from_millis(250)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "64 MiB write unexpectedly completed before cancellation"
        );

        let (kill_tx, kill_rx) = mpsc::channel();
        let kill_session = Arc::clone(&session);
        let kill_thread = std::thread::spawn(move || {
            kill_tx
                .send(kill_session.kill())
                .expect("publish real ConPTY kill result");
        });
        let kill_result = kill_rx
            .recv_timeout(Duration::from_secs(4))
            .expect("kill blocked while a real ConPTY writer held the writer mutex");
        assert!(
            kill_result.is_ok(),
            "real ConPTY kill failed: {kill_result:#?}"
        );

        let write_result = write_rx
            .recv_timeout(Duration::from_secs(4))
            .expect("CancelIoEx did not release the blocked real ConPTY write");
        assert!(
            write_result.is_err(),
            "cancelled real ConPTY write must not report successful delivery"
        );
        poll_until(Duration::from_secs(4), || {
            session
                .job
                .live_process_ids()
                .ok()
                .and_then(|process_ids| process_ids.is_empty().then_some(()))
        })
        .expect("session job retained processes after kill");

        kill_thread.join().expect("join real ConPTY kill thread");
        writer_thread
            .join()
            .expect("join real ConPTY writer thread");
    }

    #[test]
    fn cancel_input_write_keeps_the_real_conpty_session_alive_and_reusable() {
        let drain_control = TempInputDrainControl::new();
        let output = Arc::new(Mutex::new(String::new()));
        let captured_output = Arc::clone(&output);
        let spec = LaunchSpec {
            program: std::env::current_exe()
                .expect("locate pty-host test binary")
                .to_string_lossy()
                .into_owned(),
            args: vec![
                "--exact".to_owned(),
                "windows_real_process_tests::input_write_cancel_child_helper".to_owned(),
                "--nocapture".to_owned(),
            ],
            working_dir: drain_control.working_dir_string(),
            env: vec![
                EnvVar {
                    key: INPUT_WRITE_CANCEL_HELPER_ENV.to_owned(),
                    value: "1".to_owned(),
                },
                EnvVar {
                    key: INPUT_WRITE_CANCEL_RELEASE_ENV.to_owned(),
                    value: drain_control.release_path_string(),
                },
            ],
            display_name: "cancel-real-conpty-input-write".to_owned(),
        };
        let session = Arc::new(
            ConcretePtySession::spawn(
                &spec,
                Arc::new(move |event| {
                    if let PtyEvent::Output(chunk) = event {
                        captured_output
                            .lock()
                            .expect("capture output mutex poisoned")
                            .push_str(&chunk);
                    }
                }),
            )
            .expect("spawn real ConPTY input-drain helper"),
        );

        poll_until(Duration::from_secs(2), || {
            output
                .lock()
                .ok()
                .and_then(|output| output.contains("\u{1b}[6n").then_some(()))
        })
        .expect("ConPTY cursor query was not observed");
        session
            .send_input("\u{1b}[1;1R")
            .expect("release ConPTY cursor handshake");

        let root_pid = session.process_id().expect("real ConPTY root process id");
        poll_until(Duration::from_secs(4), || {
            session
                .job
                .live_process_ids()
                .ok()
                .and_then(|process_ids| process_ids.contains(&root_pid).then_some(()))
        })
        .expect("real ConPTY helper did not remain in its process job");

        let payload = "x".repeat(64 * 1024 * 1024);
        let (started_tx, started_rx) = mpsc::channel();
        let (write_tx, write_rx) = mpsc::channel();
        let writer_session = Arc::clone(&session);
        let writer_thread = std::thread::spawn(move || {
            started_tx.send(()).expect("publish writer start");
            write_tx
                .send(writer_session.send_input(&payload))
                .expect("publish cancelled ConPTY write result");
        });
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("real ConPTY writer thread did not start");
        assert!(
            matches!(
                write_rx.recv_timeout(Duration::from_millis(250)),
                Err(mpsc::RecvTimeoutError::Timeout)
            ),
            "64 MiB write unexpectedly completed before cancellation"
        );

        if let Err(error) = session.cancel_input_write() {
            let _ = session.kill();
            panic!("isolated ConPTY input-write cancellation failed: {error:#}");
        }
        let write_result = match write_rx.recv_timeout(Duration::from_secs(4)) {
            Ok(result) => result,
            Err(error) => {
                let _ = session.kill();
                panic!("CancelIoEx did not release the blocked real ConPTY write: {error}");
            }
        };
        let write_error = match write_result {
            Ok(bytes_written) => {
                let _ = session.kill();
                panic!(
                    "cancelled real ConPTY write incorrectly reported {bytes_written} delivered bytes"
                );
            }
            Err(error) => error,
        };
        assert_eq!(
            write_error
                .io_error()
                .and_then(std::io::Error::raw_os_error),
            Some(ERROR_OPERATION_ABORTED as i32),
            "blocked WriteFile must return ERROR_OPERATION_ABORTED after CancelIoEx: {write_error:#}"
        );
        assert!(
            session
                .try_wait()
                .expect("poll helper after write cancellation")
                .is_none(),
            "input-write cancellation must not terminate the PTY child"
        );
        assert!(
            session
                .job
                .live_process_ids()
                .expect("query job after write cancellation")
                .contains(&root_pid),
            "input-write cancellation must not terminate the PTY job"
        );
        assert!(
            session
                .master
                .lock()
                .expect("pty master poisoned")
                .is_some(),
            "input-write cancellation must not close the PTY master"
        );

        drain_control.release();
        let probe = "writer-reuse-probe\r\n".to_owned();
        let expected_probe_bytes = probe.len();
        let (probe_tx, probe_rx) = mpsc::channel();
        let probe_session = Arc::clone(&session);
        let probe_thread = std::thread::spawn(move || {
            probe_tx
                .send(probe_session.send_input(&probe))
                .expect("publish reusable-writer probe result");
        });
        let probe_result = match probe_rx.recv_timeout(Duration::from_secs(6)) {
            Ok(result) => result,
            Err(error) => {
                let _ = session.kill();
                panic!("PTY writer was not reusable after cancellation: {error}");
            }
        };
        assert_eq!(
            probe_result.expect("small input failed after isolated write cancellation"),
            expected_probe_bytes
        );
        assert!(
            session
                .try_wait()
                .expect("poll helper after reusable-writer probe")
                .is_none(),
            "helper must remain alive after the reusable-writer probe"
        );

        session
            .kill()
            .expect("explicitly terminate reusable ConPTY session");
        poll_until(Duration::from_secs(4), || {
            session
                .job
                .live_process_ids()
                .ok()
                .and_then(|process_ids| process_ids.is_empty().then_some(()))
        })
        .expect("session job retained processes after explicit cleanup");

        probe_thread.join().expect("join reusable-writer thread");
        writer_thread
            .join()
            .expect("join cancelled ConPTY writer thread");
    }

    #[test]
    fn input_write_cancel_child_helper() {
        if std::env::var(INPUT_WRITE_CANCEL_HELPER_ENV).as_deref() != Ok("1") {
            return;
        }

        let release_path = PathBuf::from(
            std::env::var(INPUT_WRITE_CANCEL_RELEASE_ENV).expect("input-drain helper release path"),
        );
        poll_until(Duration::from_secs(30), || {
            release_path.exists().then_some(())
        })
        .expect("parent did not release input-drain helper");
        set_raw_console_input_for_helper();

        let mut input = std::io::stdin().lock();
        let mut buffer = [0_u8; 8192];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => break,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("input-drain helper failed to read ConPTY input: {error}"),
            }
        }
    }

    #[test]
    fn agent_alive_reports_exited_when_only_cmd_wrapper_survives() {
        let batch = TempBatch::new(
            "@echo off\r\n\
             set /p PRIM1_START=\r\n\
             node -e \"setInterval(() => {}, 1000)\"\r\n\
             set /p PRIM1_HOLD=\r\n",
        );
        let job = ProcessJob::new().expect("create process job");
        let mut child = Command::new("cmd.exe")
            .args(["/d", "/c"])
            .arg(batch.path_string())
            .current_dir(batch.working_dir_string())
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cmd wrapper");
        let mut child_stdin = child.stdin.take().expect("capture cmd stdin");

        job.assign_raw(AsRawHandle::as_raw_handle(&child))
            .expect("assign cmd wrapper to job");
        child_stdin
            .write_all(b"start\r\n")
            .expect("release cmd wrapper startup gate");

        let session = concrete_session_for_job_child(job, child);

        let (agent_processes, live_processes) = poll_until(Duration::from_secs(8), || {
            let live_processes = session
                .live_process_identities()
                .expect("query live job process identities");
            if !live_processes
                .iter()
                .any(|process| process_basename_is(process, &["cmd.exe", "cmd"]))
            {
                return None;
            }

            match session
                .agent_alive(DriverKind::Codex)
                .expect("query agent liveness")
            {
                AgentLiveness::Alive(agent_processes) => Some((agent_processes, live_processes)),
                AgentLiveness::Exited { .. } | AgentLiveness::NotYetObserved(_) => None,
            }
        })
        .unwrap_or_else(|| panic!("cmd.exe + node.exe did not become live in the job"));

        assert!(
            live_processes
                .iter()
                .any(|process| process_basename_is(process, &["cmd.exe", "cmd"])),
            "cmd.exe wrapper should be alive with node.exe: {live_processes:?}"
        );
        let node_pid = agent_processes
            .iter()
            .find(|process| process_basename_is(process, &["node.exe", "node"]))
            .map(|process| process.process_id)
            .unwrap_or_else(|| panic!("agent_alive did not report node.exe: {agent_processes:?}"));

        terminate_process_id(node_pid).expect("terminate only the inner node.exe process");

        let mut last_liveness = String::new();
        let (last_agent_pids, wrapper_only_processes) = poll_until(Duration::from_secs(8), || {
            let liveness = session
                .agent_alive(DriverKind::Codex)
                .expect("query agent liveness after node.exe termination");
            last_liveness = format!("{liveness:?}");
            match liveness {
                AgentLiveness::Exited {
                    last_agent_pids,
                    live_processes,
                } if live_processes
                    .iter()
                    .any(|process| process_basename_is(process, &["cmd.exe", "cmd"]))
                    && live_processes
                        .iter()
                        .all(|process| !is_expected_agent_process(DriverKind::Codex, process)) =>
                {
                    Some((last_agent_pids, live_processes))
                }
                AgentLiveness::Alive(_)
                | AgentLiveness::NotYetObserved(_)
                | AgentLiveness::Exited { .. } => None,
            }
        })
        .unwrap_or_else(|| {
            panic!(
                "agent_alive did not report Exited with only non-agent survivors; last liveness: {last_liveness}"
            );
        });

        assert!(
            last_agent_pids.contains(&node_pid),
            "last agent PID cache should include killed node.exe PID {node_pid}: {last_agent_pids:?}"
        );
        assert!(
            wrapper_only_processes
                .iter()
                .any(|process| process_basename_is(process, &["cmd.exe", "cmd"])),
            "cmd.exe wrapper should still be alive after node.exe exits: {wrapper_only_processes:?}"
        );
        assert!(
            wrapper_only_processes
                .iter()
                .all(|process| !is_expected_agent_process(DriverKind::Codex, process)),
            "wrapper-only survivors must not read as Codex agents: {wrapper_only_processes:?}"
        );

        let _ = session.kill();
        drop(child_stdin);
    }

    struct TempBatch {
        root: PathBuf,
        path: PathBuf,
    }

    impl TempBatch {
        fn new(contents: &str) -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir()
                .join(format!("prim1-agent-alive-{}-{unique}", std::process::id()));
            fs::create_dir_all(&root).expect("create temporary batch directory");
            let path = root.join("agent-wrapper.cmd");
            fs::write(&path, contents).expect("write temporary batch file");
            Self { root, path }
        }

        fn path_string(&self) -> String {
            self.path.to_string_lossy().into_owned()
        }

        fn working_dir_string(&self) -> String {
            self.root.to_string_lossy().into_owned()
        }
    }

    impl Drop for TempBatch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct TempInputDrainControl {
        root: PathBuf,
        release_path: PathBuf,
    }

    impl TempInputDrainControl {
        fn new() -> Self {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "prim1-input-write-cancel-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&root).expect("create input-drain helper directory");
            let release_path = root.join("release-input-drain.flag");
            Self { root, release_path }
        }

        fn working_dir_string(&self) -> String {
            self.root.to_string_lossy().into_owned()
        }

        fn release_path_string(&self) -> String {
            self.release_path.to_string_lossy().into_owned()
        }

        fn release(&self) {
            fs::write(&self.release_path, b"release")
                .expect("release real ConPTY input-drain helper");
        }
    }

    impl Drop for TempInputDrainControl {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn set_raw_console_input_for_helper() {
        use windows_sys::Win32::System::Console::{
            ENABLE_ECHO_INPUT, ENABLE_LINE_INPUT, ENABLE_PROCESSED_INPUT, GetConsoleMode,
            GetStdHandle, STD_INPUT_HANDLE, SetConsoleMode,
        };

        let input_handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        let mut mode = 0;
        if unsafe { GetConsoleMode(input_handle, &mut mode) } == 0 {
            panic!(
                "GetConsoleMode failed for input-drain helper: {}",
                std::io::Error::last_os_error()
            );
        }
        let raw_mode = mode & !(ENABLE_ECHO_INPUT | ENABLE_LINE_INPUT | ENABLE_PROCESSED_INPUT);
        if unsafe { SetConsoleMode(input_handle, raw_mode) } == 0 {
            panic!(
                "SetConsoleMode failed for input-drain helper: {}",
                std::io::Error::last_os_error()
            );
        }
    }

    fn poll_until<T>(timeout: Duration, mut check: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(value) = check() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    fn process_basename_is(process: &ProcessIdentity, expected_names: &[&str]) -> bool {
        process
            .image_name
            .as_deref()
            .map(|name| {
                let base = process_image_basename(name).to_ascii_lowercase();
                expected_names.iter().any(|expected| base == *expected)
            })
            .unwrap_or(false)
    }

    fn is_expected_agent_process(driver: DriverKind, process: &ProcessIdentity) -> bool {
        process
            .image_name
            .as_deref()
            .map(|name| expected_agent_image_name(driver, name))
            .unwrap_or(false)
    }

    fn terminate_process_id(process_id: u32) -> Result<()> {
        use windows_sys::Win32::{
            Foundation::CloseHandle,
            System::Threading::{OpenProcess, PROCESS_TERMINATE, TerminateProcess},
        };

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, process_id);
            if handle.is_null() {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!("OpenProcess(PROCESS_TERMINATE) failed for {process_id}")
                });
            }

            let ok = TerminateProcess(handle, 1);
            let _ = CloseHandle(handle);
            if ok == 0 {
                return Err(std::io::Error::last_os_error())
                    .with_context(|| format!("TerminateProcess failed for {process_id}"));
            }
        }

        Ok(())
    }

    fn concrete_session_for_job_child(
        job: ProcessJob,
        child: std::process::Child,
    ) -> ConcretePtySession {
        let process_id = child.id();
        ConcretePtySession {
            master: Mutex::new(Some(Box::new(NullMasterPty))),
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            writer_handle: 0,
            child: Arc::new(Mutex::new(Box::new(child))),
            process_id: Some(process_id),
            agent_seen: Arc::new(AtomicBool::new(false)),
            agent_pids_seen: Arc::new(Mutex::new(HashSet::new())),
            job,
        }
    }

    struct NullMasterPty;

    #[derive(Default)]
    struct WriteGate {
        entered: bool,
        interrupted: bool,
    }

    struct BlockingWriter {
        gate: Arc<(Mutex<WriteGate>, Condvar)>,
    }

    impl std::io::Write for BlockingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            let (lock, ready) = &*self.gate;
            let mut state = lock.lock().expect("write gate poisoned");
            state.entered = true;
            ready.notify_all();
            while !state.interrupted {
                state = ready.wait(state).expect("write gate poisoned");
            }
            Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY master closed",
            ))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    struct InterruptingMaster {
        gate: Arc<(Mutex<WriteGate>, Condvar)>,
    }

    impl Drop for InterruptingMaster {
        fn drop(&mut self) {
            let (lock, ready) = &*self.gate;
            let mut state = lock.lock().expect("write gate poisoned");
            state.interrupted = true;
            ready.notify_all();
        }
    }

    impl MasterPty for InterruptingMaster {
        fn resize(&self, _size: PtySize) -> std::result::Result<(), anyhow::Error> {
            Ok(())
        }

        fn get_size(&self) -> std::result::Result<PtySize, anyhow::Error> {
            Ok(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
        }

        fn try_clone_reader(
            &self,
        ) -> std::result::Result<Box<dyn std::io::Read + Send>, anyhow::Error> {
            Ok(Box::new(std::io::empty()))
        }

        fn take_writer(
            &self,
        ) -> std::result::Result<Box<dyn std::io::Write + Send>, anyhow::Error> {
            Ok(Box::new(std::io::sink()))
        }
    }

    fn wait_for_blocked_writer(gate: &Arc<(Mutex<WriteGate>, Condvar)>, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let (lock, ready) = &**gate;
        let mut state = lock.lock().expect("write gate poisoned");
        while !state.entered {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .expect("writer did not enter blocking write before timeout");
            let (next_state, wait_result) = ready
                .wait_timeout(state, remaining)
                .expect("write gate poisoned");
            state = next_state;
            assert!(
                !wait_result.timed_out() || state.entered,
                "writer did not enter blocking write before timeout"
            );
        }
    }

    impl MasterPty for NullMasterPty {
        fn resize(&self, _size: PtySize) -> std::result::Result<(), anyhow::Error> {
            Ok(())
        }

        fn get_size(&self) -> std::result::Result<PtySize, anyhow::Error> {
            Ok(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
        }

        fn try_clone_reader(
            &self,
        ) -> std::result::Result<Box<dyn std::io::Read + Send>, anyhow::Error> {
            Ok(Box::new(std::io::empty()))
        }

        fn take_writer(
            &self,
        ) -> std::result::Result<Box<dyn std::io::Write + Send>, anyhow::Error> {
            Ok(Box::new(std::io::sink()))
        }
    }
}
