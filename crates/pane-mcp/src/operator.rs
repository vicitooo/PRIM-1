//! `<prim1 exe> --prim1-operator [--runtime <dir>] [--actor <label>]
//! [--token-file <path>] <command…>` (A16): the operator entry for a local
//! process of the same Windows user that is not a pane, for example a
//! supervisor's script on this PC over the SSH mesh. It finds PRIM-1 through
//! `<runtime>/guest-endpoint.json` as the guest adapter does, pins the exact
//! server process, and sends one request after an operator preamble. The
//! commands are the `--prim1-room` member, room, guest and fallback commands;
//! the supervisor checks that the caller is outside every pane Job.
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use shared_types::{OperatorIntro, OperatorPreamble, SidebandRequest, SidebandResponse};

pub const OPERATOR_CLI_USAGE: &str = "usage: --prim1-operator [--runtime <dir>] [--actor <label>] [--token-file <path>] <command…>
  commands (as --prim1-room): ping | rooms | members | member … | room … | guests … | settings … | conversations … | fallbacks … | deliver --room <room id> <member|all> <text> | dispose --room <room id> <message id> <member, or guest label or id> (--next|--decline) <text> | cancel --room <room id> <message id> --reason <text>
  fallbacks get --room <room id> | fallbacks set <revision> <settings-json> --room <room id>: the entry has no room of its own
  --runtime: PRIM-1's runtime folder (default PRIM1_RUNTIME_DIR, else %LOCALAPPDATA%\\io.prim1.v2\\runtime)
  --actor: how the room feed names you, beside this process id (display only)
  --token-file: required by guests invite; the token and its MCP config are written there (a new file) and never printed
  Refused from inside a PRIM-1 pane, also from anything a pane started: a pane uses its own member tools and grants.
  prints the supervisor's JSON response; exit 0 ok, 1 refused/transport, 2 usage
  PowerShell: this is a windowed exe; call it piped (& $env:PRIM1_CLI … | Out-String) and read $LASTEXITCODE, or use Start-Process -Wait -PassThru; a bare call neither waits for it nor sets the exit code";

struct OperatorCli {
    runtime_dir: Option<PathBuf>,
    actor: Option<String>,
    token_file: Option<PathBuf>,
    request: SidebandRequest,
}

fn parse_operator_cli(args: &[String]) -> std::result::Result<OperatorCli, String> {
    let (mut runtime_dir, mut actor, mut token_file) = (None, None, None);
    let mut rest = args;
    loop {
        match rest {
            [flag, value, tail @ ..] if flag == "--runtime" && runtime_dir.is_none() => {
                runtime_dir = Some(PathBuf::from(value));
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--actor" && actor.is_none() => {
                actor = Some(value.clone());
                rest = tail;
            }
            [flag, value, tail @ ..] if flag == "--token-file" && token_file.is_none() => {
                token_file = Some(PathBuf::from(value));
                rest = tail;
            }
            _ => break,
        }
    }
    let command = rest.first().map(String::as_str).ok_or_else(|| "missing command".to_string())?;
    if !matches!(command, "ping" | "rooms" | "members" | "member" | "room" | "guests" | "settings" | "conversations" | "fallbacks" | "deliver" | "dispose" | "cancel") {
        return Err(format!("the operator entry has no '{command}' command"));
    }
    let request = super::parse_room_cli(rest)?;
    if matches!(&request, SidebandRequest::RoomDeliver { .. }) {
        return Err("deliver from the operator entry needs --room <room id>: the entry has no room of its own".into());
    }
    if matches!(&request, SidebandRequest::RoomFallbacks { room_id: None, .. }) {
        return Err("fallbacks from the operator entry needs --room <room id>: the entry has no room of its own".into());
    }
    let invites = matches!(&request, SidebandRequest::Room { action: shared_types::RoomAction::GuestInvite { .. }, .. });
    match (invites, &token_file) {
        (true, None) => return Err("guests invite needs --token-file <path>: the token is never printed".into()),
        (false, Some(_)) => return Err("--token-file is only for guests invite".into()),
        _ => {}
    }
    Ok(OperatorCli { runtime_dir, actor, token_file, request })
}

