//! v3 W4 S1 (opt-in, per Codex member): the App Server transport.
//!
//! One private owner (`codex app-server --listen unix://<private dir>`) per
//! run of the member; the visible terminal attaches to it with `--remote`;
//! PRIM-1's own client on the same socket follows the member's thread from
//! protocol facts only: `thread/started`, `thread/loaded/list` and
//! `thread/read`, never a title or the newest rollout (W4-DECISIONS 1-4).
//! The client answers no server request (approvals stay the person's, S3).
//! S2: a room wake for the bound thread is one `turn/start` sent by the
//! observer, the only owner of the socket; its receipt comes only from the
//! response to that request's own id (W4-DECISIONS S2).
//!
//! The owner lives exactly as long as its run: the observer thread owns the
//! process and stops it when the run ends, when PRIM-1 shuts down, or when
//! the launch that started it fails.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use shared_types::{LaunchSpec, SessionId};
use uuid::Uuid;

/// Owner sockets live under `<runtime>\cxo\<member>\`.
pub(crate) const OWNER_ROOT: &str = "cxo";
/// `sockaddr_un.sun_path` holds 108 bytes with its terminator.
const MAX_SOCKET_PATH_BYTES: usize = 107;
/// Every blocking read or write on the socket returns within this.
pub(crate) const IO_DEADLINE: Duration = Duration::from_secs(5);
/// The owner must be listening, and answer initialize, within this.
pub(crate) const LISTEN_DEADLINE: Duration = Duration::from_secs(30);
/// One request (loaded list, read) is answered within this.
const REQUEST_DEADLINE: Duration = Duration::from_secs(25);
/// The terminal's thread is bound within this of the owner being ready.
pub(crate) const BIND_DEADLINE: Duration = Duration::from_secs(180);
/// Without a thread/started (a resume), the loaded list is asked this often.
const LOADED_POLL: Duration = Duration::from_secs(2);
/// Notifications kept for later slices; older ones are dropped.
const NOTIFICATIONS_KEPT: usize = 256;
/// The observer's socket reads return this often, so a dispatch waits at
/// most this long for the observer to pick it up.
const OBSERVE_READ: Duration = Duration::from_millis(250);
/// A dispatch the observer has not picked up within this is never sent.
const DISPATCH_PICKUP: Duration = Duration::from_secs(10);
/// turn/start must be answered within this once sent.
const TURN_START_DEADLINE: Duration = Duration::from_secs(25);
/// Accepted attempts whose userMessage echo is still looked for.
const ECHOES_KEPT: usize = 64;
/// v3 W4 S4: one reconnect to the same owner, all attempts and calls included.
const RECONNECT_DEADLINE: Duration = Duration::from_secs(10);

/// The private socket of one run's owner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OwnerSocket {
    pub(crate) dir: PathBuf,
    pub(crate) path: PathBuf,
}

impl OwnerSocket {
    /// A fresh socket name per launch, in the member's own directory.
    pub(crate) fn for_launch(runtime_dir: &Path, session_id: SessionId, nonce: Uuid) -> Result<Self> {
        let dir = runtime_dir.join(OWNER_ROOT).join(session_id.simple().to_string());
        let path = dir.join(format!("{}.sock", &nonce.simple().to_string()[..8]));
        let text = path.to_str().ok_or_else(|| anyhow!("the owner socket path is not valid Unicode"))?;
        ensure!(text.len() <= MAX_SOCKET_PATH_BYTES,
            "the Codex App Server socket path would be {} bytes ({}); the limit is {MAX_SOCKET_PATH_BYTES}. \
             Use a shorter PRIM-1 runtime folder or the terminal transport", text.len(), path.display());
        Ok(Self { dir, path })
    }

    /// The endpoint both the owner and the terminal are given.
    pub(crate) fn endpoint(&self) -> String {
        format!("unix://{}", self.path.to_string_lossy().replace('\\', "/"))
    }

    /// Creates the directory private to the user (Codex refuses any other)
    /// and removes a leftover socket file of the same name.
    pub(crate) fn prepare(&self) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("failed to create {}", self.dir.display()))?;
        super::restrict_path_to_current_user(&self.dir, true)?;
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error).with_context(|| format!("failed to remove stale {}", self.path.display())),
        }
    }
}

/// Prepared at launch, started just before the terminal spawns.
#[derive(Debug, Clone)]
pub(crate) struct OwnerPlan {
    pub(crate) socket: OwnerSocket,
    pub(crate) spec: LaunchSpec,
    pub(crate) working_dir: String,
    pub(crate) expect: Expect,
    pub(crate) label: String,
}

/// What the terminal is launched to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Expect {
    /// A fresh thread the terminal starts (TUI-first, decision 1).
    Fresh,
    /// The stored thread the terminal resumes.
    Resume(String),
}

// ---------------------------------------------------------------- protocol

/// One text frame in each direction; `recv` returns None on a read timeout.
pub(crate) trait Transport: Send {
    fn send(&mut self, text: String) -> Result<()>;
    fn recv(&mut self) -> Result<Option<String>>;
    /// v3 W4 S4 (Reviewer 2ca77928): caps the next blocking read or write at
    /// `left`, what remains of an aggregate deadline; None restores the
    /// transport's usual timeouts.
    fn bound_io(&mut self, _left: Option<Duration>) -> Result<()> { Ok(()) }
}

/// A server request PRIM-1 saw and did not answer (approvals are the
/// person's; S3 proves how the terminal receives them).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UnansweredRequest {
    pub(crate) id: Value,
    pub(crate) method: String,
}

/// v3 W4 S3: the server requests that wait for a person. At 0.164 the owner
/// sends each one to every connection subscribed to its thread, takes the
/// first reply from any of them, and then sends `serverRequest/resolved`
/// {threadId, requestId} to all; a connection that subscribes later gets the
/// pending ones replayed (rust-v0.164.0 app-server outgoing_message.rs,
/// request_processors/thread_lifecycle.rs). The terminal is the one that
/// answers; PRIM-1 never replies to any of them, not even with an error,
/// because any reply would resolve the person's prompt.
const PERSON_REQUESTS: &[&str] = &[
    "item/commandExecution/requestApproval",
    "item/fileChange/requestApproval",
    "item/permissions/requestApproval",
    "item/tool/requestUserInput",
    "mcpServer/elicitation/request",
    "applyPatchApproval",
    "execCommandApproval",
];

/// One person request still open on the owner, by its exact request id.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpenRequest {
    pub(crate) id: Value,
    pub(crate) method: String,
    /// The thread it names; resolution must name the same one.
    pub(crate) thread_id: Option<String>,
    /// v3 W4 S4: seen open before PRIM-1 lost its connection to the owner and
    /// not seen again since, so whether it was resolved meanwhile is unknown.
    /// Only its exact resolution, a replay of it (still open) or the run's end
    /// settles it; nothing else clears it (Codex PRIM-1, S4 decision).
    pub(crate) uncertain: bool,
}

/// The person requests open on one owner, shared with the run's dispatcher.
pub(crate) type OpenRequests = Arc<parking_lot::Mutex<Vec<OpenRequest>>>;

/// Records a person request, or clears one on its exact resolution. Other
/// frames change nothing.
fn track_open_request(open: &mut Vec<OpenRequest>, value: &Value) {
    let Some(method) = value.get("method").and_then(Value::as_str) else { return };
    let thread = |params: Option<&Value>| params.and_then(|params| params.get("threadId"))
        .and_then(Value::as_str).map(str::to_string);
    if let Some(id) = value.get("id") {
        if !PERSON_REQUESTS.contains(&method) { return; }
        let thread_id = thread(value.get("params"));
        match open.iter_mut().find(|request| &request.id == id) {
            // A replay to a reconnected client: the same request, positively still open.
            Some(request) if request.thread_id == thread_id => request.uncertain = false,
            Some(_) => {}
            None => open.push(OpenRequest { id: id.clone(), method: method.to_string(), thread_id, uncertain: false }),
        }
        return;
    }
    if method != "serverRequest/resolved" { return; }
    let params = value.get("params");
    let (Some(id), Some(resolved_thread)) = (params.and_then(|params| params.get("requestId")), thread(params)) else { return };
    open.retain(|request| !(&request.id == id
        && request.thread_id.as_deref().is_none_or(|thread| thread.eq_ignore_ascii_case(&resolved_thread))));
}

/// v3 W4 S4: the connection to the owner was lost; everything seen open is
/// from now on uncertain until its own resolution or replay.
fn mark_uncertain(open: &OpenRequests) {
    for request in open.lock().iter_mut() { request.uncertain = true; }
}

/// JSON-RPC over the owner's websocket. Requests are numbered by this
/// client; a response is matched by its exact id, never by a method name.
pub(crate) struct Client<T: Transport> {
    transport: T,
    next_id: u64,
    notifications: VecDeque<Value>,
    pub(crate) unanswered: Vec<UnansweredRequest>,
    /// v3 W4 S3: kept current as frames are read, also while a request
    /// waits for its own response.
    pub(crate) open: OpenRequests,
    /// v3 W4 S4: no wait of this client runs past this instant (a reconnect's
    /// one aggregate deadline).
    limit: Option<Instant>,
}

impl<T: Transport> Client<T> {
    pub(crate) fn new(transport: T) -> Self {
        Self { transport, next_id: 1, notifications: VecDeque::new(), unanswered: Vec::new(), open: OpenRequests::default(),
            limit: None }
    }

    pub(crate) fn request(&mut self, method: &str, params: Value, timeout: Duration, cancel: &AtomicBool) -> Result<Value> {
        self.io_budget(method, cancel)?;
        let id = self.next_id;
        self.next_id += 1;
        self.transport.send(json!({ "id": id, "method": method, "params": params }).to_string())?;
        match self.response(id, method, timeout, cancel)? {
            Ok(result) => Ok(result),
            Err(message) => bail!("{method} was refused: {message}"),
        }
    }

