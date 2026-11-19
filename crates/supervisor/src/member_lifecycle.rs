//! A110 agent lifecycle: a member the operator lets manage agents creates,
//! starts, stops, restarts, briefs and removes members of its own room (another
//! room also needs roam). A16 (Victor, 20 Oct) adds the rest of the operator's
//! member actions: the Edit dialog (label, profile, grants), Start fresh and
//! Resume here, adding a lobby session to a room, Close session, and the list
//! of every session. Every action calls the supervisor method behind the
//! operator's desktop command, and the caller's run, generation and grant are
//! checked again inside the critical section where that method commits. Each
//! action posts a notice in the room; a retry with the same request_id and
//! arguments returns the first result.
use super::*;
use sha2::{Digest, Sha256};
use shared_types::{
    MemberAction, MemberActionResult, MemberDirectoryEntry, MemberFollowUp, PermissionProfile, StartMode,
};
use std::collections::VecDeque;

const MANAGE_REFUSAL: &str = "this member may not manage agents (members_list and the member_* tools): ask the operator to tick \"Can manage agents\" in its Edit dialog";
const MANAGE_ROAM_REFUSAL: &str = "managing a member of another room also needs \"Can roam\": ask the operator to tick it in this member's Edit dialog";
const LOBBY_ROAM_REFUSAL: &str = "a session in the lobby is outside your room, so acting on it also needs \"Can roam\": ask the operator to tick it in this member's Edit dialog";
const SELF_REFUSAL: &str = "member tools never act on the calling member itself";
/// Finished results kept for retries, oldest dropped first.
const MEMBER_RECEIPTS_MAX: usize = 1024;
/// Close session waits this long for a stopped member's event stream to
/// drain before deleting it, as the desktop's Close does.
const DELETE_DRAIN_WAIT: Duration = Duration::from_secs(15);

pub(super) fn is_member_request(request: &SidebandRequest) -> bool {
    matches!(request, SidebandRequest::Member { .. } | SidebandRequest::MembersList {})
}

/// Who asks for a member or room action: a pane holding the grants (A110),
/// or (A16) a local process of the same Windows user outside every pane
/// Job, through the operator entry, with the operator's authority and its own
/// attribution in the feed.
#[derive(Clone, Copy)]
pub(super) enum Actor<'a> {
    Pane(&'a PaneCaller),
    Local(&'a LocalOperator),
}

/// The operator entry's caller (A16): how the feed names it, for example
/// "MAINPC Claude PRIM-1 over SSH (local process 4120)".
#[derive(Debug, Clone)]
pub(super) struct LocalOperator {
    pub(super) label: String,
}

/// Receipts of the operator entry share one key: it has no session.
const LOCAL_OPERATOR_RECEIPT_KEY: SessionId = Uuid::nil();

impl Actor<'_> {
    pub(super) fn pane(&self) -> Option<&PaneCaller> {
        match self { Self::Pane(caller) => Some(caller), Self::Local(_) => None }
    }

    /// The caller's own session; the operator entry has none.
    pub(super) fn is(&self, session_id: SessionId) -> bool {
        self.pane().is_some_and(|caller| caller.session_id == session_id)
    }

    /// A pane's current run and manage grant; the operator entry holds the
    /// operator's authority.
    pub(super) fn manage_locked(&self, slots: &SessionRegistry) -> Result<()> {
        match self { Self::Pane(caller) => SupervisorHandle::pane_manage_locked(caller, slots), Self::Local(_) => Ok(()) }
    }

    /// A pane's current run and maintain grant (A108's "Can upgrade PRIM-1",
    /// which A16 also uses for the runtime settings).
    pub(super) fn maintain_locked(&self, slots: &SessionRegistry) -> Result<()> {
        match self { Self::Pane(caller) => SupervisorHandle::pane_maintain_locked(caller, slots), Self::Local(_) => Ok(()) }
    }

    pub(super) fn may_roam_locked(&self, slots: &SessionRegistry) -> bool {
        match self { Self::Pane(caller) => SupervisorHandle::member_may_roam_locked(slots, caller.session_id), Self::Local(_) => true }
    }

    pub(super) fn own_room(&self, rooms: &RoomState) -> Option<RoomId> {
        self.pane().and_then(|caller| rooms.room_by_session.get(&caller.session_id).copied())
    }

    pub(super) fn label_locked(&self, slots: &SessionRegistry) -> String {
        match self { Self::Pane(caller) => SupervisorHandle::member_label_locked(slots, caller), Self::Local(local) => local.label.clone() }
    }

    pub(super) fn sender(&self) -> RoomMessageSender {
        match self {
            Self::Pane(caller) => RoomMessageSender::Session { session_id: caller.session_id },
            Self::Local(local) => RoomMessageSender::LocalProcess { label: local.label.clone() },
        }
    }

    pub(super) fn receipt_key(&self) -> SessionId {
        self.pane().map_or(LOCAL_OPERATOR_RECEIPT_KEY, |caller| caller.session_id)
    }
}

/// The recheck a managing pane's effect runs where it commits: the caller's
/// current run and manage grant, and the room it acts in (its own, or any
/// room when it may also roam). A16: an action on a lobby session also needs
/// roam, wherever it lands.
pub(super) struct ManageCheck<'a> {
    pub(super) caller: Actor<'a>,
    /// None: the target is a session in the lobby (A16).
    room_id: Option<RoomId>,
    lobby_target: bool,
}

impl<'a> ManageCheck<'a> {
    pub(super) fn new(caller: &'a PaneCaller, room_id: RoomId) -> Self {
        Self::of(Actor::Pane(caller), room_id)
    }

    pub(super) fn of(caller: Actor<'a>, room_id: RoomId) -> Self {
        Self { caller, room_id: Some(room_id), lobby_target: false }
    }

    /// A16: a session in the lobby, which is outside the caller's room.
    fn lobby(caller: Actor<'a>) -> Self {
        Self { caller, room_id: None, lobby_target: true }
    }

    /// A16: a lobby session joining `room_id` (member_add).
    fn for_lobby_target(caller: Actor<'a>, room_id: RoomId) -> Self {
        Self { caller, room_id: Some(room_id), lobby_target: true }
    }

    pub(super) fn for_room(caller: Actor<'a>, room_id: Option<RoomId>) -> Self {
        match room_id {
            Some(room_id) => Self::of(caller, room_id),
            None => Self::lobby(caller),
        }
    }
}

