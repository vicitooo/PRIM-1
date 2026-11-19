//! Owned temporary files only: no process, scheduler, profile, task store or
//! supervisor is started. External validation remains the desktop/helper's job.
use super::*;
use crate::rebuild::persist_bounded_json;

struct Fixture {
    _dir: tempfile::TempDir,
    store: UpgradeStore,
    now: DateTime<Utc>,
    request: UpgradeRequest,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let runtime = dir.path().join("runtime");
        fs::create_dir(&runtime).unwrap();
        fs::write(
            dir.path().join("next.exe"),
            b"MZfixture executable, never run",
        )
        .unwrap();
        let source = UpgradeSource {
            pid: 123,
            creation_filetime: "134000000000000001".into(),
            exe_path: dir.path().join("old.exe"),
            exe_sha256: "a".repeat(64),
        };
        let requester = Uuid::new_v4();
        let member = UpgradeMember {
            session_id: requester,
            room_id: Some(Uuid::new_v4()),
            driver: DriverKind::Claude,
            run_id: Uuid::new_v4(),
            generation: 7,
            resume_mode: UpgradeResumeMode::Exact,
            conversation_id: Some("exact-owned-conversation".into()),
            task_ref: Some(TaskRef {
                task_id: "T108".into(),
                revision: 4,
            }),
            continuation_id: Uuid::nil(),
        };
        let request = UpgradeRequest::new(
            source,
            UpgradeCandidate {
                exe_path: dir.path().join("next.exe"),
                exe_sha256: digest(b"MZfixture executable, never run"),
            },
            runtime.clone(),
            dir.path().join("profile"),
            dir.path().join("external-tasks"),
            Some("off".into()),
            requester,
            vec![member],
        );
        let now = request.prepared_at;
        Self {
            _dir: dir,
            store: UpgradeStore::open(&runtime).unwrap(),
            now,
            request,
        }
    }
    fn prepare(&self) -> UpgradeRecord {
        self.store
            .prepare_at(self.request.clone(), self.now)
            .unwrap()
    }
    fn normalized(&self, request: &UpgradeRequest) -> UpgradeRequest {
        let mut request = request.clone();
        request.candidate.exe_path = self
            .store
            .attempt_dir(request.attempt_id)
            .join("candidate/prim1.exe");
        request
    }
    fn ready(&self, record: &UpgradeRecord) {
        persist_bounded_json(
            &self.store.attempt_dir(record.request.attempt_id),
            "helper-ready.json",
            &HelperReady {
                schema_version: SCHEMA_VERSION,
                attempt_id: record.request.attempt_id,
                request_sha256: record.state.request_sha256.clone(),
                helper_pid: 456,
                helper_creation_filetime: "134000000000000002".into(),
                at_utc: self.now,
            },
            RECORD_MAX_BYTES,
        )
        .unwrap();
    }
    fn close(&self) -> UpgradeRecord {
        let record = self.prepare();
        self.ready(&record);
        self.store
            .transition(
                self.request.attempt_id,
                &self.request.source,
                UpgradePhase::Closing,
                self.request.notice_until,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into(),
                }),
            )
            .unwrap()
    }
    fn shutdown(&self) -> UpgradeRecord {
        self.close();
        self.store
            .transition(
                self.request.attempt_id,
                &self.request.source,
                UpgradePhase::ShutdownComplete,
                self.request.notice_until + Duration::seconds(1),
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into(),
                }),
            )
            .unwrap()
    }
    fn result(
        &self,
        record: &UpgradeRecord,
        outcome: HelperOutcome,
        source: Option<UpgradeSource>,
    ) {
        persist_bounded_json(
            &self.store.attempt_dir(record.request.attempt_id),
            "helper-result.json",
            &HelperResult {
                schema_version: SCHEMA_VERSION,
                attempt_id: record.request.attempt_id,
                request_sha256: record.state.request_sha256.clone(),
                outcome,
                candidate_process: source,
                at_utc: self.request.notice_until + Duration::seconds(2),
                message: None,
            },
            RECORD_MAX_BYTES,
        )
        .unwrap();
    }
}

