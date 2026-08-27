use std::io::{BufRead, Write};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use shared_types::{RoomFeedCursor, SidebandRequest, SidebandResponse};

const MCP_FRAME_MAX_BYTES: usize = control_plane::MAX_FRAME_BYTES;
const SERVER_NAME: &str = "prim1-pane";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-03-26", "2025-06-18", "2025-11-25"];

trait SidebandClient {
    fn send(&mut self, request: SidebandRequest) -> Result<SidebandResponse>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpPhase {
    New,
    Initializing,
    Ready,
}

struct McpServer<C> {
    sideband: C,
    phase: McpPhase,
}

impl<C: SidebandClient> McpServer<C> {
    fn new(sideband: C) -> Self {
        Self {
            sideband,
            phase: McpPhase::New,
        }
    }

    fn handle(&mut self, message: Value) -> Option<Value> {
        let object = match message.as_object() {
            Some(object) => object,
            None => {
                return Some(json_rpc_error(
                    Value::Null,
                    -32600,
                    "invalid JSON-RPC request",
                ));
            }
        };
        let id = object.get("id").cloned();
        if object.get("jsonrpc") != Some(&Value::String("2.0".into())) {
            return id.map(|id| json_rpc_error(id, -32600, "invalid JSON-RPC version"));
        }
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            return id.map(|id| json_rpc_error(id, -32600, "JSON-RPC method is required"));
        };
        let params = object.get("params").cloned().unwrap_or_else(|| json!({}));

        let Some(id) = id else {
            self.handle_notification(method);
            return None;
        };
        if !valid_request_id(&id) {
            return Some(json_rpc_error(
                Value::Null,
                -32600,
                "JSON-RPC request id must be a string or integer",
            ));
        }

        let result = match method {
            "initialize" => self.initialize(params),
            // MCP lifecycle explicitly permits protocol pings before initialization.
            // The separately exposed PRIM-1 `ping` tool remains readiness-gated.
            "ping" => Ok(json!({})),
            "tools/list" => self.list_tools(params),
            "tools/call" => self.call_tool(params),
            _ => Err(RpcFailure::new(-32601, "method not found")),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json_rpc_error(id, error.code, &error.message),
        })
    }

    fn handle_notification(&mut self, method: &str) {
        if method == "notifications/initialized" && self.phase == McpPhase::Initializing {
            self.phase = McpPhase::Ready;
        }
    }

    fn initialize(&mut self, params: Value) -> RpcResult<Value> {
        if self.phase != McpPhase::New {
            return Err(RpcFailure::new(-32600, "MCP server is already initialized"));
        }
        let protocol_version = params
            .as_object()
            .and_then(|object| object.get("protocolVersion"))
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::new(-32602, "initialize protocolVersion is required"))?;
        let negotiated_version = if SUPPORTED_PROTOCOL_VERSIONS.contains(&protocol_version) {
            protocol_version
        } else {
            SUPPORTED_PROTOCOL_VERSIONS
                .last()
                .copied()
                .expect("PRIM-1 supports at least one MCP protocol version")
        };
        self.phase = McpPhase::Initializing;
        Ok(json!({
            "protocolVersion": negotiated_version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": SERVER_VERSION },
            "instructions": "PRIM-1 pane-local tools. room_post appends only to the current room feed; room_read reads that feed from an optional cursor. Neither tool delivers a prompt to another harness."
        }))
    }

    fn ensure_ready(&self) -> RpcResult<()> {
        if self.phase == McpPhase::Ready {
            Ok(())
        } else {
            Err(RpcFailure::new(-32002, "MCP server is not initialized"))
        }
    }

    fn list_tools(&self, params: Value) -> RpcResult<Value> {
        self.ensure_ready()?;
        if let Some(object) = params.as_object() {
            if object.keys().any(|key| key != "cursor" && key != "_meta")
                || object.get("cursor").is_some_and(|cursor| !cursor.is_null())
                || object.get("_meta").is_some_and(|meta| !meta.is_object())
            {
                return Err(RpcFailure::new(
                    -32602,
                    "tools/list accepts only a null cursor and object _meta",
                ));
            }
        } else {
            return Err(RpcFailure::new(
                -32602,
                "tools/list params must be an object",
            ));
        }
        Ok(json!({ "tools": tool_definitions() }))
    }

    fn call_tool(&mut self, params: Value) -> RpcResult<Value> {
        self.ensure_ready()?;
        let object = params
            .as_object()
            .ok_or_else(|| RpcFailure::new(-32602, "tools/call params must be an object"))?;
        if object
            .keys()
            .any(|key| key != "name" && key != "arguments" && key != "_meta")
        {
            return Err(RpcFailure::new(
                -32602,
                "tools/call contains an unknown field",
            ));
        }
        let name = object
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcFailure::new(-32602, "tools/call name is required"))?;
        let arguments = object
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let request = match name {
            "ping" => {
                decode_tool_arguments::<EmptyArguments>(arguments)?;
                SidebandRequest::Ping {}
            }
            "room_read" => {
                let arguments = decode_tool_arguments::<RoomReadArguments>(arguments)?;
                SidebandRequest::RoomRead {
                    cursor: arguments.cursor,
                }
            }
            "room_post" => {
                let arguments = decode_tool_arguments::<RoomPostArguments>(arguments)?;
                SidebandRequest::RoomPost {
                    content: arguments.content,
                }
            }
            _ => return Err(RpcFailure::new(-32602, "unknown PRIM-1 pane tool")),
        };

        let response = match self.sideband.send(request) {
            Ok(response) => response,
            Err(error) => {
                return Ok(tool_error(format!(
                    "PRIM-1 pane sideband transport failed: {error:#}"
                )));
            }
        };
        let response_value = serde_json::to_value(&response)
            .map_err(|_| RpcFailure::new(-32603, "failed to encode sideband response"))?;
        let text = serde_json::to_string(&response_value)
            .map_err(|_| RpcFailure::new(-32603, "failed to encode sideband response"))?;
        Ok(json!({
            "content": [{ "type": "text", "text": text }],
            "structuredContent": response_value,
            "isError": !response.ok,
        }))
    }
}

