//! A108 W2: exact-ID followers. One background thread proves, for every
//! running Claude, Codex and Grok member, which conversation the harness
//! process PRIM-1 launched holds now. Every proof binds that process's pid
//! AND start time to harness-owned evidence: Claude's per-process file, the
//! Codex writer locks it or its own codex.exe child holds (the file system's
//! open-handle query), Grok's create/load log lines. Nothing is chosen by
//! folder, time or name alone; two held Codex roots hold Resume instead.
//! Harness files are read with no supervisor lock held; a proven change goes
//! through `apply_tracked_conversation`, which re-validates the run.
use super::*;
use super::session_ledger::ConversationChange;
use std::io::{Read, Seek, SeekFrom};
use std::time::{SystemTime, UNIX_EPOCH};

const TRACK_POLL: Duration = Duration::from_secs(3);
/// FILETIME (100 ns ticks since 1601) of the Unix epoch.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
const PID_FILE_MAX_BYTES: u64 = 64 * 1024;
const SUMMARY_MAX_BYTES: u64 = 4 * 1024 * 1024;
const TRANSCRIPT_HEAD_BYTES: u64 = 64 * 1024;
const DIR_SCAN_MAX: usize = 4096;
const GROK_LOG_CHUNK: u64 = 8 * 1024 * 1024;
const GROK_EVENTS_KEPT: usize = 512;
const GROK_PENDING_MAX_AGE: Duration = Duration::from_secs(10 * 60);
const CODEX_UNWRITTEN_RECHECK: Duration = Duration::from_secs(6);
const CODEX_REJECTED_RECHECK: Duration = Duration::from_secs(60);
/// Lock files listed per pass; beyond it no Codex proof is made.
const CODEX_LOCKS_MAX: usize = 256;
/// Codex creates a thread's lock file when it takes the thread and deletes
/// it on release, so a lock is asked again when its listing changes. A
/// crashed holder leaves its file behind for another process to reuse
/// unchanged; that reuse is seen within this refresh.
const CODEX_LOCK_REFRESH: Duration = Duration::from_secs(60);
/// Refresh queries per pass, so a full lock folder spreads its refresh over
/// passes instead of stalling one (a new or changed lock is always asked).
const CODEX_REFRESHES_PER_PASS: usize = 16;
/// Clock slack between a process start and its first log line, and between
/// file times (timer-tick precision) and the precise clock.
const CLOCK_SLACK: Duration = Duration::from_secs(2);
/// FILETIME ticks per millisecond, the precision of Grok's log times.
const FILETIME_TICKS_PER_MS: u64 = 10_000;
/// The hold on a Codex member holding more than one root conversation.
pub(crate) const CODEX_AMBIGUOUS: &str = "codex_ambiguous";

/// Where each harness keeps its state; `pid_domain` is Claude's
/// `win32:<computer>` for this machine. Tests inject fixture homes.
#[derive(Debug, Clone)]
pub(crate) struct HarnessHomes {
    pub claude: Option<PathBuf>,
    pub codex: Option<PathBuf>,
    pub grok: Option<PathBuf>,
    pub pid_domain: String,
}

impl HarnessHomes {
    pub(crate) fn from_env() -> Self {
        Self::from_vars(|name| std::env::var_os(name))
    }

    fn from_vars(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> Self {
        // An override that is set but not absolute disables that follower.
        let home = |variable: &str, default: &str| {
            let profile = var("USERPROFILE").or_else(|| cfg!(target_os = "macos").then(|| var("HOME")).flatten());
            match var(variable) {
                Some(value) => Some(PathBuf::from(value)),
                None => profile.map(|profile| PathBuf::from(profile).join(default)),
            }
            .filter(|path| path.is_absolute())
        };
        let computer = var("COMPUTERNAME").and_then(|name| name.into_string().ok()).unwrap_or_default();
        Self {
            claude: home("CLAUDE_CONFIG_DIR", ".claude"),
            codex: home("CODEX_HOME", ".codex"),
            grok: home("GROK_HOME", ".grok"),
            // Claude Code's own label for this machine's pid namespace.
            pid_domain: if cfg!(target_os = "macos") { "darwin".to_owned() } else { format!("win32:{}", computer.to_lowercase()) },
        }
    }
}

/// The member's harness process: inside its pane's Job, its image the
/// harness. Returns the process start (FILETIME) every proof must match.
pub(crate) trait ProcessProbe: Send {
    fn harness_start(&self, pty: &dyn PtySession, pid: u32, driver: DriverKind) -> Result<u64>;

    /// The harness process's own `codex*.exe` children inside its pane's
    /// Job, each as (pid, start). Through the npm shim the harness is node
    /// running codex.js, and its vendored codex.exe child holds the locks.
    fn codex_children(&self, _pty: &dyn PtySession, _pid: u32, _started: u64) -> Vec<(u32, u64)> {
        Vec::new()
    }
}

pub(crate) struct NativeProcessProbe;

impl ProcessProbe for NativeProcessProbe {
    #[cfg(any(windows, target_os = "macos"))]
    fn harness_start(&self, pty: &dyn PtySession, pid: u32, driver: DriverKind) -> Result<u64> {
        let process = PinnedProcess::open(pid)?;
        if !pty.contains_process(&process)? {
            bail!("process {pid} is outside its pane's Job");
        }
        if !pty_host::expected_agent_image_name(driver, &process.image_path()?) {
            bail!("process {pid} is not the pane's harness");
        }
        Ok(process.creation_time_filetime())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn harness_start(&self, _pty: &dyn PtySession, _pid: u32, _driver: DriverKind) -> Result<u64> {
        bail!("exact-ID following needs Windows process identity")
    }

    /// Direct children only: a Codex the agent starts from its own shell is
    /// a deeper descendant and never the member's. A child is created after
    /// its parent, so one whose recorded parent pid now names the harness
    /// only by reuse started earlier and is refused.
    #[cfg(any(windows, target_os = "macos"))]
    fn codex_children(&self, pty: &dyn PtySession, pid: u32, started: u64) -> Vec<(u32, u64)> {
        let Ok(members) = pty.live_processes() else { return Vec::new() };
        members.into_iter()
            .filter(|member| member.process_id != pid && member.image_name.as_deref().is_some_and(is_codex_image))
            .filter_map(|member| {
                // While pinned its pid cannot be reused, so the parent read
                // through a second handle is this process's.
                let child = PinnedProcess::open(member.process_id).ok()?;
                let own = child.creation_time_filetime() >= started
                    && pty.contains_process(&child).ok()?
                    && is_codex_image(&child.image_path().ok()?)
                    && parent_process_id(child.pid()) == Some(pid);
                own.then(|| (child.pid(), child.creation_time_filetime()))
            })
            .collect()
    }
}

/// `codex.exe` or a variant of it, by file name.
#[cfg(any(windows, target_os = "macos"))]
fn is_codex_image(path: &str) -> bool {
    let name = path.rsplit(['\\', '/']).next().unwrap_or(path).to_ascii_lowercase();
    name.starts_with("codex") && (name.ends_with(".exe") || (cfg!(target_os = "macos") && !name.contains('.')))
}

#[cfg(target_os = "macos")]
pub(super) use pty_host::parent_process_id;

/// The process that created `pid`, as Windows recorded it at creation.
#[cfg(windows)]
pub(super) fn parent_process_id(pid: u32) -> Option<u32> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Wdk::System::Threading::{NtQueryInformationProcess, ProcessBasicInformation};
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_BASIC_INFORMATION, PROCESS_QUERY_LIMITED_INFORMATION};
    let raw = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if raw.is_null() {
        return None;
    }
    let process = unsafe { OwnedHandle::from_raw_handle(raw as _) };
    let mut info: PROCESS_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
    let status = unsafe {
        NtQueryInformationProcess(process.as_raw_handle() as HANDLE, ProcessBasicInformation,
            (&raw mut info).cast(), std::mem::size_of::<PROCESS_BASIC_INFORMATION>() as u32, std::ptr::null_mut())
    };
    if status < 0 {
        return None;
    }
    u32::try_from(info.InheritedFromUniqueProcessId).ok()
}

/// Processes holding a file open, as (pid, start FILETIME). Read-only.
pub(crate) trait LockHolders: Send {
    fn holders(&self, path: &Path) -> Result<Vec<(u32, u64)>>;
}

/// Asks the file system which processes have a file open
/// (FileProcessIdsUsingFileInformation, the query behind the Restart
/// Manager's list) and pins each to its start time. A Restart Manager
/// session writes two or three Application event-log entries per call; this
/// writes none. It still walks every handle table (about 15 ms on a busy
/// PC), so the tracker asks only when a lock file changes or once a minute.
pub(crate) struct OpenFileHolders;

impl LockHolders for OpenFileHolders {
    #[cfg(windows)]
    fn holders(&self, path: &Path) -> Result<Vec<(u32, u64)>> {
        use std::os::windows::ffi::OsStrExt as _;
        use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
        use windows_sys::Wdk::Storage::FileSystem::{FileProcessIdsUsingFileInformation, NtQueryInformationFile};
        use windows_sys::Win32::Foundation::{
            HANDLE, INVALID_HANDLE_VALUE, STATUS_BUFFER_OVERFLOW, STATUS_INFO_LENGTH_MISMATCH,
        };
        use windows_sys::Win32::Storage::FileSystem::{
            CreateFileW, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
            FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
        };
        use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // Attribute-only access takes no share access, so Codex's own opens,
        // locks and deletes are never refused because of this handle.
        let raw = unsafe {
            CreateFileW(wide.as_ptr(), FILE_READ_ATTRIBUTES, FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(), OPEN_EXISTING, FILE_FLAG_OPEN_REPARSE_POINT, std::ptr::null_mut())
        };
        if raw == INVALID_HANDLE_VALUE {
            let error = std::io::Error::last_os_error();
            // Released and deleted since it was listed: nobody holds it.
            if error.kind() == std::io::ErrorKind::NotFound {
                return Ok(Vec::new());
            }
            return Err(error).with_context(|| format!("cannot open {} to list its holders", path.display()));
        }
        let file = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        // FILE_PROCESS_IDS_USING_FILE_INFORMATION: a u32 count, then ULONG_PTR
        // ids at pointer alignment, so word 0 is the count, words 1.. the ids.
        let mut words = 64_usize;
        loop {
            let mut buffer = vec![0_usize; words + 1];
            let mut status_block = IO_STATUS_BLOCK::default();
            let status = unsafe {
                NtQueryInformationFile(file.as_raw_handle() as HANDLE, &mut status_block, buffer.as_mut_ptr().cast(),
                    (buffer.len() * std::mem::size_of::<usize>()) as u32, FileProcessIdsUsingFileInformation)
            };
            let count = buffer[0] as u32 as usize;
            let short = matches!(status, STATUS_INFO_LENGTH_MISMATCH | STATUS_BUFFER_OVERFLOW)
                || (status >= 0 && count > words);
            if short && words < 1 << 16 {
                words = (count + 16).max(words * 4);
                continue;
            }
            if status < 0 || count > words {
                bail!("cannot list the processes using {} (NTSTATUS {status:#010x})", path.display());
            }
            // A holder that exited holds nothing; one PRIM-1 cannot open
            // (another user's, protected) is never a member's own harness.
            return Ok(buffer[1..=count].iter()
                .filter_map(|id| u32::try_from(*id).ok())
                .filter_map(|pid| PinnedProcess::open(pid).ok().map(|process| (pid, process.creation_time_filetime())))
                .collect());
        }
    }

    /// The processes with `path` open, as the kernel lists them, each with
    /// its start time.
    #[cfg(target_os = "macos")]
    fn holders(&self, path: &Path) -> Result<Vec<(u32, u64)>> {
        Ok(pty_host::processes_holding_path(path)?
            .into_iter()
            .filter_map(|pid| PinnedProcess::open(pid).ok().map(|process| (pid, process.creation_time_filetime())))
            .collect())
    }

    #[cfg(not(any(windows, target_os = "macos")))]
    fn holders(&self, _path: &Path) -> Result<Vec<(u32, u64)>> {
        bail!("listing a file's holders is not available on this platform")
    }
}