#[test]
fn immutable_retry_preserves_ids_and_does_not_open_external_settings() {
    let mut f = Fixture::new();
    // Files in place of directories would make an attempted TaskStore/profile
    // open fail. Prepare only records the caller's selection and exact metadata.
    fs::write(&f.request.task_folder, b"private external task bytes").unwrap();
    fs::write(&f.request.profile_dir, b"private profile bytes").unwrap();
    fs::write(
        f.request.runtime_dir.join("task-folder.json"),
        b"malformed, not ours to read",
    )
    .unwrap();
    let mut other = f.request.members[0].clone();
    other.session_id = Uuid::new_v4();
    other.run_id = Uuid::new_v4();
    other.continuation_id = Uuid::new_v4();
    other.task_ref = None;
    f.request.members.push(other);
    let record = f.prepare();
    assert_eq!(
        f.store
            .prepare_at(f.request.clone(), f.now)
            .unwrap()
            .request,
        f.normalized(&f.request)
    );
    assert_eq!(record.request.members[1].task_ref, None);
    assert_eq!(
        fs::read(&record.request_path).unwrap(),
        json_bytes(&f.normalized(&f.request), REQUEST_MAX_BYTES).unwrap()
    );
    assert_eq!(fs::read(&record.helper_path).unwrap(), HELPER);
    assert_eq!(
        fs::read(&f.request.task_folder).unwrap(),
        b"private external task bytes"
    );
    assert_eq!(
        fs::read(&f.request.profile_dir).unwrap(),
        b"private profile bytes"
    );
    let mut altered = f.request.clone();
    altered.members[0].conversation_id = Some("other-conversation".into());
    assert!(
        f.store
            .prepare_at(altered, f.now)
            .unwrap_err()
            .to_string()
            .contains("immutable")
    );
    assert_eq!(
        f.store.read(f.request.attempt_id).unwrap().request.members[0].continuation_id,
        f.request.members[0].continuation_id
    );
}

