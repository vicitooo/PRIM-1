// A16 (PC3 walk, 20 Oct 13:34Z): a pane's descendant that no pane's Job
// claims is still that pane at the operator entry; a process started outside
// the panes is admitted. Mock processes and a mock parent chain.

enum A16Link {
    Process(Arc<TestPaneProcess>),
    Root,
    Gone(u32),
    Unreadable(u32),
    Indeterminate(u32),
}

/// Parent links by child pid.
struct A16Ancestry(HashMap<u32, A16Link>);

impl pane_ancestry::ProcessAncestry for A16Ancestry {
    fn parent_of(&self, child: &dyn PaneProcess) -> pane_ancestry::Parent {
        match self.0.get(&child.pid()) {
            Some(A16Link::Process(parent)) => pane_ancestry::Parent::Pinned(parent.clone()),
            Some(A16Link::Root) | None => pane_ancestry::Parent::Root,
            Some(A16Link::Gone(pid)) => pane_ancestry::Parent::Gone(*pid),
            Some(A16Link::Unreadable(pid)) => pane_ancestry::Parent::Unreadable(*pid),
            Some(A16Link::Indeterminate(pid)) => pane_ancestry::Parent::Indeterminate(*pid),
        }
    }
}

/// `pids[0]` is the caller; each next pid is the previous one's parent (in no
/// pane's Job unless listed in `in_job`, by the pane root pid it belongs to);
/// `end` follows the last.
fn a16_chain(pids: &[u32], in_job: &[(u32, u32)], end: A16Link) -> (Arc<TestPaneProcess>, A16Ancestry) {
    let process = |pid: u32| TestPaneProcess::new(pid, in_job.iter().filter(|(member, _)| *member == pid).map(|(_, root)| *root));
    let mut links = HashMap::new();
    for pair in pids.windows(2) {
        links.insert(pair[0], A16Link::Process(process(pair[1])));
    }
    links.insert(*pids.last().unwrap(), end);
    (process(pids[0]), A16Ancestry(links))
}

/// A running Claude pane whose own process (the PTY root) is `pid`.
fn a16_pane(supervisor: &SupervisorHandle, name: &str, pid: u32) -> SessionId {
    let (pty, _, _) = mock_pty_session(Some(pid), MockKillBehavior::Immediate);
    install_mock_running_session_with_process_id(supervisor, name, DriverKind::Claude, Some(pid), pty);
    test_session_id(supervisor, name)
}

fn a16_member_create(room_id: RoomId) -> SidebandRequest {
    SidebandRequest::Member { request_id: Uuid::new_v4().to_string(), action: shared_types::MemberAction::Create { codex_transport: Default::default(),
        driver: DriverKind::Claude, label: "Walk Bypass Probe".into(), permission_profile: shared_types::PermissionProfile::Normal,
        brief: false, brief_text: None, start: false, room_id: Some(room_id), grants: None } }
}

#[test]
fn a16_a_panes_descendant_outside_its_job_is_refused_at_the_operator_entry_with_or_without_grants() {
    let supervisor = test_supervisor();
    let pane = a16_pane(&supervisor, "claude", 13824);
    let room = supervisor.create_room(CreateRoomRequest { label: Some("A16 Walk B".into()), member_ids: vec![pane],
        brief_on_join: false, brief_template: None }).unwrap();
    let events = capture_runtime_events(&supervisor);
    // PC3: exe 6436 <- pwsh 15932 <- cmd 9720 <- claude 13824 (the pane) <- PRIM-1 600 <- the Schedule service.
    let (caller, ancestry) = a16_chain(&[6436, 15932, 9720, 13824, 600], &[], A16Link::Unreadable(2024));
    for grants in [MemberPermissions::default(), MemberPermissions { roam: true, maintain: true, manage: true }] {
        supervisor.set_member_permissions(pane, grants).unwrap();
        let attributed = supervisor.attribute_unclaimed(caller.as_ref(), Some(1), &ancestry);
        assert!(matches!(attributed.attribution, JobAttribution::Unknown));
        assert!(attributed.trace.contains("6436 -> 15932 -> 9720 -> 13824") && attributed.trace.contains("13824 is pane 'claude'"),
            "{}", attributed.trace);
        let sessions_before = supervisor.snapshot().sessions.len();
        for request in [SidebandRequest::MembersList {}, a16_member_create(room.room_id)] {
            let attributed = supervisor.attribute_unclaimed(caller.as_ref(), Some(1), &ancestry);
            let refused = operator_exchange(&supervisor, attributed, Some("pane test"), &request);
            assert!(!refused.ok && refused.message.contains("could not confirm"), "{refused:?}");
        }
        assert_eq!(supervisor.snapshot().sessions.len(), sessions_before, "no member was created");
    }
    // The pane's own process, unclaimed by its Job, is that pane (the
    // Reviewer's c4cfaa6 hold): its chain holds only PRIM-1 and the system.
    let (root, ancestry) = a16_chain(&[13824, 600], &[], A16Link::Root);
    let attributed = supervisor.attribute_unclaimed(root.as_ref(), Some(1), &ancestry);
    assert!(matches!(attributed.attribution, JobAttribution::Unknown), "{}", attributed.trace);
    assert!(attributed.trace.contains("13824 is pane 'claude'"), "{}", attributed.trace);
    let refused = operator_exchange(&supervisor, attributed, None, &SidebandRequest::MembersList {});
    assert!(!refused.ok, "{refused:?}");
    // An ancestor in the pane's Job (not its root) decides it as well.
    let (caller, ancestry) = a16_chain(&[7000, 7001, 600], &[(7001, 13824)], A16Link::Root);
    assert!(matches!(supervisor.attribute_unclaimed(caller.as_ref(), Some(1), &ancestry).attribution, JobAttribution::Unknown));
    // Every decision is in the audit log, with the deciding ancestor.
    let audited: Vec<String> = events.lock().iter().filter_map(|event| match event {
        RuntimeEvent::SystemLog { message, .. } if message.starts_with("operator entry") => Some(message.clone()),
        _ => None,
    }).collect();
    assert_eq!(audited.len(), 5);
    assert!(audited[..4].iter().all(|line| line.starts_with("operator entry refused: process 6436 (session 1")
        && line.contains("13824 is pane 'claude'")), "{audited:?}");
    assert!(audited[4].starts_with("operator entry refused: process 13824 (session 1"), "{audited:?}");
    supervisor.shutdown().unwrap();
}