/// The run moved on (stopped, restarted or replaced) while its proof was read.
#[derive(Debug)]
pub(crate) struct TrackedRunGone;

impl std::fmt::Display for TrackedRunGone {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("the tracked run is no longer current")
    }
}

impl std::error::Error for TrackedRunGone {}

/// Stops the follower thread on shutdown. The thread holds the supervisor
/// only during a pass, so a dropped supervisor ends it too.
#[derive(Default)]
pub(crate) struct TrackerControl {
    stopped: Mutex<bool>,
    wake: Condvar,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
    /// Claude's home for the launch-time transcript check, set only when the
    /// native follower starts at boot; unset (tests, held trials) keeps resume.
    claude_home: Mutex<Option<PathBuf>>,
    /// Conversation ids PRIM-1 minted for Claude members in this run and never
    /// seen used. Only such an id may start as new when its transcript is
    /// definitely absent. Any evidence of use (a submitted line or message, a
    /// found transcript, attach, Resume here) removes it for good: nothing
    /// re-adds a removed id, because only a launch that is already eligible or
    /// a brand-new id is recorded. Not used across a restart: after one, every
    /// stored id resumes as before.
    minted_claude: Mutex<HashMap<SessionId, String>>,
}

impl TrackerControl {
    pub(crate) fn set_claude_home(&self, home: Option<PathBuf>) {
        *self.claude_home.lock() = home;
    }

    pub(crate) fn claude_home(&self) -> Option<PathBuf> {
        self.claude_home.lock().clone()
    }

    pub(crate) fn note_minted_claude(&self, session_id: SessionId, conversation_id: String) {
        self.minted_claude.lock().insert(session_id, conversation_id);
    }

    /// True only when `conversation_id` is an id PRIM-1 minted for this
    /// member in this process and no input or other use of it was seen.
    /// v3 (Claude PRIM-1 72a26ffe, Reviewer b87359eb): marks are never
    /// restored across a PRIM-1 restart, so after one a missing conversation
    /// is named and Start fresh session stays the operator's.
    pub(crate) fn never_used_minted(&self, session_id: SessionId, conversation_id: &str) -> bool {
        self.minted_claude.lock().get(&session_id).is_some_and(|id| id == conversation_id)
    }

    pub(crate) fn minted_claude(&self, session_id: SessionId) -> Option<String> {
        self.minted_claude.lock().get(&session_id).cloned()
    }

    pub(crate) fn revoke_minted_claude(&self, session_id: SessionId) {
        self.minted_claude.lock().remove(&session_id);
    }

    /// Revokes only when the used conversation is the minted one (None: any),
    /// so input to an old run cannot revoke the brand-new id a fresh restart
    /// minted.
    pub(crate) fn revoke_minted_claude_conversation(&self, session_id: SessionId, conversation: Option<&str>) {
        let mut minted = self.minted_claude.lock();
        if conversation.is_none_or(|conversation| minted.get(&session_id).is_some_and(|id| id == conversation)) {
            minted.remove(&session_id);
        }
    }

    pub(crate) fn stop(&self) {
        *self.stopped.lock() = true;
        self.wake.notify_all();
    }

    /// Waits one poll interval; true once stopped.
    fn wait(&self, timeout: Duration) -> bool {
        let mut stopped = self.stopped.lock();
        if !*stopped {
            self.wake.wait_for(&mut stopped, timeout);
        }
        *stopped
    }

    #[cfg(test)]
    pub(crate) fn take_thread(&self) -> Option<thread::JoinHandle<()>> {
        self.thread.lock().take()
    }
}

/// One running member as read under the registry lock; `started` is filled
/// in by the process probe, outside it.
#[derive(Debug, Clone)]
struct Member {
    session_id: SessionId,
    run_id: Uuid,
    generation: SessionGeneration,
    driver: DriverKind,
    label: String,
    working_dir: String,
    pid: u32,
    started: u64,
    /// Codex: the harness's own codex.exe children (the npm shim's), whose
    /// locks are the member's as its own are.
    children: Vec<(u32, u64)>,
    /// When PRIM-1 spawned this run; the process it follows started before.
    launched_at: Option<SystemTime>,
    current: Option<String>,
    /// A Codex launch, shell-report or ambiguity hold a writer-lock proof
    /// releases.
    codex_hold: bool,
    /// This run's own `--prim1-identify` report, if any.
    reported: Option<Report>,
}

/// This run's `--prim1-identify` report: its thread and when it was made.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Report {
    thread: String,
    at: Option<SystemTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Proof {
    id: String,
    reason: &'static str,
    via: &'static str,
    /// A "title" (first seen) or "rename" proof's title, for the register.
    title: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexThread {
    Root { created: Option<SystemTime> },
    /// Lock-proved, not yet written (no rollout, or its first line is still
    /// being written): nothing says root or subagent yet.
    Unwritten,
    Rejected,
}

#[derive(Debug, Clone, Copy)]
struct HeldThread {
    verdict: CodexThread,
    checked: Instant,
    /// When this run first saw its process hold the thread's lock.
    first_held: SystemTime,
}

/// Logged once per run (ambiguity: once per distinct root set).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CodexNotice {
    Ambiguous,
    HeaderTooLarge,
}

/// One lock file as last listed and asked.
#[derive(Debug, Clone)]
struct LockEntry {
    /// Creation and write times from the folder listing: a trigger to ask
    /// again, never proof of a holder.
    listed: (Option<SystemTime>, Option<SystemTime>),
    holders: Vec<(u32, u64)>,
    asked: Instant,
}

#[derive(Debug, Clone)]
struct HeldLock {
    thread: String,
    holders: Vec<(u32, u64)>,
}

#[derive(Debug, Clone)]
struct GrokEvent {
    seq: u64,
    pid: u32,
    at: SystemTime,
    sid: String,
    created: bool,
}

/// What one run has proved so far. Dropped when the run ends.
#[derive(Default)]
struct RunTrack {
    /// The process start first seen for this run; another is never followed.
    started: u64,
    /// Not followed: its process changed, or started after the launch.
    refused: bool,
    /// The ID this run last proved current, and when PRIM-1 confirmed it.
    confirmed: Option<(String, SystemTime)>,
    /// Claude: when the current ID was last read from the pid file.
    seen_at: Option<SystemTime>,
    /// Claude: the title last given to the register.
    claude_name: Option<String>,
    /// Codex threads whose lock this run's process holds now.
    codex_threads: HashMap<String, HeldThread>,
    /// The identify report whose thread was last re-verified.
    report_checked: Option<Report>,
    header_too_large_logged: bool,
    /// Codex roots held when the current ID was last proved exact; a root
    /// outside them makes a multi-root member ambiguous again.
    exact_roots: Vec<String>,
    /// The roots behind a proof not yet applied.
    proposed_roots: Vec<String>,
    /// The ambiguous root set last logged, so it is logged once.
    ambiguous: Option<Vec<String>>,
    grok_next: u64,
    /// Log lines not yet applied: a proven one stays until its change is
    /// applied and persisted, so a failed apply is retried.
    grok_pending: Vec<(GrokEvent, Instant)>,
    /// The log line behind a proof not yet applied.
    grok_proposed: Option<u64>,
    /// Proofs whose apply failed, logged once each.
    failed: HashSet<String>,
}

impl RunTrack {
    fn new(member: &Member) -> Self {
        Self { started: member.started, ..Self::default() }
    }

    fn confirm(&mut self, id: &str) {
        self.confirmed = Some((id.to_owned(), SystemTime::now()));
        self.ambiguous = None;
    }
}

/// The shared tail of Grok's unified log; members take their own pid's lines.
#[derive(Default)]
struct GrokLog {
    offset: Option<u64>,
    file_created: Option<SystemTime>,
    events: VecDeque<GrokEvent>,
    next_seq: u64,
}

impl GrokLog {
    fn path(home: &Path) -> PathBuf {
        home.join("logs").join("unified.jsonl")
    }

    /// Starts at the current end: earlier lines belong to earlier processes.
    fn prime(&mut self, home: Option<&Path>) {
        let metadata = home.and_then(|home| fs::metadata(Self::path(home)).ok());
        self.offset = Some(metadata.as_ref().map_or(0, fs::Metadata::len));
        self.file_created = metadata.and_then(|metadata| metadata.created().ok());
    }

    fn read_new(&mut self, home: Option<&Path>) {
        let Some(home) = home else { return };
        let path = Self::path(home);
        let Ok(metadata) = fs::metadata(&path) else { return };
        let created = metadata.created().ok();
        let mut offset = self.offset.unwrap_or(0);
        // Rotated or truncated: the new file is read from its start.
        if created != self.file_created || metadata.len() < offset {
            offset = 0;
            self.file_created = created;
        }
        self.offset = Some(offset);
        if metadata.len() == offset {
            return;
        }
        let Ok(mut file) = File::open(&path) else { return };
        let mut chunk = Vec::new();
        if file.seek(SeekFrom::Start(offset)).is_err()
            || (&mut file).take(GROK_LOG_CHUNK).read_to_end(&mut chunk).is_err()
        {
            return;
        }
        let consumed = match chunk.iter().rposition(|byte| *byte == b'\n') {
            Some(end) => end + 1,
            // One line larger than a chunk carries nothing we read: skip it.
            None if chunk.len() as u64 >= GROK_LOG_CHUNK => chunk.len(),
            None => 0,
        };
        for line in chunk[..consumed].split(|byte| *byte == b'\n') {
            self.observe(line);
        }
        self.offset = Some(offset + consumed as u64);
    }

    fn observe(&mut self, line: &[u8]) {
        let has = |needle: &[u8]| line.windows(needle.len()).any(|window| window == needle);
        if !has(b"session created") && !has(b"session loaded") {
            return;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else { return };
        let created = match value["msg"].as_str() {
            Some("session created") => true,
            Some("session loaded") => false,
            _ => return,
        };
        let pid = value["pid"].as_u64().and_then(|pid| u32::try_from(pid).ok());
        let sid = value["sid"].as_str().and_then(canonical_uuid);
        let at = value["ts"].as_str().and_then(|ts| DateTime::parse_from_rfc3339(ts).ok())
            .map(|at| SystemTime::from(at.with_timezone(&Utc)));
        let (Some(pid), Some(sid), Some(at)) = (pid, sid, at) else { return };
        self.events.push_back(GrokEvent { seq: self.next_seq, pid, at, sid, created });
        self.next_seq += 1;
        while self.events.len() > GROK_EVENTS_KEPT {
            self.events.pop_front();
        }
    }
}

pub(crate) struct IdentityTracker {
    homes: HarnessHomes,
    holders: Box<dyn LockHolders>,
    probe: Box<dyn ProcessProbe>,
    runs: HashMap<Uuid, RunTrack>,
    grok_log: GrokLog,
    /// Codex lock files by thread, kept while a Codex member runs.
    codex_locks: HashMap<String, LockEntry>,
    /// Set while Codex locks cannot all be read, so that is logged once.
    codex_locks_unread: bool,
}

impl IdentityTracker {
    pub(crate) fn new(homes: HarnessHomes, holders: Box<dyn LockHolders>, probe: Box<dyn ProcessProbe>) -> Self {
        Self { homes, holders, probe, runs: HashMap::new(), grok_log: GrokLog::default(),
            codex_locks: HashMap::new(), codex_locks_unread: false }
    }

    pub(crate) fn native() -> Self {
        Self::new(HarnessHomes::from_env(), Box::new(OpenFileHolders), Box::new(NativeProcessProbe))
    }

    pub(crate) fn prime(&mut self) {
        self.grok_log.prime(self.homes.grok.as_deref());
    }

