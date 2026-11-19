//! A108 step 11: after the new build consumed its upgrade and its window is
//! shown, the stable Desktop launcher task follows it. The launcher format is
//! `scripts/launch-experimental.ps1`'s: task `PRIM1-<root leaf>`, one action
//! with no arguments, description `PRIM1 isolated launcher v1; root=<root>;
//! sha256=<UPPER>`. Only a task whose action is exactly the upgrade's source
//! build (or already the candidate) changes; anything else is refused.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use serde::Deserialize;

const DESCRIPTION_PREFIX: &str = "PRIM1 isolated launcher v1; root=";
const SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LauncherAction {
    pub execute: String,
    #[serde(default)]
    pub arguments: Option<String>,
    #[serde(default)]
    pub working_directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LauncherTask {
    pub description: String,
    pub actions: Vec<LauncherAction>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LauncherDecision {
    /// No launcher task exists for this runtime's root.
    NoTask,
    AlreadyCurrent,
    /// Run this hidden script; it rechecks the task before changing it.
    Update { script: String },
}

/// `PRIM1-<leaf of the runtime's parent>`, the launcher's task name.
pub(crate) fn launcher_task_name(runtime_dir: &Path) -> Option<String> {
    let leaf = runtime_dir.parent()?.file_name()?.to_str()?;
    Some(format!("PRIM1-{leaf}"))
}

fn quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("a launcher value contains a control character".into());
    }
    Ok(format!("'{}'", value.replace('\'', "''")))
}

fn same_path(left: &str, right: &str) -> bool {
    let key = |path: &str| {
        let path = path.strip_prefix(r"\\?\").unwrap_or(path).replace('/', "\\");
        path.trim_end_matches('\\').to_ascii_lowercase()
    };
    key(left) == key(right)
}

/// Lists the launcher task as JSON: `{"exists":false}` or its description
/// and actions. Read-only.
pub(crate) fn query_script(task_name: &str) -> Result<String, String> {
    let name = quote(task_name)?;
    Ok(format!(
        "$ErrorActionPreference = 'Stop'\n\
         $tasks = @(Get-ScheduledTask -TaskPath '\\' | Where-Object {{ $_.TaskName -ceq {name} }})\n\
         if ($tasks.Count -gt 1) {{ throw 'Ambiguous launcher task.' }}\n\
         if ($tasks.Count -eq 0) {{ '{{\"exists\":false}}'; return }}\n\
         $task = $tasks[0]\n\
         [pscustomobject]@{{ exists = $true; description = [string]$task.Description; actions = @(@($task.Actions) | ForEach-Object {{ [pscustomobject]@{{ execute = [string]$_.Execute; arguments = [string]$_.Arguments; working_directory = [string]$_.WorkingDirectory }} }}) }} | ConvertTo-Json -Depth 4 -Compress\n"
    ))
}

pub(crate) fn parse_query(output: &str) -> Result<Option<LauncherTask>, String> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Actions {
        Many(Vec<LauncherAction>),
        One(LauncherAction),
    }
    #[derive(Deserialize)]
    struct Query {
        exists: bool,
        #[serde(default)]
        description: Option<String>,
        #[serde(default)]
        actions: Option<Actions>,
    }
    let query: Query = serde_json::from_str(output.trim())
        .map_err(|error| format!("the launcher task could not be read: {error}"))?;
    if !query.exists {
        return Ok(None);
    }
    let actions = match query.actions {
        None => Vec::new(),
        Some(Actions::Many(actions)) => actions,
        Some(Actions::One(action)) => vec![action],
    };
    Ok(Some(LauncherTask { description: query.description.unwrap_or_default(), actions }))
}

