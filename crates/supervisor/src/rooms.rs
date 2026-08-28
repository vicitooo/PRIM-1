use std::collections::{HashMap, HashSet, VecDeque};

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use shared_types::{
    RoomFeedCursor, RoomFeedEvent, RoomFeedGap, RoomFeedGapReason, RoomFeedItem, RoomFeedPage,
    RoomId, RoomRevision, RoomSequence, RoomSnapshot, SessionId, now_rfc3339,
};
use uuid::Uuid;

pub(crate) const ROOM_CATALOG_SCHEMA_VERSION: u32 = 1;
pub(crate) const ROOM_CATALOG_FILE_NAME: &str = "room-catalog-v1.json";
pub(crate) const ROOM_MAX_COUNT: usize = 64;
pub(crate) const ROOM_MEMBER_MAX_COUNT: usize = 64;
pub(crate) const ROOM_LABEL_MAX_CHARS: usize = 128;
const ROOM_FEED_MAX_EVENTS: usize = 512;
const ROOM_FEED_MAX_BYTES: usize = 16 * 1024 * 1024;
const ROOM_FEED_PAGE_MAX_EVENTS: usize = 64;
const ROOM_FEED_PAGE_MAX_BYTES: usize = 2 * 1024 * 1024;

fn default_brief_on_join() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct PersistedRoomV1 {
    pub(crate) room_id: RoomId,
    pub(crate) label: String,
    pub(crate) member_ids: Vec<SessionId>,
    pub(crate) membership_revision: RoomRevision,
    /// Whether the room brief is typed into members' terminals automatically
    /// (on create, join, and a run's first idle). Catalogs written before
    /// this field existed always briefed, so absent means true.
    #[serde(default = "default_brief_on_join")]
    pub(crate) brief_on_join: bool,
    /// Operator-edited brief template; `None` means the canonical brief.
    #[serde(default)]
    pub(crate) brief_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct RoomCatalogV1 {
    pub(crate) schema_version: u32,
    pub(crate) rooms: Vec<PersistedRoomV1>,
}

impl RoomCatalogV1 {
    pub(crate) fn empty() -> Self {
        Self {
            schema_version: ROOM_CATALOG_SCHEMA_VERSION,
            rooms: Vec::new(),
        }
    }
}

pub(crate) struct RoomRuntime {
    pub(crate) definition: PersistedRoomV1,
    pub(crate) feed_epoch: Uuid,
    feed: VecDeque<(RoomFeedEvent, usize)>,
    feed_bytes: usize,
    pub(crate) next_sequence: RoomSequence,
    pub(crate) join_floor_by_session: HashMap<SessionId, RoomSequence>,
    pub(crate) in_flight_deliveries: usize,
}

pub(crate) struct PreparedRoomFeedEvent {
    event: RoomFeedEvent,
    bytes: usize,
}

impl RoomRuntime {
    pub(crate) fn from_persisted(definition: PersistedRoomV1) -> Self {
        let join_floor_by_session = definition
            .member_ids
            .iter()
            .copied()
            .map(|session_id| (session_id, 0))
            .collect();
        Self {
            definition,
            feed_epoch: Uuid::new_v4(),
            feed: VecDeque::new(),
            feed_bytes: 0,
            next_sequence: 1,
            join_floor_by_session,
            in_flight_deliveries: 0,
        }
    }

    pub(crate) fn snapshot(&self) -> RoomSnapshot {
        RoomSnapshot {
            room_id: self.definition.room_id,
            label: self.definition.label.clone(),
            member_ids: self.definition.member_ids.clone(),
            membership_revision: self.definition.membership_revision,
            feed_epoch: self.feed_epoch,
            feed_oldest_sequence: self
                .feed
                .front()
                .map(|(event, _bytes)| event.cursor.sequence)
                .unwrap_or(self.next_sequence),
            feed_next_sequence: self.next_sequence,
        }
    }

    pub(crate) fn ensure_sequence_capacity(&self, additional: usize) -> Result<()> {
        let additional = RoomSequence::try_from(additional)
            .map_err(|_| anyhow!("room feed reservation is too large"))?;
        self.next_sequence
            .checked_add(additional)
            .ok_or_else(|| anyhow!("room feed sequence exhausted"))?;
        Ok(())
    }

    pub(crate) fn prepare_append(&self, item: RoomFeedItem) -> Result<PreparedRoomFeedEvent> {
        let sequence = self.next_sequence;
        sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("room feed sequence exhausted"))?;
        let event = RoomFeedEvent {
            schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
            room_id: self.definition.room_id,
            cursor: RoomFeedCursor {
                epoch: self.feed_epoch,
                sequence,
            },
            item,
            timestamp: now_rfc3339(),
        };
        let event_bytes = serde_json::to_vec(&event)
            .map_err(|error| anyhow!("failed to measure room feed event: {error}"))?
            .len();
        if event_bytes > ROOM_FEED_MAX_BYTES {
            return Err(anyhow!(
                "room feed event requires {event_bytes} bytes, exceeding the {} byte feed bound",
                ROOM_FEED_MAX_BYTES
            ));
        }
        Ok(PreparedRoomFeedEvent {
            event,
            bytes: event_bytes,
        })
    }

    pub(crate) fn commit_append(&mut self, prepared: PreparedRoomFeedEvent) -> RoomFeedEvent {
        debug_assert_eq!(prepared.event.room_id, self.definition.room_id);
        debug_assert_eq!(prepared.event.cursor.epoch, self.feed_epoch);
        debug_assert_eq!(prepared.event.cursor.sequence, self.next_sequence);
        let next_sequence = self
            .next_sequence
            .checked_add(1)
            .expect("prepared room feed event reserved sequence capacity");
        while self.feed.len() >= ROOM_FEED_MAX_EVENTS
            || self.feed_bytes.saturating_add(prepared.bytes) > ROOM_FEED_MAX_BYTES
        {
            let (_evicted, evicted_bytes) = self
                .feed
                .pop_front()
                .expect("a room feed bound required eviction from a non-empty feed");
            self.feed_bytes -= evicted_bytes;
        }
        self.feed
            .push_back((prepared.event.clone(), prepared.bytes));
        self.feed_bytes += prepared.bytes;
        self.next_sequence = next_sequence;
        prepared.event
    }

    pub(crate) fn append(&mut self, item: RoomFeedItem) -> Result<RoomFeedEvent> {
        let prepared = self.prepare_append(item)?;
        Ok(self.commit_append(prepared))
    }

    pub(crate) fn read(
        &self,
        cursor: Option<RoomFeedCursor>,
        floor: RoomSequence,
    ) -> Result<RoomFeedPage> {
        let mut gap = None;
        let requested_after = match cursor {
            Some(cursor) if cursor.epoch == self.feed_epoch => {
                if cursor.sequence >= self.next_sequence {
                    return Err(anyhow!(
                        "room feed cursor sequence {} is ahead of the next sequence {}",
                        cursor.sequence,
                        self.next_sequence
                    ));
                }
                cursor.sequence.max(floor)
            }
            Some(_) => {
                gap = Some(RoomFeedGap {
                    reason: RoomFeedGapReason::EpochReset,
                    from_sequence: None,
                    through_sequence: None,
                });
                floor
            }
            None => floor,
        };
        let oldest = self
            .feed
            .front()
            .map(|(event, _bytes)| event.cursor.sequence)
            .unwrap_or(self.next_sequence);
        if gap.is_none() && requested_after.saturating_add(1) < oldest {
            gap = Some(RoomFeedGap {
                reason: RoomFeedGapReason::Evicted,
                from_sequence: Some(requested_after.saturating_add(1)),
                through_sequence: Some(oldest - 1),
            });
        }

        let effective_after = requested_after.max(oldest.saturating_sub(1));
        let mut events = Vec::new();
        let mut page_bytes = 0usize;
        for event in self
            .feed
            .iter()
            .map(|(event, _bytes)| event)
            .filter(|event| event.cursor.sequence > effective_after)
        {
            let event_bytes = serde_json::to_vec(event)
                .map_err(|error| anyhow!("failed to measure room feed page event: {error}"))?
                .len();
            if !events.is_empty()
                && (events.len() >= ROOM_FEED_PAGE_MAX_EVENTS
                    || page_bytes.saturating_add(event_bytes) > ROOM_FEED_PAGE_MAX_BYTES)
            {
                break;
            }
            page_bytes += event_bytes;
            events.push(event.clone());
        }
        let last_sequence = events
            .last()
            .map(|event| event.cursor.sequence)
            .unwrap_or(effective_after);
        let has_more = self
            .feed
            .back()
            .is_some_and(|(event, _bytes)| event.cursor.sequence > last_sequence);
        Ok(RoomFeedPage {
            schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
            room_id: self.definition.room_id,
            cursor: RoomFeedCursor {
                epoch: self.feed_epoch,
                sequence: last_sequence,
            },
            gap,
            events,
            has_more,
            // Filled by the supervisor, which owns session labels.
            members: Vec::new(),
        })
    }
}

