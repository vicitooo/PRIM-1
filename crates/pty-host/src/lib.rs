use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shared_types::LaunchSpec;

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

pub trait PtySession: Send {
    fn send_input(&self, input: &str) -> anyhow::Result<()>;
    fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()>;
    fn kill(&self) -> anyhow::Result<()>;
    fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>>;
    fn process_id(&self) -> Option<u32>;
}

pub struct ConcretePtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_id: Option<u32>,
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
}

impl PtySession for ConcretePtySession {
    fn send_input(&self, input: &str) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().expect("pty writer poisoned");
        writer
            .write_all(input.as_bytes())
            .context("failed to write PTY input")?;
        writer.flush().context("failed to flush PTY input")?;
        Ok(())
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
}

impl Drop for ConcretePtySession {
    fn drop(&mut self) {
        #[cfg(windows)]
        let _ = self.job.terminate();

        let _ = self.kill_immediate_child();
        self.wait_for_child_exit(Duration::from_millis(200));
    }
}