    /// One pass over every running member. Holds no supervisor lock while
    /// it reads harness state.
    pub(crate) fn poll(&mut self, handle: &SupervisorHandle) {
        let candidates = handle.tracked_members();
        self.runs.retain(|run_id, _| candidates.iter().any(|(member, _)| member.run_id == *run_id));
        self.grok_log.read_new(self.homes.grok.as_deref());
        let mut members = Vec::new();
        for (mut member, pty) in candidates {
            let Ok(started) = self.probe.harness_start(pty.as_ref(), member.pid, member.driver) else {
                continue;
            };
            member.started = started;
            let track = self.runs.entry(member.run_id).or_insert_with(|| RunTrack::new(&member));
            // The process first seen must predate the launch, and stays the
            // only one followed for this run: a later process behind the
            // same pid (even in the same Job) is refused, never followed.
            let refuse = if track.started != started {
                Some("the process behind its pid changed")
            } else if member.launched_at.is_some_and(|launched| {
                filetime_to_system(started).is_none_or(|start| start > launched + CLOCK_SLACK)
            }) {
                Some("its process started after the launch")
            } else {
                None
            };
            if let Some(why) = refuse && !std::mem::replace(&mut track.refused, true) {
                handle.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Warn,
                    message: format!("{}: {why}, so this run's conversation is no longer followed (run_id={})", member.label, member.run_id),
                    timestamp: now_rfc3339(),
                });
            }
            if !track.refused {
                if member.driver == DriverKind::Codex {
                    member.children = self.probe.codex_children(pty.as_ref(), member.pid, started);
                }
                members.push(member);
            }
        }
        let locks = match &self.homes.codex {
            Some(home) if members.iter().any(|member| member.driver == DriverKind::Codex) => {
                let locks = codex_held_locks(home, self.holders.as_ref(), &mut self.codex_locks, CODEX_LOCK_REFRESH);
                if locks.is_none() && !std::mem::replace(&mut self.codex_locks_unread, true) {
                    handle.emit(RuntimeEvent::SystemLog {
                        level: LogLevel::Warn,
                        message: "Codex writer locks could not all be read; Codex conversation changes are not followed until they can".into(),
                        timestamp: now_rfc3339(),
                    });
                }
                self.codex_locks_unread &= locks.is_none();
                locks
            }
            _ => {
                self.codex_locks.clear();
                None
            }
        };
        for member in members {
            let track = self.runs.get_mut(&member.run_id).expect("tracked above");
            let proofs = match member.driver {
                DriverKind::Claude => self.homes.claude.as_deref()
                    .map(|home| claude_step(home, &self.homes.pid_domain, &member, track))
                    .unwrap_or_default(),
                DriverKind::Codex => {
                    let (Some(home), Some(locks)) = (self.homes.codex.as_deref(), locks.as_deref()) else { continue };
                    let (proof, notices) = codex_step(home, &member, track, locks);
                    for notice in notices {
                        let message = match notice {
                            CodexNotice::Ambiguous => format!("{}: Codex holds more than one root conversation, so the current one is ambiguous and Resume is held. It becomes exact when the agent runs --prim1-identify or the older conversation is released (run_id={})", member.label, member.run_id),
                            CodexNotice::HeaderTooLarge => format!("{}: a Codex conversation's first line is larger than PRIM-1 reads ({} KiB), so that conversation cannot be proved (run_id={})", member.label, session::CODEX_HEADER_LIMIT / 1024, member.run_id),
                        };
                        handle.emit(RuntimeEvent::SystemLog { level: LogLevel::Warn, message, timestamp: now_rfc3339() });
                    }
                    // The ID Launch would resume may be stale: held until a
                    // proof releases it; a failed write is retried next pass.
                    if track.ambiguous.is_some() {
                        let seen_report = member.reported.as_ref().map(|report| report.thread.as_str());
                        match handle.hold_ambiguous_codex(member.session_id, member.run_id, member.generation, member.pid, seen_report) {
                            Ok(_) => {}
                            Err(error) if error.is::<TrackedRunGone>() => {}
                            Err(error) => {
                                if track.failed.insert(CODEX_AMBIGUOUS.into()) {
                                    handle.emit(RuntimeEvent::SystemLog {
                                        level: LogLevel::Warn,
                                        message: format!("{}: Resume of its ambiguous Codex conversation could not be held yet: {error:#}", member.label),
                                        timestamp: now_rfc3339(),
                                    });
                                }
                            }
                        }
                    }
                    proof.into_iter().collect()
                }
                DriverKind::Grok => self.homes.grok.as_deref()
                    .and_then(|home| grok_step(home, &member, track, &self.grok_log))
                    .into_iter().collect(),
                DriverKind::Prime | DriverKind::GenericTerminal => Vec::new(),
            };
            let seen_report = member.reported.as_ref().map(|report| report.thread.as_str());
            for proof in proofs {
                match handle.apply_tracked_conversation(member.session_id, member.run_id, member.generation,
                    &proof.id, proof.reason, proof.via, member.pid, seen_report)
                {
                    // A title is the register's once its ID is still the one
                    // followed; until then it is offered again.
                    Ok(applied) if proof.title.is_some() => {
                        if applied && let Some(title) = proof.title {
                            handle.note_conversation_title(member.session_id, title.clone(), "claude_name");
                            track.claude_name = Some(title);
                        }
                    }
                    Ok(_) => {
                        track.confirm(&proof.id);
                        track.exact_roots = std::mem::take(&mut track.proposed_roots);
                        if let Some(seq) = track.grok_proposed.take() {
                            track.grok_pending.retain(|(event, _)| event.seq > seq);
                        }
                    }
                    Err(error) if error.is::<TrackedRunGone>() => {}
                    Err(error) => {
                        if track.failed.insert(format!("{}:{}", proof.reason, proof.id)) {
                            handle.emit(RuntimeEvent::SystemLog {
                                level: LogLevel::Warn,
                                message: format!("{}: followed conversation change ({}, proved by {}) was not applied: {error:#}", member.label, proof.reason, proof.via),
                                timestamp: now_rfc3339(),
                            });
                        }
                    }
                }
            }
        }
    }
}

/// Claude rewrites `sessionId` in place on /clear, an in-session /resume and
/// a fork; a rename goes to `name`. A missing or foreign file is pending.
fn claude_step(home: &Path, pid_domain: &str, member: &Member, track: &mut RunTrack) -> Vec<Proof> {
    let Some((session_id, name)) = read_claude_pid_file(home, pid_domain, member) else {
        return Vec::new();
    };
    let now = SystemTime::now();
    let mut proofs = Vec::new();
    let current = member.current.as_deref().map(conversation_key);
    let is_current = current.as_deref() == Some(session_id.as_str());
    if is_current {
        track.seen_at = Some(now);
        if track.confirmed.is_none() {
            track.confirm(&session_id);
        }
    } else if let Some(transcript) = claude_transcript(home, &member.working_dir, &session_id) {
        // Used once its transcript exists; until then a resume would fail.
        let created = fs::metadata(&transcript).and_then(|metadata| metadata.created()).ok();
        let reason = if transcript_is_fork(&transcript) {
            "fork"
        } else {
            match (track.seen_at, created) {
                (Some(seen), Some(created)) if created + CLOCK_SLACK < seen => "resume",
                (Some(_), _) => "clear",
                // Changed before the current ID was ever read: proven, its
                // cause unknown.
                (None, _) => "observed",
            }
        };
        proofs.push(Proof { id: session_id.clone(), reason, via: "pid_file", title: None });
    }
    // Titles only, never identity: one waits until its ID is followed, and
    // counts as given once `poll` has applied it.
    if let Some(name) = name
        && track.claude_name.as_ref() != Some(&name)
        && (is_current || !proofs.is_empty())
    {
        let reason = if track.claude_name.is_some() { "rename" } else { "title" };
        proofs.push(Proof { id: session_id, reason, via: "pid_file", title: Some(name) });
    }
    proofs
}

/// Accepts `sessions\<pid>.json` only for this exact process on this machine,
/// in the member's folder, as an interactive session.
/// Does this per-process file's start time name the launched process?
/// Windows: FILETIME ticks, as text or a number. macOS: Claude writes the
/// `ps -o lstart` form in local time ("Sat Nov  7 07:22:22 2026"), good to one
/// second, and a future `procStartFt` is taken when present.
#[cfg(not(target_os = "macos"))]
fn claude_proc_start_matches(value: &serde_json::Value, started: u64) -> bool {
    match &value["procStart"] {
        serde_json::Value::String(text) => text.parse::<u64>().ok() == Some(started),
        serde_json::Value::Number(number) => number.as_u64() == Some(started),
        _ => false,
    }
}

#[cfg(target_os = "macos")]
fn claude_proc_start_matches(value: &serde_json::Value, started: u64) -> bool {
    if let Some(ticks) = value["procStartFt"].as_str().and_then(|text| text.parse::<u64>().ok()) {
        return ticks == started;
    }
    // A number or a numeric string is the FILETIME form (test fixtures, or a
    // file written by a Windows build); the lstart text is Claude's own here.
    match &value["procStart"] {
        serde_json::Value::Number(number) => return number.as_u64() == Some(started),
        serde_json::Value::String(text) if text.parse::<u64>().is_ok() => {
            return text.parse::<u64>().ok() == Some(started);
        }
        _ => {}
    }
    let Some(text) = value["procStart"].as_str() else { return false };
    let Ok(naive) = chrono::NaiveDateTime::parse_from_str(text.trim(), "%a %b %e %H:%M:%S %Y") else { return false };
    let Some(at) = naive.and_local_timezone(chrono::Local).earliest() else { return false };
    let started_seconds = started.saturating_sub(FILETIME_UNIX_EPOCH) / 10_000_000;
    u64::try_from(at.timestamp()).is_ok_and(|seconds| seconds.abs_diff(started_seconds) <= 1)
}

fn read_claude_pid_file(home: &Path, pid_domain: &str, member: &Member) -> Option<(String, Option<String>)> {
    let bytes = read_bounded(&home.join("sessions").join(format!("{}.json", member.pid)), PID_FILE_MAX_BYTES).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let start_matches = claude_proc_start_matches(&value, member.started);
    let same_folder = value["cwd"].as_str().is_some_and(|cwd| {
        session::normalize_windows_path(cwd) == session::normalize_windows_path(&member.working_dir)
    });
    if value["pid"].as_u64() != Some(u64::from(member.pid))
        || !start_matches
        || value["pidDomain"].as_str() != Some(pid_domain)
        || value["kind"].as_str() != Some("interactive")
        || !same_folder
    {
        return None;
    }
    let session_id = value["sessionId"].as_str().and_then(canonical_uuid)?;
    Some((session_id, value["name"].as_str().map(str::to_owned)))
}

/// What the launch-time D2 check knows about a Claude transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TranscriptLookup {
    Found,
    /// Every place Claude could have put it was read, and it is not there.
    Absent,
    /// A read failed, a name was not a plain file, or the scan was cut short.
    Unknown,
}

/// Whether Claude has written this conversation's transcript. Unlike
/// `claude_transcript`, any read error or an incomplete scan is `Unknown`,
/// never `Absent`: a member that never received a message has a minted id but
/// no transcript, and only a definite absence may start it as new.
pub(crate) fn claude_transcript_lookup(home: &Path, working_dir: &str, id: &str) -> TranscriptLookup {
    claude_transcript_lookup_within(home, working_dir, id, DIR_SCAN_MAX)
}

fn claude_transcript_lookup_within(home: &Path, working_dir: &str, id: &str, max_folders: usize) -> TranscriptLookup {
    let file_state = |path: &Path| match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Some(true),
        Ok(_) => None,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    };
    let projects = home.join("projects");
    let file = format!("{id}.jsonl");
    let folder: String = working_dir
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect();
    match file_state(&projects.join(folder).join(&file)) {
        Some(true) => return TranscriptLookup::Found,
        Some(false) => {}
        None => return TranscriptLookup::Unknown,
    }
    let entries = match fs::read_dir(&projects) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return TranscriptLookup::Absent,
        Err(_) => return TranscriptLookup::Unknown,
    };
    for (index, entry) in entries.enumerate() {
        if index >= max_folders {
            return TranscriptLookup::Unknown;
        }
        let Ok(entry) = entry else { return TranscriptLookup::Unknown };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => {}
            Ok(_) => continue,
            Err(_) => return TranscriptLookup::Unknown,
        }
        match file_state(&entry.path().join(&file)) {
            Some(true) => return TranscriptLookup::Found,
            Some(false) => {}
            None => return TranscriptLookup::Unknown,
        }
    }
    TranscriptLookup::Absent
}

