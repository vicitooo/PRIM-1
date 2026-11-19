//! A111 pane/CLI shaping only. The supervisor checks manage, roam, revision,
//! settings policy and current membership at the actual read/save boundary.
use serde::Deserialize;
use serde_json::{Value, json};
use shared_types::{RoomFallbackAction, RoomFallbackSettings, RoomId, SidebandRequest};

use super::{RpcResult, decode_tool_arguments, parse_room_id};

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
enum Arguments {
    Get { room_id: Option<RoomId> },
    Set { revision: u64, settings: RoomFallbackSettings, room_id: Option<RoomId> },
}

pub(super) fn request(value: Value) -> RpcResult<SidebandRequest> {
    Ok(match decode_tool_arguments::<Arguments>(value)? {
        Arguments::Get { room_id } => SidebandRequest::RoomFallbacks {
            action: RoomFallbackAction::Get, room_id, revision: None, settings: None,
        },
        Arguments::Set { revision, settings, room_id } => SidebandRequest::RoomFallbacks {
            action: RoomFallbackAction::Set, room_id, revision: Some(revision), settings: Some(settings),
        },
    })
}

pub(super) fn definition(roam: bool) -> Value {
    let mut definition = json!({
        "name": "room_fallbacks",
        "title": "Room fallback reminders",
        "description": "Read or save scheduled room reminders. Requires manage; another room also requires roam. Get returns settings, revision and member due/outcome status. Set supplies the complete settings and the revision read from get; a stale revision is refused. PRIM-1 must be running. One schedule per hosted Claude/Codex/Grok member; excludes guests, Terminal and Prime, skips stopped members and missed intervals. Enabling or editing waits the selected interval before the first reminder. Off cancels unattempted reminders, not prompts already written or uncertain. Message and optional brief are literal reminder text, not new permissions or a live instruction from Victor. Every save posts an attributed notice.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": { "enum": ["get", "set"] },
                "revision": { "type": "integer", "minimum": 0 },
                "settings": {
                    "type": "object",
                    "properties": {
                        "enabled": { "type": "boolean" },
                        "interval_minutes": { "type": "integer", "enum": [15, 30, 60] },
                        "message": { "type": "string" },
                        "recipient_mode": { "enum": ["all", "selected"] },
                        "recipient_ids": { "type": "array", "items": { "type": "string", "format": "uuid" } },
                        "overrides": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "session_id": { "type": "string", "format": "uuid" },
                                    "mode": { "enum": ["inherit", "custom", "off"] },
                                    "interval_minutes": { "type": ["integer", "null"], "enum": [15, 30, 60, null] },
                                    "message": { "type": ["string", "null"] },
                                    "brief": { "type": ["string", "null"], "description": "Optional literal text appended to this member's effective reminder as one prompt." }
                                },
                                "required": ["session_id", "mode"],
                                "additionalProperties": false
                            }
                        }
                    },
                    "required": ["enabled", "interval_minutes", "message", "recipient_mode", "recipient_ids", "overrides"],
                    "additionalProperties": false
                }
            },
            "required": ["action"],
            "additionalProperties": false
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
    });
    if roam {
        definition["inputSchema"]["properties"]["room_id"] = json!({
            "type": "string", "format": "uuid", "description": "Omit for your room. A different room additionally requires roam."
        });
    }
    definition
}