impl ManageCheck<'_> {
    /// Run, generation and manage grant, then the room: it still exists and
    /// is the caller's own, or the caller may roam. The lobby needs roam.
    pub(super) fn check(&self, rooms: &RoomState, slots: &SessionRegistry) -> Result<()> {
        self.caller.manage_locked(slots)?;
        let may_roam = self.caller.may_roam_locked(slots);
        if self.lobby_target && !may_roam {
            bail!(LOBBY_ROAM_REFUSAL);
        }
        if let Some(room_id) = self.room_id {
            rooms.get(room_id).ok_or_else(|| anyhow!("room '{room_id}' no longer exists"))?;
            if self.caller.own_room(rooms) != Some(room_id) && !may_roam {
                bail!(MANAGE_ROAM_REFUSAL);
            }
        }
        Ok(())
    }

    /// member_update's recheck (A16): the target is still in that room, and
    /// may be the caller itself (no lifecycle constraint applies to an edit).
    /// The sessions this caller may list (members_list), in tab order: its
    /// own room's members, or every session with roam.
    pub(super) fn listed_order(&self, rooms: &RoomState, slots: &SessionRegistry) -> Vec<SessionId> {
        let everywhere = self.caller.may_roam_locked(slots);
        let own_room = self.caller.own_room(rooms);
        slots.order.iter().copied().filter(|session_id| {
            let room_id = rooms.room_by_session.get(session_id).copied();
            everywhere || (room_id.is_some() && room_id == own_room)
        }).collect()
    }

    pub(super) fn check_editable(&self, rooms: &RoomState, slots: &SessionRegistry, target: SessionId) -> Result<()> {
        if self.caller.is(target) {
            self.check(rooms, slots)?;
            return match self.room_id {
                Some(room_id) if rooms.room_by_session.get(&target) == Some(&room_id) => Ok(()),
                _ => bail!("you are no longer in that room"),
            };
        }
        self.check_target(rooms, slots, target)
    }

    /// Also: the target is another member, still in that room (or still in
    /// the lobby).
    pub(super) fn check_target(&self, rooms: &RoomState, slots: &SessionRegistry, target: SessionId) -> Result<()> {
        self.check(rooms, slots)?;
        if self.caller.is(target) {
            bail!(SELF_REFUSAL);
        }
        match self.room_id {
            Some(room_id) if rooms.room_by_session.get(&target) != Some(&room_id) =>
                bail!("that member is no longer in the room"),
            None if rooms.room_by_session.contains_key(&target) => bail!("that session is no longer in the lobby"),
            _ => Ok(()),
        }
    }
}

/// The recheck where a managing pane's Close session deletes the session
/// (A16), under the rooms and sessions locks of that commit. By then the
/// target is in the lobby, removed a moment ago from `original_room` or found
/// there (None). Without roam the caller must still be in that original room,
/// which must have been its own: the drain wait before the delete may have
/// let the caller move.
pub(super) struct DeleteCheck<'a> {
    pub(super) caller: Actor<'a>,
    pub(super) original_room: Option<RoomId>,
}

impl DeleteCheck<'_> {
    pub(super) fn check(&self, rooms: &RoomState, slots: &SessionRegistry, target: SessionId) -> Result<()> {
        self.caller.manage_locked(slots)?;
        if self.caller.is(target) {
            bail!(SELF_REFUSAL);
        }
        let in_scope = self.original_room.is_some() && self.caller.own_room(rooms) == self.original_room;
        if !in_scope && !self.caller.may_roam_locked(slots) {
            bail!(LOBBY_ROAM_REFUSAL);
        }
        Ok(())
    }
}

/// "roam, maintain, manage", or "none".
pub(super) fn grant_names(grants: MemberPermissions) -> String {
    let names: Vec<&str> = [(grants.roam, "roam"), (grants.maintain, "maintain"), (grants.manage, "manage")]
        .into_iter().filter_map(|(on, name)| on.then_some(name)).collect();
    if names.is_empty() { "none".into() } else { names.join(", ") }
}

fn profile_words(profile: PermissionProfile) -> &'static str {
    match profile {
        PermissionProfile::Unsafe => "bypass permissions",
        PermissionProfile::Normal => "normal permissions",
    }
}

/// A managing pane's brief: it is the sender, `request_id` keys the post,
/// `content` (if any) replaces the room brief.
pub(super) struct BriefBy<'a> {
    pub(super) caller: Actor<'a>,
    pub(super) request_id: String,
    pub(super) content: Option<String>,
}

pub(super) struct MemberInput {
    text: Option<String>,
    keys: Vec<ControlKey>,
    show_text: bool,
}

/// The room's account of a `member_input`: the text's length, or the text
/// itself (at most 200 characters) when the caller marked it not sensitive,
/// and the keys by name. Logging is the caller's explicit choice, never a
/// guess at what is secret.
fn describe_member_input(text: Option<&str>, keys: &[ControlKey], show_text: bool) -> String {
    let keys = keys.iter().map(|key| format!("{key:?}")).collect::<Vec<_>>().join(", ");
    let quoted = text.map(|text| if show_text {
        let shown: String = text.chars().take(200).collect();
        if shown.len() < text.len() { format!("\"{shown}…\"") } else { format!("\"{shown}\"") }
    } else {
        let count = text.chars().count();
        format!("{count} character{}", if count == 1 { "" } else { "s" })
    });
    match (quoted, keys.is_empty()) {
        (Some(quoted), true) => quoted,
        (Some(quoted), false) => format!("{quoted}, then {keys}"),
        (None, _) => format!("pressed {keys}"),
    }
}

/// SHA-256 of a request's canonical arguments, which a retry must repeat.
/// The receipts compare it; member_create also stores it with the session it
/// creates, so a retry after a PRIM-1 restart is compared the same way.
fn member_action_fingerprint(action: &MemberAction) -> String {
    let canonical = match action {
        // A16 fields join the canonical form only when set, so a create or
        // start recorded by an A110 build keeps its fingerprint.
        MemberAction::Create { driver, label, permission_profile, brief, brief_text, start, room_id, grants,
            codex_transport } => {
            let mut canonical = vec![serde_json::json!("create"), serde_json::json!(driver), serde_json::json!(label),
                serde_json::json!(permission_profile), serde_json::json!(brief), serde_json::json!(brief_text),
                serde_json::json!(start), serde_json::json!(room_id)];
            if let Some(grants) = grants { canonical.push(serde_json::json!(grants)); }
            // v3 W4: joins only when set, as the A16 fields.
            if !codex_transport.is_pty() { canonical.push(serde_json::json!(["codex_transport", codex_transport])); }
            serde_json::Value::Array(canonical)
        }
        MemberAction::Start { member, mode, launch_choices, search }
            if mode.is_resume() && launch_choices.is_none() && !search => serde_json::json!(["start", member]),
        MemberAction::Start { member, mode, launch_choices, search } =>
            serde_json::json!(["start", member, mode, launch_choices, search]),
        MemberAction::Stop { member } | MemberAction::Restart { member } | MemberAction::Remove { member }
        | MemberAction::Delete { member } => serde_json::json!([action.name(), member]),
        MemberAction::Brief { member, content } => serde_json::json!(["brief", member, content]),
        MemberAction::Input { member, text, keys, show_text } => serde_json::json!(["input", member, text, keys, show_text]),
        MemberAction::Update { member, label, permission_profile, grants, working_directory, position } => {
            // The working directory and position join only when set.
            let mut canonical = vec![serde_json::json!("update"), serde_json::json!(member), serde_json::json!(label),
                serde_json::json!(permission_profile), serde_json::json!(grants)];
            if working_directory.is_some() || position.is_some() {
                canonical.extend([serde_json::json!(working_directory), serde_json::json!(position)]);
            }
            serde_json::Value::Array(canonical)
        }
        MemberAction::Add { member, room_id } => serde_json::json!(["add", member, room_id]),
    };
    format!("{:x}", Sha256::digest(canonical.to_string().as_bytes()))
}

/// Member (A110) and room (A16) requests by caller session and request_id:
/// running (no result yet) or finished. A refused request leaves nothing
/// behind.
pub(super) struct ActionReceipts<T> {
    entries: VecDeque<ActionReceipt<T>>,
}

impl<T> Default for ActionReceipts<T> {
    fn default() -> Self {
        Self { entries: VecDeque::new() }
    }
}

