//! The macOS process layer: process identity, the per-run process scope
//! (PRIM-1's counterpart of a Windows Job), the resource readings telemetry
//! uses, and the crash guard.
//!
//! macOS has no kernel job a process can be put in and not leave, so a run's
//! process tree is derived from evidence, in this order:
//!
//! 1. the run's **session and process group**: the PTY child is a session
//!    and group leader (portable-pty calls `setsid` before `exec`), so its
//!    pid is the sid and the pgid every ordinary child inherits;
//! 2. **parentage**: a process whose parent chain reaches a member;
//! 3. a per-run **environment marker** (`PRIM1_PROCESS_SCOPE=<random>`), read
//!    back from the kernel with `KERN_PROCARGS2`: a descendant that called
//!    `setsid` and was reparented to launchd still carries it;
//! 4. members already seen, by pid and start time, so one whose parent died
//!    between two looks is not forgotten.
//!
//! This is weaker than a Job and says so: a process that both daemonizes and
//! scrubs its environment before PRIM-1 ever looks at it can escape. Every
//! step fails closed: `terminate` returns `Ok` only when no member is left.
//!
//! The Job's other guarantee, that the tree dies with the app even when the
//! app is killed outright, is the crash guard: a small helper process
//! (`--prim1-orphan-guard`, started once per app) holds the list of live
//! scopes over a pipe and sweeps them when that pipe breaks.

use std::{
    collections::{HashMap, HashSet},
    ffi::CStr,
    io::{self, BufRead, Write},
    mem::{MaybeUninit, size_of},
    os::{fd::RawFd, unix::process::CommandExt},
    path::Path,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};

use crate::{ProcessAlreadyExited, ProcessIdentity, ProcessUsage, ScopeAccounting};

/// The environment variable a PTY child inherits and everything it starts
/// keeps; its value identifies one run's scope. Not a credential.
pub const PROCESS_SCOPE_ENV: &str = "PRIM1_PROCESS_SCOPE";
/// `main` runs the crash guard when given this as its first argument.
pub const ORPHAN_GUARD_ARGUMENT: &str = "--prim1-orphan-guard";

const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;
const TICKS_PER_SECOND: u64 = 10_000_000;
const SZOMB: u32 = 5;
/// Termination budget: SIGTERM, a short grace, SIGKILL, then the proof.
const TERMINATE_GRACE: Duration = Duration::from_millis(600);
const TERMINATE_DEADLINE: Duration = Duration::from_secs(4);
const TERMINATE_POLL: Duration = Duration::from_millis(15);
/// Parent chains longer than this are not walked.
const PARENT_WALK_MAX_HOPS: usize = 64;
const PROC_PIDUNIQIDENTIFIERINFO: libc::c_int = 17;

/// `struct proc_uniqidentifierinfo` from `<sys/proc_info.h>`.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ProcUniqIdentifierInfo {
    p_uuid: [u8; 16],
    p_uniqueid: u64,
    p_puniqueid: u64,
    p_idversion: i32,
    p_orig_ppidversion: i32,
    p_reserve2: u64,
    p_reserve3: u64,
    p_reserve4: u64,
}

/// One process-table line of this user's process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcEntry {
    pub(crate) pid: u32,
    pub(crate) ppid: u32,
    pub(crate) pgid: u32,
    pub(crate) sid: i32,
    pub(crate) started: u64,
}

fn timeval_to_filetime(seconds: u64, microseconds: u64) -> u64 {
    seconds
        .saturating_mul(TICKS_PER_SECOND)
        .saturating_add(microseconds.saturating_mul(10))
        .saturating_add(FILETIME_UNIX_EPOCH)
}

fn bsd_info(pid: i32) -> Option<libc::proc_bsdinfo> {
    let size = size_of::<libc::proc_bsdinfo>() as libc::c_int;
    let mut info = MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    // SAFETY: `info` is a correctly sized, writable proc_bsdinfo.
    let read = unsafe {
        libc::proc_pidinfo(pid, libc::PROC_PIDTBSDINFO, 0, info.as_mut_ptr().cast(), size)
    };
    // SAFETY: the kernel filled exactly `size` bytes when the call returned it.
    (read == size).then(|| unsafe { info.assume_init() })
}

fn unique_process_id(pid: i32) -> Option<u64> {
    let mut info = ProcUniqIdentifierInfo::default();
    let size = size_of::<ProcUniqIdentifierInfo>() as libc::c_int;
    // SAFETY: `info` is writable and at least the size given.
    let read = unsafe {
        libc::proc_pidinfo(
            pid,
            PROC_PIDUNIQIDENTIFIERINFO,
            0,
            (&raw mut info).cast(),
            size,
        )
    };
    (read > 0).then_some(info.p_uniqueid)
}

fn is_zombie(info: &libc::proc_bsdinfo) -> bool {
    info.pbi_status == SZOMB
}

fn started_filetime(info: &libc::proc_bsdinfo) -> u64 {
    timeval_to_filetime(info.pbi_start_tvsec, info.pbi_start_tvusec)
}

/// The start of the process, in FILETIME ticks, when it exists.
pub fn process_started_filetime(pid: u32) -> Option<u64> {
    let info = bsd_info(i32::try_from(pid).ok()?)?;
    (!is_zombie(&info)).then(|| started_filetime(&info))
}

/// The process's parent, as the kernel records it now.
pub fn parent_process_id(pid: u32) -> Option<u32> {
    bsd_info(i32::try_from(pid).ok()?).map(|info| info.pbi_ppid)
}

fn own_uid() -> u32 {
    // SAFETY: getuid has no failure mode and takes no arguments.
    unsafe { libc::getuid() }
}

