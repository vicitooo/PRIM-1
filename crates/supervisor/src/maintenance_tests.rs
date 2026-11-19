//! Isolated catalogs and synthetic runs only; no executable or named pipe starts.
use super::*;
use serde_json::{Value, json};
use shared_types::{CreateSessionRequest, PermissionProfile};

fn fixture() -> (tempfile::TempDir, SupervisorHandle) {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    fs::create_dir(&work).unwrap();
    let supervisor = SupervisorHandle::new(SupervisorConfig {
        working_root: work,
        runtime_dir: root.path().join("runtime"),
        pane_mcp_executable: None,
        heartbeat_interval: Some(Duration::from_secs(3600)),
        auto_restart_on_stall_sessions: Some(Vec::new()),
        auto_restart_stall_threshold: None,
        room_brief_on_join: false,
        task_folder: None,
    })
    .unwrap();
    (root, supervisor)
}

fn member(supervisor: &SupervisorHandle, driver: DriverKind) -> SessionId {
    supervisor
        .create_session(CreateSessionRequest { codex_transport: Default::default(), permissions: None,
            label: None,
            driver,
            permission_profile: PermissionProfile::Normal,
            linux_working_directory: None,
            room_id: None,
            launch_choices: None,
        })
        .unwrap()
        .session_id
}

fn synthetic_run(supervisor: &SupervisorHandle, id: SessionId) -> PaneCaller {
    let mut slots = supervisor.inner.slots.lock();
    let slot = slots.get_by_id_mut(id).unwrap();
    slot.running = Some(RunningSession::new(None).with_pane_secret(Some("fixture-secret".into())));
    slot.run_id = Some(Uuid::new_v4());
    slot.generation += 1;
    slot.state = LifecycleState::Busy;
    PaneCaller {
        session: slot.definition.alias.clone(),
        session_id: id,
        generation: slot.generation,
        run_id: slot.run_id.unwrap(),
        identity: PaneCallerIdentity::Secret("fixture-secret".into()),
    }
}

#[test]
fn doctor_is_read_only_and_uses_allowlisted_metadata() {
    let (_root, supervisor) = fixture();
    let id = member(&supervisor, DriverKind::Codex);
    let caller = synthetic_run(&supervisor, id);
    {
        let mut slots = supervisor.inner.slots.lock();
        let slot = slots.get_by_id_mut(id).unwrap();
        slot.harness_session_id = Some("sensitive-conversation".into());
        slot.last_error = Some("sensitive-error".into());
        slot.run_output_tail = "sensitive-output".into();
        slot.launch_choices.brief = SessionBriefChoice::Custom {
            text: "sensitive-task".into(),
        };
        slot.definition.working_dir = "sensitive-workspace".into();
    }
    let catalog_before = fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap();
    let audit_before = fs::read(supervisor.audit_log_path()).unwrap();
    let response = supervisor.apply_sideband_request(&caller, SidebandRequest::Doctor {});
    assert!(response.ok, "{}", response.message);
    let value = serde_json::to_string(&response).unwrap();
    for excluded in [
        "sensitive-",
        "fixture-secret",
        "prefix_args",
        "working_dir",
        "environment",
        "last_error",
    ] {
        assert!(!value.contains(excluded), "unexpected metadata: {excluded}");
    }
    assert_eq!(supervisor.doctor().unwrap().active_or_uncertain_members, 1);
    assert!(
        supervisor
            .inner
            .slots
            .lock()
            .get_by_id(id)
            .unwrap()
            .running
            .is_some()
    );
    assert_eq!(
        fs::read(session_catalog_path(supervisor.runtime_dir())).unwrap(),
        catalog_before
    );
    assert_eq!(fs::read(supervisor.audit_log_path()).unwrap(), audit_before);
    assert!(!supervisor.runtime_dir().join("maintenance").exists());
}

#[test]
fn maintenance_revalidates_stale_caller_before_observation_or_write() {
    for request in [
        SidebandRequest::Doctor {},
        SidebandRequest::ReportIssue {
            summary: "fixture".into(),
        },
    ] {
        let (_root, supervisor) = fixture();
        let id = member(&supervisor, DriverKind::Codex);
        let caller = synthetic_run(&supervisor, id);
        let handle = supervisor.clone();
        *supervisor.inner.sideband_after_initial_authorization.lock() = Some(Box::new(move || {
            handle
                .inner
                .slots
                .lock()
                .get_by_id_mut(id)
                .unwrap()
                .generation += 1;
        }));
        let response = supervisor.apply_sideband_request(&caller, request);
        assert!(!response.ok);
        assert!(response.message.contains("stale"));
        assert!(!supervisor.runtime_dir().join("maintenance").exists());
    }
}

#[test]
fn incident_store_is_bounded_durable_and_does_not_echo_summary() {
    let (_root, supervisor) = fixture();
    let first = supervisor
        .report_issue("deliberate-issue-text".into())
        .unwrap();
    let encoded = serde_json::to_string(&first).unwrap();
    assert!(!encoded.contains("deliberate-issue-text"));
    for n in 0..130 {
        supervisor.report_issue(format!("issue {n}")).unwrap();
    }
    let path = supervisor
        .runtime_dir()
        .join("maintenance/incidents-v1.json");
    let store: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(store["incidents"].as_array().unwrap().len(), 128);
    assert!(fs::metadata(&path).unwrap().len() <= 1024 * 1024);
    assert!(
        !fs::read_to_string(&path)
            .unwrap()
            .contains(&first.incident_id.to_string())
    );
    assert!(supervisor.report_issue(" ".into()).is_err());
    assert!(supervisor.report_issue("x".repeat(2049)).is_err());
    fs::write(&path, b"corrupt preserved fixture").unwrap();
    assert!(supervisor.report_issue("new issue".into()).is_err());
    assert_eq!(fs::read(path).unwrap(), b"corrupt preserved fixture");
}