    /// Waits for the response to exactly request `id`: Ok(Ok(result)),
    /// Ok(Err(the server's error message)), or Err when no answer came
    /// (deadline, cancel, transport). Everything else read meanwhile is kept.
    fn response(&mut self, id: u64, method: &str, timeout: Duration, cancel: &AtomicBool)
        -> Result<std::result::Result<Value, String>>
    {
        let deadline = self.limit.map_or(Instant::now() + timeout, |limit| limit.min(Instant::now() + timeout));
        loop {
            ensure!(!cancel.load(Ordering::Acquire), "cancelled");
            ensure!(Instant::now() < deadline, "{method} was not answered in time (at most {}s)", timeout.as_secs());
            self.io_budget(method, cancel)?;
            let Some(text) = self.transport.recv()? else { continue };
            let value: Value = serde_json::from_str(&text).context("the owner sent a frame that is not JSON")?;
            if value.get("method").is_none() && value.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(error) = value.get("error") {
                    return Ok(Err(error.get("message").and_then(Value::as_str).unwrap_or("no message").to_string()));
                }
                return Ok(Ok(value.get("result").cloned().unwrap_or(Value::Null)));
            }
            self.consume(value);
        }
    }

    /// v3 W4 S2: one room wake as `turn/start` on the bound thread. The
    /// outcome comes only from the response to this request's own id:
    /// its `turn.id` admits it, its error refuses it, and no answer (or a
    /// failed write) leaves it unknown. Nothing is written unless this call
    /// wins the dispatch's claim (pending -> sending) first.
    pub(crate) fn turn_start(&mut self, thread_id: &str, text: &str, client_message_id: &str, claim: &DispatchClaim,
        cancel: &AtomicBool) -> DispatchOutcome
    {
        if !claim.begin_sending() {
            return DispatchOutcome::NotSent("the caller had already given up on this wake; nothing was sent".into());
        }
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({ "id": id, "method": "turn/start", "params": {
            "threadId": thread_id,
            "input": [{ "type": "text", "text": text, "text_elements": [] }],
            "clientUserMessageId": client_message_id,
        } });
        if let Err(error) = self.transport.send(request.to_string()) {
            return DispatchOutcome::Unknown(format!("the request could not be fully written ({error:#})"));
        }
        match self.response(id, "turn/start", TURN_START_DEADLINE, cancel) {
            Ok(Ok(result)) => match result.get("turn").and_then(|turn| turn.get("id")).and_then(Value::as_str) {
                Some(turn) if !turn.is_empty() => DispatchOutcome::Accepted { turn_id: turn.to_string() },
                _ => DispatchOutcome::Unknown("the Codex App Server answered without a turn id".into()),
            },
            Ok(Err(message)) => DispatchOutcome::Refused(message),
            Err(error) => DispatchOutcome::Unknown(format!("{error:#}")),
        }
    }

    pub(crate) fn notify(&mut self, method: &str, params: Value, cancel: &AtomicBool) -> Result<()> {
        self.io_budget(method, cancel)?;
        self.transport.send(json!({ "method": method, "params": params }).to_string())
    }

    /// v3 W4 S4 (Reviewer 2ca77928): checked before every write and read of
    /// a client under an aggregate deadline: nothing is written once it is
    /// cancelled or the deadline has passed, and the transport's next
    /// blocking call gets only what is left.
    fn io_budget(&mut self, method: &str, cancel: &AtomicBool) -> Result<()> {
        ensure!(!cancel.load(Ordering::Acquire), "cancelled before {method}; nothing more was sent");
        let Some(limit) = self.limit else { return Ok(()) };
        let left = limit.saturating_duration_since(Instant::now());
        ensure!(!left.is_zero(), "the reconnect deadline passed before {method}; nothing more was sent");
        self.transport.bound_io(Some(left))
    }

    /// v3 W4 S4: ends the aggregate deadline and restores the usual timeouts.
    pub(crate) fn clear_limit(&mut self) -> Result<()> {
        self.limit = None;
        self.transport.bound_io(None)
    }

    /// The next notification: a queued one, else one read (None on timeout).
    pub(crate) fn next_notification(&mut self) -> Result<Option<Value>> {
        if let Some(value) = self.notifications.pop_front() {
            return Ok(Some(value));
        }
        let Some(text) = self.transport.recv()? else { return Ok(None) };
        let value: Value = serde_json::from_str(&text).context("the owner sent a frame that is not JSON")?;
        self.consume(value);
        Ok(self.notifications.pop_front())
    }

    fn consume(&mut self, value: Value) {
        let Some(method) = value.get("method").and_then(Value::as_str) else { return };
        track_open_request(&mut self.open.lock(), &value);
        if let Some(id) = value.get("id") {
            // A server request. Never answered here (S1, S3); recorded once.
            if self.unanswered.len() >= NOTIFICATIONS_KEPT { self.unanswered.remove(0); }
            self.unanswered.push(UnansweredRequest { id: id.clone(), method: method.to_string() });
            return;
        }
        if self.notifications.len() >= NOTIFICATIONS_KEPT {
            self.notifications.pop_front();
        }
        self.notifications.push_back(value);
    }

    /// initialize + initialized; the owner must be empty when a launch starts.
    pub(crate) fn handshake(&mut self, cancel: &AtomicBool) -> Result<()> {
        self.request("initialize", json!({
            "clientInfo": { "name": "prim1", "title": "PRIM-1", "version": env!("CARGO_PKG_VERSION") },
            "capabilities": { "experimentalApi": true },
        }), LISTEN_DEADLINE, cancel)?;
        self.notify("initialized", json!({}), cancel)
    }

    pub(crate) fn loaded(&mut self, cancel: &AtomicBool) -> Result<Vec<String>> {
        let result = self.request("thread/loaded/list", json!({}), REQUEST_DEADLINE, cancel)?;
        // Reviewer 0eb0b52d: a paged answer is not the whole loaded set, so
        // it can never prove "exactly one"; refused rather than paged through.
        if result.get("nextCursor").is_some_and(|cursor| !cursor.is_null()) {
            bail!("thread/loaded/list answered with more pages; the loaded set is not complete, so nothing is bound");
        }
        let data = result.get("data").and_then(Value::as_array)
            .ok_or_else(|| anyhow!("thread/loaded/list returned no data"))?;
        data.iter().map(|id| id.as_str().map(str::to_string)
            .ok_or_else(|| anyhow!("thread/loaded/list returned a non-string id"))).collect()
    }

    /// v3 W4 S4: subscribes this connection to a loaded thread (a client that
    /// joins after the thread was created is not subscribed automatically);
    /// the owner then replays that thread's still-open server requests.
    pub(crate) fn resume(&mut self, thread_id: &str, cancel: &AtomicBool) -> Result<()> {
        let result = self.request("thread/resume", json!({ "threadId": thread_id }), REQUEST_DEADLINE, cancel)?;
        let resumed = result.get("thread").and_then(|thread| thread.get("id")).and_then(Value::as_str);
        ensure!(resumed.is_some_and(|id| id.eq_ignore_ascii_case(thread_id)),
            "thread/resume answered for {resumed:?}, not {thread_id}");
        Ok(())
    }

    pub(crate) fn read(&mut self, thread_id: &str, cancel: &AtomicBool) -> Result<ThreadFacts> {
        let result = self.request("thread/read", json!({ "threadId": thread_id, "includeTurns": false }),
            REQUEST_DEADLINE, cancel)?;
        result.get("thread").and_then(ThreadFacts::from_thread)
            .ok_or_else(|| anyhow!("thread/read returned no thread"))
    }
}

/// v3 W4 S4: attaches a fresh connection to the SAME owner's bound thread:
/// initialize, the bound thread still loaded, still the member's root
/// thread, then thread/resume to subscribe. Any other answer refuses; nothing
/// is rebound here (a thread started while PRIM-1 was away was not seen).
///
/// Reviewer 20fc86d8: every other loaded thread is read, and each must be a
/// proved descendant of the bound root (its parent chain, all loaded, ends
/// at it). Those are subscribed too, after the root, because the owner
/// subscribes a connection only to threads created after it initialized,
/// and a sub-agent's person request goes to its own thread's subscribers
/// (rust-v0.164.0 app-server lib.rs thread_created -> try_attach_thread_listener).
/// Another root, or a thread whose ancestry cannot be proved, refuses the
/// reattach: the terminal may have moved to it, and its requests would not be
/// seen. The root stays the only dispatch target.
pub(crate) fn reattach<T: Transport>(client: &mut Client<T>, bound: &str, working_dir: &str, cancel: &AtomicBool)
    -> Result<()>
{
    client.handshake(cancel)?;
    let loaded = client.loaded(cancel)?;
    ensure!(loaded.iter().any(|id| id.eq_ignore_ascii_case(bound)),
        "the owner no longer has thread {bound} loaded ({} loaded)", loaded.len());
    let facts = client.read(bound, cancel)?;
    ensure!(facts.id.eq_ignore_ascii_case(bound) && facts.is_member_root(working_dir),
        "thread {bound} is no longer this member's root thread in {working_dir}");
    let mut parents = std::collections::HashMap::new();
    for id in loaded.iter().filter(|id| !id.eq_ignore_ascii_case(bound)) {
        let facts = client.read(id, cancel)?;
        ensure!(facts.id.eq_ignore_ascii_case(id), "thread/read for {id} answered for {}", facts.id);
        parents.insert(id.to_ascii_lowercase(), facts.parent.map(|parent| parent.to_ascii_lowercase()));
    }
    let root = bound.to_ascii_lowercase();
    for id in parents.keys() {
        let mut at = id.clone();
        // Each step moves to a loaded thread; more steps than threads is a cycle.
        for _ in 0..=parents.len() {
            match parents.get(&at) {
                Some(Some(parent)) if *parent == root => break,
                Some(Some(parent)) => at = parent.clone(),
                Some(None) => bail!("thread {id} is loaded beside {bound} and is not its sub-agent; the terminal                     may have moved to another conversation, so PRIM-1 does not follow this owner again"),
                None => bail!("thread {id}'s ancestry passes through {at}, which is not loaded; it cannot be proved                     a sub-agent of {bound}"),
            }
        }
        ensure!(parents.get(&at).is_some_and(|parent| parent.as_deref() == Some(root.as_str())),
            "thread {id}'s ancestry does not end at {bound}");
    }
    client.resume(bound, cancel)?;
    // Loaded-list order, so the subscriptions are deterministic.
    for id in loaded.iter().filter(|id| !id.eq_ignore_ascii_case(bound)) {
        client.resume(id, cancel)?;
    }
    Ok(())
}

/// v3 W4 S4 (Reviewer c799bf30): one reconnect attempt's reattach. A replay
/// seen during an attempt whose connection then fails proves nothing about
/// the time after it, so every failed attempt marks the whole set uncertain
/// again; only exact resolution removes a request, as before.
fn reattach_attempt<T: Transport>(client: &mut Client<T>, bound: &str, working_dir: &str, cancel: &AtomicBool)
    -> Result<()>
{
    let result = reattach(client, bound, working_dir, cancel).and_then(|()| client.clear_limit());
    if result.is_err() { mark_uncertain(&client.open); }
    result
}

/// What became of a proved binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BindOutcome {
    /// Bound to the run (or already its conversation).
    Applied,
    /// This run is not installed yet; try again.
    NotReady,
    /// The run ended or was replaced; nothing to bind.
    Stale,
}

/// v3 W4 S2: what became of one room wake sent as `turn/start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DispatchOutcome {
    /// The response to this request's own id named this turn.
    Accepted { turn_id: String },
    /// The response to this request's own id was an error.
    Refused(String),
    /// Nothing was written to the socket; the wake is planned again.
    NotSent(String),
    /// Written (or possibly written) and not answered: never sent again.
    Unknown(String),
}

/// Run on the observer thread immediately before the socket write; an Err
/// means nothing is sent (Reviewer 617c8f1c: revalidate at the write, not
/// at enqueue).
pub(crate) type SendCheck = Box<dyn FnOnce() -> std::result::Result<(), String> + Send>;

/// v3 W4 S2 (Claude PRIM-1 + Codex PRIM-1 c85f8ae0): one claim per
/// dispatch. The observer must win pending -> sending before it writes; the
/// caller reports NotSent (and re-plans) only if its own pending ->
/// cancelled won. A claim found sending is Unknown, never replayed.
#[derive(Default)]
pub(crate) struct DispatchClaim(std::sync::atomic::AtomicU8);

const CLAIM_PENDING: u8 = 0;
const CLAIM_SENDING: u8 = 1;
const CLAIM_CANCELLED: u8 = 2;

