//! One locked writer per room. A successful append includes FlushFileBuffers
//! (`sync_all`); a write/sync failure poisons the writer until replay.
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::OnceLock,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use shared_types::{RoomFeedCursor, RoomFeedEvent, RoomFeedItem, RoomId};
use uuid::Uuid;

pub(crate) const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024 + 1024;
pub(crate) const MAX_LOG_BYTES: u64 = 256 * 1024 * 1024;
const RECEIPT_HEADROOM_BYTES: u64 = 4 * 1024 * 1024;
pub(crate) const ORDINARY_LOG_BYTES: u64 = MAX_LOG_BYTES - RECEIPT_HEADROOM_BYTES;
const MAX_RECOVERY_RECEIPTS: usize = 128;
// Existing display bound is 4096 characters plus its truncation marker.
pub(crate) const MAX_RECEIPT_ERROR_CHARS: usize = 4097;
const MAX_RECEIPT_TIMESTAMP_CHARS: usize = 64;

/// Derive the reserve from the exact serialized record shape, not a guessed
/// byte allowance. Control characters use JSON's longest six-byte escaping.
fn max_receipt_record_bytes() -> usize {
    static MAXIMUM: OnceLock<usize> = OnceLock::new();
    *MAXIMUM.get_or_init(|| {
        let event = RoomFeedEvent { schema_version: u32::MAX, room_id: Uuid::nil(),
            cursor: RoomFeedCursor { epoch: Uuid::nil(), sequence: u64::MAX },
            timestamp: "\0".repeat(MAX_RECEIPT_TIMESTAMP_CHARS),
            // Every optional field at its largest (v3 W3: pointer, readmit, already_read; W2 #2: the longest title state).
            item: RoomFeedItem::Delivery { app_server_turn: None, title_busy: Some("Action Required".into()), already_read: true, readmit: true, pointer: Some(Uuid::nil()), message_id: Uuid::nil(), recipient_id: Uuid::nil(),
                status: shared_types::RoomDeliveryStatus::Written, bytes_written: usize::MAX,
                zero_write_confirmed: false, error: Some("\0".repeat(MAX_RECEIPT_ERROR_CHARS)),
                run_id: Some(Uuid::nil()), generation: Some(u64::MAX) } };
        serde_json::to_vec(&Record::Event { event }).expect("bounded receipt record serializes").len() + 1
    })
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case", deny_unknown_fields)]
enum Record {
    Header { version: u32, room_id: RoomId, epoch: Uuid },
    Event { event: RoomFeedEvent },
}

pub(crate) struct RoomLog {
    file: File,
    pub(crate) epoch: Uuid,
    pub(crate) created_epoch: bool,
    bytes: u64,
    poisoned: bool,
    // Only the exact failed Message append can establish that no wake began.
    // Keep this small witness until candidate installation, including failures
    // while appending recovery receipts. It is not a retained-feed inference.
    uncertain_message: Option<(Uuid, RoomFeedCursor)>,
    #[cfg(test)]
    fault: Option<bool>, // false: partial write; true: failure after write, before sync
    #[cfg(test)]
    fail_replay_sync: bool,
    #[cfg(test)]
    write_and_sync_count: usize,
    #[cfg(test)]
    before_replay_io: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    quota_for_tests: Option<(u64, u64)>, // ordinary, hard; exercise real small files
    #[cfg(test)]
    fail_prefix_read: Option<bool>, // false: the read fails; true: restoring EOF fails too
}

#[derive(Debug)]
pub(crate) struct UncertainCommit(pub(crate) String);

impl std::fmt::Display for UncertainCommit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for UncertainCommit {}

/// This operation refused before a new append. A prior operation may still
/// have an uncertain outcome, so callers retain its request key for replay.
#[derive(Debug)]
pub(crate) struct RecoveryRequired(pub(crate) String);
impl std::fmt::Display for RecoveryRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for RecoveryRequired {}

/// Replay was valid, but its startup durability fence did not complete. The
/// caller retains the locked writer and keeps the parsed state unpublished.
#[derive(Debug, Clone)]
pub(crate) struct StartupSyncFailed(pub(crate) String);
impl std::fmt::Display for StartupSyncFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for StartupSyncFailed {}

pub(crate) struct StartupSyncWork {
    file: File,
    #[cfg(test)] fail_sync: bool,
    #[cfg(test)] before_sync: Option<Box<dyn FnOnce() + Send>>,
}

impl StartupSyncWork {
    pub(crate) fn run(&mut self) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.before_sync.take() { hook(); }
        #[cfg(test)]
        if std::mem::take(&mut self.fail_sync) {
            return Err(StartupSyncFailed("injected startup replay sync failure".into()).into());
        }
        self.file.sync_all().map_err(|error| StartupSyncFailed(
            format!("sync replayed room history before startup publication: {error}")))?;
        Ok(())
    }
}

/// Complete invalid history cannot be repaired by retrying the same operation.
/// Keep this typed through replay context so an owned writer can latch the hold.
#[derive(Debug)]
pub(crate) struct StructuralHistory(pub(crate) String);
impl std::fmt::Display for StructuralHistory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.0) }
}
impl std::error::Error for StructuralHistory {}

pub(crate) fn structural_history(message: impl std::fmt::Display) -> anyhow::Error {
    StructuralHistory(message.to_string()).into()
}

pub(crate) enum ReplayRecord {
    Header(Uuid),
    Event(RoomFeedEvent),
}

