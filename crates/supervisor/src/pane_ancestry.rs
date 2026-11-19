//! A16 (PC3 walk, 20 Oct 13:34Z): who may use the operator entry and guest
//! tokens. The kernel check ("in no running pane's Job") alone admitted a
//! no-grant pane's own shell: on a PRIM-1 started by its launcher task the
//! pane's tool processes were not in the pane's Job (cause in decision audit
//! 62). A client that no pane's Job claims is therefore walked up its parent
//! chain; one that descends from a pane is that pane, refused there and left
//! to its member tools and grants. Every decision is written to the audit log.
use super::*;

/// Beyond this many parents the chain is not believed: refused.
const PANE_ANCESTRY_MAX_HOPS: usize = 32;

/// One step up a caller's parent chain.
pub(super) enum Parent {
    /// Pinned, alive and created no later than its child.
    Pinned(Arc<dyn PaneProcess>),
    /// The child has no parent left to ask (the system's own processes).
    Root,
    /// The recorded parent has exited, or its id now names a younger process.
    Gone(u32),
    /// The parent exists but this user may not query it (a service host, an
    /// elevated process).
    Unreadable(u32),
    /// A link that could not be verified: this process's parent id, or the
    /// parent itself, could not be read for a reason other than a known end
    /// (start times, liveness, resources). The walk proves nothing: refused.
    Indeterminate(u32),
}

pub(super) trait ProcessAncestry {
    fn parent_of(&self, child: &dyn PaneProcess) -> Parent;
}

/// The kernel's record of who created each process, verified by start time:
/// the parent id from `NtQueryInformationProcess`, then the parent pinned
/// (`GetProcessTimes` on a query handle) and accepted only if it is older.
#[cfg(windows)]
pub(super) struct WindowsAncestry;

#[cfg(windows)]
impl ProcessAncestry for WindowsAncestry {
    fn parent_of(&self, child: &dyn PaneProcess) -> Parent {
        let Some(parent) = identity_tracker::parent_process_id(child.pid()) else {
            return Parent::Indeterminate(child.pid());
        };
        if parent == 0 || parent == 4 {
            return Parent::Root;
        }
        // Classified on the open itself, before the pin's own queries.
        let open_error = {
            use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
            use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
            let probe = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, parent) };
            if probe.is_null() {
                Some(unsafe { GetLastError() })
            } else {
                unsafe { CloseHandle(probe) };
                None
            }
        };
        if open_error.is_some() {
            return unpinned_parent(parent, open_error, None);
        }
        let pinned = match WindowsPaneProcess::open(parent) {
            Ok(pinned) => pinned,
            Err(error) => return unpinned_parent(parent, None, Some(&error)),
        };
        match (pinned.creation_time(), child.creation_time()) {
            (Some(parent_created), Some(child_created)) if parent_created <= child_created => Parent::Pinned(Arc::new(pinned)),
            // A younger process holds the id: the real parent has exited.
            (Some(_), Some(_)) => Parent::Gone(parent),
            _ => Parent::Indeterminate(parent),
        }
    }
}

/// A parent that could not be pinned counts as a known end only when that is
/// known: no such process (`ERROR_INVALID_PARAMETER` from the open) or the
/// process found already exited is gone; `ERROR_ACCESS_DENIED` from the open
/// is unreadable (a service host, an elevated process). Any other open, start
/// time or liveness failure establishes nothing (the Reviewer's ff7a97e hold).
#[cfg(windows)]
pub(super) fn unpinned_parent(parent: u32, open_error: Option<u32>, pin_error: Option<&anyhow::Error>) -> Parent {
    const ERROR_ACCESS_DENIED: u32 = 5;
    const ERROR_INVALID_PARAMETER: u32 = 87;
    match (open_error, pin_error) {
        (Some(ERROR_INVALID_PARAMETER), _) => Parent::Gone(parent),
        (Some(ERROR_ACCESS_DENIED), _) => Parent::Unreadable(parent),
        (None, Some(error)) if error.is::<pty_host::ProcessAlreadyExited>() => Parent::Gone(parent),
        _ => Parent::Indeterminate(parent),
    }
}

/// How a pipe client was admitted, and the line the audit log keeps for it.
pub(super) struct Attributed {
    pub(super) attribution: JobAttribution,
    pub(super) trace: String,
}

impl From<JobAttribution> for Attributed {
    fn from(attribution: JobAttribution) -> Self {
        let trace = match &attribution {
            JobAttribution::Pane(caller) => format!("pane '{}' by its Job", caller.session),
            JobAttribution::Outside(pid) => format!("process {pid} outside every pane"),
            JobAttribution::Unknown => "not proven outside the panes".into(),
        };
        Self { attribution, trace }
    }
}

