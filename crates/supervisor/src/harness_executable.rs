//! Resolve the host binary for a harness.
//!
//! Order, for every host driver (Claude, Codex, Grok, Generic terminal):
//! 1. `{name}.exe` (or the Unix name) on PATH.
//! 2. Follow `{name}.cmd` on PATH to a real `.exe`, or to `node.exe` + the
//!    `.js` the shim names. Never spawn the `.cmd` itself.
//! 3. If `search` is set: walk a bounded set of usual install locations.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};
use shared_types::{DriverKind, SessionErrorKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedLaunchProgram {
    pub program: String,
    pub prefix_args: Vec<String>,
}

#[derive(Debug)]
pub(crate) struct HarnessExecutableError {
    #[allow(dead_code)]
    pub driver: DriverKind,
    pub names: Vec<String>,
    pub searched: bool,
}

impl HarnessExecutableError {
    #[cfg(test)]
    pub(crate) fn kind(&self) -> Option<SessionErrorKind> {
        if self.searched {
            None
        } else {
            Some(SessionErrorKind::ExecutableNotFound)
        }
    }
}

impl std::fmt::Display for HarnessExecutableError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names = self.names.join(", ");
        if self.searched {
            write!(
                f,
                "Looked in the usual places and still can't find {names}."
            )
        } else {
            write!(f, "Can't find {names} on PATH.")
        }
    }
}

impl std::error::Error for HarnessExecutableError {}

struct DriverBins {
    /// Ordered preference for a direct executable on PATH.
    path_names: &'static [&'static str],
    /// Windows cmd shim name, if any.
    cmd_name: Option<&'static str>,
}

fn bins_for(driver: DriverKind) -> Option<DriverBins> {
    match driver {
        DriverKind::Claude => Some(DriverBins {
            path_names: if cfg!(windows) {
                &["claude.exe"]
            } else {
                &["claude"]
            },
            cmd_name: cfg!(windows).then_some("claude.cmd"),
        }),
        DriverKind::Codex => Some(DriverBins {
            path_names: if cfg!(windows) {
                &["codex.exe"]
            } else {
                &["codex"]
            },
            cmd_name: cfg!(windows).then_some("codex.cmd"),
        }),
        DriverKind::Grok => Some(DriverBins {
            path_names: if cfg!(windows) {
                &["grok.exe"]
            } else {
                &["grok"]
            },
            cmd_name: cfg!(windows).then_some("grok.cmd"),
        }),
        DriverKind::GenericTerminal => Some(DriverBins {
            path_names: if cfg!(windows) {
                &["powershell.exe", "pwsh.exe"]
            } else {
                &["bash"]
            },
            cmd_name: None,
        }),
        DriverKind::Prime => None,
    }
}

pub(crate) fn resolve_driver_executable(
    driver: DriverKind,
    search: bool,
) -> Result<ResolvedLaunchProgram> {
    let search_path = env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
    resolve_driver_executable_on_path(driver, &search_path, search)
}

pub(crate) fn resolve_driver_executable_on_path(
    driver: DriverKind,
    search_path: &std::ffi::OsStr,
    search: bool,
) -> Result<ResolvedLaunchProgram> {
    resolve_driver_executable_on_path_with(driver, search_path, search, true)
}

fn resolve_driver_executable_on_path_with(
    driver: DriverKind,
    search_path: &std::ffi::OsStr,
    search: bool,
    include_user_roots: bool,
) -> Result<ResolvedLaunchProgram> {
    let Some(bins) = bins_for(driver) else {
        return Err(anyhow!(
            "Prime executable resolution is owned by the Ubuntu WSL controller"
        ));
    };

    if let Ok(program) = find_direct_executable_on_path(bins.path_names, search_path) {
        return Ok(ResolvedLaunchProgram {
            program,
            prefix_args: Vec::new(),
        });
    }

    if let Some(cmd_name) = bins.cmd_name
        && let Some(resolved) = follow_cmd_shims_on_path(cmd_name, search_path)
    {
        return Ok(resolved);
    }

    if search
        && let Some(resolved) = search_usual_places(
            bins.path_names,
            bins.cmd_name,
            search_path,
            include_user_roots,
        )
    {
        return Ok(resolved);
    }

    Err(HarnessExecutableError {
        driver,
        names: bins.path_names.iter().map(|name| (*name).to_string()).collect(),
        searched: search,
    }
    .into())
}

pub(crate) fn find_direct_executable(names: &[&str]) -> Result<String> {
    let search_path = env::var_os("PATH").ok_or_else(|| anyhow!("PATH is not set"))?;
    find_direct_executable_on_path(names, &search_path)
}

