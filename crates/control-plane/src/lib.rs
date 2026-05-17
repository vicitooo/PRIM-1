use anyhow::Context;
use shared_types::{SidebandRequest, SidebandResponse};

#[cfg(windows)]
pub const DEFAULT_ENDPOINT: &str = r"\\.\pipe\prim1";

#[cfg(not(windows))]
pub const DEFAULT_ENDPOINT: &str = "/tmp/prim1.sock";

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
    use shared_types::{ControlKey, RuntimeSnapshot};

    #[test]
    fn round_trips_request_json() {
        let json = encode_request(&SidebandRequest::ListSessions {
            token: "abc".into(),
        })
        .unwrap();
        let decoded = decode_request(&json).unwrap();

        match decoded {
            SidebandRequest::ListSessions { token } => assert_eq!(token, "abc"),
            _ => panic!("unexpected request variant"),
        }
    }

    #[test]
    fn round_trips_send_key_request_json() {
        let json = encode_request(&SidebandRequest::SendKey {
            token: "abc".into(),
            name: "claude".into(),
            key: ControlKey::CtrlC,
        })
        .unwrap();
        let decoded = decode_request(&json).unwrap();

        match decoded {
            SidebandRequest::SendKey { token, name, key } => {
                assert_eq!(token, "abc");
                assert_eq!(name, "claude");
                assert_eq!(key, ControlKey::CtrlC);
            }
            _ => panic!("unexpected request variant"),
        }
    }

    #[test]
    fn round_trips_create_pair_request_json() {
        let json = encode_request(&SidebandRequest::CreatePair {
            token: "abc".into(),
            name: "frontend-qa".into(),
        })
        .unwrap();
        let decoded = decode_request(&json).unwrap();

        match decoded {
            SidebandRequest::CreatePair { token, name } => {
                assert_eq!(token, "abc");
                assert_eq!(name, "frontend-qa");
            }
            _ => panic!("unexpected request variant"),
        }
    }

    #[test]
    fn round_trips_start_session_extra_args_request_json() {
        let json = encode_request(&SidebandRequest::StartSession {
            token: "abc".into(),
            name: "claude".into(),
            extra_args: vec!["--resume".into(), "abc-123".into()],
        })
        .unwrap();
        let decoded = decode_request(&json).unwrap();

        match decoded {
            SidebandRequest::StartSession {
                token,
                name,
                extra_args,
            } => {
                assert_eq!(token, "abc");
                assert_eq!(name, "claude");
                assert_eq!(
                    extra_args,
                    vec!["--resume".to_string(), "abc-123".to_string()]
                );
            }
            _ => panic!("unexpected request variant"),
        }
    }

    #[test]
    fn round_trips_response_json() {
        let json = encode_response(&SidebandResponse {
            ok: true,
            message: "pong".into(),
            snapshot: Some(RuntimeSnapshot {
                sessions: vec![],
                control_plane: None,
                runtime_dir: "runtime".into(),
                audit_log_path: "audit".into(),
                generated_at: "2026-04-15T00:00:00Z".into(),
            }),
            timed_out: false,
            payload: None,
        })
        .unwrap();
        let decoded = decode_response(&json).unwrap();

        assert!(decoded.ok);
        assert_eq!(decoded.message, "pong");
        assert!(decoded.snapshot.is_some());
    }

    #[test]
    fn decode_request_rejects_invalid_json() {
        assert!(decode_request("{not json}").is_err());
    }

    #[test]
    fn decode_request_accepts_utf8_bom_prefixed_json() {
        let json = format!(
            "\u{feff}{}",
            encode_request(&SidebandRequest::Ping {
                token: "abc".into(),
            })
            .unwrap()
        );

        let decoded = decode_request(&json).unwrap();
        match decoded {
            SidebandRequest::Ping { token } => assert_eq!(token, "abc"),
            _ => panic!("unexpected request variant"),
        }
    }
}
