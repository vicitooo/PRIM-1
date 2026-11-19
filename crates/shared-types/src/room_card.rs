//! Fixed room-card contract. Cards contain data, never executable markup or paths.
use serde::{Deserialize, Serialize};

pub const ROOM_CARD_MAX_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomCardKind { Decision, Progress, Result, Metric, Alert, Report, Image, Question }

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RoomCardState { #[default] Open, Done }

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomCardData {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<serde_json::Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub series: Option<Vec<serde_json::Number>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RoomCardContent {
    pub key: String,
    pub kind: RoomCardKind,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub state: RoomCardState,
    #[serde(default)]
    pub data: RoomCardData,
}

impl RoomCardContent {
    pub fn pinned(&self) -> bool {
        self.kind == RoomCardKind::Decision && self.state == RoomCardState::Open
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.key.is_empty() || self.key.len() > 48
            || !self.key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._:-".contains(&b)) {
            return Err("card key must be 1–48 lowercase ASCII letters, digits, '.', '_', ':' or '-'".into());
        }
        text_bound(&self.title, 60, false, "title")?;
        text_bound(&self.summary, 120, false, "summary")?;
        text_bound(&self.body, 4000, true, "body")?;
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > ROOM_CARD_MAX_BYTES {
            return Err("serialized card exceeds 128 KiB".into());
        }
        let data = &self.data;
        if let Some(unit) = &data.unit { text_bound(unit, 16, true, "unit")?; }
        let progress = data.done.is_some() || data.total.is_some();
        let metric = data.value.is_some() || data.series.is_some();
        let image = data.png_base64.is_some() || data.alt.is_some();
        if progress && self.kind != RoomCardKind::Progress
            || metric && self.kind != RoomCardKind::Metric
            || image && self.kind != RoomCardKind::Image
            || data.unit.is_some() && !matches!(self.kind, RoomCardKind::Metric | RoomCardKind::Progress) {
            return Err("card data does not match its kind".into());
        }
        match self.kind {
            RoomCardKind::Progress => match (data.done, data.total) {
                (Some(done), Some(total)) => {
                    if total == 0 || done > total || total > 9_007_199_254_740_991 {
                        return Err("progress requires 0 <= done <= total, total > 0 and safe JSON integers".into());
                    }
                }
                _ => return Err("progress requires done and total".into()),
            },
            RoomCardKind::Metric => {
                if data.value.is_none() { return Err("metric requires a numeric value".into()); }
                if data.series.as_ref().is_some_and(|series| series.is_empty() || series.len() > 60) {
                    return Err("metric series must contain 1–60 numbers".into());
                }
                if data.value.iter().chain(data.series.iter().flatten()).any(|n| n.as_f64().is_none_or(|n| !n.is_finite() || n.abs() > 1e100)) {
                    return Err("metric numbers must be finite and within ±1e100".into());
                }
            }
            RoomCardKind::Image => {
                let encoded = data.png_base64.as_deref().ok_or("image requires png_base64")?;
                let alt = data.alt.as_deref().ok_or("image requires alt text")?;
                text_bound(alt, 160, false, "alt")?;
                validate_png(encoded)?;
            }
            _ => {}
        }
        Ok(())
    }
}

fn text_bound(value: &str, max: usize, empty: bool, name: &str) -> Result<(), String> {
    if (!empty && value.trim().is_empty()) || value.chars().count() > max
        || value.chars().any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r') {
        return Err(format!("card {name} must contain at most {max} characters and no control codes"));
    }
    Ok(())
}

/// Strict standard base64, bounded before allocation. No URL/path or alternate
/// media type is accepted. The browser only sees a locally constructed PNG URI.
fn validate_png(encoded: &str) -> Result<(), String> {
    if encoded.is_empty() || encoded.len() > ROOM_CARD_MAX_BYTES || encoded.len() % 4 != 0 {
        return Err("image requires bounded padded base64 PNG bytes".into());
    }
    let mut decoded = Vec::with_capacity(encoded.len() / 4 * 3);
    let val = |b: u8| -> Result<u8, String> { match b {
        b'A'..=b'Z' => Ok(b - b'A'), b'a'..=b'z' => Ok(b - b'a' + 26),
        b'0'..=b'9' => Ok(b - b'0' + 52), b'+' => Ok(62), b'/' => Ok(63),
        _ => Err("image contains invalid base64".into()),
    }};
    for (index, chunk) in encoded.as_bytes().chunks_exact(4).enumerate() {
        let a = val(chunk[0])?; let b = val(chunk[1])?;
        let last = (index + 1) * 4 == encoded.len();
        decoded.push((a << 2) | (b >> 4));
        if chunk[2] == b'=' {
            if !last || chunk[3] != b'=' || b & 15 != 0 { return Err("invalid base64 padding".into()); }
        } else {
            let c = val(chunk[2])?;
            decoded.push((b << 4) | (c >> 2));
            if chunk[3] == b'=' {
                if !last || c & 3 != 0 { return Err("invalid base64 padding".into()); }
            } else { decoded.push((c << 6) | val(chunk[3])?); }
        }
    }
    if decoded.len() > 96 * 1024 || decoded.len() < 33 || &decoded[..8] != b"\x89PNG\r\n\x1a\n"
        || decoded[8..12] != [0, 0, 0, 13] || &decoded[12..16] != b"IHDR" {
        return Err("image must be a PNG of at most 96 KiB with an IHDR header".into());
    }
    let width = u32::from_be_bytes(decoded[16..20].try_into().unwrap());
    let height = u32::from_be_bytes(decoded[20..24].try_into().unwrap());
    if width == 0 || height == 0 || width > 1024 || height > 1024 {
        return Err("PNG dimensions must be 1–1024 pixels per side".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn card(kind: RoomCardKind) -> RoomCardContent {
        RoomCardContent { key: "review".into(), kind, title: "Review".into(), summary: "Ready".into(),
            body: String::new(), state: RoomCardState::Open, data: RoomCardData::default() }
    }
    #[test]
    fn validates_fixed_data_and_rejects_markup_fields_and_mismatched_kind() {
        let mut value = card(RoomCardKind::Decision);
        assert!(value.validate().is_ok());
        assert!(value.pinned());
        value.data.value = Some(1.into());
        assert!(value.validate().is_err());
        let mut json = serde_json::to_value(card(RoomCardKind::Report)).unwrap();
        json["html"] = "<script>".into();
        assert!(serde_json::from_value::<RoomCardContent>(json).is_err());
        let mut value = card(RoomCardKind::Progress);
        value.data.done = Some(2); value.data.total = Some(1);
        assert!(value.validate().is_err());
        value.data.total = Some(3);
        assert!(value.validate().is_ok());
    }
    #[test]
    fn png_is_embedded_bounded_and_dimension_checked() {
        let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+a8V8AAAAASUVORK5CYII=";
        assert!(validate_png(png).is_ok());
        assert!(validate_png("https://example.com/image.png").is_err());
        assert!(validate_png(&"A".repeat(ROOM_CARD_MAX_BYTES + 4)).is_err());
        assert!(validate_png(&png.replace("AAAAEAAAAB", "AAAEAQAAA")).is_err());
        assert!(validate_png(&format!("{}=", png)).is_err());
    }
}
