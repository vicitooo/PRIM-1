//! v3 W5: an outgoing spool for an outside agent on another computer
//! (`--prim1-guest spool <outbox> <inbox>`). Envelopes are data files in a
//! local outbox; each becomes one existing `GuestRequest`, and its outcome is
//! written beside it in the inbox. Moving complete files between computers is
//! the existing mesh's job, never this one's (W5-DECISIONS "Cross-PC outgoing
//! spool"). Nothing is deleted; the inbox result is the ledger. The guest
//! token stays on this computer: no envelope or result may carry one.
//! An envelope name is used once (Reviewer b1ecd212): its result records the
//! SHA-256 of the bytes that were sent, and a file changed under the same name
//! is refused visibly, never sent as a new request or as the old one.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use shared_types::{GuestRequest, GuestResponse, GuestResponsePayload, MessageId, RoomFeedCursor};

use super::guest::GuestClient;

const SPOOL_VERSION: u32 = 1;
const ENVELOPE_MAX_BYTES: u64 = 64 * 1024;
const NAME_MAX_CHARS: usize = 96;
const REQUEST_ID_MAX_CHARS: usize = 128;
pub(crate) const RETRY_MAX_SECS: u64 = 3600;
const BACKOFF_SECS: [u64; 6] = [1, 2, 4, 8, 16, 30];
const RESULT_SUFFIX: &str = ".result.json";
const WAITING_BEHIND: &str = "waiting_behind";
/// The pending result written before a send; any outcome replaces it.
const SENDING: &str = "sending";

/// One envelope as written by the outside agent: the op and only its fields.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    v: u32,
    op: String,
    request_id: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    message_id: Option<MessageId>,
    #[serde(default)]
    next: Option<String>,
    #[serde(default)]
    decline: Option<String>,
    #[serde(default)]
    cursor: Option<RoomFeedCursor>,
    /// v3 W5: read only; a previous page's `retained_next`.
    #[serde(default)]
    owed_after: Option<RoomFeedCursor>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpoolState { Stored, Answered, Refused, Pending }

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct SpoolResult {
    pub(crate) v: u32,
    pub(crate) envelope: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) request_id: Option<String>,
    pub(crate) state: SpoolState,
    /// `invalid_envelope`, `refused`, `prim1_down`, `retry_later`, `waiting_behind`, `sending`,
    /// `changed_envelope` or `envelope_unreadable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    pub(crate) attempts: u32,
    pub(crate) at_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) next_attempt_in_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) response: Option<GuestResponse>,
    /// v3 W5 (b1ecd212): the SHA-256 of the envelope bytes this result is
    /// for, once they were read; absent for results of older spools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) envelope_sha256: Option<String>,
    /// v3 W5 (b1ecd212): set on a finished result when the file under its
    /// name now holds other bytes (their SHA-256); the change was not sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) changed_sha256: Option<String>,
}

#[derive(Debug, Default, Serialize, PartialEq, Eq)]
pub(crate) struct SpoolSummary {
    pub(crate) ok: bool,
    pub(crate) stored: u32,
    pub(crate) answered: u32,
    pub(crate) refused: u32,
    pub(crate) already_done: u32,
    /// v3 W5 (b1ecd212): envelopes whose file changed under a used name; not sent.
    pub(crate) changed: u32,
    /// v3 W5 (bf2cb722): finished envelopes whose current file could not be
    /// compared with what was sent (unreadable, over the limit, or a result
    /// from an older spool without a digest); their results stand, nothing sent.
    pub(crate) unverifiable: u32,
    /// The envelope that holds the ones after it, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) pending: Option<String>,
    pub(crate) message: String,
}

fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= NAME_MAX_CHARS
        && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !name.starts_with('.')
}

fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= REQUEST_ID_MAX_CHARS
        && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | ':' | '-'))
}

/// A guest credential in any form: the token prefix in the bytes or in any decoded string.
fn carries_credential(text: &str) -> bool { text.to_ascii_lowercase().contains(shared_types::GUEST_TOKEN_PREFIX) }

fn now_unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or_default()
}

/// The envelope's request, or why it is not one. Only the op's own fields may be present.
fn envelope_request(envelope: Envelope) -> std::result::Result<GuestRequest, String> {
    if envelope.v != SPOOL_VERSION { return Err(format!("envelope version {} is not {SPOOL_VERSION}", envelope.v)); }
    if !valid_request_id(&envelope.request_id) {
        return Err("request_id must be 1-128 characters of A-Z a-z 0-9 . _ : -".into());
    }
    let Envelope { op, request_id, content, message_id, next, decline, cursor, owed_after, .. } = envelope;
    let extra = |present: &[(&str, bool)]| present.iter().find(|(_, set)| *set).map(|(name, _)| format!("'{name}' is not a field of {op}"));
    match op.as_str() {
        "post" => {
            if let Some(error) = extra(&[("message_id", message_id.is_some()), ("next", next.is_some()),
                ("decline", decline.is_some()), ("cursor", cursor.is_some()), ("owed_after", owed_after.is_some())]) { return Err(error); }
            let content = content.filter(|content| !content.trim().is_empty()).ok_or("post needs non-empty content")?;
            Ok(GuestRequest::Post { content, request_id: Some(request_id) })
        }
        "ack" => {
            if let Some(error) = extra(&[("content", content.is_some()), ("cursor", cursor.is_some()),
                ("owed_after", owed_after.is_some())]) { return Err(error); }
            let message_id = message_id.ok_or("ack needs message_id")?;
            if next.is_some() == decline.is_some() { return Err("ack needs exactly one of next or decline".into()); }
            Ok(GuestRequest::Ack { message_id, next_action: next, decline, outcome_ref: None, request_id: Some(request_id) })
        }
        "read" => {
            if let Some(error) = extra(&[("content", content.is_some()), ("message_id", message_id.is_some()),
                ("next", next.is_some()), ("decline", decline.is_some())]) { return Err(error); }
            Ok(GuestRequest::Read { cursor, owed_after })
        }
        "status" => {
            if let Some(error) = extra(&[("content", content.is_some()), ("message_id", message_id.is_some()),
                ("next", next.is_some()), ("decline", decline.is_some()), ("cursor", cursor.is_some()),
                ("owed_after", owed_after.is_some())]) { return Err(error); }
            Ok(GuestRequest::Status {})
        }
        other => Err(format!("unknown op '{other}' (post, ack, read or status)")),
    }
}

