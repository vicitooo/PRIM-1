//! Room-feed long-polls: an outside agent's `wait` and (A16) a pane's
//! `room_wait`. Both subscribe, read on a blocking worker and sleep on the
//! room-feed watch until news or the deadline, holding no room or registry
//! lock while they sleep.
use super::*;
use shared_types::RoomFeedCursor;

/// Under the server's 30 s response write and the pane client's 45 s read,
/// as the guests' `GUEST_WAIT_MAX`.
pub(crate) const PANE_WAIT_MAX: Duration = Duration::from_secs(25);
/// Long-polls share the pipe's 64 connections with the guests' 8.
pub(crate) const PANE_WAIT_SLOTS: usize = 16;
/// As `GUEST_WAIT_SLOT_GRACE`: a wait that gets neither its turn nor a slot
/// in this long answers at once, so a member that loops on it still paces itself.
pub(crate) const PANE_WAIT_SLOT_GRACE: Duration = Duration::from_secs(2);

/// Who waits, on what budget: one open wait per waiter (`own`) and `slots`
/// in all. A wait that gets neither within `grace` answers at once with what
/// is there, saying `crowded` when the slots were the reason.
pub(super) struct FeedWaitTurn {
    pub(super) own: Arc<tokio::sync::Mutex<()>>,
    pub(super) slots: Arc<Semaphore>,
    pub(super) grace: Duration,
    pub(super) crowded: &'static str,
}