/// Decide what the launcher task needs. `source_exe` is the build the
/// upgrade replaced; `candidate_exe` the running new build.
pub(crate) fn plan_launcher_update(
    task_name: &str,
    task: Option<&LauncherTask>,
    source_exe: &Path,
    candidate_exe: &Path,
    candidate_sha256: &str,
) -> Result<LauncherDecision, String> {
    let Some(task) = task else { return Ok(LauncherDecision::NoTask) };
    let foreign = |why: &str| Err(format!("the launcher task {task_name} was not changed: {why}"));
    let [action] = task.actions.as_slice() else { return foreign("it does not have exactly one action") };
    if action.arguments.as_deref().is_some_and(|arguments| !arguments.trim().is_empty()) {
        return foreign("its action has arguments");
    }
    let Some(rest) = task.description.strip_prefix(DESCRIPTION_PREFIX) else {
        return foreign("its description is not a PRIM-1 isolated launcher");
    };
    let Some((root, sha)) = rest.rsplit_once("; sha256=") else {
        return foreign("its description has no build hash");
    };
    let leaf = Path::new(root).file_name().and_then(|leaf| leaf.to_str()).unwrap_or_default();
    if format!("PRIM1-{leaf}") != task_name || sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        return foreign("its description does not match this launcher");
    }
    let candidate = candidate_exe.to_str().ok_or("the new build's path is not valid Unicode")?;
    let source = source_exe.to_str().ok_or("the replaced build's path is not valid Unicode")?;
    let candidate_dir = candidate_exe.parent().and_then(Path::to_str).ok_or("the new build has no folder")?;
    let sha_upper = candidate_sha256.to_ascii_uppercase();
    let description = format!("{DESCRIPTION_PREFIX}{root}; sha256={sha_upper}");
    let on_candidate = same_path(&action.execute, candidate);
    if on_candidate && action.working_directory.as_deref().is_some_and(|dir| same_path(dir, candidate_dir))
        && task.description == description {
        return Ok(LauncherDecision::AlreadyCurrent);
    }
    if !on_candidate && !same_path(&action.execute, source) {
        return foreign("its action is not the build this upgrade replaced");
    }
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $task = Get-ScheduledTask -TaskName {name} -TaskPath '\\'\n\
         $actions = @($task.Actions)\n\
         if ($actions.Count -ne 1 -or $actions[0].Arguments -or $task.Description -cne {old_description} -or -not [string]::Equals([string]$actions[0].Execute, {old_execute}, [StringComparison]::OrdinalIgnoreCase)) {{ throw 'The launcher task changed while it was being updated; it was left as it is.' }}\n\
         $task.Actions = @(New-ScheduledTaskAction -Execute {candidate} -WorkingDirectory {directory})\n\
         $task.Description = {description}\n\
         $null = Set-ScheduledTask -InputObject $task\n\
         'updated'\n",
        name = quote(task_name)?,
        old_description = quote(&task.description)?,
        old_execute = quote(&action.execute)?,
        candidate = quote(candidate)?,
        directory = quote(candidate_dir)?,
        description = quote(&description)?,
    );
    Ok(LauncherDecision::Update { script })
}

/// `-EncodedCommand` takes base64 of the script's UTF-16LE bytes: no quoting.
pub(crate) fn encoded_command(script: &str) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..4 {
            if index <= chunk.len() {
                encoded.push(ALPHABET[(value >> (18 - 6 * index) & 63) as usize] as char);
            } else {
                encoded.push('=');
            }
        }
    }
    encoded
}

pub(crate) fn hidden_powershell(system_root: &Path, script: &str) -> (PathBuf, Vec<OsString>) {
    let program = system_root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let args = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-EncodedCommand"]
        .into_iter().map(OsString::from)
        .chain([OsString::from(encoded_command(script))])
        .collect();
    (program, args)
}

