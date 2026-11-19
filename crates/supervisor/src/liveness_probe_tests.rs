// Included in the supervisor test module. These fixtures use only mock PTYs.

struct F3ProbeControl {
    release: Arc<(Mutex<bool>, Condvar)>,
    entered: mpsc::Receiver<()>,
    calls: Arc<AtomicUsize>,
    kills: Arc<AtomicUsize>,
    dropped_off_shared_locks: Arc<AtomicBool>,
}

impl F3ProbeControl {
    fn release(&self) {
        *self.release.0.lock() = true;
        self.release.1.notify_all();
    }

    fn start(&self, supervisor: &SupervisorHandle) -> thread::JoinHandle<()> {
        let supervisor = supervisor.clone();
        let worker = thread::spawn(move || supervisor.refresh_session_liveness());
        self.entered.recv_timeout(Duration::from_secs(5)).expect("probe entered");
        worker
    }
}

impl Drop for F3ProbeControl {
    fn drop(&mut self) { self.release(); }
}

fn f3_blocking_probe(
    supervisor: &SupervisorHandle,
    result: AgentLiveness,
) -> (Box<dyn PtySessionTrait>, F3ProbeControl) {
    struct ProbePty {
        supervisor: Weak<SupervisorInner>,
        result: AgentLiveness,
        release: Arc<(Mutex<bool>, Condvar)>,
        entered: mpsc::SyncSender<()>,
        calls: Arc<AtomicUsize>,
        kills: Arc<AtomicUsize>,
        dropped_off_shared_locks: Arc<AtomicBool>,
    }
    impl PtySessionTrait for ProbePty {
        fn send_input(&self, input: &str) -> pty_host::PtyWriteResult { Ok(input.len()) }
        fn resize(&self, _: u16, _: u16) -> Result<()> { Ok(()) }
        fn kill(&self) -> Result<()> { self.kills.fetch_add(1, Ordering::SeqCst); Ok(()) }
        fn try_wait(&self) -> Result<Option<pty_host::PtyExitStatus>> { Ok(None) }
        fn process_id(&self) -> Option<u32> { None }
        fn agent_alive(&self, _: DriverKind) -> Result<AgentLiveness> {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let _ = self.entered.try_send(());
            }
            let deadline = Instant::now() + Duration::from_secs(15);
            let mut released = self.release.0.lock();
            while !*released {
                let remaining = deadline.saturating_duration_since(Instant::now());
                assert!(!remaining.is_zero(), "test did not release its native-probe barrier");
                self.release.1.wait_for(&mut released, remaining);
            }
            Ok(self.result.clone())
        }
    }
    impl Drop for ProbePty {
        fn drop(&mut self) {
            if let Some(inner) = self.supervisor.upgrade() {
                self.dropped_off_shared_locks.store(
                    inner.shutdown_lifecycle.try_write().is_some() && inner.slots.try_lock().is_some(),
                    Ordering::SeqCst,
                );
            }
        }
    }
    let (entered_tx, entered) = mpsc::sync_channel(1);
    let control = F3ProbeControl {
        release: Arc::new((Mutex::new(false), Condvar::new())),
        entered,
        calls: Arc::new(AtomicUsize::new(0)),
        kills: Arc::new(AtomicUsize::new(0)),
        dropped_off_shared_locks: Arc::new(AtomicBool::new(false)),
    };
    let pty = ProbePty {
        supervisor: Arc::downgrade(&supervisor.inner),
        result,
        release: control.release.clone(),
        entered: entered_tx,
        calls: control.calls.clone(),
        kills: control.kills.clone(),
        dropped_off_shared_locks: control.dropped_off_shared_locks.clone(),
    };
    (Box::new(pty), control)
}

fn f3_bounded<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = mpsc::sync_channel(1);
    let worker = thread::spawn(move || { let _ = tx.send(work()); });
    let value = rx.recv_timeout(Duration::from_secs(5)).expect("operation blocked behind native probe");
    worker.join().expect("operation panicked");
    value
}

#[test]
fn f3_native_probe_releases_input_output_resize_snapshot_and_stop() {
    let supervisor = test_supervisor();
    let (pty, probe) = f3_blocking_probe(&supervisor, AgentLiveness::Alive(Vec::new()));
    install_mock_running_session(&supervisor, "claude", DriverKind::Claude, pty);
    let (pty, inputs) = recording_pty_session(std::process::id());
    install_mock_running_session(&supervisor, "codex", DriverKind::GenericTerminal, pty);
    let other_id = test_session_id(&supervisor, "codex");
    let events = capture_runtime_events(&supervisor);
    let sweep = probe.start(&supervisor);

    drop(supervisor.inner.slots.try_lock().expect("probe holds no session lock"));
    drop(supervisor.inner.shutdown_lifecycle.try_write().expect("probe holds no shutdown lock"));
    let other = supervisor.clone();
    f3_bounded(move || {
        other.enqueue_operator_input(SendInputRequest { session_id: other_id, input: "queued".into() }).unwrap();
        assert!(wait_for_condition(Duration::from_secs(2), || inputs.lock().len() == 1));
        other.send_input(SendInputRequest { session_id: other_id, input: "direct".into() }).unwrap();
        assert_eq!(&*inputs.lock(), &["queued", "direct"]);
        handle_current_pty_event(&other, "codex", 0, PtyEvent::Output("F3_UNRELATED_TAIL".into()));
        other.resize_session_by_id(other_id, 91, 33).unwrap();
        let snapshot = other.snapshot();
        assert!(snapshot.sessions.iter().find(|s| s.session_id == other_id).unwrap().running);
        assert!(events.lock().iter().any(|event| matches!(event,
            RuntimeEvent::SessionOutput { chunk, .. } if chunk == "F3_UNRELATED_TAIL")));
        assert_eq!(other.stop_session_by_id(other_id).unwrap().lifecycle_state, LifecycleState::Closed);
    });
    assert_eq!(probe.calls.load(Ordering::SeqCst), 1);
    probe.release();
    sweep.join().unwrap();
}