#[test]
fn notice_ready_and_successful_shutdown_are_distinct_boundaries() {
    let f = Fixture::new();
    let record = f.prepare();
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    f.ready(&record);
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until - Duration::milliseconds(1),
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    assert!(
        f.store
            .shutdown_complete(f.request.attempt_id, &f.request.source)
            .is_err()
    );
    assert_eq!(
        f.store.read(f.request.attempt_id).unwrap().state.phase,
        UpgradePhase::Prepared
    );
    let closing = f
        .store
        .transition(
            f.request.attempt_id,
            &f.request.source,
            UpgradePhase::Closing,
            f.request.notice_until,
            Some(&HelperIdentity {
                pid: 456,
                creation_filetime: "134000000000000002".into(),
            }),
        )
        .unwrap();
    assert_eq!(closing.state.phase, UpgradePhase::Closing);
    assert!(
        f.store
            .cancel(f.request.attempt_id, &f.request.source)
            .is_err()
    );
    let completed = f
        .store
        .transition(
            f.request.attempt_id,
            &f.request.source,
            UpgradePhase::ShutdownComplete,
            f.request.notice_until + Duration::seconds(1),
            Some(&HelperIdentity {
                pid: 456,
                creation_filetime: "134000000000000002".into(),
            }),
        )
        .unwrap();
    assert_eq!(completed.state.phase, UpgradePhase::ShutdownComplete);
    assert_eq!(
        f.store
            .shutdown_complete(f.request.attempt_id, &f.request.source)
            .unwrap()
            .state,
        completed.state
    );
    assert!(
        f.store
            .helper_result(f.request.attempt_id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn cancelled_or_expired_never_enters_closing() {
    let f = Fixture::new();
    let record = f.prepare();
    f.ready(&record);
    let cancelled = f
        .store
        .cancel(f.request.attempt_id, &f.request.source)
        .unwrap();
    assert_eq!(
        f.store
            .cancel(f.request.attempt_id, &f.request.source)
            .unwrap()
            .state,
        cancelled.state
    );
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    assert!(
        f.store
            .shutdown_complete(f.request.attempt_id, &f.request.source)
            .is_err()
    );
    let mut next = f.request.clone();
    next.attempt_id = Uuid::new_v4();
    assert!(f.store.prepare_at(next, f.now).is_ok());
    let expired = Fixture::new();
    let record = expired.prepare();
    expired.ready(&record);
    assert!(
        expired
            .store
            .transition(
                expired.request.attempt_id,
                &expired.request.source,
                UpgradePhase::Closing,
                expired.request.expires_at,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    assert_eq!(
        expired
            .store
            .read(expired.request.attempt_id)
            .unwrap()
            .state
            .phase,
        UpgradePhase::Prepared
    );
}

#[test]
fn wrong_source_wrong_ready_and_changed_request_cannot_close() {
    let f = Fixture::new();
    let record = f.prepare();
    f.ready(&record);
    let mut source = f.request.source.clone();
    source.creation_filetime = "134000000000000009".into();
    assert!(f.store.cancel(f.request.attempt_id, &source).is_err());
    let path = f
        .store
        .attempt_dir(f.request.attempt_id)
        .join("helper-ready.json");
    let mut ready: HelperReady = read_bounded_json(&path, RECORD_MAX_BYTES).unwrap();
    ready.request_sha256 = "c".repeat(64);
    fs::write(path, json_bytes(&ready, RECORD_MAX_BYTES).unwrap()).unwrap();
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    // A parseable request modification also fails its original byte binding.
    let mut altered = f.request.clone();
    altered.members[0].conversation_id = Some("different".into());
    fs::write(
        &record.request_path,
        json_bytes(&altered, REQUEST_MAX_BYTES).unwrap(),
    )
    .unwrap();
    assert!(f.store.read(f.request.attempt_id).is_err());
}

#[test]
fn ambiguous_outcomes_hold_but_definite_prestart_failure_admits_reopened_original() {
    for outcome in [
        None,
        Some(HelperOutcome::Launching),
        Some(HelperOutcome::Uncertain),
    ] {
        let f = Fixture::new();
        let record = f.shutdown();
        if let Some(outcome) = outcome {
            f.result(&record, outcome, None);
        }
        let mut next = f.request.clone();
        next.attempt_id = Uuid::new_v4();
        assert!(
            f.store.prepare_at(next, f.now).is_err(),
            "outcome {outcome:?}"
        );
        assert_eq!(
            f.store.current_attempt_id().unwrap(),
            Some(f.request.attempt_id)
        );
    }
    for outcome in [
        HelperOutcome::Failed,
        HelperOutcome::Cancelled,
        HelperOutcome::Expired,
    ] {
        let f = Fixture::new();
        let record = f.shutdown();
        f.result(&record, outcome, None);
        let mut next = f.request.clone();
        next.attempt_id = Uuid::new_v4();
        next.source.pid += 1;
        next.source.creation_filetime = "134000000000000009".into();
        // Failed replacement is not permission to adopt an unrelated executable.
        let mut foreign = next.clone();
        foreign.source.exe_sha256 = "f".repeat(64);
        assert!(f.store.prepare_at(foreign, f.now).is_err());
        assert!(
            f.store.prepare_at(next, f.now).is_ok(),
            "definite pre-start outcome {outcome:?}"
        );
        assert_eq!(
            f.store.read(f.request.attempt_id).unwrap().state.phase,
            UpgradePhase::ShutdownComplete
        );
    }
}

#[test]
fn storage_owner_and_request_bounds_refuse_without_creating_attempts() {
    let f = Fixture::new();
    assert!(UpgradeStore::open(&f.request.runtime_dir).is_err());
    let mut invalid = f.request.clone();
    invalid.members[0].task_ref = None;
    assert!(f.store.prepare_at(invalid, f.now).is_err());
    let mut invalid = f.request.clone();
    invalid.members[0].conversation_id = Some("x".repeat(4097));
    assert!(f.store.prepare_at(invalid, f.now).is_err());
    let mut invalid = f.request.clone();
    invalid.members.push(invalid.members[0].clone());
    assert!(f.store.prepare_at(invalid, f.now).is_err());
    let mut invalid = f.request.clone();
    invalid.personal_repo_override = Some("relative-private-repo".into());
    assert!(f.store.prepare_at(invalid, f.now).is_err());
    assert_eq!(f.store.current_attempt_id().unwrap(), None);
    assert!(!f.store.request_path(f.request.attempt_id).exists());
}

#[test]
fn failed_candidate_copy_retries_same_attempt_and_final_candidate_is_pinned() {
    let f = Fixture::new();
    fs::write(&f.request.candidate.exe_path, b"MZwrong build").unwrap();
    assert!(
        f.store
            .prepare_at(f.request.clone(), f.now)
            .unwrap_err()
            .to_string()
            .contains("hash/size")
    );
    assert_eq!(f.store.current_attempt_id().unwrap(), None);
    assert!(!f.store.request_path(f.request.attempt_id).exists());
    let partial = f
        .store
        .attempt_dir(f.request.attempt_id)
        .join("candidate/prim1.partial");
    assert!(partial.is_file());
    fs::write(
        &f.request.candidate.exe_path,
        b"MZfixture executable, never run",
    )
    .unwrap();
    let record = f.prepare();
    assert_eq!(
        fs::read(&record.request.candidate.exe_path).unwrap(),
        b"MZfixture executable, never run"
    );
    assert_eq!(
        record.request.personal_repo_override.as_deref(),
        Some("off")
    );
    assert_ne!(
        record.request.candidate.exe_path,
        f.request.candidate.exe_path
    );
    // Retry using the returned request is also idempotent and retains all IDs.
    assert_eq!(
        f.store
            .prepare_at(record.request.clone(), f.now)
            .unwrap()
            .request,
        record.request
    );
}

#[test]
fn malformed_prelaunch_records_are_preserved_without_guessing_execution() {
    for name in ["request.json", "state.json"] {
        let f = Fixture::new();
        let attempt = f.store.attempt_dir(f.request.attempt_id);
        fs::create_dir(&attempt).unwrap();
        fs::write(attempt.join(name), b"malformed retained bytes").unwrap();
        assert!(f.store.prepare_at(f.request.clone(), f.now).is_err());
        assert_eq!(
            fs::read(attempt.join(name)).unwrap(),
            b"malformed retained bytes"
        );
        assert_eq!(f.store.current_attempt_id().unwrap(), None);
        assert!(!attempt.join("helper-claim.json").exists());
    }
}

#[test]
fn restarted_owner_can_cancel_old_prepared_ready_attempt_but_not_closing() {
    let f = Fixture::new();
    let record = f.prepare();
    f.ready(&record);
    // A vanished old process cannot call ordinary source-bound cancellation.
    let mut restarted = f.request.source.clone();
    restarted.pid += 1;
    assert!(f.store.cancel(f.request.attempt_id, &restarted).is_err());
    assert_eq!(
        f.store.prepare_at(f.request.clone(), f.now).unwrap().state,
        record.state
    );
    assert!(
        f.store
            .cancel_prelaunch(f.request.attempt_id, &"0".repeat(64))
            .is_err()
    );
    let cancelled = f
        .store
        .cancel_prelaunch(f.request.attempt_id, &record.state.request_sha256)
        .unwrap();
    assert_eq!(cancelled.state.phase, UpgradePhase::Cancelled);
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                Some(&HelperIdentity {
                    pid: 456,
                    creation_filetime: "134000000000000002".into()
                }),
            )
            .is_err()
    );
    assert!(
        f.store
            .attempt_dir(f.request.attempt_id)
            .join("helper-ready.json")
            .exists()
    );
    let mut next = f.request.clone();
    next.attempt_id = Uuid::new_v4();
    next.source = restarted;
    assert!(f.store.prepare_at(next, f.now).is_ok());
    let g = Fixture::new();
    let closing = g.close();
    assert!(
        g.store
            .cancel_prelaunch(g.request.attempt_id, &closing.state.request_sha256)
            .is_err()
    );
}

#[test]
fn resume_is_owned_once_and_consumed_build_can_upgrade_from_stable_launcher() {
    let f = Fixture::new();
    let record = f.shutdown();
    let candidate = UpgradeSource {
        pid: 789,
        creation_filetime: "134000000000000003".into(),
        exe_path: record.request.candidate.exe_path.clone(),
        exe_sha256: record.request.candidate.exe_sha256.clone(),
    };
    assert!(
        f.store
            .begin_resume(f.request.attempt_id, &candidate)
            .is_err()
    );
    // Startup may beat the helper's final Launched publication.
    f.result(&record, HelperOutcome::Launching, None);
    let resuming = f
        .store
        .begin_resume(f.request.attempt_id, &candidate)
        .unwrap();
    assert_eq!(resuming.state.phase, UpgradePhase::Resuming);
    assert_eq!(
        f.store
            .begin_resume(f.request.attempt_id, &candidate)
            .unwrap()
            .state,
        resuming.state
    );
    let mut other = candidate.clone();
    other.pid += 1;
    assert!(f.store.begin_resume(f.request.attempt_id, &other).is_err());
    assert!(
        f.store
            .complete_resume(f.request.attempt_id, &other)
            .is_err()
    );
    let mut next = f.request.clone();
    next.attempt_id = Uuid::new_v4();
    next.source = candidate.clone();
    assert!(f.store.prepare_at(next.clone(), f.now).is_err());
    let consumed = f
        .store
        .complete_resume(f.request.attempt_id, &candidate)
        .unwrap();
    assert_eq!(consumed.state.phase, UpgradePhase::Consumed);
    assert_eq!(
        f.store
            .complete_resume(f.request.attempt_id, &candidate)
            .unwrap()
            .state,
        consumed.state
    );
    other.exe_path = f._dir.path().join("stable-launcher/prim1.exe");
    assert_eq!(
        f.store
            .begin_resume(f.request.attempt_id, &other)
            .unwrap()
            .state
            .phase,
        UpgradePhase::Consumed
    );
    next.source = other;
    assert!(f.store.prepare_at(next, f.now).is_ok());
    assert_eq!(
        f.store.read(f.request.attempt_id).unwrap().state,
        consumed.state
    );
}

#[test]
fn operator_abandon_retains_uncertain_bytes_without_authorizing_unknown_launch_retry() {
    for outcome in [
        None,
        Some(HelperOutcome::Launching),
        Some(HelperOutcome::Uncertain),
        Some(HelperOutcome::Failed),
    ] {
        let f = Fixture::new();
        let record = f.shutdown();
        if let Some(outcome) = outcome {
            f.result(&record, outcome, None);
        }
        let retained = f
            .store
            .attempt_dir(f.request.attempt_id)
            .join(".state.json.pending");
        fs::write(&retained, b"uncommitted old writer bytes").unwrap();
        assert!(
            f.store
                .abandon_held(f.request.attempt_id, &record.state.request_sha256, false)
                .is_err()
        );
        assert!(
            f.store
                .abandon_held(f.request.attempt_id, &"0".repeat(64), true)
                .is_err()
        );
        assert_eq!(
            f.store
                .abandon_held(f.request.attempt_id, &record.state.request_sha256, true)
                .unwrap()
                .state
                .phase,
            UpgradePhase::Abandoned
        );
        assert_eq!(
            fs::read(&retained).unwrap(),
            b"uncommitted old writer bytes"
        );
        let mut next = f.request.clone();
        next.attempt_id = Uuid::new_v4();
        assert_eq!(
            f.store.prepare_at(next, f.now).is_ok(),
            outcome == Some(HelperOutcome::Failed)
        );
    }
}

#[test]
fn closing_needs_the_verified_ready_process_and_five_minutes_remaining() {
    let f = Fixture::new();
    let record = f.prepare();
    assert!(
        f.store
            .helper_ready(f.request.attempt_id)
            .unwrap()
            .is_none()
    );
    f.ready(&record);
    let ready = f.store.helper_ready(f.request.attempt_id).unwrap().unwrap();
    let good = HelperIdentity {
        pid: ready.helper_pid,
        creation_filetime: ready.helper_creation_filetime.clone(),
    };
    let bad = HelperIdentity {
        pid: good.pid,
        creation_filetime: "134000000000000099".into(),
    };
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                None
            )
            .is_err()
    );
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.notice_until,
                Some(&bad)
            )
            .is_err()
    );
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.expires_at - Duration::seconds(CLOSE_REMAINING_SECONDS)
                    + Duration::milliseconds(1),
                Some(&good)
            )
            .is_err()
    );
    assert!(
        f.store
            .transition(
                f.request.attempt_id,
                &f.request.source,
                UpgradePhase::Closing,
                f.request.expires_at - Duration::seconds(CLOSE_REMAINING_SECONDS),
                Some(&good)
            )
            .is_ok()
    );
}