pub fn run_operator_cli(args: &[std::ffi::OsString]) -> Result<i32> {
    let args: Vec<String> = args.iter().map(|argument| argument.to_string_lossy().into_owned()).collect();
    let cli = match parse_operator_cli(&args) {
        Ok(cli) => cli,
        Err(usage) => {
            super::print_error_line(format!("{usage}\n{OPERATOR_CLI_USAGE}"));
            return Ok(2);
        }
    };
    #[cfg(windows)]
    super::attach_parent_console_if_detached();
    let response = match send_operator_request(&cli) {
        Ok(response) => response,
        Err(error) => {
            super::print_cli_failure(format!("PRIM-1 operator entry failed: {error:#}"))?;
            return Ok(1);
        }
    };
    let ok = response.ok;
    let mut printed = serde_json::to_value(&response)?;
    if let Some(path) = &cli.token_file {
        if let Err(error) = keep_invitation_off_stdout(&mut printed, path) {
            // The guest exists; say so, and never print its token.
            strip_invitation(&mut printed);
            super::print_json_line(&printed)?;
            super::print_error_line(format!(
                "the guest was invited, but its token could not be written to {}: {error:#}; revoke it and invite again",
                path.display()));
            return Ok(1);
        }
    }
    super::print_json_line(&printed)?;
    Ok(if ok { 0 } else { 1 })
}

/// Writes the invitation (token, MCP config, CLI example) to a new file and
/// leaves only its path in the printed response.
fn keep_invitation_off_stdout(printed: &mut Value, path: &std::path::Path) -> Result<()> {
    let Some(invitation) = printed.pointer("/payload/result/invitation").cloned().filter(|value| !value.is_null()) else {
        return Ok(());
    };
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(path)
        .with_context(|| format!("{} must not exist yet", path.display()))?;
    std::io::Write::write_all(&mut file, serde_json::to_string_pretty(&invitation)?.as_bytes())?;
    file.sync_all()?;
    if let Some(slot) = printed.pointer_mut("/payload/result/invitation") {
        *slot = serde_json::json!({ "written_to": path.display().to_string() });
    }
    Ok(())
}

fn strip_invitation(printed: &mut Value) {
    if let Some(slot) = printed.pointer_mut("/payload/result/invitation") {
        *slot = Value::Null;
    }
}

fn runtime_dir(cli: &OperatorCli) -> Result<PathBuf> {
    if let Some(dir) = &cli.runtime_dir {
        return Ok(dir.clone());
    }
    super::guest::guest_runtime_dir()
}

#[cfg(windows)]
fn send_operator_request(cli: &OperatorCli) -> Result<SidebandResponse> {
    let runtime_dir = runtime_dir(cli)?;
    let path = runtime_dir.join(shared_types::GUEST_ENDPOINT_FILE_NAME);
    let bytes = std::fs::read(&path).map_err(|error| anyhow!(
        "PRIM-1 is not running for {} ({}: {error}); start PRIM-1 and retry",
        runtime_dir.display(), shared_types::GUEST_ENDPOINT_FILE_NAME))?;
    let endpoint: shared_types::GuestEndpoint = serde_json::from_slice(&bytes)
        .with_context(|| format!("{} is not a PRIM-1 endpoint file", path.display()))?;
    if !endpoint.endpoint.starts_with(r"\\.\pipe\") || endpoint.server_pid == 0 || endpoint.server_started == 0 {
        bail!("{} names no PRIM-1 named pipe", path.display());
    }
    if let Some(refusal) = own_pane_refusal(std::env::var("PRIM1_CONTROL_PLANE_SERVER_PID").ok().as_deref(), &endpoint) {
        bail!(refusal);
    }
    let mut request_line = control_plane::encode_request(&cli.request)?;
    request_line.push('\n');
    let mut frames = control_plane::encode_operator_preamble(&OperatorPreamble {
        operator: OperatorIntro { actor: cli.actor.clone() },
    })?;
    frames.push('\n');
    frames.push_str(&request_line);
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()
        .context("failed to create the operator entry's runtime")?;
    let response = runtime.block_on(super::pipe_round_trip(
        &endpoint.endpoint, endpoint.server_pid, endpoint.server_started, &request_line, &frames))?;
    control_plane::decode_response(&response)
}

/// A16: a pane's shell carries the id of the PRIM-1 that started it. Calling
/// that same PRIM-1's operator entry from it is refused here before
/// connecting; the supervisor refuses a pane's descendant in any case. Another
/// PRIM-1 (a QA root on this PC) is not this pane's, so it is let through.
fn own_pane_refusal(pane_server_pid: Option<&str>, endpoint: &shared_types::GuestEndpoint) -> Option<&'static str> {
    (pane_server_pid?.trim().parse::<u32>().ok()? == endpoint.server_pid).then_some(
        "this shell belongs to a pane of this PRIM-1: use your member tools (members_list, member_*, room_*) and your grants; the operator entry is for processes PRIM-1 did not start")
}