pub(super) type MemberReceipts = ActionReceipts<MemberActionResult>;

struct ActionReceipt<T> {
    caller: SessionId,
    request_id: String,
    fingerprint: String,
    /// None while the first attempt runs. Its result, or the error of an
    /// attempt that failed after an effect (`PartialEffect`): either is the
    /// request's outcome, replayed to every retry.
    outcome: Option<std::result::Result<T, String>>,
}

pub(super) enum ReceiptClaim<T> {
    Fresh,
    Replay(T),
}

/// An action that failed after it changed something (A16, Reviewer): its
/// error is the request's outcome, and a retry with its request_id replays
/// it instead of applying anything more. An error before any effect leaves
/// nothing behind, so its request_id may be used again.
#[derive(Debug)]
pub(super) struct PartialEffect(pub(super) String);

impl std::fmt::Display for PartialEffect {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for PartialEffect {}

impl<T: Clone> ActionReceipts<T> {
    pub(super) fn claim(&mut self, caller: SessionId, request_id: &str, fingerprint: &str) -> Result<ReceiptClaim<T>> {
        if let Some(entry) = self.entries.iter().find(|entry| entry.caller == caller && entry.request_id == request_id) {
            if entry.fingerprint != fingerprint {
                bail!("request_id {request_id} was already used for a different request; use a fresh UUID");
            }
            return match &entry.outcome {
                Some(Ok(result)) => Ok(ReceiptClaim::Replay(result.clone())),
                Some(Err(error)) => Err(anyhow!(
                    "{error} (the first outcome of request_id {request_id}, replayed; use a fresh UUID for a new action)")),
                None => Err(anyhow!("the first request with request_id {request_id} is still running; retry in a moment")),
            };
        }
        while self.entries.len() >= MEMBER_RECEIPTS_MAX {
            let oldest = self.entries.iter().position(|entry| entry.outcome.is_some())
                .ok_or_else(|| anyhow!("too many requests are running; retry in a moment"))?;
            self.entries.remove(oldest);
        }
        self.entries.push_back(ActionReceipt { caller, request_id: request_id.into(), fingerprint: fingerprint.into(),
            outcome: None });
        Ok(ReceiptClaim::Fresh)
    }
}

/// Ends a running claim, also on panic: with its result, with the error of
/// a partial effect, or (an error before any effect, or a panic) removed.
pub(super) struct ReceiptGuard<'a, T> {
    receipts: &'a Mutex<ActionReceipts<T>>,
    caller: SessionId,
    request_id: &'a str,
    finished: bool,
}

impl<'a, T: Clone> ReceiptGuard<'a, T> {
    pub(super) fn new(receipts: &'a Mutex<ActionReceipts<T>>, caller: SessionId, request_id: &'a str) -> Self {
        Self { receipts, caller, request_id, finished: false }
    }

    /// The outcome of `outcome`: a result or a partial effect is kept.
    pub(super) fn settle(&mut self, outcome: &Result<T>) {
        match outcome {
            Ok(result) => self.record(Ok(result.clone())),
            Err(error) if error.is::<PartialEffect>() => self.record(Err(format!("{error:#}"))),
            Err(_) => {}
        }
    }

    fn record(&mut self, outcome: std::result::Result<T, String>) {
        let mut receipts = self.receipts.lock();
        if let Some(entry) = receipts.entries.iter_mut()
            .find(|entry| entry.caller == self.caller && entry.request_id == self.request_id) {
            entry.outcome = Some(outcome);
        }
        self.finished = true;
    }
}

impl<T> Drop for ReceiptGuard<'_, T> {
    fn drop(&mut self) {
        if !self.finished {
            self.receipts.lock().entries
                .retain(|entry| !(entry.caller == self.caller && entry.request_id == self.request_id));
        }
    }
}

fn unconfirmed_follow_ups(what: &str, tools: &str) -> String {
    format!("the member was created, but its {what} may not have happened; check it and use {tools}")
}

fn harness_name(driver: DriverKind) -> &'static str {
    match driver {
        DriverKind::Claude => "Claude",
        DriverKind::Codex => "Codex",
        DriverKind::Grok => "Grok",
        DriverKind::Prime => "Prime",
        DriverKind::GenericTerminal => "Terminal",
    }
}

/// Every act is recorded in a room feed, an act on a session in the lobby in
/// the caller's own room (A16): a caller in the lobby and the operator entry
/// have none.
fn lobby_no_record(caller: Actor<'_>) -> anyhow::Error {
    match caller {
        Actor::Pane(_) => anyhow!("you and that session are both in the lobby, so no room would record it; join a room first (room_join)"),
        Actor::Local(_) => anyhow!("the operator entry has no room of its own to record this in; put the session into a room first (member add <session id> --room <room id>)"),
    }
}

/// A resolved target: its room, session and label.
struct ManagedMember {
    /// The room that records the action: the target's, or for a session in
    /// the lobby the caller's own (A16).
    room_id: RoomId,
    /// The target's room; None for a session in the lobby (A16), which
    /// every recheck then requires to still be there, with roam.
    scope: Option<RoomId>,
    session_id: SessionId,
    label: String,
    caller_label: String,
}

/// A16: a target that may also be in the lobby (member_add, member_delete,
/// conversation_attach).
pub(super) struct AnyMember {
    pub(super) room_id: Option<RoomId>,
    pub(super) session_id: SessionId,
    pub(super) label: String,
    pub(super) caller_label: String,
}

impl SupervisorHandle {
    /// The caller's current run and its manage grant.
    pub(super) fn pane_manage_locked(caller: &PaneCaller, slots: &SessionRegistry) -> Result<()> {
        Self::validate_pane_caller_locked(caller, slots)?;
        if Self::member_grants_locked(slots, caller.session_id).manage { Ok(()) } else { Err(anyhow!(MANAGE_REFUSAL)) }
    }

    /// Runs on a blocking worker: actions write catalogs and room journals
    /// and wait for a stopped process.
    pub(super) fn apply_member_request(&self, caller: &PaneCaller, request: SidebandRequest) -> SidebandResponse {
        if let Err(error) = self.authorize_sideband_request(caller, &request) {
            return Self::rejected_sideband_response(error.to_string());
        }
        #[cfg(test)]
        self.run_sideband_after_initial_authorization_for_tests();
        if matches!(request, SidebandRequest::MembersList {}) {
            return match self.members_list(Actor::Pane(caller)) {
                Ok(members) => SidebandResponse { ok: true, message: format!("{} sessions", members.len()),
                    timed_out: false, payload: Some(SidebandResponsePayload::Members { members }), request_id: None },
                Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
            };
        }
        let SidebandRequest::Member { request_id, action } = request else {
            return Self::rejected_sideband_response("not a member request");
        };
        let Ok(request_id) = Uuid::parse_str(request_id.trim()).map(|id| id.to_string()) else {
            return Self::rejected_sideband_response("request_id must be a UUID: a fresh one per action, the same one only to retry it");
        };
        let outcome = self.claim_member_action(Actor::Pane(caller), &request_id, action);
        match outcome {
            Ok(result) => SidebandResponse {
                ok: true,
                message: format!("{} {}: {}{}", result.op, result.member.label,
                    if result.replayed { "replayed the first result" } else { "done" },
                    if result.warnings.is_empty() { String::new() } else { format!("; {}", result.warnings.join("; ")) }),
                timed_out: false,
                payload: Some(SidebandResponsePayload::Member { result }),
                request_id: None,
            },
            Err(error) => Self::rejected_sideband_response(format!("{error:#}")),
        }
    }

