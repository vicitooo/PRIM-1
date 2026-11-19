//! Display-output ownership from the PTY reader through the xterm parser.
//! Only `reserve`/`renderer_ready` wait. Neither may run under supervisor,
//! publisher, ingress, input, or lifecycle locks. ACK takes this mutex alone.
use std::{collections::HashMap, sync::Arc, time::{Duration, Instant}};
use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use shared_types::{RunEventIdentity, SessionGeneration, SessionId};
use uuid::Uuid;

pub const RUN_BYTES: usize = 64 * 1024;
pub const TOTAL_BYTES: usize = 1024 * 1024;
pub const RUN_ITEMS: usize = 64;
pub const TOTAL_ITEMS: usize = 256;
// One decoded 4096-byte PTY read, including UTF-8 replacement/carry expansion.
pub const MAX_CHUNK_BYTES: usize = 16 * 1024;
// One carried ESC + CAN/SGR reset + seven supported mode replays (<=8 bytes
// each) need <=62 bytes. Charge 64 on every reservation, including ordinary
// output, so the recovery prefix never exceeds the byte ledger.
pub const DISPLAY_OVERHEAD: usize = 64;
// Initial liveness policy, not measured healthy-renderer latency targets.
pub const NO_PROGRESS_WAIT: Duration = Duration::from_secs(2);
pub const MAX_ADMISSION_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OutputRun {
    pub session_id: SessionId,
    pub run_id: Uuid,
    pub generation: SessionGeneration,
}
impl From<RunEventIdentity> for OutputRun {
    fn from(id: RunEventIdentity) -> Self {
        Self { session_id: id.session_id, run_id: id.run_id, generation: id.generation }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputCredit {
    pub epoch: Uuid,
    pub run: OutputRun,
    pub tickets: Vec<Uuid>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct DiscardedOutput {
    pub bytes: usize,
    pub items: usize,
}
#[derive(Debug, Serialize)]
pub struct RendererReset {
    pub epoch: Uuid,
    pub display_reset: bool,
    pub discarded: DiscardedOutput,
}

const ACK_LATENCY_BOUNDS_MS: [u64; 7] = [16, 50, 100, 250, 1000, 2000, 5000];

/// Aggregate native-dispatch to validated parser-ACK timing, not paint timing.
/// Contains no payload, run identity or per-ticket history. Bucket upper bounds
/// are inclusive; the final bucket is >5000 ms. Times round up to milliseconds.
#[derive(Debug, Serialize)]
pub struct OutputCreditMetrics {
    pub epoch: Option<Uuid>,
    pub parser_ack_tickets: u64,
    pub max_ms: u64,
    pub latency_bucket_upper_bounds_ms: [u64; 7],
    pub latency_histogram: [u64; 8],
    pub over_250_ms: u64,
    pub over_2000_ms: u64,
    pub over_5000_ms: u64,
    pub observer_entries: u64,
    pub observer_max_quiet_ms: u64,
    pub current_observing_runs: usize,
    pub current_bytes: usize,
    pub peak_bytes: usize,
    pub current_items: usize,
    pub peak_items: usize,
}

#[derive(Default)]
struct EpochMetrics {
    started_at: Option<Instant>,
    parser_ack_tickets: u64,
    max_ms: u64,
    latency_histogram: [u64; 8],
    observer_entries: u64,
    observer_max_quiet_ms: u64,
    peak_bytes: usize,
    peak_items: usize,
}
impl EpochMetrics {
    fn parsed(&mut self, elapsed: Duration) {
        let millis = elapsed_millis_ceil(elapsed);
        let bucket = ACK_LATENCY_BOUNDS_MS.iter().position(|upper| millis <= *upper).unwrap_or(7);
        self.parser_ack_tickets = self.parser_ack_tickets.saturating_add(1);
        self.max_ms = self.max_ms.max(millis);
        self.latency_histogram[bucket] = self.latency_histogram[bucket].saturating_add(1);
    }
}

fn elapsed_millis_ceil(elapsed: Duration) -> u64 {
    (elapsed.as_nanos().saturating_add(999_999) / 1_000_000).min(u64::MAX as u128) as u64
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Stage { Reserved, Published(u64), Queued, Dispatched }
struct Charge { epoch: Uuid, run: OutputRun, bytes: usize, stage: Stage, dispatched_at: Option<Instant> }
#[derive(Default)]
struct RunState {
    bytes: usize,
    items: usize,
    cancelled: bool,
    reader_closed: bool,
    process_ended: bool,
    observing: bool,
}
#[derive(Default)]
struct State {
    enabled: bool,
    stopped: bool,
    ready: bool,
    epoch: Option<Uuid>,
    bytes: usize,
    runs: HashMap<OutputRun, RunState>,
    charges: HashMap<Uuid, Charge>,
    published: HashMap<(OutputRun, u64), Uuid>,
    progress: Option<(Uuid, Instant)>,
    metrics: EpochMetrics,
    #[cfg(test)]
    waiting: usize,
}
#[derive(Default)]
pub struct OutputCreditLedger { state: Mutex<State>, changed: Condvar }

pub enum Admission { Disabled, Cancelled, Oversized, ObserverOnly, Reserved(OutputReservation) }
pub struct OutputReservation { ledger: Arc<OutputCreditLedger>, ticket: Option<Uuid> }
impl Drop for OutputReservation {
    fn drop(&mut self) {
        if let Some(ticket) = self.ticket.take() { self.ledger.release(&[ticket]); }
    }
}
impl OutputReservation {
    /// A reset can race sequence allocation. Publish the old-epoch ticket too:
    /// the bridge disposes it in order, without leaving a sequence hole.
    pub fn publish(mut self, identity: RunEventIdentity) {
        let ticket = self.ticket.take().expect("reservation consumed once");
        let mut state = self.ledger.state.lock();
        let charge = state.charges.get_mut(&ticket).expect("live reservation");
        assert_eq!(charge.run, OutputRun::from(identity));
        charge.stage = Stage::Published(identity.sequence);
        state.published.insert((identity.into(), identity.sequence), ticket);
    }
}

impl OutputCreditLedger {
    pub fn enable(&self) { self.state.lock().enabled = true; }
    pub fn open_run(&self, run: OutputRun) {
        let mut state = self.state.lock();
        if state.enabled && !state.stopped { state.runs.entry(run).or_default(); }
    }
    pub fn reserve(self: &Arc<Self>, run: OutputRun, bytes: usize) -> Admission {
        self.reserve_with_limits(run, bytes, NO_PROGRESS_WAIT, MAX_ADMISSION_WAIT)
    }
    fn reserve_with_limits(self: &Arc<Self>, run: OutputRun, bytes: usize, idle_limit: Duration, hard_limit: Duration) -> Admission {
        let started = Instant::now();
        let hard_deadline = started + hard_limit;
        let mut state = self.state.lock();
        if !state.enabled { return Admission::Disabled; }
        if state.stopped || state.runs.get(&run).is_none_or(|r| r.cancelled) {
            return Admission::Cancelled;
        }
        // The caller holds this run's display admission with a named error.
        if bytes > MAX_CHUNK_BYTES { return Admission::Oversized; }
        let bytes = bytes + DISPLAY_OVERHEAD;
        loop {
            if state.stopped || state.runs.get(&run).is_none_or(|r| r.cancelled) {
                return Admission::Cancelled;
            }
            let usage = &state.runs[&run];
            if state.ready && state.bytes + bytes <= TOTAL_BYTES
                && state.charges.len() < TOTAL_ITEMS
                && usage.bytes + bytes <= RUN_BYTES && usage.items < RUN_ITEMS
            {
                let ticket = Uuid::new_v4();
                let epoch = state.epoch.expect("ready renderer has an epoch");
                state.bytes += bytes;
                let usage = state.runs.get_mut(&run).unwrap();
                usage.observing = false;
                usage.bytes += bytes;
                usage.items += 1;
                state.charges.insert(ticket, Charge { epoch, run, bytes, stage: Stage::Reserved, dispatched_at: None });
                state.metrics.peak_bytes = state.metrics.peak_bytes.max(state.bytes);
                state.metrics.peak_items = state.metrics.peak_items.max(state.charges.len());
                return Admission::Reserved(OutputReservation { ledger: self.clone(), ticket: Some(ticket) });
            }
            // Only exact current-epoch ACK/discard progress refreshes liveness.
            // Another run can show a busy renderer is alive, but never extend
            // this blocked reader beyond the hard ceiling.
            let progress_stamp = state.progress.filter(|(epoch, _)| Some(*epoch) == state.epoch).map(|(_, at)| at);
            let progress_at = progress_stamp.map(|at| at.max(started)).unwrap_or(started);
            let deadline = hard_deadline.min(progress_at + idle_limit);
            let remaining = deadline.saturating_duration_since(Instant::now());
            if usage.observing || remaining.is_zero() {
                if !usage.observing {
                    let quiet_since = progress_stamp.or(state.metrics.started_at).unwrap_or(started);
                    let quiet_ms = elapsed_millis_ceil(Instant::now().saturating_duration_since(quiet_since));
                    state.metrics.observer_entries = state.metrics.observer_entries.saturating_add(1);
                    state.metrics.observer_max_quiet_ms = state.metrics.observer_max_quiet_ms.max(quiet_ms);
                }
                state.runs.get_mut(&run).unwrap().observing = true;
                return Admission::ObserverOnly;
            }
            #[cfg(test)] { state.waiting += 1; self.changed.notify_all(); }
            self.changed.wait_for(&mut state, remaining);
            #[cfg(test)] { state.waiting -= 1; }
        }
    }
    /// Conservative recovery signal for a quiet run's coalesced display gap.
    /// A resumed smaller chunk can also carry its gap before this becomes true.
    pub fn can_report_display_gap(&self, run: OutputRun) -> bool {
        let state = self.state.lock();
        state.ready && state.bytes + MAX_CHUNK_BYTES + DISPLAY_OVERHEAD <= TOTAL_BYTES
            && state.charges.len() < TOTAL_ITEMS
            && state.runs.get(&run).is_none_or(|usage|
                usage.bytes + MAX_CHUNK_BYTES + DISPLAY_OVERHEAD <= RUN_BYTES && usage.items < RUN_ITEMS)
    }
    pub fn take_published(&self, identity: RunEventIdentity) -> Option<OutputCredit> {
        let mut state = self.state.lock();
        let ticket = state.published.remove(&(identity.into(), identity.sequence))?;
        let charge = state.charges.get_mut(&ticket)?;
        charge.stage = Stage::Queued;
        Some(OutputCredit { epoch: charge.epoch, run: charge.run, tickets: vec![ticket] })
    }
    pub fn discard_published(&self, identity: RunEventIdentity) {
        if let Some(credit) = self.take_published(identity) { self.discard_native(&credit); }
    }
    pub fn is_current(&self, credit: &OutputCredit) -> bool {
        self.state.lock().epoch == Some(credit.epoch)
    }
    pub fn metrics(&self) -> OutputCreditMetrics {
        let state = self.state.lock();
        let m = &state.metrics;
        let sum_from = |start: usize| m.latency_histogram[start..].iter().fold(0_u64, |sum, count| sum.saturating_add(*count));
        OutputCreditMetrics {
            epoch: state.epoch, parser_ack_tickets: m.parser_ack_tickets, max_ms: m.max_ms,
            latency_bucket_upper_bounds_ms: ACK_LATENCY_BOUNDS_MS, latency_histogram: m.latency_histogram,
            over_250_ms: sum_from(4), over_2000_ms: sum_from(6), over_5000_ms: sum_from(7),
            observer_entries: m.observer_entries, current_bytes: state.bytes, peak_bytes: m.peak_bytes,
            observer_max_quiet_ms: m.observer_max_quiet_ms,
            current_observing_runs: state.runs.values().filter(|run| run.observing && !run.cancelled && !run.reader_closed).count(),
            current_items: state.charges.len(), peak_items: m.peak_items,
        }
    }
    /// Returns false for reset-epoch output. The pump disposes it explicitly,
    /// never waits for credit and never emits it into the new parser stream.
    pub fn dispatch(&self, credit: &OutputCredit) -> bool {
        let mut state = self.state.lock();
        if state.epoch != Some(credit.epoch) { return false; }
        if credit.tickets.iter().any(|id| state.charges.get(id).is_none_or(|c|
            c.epoch != credit.epoch || c.run != credit.run || c.stage != Stage::Queued)) { return false; }
        let now = Instant::now();
        for ticket in &credit.tickets {
            let charge = state.charges.get_mut(ticket).unwrap();
            charge.stage = Stage::Dispatched;
            charge.dispatched_at = Some(now);
        }
        true
    }
    pub fn acknowledge(&self, credit: &OutputCredit) -> Result<usize, &'static str> {
        if credit.tickets.len() > TOTAL_ITEMS { return Err("too many output tickets"); }
        let mut state = self.state.lock();
        if state.epoch != Some(credit.epoch) { return Ok(0); }
        // Validate the whole request first: a wrong-run or guessed early ACK
        // must not partially release an otherwise matching batch.
        if credit.tickets.iter().filter_map(|id| state.charges.get(id)).any(|c|
            c.epoch != credit.epoch || c.run != credit.run || c.stage != Stage::Dispatched)
        { return Err("output ticket does not match this dispatched run"); }
        let now = Instant::now();
        for ticket in &credit.tickets {
            // Taking the timestamp counts repeated IDs in one batch once.
            if let Some(dispatched) = state.charges.get_mut(ticket).and_then(|charge| charge.dispatched_at.take()) {
                state.metrics.parsed(now.saturating_duration_since(dispatched));
            }
        }
        let released = release_locked(&mut state, &credit.tickets);
        if released.items > 0 { state.progress = Some((credit.epoch, now)); }
        self.changed.notify_all();
        Ok(released.items)
    }
    /// Non-parser dispositions (filtered empty, explicit display reset/drop)
    /// are deliberately separate from acknowledge and never counted as parsed.
    /// This one is the renderer's own disposal (its `discard_output`), so a
    /// current-epoch release is evidence the page is alive.
    pub fn discard(&self, credit: &OutputCredit) -> DiscardedOutput {
        self.discard_with(credit, true)
    }
    /// The desktop's own disposals (a sanitizer-emptied chunk, a stale epoch,
    /// a closed queue, a failed dispatch or emit) release the same tickets but
    /// prove nothing about the renderer, so they never refresh liveness.
    pub fn discard_native(&self, credit: &OutputCredit) -> DiscardedOutput {
        self.discard_with(credit, false)
    }
    fn discard_with(&self, credit: &OutputCredit, renderer_progress: bool) -> DiscardedOutput {
        let mut state = self.state.lock();
        let valid = credit.tickets.iter().copied().filter(|id| state.charges.get(id)
            .is_some_and(|c| c.epoch == credit.epoch && c.run == credit.run)).collect::<Vec<_>>();
        let released = release_locked(&mut state, &valid);
        if renderer_progress && released.items > 0 && state.epoch == Some(credit.epoch) {
            state.progress = Some((credit.epoch, Instant::now()));
        }
        self.changed.notify_all();
        released
    }
    fn release(&self, tickets: &[Uuid]) {
        release_locked(&mut self.state.lock(), tickets);
        self.changed.notify_all();
    }
    pub fn begin_renderer(&self) -> RendererReset {
        let mut state = self.state.lock();
        let display_reset = state.epoch.is_some();
        state.ready = false;
        let epoch = Uuid::new_v4();
        state.epoch = Some(epoch);
        state.progress = None;
        // The new page explicitly disposes the lost previous parser. Native
        // reserved/published/queued payloads stay charged until pump disposal.
        let dispatched = state.charges.iter().filter_map(|(id, c)|
            (c.stage == Stage::Dispatched).then_some(*id)).collect::<Vec<_>>();
        let discarded = release_locked(&mut state, &dispatched);
        // Native old-epoch payloads remain charged; include that baseline in
        // this epoch's peaks without attributing old ACKs to the new parser.
        state.metrics = EpochMetrics { started_at: Some(Instant::now()), peak_bytes: state.bytes,
            peak_items: state.charges.len(), ..EpochMetrics::default() };
        self.changed.notify_all();
        RendererReset { epoch, display_reset, discarded }
    }
    pub fn renderer_ready(&self, epoch: Uuid, timeout: Duration) -> Result<(), &'static str> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        loop {
            if state.stopped || state.epoch != Some(epoch) { return Err("renderer epoch was superseded"); }
            if state.charges.values().all(|charge| charge.epoch == epoch) {
                state.ready = true;
                self.changed.notify_all();
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err("previous native display output has not drained; display is paused while native observation continues; retry renderer readiness"); }
            self.changed.wait_for(&mut state, remaining);
        }
    }
    /// Cancellation wakes producers but retains already admitted charges until
    /// parsing or a named disposal. Releasing them here would overfill ingress.
    pub fn cancel_session(&self, session_id: SessionId) {
        let mut state = self.state.lock();
        for (key, run) in &mut state.runs { if key.session_id == session_id { run.cancelled = true; } }
        state.runs.retain(|_, run| !run.cancelled || run.items > 0);
        self.changed.notify_all();
    }
    pub fn cancel_all(&self) {
        let mut state = self.state.lock();
        state.stopped = true;
        self.changed.notify_all();
    }
    pub fn reader_closed(&self, run: OutputRun) {
        if let Some(state) = self.state.lock().runs.get_mut(&run) { state.reader_closed = true; }
    }
    /// Natural process exit is not reader EOF. True only on the first observed
    /// transition; subsequent polls neither repeat status nor retire the run.
    pub fn mark_process_ended(&self, run: OutputRun) -> Option<bool> {
        let mut state = self.state.lock();
        let run = state.runs.get_mut(&run)?;
        if run.reader_closed { return None; }
        let first = !run.process_ended;
        run.process_ended = true;
        Some(first)
    }
    pub fn retire_run(&self, run: OutputRun) {
        let mut state = self.state.lock();
        if let Some(usage) = state.runs.get_mut(&run) {
            usage.cancelled = true;
            if usage.items == 0 { state.runs.remove(&run); }
        }
        self.changed.notify_all();
    }
    #[cfg(test)]
    pub(crate) fn wait_for_blocked_producer(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock();
        while state.waiting == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() { return false; }
            self.changed.wait_for(&mut state, left);
        }
        true
    }
}