pub(crate) fn find_direct_executable_on_path(
    names: &[&str],
    search_path: &std::ffi::OsStr,
) -> Result<String> {
    // `names` is an ordered preference list. Search every PATH directory for
    // the preferred executable before considering a fallback. On Windows this
    // keeps the built-in powershell.exe ahead of Store/App Execution Alias
    // pwsh.exe entries that cannot be launched in the session job.
    for name in names {
        for directory in env::split_paths(search_path) {
            let candidate = directory.join(name);
            if let Some(qualified) = qualify_direct_file(&candidate) {
                return Ok(qualified);
            }
        }
    }
    Err(anyhow!(
        "no supported direct executable found on PATH (looked for {})",
        names.join(", ")
    ))
}

fn qualify_direct_file(candidate: &Path) -> Option<String> {
    let Ok(metadata) = fs::metadata(candidate) else {
        return None;
    };
    if !metadata.is_file() {
        return None;
    }
    let canonical = fs::canonicalize(candidate).ok()?;
    Some(
        crate::child_process_path(&canonical)
            .to_string_lossy()
            .into_owned(),
    )
}

fn follow_cmd_shims_on_path(
    cmd_name: &str,
    search_path: &std::ffi::OsStr,
) -> Option<ResolvedLaunchProgram> {
    for directory in env::split_paths(search_path) {
        let shim = directory.join(cmd_name);
        if !shim.is_file() {
            continue;
        }
        if let Some(resolved) = resolve_cmd_shim(&shim, search_path) {
            return Some(resolved);
        }
    }
    None
}

fn resolve_cmd_shim(
    cmd_path: &Path,
    search_path: &std::ffi::OsStr,
) -> Option<ResolvedLaunchProgram> {
    let text = fs::read_to_string(cmd_path).ok()?;
    let dp0 = cmd_path.parent()?;
    let refs = quoted_cmd_paths(&text, dp0);

    // Prefer a native harness exe (not node / cmd / powershell).
    for candidate in &refs {
        if is_harness_native_exe(candidate)
            && let Some(program) = qualify_direct_file(candidate)
        {
            return Some(ResolvedLaunchProgram {
                program,
                prefix_args: Vec::new(),
            });
        }
    }

    // npm js shim: node.exe + the .js named in the cmd (Codex's layout).
    let script = refs.iter().find(|path| is_js_file(path) && path.is_file())?;
    let node_names: &[&str] = if cfg!(windows) {
        &["node.exe"]
    } else {
        &["node"]
    };
    let node = qualify_direct_file(&dp0.join(node_names[0]))
        .or_else(|| find_direct_executable_on_path(node_names, search_path).ok())
        .or_else(|| find_direct_executable(node_names).ok())?;
    let script = fs::canonicalize(script).ok()?;
    Some(ResolvedLaunchProgram {
        program: node,
        prefix_args: vec![crate::child_process_path(&script)
            .to_string_lossy()
            .into_owned()],
    })
}

fn quoted_cmd_paths(text: &str, dp0: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('"') {
        rest = &rest[start + 1..];
        let Some(end) = rest.find('"') else {
            break;
        };
        let inner = &rest[..end];
        rest = &rest[end + 1..];
        if inner.is_empty() {
            continue;
        }
        paths.push(expand_cmd_path(inner, dp0));
    }
    paths
}

fn expand_cmd_path(raw: &str, dp0: &Path) -> PathBuf {
    let mut dp0_text = dp0.to_string_lossy().into_owned();
    if !dp0_text.ends_with(['\\', '/']) {
        dp0_text.push(if cfg!(windows) { '\\' } else { '/' });
    }
    let expanded = raw
        .replace("%~dp0", &dp0_text)
        .replace("%~DP0", &dp0_text)
        .replace("%dp0%", &dp0_text)
        .replace("%DP0%", &dp0_text);
    PathBuf::from(expanded)
}

fn is_harness_native_exe(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".exe") && cfg!(windows) {
        return false;
    }
    !matches!(
        lower.as_str(),
        "node.exe"
            | "nodejs.exe"
            | "cmd.exe"
            | "powershell.exe"
            | "pwsh.exe"
            | "wscript.exe"
            | "cscript.exe"
    )
}

fn is_js_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("js"))
}

