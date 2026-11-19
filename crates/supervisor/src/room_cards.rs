// Included by rooms.rs: one bounded, replay-built latest-card projection.
const ROOM_CARD_MAX_KEYS: usize = 64;
const ROOM_CARD_INDEX_MAX_BYTES: usize = 2 * 1024 * 1024;

#[derive(Default)]
struct CardIndex {
    // Insertion order is stable across an update; eviction chooses the oldest
    // unpinned identity. This is independent of the raw event/dedup horizons.
    entries: VecDeque<(RoomFeedEvent, usize)>,
}

fn card_identity(event: &RoomFeedEvent) -> Option<(SessionId, &str)> {
    match &event.item {
        RoomFeedItem::Card { author, card, .. } => Some((*author, card.key.as_str())),
        _ => None,
    }
}

impl CardIndex {
    fn victims(&self, event: &RoomFeedEvent, bytes: usize) -> Result<Vec<usize>> {
        let key = card_identity(event).expect("card admission requires a card");
        let existing = self.entries.iter().position(|(old, _)| card_identity(old) == Some(key));
        let mut count = self.entries.len() + usize::from(existing.is_none());
        let mut size = self.entries.iter().map(|(_, size)| size).sum::<usize>() + bytes
            - existing.map(|index| self.entries[index].1).unwrap_or(0);
        let mut victims = Vec::new();
        for (index, (old, old_bytes)) in self.entries.iter().enumerate() {
            if count <= ROOM_CARD_MAX_KEYS && size <= ROOM_CARD_INDEX_MAX_BYTES { break; }
            if Some(index) == existing || matches!(&old.item, RoomFeedItem::Card { card, .. } if card.pinned()) { continue; }
            victims.push(index); count -= 1; size -= old_bytes;
        }
        anyhow::ensure!(count <= ROOM_CARD_MAX_KEYS && size <= ROOM_CARD_INDEX_MAX_BYTES,
            "room card capacity is held by open decisions; finish an existing decision before adding more");
        Ok(victims)
    }

    fn apply(&mut self, event: &RoomFeedEvent) {
        if card_identity(event).is_none() { return; }
        let bytes = serde_json::to_vec(event).expect("admitted card serializes").len();
        let victims = self.victims(event, bytes).expect("card admission checked before journal write/replay");
        for index in victims.into_iter().rev() { self.entries.remove(index); }
        if let Some(entry) = self.entries.iter_mut().find(|(old, _)| card_identity(old) == card_identity(event)) {
            *entry = (event.clone(), bytes);
        } else { self.entries.push_back((event.clone(), bytes)); }
    }

    fn snapshot(&self, floor: RoomSequence) -> Vec<RoomFeedEvent> {
        self.entries.iter().filter(|(event, _)| event.cursor.sequence > floor)
            .map(|(event, _)| event.clone()).collect()
    }
}

impl RoomRuntime {
    fn validate_card_event(&self, event: &RoomFeedEvent) -> Result<()> {
        if let RoomFeedItem::Card { card, request_id: id, .. } = &event.item {
            card.validate().map_err(anyhow::Error::msg)?;
            request_id(Some(id.clone()))?;
            self.cards.victims(event, serde_json::to_vec(event)?.len())?;
        }
        Ok(())
    }

    pub(crate) fn post_card(&mut self, request_id: String, author: SessionId,
        card: shared_types::RoomCardContent) -> Result<(RoomFeedEvent, bool)> {
        self.ensure_available()?;
        card.validate().map_err(anyhow::Error::msg)?;
        if let Some(stored) = self.posts.get(&(SenderKey::Session(author), request_id.clone())) {
            if !matches!(&stored.event.item, RoomFeedItem::Card { card: old, .. } if old == &card) {
                return Err(RequestConflict.into());
            }
            return Ok((stored.event.clone(), true));
        }
        let event = self.append(RoomFeedItem::Card { request_id, author, card,
            card_id: Uuid::new_v4() })?;
        Ok((event, false))
    }
}
