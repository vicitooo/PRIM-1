//! Resource telemetry (PLAN D13, E7): PRIM-1's true footprint, split so that
//! no process is counted twice.
//!
//! The inventory is the application Job PRIM-1 puts itself into at startup:
//! every process it starts, directly or not, belongs to it and cannot break
//! away, so live orphans are included and no parent chain is trusted.
//!
//! - **core**: the PRIM-1 process itself;
//! - **one row per live run**: the members of that run's Job;
//! - **webview**: the other application-Job members that are `msedgewebview2.exe`;
//! - **helpers**: every remaining member, chiefly the console host
//!   (conhost.exe) ConPTY starts beside each terminal.
//!
//! Every reading of a process comes through one handle that is rechecked
//! against its Job, so a process ID reused between listing and opening is
//! never counted. Pull-based: nothing is measured unless the renderer asks.
//! The sampler keeps only the previous sample's cumulative CPU times (for
//! deltas) and each live run's peaks.
#![cfg_attr(not(windows), allow(dead_code))]

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
    time::{Duration, Instant},
};

use pty_host::{ProcessUsage, PtySession};
use shared_types::{
    DriverKind, RunId, SessionId, TelemetrySessionUsage, TelemetrySnapshot, TelemetryUsage,
};

/// Below this window CPU time is mostly scheduler-tick noise (process times
/// advance in ~15.6 ms steps), and a noisy reading would become a sticky peak.
const MIN_CPU_WINDOW: Duration = Duration::from_millis(500);
/// Past this, "CPU now" would average minutes nobody was watching, so the
/// sample starts a fresh baseline instead of reporting one.
const MAX_CPU_WINDOW: Duration = Duration::from_secs(10);
const WEBVIEW_IMAGE: &str = "msedgewebview2.exe";

/// A run with a PTY at the moment the session lock was held.
pub(crate) struct LiveRun {
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) label: String,
    pub(crate) driver: DriverKind,
    pub(crate) pty: Arc<dyn PtySession>,
}

/// One process outside every session, read at one instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProcessReading {
    pub(crate) pid: u32,
    /// FILETIME ticks: 100 ns units since 1601-01-01 UTC.
    pub(crate) created: u64,
    pub(crate) cpu_time_100ns: u64,
    pub(crate) private_bytes: u64,
}

impl From<&ProcessUsage> for ProcessReading {
    fn from(usage: &ProcessUsage) -> Self {
        Self {
            pid: usage.pid,
            created: usage.created,
            cpu_time_100ns: usage.cpu_time_100ns,
            private_bytes: usage.private_bytes,
        }
    }
}

/// One live run, read at one instant.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RunReading {
    pub(crate) session_id: SessionId,
    pub(crate) run_id: RunId,
    pub(crate) label: String,
    pub(crate) driver: DriverKind,
    /// The Job's cumulative CPU time, exited processes included.
    pub(crate) cpu_time_100ns: u64,
    /// The kernel's record of the Job's peak commit charge.
    pub(crate) peak_job_memory_bytes: u64,
    pub(crate) private_bytes: u64,
    pub(crate) processes: u32,
}

/// Everything one sample reads from the OS, before any delta.
#[derive(Debug, Clone)]
pub(crate) struct Readings {
    pub(crate) at: Instant,
    /// `at` as FILETIME ticks, to tell a process born since the previous
    /// sample from one seen for the first time.
    pub(crate) at_filetime: u64,
    pub(crate) sampled_at: String,
    pub(crate) logical_cpus: u32,
    pub(crate) core: ProcessReading,
    pub(crate) webview: Vec<ProcessReading>,
    pub(crate) helpers: Vec<ProcessReading>,
    pub(crate) runs: Vec<RunReading>,
    /// Listed process IDs with a failed reading in at least one inventory.
    /// Memory/counts may be incomplete; run CPU still uses Job accounting.
    pub(crate) unreadable_processes: u32,
}

/// The previous sample's cumulative CPU times.
struct Baseline {
    at: Instant,
    at_filetime: u64,
    core_cpu_time: u64,
    /// Webview and helper processes by (pid, creation time): Windows reuses
    /// process IDs.
    processes: HashMap<(u32, u64), u64>,
    runs: HashMap<RunId, u64>,
}

impl Baseline {
    fn of(readings: &Readings) -> Self {
        Self {
            at: readings.at,
            at_filetime: readings.at_filetime,
            core_cpu_time: readings.core.cpu_time_100ns,
            processes: readings
                .webview
                .iter()
                .chain(&readings.helpers)
                .map(|process| ((process.pid, process.created), process.cpu_time_100ns))
                .collect(),
            runs: readings
                .runs
                .iter()
                .map(|run| (run.run_id, run.cpu_time_100ns))
                .collect(),
        }
    }