pub(super) enum FeedWaitOutcome {
    /// `timed_out`: no news came; `note` says why it answered early.
    Page { page: RoomFeedPage, timed_out: bool, note: Option<&'static str> },
    Refused(String),
}

/// News for a waiting pane: anything but delivery receipts and its own posts
/// and cards.
fn pane_page_has_news(page: &RoomFeedPage, me: SessionId) -> bool {
    page.gap.is_some() || page.events.iter().any(|event| match &event.item {
        RoomFeedItem::Message { sender: RoomMessageSender::Session { session_id }, .. } => *session_id != me,
        RoomFeedItem::Card { author, .. } => *author != me,
        RoomFeedItem::Delivery { .. } | RoomFeedItem::Waiting { .. } | RoomFeedItem::InputObserved { .. } | RoomFeedItem::SubmitAttempt { .. } => false,
        // v3 W5: a guest's disposition is news for members (a sender waits for it).
        RoomFeedItem::GuestAck { .. } => true,
        // v3 W3: someone else's disposition is news (a sender waits for it).
        RoomFeedItem::Ack { actor: shared_types::RoomAckActor::Session { session_id }, .. } => *session_id != me,
        // v3 W3 (D6): a withdrawal is news (its recipients stop owing it).
        RoomFeedItem::Message { .. } | RoomFeedItem::Membership { .. } | RoomFeedItem::Ack { .. }
        | RoomFeedItem::Cancel { .. } => true,
    })
}

impl SupervisorHandle {
    /// `read` runs on a blocking worker for each page, so it may take locks
    /// and check the waiter again; `news` decides whether a page ends the wait.
    pub(super) async fn wait_for_feed_news(
        &self, turn: FeedWaitTurn, cursor: RoomFeedCursor, timeout: Duration,
        read: impl Fn(&SupervisorHandle, RoomFeedCursor) -> Result<RoomFeedPage> + Send + Sync + 'static,
        news: impl Fn(&RoomFeedPage) -> bool,
    ) -> FeedWaitOutcome {
        let now = tokio::time::Instant::now();
        let deadline = now + timeout;
        let grace = deadline.min(now + turn.grace);
        let (_own, mut busy) = match tokio::time::timeout_at(grace, turn.own.lock_owned()).await {
            Ok(own) => (Some(own), None),
            Err(_) => (None, Some("another wait of yours is still open, so this one answered early")),
        };
        let _permit = match busy {
            Some(_) => None,
            None => match tokio::time::timeout_at(grace, turn.slots.acquire_owned()).await {
                Ok(Ok(permit)) => Some(permit),
                Ok(Err(_)) => return FeedWaitOutcome::Refused("PRIM-1 is shutting down".into()),
                Err(_) => {
                    busy = Some(turn.crowded);
                    None
                }
            },
        };
        let read = Arc::new(read);
        let mut changes = self.inner.room_feed_watch.subscribe();
        let mut cursor = cursor;
        loop {
            changes.borrow_and_update();
            let (handle, read) = (self.clone(), read.clone());
            let page = match tokio::task::spawn_blocking(move || read(&handle, cursor)).await {
                Ok(Ok(page)) => page,
                Ok(Err(error)) => return FeedWaitOutcome::Refused(format!("{error:#}")),
                Err(error) => return FeedWaitOutcome::Refused(format!("room wait worker failed: {error}")),
            };
            if news(&page) {
                return FeedWaitOutcome::Page { page, timed_out: false, note: None };
            }
            if busy.is_some() || tokio::time::Instant::now() >= deadline {
                return FeedWaitOutcome::Page { page, timed_out: true, note: busy };
            }
            // Receipts and the waiter's own posts move the cursor, not the wait.
            cursor = page.cursor;
            if page.has_more {
                continue;
            }
            tokio::select! {
                changed = changes.changed() => {
                    if changed.is_err() {
                        return FeedWaitOutcome::Refused("PRIM-1 is shutting down".into());
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {}
            }
            if self.ensure_active().is_err() {
                return FeedWaitOutcome::Refused("PRIM-1 is shutting down".into());
            }
        }
    }

    /// A16 `room_wait`: `room_read` that waits for news from someone else,
    /// at most `PANE_WAIT_MAX`. Every read checks the caller's run and room
    /// again and records Fetched receipts as `room_read` does.
    pub(super) async fn pane_room_wait(&self, caller: &PaneCaller, cursor: RoomFeedCursor, timeout_ms: Option<u64>)
        -> SidebandResponse {
        let me = caller.session_id;
        let own = self.inner.pane_waits_by_session.lock().entry(me).or_default().clone();
        let turn = FeedWaitTurn { own, slots: self.inner.pane_waits.clone(), grace: PANE_WAIT_SLOT_GRACE,
            crowded: "too many members are waiting, so this answered early" };
        let timeout = timeout_ms.map_or(PANE_WAIT_MAX, Duration::from_millis).min(PANE_WAIT_MAX);
        let reader = caller.clone();
        let outcome = self.wait_for_feed_news(turn, cursor, timeout, move |handle, cursor| {
            handle.authorize_sideband_request(&reader, &SidebandRequest::RoomRead { owed_after: None, cursor: Some(cursor), expect_room: None })?;
            handle.read_room_feed_from_pane(&reader, Some(cursor))
        }, |page| pane_page_has_news(page, me)).await;
        {
            // The pane's turn goes with its last waiter.
            let mut waits = self.inner.pane_waits_by_session.lock();
            if waits.get(&me).is_some_and(|own| Arc::strong_count(own) == 1) {
                waits.remove(&me);
            }
        }
        match outcome {
            FeedWaitOutcome::Page { page, timed_out, note } => {
                let mut message = match (&page.gap, timed_out) {
                    (Some(_), _) => "room feed read; part of the history is unavailable (see gap)".to_string(),
                    (None, true) => "no news before the deadline; wait again with this cursor".to_string(),
                    (None, false) => "room feed read".to_string(),
                };
                if let Some(note) = note {
                    message = format!("{message}; {note}");
                }
                SidebandResponse { ok: true, message, timed_out, payload: Some(SidebandResponsePayload::RoomFeed { page }),
                    request_id: None }
            }
            FeedWaitOutcome::Refused(message) => Self::rejected_sideband_response(message),
        }
    }
}
