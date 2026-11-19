//! Read-only observations and explicitly submitted incident metadata. Never
//! call snapshot()/refresh_session_liveness() from this path.
use super::*;
use shared_types::{DoctorCaller, DoctorReport, IssueReceipt};

const ISSUE_FILE: &str = "incidents-v1.json";
const ISSUE_MAX_BYTES: usize = 1024 * 1024;
const ISSUE_MAX_COUNT: usize = 128;
const SUMMARY_MAX_BYTES: usize = 2048;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Incident {
    incident_id: Uuid,
    stored_at: String,
    summary: String,
    /// The doctor report as stored; older builds wrote other fields.
    metadata: serde_json::Value,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IncidentStore {
    schema_version: u32,
    incidents: Vec<Incident>,
}

pub(crate) fn validate_summary(summary: &str) -> Result<()> {
    if summary.trim().is_empty() || summary.len() > SUMMARY_MAX_BYTES || summary.contains('\0') {
        bail!("issue summary must contain text, be at most 2048 bytes, and contain no NUL");
    }
    Ok(())
}

impl SupervisorHandle {
    pub fn doctor(&self) -> Result<DoctorReport> {
        self.doctor_for_caller(None)
    }

    fn doctor_for_caller(&self, caller: Option<&PaneCaller>) -> Result<DoctorReport> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        if let Some(caller) = caller {
            Self::validate_pane_caller_locked(caller, &slots)?;
        }
        let caller = caller.map(|caller| {
            let slot = slots
                .get_by_id(caller.session_id)
                .expect("validated caller");
            DoctorCaller {
                session_id: caller.session_id,
                run_id: caller.run_id,
                generation: caller.generation,
                driver: slot.definition.driver,
                lifecycle: slot.state,
                work_state: slot.work_state,
                recovery_present: slot.recovery.is_some(),
                conversation_reference_present: slot.harness_session_id.is_some(),
                launch_record_matches_run: slot.last_launch.as_ref().is_some_and(|record| {
                    record.run_id == caller.run_id && record.generation == caller.generation
                }),
            }
        });
        Ok(DoctorReport {
            schema_version: 1,
            generated_at: now_rfc3339(),
            supervisor_version: env!("CARGO_PKG_VERSION").into(),
            build_revision: option_env!("PRIM1_BUILD_REVISION").map(str::to_owned),
            os: std::env::consts::OS.into(),
            uptime_seconds: self.inner.started_at.elapsed().as_secs(),
            shutdown_started: self.inner.shutdown_started.load(Ordering::Acquire),
            control_plane_registered: self.inner.control_plane.read().is_some(),
            sessions: slots.by_id.len(),
            active_or_uncertain_members: slots
                .by_id
                .values()
                .filter(|slot| rebuild::active_or_uncertain(slot))
                .count(),
            recovery_items: slots
                .by_id
                .values()
                .filter(|slot| slot.recovery.is_some())
                .count(),
            rooms: rooms.catalog.rooms.len(),
            selected_rooms: rooms
                .catalog
                .rooms
                .iter()
                .filter(|room| room.resume_on_restart)
                .count(),
            caller,
            observation_scope:
                "stored supervisor observations; no liveness, storage, renderer or harness probe"
                    .into(),
            upgrade: self.upgrade_attempt_summary(),
        })
    }

    pub fn report_issue(&self, summary: String) -> Result<IssueReceipt> {
        self.report_issue_for_caller(None, summary)
    }

    fn report_issue_for_caller(
        &self,
        caller: Option<&PaneCaller>,
        summary: String,
    ) -> Result<IssueReceipt> {
        self.ensure_mutations_admitted()?;
        validate_summary(&summary)?;
        let _write = self.inner.maintenance_write.lock();
        let metadata = self.doctor_for_caller(caller)?;
        let directory = rebuild::maintenance_directory(&self.inner.runtime_dir)?;
        let path = directory.join(ISSUE_FILE);
        let committed_exists = path.try_exists()?;
        let mut store = if committed_exists {
            let store: IncidentStore = rebuild::read_bounded_json(&path, ISSUE_MAX_BYTES)?;
            if store.schema_version != 1 || store.incidents.len() > ISSUE_MAX_COUNT {
                bail!("incident store is unsupported or invalid; existing bytes preserved");
            }
            store
        } else {
            IncidentStore {
                schema_version: 1,
                incidents: Vec::new(),
            }
        };
        if committed_exists {
            rebuild::retain_pending_once(&directory, ISSUE_FILE, ISSUE_MAX_BYTES)?;
        }
        let incident_id = Uuid::new_v4();
        let stored_at = now_rfc3339();
        store.incidents.push(Incident {
            incident_id,
            stored_at: stored_at.clone(),
            summary,
            metadata: serde_json::to_value(metadata)?,
        });
        while store.incidents.len() > ISSUE_MAX_COUNT
            || serde_json::to_vec_pretty(&store)?.len() + 1 > ISSUE_MAX_BYTES
        {
            store.incidents.remove(0);
        }
        rebuild::persist_bounded_json(&directory, ISSUE_FILE, &store, ISSUE_MAX_BYTES)?;
        Ok(IssueReceipt {
            incident_id,
            stored_at,
            retained_incidents: store.incidents.len(),
        })
    }

    pub(crate) fn apply_maintenance_request(
        &self,
        caller: &PaneCaller,
        request: SidebandRequest,
    ) -> SidebandResponse {
        let result = match request {
            SidebandRequest::Doctor {} => self
                .doctor_for_caller(Some(caller))
                .map(|report| SidebandResponsePayload::Doctor { report }),
            SidebandRequest::ReportIssue { summary } => self
                .report_issue_for_caller(Some(caller), summary)
                .map(|receipt| SidebandResponsePayload::Issue { receipt }),
            _ => unreachable!("maintenance dispatch only"),
        };
        match result {
            Ok(payload) => SidebandResponse {
                ok: true,
                message: match &payload {
                    SidebandResponsePayload::Doctor { .. } => {
                        "stored observations read; health not probed"
                    }
                    _ => "incident metadata stored",
                }
                .into(),
                timed_out: false,
                payload: Some(payload),
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(error.to_string()),
        }
    }
}