    /// CPU a webview or helper process used since this baseline. One the
    /// baseline lacks counts in full only if it started after the baseline;
    /// otherwise its earlier history is unknown and it counts nothing.
    fn process_delta(&self, process: &ProcessReading) -> u64 {
        match self.processes.get(&(process.pid, process.created)) {
            Some(previous) => process.cpu_time_100ns.saturating_sub(*previous),
            None if process.created >= self.at_filetime => process.cpu_time_100ns,
            None => 0,
        }
    }

    fn group_delta(&self, processes: &[ProcessReading]) -> u64 {
        processes
            .iter()
            .map(|process| self.process_delta(process))
            .fold(0, u64::saturating_add)
    }

    /// A run the baseline lacks started since it, so the Job's whole history
    /// lies inside the window.
    fn run_delta(&self, run: &RunReading) -> u64 {
        run.cpu_time_100ns
            .saturating_sub(self.runs.get(&run.run_id).copied().unwrap_or(0))
    }
}

/// Peaks of one run since it started.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub(crate) struct RunPeaks {
    pub(crate) cpu_percent: Option<f64>,
    pub(crate) private_bytes: u64,
}

impl RunPeaks {
    /// Folds one sample in. The Job's own peak commit also covers the time
    /// between samples, including while telemetry was off screen.
    pub(crate) fn observe(
        &mut self,
        cpu_percent: Option<f64>,
        private_bytes: u64,
        job_peak_bytes: u64,
    ) {
        if let Some(cpu_percent) = cpu_percent {
            self.cpu_percent = Some(
                self.cpu_percent
                    .map_or(cpu_percent, |peak| peak.max(cpu_percent)),
            );
        }
        self.private_bytes = self.private_bytes.max(private_bytes).max(job_peak_bytes);
    }
}

#[derive(Default)]
pub(crate) struct TelemetrySampler {
    baseline: Option<Baseline>,
    peaks: HashMap<RunId, RunPeaks>,
}

impl TelemetrySampler {
    /// Turns one sample's readings into usage, against the previous sample.
    /// The caller fills in `sampler_cost_ms`.
    pub(crate) fn advance(&mut self, readings: Readings) -> TelemetrySnapshot {
        let since_baseline = self
            .baseline
            .as_ref()
            .map(|baseline| readings.at.saturating_duration_since(baseline.at));
        let window =
            since_baseline.filter(|elapsed| (MIN_CPU_WINDOW..=MAX_CPU_WINDOW).contains(elapsed));
        let baseline = window.and(self.baseline.as_ref());
        let logical_cpus = readings.logical_cpus;
        let percent = |delta: Option<u64>| {
            delta
                .zip(window)
                .map(|(delta, window)| cpu_percent(0, delta, window, logical_cpus))
        };

        let core_delta = baseline.map(|baseline| {
            readings
                .core
                .cpu_time_100ns
                .saturating_sub(baseline.core_cpu_time)
        });
        let webview_delta = baseline.map(|baseline| baseline.group_delta(&readings.webview));
        let helpers_delta = baseline.map(|baseline| baseline.group_delta(&readings.helpers));
        let mut runs_delta = baseline.map(|_| 0_u64);
        let mut sessions = Vec::with_capacity(readings.runs.len());
        for run in &readings.runs {
            let delta = baseline.map(|baseline| baseline.run_delta(run));
            runs_delta = runs_delta
                .zip(delta)
                .map(|(sum, delta)| sum.saturating_add(delta));
            let cpu_percent = percent(delta);
            let peaks = self.peaks.entry(run.run_id).or_default();
            peaks.observe(cpu_percent, run.private_bytes, run.peak_job_memory_bytes);
            sessions.push(TelemetrySessionUsage {
                session_id: run.session_id,
                label: run.label.clone(),
                driver: run.driver,
                cpu_percent,
                private_bytes: run.private_bytes,
                processes: run.processes,
                peak_cpu_percent: peaks.cpu_percent,
                peak_private_bytes: peaks.private_bytes,
            });
        }
        // Peaks belong to a run: a stopped or restarted run's record goes.
        let live_runs = readings
            .runs
            .iter()
            .map(|run| run.run_id)
            .collect::<HashSet<_>>();
        self.peaks.retain(|run_id, _| live_runs.contains(run_id));

        let core = TelemetryUsage {
            cpu_percent: percent(core_delta),
            private_bytes: readings.core.private_bytes,
            processes: 1,
        };
        let webview = group_usage(&readings.webview, percent(webview_delta));
        let helpers = group_usage(&readings.helpers, percent(helpers_delta));
        let total_delta = [core_delta, webview_delta, helpers_delta, runs_delta]
            .into_iter()
            .try_fold(0_u64, |sum, delta| {
                delta.map(|delta| sum.saturating_add(delta))
            });
        let total = TelemetryUsage {
            cpu_percent: percent(total_delta),
            private_bytes: [core, webview, helpers]
                .iter()
                .map(|usage| usage.private_bytes)
                .chain(sessions.iter().map(|session| session.private_bytes))
                .fold(0, u64::saturating_add),
            processes: [core, webview, helpers]
                .iter()
                .map(|usage| usage.processes)
                .chain(sessions.iter().map(|session| session.processes))
                .fold(0, u32::saturating_add),
        };

        // A sample that came too soon keeps the baseline, so the next one
        // still measures over a full window.
        if since_baseline.is_none_or(|elapsed| elapsed >= MIN_CPU_WINDOW) {
            self.baseline = Some(Baseline::of(&readings));
        }

        TelemetrySnapshot {
            sampled_at: readings.sampled_at,
            logical_cpus,
            core,
            webview,
            helpers,
            sessions,
            total,
            unreadable_processes: readings.unreadable_processes,
            sampler_cost_ms: 0.0,
        }
    }
}

