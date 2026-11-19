#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
#[cfg(windows)]
use std::{
    sync::{Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

#[cfg(windows)]
use anyhow::{Context, Result, anyhow, bail};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{FILETIME, HANDLE, WAIT_FAILED, WAIT_OBJECT_0, WAIT_TIMEOUT},
    Storage::FileSystem::SYNCHRONIZE,
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        JOBOBJECT_BASIC_PROCESS_ID_LIST, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectBasicAccountingInformation, JobObjectBasicProcessIdList,
        JobObjectExtendedLimitInformation, QueryInformationJobObject, SetInformationJobObject,
        TerminateJobObject,
    },
    System::Threading::{
        GetCurrentProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SET_QUOTA, PROCESS_TERMINATE, WaitForSingleObject,
    },
};

#[cfg(windows)]
const JOB_TERMINATION_WAIT: Duration = Duration::from_secs(4);
#[cfg(windows)]
const JOB_TERMINATION_POLL: Duration = Duration::from_millis(5);

/// `PinnedProcess::open` found the process object but it has already exited:
/// a known fact, unlike a failed query.
#[derive(Debug)]
pub struct ProcessAlreadyExited(pub u32);

impl std::fmt::Display for ProcessAlreadyExited {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "cannot pin process {}: process has already exited", self.0)
    }
}

impl std::error::Error for ProcessAlreadyExited {}

/// An owned handle to one exact Windows process object.
///
/// Keeping the handle open pins the process object even after exit, while the
/// process ID and creation time let two independently opened handles be checked
/// for the same identity. Fields stay private so callers cannot accidentally
/// turn a process ID into durable authority.
#[cfg(windows)]
pub struct PinnedProcess {
    handle: OwnedHandle,
    process_id: u32,
    creation_time: u64,
}

#[cfg(windows)]
impl PinnedProcess {
    /// Opens a live process with only query and synchronization rights.
    pub fn open(process_id: u32) -> Result<Self> {
        if process_id == 0 {
            bail!("cannot pin process ID 0");
        }

        let handle = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
                0,
                process_id,
            )
        };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("OpenProcess failed for process {process_id}"));
        }

        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        let creation_time = process_creation_time(handle.as_raw_handle() as HANDLE)
            .with_context(|| format!("failed to read creation time for process {process_id}"))?;
        let process = Self {
            handle,
            process_id,
            creation_time,
        };
        if !process.is_alive()? {
            return Err(anyhow::Error::new(ProcessAlreadyExited(process_id)));
        }

        Ok(process)
    }

    pub fn pid(&self) -> u32 {
        self.process_id
    }

    pub fn creation_time_filetime(&self) -> u64 {
        self.creation_time
    }

    pub fn is_alive(&self) -> Result<bool> {
        process_handle_is_alive(self.handle.as_raw_handle() as HANDLE)
            .with_context(|| format!("failed to query liveness for process {}", self.process_id))
    }

    /// Waits up to `timeout` for this exact process to exit; true once it has.
    /// The handle carries SYNCHRONIZE, so this is a kernel wait, not a poll.
    pub fn wait_for_exit(&self, timeout: Duration) -> Result<bool> {
        let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
        match unsafe { WaitForSingleObject(self.handle.as_raw_handle() as HANDLE, millis) } {
            WAIT_OBJECT_0 => Ok(true),
            WAIT_TIMEOUT => Ok(false),
            WAIT_FAILED => Err(std::io::Error::last_os_error())
                .with_context(|| format!("WaitForSingleObject failed for process {}", self.process_id)),
            result => Err(anyhow!(
                "WaitForSingleObject returned unexpected status {result:#x} for process {}",
                self.process_id
            )),
        }
    }

    pub fn same_identity(&self, other: &Self) -> bool {
        self.process_id == other.process_id && self.creation_time == other.creation_time
    }

    /// Win32 image path of this exact process (the pinned handle, not the pid).
    pub fn image_path(&self) -> Result<String> {
        use windows_sys::Win32::System::Threading::{PROCESS_NAME_WIN32, QueryFullProcessImageNameW};
        let mut buffer = vec![0_u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok = unsafe {
            QueryFullProcessImageNameW(self.handle.as_raw_handle() as HANDLE, PROCESS_NAME_WIN32,
                buffer.as_mut_ptr(), &mut size)
        };
        if ok == 0 || size == 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("QueryFullProcessImageNameW failed for process {}", self.process_id));
        }
        Ok(String::from_utf16_lossy(&buffer[..size as usize]))
    }
}

