// Included in rooms.rs. Small in-process evidence, never a second message queue.
// Every delivery reserves its complete target count before Message storage.
const ROOM_RECEIPT_EVIDENCE_MAX: usize = ROOM_MEMBER_MAX_COUNT;

#[derive(Clone, PartialEq, Eq)]
struct ReceiptEvidence {
    message_cursor: RoomFeedCursor,
    attempt_sequence: RoomSequence,
    pending: Option<RoomFeedCursor>,
    item: RoomFeedItem,
}

struct ReceiptEvidenceProof {
    evidence: ReceiptEvidence,
    message_seen: bool,
    pending_seen: bool,
    fetched: bool,
    later_receipt: bool,
    terminal_seen: bool,
}

impl ReceiptEvidenceProof {
    fn matches_repair(&self, event: &RoomFeedEvent) -> bool {
        let mut expected = self.evidence.item.clone();
        if self.fetched {
            if let RoomFeedItem::Delivery { zero_write_confirmed, .. } = &mut expected {
                *zero_write_confirmed = false;
            }
        }
        self.message_seen && (self.evidence.pending.is_none() || self.pending_seen)
            && event.cursor.sequence >= self.evidence.attempt_sequence && event.item == expected
    }

    fn observe(&mut self, event: &RoomFeedEvent) -> Result<()> {
        let RoomFeedItem::Delivery { message_id, recipient_id, run_id, generation, .. } = &self.evidence.item
            else { unreachable!() };
        match &event.item {
            RoomFeedItem::Message { message_id: id, recipient_ids, .. } if id == message_id => {
                anyhow::ensure!(!self.message_seen && event.cursor == self.evidence.message_cursor
                    && recipient_ids.contains(recipient_id), "receipt evidence Message identity changed");
                self.message_seen = true;
            }
            RoomFeedItem::Delivery { message_id: id, recipient_id: recipient, status,
                run_id: run, generation: observed_generation, .. } if id == message_id && recipient == recipient_id => {
                self.fetched |= *status == RoomDeliveryStatus::Fetched;
                if Some(event.cursor) == self.evidence.pending {
                    anyhow::ensure!(*status == RoomDeliveryStatus::Pending && run == run_id && observed_generation == generation,
                        "receipt evidence Pending identity changed");
                    self.pending_seen = true;
                }
                if event.cursor.sequence >= self.evidence.attempt_sequence {
                    self.later_receipt = true;
                    if matches!(status, RoomDeliveryStatus::Written | RoomDeliveryStatus::Woken | RoomDeliveryStatus::Failed) {
                        let mut expected = self.evidence.item.clone();
                        if self.fetched {
                            if let RoomFeedItem::Delivery { zero_write_confirmed, .. } = &mut expected {
                                *zero_write_confirmed = false;
                            }
                        }
                        anyhow::ensure!(event.item == expected,
                            "receipt evidence terminal outcome conflicts with history");
                        self.terminal_seen = true;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn repair(self, candidate: &RoomRuntime) -> Result<Option<RoomFeedItem>> {
        anyhow::ensure!(self.message_seen, "receipt evidence Message is missing from full history");
        let RoomFeedItem::Delivery { recipient_id, .. } = &self.evidence.item else { unreachable!() };
        if self.evidence.pending.is_some() {
            anyhow::ensure!(self.pending_seen, "receipt evidence admitted Pending is missing from full history");
            if self.terminal_seen { return Ok(None); }
            // This is the historical outcome of an already-authorized attempt.
            // Removing membership does not erase it or strand its exact run gate.
            let mut item = self.evidence.item;
            if self.fetched {
                if let RoomFeedItem::Delivery { zero_write_confirmed, .. } = &mut item {
                    *zero_write_confirmed = false;
                }
            }
            Ok(Some(item))
        } else {
            // Only positively known pretransport refusals get retry eligibility.
            // Full replay, never the bounded receipt map, proves no newer claim.
            if self.fetched || self.later_receipt
                || !candidate.definition.member_ids.contains(recipient_id)
                || self.evidence.message_cursor.sequence <= candidate.join_floor_by_session.get(recipient_id).copied().unwrap_or(0) {
                return Ok(None);
            }
            Ok(Some(self.evidence.item))
        }
    }
}

impl RoomRuntime {
    pub(crate) fn has_receipt_evidence(&self) -> bool { !self.receipt_evidence.is_empty() }

    #[cfg(test)]
    pub(crate) fn receipt_evidence_count_for_tests(&self) -> usize { self.receipt_evidence.len() }

    pub(crate) fn ensure_receipt_evidence_capacity(&self, count: usize) -> Result<()> {
        anyhow::ensure!(count <= ROOM_RECEIPT_EVIDENCE_MAX
            && self.receipt_reservations.saturating_add(self.receipt_evidence.len()).saturating_add(count)
                <= ROOM_RECEIPT_EVIDENCE_MAX,
            "room delivery evidence capacity is occupied; no new message or wake was attempted");
        if let Some(log) = &self.log {
            // A Pending plus a worst-case completion for every outstanding or
            // newly admitted recipient. Ordinary traffic cannot spend this gap.
            log.ensure_wake_headroom(2 * (self.receipt_reservations + self.receipt_evidence.len() + count))?;
        }
        Ok(())
    }

    pub(crate) fn retain_receipt_evidence(&mut self, message: &RoomFeedEvent,
        attempt_sequence: RoomSequence, pending: Option<RoomFeedCursor>, item: RoomFeedItem) {
        debug_assert!(self.receipt_reservations > 0);
        assert!(self.receipt_evidence.len() < ROOM_RECEIPT_EVIDENCE_MAX, "reserved receipt evidence capacity");
        self.receipt_evidence.push(ReceiptEvidence { message_cursor: message.cursor,
            attempt_sequence, pending, item });
        // Even a definite quota refusal can leave a known unsaved outcome.
        // Preserve the original error classification, but require replay/repair
        // before subsequent mutations rather than abandoning this evidence.
        self.log.as_mut().expect("delivery owns its room writer").require_receipt_recovery();
    }
}