#[cfg(windows)]
fn run_hidden(script: &str) -> Result<String, String> {
    use std::{io::Read as _, os::windows::process::CommandExt as _, process::{Command, Stdio}};
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let system_root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let (program, args) = hidden_powershell(&system_root, script);
    let mut child = Command::new(program).args(args).creation_flags(CREATE_NO_WINDOW)
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|error| format!("PowerShell could not start: {error}"))?;
    let started = Instant::now();
    loop {
        match child.try_wait().map_err(|error| error.to_string())? {
            Some(status) => {
                let mut stdout = String::new();
                let mut stderr = String::new();
                if let Some(mut out) = child.stdout.take() { let _ = out.read_to_string(&mut stdout); }
                if let Some(mut err) = child.stderr.take() { let _ = err.read_to_string(&mut stderr); }
                return if status.success() { Ok(stdout) } else { Err(stderr.trim().chars().take(2000).collect()) };
            }
            None if started.elapsed() >= SCRIPT_TIMEOUT => {
                let _ = child.kill();
                return Err("the launcher script did not finish in time".into());
            }
            None => std::thread::sleep(Duration::from_millis(100)),
        }
    }
}

#[cfg(not(windows))]
fn run_hidden(_script: &str) -> Result<String, String> {
    Err("the stable launcher is a Windows scheduled task".into())
}

