//! Resource readings of single processes, for telemetry. Every field of one
//! reading comes through one handle, so the process cannot change identity
//! halfway through it.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle, RawHandle};

use anyhow::{Context, Result};
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_PARAMETER, FILETIME, HANDLE, WAIT_TIMEOUT},
    Storage::FileSystem::SYNCHRONIZE,
    System::{
        JobObjects::IsProcessInJob,
        ProcessStatus::{
            K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
        },
        Threading::{
            ALL_PROCESSOR_GROUPS, GetActiveProcessorCount, GetCurrentProcess, GetCurrentProcessId,
            GetProcessTimes, OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
            QueryFullProcessImageNameW, WaitForSingleObject,
        },
    },
};

use crate::ProcessUsage;

/// What one listed job member turned out to be when it was opened.
#[derive(Debug)]
pub(crate) enum MemberReading {
    Measured(ProcessUsage),
    /// Exited, or its ID now names a process outside the job.
    Gone,
    /// Alive, but it could not be opened or read with query rights.
    Unreadable,
}

/// Reads one listed member of `job` through a fresh handle. Membership is
/// rechecked on that handle, because a listed ID can be reused by another
/// process before it is opened.
pub(crate) fn read_job_member(job: HANDLE, process_id: u32, image_name: bool) -> MemberReading {
    let raw = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE,
            0,
            process_id,
        )
    };
    if raw.is_null() {
        let error = std::io::Error::last_os_error();
        return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
            MemberReading::Gone
        } else {
            MemberReading::Unreadable
        };
    }
    let owned = unsafe { OwnedHandle::from_raw_handle(raw as RawHandle) };
    let handle = owned.as_raw_handle() as HANDLE;
    // An exited process stays openable while anyone holds a handle to it.
    if unsafe { WaitForSingleObject(handle, 0) } != WAIT_TIMEOUT {
        return MemberReading::Gone;
    }
    let mut in_job = 0;
    if unsafe { IsProcessInJob(handle, job, &mut in_job) } == 0 {
        return MemberReading::Unreadable;
    }
    if in_job == 0 {
        return MemberReading::Gone;
    }
    match read_usage(process_id, handle, image_name) {
        Ok(usage) => MemberReading::Measured(usage),
        Err(_) => MemberReading::Unreadable,
    }
}

/// This process's own reading.
pub fn current_process_usage() -> Result<ProcessUsage> {
    let process_id = unsafe { GetCurrentProcessId() };
    read_usage(process_id, unsafe { GetCurrentProcess() }, false)
}

/// Logical processors across every processor group: what Task Manager's CPU
/// column is a share of.
pub fn logical_processor_count() -> u32 {
    match unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) } {
        0 => std::thread::available_parallelism()
            .map_or(1, |count| u32::try_from(count.get()).unwrap_or(u32::MAX)),
        count => count,
    }
}

fn read_usage(process_id: u32, handle: HANDLE, image_name: bool) -> Result<ProcessUsage> {
    let mut created = FILETIME::default();
    let mut exited = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    if unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) } == 0 {
        return Err(std::io::Error::last_os_error()).context("GetProcessTimes failed");
    }
    let mut memory = PROCESS_MEMORY_COUNTERS_EX {
        cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32,
        ..Default::default()
    };
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            handle,
            (&mut memory as *mut PROCESS_MEMORY_COUNTERS_EX).cast::<PROCESS_MEMORY_COUNTERS>(),
            memory.cb,
        )
    };
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("GetProcessMemoryInfo failed");
    }
    Ok(ProcessUsage {
        pid: process_id,
        created: ticks(created),
        cpu_time_100ns: ticks(kernel).saturating_add(ticks(user)),
        private_bytes: memory.PrivateUsage as u64,
        image_name: if image_name {
            image_file_name(handle)
        } else {
            None
        },
    })
}

/// The executable's file name, such as "msedgewebview2.exe".
fn image_file_name(handle: HANDLE) -> Option<String> {
    let mut buffer = [0_u16; 1024];
    let mut size = buffer.len() as u32;
    let ok = unsafe {
        QueryFullProcessImageNameW(handle, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
    };
    if ok == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..size as usize]);
    path.rsplit(['\\', '/']).next().map(str::to_owned)
}

fn ticks(time: FILETIME) -> u64 {
    (u64::from(time.dwHighDateTime) << 32) | u64::from(time.dwLowDateTime)
}