/// v3 W3: the transcript a room wake's submission is confirmed in.
pub(crate) fn claude_transcript_path(home: &Path, working_dir: &str, id: &str) -> Option<PathBuf> {
    claude_transcript(home, working_dir, id)
}

/// `projects\<folder>\<id>.jsonl`, the folder Claude derives from the cwd;
/// else the exact file name in any project folder (long paths are hashed).
fn claude_transcript(home: &Path, working_dir: &str, id: &str) -> Option<PathBuf> {
    let projects = home.join("projects");
    let file = format!("{id}.jsonl");
    let folder: String = working_dir
        .chars()
        .map(|character| if character.is_ascii_alphanumeric() { character } else { '-' })
        .collect();
    let direct = projects.join(folder).join(&file);
    if direct.is_file() {
        return Some(direct);
    }
    fs::read_dir(&projects).ok()?.flatten().take(DIR_SCAN_MAX)
        .map(|entry| entry.path().join(&file))
        .find(|path| path.is_file())
}

fn transcript_is_fork(path: &Path) -> bool {
    let Ok(file) = File::open(path) else { return false };
    let mut head = Vec::new();
    if file.take(TRANSCRIPT_HEAD_BYTES).read_to_end(&mut head).is_err() {
        return false;
    }
    head.split(|byte| *byte == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .any(|record| record.get("forkedFrom").is_some_and(serde_json::Value::is_object))
}

/// Every thread lock some process holds, shared by all Codex members. The
/// folder listing is only the trigger: a lock is asked (one query each)
/// when it first appears, when its listed times change (Codex deletes a lock
/// on release and creates it again on the next take) and at least once per
/// `refresh` (a few per pass), never on every pass. None when any lock could
/// not be asked: a missed lock could make a second root look like the only one.
fn codex_held_locks(home: &Path, holders: &dyn LockHolders, cache: &mut HashMap<String, LockEntry>,
    refresh: Duration) -> Option<Vec<HeldLock>> {
    let entries = match fs::read_dir(home.join("thread-writer-locks")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            cache.clear();
            return Some(Vec::new());
        }
        Err(_) => return None,
    };
    let mut listed = HashSet::new();
    let mut refreshed = 0;
    for entry in entries {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let Some(thread) = name.to_str().and_then(|name| name.strip_suffix(".lock"))
            .and_then(|stem| session::canonical_thread_id(stem).ok()) else { continue };
        if !entry.file_type().ok()?.is_file() {
            continue;
        }
        if listed.len() >= CODEX_LOCKS_MAX {
            return None;
        }
        // On Windows the listing carries these times; nothing is opened.
        let metadata = entry.metadata().ok()?;
        let times = (metadata.created().ok(), metadata.modified().ok());
        let ask = match cache.get(&thread) {
            Some(cached) if cached.listed == times => {
                let due = cached.asked.elapsed() >= refresh && refreshed < CODEX_REFRESHES_PER_PASS;
                refreshed += usize::from(due);
                due
            }
            _ => true,
        };
        if ask {
            let holders = holders.holders(&entry.path()).ok()?;
            cache.insert(thread.clone(), LockEntry { listed: times, holders, asked: Instant::now() });
        }
        listed.insert(thread);
    }
    cache.retain(|thread, _| listed.contains(thread));
    Some(cache.iter().filter(|(_, entry)| !entry.holders.is_empty())
        .map(|(thread, entry)| HeldLock { thread: thread.clone(), holders: entry.holders.clone() })
        .collect())
}

/// A rollout check for one held thread. Missing or still being written is
/// Unwritten; Rejected only for a definite non-root or another folder.
fn codex_verdict(sessions: &Path, thread: &str, working_dir: &str) -> (CodexThread, Option<CodexNotice>) {
    match session::verify_codex_rollout(sessions, thread, working_dir) {
        Ok(path) => (CodexThread::Root { created: fs::metadata(path).and_then(|metadata| metadata.created()).ok() }, None),
        Err(error) if error.is::<session::CodexRolloutMissing>() || error.is::<session::CodexRolloutIncomplete>() => {
            (CodexThread::Unwritten, None)
        }
        Err(error) if error.is::<session::CodexRolloutHeaderTooLarge>() => (CodexThread::Rejected, Some(CodexNotice::HeaderTooLarge)),
        Err(_) => (CodexThread::Rejected, None),
    }
}

/// A thread is the member's only when its lock holder is the member's
/// process or its own codex.exe child (pid AND start time) and its rollout
/// is a root CLI thread for the member's folder; a lock-proved thread not yet
/// written waits, and so does a lone root while the process also holds an
/// unwritten thread or the thread this run reported (either may be the one
/// in front). After /new the old thread stays loaded: with more than one held
/// root the member is ambiguous (noted once per distinct set; `poll` holds
/// its Resume) and no ID changes, unless this run's own `--prim1-identify`
/// named one of them after every other thread it holds appeared.
fn codex_step(home: &Path, member: &Member, track: &mut RunTrack, locks: &[HeldLock]) -> (Option<Proof>, Vec<CodexNotice>) {
    let sessions = home.join("sessions");
    let mut notices = Vec::new();
    let own = |holder: &(u32, u64)| *holder == (member.pid, member.started) || member.children.contains(holder);
    let held: Vec<&str> = locks.iter().filter(|lock| lock.holders.iter().any(own))
        .map(|lock| lock.thread.as_str()).collect();
    track.codex_threads.retain(|thread, _| held.contains(&thread.as_str()));
    // A new report re-verifies its thread whatever was cached: identify had
    // just found its root rollout, so an older verdict is stale.
    let report = member.reported.as_ref();
    let forced = report.filter(|report| track.report_checked.as_ref() != Some(*report)).map(|report| report.thread.as_str());
    track.report_checked = report.cloned();
    let now = SystemTime::now();
    for thread in &held {
        let seen = track.codex_threads.get(*thread).copied();
        let recheck = forced == Some(*thread) || match seen.map(|seen| (seen.verdict, seen.checked)) {
            Some((CodexThread::Root { .. }, _)) => false,
            Some((CodexThread::Unwritten, checked)) => checked.elapsed() >= CODEX_UNWRITTEN_RECHECK,
            Some((CodexThread::Rejected, checked)) => checked.elapsed() >= CODEX_REJECTED_RECHECK,
            None => true,
        };
        if recheck {
            let (verdict, notice) = codex_verdict(&sessions, thread, &member.working_dir);
            if notice == Some(CodexNotice::HeaderTooLarge) && !std::mem::replace(&mut track.header_too_large_logged, true) {
                notices.push(CodexNotice::HeaderTooLarge);
            }
            let first_held = seen.map_or(now, |seen| seen.first_held);
            track.codex_threads.insert((*thread).to_owned(), HeldThread { verdict, checked: Instant::now(), first_held });
        }
    }
    let threads = &track.codex_threads;
    let roots: Vec<(&str, Option<SystemTime>)> = held.iter().filter_map(|thread| match threads[*thread].verdict {
        CodexThread::Root { created } => Some((*thread, created)),
        _ => None,
    }).collect();
    let unwritten = held.iter().any(|thread| threads[*thread].verdict == CodexThread::Unwritten);
    // A report names the front thread only if made after this process took
    // every other thread it holds that is or may become a root.
    let named = report.filter(|report| report.at.is_some_and(|at| held.iter().copied()
        .filter(|thread| *thread != report.thread && threads[*thread].verdict != CodexThread::Rejected)
        .all(|thread| threads[thread].first_held + CLOCK_SLACK < at)))
        .and_then(|report| roots.iter().find(|(thread, _)| *thread == report.thread).copied());
    let mut ids: Vec<String> = roots.iter().map(|(thread, _)| (*thread).to_owned()).collect();
    ids.sort();
    let ((thread, created), via, waits) = match (roots.as_slice(), named) {
        ([], _) => return (None, notices),
        ([only], Some(_)) => (*only, "writer_lock", false),
        (_, Some(named)) => (named, "codex_identify", false),
        // A lone root waits while the process also holds an unwritten thread
        // or the thread this run reported: either may be the one in front.
        ([only], None) => (*only, "writer_lock", unwritten || report.is_some_and(|report|
            report.thread != only.0 && held.contains(&report.thread.as_str()))),
        (_, None) => {
            // Nothing opened since the current ID was last proved exact.
            if track.confirmed.as_ref().is_some_and(|(id, _)| ids.contains(id))
                && ids.iter().all(|id| track.exact_roots.contains(id))
            {
                track.exact_roots = ids;
            } else if track.ambiguous.as_ref() != Some(&ids) {
                track.ambiguous = Some(ids);
                notices.push(CodexNotice::Ambiguous);
            }
            return (None, notices);
        }
    };
    track.ambiguous = None;
    let current = member.current.as_deref().map(conversation_key);
    if current.as_deref() == Some(thread) && !member.codex_hold {
        if track.confirmed.as_ref().is_none_or(|(id, _)| id != thread) {
            track.confirm(thread);
        }
        track.exact_roots = ids;
        return (None, notices);
    }
    if waits {
        return (None, notices);
    }
    track.proposed_roots = ids;
    let reason = match &track.confirmed {
        None => "launch",
        Some((id, _)) if id == thread => "launch",
        Some((_, at)) if created.is_some_and(|created| created + CLOCK_SLACK < *at) => "resume",
        Some(_) => "new",
    };
    (Some(Proof { id: thread.to_owned(), reason, via, title: None }), notices)
}

/// Grok's `session created` / `session loaded` lines for the member's pid,
/// confirmed by that session's own summary: same ID, same folder, and no
/// `session_kind` (a subagent or headless run is never the member's).
/// A line is this process's only when written at or after its creation (an
/// earlier one is another process's that had the pid): no slack before it,
/// only the start floored to the log's milliseconds. A proven line stays
/// pending until `poll` has applied its change.
fn grok_step(home: &Path, member: &Member, track: &mut RunTrack, log: &GrokLog) -> Option<Proof> {
    // The FILETIME epoch is a whole number of milliseconds before Unix's.
    let start = filetime_to_system(member.started / FILETIME_TICKS_PER_MS * FILETIME_TICKS_PER_MS)?;
    for event in log.events.iter().filter(|event| event.seq >= track.grok_next) {
        if event.pid == member.pid && event.at >= start {
            track.grok_pending.push((event.clone(), Instant::now()));
        }
    }
    track.grok_next = log.next_seq;
    track.grok_proposed = None;
    let mut proven: Option<GrokEvent> = None;
    track.grok_pending.retain(|(event, seen)| match grok_summary_matches(home, &member.working_dir, &event.sid) {
        Some(true) => {
            if proven.as_ref().is_none_or(|proven| proven.seq < event.seq) {
                proven = Some(event.clone());
            }
            seen.elapsed() < GROK_PENDING_MAX_AGE
        }
        Some(false) => false,
        None => seen.elapsed() < GROK_PENDING_MAX_AGE,
    });
    let proven = proven?;
    if member.current.as_deref().map(conversation_key).as_deref() == Some(proven.sid.as_str()) {
        track.grok_pending.retain(|(event, _)| event.seq > proven.seq);
        track.confirm(&proven.sid);
        return None;
    }
    track.grok_proposed = Some(proven.seq);
    Some(Proof {
        id: proven.sid,
        reason: if proven.created { "new" } else { "resume" },
        via: "grok_log",
        title: None,
    })
}

/// None while the summary is not written (pending); Some(false) for a
/// session that is not the member's.
fn grok_summary_matches(home: &Path, working_dir: &str, sid: &str) -> Option<bool> {
    let sessions = home.join("sessions");
    let direct = sessions.join(grok_folder_name(working_dir)).join(sid).join("summary.json");
    let path = if direct.is_file() {
        direct
    } else {
        // The same folder spelled differently (drive-letter case).
        let wanted = session::normalize_windows_path(working_dir);
        fs::read_dir(&sessions).ok()?.flatten().take(DIR_SCAN_MAX)
            .filter(|entry| entry.file_name().to_str().and_then(percent_decode)
                .is_some_and(|folder| session::normalize_windows_path(&folder) == wanted))
            .map(|entry| entry.path().join(sid).join("summary.json"))
            .find(|path| path.is_file())?
    };
    let value: serde_json::Value = serde_json::from_slice(&read_bounded(&path, SUMMARY_MAX_BYTES).ok()?).ok()?;
    Some(value["info"]["id"].as_str() == Some(sid)
        && value["info"]["cwd"].as_str().is_some_and(|cwd| {
            session::normalize_windows_path(cwd) == session::normalize_windows_path(working_dir)
        })
        && value.get("session_kind").is_none_or(serde_json::Value::is_null))
}

