#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

#[cfg(windows)]
use anyhow::{Context, Result};

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::HANDLE,
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject,
    },
    System::Threading::GetCurrentProcess,
};

#[cfg(windows)]
pub struct ProcessJob {
    handle: OwnedHandle,
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

        Ok(Self { handle })
    }

    pub fn assign_raw(&self, process_handle: RawHandle) -> Result<()> {
        let ok = unsafe {
            AssignProcessToJobObject(
                self.handle.as_raw_handle() as HANDLE,
                process_handle as HANDLE,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("AssignProcessToJobObject failed");
        }

        Ok(())
    }

    pub fn assign_current_process(&self) -> Result<()> {
        let current_process = unsafe { GetCurrentProcess() };
        self.assign_raw(current_process as RawHandle)
    }

    pub fn terminate(&self) -> Result<()> {
        let ok = unsafe { TerminateJobObject(self.handle.as_raw_handle() as HANDLE, 1) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error()).context("TerminateJobObject failed");
        }

        Ok(())
    }
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
}

#[cfg(all(test, windows))]
mod tests {
    use super::ProcessJob;
    use std::{
        os::windows::io::AsRawHandle,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

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
    fn process_job_drops_cleanly_with_no_assigned_processes() {
        let job = ProcessJob::new().expect("create job");
        drop(job);
    }
}