#[derive(Debug)]
struct RpcFailure {
    code: i32,
    message: String,
}

impl RpcFailure {
    fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

type RpcResult<T> = std::result::Result<T, RpcFailure>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArguments {}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomReadArguments {
    #[serde(default)]
    cursor: Option<RoomFeedCursor>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoomPostArguments {
    content: String,
}

fn decode_tool_arguments<T: for<'de> Deserialize<'de>>(value: Value) -> RpcResult<T> {
    serde_json::from_value(value)
        .map_err(|error| RpcFailure::new(-32602, format!("invalid tool arguments: {error}")))
}

fn valid_request_id(id: &Value) -> bool {
    match id {
        Value::String(_) => true,
        Value::Number(number) => number.as_i64().is_some() || number.as_u64().is_some(),
        _ => false,
    }
}

fn json_rpc_error(id: Value, code: i32, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn tool_error(message: String) -> Value {
    json!({
        "content": [{ "type": "text", "text": message }],
        "isError": true,
    })
}

fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "ping",
            "title": "Check PRIM-1 pane connection",
            "description": "Verify that this exact live pane can reach its PRIM-1 supervisor.",
            "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "room_read",
            "title": "Read current PRIM-1 room feed",
            "description": "Read this pane's current PRIM-1 room feed from an optional cursor. The supervisor derives room membership and the join floor; no room or sender identifier is accepted.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "cursor": {
                        "type": "object",
                        "properties": {
                            "epoch": { "type": "string", "format": "uuid" },
                            "sequence": { "type": "integer", "minimum": 0 }
                        },
                        "required": ["epoch", "sequence"],
                        "additionalProperties": false
                    }
                },
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false }
        }),
        json!({
            "name": "room_post",
            "title": "Post to current PRIM-1 room feed",
            "description": "Append one message from this exact pane to its current PRIM-1 room feed. This is feed-only and never injects a prompt into another harness.",
            "inputSchema": {
                "type": "object",
                "properties": { "content": { "type": "string", "minLength": 1 } },
                "required": ["content"],
                "additionalProperties": false
            },
            "annotations": { "readOnlyHint": false, "destructiveHint": false, "idempotentHint": false, "openWorldHint": false }
        }),
    ]
}

pub fn run_stdio() -> Result<()> {
    let sideband = EnvironmentSidebandClient::from_environment()?;
    serve(
        std::io::BufReader::new(std::io::stdin().lock()),
        std::io::stdout().lock(),
        sideband,
    )
}

