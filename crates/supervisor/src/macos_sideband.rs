//! The macOS pane sideband: a Unix domain socket that keeps what the named
//! pipe gave on Windows.
//!
//! | Named pipe | Here |
//! |---|---|
//! | ACL for the current user only | socket mode 0600 in a fresh 0700 directory under the per-user temp folder |
//! | `first_pipe_instance`, random name | a new random directory each app start; `bind` fails if the path exists |
//! | `reject_remote_clients` | a Unix socket is local by definition |
//! | client PID from the pipe, then the Job | `LOCAL_PEERPID`, then the run's process scope |
//! | client checks the server PID and creation time | `LOCAL_PEERPID` on the client side, then that process's start time |
//!
//! Frames, limits, authorization and everything after the identity check are
//! the same code the named pipe uses (`handle_sideband_stream`).

use super::*;
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use tokio::net::{UnixListener, UnixStream};

/// `sockaddr_un.sun_path` on macOS holds 104 bytes with its terminator.
pub(super) const MAX_SOCKET_PATH_BYTES: usize = 103;
const SOCKET_FILE_NAME: &str = "cp.sock";

/// The per-user temporary directory, from `confstr`; the environment can be
/// absent or wrong in a GUI app and this cannot be.
fn user_temp_dir() -> Result<PathBuf> {
    let mut buffer = vec![0_u8; 1024];
    // SAFETY: the buffer is writable and its length is passed.
    let length = unsafe { libc::confstr(libc::_CS_DARWIN_USER_TEMP_DIR, buffer.as_mut_ptr().cast(), buffer.len()) };
    if length == 0 || length > buffer.len() {
        return Err(std::io::Error::last_os_error()).context("no per-user temporary directory");
    }
    buffer.truncate(length - 1);
    let path = String::from_utf8(buffer).context("the per-user temporary directory is not UTF-8")?;
    Ok(PathBuf::from(path))
}

/// Creates the directory that will hold this app's sideband socket and
/// returns the socket path. Each app run gets a new directory.
pub(super) fn private_socket_endpoint() -> Result<String> {
    let base = user_temp_dir()?;
    for _ in 0..16 {
        let directory = base.join(format!("prim1-{}", &Uuid::new_v4().simple().to_string()[..16]));
        match fs::DirBuilder::new().mode(0o700).create(&directory) {
            Ok(()) => {
                let socket = directory.join(SOCKET_FILE_NAME);
                let text = socket.to_str().ok_or_else(|| anyhow!("the sideband socket path is not valid Unicode"))?.to_string();
                if text.len() > MAX_SOCKET_PATH_BYTES {
                    let _ = fs::remove_dir(&directory);
                    bail!("the sideband socket path would be {} bytes; macOS allows {MAX_SOCKET_PATH_BYTES}", text.len());
                }
                return Ok(text);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error).with_context(|| format!("failed to create {}", directory.display()));
            }
        }
    }
    bail!("could not create a private directory for the sideband socket");
}

/// Removes the socket and its private directory; never another path's.
pub(super) fn remove_socket_endpoint(endpoint: &str) {
    let socket = Path::new(endpoint);
    let _ = fs::remove_file(socket);
    if let Some(directory) = socket.parent()
        && directory.file_name().and_then(|name| name.to_str()).is_some_and(|name| name.starts_with("prim1-"))
    {
        let _ = fs::remove_dir(directory);
    }
}