impl RoomLog {
    fn ordinary_quota(&self) -> u64 {
        #[cfg(test)]
        if let Some((ordinary, _)) = self.quota_for_tests { return ordinary; }
        ORDINARY_LOG_BYTES
    }

    fn hard_quota(&self) -> u64 {
        #[cfg(test)]
        if let Some((_, hard)) = self.quota_for_tests { return hard; }
        MAX_LOG_BYTES
    }

    pub(crate) fn ensure_wake_headroom(&self, receipt_count: usize) -> Result<()> {
        let maximum = max_receipt_record_bytes() as u64;
        anyhow::ensure!(maximum * MAX_RECOVERY_RECEIPTS as u64 <= RECEIPT_HEADROOM_BYTES,
            "receipt format exceeds reserved room headroom; wake was not attempted");
        anyhow::ensure!(receipt_count <= MAX_RECOVERY_RECEIPTS
            && self.bytes.saturating_add(maximum.saturating_mul(receipt_count as u64)) <= self.hard_quota(),
            "room receipt headroom is unavailable; no new message or wake was attempted");
        Ok(())
    }

    fn validate_receipt_bound(&self, event: &RoomFeedEvent) -> Result<()> {
        let RoomFeedItem::Delivery { error, .. } = &event.item else {
            bail!("receipt headroom cannot store ordinary room events");
        };
        anyhow::ensure!(error.as_ref().is_none_or(|error| error.chars().count() <= MAX_RECEIPT_ERROR_CHARS)
            && event.timestamp.chars().count() <= MAX_RECEIPT_TIMESTAMP_CHARS,
            "receipt metadata exceeds its reserved format bound");
        let bytes = serde_json::to_vec(&Record::Event { event: event.clone() })?.len() + 1;
        anyhow::ensure!(bytes <= max_receipt_record_bytes(), "receipt exceeds reserved serialized bound");
        Ok(())
    }

