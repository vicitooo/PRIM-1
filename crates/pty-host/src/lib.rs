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
use shared_types::{DriverKind, LaunchSpec};

mod job;
pub use job::ProcessJob;

#[derive(Debug, Clone)]
pub enum PtyEvent {
    Output(String),
    Closed,
    Error(String),
}

pub type PtyEventHandler = Arc<dyn Fn(PtyEvent) + Send + Sync>;

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

pub trait PtySession: Send {
    fn send_input(&self, input: &str) -> anyhow::Result<usize>;
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;
    fn kill(&self) -> anyhow::Result<()>;
    fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>>;
    fn process_id(&self) -> Option<u32>;
    fn note_real_output(&self, _driver: DriverKind) {}
    fn agent_alive(&self, _driver: DriverKind) -> anyhow::Result<AgentLiveness> {
        Ok(AgentLiveness::NotYetObserved(Vec::new()))
    }
}

pub struct ConcretePtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_id: Option<u32>,
    agent_seen: Arc<AtomicBool>,
    agent_pids_seen: Arc<Mutex<HashSet<u32>>>,
    #[cfg(windows)]
    job: ProcessJob,
}

impl ConcretePtySession {
    pub fn spawn(spec: &LaunchSpec, handler: PtyEventHandler) -> anyhow::Result<Self> {
        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: 40,
                cols: 120,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let mut command = CommandBuilder::new(&spec.program);
        command.args(&spec.args);
        command.cwd(&spec.working_dir);

        for env_var in &spec.env {
            command.env(&env_var.key, &env_var.value);
        }

        let mut child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to spawn {}", spec.program))?;
        drop(pair.slave);
        let process_id = child.process_id();

        #[cfg(windows)]
        let job = {
            let job = match ProcessJob::new().context("failed to create per-session process job") {
                Ok(job) => job,
                Err(error) => {
                    let _ = child.kill();
                    return Err(error);
                }
            };
            let raw_handle = match child.as_raw_handle() {
                Some(handle) => handle,
                None => {
                    let _ = child.kill();
                    return Err(anyhow!(
                        "PTY child did not expose a Windows process handle for job assignment"
                    ));
                }
            };
            if let Err(error) = job.assign_raw(raw_handle) {
                let _ = child.kill();
                return Err(error.context(
                    "pty spawn: job assignment failed; killed child to prevent ghost process tree",
                ));
            }
            job
        };

        let reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;
        let writer = pair
            .master
            .take_writer()
            .context("failed to take PTY writer")?;
        let child = Arc::new(Mutex::new(child));

        let reader_handler = Arc::clone(&handler);
        thread::spawn(move || {
            let mut reader = reader;
            let mut buffer = [0_u8; 4096];

            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        reader_handler(PtyEvent::Closed);
                        break;
                    }
                    Ok(size) => {
                        reader_handler(PtyEvent::Output(
                            String::from_utf8_lossy(&buffer[..size]).into_owned(),
                        ));
                    }
                    Err(error) => {
                        reader_handler(PtyEvent::Error(error.to_string()));
                        break;
                    }
                }
            }
        });

        Ok(Self {
            master: pair.master,
            writer: Arc::new(Mutex::new(writer)),
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

    fn live_process_identities(&self) -> Result<Vec<ProcessIdentity>> {
        #[cfg(windows)]
        {
            return self
                .job
                .live_process_ids()
                .map(|process_ids| process_identities_for_ids(&process_ids));
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
    fn send_input(&self, input: &str) -> anyhow::Result<usize> {
        let mut writer = self.writer.lock().expect("pty writer poisoned");
        writer
            .write_all(input.as_bytes())
            .context("failed to write PTY input")?;
        writer.flush().context("failed to flush PTY input")?;
        Ok(input.as_bytes().len())
    }

    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
        self.master
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
    }
}

#[cfg(all(test, windows))]
mod windows_real_process_tests {
    use super::*;
    use std::{
        collections::HashSet,
        fs,
        io::Write,
        os::windows::io::AsRawHandle,
        path::PathBuf,
        process::{Command, Stdio},
        sync::atomic::AtomicBool,
        sync::{Arc, Mutex},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

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
            master: Box::new(NullMasterPty),
            writer: Arc::new(Mutex::new(Box::new(std::io::sink()))),
            child: Arc::new(Mutex::new(Box::new(child))),
            process_id: Some(process_id),
            agent_seen: Arc::new(AtomicBool::new(false)),
            agent_pids_seen: Arc::new(Mutex::new(HashSet::new())),
            job,
        }
    }

    struct NullMasterPty;

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