/// The executable path of a process; the short command name when the path
/// cannot be read (a process that exited or was replaced).
pub fn process_image_path(pid: u32) -> Option<String> {
    let pid = i32::try_from(pid).ok()?;
    let mut buffer = vec![0_u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
    // SAFETY: the buffer is writable and at least the size given.
    let length = unsafe {
        libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32)
    };
    if length > 0 {
        buffer.truncate(length as usize);
        return String::from_utf8(buffer).ok();
    }
    let info = bsd_info(pid)?;
    let name = unsafe { CStr::from_ptr(info.pbi_comm.as_ptr()) };
    let name = name.to_string_lossy().into_owned();
    (!name.is_empty()).then_some(name)
}

fn list_all_pids() -> Result<Vec<i32>> {
    // SAFETY: a null buffer with size 0 asks for the current process count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    if count < 0 {
        return Err(io::Error::last_os_error()).context("proc_listallpids failed");
    }
    let mut capacity = (count as usize).saturating_add(64).max(256);
    loop {
        let mut pids = vec![0_i32; capacity];
        let bytes = (pids.len() * size_of::<i32>()) as libc::c_int;
        // SAFETY: the buffer is writable and `bytes` long.
        let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
        if filled < 0 {
            return Err(io::Error::last_os_error()).context("proc_listallpids failed");
        }
        let filled = filled as usize;
        if filled < capacity {
            pids.truncate(filled);
            return Ok(pids);
        }
        capacity *= 2;
        if capacity > 1 << 22 {
            bail!("process table is implausibly large");
        }
    }
}

/// Every live process of this user, by pid, without launchd or this process.
fn read_process_table() -> Result<HashMap<u32, ProcEntry>> {
    let uid = own_uid();
    let me = std::process::id();
    let mut table = HashMap::new();
    for pid in list_all_pids()? {
        if pid <= 1 || pid as u32 == me {
            continue;
        }
        let Some(info) = bsd_info(pid) else { continue };
        if is_zombie(&info) || info.pbi_uid != uid {
            continue;
        }
        // SAFETY: getsid takes any pid and reports failure through -1.
        let sid = unsafe { libc::getsid(pid) };
        table.insert(
            pid as u32,
            ProcEntry {
                pid: pid as u32,
                ppid: info.pbi_ppid,
                pgid: info.pbi_pgid,
                sid,
                started: started_filetime(&info),
            },
        );
    }
    Ok(table)
}

/// The argument and environment area of one process, as the kernel recorded
/// it at exec. Same-user processes only; `None` when it cannot be read.
fn read_process_arguments(pid: u32, buffer: &mut [u8]) -> Option<usize> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, i32::try_from(pid).ok()?];
    let mut size = buffer.len();
    // SAFETY: mib and the buffer are valid for the lengths passed.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (result == 0).then_some(size)
}

/// True when the exact `name=value` string is one of the process's arguments
/// or environment strings (NUL-delimited, so a prefix cannot match).
fn arguments_contain(arguments: &[u8], entry: &[u8]) -> bool {
    arguments.split(|byte| *byte == 0).any(|part| part == entry)
}

/// `kern.argmax`: the size of the largest possible argument area.
fn argmax() -> usize {
    let mut value: libc::c_int = 0;
    let mut size = size_of::<libc::c_int>();
    let mut mib = [libc::CTL_KERN, libc::KERN_ARGMAX];
    // SAFETY: mib and value are valid for the lengths passed.
    let result = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            (&raw mut value).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if result == 0 && value > 0 { value as usize } else { 1 << 20 }
}