fn search_usual_places(
    path_names: &[&str],
    cmd_name: Option<&str>,
    search_path: &std::ffi::OsStr,
    include_user_roots: bool,
) -> Option<ResolvedLaunchProgram> {
    let mut places = Vec::new();
    for name in path_names {
        places.extend(usual_place_candidates(name, search_path, include_user_roots));
    }
    for candidate in &places {
        if let Some(program) = qualify_direct_file(candidate) {
            return Some(ResolvedLaunchProgram {
                program,
                prefix_args: Vec::new(),
            });
        }
    }
    if let Some(cmd_name) = cmd_name {
        for candidate in usual_cmd_candidates(cmd_name, search_path, include_user_roots) {
            if candidate.is_file()
                && let Some(resolved) = resolve_cmd_shim(&candidate, search_path)
            {
                return Some(resolved);
            }
        }
    }
    None
}

fn usual_place_candidates(
    exe_name: &str,
    search_path: &std::ffi::OsStr,
    include_user_roots: bool,
) -> Vec<PathBuf> {
    let mut places = Vec::new();
    for directory in env::split_paths(search_path) {
        places.push(directory.join(exe_name));
        places.extend(npm_bin_candidates(&directory, exe_name));
    }
    if include_user_roots {
        for root in user_install_roots() {
            places.push(root.join(exe_name));
            places.extend(npm_bin_candidates(&root, exe_name));
        }
    }
    places
}

fn usual_cmd_candidates(
    cmd_name: &str,
    search_path: &std::ffi::OsStr,
    include_user_roots: bool,
) -> Vec<PathBuf> {
    let mut places = Vec::new();
    for directory in env::split_paths(search_path) {
        places.push(directory.join(cmd_name));
    }
    if include_user_roots {
        for root in user_install_roots() {
            places.push(root.join(cmd_name));
        }
    }
    places
}

fn user_install_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = home_dir() {
        roots.push(home.join(".local").join("bin"));
        roots.push(home.join(".grok").join("bin"));
    }
    if let Some(appdata) = env::var_os("APPDATA") {
        roots.push(PathBuf::from(appdata).join("npm"));
    }
    if let Some(local) = env::var_os("LOCALAPPDATA") {
        roots.push(PathBuf::from(local).join("npm"));
    }
    if let Some(program_files) = env::var_os("ProgramFiles") {
        roots.push(PathBuf::from(program_files).join("nodejs"));
    }
    if let Some(program_files) = env::var_os("ProgramFiles(x86)") {
        roots.push(PathBuf::from(program_files).join("nodejs"));
    }
    roots
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
}

fn npm_bin_candidates(prefix: &Path, exe_name: &str) -> Vec<PathBuf> {
    let node_modules = prefix.join("node_modules");
    let Ok(entries) = fs::read_dir(&node_modules) else {
        return Vec::new();
    };
    let mut places = Vec::new();
    for entry in entries.flatten().take(64) {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('@') {
            let Ok(packages) = fs::read_dir(entry.path()) else {
                continue;
            };
            for package in packages.flatten().take(32) {
                places.push(package.path().join("bin").join(exe_name));
            }
        } else {
            places.push(entry.path().join("bin").join(exe_name));
        }
    }
    places
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn write_file(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, bytes).unwrap();
    }

    fn claude_cmd_text() -> &'static str {
        r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0
"%dp0%\node_modules\@anthropic-ai\claude-code\bin\claude.exe"   %*
"#
    }

    fn codex_cmd_text() -> &'static str {
        r#"@ECHO off
GOTO start
:find_dp0
SET dp0=%~dp0
EXIT /b
:start
SETLOCAL
CALL :find_dp0

IF EXIST "%dp0%\node.exe" (
  SET "_prog=%dp0%\node.exe"
) ELSE (
  SET "_prog=node"
)

endLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & "%_prog%"  "%dp0%\node_modules\@openai\codex\bin\codex.js" %*
"#
    }

    #[cfg(windows)]
    #[test]
    fn path_exe_wins_before_cmd_shim() {
        let root = tempfile::tempdir().unwrap();
        write_file(&root.path().join("claude.exe"), b"direct");
        write_file(&root.path().join("claude.cmd"), claude_cmd_text().as_bytes());
        write_file(
            &root
                .path()
                .join("node_modules/@anthropic-ai/claude-code/bin/claude.exe"),
            b"nested",
        );
        let search = OsString::from(root.path().as_os_str());
        let resolved =
            resolve_driver_executable_on_path(DriverKind::Claude, &search, false).unwrap();
        let expected = crate::child_process_path(
            &fs::canonicalize(root.path().join("claude.exe")).unwrap(),
        );
        assert_eq!(PathBuf::from(resolved.program), expected);
        assert!(resolved.prefix_args.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn cmd_shim_follows_nested_native_exe() {
        let root = tempfile::tempdir().unwrap();
        write_file(&root.path().join("claude.cmd"), claude_cmd_text().as_bytes());
        let nested = root
            .path()
            .join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        write_file(&nested, b"nested-claude");
        let search = OsString::from(root.path().as_os_str());
        let resolved =
            resolve_driver_executable_on_path(DriverKind::Claude, &search, false).unwrap();
        let expected = crate::child_process_path(&fs::canonicalize(&nested).unwrap());
        assert_eq!(PathBuf::from(resolved.program), expected);
        assert!(resolved.prefix_args.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn cmd_shim_follows_node_plus_js() {
        let root = tempfile::tempdir().unwrap();
        write_file(&root.path().join("codex.cmd"), codex_cmd_text().as_bytes());
        write_file(&root.path().join("node.exe"), b"node");
        let script = root
            .path()
            .join("node_modules/@openai/codex/bin/codex.js");
        write_file(&script, b"module.exports = {}");
        let search = OsString::from(root.path().as_os_str());
        let resolved =
            resolve_driver_executable_on_path(DriverKind::Codex, &search, false).unwrap();
        let expected_node =
            crate::child_process_path(&fs::canonicalize(root.path().join("node.exe")).unwrap());
        let expected_js = crate::child_process_path(&fs::canonicalize(&script).unwrap());
        assert_eq!(PathBuf::from(resolved.program), expected_node);
        assert_eq!(resolved.prefix_args, vec![expected_js.to_string_lossy().into_owned()]);
    }

    #[cfg(windows)]
    #[test]
    fn missing_on_path_is_searchable_until_search_runs() {
        let root = tempfile::tempdir().unwrap();
        let search = OsString::from(root.path().as_os_str());
        let error = resolve_driver_executable_on_path(DriverKind::Grok, &search, false)
            .unwrap_err()
            .downcast::<HarnessExecutableError>()
            .unwrap();
        assert!(!error.searched);
        assert_eq!(error.kind(), Some(SessionErrorKind::ExecutableNotFound));
        assert!(error.to_string().contains("Can't find grok.exe on PATH"));

        // Isolate from the real ~/.grok/bin so this miss is about the fixture.
        let error = resolve_driver_executable_on_path_with(
            DriverKind::Grok,
            &search,
            true,
            false,
        )
        .unwrap_err()
        .downcast::<HarnessExecutableError>()
        .unwrap();
        assert!(error.searched);
        assert_eq!(error.kind(), None);
        assert!(
            error
                .to_string()
                .contains("Looked in the usual places and still can't find grok.exe")
        );
    }

    #[cfg(windows)]
    #[test]
    fn search_finds_nested_npm_bin_off_path() {
        let root = tempfile::tempdir().unwrap();
        let prefix = root.path().join("nodejs");
        let nested = prefix.join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        write_file(&nested, b"searched-claude");
        // PATH is empty of the prefix; search still walks usual-place node_modules
        // only for PATH entries + user roots. Seed PATH with the prefix so the
        // bounded npm walk sees it, without a claude.exe/cmd at the prefix root.
        let search = OsString::from(prefix.as_os_str());
        let resolved =
            resolve_driver_executable_on_path(DriverKind::Claude, &search, true).unwrap();
        let expected = crate::child_process_path(&fs::canonicalize(&nested).unwrap());
        assert_eq!(PathBuf::from(resolved.program), expected);
    }

    #[cfg(windows)]
    #[test]
    fn host_path_resolves_claude_through_cmd_shim_when_installed() {
        match resolve_driver_executable(DriverKind::Claude, false) {
            Ok(resolved) => {
                let lower = resolved.program.to_ascii_lowercase();
                assert!(
                    lower.ends_with("claude.exe"),
                    "resolved Claude to {}, not a native exe",
                    resolved.program
                );
                assert!(
                    !lower.ends_with("claude.cmd"),
                    "must not spawn the cmd shim: {}",
                    resolved.program
                );
                assert!(resolved.prefix_args.is_empty());
            }
            Err(error) => {
                let missing = error.downcast::<HarnessExecutableError>().expect(
                    "missing Claude must be the searchable harness error, not a random anyhow",
                );
                assert!(!missing.searched);
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn name_preference_still_picks_powershell_before_pwsh() {
        let root = tempfile::tempdir().unwrap();
        let early = root.path().join("early");
        let late = root.path().join("late");
        write_file(&early.join("pwsh.exe"), b"fixture");
        write_file(&late.join("powershell.exe"), b"fixture");
        let search = env::join_paths([&early, &late]).unwrap();
        let resolved = find_direct_executable_on_path(
            &["powershell.exe", "pwsh.exe"],
            search.as_os_str(),
        )
        .unwrap();
        let expected = crate::child_process_path(
            &fs::canonicalize(late.join("powershell.exe")).unwrap(),
        );
        assert_eq!(PathBuf::from(resolved), expected);
    }
}