    fn encode_completion(&self, event: &RoomFeedEvent) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(RecoveryRequired("room history needs recovery before storing its known completion".into()).into());
        }
        anyhow::ensure!(matches!(event.item, RoomFeedItem::Delivery {
            status: shared_types::RoomDeliveryStatus::Written | shared_types::RoomDeliveryStatus::Woken
                | shared_types::RoomDeliveryStatus::Failed, .. }),
            "receipt headroom is only for an admitted wake's terminal outcome");
        self.validate_receipt_bound(event)?;
        self.encode_record_bytes(&Record::Event { event: event.clone() })
    }

    pub(crate) fn validate_completion(&self, event: &RoomFeedEvent) -> Result<()> {
        self.encode_completion(event).map(|_| ())
    }

    pub(crate) fn append_completion(&mut self, event: &RoomFeedEvent) -> Result<()> {
        let bytes = self.encode_completion(event)?;
        self.commit_bytes(&bytes)
    }

    #[cfg(test)]
    pub(crate) fn set_quota_for_tests(&mut self, ordinary: u64, hard: u64) {
        assert!(ordinary <= hard && hard <= MAX_LOG_BYTES);
        self.quota_for_tests = Some((ordinary, hard));
    }

    #[cfg(test)]
    pub(crate) fn bytes_for_tests(&self) -> u64 { self.bytes }

    #[cfg(test)]
    pub(crate) fn open(
        path: &Path,
        room_id: RoomId,
        mut replay: impl FnMut(RoomFeedEvent) -> Result<()>,
    ) -> Result<Self> {
        let (log, failure) = Self::open_with_header_sync(path, room_id, |record| match record {
            ReplayRecord::Header(_) => Ok(()),
            ReplayRecord::Event(event) => replay(event),
        }, |file| file.sync_all())?;
        if let Some(failure) = failure { return Err(failure.into()); }
        Ok(log)
    }

    pub(crate) fn open_with_header_sync(
        path: &Path,
        room_id: RoomId,
        mut replay: impl FnMut(ReplayRecord) -> Result<()>,
        mut final_sync: impl FnMut(&File) -> std::io::Result<()>,
    ) -> Result<(Self, Option<StartupSyncFailed>)> {
        fs::create_dir_all(path.parent().context("room log has no parent")?)?;
        let mut file = OpenOptions::new().read(true).write(true).create(true)
            .truncate(false).open(path).context("open room log")?;
        file.try_lock().map_err(|e| anyhow!("room log already owned or unavailable: {e}"))?;
        let bytes = file.metadata()?.len();
        if bytes > MAX_LOG_BYTES { bail!("room log exceeds its {MAX_LOG_BYTES} byte quota"); }
        if bytes == 0 {
            let epoch = Uuid::new_v4();
            let mut log = Self { file, epoch, created_epoch: true, bytes: 0, poisoned: false, uncertain_message: None,
                #[cfg(test)] fault: None, #[cfg(test)] fail_replay_sync: false,
                #[cfg(test)] write_and_sync_count: 0, #[cfg(test)] before_replay_io: None,
                #[cfg(test)] quota_for_tests: None, #[cfg(test)] fail_prefix_read: None };
            log.write_record(&Record::Header { version: 1, room_id, epoch })?;
            replay(ReplayRecord::Header(epoch))?;
            return Ok((log, None));
        }
        let (epoch, offset) = Self::read_locked(&mut file, path, room_id, None, &mut replay)?;
        let Some(epoch) = epoch else {
            // A torn header cannot have acknowledged messages behind it.
            drop(file);
            return Self::open_with_header_sync(path, room_id, replay, final_sync);
        };
        // A prior process may have written complete records without confirming
        // their sync. Startup publishes no recovered Stored result before this
        // same durability fence used by in-process recovery succeeds.
        file.seek(SeekFrom::End(0))?;
        let failure = final_sync(&file).err().map(|error| StartupSyncFailed(
            format!("sync replayed room history before startup publication: {error}")));
        Ok((Self { file, epoch, created_epoch: false, bytes: offset, poisoned: false, uncertain_message: None,
            #[cfg(test)] fault: None, #[cfg(test)] fail_replay_sync: false,
            #[cfg(test)] write_and_sync_count: 0, #[cfg(test)] before_replay_io: None,
            #[cfg(test)] quota_for_tests: None, #[cfg(test)] fail_prefix_read: None }, failure))
    }

    pub(crate) fn prepare_startup_sync(&mut self) -> Result<StartupSyncWork> {
        Ok(StartupSyncWork {
            file: self.file.try_clone().context("duplicate held startup history handle")?,
            #[cfg(test)] fail_sync: std::mem::take(&mut self.fail_replay_sync),
            #[cfg(test)] before_sync: self.before_replay_io.take(),
        })
    }

    fn read_locked(
        file: &mut File, path: &Path, room_id: RoomId, expected_epoch: Option<Uuid>,
        mut replay: impl FnMut(ReplayRecord) -> Result<()>,
    ) -> Result<(Option<Uuid>, u64)> {
        if file.metadata()?.len() > MAX_LOG_BYTES {
            return Err(structural_history(format!("room log exceeds its {MAX_LOG_BYTES} byte quota")));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut epoch = None;
        let mut offset = 0u64;
        let mut sequence = 0u64;
        let mut reader = BufReader::new(file.try_clone()?);
        loop {
            let mut line = Vec::new();
            // Bound even a damaged line before allocating unbounded memory.
            let count = reader.by_ref().take((MAX_RECORD_BYTES + 1) as u64).read_until(b'\n', &mut line)?;
            if count == 0 { break; }
            if count > MAX_RECORD_BYTES {
                return Err(structural_history(format!("room log record exceeds its size bound at byte {offset}")));
            }
            if line.last() != Some(&b'\n') {
                if offset == 0 && expected_epoch.is_some() {
                    return Err(structural_history("locked room history lost its complete header; explicit recovery is required"));
                }
                // Only an unterminated final record can be repaired. Preserve
                // its exact bytes first; malformed complete records fail closed.
                let tail = path.with_extension(format!("torn-{}.bin", Uuid::new_v4()));
                let mut evidence = OpenOptions::new().create_new(true).write(true).open(tail)?;
                evidence.write_all(&line)?;
                evidence.sync_all()?;
                file.set_len(offset)?;
                file.sync_all()?;
                break;
            }
            let record: Record = serde_json::from_slice(&line).map_err(|error|
                structural_history(format!("invalid complete room log record at byte {offset}: {error}")))?;
            match record {
                Record::Header { version: 1, room_id: id, epoch: value }
                    if offset == 0 && id == room_id => {
                        if expected_epoch.is_some_and(|expected| expected != value) {
                            return Err(structural_history("locked room history epoch changed; explicit recovery is required"));
                        }
                        epoch = Some(value);
                        replay(ReplayRecord::Header(value))?;
                    }
                Record::Event { event } if epoch.is_some() => {
                    if event.room_id != room_id || Some(event.cursor.epoch) != epoch
                        || event.cursor.sequence != sequence + 1
                        || event.schema_version != shared_types::ROOM_EVENT_SCHEMA_VERSION {
                        return Err(structural_history(format!("room log identity/schema/sequence mismatch at byte {offset}")));
                    }
                    sequence = event.cursor.sequence;
                    replay(ReplayRecord::Event(event))?;
                }
                _ => return Err(structural_history(format!("invalid room log header/order at byte {offset}"))),
            }
            offset += count as u64;
        }
        drop(reader);
        Ok((epoch, offset))
    }

    pub(crate) fn needs_replay(&self) -> bool { self.poisoned }

    /// Copy the acknowledged prefix through this writer's own handle (its lock
    /// refuses a second reader on Windows). Appends write at the current
    /// position without seeking, so every path restores it to the committed
    /// end; if that restore fails the writer is poisoned (held for replay),
    /// never left appendable at a possibly wrong position.
    pub(crate) fn read_committed_prefix(&mut self) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(RecoveryRequired("room history needs recovery before it can be copied".into()).into());
        }
        let length = self.bytes;
        #[cfg(test)]
        let fault = self.fail_prefix_read.take();
        let read: Result<Vec<u8>> = (|| {
            self.file.seek(SeekFrom::Start(0))?;
            #[cfg(test)]
            if fault.is_some() { bail!("injected room history copy failure"); }
            let mut bytes = Vec::with_capacity(usize::try_from(length)?);
            (&self.file).take(length).read_to_end(&mut bytes)?;
            anyhow::ensure!(bytes.len() as u64 == length, "room history ended before its committed length");
            Ok(bytes)
        })();
        let restored = self.file.seek(SeekFrom::Start(length)).map(|_| ());
        #[cfg(test)]
        let restored = if fault == Some(true) { Err(std::io::Error::other("injected append position restore failure")) } else { restored };
        if let Err(error) = restored {
            self.poisoned = true;
            return Err(RecoveryRequired(format!(
                "room history append position could not be restored after copying ({error}); the room is held until its history is replayed")).into());
        }
        read
    }

    #[cfg(test)]
    pub(crate) fn fail_prefix_read_for_tests(&mut self, restore_fails: bool) {
        self.fail_prefix_read = Some(restore_fails);
    }

    /// A known unsaved receipt also needs private replay/repair. This changes
    /// writer admission, not the definite/uncertain classification of its error.
    pub(crate) fn require_receipt_recovery(&mut self) { self.poisoned = true; }

    /// Duplicate the owned handle, never reopen/relock the pathname. The live
    /// writer stays installed and poisoned while this private copy does I/O.
    pub(crate) fn clone_for_recovery(&mut self) -> Result<Self> {
        anyhow::ensure!(self.poisoned, "only a poisoned writer can prepare recovery");
        let file = self.file.try_clone().context("duplicate owned room history handle")?;
        Ok(Self { file, epoch: self.epoch, created_epoch: self.created_epoch,
            bytes: self.bytes, poisoned: true, uncertain_message: self.uncertain_message,
            #[cfg(test)] fault: self.fault.take(),
            #[cfg(test)] fail_replay_sync: std::mem::take(&mut self.fail_replay_sync),
            #[cfg(test)] write_and_sync_count: self.write_and_sync_count,
            #[cfg(test)] before_replay_io: self.before_replay_io.take(),
            #[cfg(test)] quota_for_tests: self.quota_for_tests, #[cfg(test)] fail_prefix_read: None })
    }

    /// The clone has synced, restored EOF and passed semantic validation. Keep
    /// this original handle/lock; publishing its new accounting requires no I/O.
    pub(crate) fn accept_recovered_writer(&mut self, recovered: Self) {
        debug_assert!(self.poisoned && recovered.poisoned && self.epoch == recovered.epoch);
        self.bytes = recovered.bytes;
        self.created_epoch = recovered.created_epoch;
        #[cfg(test)] {
            self.fault = recovered.fault;
            self.fail_replay_sync = recovered.fail_replay_sync;
            self.write_and_sync_count = recovered.write_and_sync_count;
        }
        self.accept_replay();
    }

    /// Retain the writer lock while rebuilding a separate runtime candidate.
    /// A complete uncertain record is evidence, never a truncation boundary.
    /// The caller must not publish that candidate unless this final sync passes.
    pub(crate) fn replay_poisoned(
        &mut self, path: &Path, room_id: RoomId,
        replay: impl FnMut(ReplayRecord) -> Result<()>,
    ) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = self.before_replay_io.take() { hook(); }
        let outcome: Result<()> = (|| {
            let (epoch, bytes) = Self::read_locked(&mut self.file, path, room_id, Some(self.epoch), replay)?;
            if epoch != Some(self.epoch) {
                return Err(structural_history("locked room history epoch changed; explicit recovery is required"));
            }
            #[cfg(test)]
            if std::mem::take(&mut self.fail_replay_sync) {
                bail!("injected room replay sync failure");
            }
            self.file.sync_all().context("sync replayed room history")?;
            self.file.seek(SeekFrom::End(0))?;
            self.bytes = bytes;
            self.created_epoch = false;
            Ok(())
        })();
        match outcome {
            Ok(()) => Ok(()),
            Err(error) => {
                self.poisoned = true;
                if error.is::<StructuralHistory>() {
                    Err(error.context("room history recovery remains held"))
                } else {
                    Err(RecoveryRequired(format!("room history recovery remains held: {error:#}")).into())
                }
            }
        }
    }

    /// The replayed candidate passed semantic validation and is being installed.
    pub(crate) fn accept_replay(&mut self) {
        self.poisoned = false;
        self.uncertain_message = None;
    }

    pub(crate) fn uncertain_message(&self) -> Option<(Uuid, RoomFeedCursor)> {
        self.uncertain_message
    }

    pub(crate) fn validate_append(&self, event: &RoomFeedEvent) -> Result<()> {
        self.encode_record(&Record::Event { event: event.clone() }).map(|_| ())
    }

    pub(crate) fn append(&mut self, event: &RoomFeedEvent) -> Result<()> {
        let result = self.write_record(&Record::Event { event: event.clone() });
        if result.as_ref().is_err_and(|error| error.downcast_ref::<UncertainCommit>().is_some()) {
            if let RoomFeedItem::Message { message_id, .. } = &event.item {
                self.uncertain_message = Some((*message_id, event.cursor));
            }
        }
        result
    }

    /// Ordinary JSONL records share one sync. A failed write can leave a complete
    /// prefix for replay; callers publish none of the batch until this succeeds.
    pub(crate) fn append_batch(&mut self, events: &[RoomFeedEvent]) -> Result<()> {
        self.append_batch_inner(events, false)
    }

    /// Repair only a privately replayed candidate. The live writer remains
    /// poisoned even on success, until the caller installs that candidate.
    pub(crate) fn append_recovery_batch(&mut self, events: &[RoomFeedEvent]) -> Result<()> {
        anyhow::ensure!(self.poisoned, "recovery receipts require a poisoned writer");
        anyhow::ensure!(events.len() <= MAX_RECOVERY_RECEIPTS && events.iter().all(|event| matches!(event.item,
            RoomFeedItem::Delivery { status: shared_types::RoomDeliveryStatus::Held,
                bytes_written: 0, zero_write_confirmed: true, .. }
            | RoomFeedItem::Delivery { status: shared_types::RoomDeliveryStatus::Written
                | shared_types::RoomDeliveryStatus::Woken | shared_types::RoomDeliveryStatus::Failed, .. })),
            "recovery can append only bounded proven Held or terminal receipts");
        self.append_batch_inner(events, true)
    }

    fn append_batch_inner(&mut self, events: &[RoomFeedEvent], recovery: bool) -> Result<()> {
        if events.is_empty() { return Ok(()); }
        let mut bytes = Vec::new();
        let mut total_bytes = self.bytes;
        for event in events {
            let record = Record::Event { event: event.clone() };
            let encoded = if recovery {
                self.validate_receipt_bound(event)?;
                self.encode_record_bytes(&record)?
            } else { self.encode_record(&record)? };
            total_bytes = total_bytes.checked_add(encoded.len() as u64)
                .filter(|total| *total <= (if recovery { self.hard_quota() } else { self.ordinary_quota() }))
                .ok_or_else(|| anyhow!(if recovery { "room receipt recovery quota exhausted; history remains held and no repaired state was published" }
                    else { "room ordinary log quota reached; batch was not stored; receipt headroom is reserved" }))?;
            bytes.extend_from_slice(&encoded);
        }
        self.commit_bytes(&bytes)
    }

    #[cfg(test)]
    pub(crate) fn fail_commit_for_tests(&mut self, after_write: bool) {
        self.fault = Some(after_write);
    }

    #[cfg(test)]
    pub(crate) fn fail_replay_sync_for_tests(&mut self) { self.fail_replay_sync = true; }

    #[cfg(test)]
    pub(crate) fn before_replay_io_for_tests(&mut self, hook: impl FnOnce() + Send + 'static) {
        self.before_replay_io = Some(Box::new(hook));
    }

    #[cfg(test)]
    pub(crate) fn exhaust_quota_for_tests(&mut self) { self.bytes = MAX_LOG_BYTES; }

    #[cfg(test)]
    pub(crate) fn write_and_sync_count_for_tests(&self) -> usize { self.write_and_sync_count }

    #[cfg(test)]
    pub(crate) fn read_owned_for_tests(&mut self) -> Vec<u8> {
        self.file.seek(SeekFrom::Start(0)).unwrap();
        let mut bytes = Vec::new();
        self.file.read_to_end(&mut bytes).unwrap();
        self.file.seek(SeekFrom::End(0)).unwrap();
        bytes
    }

    #[cfg(test)]
    pub(crate) fn append_invalid_complete_record_for_tests(&mut self) {
        self.file.seek(SeekFrom::End(0)).unwrap();
        self.file.write_all(b"{invalid complete record}\n").unwrap();
        self.file.sync_all().unwrap();
        self.poisoned = true;
    }

    fn encode_record(&self, record: &Record) -> Result<Vec<u8>> {
        if self.poisoned {
            return Err(RecoveryRequired("room log needs replay of an earlier uncertain write; no new record was attempted".into()).into());
        }
        if let Record::Event { event } = record {
            if matches!(event.item, RoomFeedItem::Delivery { status: shared_types::RoomDeliveryStatus::Pending, .. }) {
                self.validate_receipt_bound(event)?;
            }
        }
        let bytes = self.encode_record_bytes(record)?;
        if self.bytes.saturating_add(bytes.len() as u64) > self.ordinary_quota() {
            bail!("room ordinary log quota reached; record was not stored; receipt headroom is reserved");
        }
        Ok(bytes)
    }

    fn encode_record_bytes(&self, record: &Record) -> Result<Vec<u8>> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_RECORD_BYTES || self.bytes.saturating_add(bytes.len() as u64) > self.hard_quota() {
            bail!("room log quota reached; record was not stored");
        }
        Ok(bytes)
    }

    fn write_record(&mut self, record: &Record) -> Result<()> {
        let bytes = self.encode_record(record)?;
        self.commit_bytes(&bytes)
    }

    fn commit_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        if let Err(error) = self.write_and_sync(bytes) {
            self.poisoned = true;
            return Err(UncertainCommit(format!("room log commit uncertain; no Stored acknowledgement: {error}")).into());
        }
        self.bytes += bytes.len() as u64;
        Ok(())
    }

    fn write_and_sync(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        #[cfg(test)]
        { self.write_and_sync_count += 1; }
        #[cfg(test)]
        if let Some(after_write) = self.fault.take() {
            self.file.write_all(if after_write { bytes } else { &bytes[..bytes.len() / 2] })?;
            return Err(std::io::Error::other("injected room commit I/O failure"));
        }
        self.file.write_all(bytes)?;
        self.file.sync_all()
    }
}