impl DispatchClaim {
    fn begin_sending(&self) -> bool {
        self.0.compare_exchange(CLAIM_PENDING, CLAIM_SENDING, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    fn cancel(&self) -> bool {
        self.0.compare_exchange(CLAIM_PENDING, CLAIM_CANCELLED, Ordering::AcqRel, Ordering::Acquire).is_ok()
    }

    /// The caller's outcome when no answer came back: unsent only if the
    /// caller's own cancel won the claim.
    fn give_up(&self, why: &str) -> DispatchOutcome {
        if self.cancel() {
            DispatchOutcome::NotSent(format!("{why}; nothing was sent"))
        } else {
            DispatchOutcome::Unknown(format!("{why} after the request was being sent"))
        }
    }
}

/// One room wake handed to the observer, the socket's only user.
struct Dispatch {
    check: SendCheck,
    thread_id: String,
    text: String,
    client_message_id: String,
    attempt: shared_types::RoomFeedCursor,
    pick_up_by: Instant,
    claim: Arc<DispatchClaim>,
    reply: mpsc::SyncSender<DispatchOutcome>,
}

impl Dispatch {
    /// Answers a dispatch that is not written. Only an observer cancel that
    /// wins the claim may say so; otherwise the caller already gave up and
    /// owns the outcome.
    fn refuse(self, why: String) {
        if self.claim.cancel() {
            let _ = self.reply.send(DispatchOutcome::NotSent(why));
        }
    }
}

#[derive(Default)]
struct DispatchState {
    /// The thread the observer applied to this run, once bound.
    bound: parking_lot::Mutex<Option<String>>,
    /// False once the observer stopped following the owner.
    stopped: AtomicBool,
    /// v3 W4 S5: true only while the one bounded reconnect runs.
    reconnecting: AtomicBool,
    /// v3 W4 S3: the person requests open on the owner, as the observer reads them.
    open: OpenRequests,
}

/// v3 W4 S2: the run's way to the observer. A wake takes this route only
/// when the observer still follows the owner and has applied exactly the
/// thread the attempt's recipe names; the route is chosen before any send.
#[derive(Clone)]
pub(crate) struct Dispatcher {
    requests: mpsc::Sender<Dispatch>,
    state: Arc<DispatchState>,
}

impl Dispatcher {
    fn new() -> (Self, mpsc::Receiver<Dispatch>) {
        let (requests, receiver) = mpsc::channel();
        (Self { requests, state: Arc::default() }, receiver)
    }

    /// Whether a wake for `thread_id` goes to the App Server.
    pub(crate) fn serves(&self, thread_id: &str) -> bool {
        !self.state.stopped.load(Ordering::Acquire)
            && self.state.bound.lock().as_deref().is_some_and(|bound| bound.eq_ignore_ascii_case(thread_id))
    }

    /// v3 W4 S5: what the member's state row says about this run's route.
    pub(crate) fn follow(&self) -> shared_types::AppServerFollow {
        use shared_types::AppServerFollow;
        if self.state.reconnecting.load(Ordering::Acquire) {
            AppServerFollow::Reconnecting
        } else if self.state.stopped.load(Ordering::Acquire) {
            AppServerFollow::NotFollowed
        } else if self.state.bound.lock().is_some() {
            AppServerFollow::Followed
        } else {
            AppServerFollow::WaitingForThread
        }
    }

    /// v3 W4 S3: the oldest request the member's App Server was seen waiting
    /// on a person for and not seen resolved, with whether the observer still
    /// follows the owner. Losing the observer is not a resolution (Reviewer
    /// 7b925a34): a request seen open stays open, now uncertain, for this run
    /// only; the dispatcher, and with it this record, ends with the run.
    pub(crate) fn open_request(&self) -> Option<(OpenRequest, bool)> {
        let request = self.state.open.lock().first().cloned()?;
        let followed = !self.state.stopped.load(Ordering::Acquire) && !request.uncertain;
        Some((request, followed))
    }

    /// Sends one wake and waits for its outcome. The attempt token is the
    /// correlation id (W4-REVIEW-01): never an idempotency key, never replayed.
    pub(crate) fn turn_start(&self, thread_id: &str, text: &str, attempt: shared_types::RoomFeedCursor,
        check: SendCheck) -> DispatchOutcome
    {
        let (reply, outcome) = mpsc::sync_channel(1);
        let claim = Arc::new(DispatchClaim::default());
        let pick_up_by = Instant::now() + DISPATCH_PICKUP;
        let dispatch = Dispatch { check, thread_id: thread_id.to_string(), text: text.to_string(),
            client_message_id: client_message_id(attempt), attempt, pick_up_by, claim: claim.clone(), reply };
        if self.requests.send(dispatch).is_err() {
            return DispatchOutcome::NotSent("PRIM-1 no longer follows this member's Codex App Server".into());
        }
        match outcome.recv_timeout(DISPATCH_PICKUP + TURN_START_DEADLINE + IO_DEADLINE * 2) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Timeout) => claim.give_up("the Codex App Server observer gave no outcome in time"),
            Err(mpsc::RecvTimeoutError::Disconnected) => claim.give_up("the Codex App Server observer ended"),
        }
    }
}

/// What a test observer does with one dispatch it has claimed for sending.
#[cfg(test)]
pub(crate) enum TestAnswer {
    Outcome(DispatchOutcome),
    /// Never answers: the reply is dropped while the claim reads sending.
    Silent,
}

/// A stand-in observer for supervisor tests: one thread that, like the real
/// one, runs each dispatch's own check, wins the claim, records the text it
/// would send and answers as told. `before_check` runs first (to change room
/// state between enqueue and write).
#[cfg(test)]
pub(crate) fn dispatcher_for_tests(thread_id: &str,
    mut before_check: impl FnMut() + Send + 'static,
    mut answer: impl FnMut(&str, &str) -> TestAnswer + Send + 'static) -> (Dispatcher, Arc<parking_lot::Mutex<Vec<String>>>)
{
    let (dispatcher, receiver) = Dispatcher::new();
    *dispatcher.state.bound.lock() = Some(thread_id.to_string());
    let sent = Arc::new(parking_lot::Mutex::new(Vec::new()));
    let recorded = sent.clone();
    std::thread::spawn(move || {
        while let Ok(mut dispatch) = receiver.recv() {
            before_check();
            let check = std::mem::replace(&mut dispatch.check, Box::new(|| Ok(())));
            if let Err(why) = check() {
                dispatch.refuse(why);
                continue;
            }
            if !dispatch.claim.begin_sending() { continue; }
            recorded.lock().push(dispatch.text.clone());
            match answer(&dispatch.text, &dispatch.client_message_id) {
                TestAnswer::Outcome(outcome) => { let _ = dispatch.reply.send(outcome); }
                TestAnswer::Silent => drop(dispatch),
            }
        }
    });
    (dispatcher, sent)
}

#[cfg(test)]
impl Dispatcher {
    pub(crate) fn stop_for_tests(&self) {
        self.state.stopped.store(true, Ordering::Release);
    }

    /// A dispatcher whose observer has bound no thread yet.
    pub(crate) fn unbound_for_tests() -> Self {
        Self::new().0
    }

    /// The observer's state during and after its one bounded reconnect.
    pub(crate) fn reconnecting_for_tests(&self, reconnecting: bool) {
        self.state.stopped.store(reconnecting, Ordering::Release);
        self.state.reconnecting.store(reconnecting, Ordering::Release);
    }

    /// Feeds one owner frame to the request tracker, as the observer would.
    pub(crate) fn observe_for_tests(&self, frame: Value) {
        track_open_request(&mut self.state.open.lock(), &frame);
    }

    /// What a lost connection does to what was seen open.
    pub(crate) fn connection_lost_for_tests(&self) {
        mark_uncertain(&self.state.open);
    }
}

/// The correlation id one attempt is sent with.
pub(crate) fn client_message_id(attempt: shared_types::RoomFeedCursor) -> String {
    format!("prim1-attempt-{}-{}", attempt.epoch, attempt.sequence)
}

/// An accepted attempt whose own userMessage item is looked for.
#[derive(Debug, Clone, PartialEq)]
struct Echo {
    thread_id: String,
    client_message_id: String,
    attempt: shared_types::RoomFeedCursor,
}

/// The attempt an `item/started` userMessage on that same thread echoes by
/// its clientId, and where. Whether 0.164 copies clientUserMessageId into
/// item.clientId is not proved (Reviewer 6cf1be95); without the echo the
/// attempt keeps its other evidence paths.
fn echoed(echoes: &[Echo], notification: &Value) -> Option<(usize, String)> {
    if notification.get("method").and_then(Value::as_str) != Some("item/started") { return None; }
    let params = notification.get("params")?;
    let item = params.get("item")?;
    if item.get("type").and_then(Value::as_str) != Some("userMessage") { return None; }
    let thread = params.get("threadId").and_then(Value::as_str)?;
    let client_id = item.get("clientId").and_then(Value::as_str)?;
    let index = echoes.iter().position(|echo| echo.client_message_id == client_id
        && echo.thread_id.eq_ignore_ascii_case(thread))?;
    let turn = params.get("turnId").and_then(Value::as_str).unwrap_or("?");
    let id = item.get("id").and_then(Value::as_str).unwrap_or("?");
    Some((index, format!("thread {thread} turn {turn} item {id}")))
}

/// What PRIM-1 reads from one protocol Thread object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ThreadFacts {
    pub(crate) id: String,
    pub(crate) cwd: String,
    pub(crate) ephemeral: bool,
    pub(crate) parent: Option<String>,
    pub(crate) forked_from: Option<String>,
}

impl ThreadFacts {
    pub(crate) fn from_thread(thread: &Value) -> Option<Self> {
        let text = |key: &str| thread.get(key).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            id: text("id")?,
            cwd: text("cwd")?,
            ephemeral: thread.get("ephemeral").and_then(Value::as_bool)?,
            parent: text("parentThreadId"),
            forked_from: text("forkedFromId"),
        })
    }

    /// A thread of this member: a persistent root thread in its folder.
    fn is_member_root(&self, working_dir: &str) -> bool {
        !self.ephemeral && self.parent.is_none() && same_folder(&self.cwd, working_dir)
    }
}

/// The started thread a `thread/started` notification names.
pub(crate) fn started_thread(notification: &Value) -> Option<ThreadFacts> {
    (notification.get("method").and_then(Value::as_str) == Some("thread/started"))
        .then(|| notification.get("params")?.get("thread").and_then(ThreadFacts::from_thread))
        .flatten()
}

/// Windows folder identity for comparison only: separators, a verbatim
/// prefix, a trailing separator and case are not meaningful.
fn same_folder(left: &str, right: &str) -> bool {
    let normal = |path: &str| {
        let path = path.replace('/', "\\");
        let path = path.strip_prefix("\\\\?\\").unwrap_or(&path).trim_end_matches('\\').to_lowercase();
        path
    };
    normal(left) == normal(right)
}

