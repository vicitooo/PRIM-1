use anyhow::Context;
use shared_types::{
    GuestPreamble, GuestRequest, GuestResponse, SidebandPreamble, SidebandRequest,
    SidebandResponse,
};

#[cfg(windows)]
pub const DEFAULT_ENDPOINT: &str = r"\\.\pipe\prim1";

#[cfg(not(windows))]
pub const DEFAULT_ENDPOINT: &str = "/tmp/prim1.sock";

pub const MAX_FRAME_BYTES: usize = 8 * 1024 * 1024;

pub fn decode_request(raw: &str) -> anyhow::Result<SidebandRequest> {
    serde_json::from_str(strip_utf8_bom(raw)).context("failed to decode sideband request")
}

pub fn encode_request(request: &SidebandRequest) -> anyhow::Result<String> {
    serde_json::to_string(request).context("failed to encode sideband request")
}

pub fn decode_response(raw: &str) -> anyhow::Result<SidebandResponse> {
    serde_json::from_str(strip_utf8_bom(raw)).context("failed to decode sideband response")
}

pub fn encode_response(response: &SidebandResponse) -> anyhow::Result<String> {
    serde_json::to_string(response).context("failed to encode sideband response")
}

/// The optional first frame of a connection. A request frame is never a
/// preamble (it has `kind` and no `secret`), and a preamble is never a request,
/// so the server can tell them apart without any mode negotiation.
pub fn encode_preamble(preamble: &SidebandPreamble) -> anyhow::Result<String> {
    serde_json::to_string(preamble).context("failed to encode sideband preamble")
}

pub fn decode_preamble(raw: &str) -> Option<SidebandPreamble> {
    serde_json::from_str(strip_utf8_bom(raw)).ok()
}

/// An outside agent's first frame. It has `guest_token` and nothing else, so
/// it is never a pane preamble or any request; the server checks it first.
pub fn encode_guest_preamble(preamble: &GuestPreamble) -> anyhow::Result<String> {
    serde_json::to_string(preamble).context("failed to encode guest preamble")
}

pub fn decode_guest_preamble(raw: &str) -> Option<GuestPreamble> {
    serde_json::from_str(strip_utf8_bom(raw)).ok()
}

/// A16: the operator entry's first frame (`{"operator":{...}}`): a local
/// process of the same Windows user outside every pane. It is never a pane
/// preamble, a guest preamble or a request; the server checks it before them.
pub fn encode_operator_preamble(preamble: &shared_types::OperatorPreamble) -> anyhow::Result<String> {
    serde_json::to_string(preamble).context("failed to encode operator preamble")
}

pub fn decode_operator_preamble(raw: &str) -> Option<shared_types::OperatorPreamble> {
    serde_json::from_str(strip_utf8_bom(raw)).ok()
}

pub fn encode_guest_request(request: &GuestRequest) -> anyhow::Result<String> {
    serde_json::to_string(request).context("failed to encode guest request")
}

pub fn decode_guest_request(raw: &str) -> anyhow::Result<GuestRequest> {
    serde_json::from_str(strip_utf8_bom(raw)).context("failed to decode guest request")
}

pub fn encode_guest_response(response: &GuestResponse) -> anyhow::Result<String> {
    serde_json::to_string(response).context("failed to encode guest response")
}

pub fn decode_guest_response(raw: &str) -> anyhow::Result<GuestResponse> {
    serde_json::from_str(strip_utf8_bom(raw)).context("failed to decode guest response")
}

