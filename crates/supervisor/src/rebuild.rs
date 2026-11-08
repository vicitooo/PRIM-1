//! Shared maintenance storage helpers: the private maintenance directory,
//! bounded committed JSON records, and the live-installation guard. The S7
//! restart preflight and candidate slot were retired by the A108 upgrade
//! transaction (`upgrade.rs`, `upgrade_flow.rs`).
use super::*;
#[path = "closed_checkpoint.rs"]
pub(crate) mod closed_checkpoint;
use std::io::Read;

pub(crate) fn active_or_uncertain(slot: &SessionSlot) -> bool {
    slot.running.is_some()
        || slot.spawn_in_flight.is_some()
        || slot.lifecycle_operation.is_some()
        || slot.termination_uncertain
}

pub(crate) fn maintenance_directory(runtime: &Path) -> Result<PathBuf> {
    let directory = canonical_destination_without_reparse_points(&runtime.join("maintenance"))?;
    // Maintenance writes belong only to preview storage. Never adopt the installed v1
    // runtime/install, including a destination that contains either one.
    reject_live_overlap(&directory)?;
    fs::create_dir_all(&directory)?;
    restrict_path_to_current_user(&directory, true)?;
    Ok(directory)
}

pub(crate) fn reject_live_overlap(path: &Path) -> Result<()> {
    let local = crate::local_app_data_root()
        .ok_or_else(|| anyhow!("LOCALAPPDATA is required to protect live maintenance paths"))?;
    reject_live_overlap_under(path, Path::new(&local))
}

pub(crate) fn reject_live_overlap_under(path: &Path, local: &Path) -> Result<()> {
    if !local.is_absolute() {
        bail!("LOCALAPPDATA must be an absolute directory");
    }
    // Canonicalize the common ancestor, not the protected live children. On
    // Windows this gives both sides the same verbatim drive/UNC prefix.
    let local = canonical_destination_without_reparse_points(local)?;
    if !local.is_dir() {
        bail!("LOCALAPPDATA must name an existing directory");
    }
    for live in [
        local.join("io.prim1.runtime"),
        local.join("Programs/PRIM-1"),
    ] {
        if paths_overlap(path, &live) {
            bail!("maintenance path overlaps the live installation or runtime");
        }
    }
    Ok(())
}

fn validated_file_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure_existing_path_chain_has_no_reparse_points(&absolute)?;
    let name = absolute
        .file_name()
        .ok_or_else(|| anyhow!("maintenance file has no name"))?;
    let parent = absolute
        .parent()
        .ok_or_else(|| anyhow!("maintenance file has no parent"))?;
    let destination = canonical_destination_without_reparse_points(parent)?.join(name);
    match fs::metadata(&destination) {
        Ok(metadata) if !metadata.is_file() => bail!("maintenance record is not a regular file"),
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    Ok(destination)
}

pub(crate) fn read_bounded_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    max: usize,
) -> Result<T> {
    let path = validated_file_path(path)?;
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        bail!("maintenance record exceeds its storage bound; existing bytes preserved");
    }
    serde_json::from_slice(&bytes).context("invalid maintenance record; existing bytes preserved")
}

pub(crate) fn persist_bounded_json(
    directory: &Path,
    name: &str,
    value: &impl Serialize,
    max: usize,
) -> Result<()> {
    let destination = validated_file_path(&directory.join(name))?;
    let pending = validated_file_path(&directory.join(format!(".{name}.pending")))?;
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    if bytes.len() > max {
        bail!("maintenance record exceeds its storage bound");
    }
    // One exclusive pending slot bounds interrupted writes across crashes. Do
    // not overwrite or remove it on failure: it may contain uncertain evidence.
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&pending)
        .context(
            "maintenance pending record exists or cannot be created; existing bytes preserved",
        )?;
    restrict_path_to_current_user(&pending, false)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    drop(file);
    replace_catalog_file(&pending, &destination, "maintenance record")
}

/// Call only after successfully parsing and validating the committed record.
/// Retain one uncertain write without a growing archive. Health/candidate
/// receipts are intentionally never recovered through this function.
pub(crate) fn retain_pending_once(directory: &Path, name: &str, max: usize) -> Result<()> {
    let pending = validated_file_path(&directory.join(format!(".{name}.pending")))?;
    let metadata = match fs::metadata(&pending) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        other => other?,
    };
    if metadata.len() > max as u64 {
        bail!("uncertain maintenance record exceeds its bound; bytes preserved");
    }
    let retained = validated_file_path(&directory.join(format!(".{name}.retained")))?;
    // Hard-link creation refuses an existing destination atomically. Removing
    // the pending name afterwards cannot lose the bytes. A crash between these
    // steps retains both names and fails closed on the next attempt.
    fs::hard_link(&pending, &retained)
        .context("maintenance evidence slot is occupied or unavailable; pending and committed bytes preserved")?;
    fs::remove_file(&pending).context(
        "maintenance evidence retained but pending name could not be cleared; writes remain held",
    )?;
    Ok(())
}
