//! v3 W5: a room's capability directory for its own members. It reports how
//! each member and guest is reached and what PRIM-1 knows of it now, from the
//! same checks a wake plans with, read-only: no claim, no queue entry, no
//! paths, conversation or run ids, and no claim beyond PRIM-1's own readiness
//! (Reviewer 60269256).
use super::*;
use shared_types::{RoomCapabilityDirectory, RoomFeedCursor, RoomGuestCapability, RoomMemberCapability};

/// Room operations a pane with room tools offers, as MCP tools and CLI verbs.
const PANE_ROOM_OPERATIONS: [&str; 5] = ["read", "wait", "post", "deliver", "ack"];
/// What any guest invitation allows.
const GUEST_ROOM_OPERATIONS: [&str; 3] = ["read", "wait", "post"];
/// v3 W5: what a guest whose latest connection reads addressed messages also has.
const ADDRESSED_GUEST_OPERATIONS: [&str; 3] = ["ack", "directory", "status"];

impl SupervisorHandle {
    pub(super) fn room_directory_for_pane(&self, caller: &PaneCaller) -> Result<RoomCapabilityDirectory> {
        let room_id = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            Self::pane_room_binding_locked(caller, &slots, &rooms)?.0
        };
        self.room_directory_for_room(room_id)
    }

    /// v3 W5: the directory of a room, for one of its members or guests.
    pub(super) fn room_directory_for_room(&self, room_id: RoomId) -> Result<RoomCapabilityDirectory> {
        let (label, members) = {
            let rooms = self.inner.rooms.lock();
            let slots = self.inner.slots.lock();
            let room = rooms.get(room_id).ok_or_else(|| anyhow!("room not found"))?;
            let confirmations = room.member_confirmations();
            let settle = *self.inner.room_wake_output_settle.lock();
            let title_cap = *self.inner.room_wake_title_busy_cap.lock();
            let members: Vec<_> = room.definition.member_ids.iter()
                .filter_map(|id| slots.get_by_id(*id).map(|slot| (*id, slot)))
                .map(|(id, slot)| member_capability(slot, settle, title_cap,
                    confirmations.get(&id).copied().unwrap_or_default()))
                .collect();
            (room.definition.label.clone(), members)
        };
        let addressed = self.inner.guests.lock().addressed_readers.clone();
        let guests = self.room_guests(room_id).into_iter().map(|guest| {
            // Addressable as of the guest's latest connection: a later older
            // client turns it back, and admission resolves against the same.
            let addressable = addressed.get(&guest.guest_id).copied().unwrap_or(false);
            RoomGuestCapability {
                guest_id: guest.guest_id,
                label: guest.label,
                operations: GUEST_ROOM_OPERATIONS.iter()
                    .chain(if addressable { ADDRESSED_GUEST_OPERATIONS.iter() } else { [].iter() })
                    .map(|op| (*op).to_owned()).collect(),
                wake_supported: false,
                addressable,
                last_seen: guest.last_seen,
            }
        }).collect();
        Ok(RoomCapabilityDirectory { room_id, label, prim1_build: prim1_build(), members, guests })
    }
}

fn member_capability(slot: &SessionSlot, settle: Duration, title_cap: Duration,
    (last_fetched, last_acked): (Option<RoomFeedCursor>, Option<RoomFeedCursor>)) -> RoomMemberCapability {
    let snapshot = slot.snapshot();
    let wake_note = room_delivery::unsupported_room_wake(slot.definition.driver).map(str::to_owned);
    // The order of `plan_room_wake`'s own checks, for a pointer wake (every
    // ACK-tracked message wakes by pointer). Reviewer cb415335: the App Server
    // person-request hold applies on either route; the screen-quiet wait and
    // the title hold protect a typed paste only, so an App Server route skips them.
    let app_server = room_delivery::app_server_route(slot);
    let typed_codex = slot.definition.driver == DriverKind::Codex && !app_server;
    let not_ready = wake_note.clone()
        .or_else(|| room_delivery::room_wake_busy(slot).map(std::borrow::Cow::into_owned))
        .or_else(|| room_delivery::person_request_hold(slot))
        .or_else(|| (typed_codex && !slot.room_composer.output_quiet_for(settle))
            .then(|| "the member's screen is still changing; a wake waits until it settles".to_owned()))
        .or_else(|| typed_codex.then(|| slot.codex_work_state.title_busy()).flatten()
            .filter(|(_, busy_for)| *busy_for < title_cap)
            .map(|(state, _)| format!("Codex's own title reads {}; a wake waits until that changes (at most {} s)",
                state.label(), title_cap.as_secs())))
        .or_else(|| wake_target_refusal(slot));
    RoomMemberCapability {
        session_id: snapshot.session_id,
        label: snapshot.label,
        driver: snapshot.driver,
        operations: if wake_note.is_some() { Vec::new() }
            else { PANE_ROOM_OPERATIONS.iter().map(|op| (*op).to_owned()).collect() },
        wake_supported: wake_note.is_none(),
        wake_note,
        state: serde_json::to_value(snapshot.lifecycle_state).ok()
            .and_then(|value| value.as_str().map(str::to_owned)).unwrap_or_default(),
        ready: not_ready.is_none(),
        not_ready,
        // v3 W2: what this run's launch program reported; None is unknown.
        harness_version: slot.harness_version.clone(),
        last_fetched,
        last_acked,
    }
}

/// The wake target checks of `delivery_target_for_session_id`, without its
/// announcement side effect: prompt, startup, paste mode, a changed run, or
/// another wake being typed.
fn wake_target_refusal(slot: &SessionSlot) -> Option<String> {
    let behavior = routed_message_submit_behavior(effective_delivery_driver(slot));
    let target = run_write_target_from_slot(&slot.definition.alias, slot)
        .and_then(|target| ensure_run_input_safety_locked(slot, &target, RunInputSafety::from(behavior)).map(|_| target));
    match target {
        Ok(target) if target.input_gate.room_wake_pending.load(Ordering::SeqCst) =>
            Some(room_delivery::ROOM_WAKE_PENDING.into()),
        Ok(_) => None,
        Err(error) => Some(room_delivery::room_wake_error(&error)),
    }
}

/// The PRIM-1 version, with the build revision when the build recorded one.
fn prim1_build() -> String {
    match option_env!("PRIM1_BUILD_REVISION") {
        Some(revision) => format!("{} ({revision})", env!("CARGO_PKG_VERSION")),
        None => env!("CARGO_PKG_VERSION").to_owned(),
    }
}
