use anyhow::Context;
use shared_types::{SidebandRequest, SidebandResponse};

#[cfg(windows)]
pub const DEFAULT_ENDPOINT: &str = r"\\.\pipe\cli-master-wrapper";

#[cfg(not(windows))]
pub const DEFAULT_ENDPOINT: &str = "/tmp/cli-master-wrapper.sock";

pub fn decode_request(raw: &str) -> anyhow::Result<SidebandRequest> {
    serde_json::from_str(raw).context("failed to decode sideband request")
}

pub fn encode_request(request: &SidebandRequest) -> anyhow::Result<String> {
    serde_json::to_string(request).context("failed to encode sideband request")
}

pub fn decode_response(raw: &str) -> anyhow::Result<SidebandResponse> {
    serde_json::from_str(raw).context("failed to decode sideband response")
}

pub fn encode_response(response: &SidebandResponse) -> anyhow::Result<String> {
    serde_json::to_string(response).context("failed to encode sideband response")
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