#[test]
fn unsupported_running_members_are_explicitly_interrupted_without_invented_ids() {
    let mut f = Fixture::new();
    for (driver, room) in [
        (DriverKind::GenericTerminal, Some(Uuid::new_v4())),
        (DriverKind::Prime, Some(Uuid::new_v4())),
        (DriverKind::Claude, None),
    ] {
        let mut member = f.request.members[0].clone();
        member.session_id = Uuid::new_v4();
        member.run_id = Uuid::new_v4();
        member.driver = driver;
        member.room_id = room;
        member.task_ref = None;
        f.request.members.push(member);
    }
    f.request = UpgradeRequest::new(
        f.request.source.clone(),
        f.request.candidate.clone(),
        f.request.runtime_dir.clone(),
        f.request.profile_dir.clone(),
        f.request.task_folder.clone(),
        f.request.personal_repo_override.clone(),
        f.request.requester_session_id,
        f.request.members.clone(),
    );
    f.now = f.request.prepared_at;
    let record = f.prepare();
    assert!(record.request.members[1..].iter().all(|m| m.resume_mode
        == UpgradeResumeMode::InterruptedWithoutResume
        && m.conversation_id.is_none()));
    assert_eq!(
        record.request.members[0].resume_mode,
        UpgradeResumeMode::Exact
    );
    let mut invalid = f.request.clone();
    invalid.attempt_id = Uuid::new_v4();
    invalid.requester_session_id = invalid.members[1].session_id;
    invalid.members[1].task_ref = Some(TaskRef {
        task_id: "T108".into(),
        revision: 4,
    });
    assert!(validate_request(&invalid).is_err());
}

#[test]
fn installed_v2_root_descendant_and_ancestor_are_refused_before_writes() {
    let dir = tempfile::tempdir().unwrap();
    let local = canonical_destination_without_reparse_points(dir.path()).unwrap();
    for runtime in [
        local.clone(),
        local.join("io.prim1.v2"),
        local.join("io.prim1.v2/runtime"),
    ] {
        assert!(reject_installed_v2_overlap(&runtime, &local).is_err());
    }
    assert!(reject_installed_v2_overlap(&local.join("isolated-experimental"), &local).is_ok());
    assert!(!local.join("io.prim1.v2").exists());
}
