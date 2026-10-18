//! The locally saved drives, mirroring the original's `config.json`.
//!
//! The original keeps one `channel_id` in `config.json` (the personal drive)
//! and the rest of its drive list in SQLite. This port has no database yet, so
//! the same file also carries the small amount of channel metadata a peer
//! reference needs: access hash, title and kind. The `channel_id` key is kept
//! verbatim so a file written by the original still reads as "a drive is
//! saved", and an unreadable or missing file behaves like the original's
//! `ErrConfigInvalid`: the drive picker appears rather than an error.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The title and about text the original gives a drive it creates itself.
pub const DEFAULT_CHANNEL_TITLE: &str = "TDrive";
pub const DEFAULT_CHANNEL_ABOUT: &str = "TDrive personal storage";

/// The directory TDrive keeps its own files in.
///
/// This is the original's `datadir.Dir()`: the platform's config directory plus
/// `TDrive`, which is `~/.config/TDrive` on Linux, `Application Support/TDrive`
/// on macOS and `%AppData%\TDrive` on Windows. Sharing it is what lets this port
/// find the drive the original saved — and the application credentials its
/// setup screen remembers in `imp_config.json` — instead of asking for them
/// again.
pub fn data_dir() -> PathBuf {
    directories::BaseDirs::new()
        .map(|dirs| dirs.config_dir().join("TDrive"))
        .unwrap_or_else(|| PathBuf::from("TDrive"))
}

/// Create the data directory with the original's private mode.
fn ensure_data_dir() -> Result<(), String> {
    let path = data_dir();
    std::fs::create_dir_all(&path).map_err(|e| e.to_string())?;
    restrict(&path, 0o700);
    Ok(())
}

/// Give a file or directory the original's private mode, where the platform
/// has one. Windows has no equivalent, so this is a no-op there.
fn restrict(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
}

/// Write a file only this user can read, as the original does for `config.json`
/// and `imp_config.json`.
fn write_private(path: &Path, contents: &str) -> Result<(), String> {
    std::fs::write(path, contents).map_err(|e| e.to_string())?;
    restrict(path, 0o600);
    Ok(())
}

/// The credentials the original's setup screen stores, in the file and spelling
/// it uses (`API_ID` / `API_HASH` in `imp_config.json`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ImpCredentials {
    #[serde(rename = "API_ID")]
    pub api_id: i32,
    #[serde(rename = "API_HASH")]
    pub api_hash: String,
}

#[derive(Serialize)]
struct ImpCredentialsOut<'a> {
    #[serde(rename = "API_ID")]
    api_id: i32,
    #[serde(rename = "API_HASH")]
    api_hash: &'a str,
}

/// The saved application credentials, if the setup screen has ever completed.
pub fn load_credentials() -> Option<ImpCredentials> {
    let raw = std::fs::read_to_string(data_dir().join("imp_config.json")).ok()?;
    parse_credentials(&raw)
}

/// Read the credentials file's contents, the way the original's
/// `LoadImpCredentials` does.
fn parse_credentials(raw: &str) -> Option<ImpCredentials> {
    let creds: ImpCredentials = serde_json::from_str(raw).ok()?;
    let usable = creds.api_id != 0 && !creds.api_hash.trim().is_empty();
    usable.then_some(creds)
}

/// Remember the application credentials the way the original does, so signing
/// in again does not mean retyping them from my.telegram.org.
pub fn save_credentials(api_id: i32, api_hash: &str) -> Result<(), String> {
    ensure_data_dir()?;
    let json = serde_json::to_string_pretty(&ImpCredentialsOut { api_id, api_hash })
        .map_err(|e| e.to_string())?;
    write_private(&data_dir().join("imp_config.json"), &json)
}

/// A drive as it is remembered between runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredDrive {
    pub id: i64,
    /// Telegram's `access_hash` for the channel. Without it the peer cannot be
    /// addressed on a later run.
    pub access_hash: i64,
    pub title: String,
    /// `personal` for My Drive, `shared` for a drive the user made or joined.
    pub kind: String,
}

