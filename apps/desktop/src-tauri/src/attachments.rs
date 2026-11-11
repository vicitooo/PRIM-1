//! Room image attachments (Georgi themes). The composer saves a pasted or
//! dropped image under `<runtime>/attachments/<room id>/` and shares its path
//! with members as text; the feed reads it back for a thumbnail. Only image
//! files of at most `MAX_ATTACHMENT_BYTES` are accepted, and reads are
//! confined to the attachments folder.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::Serialize;
use tauri::State;

use crate::DesktopState;

pub const MAX_ATTACHMENT_BYTES: usize = 10 * 1024 * 1024;
const ALLOWED: [(&str, &str); 5] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("gif", "image/gif"),
    ("webp", "image/webp"),
];

#[derive(Serialize)]
pub struct SavedAttachment {
    pub path: String,
    pub name: String,
    pub bytes: usize,
}

fn attachments_root(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("attachments")
}

fn mime_for(extension: &str) -> Option<&'static str> {
    let lower = extension.to_ascii_lowercase();
    ALLOWED.iter().find(|(ext, _)| *ext == lower).map(|(_, mime)| *mime)
}

/// A file name safe on Windows: letters, digits, `-`, `_` and `.`, at most 80 chars.
fn sanitize_stem(name: &str) -> String {
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("image");
    let cleaned: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .take(80)
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() { "image".to_string() } else { trimmed.to_string() }
}

fn valid_room_id(room_id: &str) -> bool {
    !room_id.is_empty()
        && room_id.len() <= 64
        && room_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

pub fn save_attachment(
    runtime_dir: &Path,
    room_id: &str,
    file_name: &str,
    data_base64: &str,
) -> Result<SavedAttachment, String> {
    if !valid_room_id(room_id) {
        return Err("invalid room id".into());
    }
    let extension = Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if mime_for(extension).is_none() {
        return Err(format!(
            "{file_name}: only PNG, JPG, GIF and WebP images can be attached"
        ));
    }
    if data_base64.len() > MAX_ATTACHMENT_BYTES / 3 * 4 + 8 {
        return Err(format!("{file_name} is over the 10 MB limit"));
    }
    let bytes = STANDARD
        .decode(data_base64.as_bytes())
        .map_err(|error| format!("{file_name}: unreadable image data ({error})"))?;
    if bytes.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!("{file_name} is over the 10 MB limit"));
    }
    let dir = attachments_root(runtime_dir).join(room_id);
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create {}: {error}", dir.display()))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    let name = format!(
        "{stamp}-{}.{}",
        sanitize_stem(file_name),
        extension.to_ascii_lowercase()
    );
    let path = dir.join(&name);
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("cannot save {}: {error}", path.display()))?;
    Ok(SavedAttachment {
        path: plain_path(&path),
        name,
        bytes: bytes.len(),
    })
}

/// The path as agents and people expect it: without the `\\?\` verbatim
/// prefix a canonicalized runtime directory carries, except for UNC shares.
fn plain_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with(r"UNC\") => rest.to_string(),
        _ => text.into_owned(),
    }
}

/// A data URL for an attachment that lives under the attachments folder.
pub fn read_attachment(runtime_dir: &Path, path: &str) -> Result<String, String> {
    let root = attachments_root(runtime_dir)
        .canonicalize()
        .map_err(|_| "no attachments have been saved yet".to_string())?;
    let file = Path::new(path)
        .canonicalize()
        .map_err(|_| format!("{path} no longer exists"))?;
    if !file.starts_with(&root) {
        return Err("only PRIM-1 room attachments can be previewed".into());
    }
    let mime = file
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(mime_for)
        .ok_or_else(|| "not an image attachment".to_string())?;
    let metadata = std::fs::metadata(&file).map_err(|error| error.to_string())?;
    if metadata.len() as usize > MAX_ATTACHMENT_BYTES {
        return Err("attachment is over the 10 MB limit".into());
    }
    let bytes = std::fs::read(&file).map_err(|error| error.to_string())?;
    Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
}

#[tauri::command]
pub fn save_room_attachment(
    state: State<'_, DesktopState>,
    room_id: String,
    file_name: String,
    data_base64: String,
) -> Result<SavedAttachment, String> {
    let saved = save_attachment(state.supervisor.runtime_dir(), &room_id, &file_name, &data_base64)?;
    state
        .diagnostics
        .log("info", "save_room_attachment", format!("{} bytes", saved.bytes));
    Ok(saved)
}

#[tauri::command]
pub fn read_room_attachment(state: State<'_, DesktopState>, path: String) -> Result<String, String> {
    read_attachment(state.supervisor.runtime_dir(), &path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("prim1-attach-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn saves_and_reads_back_an_image() {
        let root = temp_root("roundtrip");
        let data = STANDARD.encode(b"\x89PNG fake");
        let saved = save_attachment(&root, "room-1", "my shot (1).png", &data).unwrap();
        assert!(saved.name.ends_with("-my-shot--1.png"));
        let url = read_attachment(&root, &saved.path).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
    }

    #[test]
    fn strips_the_verbatim_prefix_but_not_unc() {
        assert_eq!(plain_path(Path::new(r"\\?\C:\x\a.png")), r"C:\x\a.png");
        assert_eq!(plain_path(Path::new(r"\\?\UNC\srv\a.png")), r"\\?\UNC\srv\a.png");
        assert_eq!(plain_path(Path::new(r"C:\x\a.png")), r"C:\x\a.png");
    }

    #[test]
    fn rejects_other_types_bad_rooms_and_outside_paths() {
        let root = temp_root("reject");
        let data = STANDARD.encode(b"x");
        assert!(save_attachment(&root, "room-1", "notes.exe", &data).is_err());
        assert!(save_attachment(&root, "../evil", "a.png", &data).is_err());
        save_attachment(&root, "room-1", "a.png", &data).unwrap();
        let outside = root.join("outside.png");
        std::fs::write(&outside, b"x").unwrap();
        assert!(read_attachment(&root, &outside.to_string_lossy()).is_err());
    }
}