fn random_token() -> Result<String> {
    let mut bytes = [0_u8; 16];
    // SAFETY: the buffer is writable and shorter than getentropy's limit.
    if unsafe { libc::getentropy(bytes.as_mut_ptr().cast(), bytes.len()) } != 0 {
        return Err(io::Error::last_os_error()).context("getentropy failed");
    }
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn signal_process(pid: u32, signal: libc::c_int) {
    // SAFETY: kill takes any pid; failures (a process already gone) are fine
    // here because the next pass of the proof loop judges the outcome.
    unsafe {
        libc::kill(pid as libc::pid_t, signal);
    }
}

fn signal_group(pgid: u32, signal: libc::c_int) {
    if pgid > 1 {
        // SAFETY: as for `signal_process`.
        unsafe {
            libc::killpg(pgid as libc::pid_t, signal);
        }
    }
}

// ---------------------------------------------------------------------------
// Pinned process identity
// ---------------------------------------------------------------------------

/// One exact process, by pid and start time.
///
/// macOS has no process handle, so a pid is never held as authority: every
/// use rechecks that the pid still names the process that started at the
/// recorded time (and the kernel's unique process id when it can be read).
#[derive(Clone, Debug)]
pub struct PinnedProcess {
    process_id: u32,
    creation_time: u64,
    unique_id: Option<u64>,
}

impl PinnedProcess {
    pub fn open(process_id: u32) -> Result<Self> {
        if process_id == 0 {
            bail!("cannot pin process ID 0");
        }
        let pid = i32::try_from(process_id)?;
        let Some(info) = bsd_info(pid) else {
            bail!("process {process_id} does not exist or cannot be read");
        };
        if is_zombie(&info) {
            return Err(anyhow::Error::new(ProcessAlreadyExited(process_id)));
        }
        Ok(Self {
            process_id,
            creation_time: started_filetime(&info),
            unique_id: unique_process_id(pid),
        })
    }

    pub fn pid(&self) -> u32 {
        self.process_id
    }

    pub fn creation_time_filetime(&self) -> u64 {
        self.creation_time
    }

    pub fn is_alive(&self) -> Result<bool> {
        let pid = i32::try_from(self.process_id)?;
        let Some(info) = bsd_info(pid) else {
            return Ok(false);
        };
        if is_zombie(&info) || started_filetime(&info) != self.creation_time {
            return Ok(false);
        }
        Ok(match (self.unique_id, unique_process_id(pid)) {
            (Some(pinned), Some(current)) => pinned == current,
            _ => true,
        })
    }

    /// Waits up to `timeout` for this exact process to exit; true once it has.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<bool> {
        if !self.is_alive()? {
            return Ok(true);
        }
        let deadline = Instant::now() + timeout;
        // kqueue gives a wakeup the moment the process exits; the recheck
        // loop keeps this correct for a pid whose process was replaced.
        // SAFETY: kqueue takes no arguments and reports failure through -1.
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(io::Error::last_os_error()).context("kqueue failed");
        }
        struct Queue(libc::c_int);
        impl Drop for Queue {
            fn drop(&mut self) {
                // SAFETY: the descriptor is owned by this guard.
                unsafe {
                    libc::close(self.0);
                }
            }
        }
        let queue = Queue(queue);
        let change = libc::kevent {
            ident: self.process_id as usize,
            filter: libc::EVFILT_PROC,
            flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
            fflags: libc::NOTE_EXIT,
            data: 0,
            udata: std::ptr::null_mut(),
        };
        // SAFETY: one change record, no events requested.
        let registered = unsafe {
            libc::kevent(queue.0, &raw const change, 1, std::ptr::null_mut(), 0, std::ptr::null())
        };
        if registered < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                return Ok(true);
            }
            return Err(error).context("kevent(EVFILT_PROC) failed");
        }
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(!self.is_alive()?);
            }
            let wait = remaining.min(Duration::from_millis(250));
            let timespec = libc::timespec {
                tv_sec: wait.as_secs() as libc::time_t,
                tv_nsec: i64::from(wait.subsec_nanos()),
            };
            let mut event = MaybeUninit::<libc::kevent>::zeroed();
            // SAFETY: no changes, one writable event, a valid timeout.
            let fired = unsafe {
                libc::kevent(queue.0, std::ptr::null(), 0, event.as_mut_ptr(), 1, &raw const timespec)
            };
            if fired < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                return Err(io::Error::last_os_error()).context("kevent wait failed");
            }
            if fired > 0 || !self.is_alive()? {
                return Ok(true);
            }
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.process_id == other.process_id && self.creation_time == other.creation_time
    }

    /// The executable path of this exact process.
    pub fn image_path(&self) -> Result<String> {
        if !self.is_alive()? {
            bail!("process {} has exited", self.process_id);
        }
        process_image_path(self.process_id)
            .ok_or_else(|| anyhow!("cannot read the image path of process {}", self.process_id))
    }
}

// ---------------------------------------------------------------------------
// The per-run process scope
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScopeRoot {
    pid: u32,
    started: u64,
}

/// CPU seen so far, so a process that exited between two readings keeps its
/// share: the Job's cumulative accounting has no direct macOS counterpart.
#[derive(Default)]
struct AccountingState {
    exited_cpu: u64,
    last_cpu: HashMap<(u32, u64), u64>,
    peak_memory: u64,
}

/// One run's owned process tree. See the module documentation.
pub struct ProcessJob {
    marker: String,
    marker_entry: Vec<u8>,
    root: Mutex<Option<ScopeRoot>>,
    known: Mutex<HashMap<u32, u64>>,
    accounting: Mutex<AccountingState>,
    guard_slot: AtomicBool,
    argument_buffer_bytes: usize,
}

impl ProcessJob {
    pub fn new() -> Result<Self> {
        Self::with_marker(random_token()?)
    }

    fn with_marker(marker: String) -> Result<Self> {
        let marker_entry = format!("{PROCESS_SCOPE_ENV}={marker}").into_bytes();
        Ok(Self {
            marker,
            marker_entry,
            root: Mutex::new(None),
            known: Mutex::new(HashMap::new()),
            accounting: Mutex::new(AccountingState::default()),
            guard_slot: AtomicBool::new(false),
            argument_buffer_bytes: argmax(),
        })
    }

