//! A16 runtime tools: runtime_settings, upgrade_control and telemetry
//! (maintain), local_conversations and room_package (manage), and their
//! `--prim1-room settings|upgrade-control|telemetry|conversations|room-package …`
//! CLI twins. Each sends one `SidebandRequest::Runtime`; the supervisor checks
//! the grant, the scope and the request_id.
use serde::Deserialize;
use serde_json::{Value, json};
use shared_types::{RuntimeAction, SidebandRequest};

use super::{RpcFailure, RpcResult, decode_tool_arguments};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SettingsArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    task_folder: Option<String>,
    #[serde(default)]
    personal_repo: Option<String>,
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConversationsArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    member: Option<String>,
    #[serde(default)]
    conversation_id: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpgradeControlArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    attempt_id: Option<shared_types::RoomId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageArguments {
    action: String,
    #[serde(default)]
    request_id: Option<String>,
    path: String,
    #[serde(default)]
    room_id: Option<shared_types::RoomId>,
    #[serde(default)]
    source_room_id: Option<shared_types::RoomId>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

pub(crate) const TOOL_NAMES: [&str; 5] = ["runtime_settings", "local_conversations", "upgrade_control", "telemetry", "room_package"];

pub(crate) fn request(name: &str, arguments: Value) -> RpcResult<SidebandRequest> {
    let invalid = |message: &str| RpcFailure::new(-32602, format!("invalid tool arguments: {message}"));
    Ok(match name {
        "runtime_settings" => {
            let SettingsArguments { action, request_id, task_folder, personal_repo, workspace } = decode_tool_arguments(arguments)?;
            match action.as_str() {
                "get" if request_id.is_none() && task_folder.is_none() && personal_repo.is_none() && workspace.is_none() =>
                    SidebandRequest::Runtime { request_id: None, action: RuntimeAction::SettingsGet {} },
                "get" => return Err(invalid("get takes no other field")),
                "set" => SidebandRequest::Runtime { request_id: Some(request_id.ok_or_else(|| invalid("set needs request_id"))?),
                    action: RuntimeAction::SettingsSet { task_folder, personal_repo, workspace } },
                _ => return Err(invalid("action must be get or set")),
            }
        }
        "local_conversations" => {
            let ConversationsArguments { action, request_id, member, conversation_id } = decode_tool_arguments(arguments)?;
            match action.as_str() {
                "scan" if request_id.is_none() && member.is_none() && conversation_id.is_none() =>
                    SidebandRequest::Runtime { request_id: None, action: RuntimeAction::ConversationsScan {} },
                "scan" => return Err(invalid("scan takes no other field")),
                "attach" => SidebandRequest::Runtime {
                    request_id: Some(request_id.ok_or_else(|| invalid("attach needs request_id"))?),
                    action: RuntimeAction::ConversationAttach {
                        member: member.ok_or_else(|| invalid("attach needs member"))?,
                        conversation_id: conversation_id.ok_or_else(|| invalid("attach needs conversation_id"))?,
                    } },
                _ => return Err(invalid("action must be scan or attach")),
            }
        }
        "upgrade_control" => {
            let UpgradeControlArguments { action, request_id, attempt_id } = decode_tool_arguments(arguments)?;
            match (action.as_str(), request_id, attempt_id) {
                ("status", None, None) => SidebandRequest::Runtime { request_id: None, action: RuntimeAction::UpgradeStatus {} },
                ("status", _, _) => return Err(invalid("status takes no other field")),
                ("cancel" | "abandon", Some(request_id), Some(attempt_id)) => SidebandRequest::Runtime { request_id: Some(request_id),
                    action: if action == "cancel" { RuntimeAction::UpgradeCancel { attempt_id } } else { RuntimeAction::UpgradeAbandon { attempt_id } } },
                ("cancel" | "abandon", _, _) => return Err(invalid("cancel and abandon need request_id and attempt_id")),
                _ => return Err(invalid("action must be status, cancel or abandon")),
            }
        }
        "telemetry" => {
            let EmptyArguments {} = decode_tool_arguments(arguments)?;
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::Telemetry {} }
        }
        "room_package" => {
            let PackageArguments { action, request_id, path, room_id, source_room_id } = decode_tool_arguments(arguments)?;
            match action.as_str() {
                "save" if source_room_id.is_none() => SidebandRequest::Runtime {
                    request_id: Some(request_id.ok_or_else(|| invalid("save needs request_id"))?),
                    action: RuntimeAction::RoomSave { room_id, path } },
                "inspect" if request_id.is_none() && room_id.is_none() && source_room_id.is_none() =>
                    SidebandRequest::Runtime { request_id: None, action: RuntimeAction::RoomInspect { path } },
                "load" if room_id.is_none() => SidebandRequest::Runtime {
                    request_id: Some(request_id.ok_or_else(|| invalid("load needs request_id"))?),
                    action: RuntimeAction::RoomLoad { path, source_room_id } },
                "save" | "inspect" | "load" =>
                    return Err(invalid("save takes request_id, path, room_id; inspect takes path only; load takes request_id, path, source_room_id")),
                _ => return Err(invalid("action must be save, inspect or load")),
            }
        }
        _ => unreachable!("only runtime tool names are routed here"),
    })
}