#[cfg(windows)]
fn process_creation_time(handle: HANDLE) -> Result<u64> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    let ok = unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("GetProcessTimes failed");
    }

    Ok((u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime))
}

#[cfg(windows)]
fn process_handle_is_alive(handle: HANDLE) -> Result<bool> {
    match unsafe { WaitForSingleObject(handle, 0) } {
        WAIT_TIMEOUT => Ok(true),
        WAIT_OBJECT_0 => Ok(false),
        WAIT_FAILED => Err(std::io::Error::last_os_error()).context("WaitForSingleObject failed"),
        result => Err(anyhow!(
            "WaitForSingleObject returned unexpected status {result:#x}"
        )),
    }
}

#[cfg(windows)]
pub struct ProcessJob {
    handle: OwnedHandle,
    explicitly_assigned_processes: Mutex<Vec<OwnedHandle>>,
}

#[cfg(windows)]
impl ProcessJob {
    pub fn new() -> Result<Self> {
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            return Err(std::io::Error::last_os_error()).context("CreateJobObjectW failed");
        }

        let handle = unsafe { OwnedHandle::from_raw_handle(handle as RawHandle) };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        let ok = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("SetInformationJobObject(KILL_ON_JOB_CLOSE) failed");
        }

        Ok(Self {
            handle,
            explicitly_assigned_processes: Mutex::new(Vec::new()),
        })
    }

    pub(crate) fn assign_raw(&self, process_handle: RawHandle) -> Result<()> {
        let pinned_handle = unsafe { BorrowedHandle::borrow_raw(process_handle) }
            .try_clone_to_owned()
            .context("failed to duplicate process handle before job assignment")?;
        self.assign_owned_process_handle(pinned_handle)
    }

    fn assign_owned_process_handle(&self, process_handle: OwnedHandle) -> Result<()> {
        let ok = unsafe {
            AssignProcessToJobObject(
                self.handle.as_raw_handle() as HANDLE,
                process_handle.as_raw_handle() as HANDLE,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("AssignProcessToJobObject failed");
        }

        self.explicitly_assigned_processes
            .lock()
            .map_err(|_| anyhow!("assigned-process handle registry was poisoned"))?
            .push(process_handle);

        Ok(())
    }

    /// Assigns one exact, already pinned process identity to this job.
    ///
    /// External callers never pass a reusable PID or an untracked raw handle;
    /// the same pinned process object is retained for the termination receipt.
    pub fn assign_process(&self, process: &PinnedProcess) -> Result<()> {
        let assignment_handle = unsafe {
            OpenProcess(
                PROCESS_SET_QUOTA
                    | PROCESS_TERMINATE
                    | PROCESS_QUERY_LIMITED_INFORMATION
                    | SYNCHRONIZE,
                0,
                process.pid(),
            )
        };
        if assignment_handle.is_null() {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "OpenProcess failed while preparing to assign process {}",
                    process.pid()
                )
            });
        }
        let assignment_handle = unsafe { OwnedHandle::from_raw_handle(assignment_handle as _) };
        let assignment_creation_time = process_creation_time(
            assignment_handle.as_raw_handle() as HANDLE
        )
        .with_context(|| {
            format!(
                "failed to verify identity before assigning process {}",
                process.pid()
            )
        })?;
        if assignment_creation_time != process.creation_time_filetime() {
            bail!(
                "process {} identity changed before job assignment",
                process.pid()
            );
        }
        self.assign_owned_process_handle(assignment_handle)
    }

    /// Pins a process that was atomically assigned at creation time.
    ///
    /// `spawn_command_in_job` establishes containment before the child can
    /// execute. The returned process handle is registered here so termination
    /// can wait for the exact root process object, not only for job accounting
    /// to stop listing it.
    pub(crate) fn track_creation_assigned_process(&self, process_handle: RawHandle) -> Result<()> {
        let pinned_handle = unsafe { BorrowedHandle::borrow_raw(process_handle) }
            .try_clone_to_owned()
            .context("failed to duplicate creation-assigned process handle")?;
        self.explicitly_assigned_processes
            .lock()
            .map_err(|_| anyhow!("assigned-process handle registry was poisoned"))?
            .push(pinned_handle);
        Ok(())
    }

    pub fn raw_handle(&self) -> RawHandle {
        self.handle.as_raw_handle()
    }

    pub fn assign_current_process(&self) -> Result<()> {
        let current_process = unsafe { GetCurrentProcess() };
        self.assign_raw(current_process as RawHandle)
    }

    pub fn live_process_ids(&self) -> Result<Vec<u32>> {
        let mut capacity = 16_usize;

        loop {
            let bytes = std::mem::size_of::<JOBOBJECT_BASIC_PROCESS_ID_LIST>()
                + capacity.saturating_sub(1) * std::mem::size_of::<usize>();
            let mut buffer = vec![0_u8; bytes];
            let ok = unsafe {
                QueryInformationJobObject(
                    self.handle.as_raw_handle() as HANDLE,
                    JobObjectBasicProcessIdList,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    std::ptr::null_mut(),
                )
            };
            let info = unsafe { &*(buffer.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST) };

            if ok != 0 {
                let listed = info.NumberOfProcessIdsInList as usize;
                let assigned = info.NumberOfAssignedProcesses as usize;
                if assigned > listed && assigned > capacity {
                    capacity = assigned.saturating_add(4);
                    continue;
                }

                let process_ids =
                    unsafe { std::slice::from_raw_parts(info.ProcessIdList.as_ptr(), listed) };
                return Ok(process_ids
                    .iter()
                    .filter_map(|pid| u32::try_from(*pid).ok())
                    .collect());
            }

            if capacity >= 4096 {
                return Err(std::io::Error::last_os_error())
                    .context("QueryInformationJobObject(BasicProcessIdList) failed");
            }
            capacity *= 2;
        }
    }

    /// Snapshot-only diagnostic. Never retain a PID from this API as authority.
    pub fn contains_process_id(&self, process_id: u32) -> Result<bool> {
        Ok(self.live_process_ids()?.contains(&process_id))
    }

    /// Checks job membership through an already pinned process handle.
    ///
    /// Security-sensitive callers should prefer this over retaining a PID and
    /// later calling `contains_process_id`, because Windows can reuse PIDs.
    pub fn contains_process(&self, process: &PinnedProcess) -> Result<bool> {
        if !process.is_alive()? {
            return Ok(false);
        }

        let mut result = 0;
        let ok = unsafe {
            IsProcessInJob(
                process.handle.as_raw_handle() as HANDLE,
                self.handle.as_raw_handle() as HANDLE,
                &mut result,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("IsProcessInJob failed");
        }

        Ok(result != 0)
    }

    fn basic_accounting(&self) -> Result<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION> {
        let mut information = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle() as HANDLE,
                JobObjectBasicAccountingInformation,
                (&mut information as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("QueryInformationJobObject(BasicAccountingInformation) failed");
        }
        Ok(information)
    }

    fn active_process_count(&self) -> Result<u32> {
        Ok(self.basic_accounting()?.ActiveProcesses)
    }

    /// Non-destructive whole-scope exit proof. Accounting can reach zero before
    /// an explicitly assigned root handle becomes signaled, so require both.
    pub(crate) fn has_exited(&self) -> Result<bool> {
        if self.active_process_count()? != 0 {
            return Ok(false);
        }
        let assigned = self.explicitly_assigned_processes
            .lock()
            .map_err(|_| anyhow!("assigned-process handle registry was poisoned"))?;
        for process in assigned.iter() {
            if process_handle_is_alive(process.as_raw_handle() as HANDLE)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Resource accounting for telemetry. The CPU time covers every process
    /// that ever ran in the job, exited ones included, so a delta between two
    /// readings keeps short-lived children; the peak is the kernel's own
    /// record of the job's commit charge since it was created. Each listed
    /// member is read through a fresh handle that is rechecked against this
    /// job, so an ID reused between listing and opening is never counted.
    pub fn accounting(&self, image_names: bool) -> Result<crate::ScopeAccounting> {
        let basic = self.basic_accounting()?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        let ok = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle() as HANDLE,
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("QueryInformationJobObject(ExtendedLimitInformation) failed");
        }
        let mut members = Vec::new();
        let mut unreadable = Vec::new();
        for process_id in self.live_process_ids()? {
            match crate::usage::read_job_member(
                self.handle.as_raw_handle() as HANDLE,
                process_id,
                image_names,
            ) {
                crate::usage::MemberReading::Measured(usage) => members.push(usage),
                crate::usage::MemberReading::Gone => {}
                crate::usage::MemberReading::Unreadable => unreadable.push(process_id),
            }
        }
        let ticks = |value: i64| u64::try_from(value).unwrap_or(0);
        Ok(crate::ScopeAccounting {
            cpu_time_100ns: ticks(basic.TotalUserTime).saturating_add(ticks(basic.TotalKernelTime)),
            peak_memory_bytes: limits.PeakJobMemoryUsed as u64,
            members,
            unreadable,
        })
    }

    pub fn terminate(&self) -> Result<()> {
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, 1) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("TerminateJobObject failed");
        }

        // TerminateJobObject applies asynchronous process termination. Job
        // accounting can reach zero slightly before a process handle becomes
        // signaled, so prove both facts: every explicit root has terminated,
        // and the job has no active root or descendant left.
        let deadline = Instant::now() + JOB_TERMINATION_WAIT;
        let assigned_processes = self
            .explicitly_assigned_processes
            .lock()
            .map_err(|_| anyhow!("assigned-process handle registry was poisoned"))?;
        for process in assigned_processes.iter() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            let timeout_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
            match unsafe { WaitForSingleObject(process.as_raw_handle() as HANDLE, timeout_ms) } {
                WAIT_OBJECT_0 => {}
                WAIT_TIMEOUT => bail!(
                    "TerminateJobObject succeeded but an assigned root process was not signaled after {}ms",
                    JOB_TERMINATION_WAIT.as_millis()
                ),
                WAIT_FAILED => {
                    return Err(std::io::Error::last_os_error())
                        .context("WaitForSingleObject failed for an assigned root process");
                }
                result => bail!(
                    "WaitForSingleObject returned unexpected status {result:#x} for an assigned root process"
                ),
            }
        }
        drop(assigned_processes);

        loop {
            let active_processes = self.active_process_count()?;
            if active_processes == 0 {
                return Ok(());
            }
            if Instant::now() >= deadline {
                bail!(
                    "TerminateJobObject succeeded but {active_processes} process(es) remained active after {}ms",
                    JOB_TERMINATION_WAIT.as_millis()
                );
            }
            thread::sleep(JOB_TERMINATION_POLL);
        }
    }
}