/// One complete log line, parsed with the writer's own record schema. A
/// header also returns the room it names.
pub(crate) fn parse_record_line(line: &[u8]) -> Result<(ReplayRecord, Option<RoomId>)> {
    Ok(match serde_json::from_slice::<Record>(line)? {
        Record::Header { version: 1, room_id, epoch } => (ReplayRecord::Header(epoch), Some(room_id)),
        Record::Header { version, .. } => bail!("unsupported room log header version {version}"),
        Record::Event { event } => (ReplayRecord::Event(event), None),
    })
}

/// Encode a header or event exactly as the writer does, newline included.
pub(crate) fn encode_record_line(room_id: RoomId, record: &ReplayRecord) -> Result<Vec<u8>> {
    let mut bytes = match record {
        ReplayRecord::Header(epoch) => serde_json::to_vec(&Record::Header { version: 1, room_id, epoch: *epoch })?,
        ReplayRecord::Event(event) => serde_json::to_vec(&Record::Event { event: event.clone() })?,
    };
    bytes.push(b'\n');
    anyhow::ensure!(bytes.len() <= MAX_RECORD_BYTES, "room log record exceeds its size bound");
    Ok(bytes)
}

/// Delete only files this room writer creates. No recursive deletion, broad
/// glob, directory/reparse traversal or implicit cleanup of other rooms.
pub(crate) fn remove_room_files(runtime_dir: &Path, room_id: RoomId) -> Result<()> {
    let directory = runtime_dir.join("rooms");
    let metadata = match fs::symlink_metadata(&directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).context("inspect room history directory"),
    };
    if !metadata.is_dir() || is_reparse_or_symlink(&metadata) {
        bail!("refusing non-directory or reparse room history path: {}", directory.display());
    }
    let resolved = fs::canonicalize(&directory)?;
    let runtime = fs::canonicalize(runtime_dir)?;
    if resolved.parent() != Some(runtime.as_path()) {
        bail!("room history directory is outside its runtime parent");
    }
    let mut failures = Vec::new();
    let mut failure_count = 0usize;
    let mut record_failure = |result: Result<()>| {
        if let Err(error) = result {
            failure_count += 1;
            if failures.len() < 8 { failures.push(format!("{error:#}")); }
        }
    };
    record_failure(remove_regular_room_file(&resolved.join(format!("{room_id}.jsonl"))));
    let prefix = format!("{room_id}.torn-");
    for entry in fs::read_dir(&resolved).context("enumerate this room's recovery fragments")? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => { record_failure(Err(error).context("read room history entry")); continue; }
        };
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue; };
        let Some(id) = name.strip_prefix(&prefix).and_then(|rest| rest.strip_suffix(".bin")) else { continue; };
        let Ok(uuid) = Uuid::parse_str(id) else { continue; };
        if uuid.to_string() != id { continue; }
        record_failure(remove_regular_room_file(&entry.path()));
    }
    if failure_count > 0 {
        bail!("{failure_count} room history file(s) remain: {}", failures.join("; "));
    }
    Ok(())
}