/// Grok's session folder: the cwd with every byte outside A-Z a-z 0-9 - . _ ~
/// percent-encoded (`D%3A%5CAll%20Repos%5Cvictor`).
fn grok_folder_name(working_dir: &str) -> String {
    working_dir.bytes().map(|byte| if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
        char::from(byte).to_string()
    } else {
        format!("%{byte:02X}")
    }).collect()
}

fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = std::str::from_utf8(bytes.get(index + 1..index + 3)?).ok()?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        bail!("{} exceeds its size limit", path.display());
    }
    Ok(bytes)
}

/// Canonical lower-case UUID text; anything else (paths, nil) is refused.
fn canonical_uuid(text: &str) -> Option<String> {
    Uuid::parse_str(text).ok().filter(|id| !id.is_nil()).map(|id| id.to_string())
}

/// UUID spelling is compared normalized; any other ID is compared as stored.
fn conversation_key(id: &str) -> String {
    canonical_uuid(id).unwrap_or_else(|| id.to_owned())
}

fn filetime_to_system(filetime: u64) -> Option<SystemTime> {
    let ticks = filetime.checked_sub(FILETIME_UNIX_EPOCH)?;
    UNIX_EPOCH.checked_add(Duration::from_nanos(ticks.checked_mul(100)?))
}

impl SupervisorHandle {
    /// Starts the follower thread; it stops on shutdown.
    pub(crate) fn start_identity_tracker(&self, mut tracker: IdentityTracker) {
        let weak = Arc::downgrade(&self.inner);
        let control = Arc::clone(&self.inner.identity_tracker);
        let spawned = thread::Builder::new()
            .name("prim1-identity-tracker".into())
            .spawn(move || {
                tracker.prime();
                while !control.wait(TRACK_POLL) {
                    let Some(inner) = weak.upgrade() else { break };
                    if inner.shutdown_started.load(Ordering::Acquire) {
                        break;
                    }
                    tracker.poll(&SupervisorHandle { inner });
                }
            });
        match spawned {
            Ok(thread) => *self.inner.identity_tracker.thread.lock() = Some(thread),
            Err(error) => self.emit(RuntimeEvent::SystemLog {
                level: LogLevel::Warn,
                message: format!("conversation follower could not start: {error}; conversation changes inside panes are not followed"),
                timestamp: now_rfc3339(),
            }),
        }
    }

    /// Running Claude, Codex and Grok members with their PTY, read under the
    /// registry lock only.
    fn tracked_members(&self) -> Vec<(Member, Arc<dyn PtySession>)> {
        let slots = self.inner.slots.lock();
        slots.ordered_slots().filter_map(|slot| {
            if !matches!(slot.definition.driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok) {
                return None;
            }
            // v3 W4: an App Server member's owner holds the thread lock and
            // its observer binds the thread from protocol facts; the lock
            // proof is not used for it.
            if slot.definition.codex_transport == shared_types::CodexTransport::AppServer {
                return None;
            }
            let pty = slot.running.as_ref()?.pty.clone()?;
            let member = Member {
                session_id: slot.session_id,
                run_id: slot.run_id?,
                generation: slot.generation,
                driver: slot.definition.driver,
                label: slot.definition.label.clone(),
                working_dir: slot.definition.working_dir.clone(),
                pid: slot.process_id?,
                started: 0,
                children: Vec::new(),
                launched_at: slot.run_started_at,
                current: slot.harness_session_id.clone(),
                codex_hold: slot.definition.driver == DriverKind::Codex
                    && slot.recovery.as_ref().is_some_and(releasable_codex_hold),
                reported: same_run_report(slot).map(|evidence| Report {
                    thread: conversation_key(&evidence.thread_id),
                    at: DateTime::parse_from_rfc3339(&evidence.reported_at).ok().map(SystemTime::from),
                }),
            };
            Some((member, pty))
        }).collect()
    }

    /// Binds a follower's proven conversation to the run that proved it. The
    /// run is re-validated under the registry lock and the catalog persisted
    /// before the slot changes; the register hears of it in catalog-write
    /// order. A Codex writer-lock proof (or this run's identify report among
    /// its held roots) releases only the launch, shell-report and ambiguity
    /// holds, so Launch resumes it; any other hold refuses. `seen_report` is
    /// this run's identify report the proof was read with; a different one
    /// now means TrackedRunGone and the next pass decides again. A title
    /// (first seen) or rename changes no identity and is true only while its
    /// ID is the one followed. True when anything changed.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn apply_tracked_conversation(
        &self,
        session_id: SessionId,
        run_id: Uuid,
        generation: SessionGeneration,
        new_id: &str,
        reason: &'static str,
        via: &'static str,
        pid: u32,
        seen_report: Option<&str>,
    ) -> Result<bool> {
        self.ensure_active()?;
        let new_id = canonical_uuid(new_id).ok_or_else(|| anyhow!("a followed conversation ID must be a UUID"))?;
        let mut slots = self.inner.slots.lock();
        let slot = slots.get_by_id(session_id).ok_or(TrackedRunGone)?;
        let driver = slot.definition.driver;
        if !matches!(driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok)
            || slot.running.is_none()
            || slot.run_id != Some(run_id)
            || slot.generation != generation
            || slot.process_id != Some(pid)
        {
            return Err(TrackedRunGone.into());
        }
        let report = same_run_report(slot).map(|evidence| conversation_key(&evidence.thread_id));
        if report.as_deref() != seen_report.map(conversation_key).as_deref() {
            return Err(TrackedRunGone.into());
        }
        if via == "codex_identify" && report.as_deref() != Some(new_id.as_str()) {
            bail!("an identify proof must name this run's reported thread");
        }
        let label = slot.definition.label.clone();
        let changed = slot.harness_session_id.as_deref().map(conversation_key).as_deref() != Some(new_id.as_str());
        let note = |handle: &Self| handle.note_conversation_change(ConversationChange {
            session_id, harness: driver, conversation_id: new_id.clone(), reason, via,
            run_id: Some(run_id), pid: Some(pid),
        });
        if matches!(reason, "rename" | "title") {
            // The title belongs to a conversation not followed yet.
            if changed {
                return Ok(false);
            }
            drop(slots);
            if reason == "rename" {
                self.emit(RuntimeEvent::SystemLog {
                    level: LogLevel::Info,
                    message: format!("{label}: conversation renamed (proved by {via}); run_id={run_id}"),
                    timestamp: now_rfc3339(),
                });
                note(self);
            }
            return Ok(true);
        }
        let release = match slot.recovery.as_ref() {
            None => false,
            // v3 W4: the App Server owner's protocol proof is as exact as a
            // writer lock (one dedicated owner per member, decision 2).
            Some(hold) if driver == DriverKind::Codex && matches!(via, "writer_lock" | "codex_identify" | "app_server")
                && releasable_codex_hold(hold) => true,
            Some(hold) => bail!("{label} is held ({}); its followed conversation is not applied", hold.provenance),
        };
        if !changed && !release {
            return Ok(false);
        }
        let names = |id: Option<&str>| id.map(conversation_key).as_deref() == Some(new_id.as_str());
        if changed && let Some(owner) = slots.ordered_slots().find(|other| other.session_id != session_id
            && other.definition.driver == driver
            && (names(other.harness_session_id.as_deref())
                || names(other.recovery.as_ref().and_then(|hold| hold.original_id.as_deref()))
                || names(other.recovery.as_ref().and_then(|hold| hold.reported_codex.as_ref())
                    .map(|evidence| evidence.thread_id.as_str()))))
        {
            bail!("\"{}\" in this PRIM-1 already uses this conversation; it was not bound twice", owner.definition.label);
        }
        let mut catalog = self.inner.catalog.lock();
        if changed && catalog.sessions.iter().any(|row| row.session_id != session_id && row.driver == driver
            && names(row.harness_session_id.as_deref()))
        {
            bail!("another catalog session already uses this conversation; it was not bound twice");
        }
        // A plain launch or ambiguity hold is superseded by the proof itself;
        // only a hold carrying identify evidence is kept in history.
        let mut recovery_history = slot.recovery_history.clone();
        if release && let Some(previous) = slot.recovery.clone()
            && (!matches!(previous.provenance.as_str(), "codex_launch" | CODEX_AMBIGUOUS) || previous.reported_codex.is_some())
            && !recovery_history.contains(&previous)
        {
            recovery_history.push(previous);
        }
        let mut candidate = catalog.clone();
        let row = candidate.sessions.iter_mut().find(|row| row.session_id == session_id)
            .ok_or_else(|| anyhow!("session is missing from the catalog"))?;
        row.harness_session_id = Some(new_id.clone());
        if release {
            row.recovery = None;
            row.recovery_history = recovery_history.clone();
        }
        recovery_automation::bind_verified_identity(&mut candidate, session_id, run_id, generation, &new_id);
        self.persist_catalog_candidate(&candidate)?;
        let slot = slots.get_by_id_mut(session_id).expect("validated above");
        slot.harness_session_id = Some(new_id.clone());
        if release {
            slot.recovery = None;
            slot.recovery_history = recovery_history;
        }
        *catalog = candidate;
        // Under the catalog lock, so the register hears in catalog-write order.
        note(self);
        drop(catalog);
        drop(slots);
        self.emit(RuntimeEvent::SystemLog {
            level: LogLevel::Info,
            message: format!("{label}: current conversation followed ({reason}, proved by {via}){}; run_id={run_id}",
                if release { "; Launch resumes it" } else { "" }),
            timestamp: now_rfc3339(),
        });
        Ok(true)
    }

    /// Codex holds more than one root conversation, so the ID Launch would
    /// resume may be stale: the run's member is held (`codex_ambiguous`, the
    /// catalog persisted before the slot changes) until a writer-lock proof
    /// or this run's identify report releases it. Only an exact member or a
    /// launch or report hold (whose ID Launch would resume) is replaced; any
    /// other hold already refuses. `seen_report` as for
    /// `apply_tracked_conversation`: a report that landed since is decided on
    /// the next pass. True when the hold was placed now.
    pub(crate) fn hold_ambiguous_codex(
        &self,
        session_id: SessionId,
        run_id: Uuid,
        generation: SessionGeneration,
        pid: u32,
        seen_report: Option<&str>,
    ) -> Result<bool> {
        self.ensure_active()?;
        let mut slots = self.inner.slots.lock();
        let slot = slots.get_by_id(session_id).ok_or(TrackedRunGone)?;
        if slot.definition.driver != DriverKind::Codex
            || slot.running.is_none()
            || slot.run_id != Some(run_id)
            || slot.generation != generation
            || slot.process_id != Some(pid)
            || same_run_report(slot).map(|evidence| conversation_key(&evidence.thread_id)) != seen_report.map(conversation_key)
        {
            return Err(TrackedRunGone.into());
        }
        if slot.recovery.as_ref().is_some_and(|hold| hold.provenance == CODEX_AMBIGUOUS || !releasable_codex_hold(hold)) {
            return Ok(false);
        }
        let hold = RecoveryReference {
            original_id: slot.harness_session_id.clone(),
            provenance: CODEX_AMBIGUOUS.into(),
            reason: "Codex holds more than one root conversation, so the current one is not proved and the one recorded may be stale. Resume remains held; nothing was guessed. It becomes exact when the agent runs --prim1-identify or the older conversation is released.".into(),
            reported_codex: None,
        };
        // A report is evidence, kept in history; a plain launch hold is superseded.
        let mut recovery_history = slot.recovery_history.clone();
        if let Some(previous) = slot.recovery.clone()
            && previous.reported_codex.is_some()
            && !recovery_history.contains(&previous)
        {
            recovery_history.push(previous);
        }
        let mut catalog = self.inner.catalog.lock();
        let mut candidate = catalog.clone();
        let row = candidate.sessions.iter_mut().find(|row| row.session_id == session_id)
            .ok_or_else(|| anyhow!("session is missing from the catalog"))?;
        row.recovery = Some(hold.clone());
        row.recovery_history = recovery_history.clone();
        self.persist_catalog_candidate(&candidate)?;
        let slot = slots.get_by_id_mut(session_id).expect("validated above");
        slot.recovery = Some(hold);
        slot.recovery_history = recovery_history;
        *catalog = candidate;
        Ok(true)
    }
}