pub(crate) struct RoomState {
    pub(crate) catalog: RoomCatalogV1,
    pub(crate) by_id: HashMap<RoomId, RoomRuntime>,
    pub(crate) order: Vec<RoomId>,
    pub(crate) room_by_session: HashMap<SessionId, RoomId>,
}

impl RoomState {
    pub(crate) fn from_catalog(
        catalog: RoomCatalogV1,
        known_sessions: &HashSet<SessionId>,
    ) -> Result<Self> {
        if catalog.schema_version != ROOM_CATALOG_SCHEMA_VERSION {
            return Err(anyhow!(
                "unsupported room catalog schema version {} (expected {})",
                catalog.schema_version,
                ROOM_CATALOG_SCHEMA_VERSION
            ));
        }
        if catalog.rooms.len() > ROOM_MAX_COUNT {
            return Err(anyhow!(
                "room catalog contains {} rooms, exceeding the limit of {ROOM_MAX_COUNT}",
                catalog.rooms.len()
            ));
        }
        let mut by_id = HashMap::new();
        let mut order = Vec::with_capacity(catalog.rooms.len());
        let mut room_by_session = HashMap::new();
        for definition in &catalog.rooms {
            validate_room_label(&definition.label)?;
            if definition.membership_revision == 0 {
                return Err(anyhow!(
                    "room '{}' has invalid zero membership revision",
                    definition.room_id
                ));
            }
            if definition.member_ids.len() > ROOM_MEMBER_MAX_COUNT {
                return Err(anyhow!(
                    "room '{}' has {} members, exceeding the limit of {ROOM_MEMBER_MAX_COUNT}",
                    definition.room_id,
                    definition.member_ids.len()
                ));
            }
            let mut unique_members = HashSet::new();
            for session_id in &definition.member_ids {
                if !known_sessions.contains(session_id) {
                    return Err(anyhow!(
                        "room '{}' references unknown session '{}'",
                        definition.room_id,
                        session_id
                    ));
                }
                if !unique_members.insert(*session_id) {
                    return Err(anyhow!(
                        "room '{}' contains duplicate session '{}'",
                        definition.room_id,
                        session_id
                    ));
                }
                if let Some(existing_room) = room_by_session.insert(*session_id, definition.room_id)
                {
                    return Err(anyhow!(
                        "session '{}' belongs to both room '{}' and room '{}'",
                        session_id,
                        existing_room,
                        definition.room_id
                    ));
                }
            }
            if by_id
                .insert(
                    definition.room_id,
                    RoomRuntime::from_persisted(definition.clone()),
                )
                .is_some()
            {
                return Err(anyhow!(
                    "room catalog contains duplicate room id '{}'",
                    definition.room_id
                ));
            }
            order.push(definition.room_id);
        }
        Ok(Self {
            catalog,
            by_id,
            order,
            room_by_session,
        })
    }

