//! A16 room tools: room_create, room_settings, room_delete and room_guests,
//! and their `--prim1-room room|guests …` CLI twins. Both send one
//! `SidebandRequest::Room`; the supervisor checks manage (and roam for a room
//! that is not the caller's) and the request_id.
use serde::Deserialize;
use serde_json::{Value, json};
use shared_types::{RoomAction, RoomFeedCursor, RoomId, SidebandRequest};

use super::{RpcFailure, RpcResult, cli_content, decode_tool_arguments, parse_room_id};

/// A guest id: a UUID, as every PRIM-1 id.
type GuestId = RoomId;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateArguments {
    request_id: String,
    label: String,
    #[serde(default)]
    brief_on_join: Option<bool>,
    #[serde(default)]
    brief_template: Option<String>,
    #[serde(default)]
    resume_on_restart: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    room_id: Option<RoomId>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    resume_on_restart: Option<bool>,
    #[serde(default)]
    brief_on_join: Option<bool>,
    #[serde(default)]
    brief_template: Option<String>,
    #[serde(default)]
    position: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DeleteArguments {
    request_id: String,
    room_id: RoomId,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArguments {
    request_id: String,
    #[serde(default)]
    room_id: Option<RoomId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestsArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    room_id: Option<RoomId>,
    #[serde(default)]
    label: Option<String>,
    #[serde(default)]
    guest_id: Option<GuestId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RetryArguments {
    request_id: String,
    #[serde(default)]
    room_id: Option<RoomId>,
    message_id: RoomId,
    recipient: String,
    receipt: RoomFeedCursor,
}

pub(crate) const TOOL_NAMES: [&str; 7] = ["room_create", "room_settings", "room_delete", "room_guests", "room_start", "room_stop",
    "room_retry"];

pub(crate) fn request(name: &str, arguments: Value) -> RpcResult<SidebandRequest> {
    let invalid = |message: &str| RpcFailure::new(-32602, format!("invalid tool arguments: {message}"));
    Ok(match name {
        "room_create" => {
            let CreateArguments { request_id, label, brief_on_join, brief_template, resume_on_restart } =
                decode_tool_arguments(arguments)?;
            SidebandRequest::Room { request_id: Some(request_id),
                action: RoomAction::Create { label, brief_on_join, brief_template, resume_on_restart } }
        }
        "room_settings" => {
            let SettingsArguments { action, request_id, room_id, label, resume_on_restart, brief_on_join,
                brief_template, position } = decode_tool_arguments(arguments)?;
            match action.as_str() {
                "get" => {
                    if request_id.is_some() || label.is_some() || resume_on_restart.is_some() || brief_on_join.is_some()
                        || brief_template.is_some() || position.is_some() {
                        return Err(invalid("get takes only room_id"));
                    }
                    SidebandRequest::Room { request_id: None, action: RoomAction::Get { room_id } }
                }
                "set" => SidebandRequest::Room { request_id: Some(request_id.ok_or_else(|| invalid("set needs request_id"))?),
                    action: RoomAction::Update { room_id, label, resume_on_restart, brief_on_join, brief_template, position } },
                _ => return Err(invalid("action must be get or set")),
            }
        }
        "room_start" | "room_stop" => {
            let RunArguments { request_id, room_id } = decode_tool_arguments(arguments)?;
            let action = if name == "room_start" { RoomAction::Start { room_id } } else { RoomAction::Stop { room_id } };
            SidebandRequest::Room { request_id: Some(request_id), action }
        }
        "room_retry" => {
            let RetryArguments { request_id, room_id, message_id, recipient, receipt } = decode_tool_arguments(arguments)?;
            SidebandRequest::Room { request_id: Some(request_id), action: RoomAction::Retry { room_id, message_id, recipient, receipt } }
        }
        "room_delete" => {
            let DeleteArguments { request_id, room_id } = decode_tool_arguments(arguments)?;
            SidebandRequest::Room { request_id: Some(request_id), action: RoomAction::Delete { room_id } }
        }
        "room_guests" => {
            let GuestsArguments { action, request_id, room_id, label, guest_id } = decode_tool_arguments(arguments)?;
            match action.as_str() {
                "list" if request_id.is_none() && label.is_none() && guest_id.is_none() =>
                    SidebandRequest::Room { request_id: None, action: RoomAction::GuestList { room_id } },
                "list" => return Err(invalid("list takes only room_id")),
                "invite" if guest_id.is_none() => SidebandRequest::Room {
                    request_id: Some(request_id.ok_or_else(|| invalid("invite needs request_id"))?),
                    action: RoomAction::GuestInvite { room_id, label } },
                "revoke" if room_id.is_none() && label.is_none() => SidebandRequest::Room {
                    request_id: Some(request_id.ok_or_else(|| invalid("revoke needs request_id"))?),
                    action: RoomAction::GuestRevoke { guest_id: guest_id.ok_or_else(|| invalid("revoke needs guest_id"))? } },
                "invite" | "revoke" => return Err(invalid("invite takes room_id and label; revoke takes guest_id")),
                _ => return Err(invalid("action must be list, invite or revoke")),
            }
        }
        _ => unreachable!("only room tool names are routed here"),
    })
}

/// The four tools, for members holding manage; `roam` adds room_id where a
/// room other than the caller's can be named.
pub(crate) fn definitions(roam: bool) -> Vec<Value> {
    let request_id = json!({ "type": "string", "format": "uuid",
        "description": "A fresh UUID per change. Reuse it only to retry this same change after an uncertain result: the retry returns the first result and repeats nothing." });
    let room_id = json!({ "type": "string", "format": "uuid", "description": "Another room's id from rooms_list. Omit for your own room." });
    let mut settings = json!({
        "name": "room_settings",
        "title": "Read or change a room's settings",
        "description": "The operator's room settings. action \"get\" returns the room, its brief settings (brief_on_join, brief_template; null is the built-in brief, also returned as default_brief) and its position in the room order. action \"set\" (with request_id) changes any of: label, resume_on_restart (Resume on restart: its running agents relaunch when PRIM-1 starts), brief_on_join (the room brief as the first prompt of a fresh conversation), brief_template (\"\" restores the built-in brief; placeholders {room_label} {members} {your_label} {tools} {roaming}), position (0-based place in the room order). Fields already as asked change nothing. The room reads \"<you> updated the room: …\".",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action"], "properties": {
            "action": { "enum": ["get", "set"] }, "request_id": request_id,
            "label": { "type": "string", "minLength": 1 }, "resume_on_restart": { "type": "boolean" },
            "brief_on_join": { "type": "boolean" }, "brief_template": { "type": "string" },
            "position": { "type": "integer", "minimum": 0 } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    });
    let mut guests = json!({
        "name": "room_guests",
        "title": "Invite, list or revoke outside agents",
        "description": "Outside agents (guests) as the operator invites them: action \"list\" shows the room's guests; \"invite\" (request_id, optional label) binds a new guest to the room and returns its token with a ready MCP config and CLI example once, to you only: it is never posted in the feed and a retry returns the guest without it. Hand it only to the outside agent it is for. \"revoke\" (request_id, guest_id) retires a guest; its queued messages are held. A guest is never a member and cannot use member or room tools. The room reads \"<you> invited|revoked outside agent <label>\".",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action"], "properties": {
            "action": { "enum": ["list", "invite", "revoke"] }, "request_id": request_id,
            "label": { "type": "string", "minLength": 1 }, "guest_id": { "type": "string", "format": "uuid" } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": true }
    });
    let run = |name: &str, title: &str, description: &str| {
        let mut tool = json!({
            "name": name, "title": title, "description": description,
            "inputSchema": { "type": "object", "additionalProperties": false, "required": ["request_id"],
                "properties": { "request_id": request_id } },
            "annotations": { "readOnlyHint": false, "destructiveHint": name == "room_stop", "idempotentHint": true, "openWorldHint": false }
        });
        if roam {
            tool["inputSchema"]["properties"]["room_id"] = room_id.clone();
        }
        tool
    };
    let start = run("room_start", "Start every member of a room",
        "Launch every stopped member of the room (each resumes its own conversation, as Launch does); you are never one of them. After a PC restart this brings the room back whatever Resume on restart recorded. A member that fails is named in warnings; the others still start. The room reads \"<you> started the room: …\".");
    let stop = run("room_stop", "Stop every member of a room",
        "Stop every running member of the room but you (stopping yourself would end the run that asked). Each stays in the room. The room reads \"<you> stopped the room: …\".");
    let cursor = json!({ "type": "object", "additionalProperties": false, "required": ["epoch", "sequence"],
        "properties": { "epoch": { "type": "string", "format": "uuid" }, "sequence": { "type": "integer", "minimum": 0 } } });
    let mut retry = json!({
        "name": "room_retry",
        "title": "Retry a held or failed wake",
        "description": "The operator's Retry wake: attempt again one recipient's held or failed wake of a stored message, named by message_id, the recipient (label or session id) and receipt, that receipt's cursor in room_read. Never a second post and never a recipient it already reached; a recipient that restarted since, or is not ready, is refused (the message stays readable). The result carries the delivery outcome; the room reads \"<you> retried the wake of message <id> for <label>\".",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["request_id", "message_id", "recipient", "receipt"],
            "properties": { "request_id": request_id, "message_id": { "type": "string", "format": "uuid" },
                "recipient": { "type": "string", "minLength": 1 }, "receipt": cursor } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    });
    if roam {
        settings["inputSchema"]["properties"]["room_id"] = room_id.clone();
        retry["inputSchema"]["properties"]["room_id"] = room_id.clone();
        guests["inputSchema"]["properties"]["room_id"] = room_id;
    }
    vec![
        json!({
            "name": "room_create",
            "title": "Create a room",
            "description": "Create a new, empty room as the operator does; you stay where you are. brief_on_join defaults to true; brief_template omitted is the built-in brief; resume_on_restart defaults to false. Fill it with member_create (room_id) or member_add, which need roam for a room that is not yours. The new room and yours read \"<you> created …\".",
            "inputSchema": { "type": "object", "additionalProperties": false, "required": ["request_id", "label"], "properties": {
                "request_id": request_id, "label": { "type": "string", "minLength": 1 },
                "brief_on_join": { "type": "boolean" }, "brief_template": { "type": "string" },
                "resume_on_restart": { "type": "boolean" } } },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
        settings,
        json!({
            "name": "room_delete",
            "title": "Delete a room",
            "description": "The operator's Delete room: its sessions go to the lobby (they are not stopped or deleted) and its feed history is erased. Refused while a delivery is in flight. Your own room needs manage; another room also needs roam. Your room (when it is another) and the system log record it.",
            "inputSchema": { "type": "object", "additionalProperties": false, "required": ["request_id", "room_id"], "properties": {
                "request_id": request_id, "room_id": { "type": "string", "format": "uuid" } } },
            "annotations": { "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false }
        }),
        guests,
        start,
        stop,
        retry,
    ]
}

/// `room create|get|set|delete …` and `guests list|invite|revoke …`.
pub(crate) fn parse_cli(command: &str, args: &[String]) -> Result<SidebandRequest, String> {
    let (op, mut rest) = args.split_first().ok_or_else(|| format!("{command} needs an action"))?;
    let mut request_id = None;
    let mut room_id = None;
    let mut label = None;
    let mut resume = None;
    let mut brief_on_join = None;
    let mut brief_template = None;
    let mut position = None;
    let (mut message, mut recipient, mut receipt) = (None, None, None);
    let mut positional = Vec::new();
    let on_off = |flag: &str, value: &str| match value {
        "on" | "true" => Ok(true),
        "off" | "false" => Ok(false),
        other => Err(format!("{flag} takes on or off, not '{other}'")),
    };
    while let Some((flag, tail)) = rest.split_first() {
        let value = || tail.first().filter(|value| !value.starts_with("--")).cloned().ok_or_else(|| format!("{flag} needs a value"));
        let once = |seen: bool| if seen { Err(format!("{flag} is given twice")) } else { Ok(()) };
        rest = match flag.as_str() {
            "--request-id" => {
                once(request_id.is_some())?;
                let id = value()?;
                if id.parse::<GuestId>().is_err() {
                    return Err(format!("--request-id '{id}' is not a UUID"));
                }
                request_id = Some(id);
                &tail[1..]
            }
            "--room" => { once(room_id.is_some())?; room_id = Some(parse_room_id(&value()?)?); &tail[1..] }
            "--label" => { once(label.is_some())?; label = Some(value()?); &tail[1..] }
            "--resume" => { once(resume.is_some())?; resume = Some(on_off(flag, &value()?)?); &tail[1..] }
            "--brief-on-join" => { once(brief_on_join.is_some())?; brief_on_join = Some(on_off(flag, &value()?)?); &tail[1..] }
            "--brief-text" => { once(brief_template.is_some())?; brief_template = Some(value()?); &tail[1..] }
            "--brief-file" => { once(brief_template.is_some())?; brief_template = Some(cli_content(&["--file".into(), value()?])?); &tail[1..] }
            "--brief-default" => { once(brief_template.is_some())?; brief_template = Some(String::new()); tail }
            "--position" => {
                once(position.is_some())?;
                let text = value()?;
                position = Some(text.parse().map_err(|_| format!("--position takes a number, not '{text}'"))?);
                &tail[1..]
            }
            "--message" if command == "room" => { once(message.is_some())?; message = Some(parse_room_id(&value()?)?); &tail[1..] }
            "--recipient" if command == "room" => { once(recipient.is_some())?; recipient = Some(value()?); &tail[1..] }
            "--receipt" if command == "room" => {
                once(receipt.is_some())?;
                let json = value()?;
                receipt = Some(serde_json::from_str::<RoomFeedCursor>(&json)
                    .map_err(|error| format!("--receipt must be {{\"epoch\",\"sequence\"}} JSON: {error}"))?);
                &tail[1..]
            }
            other if other.starts_with("--") => return Err(format!("unknown {command} {op} flag '{other}'")),
            other => { positional.push(other.to_string()); tail }
        };
    }
    let needs_id = |request_id: Option<String>| request_id.ok_or_else(|| format!("{command} {op} needs --request-id <uuid>"));
    let no_settings = label.is_none() && resume.is_none() && brief_on_join.is_none() && brief_template.is_none() && position.is_none();
    let action = match (command, op.as_str(), positional.as_slice()) {
        ("room", "create", [name]) if room_id.is_none() && position.is_none() && label.is_none() => {
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?), action: RoomAction::Create {
                label: name.clone(), brief_on_join, brief_template, resume_on_restart: resume } });
        }
        ("room", "get", []) if request_id.is_none() && no_settings => RoomAction::Get { room_id },
        ("room", "set", []) if !no_settings => {
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?), action: RoomAction::Update {
                room_id, label, resume_on_restart: resume, brief_on_join, brief_template, position } });
        }
        ("room", "delete", [room]) if room_id.is_none() && no_settings => {
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?),
                action: RoomAction::Delete { room_id: parse_room_id(room)? } });
        }
        ("room", "start" | "stop", []) if no_settings => {
            let action = if op == "start" { RoomAction::Start { room_id } } else { RoomAction::Stop { room_id } };
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?), action });
        }
        ("room", "retry", []) if no_settings => {
            let (Some(message_id), Some(recipient), Some(receipt)) = (message, recipient, receipt) else {
                return Err("room retry needs --message <id> --recipient <member> --receipt '<cursor json>'".into());
            };
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?),
                action: RoomAction::Retry { room_id, message_id, recipient, receipt } });
        }
        ("guests", "list", []) if request_id.is_none() && no_settings => RoomAction::GuestList { room_id },
        ("guests", "invite", []) if resume.is_none() && brief_on_join.is_none() && brief_template.is_none() && position.is_none() => {
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?),
                action: RoomAction::GuestInvite { room_id, label } });
        }
        ("guests", "revoke", [guest]) if room_id.is_none() && no_settings => {
            let guest_id = guest.parse::<GuestId>().map_err(|_| format!("'{guest}' is not a guest id"))?;
            return Ok(SidebandRequest::Room { request_id: Some(needs_id(request_id)?), action: RoomAction::GuestRevoke { guest_id } });
        }
        _ => return Err(format!("{command} {op}: wrong arguments (see usage)")),
    };
    Ok(SidebandRequest::Room { request_id: None, action })
}
