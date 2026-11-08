//! A disposable trial starts held and can never promote itself. These receipts
//! prove only that this backend and renderer reached the instrumented stages.
use super::*;
use sha2::{Digest, Sha256};
use std::io::Read;

pub const TRIAL_ADMISSION_HELD: &str =
    "preview trial is held; agent starts, input and mutations are disabled";

#[derive(Clone, Default)]
pub enum StartupAdmission {
    #[default]
    Normal,
    TrialHeld(TrialAdmission),
}

/// Constructed only from validated, attempt-private trial paths.
#[derive(Clone)]
pub struct TrialAdmission {
    attempt_id: Uuid,
    root: PathBuf,
    runtime_dir: PathBuf,
}

impl StartupAdmission {
    pub(crate) fn is_trial(&self) -> bool {
        matches!(self, Self::TrialHeld(_))
    }

    fn attempt_id(&self) -> Option<Uuid> {
        match self {
            Self::Normal => None,
            Self::TrialHeld(trial) => Some(trial.attempt_id),
        }
    }

    pub(crate) fn validate_config(&self, config: &SupervisorConfig) -> Result<()> {
        if let Self::TrialHeld(trial) = self {
            let runtime = canonical_destination_without_reparse_points(&config.runtime_dir)?;
            if comparable_storage_path(&runtime) != comparable_storage_path(&trial.runtime_dir) {
                bail!("trial supervisor must use the validated attempt runtime");
            }
            let working = canonical_destination_without_reparse_points(&config.working_root)?;
            if paths_overlap(&trial.root, &working) {
                bail!("trial attempt and agent working root must be disjoint");
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TrialChallenge {
    pub attempt_id: Uuid,
    pub boot_nonce: Uuid,
    pub candidate_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialHealthReceipt {
    pub schema_version: u32,
    pub challenge: TrialChallenge,
    pub stage: String,
    pub process_id: u32,
    pub elapsed_ms: u64,
    pub admission: String,
    pub readiness_verified: bool,
    pub hold_reasons: Vec<String>,
}

/// This owns no updater or process handles. The launcher must supply a copied
/// runtime/profile; compatibility and quiescence are deliberately unverified.
pub struct TrialStartup {
    root: PathBuf,
    runtime: PathBuf,
    profile: PathBuf,
    challenge: TrialChallenge,
    started: Instant,
    health_window: Duration,
    backend_recorded: Mutex<bool>,
    #[cfg(test)]
    before_receipt_for_tests: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl TrialStartup {
    /// Validate before creating any WebView or supervisor. The final root
    /// component must be the attempt UUID. No inherited/default profile is used.
    pub fn prepare(attempt_id: Uuid, root: &Path, health_window: Duration) -> Result<Self> {
        let executable = std::env::current_exe().context("cannot locate trial executable")?;
        Self::prepare_with_executable(attempt_id, root, health_window, &executable)
    }

    fn prepare_with_executable(
        attempt_id: Uuid,
        root: &Path,
        health_window: Duration,
        executable: &Path,
    ) -> Result<Self> {
        if attempt_id.is_nil()
            || !(Duration::from_secs(1)..=Duration::from_secs(300)).contains(&health_window)
        {
            bail!(
                "trial requires a non-nil attempt UUID and an explicit 1..300 second health window"
            );
        }
        if !root.is_absolute()
            || root.file_name().and_then(|name| name.to_str()) != Some(&attempt_id.to_string())
        {
            bail!("trial root must be absolute and end in its attempt UUID");
        }
        let root = canonical_destination_without_reparse_points(root)?;
        rebuild::reject_live_overlap(&root)?;
        let local =
            crate::local_app_data_root().ok_or_else(|| anyhow!("LOCALAPPDATA is required"))?;
        let local = canonical_destination_without_reparse_points(Path::new(&local))?;
        if paths_overlap(&root, &local.join("io.prim1.v2")) {
            bail!("trial root must be separate from the shared v2 profile/runtime");
        }
        let runtime = canonical_destination_without_reparse_points(&root.join("runtime"))?;
        let profile = canonical_destination_without_reparse_points(&root.join("profile"))?;
        if !runtime.is_dir() || !profile.is_dir() {
            bail!("trial requires pre-staged private runtime and profile directories");
        }
        for name in [SESSION_CATALOG_FILE_NAME, ROOM_CATALOG_FILE_NAME] {
            let path = runtime.join(name);
            ensure_existing_path_chain_has_no_reparse_points(&path)?;
            if !path.is_file() {
                bail!("trial requires existing copied session and room catalogs");
            }
        }
        for name in ["backend-v1.json", "renderer-v1.json"] {
            if root.join(name).try_exists()?
                || root.join(format!(".{name}.pending")).try_exists()?
            {
                bail!("trial attempt already has health evidence; use a new attempt");
            }
        }
        let mut input = File::open(executable).context("cannot hash current trial executable")?;
        let size = input.metadata()?.len();
        if !(1..=512 * 1024 * 1024).contains(&size) {
            bail!("trial executable exceeds its hash bound");
        }
        let mut hash = Sha256::new();
        let mut bytes = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let n = input.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            bytes += n as u64;
            if bytes > size {
                bail!("trial executable changed while hashing");
            }
            hash.update(&buffer[..n]);
        }
        if bytes != size {
            bail!("trial executable changed while hashing");
        }
        Ok(Self {
            root,
            runtime,
            profile,
            challenge: TrialChallenge {
                attempt_id,
                boot_nonce: Uuid::new_v4(),
                candidate_sha256: format!("{:x}", hash.finalize()),
            },
            started: Instant::now(),
            health_window,
            backend_recorded: Mutex::new(false),
            #[cfg(test)]
            before_receipt_for_tests: Mutex::new(None),
        })
    }

    pub fn runtime_dir(&self) -> &Path {
        &self.runtime
    }
    pub fn webview_profile_dir(&self) -> &Path {
        &self.profile
    }
    pub fn challenge(&self) -> TrialChallenge {
        self.challenge.clone()
    }
    pub fn admission(&self) -> StartupAdmission {
        StartupAdmission::TrialHeld(TrialAdmission {
            attempt_id: self.challenge.attempt_id,
            root: self.root.clone(),
            runtime_dir: self.runtime.clone(),
        })
    }

    pub fn record_backend_initialized(
        &self,
        supervisor: &SupervisorHandle,
    ) -> Result<TrialHealthReceipt> {
        let mut recorded = self.backend_recorded.lock();
        let _shutdown = supervisor.inner.shutdown_lifecycle.read();
        if *recorded {
            bail!("backend receipt requires this attempt's held supervisor exactly once");
        }
        self.validate_current_backend(supervisor)?;
        let receipt = self.write_receipt("backend-v1.json", "backend_initialized_held")?;
        *recorded = true;
        Ok(receipt)
    }

    /// The desktop invoke handler must pass the actual calling WebView label,
    /// never a renderer-supplied label. This acknowledgement is not promotion.
    pub fn record_renderer_acknowledged(
        &self,
        supervisor: &SupervisorHandle,
        actual_webview_label: &str,
        challenge: &TrialChallenge,
    ) -> Result<TrialHealthReceipt> {
        let recorded = self.backend_recorded.lock();
        let _shutdown = supervisor.inner.shutdown_lifecycle.read();
        if actual_webview_label != "main" || !*recorded || challenge != &self.challenge {
            bail!("renderer acknowledgement does not match this main WebView/backend boot");
        }
        self.validate_current_backend(supervisor)?;
        self.write_receipt("renderer-v1.json", "renderer_acknowledged_held")
    }

    fn validate_current_backend(&self, supervisor: &SupervisorHandle) -> Result<()> {
        if supervisor.inner.startup_admission.attempt_id() != Some(self.challenge.attempt_id)
            || comparable_storage_path(supervisor.runtime_dir())
                != comparable_storage_path(&self.runtime)
        {
            bail!("health receipt does not match the held trial supervisor");
        }
        let observation = supervisor.doctor()?;
        if observation.shutdown_started
            || observation.active_or_uncertain_members != 0
            || observation.control_plane_registered
        {
            bail!("trial backend is shut down, active or listening; health evidence refused");
        }
        Ok(())
    }

    fn write_receipt(&self, name: &str, stage: &str) -> Result<TrialHealthReceipt> {
        let elapsed = self.started.elapsed();
        if elapsed > self.health_window {
            bail!("trial health window expired; admission remains held");
        }
        if self.root.join(name).try_exists()? {
            bail!("trial health stage already recorded");
        }
        let receipt = TrialHealthReceipt {
            schema_version: 1,
            challenge: self.challenge.clone(),
            stage: stage.into(),
            process_id: std::process::id(),
            elapsed_ms: elapsed.as_millis() as u64,
            admission: "held".into(),
            readiness_verified: false,
            hold_reasons: vec![
                "quiescent_checkpoint_unverified".into(),
                "profile_checkpoint_unverified".into(),
                "production_smoke_unverified".into(),
                "conversation_recovery_unverified".into(),
                "bounded_health_acceptance_unverified".into(),
                "promotion_not_implemented".into(),
            ],
        };
        #[cfg(test)]
        if let Some(hook) = self.before_receipt_for_tests.lock().take() {
            hook();
        }
        // Revalidate before the path-based DACL mutation, not only later while
        // preparing the receipt file. A replaced root must not affect its target.
        ensure_existing_path_chain_has_no_reparse_points(&self.root)?;
        restrict_path_to_current_user(&self.root, true)?;
        rebuild::persist_bounded_json(&self.root, name, &receipt, 16 * 1024)?;
        Ok(receipt)
    }
}

#[cfg(test)]
#[path = "trial_tests.rs"]
mod tests;