#[test]
fn f3_native_probe_discards_replaced_identity_or_transport_and_drops_capture_off_locks() {
    for retain_identity in [false, true] {
        let supervisor = test_supervisor();
        let (pty, probe) = f3_blocking_probe(&supervisor, AgentLiveness::Exited {
            last_agent_pids: Vec::new(), live_processes: Vec::new(),
        });
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, pty);
        let old_run = current_run_id(&supervisor, "claude");
        let sweep = probe.start(&supervisor);
        let (replacement, _, replacement_kills) = mock_pty_session(None, MockKillBehavior::Immediate);
        let other = supervisor.clone();
        let new_run = f3_bounded(move || {
            if retain_identity {
                other.inner.slots.lock().get_mut("claude").unwrap().running =
                    Some(RunningSession::new(Some(Arc::from(replacement))));
            } else {
                install_mock_running_session(&other, "claude", DriverKind::Claude, replacement);
            }
            current_run_id(&other, "claude")
        });
        assert_eq!(new_run == old_run, retain_identity);
        let events = capture_runtime_events(&supervisor);
        probe.release();
        sweep.join().unwrap();
        assert_eq!(current_run_id(&supervisor, "claude"), new_run);
        assert_eq!(replacement_kills.load(Ordering::SeqCst), 0);
        assert!(session_exit_events(&events).is_empty());
        assert!(probe.dropped_off_shared_locks.load(Ordering::SeqCst), "stale captured PTY dropped under a shared guard");
    }
}

#[test]
fn f3_shutdown_finishes_during_probe_and_suppresses_its_late_result() {
    let supervisor = test_supervisor();
    let (pty, probe) = f3_blocking_probe(&supervisor, AgentLiveness::Exited {
        last_agent_pids: Vec::new(), live_processes: Vec::new(),
    });
    install_mock_running_session(&supervisor, "claude", DriverKind::Claude, pty);
    let events = capture_runtime_events(&supervisor);
    let sweep = probe.start(&supervisor);
    let other = supervisor.clone();
    f3_bounded(move || other.shutdown()).unwrap();
    let published = events.lock().len();
    assert!(supervisor.inner.shutdown_started.load(Ordering::Acquire));
    assert_eq!(probe.kills.load(Ordering::SeqCst), 1);
    probe.release();
    sweep.join().unwrap();
    assert_eq!(events.lock().len(), published, "late probe published after terminal shutdown");
    assert_eq!(probe.kills.load(Ordering::SeqCst), 1);
}

#[test]
fn f3_not_yet_observed_rejects_changed_terminal_evidence_then_rechecks_fresh() {
    for exited_before_probe in [false, true] {
        let supervisor = test_supervisor();
        let (pty, probe) = f3_blocking_probe(&supervisor, AgentLiveness::NotYetObserved(Vec::new()));
        install_mock_running_session(&supervisor, "claude", DriverKind::Claude, pty);
        let run = current_run_id(&supervisor, "claude");
        if exited_before_probe {
            let event = {
                let mut slots = supervisor.inner.slots.lock();
                let slot = slots.get_mut("claude").unwrap();
                transition_work_state_locked("claude", slot, run, WorkState::Exited, None)
            };
            if let Some(event) = event { supervisor.emit_run_event(event); }
        }
        let sweep = probe.start(&supervisor);
        let other = supervisor.clone();
        f3_bounded(move || {
            // Even when the work state stays Exited, newer output changes the
            // existing event sequence and invalidates this mixed-time inference.
            handle_current_pty_event(&other, "claude", 0, PtyEvent::Output("FRESH_EVIDENCE".into()));
            let event = {
                let mut slots = other.inner.slots.lock();
                let slot = slots.get_mut("claude").unwrap();
                cancel_quiesce_timer_locked(slot);
                transition_work_state_locked("claude", slot, run, WorkState::Exited, None)
            };
            if let Some(event) = event { other.emit_run_event(event); }
        });
        probe.release();
        sweep.join().unwrap();
        assert_eq!(current_run_id(&supervisor, "claude"), run);
        assert_eq!(probe.kills.load(Ordering::SeqCst), 0, "mixed-time inference retired the run");
        supervisor.refresh_session_liveness();
        assert_eq!(probe.kills.load(Ordering::SeqCst), 1, "a fresh unchanged observation still retires");
    }
}