fn group_usage(processes: &[ProcessReading], cpu_percent: Option<f64>) -> TelemetryUsage {
    TelemetryUsage {
        cpu_percent,
        private_bytes: processes
            .iter()
            .map(|process| process.private_bytes)
            .fold(0, u64::saturating_add),
        processes: u32::try_from(processes.len()).unwrap_or(u32::MAX),
    }
}

/// Average share of the whole machine between two readings of a cumulative
/// CPU time. This is Task Manager's convention: one saturated core of a
/// 12-thread PC reads 8.3 %. Clamped to 0..=100 because process times advance
/// in scheduler ticks, so a short window can overshoot.
pub(crate) fn cpu_percent(
    previous_100ns: u64,
    current_100ns: u64,
    elapsed: Duration,
    logical_cpus: u32,
) -> f64 {
    let capacity_seconds = elapsed.as_secs_f64() * f64::from(logical_cpus.max(1));
    if capacity_seconds <= 0.0 {
        return 0.0;
    }
    let used_seconds = current_100ns.saturating_sub(previous_100ns) as f64 / 10_000_000.0;
    (used_seconds / capacity_seconds * 100.0).clamp(0.0, 100.0)
}

/// Splits the members of PRIM-1's application Job into WebView2 and helper
/// processes. PRIM-1 itself and members a run's Job counted are skipped.
///
/// The application Job is read before the runs' Jobs. A member that no run
/// claimed and that is gone by now (`still_alive` is false) was most likely a
/// run's process that exited in between; it is dropped rather than charged to
/// PRIM-1. A process that exits during a sample is counted nowhere.
/// A failed run-member reading must not fall back into Helpers: the run's
/// cumulative Job CPU already includes it. An unreadable PID is only a
/// conservative exclusion for this sample, never evidence of ownership. If
/// it was reused, omitting the replacement also keeps attribution uncertain.
pub(crate) fn split_app_members(
    members: &[ProcessUsage],
    core: (u32, u64),
    claimed: &HashSet<(u32, u64)>,
    unreadable: &HashSet<u32>,
    mut still_alive: impl FnMut(u32, u64) -> bool,
) -> (Vec<ProcessReading>, Vec<ProcessReading>) {
    let mut webview = Vec::new();
    let mut helpers = Vec::new();
    for member in members {
        let identity = (member.pid, member.created);
        if identity == core
            || claimed.contains(&identity)
            || unreadable.contains(&member.pid)
            || !still_alive(member.pid, member.created)
        {
            continue;
        }
        let is_webview = member
            .image_name
            .as_deref()
            .is_some_and(|name| name.eq_ignore_ascii_case(WEBVIEW_IMAGE));
        if is_webview {
            webview.push(ProcessReading::from(member));
        } else {
            helpers.push(ProcessReading::from(member));
        }
    }
    (webview, helpers)
}

/// Reads one sample against the application Job registered at startup.
#[cfg(windows)]
pub(crate) fn read(runs: &[LiveRun]) -> anyhow::Result<Readings> {
    let app_job = pty_host::app_job().ok_or_else(|| {
        anyhow::anyhow!(
            "PRIM-1's own process Job is not registered, so its footprint cannot be measured"
        )
    })?;
    read_from(app_job, runs)
}

#[cfg(not(windows))]
pub(crate) fn read(_runs: &[LiveRun]) -> anyhow::Result<Readings> {
    anyhow::bail!("resource telemetry is unsupported on this platform; it is Windows-only")
}