#[test]
fn a16_a_process_started_outside_the_panes_is_admitted_and_its_walk_end_is_named() {
    let supervisor = test_supervisor();
    a16_pane(&supervisor, "claude", 13824);
    let events = capture_runtime_events(&supervisor);
    // The SSH door: exe <- pwsh <- sshd, which this user may not query.
    let (caller, ancestry) = a16_chain(&[9084, 4400], &[], A16Link::Unreadable(1732));
    let attributed = supervisor.attribute_unclaimed(caller.as_ref(), Some(0), &ancestry);
    assert!(matches!(attributed.attribution, JobAttribution::Outside(9084)));
    assert!(attributed.trace.contains("ends at process 1732, which this user may not query without reaching a pane"),
        "{}", attributed.trace);
    let admitted = operator_exchange(&supervisor, attributed, Some("MAINPC Runtime Operator over SSH"), &SidebandRequest::MembersList {});
    assert!(admitted.ok, "{admitted:?}");
    assert!(events.lock().iter().any(|event| matches!(event,
        RuntimeEvent::SystemLog { message, .. } if message.starts_with("operator entry admitted: process 9084 (session 0"))));

    // The accepted same-user limit: a pane-launched process whose parent has
    // exited cannot be traced to the pane, and reads as outside.
    let (orphan, ancestry) = a16_chain(&[368], &[], A16Link::Gone(1092));
    let attributed = supervisor.attribute_unclaimed(orphan.as_ref(), Some(1), &ancestry);
    assert!(matches!(attributed.attribution, JobAttribution::Outside(368)));
    assert!(attributed.trace.contains("ends at parent 1092, which has exited"), "{}", attributed.trace);

    // A parent that could not be established proves nothing: refused, unlike
    // the known ends above (the Reviewer's c4cfaa6 hold).
    let (unproven, ancestry) = a16_chain(&[5500], &[], A16Link::Indeterminate(5500));
    let attributed = supervisor.attribute_unclaimed(unproven.as_ref(), Some(1), &ancestry);
    assert!(matches!(attributed.attribution, JobAttribution::Unknown), "{}", attributed.trace);
    assert!(attributed.trace.contains("a link that could not be verified"), "{}", attributed.trace);

    // A chain longer than any real one is not believed.
    let long: Vec<u32> = (20_000..20_040).collect();
    let (deep, ancestry) = a16_chain(&long, &[], A16Link::Root);
    let attributed = supervisor.attribute_unclaimed(deep.as_ref(), Some(1), &ancestry);
    assert!(matches!(attributed.attribution, JobAttribution::Unknown));
    assert!(attributed.trace.contains("was not believed"), "{}", attributed.trace);
    supervisor.shutdown().unwrap();
}

#[cfg(windows)]
#[test]
fn a16_an_unpinned_parent_is_a_known_end_only_when_that_is_known() {
    use pane_ancestry::{Parent, unpinned_parent};
    // Known ends: no such process, or found already exited; access denied.
    assert!(matches!(unpinned_parent(1092, Some(87), None), Parent::Gone(1092)));
    let exited = anyhow::Error::new(pty_host::ProcessAlreadyExited(1092));
    assert!(matches!(unpinned_parent(1092, None, Some(&exited)), Parent::Gone(1092)));
    assert!(matches!(unpinned_parent(1732, Some(5), None), Parent::Unreadable(1732)));
    // Faulted queries establish nothing (the Reviewer's ff7a97e hold): an
    // other open error, a failed start-time read, a failed liveness query.
    assert!(matches!(unpinned_parent(1092, Some(1450), None), Parent::Indeterminate(1092)));
    let times = Err::<(), _>(std::io::Error::from_raw_os_error(5))
        .context("failed to read creation time for process 1092").unwrap_err();
    assert!(matches!(unpinned_parent(1092, None, Some(&times)), Parent::Indeterminate(1092)),
        "access denied on the start time is not an unreadable parent");
    let liveness = Err::<(), _>(std::io::Error::from_raw_os_error(6))
        .context("failed to query liveness for process 1092").unwrap_err();
    assert!(matches!(unpinned_parent(1092, None, Some(&liveness)), Parent::Indeterminate(1092)));
}