#[test]
fn incidents_stored_before_the_upgrade_view_still_load() {
    let (_root, supervisor) = fixture();
    supervisor.report_issue("current issue".into()).unwrap();
    let path = supervisor.runtime_dir().join("maintenance/incidents-v1.json");
    let mut store: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    // A report stored by an S7 build carried its retired hold reasons.
    let metadata = &mut store["incidents"][0]["metadata"];
    metadata.as_object_mut().unwrap().remove("upgrade");
    metadata["rebuild_hold_reasons"] = json!(["executor_not_implemented"]);
    fs::write(&path, serde_json::to_vec_pretty(&store).unwrap()).unwrap();
    supervisor.report_issue("next issue".into()).unwrap();
    let store: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(store["incidents"].as_array().unwrap().len(), 2);
    assert_eq!(store["incidents"][0]["metadata"]["rebuild_hold_reasons"][0], "executor_not_implemented");
    assert!(store["incidents"][1]["metadata"].get("rebuild_hold_reasons").is_none());
}

#[test]
fn maintenance_recovers_one_pending_record_then_bounds_further_uncertainty() {
    let (_root, supervisor) = fixture();
    supervisor.report_issue("committed issue".into()).unwrap();
    let directory = supervisor.runtime_dir().join("maintenance");
    let incident_path = directory.join("incidents-v1.json");
    fs::write(
        directory.join(".incidents-v1.json.pending"),
        b"uncertain bytes from interrupted write",
    )
    .unwrap();
    supervisor
        .report_issue("accepted after retaining uncertainty".into())
        .unwrap();
    assert!(
        fs::read_to_string(&incident_path)
            .unwrap()
            .contains("committed issue")
    );
    // A normal later write still works: retained evidence does not block a
    // healthy store when no new uncertain pending file exists.
    supervisor
        .report_issue("later accepted issue".into())
        .unwrap();
    let incident_before = fs::read(&incident_path).unwrap();
    assert_eq!(
        fs::read(directory.join(".incidents-v1.json.retained")).unwrap(),
        b"uncertain bytes from interrupted write"
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    fs::write(directory.join(".incidents-v1.json.pending"), b"second uncertain write").unwrap();
    for _ in 0..3 {
        assert!(supervisor.report_issue("must remain held".into()).is_err());
    }
    assert_eq!(fs::read(&incident_path).unwrap(), incident_before);
    assert_eq!(
        fs::read(directory.join(".incidents-v1.json.pending")).unwrap(),
        b"second uncertain write"
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 3);
    assert!(
        rebuild::persist_bounded_json(&directory, "oversize.json", &"x".repeat(64), 32).is_err()
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 3);
}

#[test]
fn pending_recovery_refuses_corrupt_or_missing_committed_records() {
    let (_root, supervisor) = fixture();
    let directory = rebuild::maintenance_directory(supervisor.runtime_dir()).unwrap();
    fs::write(directory.join("incidents-v1.json"), b"corrupt committed evidence").unwrap();
    fs::write(directory.join(".incidents-v1.json.pending"), b"uncertain evidence").unwrap();
    assert!(supervisor.report_issue("must refuse".into()).is_err());
    assert_eq!(
        fs::read(directory.join("incidents-v1.json")).unwrap(),
        b"corrupt committed evidence"
    );
    assert_eq!(
        fs::read(directory.join(".incidents-v1.json.pending")).unwrap(),
        b"uncertain evidence"
    );
    assert!(!directory.join(".incidents-v1.json.retained").exists());
    let (_other_root, other) = fixture();
    let directory = rebuild::maintenance_directory(other.runtime_dir()).unwrap();
    fs::write(
        directory.join(".incidents-v1.json.pending"),
        b"uncertain first record",
    )
    .unwrap();
    assert!(
        other
            .report_issue("must not adopt missing committed state".into())
            .is_err()
    );
    assert!(!directory.join("incidents-v1.json").exists());
    assert_eq!(fs::read_dir(directory).unwrap().count(), 1);
}

#[test]
fn maintenance_protected_roots_use_the_same_canonical_prefix_without_live_reads() {
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local");
    fs::create_dir(&local).unwrap();
    // The protected children deliberately do not exist. Only this temp ancestor
    // may be inspected; Windows canonicalization adds its verbatim prefix.
    let canonical = fs::canonicalize(&local).unwrap();
    for path in [
        canonical.clone(),
        canonical.join("io.prim1.runtime"),
        canonical.join("io.prim1.runtime/runtime/maintenance"),
        canonical.join("Programs"),
        canonical.join("Programs/PRIM-1/prim1.exe"),
    ] {
        assert!(
            rebuild::reject_live_overlap_under(&path, &local).is_err(),
            "{}",
            path.display()
        );
    }
    assert!(
        rebuild::reject_live_overlap_under(
            &canonical.join("io.prim1.v2/runtime/maintenance"),
            &local
        )
        .is_ok()
    );
    assert!(
        rebuild::reject_live_overlap_under(&canonical.join("Programs/PRIM-1-other"), &local)
            .is_ok()
    );
    assert!(rebuild::reject_live_overlap_under(&canonical, Path::new("relative-root")).is_err());
    assert!(rebuild::reject_live_overlap_under(&canonical, &local.join("missing-root")).is_err());
    assert_eq!(fs::read_dir(local).unwrap().count(), 0);
}