fn serve<R: BufRead, W: Write, C: SidebandClient>(input: R, output: W, sideband: C) -> Result<()> {
    serve_with_frame_limit(input, output, sideband, MCP_FRAME_MAX_BYTES)
}

fn serve_with_frame_limit<R: BufRead, W: Write, C: SidebandClient>(
    mut input: R,
    mut output: W,
    sideband: C,
    request_frame_max_bytes: usize,
) -> Result<()> {
    let mut server = McpServer::new(sideband);
    loop {
        let Some(frame) = read_frame(&mut input, request_frame_max_bytes)? else {
            output.flush().context("failed to flush MCP stdout")?;
            return Ok(());
        };
        let response = match frame {
            FrameRead::Message(frame) => match serde_json::from_str::<Value>(&frame) {
                Ok(message) => server.handle(message),
                Err(_) => Some(json_rpc_error(Value::Null, -32700, "invalid JSON")),
            },
            FrameRead::InvalidUtf8 => Some(json_rpc_error(
                Value::Null,
                -32700,
                "MCP request is not valid UTF-8",
            )),
            FrameRead::Oversized => Some(json_rpc_error(
                Value::Null,
                -32700,
                "MCP request exceeds the bounded frame limit",
            )),
            FrameRead::Unterminated => Some(json_rpc_error(
                Value::Null,
                -32700,
                "unterminated MCP request",
            )),
        };
        if let Some(response) = response {
            write_response_frame(&mut output, response)?;
            output.flush().context("failed to flush MCP response")?;
        }
    }
}

fn write_response_frame<W: Write>(output: &mut W, response: Value) -> Result<()> {
    let response_id = response.get("id").cloned().unwrap_or(Value::Null);
    let mut encoded = serde_json::to_vec(&response).context("failed to encode MCP response")?;
    if encoded.len().saturating_add(1) > MCP_FRAME_MAX_BYTES {
        encoded = serde_json::to_vec(&json_rpc_error(
            response_id,
            -32603,
            "MCP response exceeds the bounded frame limit",
        ))
        .context("failed to encode bounded MCP error response")?;
    }
    encoded.push(b'\n');
    output
        .write_all(&encoded)
        .context("failed to write MCP response")
}

#[derive(Debug, PartialEq, Eq)]
enum FrameRead {
    Message(String),
    InvalidUtf8,
    Oversized,
    Unterminated,
}

fn read_frame<R: BufRead>(reader: &mut R, max_bytes: usize) -> Result<Option<FrameRead>> {
    let mut bytes = Vec::new();
    let mut oversized = false;
    loop {
        let available = reader.fill_buf().context("failed to read MCP stdin")?;
        if available.is_empty() {
            if bytes.is_empty() && !oversized {
                return Ok(None);
            }
            return Ok(Some(FrameRead::Unterminated));
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index + 1);
        if !oversized {
            if bytes.len().saturating_add(take) > max_bytes {
                oversized = true;
            } else {
                bytes.extend_from_slice(&available[..take]);
            }
        }
        reader.consume(take);
        if newline.is_some() {
            if oversized {
                return Ok(Some(FrameRead::Oversized));
            }
            bytes.pop();
            if bytes.last() == Some(&b'\r') {
                bytes.pop();
            }
            return Ok(Some(match String::from_utf8(bytes) {
                Ok(frame) => FrameRead::Message(frame),
                Err(_) => FrameRead::InvalidUtf8,
            }));
        }
    }
}

#[cfg(windows)]
struct EnvironmentSidebandClient {
    runtime: tokio::runtime::Runtime,
    endpoint: String,
    expected_server_pid: u32,
    expected_server_creation_time: u64,
}