/// Point the stable launcher at this new build. Returns what was done.
pub(crate) fn follow_new_build(
    runtime_dir: &Path,
    source_exe: &Path,
    candidate_exe: &Path,
    candidate_sha256: &str,
) -> Result<String, String> {
    let running = std::env::current_exe().map_err(|error| error.to_string())?;
    if !same_path(&running.to_string_lossy(), &candidate_exe.to_string_lossy()) {
        return Ok("this process is not the upgrade's staged build; the launcher is unchanged".into());
    }
    let Some(task_name) = launcher_task_name(runtime_dir) else {
        return Ok("the runtime has no launcher root; the launcher is unchanged".into());
    };
    let task = parse_query(&run_hidden(&query_script(&task_name)?)?)?;
    match plan_launcher_update(&task_name, task.as_ref(), source_exe, candidate_exe, candidate_sha256)? {
        LauncherDecision::NoTask => Ok(format!("no launcher task {task_name}; nothing to follow")),
        LauncherDecision::AlreadyCurrent => Ok(format!("{task_name} already starts this build")),
        LauncherDecision::Update { script } => {
            run_hidden(&script)?;
            Ok(format!("{task_name} now starts {}", candidate_exe.display()))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = r"D:\prim1-v2-experimental-20261019";
    const NAME: &str = "PRIM1-prim1-v2-experimental-20261019";

    fn source() -> PathBuf {
        PathBuf::from(format!(r"{ROOT}\bin\prim1-experimental.exe"))
    }

    fn candidate() -> PathBuf {
        PathBuf::from(format!(r"{ROOT}\runtime\maintenance\upgrades\0b6b2b9e-7d55-4b1e-9b8a-1d2c3e4f5a6b\candidate\prim1.exe"))
    }

    fn task(execute: &str, directory: &str, sha: &str) -> LauncherTask {
        LauncherTask {
            description: format!("PRIM1 isolated launcher v1; root={ROOT}; sha256={sha}"),
            actions: vec![LauncherAction { execute: execute.into(), arguments: Some(String::new()), working_directory: Some(directory.into()) }],
        }
    }

    #[test]
    fn the_task_name_follows_the_launcher_root() {
        assert_eq!(launcher_task_name(Path::new(&format!(r"{ROOT}\runtime"))).as_deref(), Some(NAME));
        assert_eq!(launcher_task_name(Path::new(r"C:\")), None);
    }

    #[test]
    fn the_launcher_moves_from_the_replaced_build_to_the_candidate_with_its_hash() {
        let sha = "c".repeat(64);
        let from_source = task(&source().display().to_string(), &format!(r"{ROOT}\bin"), &"A".repeat(64));
        let LauncherDecision::Update { script } = plan_launcher_update(NAME, Some(&from_source), &source(), &candidate(), &sha).unwrap()
        else { panic!("an update was expected") };
        let candidate_dir = candidate().parent().unwrap().display().to_string();
        assert!(script.contains(&format!("-Execute '{}' -WorkingDirectory '{candidate_dir}'", candidate().display())), "{script}");
        assert!(script.contains(&format!("'PRIM1 isolated launcher v1; root={ROOT}; sha256={}'", "C".repeat(64))), "{script}");
        assert!(script.contains(&format!("{}'", "A".repeat(64))), "the old description is rechecked: {script}");
        assert!(script.contains(&format!("'{}'", source().display())), "the old action is rechecked: {script}");
        assert!(script.contains("Set-ScheduledTask -InputObject $task") && !script.contains("Start-ScheduledTask"), "{script}");

        // Already moved (a repeated run): nothing to do; a stale hash is fixed.
        let moved = task(&candidate().display().to_string().to_uppercase(), &candidate_dir, &"C".repeat(64));
        assert_eq!(plan_launcher_update(NAME, Some(&moved), &source(), &candidate(), &sha), Ok(LauncherDecision::AlreadyCurrent));
        let stale = task(&candidate().display().to_string(), &candidate_dir, &"A".repeat(64));
        assert!(matches!(plan_launcher_update(NAME, Some(&stale), &source(), &candidate(), &sha), Ok(LauncherDecision::Update { .. })));
        assert_eq!(plan_launcher_update(NAME, None, &source(), &candidate(), &sha), Ok(LauncherDecision::NoTask));
    }

    #[test]
    fn a_foreign_launcher_task_is_refused() {
        let sha = "c".repeat(64);
        let bin = format!(r"{ROOT}\bin");
        let mut two = task(&source().display().to_string(), &bin, &sha);
        two.actions.push(two.actions[0].clone());
        let mut arguments = task(&source().display().to_string(), &bin, &sha);
        arguments.actions[0].arguments = Some("--evil".into());
        let mut description = task(&source().display().to_string(), &bin, &sha);
        description.description = "Someone else's task".into();
        let mut other_root = task(&source().display().to_string(), &bin, &sha);
        other_root.description = format!(r"PRIM1 isolated launcher v1; root=D:\elsewhere; sha256={sha}");
        for (case, foreign) in [
            ("other program", task(r"C:\Windows\System32\cmd.exe", &bin, &sha)),
            ("two actions", two),
            ("arguments", arguments),
            ("description", description),
            ("other root", other_root),
        ] {
            let refused = plan_launcher_update(NAME, Some(&foreign), &source(), &candidate(), &sha);
            assert!(refused.as_ref().is_err_and(|error| error.contains("was not changed")), "{case}: {refused:?}");
        }
    }

    #[test]
    fn quoting_and_the_query_are_literal_powershell() {
        assert_eq!(quote("O'Neil").unwrap(), "'O''Neil'");
        assert!(quote("line\nbreak").is_err());
        let script = query_script(NAME).unwrap();
        assert!(script.contains(&format!("$_.TaskName -ceq '{NAME}'")) && script.contains("ConvertTo-Json"), "{script}");
        assert_eq!(parse_query(r#"{"exists":false}"#), Ok(None));
        let one = parse_query(r#"{"exists":true,"description":"d","actions":{"execute":"a.exe","arguments":"","working_directory":"w"}}"#).unwrap().unwrap();
        assert_eq!(one.actions.len(), 1);
        let many = parse_query(r#"{"exists":true,"description":"d","actions":[{"execute":"a"},{"execute":"b"}]}"#).unwrap().unwrap();
        assert_eq!(many.actions.len(), 2);
        assert!(parse_query("not json").is_err());
    }

    #[test]
    fn the_hidden_command_carries_the_script_as_utf16_base64() {
        // "A" is 0x41 0x00 in UTF-16LE.
        assert_eq!(encoded_command("A"), "QQA=");
        assert_eq!(encoded_command("ab"), "YQBiAA==");
        let (program, args) = hidden_powershell(Path::new(r"C:\Windows"), "A");
        assert_eq!(program, PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe"));
        let args: Vec<_> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
        assert_eq!(args, ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-EncodedCommand", "QQA="]);
    }
}