/// Reads PRIM-1's own process, then `app_job`, then each live run's Job.
#[cfg(windows)]
pub(crate) fn read_from(
    app_job: &pty_host::ProcessJob,
    runs: &[LiveRun],
) -> anyhow::Result<Readings> {
    use anyhow::Context;

    let at = Instant::now();
    let at_filetime = filetime_now();
    let sampled_at = shared_types::now_rfc3339();
    let logical_cpus = pty_host::logical_processor_count();
    let core = pty_host::current_process_usage().context("failed to read PRIM-1's own process")?;
    let app = app_job
        .accounting(true)
        .context("failed to read PRIM-1's own process Job")?;
    let mut unreadable = app.unreadable.iter().copied().collect::<HashSet<_>>();
    let mut claimed = HashSet::new();
    let mut run_readings = Vec::with_capacity(runs.len());
    for run in runs {
        let Some(scope) = run
            .pty
            .scope_accounting()
            .with_context(|| format!("failed to read the Job of session '{}'", run.label))?
        else {
            continue;
        };
        unreadable.extend(scope.unreadable.iter().copied());
        claimed.extend(
            scope
                .members
                .iter()
                .map(|member| (member.pid, member.created)),
        );
        run_readings.push(RunReading {
            session_id: run.session_id,
            run_id: run.run_id,
            label: run.label.clone(),
            driver: run.driver,
            cpu_time_100ns: scope.cpu_time_100ns,
            peak_job_memory_bytes: scope.peak_memory_bytes,
            private_bytes: scope
                .members
                .iter()
                .map(|member| member.private_bytes)
                .fold(0, u64::saturating_add),
            processes: u32::try_from(scope.members.len()).unwrap_or(u32::MAX),
        });
    }
    let (webview, helpers) = split_app_members(
        &app.members,
        (core.pid, core.created),
        &claimed,
        &unreadable,
        |pid, created| {
            pty_host::PinnedProcess::open(pid)
                .is_ok_and(|process| process.creation_time_filetime() == created)
        },
    );

    Ok(Readings {
        at,
        at_filetime,
        sampled_at,
        logical_cpus,
        core: ProcessReading::from(&core),
        webview,
        helpers,
        runs: run_readings,
        unreadable_processes: u32::try_from(unreadable.len()).unwrap_or(u32::MAX),
    })
}