    /// The value for [`PROCESS_SCOPE_ENV`] in this scope's PTY child.
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// Binds the scope to the PTY child that was just spawned as a session
    /// leader. The PTY backend called `setsid` before `exec`, so the child's
    /// pid is its session and group id for as long as any member keeps them.
    pub fn bind_root(&self, process_id: u32) -> Result<()> {
        let pid = i32::try_from(process_id)?;
        let started = bsd_info(pid).map(|info| started_filetime(&info));
        let Some(started) = started else {
            bail!("process {process_id} vanished before its scope could be bound");
        };
        *self.root.lock().map_err(|_| anyhow!("process scope poisoned"))? =
            Some(ScopeRoot { pid: process_id, started });
        self.known
            .lock()
            .map_err(|_| anyhow!("process scope poisoned"))?
            .insert(process_id, started);
        if orphan_guard_send(&format!("R {} {process_id} {started}", self.marker)) {
            self.guard_slot.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    fn root(&self) -> Option<ScopeRoot> {
        *self.root.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The members of the scope at this moment. `sweep_markers` also reads
    /// the environment of every other process of this user, which is what
    /// finds a descendant that left the session and lost its parent; it is
    /// the slower pass, used for termination and accounting.
    pub(crate) fn scan(&self, sweep_markers: bool) -> Result<Vec<ProcEntry>> {
        let table = read_process_table()?;
        let mut members = HashSet::new();
        if let Some(root) = self.root() {
            if table.get(&root.pid).is_some_and(|entry| entry.started == root.started) {
                members.insert(root.pid);
            }
            for entry in table.values() {
                if entry.pgid == root.pid || entry.sid == root.pid as i32 {
                    members.insert(entry.pid);
                }
            }
        }
        {
            let known = self.known.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for (pid, started) in known.iter() {
                if table.get(pid).is_some_and(|entry| entry.started == *started) {
                    members.insert(*pid);
                }
            }
        }
        if sweep_markers {
            let mut buffer = vec![0_u8; self.argument_buffer_bytes];
            for entry in table.values() {
                if members.contains(&entry.pid) {
                    continue;
                }
                if let Some(size) = read_process_arguments(entry.pid, &mut buffer)
                    && arguments_contain(&buffer[..size], &self.marker_entry)
                {
                    members.insert(entry.pid);
                }
            }
        }
        // Everything those processes started, however deep. Parents are older
        // than their children, so one ordered pass reaches a fixed point.
        let mut ordered: Vec<&ProcEntry> = table.values().collect();
        ordered.sort_by_key(|entry| entry.started);
        for entry in ordered {
            if members.contains(&entry.ppid) {
                members.insert(entry.pid);
            }
        }
        let live: Vec<ProcEntry> = members.iter().filter_map(|pid| table.get(pid)).copied().collect();
        let mut known = self.known.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        known.clear();
        known.extend(live.iter().map(|entry| (entry.pid, entry.started)));
        Ok(live)
    }

    /// The scope's live members; the process table and the scope state only.
    pub fn live_process_ids(&self) -> Result<Vec<u32>> {
        let mut pids: Vec<u32> = self.scan(false)?.into_iter().map(|entry| entry.pid).collect();
        pids.sort_unstable();
        Ok(pids)
    }

    /// Snapshot-only diagnostic. Never retain a PID from this API as authority.
    pub fn contains_process_id(&self, process_id: u32) -> Result<bool> {
        Ok(self.scan(false)?.iter().any(|entry| entry.pid == process_id))
    }

    /// True when this exact process belongs to the scope. Each check reads
    /// that one process (and at most its parent chain), never the whole
    /// table, because callers hold the supervisor's session lock.
    pub fn contains_process(&self, process: &PinnedProcess) -> Result<bool> {
        if !process.is_alive()? {
            return Ok(false);
        }
        let Some(root) = self.root() else {
            return Ok(false);
        };
        let pid = process.pid();
        if pid == root.pid && process.creation_time_filetime() == root.started {
            return Ok(true);
        }
        // SAFETY: getsid takes any pid and reports failure through -1.
        if unsafe { libc::getsid(pid as libc::pid_t) } == root.pid as i32 {
            return Ok(true);
        }
        if self
            .known
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&pid)
            .is_some_and(|started| *started == process.creation_time_filetime())
        {
            return Ok(true);
        }
        // The parent chain, each link no younger than its child.
        let mut child_started = process.creation_time_filetime();
        let mut cursor = pid;
        for _ in 0..PARENT_WALK_MAX_HOPS {
            let Some(info) = bsd_info(cursor as i32) else { break };
            let parent = info.pbi_ppid;
            if parent <= 1 {
                break;
            }
            let Some(parent_info) = bsd_info(parent as i32) else { break };
            let parent_started = started_filetime(&parent_info);
            if parent_started > child_started {
                break;
            }
            if parent == root.pid && parent_started == root.started {
                return Ok(true);
            }
            // SAFETY: as above.
            if unsafe { libc::getsid(parent as libc::pid_t) } == root.pid as i32 {
                return Ok(true);
            }
            child_started = parent_started;
            cursor = parent;
        }
        // Last, the marker its environment was started with: a descendant
        // that left the session and whose ancestors are gone.
        let mut buffer = vec![0_u8; self.argument_buffer_bytes];
        Ok(read_process_arguments(pid, &mut buffer)
            .is_some_and(|size| arguments_contain(&buffer[..size], &self.marker_entry)))
    }

    /// Resource accounting for telemetry. CPU keeps every process that has
    /// run in the scope while PRIM-1 was looking; the peak is the larger of
    /// the sampled total and the highest footprint the kernel recorded for
    /// any single member.
    pub fn accounting(&self, image_names: bool) -> Result<ScopeAccounting> {
        let members = self.scan(false)?;
        let mut state = self.accounting.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut seen = HashSet::new();
        let mut usage = Vec::with_capacity(members.len());
        let mut unreadable = Vec::new();
        let mut member_peak = 0_u64;
        for entry in &members {
            match read_usage(entry.pid, image_names) {
                Some((reading, lifetime_peak)) => {
                    let identity = (reading.pid, reading.created);
                    seen.insert(identity);
                    state.last_cpu.insert(identity, reading.cpu_time_100ns);
                    member_peak = member_peak.max(lifetime_peak);
                    usage.push(reading);
                }
                None => {
                    if process_started_filetime(entry.pid).is_some() {
                        unreadable.push(entry.pid);
                    }
                }
            }
        }
        // A member gone since the previous reading keeps the CPU it used.
        let gone: Vec<(u32, u64)> =
            state.last_cpu.keys().filter(|identity| !seen.contains(*identity)).copied().collect();
        for identity in gone {
            if let Some(cpu) = state.last_cpu.remove(&identity) {
                state.exited_cpu = state.exited_cpu.saturating_add(cpu);
            }
        }
        let live_cpu: u64 = usage.iter().map(|member| member.cpu_time_100ns).sum();
        let live_memory: u64 = usage.iter().map(|member| member.private_bytes).sum();
        state.peak_memory = state.peak_memory.max(live_memory).max(member_peak);
        Ok(ScopeAccounting {
            cpu_time_100ns: state.exited_cpu.saturating_add(live_cpu),
            peak_memory_bytes: state.peak_memory,
            members: usage,
            unreadable,
        })
    }

    /// Terminates every member and returns only once none is left. Polite
    /// first (SIGTERM, which lets a CLI save its state), then SIGKILL; the
    /// proof is a full pass that finds nobody, markers included.
    pub fn terminate(&self) -> Result<()> {
        let deadline = Instant::now() + TERMINATE_DEADLINE;
        let grace = Instant::now() + TERMINATE_GRACE;
        let mut sent_term = false;
        let mut sent_kill = false;
        let root = self.root();
        loop {
            let members = self.scan(true)?;
            if members.is_empty() {
                self.release_guard_slot();
                return Ok(());
            }
            let now = Instant::now();
            if now >= deadline {
                bail!(
                    "{} process(es) of this run remained after {}ms: {:?}",
                    members.len(),
                    TERMINATE_DEADLINE.as_millis(),
                    members.iter().map(|entry| entry.pid).collect::<Vec<_>>()
                );
            }
            let signal = if now >= grace { libc::SIGKILL } else { libc::SIGTERM };
            let already_sent = if signal == libc::SIGKILL { sent_kill } else { sent_term };
            if !already_sent || signal == libc::SIGKILL {
                // The group first: it reaches anything forked since the scan.
                if let Some(root) = root {
                    signal_group(root.pid, signal);
                }
                let mut groups = HashSet::new();
                for entry in &members {
                    groups.insert(entry.pgid);
                    // Never signal a pid whose process changed since the scan.
                    if process_started_filetime(entry.pid) == Some(entry.started) {
                        signal_process(entry.pid, signal);
                    }
                }
                for group in groups {
                    signal_group(group, signal);
                }
                if signal == libc::SIGKILL {
                    sent_kill = true;
                } else {
                    sent_term = true;
                }
            }
            thread::sleep(TERMINATE_POLL);
        }
    }

    fn release_guard_slot(&self) {
        if self.guard_slot.swap(false, Ordering::SeqCst) {
            orphan_guard_send(&format!("D {}", self.marker));
        }
    }

    /// The members with image names, for liveness checks.
    pub(crate) fn live_identities(&self) -> Result<Vec<ProcessIdentity>> {
        let mut members = self.scan(false)?;
        members.sort_by_key(|entry| entry.pid);
        Ok(members
            .into_iter()
            .map(|entry| ProcessIdentity {
                process_id: entry.pid,
                image_name: process_image_path(entry.pid),
            })
            .collect())
    }

}

impl Drop for ProcessJob {
    fn drop(&mut self) {
        self.release_guard_slot();
    }
}

// ---------------------------------------------------------------------------
// Resource readings
// ---------------------------------------------------------------------------

/// Nanoseconds per mach time unit, as (numerator, denominator).
fn mach_timebase() -> (u64, u64) {
    static TIMEBASE: OnceLock<(u64, u64)> = OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info { numer: 0, denom: 0 };
        // SAFETY: `info` is writable; on failure the fallback is the 1:1 rate.
        if unsafe { libc::mach_timebase_info(&raw mut info) } == 0 && info.denom != 0 {
            (u64::from(info.numer), u64::from(info.denom))
        } else {
            (1, 1)
        }
    })
}