fn release_locked(state: &mut State, tickets: &[Uuid]) -> DiscardedOutput {
    let mut released = DiscardedOutput::default();
    for ticket in tickets {
        let Some(charge) = state.charges.remove(ticket) else { continue; };
        if let Stage::Published(sequence) = charge.stage { state.published.remove(&(charge.run, sequence)); }
        state.bytes -= charge.bytes;
        released.bytes += charge.bytes;
        released.items += 1;
        if let Some(run) = state.runs.get_mut(&charge.run) {
            run.bytes -= charge.bytes;
            run.items -= 1;
            if run.items == 0 && run.cancelled { state.runs.remove(&charge.run); }
        }
    }
    released
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread};

    fn ready() -> Arc<OutputCreditLedger> {
        let ledger = Arc::new(OutputCreditLedger::default());
        ledger.enable();
        let reset = ledger.begin_renderer();
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        ledger
    }
    fn run(ledger: &OutputCreditLedger) -> OutputRun {
        let run = OutputRun { session_id: Uuid::new_v4(), run_id: Uuid::new_v4(), generation: 7 };
        ledger.open_run(run);
        run
    }
    fn reserve(ledger: &Arc<OutputCreditLedger>, run: OutputRun, bytes: usize) -> OutputReservation {
        match ledger.reserve(run, bytes) { Admission::Reserved(lease) => lease, _ => panic!("expected reservation") }
    }
    fn queued(ledger: &Arc<OutputCreditLedger>, run: OutputRun, sequence: u64) -> OutputCredit {
        let identity = RunEventIdentity { session_id: run.session_id, run_id: run.run_id, generation: run.generation, sequence };
        reserve(ledger, run, 10).publish(identity);
        ledger.take_published(identity).unwrap()
    }
    fn wait_for_waiter(ledger: &OutputCreditLedger) {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut state = ledger.state.lock();
        while state.waiting == 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "producer did not reach credit wait");
            ledger.changed.wait_for(&mut state, left);
        }
    }

    #[test]
    fn per_run_byte_limit_waits_and_stop_wakes_without_releasing_admitted_bytes() {
        let ledger = ready();
        let run = run(&ledger);
        let leases = (0..4).map(|_| reserve(&ledger, run, MAX_CHUNK_BYTES - DISPLAY_OVERHEAD)).collect::<Vec<_>>();
        let (tx, rx) = mpsc::channel();
        let producer_ledger = ledger.clone();
        let producer = thread::spawn(move || tx.send(matches!(producer_ledger.reserve(run, 1), Admission::Cancelled)).unwrap());
        wait_for_waiter(&ledger);
        assert_eq!(ledger.state.lock().bytes, RUN_BYTES);
        ledger.cancel_session(run.session_id);
        assert!(rx.recv_timeout(Duration::from_secs(2)).unwrap());
        producer.join().unwrap();
        assert_eq!(ledger.state.lock().bytes, RUN_BYTES, "stop cannot recycle queue capacity prematurely");
        drop(leases);
        assert_eq!(ledger.state.lock().bytes, 0);
        assert!(ledger.state.lock().runs.is_empty());
    }

    #[test]
    fn aggregate_items_bound_even_tiny_output_and_shutdown_unblocks() {
        let ledger = ready();
        let mut leases = Vec::new();
        for _ in 0..4 {
            let run = run(&ledger);
            for _ in 0..RUN_ITEMS { leases.push(reserve(&ledger, run, 1)); }
        }
        let extra = run(&ledger);
        let l = ledger.clone();
        let producer = thread::spawn(move || assert!(matches!(l.reserve(extra, 1), Admission::Cancelled)));
        wait_for_waiter(&ledger);
        assert_eq!(ledger.state.lock().charges.len(), TOTAL_ITEMS);
        ledger.cancel_all();
        producer.join().unwrap();
        assert_eq!(ledger.state.lock().charges.len(), TOTAL_ITEMS);
        drop(leases);
        assert!(ledger.state.lock().charges.is_empty());
    }

    #[test]
    fn aggregate_bytes_are_shared_across_runs() {
        let ledger = ready();
        let mut leases = Vec::new();
        for _ in 0..16 {
            let run = run(&ledger);
            for _ in 0..4 { leases.push(reserve(&ledger, run, MAX_CHUNK_BYTES - DISPLAY_OVERHEAD)); }
        }
        let extra = run(&ledger);
        let l = ledger.clone();
        let producer = thread::spawn(move || reserve(&l, extra, 1));
        wait_for_waiter(&ledger);
        assert_eq!(ledger.state.lock().bytes, TOTAL_BYTES);
        drop(leases.pop());
        let admitted = producer.join().unwrap();
        assert!(ledger.state.lock().bytes <= TOTAL_BYTES);
        drop(admitted);
    }

    #[test]
    fn parser_ack_requires_dispatched_exact_epoch_run_and_releases_once() {
        let ledger = ready();
        let run = run(&ledger);
        let credit = queued(&ledger, run, 1);
        assert!(ledger.acknowledge(&credit).is_err(), "enqueue is not parsing");
        assert!(ledger.dispatch(&credit));
        let mut wrong = credit.clone();
        wrong.run.run_id = Uuid::new_v4();
        assert!(ledger.acknowledge(&wrong).is_err());
        wrong = credit.clone(); wrong.run.generation += 1;
        assert!(ledger.acknowledge(&wrong).is_err());
        wrong = credit.clone(); wrong.epoch = Uuid::new_v4();
        assert_eq!(ledger.acknowledge(&wrong).unwrap(), 0);
        assert_eq!(ledger.state.lock().bytes, 10 + DISPLAY_OVERHEAD);
        let mut duplicate = credit.clone(); duplicate.tickets.extend(credit.tickets.clone());
        assert_eq!(ledger.acknowledge(&duplicate).unwrap(), 1);
        assert_eq!(ledger.acknowledge(&credit).unwrap(), 0);
    }

    #[test]
    fn reload_fences_old_ack_and_waits_for_native_disposal_before_new_admission() {
        let ledger = ready();
        let run = run(&ledger);
        let dispatched = queued(&ledger, run, 1);
        assert!(ledger.dispatch(&dispatched));
        let native = queued(&ledger, run, 2);
        let reset = ledger.begin_renderer();
        assert!(reset.display_reset);
        assert_eq!(reset.discarded.items, 1);
        assert!(ledger.renderer_ready(reset.epoch, Duration::ZERO).is_err());
        assert!(!ledger.dispatch(&native));
        assert_eq!(ledger.discard_native(&native).items, 1);
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        let fresh = queued(&ledger, run, 3);
        assert!(ledger.dispatch(&fresh));
        assert_eq!(ledger.acknowledge(&dispatched).unwrap(), 0);
        assert_eq!(ledger.state.lock().bytes, 10 + DISPLAY_OVERHEAD);
        assert_eq!(ledger.acknowledge(&fresh).unwrap(), 1);
    }

    #[test]
    fn process_exit_is_not_eof_and_tail_tickets_outlive_retirement() {
        let ledger = ready();
        let run = run(&ledger);
        assert_eq!(ledger.mark_process_ended(run), Some(true));
        assert_eq!(ledger.mark_process_ended(run), Some(false));
        let tail = queued(&ledger, run, 3);
        ledger.reader_closed(run);
        assert_eq!(ledger.mark_process_ended(run), None);
        ledger.retire_run(run);
        assert!(ledger.dispatch(&tail));
        assert_eq!(ledger.acknowledge(&tail).unwrap(), 1);
        assert!(ledger.state.lock().runs.is_empty());
    }

    #[test]
    fn rejected_spawn_and_stale_oversized_output_cannot_cancel_a_successor() {
        let ledger = ready();
        let old = run(&ledger);
        ledger.retire_run(old); // failed/rejected spawn without any output
        assert!(ledger.state.lock().runs.is_empty());
        let next = OutputRun { run_id: Uuid::new_v4(), generation: old.generation + 1, ..old };
        ledger.open_run(next);
        assert!(matches!(ledger.reserve(old, MAX_CHUNK_BYTES + 1), Admission::Cancelled));
        assert!(matches!(ledger.reserve(next, MAX_CHUNK_BYTES + 1), Admission::Oversized));
        let owned = reserve(&ledger, next, 1);
        ledger.open_run(next); // idempotent, never erase outstanding accounting
        assert_eq!(ledger.state.lock().bytes, 1 + DISPLAY_OVERHEAD);
        drop(owned);
        ledger.retire_run(next);
        assert!(ledger.state.lock().runs.is_empty());
    }

    #[test]
    fn missing_parser_ack_expires_once_keeps_tickets_and_recovers_on_exact_ack() {
        let ledger = ready();
        let run = run(&ledger);
        let credits = (0..RUN_ITEMS).map(|sequence| {
            let credit = queued(&ledger, run, sequence as u64);
            assert!(ledger.dispatch(&credit));
            credit
        }).collect::<Vec<_>>();
        let charged = ledger.state.lock().bytes;
        let worker = ledger.clone();
        let (tx, rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            tx.send(matches!(worker.reserve(run, 1), Admission::ObserverOnly)).unwrap();
            // Once observing, even a far-future deadline must not wait again.
            tx.send(matches!(worker.reserve_with_limits(run, 1, Duration::from_secs(60), Duration::from_secs(60)),
                Admission::ObserverOnly)).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_secs(4)).unwrap(), "missing ACK cannot freeze producer");
        assert!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), "observer mode cannot wait per read");
        producer.join().unwrap();
        assert_eq!(ledger.state.lock().bytes, charged);
        assert_eq!(ledger.state.lock().charges.len(), RUN_ITEMS);
        assert!(!ledger.can_report_display_gap(run));
        let mut wrong = credits[0].clone();
        wrong.run.generation += 1;
        assert!(ledger.acknowledge(&wrong).is_err());
        assert!(matches!(ledger.reserve(run, 1), Admission::ObserverOnly));
        assert_eq!(ledger.acknowledge(&credits[0]).unwrap(), 1);
        let resumed = reserve(&ledger, run, 1);
        assert!(!ledger.state.lock().runs[&run].observing);
        assert_eq!(ledger.state.lock().charges.len(), RUN_ITEMS);
        ledger.cancel_session(run.session_id);
        assert!(matches!(ledger.reserve(run, 1), Admission::Cancelled));
        drop(resumed);
        for credit in credits { ledger.discard(&credit); }
        assert!(ledger.state.lock().runs.is_empty());
    }

    #[test]
    fn unavailable_renderer_observes_then_same_epoch_readiness_reopens_admission() {
        let ledger = Arc::new(OutputCreditLedger::default());
        ledger.enable();
        let run = run(&ledger);
        let reset = ledger.begin_renderer();
        assert!(matches!(ledger.reserve_with_limits(run, 10, Duration::ZERO, Duration::ZERO), Admission::ObserverOnly));
        assert!(ledger.state.lock().charges.is_empty());
        assert!(!ledger.can_report_display_gap(run));
        assert!(matches!(ledger.reserve(run, 10), Admission::ObserverOnly));
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        assert!(ledger.can_report_display_gap(run));
        let resumed = reserve(&ledger, run, 10);
        assert_eq!(ledger.state.lock().bytes, 10 + DISPLAY_OVERHEAD);
        drop(resumed);
    }

    #[test]
    fn acknowledgements_every_300ms_preserve_backpressure_without_display_loss() {
        let ledger = ready();
        let run = run(&ledger);
        let credits = (0..RUN_ITEMS).map(|sequence| {
            let identity = RunEventIdentity { session_id: run.session_id, run_id: run.run_id,
                generation: run.generation, sequence: sequence as u64 };
            reserve(&ledger, run, RUN_BYTES / RUN_ITEMS - DISPLAY_OVERHEAD).publish(identity);
            let credit = ledger.take_published(identity).unwrap();
            assert!(ledger.dispatch(&credit));
            credit
        }).collect::<Vec<_>>();
        let worker = ledger.clone();
        let (tx, rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            // Eight small ACKs are needed: the total wait exceeds the idle
            // limit, while genuine parser progress arrives every 300 ms.
            tx.send(worker.reserve(run, 8 * (RUN_BYTES / RUN_ITEMS) - DISPLAY_OVERHEAD)).unwrap();
        });
        wait_for_waiter(&ledger);
        for credit in credits.iter().take(8) {
            assert!(matches!(rx.recv_timeout(Duration::from_millis(300)), Err(mpsc::RecvTimeoutError::Timeout)),
                "healthy slow ACKs must not trigger an observer/display-loss result");
            assert_eq!(ledger.acknowledge(credit).unwrap(), 1);
        }
        let Admission::Reserved(resumed) = rx.recv_timeout(Duration::from_secs(2)).unwrap()
            else { panic!("progressing renderer lost output"); };
        producer.join().unwrap();
        assert!(!ledger.state.lock().runs[&run].observing);
        drop(resumed);
        for credit in credits { ledger.discard(&credit); }
    }

    #[test]
    fn unrelated_progress_cannot_extend_one_blocked_reader_past_hard_ceiling() {
        // Judged by the producer's own wait, not by a count of 20 ms ticks:
        // under a loaded parallel run the ticks stretch, the bounds must not.
        let (idle, hard) = (Duration::from_millis(200), Duration::from_millis(600));
        let ledger = ready();
        let blocked_run = run(&ledger);
        let held = (0..RUN_ITEMS).map(|sequence| queued(&ledger, blocked_run, sequence as u64)).collect::<Vec<_>>();
        let other_run = run(&ledger);
        let worker = ledger.clone();
        let (tx, rx) = mpsc::channel();
        let producer = thread::spawn(move || {
            let started = Instant::now();
            let observing = matches!(worker.reserve_with_limits(blocked_run, 1, idle, hard), Admission::ObserverOnly);
            tx.send((observing, started.elapsed())).unwrap();
        });
        wait_for_waiter(&ledger);
        let deadline = Instant::now() + hard + Duration::from_secs(10);
        let mut sequence = 0;
        let waited = loop {
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok((observing, waited)) => { assert!(observing); break waited; }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    assert!(Instant::now() < deadline, "continuous other-run progress defeated the hard bound");
                    sequence += 1;
                    let credit = queued(&ledger, other_run, sequence);
                    assert!(ledger.dispatch(&credit));
                    ledger.acknowledge(&credit).unwrap();
                }
                Err(error) => panic!("producer disconnected: {error}"),
            }
        };
        producer.join().unwrap();
        // Without progress it would return at the idle limit; with progress
        // every ~20 ms it waits toward the hard ceiling. Only a 200 ms stall
        // of this thread could keep it under this margin.
        assert!(waited >= idle + Duration::from_millis(100), "valid progress must extend waiting beyond the idle limit ({waited:?})");
        for credit in held { ledger.discard(&credit); }
    }

    #[test]
    fn native_disposal_releases_credit_but_is_not_renderer_progress() {
        let ledger = ready();
        let run = run(&ledger);
        let native = queued(&ledger, run, 1);
        assert_eq!(ledger.discard_native(&native).items, 1, "native disposal still releases its ticket");
        assert!(ledger.state.lock().progress.is_none(), "a desktop-side disposal proves nothing about the page");
        let rendered = queued(&ledger, run, 2);
        assert!(ledger.dispatch(&rendered));
        assert_eq!(ledger.discard(&rendered).items, 1);
        assert!(ledger.state.lock().progress.is_some(), "the renderer's own disposal is progress");
    }

    #[test]
    fn stale_empty_and_wrong_run_settlements_are_not_renderer_progress() {
        let ledger = ready();
        let run = run(&ledger);
        let old = queued(&ledger, run, 1);
        let reset = ledger.begin_renderer();
        assert!(ledger.renderer_ready(reset.epoch, Duration::ZERO).is_err());
        ledger.discard(&old); // Valid native reset disposal, but a stale epoch.
        assert!(ledger.state.lock().progress.is_none());
        ledger.renderer_ready(reset.epoch, Duration::ZERO).unwrap();
        let current = queued(&ledger, run, 2);
        assert!(ledger.dispatch(&current));
        let mut wrong = current.clone(); wrong.run.generation += 1;
        assert!(ledger.acknowledge(&wrong).is_err());
        assert_eq!(ledger.discard(&wrong).items, 0);
        let empty = OutputCredit { tickets: vec![Uuid::new_v4()], ..current.clone() };
        assert_eq!(ledger.acknowledge(&empty).unwrap(), 0);
        assert_eq!(ledger.discard(&empty).items, 0);
        assert!(ledger.state.lock().progress.is_none());
        ledger.discard(&current);
        assert_eq!(ledger.state.lock().progress.map(|(epoch, _)| epoch), Some(reset.epoch));
    }

    #[test]
    fn metrics_count_only_valid_dispatched_ack_tickets_once_and_exclude_discards() {
        let ledger = ready();
        let run = run(&ledger);
        let other = OutputRun { run_id: Uuid::new_v4(), ..run };
        ledger.open_run(other);
        let first = queued(&ledger, run, 1);
        let second = queued(&ledger, run, 2);
        let foreign = queued(&ledger, other, 1);
        assert!(ledger.acknowledge(&first).is_err(), "queued is not dispatched/parsed");
        assert_eq!(ledger.metrics().parser_ack_tickets, 0);
        assert!(ledger.dispatch(&first) && ledger.dispatch(&second) && ledger.dispatch(&foreign));
        assert_eq!(ledger.metrics().parser_ack_tickets, 0, "dispatch is not a parser callback");
        let mut mixed = first.clone(); mixed.tickets.extend(foreign.tickets.clone());
        assert!(ledger.acknowledge(&mixed).is_err());
        assert_eq!(ledger.metrics().parser_ack_tickets, 0, "whole request validates before recording anything");
        let mut batch = first.clone();
        batch.tickets.extend(first.tickets.clone());
        batch.tickets.extend(second.tickets.clone());
        assert_eq!(ledger.acknowledge(&batch).unwrap(), 2);
        assert_eq!(ledger.acknowledge(&batch).unwrap(), 0);
        assert_eq!(ledger.discard(&foreign).items, 1);
        assert_eq!(ledger.acknowledge(&foreign).unwrap(), 0, "disposed is not parsed");
        let metrics = ledger.metrics();
        assert_eq!(metrics.parser_ack_tickets, 2);
        assert_eq!(metrics.latency_histogram.iter().sum::<u64>(), 2);
        assert_eq!((metrics.current_bytes, metrics.current_items), (0, 0));
        assert_eq!((metrics.peak_bytes, metrics.peak_items), (3 * (10 + DISPLAY_OVERHEAD), 3));
        assert_eq!(ledger.metrics().parser_ack_tickets, 2, "reading diagnostics changes no accounting");
    }

    #[test]
    fn metrics_reset_by_epoch_retain_only_native_usage_baseline_and_count_observer_entry_once() {
        let ledger = ready();
        let run = run(&ledger);
        let parsed = queued(&ledger, run, 1);
        assert!(ledger.dispatch(&parsed));
        ledger.acknowledge(&parsed).unwrap();
        let native = queued(&ledger, run, 2);
        let dispatched = queued(&ledger, run, 3);
        assert!(ledger.dispatch(&dispatched));
        let reset = ledger.begin_renderer();
        assert_eq!(reset.discarded.items, 1);
        let metrics = ledger.metrics();
        assert_eq!(metrics.epoch, Some(reset.epoch));
        assert_eq!((metrics.parser_ack_tickets, metrics.max_ms), (0, 0));
        assert_eq!(metrics.latency_histogram, [0; 8]);
        assert_eq!((metrics.current_bytes, metrics.peak_bytes), (10 + DISPLAY_OVERHEAD, 10 + DISPLAY_OVERHEAD));
        assert_eq!((metrics.current_items, metrics.peak_items), (1, 1));
        assert!(matches!(ledger.reserve_with_limits(run, 1, Duration::ZERO, Duration::ZERO), Admission::ObserverOnly));
        for _ in 0..10 { assert!(matches!(ledger.reserve(run, 1), Admission::ObserverOnly)); }
        assert_eq!(ledger.metrics().observer_entries, 1, "not one counter increment per omitted read");
        let quiet = ledger.metrics().observer_max_quiet_ms;
        assert!(matches!(ledger.reserve(run, 1), Admission::ObserverOnly));
        assert_eq!(ledger.metrics().observer_max_quiet_ms, quiet, "sample quiet time only on entry");
        assert_eq!(ledger.metrics().current_observing_runs, 1);
        let next = ledger.begin_renderer();
        assert_eq!(ledger.metrics().observer_entries, 0);
        assert_eq!(ledger.metrics().observer_max_quiet_ms, 0);
        assert_eq!(ledger.metrics().current_observing_runs, 1, "resetting history does not hide an ongoing observer episode");
        assert_eq!(ledger.acknowledge(&dispatched).unwrap(), 0);
        ledger.discard(&native);
        assert_eq!(ledger.metrics().parser_ack_tickets, 0, "old-epoch settlements cannot enter new metrics");
        ledger.renderer_ready(next.epoch, Duration::ZERO).unwrap();
        let fresh = queued(&ledger, run, 4);
        assert!(ledger.dispatch(&fresh));
        ledger.acknowledge(&fresh).unwrap();
        assert_eq!(ledger.metrics().parser_ack_tickets, 1);
        assert_eq!(ledger.metrics().current_items, 0);
        assert_eq!(ledger.metrics().current_observing_runs, 0);
    }

    #[test]
    fn latency_metrics_have_fixed_exclusive_buckets_exact_thresholds_and_saturating_counters() {
        let ledger = ready();
        {
            let mut state = ledger.state.lock();
            for millis in [16, 50, 100, 250, 1000, 2000, 5000, 5001] {
                state.metrics.parsed(Duration::from_millis(millis));
            }
            assert_eq!(state.metrics.latency_histogram, [1; 8]);
            state.metrics.parsed(Duration::from_millis(250) + Duration::from_nanos(1));
        }
        let metrics = ledger.metrics();
        assert_eq!(metrics.parser_ack_tickets, 9);
        assert_eq!(metrics.max_ms, 5001);
        assert_eq!(metrics.latency_histogram, [1, 1, 1, 1, 2, 1, 1, 1]);
        assert_eq!((metrics.over_250_ms, metrics.over_2000_ms, metrics.over_5000_ms), (5, 2, 1));
        let mut state = ledger.state.lock();
        state.metrics.parser_ack_tickets = u64::MAX;
        state.metrics.latency_histogram[7] = u64::MAX;
        state.metrics.parsed(Duration::from_secs(6));
        assert_eq!(state.metrics.parser_ack_tickets, u64::MAX);
        assert_eq!(state.metrics.latency_histogram[7], u64::MAX);
        drop(state);
        assert_eq!(ledger.metrics().over_250_ms, u64::MAX, "derived threshold totals saturate too");
    }

    #[test]
    fn observer_quiet_metric_uses_only_current_epoch_settlement_or_epoch_start() {
        let ledger = ready();
        let run = run(&ledger);
        let reset = ledger.begin_renderer(); // No admission capacity.
        {
            let mut state = ledger.state.lock();
            state.metrics.started_at = Some(Instant::now() - Duration::from_secs(2));
            state.progress = Some((reset.epoch, Instant::now() - Duration::from_millis(300)));
        }
        assert!(matches!(ledger.reserve_with_limits(run, 1, Duration::ZERO, Duration::ZERO), Admission::ObserverOnly));
        let quiet = ledger.metrics().observer_max_quiet_ms;
        assert!(quiet >= 300 && quiet < 2000, "current-epoch progress resets quiet age: {quiet}");
        let next_run = OutputRun { run_id: Uuid::new_v4(), ..run };
        ledger.open_run(next_run);
        {
            let mut state = ledger.state.lock();
            state.metrics.observer_max_quiet_ms = 0;
            state.progress = Some((Uuid::new_v4(), Instant::now()));
        }
        assert!(matches!(ledger.reserve_with_limits(next_run, 1, Duration::ZERO, Duration::ZERO), Admission::ObserverOnly));
        assert!(ledger.metrics().observer_max_quiet_ms >= 2000, "stale epoch progress cannot hide quiet time since epoch start");
    }
}