fn bind_private_listener(endpoint: &str) -> Result<UnixListener> {
    let listener = UnixListener::bind(endpoint)
        .with_context(|| format!("failed to bind the sideband socket {endpoint}"))?;
    fs::set_permissions(endpoint, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict the sideband socket {endpoint}"))?;
    Ok(listener)
}

pub(super) async fn run_unix_socket_server(
    handle: SupervisorHandle,
    status: ControlPlaneStatus,
    ready: mpsc::SyncSender<Result<(), String>>,
    activation: mpsc::Receiver<()>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let listener = match bind_private_listener(&status.endpoint) {
        Ok(listener) => listener,
        Err(error) => {
            let message = format!("{error:#}");
            let _ = ready.send(Err(message.clone()));
            return Err(anyhow!(message));
        }
    };
    ready
        .send(Ok(()))
        .map_err(|_| anyhow!("control plane startup receiver disconnected"))?;
    if activation.recv().is_err() {
        remove_socket_endpoint(&status.endpoint);
        return Ok(());
    }
    if *shutdown.borrow() {
        remove_socket_endpoint(&status.endpoint);
        return Ok(());
    }

    let connection_slots = Arc::new(Semaphore::new(SIDEBAND_MAX_CONNECTIONS));
    loop {
        tokio::select! {
            changed = shutdown.changed() => {
                match changed {
                    Ok(()) if *shutdown.borrow() => {
                        remove_socket_endpoint(&status.endpoint);
                        return Ok(());
                    }
                    Ok(()) => {}
                    Err(_) => {
                        remove_socket_endpoint(&status.endpoint);
                        return Ok(());
                    }
                }
            }
            accepted = listener.accept() => {
                match accepted {
                    Ok((stream, _)) => {
                        let Some(connection_permit) = reserve_sideband_connection(&connection_slots) else {
                            continue;
                        };
                        let handle_clone = handle.clone();
                        tokio::spawn(async move {
                            let _connection_permit = connection_permit;
                            if let Err(error) = handle_unix_sideband_connection(handle_clone, stream).await {
                                eprintln!("unix socket connection failed: {error}");
                            }
                        });
                    }
                    Err(error) => {
                        // Out of descriptors or a client already gone: this
                        // connection fails, the listener keeps serving.
                        eprintln!("sideband accept failed: {error}");
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }
        }
    }
}

/// The connected client's process id, from the kernel.
pub(super) fn peer_process_id(stream: &UnixStream) -> Result<u32> {
    let mut pid: libc::pid_t = 0;
    let mut length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    // SAFETY: the pointers are valid for the lengths passed.
    let result = unsafe {
        libc::getsockopt(stream.as_raw_fd(), libc::SOL_LOCAL, libc::LOCAL_PEERPID,
            (&raw mut pid).cast(), &raw mut length)
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error()).context("LOCAL_PEERPID failed");
    }
    u32::try_from(pid).context("the peer process id is invalid")
}

/// One sideband client, by the identity of its process.
pub(super) struct MacosPaneProcess {
    process: PinnedProcess,
}

impl MacosPaneProcess {
    pub(super) fn open(process_id: u32) -> Result<Self> {
        Ok(Self { process: PinnedProcess::open(process_id)? })
    }
}

impl PaneProcess for MacosPaneProcess {
    fn pid(&self) -> u32 {
        self.process.pid()
    }

    fn is_alive(&self) -> Result<bool> {
        self.process.is_alive()
    }

    fn belongs_to(&self, pty: &dyn PtySession) -> Result<bool> {
        pty.contains_process(&self.process)
    }

    fn creation_time(&self) -> Option<u64> {
        Some(self.process.creation_time_filetime())
    }
}

/// The parent chain macOS records, each link no younger than its child. A
/// parent the current user may not inspect is `Unreadable`, not an error.
pub(super) struct MacosAncestry;

impl pane_ancestry::ProcessAncestry for MacosAncestry {
    fn parent_of(&self, child: &dyn PaneProcess) -> pane_ancestry::Parent {
        use pane_ancestry::Parent;
        let Some(parent) = pty_host::parent_process_id(child.pid()) else {
            return Parent::Indeterminate(child.pid());
        };
        if parent <= 1 {
            return Parent::Root;
        }
        match MacosPaneProcess::open(parent) {
            Ok(pinned) => match (pinned.creation_time(), child.creation_time()) {
                (Some(parent_started), Some(child_started)) if parent_started <= child_started => {
                    Parent::Pinned(Arc::new(pinned))
                }
                // A younger process holds the id: the real parent has exited.
                (Some(_), Some(_)) => Parent::Gone(parent),
                _ => Parent::Indeterminate(parent),
            },
            Err(error) => {
                if error.is::<pty_host::ProcessAlreadyExited>() {
                    return Parent::Gone(parent);
                }
                // SAFETY: signal 0 only checks existence; ESRCH or EPERM tell
                // apart a parent that is gone from one this user may not read.
                let exists = unsafe { libc::kill(parent as libc::pid_t, 0) } == 0
                    || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
                if exists { Parent::Unreadable(parent) } else { Parent::Gone(parent) }
            }
        }
    }
}

async fn handle_unix_sideband_connection(handle: SupervisorHandle, stream: UnixStream) -> Result<()> {
    // Directory and socket permissions already keep other users out; the
    // kernel's word on the client is checked as well.
    let credentials = stream.peer_cred().context("failed to read the sideband client's credentials")?;
    // SAFETY: geteuid has no failure mode.
    if credentials.uid() != unsafe { libc::geteuid() } {
        bail!("refused a sideband client of another user");
    }
    let client_pid = peer_process_id(&stream)?;
    let process = MacosPaneProcess::open(client_pid).map(|process| Arc::new(process) as Arc<dyn PaneProcess>);
    let resolved = match &process {
        Ok(process) => handle.resolve_pane_caller(process.clone()),
        Err(error) => Err(anyhow!("{error:#}")),
    };
    let attributed = match resolved {
        Ok(caller) => {
            let pid = process.as_ref().map_or(client_pid, |process| process.pid());
            let started = process.as_ref().ok().and_then(|process| process.creation_time());
            let trace = format!("{}: pane '{}' by its process scope",
                pane_ancestry::describe_process(pid, None, started), caller.session);
            pane_ancestry::Attributed { attribution: JobAttribution::Pane(caller), trace }
        }
        Err(error) => {
            eprintln!("sideband client is in no verified pane scope; awaiting a pane secret: {error:#}");
            match (attribution_without_job(&error, None, None), &process) {
                (JobAttribution::Outside(_), Ok(process)) if error.is::<OutsideEveryPane>() =>
                    handle.attribute_unclaimed(process.as_ref(), None, &MacosAncestry),
                (JobAttribution::Outside(pid), _) => pane_ancestry::Attributed {
                    attribution: JobAttribution::Outside(pid),
                    trace: format!("{}: not readable here", pane_ancestry::describe_process(pid, None, None)),
                },
                (attribution, _) => pane_ancestry::Attributed { attribution,
                    trace: format!("{}: {error:#}", pane_ancestry::describe_process(client_pid, None, None)) },
            }
        }
    };
    handle_sideband_stream(handle, attributed, stream).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_endpoint_is_short_owner_only_and_in_a_fresh_directory() {
        let first = private_socket_endpoint().unwrap();
        let second = private_socket_endpoint().unwrap();
        assert_ne!(first, second, "a new directory each time");
        assert!(first.len() <= MAX_SOCKET_PATH_BYTES, "{first}");
        for endpoint in [&first, &second] {
            let metadata = fs::metadata(Path::new(endpoint).parent().unwrap()).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
        }
        remove_socket_endpoint(&first);
        remove_socket_endpoint(&second);
        assert!(!Path::new(&first).parent().unwrap().exists());
        // A path outside the pattern is left alone.
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("keep").join("cp.sock");
        fs::create_dir_all(other.parent().unwrap()).unwrap();
        fs::write(&other, b"").unwrap();
        remove_socket_endpoint(other.to_str().unwrap());
        assert!(other.parent().unwrap().exists(), "only prim1-* directories are removed");
    }

    #[tokio::test]
    async fn the_listener_binds_owner_only_and_reports_the_peer_process() {
        let endpoint = private_socket_endpoint().unwrap();
        let listener = bind_private_listener(&endpoint).unwrap();
        let mode = fs::symlink_metadata(&endpoint).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let (client, accepted) = tokio::join!(UnixStream::connect(&endpoint), listener.accept());
        let client = client.unwrap();
        let (server_side, _) = accepted.unwrap();
        assert_eq!(peer_process_id(&server_side).unwrap(), std::process::id());
        assert_eq!(peer_process_id(&client).unwrap(), std::process::id());
        assert!(server_side.peer_cred().unwrap().uid() == unsafe { libc::geteuid() });
        // A second bind on the same path fails: nothing can squat a live socket.
        assert!(bind_private_listener(&endpoint).is_err());
        drop(server_side);
        drop(listener);
        remove_socket_endpoint(&endpoint);
    }

    #[test]
    fn the_ancestry_walk_reaches_the_test_process_and_ends_at_launchd() {
        let child = MacosPaneProcess::open(std::process::id()).unwrap();
        let mut cursor: Arc<dyn PaneProcess> = Arc::new(child);
        let mut hops = 0;
        loop {
            hops += 1;
            assert!(hops < 64, "the parent chain did not end");
            match pane_ancestry::ProcessAncestry::parent_of(&MacosAncestry, cursor.as_ref()) {
                pane_ancestry::Parent::Pinned(next) => cursor = next,
                pane_ancestry::Parent::Root | pane_ancestry::Parent::Unreadable(_) | pane_ancestry::Parent::Gone(_) => break,
                pane_ancestry::Parent::Indeterminate(pid) => panic!("indeterminate at {pid}"),
            }
        }
    }
}