fn strip_utf8_bom(raw: &str) -> &str {
    raw.trim_start_matches('\u{feff}')
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_types::{ControlKey, SidebandResponsePayload};

    #[test]
    fn round_trips_admitted_request_json() {
        let requests = [
            SidebandRequest::Ping {},
            SidebandRequest::WaitQuiet {
                name: "claude".into(),
                quiet_seconds: 3,
                timeout_seconds: 30,
            },
            SidebandRequest::SendInput {
                name: "codex".into(),
                input: "status".into(),
            },
            SidebandRequest::SendKey {
                name: "claude".into(),
                key: ControlKey::CtrlC,
            },
        ];

        for request in requests {
            let json = encode_request(&request).unwrap();
            assert!(!json.contains("token"));
            assert_eq!(decode_request(&json).unwrap(), request);
        }
    }

    #[test]
    fn round_trips_response_json() {
        let response = SidebandResponse {
            ok: true,
            message: "quiet".into(),
            timed_out: false,
            payload: Some(SidebandResponsePayload::WaitQuiet {
                quiet_duration_ms: 3_000,
            }),
            request_id: Some("req-1".into()),
        };
        let json = encode_response(&response).unwrap();

        assert!(!json.contains("snapshot"));
        assert_eq!(decode_response(&json).unwrap(), response);
    }

    #[test]
    fn decode_request_rejects_legacy_authority_and_removed_variants() {
        assert!(decode_request(r#"{"kind":"ping","token":"abc"}"#).is_err());
        assert!(decode_request(r#"{"kind":"list_sessions","token":"abc"}"#).is_err());
        assert!(
            decode_request(
                r#"{"kind":"send_input","name":"codex","input":"status","require_idle":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn decode_response_rejects_legacy_snapshot_projection() {
        assert!(decode_response(r#"{"ok":true,"message":"pong","snapshot":null}"#).is_err());
    }

    #[test]
    fn preamble_and_request_frames_are_disjoint() {
        let preamble = SidebandPreamble {
            secret: "0123456789abcdef".into(),
        };
        let encoded = encode_preamble(&preamble).unwrap();
        assert_eq!(decode_preamble(&encoded), Some(preamble));
        assert!(
            decode_request(&encoded).is_err(),
            "a preamble is not a request"
        );
        let request = encode_request(&SidebandRequest::RoomPost {
            request_id: None,
            content: "hello".into(),
        })
        .unwrap();
        assert!(
            decode_preamble(&request).is_none(),
            "a request is not a preamble"
        );
        assert!(decode_preamble(r#"{"secret":"s","kind":"ping"}"#).is_none());
        assert!(decode_preamble("{not json}").is_none());
        // The secret never rides inside a request: the request codec still
        // rejects any caller-supplied authority field.
        assert!(decode_request(r#"{"kind":"room_post","content":"x","secret":"s"}"#).is_err());
    }

    #[test]
    fn guest_frames_and_pane_frames_never_parse_as_each_other() {
        let token = "prim1g_0123456789abcdef".to_string();
        let guest_preamble = encode_guest_preamble(&GuestPreamble { capabilities: Vec::new(), guest_token: token.clone() }).unwrap();
        assert_eq!(decode_guest_preamble(&guest_preamble), Some(GuestPreamble { capabilities: Vec::new(), guest_token: token }));
        assert!(decode_preamble(&guest_preamble).is_none(), "a guest token is not a pane secret");
        assert!(decode_request(&guest_preamble).is_err());
        assert!(decode_guest_request(&guest_preamble).is_err());

        let cursor = shared_types::RoomFeedCursor { epoch: uuid_for_tests(), sequence: 3 };
        let guest_requests = [
            GuestRequest::Hello { label: "ChatGPT".into() },
            GuestRequest::Rooms {},
            GuestRequest::Read { cursor: Some(cursor), owed_after: None },
            GuestRequest::Post { content: "hello".into(), request_id: Some("r-1".into()) },
            GuestRequest::Wait { cursor, timeout_ms: 25_000 },
        ];
        for request in guest_requests {
            let json = encode_guest_request(&request).unwrap();
            assert_eq!(decode_guest_request(&json).unwrap(), request);
            assert!(decode_request(&json).is_err(), "{json} must not be a pane request");
            assert!(decode_preamble(&json).is_none());
            assert!(decode_guest_preamble(&json).is_none());
        }

        let pane_preamble = encode_preamble(&SidebandPreamble { secret: "s".into() }).unwrap();
        assert!(decode_guest_preamble(&pane_preamble).is_none());
        assert!(decode_guest_request(&pane_preamble).is_err());
        let pane_requests = [
            SidebandRequest::Ping {},
            SidebandRequest::RoomRead { expect_room: None, owed_after: None, cursor: Some(cursor) },
            SidebandRequest::RoomPost { request_id: None, content: "x".into() },
            SidebandRequest::RoomDeliver { request_id: None, recipient: "all".into(), content: "x".into() },
            SidebandRequest::SendInput { name: "codex".into(), input: "x".into() },
        ];
        for request in pane_requests {
            let json = encode_request(&request).unwrap();
            assert!(decode_guest_request(&json).is_err(), "{json} must not be a guest request");
            assert!(decode_guest_preamble(&json).is_none());
        }
        // Both keys at once is neither frame.
        assert!(decode_guest_preamble(r#"{"guest_token":"t","secret":"s"}"#).is_none());
        assert!(decode_preamble(r#"{"guest_token":"t","secret":"s"}"#).is_none());
        // A guest request carries no authority field of its own.
        assert!(decode_guest_request(r#"{"guest":"post","content":"x","guest_token":"t"}"#).is_err());
    }

    fn uuid_for_tests() -> shared_types::RoomId {
        "44444444-4444-4444-4444-444444444444".parse().unwrap()
    }

    #[test]
    fn decode_request_rejects_invalid_json() {
        assert!(decode_request("{not json}").is_err());
    }

    #[test]
    fn decode_request_accepts_utf8_bom_prefixed_json() {
        let json = format!(
            "\u{feff}{}",
            encode_request(&SidebandRequest::Ping {}).unwrap()
        );

        let decoded = decode_request(&json).unwrap();
        assert_eq!(decoded, SidebandRequest::Ping {});
    }
}