fn mach_time_to_100ns(value: u64) -> u64 {
    let (numerator, denominator) = mach_timebase();
    (u128::from(value) * u128::from(numerator) / u128::from(denominator) / 100) as u64
}

/// The process's usage and its lifetime peak footprint. `None` when it has
/// gone or cannot be read.
fn read_usage(pid: u32, image_name: bool) -> Option<(ProcessUsage, u64)> {
    let info = bsd_info(i32::try_from(pid).ok()?)?;
    if is_zombie(&info) {
        return None;
    }
    let mut usage = MaybeUninit::<libc::rusage_info_v4>::zeroed();
    // SAFETY: the flavour and buffer type agree (rusage_info_v4).
    let result = unsafe {
        libc::proc_pid_rusage(pid as i32, libc::RUSAGE_INFO_V4, usage.as_mut_ptr().cast())
    };
    if result != 0 {
        return None;
    }
    // SAFETY: the call succeeded, so the buffer holds a rusage_info_v4.
    let usage = unsafe { usage.assume_init() };
    Some((
        ProcessUsage {
            pid,
            created: started_filetime(&info),
            cpu_time_100ns: mach_time_to_100ns(usage.ri_user_time.saturating_add(usage.ri_system_time)),
            private_bytes: usage.ri_phys_footprint,
            image_name: if image_name { process_image_path(pid) } else { None },
        },
        usage.ri_lifetime_max_phys_footprint,
    ))
}

/// This process's own reading, for telemetry.
pub fn current_process_usage() -> Result<ProcessUsage> {
    read_usage(std::process::id(), false)
        .map(|(usage, _)| usage)
        .ok_or_else(|| anyhow!("failed to read PRIM-1's own process usage"))
}

/// The processor count a machine-wide CPU share is taken against.
pub fn logical_processor_count() -> u32 {
    thread::available_parallelism().map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX))
}

unsafe extern "C" {
    fn proc_listpidspath(
        kind: u32,
        typeinfo: u32,
        path: *const libc::c_char,
        path_flags: u32,
        buffer: *mut libc::c_void,
        buffer_size: libc::c_int,
    ) -> libc::c_int;
}

const PROC_ALL_PIDS: u32 = 1;
const PROC_LISTPIDSPATH_EXCLUDE_EVTONLY: u32 = 2;

