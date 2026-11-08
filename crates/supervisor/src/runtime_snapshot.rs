//! v3 W7 (Claude PRIM-1): a v2 build cannot read room history a v3 build has
//! written (it parses journal records with `deny_unknown_fields` and fails
//! closed), so rolling back means restoring the runtime as it was before v3.
//! The upgrade helper that runs is the OLD build's, so v2 cannot take that
//! copy; v3 takes it itself on its first start on a runtime it has not
//! written yet, before anything opens a file for writing. Same disk (a sibling
//! folder), free space checked first, verified, never deleted by PRIM-1. A
//! marker file then records that this runtime is v3 format and where the copy
//! is. `scripts/rollback-runtime.ps1` restores it.
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail, ensure};

/// Written once the runtime is v3 format (after a verified snapshot, or on a
/// fresh runtime with nothing to keep).
pub const RUNTIME_FORMAT_FILE: &str = "runtime-format.json";
/// Room for the runtime to keep growing after the copy.
const SNAPSHOT_MARGIN_BYTES: u64 = 512 * 1024 * 1024;
/// Held open by this very process; ownership state, not data.
const NOT_COPIED: [&str; 1] = ["desktop-instance.lock"];
/// Any of these means an earlier build wrote this runtime.
const HISTORY: [&str; 3] = ["session-catalog-v1.json", "room-catalog-v1.json", "rooms"];

/// Before v3 writes to a runtime an earlier build wrote: a verified copy, or
/// an error that stops the start with the reason (the old data is untouched).
/// `free_bytes` is the free space on the runtime's disk (a seam for tests).
pub fn ensure_pre_v3_snapshot(runtime_dir: &Path, free_bytes: impl Fn(&Path) -> Result<u64>,
    now: chrono::DateTime<chrono::Utc>) -> Result<Option<PathBuf>> {
    let marker = runtime_dir.join(RUNTIME_FORMAT_FILE);
    if marker.exists() {
        return Ok(None);
    }
    if !HISTORY.iter().any(|name| runtime_dir.join(name).exists()) {
        write_marker(&marker, None, now)?;
        return Ok(None);
    }
    let (files, bytes) = tree_size(runtime_dir, true)?;
    let free = free_bytes(runtime_dir)?;
    if free < bytes.saturating_add(SNAPSHOT_MARGIN_BYTES) {
        bail!("PRIM-1 v3 keeps a copy of the earlier runtime before its first start, and {} has {} MB free of the {} MB \
            it needs. Nothing was changed: free some space and start again, or keep using the earlier build.",
            runtime_dir.display(), free / (1024 * 1024), bytes.saturating_add(SNAPSHOT_MARGIN_BYTES) / (1024 * 1024));
    }
    let name = runtime_dir.file_name().ok_or_else(|| anyhow!("the runtime folder has no name"))?.to_string_lossy();
    let target = runtime_dir.with_file_name(format!("{name}.snapshot-pre-v3-{}", now.format("%Y%m%dT%H%M%SZ")));
    ensure!(!target.exists(), "{} already exists; a snapshot is never overwritten", target.display());
    copy_tree(runtime_dir, &target, true).with_context(|| format!("the snapshot {} is incomplete and must not be used",
        target.display()))?;
    let copied = tree_size(&target, false)?;
    ensure!(copied == (files, bytes), "the snapshot {} does not match the runtime ({} files / {} bytes of {files} / {bytes}); \
        it must not be used", target.display(), copied.0, copied.1);
    let receipt = serde_json::json!({ "outcome": "snapshot_taken", "runtime": runtime_dir, "snapshot": target,
        "files": files, "bytes": bytes, "at_utc": now.to_rfc3339(),
        "why": "a v2 build cannot read room history v3 writes; restore this copy to roll back (scripts/rollback-runtime.ps1)" });
    fs::write(target.join("SNAPSHOT.json"), serde_json::to_vec_pretty(&receipt)?)?;
    write_marker(&marker, Some(&target), now)?;
    Ok(Some(target))
}