pub(super) fn parse_cli(args: &[String]) -> Result<SidebandRequest, String> {
    let (mut value, rest) = match args {
        [action, rest @ ..] if action == "get" => (json!({ "action": "get" }), rest),
        [action, revision, settings, rest @ ..] if action == "set" => {
            let revision: u64 = revision.parse().map_err(|_| "fallback revision must be an unsigned integer".to_string())?;
            let settings: Value = serde_json::from_str(settings).map_err(|error| format!("invalid fallback settings JSON: {error}"))?;
            (json!({ "action": "set", "revision": revision, "settings": settings }), rest)
        }
        _ => return Err("fallbacks takes get [--room ID] or set <revision> <settings-json> [--room ID]".into()),
    };
    match rest {
        [] => {}
        [flag, room] if flag == "--room" => value["room_id"] = json!(parse_room_id(room)?),
        _ => return Err("fallbacks accepts only an optional trailing --room ID; settings must be one literal JSON argument".into()),
    }
    request(value).map_err(|error| error.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Value {
        json!({ "enabled": false, "interval_minutes": 30, "message": "continue $(literal); > not a shell",
            "recipient_mode": "all", "recipient_ids": [], "overrides": [] })
    }

    #[test]
    fn fallback_get_and_set_keep_their_distinct_shapes_and_literal_text() {
        let room_id = RoomId::new_v4();
        assert_eq!(request(json!({ "action": "get" })).unwrap(), SidebandRequest::RoomFallbacks {
            action: RoomFallbackAction::Get, room_id: None, revision: None, settings: None,
        });
        assert_eq!(request(json!({ "action": "set", "room_id": room_id, "revision": 0, "settings": settings() })).unwrap(),
            SidebandRequest::RoomFallbacks { action: RoomFallbackAction::Set, room_id: Some(room_id), revision: Some(0),
                settings: Some(serde_json::from_value(settings()).unwrap()) });
        let mut custom = settings();
        custom["overrides"] = json!([
            { "session_id": uuid::Uuid::new_v4(), "mode": "inherit" },
            { "session_id": uuid::Uuid::new_v4(), "mode": "off", "interval_minutes": null, "message": null, "brief": null },
            { "session_id": uuid::Uuid::new_v4(), "mode": "custom", "interval_minutes": 60, "message": "check", "brief": "literal" }
        ]);
        let decoded = request(json!({ "action": "set", "revision": 3, "settings": custom })).unwrap();
        let SidebandRequest::RoomFallbacks { settings: Some(decoded), .. } = decoded else { panic!("not a fallback set") };
        assert_eq!(decoded.overrides.len(), 3);
        assert_eq!(decoded.overrides[0].brief, None);
        assert_eq!(decoded.overrides[2].brief.as_deref(), Some("literal"));
    }

    #[test]
    fn fallback_arguments_reject_missing_unknown_and_inapplicable_fields() {
        for value in [
            json!({}), json!({ "action": "other" }), json!({ "action": "get", "revision": null }),
            json!({ "action": "get", "settings": settings() }), json!({ "action": "get", "sender": "operator" }),
            json!({ "action": "get", "room_id": "wrong" }), json!({ "action": "set", "settings": settings() }),
            json!({ "action": "set", "revision": 0 }), json!({ "action": "set", "revision": -1, "settings": settings() }),
            json!({ "action": "set", "revision": "0", "settings": settings() }),
            json!({ "action": "set", "revision": 0, "settings": null }),
        ] {
            assert_eq!(request(value.clone()).unwrap_err().code, -32602, "{value}");
        }
        for field in ["enabled", "interval_minutes", "message", "recipient_mode", "recipient_ids", "overrides"] {
            let mut missing = settings();
            missing.as_object_mut().unwrap().remove(field);
            assert!(request(json!({ "action": "set", "revision": 0, "settings": missing })).is_err(), "{field}");
        }
        for (field, value) in [("permissions", json!({ "manage": true })), ("recipient_mode", json!("guests")),
            ("recipient_ids", json!(["not-a-uuid"])),
            ("overrides", json!([{ "session_id": uuid::Uuid::new_v4(), "mode": "off", "sender": "operator" }])),
            ("overrides", json!([{ "session_id": uuid::Uuid::new_v4() }]))] {
            let mut invalid = settings();
            invalid[field] = value;
            assert!(request(json!({ "action": "set", "revision": 0, "settings": invalid })).is_err(), "{field}");
        }
    }

    #[test]
    fn fallback_schema_exposes_fixed_settings_and_room_only_with_roam() {
        for roam in [false, true] {
            for maintain in [false, true] {
                for manage in [false, true] {
                    let listed = super::super::tool_definitions(roam, maintain, manage);
                    assert_eq!(listed.iter().filter(|tool| tool["name"] == "room_fallbacks").count(), usize::from(manage));
                }
            }
        }
        let definition = definition(false);
        let schema = &definition["inputSchema"];
        assert_eq!(schema["additionalProperties"], false);
        assert!(schema["properties"].get("room_id").is_none());
        assert_eq!(schema["required"], json!(["action"]));
        // Harness tool adapters require a plain top-level object. The tagged
        // Arguments enum still enforces the distinct get/set shapes above.
        assert!(schema.get("oneOf").is_none());
        assert!(schema.get("anyOf").is_none());
        assert!(schema.get("allOf").is_none());
        let settings = &schema["properties"]["settings"];
        assert_eq!(settings["required"].as_array().unwrap().len(), 6);
        assert_eq!(settings["additionalProperties"], false);
        assert_eq!(settings["properties"]["interval_minutes"]["enum"], json!([15, 30, 60]));
        let member = &settings["properties"]["overrides"]["items"];
        assert_eq!(member["required"], json!(["session_id", "mode"]));
        assert_eq!(member["additionalProperties"], false);
        assert_eq!(member["properties"]["brief"]["type"], json!(["string", "null"]));
        assert_eq!(super::definition(true)["inputSchema"]["properties"]["room_id"]["format"], "uuid");
    }

    #[test]
    fn fallback_cli_matches_mcp_and_never_reads_settings_from_a_file_or_stdin() {
        let room = RoomId::new_v4().to_string();
        let settings_json = settings().to_string();
        for (args, expected) in [
            (vec!["get"], json!({ "action": "get" })),
            (vec!["get", "--room", &room], json!({ "action": "get", "room_id": room })),
            (vec!["set", "0", &settings_json], json!({ "action": "set", "revision": 0, "settings": settings() })),
            (vec!["set", "12", &settings_json, "--room", &room], json!({ "action": "set", "revision": 12, "settings": settings(), "room_id": room })),
        ] {
            let mut full = vec!["fallbacks".to_owned()];
            full.extend(args.into_iter().map(str::to_owned));
            assert_eq!(super::super::parse_room_cli(&full).unwrap(), request(expected).unwrap());
        }
        for args in [
            vec![], vec!["get", "0"], vec!["get", "--room"], vec!["get", "--room", "wrong"],
            vec!["get", "--room", &room, "--room", &room], vec!["set"], vec!["set", "0"],
            vec!["set", "-1", &settings_json], vec!["set", "18446744073709551616", &settings_json],
            vec!["set", "0", "-"], vec!["set", "0", "--file", "settings.json"],
            vec!["set", "0", "{}"], vec!["set", "0", &settings_json, "extra"],
        ] {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            assert!(parse_cli(&args).is_err(), "{args:?}");
        }
    }
}