/// The processes that have `path` open, from the kernel's file table. A file
/// nobody holds lists nobody; a missing file is an empty list as well.
pub fn processes_holding_path(path: &Path) -> Result<Vec<u32>> {
    use std::os::unix::ffi::OsStrExt as _;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes()).context("path contains a NUL")?;
    let mut pids = vec![0_i32; 256];
    loop {
        let bytes = (pids.len() * size_of::<i32>()) as libc::c_int;
        // SAFETY: the path is NUL-terminated and the buffer is writable.
        let filled = unsafe {
            proc_listpidspath(
                PROC_ALL_PIDS,
                0,
                c_path.as_ptr(),
                PROC_LISTPIDSPATH_EXCLUDE_EVTONLY,
                pids.as_mut_ptr().cast(),
                bytes,
            )
        };
        if filled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::NotFound {
                return Ok(Vec::new());
            }
            return Err(error).with_context(|| format!("cannot list the processes using {}", path.display()));
        }
        let count = filled as usize / size_of::<i32>();
        if count < pids.len() {
            pids.truncate(count);
            return Ok(pids.into_iter().filter(|pid| *pid > 0).map(|pid| pid as u32).collect());
        }
        if pids.len() >= 1 << 20 {
            bail!("too many processes hold {}", path.display());
        }
        pids.resize(pids.len() * 2, 0);
    }
}

// ---------------------------------------------------------------------------
// The crash guard
// ---------------------------------------------------------------------------

struct OrphanGuard {
    stdin: Mutex<ChildStdin>,
    _child: Child,
}

static ORPHAN_GUARD: OnceLock<OrphanGuard> = OnceLock::new();

/// Starts the helper that sweeps this app's scopes if the app dies without
/// shutting down. Failing to start it is logged by the caller and costs only
/// that guarantee: an orderly shutdown does not depend on it.
pub fn start_orphan_guard(executable: &Path) -> Result<()> {
    if ORPHAN_GUARD.get().is_some() {
        return Ok(());
    }
    let mut command = Command::new(executable);
    command
        .arg(ORPHAN_GUARD_ARGUMENT)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // SAFETY: setsid is async-signal-safe and touches only the child. The
    // helper leaves the app's session so a signal to the app does not reach it.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().context("failed to start the process guard helper")?;
    let stdin = child.stdin.take().ok_or_else(|| anyhow!("guard helper has no stdin pipe"))?;
    ORPHAN_GUARD
        .set(OrphanGuard { stdin: Mutex::new(stdin), _child: child })
        .map_err(|_| anyhow!("the process guard is already running"))?;
    Ok(())
}

fn orphan_guard_send(line: &str) -> bool {
    let Some(guard) = ORPHAN_GUARD.get() else { return false };
    let Ok(mut stdin) = guard.stdin.lock() else { return false };
    stdin.write_all(line.as_bytes()).is_ok()
        && stdin.write_all(b"\n").is_ok()
        && stdin.flush().is_ok()
}

/// The helper's whole life: read registrations until the pipe breaks (the app
/// has gone, however it went), then sweep whatever is still registered.
pub fn run_orphan_guard() -> ! {
    // SAFETY: changing dispositions in this single-threaded helper. It must
    // outlive the app's session and terminal to do its one job.
    unsafe {
        libc::signal(libc::SIGHUP, libc::SIG_IGN);
        libc::signal(libc::SIGINT, libc::SIG_IGN);
        libc::signal(libc::SIGTERM, libc::SIG_IGN);
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
    let mut scopes: HashMap<String, ScopeRoot> = HashMap::new();
    for line in io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let mut parts = line.split(' ');
        match (parts.next(), parts.next(), parts.next(), parts.next()) {
            (Some("R"), Some(marker), Some(pid), Some(started)) => {
                if let (Ok(pid), Ok(started)) = (pid.parse(), started.parse()) {
                    scopes.insert(marker.to_owned(), ScopeRoot { pid, started });
                }
            }
            (Some("D"), Some(marker), _, _) => {
                scopes.remove(marker);
            }
            _ => {}
        }
    }
    for (marker, root) in scopes {
        if let Ok(job) = ProcessJob::with_marker(marker) {
            *job.root.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(root);
            job.known
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(root.pid, root.started);
            let _ = job.terminate();
        }
    }
    std::process::exit(0);
}

// ---------------------------------------------------------------------------
// A PTY writer that can be cancelled
// ---------------------------------------------------------------------------

/// Shared between `send_input` and whoever cancels it.
#[derive(Default)]
pub(crate) struct WriteControl {
    cancel: AtomicBool,
    writing: AtomicBool,
}

impl WriteControl {
    pub(crate) fn begin(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        self.writing.store(true, Ordering::SeqCst);
    }

    pub(crate) fn end(&self) {
        self.writing.store(false, Ordering::SeqCst);
    }

    /// Cancels the write in flight, if any. The reversible flag means a write
    /// that finishes first simply ignores it: the next write clears it.
    pub(crate) fn request_cancel(&self) {
        if self.writing.load(Ordering::SeqCst) {
            self.cancel.store(true, Ordering::SeqCst);
        }
    }

    fn take_cancel(&self) -> bool {
        self.cancel.swap(false, Ordering::SeqCst)
    }
}

/// Writes to the (non-blocking) PTY master without ever blocking past a
/// poll tick, so a stuck write can be cancelled and the PTY kept: the same
/// distinction Windows gets from `CancelIoEx` on the writer handle.
pub(crate) struct PtyMasterWriter {
    fd: RawFd,
    control: std::sync::Arc<WriteControl>,
}

impl PtyMasterWriter {
    /// Takes ownership of `fd`, a duplicate of the master.
    pub(crate) fn new(fd: RawFd, control: std::sync::Arc<WriteControl>) -> Self {
        Self { fd, control }
    }
}

