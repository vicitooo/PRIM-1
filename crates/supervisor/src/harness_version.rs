//! v3 W2: the harness CLI version a member runs, and whether it is outside the
//! version this build was tested with (W1 pins). Observed from the exact
//! resolved launch program with `--version`, bounded in time and output, off
//! every lock, and cached per executable identity (path, size, modified time),
//! never per tick. Unknown stays unknown: it is never a mismatch and never a
//! refusal; a drift only says so, once per run. Test builds observe a seam
//! instead of running harnesses at launch (the probe has its own test).
#![cfg_attr(test, allow(dead_code))]
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

use parking_lot::Mutex;
use shared_types::DriverKind;

use crate::harness_executable::ResolvedLaunchProgram;

/// The versions W1 measured and this build was tested with.
pub(crate) fn tested_version(driver: DriverKind) -> Option<&'static str> {
    match driver {
        DriverKind::Codex => Some("0.164.0"),
        DriverKind::Claude => Some("2.1.310"),
        _ => None,
    }
}

pub(crate) fn harness_name(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Codex => "Codex CLI",
        DriverKind::Claude => "Claude Code",
        DriverKind::Grok => "Grok CLI",
        _ => "harness",
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
const PROBE_MAX_BYTES: usize = 4096;

/// The first dotted version (`a.b.c`, digits only) in `--version` output:
/// `codex-cli 0.164.0` and `2.1.310 (Claude Code)` both read as the number.
pub(crate) fn parse_version(output: &str) -> Option<String> {
    output.split(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == ',').find_map(|token| {
        let token = token.trim_start_matches('v');
        let parts: Vec<&str> = token.split('.').collect();
        (parts.len() == 3 && parts.iter().all(|part| !part.is_empty() && part.len() <= 6
            && part.bytes().all(|byte| byte.is_ascii_digit()))).then(|| token.to_string())
    })
}

/// The once-per-run notice when `observed` differs from the tested version;
/// None when they match or either is unknown.
pub(crate) fn drift_notice(driver: DriverKind, label: &str, observed: Option<&str>) -> Option<String> {
    let (observed, tested) = (observed?, tested_version(driver)?);
    (observed != tested).then(|| format!("[PRIM-1] {label} runs {} {observed}; this PRIM-1 build was tested with {tested}. \
        Delivery works the same, but readiness detection may not match this version.", harness_name(driver)))
}

type FileIdentity = (String, u64, Option<SystemTime>);
/// The program, its prefix arguments, and the script a shim runs: `node.exe`
/// runs both Claude's and Codex's script, so the program alone is not the harness.
type Identity = (FileIdentity, Vec<String>, Option<FileIdentity>);

fn file_identity(path: &str) -> Option<FileIdentity> {
    let meta = std::fs::metadata(Path::new(path)).ok().filter(std::fs::Metadata::is_file)?;
    Some((path.to_string(), meta.len(), meta.modified().ok()))
}

fn identity(resolved: &ResolvedLaunchProgram) -> Option<Identity> {
    Some((file_identity(&resolved.program)?, resolved.prefix_args.clone(),
        resolved.prefix_args.first().and_then(|script| file_identity(script))))
}

/// Cached per executable identity: the same binary is asked once.
#[derive(Default)]
pub(crate) struct VersionCache {
    known: Mutex<HashMap<Identity, Option<String>>>,
}

impl VersionCache {
    pub(crate) fn observe(&self, resolved: &ResolvedLaunchProgram) -> Option<String> {
        let key = identity(resolved)?;
        if let Some(found) = self.known.lock().get(&key) { return found.clone(); }
        let found = probe(resolved);
        self.known.lock().insert(key, found.clone());
        found
    }
}

/// `program [prefix…] --version`, no window, stdin closed, stopped after the
/// timeout, at most PROBE_MAX_BYTES read.
fn probe(resolved: &ResolvedLaunchProgram) -> Option<String> {
    probe_within(resolved, PROBE_TIMEOUT)
}

/// v3 W2 (Reviewer 1fba3ea7): `timeout` bounds the whole probe, the pipe
/// drain included: a child that exits while a process it started still holds
/// its stdout gives no answer. The reader is then left to end with that pipe
/// (one thread per probe; each launch program is probed once per identity).
/// Only a successful exit's output is read as a version.
fn probe_within(resolved: &ResolvedLaunchProgram, timeout: Duration) -> Option<String> {
    use std::io::Read as _;
    let mut command = std::process::Command::new(&resolved.program);
    command.args(&resolved.prefix_args).arg("--version")
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let mut stdout = child.stdout.take()?;
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = (&mut stdout).take(PROBE_MAX_BYTES as u64).read_to_end(&mut buffer);
        let _ = sender.send(buffer);
    });
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => { let _ = child.kill(); let _ = child.wait(); return None; }
        }
    };
    if !status.success() { return None; }
    let output = received.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now())).ok()?;
    parse_version(&String::from_utf8_lossy(&output))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_version_is_the_first_dotted_number_and_unknown_stays_unknown() {
        assert_eq!(parse_version("codex-cli 0.164.0\n").as_deref(), Some("0.164.0"));
        assert_eq!(parse_version("2.1.310 (Claude Code)\n").as_deref(), Some("2.1.310"));
        assert_eq!(parse_version("grok v1.2.3").as_deref(), Some("1.2.3"));
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("error: unknown option --version"), None);
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
    }

    /// The real probe on a real program: `cmd /c echo …` stands in for a
    /// harness, whose output then ends in the appended `--version`.
    #[cfg(windows)]
    #[test]
    fn the_probe_reads_the_launch_program_and_the_cache_asks_each_harness_once() {
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        let launch = |words: &[&str]| ResolvedLaunchProgram { program: cmd.clone(),
            prefix_args: ["/c", "echo"].iter().chain(words).map(|word| (*word).to_string()).collect() };
        assert_eq!(probe(&launch(&["codex-cli", "0.164.0"])).as_deref(), Some("0.164.0"));
        assert_eq!(probe(&ResolvedLaunchProgram { program: r"C:\no\such\harness.exe".into(), prefix_args: Vec::new() }), None);
        let cache = VersionCache::default();
        assert_eq!(cache.observe(&launch(&["2.1.310"])).as_deref(), Some("2.1.310"));
        assert_eq!(cache.observe(&launch(&["2.1.310"])).as_deref(), Some("2.1.310"));
        assert_eq!(cache.known.lock().len(), 1, "the same launch program is asked once");
        // The same program with other arguments (a shim running another
        // script) is another harness, never the cached answer.
        assert_eq!(cache.observe(&launch(&["0.164.0"])).as_deref(), Some("0.164.0"));
        assert_eq!(cache.known.lock().len(), 2);
    }

    /// Reviewer 1fba3ea7: an error exit is no answer even when its output
    /// holds a dotted version, and a child that exits while a process it
    /// started keeps its stdout open gives no answer within the bound.
    #[cfg(windows)]
    #[test]
    fn an_error_exit_or_a_held_pipe_is_no_answer_within_the_bound() {
        let cmd = std::env::var("ComSpec").unwrap_or_else(|_| r"C:\Windows\System32\cmd.exe".into());
        let launch = |words: &[&str]| ResolvedLaunchProgram { program: cmd.clone(),
            prefix_args: std::iter::once("/c").chain(words.iter().copied()).map(str::to_string).collect() };
        assert_eq!(probe_within(&launch(&["echo", "codex-cli", "0.164.0", "&", "exit", "/b", "3"]), PROBE_TIMEOUT), None,
            "a dotted version from an error exit is not observed");
        assert_eq!(probe_within(&launch(&["echo", "codex-cli", "0.164.0", "&", "exit", "/b", "0"]), PROBE_TIMEOUT).as_deref(),
            Some("0.164.0"), "the same output from a successful exit is");
        let started = std::time::Instant::now();
        assert_eq!(probe_within(&launch(&["start", "/b", "ping", "-n", "20", "127.0.0.1", "&", "echo", "0.164.0"]),
            Duration::from_secs(2)), None, "the pipe is still held");
        assert!(started.elapsed() < Duration::from_secs(5), "bounded: {:?}", started.elapsed());
    }

    #[test]
    fn a_drift_is_said_only_for_a_known_version_that_differs_from_the_tested_one() {
        assert_eq!(drift_notice(DriverKind::Claude, "Claude J", Some("2.1.310")), None);
        assert_eq!(drift_notice(DriverKind::Codex, "Codex J", Some("0.164.0")), None);
        assert_eq!(drift_notice(DriverKind::Claude, "Claude J", None), None, "unknown is never a mismatch");
        assert_eq!(drift_notice(DriverKind::Grok, "Grok J", Some("9.9.9")), None, "no tested version, no claim");
        let notice = drift_notice(DriverKind::Codex, "Codex J", Some("0.165.0")).unwrap();
        assert!(notice.starts_with("[PRIM-1] Codex J runs Codex CLI 0.165.0; this PRIM-1 build was tested with 0.164.0."), "{notice}");
    }
}