/// v3 W7 (Claude PRIM-1): the upgrade helper that started this build is the
/// old build's and never starts the old build again. When v3 stops BEFORE it
/// wrote anything (no marker yet), the build it replaced can start safely:
/// this finds it. Only an upgrade request in this runtime whose candidate is
/// exactly this executable (path and SHA-256), still recent, naming a source
/// build that exists with its recorded SHA-256. Read as plain JSON, so a
/// request written by another build's code still parses.
pub fn upgrade_source_to_restart(runtime_dir: &Path, current_exe: &Path, current_sha256: &str,
    now: chrono::DateTime<chrono::Utc>) -> Option<PathBuf> {
    if runtime_dir.join(RUNTIME_FORMAT_FILE).exists() { return None; }
    // The committed request names the staged candidate as a verbatim path
    // (`\\?\D:\…`, found on the real v2-helper upgrade); this process may see
    // it without the prefix.
    let plain = |path: &Path| {
        let text = path.to_string_lossy().into_owned();
        match text.strip_prefix(r"\\?\UNC\") {
            Some(rest) => format!(r"\\{rest}"),
            None => text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text),
        }
    };
    let same = |a: &Path, b: &Path| plain(a).eq_ignore_ascii_case(&plain(b));
    let mut best: Option<(chrono::DateTime<chrono::Utc>, PathBuf)> = None;
    for entry in fs::read_dir(runtime_dir.join("maintenance").join("upgrades")).ok()?.flatten() {
        let Ok(bytes) = fs::read(entry.path().join("request.json")) else { continue };
        let Ok(request) = serde_json::from_slice::<serde_json::Value>(&bytes) else { continue };
        let text = |pointer: &str| request.pointer(pointer).and_then(serde_json::Value::as_str).map(str::to_owned);
        let (Some(candidate), Some(candidate_sha), Some(source), Some(source_sha), Some(prepared), Some(expires)) =
            (text("/candidate/exe_path"), text("/candidate/exe_sha256"), text("/source/exe_path"), text("/source/exe_sha256"),
                text("/prepared_at"), text("/expires_at")) else { continue };
        let (Ok(prepared), Ok(expires)) = (chrono::DateTime::parse_from_rfc3339(&prepared), chrono::DateTime::parse_from_rfc3339(&expires))
            else { continue };
        let source = PathBuf::from(source);
        if !same(Path::new(&candidate), current_exe) || !candidate_sha.eq_ignore_ascii_case(current_sha256)
            || now > expires.with_timezone(&chrono::Utc) + chrono::Duration::hours(1)
            || !file_sha256(&source).is_ok_and(|sha| sha.eq_ignore_ascii_case(&source_sha)) { continue; }
        let prepared = prepared.with_timezone(&chrono::Utc);
        if best.as_ref().is_none_or(|(at, _)| prepared > *at) { best = Some((prepared, source)); }
    }
    best.map(|(_, source)| source)
}

/// Starts `source` on its own (not a child of this process), with this
/// process's environment (the same runtime and profile).
#[cfg(windows)]
pub fn start_detached(source: &Path) -> Result<()> {
    use std::os::windows::process::CommandExt as _;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    std::process::Command::new(source)
        .current_dir(source.parent().unwrap_or(Path::new(".")))
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
        .spawn().with_context(|| format!("cannot start {}", source.display()))?;
    Ok(())
}

#[cfg(not(windows))]
pub fn start_detached(_source: &Path) -> Result<()> {
    bail!("starting the previous build is Windows only")
}

pub fn file_sha256(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(&bytes)))
}

/// The desktop's call when this build stopped before writing: the build it
/// replaced, started again, if one is found.
pub fn restart_replaced_build(runtime_dir: &Path) -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let sha = file_sha256(&current).ok()?;
    let source = upgrade_source_to_restart(runtime_dir, &current, &sha, chrono::Utc::now())?;
    start_detached(&source).ok()?;
    Some(source)
}

/// The desktop's call, at startup right after it owns the runtime.
pub fn ensure_pre_v3_snapshot_now(runtime_dir: &Path) -> Result<Option<PathBuf>> {
    ensure_pre_v3_snapshot(runtime_dir, disk_free_bytes, chrono::Utc::now())
}