impl Write for PtyMasterWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        loop {
            if self.control.take_cancel() {
                return Err(io::Error::other("PTY input write was cancelled"));
            }
            // SAFETY: the descriptor is owned here; the slice is valid.
            let written = unsafe { libc::write(self.fd, buffer.as_ptr().cast(), buffer.len()) };
            if written >= 0 {
                return Ok(written as usize);
            }
            let error = io::Error::last_os_error();
            match error.raw_os_error() {
                Some(code) if code == libc::EINTR => continue,
                Some(code) if code == libc::EAGAIN => {
                    let mut pollfd = libc::pollfd { fd: self.fd, events: libc::POLLOUT, revents: 0 };
                    // SAFETY: one valid pollfd; a short timeout so a cancel
                    // request is seen promptly.
                    let ready = unsafe { libc::poll(&raw mut pollfd, 1, 100) };
                    if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                        return Err(io::Error::last_os_error());
                    }
                }
                _ => return Err(error),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for PtyMasterWriter {
    fn drop(&mut self) {
        // SAFETY: the descriptor is owned by this writer.
        unsafe {
            libc::close(self.fd);
        }
    }
}

/// Waits until the descriptor is readable or the timeout passes; the reader
/// thread uses it because the master is non-blocking for the writer's sake.
pub(crate) fn wait_readable(fd: RawFd, timeout_ms: libc::c_int) {
    let mut pollfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
    // SAFETY: one valid pollfd and a bounded timeout.
    unsafe {
        libc::poll(&raw mut pollfd, 1, timeout_ms);
    }
}

/// Duplicates a descriptor with close-on-exec set.
pub(crate) fn dup_cloexec(fd: RawFd) -> Result<RawFd> {
    // SAFETY: fcntl with F_DUPFD_CLOEXEC on a descriptor owned by the caller.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error()).context("failed to duplicate the PTY master");
    }
    Ok(duplicate)
}

/// Sets `O_NONBLOCK` on the master's open file description.
pub(crate) fn set_nonblocking(fd: RawFd) -> Result<()> {
    // SAFETY: fcntl on a descriptor owned by the caller.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error()).context("failed to read PTY master flags");
    }
    // SAFETY: as above.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error()).context("failed to set the PTY master non-blocking");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// The child environment on macOS
// ---------------------------------------------------------------------------

/// Terminal-identifying variables of whatever launched PRIM-1. The pane is
/// an xterm.js terminal; another terminal's identity would make a CLI use
/// the wrong features (iTerm inline images, tmux passthrough). The variables
/// `CLAUDE_CODE_*` are a parent Claude Code's own, not this pane's.
const INHERITED_TERMINAL_ENV: [&str; 12] = [
    "TERM_PROGRAM",
    "TERM_PROGRAM_VERSION",
    "TERM_SESSION_ID",
    "LC_TERMINAL",
    "LC_TERMINAL_VERSION",
    "ITERM_SESSION_ID",
    "ITERM_PROFILE",
    "TMUX",
    "TMUX_PANE",
    "KITTY_WINDOW_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_CODE_SSE_PORT",
];

/// Applies the environment a macOS terminal program expects. A GUI-launched
/// app has no `TERM` and usually no `LANG` (which leaves CLIs in ASCII).
pub(crate) fn apply_terminal_environment(command: &mut portable_pty::CommandBuilder) {
    for key in INHERITED_TERMINAL_ENV {
        command.env_remove(key);
    }
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    let has_locale = ["LANG", "LC_ALL", "LC_CTYPE"]
        .iter()
        .any(|key| command.get_env(key).is_some_and(|value| !value.is_empty()));
    if !has_locale {
        command.env("LANG", "en_US.UTF-8");
    }
}