fn releasable_codex_hold(hold: &RecoveryReference) -> bool {
    matches!(hold.provenance.as_str(), "codex_launch" | "job_attested_codex_shell_report" | CODEX_AMBIGUOUS)
}

/// This run's own `--prim1-identify` evidence; an earlier run's is history.
pub(crate) fn same_run_report(slot: &SessionSlot) -> Option<&session::CodexIdentityEvidence> {
    slot.recovery.as_ref().and_then(|hold| hold.reported_codex.as_ref())
        .filter(|evidence| slot.run_id == Some(evidence.run_id) && slot.generation == evidence.generation)
}

#[cfg(test)]
mod tests {
    use super::*;

    const START: u64 = 134_368_777_644_688_307;

    fn member(driver: DriverKind, working_dir: &str) -> Member {
        Member {
            session_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            generation: 1,
            driver,
            label: "member".into(),
            working_dir: working_dir.into(),
            pid: 4242,
            started: START,
            children: Vec::new(),
            launched_at: None,
            current: None,
            codex_hold: false,
            reported: None,
        }
    }

    fn write(path: &Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn claude_transcript_lookup_is_absent_only_after_a_complete_clean_scan() {
        let id = "0f1c7f0e-5a52-4b8e-9d3e-2f6a1b9c8d70";
        let wd = r"C:\Users\me\victor";
        // No projects folder at all, then an empty one: definitely absent.
        let home = tempfile::tempdir().unwrap();
        assert_eq!(claude_transcript_lookup(home.path(), wd, id), TranscriptLookup::Absent);
        fs::create_dir_all(home.path().join(r"projects\C--Users-me-other")).unwrap();
        assert_eq!(claude_transcript_lookup(home.path(), wd, id), TranscriptLookup::Absent);
        // Found in the cwd folder, and in any other project folder (hashed long paths).
        write(&home.path().join(r"projects\C--Users-me-other").join(format!("{id}.jsonl")), "{}\n");
        assert_eq!(claude_transcript_lookup(home.path(), wd, id), TranscriptLookup::Found);
        write(&home.path().join(r"projects\C--Users-me-victor").join(format!("{id}.jsonl")), "{}\n");
        assert_eq!(claude_transcript_lookup(home.path(), wd, id), TranscriptLookup::Found);
        // A name that is not a plain file is not proof of absence.
        let odd = tempfile::tempdir().unwrap();
        fs::create_dir_all(odd.path().join(r"projects\C--Users-me-victor").join(format!("{id}.jsonl"))).unwrap();
        assert_eq!(claude_transcript_lookup(odd.path(), wd, id), TranscriptLookup::Unknown);
        // An unreadable projects folder (here a file) is unknown, not absent.
        let broken = tempfile::tempdir().unwrap();
        write(&broken.path().join("projects"), "not a folder");
        assert_eq!(claude_transcript_lookup(broken.path(), wd, id), TranscriptLookup::Unknown);
        // A scan cut short by the folder bound is unknown, not absent.
        let many = tempfile::tempdir().unwrap();
        for name in ["a", "b", "c"] {
            fs::create_dir_all(many.path().join("projects").join(name)).unwrap();
        }
        assert_eq!(claude_transcript_lookup_within(many.path(), wd, id, 2), TranscriptLookup::Unknown);
        assert_eq!(claude_transcript_lookup_within(many.path(), wd, id, 3), TranscriptLookup::Absent);
    }

    fn claude_pid_file(home: &Path, member: &Member, session_id: &str, name: &str, overrides: serde_json::Value) {
        let mut value = serde_json::json!({
            "pid": member.pid, "sessionId": session_id, "cwd": member.working_dir,
            "procStart": START.to_string(), "kind": "interactive", "pidDomain": "win32:testpc",
            "name": name, "status": "idle",
        });
        for (key, field) in overrides.as_object().unwrap() {
            value[key] = field.clone();
        }
        write(&home.join("sessions").join(format!("{}.json", member.pid)), &value.to_string());
    }

    #[test]
    fn claude_pid_file_is_accepted_only_for_this_exact_process_machine_and_folder() {
        let home = tempfile::tempdir().unwrap();
        let member = member(DriverKind::Claude, r"D:\All Repos\victor");
        let id = Uuid::new_v4().to_string();
        claude_pid_file(home.path(), &member, &id, "session-a", serde_json::json!({}));
        assert_eq!(read_claude_pid_file(home.path(), "win32:testpc", &member), Some((id.clone(), Some("session-a".into()))));
        // A numeric procStart is the same FILETIME.
        claude_pid_file(home.path(), &member, &id, "session-a", serde_json::json!({"procStart": START}));
        assert!(read_claude_pid_file(home.path(), "win32:testpc", &member).is_some());
        for (field, value) in [
            // PID reuse: another process once had this pid; exact, either side.
            ("procStart", serde_json::json!((START + 1).to_string())),
            ("procStart", serde_json::json!((START - 1).to_string())),
            // A stale file of another machine (before a reinstall).
            ("pidDomain", serde_json::json!("win32:desktop-j00kcre")),
            ("cwd", serde_json::json!(r"D:\elsewhere")),
            ("kind", serde_json::json!("bg")),
            ("pid", serde_json::json!(member.pid + 1)),
            ("sessionId", serde_json::json!("../../escape")),
        ] {
            claude_pid_file(home.path(), &member, &id, "session-a", serde_json::json!({ field: value }));
            assert_eq!(read_claude_pid_file(home.path(), "win32:testpc", &member), None, "accepted {field}");
        }
        fs::remove_file(home.path().join("sessions").join("4242.json")).unwrap();
        assert_eq!(read_claude_pid_file(home.path(), "win32:testpc", &member), None);
    }

    #[test]
    fn claude_follows_clear_resume_fork_and_rename_once_each_transcript_exists() {
        let home = tempfile::tempdir().unwrap();
        let mut member = member(DriverKind::Claude, r"D:\All Repos\victor");
        let first = Uuid::new_v4().to_string();
        member.current = Some(first.clone());
        let mut track = RunTrack::new(&member);
        claude_pid_file(home.path(), &member, &first, "session-a", serde_json::json!({}));
        // The followed ID's title, first seen, goes to the register.
        let title = |id: &str, reason, name: &str| Proof { id: id.into(), reason, via: "pid_file", title: Some(name.into()) };
        assert_eq!(claude_step(home.path(), "win32:testpc", &member, &mut track), vec![title(&first, "title", "session-a")]);
        assert_eq!(track.confirmed.as_ref().map(|(id, _)| id.as_str()), Some(first.as_str()));
        track.claude_name = Some("session-a".into());
        // /clear: a new ID is pending until its transcript exists.
        let cleared = Uuid::new_v4().to_string();
        claude_pid_file(home.path(), &member, &cleared, "session-a", serde_json::json!({}));
        assert!(claude_step(home.path(), "win32:testpc", &member, &mut track).is_empty());
        let projects = home.path().join("projects").join("D--All-Repos-victor");
        write(&projects.join(format!("{cleared}.jsonl")), "{\"type\":\"user\"}\n");
        assert_eq!(claude_step(home.path(), "win32:testpc", &member, &mut track),
            vec![Proof { id: cleared.clone(), reason: "clear", via: "pid_file", title: None }]);
        // An in-session /resume of a conversation written before.
        let older = Uuid::new_v4().to_string();
        let transcript = projects.join(format!("{older}.jsonl"));
        write(&transcript, "{\"type\":\"user\"}\n");
        track.seen_at = Some(SystemTime::now() + Duration::from_secs(60));
        claude_pid_file(home.path(), &member, &older, "session-a", serde_json::json!({}));
        assert_eq!(claude_step(home.path(), "win32:testpc", &member, &mut track)[0].reason, "resume");
        // A fork, found in a project folder named differently.
        let forked = Uuid::new_v4().to_string();
        write(&home.path().join("projects").join("hashed-long-path").join(format!("{forked}.jsonl")),
            "{\"type\":\"user\",\"forkedFrom\":{\"sessionId\":\"x\",\"messageUuid\":\"y\"}}\n");
        claude_pid_file(home.path(), &member, &forked, "session-a", serde_json::json!({}));
        assert_eq!(claude_step(home.path(), "win32:testpc", &member, &mut track)[0].reason, "fork");
        // A rename is a title, never a change of ID; offered until applied,
        // then once only.
        claude_pid_file(home.path(), &member, &first, "courses", serde_json::json!({}));
        for _ in 0..2 {
            assert_eq!(claude_step(home.path(), "win32:testpc", &member, &mut track), vec![title(&first, "rename", "courses")]);
        }
        track.claude_name = Some("courses".into());
        assert!(claude_step(home.path(), "win32:testpc", &member, &mut track).is_empty());
    }

    #[test]
    fn claude_change_found_before_the_current_id_was_read_is_observed() {
        let home = tempfile::tempdir().unwrap();
        let mut member = member(DriverKind::Claude, r"D:\All Repos\victor");
        member.current = Some(Uuid::new_v4().to_string());
        let mut track = RunTrack::new(&member);
        let other = Uuid::new_v4().to_string();
        write(&home.path().join("projects").join("D--All-Repos-victor").join(format!("{other}.jsonl")), "{}\n");
        claude_pid_file(home.path(), &member, &other, "session-a", serde_json::json!({}));
        let proofs = claude_step(home.path(), "win32:testpc", &member, &mut track);
        assert_eq!((proofs[0].id.as_str(), proofs[0].reason), (other.as_str(), "observed"));
    }

    /// Lock holders by thread; a thread mapped to None cannot be asked.
    /// Counts every query, so tests can prove unchanged locks are not asked.
    #[derive(Default)]
    struct FakeHolders(HashMap<String, Option<Vec<(u32, u64)>>>, std::sync::atomic::AtomicUsize);

    impl FakeHolders {
        fn of(entries: impl IntoIterator<Item = (String, Option<Vec<(u32, u64)>>)>) -> Self {
            Self(entries.into_iter().collect(), Default::default())
        }

        fn asked(&self) -> usize {
            self.1.swap(0, std::sync::atomic::Ordering::Relaxed)
        }
    }

    impl LockHolders for FakeHolders {
        fn holders(&self, path: &Path) -> Result<Vec<(u32, u64)>> {
            self.1.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let name = path.file_name().unwrap().to_str().unwrap();
            match self.0.get(name.trim_end_matches(".lock")) {
                Some(Some(holders)) => Ok(holders.clone()),
                Some(None) => bail!("holders could not be listed"),
                None => Ok(Vec::new()),
            }
        }
    }

    /// Every lock asked afresh, as the first pass of a tracker does.
    fn fresh_locks(home: &Path, holders: &FakeHolders) -> Option<Vec<HeldLock>> {
        codex_held_locks(home, holders, &mut HashMap::new(), CODEX_LOCK_REFRESH)
    }

    fn codex_lock(home: &Path, thread: &str) {
        write(&home.join("thread-writer-locks").join(format!("{thread}.lock")), "");
    }

    fn codex_rollout(home: &Path, thread: &str, cwd: &str, source: &str) -> PathBuf {
        let path = session::write_codex_fixture(&home.join("sessions"), thread, cwd);
        let mut header: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        header["payload"]["source"] = serde_json::json!(source);
        fs::write(&path, format!("{header}\n")).unwrap();
        path
    }

    fn thread() -> String {
        // Codex threads are v7; any canonical UUID is followed the same way.
        Uuid::new_v4().to_string()
    }

    fn report(thread: &str, at: SystemTime) -> Option<Report> {
        Some(Report { thread: conversation_key(thread), at: Some(at) })
    }

    /// Makes a cached verdict due for its recheck.
    fn age(track: &mut RunTrack, thread: &str) {
        let seen = track.codex_threads.get_mut(thread).unwrap();
        seen.checked = Instant::now().checked_sub(CODEX_REJECTED_RECHECK + Duration::from_secs(1)).unwrap();
    }

    /// What `poll` does after a successful apply.
    fn applied(member: &mut Member, track: &mut RunTrack, id: &str) {
        track.confirm(id);
        track.exact_roots = std::mem::take(&mut track.proposed_roots);
        member.current = Some(id.into());
        member.codex_hold = false;
        member.reported = None;
    }

    #[test]
    fn codex_follows_only_locks_held_by_the_member_process_and_waits_for_a_root_rollout() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let mut member = member(DriverKind::Codex, cwd);
        assert_eq!(fresh_locks(home.path(), &FakeHolders::default()).unwrap().len(), 0, "no lock folder yet");
        let (root, subagent, fresh, reused, unheld) = (thread(), thread(), thread(), thread(), thread());
        for thread in [&root, &subagent, &fresh, &reused, &unheld] {
            codex_lock(home.path(), thread);
        }
        write(&home.path().join("thread-writer-locks").join(".coordination.lock"), "");
        codex_rollout(home.path(), &root, cwd, "cli");
        codex_rollout(home.path(), &subagent, cwd, "vscode");
        codex_rollout(home.path(), &reused, cwd, "cli");
        codex_rollout(home.path(), &unheld, cwd, "cli");
        let mut holders = FakeHolders::of([
            (root.clone(), Some(vec![(member.pid, START)])),
            (subagent.clone(), Some(vec![(member.pid, START)])),
            (fresh.clone(), Some(vec![(member.pid, START)])),
            // Same pid, another process: PID reuse is never the member.
            (reused.clone(), Some(vec![(member.pid, START + 10)])),
        ]);
        let locks = fresh_locks(home.path(), &holders).unwrap();
        assert_eq!(locks.len(), 4);
        let mut track = RunTrack::new(&member);
        // The lone root waits while a fresh thread is unwritten: it may be
        // the one in front.
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        assert_eq!(track.codex_threads[&fresh].verdict, CodexThread::Unwritten);
        assert_eq!(track.codex_threads[&subagent].verdict, CodexThread::Rejected);
        assert!(!track.codex_threads.contains_key(&reused));
        // Its rollout is still being written: still unwritten, not rejected.
        let partial = codex_rollout(home.path(), &fresh, cwd, "vscode");
        fs::write(&partial, "{\"type\":\"session_meta\",\"payload\":{\"id\":").unwrap();
        age(&mut track, &fresh);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        assert_eq!(track.codex_threads[&fresh].verdict, CodexThread::Unwritten);
        // Written as a subagent: the root is the member's.
        codex_rollout(home.path(), &fresh, cwd, "vscode");
        age(&mut track, &fresh);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks),
            (Some(Proof { id: root.clone(), reason: "launch", via: "writer_lock", title: None }), vec![]));
        assert_eq!(track.codex_threads[&fresh].verdict, CodexThread::Rejected);
        applied(&mut member, &mut track, &root);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        // A launch hold on the same ID is released by the same proof.
        member.codex_hold = true;
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks).0.unwrap().reason, "launch");
        // One lock that cannot be asked: no Codex proof at all this pass.
        holders.0.insert(unheld.clone(), None);
        assert!(fresh_locks(home.path(), &holders).is_none());
    }

    #[test]
    fn codex_locks_are_asked_only_when_listed_new_or_changed_or_due() {
        let home = tempfile::tempdir().unwrap();
        let (first, second) = (thread(), thread());
        codex_lock(home.path(), &first);
        codex_lock(home.path(), &second);
        let holders = FakeHolders::of([(first.clone(), Some(vec![(4242, START)]))]);
        let mut cache = HashMap::new();
        let pass = |cache: &mut HashMap<String, LockEntry>, refresh| {
            let mut threads: Vec<String> = codex_held_locks(home.path(), &holders, cache, refresh).unwrap()
                .into_iter().map(|lock| lock.thread).collect();
            threads.sort();
            threads
        };
        assert_eq!(pass(&mut cache, CODEX_LOCK_REFRESH), vec![first.clone()]);
        assert_eq!(holders.asked(), 2);
        // An unchanged folder asks nothing, pass after pass.
        for _ in 0..5 {
            assert_eq!(pass(&mut cache, CODEX_LOCK_REFRESH), vec![first.clone()]);
        }
        assert_eq!(holders.asked(), 0);
        // A new lock is asked alone; a released (deleted) one is dropped.
        let third = thread();
        codex_lock(home.path(), &third);
        fs::remove_file(home.path().join("thread-writer-locks").join(format!("{second}.lock"))).unwrap();
        pass(&mut cache, CODEX_LOCK_REFRESH);
        assert_eq!(holders.asked(), 1);
        assert!(!cache.contains_key(&second));
        // Taken again: Codex recreated the file, so its listing changed.
        let path = home.path().join("thread-writer-locks").join(format!("{first}.lock"));
        File::options().write(true).open(&path).unwrap()
            .set_modified(SystemTime::now() + Duration::from_secs(5)).unwrap();
        pass(&mut cache, CODEX_LOCK_REFRESH);
        assert_eq!(holders.asked(), 1);
        // The slow refresh asks every lock again (a crashed holder's file
        // can be reused unchanged), a bounded number per pass.
        pass(&mut cache, Duration::ZERO);
        assert_eq!(holders.asked(), 2);
        for _ in 0..CODEX_REFRESHES_PER_PASS {
            codex_lock(home.path(), &thread());
        }
        pass(&mut cache, Duration::ZERO);
        assert_eq!(holders.asked(), CODEX_REFRESHES_PER_PASS + 2, "every new lock is asked");
        pass(&mut cache, Duration::ZERO);
        assert_eq!(holders.asked(), CODEX_REFRESHES_PER_PASS, "the refresh of 18 due locks is spread");
    }

    #[test]
    fn codex_two_held_roots_are_ambiguous_until_identify_or_release() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let mut member = member(DriverKind::Codex, cwd);
        let (old, new, third) = (thread(), thread(), thread());
        for thread in [&old, &new, &third] {
            codex_lock(home.path(), thread);
            codex_rollout(home.path(), thread, cwd, "cli");
        }
        let held = |threads: &[&String]| FakeHolders::of(threads.iter()
            .map(|thread| ((*thread).clone(), Some(vec![(4242, START)]))));
        let mut track = RunTrack::new(&member);
        let locks = fresh_locks(home.path(), &held(&[&old])).unwrap();
        let proof = codex_step(home.path(), &member, &mut track, &locks).0.unwrap();
        applied(&mut member, &mut track, &proof.id);
        // /new keeps the old thread loaded: two roots, no newest-file choice,
        // even with the new lock written later; noted once.
        let both = fresh_locks(home.path(), &held(&[&old, &new])).unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &both), (None, vec![CodexNotice::Ambiguous]));
        assert_eq!(codex_step(home.path(), &member, &mut track, &both), (None, vec![]));
        assert_eq!(member.current.as_deref(), Some(old.as_str()));
        // The agent's --prim1-identify, run after /new, names a held root.
        member.reported = report(&new.to_uppercase(), SystemTime::now() + Duration::from_secs(5));
        member.current = Some(new.clone());
        member.codex_hold = true;
        let proof = codex_step(home.path(), &member, &mut track, &both).0.unwrap();
        assert_eq!((proof.id.as_str(), proof.via), (new.as_str(), "codex_identify"));
        applied(&mut member, &mut track, &new);
        // Still exact while nothing new is opened; a third root is not.
        assert_eq!(codex_step(home.path(), &member, &mut track, &both), (None, vec![]));
        let three = fresh_locks(home.path(), &held(&[&old, &new, &third])).unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &three), (None, vec![CodexNotice::Ambiguous]));
        // A report naming a thread this process does not hold is ignored.
        member.reported = report(&thread(), SystemTime::now() + Duration::from_secs(5));
        assert_eq!(codex_step(home.path(), &member, &mut track, &three), (None, vec![]));
        // The older roots are released: the one left is exact.
        member.reported = None;
        let only = fresh_locks(home.path(), &held(&[&third])).unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &only),
            (Some(Proof { id: third.clone(), reason: "new", via: "writer_lock", title: None }), vec![]));
    }

    #[test]
    fn codex_report_made_before_another_root_appeared_stays_ambiguous() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let mut member = member(DriverKind::Codex, cwd);
        let (old, new) = (thread(), thread());
        for thread in [&old, &new] {
            codex_lock(home.path(), thread);
            codex_rollout(home.path(), thread, cwd, "cli");
        }
        let held = |threads: &[&String]| FakeHolders::of(threads.iter()
            .map(|thread| ((*thread).clone(), Some(vec![(4242, START)]))));
        // The run reported `old`, but that report was never applied (a lock
        // could not be read, an apply failed) before /new opened `new`.
        let reported_at = SystemTime::now() - Duration::from_secs(60);
        member.reported = report(&old, reported_at);
        member.current = Some(old.clone());
        member.codex_hold = true;
        let mut track = RunTrack::new(&member);
        let both = fresh_locks(home.path(), &held(&[&old, &new])).unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &both), (None, vec![CodexNotice::Ambiguous]));
        // A report made after both were taken is exact.
        member.reported = report(&new, SystemTime::now() + Duration::from_secs(5));
        assert_eq!(codex_step(home.path(), &member, &mut track, &both).0.unwrap().via, "codex_identify");
    }

    #[test]
    fn codex_lone_root_waits_while_the_reported_thread_is_held_and_rereads_it() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let mut member = member(DriverKind::Codex, cwd);
        let (a, b) = (thread(), thread());
        codex_lock(home.path(), &a);
        codex_lock(home.path(), &b);
        codex_rollout(home.path(), &a, cwd, "cli");
        let mut track = RunTrack::new(&member);
        let locks = fresh_locks(home.path(), &FakeHolders::of([a.clone(), b.clone()].map(|thread|
            (thread, Some(vec![(4242, START)]))))).unwrap();
        // B is held but has no rollout yet; A, current and proven, stays.
        member.current = Some(a.clone());
        track.confirm(&a);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        assert_eq!(track.codex_threads[&b].verdict, CodexThread::Unwritten);
        // B's first line is written; then the agent identifies B (identify
        // sets the slot's ID and a report hold). The cached Unwritten is not
        // due for 6 s, yet the new report re-reads B at once.
        codex_rollout(home.path(), &b, cwd, "cli");
        member.reported = report(&b, SystemTime::now() + Duration::from_secs(5));
        member.current = Some(b.clone());
        member.codex_hold = true;
        let proof = codex_step(home.path(), &member, &mut track, &locks).0.unwrap();
        assert_eq!((proof.id.as_str(), proof.via), (b.as_str(), "codex_identify"));
        // Had B still read as unwritten, the lone root A must wait, never
        // overwrite the report.
        let mut track = RunTrack::new(&member);
        fs::write(codex_rollout(home.path(), &b, cwd, "cli"), "").unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        assert_eq!(track.codex_threads[&b].verdict, CodexThread::Unwritten);
        // Rejected by a mismatch (another folder), B still blocks A while held.
        let mut track = RunTrack::new(&member);
        codex_rollout(home.path(), &b, r"C:\elsewhere", "cli");
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
        assert_eq!(track.codex_threads[&b].verdict, CodexThread::Rejected);
        // B released: the report is stale and the lone root is exact.
        let only_a = fresh_locks(home.path(), &FakeHolders::of([(a.clone(), Some(vec![(4242, START)]))])).unwrap();
        assert_eq!(codex_step(home.path(), &member, &mut track, &only_a).0.unwrap(),
            Proof { id: a.clone(), reason: "launch", via: "writer_lock", title: None });
    }

    #[test]
    fn codex_oversized_header_is_rejected_and_noted_once_per_run() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let member = member(DriverKind::Codex, cwd);
        let big = thread();
        codex_lock(home.path(), &big);
        let path = codex_rollout(home.path(), &big, cwd, "cli");
        let mut header: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        header["payload"]["base_instructions"] = serde_json::json!("x".repeat(session::CODEX_HEADER_LIMIT as usize));
        fs::write(&path, format!("{header}\n")).unwrap();
        let locks = fresh_locks(home.path(), &FakeHolders::of([(big.clone(), Some(vec![(4242, START)]))])).unwrap();
        let mut track = RunTrack::new(&member);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![CodexNotice::HeaderTooLarge]));
        assert_eq!(track.codex_threads[&big].verdict, CodexThread::Rejected);
        age(&mut track, &big);
        assert_eq!(codex_step(home.path(), &member, &mut track, &locks), (None, vec![]));
    }

    fn grok_line(pid: u32, sid: &str, msg: &str, at: SystemTime) -> String {
        let ts = DateTime::<Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        format!("{}\n", serde_json::json!({"ts": ts, "src": "shell", "pid": pid, "lvl": "info", "sid": sid, "msg": msg}))
    }

    fn grok_summary(home: &Path, cwd: &str, sid: &str, kind: Option<&str>) {
        let mut value = serde_json::json!({"info": {"id": sid, "cwd": cwd}, "generated_title": "t"});
        if let Some(kind) = kind {
            value["session_kind"] = serde_json::json!(kind);
        }
        write(&home.join("sessions").join(grok_folder_name(cwd)).join(sid).join("summary.json"), &value.to_string());
    }

    fn append(path: &Path, text: &str) {
        use std::io::Write as _;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        File::options().create(true).append(true).open(path).unwrap().write_all(text.as_bytes()).unwrap();
    }

    #[test]
    fn grok_follows_create_and_load_lines_of_its_pid_confirmed_by_the_summary() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"D:\All Repos\victor";
        assert_eq!(grok_folder_name(cwd), "D%3A%5CAll%20Repos%5Cvictor");
        let log_path = GrokLog::path(home.path());
        let start = filetime_to_system(START).unwrap();
        append(&log_path, &grok_line(4242, &Uuid::new_v4().to_string(), "session created", start));
        let mut log = GrokLog::default();
        log.prime(Some(home.path()));
        let mut member = member(DriverKind::Grok, cwd);
        let minted = Uuid::new_v4().to_string();
        member.current = Some(minted.clone());
        let mut track = RunTrack::new(&member);
        // Launch: the minted ID is created; nothing changes.
        append(&log_path, &grok_line(4242, &minted, "session created", start + Duration::from_secs(2)));
        grok_summary(home.path(), cwd, &minted, None);
        log.read_new(Some(home.path()));
        assert_eq!(log.events.len(), 1, "lines before priming are not read");
        assert_eq!(grok_step(home.path(), &member, &mut track, &log), None);
        // /new: a subagent of the same pid, another pid and an older process
        // with the same pid are ignored; the summary is written late.
        let (new, subagent, other, stale) = (Uuid::new_v4().to_string(), Uuid::new_v4().to_string(),
            Uuid::new_v4().to_string(), Uuid::new_v4().to_string());
        append(&log_path, &grok_line(4242, &new, "session created", start + Duration::from_secs(30)));
        append(&log_path, &grok_line(4242, &subagent, "session created", start + Duration::from_secs(31)));
        append(&log_path, &grok_line(4242, &subagent, "turn.complete", start + Duration::from_secs(32)));
        append(&log_path, &grok_line(9999, &other, "session created", start + Duration::from_secs(33)));
        append(&log_path, &grok_line(4242, &stale, "session loaded", start - Duration::from_secs(60)));
        append(&log_path, "{\"ts\":\"partial");
        grok_summary(home.path(), cwd, &subagent, Some("subagent"));
        grok_summary(home.path(), cwd, &other, None);
        grok_summary(home.path(), cwd, &stale, None);
        log.read_new(Some(home.path()));
        assert_eq!(grok_step(home.path(), &member, &mut track, &log), None, "pending until the summary exists");
        grok_summary(home.path(), cwd, &new, None);
        assert_eq!(grok_step(home.path(), &member, &mut track, &log),
            Some(Proof { id: new.clone(), reason: "new", via: "grok_log", title: None }));
        // Only the proven line is left, until poll has applied it.
        assert_eq!(track.grok_pending.iter().map(|(event, _)| event.sid.as_str()).collect::<Vec<_>>(), [new.as_str()]);
        member.current = Some(new.clone());
        // An in-session resume, its summary in the same folder spelled d:\.
        let resumed = Uuid::new_v4().to_string();
        append(&log_path, &format!("\"}}\n{}", grok_line(4242, &resumed, "session loaded", start + Duration::from_secs(40))));
        write(&home.path().join("sessions").join(grok_folder_name(&cwd.to_lowercase())).join(&resumed).join("summary.json"),
            &serde_json::json!({"info": {"id": resumed, "cwd": cwd.to_lowercase()}}).to_string());
        log.read_new(Some(home.path()));
        assert_eq!(grok_step(home.path(), &member, &mut track, &log),
            Some(Proof { id: resumed, reason: "resume", via: "grok_log", title: None }));
    }

    #[test]
    fn grok_refuses_a_line_written_before_its_process_was_created() {
        let home = tempfile::tempdir().unwrap();
        let cwd = r"C:\work";
        let log_path = GrokLog::path(home.path());
        append(&log_path, "");
        let mut log = GrokLog::default();
        log.prime(Some(home.path()));
        let member = member(DriverKind::Grok, cwd);
        let mut track = RunTrack::new(&member);
        // START is 10:02:44.4688307; the log keeps milliseconds.
        let start = filetime_to_system(START).unwrap();
        let line = |log: &mut GrokLog, at: SystemTime| {
            let sid = Uuid::new_v4().to_string();
            append(&log_path, &grok_line(4242, &sid, "session created", at));
            grok_summary(home.path(), cwd, &sid, None);
            log.read_new(Some(home.path()));
            sid
        };
        // Another process that had this pid wrote these.
        for before in [Duration::from_secs(1), Duration::from_millis(1)] {
            line(&mut log, start - before);
            assert_eq!(grok_step(home.path(), &member, &mut track, &log), None, "{before:?} before");
            assert!(track.grok_pending.is_empty());
        }
        // Its creation millisecond, then later: this process's own.
        for after in [Duration::ZERO, Duration::from_secs(1)] {
            let sid = line(&mut log, start + after);
            assert_eq!(grok_step(home.path(), &member, &mut track, &log).map(|proof| proof.id), Some(sid), "{after:?} after");
        }
    }

    #[test]
    fn grok_log_rereads_a_rotated_file_and_skips_garbage() {
        let home = tempfile::tempdir().unwrap();
        let log_path = GrokLog::path(home.path());
        let start = filetime_to_system(START).unwrap();
        append(&log_path, &"x".repeat(4096));
        let mut log = GrokLog::default();
        log.prime(Some(home.path()));
        fs::write(&log_path, grok_line(1, &Uuid::new_v4().to_string(), "session created", start)).unwrap();
        log.read_new(Some(home.path()));
        assert_eq!(log.events.len(), 1);
        append(&log_path, "not json \"session loaded\"\n{\"msg\":\"session loaded\",\"pid\":1,\"sid\":\"../x\",\"ts\":\"2026-10-19T10:00:00Z\"}\n");
        log.read_new(Some(home.path()));
        assert_eq!(log.events.len(), 1);
        assert_eq!(percent_decode("D%3A%5Cx%20y"), Some(r"D:\x y".into()));
        assert_eq!(percent_decode("bad%zz"), None);
    }

    #[cfg(windows)]
    #[test]
    fn open_file_holders_lists_each_process_holding_the_file_with_its_start_time() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("{}.lock", Uuid::new_v4()));
        let held = File::create(&path).unwrap();
        let me = PinnedProcess::open(std::process::id()).unwrap();
        let holders = OpenFileHolders.holders(&path).unwrap();
        assert!(holders.contains(&(me.pid(), me.creation_time_filetime())), "{holders:?}");
        // Another process holding it (an inherited handle) is listed; this
        // process, its handle closed, is not.
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"])
            .stdout(held.try_clone().unwrap()).stderr(std::process::Stdio::null()).spawn().unwrap();
        drop(held);
        let other = PinnedProcess::open(child.id()).unwrap();
        let holders = OpenFileHolders.holders(&path);
        child.kill().unwrap();
        child.wait().unwrap();
        let holders = holders.unwrap();
        assert!(holders.contains(&(other.pid(), other.creation_time_filetime())), "{holders:?}");
        assert!(!holders.iter().any(|(pid, _)| *pid == me.pid()), "{holders:?}");
        assert_eq!(OpenFileHolders.holders(&path).unwrap(), vec![]);
        // Released and deleted since it was listed: nobody holds it.
        fs::remove_file(&path).unwrap();
        assert_eq!(OpenFileHolders.holders(&path).unwrap(), vec![]);
    }

    #[cfg(windows)]
    #[test]
    fn parent_process_id_names_the_creating_process_and_codex_images_are_matched_by_name() {
        let mut child = std::process::Command::new("ping").args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null()).spawn().unwrap();
        let parent = parent_process_id(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(parent, Some(std::process::id()));
        assert!(is_codex_image(r"C:\Users\me\AppData\Roaming\npm\node_modules\@openai\codex\vendor\x86_64-pc-windows-msvc\codex\codex.exe"));
        assert!(is_codex_image("Codex-X86_64.EXE"));
        assert!(!is_codex_image(r"C:\Program Files\nodejs\node.exe"));
        assert!(!is_codex_image(r"C:\codex\node.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn native_probe_refuses_a_process_outside_the_job_or_not_the_harness() {
        struct Scope(bool);
        impl PtySession for Scope {
            fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult { Ok(0) }
            fn resize(&self, _cols: u16, _rows: u16) -> Result<()> { Ok(()) }
            fn kill(&self) -> Result<()> { Ok(()) }
            fn try_wait(&self) -> Result<Option<PtyExitStatus>> { Ok(None) }
            fn process_id(&self) -> Option<u32> { Some(std::process::id()) }
            fn contains_process(&self, _process: &PinnedProcess) -> Result<bool> { Ok(self.0) }
        }
        let pid = std::process::id();
        let outside = NativeProcessProbe.harness_start(&Scope(false), pid, DriverKind::Claude).unwrap_err();
        assert!(outside.to_string().contains("outside"), "{outside:#}");
        // This test binary is in the Job here but is no harness image.
        let image = NativeProcessProbe.harness_start(&Scope(true), pid, DriverKind::Codex).unwrap_err();
        assert!(image.to_string().contains("not the pane's harness"), "{image:#}");
    }

    #[cfg(windows)]
    #[test]
    fn harness_homes_follow_their_overrides() {
        let vars = HashMap::from([
            ("USERPROFILE", r"C:\Users\someone"),
            ("CLAUDE_CONFIG_DIR", r"D:\claude-config"),
            ("GROK_HOME", r"relative\grok"),
            ("COMPUTERNAME", "MAINPC"),
        ]);
        let homes = HarnessHomes::from_vars(|name| vars.get(name).map(Into::into));
        assert_eq!(homes.claude, Some(PathBuf::from(r"D:\claude-config")));
        assert_eq!(homes.codex, Some(PathBuf::from(r"C:\Users\someone\.codex")));
        assert_eq!(homes.grok, None, "a relative override disables that follower");
        assert_eq!(homes.pid_domain, "win32:mainpc");
    }

    #[test]
    fn filetime_converts_to_the_matching_wall_clock() {
        let at = filetime_to_system(START).unwrap();
        // procStart 134368777644688307 of a live pane: 2026-10-19T10:02:44.4688307Z.
        assert_eq!(DateTime::<Utc>::from(at).to_rfc3339_opts(chrono::SecondsFormat::Millis, true), "2026-10-19T10:02:44.468Z");
        assert_eq!(filetime_to_system(0), None);
    }
}