fn write_marker(marker: &Path, snapshot: Option<&Path>, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    let value = serde_json::json!({ "format": 3, "since_utc": now.to_rfc3339(), "pre_v3_snapshot": snapshot });
    let temporary = marker.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec_pretty(&value)?)?;
    fs::rename(&temporary, marker)?;
    Ok(())
}

/// Files and bytes under `root`; at the top level, without the files never copied.
fn tree_size(root: &Path, top: bool) -> Result<(u64, u64)> {
    let (mut files, mut bytes) = (0, 0);
    for entry in fs::read_dir(root).with_context(|| format!("cannot read {}", root.display()))? {
        let entry = entry?;
        let name = entry.file_name();
        if top && NOT_COPIED.iter().any(|skip| name == *skip) { continue; }
        let kind = entry.file_type()?;
        ensure!(!kind.is_symlink(), "{} is a link; the runtime snapshot copies only plain folders and files", entry.path().display());
        if kind.is_dir() {
            let (inner_files, inner_bytes) = tree_size(&entry.path(), false)?;
            files += inner_files; bytes += inner_bytes;
        } else {
            files += 1; bytes += entry.metadata()?.len();
        }
    }
    Ok((files, bytes))
}

fn copy_tree(from: &Path, to: &Path, top: bool) -> Result<()> {
    fs::create_dir(to).with_context(|| format!("cannot create {}", to.display()))?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if top && NOT_COPIED.iter().any(|skip| name == *skip) { continue; }
        let target = to.join(&name);
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target, false)?;
        } else {
            fs::copy(entry.path(), &target).with_context(|| format!("cannot copy {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Free bytes for this user on the disk that holds `path`.
#[cfg(windows)]
pub fn disk_free_bytes(path: &Path) -> Result<u64> {
    use std::os::windows::ffi::OsStrExt as _;
    let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    // SAFETY: a NUL-terminated path and valid out-pointers (the others may be null).
    let ok = unsafe { windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
        wide.as_ptr(), &mut free, std::ptr::null_mut(), std::ptr::null_mut()) };
    if ok == 0 { bail!("cannot read the free space of {}: {}", path.display(), std::io::Error::last_os_error()); }
    Ok(free)
}

#[cfg(unix)]
pub fn disk_free_bytes(path: &Path) -> Result<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let bytes = path.as_os_str().as_bytes();
    let c_path = std::ffi::CString::new(bytes).context("the runtime path contains a NUL")?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: a NUL-terminated path and a writable statvfs.
    if unsafe { libc::statvfs(c_path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        bail!("cannot read the free space of {}: {}", path.display(), std::io::Error::last_os_error());
    }
    // SAFETY: statvfs succeeded and filled the structure.
    let stats = unsafe { stats.assume_init() };
    Ok(u64::from(stats.f_bavail).saturating_mul(u64::from(stats.f_frsize)))
}

#[cfg(not(any(windows, unix)))]
pub fn disk_free_bytes(_path: &Path) -> Result<u64> {
    bail!("free space is not read on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v2_runtime(root: &Path) -> PathBuf {
        let runtime = root.join("runtime");
        fs::create_dir_all(runtime.join("rooms")).unwrap();
        fs::write(runtime.join("session-catalog-v1.json"), b"{\"sessions\":[]}").unwrap();
        fs::write(runtime.join("rooms").join("a.jsonl"), b"{\"record\":\"header\"}\n").unwrap();
        fs::write(runtime.join("desktop-instance.lock"), b"").unwrap();
        runtime
    }

    #[test]
    fn a_runtime_an_earlier_build_wrote_is_copied_and_verified_once_before_v3_writes() {
        let root = tempfile::tempdir().unwrap();
        let runtime = v2_runtime(root.path());
        let now = chrono::Utc::now();
        let snapshot = ensure_pre_v3_snapshot(&runtime, |_| Ok(u64::MAX), now).unwrap().expect("a snapshot");
        assert!(snapshot.starts_with(root.path()) && snapshot.file_name().unwrap().to_string_lossy().starts_with("runtime.snapshot-pre-v3-"));
        assert_eq!(fs::read(snapshot.join("rooms").join("a.jsonl")).unwrap(), b"{\"record\":\"header\"}\n");
        assert!(!snapshot.join("desktop-instance.lock").exists(), "ownership state is not copied");
        assert!(snapshot.join("SNAPSHOT.json").exists() && runtime.join(RUNTIME_FORMAT_FILE).exists());
        assert_eq!(ensure_pre_v3_snapshot(&runtime, |_| Ok(u64::MAX), now).unwrap(), None, "once only");
    }

    /// Claude PRIM-1: a v3 that stops before writing finds the build it
    /// replaced only from an exact, recent request for itself.
    #[test]
    fn the_replaced_build_is_found_only_from_an_exact_recent_request_for_this_executable() {
        let root = tempfile::tempdir().unwrap();
        let runtime = v2_runtime(root.path());
        let source = root.path().join("v2.exe");
        fs::write(&source, b"the v2 build").unwrap();
        let candidate = root.path().join("v3.exe");
        fs::write(&candidate, b"the v3 build").unwrap();
        let (source_sha, candidate_sha) = (file_sha256(&source).unwrap(), file_sha256(&candidate).unwrap());
        let now = chrono::Utc::now();
        let request = |dir: &str, candidate_path: &Path, candidate_sha: &str, source_sha: &str,
            expires: chrono::DateTime<chrono::Utc>| {
            let attempt = runtime.join("maintenance").join("upgrades").join(dir);
            fs::create_dir_all(&attempt).unwrap();
            fs::write(attempt.join("request.json"), serde_json::to_vec(&serde_json::json!({
                "prepared_at": now.to_rfc3339(), "expires_at": expires.to_rfc3339(),
                "source": { "exe_path": source, "exe_sha256": source_sha },
                "candidate": { "exe_path": candidate_path, "exe_sha256": candidate_sha } })).unwrap()).unwrap();
        };
        let find = || upgrade_source_to_restart(&runtime, &candidate, &candidate_sha, now);
        request("other-candidate", &root.path().join("v4.exe"), &candidate_sha, &source_sha, now);
        request("other-hash", &candidate, "00", &source_sha, now);
        request("changed-source", &candidate, &candidate_sha, "00", now);
        request("expired", &candidate, &candidate_sha, &source_sha, now - chrono::Duration::hours(3));
        assert_eq!(find(), None);
        // The real v2 helper commits the staged candidate as a verbatim path.
        let verbatim = PathBuf::from(format!(r"\\?\{}", candidate.display()));
        request("exact", &verbatim, &candidate_sha, &source_sha, now + chrono::Duration::minutes(10));
        assert_eq!(find(), Some(source.clone()));
        // Once v3 has written (the marker), the old build is never started.
        fs::write(runtime.join(RUNTIME_FORMAT_FILE), b"{}").unwrap();
        assert_eq!(find(), None);
    }

    #[test]
    fn a_fresh_runtime_has_nothing_to_keep_and_a_full_disk_stops_the_start_untouched() {
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("fresh");
        fs::create_dir_all(&fresh).unwrap();
        assert_eq!(ensure_pre_v3_snapshot(&fresh, |_| Ok(0), chrono::Utc::now()).unwrap(), None);
        assert!(fresh.join(RUNTIME_FORMAT_FILE).exists());

        let runtime = v2_runtime(&root.path().join("full"));
        let error = ensure_pre_v3_snapshot(&runtime, |_| Ok(1024), chrono::Utc::now()).unwrap_err();
        assert!(error.to_string().contains("Nothing was changed"), "{error:#}");
        assert!(!runtime.join(RUNTIME_FORMAT_FILE).exists(), "no marker without a snapshot");
        let siblings: Vec<_> = fs::read_dir(runtime.parent().unwrap()).unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(siblings, ["runtime"], "no partial snapshot is left");
    }
}