#[cfg(windows)]
impl EnvironmentSidebandClient {
    fn from_environment() -> Result<Self> {
        let transport = required_environment("PRIM1_CONTROL_PLANE_TRANSPORT")?;
        if transport != "named_pipe" {
            bail!("unsupported PRIM-1 control-plane transport '{transport}'");
        }
        let endpoint = required_environment("PRIM1_CONTROL_PLANE_ENDPOINT")?;
        if !endpoint.starts_with(r"\\.\pipe\") {
            bail!("PRIM-1 control-plane endpoint is not a Windows named pipe");
        }
        let expected_server_pid = required_environment("PRIM1_CONTROL_PLANE_SERVER_PID")?
            .parse::<u32>()
            .context("PRIM1_CONTROL_PLANE_SERVER_PID must be a positive decimal process ID")?;
        if expected_server_pid == 0 {
            bail!("PRIM1_CONTROL_PLANE_SERVER_PID must be a positive decimal process ID");
        }
        let expected_server_creation_time = required_environment(
            "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME",
        )?
        .parse::<u64>()
        .context(
            "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME must be a positive decimal FILETIME",
        )?;
        if expected_server_creation_time == 0 {
            bail!(
                "PRIM1_CONTROL_PLANE_SERVER_STARTED_FILETIME must be a positive decimal FILETIME"
            );
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("failed to create PRIM-1 pane MCP runtime")?;
        Ok(Self {
            runtime,
            endpoint,
            expected_server_pid,
            expected_server_creation_time,
        })
    }

    async fn send_async(
        endpoint: String,
        expected_server_pid: u32,
        expected_server_creation_time: u64,
        request: SidebandRequest,
    ) -> Result<SidebandResponse> {
        use std::{os::windows::io::AsRawHandle, time::Duration};

        use pty_host::PinnedProcess;
        use tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            net::windows::named_pipe::ClientOptions,
            time::{Instant, sleep, timeout},
        };
        use windows_sys::Win32::{Foundation::HANDLE, System::Pipes::GetNamedPipeServerProcessId};

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut client = loop {
            match ClientOptions::new().open(&endpoint) {
                Ok(client) => break client,
                Err(error)
                    if matches!(error.raw_os_error(), Some(2) | Some(231))
                        && Instant::now() < deadline =>
                {
                    sleep(Duration::from_millis(25)).await;
                }
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!("failed to connect to PRIM-1 named pipe {endpoint}")
                    });
                }
            }
        };

        let mut actual_server_pid = 0_u32;
        let ok = unsafe {
            GetNamedPipeServerProcessId(client.as_raw_handle() as HANDLE, &mut actual_server_pid)
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error())
                .context("failed to identify PRIM-1 named-pipe server process");
        }
        if actual_server_pid != expected_server_pid {
            bail!("connected named-pipe server process does not match the injected PRIM-1 server");
        }
        let pinned_server = PinnedProcess::open(actual_server_pid)
            .context("failed to pin PRIM-1 named-pipe server process")?;
        if pinned_server.creation_time_filetime() != expected_server_creation_time {
            bail!(
                "connected named-pipe server creation time does not match the injected PRIM-1 server"
            );
        }

        let mut encoded = control_plane::encode_request(&request)?;
        encoded.push('\n');
        if encoded.len() > control_plane::MAX_FRAME_BYTES {
            bail!(
                "encoded sideband request exceeds {}-byte frame limit",
                control_plane::MAX_FRAME_BYTES
            );
        }
        timeout(Duration::from_secs(5), async {
            client.write_all(encoded.as_bytes()).await?;
            client.flush().await
        })
        .await
        .map_err(|_| anyhow!("timed out writing PRIM-1 sideband request"))?
        .context("failed to write PRIM-1 sideband request")?;

        let mut reader = BufReader::new(client);
        let mut response = Vec::new();
        timeout(Duration::from_secs(45), async {
            loop {
                let available = reader.fill_buf().await?;
                if available.is_empty() {
                    bail!("unterminated PRIM-1 sideband response");
                }
                let newline = available.iter().position(|byte| *byte == b'\n');
                let take = newline.map_or(available.len(), |index| index + 1);
                if response.len().saturating_add(take) > control_plane::MAX_FRAME_BYTES {
                    bail!(
                        "PRIM-1 sideband response exceeds {}-byte frame limit",
                        control_plane::MAX_FRAME_BYTES
                    );
                }
                response.extend_from_slice(&available[..take]);
                reader.consume(take);
                if newline.is_some() {
                    response.pop();
                    if response.last() == Some(&b'\r') {
                        response.pop();
                    }
                    break Ok::<(), anyhow::Error>(());
                }
            }
        })
        .await
        .map_err(|_| anyhow!("timed out reading PRIM-1 sideband response"))??;
        if !pinned_server.is_alive()? {
            bail!("PRIM-1 named-pipe server exited before its response was verified");
        }
        let response = String::from_utf8(response)
            .map_err(|_| anyhow!("PRIM-1 sideband response is not valid UTF-8"))?;
        control_plane::decode_response(&response)
    }
}