/// "process 1092 (session 1, started 2026-10-20T13:37:55.825Z)".
pub(super) fn describe_process(pid: u32, session: Option<u32>, created: Option<u64>) -> String {
    let session = session.map(|session| format!("session {session}")).unwrap_or_else(|| "session unknown".into());
    let started = created.and_then(filetime_rfc3339).map(|at| format!(", started {at}")).unwrap_or_default();
    format!("process {pid} ({session}{started})")
}

fn filetime_rfc3339(filetime: u64) -> Option<String> {
    const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;
    let since_epoch = filetime.checked_sub(UNIX_EPOCH_FILETIME)?;
    let seconds = i64::try_from(since_epoch / 10_000_000).ok()?;
    let nanos = u32::try_from(since_epoch % 10_000_000 * 100).ok()?;
    DateTime::<Utc>::from_timestamp(seconds, nanos).map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

impl SupervisorHandle {
    /// A client that no running pane's Job claims (`OutsideEveryPane`). It is
    /// outside only if neither it nor its verified parent chain is a pane: a
    /// pane's own process, or a process in a running or closing pane's Job.
    /// Where the chain ends at a known end without reaching a pane (the
    /// system, an unreadable service host, an exited parent) it stays
    /// outside: a process deliberately started outside the panes (a scheduled
    /// task, ssh localhost, an orphan whose parent exited) is the documented
    /// same-user limit. A link that could not be established, or an
    /// implausibly long chain, proves nothing: refused.
    pub(super) fn attribute_unclaimed(&self, process: &dyn PaneProcess, session: Option<u32>,
        ancestry: &dyn ProcessAncestry) -> Attributed {
        let who = describe_process(process.pid(), session, process.creation_time());
        let mut chain: Vec<Arc<dyn PaneProcess>> = Vec::new();
        let mut cursor: Option<Arc<dyn PaneProcess>> = None;
        // Ok: a known end that the same-user limit covers; Err: no proof.
        let end: std::result::Result<String, String> = loop {
            if chain.len() == PANE_ANCESTRY_MAX_HOPS {
                break Err(format!("is longer than {PANE_ANCESTRY_MAX_HOPS} and was not believed"));
            }
            let parent = match &cursor {
                Some(child) => ancestry.parent_of(child.as_ref()),
                None => ancestry.parent_of(process),
            };
            match parent {
                Parent::Pinned(parent) => {
                    chain.push(parent.clone());
                    cursor = Some(parent);
                }
                Parent::Root => break Ok("the system".to_string()),
                Parent::Gone(pid) => break Ok(format!("parent {pid}, which has exited")),
                Parent::Unreadable(pid) => break Ok(format!("process {pid}, which this user may not query")),
                Parent::Indeterminate(pid) => break Err(format!("stops at {pid}, a link that could not be verified")),
            }
        };
        let path = std::iter::once(process.pid()).chain(chain.iter().map(|ancestor| ancestor.pid()))
            .map(|pid| pid.to_string()).collect::<Vec<_>>().join(" -> ");
        // The caller itself first: a pane's own process that its Job does not
        // claim is that pane (the Reviewer's c4cfaa6 hold).
        let pane = {
            let slots = self.inner.slots.lock();
            std::iter::once(process).chain(chain.iter().map(|ancestor| ancestor.as_ref()))
                .find_map(|candidate| slots.by_id.values().find_map(|slot| {
                    let pty = slot.running.as_ref()?.pty.as_deref()?;
                    let owns = slot.process_id == Some(candidate.pid()) || !matches!(candidate.belongs_to(pty), Ok(false));
                    owns.then(|| format!("{} is pane '{}'", candidate.pid(), slot.definition.label))
                }))
        };
        match (pane, end) {
            (Some(pane), _) => Attributed { attribution: JobAttribution::Unknown,
                trace: format!("{who}: in no pane's Job, but {path} reaches {pane}: a pane or its descendant outside its Job") },
            (None, Err(why)) => Attributed { attribution: JobAttribution::Unknown,
                trace: format!("{who}: in no pane's Job; parent chain {path} {why}: not proven outside") },
            (None, Ok(end)) => Attributed { attribution: JobAttribution::Outside(process.pid()),
                trace: format!("{who}: in no pane's Job; parent chain {path} ends at {end} without reaching a pane") },
        }
    }

    /// One audit line per operator-entry or guest admission decision: who,
    /// how it was decided and the verdict. Never a token or a request body.
    pub(super) fn audit_admission(&self, door: &str, admitted: bool, trace: &str) {
        self.emit(RuntimeEvent::SystemLog {
            level: if admitted { LogLevel::Info } else { LogLevel::Warn },
            message: format!("{door} {}: {trace}", if admitted { "admitted" } else { "refused" }),
            timestamp: now_rfc3339(),
        });
    }
}