/// Decides whether the facts prove the member's thread, and why. The first
/// binding of a fresh launch needs all three facts: a started root thread in
/// the member's folder, a loaded list of exactly that thread, and a read of
/// it. A resume needs the loaded list of exactly the stored thread and its
/// read (it may emit no thread/started). Once bound, a further root thread
/// the terminal starts (/new, a fork) is bound when it is loaded and read
/// the same. Anything else binds nothing; an Err is said once.
pub(crate) fn decide_binding(expect: &Expect, working_dir: &str, bound: Option<&str>,
    started: Option<&ThreadFacts>, loaded: &[String], read: Option<&ThreadFacts>)
    -> std::result::Result<Option<(String, &'static str)>, String>
{
    let read_matches = |id: &str| read.is_some_and(|facts| facts.id == id && facts.is_member_root(working_dir));
    match (bound, expect) {
        (None, Expect::Fresh) => {
            let Some(started) = started else { return Ok(None) };
            if !started.is_member_root(working_dir) {
                return Err(format!("the owner started thread {} that is not a root thread in {working_dir}; nothing was bound", started.id));
            }
            if loaded != [started.id.clone()] {
                return Err(format!("the owner has {} loaded thread(s) where exactly {} was expected; nothing was bound",
                    loaded.len(), started.id));
            }
            Ok(read_matches(&started.id).then(|| (started.id.clone(), "launch")))
        }
        (None, Expect::Resume(stored)) => match loaded {
            [] => Ok(None),
            [only] if only.eq_ignore_ascii_case(stored) => Ok(read_matches(only).then(|| (only.clone(), "resume"))),
            _ => Err(format!("the owner loaded {loaded:?}, not exactly the stored thread {stored}; nothing was bound")),
        },
        (Some(current), _) => {
            let Some(started) = started.filter(|started| !started.id.eq_ignore_ascii_case(current)) else { return Ok(None) };
            if !started.is_member_root(working_dir) {
                return Ok(None); // a sub-agent or another folder's thread: not the member's
            }
            if !loaded.iter().any(|id| id == &started.id) {
                return Ok(None);
            }
            Ok(read_matches(&started.id).then(|| (started.id.clone(),
                if started.forked_from.is_some() { "fork" } else { "new" })))
        }
    }
}

// ---------------------------------------------------------------- the owner

/// Held by the launch until the run is installed. Dropping it before
/// `detach` stops the owner (the launch failed).
pub(crate) struct OwnerGuard {
    cancel: Arc<AtomicBool>,
    detached: bool,
    dispatcher: Dispatcher,
}

impl OwnerGuard {
    pub(crate) fn detach(mut self) {
        self.detached = true;
    }

    /// The installed run keeps this to send its room wakes (S2).
    pub(crate) fn dispatcher(&self) -> Dispatcher {
        self.dispatcher.clone()
    }
}

impl Drop for OwnerGuard {
    fn drop(&mut self) {
        if !self.detached {
            self.cancel.store(true, Ordering::Release);
        }
    }
}

/// Starts the owner and the observer, and waits until the owner is listening,
/// initialized and empty. Errors name what failed; nothing is left running.
#[cfg(windows)]
pub(crate) fn start(handle: &super::SupervisorHandle, plan: OwnerPlan, session_id: SessionId,
    generation: shared_types::SessionGeneration, run_id: Uuid) -> Result<OwnerGuard>
{
    plan.socket.prepare()?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = mpsc::channel::<Result<()>>();
    let weak = Arc::downgrade(&handle.inner);
    let thread_cancel = cancel.clone();
    let (dispatcher, dispatches) = Dispatcher::new();
    let state = dispatcher.state.clone();
    std::thread::Builder::new().name("prim1-codex-owner".into()).spawn(move || {
        observe(weak, plan, session_id, generation, run_id, thread_cancel, ready_tx, dispatches, &state);
        state.stopped.store(true, Ordering::Release);
    }).context("failed to start the Codex App Server observer")?;
    let guard = OwnerGuard { cancel, detached: false, dispatcher };
    match ready_rx.recv_timeout(LISTEN_DEADLINE + IO_DEADLINE * 2) {
        Ok(Ok(())) => Ok(guard),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(anyhow!("the Codex App Server owner was not ready within {}s", LISTEN_DEADLINE.as_secs())),
    }
}

#[cfg(not(windows))]
pub(crate) fn start(_handle: &super::SupervisorHandle, _plan: OwnerPlan, _session_id: SessionId,
    _generation: shared_types::SessionGeneration, _run_id: Uuid) -> Result<OwnerGuard>
{
    bail!("the Codex App Server transport is supported on Windows only")
}

#[cfg(windows)]
struct OwnerProcess {
    child: std::process::Child,
    job: pty_host::ProcessJob,
    socket: PathBuf,
}

#[cfg(windows)]
impl OwnerProcess {
    fn spawn(plan: &OwnerPlan) -> Result<Self> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_SUSPENDED: u32 = 0x0000_0004;
        let log = plan.socket.path.with_extension("log");
        let stderr = std::fs::File::create(&log).with_context(|| format!("failed to create {}", log.display()))?;
        let mut command = std::process::Command::new(&plan.spec.program);
        command.args(&plan.spec.args).current_dir(&plan.spec.working_dir)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(stderr)
            .creation_flags(CREATE_NO_WINDOW | CREATE_SUSPENDED);
        for variable in &plan.spec.env {
            command.env(&variable.key, &variable.value);
        }
        let mut child = command.spawn()
            .with_context(|| format!("failed to start the Codex App Server owner {}", plan.spec.program))?;
        // Its own job, so the owner's whole tree (MCP servers, tools) ends
        // with the run; the application job already ends it with PRIM-1.
        // Created suspended and resumed only once it is in the job, so no
        // descendant can start outside it (Reviewer dd905854).
        let job = match pty_host::ProcessJob::new().and_then(|job| {
            let process = pty_host::PinnedProcess::open(child.id())?;
            job.assign_process(&process)?;
            resume_only_thread(child.id())?;
            Ok(job)
        }) {
            Ok(job) => job,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error.context("failed to contain the Codex App Server owner"));
            }
        };
        Ok(Self { child, job, socket: plan.socket.path.clone() })
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }
}