/// One envelope file's bytes, read once and bounded, with their SHA-256.
fn read_envelope_bytes(path: &Path) -> std::result::Result<(Vec<u8>, String), String> {
    use sha2::Digest;
    let size = std::fs::metadata(path).map_err(|error| format!("unreadable: {error}"))?.len();
    if size > ENVELOPE_MAX_BYTES { return Err(format!("{size} bytes is over the {ENVELOPE_MAX_BYTES}-byte envelope limit")); }
    let bytes = std::fs::read(path).map_err(|error| format!("unreadable: {error}"))?;
    if bytes.len() as u64 > ENVELOPE_MAX_BYTES {
        return Err(format!("{} bytes is over the {ENVELOPE_MAX_BYTES}-byte envelope limit", bytes.len()));
    }
    let digest = sha2::Sha256::digest(&bytes).iter().map(|byte| format!("{byte:02X}")).collect();
    Ok((bytes, digest))
}

/// Parses one envelope's bytes: UTF-8 JSON, strict fields, no credential.
fn read_envelope(bytes: &[u8]) -> std::result::Result<(String, GuestRequest), (Option<String>, String)> {
    const NO_CREDENTIAL: &str = "an envelope never carries a guest credential; the token stays on the relay computer";
    if carries_credential(&String::from_utf8_lossy(bytes)) { return Err((None, NO_CREDENTIAL.into())); }
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|error| (None, format!("not an envelope: {error}")))?;
    // Escaped JSON (prim1g_…) decodes here; every string is checked again.
    let decoded = [Some(&envelope.op), Some(&envelope.request_id), envelope.content.as_ref(), envelope.next.as_ref(),
        envelope.decline.as_ref()];
    if decoded.into_iter().flatten().any(|text| carries_credential(text)) { return Err((None, NO_CREDENTIAL.into())); }
    let request_id = envelope.request_id.clone();
    let request = envelope_request(envelope).map_err(|error| (Some(request_id.clone()), error))?;
    Ok((request_id, request))
}

fn result_path(inbox: &Path, name: &str) -> PathBuf { inbox.join(format!("{name}{RESULT_SUFFIX}")) }

/// A name's result file as found. Codex PRIM-1: only a missing file means the
/// name was never used; one that cannot be read or parsed is a binding that
/// cannot be checked, so nothing is sent under that name and the file is kept.
enum ResultFile { Absent, Present(SpoolResult), Unusable(String) }

fn result_file(inbox: &Path, name: &str) -> ResultFile {
    match std::fs::read(result_path(inbox, name)) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => ResultFile::Absent,
        Err(error) => ResultFile::Unusable(format!("unreadable: {error}")),
        Ok(bytes) => match serde_json::from_slice(&bytes) {
            Ok(result) => ResultFile::Present(result),
            Err(error) => ResultFile::Unusable(format!("not a spool result: {error}")),
        },
    }
}

fn read_result(inbox: &Path, name: &str) -> Option<SpoolResult> {
    match result_file(inbox, name) { ResultFile::Present(result) => Some(result), _ => None }
}

/// The inbox's lock file; envelope names never start with a dot.
const LOCK_FILE: &str = ".spool.lock";

/// Complete files only: a temporary file renamed over the result. A response
/// that holds a credential-shaped string is withheld; its state still stands.
fn write_result(inbox: &Path, result: &mut SpoolResult) -> Result<()> {
    let path = result_path(inbox, &result.envelope);
    let temporary = inbox.join(format!("{}{RESULT_SUFFIX}.tmp", result.envelope));
    let mut bytes = serde_json::to_vec_pretty(result)?;
    if carries_credential(&String::from_utf8_lossy(&bytes)) {
        result.response = None;
        result.message = Some("PRIM-1's response was withheld: it contained a guest-credential-shaped string".into());
        bytes = serde_json::to_vec_pretty(result)?;
        if carries_credential(&String::from_utf8_lossy(&bytes)) {
            bail!("{} would carry a guest credential; nothing was written", path.display());
        }
    }
    std::fs::write(&temporary, bytes).with_context(|| format!("cannot write {}", temporary.display()))?;
    std::fs::rename(&temporary, &path).with_context(|| format!("cannot replace {}", path.display()))
}

/// Envelope names in the outbox, in name order: `<name>.json`, never a result or temporary file.
fn envelope_names(outbox: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for entry in std::fs::read_dir(outbox).with_context(|| format!("cannot read the outbox {}", outbox.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let Some(file) = entry.file_name().to_str().map(str::to_owned) else { continue };
        let Some(name) = file.strip_suffix(".json") else { continue };
        if name.ends_with(".result") { continue; }
        names.push(name.to_owned());
    }
    names.sort();
    Ok(names)
}

enum Attempt { Done(SpoolState), Held { wait: u64, reason: String } }

/// How a finished envelope's current file compares with the bytes it was sent with.
enum Identity { Same, Changed, Unverifiable }

pub(crate) struct Spool<'a, G, F: FnMut() -> Result<G>, S: FnMut(Duration)> {
    pub(crate) outbox: &'a Path,
    pub(crate) inbox: &'a Path,
    pub(crate) connect: F,
    pub(crate) sleep: S,
    pub(crate) retry_for: Duration,
}