    /// One member action under its request_id: a retry returns the first
    /// result and repeats nothing. Shared by panes and the operator entry.
    pub(super) fn claim_member_action(&self, caller: Actor<'_>, request_id: &str, action: MemberAction)
        -> Result<MemberActionResult> {
        let fingerprint = member_action_fingerprint(&action);
        let claim = self.inner.member_receipts.lock().claim(caller.receipt_key(), request_id, &fingerprint)?;
        match claim {
            ReceiptClaim::Replay(result) => Ok(MemberActionResult { replayed: true, ..result }),
            ReceiptClaim::Fresh => {
                let mut guard = ReceiptGuard::new(&self.inner.member_receipts, caller.receipt_key(), request_id);
                let outcome = self.run_member_action(caller, request_id, &fingerprint, action);
                guard.settle(&outcome);
                outcome
            }
        }
    }

    fn run_member_action(&self, caller: Actor<'_>, request_id: &str, fingerprint: &str, action: MemberAction)
        -> Result<MemberActionResult> {
        match action {
            MemberAction::Create { driver, label, permission_profile, brief, brief_text, start, room_id, grants,
                codex_transport } =>
                self.member_create(caller, request_id, fingerprint,
                    NewMember { driver, label, permission_profile, brief, brief_text, start, room_id, grants, codex_transport }),
            MemberAction::Start { member, mode, launch_choices, search } =>
                self.member_start(caller, request_id, &member, StartRequest { mode, launch_choices, search }),
            MemberAction::Stop { member } => self.member_stop(caller, request_id, &member),
            MemberAction::Restart { member } => self.member_restart(caller, request_id, &member),
            MemberAction::Brief { member, content } => self.member_brief(caller, request_id, &member, content),
            MemberAction::Input { member, text, keys, show_text } =>
                self.member_input(caller, request_id, &member, MemberInput { text, keys, show_text }),
            MemberAction::Remove { member } => self.member_remove(caller, request_id, &member),
            MemberAction::Update { member, label, permission_profile, grants, working_directory, position } =>
                self.member_update(caller, request_id, &member,
                    MemberEdit { label, permission_profile, grants, working_directory, position }),
            MemberAction::Add { member, room_id } => self.member_add(caller, request_id, &member, room_id),
            MemberAction::Delete { member } => self.member_delete(caller, request_id, &member),
        }
    }

    /// The sessions the caller may act on, as the operator's tab bar shows
    /// them (A16): its own room's members, and with roam every session, the
    /// lobby included.
    pub(super) fn members_list(&self, caller: Actor<'_>) -> Result<Vec<MemberDirectoryEntry>> {
        self.ensure_active()?;
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        caller.manage_locked(&slots)?;
        let everywhere = caller.may_roam_locked(&slots);
        let own_room = caller.own_room(&rooms);
        Ok(slots.ordered_slots().into_iter().filter_map(|slot| {
            let room_id = rooms.room_by_session.get(&slot.session_id).copied();
            if !everywhere && (room_id.is_none() || room_id != own_room) {
                return None;
            }
            let room = room_id.and_then(|room_id| rooms.get(room_id));
            Some(MemberDirectoryEntry { room_id, room_label: room.map(|room| room.definition.label.clone()),
                session: slot.snapshot() })
        }).collect())
    }

    /// `member` is a session id, or a label unique in the caller's room;
    /// anything else is refused with that room's members listed. Needs the
    /// manage grant; the scope is checked by the caller of this lookup.
    fn find_member_locked(rooms: &RoomState, slots: &SessionRegistry, caller: Actor<'_>, member: &str)
        -> Result<SessionId> {
        caller.manage_locked(slots)?;
        let own_room = caller.own_room(rooms).and_then(|room_id| rooms.get(room_id));
        let candidates = || own_room.map(|room| room.definition.member_ids.iter()
            .filter_map(|id| slots.get_by_id(*id).map(|slot| format!("{} ({id})", slot.definition.label)))
            .collect::<Vec<_>>().join(", ")).unwrap_or_else(|| "none: you are in the lobby".into());
        let wanted = member.trim();
        if wanted.is_empty() {
            bail!("name the member by its label or session id; members: {}", candidates());
        }
        Ok(match Uuid::parse_str(wanted) {
            Ok(id) if slots.get_by_id(id).is_some() => id,
            Ok(id) => bail!("no member has session id {id}; members: {}", candidates()),
            Err(_) => {
                let room = own_room.ok_or_else(|| anyhow!(
                    "you are in the lobby: name the member by its session id (members_list shows them)"))?;
                let named: Vec<SessionId> = room.definition.member_ids.iter().copied()
                    .filter(|id| slots.get_by_id(*id).is_some_and(|slot| slot.definition.label.trim().eq_ignore_ascii_case(wanted)))
                    .collect();
                match named.as_slice() {
                    [id] => *id,
                    [] => bail!("no member of room \"{}\" is labelled '{wanted}'; members: {}; a session in the lobby or another room is named by its session id (members_list)",
                        room.definition.label, candidates()),
                    _ => bail!("'{wanted}' names more than one member of room \"{}\"; use a session id; members: {}",
                        room.definition.label, candidates()),
                }
            }
        })
    }

    /// A room member (A110's scope): the caller's own room, or another with
    /// roam. The grant is checked now and again where each effect commits.
    fn resolve_managed_member(&self, caller: Actor<'_>, member: &str) -> Result<ManagedMember> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let target = Self::find_member_locked(&rooms, &slots, caller, member)?;
        let room_id = *rooms.room_by_session.get(&target)
            .ok_or_else(|| anyhow!("that session is in the lobby; member_add puts it into a room first"))?;
        ManageCheck::of(caller, room_id).check_target(&rooms, &slots, target)?;
        Ok(ManagedMember {
            room_id,
            scope: Some(room_id),
            session_id: target,
            label: slots.get_by_id(target).map(|slot| slot.definition.label.clone()).unwrap_or_default(),
            caller_label: caller.label_locked(&slots),
        })
    }

    /// A16: as resolve_managed_member, or a session in the lobby (start,
    /// stop, restart), which also needs roam; the caller's own room records
    /// what is done to it.
    fn resolve_managed_or_lobby_member(&self, caller: Actor<'_>, member: &str) -> Result<ManagedMember> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let target = Self::find_member_locked(&rooms, &slots, caller, member)?;
        let scope = rooms.room_by_session.get(&target).copied();
        ManageCheck::for_room(caller, scope).check_target(&rooms, &slots, target)?;
        Ok(ManagedMember {
            room_id: scope.or_else(|| caller.own_room(&rooms)).ok_or_else(|| lobby_no_record(caller))?,
            scope,
            session_id: target,
            label: slots.get_by_id(target).map(|slot| slot.definition.label.clone()).unwrap_or_default(),
            caller_label: caller.label_locked(&slots),
        })
    }

    /// A16: a room member as above, or a session in the lobby, which also
    /// needs roam (it is outside the caller's room).
    pub(super) fn resolve_any_member(&self, caller: Actor<'_>, member: &str) -> Result<AnyMember> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let target = Self::find_member_locked(&rooms, &slots, caller, member)?;
        let room_id = rooms.room_by_session.get(&target).copied();
        ManageCheck::for_room(caller, room_id).check_target(&rooms, &slots, target)?;
        Ok(AnyMember {
            room_id,
            session_id: target,
            label: slots.get_by_id(target).map(|slot| slot.definition.label.clone()).unwrap_or_default(),
            caller_label: caller.label_locked(&slots),
        })
    }

    fn member_snapshot(&self, session_id: SessionId) -> Result<SessionSnapshot> {
        self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.snapshot())
            .ok_or_else(|| anyhow!("member {session_id} no longer exists"))
    }

    /// A stored post that wakes no one, keyed by the request so a retry
    /// never repeats it. The action already happened, so a refused notice is
    /// reported rather than undoing it.
    fn post_member_notice(&self, room_id: RoomId, caller: Actor<'_>, request_id: &str, content: String,
        warnings: &mut Vec<String>) {
        self.post_member_notice_keyed(room_id, caller, &format!("{request_id}.notice"), content, warnings);
    }

    fn post_member_notice_keyed(&self, room_id: RoomId, caller: Actor<'_>, key: &str, content: String,
        warnings: &mut Vec<String>) {
        let sender = caller.sender();
        if let Err(error) = self.post_room_message_as(room_id, sender, content.clone(), Some(key.into())) {
            warnings.push(format!("notice \"{content}\" was not stored: {error}"));
        }
    }

    fn member_result(&self, request_id: &str, op: &str, room_id: RoomId, session_id: SessionId,
        brief: Option<RoomDeliveryResult>, warnings: Vec<String>) -> Result<MemberActionResult> {
        Ok(MemberActionResult { request_id: request_id.into(), replayed: false, op: op.into(), room_id,
            member: self.member_snapshot(session_id)?, brief, incomplete: Vec::new(), warnings })
    }

    #[cfg(test)]
    fn run_member_before_effect_for_tests(&self) {
        let hook = self.inner.member_before_effect.lock().take();
        if let Some(hook) = hook { hook(); }
    }

    fn member_create(&self, caller: Actor<'_>, request_id: &str, fingerprint: &str, new: NewMember)
        -> Result<MemberActionResult> {
        let NewMember { driver, label, permission_profile, brief, brief_text, start, room_id, grants, codex_transport } = new;
        if !matches!(driver, DriverKind::Claude | DriverKind::Codex | DriverKind::Grok) {
            bail!("member_create adds Claude, Codex or Grok members only");
        }
        validate_session_label(&label)?;
        if brief_text.is_some() && !brief {
            bail!("brief_text is sent only with brief: true");
        }
        if let Some(text) = &brief_text {
            if text.trim().is_empty() {
                bail!("brief_text must contain text; omit it to send the room brief");
            }
            validate_message_body(text)?;
        }
        let creation = MemberCreation { session_id: caller.receipt_key(), request_id: request_id.into(),
            fingerprint: fingerprint.into() };
        // After a PRIM-1 restart the receipts are gone; the session row keeps
        // the request that created it and its arguments' fingerprint.
        let existing = self.inner.slots.lock().by_id.values()
            .find_map(|slot| slot.created_by.as_ref().filter(|earlier| earlier.same_request(&creation))
                .map(|earlier| (slot.session_id, earlier.fingerprint == creation.fingerprint)));
        if let Some((session_id, same_arguments)) = existing {
            if !same_arguments {
                bail!("request_id {request_id} was used for a different create; use a fresh UUID");
            }
            return self.replayed_create(caller, request_id, session_id, start, brief);
        }
        let (room_id, caller_label) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            caller.manage_locked(&slots)?;
            let room_id = match room_id {
                Some(room_id) => room_id,
                None => caller.own_room(&rooms).ok_or_else(|| anyhow!(
                    "you are in the lobby: give member_create a room_id (another room also needs Can roam)"))?,
            };
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("unknown room id '{room_id}'; rooms_list names every room"))?;
            ManageCheck::of(caller, room_id).check(&rooms, &slots)?;
            // Labels address members: keep them unique in the room.
            if room.definition.member_ids.iter().any(|id| slots.get_by_id(*id)
                .is_some_and(|slot| slot.definition.label.trim().eq_ignore_ascii_case(label.trim()))) {
                bail!("room \"{}\" already has a member labelled '{label}'; choose another label", room.definition.label);
            }
            (room_id, caller.label_locked(&slots))
        };
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::of(caller, room_id);
        let created = self.create_session_as(shared_types::CreateSessionRequest { permissions: grants,
            codex_transport,
            label: Some(label.clone()),
            driver,
            permission_profile,
            linux_working_directory: None,
            room_id: Some(room_id),
            // The brief, if asked for, arrives once through the room delivery
            // below, never also as a launch prompt.
            launch_choices: Some(SessionLaunchChoices { brief: SessionBriefChoice::None {},
                ..session::default_launch_choices(driver) }),
        }, Some((&check, &creation)))?;
        let mut warnings = Vec::new();
        let mut incomplete = Vec::new();
        let mut started = false;
        if start {
            match self.start_session_checked(created.session_id, false, false, None, Some(&check)) {
                Ok(_) => started = true,
                Err(error) => {
                    warnings.push(format!("created but not started: {error:#}"));
                    incomplete.push(MemberFollowUp::Start);
                }
            }
        }
        let brief = if brief {
            let by = BriefBy { caller, request_id: format!("{request_id}.brief"), content: brief_text };
            match self.brief_room_member_as(BriefRoomMemberRequest { room_id, session_id: created.session_id }, Some(by)) {
                Ok(result) => Some(result),
                Err(error) => {
                    warnings.push(format!("created but not briefed: {error:#}"));
                    incomplete.push(MemberFollowUp::Brief);
                    None
                }
            }
        } else {
            None
        };
        let mut details = vec![harness_name(driver).to_string(), profile_words(permission_profile).to_string()];
        if let Some(grants) = grants.filter(|grants| !grants.is_default()) {
            details.push(format!("grants {}", grant_names(grants)));
        }
        if !started {
            details.push("not started".into());
        }
        if brief.is_some() {
            details.push("briefed".into());
        }
        self.post_member_notice(room_id, caller, request_id,
            format!("{caller_label} added {label} ({})", details.join(", ")), &mut warnings);
        let result = self.member_result(request_id, "create", room_id, created.session_id, brief, warnings)?;
        Ok(MemberActionResult { incomplete, ..result })
    }

    /// The member this request already created: no second one, no second
    /// brief. Its first result is gone (a PRIM-1 restart, or the first
    /// request did not finish), so a requested start or brief counts as done
    /// only when it can be seen: the member running, the brief's delivery
    /// retained. Any other is named in `incomplete`, never replayed as done.
    fn replayed_create(&self, caller: Actor<'_>, request_id: &str, session_id: SessionId, start: bool, brief: bool)
        -> Result<MemberActionResult> {
        let rooms = self.inner.rooms.lock();
        let label = self.inner.slots.lock().get_by_id(session_id).map(|slot| slot.definition.label.clone()).unwrap_or_default();
        let room_id = *rooms.room_by_session.get(&session_id).ok_or_else(|| anyhow!(
            "this request_id already created \"{label}\" ({session_id}); it is no longer in a room"))?;
        let delivery = rooms.get(room_id).and_then(|room| room.retained_delivery(
            &caller.sender(), &format!("{request_id}.brief")));
        drop(rooms);
        let result = self.member_result(request_id, "create", room_id, session_id, delivery, Vec::new())?;
        let mut incomplete = Vec::new();
        if start && !result.member.running {
            incomplete.push(MemberFollowUp::Start);
        }
        if brief && result.brief.is_none() {
            incomplete.push(MemberFollowUp::Brief);
        }
        let warnings = match incomplete.as_slice() {
            [] => Vec::new(),
            [MemberFollowUp::Start] => vec![unconfirmed_follow_ups("start", "member_start")],
            [MemberFollowUp::Brief] => vec![unconfirmed_follow_ups("brief", "member_brief")],
            _ => vec![unconfirmed_follow_ups("start and brief", "member_start / member_brief")],
        };
        Ok(MemberActionResult { replayed: true, incomplete, warnings, ..result })
    }

    /// Resume (the operator's Launch), fresh (Start fresh session: a running
    /// member is stopped first, as the desktop asks to) or resume_here (the
    /// copied conversation's hold is released, then it launches). `search`
    /// is the pane's "Search for it" after a harness was not found on PATH.
    fn member_start(&self, caller: Actor<'_>, request_id: &str, member: &str, start: StartRequest)
        -> Result<MemberActionResult> {
        let StartRequest { mode, launch_choices, search } = start;
        let target = if mode == StartMode::Resume && launch_choices.is_none() {
            // Launching a running member changes nothing, so the caller may
            // name itself; fresh and resume_here would end the run that asked.
            let target = self.resolve_editable_member(caller, member)?;
            if caller.is(target.session_id) {
                return self.member_result(request_id, "start", target.room_id, target.session_id, None,
                    vec!["you are running; nothing changed".into()]);
            }
            target
        } else {
            self.resolve_managed_or_lobby_member(caller, member)?
        };
        let before = self.member_snapshot(target.session_id)?;
        if let Some(choices) = &launch_choices {
            // Before any effect: a fresh start never stops a member for
            // choices its start would then refuse.
            session::validate_launch_choices(before.driver, choices)?;
        }
        if launch_choices.is_some() && before.running && mode != StartMode::Fresh {
            bail!("{} is running: launch choices change only for a stopped member (stop it, or use mode \"fresh\")",
                target.label);
        }
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::for_room(caller, target.scope);
        let mut warnings = Vec::new();
        let mut stopped = false;
        let mut released = false;
        match mode {
            StartMode::Resume => {}
            StartMode::Fresh => {
                let (ticket, _) = self.begin_stop_session_checked(target.session_id, Some(&check))?;
                stopped = ticket.expected.is_some();
                let after_stop = self.finish_stop_session(ticket)?;
                if after_stop.running {
                    let message = format!("{} did not stop ({:?}); start it fresh once it has stopped", target.label,
                        after_stop.lifecycle_state);
                    return Err(if stopped { PartialEffect(message).into() } else { anyhow!(message) });
                }
            }
            StartMode::ResumeHere => {
                self.resume_held_conversation_checked(target.session_id, Some(&check))?;
                released = true;
            }
        }
        let started = match self.start_session_checked(target.session_id, mode == StartMode::Fresh, search, launch_choices,
            Some(&check)) {
            Ok(started) => started,
            Err(error) => {
                // Its own key: these record what happened before the launch failed.
                if stopped {
                    self.post_member_notice_keyed(target.room_id, caller, &format!("{request_id}.stopped"),
                        format!("{} stopped {}", target.caller_label, target.label), &mut warnings);
                }
                if released {
                    self.post_member_notice_keyed(target.room_id, caller, &format!("{request_id}.released"),
                        format!("{} released the copied conversation of {} for Resume here", target.caller_label, target.label),
                        &mut warnings);
                }
                let before_failure = match (stopped, released) {
                    (true, _) => " was stopped but",
                    (_, true) => "'s copied conversation was released but it",
                    _ => "",
                };
                let message = format!("{}{before_failure} could not start: {error:#}", target.label);
                return Err(if stopped || released { PartialEffect(message).into() } else { anyhow!(message) });
            }
        };
        if mode == StartMode::Resume && started.generation == before.generation {
            warnings.push(format!("{} was already running; nothing changed", target.label));
        } else {
            let what = match (mode, stopped) {
                (StartMode::Resume, _) => "started",
                (StartMode::Fresh, true) => "stopped and started fresh",
                (StartMode::Fresh, false) => "started fresh",
                (StartMode::ResumeHere, _) => "resumed here",
            };
            self.post_member_notice(target.room_id, caller, request_id,
                format!("{} {what} {}", target.caller_label, target.label), &mut warnings);
        }
        self.member_result(request_id, "start", target.room_id, target.session_id, None, warnings)
    }

    fn member_stop(&self, caller: Actor<'_>, request_id: &str, member: &str) -> Result<MemberActionResult> {
        let target = self.resolve_managed_or_lobby_member(caller, member)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::for_room(caller, target.scope);
        let (ticket, _) = self.begin_stop_session_checked(target.session_id, Some(&check))?;
        let declared = ticket.expected.is_some();
        let after_stop = self.finish_stop_session(ticket)?;
        let mut warnings = Vec::new();
        // A16: a stop returns Ok also when termination was not proved; the
        // notice says which it was.
        let proved = !after_stop.running && after_stop.lifecycle_state == LifecycleState::Closed;
        if declared && proved {
            self.post_member_notice(target.room_id, caller, request_id,
                format!("{} stopped {}", target.caller_label, target.label), &mut warnings);
        } else if declared {
            warnings.push(format!("the stop of {} was not proved; it reads {:?}", target.label, after_stop.lifecycle_state));
            self.post_member_notice(target.room_id, caller, request_id,
                format!("{} asked {} to stop; the stop was not proved ({:?})", target.caller_label, target.label,
                    after_stop.lifecycle_state), &mut warnings);
        } else {
            warnings.push(format!("{} was not running; nothing changed", target.label));
        }
        self.member_result(request_id, "stop", target.room_id, target.session_id, None, warnings)
    }

    fn member_restart(&self, caller: Actor<'_>, request_id: &str, member: &str) -> Result<MemberActionResult> {
        let target = self.resolve_managed_or_lobby_member(caller, member)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::for_room(caller, target.scope);
        self.restart_session_checked(target.session_id, Some(&check))?;
        let mut warnings = Vec::new();
        self.post_member_notice(target.room_id, caller, request_id,
            format!("{} restarted {}", target.caller_label, target.label), &mut warnings);
        self.member_result(request_id, "restart", target.room_id, target.session_id, None, warnings)
    }

    fn member_brief(&self, caller: Actor<'_>, request_id: &str, member: &str, content: Option<String>) -> Result<MemberActionResult> {
        if let Some(content) = &content {
            if content.trim().is_empty() {
                bail!("content must contain text; omit it to send the room brief");
            }
            validate_message_body(content)?;
        }
        let target = self.resolve_managed_member(caller, member)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let by = BriefBy { caller, request_id: format!("{request_id}.brief"), content };
        let brief = self.brief_room_member_as(
            BriefRoomMemberRequest { room_id: target.room_id, session_id: target.session_id }, Some(by))?;
        let mut warnings = Vec::new();
        self.post_member_notice(target.room_id, caller, request_id,
            format!("{} briefed {}", target.caller_label, target.label), &mut warnings);
        self.member_result(request_id, "brief", target.room_id, target.session_id, Some(brief), warnings)
    }

    /// A16: types into a running member's terminal as the operator does (raw
    /// input), its grant checked again under the locks the write target is
    /// read with. The room's notice holds metadata, the text only when the
    /// caller marked it not sensitive; the result carries no terminal output,
    /// and nothing logs the text.
    fn member_input(&self, caller: Actor<'_>, request_id: &str, member: &str, input: MemberInput)
        -> Result<MemberActionResult> {
        let MemberInput { text, keys, show_text } = input;
        let text = text.filter(|text| !text.is_empty());
        if text.is_none() && keys.is_empty() {
            bail!("give text, keys or both");
        }
        if let Some(text) = &text {
            if text.len() > shared_types::MEMBER_INPUT_TEXT_MAX_BYTES {
                bail!("text is {} bytes; at most {}", text.len(), shared_types::MEMBER_INPUT_TEXT_MAX_BYTES);
            }
            if text.chars().any(char::is_control) {
                bail!("text is typed literally and may hold no control characters (newline, tab, escape): send them as keys");
            }
        }
        if keys.len() > shared_types::MEMBER_INPUT_KEYS_MAX {
            bail!("at most {} keys", shared_types::MEMBER_INPUT_KEYS_MAX);
        }
        let target = self.resolve_managed_member(caller, member)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::of(caller, target.room_id);
        let write_target = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            check.check_target(&rooms, &slots, target.session_id)?;
            let slot = slots.get_by_id(target.session_id).ok_or_else(|| anyhow!("{} no longer exists", target.label))?;
            run_write_target_from_slot(&slot.definition.alias, slot)
                .with_context(|| format!("{} is not running, so nothing was typed", target.label))?
        };
        let typed: String = text.iter().map(String::as_str).chain(keys.iter().map(|key| control_key_sequence(*key))).collect();
        // The grant and the target are checked again once the run's writer
        // ticket is held, at the input boundary itself (the Reviewer's hold).
        // From the moment that check passes, input may have reached the
        // terminal: no later failure may be retried as if nothing happened.
        let admitted = std::cell::Cell::new(false);
        let admit = || -> Result<()> {
            check.check_target(&self.inner.rooms.lock(), &self.inner.slots.lock(), target.session_id)?;
            admitted.set(true);
            Ok(())
        };
        let written = self.write_run_input_checked(&write_target, None, &typed, RunInputSafety::Raw, None, Some(&admit));
        let described = describe_member_input(text.as_deref(), &keys, show_text);
        let mut warnings = Vec::new();
        match written {
            Err(error) if !admitted.get() => return Err(error),
            Err(error) => {
                self.post_member_notice(target.room_id, caller, request_id,
                    format!("{} typed into {}: {described}, but the write did not complete (request {request_id})",
                        target.caller_label, target.label), &mut warnings);
                return Err(PartialEffect(format!("{} may have received part of that input: {error:#}", target.label)).into());
            }
            Ok(_) => {}
        }
        self.post_member_notice(target.room_id, caller, request_id,
            format!("{} typed into {}: {described} (request {request_id})", target.caller_label, target.label),
            &mut warnings);
        self.member_result(request_id, "input", target.room_id, target.session_id, None, warnings)
            .map_err(|error| PartialEffect(format!("the input was typed into {}, then: {error:#}", target.label)).into())
    }

    /// Stop first (when it runs), then the operator's removal. The session
    /// stays in the lobby; deleting it is the operator's.
    fn member_remove(&self, caller: Actor<'_>, request_id: &str, member: &str) -> Result<MemberActionResult> {
        let target = self.resolve_managed_member(caller, member)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::of(caller, target.room_id);
        let (ticket, _) = self.begin_stop_session_checked(target.session_id, Some(&check))?;
        let stopped = ticket.expected.is_some();
        let after_stop = self.finish_stop_session(ticket)?;
        let mut warnings = Vec::new();
        if after_stop.running || after_stop.lifecycle_state != LifecycleState::Closed {
            bail!("{} did not stop ({:?}); it stays in the room", target.label, after_stop.lifecycle_state);
        }
        if let Err(error) = self.remove_room_member_checked(
            RemoveRoomMemberRequest { room_id: target.room_id, session_id: target.session_id }, Some(&check)) {
            if stopped {
                // Its own key: a retry that then removes posts the full notice.
                self.post_member_notice_keyed(target.room_id, caller, &format!("{request_id}.stopped"),
                    format!("{} stopped {}", target.caller_label, target.label), &mut warnings);
            }
            let message = format!("{}{} could not be removed: {error:#}", target.label, if stopped { " was stopped but" } else { "" });
            return Err(if stopped { PartialEffect(message).into() } else { anyhow!(message) });
        }
        let notice = if stopped {
            format!("{} stopped and removed {}", target.caller_label, target.label)
        } else {
            format!("{} removed {}", target.caller_label, target.label)
        };
        self.post_member_notice(target.room_id, caller, request_id, notice, &mut warnings);
        self.member_result(request_id, "remove", target.room_id, target.session_id, None, warnings)
    }

    /// The Edit dialog (A16): each given field through the operator's own
    /// setter, in the dialog's order, each rechecked where it commits. A
    /// member may edit itself: no lifecycle constraint applies, and there is
    /// no ceiling on the grants it gives. Fields already as asked change
    /// nothing; a later failure keeps the earlier changes, as in the dialog.
    fn member_update(&self, caller: Actor<'_>, request_id: &str, member: &str, edit: MemberEdit)
        -> Result<MemberActionResult> {
        let MemberEdit { label, permission_profile, grants, working_directory, position } = edit;
        if label.is_none() && permission_profile.is_none() && grants.is_none() && working_directory.is_none()
            && position.is_none() {
            bail!("member_update needs label, permission_profile, grants, working_directory or position");
        }
        if let Some(label) = &label {
            validate_session_label(label)?;
        }
        let target = self.resolve_editable_member(caller, member)?;
        let before = self.member_snapshot(target.session_id)?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::for_room(caller, target.scope);
        let mut changes = Vec::new();
        let mut warnings = Vec::new();
        let outcome = (|| -> Result<()> {
            if let Some(label) = label.filter(|label| *label != before.label) {
                self.rename_session_checked(target.session_id, &label, Some(&check))?;
                changes.push(format!("renamed to {label}"));
            }
            if let Some(profile) = permission_profile.filter(|profile| *profile != before.permission_profile) {
                self.set_permission_profile_checked(target.session_id, profile, Some(&check))?;
                changes.push(profile_words(profile).to_string());
            }
            if let Some(grants) = grants.filter(|grants| *grants != before.permissions) {
                self.set_member_permissions_checked(target.session_id, grants, Some(&check))?;
                changes.push(format!("grants {}", grant_names(grants)));
            }
            if let Some(directory) = working_directory.filter(|directory| Path::new(directory) != Path::new(&before.working_dir)) {
                let after = self.set_session_working_directory_checked(target.session_id, Path::new(&directory), Some(&check))?;
                if after.working_dir != before.working_dir {
                    changes.push(format!("working directory {}", after.working_dir));
                }
            }
            if let Some(position) = position {
                if self.move_session_checked(target.session_id, position as usize, Some(&check))? {
                    changes.push(format!("moved to position {position}"));
                }
            }
            Ok(())
        })();
        if !changes.is_empty() {
            self.post_member_notice(target.room_id, caller, request_id,
                format!("{} updated {}: {}", target.caller_label, before.label, changes.join("; ")), &mut warnings);
        }
        if let Err(error) = outcome {
            if changes.is_empty() {
                return Err(error);
            }
            return Err(PartialEffect(format!("{} was partly updated ({}), then: {error:#}", before.label, changes.join("; "))).into());
        }
        if changes.is_empty() {
            warnings.push(format!("{} already has those settings; nothing changed", before.label));
        }
        self.member_result(request_id, "update", target.room_id, target.session_id, None, warnings)
    }

    /// member_update's target: as resolve_managed_or_lobby_member, but the
    /// caller itself is allowed (it is in its own room by construction).
    fn resolve_editable_member(&self, caller: Actor<'_>, member: &str) -> Result<ManagedMember> {
        let rooms = self.inner.rooms.lock();
        let slots = self.inner.slots.lock();
        let target = Self::find_member_locked(&rooms, &slots, caller, member)?;
        let scope = rooms.room_by_session.get(&target).copied();
        ManageCheck::for_room(caller, scope).check_editable(&rooms, &slots, target)?;
        Ok(ManagedMember {
            room_id: scope.or_else(|| caller.own_room(&rooms)).ok_or_else(|| lobby_no_record(caller))?,
            scope,
            session_id: target,
            label: slots.get_by_id(target).map(|slot| slot.definition.label.clone()).unwrap_or_default(),
            caller_label: caller.label_locked(&slots),
        })
    }

    /// A16: a lobby session joins the caller's room, or `room_id`. A lobby
    /// session is outside the caller's room, so this always needs roam too.
    fn member_add(&self, caller: Actor<'_>, request_id: &str, member: &str, room_id: Option<RoomId>)
        -> Result<MemberActionResult> {
        let target = self.resolve_any_member(caller, member)?;
        if let Some(current) = target.room_id {
            bail!("{} is already in room {current}; member_remove puts it in the lobby first", target.label);
        }
        let room_id = match room_id {
            Some(room_id) => room_id,
            None => caller.own_room(&self.inner.rooms.lock()).ok_or_else(|| anyhow!(
                "you are in the lobby: give member_add a room_id (rooms_list names every room)"))?,
        };
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let check = ManageCheck::for_lobby_target(caller, room_id);
        self.add_room_member_checked(AddRoomMemberRequest { room_id, session_id: target.session_id }, Some(&check))?;
        let mut warnings = Vec::new();
        self.post_member_notice(room_id, caller, request_id,
            format!("{} added {} to the room", target.caller_label, target.label), &mut warnings);
        self.member_result(request_id, "add", room_id, target.session_id, None, warnings)
    }

    /// A16: the operator's Close session: out of its room (when in one),
    /// stopped (when running), then deleted once its event stream drains.
    /// The notice goes to the room it was in; a lobby session's close is
    /// recorded in the caller's room.
    fn member_delete(&self, caller: Actor<'_>, request_id: &str, member: &str) -> Result<MemberActionResult> {
        let target = self.resolve_any_member(caller, member)?;
        // Every agent action is attributed in a room feed: a lobby session's
        // close is recorded in the caller's room, so a caller in the lobby
        // too has nowhere to record it.
        let notice_room = target.room_id
            .or_else(|| caller.own_room(&self.inner.rooms.lock()))
            .ok_or_else(|| lobby_no_record(caller))?;
        #[cfg(test)]
        self.run_member_before_effect_for_tests();
        let snapshot = self.member_snapshot(target.session_id)?;
        let mut steps = Vec::new();
        // Stopped first, while the target is still in the scope that was
        // checked; then out of its room; then deleted.
        let check = ManageCheck::for_room(caller, target.room_id);
        let (ticket, _) = self.begin_stop_session_checked(target.session_id, Some(&check))?;
        let stopped = ticket.expected.is_some();
        let after_stop = self.finish_stop_session(ticket)?;
        if after_stop.running || after_stop.lifecycle_state != LifecycleState::Closed {
            let message = format!("{} did not stop ({:?}); it was not removed or closed", target.label,
                after_stop.lifecycle_state);
            return Err(if stopped { PartialEffect(message).into() } else { anyhow!(message) });
        }
        if let Some(room_id) = target.room_id {
            if let Err(error) = self.remove_room_member_checked(
                RemoveRoomMemberRequest { room_id, session_id: target.session_id }, Some(&check)) {
                if stopped {
                    let mut ignored = Vec::new();
                    self.post_member_notice_keyed(room_id, caller, &format!("{request_id}.stopped"),
                        format!("{} stopped {}", target.caller_label, target.label), &mut ignored);
                }
                let message = format!("{}{} could not leave its room: {error:#}", target.label, if stopped { " was stopped but" } else { "" });
                return Err(if stopped { PartialEffect(message).into() } else { anyhow!(message) });
            }
            steps.push("removed");
        }
        if stopped {
            steps.insert(0, "stopped");
        }
        let delete_check = DeleteCheck { caller, original_room: target.room_id };
        let deadline = Instant::now() + DELETE_DRAIN_WAIT;
        loop {
            match self.delete_session_checked(target.session_id, Some(&delete_check)) {
                Ok(()) => break,
                Err(error) => {
                    let text = format!("{error:#}");
                    let transient = text.contains("must be fully closed") || text.contains("still draining");
                    if !transient || Instant::now() >= deadline {
                        if !steps.is_empty() {
                            let mut ignored = Vec::new();
                            self.post_member_notice_keyed(notice_room, caller, &format!("{request_id}.partial"),
                                format!("{} {} {}", target.caller_label, join_steps(&steps), target.label), &mut ignored);
                        }
                        let message = format!("{} was {} but not closed: {text}", target.label,
                            if steps.is_empty() { "found".to_string() } else { steps.join(" and ") });
                        return Err(if steps.is_empty() { anyhow!(message) } else { PartialEffect(message).into() });
                    }
                    std::thread::sleep(Duration::from_millis(250));
                }
            }
        }
        steps.push("closed");
        let mut warnings = Vec::new();
        self.post_member_notice(notice_room, caller, request_id,
            format!("{} {} {}", target.caller_label, join_steps(&steps), target.label), &mut warnings);
        // The session is gone: its last snapshot, closed.
        Ok(MemberActionResult { request_id: request_id.into(), replayed: false, op: "delete".into(),
            room_id: notice_room, member: SessionSnapshot { running: false, lifecycle_state: LifecycleState::Closed,
                ..snapshot },
            brief: None, incomplete: Vec::new(), warnings })
    }
}

/// "removed, stopped and closed".
fn join_steps(steps: &[&str]) -> String {
    match steps {
        [] => String::new(),
        [one] => (*one).to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// member_start's choices.
struct StartRequest {
    mode: StartMode,
    launch_choices: Option<SessionLaunchChoices>,
    search: bool,
}

/// member_update's fields.
struct MemberEdit {
    label: Option<String>,
    permission_profile: Option<PermissionProfile>,
    grants: Option<MemberPermissions>,
    working_directory: Option<String>,
    position: Option<u32>,
}

struct NewMember {
    driver: DriverKind,
    label: String,
    permission_profile: PermissionProfile,
    brief: bool,
    brief_text: Option<String>,
    start: bool,
    room_id: Option<RoomId>,
    grants: Option<MemberPermissions>,
    codex_transport: shared_types::CodexTransport,
}