/// The job the application process put itself into at startup. Every process
/// it starts, directly or not, inherits it and cannot break away, so its
/// members are the application's whole process tree, live orphans included.
#[cfg(windows)]
static APP_JOB: OnceLock<ProcessJob> = OnceLock::new();

/// Keeps the startup job for the life of the process and makes it the
/// application job. The caller has already assigned the current process to
/// it. Registration happens once.
#[cfg(windows)]
pub fn register_app_job(job: ProcessJob) -> Result<&'static ProcessJob> {
    APP_JOB
        .set(job)
        .map_err(|_| anyhow!("the application job is already registered"))?;
    Ok(APP_JOB
        .get()
        .expect("the application job was registered above"))
}

/// The application job, when this process registered one at startup.
#[cfg(windows)]
pub fn app_job() -> Option<&'static ProcessJob> {
    APP_JOB.get()
}

#[cfg(not(windows))]
pub struct ProcessJob;

#[cfg(not(windows))]
impl ProcessJob {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self)
    }

    pub fn assign_current_process(&self) -> anyhow::Result<()> {
        Ok(())
    }

    pub fn live_process_ids(&self) -> anyhow::Result<Vec<u32>> {
        Ok(Vec::new())
    }

    /// Snapshot-only diagnostic. Never retain a PID from this API as authority.
    pub fn contains_process_id(&self, _process_id: u32) -> anyhow::Result<bool> {
        Ok(false)
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::{PinnedProcess, ProcessJob};
    use std::{
        fs,
        os::windows::{io::AsRawHandle, process::CommandExt},
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    struct KillChild(Child);

    impl Drop for KillChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    struct RemoveDir(PathBuf);

    impl Drop for RemoveDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn powershell_literal(path: &Path) -> String {
        path.display().to_string().replace('\'', "''")
    }

    #[test]
    fn job_object_kills_assigned_process_on_close() {
        let job = ProcessJob::new().expect("create job");
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 100 127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");

        job.assign_raw(child.as_raw_handle())
            .expect("assign child to job");
        drop(job);

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Ok(None) => panic!("child survived job drop"),
                Err(error) => panic!("try_wait failed: {error}"),
            }
        }
    }

    #[test]
    fn pinned_process_reports_its_own_image_path() {
        let process = PinnedProcess::open(std::process::id()).expect("pin self");
        let image = process.image_path().expect("image path");
        let expected = std::env::current_exe().unwrap();
        assert!(image.eq_ignore_ascii_case(&expected.display().to_string()), "{image}");
    }

    #[test]
    fn process_job_drops_cleanly_with_no_assigned_processes() {
        let job = ProcessJob::new().expect("create job");
        drop(job);
    }

    #[test]
    fn job_object_lists_assigned_process_id() {
        let job = ProcessJob::new().expect("create job");
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 100 127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        job.assign_raw(child.as_raw_handle())
            .expect("assign child to job");

        let process_ids = job.live_process_ids().expect("query job pids");
        assert!(process_ids.contains(&child.id()));

        let _ = child.kill();
    }

    #[test]
    fn terminate_returns_only_after_the_job_is_empty_and_root_has_exited() {
        let job = ProcessJob::new().expect("create job");
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 100 127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn child");
        job.assign_raw(child.as_raw_handle())
            .expect("assign child to job");

        job.terminate().expect("terminate and drain job");

        assert!(
            job.live_process_ids()
                .expect("query drained job")
                .is_empty()
        );
        assert!(
            child.try_wait().expect("query terminated root").is_some(),
            "job termination returned before the root process was signaled"
        );
    }

    #[test]
    fn process_job_contains_assigned_root_and_descendant_but_not_unassigned_peer() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let test_dir = std::env::temp_dir().join(format!(
            "prim1-process-job-membership-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&test_dir).expect("create membership test directory");
        let _test_dir = RemoveDir(test_dir.clone());
        let release_path = test_dir.join("release");
        let descendant_pid_path = test_dir.join("descendant.pid");
        let script = format!(
            "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 10 }}; \
             $child = Start-Process -FilePath $env:ComSpec -ArgumentList '/c ping -n 100 127.0.0.1' -PassThru; \
             [IO.File]::WriteAllText('{}', [string]$child.Id); \
             Wait-Process -Id $child.Id",
            powershell_literal(&release_path),
            powershell_literal(&descendant_pid_path),
        );

        let job = ProcessJob::new().expect("create process job");
        let root = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(KillChild)
            .expect("spawn assigned PowerShell root");
        let pinned_root = PinnedProcess::open(root.0.id()).expect("pin assigned root");
        job.assign_process(&pinned_root)
            .expect("assign PowerShell root to job");

        let peer = Command::new("cmd")
            .args(["/c", "ping -n 100 127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(KillChild)
            .expect("spawn unassigned peer");

        let pinned_peer = PinnedProcess::open(peer.0.id()).expect("pin unassigned peer");

        assert!(
            job.contains_process_id(root.0.id())
                .expect("query assigned root membership")
        );
        assert!(
            !job.contains_process_id(peer.0.id())
                .expect("query unassigned peer membership")
        );
        assert!(
            job.contains_process(&pinned_root)
                .expect("query pinned assigned root membership")
        );
        assert!(
            !job.contains_process(&pinned_peer)
                .expect("query pinned unassigned peer membership")
        );

        fs::write(&release_path, b"release").expect("release assigned PowerShell root");
        let deadline = Instant::now() + Duration::from_secs(5);
        let descendant_pid = loop {
            if let Ok(raw) = fs::read_to_string(&descendant_pid_path)
                && let Ok(process_id) = raw.trim().parse::<u32>()
            {
                break process_id;
            }
            assert!(
                Instant::now() < deadline,
                "assigned descendant PID was not published"
            );
            std::thread::sleep(Duration::from_millis(20));
        };

        assert!(
            job.contains_process_id(descendant_pid)
                .expect("query inherited descendant membership")
        );
        let pinned_descendant =
            PinnedProcess::open(descendant_pid).expect("pin assigned descendant");
        assert!(
            job.contains_process(&pinned_descendant)
                .expect("query pinned inherited descendant membership")
        );

        job.terminate().expect("terminate membership test job");
        assert!(
            !pinned_root.is_alive().expect("query terminated root"),
            "job termination returned before the assigned root was signaled"
        );
        assert!(
            !pinned_descendant
                .is_alive()
                .expect("query terminated descendant"),
            "job termination returned before the inherited descendant was signaled"
        );
        assert!(
            job.live_process_ids()
                .expect("query terminated job")
                .is_empty(),
            "job termination returned with an active process still assigned"
        );
    }

    #[test]
    fn job_accounting_sees_a_completed_allocation_in_the_member_and_the_kernel_peak() {
        const BLOCK_BYTES: u64 = 64 * 1024 * 1024;
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before Unix epoch")
            .as_nanos();
        let test_dir = std::env::temp_dir().join(format!(
            "prim1-process-job-accounting-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&test_dir).expect("create accounting test directory");
        let _test_dir = RemoveDir(test_dir.clone());
        let release_path = test_dir.join("release");
        let allocated_path = test_dir.join("allocated");
        // Commits a 64 MiB block only after it has joined the job, then says
        // so; the block stays referenced while the test reads.
        let script = format!(
            "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 10 }}; \
             $block = New-Object byte[] {BLOCK_BYTES}; $block[0] = 1; \
             [IO.File]::WriteAllText('{}', 'allocated'); Start-Sleep -Seconds 30; $block[1] = 1",
            powershell_literal(&release_path),
            powershell_literal(&allocated_path),
        );

        let job = ProcessJob::new().expect("create process job");
        let root = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(script)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .map(KillChild)
            .expect("spawn PowerShell root");
        let pinned_root = PinnedProcess::open(root.0.id()).expect("pin PowerShell root");
        job.assign_process(&pinned_root)
            .expect("assign PowerShell root to job");
        let root_reading = |accounting: &crate::ScopeAccounting| {
            accounting
                .members
                .iter()
                .find(|member| member.pid == root.0.id())
                .cloned()
                .unwrap_or_else(|| panic!("the root is a measured member: {accounting:?}"))
        };
        let before = job
            .accounting(true)
            .expect("read accounting before the block");
        let root_before = root_reading(&before);
        assert_eq!(root_before.created, pinned_root.creation_time_filetime());
        assert_eq!(
            root_before
                .image_name
                .as_deref()
                .map(str::to_ascii_lowercase)
                .as_deref(),
            Some("powershell.exe")
        );
        assert!(before.unreadable.is_empty(), "{before:?}");

        fs::write(&release_path, b"release").expect("release PowerShell root");
        let deadline = Instant::now() + Duration::from_secs(15);
        while !allocated_path.exists() {
            assert!(
                Instant::now() < deadline,
                "PowerShell never acknowledged the allocation"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        let after = job
            .accounting(false)
            .expect("read accounting after the block");
        let root_after = root_reading(&after);
        // The member's own reading holds the block...
        assert!(
            root_after.private_bytes >= root_before.private_bytes + BLOCK_BYTES * 15 / 16,
            "the member's commit did not grow by the block: {root_before:?} -> {root_after:?}"
        );
        // ...and the kernel's job peak rose with it. Half the block is far
        // above PowerShell's own drift, and the peak was read before the
        // allocation, so only the block explains the rise.
        assert!(
            after.peak_memory_bytes >= before.peak_memory_bytes + BLOCK_BYTES / 2,
            "the job peak did not follow the allocation: {before:?} -> {after:?}"
        );
        assert!(
            after.cpu_time_100ns > before.cpu_time_100ns,
            "allocating and zeroing the block takes CPU: {before:?} -> {after:?}"
        );

        job.terminate().expect("terminate accounting test job");
        let drained = job
            .accounting(false)
            .expect("read accounting after termination");
        assert!(
            drained.members.is_empty() && drained.unreadable.is_empty(),
            "{drained:?}"
        );
        assert!(
            drained.cpu_time_100ns >= after.cpu_time_100ns,
            "exited processes keep their CPU time in the job: {after:?} -> {drained:?}"
        );
    }

    #[test]
    fn a_listed_member_is_rechecked_against_the_job_through_its_own_handle() {
        use crate::usage::{MemberReading, read_job_member};
        use windows_sys::Win32::Foundation::HANDLE;

        let sleeper = || {
            Command::new("ping")
                .args(["-n", "30", "127.0.0.1"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map(KillChild)
                .expect("spawn ping")
        };
        let job = ProcessJob::new().expect("create process job");
        let member = sleeper();
        let pinned_member = PinnedProcess::open(member.0.id()).expect("pin member");
        job.assign_process(&pinned_member)
            .expect("assign member to job");
        let outsider = sleeper();
        let job_handle = job.raw_handle() as HANDLE;

        // An ID listed by the job that now names a process outside it (the
        // reuse race between listing and opening) is not a member.
        assert!(
            matches!(
                read_job_member(job_handle, outsider.0.id(), false),
                MemberReading::Gone
            ),
            "an outsider was read as a member"
        );
        // An ID no process has is gone as well.
        assert!(matches!(
            read_job_member(job_handle, u32::MAX - 3, false),
            MemberReading::Gone
        ));
        match read_job_member(job_handle, member.0.id(), true) {
            MemberReading::Measured(usage) => {
                assert_eq!(usage.pid, member.0.id());
                assert_eq!(usage.created, pinned_member.creation_time_filetime());
                assert!(usage.private_bytes > 0, "{usage:?}");
                assert_eq!(
                    usage
                        .image_name
                        .as_deref()
                        .map(str::to_ascii_lowercase)
                        .as_deref(),
                    Some("ping.exe")
                );
            }
            other => panic!("the member was not measured: {other:?}"),
        }

        let accounting = job.accounting(false).expect("account the job");
        let pids = accounting
            .members
            .iter()
            .map(|usage| usage.pid)
            .collect::<Vec<_>>();
        assert!(pids.contains(&member.0.id()), "{accounting:?}");
        assert!(!pids.contains(&outsider.0.id()), "{accounting:?}");
    }

    #[test]
    fn pinned_process_tracks_one_identity_from_live_through_exit() {
        let mut child = Command::new("cmd")
            .args(["/c", "ping -n 100 127.0.0.1"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(KillChild)
            .expect("spawn process to pin");
        let process_id = child.0.id();

        let pinned = PinnedProcess::open(process_id).expect("pin live process");
        let independently_pinned =
            PinnedProcess::open(process_id).expect("independently pin same live process");
        assert_eq!(pinned.pid(), process_id);
        assert!(pinned.is_alive().expect("query live pinned process"));
        assert!(pinned.same_identity(&independently_pinned));

        child.0.kill().expect("kill pinned process");
        child.0.wait().expect("wait for pinned process exit");

        assert!(!pinned.is_alive().expect("query exited pinned process"));
        assert!(pinned.same_identity(&independently_pinned));
        assert!(
            PinnedProcess::open(process_id).is_err(),
            "an exited process must not create new authority from its PID"
        );
    }
}