impl<G: GuestClient, F: FnMut() -> Result<G>, S: FnMut(Duration)> Spool<'_, G, F, S> {
    pub(crate) fn run(&mut self) -> Result<SpoolSummary> {
        let same = std::fs::canonicalize(self.outbox).ok().zip(std::fs::canonicalize(self.inbox).ok())
            .is_some_and(|(outbox, inbox)| outbox == inbox);
        if same { bail!("the outbox and the inbox must be different folders"); }
        // Codex PRIM-1: one spool at a time per inbox, which holds the bindings.
        // An OS lock on a file in the inbox, held for the whole run and
        // released by the system when the process ends, also after a crash.
        let lock_path = self.inbox.join(LOCK_FILE);
        let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(&lock_path)
            .with_context(|| format!("cannot open {}", lock_path.display()))?;
        match lock.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) =>
                bail!("another spool is running for the inbox {}; nothing was sent", self.inbox.display()),
            Err(std::fs::TryLockError::Error(error)) =>
                return Err(error).with_context(|| format!("cannot lock {}; nothing was sent", lock_path.display())),
        }
        let deadline = Instant::now() + self.retry_for;
        let mut summary = SpoolSummary::default();
        let mut backoff = 0usize;
        'pass: loop {
            let names = envelope_names(self.outbox)?;
            for (index, name) in names.iter().cloned().enumerate() {
                let previous = match result_file(self.inbox, &name) {
                    ResultFile::Absent => None,
                    ResultFile::Present(result) => Some(result),
                    ResultFile::Unusable(_) => { summary.unverifiable += 1; continue; }
                };
                let previous = match previous {
                    // b1ecd212: finished, so never sent again; a changed file under
                    // the same name is recorded on its result, not sent as new.
                    Some(done) if done.state != SpoolState::Pending => {
                        match self.note_changed(&name, done)? {
                            Identity::Same => summary.already_done += 1,
                            Identity::Changed => summary.changed += 1,
                            Identity::Unverifiable => summary.unverifiable += 1,
                        }
                        continue;
                    }
                    // 594f5ebd: a pending result that records an attempt (attempts
                    // or a request id) but no digest comes from an older spool:
                    // what was attempted cannot be compared with today's bytes,
                    // so it is held unsent and kept; later names still go. A
                    // waiting placeholder that was never attempted has neither.
                    Some(pending) if pending.envelope_sha256.is_none()
                        && (pending.attempts > 0 || pending.request_id.is_some()) => {
                        summary.unverifiable += 1;
                        continue;
                    }
                    // 59189c4d: a waiting result keeps an earlier attempt's
                    // request id, digest and attempts, so it is used as is.
                    previous => previous,
                };
                match self.attempt(&name, previous, backoff)? {
                    Attempt::Done(state) => {
                        backoff = 0;
                        match state {
                            SpoolState::Stored => summary.stored += 1,
                            SpoolState::Answered => summary.answered += 1,
                            _ => summary.refused += 1,
                        }
                    }
                    Attempt::Held { wait, reason } => {
                        self.mark_waiting(&names[index + 1..], &name, &reason)?;
                        let wait = Duration::from_secs(wait);
                        if Instant::now() + wait > deadline {
                            summary.pending = Some(name);
                            break 'pass;
                        }
                        (self.sleep)(wait);
                        backoff = (backoff + 1).min(BACKOFF_SECS.len() - 1);
                        // The pass starts again from the first name: count afresh.
                        (summary.already_done, summary.changed, summary.unverifiable) = (0, 0, 0);
                        continue 'pass;
                    }
                }
            }
            break;
        }
        summary.ok = summary.pending.is_none() && summary.changed == 0 && summary.unverifiable == 0;
        summary.message = match &summary.pending {
            Some(name) => format!("{name} is pending (see its result); the envelopes after it wait in name order"),
            None => "every envelope has a result; a stored post is not a read or an ACK".into(),
        };
        if summary.changed > 0 {
            summary.message.push_str(&format!("; {} envelope file(s) changed under a name already used and were not sent (see changed_sha256 in their results); a new request needs a new file name", summary.changed));
        }
        if summary.unverifiable > 0 {
            summary.message.push_str(&format!("; {} envelope(s) could not be compared with what was sent (the envelope unreadable or over the size limit, a result file that cannot be read, or a result from an older spool without a digest); their files are kept and nothing was sent for them", summary.unverifiable));
        }
        Ok(summary)
    }

    /// b1ecd212: whether the file under a finished envelope's name still
    /// holds the bytes that were sent. Other bytes are recorded once on the
    /// result (its state, request and response stay). bf2cb722: a result
    /// without a digest (an older spool), or a current file that cannot be
    /// read or is over the limit, cannot be compared: unverifiable, never the
    /// same, and the result is left as it is.
    fn note_changed(&self, name: &str, mut done: SpoolResult) -> Result<Identity> {
        let Some(admitted) = done.envelope_sha256.clone() else { return Ok(Identity::Unverifiable) };
        let Ok((_, current)) = read_envelope_bytes(&self.outbox.join(format!("{name}.json"))) else {
            return Ok(Identity::Unverifiable);
        };
        if current == admitted { return Ok(Identity::Same) }
        if done.changed_sha256.as_deref() != Some(current.as_str()) {
            done.changed_sha256 = Some(current);
            write_result(self.inbox, &mut done)?;
        }
        Ok(Identity::Changed)
    }

    fn attempt(&mut self, name: &str, previous: Option<SpoolResult>, backoff: usize) -> Result<Attempt> {
        let attempts = previous.as_ref().map_or(0, |result| result.attempts).saturating_add(1);
        let mut result = SpoolResult { v: SPOOL_VERSION, envelope: name.to_owned(), request_id: None, state: SpoolState::Refused,
            reason: None, message: None, attempts, at_unix_ms: now_unix_ms(), next_attempt_in_secs: None, response: None,
            envelope_sha256: None, changed_sha256: None };
        let bytes = if valid_name(name) {
            read_envelope_bytes(&self.outbox.join(format!("{name}.json"))).map_err(|error| (None, error))
        } else {
            Err((None, format!("envelope names are 1-{NAME_MAX_CHARS} characters of A-Z a-z 0-9 . _ -")))
        };
        // b1ecd212: a pending envelope is retried only with the bytes it was
        // first sent with. Changed bytes may sit beside an earlier request that
        // was stored before an ambiguous end: neither is sent; the result says so.
        let sent = previous.as_ref().and_then(|result| result.envelope_sha256.clone());
        // bf2cb722: an envelope already sent once whose file cannot be read
        // now keeps its earlier request id and digest and stays pending; it is
        // never rewritten as a fresh invalid envelope.
        if let (Err((_, error)), Some(sent)) = (&bytes, &sent) {
            let wait = BACKOFF_SECS[backoff];
            result.request_id = previous.as_ref().and_then(|result| result.request_id.clone());
            result.envelope_sha256 = Some(sent.clone());
            result.state = SpoolState::Pending;
            result.reason = Some("envelope_unreadable".into());
            result.next_attempt_in_secs = Some(wait);
            result.message = Some(format!("{name}.json cannot be read again ({error}) after its request {} was sent, which may have been stored; nothing more was sent", result.request_id.as_deref().unwrap_or("(unknown)")));
            write_result(self.inbox, &mut result)?;
            return Ok(Attempt::Held { wait, reason: "envelope_unreadable".into() });
        }
        if let (Ok((_, digest)), Some(sent)) = (&bytes, sent)
            && *digest != sent
        {
            result.request_id = previous.as_ref().and_then(|result| result.request_id.clone());
            result.envelope_sha256 = Some(sent);
            result.changed_sha256 = Some(digest.clone());
            result.reason = Some("changed_envelope".into());
            result.message = Some(format!("{name}.json changed after its request {} was sent, which may have been stored; the changed file was not sent. A new request needs a new file name", result.request_id.as_deref().unwrap_or("(unknown)")));
            write_result(self.inbox, &mut result)?;
            return Ok(Attempt::Done(SpoolState::Refused));
        }
        let request = bytes.and_then(|(bytes, digest)| {
            result.envelope_sha256 = Some(digest);
            read_envelope(&bytes)
        });
        let (request_id, request) = match request {
            Ok(request) => request,
            Err((request_id, error)) => {
                result.request_id = request_id;
                result.reason = Some("invalid_envelope".into());
                result.message = Some(error);
                write_result(self.inbox, &mut result)?;
                return Ok(Attempt::Done(SpoolState::Refused));
            }
        };
        result.request_id = Some(request_id);
        // Codex PRIM-1 (crash boundary): the name's binding (request id and
        // digest) is on disk before anything may be sent. If that write fails,
        // nothing is sent; a crash after a send leaves this pending result, so
        // a restart sends only these bytes again (same request id) or refuses
        // changed ones. The outcome below replaces it.
        result.state = SpoolState::Pending;
        result.reason = Some(SENDING.into());
        result.message = Some("being sent; if this stays, whether PRIM-1 stored it is unknown and the same request is sent again".into());
        write_result(self.inbox, &mut result)?;
        (result.state, result.reason, result.message) = (SpoolState::Refused, None, None);
        let is_post = matches!(request, GuestRequest::Post { .. });
        let sent = (self.connect)().and_then(|mut client| client.send(request));
        let held = match sent {
            Ok(response) if response.ok => {
                result.state = if is_post { SpoolState::Stored } else { SpoolState::Answered };
                result.message = Some(match &response.payload {
                    Some(GuestResponsePayload::Post { result: post }) =>
                        format!("stored as {}{}; not yet read or acknowledged", post.message_id,
                            if post.replayed { " (the same request, stored earlier)" } else { "" }),
                    _ => response.message.clone(),
                });
                result.response = Some(response);
                None
            }
            Ok(response) => match response.retry_after_secs {
                Some(secs) => {
                    result.reason = Some("retry_later".into());
                    result.message = Some(response.message.clone());
                    Some(secs.max(1))
                }
                None => {
                    result.reason = Some("refused".into());
                    result.message = Some(response.message.clone());
                    result.response = Some(response);
                    None
                }
            },
            Err(error) => {
                result.reason = Some("prim1_down".into());
                result.message = Some(format!("PRIM-1 on the relay host is not answering: {error:#}"));
                Some(BACKOFF_SECS[backoff])
            }
        };
        if let Some(wait) = held {
            result.state = SpoolState::Pending;
            result.next_attempt_in_secs = Some(wait);
        }
        write_result(self.inbox, &mut result)?;
        Ok(match held {
            Some(wait) => Attempt::Held { wait, reason: result.reason.clone().unwrap_or_default() },
            None => Attempt::Done(result.state),
        })
    }

    /// Claude PRIM-1 (★3): every envelope held behind a pending one says so in
    /// its own result, never a bare "pending". Waiting is not an attempt.
    /// Reviewer 59189c4d: an envelope already sent once keeps its request id,
    /// digest and attempts while it waits, so its binding survives.
    fn mark_waiting(&self, later: &[String], head: &str, reason: &str) -> Result<()> {
        for name in later {
            let previous = match result_file(self.inbox, name) {
                ResultFile::Absent => None,
                ResultFile::Present(result) => Some(result),
                // Kept as found; the run reports it when it reaches the name.
                ResultFile::Unusable(_) => continue,
            };
            if previous.as_ref().is_some_and(|result| result.state != SpoolState::Pending) { continue; }
            let (request_id, envelope_sha256, attempts) = previous
                .map(|result| (result.request_id, result.envelope_sha256, result.attempts)).unwrap_or_default();
            // 594f5ebd: a recorded attempt proves only that it was attempted.
            let earlier = request_id.as_deref()
                .map(|id| format!("; an attempt for its request {id} was recorded, and whether it reached PRIM-1 is unknown"))
                .unwrap_or_default();
            write_result(self.inbox, &mut SpoolResult { v: SPOOL_VERSION, envelope: name.clone(), request_id,
                state: SpoolState::Pending, reason: Some(WAITING_BEHIND.into()),
                message: Some(format!("waiting behind {head} ({reason}){earlier}")),
                attempts, at_unix_ms: now_unix_ms(), next_attempt_in_secs: None, response: None,
                envelope_sha256, changed_sha256: None })?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use anyhow::anyhow;
    use shared_types::GuestPostResult;

    use super::*;

    type Log = Arc<Mutex<Vec<GuestRequest>>>;
    type Script = Arc<Mutex<VecDeque<Result<GuestResponse>>>>;

    struct Fake { log: Log, script: Script }

    impl GuestClient for Fake {
        fn send(&mut self, request: GuestRequest) -> Result<GuestResponse> {
            self.log.lock().unwrap().push(request);
            self.script.lock().unwrap().pop_front().unwrap_or_else(|| Err(anyhow!("pipe closed")))
        }
    }

    struct TempRoot(PathBuf);

    impl Drop for TempRoot {
        fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
    }

    fn folders() -> (TempRoot, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("prim1-guest-spool-{}", uuid::Uuid::new_v4()));
        let (outbox, inbox) = (root.join("outbox"), root.join("inbox"));
        std::fs::create_dir_all(&outbox).unwrap(); std::fs::create_dir_all(&inbox).unwrap();
        (TempRoot(root), outbox, inbox)
    }

    fn stored(message_id: MessageId, replayed: bool) -> Result<GuestResponse> {
        let result = GuestPostResult { message_id, replayed, request_id: "r".into(), recipient_count: 1, written_count: 1,
            receipts: Vec::new(), cursor: RoomFeedCursor { epoch: uuid::Uuid::new_v4(), sequence: 1 } };
        Ok(GuestResponse { ok: true, message: "message stored".into(), timed_out: false, retry_after_secs: None,
            payload: Some(GuestResponsePayload::Post { result }) })
    }

    fn answered() -> Result<GuestResponse> {
        Ok(GuestResponse { ok: true, message: "answered".into(), timed_out: false, retry_after_secs: None, payload: None })
    }

    fn refusal(retry: Option<u64>) -> Result<GuestResponse> {
        Ok(GuestResponse { ok: false, message: "posting too fast".into(), timed_out: false, retry_after_secs: retry, payload: None })
    }

    fn run(outbox: &Path, inbox: &Path, script: Vec<Result<GuestResponse>>, retry: u64, down_connects: usize)
        -> (SpoolSummary, Vec<GuestRequest>, Vec<Duration>) {
        let log: Log = Arc::default();
        let script: Script = Arc::new(Mutex::new(script.into()));
        let mut downs = down_connects;
        let mut slept = Vec::new();
        let summary = Spool { outbox, inbox, retry_for: Duration::from_secs(retry), sleep: |wait| slept.push(wait),
            connect: || if downs > 0 { downs -= 1; Err(anyhow!("PRIM-1 is not running")) }
                else { Ok(Fake { log: log.clone(), script: script.clone() }) } }.run().unwrap();
        let requests = log.lock().unwrap().clone();
        (summary, requests, slept)
    }

    fn result(inbox: &Path, name: &str) -> SpoolResult { read_result(inbox, name).unwrap() }

    #[test]
    fn a_post_is_stored_never_delivered_and_a_terminal_result_is_never_sent_again() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"hello from PC3"}"#).unwrap();
        let id = uuid::Uuid::new_v4();
        let (summary, requests, _) = run(&outbox, &inbox, vec![stored(id, false)], 0, 0);
        assert_eq!((summary.ok, summary.stored), (true, 1));
        assert_eq!(requests, [GuestRequest::Post { content: "hello from PC3".into(), request_id: Some("pc3-1".into()) }]);
        let written = result(&inbox, "001-post");
        assert_eq!(written.state, SpoolState::Stored);
        let message = written.message.unwrap();
        assert!(message.contains(&id.to_string()) && message.contains("not yet read or acknowledged"), "{message}");
        let (again, requests, _) = run(&outbox, &inbox, vec![], 0, 0);
        assert_eq!((again.already_done, requests.len()), (1, 0), "the inbox result is the ledger");
        assert!(outbox.join("001-post.json").exists(), "the spool deletes nothing");
    }

    /// v3 W5 (Reviewer b1ecd212): a finished envelope's name stays bound to
    /// the bytes it was sent with. Identical bytes are already done; other
    /// bytes under that name are recorded on the result and never sent, and
    /// the original outcome is kept.
    #[test]
    fn a_changed_file_under_a_finished_name_is_recorded_and_never_sent() {
        let (_root, outbox, inbox) = folders();
        let original = r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#;
        std::fs::write(outbox.join("001-post.json"), original).unwrap();
        let id = uuid::Uuid::new_v4();
        run(&outbox, &inbox, vec![stored(id, false)], 0, 0);
        let first = result(&inbox, "001-post");
        assert!(first.envelope_sha256.is_some());

        std::fs::write(outbox.join("001-post.json"), original).unwrap();
        let (same, requests, _) = run(&outbox, &inbox, vec![], 0, 0);
        assert_eq!((same.already_done, same.changed, same.ok, requests.len()), (1, 0, true, 0), "same bytes: already done");

        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-9","content":"other"}"#).unwrap();
        let (changed, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert!(requests.is_empty(), "nothing is sent for a changed file under a used name");
        assert_eq!((changed.changed, changed.already_done, changed.ok), (1, 0, false));
        assert!(changed.message.contains("a new request needs a new file name"), "{}", changed.message);
        let kept = result(&inbox, "001-post");
        assert_eq!((kept.state, kept.request_id.as_deref()), (SpoolState::Stored, Some("pc3-1")), "the original outcome stays");
        assert!(kept.message.unwrap().contains(&id.to_string()));
        assert!(kept.changed_sha256.is_some() && kept.changed_sha256 != kept.envelope_sha256);
    }

    /// v3 W5 (b1ecd212): a pending envelope whose file changes before its
    /// retry is refused with its earlier request id named; neither the old
    /// request nor the new bytes are sent.
    #[test]
    fn a_pending_envelope_changed_before_its_retry_is_refused_and_not_sent() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        let (summary, _, _) = run(&outbox, &inbox, vec![], 0, 1);
        assert_eq!(summary.pending.as_deref(), Some("001-post"));
        let sent = result(&inbox, "001-post").envelope_sha256.expect("the bytes that were sent are recorded");

        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-2","content":"second"}"#).unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert!(requests.is_empty(), "neither the old request nor the new bytes are sent");
        assert_eq!(summary.refused, 1);
        let refused = result(&inbox, "001-post");
        assert_eq!((refused.state, refused.reason.as_deref(), refused.request_id.as_deref()),
            (SpoolState::Refused, Some("changed_envelope"), Some("pc3-1")));
        assert_eq!(refused.envelope_sha256.as_deref(), Some(sent.as_str()));
        let message = refused.message.unwrap();
        assert!(message.contains("pc3-1") && message.contains("may have been stored") && message.contains("new file name"), "{message}");
    }

    /// Reviewer 59189c4d: an envelope sent once (B) that then waits behind a
    /// new earlier name (A) keeps its binding; B changed meanwhile is refused
    /// when A clears, and only A is sent.
    #[test]
    fn an_envelope_sent_once_keeps_its_binding_while_it_waits_behind_another() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("002-b.json"), r#"{"v":1,"op":"post","request_id":"pc3-b","content":"b"}"#).unwrap();
        run(&outbox, &inbox, vec![], 0, 1);
        let sent = result(&inbox, "002-b").envelope_sha256.expect("B was read and sent once");
        std::fs::write(outbox.join("001-a.json"), r#"{"v":1,"op":"post","request_id":"pc3-a","content":"a"}"#).unwrap();
        run(&outbox, &inbox, vec![], 0, 1);
        let waiting = result(&inbox, "002-b");
        assert_eq!((waiting.reason.as_deref(), waiting.request_id.as_deref(), waiting.envelope_sha256.as_deref(), waiting.attempts),
            (Some("waiting_behind"), Some("pc3-b"), Some(sent.as_str()), 1), "waiting keeps the earlier attempt");
        assert!(waiting.message.unwrap().contains("an attempt for its request pc3-b was recorded, and whether it reached PRIM-1 is unknown"));

        std::fs::write(outbox.join("002-b.json"), r#"{"v":1,"op":"post","request_id":"pc3-b2","content":"other"}"#).unwrap();
        let (_, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false), stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert_eq!(requests, [GuestRequest::Post { content: "a".into(), request_id: Some("pc3-a".into()) }], "only A is sent");
        let refused = result(&inbox, "002-b");
        assert_eq!((refused.state, refused.reason.as_deref(), refused.request_id.as_deref()),
            (SpoolState::Refused, Some("changed_envelope"), Some("pc3-b")));
    }

    /// Reviewer bf2cb722: what cannot be compared is unverifiable, never done.
    /// A finished result without a digest, or whose file is now over the limit,
    /// stands untouched; a pending envelope that cannot be read again keeps its
    /// earlier request id and digest and stays pending. Nothing is sent.
    #[test]
    fn an_identity_that_cannot_be_compared_is_unverifiable_and_never_sent() {
        let (_root, outbox, inbox) = folders();
        let oversize = format!(r#"{{"v":1,"op":"post","request_id":"x","content":"{}"}}"#, "y".repeat(70 * 1024));
        // An older spool's finished result, without a digest.
        std::fs::write(outbox.join("001-old.json"), r#"{"v":1,"op":"post","request_id":"pc3-old","content":"old"}"#).unwrap();
        let legacy = r#"{"v":1,"envelope":"001-old","request_id":"pc3-old","state":"stored","attempts":1,"at_unix_ms":1}"#;
        std::fs::write(inbox.join("001-old.result.json"), legacy).unwrap();
        // A finished envelope whose file is now over the limit.
        std::fs::write(outbox.join("002-done.json"), r#"{"v":1,"op":"post","request_id":"pc3-done","content":"done"}"#).unwrap();
        std::fs::write(outbox.join("003-held.json"), r#"{"v":1,"op":"post","request_id":"pc3-held","content":"held"}"#).unwrap();
        // 002 is stored; 003 is sent once and left pending (PRIM-1 stops answering).
        run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false), Err(anyhow!("pipe closed"))], 0, 0);
        assert_eq!(result(&inbox, "003-held").state, SpoolState::Pending);
        let held_digest = result(&inbox, "003-held").envelope_sha256.expect("sent once");
        std::fs::write(outbox.join("002-done.json"), &oversize).unwrap();
        std::fs::write(outbox.join("003-held.json"), &oversize).unwrap();

        let (summary, requests, _) = run(&outbox, &inbox, vec![], 0, 0);
        assert!(requests.is_empty(), "nothing is sent");
        assert_eq!((summary.unverifiable, summary.already_done, summary.ok), (2, 0, false), "{summary:?}");
        assert_eq!(summary.pending.as_deref(), Some("003-held"));
        assert!(summary.message.contains("could not be compared"), "{}", summary.message);
        assert_eq!(std::fs::read_to_string(inbox.join("001-old.result.json")).unwrap(), legacy, "an older result stands untouched");
        assert!(result(&inbox, "002-done").changed_sha256.is_none());
        let unreadable = result(&inbox, "003-held");
        assert_eq!((unreadable.state, unreadable.reason.as_deref(), unreadable.request_id.as_deref()),
            (SpoolState::Pending, Some("envelope_unreadable"), Some("pc3-held")));
        assert_eq!(unreadable.envelope_sha256.as_deref(), Some(held_digest.as_str()));
        assert!(unreadable.message.unwrap().contains("may have been stored"));
    }

    /// Codex PRIM-1 (crash boundary): the binding is on disk when the send
    /// happens. A crash after PRIM-1 stored the post but before the outcome
    /// was written (the on-disk result as it was at the send) then refuses
    /// changed bytes under that name instead of sending them as a first request.
    #[test]
    fn the_binding_is_on_disk_before_the_send_and_survives_a_crash_after_it() {
        struct AtSend { inbox: PathBuf, seen: Arc<Mutex<Option<String>>> }
        impl GuestClient for AtSend {
            fn send(&mut self, _request: GuestRequest) -> Result<GuestResponse> {
                *self.seen.lock().unwrap() = std::fs::read_to_string(result_path(&self.inbox, "001-post")).ok();
                stored(uuid::Uuid::new_v4(), false)
            }
        }
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        let seen = Arc::new(Mutex::new(None));
        let summary = Spool { outbox: &outbox, inbox: &inbox, retry_for: Duration::ZERO, sleep: |_| {},
            connect: || Ok(AtSend { inbox: inbox.clone(), seen: seen.clone() }) }.run().unwrap();
        assert_eq!(summary.stored, 1);
        let at_send: SpoolResult = serde_json::from_str(seen.lock().unwrap().as_deref().expect("a result existed at the send")).unwrap();
        assert_eq!((at_send.state, at_send.reason.as_deref(), at_send.request_id.as_deref()),
            (SpoolState::Pending, Some("sending"), Some("pc3-1")));
        assert!(at_send.envelope_sha256.is_some());

        // The crash: the outcome write never happened.
        std::fs::write(result_path(&inbox, "001-post"), seen.lock().unwrap().as_deref().unwrap()).unwrap();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-2","content":"second"}"#).unwrap();
        let (_, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert!(requests.is_empty(), "changed bytes under an in-flight name are never sent");
        let refused = result(&inbox, "001-post");
        assert_eq!((refused.state, refused.reason.as_deref(), refused.request_id.as_deref()),
            (SpoolState::Refused, Some("changed_envelope"), Some("pc3-1")));
    }

    /// Codex PRIM-1 (crash boundary): if the binding cannot be written, nothing is sent.
    #[test]
    fn nothing_is_sent_when_the_binding_cannot_be_written() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        // A directory where the result's temporary file would go makes the write fail.
        std::fs::create_dir_all(inbox.join("001-post.result.json.tmp")).unwrap();
        let log: Log = Arc::default();
        let script: Script = Arc::new(Mutex::new(vec![stored(uuid::Uuid::new_v4(), false)].into()));
        let outcome = Spool { outbox: &outbox, inbox: &inbox, retry_for: Duration::ZERO, sleep: |_| {},
            connect: || Ok(Fake { log: log.clone(), script: script.clone() }) }.run();
        assert!(outcome.is_err(), "the spool stops");
        assert!(log.lock().unwrap().is_empty(), "nothing was sent");
    }

    /// Reviewer 594f5ebd: an older spool's pending result that records an
    /// attempt but no digest is held unsent and kept as found; a waiting
    /// placeholder that was never attempted is sent normally.
    #[test]
    fn an_older_pending_attempt_without_a_digest_is_held_unsent() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-old.json"), r#"{"v":1,"op":"post","request_id":"pc3-new","content":"today"}"#).unwrap();
        std::fs::write(outbox.join("002-never.json"), r#"{"v":1,"op":"post","request_id":"pc3-2","content":"second"}"#).unwrap();
        let older = r#"{"v":1,"envelope":"001-old","request_id":"pc3-old","state":"pending","reason":"prim1_down","attempts":2,"at_unix_ms":1,"next_attempt_in_secs":4}"#;
        std::fs::write(result_path(&inbox, "001-old"), older).unwrap();
        let placeholder = r#"{"v":1,"envelope":"002-never","state":"pending","reason":"waiting_behind","attempts":0,"at_unix_ms":1}"#;
        std::fs::write(result_path(&inbox, "002-never"), placeholder).unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert_eq!(requests, [GuestRequest::Post { content: "second".into(), request_id: Some("pc3-2".into()) }],
            "the older attempt is not sent with today's bytes; the never-attempted one is");
        assert_eq!((summary.unverifiable, summary.stored, summary.ok), (1, 1, false), "{summary:?}");
        assert_eq!(std::fs::read_to_string(result_path(&inbox, "001-old")).unwrap(), older, "kept as found");
    }

    /// Codex PRIM-1: a result file that cannot be parsed is not a missing one.
    /// Nothing is sent under that name, the file stays as found, and the run
    /// says so; a later envelope still goes.
    #[test]
    fn a_corrupt_result_file_sends_nothing_under_its_name() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        std::fs::write(outbox.join("002-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-2","content":"second"}"#).unwrap();
        std::fs::write(result_path(&inbox, "001-post"), "{ not json").unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert_eq!(requests, [GuestRequest::Post { content: "second".into(), request_id: Some("pc3-2".into()) }]);
        assert_eq!((summary.unverifiable, summary.stored, summary.ok), (1, 1, false), "{summary:?}");
        assert_eq!(std::fs::read_to_string(result_path(&inbox, "001-post")).unwrap(), "{ not json", "kept as found");
    }

    /// Codex PRIM-1: one spool at a time per inbox. While another process
    /// holds the inbox's lock, a run refuses before reading or sending anything.
    #[test]
    fn a_second_spool_on_the_same_inbox_refuses_and_sends_nothing() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        let held = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(inbox.join(LOCK_FILE)).unwrap();
        held.lock().unwrap();
        let log: Log = Arc::default();
        let script: Script = Arc::new(Mutex::new(vec![stored(uuid::Uuid::new_v4(), false)].into()));
        let error = Spool { outbox: &outbox, inbox: &inbox, retry_for: Duration::ZERO, sleep: |_| {},
            connect: || Ok(Fake { log: log.clone(), script: script.clone() }) }.run().unwrap_err();
        assert!(format!("{error:#}").contains("another spool is running"), "{error:#}");
        assert!(log.lock().unwrap().is_empty() && read_result(&inbox, "001-post").is_none(), "nothing read, bound or sent");
        held.unlock().unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![stored(uuid::Uuid::new_v4(), false)], 0, 0);
        assert_eq!((summary.stored, requests.len()), (1, 1), "the lock is the only thing that stopped it");
    }

    #[test]
    fn prim1_down_holds_in_name_order_and_the_reconnect_resends_the_same_request_id() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("001-post.json"), r#"{"v":1,"op":"post","request_id":"pc3-1","content":"first"}"#).unwrap();
        std::fs::write(outbox.join("002-status.json"), r#"{"v":1,"op":"status","request_id":"pc3-2"}"#).unwrap();
        // One pass while PRIM-1 is down: pending, nothing after it is tried.
        let (summary, requests, _) = run(&outbox, &inbox, vec![], 0, 1);
        assert_eq!(summary.pending.as_deref(), Some("001-post"));
        assert!(requests.is_empty());
        let held = result(&inbox, "001-post");
        assert_eq!((held.state, held.reason.as_deref(), held.attempts), (SpoolState::Pending, Some("prim1_down"), 1));
        assert!(held.message.unwrap().starts_with("PRIM-1 on the relay host is not answering"));
        let waiting = result(&inbox, "002-status");
        assert_eq!((waiting.state, waiting.reason.as_deref(), waiting.attempts), (SpoolState::Pending, Some("waiting_behind"), 0),
            "name order holds the later envelope, and its own result says so");
        assert_eq!(waiting.message.as_deref(), Some("waiting behind 001-post (prim1_down)"));
        // Reconnect with backoff: two more downs, then the post replays the id stored before.
        let id = uuid::Uuid::new_v4();
        let (summary, requests, slept) = run(&outbox, &inbox, vec![stored(id, true), answered()], 60, 2);
        assert!(summary.ok, "{summary:?}");
        assert_eq!(slept, [Duration::from_secs(1), Duration::from_secs(2)], "bounded backoff");
        assert_eq!(requests[0], GuestRequest::Post { content: "first".into(), request_id: Some("pc3-1".into()) });
        let done = result(&inbox, "001-post");
        assert_eq!((done.state, done.attempts), (SpoolState::Stored, 4));
        assert!(done.message.unwrap().contains("the same request, stored earlier"));
        assert_eq!(result(&inbox, "002-status").state, SpoolState::Answered);
    }

    #[test]
    fn a_retry_after_refusal_waits_and_a_plain_refusal_is_final() {
        let (_root, outbox, inbox) = folders();
        std::fs::write(outbox.join("a.json"), r#"{"v":1,"op":"post","request_id":"r-a","content":"x"}"#).unwrap();
        std::fs::write(outbox.join("b.json"), r#"{"v":1,"op":"post","request_id":"r-b","content":"y"}"#).unwrap();
        let (summary, _, slept) = run(&outbox, &inbox, vec![refusal(Some(7)), stored(uuid::Uuid::new_v4(), false), refusal(None)], 60, 0);
        assert_eq!(slept, [Duration::from_secs(7)]);
        assert_eq!((summary.stored, summary.refused, summary.ok), (1, 1, true));
        let refused = result(&inbox, "b");
        assert_eq!((refused.state, refused.reason.as_deref()), (SpoolState::Refused, Some("refused")));
    }

    #[test]
    fn only_data_reaches_a_request_and_anything_else_is_refused_as_an_envelope() {
        let (_root, outbox, inbox) = folders();
        let cases = [
            ("01", r#"{"v":1,"op":"post","request_id":"i-1","content":"x","path":"C:\\Windows"}"#, "unknown field"),
            ("02", r#"{"v":1,"op":"post","request_id":"i-2","content":"x","cursor":{"epoch":"55555555-5555-5555-5555-555555555555","sequence":1}}"#, "not a field of post"),
            ("03", r#"{"v":1,"op":"run","request_id":"i-3"}"#, "unknown op"),
            ("04", r#"{"v":1,"op":"ack","request_id":"i-4","message_id":"55555555-5555-5555-5555-555555555555","next":"a","decline":"b"}"#, "exactly one"),
            ("05", r#"{"v":1,"op":"status","request_id":"bad id; rm"}"#, "request_id must be"),
            ("06", r#"{"v":2,"op":"status","request_id":"i-6"}"#, "version"),
            ("07", "not json", "not an envelope"),
            ("11", r#"{"v":1,"op":"status","request_id":"i-11","owed_after":{"epoch":"55555555-5555-5555-5555-555555555555","sequence":1}}"#, "not a field of status"),
        ];
        for (name, body, _) in &cases { std::fs::write(outbox.join(format!("{name}.json")), body).unwrap(); }
        std::fs::write(outbox.join("08.json"), vec![b' '; (ENVELOPE_MAX_BYTES + 1) as usize]).unwrap();
        std::fs::write(outbox.join("09.json.tmp"), "{}").unwrap();
        std::fs::write(outbox.join("10.txt"), "{}").unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![], 0, 0);
        assert!(requests.is_empty(), "no invalid envelope reaches PRIM-1: {requests:?}");
        assert_eq!(summary.refused, 9);
        for (name, _, expected) in &cases {
            let refused = result(&inbox, name);
            assert_eq!(refused.reason.as_deref(), Some("invalid_envelope"));
            assert!(refused.message.as_deref().unwrap().contains(expected), "{name}: {refused:?}");
        }
        assert!(result(&inbox, "08").message.unwrap().contains("envelope limit"));
        assert!(read_result(&inbox, "09.json").is_none() && read_result(&inbox, "10").is_none(), "only complete .json files are envelopes");
        assert!(!valid_name(".hidden") && !valid_name("a b") && valid_name("2026-10-25T10.post_1"));
    }

    /// v3 W5 (Reviewer 3b9fa7b2): a read envelope may continue the guest's
    /// retained page; owed_after belongs to read only.
    #[test]
    fn a_read_envelope_carries_owed_after_and_no_other_op_does() {
        let epoch: uuid::Uuid = "55555555-5555-5555-5555-555555555555".parse().unwrap();
        let envelope: Envelope = serde_json::from_str(r#"{"v":1,"op":"read","request_id":"o-1",
            "owed_after":{"epoch":"55555555-5555-5555-5555-555555555555","sequence":7}}"#).unwrap();
        assert_eq!(envelope_request(envelope).unwrap(),
            GuestRequest::Read { cursor: None, owed_after: Some(RoomFeedCursor { epoch, sequence: 7 }) });
        let post: Envelope = serde_json::from_str(r#"{"v":1,"op":"post","request_id":"o-2","content":"x",
            "owed_after":{"epoch":"55555555-5555-5555-5555-555555555555","sequence":7}}"#).unwrap();
        assert!(envelope_request(post).unwrap_err().contains("not a field of post"));
    }

    /// Claude PRIM-1 (spool GO): the guest token never leaves the relay computer.
    #[test]
    fn no_envelope_or_result_ever_carries_a_guest_credential() {
        let (_root, outbox, inbox) = folders();
        let cases = [
            ("c1", r#"{"v":1,"op":"post","request_id":"t-1","content":"hi","guest_token":"prim1g_abc"}"#),
            ("c2", r#"{"v":1,"op":"post","request_id":"t-2","content":"my token is prim1g_abc"}"#),
            ("c3", r#"{"v":1,"op":"post","request_id":"t-3","content":"\u0070rim1g_abc"}"#),
            ("c4", r#"{"v":1,"op":"ack","request_id":"t-4","message_id":"55555555-5555-5555-5555-555555555555","next":"PRIM1G_ABC"}"#),
            ("c5", r#"{"v":1,"op":"status","request_id":"t-5","token_file":"C:\\Users\\x\\guest-driver.token"}"#),
        ];
        for (name, body) in &cases { std::fs::write(outbox.join(format!("{name}.json")), body).unwrap(); }
        std::fs::write(outbox.join("d1.json"), r#"{"v":1,"op":"read","request_id":"t-6"}"#).unwrap();
        let leaking = Ok(GuestResponse { ok: true, message: "page with prim1g_leak inside".into(), timed_out: false,
            retry_after_secs: None, payload: None });
        let (summary, requests, _) = run(&outbox, &inbox, vec![leaking], 0, 0);
        assert_eq!(requests, [GuestRequest::Read { cursor: None, owed_after: None }], "no credential-carrying envelope reaches PRIM-1");
        assert_eq!((summary.refused, summary.answered), (5, 1));
        for (name, _) in &cases { assert_eq!(result(&inbox, name).reason.as_deref(), Some("invalid_envelope"), "{name}"); }
        let withheld = result(&inbox, "d1");
        assert_eq!(withheld.state, SpoolState::Answered);
        assert!(withheld.response.is_none() && withheld.message.unwrap().contains("withheld"));
        for entry in std::fs::read_dir(&inbox).unwrap() {
            let text = std::fs::read_to_string(entry.unwrap().path()).unwrap().to_ascii_lowercase();
            assert!(!text.contains("prim1g_"), "an inbox file carries a credential: {text}");
        }
    }

    #[test]
    fn an_ack_and_a_read_carry_only_their_own_fields() {
        let (_root, outbox, inbox) = folders();
        let message = "55555555-5555-5555-5555-555555555555";
        std::fs::write(outbox.join("1.json"), r#"{"v":1,"op":"read","request_id":"k-1"}"#).unwrap();
        std::fs::write(outbox.join("2.json"), format!(r#"{{"v":1,"op":"ack","request_id":"k-2","message_id":"{message}","next":"reply on PC3"}}"#)).unwrap();
        let (summary, requests, _) = run(&outbox, &inbox, vec![answered(), answered()], 0, 0);
        assert_eq!(summary.answered, 2);
        assert_eq!(requests, [GuestRequest::Read { cursor: None, owed_after: None }, GuestRequest::Ack { message_id: message.parse().unwrap(),
            next_action: Some("reply on PC3".into()), decline: None, outcome_ref: None, request_id: Some("k-2".into()) }]);
    }

    #[test]
    fn the_outbox_and_the_inbox_are_different_folders() {
        let (_root, outbox, _) = folders();
        let mut spool = Spool { outbox: &outbox, inbox: &outbox, retry_for: Duration::ZERO, sleep: |_| {},
            connect: || -> Result<Fake> { Err(anyhow!("unused")) } };
        assert!(spool.run().is_err());
    }
}