#[cfg(windows)]
impl SidebandClient for EnvironmentSidebandClient {
    fn send(&mut self, request: SidebandRequest) -> Result<SidebandResponse> {
        self.runtime.block_on(Self::send_async(
            self.endpoint.clone(),
            self.expected_server_pid,
            self.expected_server_creation_time,
            request,
        ))
    }
}

#[cfg(not(windows))]
struct EnvironmentSidebandClient;

#[cfg(not(windows))]
impl EnvironmentSidebandClient {
    fn from_environment() -> Result<Self> {
        bail!("PRIM-1 pane MCP is unavailable on this platform")
    }
}

#[cfg(not(windows))]
impl SidebandClient for EnvironmentSidebandClient {
    fn send(&mut self, _request: SidebandRequest) -> Result<SidebandResponse> {
        bail!("PRIM-1 pane MCP is unavailable on this platform")
    }
}

#[cfg(windows)]
fn required_environment(name: &str) -> Result<String> {
    let value = std::env::var(name).with_context(|| format!("{name} is required"))?;
    if value.trim().is_empty() {
        bail!("{name} must not be empty");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, io::Cursor};

    use shared_types::{RoomFeedPage, SidebandResponsePayload};

    use super::*;

    struct RecordingClient {
        requests: Vec<SidebandRequest>,
        responses: VecDeque<Result<SidebandResponse>>,
    }

    impl RecordingClient {
        fn with_responses(responses: impl IntoIterator<Item = Result<SidebandResponse>>) -> Self {
            Self {
                requests: Vec::new(),
                responses: responses.into_iter().collect(),
            }
        }
    }

    impl SidebandClient for RecordingClient {
        fn send(&mut self, request: SidebandRequest) -> Result<SidebandResponse> {
            self.requests.push(request);
            self.responses
                .pop_front()
                .unwrap_or_else(|| Err(anyhow!("missing fake response")))
        }
    }

    fn ok_response(payload: Option<SidebandResponsePayload>) -> SidebandResponse {
        SidebandResponse {
            ok: true,
            message: "ok".into(),
            timed_out: false,
            payload,
            request_id: Some("req-1".into()),
        }
    }

    fn initialize(server: &mut McpServer<RecordingClient>) {
        let response = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "test", "version": "1" }
                }
            }))
            .unwrap();
        assert_eq!(response["result"]["protocolVersion"], "2025-06-18");
        assert!(
            server
                .handle(json!({
                    "jsonrpc": "2.0",
                    "method": "notifications/initialized"
                }))
                .is_none()
        );
    }

    #[test]
    fn initializes_and_lists_tools_in_deterministic_order() {
        let mut server = McpServer::new(RecordingClient::with_responses([]));
        initialize(&mut server);
        let response = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": "tools",
                "method": "tools/list",
                "params": { "_meta": { "traceparent": "opaque" } }
            }))
            .unwrap();
        let names = response["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(names, ["ping", "room_read", "room_post"]);
    }

    #[test]
    fn negotiates_unknown_protocol_to_latest_supported_version() {
        let mut server = McpServer::new(RecordingClient::with_responses([]));
        let response = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": {
                    "protocolVersion": "2099-01-01",
                    "capabilities": {},
                    "clientInfo": { "name": "future-test", "version": "1" }
                }
            }))
            .unwrap();
        assert_eq!(
            response["result"]["protocolVersion"],
            *SUPPORTED_PROTOCOL_VERSIONS.last().unwrap()
        );
    }

    #[test]
    fn protocol_ping_is_available_before_ready_but_tools_are_not() {
        let mut server = McpServer::new(RecordingClient::with_responses([]));
        let ping = server
            .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .unwrap();
        assert_eq!(ping["result"], json!({}));
        let tools = server
            .handle(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }))
            .unwrap();
        assert_eq!(tools["error"]["code"], -32002);
        assert!(server.sideband.requests.is_empty());
    }

    #[test]
    fn maps_room_read_and_post_without_room_or_sender_authority() {
        let cursor = RoomFeedCursor {
            epoch: uuid::Uuid::new_v4(),
            sequence: 7,
        };
        let mut server = McpServer::new(RecordingClient::with_responses([
            Ok(ok_response(Some(SidebandResponsePayload::RoomFeed {
                page: RoomFeedPage {
                    schema_version: shared_types::ROOM_EVENT_SCHEMA_VERSION,
                    room_id: shared_types::RoomId::new_v4(),
                    cursor,
                    events: Vec::new(),
                    gap: None,
                    has_more: false,
                },
            }))),
            Ok(ok_response(None)),
        ]));
        initialize(&mut server);

        let read = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "tools/call",
                "params": { "name": "room_read", "arguments": { "cursor": cursor } }
            }))
            .unwrap();
        assert_eq!(read["result"]["isError"], false);
        let post = server
            .handle(json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": { "name": "room_post", "arguments": { "content": "λ exact" } }
            }))
            .unwrap();
        assert_eq!(post["result"]["isError"], false);
        assert_eq!(
            server.sideband.requests,
            [
                SidebandRequest::RoomRead {
                    cursor: Some(cursor)
                },
                SidebandRequest::RoomPost {
                    content: "λ exact".into()
                }
            ]
        );
    }

    #[test]
    fn rejects_unknown_authority_fields_before_sideband_io() {
        let mut server = McpServer::new(RecordingClient::with_responses([]));
        initialize(&mut server);
        for arguments in [
            json!({ "content": "hello", "room_id": shared_types::RoomId::new_v4() }),
            json!({ "content": "hello", "sender": "operator" }),
        ] {
            let response = server
                .handle(json!({
                    "jsonrpc": "2.0",
                    "id": 4,
                    "method": "tools/call",
                    "params": { "name": "room_post", "arguments": arguments }
                }))
                .unwrap();
            assert_eq!(response["error"]["code"], -32602);
        }
        assert!(server.sideband.requests.is_empty());
    }

    #[test]
    fn reports_sideband_rejection_and_transport_failure_as_tool_errors() {
        let mut server = McpServer::new(RecordingClient::with_responses([
            Ok(SidebandResponse {
                ok: false,
                message: "sideband access denied".into(),
                timed_out: false,
                payload: None,
                request_id: None,
            }),
            Err(anyhow!("pipe closed")),
        ]));
        initialize(&mut server);
        for id in [5, 6] {
            let response = server
                .handle(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "method": "tools/call",
                    "params": { "name": "ping", "arguments": {} }
                }))
                .unwrap();
            assert_eq!(response["result"]["isError"], true);
        }
    }

    #[test]
    fn stdio_is_newline_framed_and_notifications_emit_nothing() {
        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\"}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        );
        let mut output = Vec::new();
        serve(
            Cursor::new(input.as_bytes()),
            &mut output,
            RecordingClient::with_responses([]),
        )
        .unwrap();
        let lines = String::from_utf8(output).unwrap();
        assert_eq!(lines.lines().count(), 2);
        assert!(
            lines
                .lines()
                .all(|line| serde_json::from_str::<Value>(line).is_ok())
        );
    }

    #[test]
    fn frame_reader_bounds_and_drains_bad_frames() {
        let mut exact = vec![b'x'; 16];
        exact.push(b'\n');
        assert_eq!(
            read_frame(&mut Cursor::new(exact), 17).unwrap().unwrap(),
            FrameRead::Message("x".repeat(16))
        );

        let mut oversized = vec![b'x'; 17];
        oversized.extend_from_slice(b"\nnext\n");
        let mut oversized = Cursor::new(oversized);
        assert_eq!(
            read_frame(&mut oversized, 17).unwrap().unwrap(),
            FrameRead::Oversized
        );
        assert_eq!(
            read_frame(&mut oversized, 17).unwrap().unwrap(),
            FrameRead::Message("next".into())
        );
        assert_eq!(
            read_frame(&mut Cursor::new(b"partial"), 17)
                .unwrap()
                .unwrap(),
            FrameRead::Unterminated
        );
        assert_eq!(
            read_frame(&mut Cursor::new([0xff, b'\n']), 17)
                .unwrap()
                .unwrap(),
            FrameRead::InvalidUtf8
        );
    }

    #[test]
    fn stdio_recovers_after_invalid_utf8_and_oversize_frames() {
        let mut input = vec![0xff, b'\n'];
        input.extend(std::iter::repeat_n(b'x', 129));
        input.extend_from_slice(
            b"\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2099-01-01\"}}\n",
        );
        input.extend_from_slice(
            b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        );
        let mut output = Vec::new();
        serve_with_frame_limit(
            Cursor::new(input),
            &mut output,
            RecordingClient::with_responses([]),
            128,
        )
        .unwrap();
        let responses = String::from_utf8(output)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(responses.len(), 4);
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[1]["error"]["code"], -32700);
        assert_eq!(
            responses[2]["result"]["protocolVersion"],
            *SUPPORTED_PROTOCOL_VERSIONS.last().unwrap()
        );
        assert_eq!(responses[3]["result"], json!({}));
    }

    #[test]
    fn response_writer_replaces_oversize_payload_with_bounded_error() {
        let mut output = Vec::new();
        write_response_frame(
            &mut output,
            json!({
                "jsonrpc": "2.0",
                "id": "large",
                "result": { "content": "x".repeat(MCP_FRAME_MAX_BYTES) }
            }),
        )
        .unwrap();
        assert!(output.len() < 1024);
        let response: Value = serde_json::from_slice(&output[..output.len() - 1]).unwrap();
        assert_eq!(response["id"], "large");
        assert_eq!(response["error"]["code"], -32603);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("bounded frame limit")
        );
    }

    #[cfg(windows)]
    fn windows_test_pipe_endpoint(label: &str) -> String {
        format!(
            r"\\.\pipe\prim1-pane-mcp-{label}-{}-{}",
            std::process::id(),
            uuid::Uuid::new_v4()
        )
    }

    #[cfg(windows)]
    fn spawn_test_pipe_server(
        endpoint: &str,
        response: SidebandResponse,
    ) -> std::thread::JoinHandle<Result<usize>> {
        use std::time::Duration;

        use tokio::{
            io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
            net::windows::named_pipe::ServerOptions,
            time::timeout,
        };

        let endpoint = endpoint.to_owned();
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("build pane MCP test server runtime")?;
            runtime.block_on(async move {
                let server = match ServerOptions::new()
                    .first_pipe_instance(true)
                    .reject_remote_clients(true)
                    .create(&endpoint)
                {
                    Ok(server) => {
                        ready_tx.send(Ok(())).ok();
                        server
                    }
                    Err(error) => {
                        ready_tx.send(Err(error.to_string())).ok();
                        return Err(error.into());
                    }
                };
                timeout(Duration::from_secs(5), server.connect())
                    .await
                    .map_err(|_| anyhow!("pane MCP test client did not connect"))??;
                let mut reader = BufReader::new(server);
                let mut request = Vec::new();
                match timeout(
                    Duration::from_secs(1),
                    reader.read_until(b'\n', &mut request),
                )
                .await
                {
                    Ok(Ok(0)) | Err(_) => return Ok(0),
                    Ok(Ok(_)) => {}
                    Ok(Err(error)) => return Err(error.into()),
                }
                let mut server = reader.into_inner();
                let mut encoded = control_plane::encode_response(&response)?;
                encoded.push('\n');
                server.write_all(encoded.as_bytes()).await?;
                server.flush().await?;
                Ok(request.len())
            })
        });
        ready_rx
            .recv()
            .expect("pane MCP test server exited before readiness")
            .expect("create pane MCP test pipe");
        worker
    }

    #[cfg(windows)]
    #[test]
    fn windows_client_verifies_server_identity_before_writing() {
        use pty_host::PinnedProcess;

        let server_process = PinnedProcess::open(std::process::id()).unwrap();
        let response = SidebandResponse {
            ok: true,
            message: "pong".into(),
            timed_out: false,
            payload: None,
            request_id: Some("req-live".into()),
        };

        let endpoint = windows_test_pipe_endpoint("positive");
        let server = spawn_test_pipe_server(&endpoint, response.clone());
        let mut client = EnvironmentSidebandClient {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            endpoint,
            expected_server_pid: server_process.pid(),
            expected_server_creation_time: server_process.creation_time_filetime(),
        };
        assert_eq!(client.send(SidebandRequest::Ping {}).unwrap(), response);
        assert!(server.join().unwrap().unwrap() > 0);

        let endpoint = windows_test_pipe_endpoint("wrong-creation");
        let server = spawn_test_pipe_server(&endpoint, response);
        let mut client = EnvironmentSidebandClient {
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            endpoint,
            expected_server_pid: server_process.pid(),
            expected_server_creation_time: server_process
                .creation_time_filetime()
                .checked_add(1)
                .unwrap(),
        };
        let error = client
            .send(SidebandRequest::Ping {})
            .expect_err("wrong server creation time must fail closed");
        assert!(error.to_string().contains("creation time"), "{error:#}");
        assert_eq!(server.join().unwrap().unwrap(), 0);
    }
}