/// Now on the clock process creation times use: FILETIME ticks, 100 ns units
/// since 1601-01-01 UTC.
fn filetime_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    const UNIX_EPOCH_AS_FILETIME: u64 = 116_444_736_000_000_000;
    let since_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_AS_FILETIME
        .saturating_add(u64::try_from(since_unix.as_nanos() / 100).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    const SECOND: u64 = 10_000_000;

    fn process(pid: u32, created: u64, cpu_seconds: f64, private_bytes: u64) -> ProcessReading {
        ProcessReading {
            pid,
            created,
            cpu_time_100ns: (cpu_seconds * SECOND as f64) as u64,
            private_bytes,
        }
    }

    fn run(run_id: RunId, label: &str, cpu_seconds: f64, private_bytes: u64) -> RunReading {
        RunReading {
            session_id: Uuid::from_u128(run_id.as_u128() + 1),
            run_id,
            label: label.into(),
            driver: DriverKind::Codex,
            cpu_time_100ns: (cpu_seconds * SECOND as f64) as u64,
            peak_job_memory_bytes: 0,
            private_bytes,
            processes: 3,
        }
    }

    /// A sample `offset` after `origin`; FILETIME starts at an arbitrary epoch.
    fn readings(origin: Instant, offset: Duration, runs: Vec<RunReading>) -> Readings {
        Readings {
            at: origin + offset,
            at_filetime: 1_000 * SECOND + offset.as_nanos() as u64 / 100,
            sampled_at: "2026-10-14T00:00:00Z".into(),
            logical_cpus: 4,
            core: process(10, 1, 0.0, 8_000_000),
            webview: Vec::new(),
            helpers: Vec::new(),
            runs,
            unreadable_processes: 0,
        }
    }

    fn assert_close(actual: Option<f64>, expected: f64) {
        let actual = actual.expect("a CPU measurement");
        assert!(
            (actual - expected).abs() < 1e-9,
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn cpu_percent_is_a_share_of_every_logical_cpu() {
        let two_seconds = Duration::from_secs(2);
        // One core saturated for the whole window of a 4-thread machine.
        assert_eq!(cpu_percent(5 * SECOND, 7 * SECOND, two_seconds, 4), 25.0);
        assert_eq!(cpu_percent(0, 8 * SECOND, two_seconds, 4), 100.0);
        assert_eq!(cpu_percent(0, SECOND / 2, Duration::from_secs(1), 1), 50.0);
        // Tick quantization overshoots are clamped, never reported above 100.
        assert_eq!(
            cpu_percent(0, 10 * SECOND, Duration::from_secs(1), 2),
            100.0
        );
        // A counter that went backwards or an empty window reads zero.
        assert_eq!(cpu_percent(9 * SECOND, 7 * SECOND, two_seconds, 4), 0.0);
        assert_eq!(cpu_percent(0, SECOND, Duration::ZERO, 4), 0.0);
        // A CPU count of zero cannot divide by zero.
        assert_eq!(cpu_percent(0, SECOND, Duration::from_secs(1), 0), 100.0);
    }

    #[test]
    fn run_peaks_keep_the_highest_cpu_and_the_job_peak_memory() {
        let mut peaks = RunPeaks::default();
        peaks.observe(None, 100, 0);
        assert_eq!(
            peaks,
            RunPeaks {
                cpu_percent: None,
                private_bytes: 100
            }
        );

        peaks.observe(Some(12.5), 80, 0);
        peaks.observe(Some(3.0), 90, 0);
        peaks.observe(None, 70, 0);
        assert_eq!(
            peaks,
            RunPeaks {
                cpu_percent: Some(12.5),
                private_bytes: 100
            }
        );

        // The Job's peak commit covers spikes between samples.
        peaks.observe(Some(1.0), 60, 400);
        assert_eq!(
            peaks,
            RunPeaks {
                cpu_percent: Some(12.5),
                private_bytes: 400
            }
        );
    }

    fn member(pid: u32, created: u64, image: Option<&str>) -> ProcessUsage {
        ProcessUsage {
            pid,
            created,
            cpu_time_100ns: u64::from(pid),
            private_bytes: u64::from(pid) * 1_000,
            image_name: image.map(str::to_owned),
        }
    }

    #[test]
    fn every_live_application_job_member_is_counted_once() {
        let members = [
            // PRIM-1 itself, measured as core.
            member(100, 1_000, Some("prim1.exe")),
            // The WebView2 browser and a renderer (names compare without case).
            member(200, 2_000, Some("msedgewebview2.exe")),
            member(201, 2_100, Some("MsEdgeWebView2.EXE")),
            // A console host beside a pane.
            member(300, 3_000, Some("conhost.exe")),
            // A live orphan: its parent exited long ago. A parent chain would
            // lose it; the Job does not.
            member(301, 3_100, Some("ping.exe")),
            // A member whose name could not be read is still PRIM-1's.
            member(302, 3_200, None),
            // A run's process, also in the application Job: its run counted it.
            member(400, 4_000, Some("codex.exe")),
            // A run's process that exited before its run's Job was read.
            member(500, 5_000, Some("git.exe")),
            // A listed ID that now names a newer process: identity mismatch.
            member(600, 6_000, Some("node.exe")),
        ];
        let claimed = HashSet::from([(400, 4_000)]);
        let alive = HashMap::from([
            (200, 2_000),
            (201, 2_100),
            (300, 3_000),
            (301, 3_100),
            (302, 3_200),
            (600, 6_500),
        ]);
        let mut asked = Vec::new();

        let (webview, helpers) = split_app_members(
            &members,
            (100, 1_000),
            &claimed,
            &HashSet::new(),
            |pid, created| {
                asked.push(pid);
                alive.get(&pid) == Some(&created)
            },
        );

        let pids = |readings: &[ProcessReading]| {
            readings
                .iter()
                .map(|reading| reading.pid)
                .collect::<Vec<_>>()
        };
        assert_eq!(pids(&webview), vec![200, 201]);
        assert_eq!(pids(&helpers), vec![300, 301, 302]);
        assert_eq!(
            helpers[0],
            ProcessReading {
                pid: 300,
                created: 3_000,
                cpu_time_100ns: 300,
                private_bytes: 300_000,
            }
        );
        // Core and claimed members are never reopened.
        assert_eq!(asked, vec![200, 201, 300, 301, 302, 500, 600]);
    }

    #[test]
    fn unreadable_processes_reach_the_snapshot() {
        let mut sampler = TelemetrySampler::default();
        let mut sample = readings(Instant::now(), Duration::ZERO, Vec::new());
        sample.unreadable_processes = 2;
        assert_eq!(sampler.advance(sample).unreadable_processes, 2);
    }

    #[test]
    fn unreadable_run_member_does_not_count_again_as_helper_cpu() {
        let origin = Instant::now();
        let run_id = Uuid::from_u128(7);
        let mut sampler = TelemetrySampler::default();
        let first = readings(origin, Duration::ZERO, vec![run(run_id, "Codex", 1.0, 100)]);
        let born = first.at_filetime + SECOND;
        sampler.advance(first);

        // The app inventory read both processes. The run inventory could not
        // read 400, but its cumulative Job CPU still includes that process's
        // 0.8 s. Both are new since the baseline, so a mistaken helper fallback
        // would count all of 400's CPU a second time in this very sample.
        let app_members = [
            ProcessUsage {
                pid: 400,
                created: born,
                cpu_time_100ns: SECOND * 8 / 10,
                private_bytes: 900,
                image_name: Some("codex.exe".into()),
            },
            ProcessUsage {
                pid: 300,
                created: born,
                cpu_time_100ns: SECOND * 2 / 10,
                private_bytes: 40,
                image_name: Some("conhost.exe".into()),
            },
        ];
        let unreadable = HashSet::from([400]);
        let mut liveness_checked = Vec::new();
        let (webview, helpers) = split_app_members(
            &app_members,
            (10, 1),
            &HashSet::new(),
            &unreadable,
            |pid, _| {
                liveness_checked.push(pid);
                true
            },
        );
        assert_eq!(liveness_checked, vec![300]);
        assert_eq!(helpers.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![300]);
        let mut next = readings(
            origin,
            Duration::from_secs(2),
            vec![run(run_id, "Codex", 1.8, 0)],
        );
        next.runs[0].processes = 0;
        next.webview = webview;
        next.helpers = helpers;
        next.unreadable_processes = unreadable.len() as u32;
        let result = sampler.advance(next);
        assert_close(result.sessions[0].cpu_percent, 0.8 / 8.0 * 100.0);
        assert_close(result.helpers.cpu_percent, 0.2 / 8.0 * 100.0);
        assert_close(result.total.cpu_percent, 1.0 / 8.0 * 100.0);
        assert_eq!(result.sessions[0].private_bytes, 0);
        assert_eq!(result.total.private_bytes, 8_000_000 + 40);
        assert_eq!(result.total.processes, 2);
        assert_eq!(result.unreadable_processes, 1);

        // A bare unreadable PID cannot authorize attributing even a newer
        // app member to a run or a helper; the warning remains conservative.
        let replacement = [ProcessUsage {
            created: born + SECOND,
            ..app_members[0].clone()
        }];
        let (webview, helpers) = split_app_members(
            &replacement,
            (10, 1),
            &HashSet::new(),
            &unreadable,
            |_, _| true,
        );
        assert!(webview.is_empty() && helpers.is_empty());
    }

    #[test]
    fn first_sample_has_memory_and_peaks_but_no_cpu() {
        let mut sampler = TelemetrySampler::default();
        let run_id = Uuid::from_u128(7);
        let mut first = readings(
            Instant::now(),
            Duration::ZERO,
            vec![run(run_id, "Codex", 3.0, 500)],
        );
        first.runs[0].peak_job_memory_bytes = 900;
        first.webview = vec![process(200, 5, 1.0, 300)];

        let snapshot = sampler.advance(first);

        assert_eq!(snapshot.core.cpu_percent, None);
        assert_eq!(
            snapshot.webview,
            TelemetryUsage {
                cpu_percent: None,
                private_bytes: 300,
                processes: 1
            }
        );
        assert_eq!(snapshot.total.cpu_percent, None);
        assert_eq!(snapshot.total.private_bytes, 8_000_000 + 300 + 500);
        assert_eq!(snapshot.total.processes, 1 + 1 + 3);
        assert_eq!(snapshot.sessions[0].cpu_percent, None);
        assert_eq!(snapshot.sessions[0].peak_cpu_percent, None);
        assert_eq!(snapshot.sessions[0].peak_private_bytes, 900);
    }

    #[test]
    fn second_sample_measures_each_group_over_the_window_without_double_counting() {
        let origin = Instant::now();
        let mut sampler = TelemetrySampler::default();
        let steady = Uuid::from_u128(7);
        let mut first = readings(origin, Duration::ZERO, vec![run(steady, "Codex", 3.0, 500)]);
        first.core.cpu_time_100ns = 10 * SECOND;
        let before = first.at_filetime - SECOND;
        first.webview = vec![process(200, before, 1.0, 300)];
        first.helpers = vec![process(300, before, 0.5, 40)];
        sampler.advance(first);

        let two = Duration::from_secs(2);
        let started = Uuid::from_u128(9);
        let mut second = readings(
            origin,
            two,
            vec![
                run(steady, "Codex", 5.0, 600),
                run(started, "Claude", 0.8, 200),
            ],
        );
        second.core.cpu_time_100ns = 10 * SECOND + SECOND / 5;
        let during = second.at_filetime - SECOND;
        second.webview = vec![
            // +1 s over the window.
            process(200, before, 2.0, 310),
            // Born inside the window: all of its 0.4 s counts.
            process(201, during, 0.4, 50),
            // Older than the baseline but never seen: its history is unknown.
            process(202, before - SECOND, 9.0, 60),
        ];
        // Same pid, new creation time: a reused ID is a new process.
        second.helpers = vec![process(300, during, 0.1, 40)];

        let snapshot = sampler.advance(second);

        // 8 CPU-seconds of capacity: 4 logical CPUs over 2 s.
        assert_close(snapshot.core.cpu_percent, 0.2 / 8.0 * 100.0);
        assert_close(snapshot.webview.cpu_percent, 1.4 / 8.0 * 100.0);
        assert_close(snapshot.helpers.cpu_percent, 0.1 / 8.0 * 100.0);
        assert_close(snapshot.sessions[0].cpu_percent, 2.0 / 8.0 * 100.0);
        // A run absent from the baseline started inside the window.
        assert_close(snapshot.sessions[1].cpu_percent, 0.8 / 8.0 * 100.0);
        assert_close(snapshot.total.cpu_percent, 4.5 / 8.0 * 100.0);
        assert_eq!(snapshot.webview.private_bytes, 420);
        assert_eq!(snapshot.webview.processes, 3);
        assert_eq!(
            snapshot.total.private_bytes,
            8_000_000 + 420 + 40 + 600 + 200
        );
        assert_eq!(snapshot.total.processes, 1 + 3 + 1 + 3 + 3);
        assert_close(snapshot.sessions[0].peak_cpu_percent, 25.0);
        assert_eq!(snapshot.sessions[0].peak_private_bytes, 600);
    }

    #[test]
    fn a_sample_too_soon_keeps_the_baseline_and_a_stale_one_restarts_it() {
        let origin = Instant::now();
        let mut sampler = TelemetrySampler::default();
        let mut sample = |offset_ms: u64, core_cpu_seconds: f64| {
            let mut next = readings(origin, Duration::from_millis(offset_ms), Vec::new());
            next.core.cpu_time_100ns = (core_cpu_seconds * SECOND as f64) as u64;
            sampler.advance(next).core.cpu_percent
        };

        assert_eq!(sample(0, 0.0), None);
        // 100 ms later: too short to measure, and the baseline stays at 0 ms.
        assert_eq!(sample(100, 4.0), None);
        // Measured over the full 2 s window since the kept baseline.
        assert_close(sample(2_000, 4.0), 4.0 / 8.0 * 100.0);
        // 30 s of silence (telemetry off screen) is not "CPU now".
        assert_eq!(sample(32_000, 100.0), None);
        assert_close(sample(34_000, 101.0), 1.0 / 8.0 * 100.0);
    }

    #[test]
    fn peaks_follow_the_run_and_are_dropped_when_it_ends() {
        let origin = Instant::now();
        let mut sampler = TelemetrySampler::default();
        let first_run = Uuid::from_u128(7);
        sampler.advance(readings(
            origin,
            Duration::ZERO,
            vec![run(first_run, "Codex", 0.0, 900)],
        ));
        let busy = sampler.advance(readings(
            origin,
            Duration::from_secs(2),
            vec![run(first_run, "Codex", 4.0, 100)],
        ));
        assert_close(busy.sessions[0].peak_cpu_percent, 50.0);
        assert_eq!(busy.sessions[0].peak_private_bytes, 900);

        let quiet = sampler.advance(readings(
            origin,
            Duration::from_secs(4),
            vec![run(first_run, "Codex", 4.0, 100)],
        ));
        assert_close(quiet.sessions[0].cpu_percent, 0.0);
        assert_close(quiet.sessions[0].peak_cpu_percent, 50.0);

        // A restart is a new run: its peaks start over.
        let restarted = Uuid::from_u128(8);
        let fresh = sampler.advance(readings(
            origin,
            Duration::from_secs(6),
            vec![run(restarted, "Codex", 0.2, 50)],
        ));
        assert_close(fresh.sessions[0].peak_cpu_percent, 0.2 / 8.0 * 100.0);
        assert_eq!(fresh.sessions[0].peak_private_bytes, 50);
        assert_eq!(sampler.peaks.len(), 1);
        assert!(sampler.peaks.contains_key(&restarted));
    }

    #[cfg(windows)]
    mod windows {
        use super::*;
        use anyhow::Result;
        use std::{
            fs,
            os::windows::process::CommandExt,
            path::{Path, PathBuf},
            process::{Child, Command, Stdio},
            time::{SystemTime, UNIX_EPOCH},
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

        /// A process with no children of its own that outlives the test.
        fn sleeper() -> KillChild {
            KillChild(
                Command::new("ping")
                    .args(["-n", "30", "127.0.0.1"])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .creation_flags(CREATE_NO_WINDOW)
                    .spawn()
                    .expect("spawn sleeper"),
            )
        }

        fn pin(child: &KillChild) -> pty_host::PinnedProcess {
            pty_host::PinnedProcess::open(child.0.id()).expect("pin a live child")
        }

        /// A session whose scope is one real Job.
        struct JobSession(pty_host::ProcessJob);

        impl PtySession for JobSession {
            fn send_input(&self, _input: &str) -> pty_host::PtyWriteResult {
                Ok(0)
            }
            fn resize(&self, _cols: u16, _rows: u16) -> Result<()> {
                Ok(())
            }
            fn kill(&self) -> Result<()> {
                self.0.terminate()
            }
            fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> {
                Ok(None)
            }
            fn process_id(&self) -> Option<u32> {
                None
            }
            fn scope_accounting(&self) -> Result<Option<pty_host::ScopeAccounting>> {
                self.0.accounting(false).map(Some)
            }
        }

        #[test]
        fn without_an_application_job_a_sample_is_refused_not_partial() {
            let error = read(&[]).expect_err("no application Job is registered in tests");
            assert!(error.to_string().contains("not registered"), "{error:#}");
        }

        #[test]
        fn a_real_sample_counts_each_application_job_member_once_orphans_included() {
            let unique = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock before Unix epoch")
                .as_nanos();
            let test_dir = std::env::temp_dir().join(format!(
                "prim1-telemetry-orphan-{}-{unique}",
                std::process::id()
            ));
            fs::create_dir_all(&test_dir).expect("create orphan test directory");
            let _test_dir = RemoveDir(test_dir.clone());
            let release_path = test_dir.join("release");
            let orphan_pid_path = test_dir.join("orphan.pid");

            let app = pty_host::ProcessJob::new().expect("create application job");
            let session = pty_host::ProcessJob::new().expect("create session job");
            // A helper of the application.
            let helper = sleeper();
            app.assign_process(&pin(&helper)).expect("assign helper");
            // A run's process: in the application Job and, nested, its own.
            let agent = sleeper();
            let pinned_agent = pin(&agent);
            app.assign_process(&pinned_agent)
                .expect("assign agent to the application job");
            session
                .assign_process(&pinned_agent)
                .expect("nest the agent's run job inside the application job");
            // A parent that starts a child and exits, leaving a live orphan.
            let script = format!(
                "while (-not (Test-Path -LiteralPath '{}')) {{ Start-Sleep -Milliseconds 10 }}; \
                 $orphan = Start-Process -FilePath ping -ArgumentList '-n','30','127.0.0.1' -NoNewWindow -PassThru; \
                 [IO.File]::WriteAllText('{}', [string]$orphan.Id)",
                powershell_literal(&release_path),
                powershell_literal(&orphan_pid_path),
            );
            let mut parent = Command::new("powershell")
                .args(["-NoProfile", "-NonInteractive", "-Command"])
                .arg(script)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .map(KillChild)
                .expect("spawn the orphan's parent");
            let parent_pid = parent.0.id();
            app.assign_process(&pin(&parent))
                .expect("assign the orphan's parent");
            fs::write(&release_path, b"release").expect("release the orphan's parent");
            let deadline = Instant::now() + Duration::from_secs(15);
            let orphan_pid = loop {
                let exited = parent.0.try_wait().expect("poll the parent").is_some();
                if exited
                    && let Ok(raw) = fs::read_to_string(&orphan_pid_path)
                    && let Ok(pid) = raw.trim().parse::<u32>()
                {
                    break pid;
                }
                assert!(
                    Instant::now() < deadline,
                    "the parent never exited leaving an orphan behind"
                );
                std::thread::sleep(Duration::from_millis(20));
            };
            // The owned application Job contains the orphan and kills it on
            // close, including panic cleanup. Never terminate a retained PID.
            // In none of these Jobs.
            let outsider = sleeper();
            let run_id = Uuid::new_v4();
            let runs = [LiveRun {
                session_id: Uuid::new_v4(),
                run_id,
                label: "fixture".into(),
                driver: DriverKind::GenericTerminal,
                pty: Arc::new(JobSession(session)),
            }];

            let readings = read_from(&app, &runs).expect("read a sample");

            assert_eq!(readings.core.pid, std::process::id());
            assert!(readings.core.private_bytes > 0);
            assert!(readings.logical_cpus >= 1);
            assert_eq!(readings.unreadable_processes, 0);
            let run = &readings.runs[0];
            assert_eq!(run.run_id, run_id);
            assert!(run.processes >= 1 && run.private_bytes > 0, "{run:?}");
            let helper_pids = readings
                .helpers
                .iter()
                .map(|process| process.pid)
                .collect::<Vec<_>>();
            assert!(helper_pids.contains(&helper.0.id()), "{helper_pids:?}");
            assert!(
                helper_pids.contains(&orphan_pid),
                "a live orphan stays in the application Job: {helper_pids:?}"
            );
            assert!(
                !helper_pids.contains(&agent.0.id()),
                "a run's process is counted by its run, never also as a helper"
            );
            assert!(!helper_pids.contains(&outsider.0.id()));
            assert!(
                !helper_pids.contains(&parent_pid),
                "an exited parent counts nowhere"
            );
            assert!(
                readings.webview.is_empty(),
                "a test process has no WebView2"
            );
        }
    }
}