pub(crate) fn settings_definition() -> Value {
    json!({
        "name": "runtime_settings",
        "title": "Read or change PRIM-1's runtime settings",
        "description": "The operator's Settings page: action \"get\" returns the task folder (its status), the personal repository (the session register) and the default workspace new sessions start in. action \"set\" (with request_id) changes any of them through the operator's own setter: task_folder opens and locks the folder first (a folder locked by another runtime is refused and the current one kept); personal_repo must exist; workspace must be an existing folder. Your room and the system log record each change.",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action"], "properties": {
            "action": { "enum": ["get", "set"] },
            "request_id": { "type": "string", "format": "uuid", "description": "A fresh UUID per change; reuse only to retry it." },
            "task_folder": { "type": "string", "description": "Absolute folder path." },
            "personal_repo": { "type": "string", "description": "Absolute folder path." },
            "workspace": { "type": "string", "description": "Absolute folder path." } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

pub(crate) fn upgrade_control_definition() -> Value {
    json!({
        "name": "upgrade_control",
        "title": "Watch, cancel or abandon a PRIM-1 upgrade",
        "description": "The operator's upgrade banner. action \"status\" returns the current self-upgrade attempt (attempt_id, stage, message, can_cancel, can_abandon) or none. \"cancel\" (request_id, attempt_id) stops an upgrade during its notice, as Victor's Cancel does; the rooms read \"…cancelled: <you> cancelled it\". \"abandon\" (request_id, attempt_id) puts a terminal note on a held attempt: it proves nothing stopped, so first check that only one PRIM-1 runs for this runtime. Your room records each cancel or abandon under your name. A new upgrade still goes through upgrade_request and its cancellable notice.",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action"], "properties": {
            "action": { "enum": ["status", "cancel", "abandon"] },
            "request_id": { "type": "string", "format": "uuid", "description": "A fresh UUID per change; reuse it only to retry it." },
            "attempt_id": { "type": "string", "format": "uuid", "description": "From status." } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

pub(crate) fn telemetry_definition() -> Value {
    json!({
        "name": "telemetry",
        "title": "Read PRIM-1's resource sample",
        "description": "The telemetry panel's sample: PRIM-1's own process and each member's process tree (CPU, memory), as the operator's panel shows them. Read only.",
        "inputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
        "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
    })
}

pub(crate) fn room_package_definition() -> Value {
    json!({
        "name": "room_package",
        "title": "Save, inspect or load a room",
        "description": "Rooms → Save room… and Load room…. action \"save\" (request_id, path, room_id?) writes one room (its settings, its members' identities and launch choices, and its feed; never harness conversation files) to a NEW .prim1room file at an absolute path: your room by default, another room also needs roam; an existing file is never replaced. \"inspect\" (path) lists the rooms a .prim1room file or a copied runtime's room-catalog-v1.json offers; it reads only. \"load\" (request_id, path, source_room_id?) creates a NEW room with NEW members from it: nothing starts, no member is given a grant, and a member whose conversation is already used in this PRIM-1 stays held. The saved room, the loaded room and your own room record it.",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action", "path"], "properties": {
            "action": { "enum": ["save", "inspect", "load"] },
            "request_id": { "type": "string", "format": "uuid", "description": "A fresh UUID per save or load; reuse it only to retry it." },
            "path": { "type": "string", "minLength": 1, "description": "Absolute path: a .prim1room file, or for inspect and load a copied runtime's room-catalog-v1.json." },
            "room_id": { "type": "string", "format": "uuid", "description": "save: the room to save (default: yours)." },
            "source_room_id": { "type": "string", "format": "uuid", "description": "load: the room of a runtime copy, from inspect." } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    })
}

pub(crate) fn conversations_definition() -> Value {
    json!({
        "name": "local_conversations",
        "title": "Find and attach local conversations",
        "description": "Settings → Local conversations. action \"scan\" lists the Claude, Codex and Grok conversations PRIM-1 finds for the personal repository (metadata only, bounded; no transcript text). action \"attach\" (request_id, member, conversation_id) binds one to a fully stopped member of the same driver for recovery; nothing starts, and the member is held until member_start with mode resume_here (close the conversation everywhere else first). Another room or the lobby also needs roam. The member's room records it.",
        "inputSchema": { "type": "object", "additionalProperties": false, "required": ["action"], "properties": {
            "action": { "enum": ["scan", "attach"] },
            "request_id": { "type": "string", "format": "uuid" },
            "member": { "type": "string", "minLength": 1, "description": "Label (unique in your room) or session id." },
            "conversation_id": { "type": "string", "format": "uuid" } } },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    })
}

/// `settings get|set …`, `upgrade-control …`, `conversations scan|attach …`
/// and `room-package save|inspect|load …`.
pub(crate) fn parse_cli(command: &str, args: &[String]) -> Result<SidebandRequest, String> {
    let (op, mut rest) = args.split_first().ok_or_else(|| format!("{command} needs an action"))?;
    let (mut request_id, mut task_folder, mut personal_repo, mut workspace) = (None, None, None, None);
    let (mut room, mut source_room) = (None, None);
    let mut positional = Vec::new();
    while let Some((flag, tail)) = rest.split_first() {
        let value = || tail.first().filter(|value| !value.starts_with("--")).cloned().ok_or_else(|| format!("{flag} needs a value"));
        let set = |slot: &mut Option<String>, value: String| if slot.replace(value).is_some() {
            Err(format!("{flag} is given twice"))
        } else { Ok(()) };
        rest = match flag.as_str() {
            "--request-id" => {
                let id = value()?;
                if id.parse::<shared_types::RoomId>().is_err() {
                    return Err(format!("--request-id '{id}' is not a UUID"));
                }
                set(&mut request_id, id)?;
                &tail[1..]
            }
            "--task-folder" if command == "settings" => { set(&mut task_folder, value()?)?; &tail[1..] }
            "--personal-repo" if command == "settings" => { set(&mut personal_repo, value()?)?; &tail[1..] }
            "--workspace" if command == "settings" => { set(&mut workspace, value()?)?; &tail[1..] }
            "--room" if command == "room-package" => { set(&mut room, value()?)?; &tail[1..] }
            "--source-room" if command == "room-package" => { set(&mut source_room, value()?)?; &tail[1..] }
            other if other.starts_with("--") => return Err(format!("unknown {command} {op} flag '{other}'")),
            other => { positional.push(other.to_string()); tail }
        };
    }
    let needs_id = |request_id: Option<String>| request_id.ok_or_else(|| format!("{command} {op} needs --request-id <uuid>"));
    let no_folders = task_folder.is_none() && personal_repo.is_none() && workspace.is_none();
    let room_id = |flag: &str, value: Option<String>| value.map(|id| id.parse::<shared_types::RoomId>()
        .map_err(|_| format!("{flag} '{id}' is not a room id"))).transpose();
    let (room, source_room) = (room_id("--room", room)?, room_id("--source-room", source_room)?);
    Ok(match (command, op.as_str(), positional.as_slice()) {
        ("settings", "get", []) if request_id.is_none() && no_folders =>
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::SettingsGet {} },
        ("settings", "set", []) if !no_folders => SidebandRequest::Runtime { request_id: Some(needs_id(request_id)?),
            action: RuntimeAction::SettingsSet { task_folder, personal_repo, workspace } },
        ("upgrade-control", "status", []) if request_id.is_none() && no_folders =>
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::UpgradeStatus {} },
        ("upgrade-control", "cancel" | "abandon", [attempt]) if no_folders => {
            let attempt_id = attempt.parse::<shared_types::RoomId>().map_err(|_| format!("'{attempt}' is not an attempt id"))?;
            SidebandRequest::Runtime { request_id: Some(needs_id(request_id)?), action: if op == "cancel" {
                RuntimeAction::UpgradeCancel { attempt_id } } else { RuntimeAction::UpgradeAbandon { attempt_id } } }
        }
        ("room-package", "save", [path]) if source_room.is_none() => SidebandRequest::Runtime {
            request_id: Some(needs_id(request_id)?), action: RuntimeAction::RoomSave { room_id: room, path: path.clone() } },
        ("room-package", "inspect", [path]) if request_id.is_none() && room.is_none() && source_room.is_none() =>
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::RoomInspect { path: path.clone() } },
        ("room-package", "load", [path]) if room.is_none() => SidebandRequest::Runtime {
            request_id: Some(needs_id(request_id)?), action: RuntimeAction::RoomLoad { path: path.clone(), source_room_id: source_room } },
        ("conversations", "scan", []) if request_id.is_none() =>
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::ConversationsScan {} },
        ("conversations", "attach", [member, conversation_id]) => SidebandRequest::Runtime {
            request_id: Some(needs_id(request_id)?),
            action: RuntimeAction::ConversationAttach { member: member.clone(), conversation_id: conversation_id.clone() } },
        _ => return Err(format!("{command} {op}: wrong arguments (see usage)")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_cli_and_tools_shape_settings_and_conversations() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "6f0d2b8e-1c3a-4b5d-9e7f-0a1b2c3d4e5f";
        assert_eq!(parse_cli("settings", &args(&["get"])).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::SettingsGet {} });
        assert_eq!(parse_cli("settings", &args(&["set", "--request-id", id, "--task-folder", r"C:\Users\operator\prim1-tasks\PC3"])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::SettingsSet {
                task_folder: Some(r"C:\Users\operator\prim1-tasks\PC3".into()), personal_repo: None, workspace: None } });
        assert_eq!(parse_cli("conversations", &args(&["attach", "--request-id", id, "Lead", id])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::ConversationAttach {
                member: "Lead".into(), conversation_id: id.into() } });
        for (command, bad) in [("settings", vec!["set", "--request-id", id]), ("settings", vec!["set", "--task-folder", "x"]),
            ("settings", vec!["get", "--request-id", id]), ("conversations", vec!["attach", "--request-id", id, "Lead"]),
            ("conversations", vec!["scan", "--task-folder", "x"]), ("settings", vec!["rename"])] {
            assert!(parse_cli(command, &args(&bad)).is_err(), "{command} {bad:?}");
        }
        assert_eq!(request("local_conversations", json!({ "action": "scan" })).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::ConversationsScan {} });
        for (name, arguments) in [("runtime_settings", json!({ "action": "get", "request_id": id })),
            ("runtime_settings", json!({ "action": "set", "workspace": "x" })),
            ("local_conversations", json!({ "action": "attach", "request_id": id, "member": "x" })),
            ("local_conversations", json!({ "action": "scan", "member": "x" }))] {
            assert!(request(name, arguments.clone()).is_err(), "{name} {arguments}");
        }
    }

    #[test]
    fn runtime_cli_and_tools_shape_upgrade_control_and_telemetry() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "6f0d2b8e-1c3a-4b5d-9e7f-0a1b2c3d4e5f";
        let attempt = "0a1b2c3d-4e5f-4b5d-9e7f-6f0d2b8e1c3a";
        let attempt_id = attempt.parse::<shared_types::RoomId>().unwrap();
        assert_eq!(parse_cli("upgrade-control", &args(&["status"])).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::UpgradeStatus {} });
        assert_eq!(parse_cli("upgrade-control", &args(&["cancel", "--request-id", id, attempt])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::UpgradeCancel { attempt_id } });
        assert_eq!(parse_cli("upgrade-control", &args(&["abandon", "--request-id", id, attempt])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::UpgradeAbandon { attempt_id } });
        for bad in [vec!["status", "--request-id", id], vec!["status", attempt], vec!["cancel", attempt],
            vec!["cancel", "--request-id", id], vec!["abandon", "--request-id", id, "not-an-id"],
            vec!["cancel", "--request-id", id, attempt, "--task-folder", "x"], vec!["pause"]] {
            assert!(parse_cli("upgrade-control", &args(&bad)).is_err(), "{bad:?}");
        }
        assert_eq!(crate::parse_room_cli(&args(&["telemetry"])).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::Telemetry {} });
        assert!(crate::parse_room_cli(&args(&["telemetry", "now"])).is_err());

        assert_eq!(request("upgrade_control", json!({ "action": "status" })).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::UpgradeStatus {} });
        assert_eq!(request("upgrade_control", json!({ "action": "cancel", "request_id": id, "attempt_id": attempt })).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::UpgradeCancel { attempt_id } });
        assert_eq!(request("telemetry", json!({})).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::Telemetry {} });
        for (name, arguments) in [("upgrade_control", json!({ "action": "status", "attempt_id": attempt })),
            ("upgrade_control", json!({ "action": "abandon", "attempt_id": attempt })),
            ("upgrade_control", json!({ "action": "cancel", "request_id": id })),
            ("upgrade_control", json!({ "action": "rollback" })),
            ("telemetry", json!({ "detail": true }))] {
            assert!(request(name, arguments.clone()).is_err(), "{name} {arguments}");
        }
    }

    #[test]
    fn runtime_cli_and_tools_shape_room_packages() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let id = "6f0d2b8e-1c3a-4b5d-9e7f-0a1b2c3d4e5f";
        let room = "0a1b2c3d-4e5f-4b5d-9e7f-6f0d2b8e1c3a";
        let room_id = room.parse::<shared_types::RoomId>().unwrap();
        let file = r"D:\rooms\Alpha.prim1room";
        assert_eq!(parse_cli("room-package", &args(&["save", "--request-id", id, "--room", room, file])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::RoomSave { room_id: Some(room_id), path: file.into() } });
        assert_eq!(parse_cli("room-package", &args(&["inspect", file])).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::RoomInspect { path: file.into() } });
        assert_eq!(parse_cli("room-package", &args(&["load", "--request-id", id, "--source-room", room, file])).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::RoomLoad { path: file.into(), source_room_id: Some(room_id) } });
        for bad in [vec!["save", file], vec!["save", "--request-id", id, "--source-room", room, file],
            vec!["inspect", "--request-id", id, file], vec!["inspect", "--room", room, file], vec!["load", file],
            vec!["load", "--request-id", id, "--room", room, file], vec!["load", "--request-id", id, "--source-room", "x", file],
            vec!["load", "--request-id", id], vec!["save", "--request-id", id, "--task-folder", "x", file], vec!["copy", file]] {
            assert!(parse_cli("room-package", &args(&bad)).is_err(), "{bad:?}");
        }
        assert!(parse_cli("settings", &args(&["get", "--room", room])).is_err(), "--room is room-package's");

        assert_eq!(request("room_package", json!({ "action": "save", "request_id": id, "path": file })).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::RoomSave { room_id: None, path: file.into() } });
        assert_eq!(request("room_package", json!({ "action": "inspect", "path": file })).unwrap(),
            SidebandRequest::Runtime { request_id: None, action: RuntimeAction::RoomInspect { path: file.into() } });
        assert_eq!(request("room_package", json!({ "action": "load", "request_id": id, "path": file, "source_room_id": room })).unwrap(),
            SidebandRequest::Runtime { request_id: Some(id.into()), action: RuntimeAction::RoomLoad { path: file.into(), source_room_id: Some(room_id) } });
        for arguments in [json!({ "action": "save", "path": file }), json!({ "action": "load", "path": file }),
            json!({ "action": "inspect", "request_id": id, "path": file }), json!({ "action": "inspect" }),
            json!({ "action": "save", "request_id": id, "path": file, "source_room_id": room }),
            json!({ "action": "load", "request_id": id, "path": file, "room_id": room }),
            json!({ "action": "export", "path": file }), json!({ "action": "inspect", "path": file, "overwrite": true })] {
            assert!(request("room_package", arguments.clone()).is_err(), "{arguments}");
        }
    }
}