/// Resumes the single thread of a process created suspended. The owner's
/// threads are counted first and only the sole one is resumed, so an
/// ambiguous enumeration runs no code before it is refused; ResumeThread
/// must return 1 (suspended once, now running), Reviewer f7642e37 and
/// Codex PRIM-1 0dd637ee. On any refusal the caller tears down the job.
#[cfg(windows)]
fn resume_only_thread(process_id: u32) -> Result<()> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, TH32CS_SNAPTHREAD, THREADENTRY32, Thread32First, Thread32Next},
            Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME},
        },
    };
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error()).context("failed to list the owner's threads");
    }
    let mut entry: THREADENTRY32 = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
    let mut threads = Vec::new();
    let mut more = unsafe { Thread32First(snapshot, &mut entry) } != 0;
    while more {
        if entry.th32OwnerProcessID == process_id {
            threads.push(entry.th32ThreadID);
        }
        more = unsafe { Thread32Next(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    let [thread_id] = threads[..] else {
        bail!("the suspended owner had {} thread(s), not one; it was not started", threads.len());
    };
    let thread = unsafe { OpenThread(THREAD_SUSPEND_RESUME, 0, thread_id) };
    if thread.is_null() {
        return Err(std::io::Error::last_os_error()).context("failed to open the owner's thread; it was not started");
    }
    let previous = unsafe { ResumeThread(thread) };
    let error = (previous == u32::MAX).then(std::io::Error::last_os_error);
    unsafe { CloseHandle(thread) };
    if let Some(error) = error {
        return Err(error).context("failed to resume the owner's thread; the owner is torn down with its job");
    }
    // 0 means it was already running and >1 that it is still suspended:
    // either way the launch is not the one verified, and the caller's job
    // teardown ends whatever ran (Codex PRIM-1, 25 Oct 19:28 Sofia).
    ensure!(previous == 1, "the owner's launch verification failed: its thread's previous suspend count was \
        {previous}, not 1; the owner is torn down with its job");
    Ok(())
}

#[cfg(windows)]
impl Drop for OwnerProcess {
    fn drop(&mut self) {
        let _ = self.job.terminate();
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.socket);
    }
}

/// The owner's websocket over its AF_UNIX socket, with read and write
/// deadlines; a read timeout is None, not an error.
#[cfg(windows)]
struct SocketTransport(tungstenite::WebSocket<uds_windows::UnixStream>);

#[cfg(windows)]
impl SocketTransport {
    /// `within` bounds the connect and the websocket handshake together, on
    /// the calling (observer) thread, with nothing left running past it
    /// (Codex PRIM-1 after b57ddd7: the native connect takes no deadline, and
    /// an abandoned connector thread is neither owned nor bounded). The
    /// socket is created nonblocking and owned at once; the connect and every
    /// handshake step wait in slices of at most 50 ms that check cancel and
    /// the deadline, and an early return closes the socket.
    fn connect(path: &Path, within: Duration, cancel: &AtomicBool) -> Result<Self> {
        use std::os::windows::io::AsRawSocket;
        use windows_sys::Win32::Networking::WinSock::POLLRDNORM;
        let deadline = Instant::now() + within;
        // Reviewer 6d6e0b03: cancel and the deadline are checked at entry,
        // before each handshake step and before success, not only while waiting.
        let still = |step: &str| -> Result<()> {
            ensure!(!cancel.load(Ordering::Acquire), "cancelled before {step} with {}; nothing more was sent", path.display());
            ensure!(Instant::now() < deadline, "the deadline passed before {step} with {}", path.display());
            Ok(())
        };
        let stream = connect_unix_nonblocking(path, deadline, cancel)?;
        let raw = stream.as_raw_socket() as windows_sys::Win32::Networking::WinSock::SOCKET;
        still("the websocket handshake")?;
        let mut attempt = tungstenite::client::client("ws://localhost/", stream);
        let socket = loop {
            match attempt {
                Ok((socket, _)) => break socket,
                Err(tungstenite::HandshakeError::Interrupted(mid)) => {
                    // Dropping `mid` on an error closes the socket.
                    wait_for_socket(raw, POLLRDNORM, deadline, cancel)
                        .with_context(|| format!("the websocket handshake with {} did not finish", path.display()))?;
                    still("a websocket handshake step")?;
                    attempt = mid.handshake();
                }
                Err(tungstenite::HandshakeError::Failure(error)) =>
                    bail!("the owner refused the websocket handshake: {error}"),
            }
        };
        // Dropping `socket` on this error closes it.
        still("using the connection")?;
        socket.get_ref().set_nonblocking(false)?;
        let mut transport = Self(socket);
        // After the handshake, reads return often enough for the observer to
        // pick up a dispatch promptly (S2); a partial frame stays buffered in
        // tungstenite across a timed-out read.
        transport.bound_io(None)?;
        Ok(transport)
    }
}

/// v3 W4 S4: an AF_UNIX connect that returns by `deadline` or on cancel.
/// The same socket as `uds_windows::UnixStream::connect` (AF_UNIX stream,
/// not inheritable, the path as UTF-8), but nonblocking: the pending connect
/// is polled for writability and its own SO_ERROR decides the result.
#[cfg(windows)]
fn connect_unix_nonblocking(path: &Path, deadline: Instant, cancel: &AtomicBool) -> Result<uds_windows::UnixStream> {
    use std::os::windows::io::{FromRawSocket, RawSocket};
    use windows_sys::Win32::Networking::WinSock as ws;
    // Nothing is created or attempted once cancelled or past the deadline.
    ensure!(!cancel.load(Ordering::Acquire), "cancelled before connecting to {}; nothing was attempted", path.display());
    ensure!(Instant::now() < deadline, "the deadline passed before connecting to {}", path.display());
    static STARTUP: std::sync::Once = std::sync::Once::new();
    // Winsock is reference counted; this one start is never cleaned up.
    STARTUP.call_once(|| unsafe {
        let mut data: ws::WSADATA = std::mem::zeroed();
        ws::WSAStartup(0x0202, &mut data);
    });
    let wsa_error = || std::io::Error::from_raw_os_error(unsafe { ws::WSAGetLastError() });
    let bytes = path.to_str().ok_or_else(|| anyhow!("the owner socket path is not valid Unicode"))?.as_bytes();
    let mut address = ws::SOCKADDR_UN { sun_family: ws::AF_UNIX, sun_path: [0; 108] };
    ensure!(!bytes.contains(&0) && bytes.len() < address.sun_path.len(),
        "the owner socket path {} does not fit an AF_UNIX address", path.display());
    for (to, from) in address.sun_path.iter_mut().zip(bytes) { *to = *from as i8; }
    let raw = unsafe { ws::WSASocketW(ws::AF_UNIX as i32, ws::SOCK_STREAM, 0, std::ptr::null(), 0,
        ws::WSA_FLAG_NO_HANDLE_INHERIT) };
    ensure!(raw != ws::INVALID_SOCKET, "failed to create a socket for {}: {}", path.display(), wsa_error());
    // Owned from here: every early return closes it.
    let stream = unsafe { uds_windows::UnixStream::from_raw_socket(raw as RawSocket) };
    stream.set_nonblocking(true)?;
    let started = unsafe { ws::connect(raw, &address as *const ws::SOCKADDR_UN as *const ws::SOCKADDR,
        std::mem::size_of::<ws::SOCKADDR_UN>() as i32) };
    if started == ws::SOCKET_ERROR {
        let error = unsafe { ws::WSAGetLastError() };
        ensure!(error == ws::WSAEWOULDBLOCK, "failed to connect to {}: {}", path.display(),
            std::io::Error::from_raw_os_error(error));
        let revents = wait_for_socket(raw, ws::POLLWRNORM, deadline, cancel)
            .with_context(|| format!("not connected to {}", path.display()))?;
        if let Some(error) = stream.take_error()? {
            bail!("failed to connect to {}: {error}", path.display());
        }
        ensure!(revents & (ws::POLLERR | ws::POLLHUP) == 0, "failed to connect to {}: the owner did not accept",
            path.display());
    }
    Ok(stream)
}

/// Waits for `events` on the socket in slices of at most 50 ms, checking
/// cancel and the deadline before each; returns the events seen (possibly
/// none after a slice, for a caller that simply retries).
#[cfg(windows)]
fn wait_for_socket(raw: windows_sys::Win32::Networking::WinSock::SOCKET, events: i16, deadline: Instant,
    cancel: &AtomicBool) -> Result<i16>
{
    use windows_sys::Win32::Networking::WinSock as ws;
    loop {
        ensure!(!cancel.load(Ordering::Acquire), "cancelled");
        let left = deadline.saturating_duration_since(Instant::now());
        ensure!(!left.is_zero(), "the deadline passed");
        let mut poll = ws::WSAPOLLFD { fd: raw, events, revents: 0 };
        let slice = left.min(Duration::from_millis(50)).as_millis().max(1) as i32;
        let ready = unsafe { ws::WSAPoll(&mut poll, 1, slice) };
        ensure!(ready != ws::SOCKET_ERROR, "waiting on the socket failed: {}",
            std::io::Error::from_raw_os_error(unsafe { ws::WSAGetLastError() }));
        if ready > 0 { return Ok(poll.revents); }
        if events == ws::POLLRDNORM { return Ok(0); }
    }
}

#[cfg(windows)]
impl Transport for SocketTransport {
    fn send(&mut self, text: String) -> Result<()> {
        self.0.send(tungstenite::Message::text(text)).map_err(|error| anyhow!("write to the owner failed: {error}"))
    }

    fn bound_io(&mut self, left: Option<Duration>) -> Result<()> {
        // At least 1 ms: a zero socket timeout would mean no timeout at all.
        let cap = |usual: Duration| left.map_or(usual, |left| left.min(usual).max(Duration::from_millis(1)));
        self.0.get_ref().set_read_timeout(Some(cap(OBSERVE_READ)))?;
        self.0.get_ref().set_write_timeout(Some(cap(IO_DEADLINE)))?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<String>> {
        // One frame per call (Reviewer dd905854): a control frame returns
        // None, so the caller's cancel, deadline and run checks always run.
        match self.0.read() {
            Ok(tungstenite::Message::Text(text)) => Ok(Some(text.as_str().to_owned())),
            Ok(tungstenite::Message::Close(_)) => bail!("the owner closed the connection"),
            Ok(_) => Ok(None), // ping/pong are answered by the library; binary is not this protocol
            Err(tungstenite::Error::Io(error)) if matches!(error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => Ok(None),
            Err(error) => bail!("read from the owner failed: {error}"),
        }
    }
}

#[cfg(windows)]
#[allow(clippy::too_many_arguments)]
fn observe(weak: std::sync::Weak<super::SupervisorInner>, plan: OwnerPlan, session_id: SessionId,
    generation: shared_types::SessionGeneration, run_id: Uuid, cancel: Arc<AtomicBool>, ready: mpsc::Sender<Result<()>>,
    dispatches: mpsc::Receiver<Dispatch>, state: &DispatchState)
{
    let mut owner = match OwnerProcess::spawn(&plan) {
        Ok(owner) => owner,
        Err(error) => { let _ = ready.send(Err(error)); return; }
    };
    let connected = (|| -> Result<Client<SocketTransport>> {
        let deadline = Instant::now() + LISTEN_DEADLINE;
        loop {
            ensure!(!cancel.load(Ordering::Acquire), "the launch was cancelled");
            ensure!(!owner.exited(), "the Codex App Server owner exited before it listened (see {})",
                plan.socket.path.with_extension("log").display());
            if plan.socket.path.exists() && let Ok(transport) = SocketTransport::connect(&plan.socket.path, IO_DEADLINE, &cancel) {
                let mut client = Client::new(transport);
                client.open = state.open.clone();
                client.handshake(&cancel)?;
                let loaded = client.loaded(&cancel)?;
                ensure!(loaded.is_empty(), "the new owner already has {} loaded thread(s); it is not this member's", loaded.len());
                return Ok(client);
            }
            ensure!(Instant::now() < deadline, "the Codex App Server owner did not listen on {} within {}s",
                plan.socket.path.display(), LISTEN_DEADLINE.as_secs());
            std::thread::sleep(Duration::from_millis(200));
        }
    })();
    let mut client = match connected {
        Ok(client) => client,
        Err(error) => { let _ = ready.send(Err(error)); return; }
    };
    let _ = ready.send(Ok(()));
    let mut bound: Option<String> = None;
    // Reviewer 8b1858f1: a binding proved before this run is installed waits
    // here and is applied once the run is installed (after installation's
    // own conversation and hold assignment, which it must not lose to).
    let mut pending: Option<(String, &'static str)> = None;
    let mut said: Option<String> = None;
    let bind_deadline = Instant::now() + BIND_DEADLINE;
    let mut next_poll = Instant::now() + LOADED_POLL;
    let mut observing = true;
    let mut echoes: Vec<Echo> = Vec::new();
    // v3 W4 S5 (Reviewer 2542ee80): the open person requests as last shown;
    // the member's snapshot projects them, so a change refreshes it.
    let mut shown_open: Vec<(Value, bool)> = Vec::new();
    loop {
        if cancel.load(Ordering::Acquire) { break; }
        let Some(inner) = weak.upgrade() else { break };
        let handle = super::SupervisorHandle { inner };
        if !run_is_current(&handle, session_id, generation, run_id) { break; }
        let open_now: Vec<(Value, bool)> = state.open.lock().iter()
            .map(|request| (request.id.clone(), request.uncertain)).collect();
        if open_now != shown_open {
            shown_open = open_now;
            handle.app_server_follow_changed(session_id, run_id);
        }
        if owner.exited() {
            state.stopped.store(true, Ordering::Release);
            handle.app_server_follow_changed(session_id, run_id);
            refuse_dispatches(&dispatches, "its Codex App Server owner exited");
            handle.codex_owner_note(session_id, &plan.label, run_id,
                "its Codex App Server owner exited; the terminal has lost its thread host".into());
            // The run ends on its own when the terminal exits; nothing restarts the owner (S4).
            while !cancel.load(Ordering::Acquire) && run_is_current(&handle, session_id, generation, run_id) {
                refuse_dispatches(&dispatches, "its Codex App Server owner exited");
                std::thread::sleep(OBSERVE_READ);
            }
            break;
        }
        if let Some((id, reason)) = pending.clone() {
            match handle.bind_codex_owner_thread(session_id, run_id, generation, &id, reason) {
                Ok(BindOutcome::Applied) => {
                    *state.bound.lock() = Some(id.clone());
                    bound = Some(id);
                    pending = None;
                    handle.app_server_follow_changed(session_id, run_id);
                }
                Ok(BindOutcome::NotReady) => {}
                Ok(BindOutcome::Stale) => break,
                Err(error) => {
                    handle.codex_owner_note(session_id, &plan.label, run_id,
                        format!("its Codex App Server thread {id} was proved but not bound: {error:#}"));
                    pending = None;
                    observing = false;
                }
            }
        }
        if !observing {
            // No route any more: every later wake is typed (S4 reconnects).
            if !state.stopped.swap(true, Ordering::AcqRel) {
                handle.app_server_follow_changed(session_id, run_id);
            }
            refuse_dispatches(&dispatches, "PRIM-1 stopped following this member's Codex App Server");
            drop(handle);
            std::thread::sleep(OBSERVE_READ);
            continue;
        }
        // S2: room wakes, one at a time, on this thread that owns the socket.
        // Each one is judged again right before its own write, also after
        // an earlier one's 25 s wait (c85f8ae0): run, owner, binding, then
        // the request's own eligibility.
        while let Ok(mut dispatch) = dispatches.try_recv() {
            if Instant::now() > dispatch.pick_up_by {
                dispatch.refuse("the Codex App Server observer was busy; nothing was sent".into());
                continue;
            }
            if cancel.load(Ordering::Acquire) || owner.exited()
                || !run_is_current(&handle, session_id, generation, run_id) {
                dispatch.refuse("the member's run or its Codex App Server owner ended; nothing was sent".into());
                continue;
            }
            if !bound.as_deref().is_some_and(|bound| bound.eq_ignore_ascii_case(&dispatch.thread_id)) {
                dispatch.refuse("the member's bound Codex thread changed; nothing was sent".into());
                continue;
            }
            let check = std::mem::replace(&mut dispatch.check, Box::new(|| Ok(())));
            if let Err(why) = check() {
                dispatch.refuse(why);
                continue;
            }
            let outcome = client.turn_start(&dispatch.thread_id, &dispatch.text, &dispatch.client_message_id,
                &dispatch.claim, &cancel);
            if matches!(outcome, DispatchOutcome::Accepted { .. }) {
                if echoes.len() >= ECHOES_KEPT { echoes.remove(0); }
                echoes.push(Echo { thread_id: dispatch.thread_id.clone(),
                    client_message_id: dispatch.client_message_id.clone(), attempt: dispatch.attempt });
            }
            let _ = dispatch.reply.send(outcome);
        }
        let step = (|| -> Result<()> {
            let notification = client.next_notification()?;
            if let Some((index, locator)) = notification.as_ref().and_then(|value| echoed(&echoes, value)) {
                let echo = echoes.remove(index);
                handle.record_app_server_input_observed(session_id, echo.attempt, &locator);
            }
            let started = notification.as_ref().and_then(started_thread);
            let poll = bound.is_none() && pending.is_none() && Instant::now() >= next_poll;
            if started.is_none() && !poll { return Ok(()); }
            if poll { next_poll = Instant::now() + LOADED_POLL; }
            let loaded = client.loaded(&cancel)?;
            let candidate = started.as_ref().map(|facts| facts.id.clone())
                .or_else(|| match (&plan.expect, loaded.as_slice()) {
                    (Expect::Resume(_), [only]) => Some(only.clone()),
                    _ => None,
                });
            let read = match candidate { Some(id) => Some(client.read(&id, &cancel)?), None => None };
            let current = bound.as_deref().or(pending.as_ref().map(|(id, _)| id.as_str()));
            match decide_binding(&plan.expect, &plan.working_dir, current, started.as_ref(), &loaded, read.as_ref()) {
                // Applied at the top of the next step, once the run is installed.
                Ok(Some(proved)) => pending = Some(proved),
                Ok(None) => {}
                Err(problem) => if said.as_deref() != Some(problem.as_str()) {
                    handle.codex_owner_note(session_id, &plan.label, run_id, problem.clone());
                    said = Some(problem);
                },
            }
            Ok(())
        })();
        if let Err(error) = step {
            // v3 W4 S4: the connection is lost. While the SAME owner lives and a
            // thread is bound, one bounded reconnect to it (a9df38f0: never a
            // replacement owner). Meanwhile no wake goes this way, what was seen
            // open turns uncertain, and nothing is sent again.
            state.stopped.store(true, Ordering::Release);
            mark_uncertain(&state.open);
            refuse_dispatches(&dispatches, "PRIM-1 lost its connection to this member's Codex App Server");
            let attempt = match bound.as_deref() {
                Some(thread) if !owner.exited() => {
                    state.reconnecting.store(true, Ordering::Release);
                    handle.app_server_follow_changed(session_id, run_id);
                    let attempt = reconnect(&plan, thread, &cancel, &state.open);
                    // Reviewer 2ca77928: the reattach blocked; the route is
                    // published again only for the same run and a live owner.
                    let attempt = attempt.and_then(|fresh| {
                        ensure!(!cancel.load(Ordering::Acquire) && !owner.exited()
                            && run_is_current(&handle, session_id, generation, run_id),
                            "the member's run or its Codex App Server owner ended during the reconnect");
                        Ok(fresh)
                    });
                    state.reconnecting.store(false, Ordering::Release);
                    Some(attempt)
                }
                _ => None,
            };
            match attempt {
                Some(Ok(fresh)) => {
                    client = fresh;
                    state.stopped.store(false, Ordering::Release);
                    handle.app_server_follow_changed(session_id, run_id);
                    handle.codex_owner_note(session_id, &plan.label, run_id, format!(
                        "PRIM-1 lost its connection to its Codex App Server ({error:#}) and reconnected to the same owner \
                         and thread; nothing was sent again"));
                }
                attempt => {
                    // A replay seen by a reattach that was then refused (the
                    // run check above) says nothing after it (c799bf30).
                    mark_uncertain(&state.open);
                    let why = attempt.and_then(Result::err).map(|why| format!("; reconnecting failed: {why:#}"))
                        .unwrap_or_default();
                    // Observation ends; the owner and the terminal keep working.
                    handle.app_server_follow_changed(session_id, run_id);
                    handle.codex_owner_note(session_id, &plan.label, run_id, format!(
                        "PRIM-1 stopped following its Codex App Server thread: {error:#}{why}. Its room messages are \
                         typed from now on; restarting the member clears this"));
                    observing = false;
                }
            }
            continue;
        }
        if bound.is_none() && pending.is_none() && Instant::now() >= bind_deadline && said.is_none() {
            let problem = format!("no thread was bound within {}s of its Codex App Server owner starting",
                BIND_DEADLINE.as_secs());
            handle.codex_owner_note(session_id, &plan.label, run_id, problem.clone());
            said = Some(problem);
        }
    }
    drop(client);
    drop(owner);
}

/// v3 W4 S4: reconnects to the same owner's socket and reattaches the bound
/// thread, all within one aggregate deadline: every connect, socket timeout
/// and request gets only what is left of it (Codex PRIM-1 2aecaa1c).
#[cfg(windows)]
fn reconnect(plan: &OwnerPlan, bound: &str, cancel: &AtomicBool, open: &OpenRequests) -> Result<Client<SocketTransport>> {
    let deadline = Instant::now() + RECONNECT_DEADLINE;
    let mut last = anyhow!("no attempt was made");
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || cancel.load(Ordering::Acquire) { break; }
        match SocketTransport::connect(&plan.socket.path, left, cancel) {
            Ok(transport) => {
                let mut client = Client::new(transport);
                client.open = open.clone();
                client.limit = Some(deadline);
                match reattach_attempt(&mut client, bound, &plan.working_dir, cancel) {
                    Ok(()) => return Ok(client),
                    Err(error) => last = error,
                }
            }
            Err(error) => last = error,
        }
        std::thread::sleep(Duration::from_millis(200).min(deadline.saturating_duration_since(Instant::now())));
    }
    Err(last.context(format!("not reconnected within {}s", RECONNECT_DEADLINE.as_secs())))
}

/// Answers every waiting dispatch: nothing of it was written.
fn refuse_dispatches(dispatches: &mpsc::Receiver<Dispatch>, why: &str) {
    while let Ok(dispatch) = dispatches.try_recv() {
        dispatch.refuse(format!("{why}; nothing was sent"));
    }
}

#[cfg(windows)]
fn run_is_current(handle: &super::SupervisorHandle, session_id: SessionId,
    generation: shared_types::SessionGeneration, run_id: Uuid) -> bool
{
    if handle.inner.shutdown_started.load(Ordering::Acquire) { return false; }
    let slots = handle.inner.slots.lock();
    slots.get_by_id(session_id).is_some_and(|slot| slot.generation == generation
        && slot.run_id == Some(run_id) && (slot.running.is_some() || slot.spawn_in_flight.is_some()))
}

impl super::SupervisorHandle {
    /// Binds the thread the owner proved to the run that owns it, through the
    /// follower's own path (catalog first, then the slot; never twice).
    pub(crate) fn bind_codex_owner_thread(&self, session_id: SessionId, run_id: Uuid,
        generation: shared_types::SessionGeneration, thread_id: &str, reason: &'static str) -> Result<BindOutcome>
    {
        let (pid, report) = {
            let slots = self.inner.slots.lock();
            let Some(slot) = slots.get_by_id(session_id)
                .filter(|slot| slot.run_id == Some(run_id) && slot.generation == generation) else {
                return Ok(BindOutcome::Stale);
            };
            // Proved before the run is installed: wait for the installation.
            match (slot.running.is_some(), slot.spawn_in_flight.is_some(), slot.process_id) {
                (true, false, Some(pid)) =>
                    (pid, super::identity_tracker::same_run_report(slot).map(|evidence| evidence.thread_id.clone())),
                (false, false, _) => return Ok(BindOutcome::Stale),
                _ => return Ok(BindOutcome::NotReady),
            }
        };
        match self.apply_tracked_conversation(session_id, run_id, generation, thread_id, reason, "app_server", pid,
            report.as_deref()) {
            Ok(_) => Ok(BindOutcome::Applied),
            Err(error) if error.is::<super::identity_tracker::TrackedRunGone>() => Ok(BindOutcome::Stale),
            Err(error) => Err(error),
        }
    }

    /// v3 W4 S2: the accepted attempt's own userMessage item was seen on the
    /// same thread. Recorded as InputObserved for each request of that
    /// attempt still open with its Written receipt, as the reconciler does;
    /// never an ACK. The attempt token is unique per attempt, so it names
    /// this run's own attempt only.
    pub(crate) fn record_app_server_input_observed(&self, session_id: SessionId,
        attempt: shared_types::RoomFeedCursor, locator: &str)
    {
        let mut events = Vec::new();
        {
            let _publish = self.inner.room_event_publish.lock();
            let mut rooms = self.inner.rooms.lock();
            let Some(room_id) = rooms.room_by_session.get(&session_id).copied() else { return };
            let Some(room) = rooms.get_mut(room_id) else { return };
            for (event, recipient, recipe, written) in room.open_submissions() {
                if recipient != session_id || recipe.attempt != attempt { continue; }
                let shared_types::RoomFeedItem::Message { message_id, .. } = &event.item else { continue };
                if !room.submission_open(&event, recipient, attempt, written)
                    || room.input_observed_for(&event, recipient, attempt) { continue; }
                if let Ok(feed_event) = room.append(shared_types::RoomFeedItem::InputObserved { message_id: *message_id,
                    recipient_id: recipient, attempt, source: "codex_app_server".into(), locator: locator.into() }) {
                    events.push(feed_event);
                }
            }
        }
        for feed_event in events { self.emit(super::RuntimeEvent::RoomFeedEvent { feed_event }); }
    }

    /// Says what the owner's observer found, once, in the log and the
    /// member's room (a PRIM-1 notice, no ACK).
    /// v3 W4 S5: the member's App Server state (its route, or a person
    /// request open on it) changed; the desktop refreshes its snapshot for
    /// this run (dropped for a run that ended).
    pub(crate) fn app_server_follow_changed(&self, session_id: SessionId, run_id: Uuid) {
        let current = self.inner.slots.lock().get_by_id(session_id)
            .filter(|slot| slot.run_id == Some(run_id)).map(|slot| (slot.definition.alias.clone(), slot.generation));
        if let Some((session, generation)) = current {
            self.emit_run_work_state(super::RuntimeEvent::SessionPresentationChanged { session_id, run_id, session,
                timestamp: super::now_rfc3339() }, generation, run_id);
        }
    }

    pub(crate) fn codex_owner_note(&self, session_id: SessionId, label: &str, run_id: Uuid, text: String) {
        let message = format!("[PRIM-1] {label}: {text}");
        self.emit(super::RuntimeEvent::SystemLog { level: shared_types::LogLevel::Warn, message: message.clone(),
            timestamp: super::now_rfc3339() });
        let room_id = self.inner.rooms.lock().room_by_session.get(&session_id).copied();
        if let Some(room_id) = room_id
            && let Err(error) = self.post_prim1_notice(room_id, message, Some(format!("codex-owner-{run_id}-{}",
                Uuid::new_v4().simple())))
        {
            self.emit(super::RuntimeEvent::SystemLog { level: shared_types::LogLevel::Warn,
                message: format!("Codex App Server notice not stored: {error:#}"), timestamp: super::now_rfc3339() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scripted { sent: Vec<String>, replies: VecDeque<Option<String>>, budgets: Vec<Option<Duration>> }

    impl Transport for Scripted {
        fn send(&mut self, text: String) -> Result<()> { self.sent.push(text); Ok(()) }
        fn recv(&mut self) -> Result<Option<String>> {
            self.replies.pop_front().ok_or_else(|| anyhow!("closed"))
        }
        fn bound_io(&mut self, left: Option<Duration>) -> Result<()> { self.budgets.push(left); Ok(()) }
    }

    fn scripted(replies: &[Option<Value>]) -> Client<Scripted> {
        Client::new(Scripted { sent: Vec::new(),
            replies: replies.iter().map(|reply| reply.as_ref().map(Value::to_string)).collect(), budgets: Vec::new() })
    }

    const FOLDER: &str = "D:\\prim1-qa\\repo";
    const ID: &str = "01a16ec3-ef81-7cb0-b55e-824f6e793a8e";
    const OTHER: &str = "01a16ec3-ef81-7cb0-b55e-824f6e793a8f";

    fn facts(id: &str) -> ThreadFacts {
        ThreadFacts { id: id.into(), cwd: "d:/PRIM1-QA/repo/".into(), ephemeral: false, parent: None, forked_from: None }
    }

    #[test]
    fn w4_a_response_is_matched_by_its_own_id_and_server_requests_are_never_answered() {
        let cancel = AtomicBool::new(false);
        let mut client = scripted(&[
            None,
            Some(json!({ "method": "thread/started", "params": { "thread": { "id": ID, "cwd": FOLDER, "ephemeral": false } } })),
            Some(json!({ "id": 7, "method": "item/commandExecution/requestApproval", "params": {} })),
            Some(json!({ "id": 99, "result": { "data": [OTHER] } })),
            Some(json!({ "id": 1, "result": { "data": [ID] } })),
        ]);
        assert_eq!(client.loaded(&cancel).unwrap(), vec![ID.to_string()], "a stray response with another id is not ours");
        assert_eq!(client.unanswered, vec![UnansweredRequest { id: json!(7), method: "item/commandExecution/requestApproval".into() }]);
        assert_eq!(client.transport.sent.len(), 1, "nothing was sent in answer to the server request");
        let started = client.next_notification().unwrap().and_then(|value| started_thread(&value)).unwrap();
        assert_eq!(started.id, ID);
        assert!(client.request("x", json!({}), Duration::from_secs(5), &AtomicBool::new(true)).is_err(), "cancel ends a wait");
    }

    /// v3 W4 S3: a person request is open from its arrival, also while
    /// another request waits for its own response, until the resolution that
    /// names exactly its id and thread; PRIM-1 sends nothing in answer.
    #[test]
    fn w4_s3_a_person_request_stays_open_until_its_exact_resolution_and_is_never_answered() {
        let cancel = AtomicBool::new(false);
        let mut client = scripted(&[
            Some(json!({ "id": 7, "method": "item/commandExecution/requestApproval",
                "params": { "threadId": ID, "turnId": "t", "itemId": "i" } })),
            Some(json!({ "id": "s-1", "method": "item/tool/requestUserInput", "params": { "threadId": ID } })),
            Some(json!({ "id": 8, "method": "item/tool/call", "params": { "threadId": ID } })),
            Some(json!({ "id": 1, "result": { "data": [ID] } })),
        ]);
        client.loaded(&cancel).unwrap();
        let open = |client: &Client<_>| client.open.lock().iter().map(|request: &OpenRequest| request.id.clone()).collect::<Vec<_>>();
        assert_eq!(open(&client), vec![json!(7), json!("s-1")], "a dynamic tool call is not a person's request");
        assert_eq!(client.transport.sent.len(), 1, "only our own request was written; no reply to any server request");
        for (frame, left) in [
            // Another thread, another id, or the same number as a string: not this request.
            (json!({ "method": "serverRequest/resolved", "params": { "threadId": OTHER, "requestId": 7 } }), vec![json!(7), json!("s-1")]),
            (json!({ "method": "serverRequest/resolved", "params": { "threadId": ID, "requestId": 9 } }), vec![json!(7), json!("s-1")]),
            (json!({ "method": "serverRequest/resolved", "params": { "threadId": ID, "requestId": "7" } }), vec![json!(7), json!("s-1")]),
            (json!({ "method": "serverRequest/resolved", "params": { "threadId": ID, "requestId": 7 } }), vec![json!("s-1")]),
            (json!({ "method": "serverRequest/resolved", "params": { "threadId": ID, "requestId": "s-1" } }), vec![]),
        ] {
            client.consume(frame);
            assert_eq!(open(&client), left);
        }
        // A replay of an open request to a reconnecting client is one request.
        client.consume(json!({ "id": 5, "method": "item/fileChange/requestApproval", "params": { "threadId": ID } }));
        client.consume(json!({ "id": 5, "method": "item/fileChange/requestApproval", "params": { "threadId": ID } }));
        assert_eq!(open(&client), vec![json!(5)]);
        assert_eq!(client.transport.sent.len(), 1);
    }

    fn thread_reply(id: u64, thread: &str, ephemeral: bool) -> Value {
        json!({ "id": id, "result": { "thread": { "id": thread, "cwd": FOLDER, "ephemeral": ephemeral } } })
    }

    /// A sub-agent thread: its parent is named, as thread/read gives it.
    fn child_reply(id: u64, thread: &str, parent: &str) -> Value {
        json!({ "id": id, "result": { "thread": { "id": thread, "cwd": FOLDER, "ephemeral": false, "parentThreadId": parent } } })
    }

    fn resumed(client: &Client<Scripted>) -> Vec<String> {
        client.transport.sent.iter().map(|text| serde_json::from_str::<Value>(text).unwrap())
            .filter(|value| value["method"] == "thread/resume")
            .map(|value| value["params"]["threadId"].as_str().unwrap().to_string()).collect()
    }

    fn methods(client: &Client<Scripted>) -> Vec<String> {
        client.transport.sent.iter().map(|text| serde_json::from_str::<Value>(text).unwrap()["method"]
            .as_str().unwrap().to_string()).collect()
    }

    /// v3 W4 S4: a reconnect to the same owner reattaches only the bound
    /// thread, still loaded and still the member's root, and subscribes to
    /// it with thread/resume; anything else refuses before the resume.
    #[test]
    fn w4_s4_a_reconnect_reattaches_only_the_bound_thread_and_subscribes_with_resume() {
        let cancel = AtomicBool::new(false);
        let mut client = scripted(&[
            Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [ID] } })),
            Some(thread_reply(3, ID, false)),
            Some(thread_reply(4, ID, false)),
        ]);
        reattach(&mut client, ID, FOLDER, &cancel).unwrap();
        assert_eq!(methods(&client), ["initialize", "initialized", "thread/loaded/list", "thread/read", "thread/resume"]);
        assert_eq!(resumed(&client), [ID]);

        let mut gone = scripted(&[Some(json!({ "id": 1, "result": {} })), Some(json!({ "id": 2, "result": { "data": [OTHER] } }))]);
        assert!(reattach(&mut gone, ID, FOLDER, &cancel).is_err(), "the bound thread is no longer loaded");
        assert!(!methods(&gone).iter().any(|method| method == "thread/resume"), "nothing is rebound");

        let mut changed = scripted(&[Some(json!({ "id": 1, "result": {} })), Some(json!({ "id": 2, "result": { "data": [ID] } })),
            Some(thread_reply(3, ID, true))]);
        assert!(reattach(&mut changed, ID, FOLDER, &cancel).is_err(), "no longer the member's root thread");
        assert!(!methods(&changed).iter().any(|method| method == "thread/resume"));

        let mut other = scripted(&[Some(json!({ "id": 1, "result": {} })), Some(json!({ "id": 2, "result": { "data": [ID] } })),
            Some(thread_reply(3, ID, false)), Some(thread_reply(4, OTHER, false))]);
        assert!(reattach(&mut other, ID, FOLDER, &cancel).is_err(), "a resume that names another thread is refused");
    }

    /// v3 W4 S4 (Reviewer 20fc86d8): every other loaded thread is read. A
    /// proved sub-agent of the bound root (its whole parent chain loaded)
    /// is subscribed again after the root; another root, or an ancestry
    /// that cannot be proved, refuses before any resume.
    #[test]
    fn w4_s4_a_reconnect_subscribes_proved_sub_agents_again_and_refuses_another_root() {
        const CHILD: &str = "01a16ec3-ef81-7cb0-b55e-824f6e793a90";
        const GONE: &str = "01a16ec3-ef81-7cb0-b55e-824f6e793a91";
        let cancel = AtomicBool::new(false);
        // OTHER is a sub-agent of the root, CHILD a sub-agent of OTHER.
        let mut client = scripted(&[
            Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [CHILD, ID, OTHER] } })),
            Some(thread_reply(3, ID, false)),
            Some(child_reply(4, CHILD, OTHER)),
            Some(child_reply(5, OTHER, ID)),
            Some(thread_reply(6, ID, false)),
            Some(child_reply(7, CHILD, OTHER)),
            Some(child_reply(8, OTHER, ID)),
        ]);
        reattach(&mut client, ID, FOLDER, &cancel).unwrap();
        assert_eq!(resumed(&client), [ID, CHILD, OTHER], "the root first, then each proved sub-agent");

        // A second root in the same folder: the terminal may have moved to it.
        let mut root = scripted(&[Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [OTHER, ID] } })),
            Some(thread_reply(3, ID, false)), Some(thread_reply(4, OTHER, false))]);
        let error = reattach(&mut root, ID, FOLDER, &cancel).unwrap_err();
        assert!(format!("{error:#}").contains("not its sub-agent"), "{error:#}");
        assert!(resumed(&root).is_empty(), "nothing is subscribed or rebound");

        // A sub-agent whose parent is not loaded cannot be proved one of ours.
        let mut orphan = scripted(&[Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [ID, CHILD] } })),
            Some(thread_reply(3, ID, false)), Some(child_reply(4, CHILD, GONE))]);
        let error = reattach(&mut orphan, ID, FOLDER, &cancel).unwrap_err();
        assert!(format!("{error:#}").contains("cannot be proved"), "{error:#}");
        assert!(resumed(&orphan).is_empty());

        // A read answered for another thread proves nothing.
        let mut swapped = scripted(&[Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [ID, CHILD] } })),
            Some(thread_reply(3, ID, false)), Some(child_reply(4, OTHER, ID))]);
        assert!(reattach(&mut swapped, ID, FOLDER, &cancel).is_err());
        assert!(resumed(&swapped).is_empty());

        // Two sub-agents naming each other: a cycle, never ending at the root.
        let mut cycle = scripted(&[Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [ID, CHILD, OTHER] } })),
            Some(thread_reply(3, ID, false)), Some(child_reply(4, CHILD, OTHER)), Some(child_reply(5, OTHER, CHILD))]);
        assert!(reattach(&mut cycle, ID, FOLDER, &cancel).is_err());
        assert!(resumed(&cycle).is_empty());
    }

    /// v3 W4 S4: the reconnect's one deadline caps every wait inside it.
    #[test]
    fn w4_s4_the_reconnect_deadline_caps_every_wait() {
        let mut client = scripted(&[None, None, None, None, None, None]);
        client.limit = Some(Instant::now());
        let started = Instant::now();
        assert!(reattach(&mut client, ID, FOLDER, &AtomicBool::new(false)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1), "no call waits past the aggregate deadline");
        // Reviewer 2ca77928: an expired deadline writes nothing at all.
        assert!(client.transport.sent.is_empty(), "nothing is sent after the deadline: {:?}", client.transport.sent);

        // Nor does a cancelled reconnect.
        let mut cancelled = scripted(&[Some(json!({ "id": 1, "result": {} }))]);
        cancelled.limit = Some(Instant::now() + Duration::from_secs(10));
        assert!(reattach(&mut cancelled, ID, FOLDER, &AtomicBool::new(true)).is_err());
        assert!(cancelled.transport.sent.is_empty());

        // Each blocking read and write gets only what is left of the deadline;
        // clearing it restores the transport's usual timeouts.
        let mut bounded = scripted(&[
            Some(json!({ "id": 1, "result": {} })),
            Some(json!({ "id": 2, "result": { "data": [ID] } })),
            Some(thread_reply(3, ID, false)),
            Some(thread_reply(4, ID, false)),
        ]);
        let window = Duration::from_secs(10);
        bounded.limit = Some(Instant::now() + window);
        reattach(&mut bounded, ID, FOLDER, &AtomicBool::new(false)).unwrap();
        let budgets = bounded.transport.budgets.clone();
        // One before each write (4 requests and initialized) and each read (4 responses).
        assert_eq!(budgets.len(), 9, "{budgets:?}");
        assert!(budgets.iter().all(|left| left.is_some_and(|left| !left.is_zero() && left <= window)), "{budgets:?}");
        bounded.clear_limit().unwrap();
        assert_eq!(bounded.transport.budgets.last(), Some(&None));
        assert!(bounded.limit.is_none());
    }

    /// v3 W4 S4 (Reviewer c799bf30): a request replayed during an attempt
    /// whose reattach then fails is uncertain again; one replayed during an
    /// attempt that succeeds stays certain.
    #[test]
    fn w4_s4_a_failed_reconnect_attempt_makes_a_replayed_request_uncertain_again() {
        let cancel = AtomicBool::new(false);
        let replay = json!({ "id": 77, "method": "item/fileChange/requestApproval", "params": { "threadId": ID } });
        let uncertain = |open: &OpenRequests| open.lock().iter().map(|request| request.uncertain).collect::<Vec<_>>();
        for (resumed, expected) in [(json!({ "id": 4, "error": { "message": "thread is closing" } }), true),
            (thread_reply(4, ID, false), false)]
        {
            let open = OpenRequests::default();
            track_open_request(&mut open.lock(), &replay);
            mark_uncertain(&open);
            let mut client = scripted(&[
                Some(json!({ "id": 1, "result": {} })),
                Some(json!({ "id": 2, "result": { "data": [ID] } })),
                Some(thread_reply(3, ID, false)),
                Some(replay.clone()),
                Some(resumed),
            ]);
            client.open = open.clone();
            client.limit = Some(Instant::now() + Duration::from_secs(10));
            assert_eq!(reattach_attempt(&mut client, ID, FOLDER, &cancel).is_err(), expected);
            assert_eq!(uncertain(&open), [expected], "the replay counts only if the attempt held");
        }
    }

    /// v3 W4 S4 (Codex PRIM-1 after b57ddd7): the connect and the websocket
    /// handshake return by their own deadline and on cancel, on the calling
    /// thread. A private socket in a temporary folder accepts at the kernel
    /// and never answers the handshake; a missing socket refuses at once.
    #[cfg(windows)]
    #[test]
    fn w4_s4_the_owner_connect_returns_by_its_deadline_and_on_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.sock");
        let _silent = uds_windows::UnixListener::bind(&path).unwrap();
        let started = Instant::now();
        let error = SocketTransport::connect(&path, Duration::from_millis(300), &AtomicBool::new(false)).err().unwrap();
        let took = started.elapsed();
        assert!(took >= Duration::from_millis(250) && took < Duration::from_secs(2), "{took:?} {error:#}");
        assert!(format!("{error:#}").contains("handshake"), "{error:#}");

        // Already cancelled: nothing is attempted, so nothing reaches the
        // listener's queue (Reviewer 6d6e0b03), not merely a later wait error.
        _silent.set_nonblocking(true).unwrap();
        while _silent.accept().is_ok() {} // the earlier attempt's connection
        let error = SocketTransport::connect(&path, Duration::from_secs(10), &AtomicBool::new(true)).err().unwrap();
        assert!(format!("{error:#}").contains("nothing was attempted"), "{error:#}");
        assert_eq!(_silent.accept().err().map(|error| error.kind()), Some(std::io::ErrorKind::WouldBlock),
            "no connection was attempted");

        let started = Instant::now();
        assert!(SocketTransport::connect(&dir.path().join("none.sock"), Duration::from_secs(10), &AtomicBool::new(false)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2), "nothing listening refuses without waiting out the deadline");
    }

    /// v3 W4 S4: after a lost connection what was seen open is uncertain; a
    /// replay of the same request makes it certain again, its exact
    /// resolution removes it, and nothing else does. A request first seen
    /// after the reconnect is certain.
    #[test]
    fn w4_s4_a_lost_connection_makes_open_requests_uncertain_until_replayed_or_resolved() {
        let open = OpenRequests::default();
        let request = |id: u64| json!({ "id": id, "method": "item/fileChange/requestApproval", "params": { "threadId": ID } });
        let state = |open: &OpenRequests| open.lock().iter().map(|request| (request.id.clone(), request.uncertain)).collect::<Vec<_>>();
        track_open_request(&mut open.lock(), &request(1));
        track_open_request(&mut open.lock(), &request(2));
        mark_uncertain(&open);
        assert_eq!(state(&open), vec![(json!(1), true), (json!(2), true)]);
        track_open_request(&mut open.lock(), &request(2));
        track_open_request(&mut open.lock(), &request(3));
        assert_eq!(state(&open), vec![(json!(1), true), (json!(2), false), (json!(3), false)]);
        track_open_request(&mut open.lock(), &json!({ "method": "thread/status/changed",
            "params": { "threadId": ID, "status": { "type": "idle" } } }));
        assert_eq!(state(&open).len(), 3, "a status without person flags clears nothing (S4 decision)");
        track_open_request(&mut open.lock(), &json!({ "method": "serverRequest/resolved", "params": { "threadId": ID, "requestId": 1 } }));
        assert_eq!(state(&open), vec![(json!(2), false), (json!(3), false)]);
    }

    fn attempt() -> shared_types::RoomFeedCursor {
        shared_types::RoomFeedCursor { epoch: Uuid::nil(), sequence: 41 }
    }

    #[test]
    fn w4_s2_turn_start_is_accepted_only_by_the_response_to_its_own_id() {
        let cancel = AtomicBool::new(false);
        let claim = DispatchClaim::default();
        let mut client = scripted(&[
            Some(json!({ "method": "turn/started", "params": { "threadId": ID, "turn": { "id": "other" } } })),
            Some(json!({ "id": 99, "result": { "turn": { "id": "not-ours" } } })),
            None,
            Some(json!({ "id": 1, "result": { "turn": { "id": "t-1", "items": [], "status": "inProgress" } } })),
        ]);
        let token = client_message_id(attempt());
        assert_eq!(client.turn_start(ID, "pointer", &token, &claim, &cancel),
            DispatchOutcome::Accepted { turn_id: "t-1".into() }, "a generic turn event or another id never admits it");
        let sent: Value = serde_json::from_str(&client.transport.sent[0]).unwrap();
        assert_eq!(sent["method"], "turn/start");
        assert_eq!(sent["params"]["threadId"], ID);
        assert_eq!(sent["params"]["clientUserMessageId"], token.as_str());
        assert_eq!(sent["params"]["input"][0]["text"], "pointer");

        let refused = |reply: Value| scripted(&[Some(reply)])
            .turn_start(ID, "pointer", &token, &DispatchClaim::default(), &cancel);
        assert_eq!(refused(json!({ "id": 1, "error": { "code": -32600, "message": "no thread" } })),
            DispatchOutcome::Refused("no thread".into()));
        assert!(matches!(refused(json!({ "id": 1, "result": {} })), DispatchOutcome::Unknown(_)),
            "a success without a turn id certifies nothing");
        assert!(matches!(scripted(&[None]).turn_start(ID, "pointer", &token, &DispatchClaim::default(), &cancel),
            DispatchOutcome::Unknown(_)), "no answer is unknown, never accepted");
    }

    #[test]
    fn w4_s2_the_claim_decides_who_owns_an_unanswered_dispatch() {
        let cancel = AtomicBool::new(false);
        // The caller gave up first: the observer writes nothing.
        let claim = DispatchClaim::default();
        assert!(matches!(claim.give_up("timeout"), DispatchOutcome::NotSent(_)));
        let mut client = scripted(&[]);
        assert!(matches!(client.turn_start(ID, "pointer", "c", &claim, &cancel), DispatchOutcome::NotSent(_)));
        assert!(client.transport.sent.is_empty(), "a cancelled claim sends no byte");
        // The observer began sending first: the caller's give-up is Unknown.
        let claim = DispatchClaim::default();
        assert!(claim.begin_sending());
        assert!(matches!(claim.give_up("timeout"), DispatchOutcome::Unknown(_)));
        assert!(!claim.begin_sending(), "never sent twice");
        // An observer refusal after the caller gave up sends no reply.
        let (reply, outcome) = mpsc::sync_channel(1);
        let claim = Arc::new(DispatchClaim::default());
        let dispatch = Dispatch { check: Box::new(|| Ok(())), thread_id: ID.into(), text: "p".into(),
            client_message_id: "c".into(), attempt: attempt(), pick_up_by: Instant::now(), claim: claim.clone(), reply };
        assert!(matches!(claim.give_up("timeout"), DispatchOutcome::NotSent(_)));
        dispatch.refuse("late".into());
        assert!(outcome.try_recv().is_err(), "the caller already owns the outcome");
    }

    #[test]
    fn w4_s2_a_dispatch_the_observer_never_takes_is_not_sent() {
        let (dispatcher, receiver) = Dispatcher::new();
        drop(receiver);
        assert!(matches!(dispatcher.turn_start(ID, "p", attempt(), Box::new(|| Ok(()))), DispatchOutcome::NotSent(_)));
        let (dispatcher, receiver) = Dispatcher::new();
        assert!(!dispatcher.serves(ID), "no route before the observer applied a thread");
        *dispatcher.state.bound.lock() = Some(ID.to_ascii_uppercase());
        assert!(dispatcher.serves(ID) && !dispatcher.serves(OTHER));
        dispatcher.state.stopped.store(true, Ordering::Release);
        assert!(!dispatcher.serves(ID), "no route once observation stopped");
        drop(receiver);
    }

    #[test]
    fn w4_s2_only_the_attempts_own_user_message_on_its_thread_is_an_echo() {
        let token = client_message_id(attempt());
        let echoes = vec![Echo { thread_id: ID.into(), client_message_id: token.clone(), attempt: attempt() }];
        let item = |thread: &str, client_id: &str, kind: &str| json!({ "method": "item/started", "params": {
            "threadId": thread, "turnId": "t-1", "item": { "type": kind, "id": "i-1", "content": [], "clientId": client_id } } });
        assert_eq!(echoed(&echoes, &item(ID, &token, "userMessage")),
            Some((0, format!("thread {ID} turn t-1 item i-1"))));
        assert_eq!(echoed(&echoes, &item(OTHER, &token, "userMessage")), None, "another thread");
        assert_eq!(echoed(&echoes, &item(ID, "prim1-attempt-other", "userMessage")), None, "another attempt");
        assert_eq!(echoed(&echoes, &item(ID, &token, "agentMessage")), None, "not a user message");
        assert_eq!(echoed(&echoes, &json!({ "method": "turn/started", "params": { "threadId": ID } })), None);
    }

    #[test]
    fn w4_a_paged_loaded_list_is_refused_never_taken_as_complete() {
        let cancel = AtomicBool::new(false);
        let mut paged = scripted(&[Some(json!({ "id": 1, "result": { "data": [ID], "nextCursor": "c2" } }))]);
        assert!(paged.loaded(&cancel).unwrap_err().to_string().contains("more pages"));
        let mut last = scripted(&[Some(json!({ "id": 1, "result": { "data": [ID], "nextCursor": null } }))]);
        assert_eq!(last.loaded(&cancel).unwrap(), vec![ID.to_string()]);
    }

    #[test]
    fn w4_a_refused_or_unanswered_request_is_an_error_not_a_guess() {
        let cancel = AtomicBool::new(false);
        let mut refused = scripted(&[Some(json!({ "id": 1, "error": { "code": -32600, "message": "no" } }))]);
        assert!(refused.read(ID, &cancel).unwrap_err().to_string().contains("refused"));
        let mut silent = scripted(&[None, None]);
        assert!(silent.loaded(&cancel).is_err(), "a closed transport ends the wait");
    }

    #[test]
    fn w4_a_fresh_launch_binds_only_from_started_loaded_and_read_together() {
        let fresh = Expect::Fresh;
        let started = facts(ID);
        let loaded = vec![ID.to_string()];
        assert_eq!(decide_binding(&fresh, FOLDER, None, Some(&started), &loaded, Some(&facts(ID))),
            Ok(Some((ID.to_string(), "launch"))));
        assert_eq!(decide_binding(&fresh, FOLDER, None, None, &loaded, Some(&facts(ID))), Ok(None),
            "a loaded thread with no thread/started is not bound on a fresh launch");
        assert_eq!(decide_binding(&fresh, FOLDER, None, Some(&started), &loaded, None), Ok(None), "no read, no binding");
        assert!(decide_binding(&fresh, FOLDER, None, Some(&started), &[ID.into(), OTHER.into()], Some(&facts(ID))).is_err(),
            "two loaded threads are never resolved by a guess");
        let mut elsewhere = facts(ID);
        elsewhere.cwd = "D:\\other".into();
        assert!(decide_binding(&fresh, FOLDER, None, Some(&elsewhere), &loaded, Some(&elsewhere)).is_err());
        let mut ephemeral = facts(ID);
        ephemeral.ephemeral = true;
        assert!(decide_binding(&fresh, FOLDER, None, Some(&ephemeral), &loaded, Some(&ephemeral)).is_err());
    }

    #[test]
    fn w4_a_resume_binds_only_the_stored_thread_when_it_alone_is_loaded() {
        let resume = Expect::Resume(ID.into());
        assert_eq!(decide_binding(&resume, FOLDER, None, None, &[], None), Ok(None), "not loaded yet");
        assert_eq!(decide_binding(&resume, FOLDER, None, None, &[ID.into()], Some(&facts(ID))),
            Ok(Some((ID.to_string(), "resume"))));
        assert!(decide_binding(&resume, FOLDER, None, None, &[OTHER.into()], Some(&facts(OTHER))).is_err(),
            "another thread is never bound in place of the stored one");
    }

    #[test]
    fn w4_once_bound_a_new_root_thread_is_followed_and_a_sub_agent_is_not() {
        let fresh = Expect::Fresh;
        let loaded = vec![ID.to_string(), OTHER.to_string()];
        assert_eq!(decide_binding(&fresh, FOLDER, Some(ID), Some(&facts(OTHER)), &loaded, Some(&facts(OTHER))),
            Ok(Some((OTHER.to_string(), "new"))));
        let mut fork = facts(OTHER);
        fork.forked_from = Some(ID.into());
        assert_eq!(decide_binding(&fresh, FOLDER, Some(ID), Some(&fork), &loaded, Some(&fork)),
            Ok(Some((OTHER.to_string(), "fork"))));
        let mut sub = facts(OTHER);
        sub.parent = Some(ID.into());
        assert_eq!(decide_binding(&fresh, FOLDER, Some(ID), Some(&sub), &loaded, Some(&sub)), Ok(None));
        assert_eq!(decide_binding(&fresh, FOLDER, Some(ID), Some(&facts(ID)), &loaded, Some(&facts(ID))), Ok(None),
            "the bound thread again changes nothing");
    }

    #[test]
    fn w4_the_owner_socket_is_short_per_launch_and_refuses_a_long_path() {
        let session = Uuid::nil();
        let socket = OwnerSocket::for_launch(Path::new("C:\\rt"), session, Uuid::nil()).unwrap();
        assert_eq!(socket.endpoint(), "unix://C:/rt/cxo/00000000000000000000000000000000/00000000.sock");
        let long = "C:\\".to_string() + &"x".repeat(60);
        assert!(OwnerSocket::for_launch(Path::new(&long), session, Uuid::nil()).unwrap_err().to_string().contains("limit"));
    }

    #[test]
    fn w4_folder_identity_ignores_separators_case_and_verbatim_prefix() {
        assert!(same_folder("\\\\?\\D:\\Repo\\", "d:/repo"));
        assert!(!same_folder("D:\\repo2", "D:\\repo"));
    }
}