fn is_reparse_or_symlink(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0 // FILE_ATTRIBUTE_REPARSE_POINT
    }
    #[cfg(not(windows))]
    { metadata.file_type().is_symlink() }
}

fn remove_regular_room_file(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error).with_context(|| format!("inspect {}", path.display())),
    };
    if !metadata.is_file() || is_reparse_or_symlink(&metadata) {
        bail!("refusing non-regular or reparse history entry: {}", path.display());
    }
    fs::remove_file(path).with_context(|| format!("erase {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{RoomFeedCursor, RoomFeedItem, RoomMembershipAction, now_rfc3339};

    fn event(room_id: RoomId, epoch: Uuid, sequence: u64) -> RoomFeedEvent {
        RoomFeedEvent { schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION, room_id,
            cursor: RoomFeedCursor { epoch, sequence }, timestamp: now_rfc3339(),
            item: RoomFeedItem::Membership { action: RoomMembershipAction::Joined,
                session_id: Uuid::new_v4(), membership_revision: 1 } }
    }

    fn read_owned(log: &mut RoomLog) -> Vec<u8> {
        log.read_owned_for_tests()
    }

    fn receipt(room_id: RoomId, epoch: Uuid, sequence: u64,
        status: shared_types::RoomDeliveryStatus, error: Option<String>) -> RoomFeedEvent {
        RoomFeedEvent { schema_version: u32::MAX, room_id,
            cursor: RoomFeedCursor { epoch, sequence }, timestamp: "\0".repeat(MAX_RECEIPT_TIMESTAMP_CHARS),
            item: RoomFeedItem::Delivery { app_server_turn: None, title_busy: Some("Action Required".into()), already_read: true, readmit: true, pointer: Some(Uuid::nil()), message_id: Uuid::nil(), recipient_id: Uuid::nil(), status,
                bytes_written: usize::MAX, zero_write_confirmed: false, error,
                run_id: Some(Uuid::nil()), generation: Some(u64::MAX) } }
    }

    #[test]
    fn reserved_receipt_bound_covers_json_escaping_and_rejects_unbounded_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let log = RoomLog::open(&dir.path().join("room.jsonl"), id, |_| Ok(())).unwrap();
        for status in [shared_types::RoomDeliveryStatus::Written, shared_types::RoomDeliveryStatus::Woken,
            shared_types::RoomDeliveryStatus::Failed] {
            for character in ['\0', '"', '\\', '\u{1f642}'] {
                let event = receipt(id, log.epoch, u64::MAX, status,
                    Some(character.to_string().repeat(MAX_RECEIPT_ERROR_CHARS)));
                log.validate_completion(&event).unwrap();
                assert!(serde_json::to_vec(&Record::Event { event }).unwrap().len() + 1 <= max_receipt_record_bytes());
            }
        }
        assert!(max_receipt_record_bytes() as u64 * MAX_RECOVERY_RECEIPTS as u64 <= RECEIPT_HEADROOM_BYTES);
        let oversized = receipt(id, log.epoch, 1, shared_types::RoomDeliveryStatus::Failed,
            Some("x".repeat(MAX_RECEIPT_ERROR_CHARS + 1)));
        assert!(log.validate_completion(&oversized).unwrap_err().to_string().contains("format bound"));
        let mut timestamp = receipt(id, log.epoch, 1, shared_types::RoomDeliveryStatus::Failed, None);
        timestamp.timestamp.push('x');
        assert!(log.validate_completion(&timestamp).unwrap_err().to_string().contains("format bound"));
    }

    #[test]
    fn receipt_headroom_is_reserved_for_admitted_completion_not_observation_or_new_claims() {
        use shared_types::RoomDeliveryStatus;
        let dir = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let mut log = RoomLog::open(&dir.path().join("room.jsonl"), id, |_| Ok(())).unwrap();
        let pending = receipt(id, log.epoch, 1, RoomDeliveryStatus::Pending, None);
        let ordinary = log.bytes + serde_json::to_vec(&Record::Event { event: pending.clone() }).unwrap().len() as u64 + 1;
        log.set_quota_for_tests(ordinary, ordinary + RECEIPT_HEADROOM_BYTES);
        log.ensure_wake_headroom(MAX_RECOVERY_RECEIPTS).unwrap();
        log.append(&pending).unwrap();
        assert_eq!(log.bytes, ordinary);
        let writes = log.write_and_sync_count_for_tests();
        for status in [RoomDeliveryStatus::Fetched, RoomDeliveryStatus::Held, RoomDeliveryStatus::Pending] {
            let event = receipt(id, log.epoch, 2, status, None);
            assert!(log.append(&event).unwrap_err().to_string().contains("ordinary log quota"));
            assert!(log.append_completion(&event).is_err(), "observations cannot claim completion headroom");
        }
        assert_eq!(log.write_and_sync_count_for_tests(), writes);
        let outcome = receipt(id, log.epoch, 2, RoomDeliveryStatus::Failed,
            Some("\0".repeat(MAX_RECEIPT_ERROR_CHARS)));
        log.append_completion(&outcome).unwrap();
        assert!(log.bytes > ordinary && log.bytes < log.hard_quota());
        let before = log.bytes;
        log.set_quota_for_tests(ordinary, before + max_receipt_record_bytes() as u64 - 1);
        assert!(log.ensure_wake_headroom(1).unwrap_err().to_string().contains("no new message or wake"));
        assert_eq!(log.bytes, before);
        assert!(!log.needs_replay(), "quota admission is a definite zero-effect refusal");
    }

    #[test]
    fn startup_replay_requires_sync_before_acceptance_and_appends_without_an_inspection_seek() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
        let epoch = log.epoch;
        log.fail_commit_for_tests(true);
        assert!(log.append_batch(&[event(id, epoch, 1), event(id, epoch, 2)]).is_err());
        drop(log);
        let original = fs::read(&path).unwrap();
        let mut seen = Vec::new();
        let (mut log, failure) = RoomLog::open_with_header_sync(&path, id,
            |record| { if let ReplayRecord::Event(event) = record { seen.push(event); } Ok(()) },
            |_| Err(std::io::Error::other("injected startup replay sync failure"))).unwrap();
        let error = failure.unwrap();
        assert_eq!(seen.len(), 2, "complete records alone do not make startup successful");
        assert!(format!("{error:#}").contains("injected startup replay sync failure"));
        assert_eq!(log.file.stream_position().unwrap(), original.len() as u64,
            "the retained writer is at EOF before any inspection helper seeks");
        assert_eq!(read_owned(&mut log), original, "failed durability fence preserves all bytes");
        assert!(RoomLog::open(&path, id, |_| Ok(())).is_err(), "fence hold retains the exclusive writer");
        log.prepare_startup_sync().unwrap().run().unwrap();
        log.append(&event(id, epoch, 3)).unwrap(); // No helper can restore EOF for this append.
        drop(log);
        let mut reopened = Vec::new();
        let _log = RoomLog::open(&path, id, |event| { reopened.push(event); Ok(()) }).unwrap();
        assert_eq!(reopened.iter().map(|event| event.cursor.sequence).collect::<Vec<_>>(), vec![1, 2, 3]);
    }

    #[test]
    fn batch_aggregate_quota_refuses_before_io_without_poisoning() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
        let events = [event(id, log.epoch, 1), event(id, log.epoch, 2)];
        let lengths = events.iter().map(|event|
            log.encode_record(&Record::Event { event: event.clone() }).unwrap().len() as u64
        ).collect::<Vec<_>>();
        let original = read_owned(&mut log);
        let committed_bytes = log.bytes;
        let initial_writes = log.write_and_sync_count_for_tests();
        log.bytes = ORDINARY_LOG_BYTES - lengths.iter().copied().max().unwrap();
        let quota_bytes = log.bytes;
        for event in &events { log.validate_append(event).unwrap(); }
        log.fail_commit_for_tests(true);
        log.append_batch(&[]).unwrap();
        assert_eq!(log.fault, Some(true), "an empty batch performs no write or sync");
        assert_eq!(log.write_and_sync_count_for_tests(), initial_writes);
        let error = log.append_batch(&events).unwrap_err();
        assert!(error.to_string().contains("quota"));
        assert!(!error.is::<UncertainCommit>() && !error.is::<RecoveryRequired>());
        assert_eq!(log.fault, Some(true), "quota refusal must happen before any I/O");
        assert_eq!(log.write_and_sync_count_for_tests(), initial_writes);
        assert_eq!(log.bytes, quota_bytes);
        assert_eq!(read_owned(&mut log), original);
        assert!(!log.needs_replay());

        // Restore the simulated quota accounting and prove the writer remains usable.
        log.bytes = committed_bytes;
        log.fault = None;
        log.append_batch(&events).unwrap();
        assert_eq!(log.write_and_sync_count_for_tests(), initial_writes + 1,
            "all records in a successful batch share one write and sync");
        assert_eq!(log.bytes, committed_bytes + lengths.iter().sum::<u64>());
        drop(log);
        let mut replayed = Vec::new();
        let _log = RoomLog::open(&path, id, |event| { replayed.push(event); Ok(()) }).unwrap();
        assert_eq!(replayed, events);
    }

    #[test]
    fn replay_after_durable_commit_with_lost_reply() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
        let expected = event(id, log.epoch, 1);
        log.append(&expected).unwrap();
        drop(log); // The caller never received an acknowledgement.
        let mut replay = Vec::new();
        let log = RoomLog::open(&path, id, |e| { replay.push(e); Ok(()) }).unwrap();
        assert_eq!(replay, [expected]);
        assert_eq!(log.epoch, replay[0].cursor.epoch);
        assert!(RoomLog::open(&path, id, |_| Ok(())).is_err(),
            "closing the replay reader must not release the writer lock");
    }

    #[test]
    fn torn_tail_is_preserved_then_repaired_before_next_append() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
        log.append(&event(id, log.epoch, 1)).unwrap();
        drop(log);
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"{\"record\":").unwrap();
        let mut seen = Vec::new();
        let mut log = RoomLog::open(&path, id, |e| { seen.push(e); Ok(()) }).unwrap();
        assert_eq!(seen.len(), 1);
        log.append(&event(id, log.epoch, 2)).unwrap();
        drop(log);
        let mut seen = Vec::new();
        let _log = RoomLog::open(&path, id, |e| { seen.push(e); Ok(()) }).unwrap();
        assert_eq!(seen.len(), 2);
        let tail = fs::read_dir(dir.path()).unwrap().map(|e| e.unwrap().path())
            .find(|p| p.extension().is_some_and(|x| x == "bin")).unwrap();
        assert_eq!(fs::read(tail).unwrap(), b"{\"record\":");
    }

    #[test]
    fn no_append_has_no_message_and_second_writer_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        let log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
        assert!(RoomLog::open(&path, id, |_| Ok(())).is_err());
        drop(log);
        let _log = RoomLog::open(&path, id, |_| panic!("no message committed")).unwrap();
    }

    #[test]
    fn complete_corruption_is_not_silently_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("room.jsonl");
        let id = Uuid::new_v4();
        drop(RoomLog::open(&path, id, |_| Ok(())).unwrap());
        OpenOptions::new().append(true).open(&path).unwrap().write_all(b"broken\n").unwrap();
        let before = fs::read(&path).unwrap();
        assert!(RoomLog::open(&path, id, |_| Ok(())).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn uncertain_write_or_sync_never_acknowledges_or_allows_another_append() {
        for after_write in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("room.jsonl");
            let id = Uuid::new_v4();
            let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
            let event = event(id, log.epoch, 1);
            log.fault = Some(after_write);
            assert!(log.append(&event).unwrap_err().to_string().contains("no Stored"));
            let uncertain_len = log.file.metadata().unwrap().len();
            assert!(log.append(&event).unwrap_err().to_string().contains("uncertain"));
            assert_eq!(log.file.metadata().unwrap().len(), uncertain_len);
            drop(log);
            let mut recovered = Vec::new();
            let _log = RoomLog::open(&path, id, |e| { recovered.push(e); Ok(()) }).unwrap();
            assert_eq!(recovered.len(), usize::from(after_write));
        }
    }

    #[test]
    fn poisoned_replay_keeps_writer_lock_and_syncs_complete_records_before_acceptance() {
        for after_write in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("room.jsonl");
            let id = Uuid::new_v4();
            let mut log = RoomLog::open(&path, id, |_| Ok(())).unwrap();
            log.append(&event(id, log.epoch, 1)).unwrap();
            let boundary = log.bytes as usize;
            log.fail_commit_for_tests(after_write);
            assert!(log.append(&event(id, log.epoch, 2)).is_err());
            let original = read_owned(&mut log);
            log.fail_replay_sync_for_tests();
            assert!(log.replay_poisoned(&path, id, |_| Ok(())).is_err());
            assert!(log.needs_replay());
            let mut replayed = Vec::new();
            log.replay_poisoned(&path, id, |record| {
                if let ReplayRecord::Event(event) = record { replayed.push(event); }
                Ok(())
            }).unwrap();
            assert_eq!(replayed.len(), 1 + usize::from(after_write));
            assert!(RoomLog::open(&path, id, |_| Ok(())).is_err(),
                "replay must never release the original writer lock");
            assert!(log.needs_replay(), "candidate validation still owns acceptance");
            if after_write {
                assert_eq!(read_owned(&mut log), original, "retain the complete uncertain record");
            } else {
                let tails: Vec<_> = fs::read_dir(dir.path()).unwrap().map(|entry| entry.unwrap().path())
                    .filter(|path| path.extension().is_some_and(|extension| extension == "bin")).collect();
                assert_eq!(tails.len(), 1);
                assert_eq!(fs::read(&tails[0]).unwrap(), original[boundary..]);
                assert_eq!(read_owned(&mut log), original[..boundary]);
            }
            log.accept_replay();
            log.append(&event(id, log.epoch, replayed.len() as u64 + 1)).unwrap();
        }
    }
}