impl StoredDrive {
    pub fn is_personal(&self) -> bool {
        self.kind == "personal"
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DriveConfig {
    /// The personal drive, spelled the way the original spells it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<i64>,
    /// The drives this device has been told about.
    ///
    /// The original keeps only `channel_id` here and the rest of its drive list
    /// in SQLite; this port has no database yet, so the same file also carries
    /// the metadata a peer reference needs. The original ignores keys it does
    /// not know, so the two can share the file.
    #[serde(default)]
    pub drives: Vec<StoredDrive>,
}

impl DriveConfig {
    pub fn path() -> PathBuf {
        data_dir().join("config.json")
    }

    /// Read the saved settings. `None` when there is no usable file, which is
    /// the state the drive picker starts from.
    pub fn load() -> Option<Self> {
        let raw = std::fs::read_to_string(Self::path()).ok()?;
        serde_json::from_str(&raw).ok()
    }

    pub fn save(&self) -> Result<(), String> {
        ensure_data_dir()?;
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_private(&Self::path(), &json)
    }

    /// The saved personal drive: the one `channel_id` names, falling back to a
    /// row recorded as personal.
    pub fn personal(&self) -> Option<&StoredDrive> {
        self.channel_id
            .and_then(|id| self.drives.iter().find(|drive| drive.id == id))
            .or_else(|| self.drives.iter().find(|drive| drive.is_personal()))
    }

    /// Remember a drive, replacing any earlier row for the same channel id.
    pub fn upsert(&mut self, drive: StoredDrive) {
        if let Some(existing) = self.drives.iter_mut().find(|row| row.id == drive.id) {
            *existing = drive;
            return;
        }
        self.drives.push(drive);
    }

    pub fn remember_personal(&mut self, drive: StoredDrive) {
        self.channel_id = Some(drive.id);
        self.upsert(drive);
    }

    /// Replace the personal drive. Selecting a different chat retires the
    /// earlier one, the way the original retires other personal rows.
    pub fn replace_personal(&mut self, drive: StoredDrive) {
        self.drives
            .retain(|row| !row.is_personal() || row.id == drive.id);
        self.remember_personal(drive);
    }
}

/// Whether a channel title is the one a TDrive-created channel carries. Only
/// used for ordering and the "Recommended" badge, never to detect a drive.
pub fn is_default_title(title: &str) -> bool {
    title.trim().eq_ignore_ascii_case(DEFAULT_CHANNEL_TITLE)
}

/// Order candidates the way the original does: a default-titled channel first,
/// then channels that have been used, then oldest, then by title.
pub fn order_candidates<T, K, C, A>(candidates: &mut [T], title: K, created: C, activity: A)
where
    K: Fn(&T) -> &str,
    C: Fn(&T) -> i64,
    A: Fn(&T) -> bool,
{
    candidates.sort_by(|a, b| {
        let default = is_default_title(title(b)).cmp(&is_default_title(title(a)));
        if default != std::cmp::Ordering::Equal {
            return default;
        }
        let activity = activity(b).cmp(&activity(a));
        if activity != std::cmp::Ordering::Equal {
            return activity;
        }
        created(a)
            .cmp(&created(b))
            .then_with(|| title(a).to_lowercase().cmp(&title(b).to_lowercase()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(id: i64, title: &str, kind: &str) -> StoredDrive {
        StoredDrive {
            id,
            access_hash: id * 7,
            title: title.to_string(),
            kind: kind.to_string(),
        }
    }

    #[test]
    fn a_file_written_by_the_original_still_names_a_personal_drive() {
        let config: DriveConfig = serde_json::from_str(r#"{"channel_id": 42}"#).unwrap();
        assert_eq!(config.channel_id, Some(42));
        assert!(config.drives.is_empty());
    }

    #[test]
    fn invalid_json_is_not_a_config() {
        assert!(serde_json::from_str::<DriveConfig>("{ not json").is_err());
    }

    /// The credentials file is the original's, key spelling included, so a
    /// machine that has already been set up does not have to be set up again.
    #[test]
    fn credentials_are_read_from_the_originals_file() {
        let original = r#"{
 "API_ID": 123456,
 "API_HASH": "0123456789abcdef0123456789abcdef"
}"#;
        let creds = parse_credentials(original).expect("credentials");
        assert_eq!(creds.api_id, 123456);
        assert_eq!(creds.api_hash, "0123456789abcdef0123456789abcdef");

        // An id with no hash, or the other way round, is not usable.
        assert!(parse_credentials(r#"{"API_ID": 1, "API_HASH": "  "}"#).is_none());
        assert!(parse_credentials(r#"{"API_ID": 0, "API_HASH": "abc"}"#).is_none());
        assert!(parse_credentials("not json").is_none());
    }

    /// And what this port writes reads back the same way, so the original can
    /// still load credentials that this port saved.
    #[test]
    fn saved_credentials_use_the_originals_encoding() {
        let json = serde_json::to_string_pretty(&ImpCredentialsOut {
            api_id: 777,
            api_hash: "hash",
        })
        .expect("encode");
        assert!(json.contains("\"API_ID\""));
        assert!(json.contains("\"API_HASH\""));
        assert_eq!(parse_credentials(&json).map(|c| c.api_id), Some(777));
    }

    /// Every file the port manages lives in the original's directory, so a
    /// migration finds the drive and the credentials that are already there.
    #[test]
    fn paths_are_the_originals_paths() {
        let dir = data_dir();
        assert!(dir.ends_with("TDrive"), "data dir: {}", dir.display());
        assert_eq!(DriveConfig::path().parent(), Some(dir.as_path()));
    }

    #[test]
    fn choosing_another_drive_retires_the_previous_personal_row() {
        let mut config = DriveConfig::default();
        config.replace_personal(drive(1, "Old", "personal"));
        config.replace_personal(drive(2, "New", "personal"));
        assert_eq!(config.channel_id, Some(2));
        assert_eq!(config.personal().map(|row| row.title.as_str()), Some("New"));
        assert_eq!(config.drives.len(), 1);
    }

    #[test]
    fn candidate_order_prefers_the_default_title_then_activity_then_age() {
        let mut rows = [
            (1i64, "Quiet", 100i64, false),
            (2, "TDrive", 300, false),
            (3, "Busy", 200, true),
        ];
        order_candidates(&mut rows, |row| row.1, |row| row.2, |row| row.3);
        assert_eq!(rows.map(|row| row.0), [2, 3, 1]);
        assert!(is_default_title("  tdrive "));
        assert!(!is_default_title("tdrive backup"));
    }
}
