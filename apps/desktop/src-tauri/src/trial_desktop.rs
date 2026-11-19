use serde::Serialize;
use shared_types::RunId;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};
use supervisor::trial::TrialChallenge;

pub(crate) const ATTEMPT_ENV: &str = "PRIM1_TRIAL_ATTEMPT_ID";
pub(crate) const ROOT_ENV: &str = "PRIM1_TRIAL_ROOT";
pub(crate) const WINDOW_ENV: &str = "PRIM1_TRIAL_HEALTH_WINDOW_SECONDS";
pub const WEBVIEW_PROFILE_ENV: &str = "WEBVIEW2_USER_DATA_FOLDER";

pub(crate) struct TrialSettings {
    pub attempt_id: RunId,
    pub root: PathBuf,
    pub health_window: Duration,
}

pub(crate) fn parse_settings(
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<TrialSettings>, String> {
    let values = [lookup(ATTEMPT_ENV), lookup(ROOT_ENV), lookup(WINDOW_ENV)];
    if values.iter().all(Option::is_none) {
        return Ok(None);
    }
    let [Some(attempt), Some(root), Some(window)] = values else {
        return Err("trial startup requires all three PRIM1_TRIAL settings".into());
    };
    let attempt = attempt.to_str().ok_or("trial attempt must be a UUID")?;
    let attempt_id: RunId = attempt
        .parse()
        .map_err(|_| "trial attempt must be a UUID")?;
    if attempt_id.is_nil() || attempt_id.to_string() != attempt {
        return Err("trial attempt must be a canonical non-nil UUID".into());
    }
    let root = PathBuf::from(root);
    if !root.is_absolute() {
        return Err("trial root must be absolute".into());
    }
    let seconds = window
        .to_str()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=300).contains(seconds))
        .ok_or("trial health window must be 1..300 seconds")?;
    Ok(Some(TrialSettings {
        attempt_id,
        root,
        health_window: Duration::from_secs(seconds),
    }))
}

#[derive(Serialize)]
pub(crate) struct TrialStatus {
    pub challenge: TrialChallenge,
    pub admission: &'static str,
    pub backend_initialized: bool,
}

/// Use Tauri's OS-resolved local directory, independently of inherited
/// LOCALAPPDATA. Inspect the common ancestor, never the live children.
pub(crate) fn validate_known_local_root(attempt_root: &Path, local: &Path) -> Result<(), String> {
    let root = comparable(
        &attempt_root
            .canonicalize()
            .map_err(|error| error.to_string())?,
    );
    let local = local.canonicalize().map_err(|error| error.to_string())?;
    for protected in ["io.prim1.runtime", "Programs/PRIM-1", "io.prim1.v2"] {
        let protected = comparable(&local.join(protected));
        if root.starts_with(&protected) || protected.starts_with(&root) {
            return Err("trial attempt overlaps installed PRIM-1 or shared v2 storage".into());
        }
    }
    Ok(())
}

fn comparable(path: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    }
    #[cfg(not(windows))]
    {
        path.to_path_buf()
    }
}

/// Suppress automatic creation only for a trial. The caller subsequently uses
/// WebviewWindowBuilder::data_directory, which sets the runtime attribute
/// directly rather than depending on config-relative path resolution.
pub(crate) fn take_trial_main_config(
    config: &mut tauri::Config,
) -> Result<tauri::utils::config::WindowConfig, String> {
    if config.app.windows.len() != 1 || config.app.windows[0].label != "main" {
        return Err("held trial requires exactly the configured main window".into());
    }
    let main = config.app.windows[0].clone();
    config.app.windows[0].create = false;
    Ok(main)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(values: &[(&str, &str)]) -> Result<Option<TrialSettings>, String> {
        parse_settings(|key| {
            values
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| OsString::from(value))
        })
    }

    #[test]
    fn partial_blank_or_invalid_trial_never_falls_back_to_normal() {
        assert!(settings(&[]).unwrap().is_none());
        assert!(settings(&[(ATTEMPT_ENV, "")]).is_err());
        let id = "11111111-1111-4111-8111-111111111111";
        let root = std::env::temp_dir().join(id).to_string_lossy().into_owned();
        for seconds in ["", "0", "301", "1.5"] {
            assert!(
                settings(&[(ATTEMPT_ENV, id), (ROOT_ENV, &root), (WINDOW_ENV, seconds)]).is_err()
            );
        }
        assert!(
            settings(&[
                (ATTEMPT_ENV, id),
                (ROOT_ENV, "relative"),
                (WINDOW_ENV, "30")
            ])
            .is_err()
        );
        let parsed = settings(&[(ATTEMPT_ENV, id), (ROOT_ENV, &root), (WINDOW_ENV, "30")])
            .unwrap()
            .unwrap();
        assert_eq!(parsed.attempt_id.to_string(), id);
        assert_eq!(parsed.root, PathBuf::from(root));
        assert_eq!(parsed.health_window, Duration::from_secs(30));
    }

    #[test]
    fn only_trial_main_creation_is_deferred_and_configuration_is_preserved() {
        let mut config: tauri::Config =
            serde_json::from_str(include_str!("../tauri.conf.json")).unwrap();
        let before = serde_json::to_value(&config.app.windows[0]).unwrap();
        let main = take_trial_main_config(&mut config).unwrap();
        assert_eq!(serde_json::to_value(main).unwrap(), before);
        assert!(!config.app.windows[0].create);
        assert_eq!(config.identifier, "io.prim1.v2");
        config.app.windows[0].label = "other".into();
        assert!(take_trial_main_config(&mut config).is_err());
    }

    #[test]
    fn protected_root_check_uses_path_components_and_rejects_an_ancestor() {
        // An existing ancestor alone is enough: the check does not open any
        // protected installation/runtime children.
        let local = std::env::temp_dir();
        assert!(validate_known_local_root(&local, &local).is_err());
        let protected = comparable(&local.join("io.prim1.v2"));
        let sibling = comparable(&local.join("io.prim1.v2-trial"));
        assert!(!protected.starts_with(&sibling));
        assert!(!sibling.starts_with(&protected));
    }
}