    pub(crate) fn snapshots(&self) -> Vec<RoomSnapshot> {
        self.order
            .iter()
            .filter_map(|room_id| self.by_id.get(room_id).map(RoomRuntime::snapshot))
            .collect()
    }

    pub(crate) fn get(&self, room_id: RoomId) -> Option<&RoomRuntime> {
        self.by_id.get(&room_id)
    }

    pub(crate) fn get_mut(&mut self, room_id: RoomId) -> Option<&mut RoomRuntime> {
        self.by_id.get_mut(&room_id)
    }
}

pub(crate) fn validate_room_label(label: &str) -> Result<()> {
    if label.trim() != label || label.is_empty() {
        return Err(anyhow!(
            "room label must be non-empty and cannot start or end with whitespace"
        ));
    }
    if label.chars().count() > ROOM_LABEL_MAX_CHARS {
        return Err(anyhow!(
            "room label exceeds the {ROOM_LABEL_MAX_CHARS} character limit"
        ));
    }
    if label.chars().any(char::is_control) {
        return Err(anyhow!("room label contains a control character"));
    }
    Ok(())
}

pub(crate) fn default_room_label(existing_count: usize) -> String {
    format!("Room {}", existing_count.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{RoomMembershipAction, RoomMessageSender};

    fn definition(room_id: RoomId, label: &str, members: Vec<SessionId>) -> PersistedRoomV1 {
        PersistedRoomV1 {
            room_id,
            label: label.into(),
            member_ids: members,
            membership_revision: 1,
            brief_on_join: true,
            brief_template: None,
        }
    }

    #[test]
    fn persisted_room_defaults_brief_fields_when_absent() {
        let json = format!(
            r#"{{"room_id":"{}","label":"old","member_ids":[],"membership_revision":1}}"#,
            Uuid::new_v4()
        );
        let room: PersistedRoomV1 = serde_json::from_str(&json).unwrap();
        assert!(room.brief_on_join);
        assert!(room.brief_template.is_none());

        let back = serde_json::to_string(&room).unwrap();
        let reparsed: PersistedRoomV1 = serde_json::from_str(&back).unwrap();
        assert_eq!(room, reparsed);
    }

    fn message(index: usize, content: String) -> RoomFeedItem {
        RoomFeedItem::Message {
            message_id: Uuid::from_u128(index as u128 + 1),
            sender: RoomMessageSender::Operator {},
            content,
            recipient_ids: Vec::new(),
            membership_revision: 1,
        }
    }

    #[test]
    fn same_label_rooms_remain_isolated_by_room_id() {
        let sessions = [
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ];
        let room_a = Uuid::new_v4();
        let room_b = Uuid::new_v4();
        let state = RoomState::from_catalog(
            RoomCatalogV1 {
                schema_version: ROOM_CATALOG_SCHEMA_VERSION,
                rooms: vec![
                    definition(room_a, "Twin", sessions[..2].to_vec()),
                    definition(room_b, "Twin", sessions[2..].to_vec()),
                ],
            },
            &sessions.into_iter().collect(),
        )
        .unwrap();

        assert_eq!(state.order, vec![room_a, room_b]);
        assert_eq!(state.room_by_session[&sessions[0]], room_a);
        assert_eq!(state.room_by_session[&sessions[3]], room_b);
        assert_ne!(
            state.get(room_a).unwrap().feed_epoch,
            state.get(room_b).unwrap().feed_epoch
        );
    }

    #[test]
    fn duplicate_or_cross_room_membership_fails_closed() {
        let member = Uuid::new_v4();
        let known = HashSet::from([member]);
        for rooms in [
            vec![definition(
                Uuid::new_v4(),
                "Duplicate",
                vec![member, member],
            )],
            vec![
                definition(Uuid::new_v4(), "First", vec![member]),
                definition(Uuid::new_v4(), "Second", vec![member]),
            ],
        ] {
            assert!(
                RoomState::from_catalog(
                    RoomCatalogV1 {
                        schema_version: ROOM_CATALOG_SCHEMA_VERSION,
                        rooms,
                    },
                    &known,
                )
                .is_err()
            );
        }
    }

    #[test]
    fn bounded_feed_reports_exact_eviction_gap_and_pages() {
        let room_id = Uuid::new_v4();
        let mut room = RoomRuntime::from_persisted(definition(room_id, "Bounded", Vec::new()));
        for index in 0..(ROOM_FEED_MAX_EVENTS + 17) {
            room.append(message(index, format!("event-{index}")))
                .unwrap();
        }

        assert_eq!(room.feed.len(), ROOM_FEED_MAX_EVENTS);
        let first = room.read(None, 0).unwrap();
        assert_eq!(
            first.gap,
            Some(RoomFeedGap {
                reason: RoomFeedGapReason::Evicted,
                from_sequence: Some(1),
                through_sequence: Some(17),
            })
        );
        assert_eq!(first.events.len(), ROOM_FEED_PAGE_MAX_EVENTS);
        assert!(first.has_more);
        let second = room.read(Some(first.cursor), 0).unwrap();
        assert_eq!(second.events[0].cursor.sequence, first.cursor.sequence + 1);
    }

    #[test]
    fn feed_byte_bound_evicts_large_messages_without_exceeding_limit() {
        let mut room =
            RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Byte bounded", Vec::new()));
        for index in 0..24 {
            room.append(message(index, "x".repeat(1024 * 1024)))
                .unwrap();
        }
        assert!(room.feed_bytes <= ROOM_FEED_MAX_BYTES);
        assert!(room.feed.len() < 24);
        assert_eq!(
            room.read(None, 0).unwrap().gap.unwrap().reason,
            RoomFeedGapReason::Evicted
        );
    }

    #[test]
    fn join_floor_exposes_only_future_feed_traffic_including_join_event() {
        let member = Uuid::new_v4();
        let mut room =
            RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Future only", Vec::new()));
        room.append(message(0, "private-before-join".into()))
            .unwrap();
        let floor = room.next_sequence - 1;
        room.append(RoomFeedItem::Membership {
            action: RoomMembershipAction::Joined,
            session_id: member,
            membership_revision: 2,
        })
        .unwrap();
        room.append(message(1, "after-join".into())).unwrap();

        let page = room.read(None, floor).unwrap();
        assert_eq!(page.events.len(), 2);
        assert!(matches!(
            page.events[0].item,
            RoomFeedItem::Membership { .. }
        ));
        assert!(matches!(page.events[1].item, RoomFeedItem::Message { .. }));
        assert!(
            !serde_json::to_string(&page)
                .unwrap()
                .contains("private-before-join")
        );
    }

    #[test]
    fn stale_epoch_is_explicit_and_cursor_ahead_is_rejected() {
        let mut room = RoomRuntime::from_persisted(definition(Uuid::new_v4(), "Epoch", Vec::new()));
        room.append(message(0, "current".into())).unwrap();
        let stale = room
            .read(
                Some(RoomFeedCursor {
                    epoch: Uuid::new_v4(),
                    sequence: 1,
                }),
                0,
            )
            .unwrap();
        assert_eq!(stale.gap.unwrap().reason, RoomFeedGapReason::EpochReset);
        for index in 1..(ROOM_FEED_MAX_EVENTS + 8) {
            room.append(message(index, format!("evict-{index}")))
                .unwrap();
        }
        let stale_after_eviction = room
            .read(
                Some(RoomFeedCursor {
                    epoch: Uuid::new_v4(),
                    sequence: 0,
                }),
                0,
            )
            .unwrap();
        assert_eq!(
            stale_after_eviction.gap.unwrap().reason,
            RoomFeedGapReason::EpochReset,
            "an eviction in the replacement epoch must not hide the epoch reset"
        );
        assert!(
            room.read(
                Some(RoomFeedCursor {
                    epoch: room.feed_epoch,
                    sequence: room.next_sequence,
                }),
                0,
            )
            .unwrap_err()
            .to_string()
            .contains("ahead")
        );
    }
}
