use anyhow::Context;
use shared_types::{SidebandRequest, SidebandResponse};

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