/// True for the native Claude Code layout on macOS
/// (`…/claude/versions/2.1.319`), whose file name is only a version number.
pub fn is_claude_versioned_install(path: &str) -> bool {
    let mut parts = path.rsplit('/');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(version), Some("versions"), Some("claude")) => {
            !version.is_empty() && version.chars().all(|c| c.is_ascii_digit() || c == '.')
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn sleep_command(seconds: &str) -> Command {
        let mut command = Command::new("/bin/sleep");
        command.arg(seconds).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        command
    }

    /// A shell that leaves a backgrounded child behind in its own session.
    fn spawn_scope_root() -> (Child, u32) {
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "/bin/sleep 300 & echo $! > \"$PRIM1_TEST_PID\"; wait"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let dir = std::env::temp_dir().join(format!("prim1-scope-{}", random_token().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("child.pid");
        command.env("PRIM1_TEST_PID", &pid_file);
        let child = command.spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let descendant = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "descendant pid never appeared");
            thread::sleep(Duration::from_millis(5));
        };
        let _ = std::fs::remove_dir_all(&dir);
        (child, descendant)
    }

    #[test]
    fn pinned_process_tracks_one_identity_from_live_through_exit() {
        let mut child = sleep_command("30").spawn().unwrap();
        let pid = child.id();
        let pinned = PinnedProcess::open(pid).unwrap();
        let second = PinnedProcess::open(pid).unwrap();
        assert!(pinned.same_identity(&second));
        assert!(pinned.is_alive().unwrap());
        assert!(pinned.image_path().unwrap().ends_with("/sleep"), "{:?}", pinned.image_path());
        assert!(!pinned.wait_for_exit(Duration::from_millis(30)).unwrap());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(!pinned.is_alive().unwrap());
        assert!(pinned.wait_for_exit(Duration::from_millis(30)).unwrap());
        assert!(PinnedProcess::open(pid).is_err());
    }

    #[test]
    fn pinned_process_wait_returns_as_soon_as_the_process_exits() {
        let mut child = sleep_command("0.2").spawn().unwrap();
        let pinned = PinnedProcess::open(child.id()).unwrap();
        let started = Instant::now();
        assert!(pinned.wait_for_exit(Duration::from_secs(5)).unwrap());
        assert!(started.elapsed() < Duration::from_secs(2), "{:?}", started.elapsed());
        let _ = child.wait();
    }

    #[test]
    fn scope_holds_root_and_descendant_and_reports_the_processes() {
        let job = ProcessJob::new().unwrap();
        let (mut root, descendant) = spawn_scope_root();
        job.bind_root(root.id()).unwrap();
        let mut pids = job.live_process_ids().unwrap();
        // The session and the parent chain both claim the sleeping child.
        pids.retain(|pid| *pid == root.id() || *pid == descendant);
        assert_eq!(pids.len(), 2, "{pids:?}");
        let pinned_root = PinnedProcess::open(root.id()).unwrap();
        let pinned_descendant = PinnedProcess::open(descendant).unwrap();
        assert!(job.contains_process(&pinned_root).unwrap());
        assert!(job.contains_process(&pinned_descendant).unwrap());
        let me = PinnedProcess::open(std::process::id()).unwrap();
        assert!(!job.contains_process(&me).unwrap(), "the supervisor process is outside the scope");
        let identities = job.live_identities().unwrap();
        assert!(identities.iter().any(|process| process.process_id == descendant
            && process.image_name.as_deref().is_some_and(|name| name.ends_with("/sleep"))));
        job.terminate().unwrap();
        assert!(!pinned_descendant.is_alive().unwrap());
        assert!(job.live_process_ids().unwrap().is_empty());
        let _ = root.wait();
    }

    #[test]
    fn terminate_escalates_past_a_process_that_ignores_sigterm() {
        let job = ProcessJob::new().unwrap();
        let mut command = Command::new("/bin/sh");
        command
            .args(["-c", "trap '' TERM; while :; do /bin/sleep 1; done"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut root = command.spawn().unwrap();
        job.bind_root(root.id()).unwrap();
        // Give the shell time to install its trap, as a real harness would.
        thread::sleep(Duration::from_millis(200));
        let started = Instant::now();
        job.terminate().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3), "{:?}", started.elapsed());
        assert!(job.live_process_ids().unwrap().is_empty());
        let _ = root.wait();
    }

    #[test]
    fn a_descendant_that_changed_session_is_found_by_its_environment_marker() {
        let job = ProcessJob::new().unwrap();
        // /usr/bin/python3 is present on macOS with Command Line Tools; a
        // shell cannot call setsid on its own.
        let mut command = Command::new("/usr/bin/python3");
        command
            .args(["-c", "import os,time,sys\nif os.fork()==0:\n  os.setsid()\n  if os.fork()==0:\n    open(sys.argv[1],'w').write(str(os.getpid()))\n    time.sleep(120)\n  os._exit(0)\nelse:\n  time.sleep(120)\n"])
            .env(PROCESS_SCOPE_ENV, job.marker())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let dir = std::env::temp_dir().join(format!("prim1-marker-{}", random_token().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        let pid_file = dir.join("escaped.pid");
        command.arg(&pid_file);
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut root = command.spawn().unwrap();
        job.bind_root(root.id()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let escaped = loop {
            if let Ok(text) = std::fs::read_to_string(&pid_file)
                && let Ok(pid) = text.trim().parse::<u32>()
            {
                break pid;
            }
            assert!(Instant::now() < deadline, "escaped pid never appeared");
            thread::sleep(Duration::from_millis(20));
        };
        let pinned = PinnedProcess::open(escaped).unwrap();
        // The double fork left it in its own session, and its parent is launchd.
        let deadline = Instant::now() + Duration::from_secs(5);
        while parent_process_id(escaped) != Some(1) {
            assert!(Instant::now() < deadline, "the escaped process was never reparented");
            thread::sleep(Duration::from_millis(20));
        }
        // SAFETY: getsid on a live child-tree process.
        assert_ne!(unsafe { libc::getsid(escaped as i32) }, root.id() as i32);
        assert!(!job.live_process_ids().unwrap().contains(&escaped), "a table pass alone misses it");
        assert!(job.contains_process(&pinned).unwrap(), "the marker still claims it");
        job.terminate().unwrap();
        assert!(!pinned.is_alive().unwrap(), "termination found it through the marker");
        let _ = root.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn accounting_reads_members_and_keeps_cpu_of_exited_ones() {
        let job = ProcessJob::new().unwrap();
        let (mut root, descendant) = spawn_scope_root();
        job.bind_root(root.id()).unwrap();
        let first = job.accounting(true).unwrap();
        assert!(first.members.iter().any(|member| member.pid == root.id()));
        assert!(first.members.iter().any(|member| member.pid == descendant
            && member.image_name.as_deref().is_some_and(|name| name.ends_with("/sleep"))));
        assert!(first.unreadable.is_empty(), "{first:?}");
        assert!(first.peak_memory_bytes > 0);
        let kill_child = PinnedProcess::open(descendant).unwrap();
        signal_process(descendant, libc::SIGKILL);
        assert!(kill_child.wait_for_exit(Duration::from_secs(5)).unwrap());
        let second = job.accounting(false).unwrap();
        assert!(second.cpu_time_100ns >= first.cpu_time_100ns, "{first:?} -> {second:?}");
        job.terminate().unwrap();
        let _ = root.wait();
    }

    #[test]
    fn write_control_cancels_only_a_write_in_flight() {
        let control = Arc::new(WriteControl::default());
        control.request_cancel();
        assert!(!control.take_cancel(), "no write in flight: nothing to cancel");
        control.begin();
        control.request_cancel();
        assert!(control.take_cancel());
        control.end();
        assert!(!control.take_cancel());
    }

    #[test]
    fn versioned_claude_installs_are_recognized_by_their_path() {
        assert!(is_claude_versioned_install("/Users/x/.local/share/claude/versions/2.1.319"));
        assert!(!is_claude_versioned_install("/Users/x/.local/bin/claude"));
        assert!(!is_claude_versioned_install("/opt/claude/versions/not-a-version"));
    }
}
