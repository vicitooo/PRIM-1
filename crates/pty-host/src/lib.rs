use std::{
    io::{Read, Write},
    sync::{Arc, Mutex},
    thread,
};

use anyhow::Context;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use shared_types::LaunchSpec;

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

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    child: Arc<Mutex<Box<dyn Child + Send + Sync>>>,
    process_id: Option<u32>,
}

impl PtySession {
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

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("failed to spawn {}", spec.program))?;
        drop(pair.slave);

        let reader = pair.master.try_clone_reader().context("failed to clone PTY reader")?;
        let writer = pair.master.take_writer().context("failed to take PTY writer")?;
        let child = Arc::new(Mutex::new(child));
        let process_id = child.lock().ok().and_then(|guard| guard.process_id());

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
        })
    }

    pub fn send_input(&self, input: &str) -> anyhow::Result<()> {
        let mut writer = self.writer.lock().expect("pty writer poisoned");
        writer
            .write_all(input.as_bytes())
            .context("failed to write PTY input")?;
        writer.flush().context("failed to flush PTY input")?;
        Ok(())
    }

    pub fn resize(&self, cols: u16, rows: u16) -> anyhow::Result<()> {
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

    pub fn kill(&self) -> anyhow::Result<()> {
        self.child
            .lock()
            .expect("pty child poisoned")
            .kill()
            .context("failed to kill PTY child")?;
        Ok(())
    }

    pub fn try_wait(&self) -> anyhow::Result<Option<PtyExitStatus>> {
        let mut child = self.child.lock().expect("pty child poisoned");
        let status = child.try_wait().context("failed to poll PTY child")?;
        Ok(status.map(|status| PtyExitStatus {
            exit_code: status.exit_code(),
            signal: status.signal().map(ToOwned::to_owned),
            success: status.success(),
        }))
    }

    pub fn process_id(&self) -> Option<u32> {
        self.process_id
    }
}