#[cfg(not(windows))]
fn send_operator_request(cli: &OperatorCli) -> Result<SidebandResponse> {
    let _ = runtime_dir(cli);
    bail!("the PRIM-1 operator entry is unavailable on this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn a_pane_of_this_prim1_is_refused_before_connecting_and_another_prim1_is_not() {
        let endpoint = shared_types::GuestEndpoint { endpoint: r"\\.\pipe\prim1-600-x".into(), server_pid: 600, server_started: 1 };
        assert!(own_pane_refusal(Some("600"), &endpoint).is_some_and(|refusal| refusal.contains("member tools")));
        assert!(own_pane_refusal(Some("8124"), &endpoint).is_none(), "a pane of another PRIM-1");
        assert!(own_pane_refusal(None, &endpoint).is_none(), "not a pane: an SSH or scheduled-task shell");
        assert!(own_pane_refusal(Some("not a pid"), &endpoint).is_none());
    }

    #[test]
    fn operator_cli_takes_the_room_commands_after_its_own_flags() {
        let cli = parse_operator_cli(&args(&["--runtime", r"D:\root\runtime", "--actor", "MAINPC supervisor",
            "members"])).unwrap();
        assert_eq!(cli.request, SidebandRequest::MembersList {});
        assert_eq!(cli.runtime_dir, Some(PathBuf::from(r"D:\root\runtime")));
        assert_eq!(cli.actor.as_deref(), Some("MAINPC supervisor"));
        let id = "6f0d2b8e-1c3a-4b5d-9e7f-0a1b2c3d4e5f";
        let room = parse_operator_cli(&args(&["room", "create", "--request-id", id, "PC3 Demo", "--resume", "on"])).unwrap();
        assert!(matches!(room.request, SidebandRequest::Room { action: shared_types::RoomAction::Create { .. }, .. }));
        let invite = parse_operator_cli(&args(&["--token-file", r"C:\Users\operator\prim1-guest\t.json", "guests", "invite",
            "--request-id", id])).unwrap();
        assert_eq!(invite.token_file, Some(PathBuf::from(r"C:\Users\operator\prim1-guest\t.json")));
        let room_id = shared_types::RoomId::parse_str(id).unwrap();
        let get = parse_operator_cli(&args(&["fallbacks", "get", "--room", id])).unwrap();
        assert_eq!(get.request, SidebandRequest::RoomFallbacks { action: shared_types::RoomFallbackAction::Get,
            room_id: Some(room_id), revision: None, settings: None });
        let set = parse_operator_cli(&args(&["fallbacks", "set", "3", r#"{"enabled":true,"interval_minutes":30,"message":"continue","recipient_mode":"all","recipient_ids":[],"overrides":[]}"#,
            "--room", id])).unwrap();
        assert!(matches!(set.request, SidebandRequest::RoomFallbacks { action: shared_types::RoomFallbackAction::Set,
            room_id: Some(room), revision: Some(3), settings: Some(_) } if room == room_id));
    }

    #[test]
    fn operator_cli_refuses_pane_only_commands_and_an_invite_that_would_print_a_token() {
        let id = "6f0d2b8e-1c3a-4b5d-9e7f-0a1b2c3d4e5f";
        for bad in [
            vec!["post", "hello"],
            vec!["deliver", "Codex", "hi"],
            vec!["read"],
            vec!["tasks", "{ tasks { total } }"],
            vec!["upgrade", "x", "y", "z", "1"],
            vec!["guests", "invite", "--request-id", id],
            vec!["--token-file", "t.json", "members"],
            vec!["fallbacks", "get"],
            vec!["fallbacks", "set", "1", "{}"],
            vec![],
        ] {
            assert!(parse_operator_cli(&args(&bad)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn an_invitation_is_written_to_a_new_file_and_never_printed() {
        let dir = tempfile_dir();
        let path = dir.join("guest.json");
        let mut printed = serde_json::json!({ "ok": true, "payload": { "kind": "room", "result": {
            "invitation": { "token": "prim1g_secret", "mcp_config_json": "{}", "cli_example": "x" } } } });
        keep_invitation_off_stdout(&mut printed, &path).unwrap();
        assert!(!printed.to_string().contains("prim1g_secret"));
        assert!(std::fs::read_to_string(&path).unwrap().contains("prim1g_secret"));
        // An existing file is never overwritten.
        let mut again = serde_json::json!({ "payload": { "result": { "invitation": { "token": "other" } } } });
        assert!(keep_invitation_off_stdout(&mut again, &path).is_err());
        assert!(std::fs::read_to_string(&path).unwrap().contains("prim1g_secret"));
        let _ = std::fs::remove_dir_all(dir);
    }

    fn tempfile_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prim1-operator-cli-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
