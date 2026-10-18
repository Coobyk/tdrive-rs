//! A local snapshot of a drive's projection.
//!
//! Reading a drive means replaying its whole operation history, and Telegram
//! serves history a hundred messages at a time: a drive of a few thousand
//! operations costs dozens of round trips, every launch. The snapshot keeps
//! what the replay produced — objects, trash groups, multipart parts, the
//! encryption config — so the next launch only asks for the operations that
//! arrived since, and "Reload" is what rebuilds the whole thing.
//!
//! A snapshot is a cache, never the authority: it lives in this port's own file
//! beside the original's `config.json`, and a snapshot this build cannot read
//! is ignored and rebuilt.

use std::path::{Path, PathBuf};

use crate::config;
use crate::tele::ReplayState;

/// The snapshot format. Bump it whenever [`ReplayState`] changes shape, so an
/// older file is rebuilt instead of misread.
pub const VERSION: u32 = 1;

/// One drive's projected state, and the message it was projected up to.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct Snapshot {
    /// Format version; see [`VERSION`].
    pub version: u32,
    /// The drive this state belongs to. A snapshot is only ever resumed by the
    /// drive that wrote it, so switching drives cannot mix two projections.
    pub drive_id: i64,
    /// The newest message the state accounted for. The next load asks for
    /// messages after this one.
    pub newest_message_id: i32,
    /// The replay's own state: see [`ReplayState`].
    pub state: ReplayState,
}

/// Where snapshots live, under the shared data directory.
fn dir() -> PathBuf {
    config::data_dir().join("projection")
}

fn path_in(dir: &Path, drive_id: i64) -> PathBuf {
    dir.join(format!("{drive_id}.json"))
}

/// The snapshot for a drive, when this build can read one.
pub fn load(drive_id: i64) -> Option<Snapshot> {
    load_in(&dir(), drive_id)
}

/// Write a drive's snapshot, replacing any earlier one atomically.
pub fn save(snapshot: &Snapshot) -> Result<(), String> {
    save_in(&dir(), snapshot)
}

/// Forget a drive's snapshot, so the next load reads the whole history again.
pub fn invalidate(drive_id: i64) {
    invalidate_in(&dir(), drive_id)
}

/// Drop one object from a drive's snapshot.
///
/// Deleting a drive object removes the message that carried its operation, and
/// a message that is gone is invisible to a watermark: the object has to be
/// taken out of the cached projection as well, or it comes back on the next
/// launch. Removing it here is what a full rebuild would produce, because a
/// full rebuild no longer sees the message either.
pub fn forget_object(drive_id: i64, object_id: &str) {
    forget_object_in(&dir(), drive_id, object_id)
}

/// [`load`], reading from an explicit directory.
pub fn load_in(dir: &Path, drive_id: i64) -> Option<Snapshot> {
    let raw = std::fs::read_to_string(path_in(dir, drive_id)).ok()?;
    let snapshot: Snapshot = match serde_json::from_str(&raw) {
        Ok(snapshot) => snapshot,
        // A half-written or older file is not an error worth reporting: the
        // drive is simply read from its history again.
        Err(_) => return None,
    };
    (snapshot.version == VERSION && snapshot.drive_id == drive_id).then_some(snapshot)
}

/// [`save`], writing into an explicit directory.
pub fn save_in(dir: &Path, snapshot: &Snapshot) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_string(snapshot).map_err(|e| e.to_string())?;
    // Write beside the target and rename, so a crash mid-write cannot leave a
    // truncated snapshot behind for the next launch to read.
    let target = path_in(dir, snapshot.drive_id);
    let staging = dir.join(format!("{}.tmp", snapshot.drive_id));
    std::fs::write(&staging, json).map_err(|e| e.to_string())?;
    std::fs::rename(&staging, &target).map_err(|e| e.to_string())
}

/// [`invalidate`], on an explicit directory.
pub fn invalidate_in(dir: &Path, drive_id: i64) {
    let _ = std::fs::remove_file(path_in(dir, drive_id));
}

/// [`forget_object`], on an explicit directory.
pub fn forget_object_in(dir: &Path, drive_id: i64, object_id: &str) {
    let Some(mut snapshot) = load_in(dir, drive_id) else {
        return;
    };
    if snapshot.state.objects.remove(object_id).is_none() {
        return;
    }
    let _ = save_in(dir, &snapshot);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ContentRef, FileKind, LocalState, RemoteFile};

    /// A directory of its own for one test, so two tests never share a file.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("tdrive-cache-{name}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).expect("a scratch directory");
        dir
    }

    fn file(object_id: &str, name: &str) -> RemoteFile {
        RemoteFile {
            message_id: 7,
            object_id: object_id.to_string(),
            name: name.to_string(),
            size: 12,
            kind: FileKind::Document,
            date: None,
            local_state: LocalState::Remote,
            parent: String::new(),
            encrypted: false,
            plaintext_size: None,
            content: ContentRef::Message(9),
            revision: 0,
        }
    }

    fn snapshot(drive_id: i64) -> Snapshot {
        let mut state = ReplayState::default();
        state
            .objects
            .insert("f:7".to_string(), file("f:7", "notes.txt"));
        state.media.insert(7, (Some(FileKind::Document), Some(12)));
        Snapshot {
            version: VERSION,
            drive_id,
            newest_message_id: 42,
            state,
        }
    }

    /// The snapshot has to survive a round trip through the file it is stored
    /// in, including the content reference that tells a download where the
    /// bytes are and the media sizes the listing shows.
    #[test]
    fn a_snapshot_round_trips_through_the_file() {
        let dir = scratch("round-trip");
        save_in(&dir, &snapshot(-1001234567890)).expect("the snapshot is written");

        let back = load_in(&dir, -1001234567890).expect("the snapshot is read");
        assert_eq!(back.newest_message_id, 42);
        let file = back.state.objects.get("f:7").expect("the object survives");
        assert_eq!(file.name, "notes.txt");
        assert_eq!(file.content, ContentRef::Message(9));
        assert_eq!(
            back.state.media.get(&7),
            Some(&(Some(FileKind::Document), Some(12)))
        );
        assert!(
            load_in(&dir, -1009999999999).is_none(),
            "another drive has no snapshot"
        );
    }

    /// A snapshot this build cannot vouch for is rebuilt, not misread: the
    /// fields may mean something else by now.
    #[test]
    fn an_older_or_foreign_snapshot_is_ignored() {
        let dir = scratch("version");
        let mut older = snapshot(1);
        older.version = VERSION - 1;
        save_in(&dir, &older).expect("written");
        assert!(load_in(&dir, 1).is_none(), "an older format is ignored");

        // A file that is not a snapshot at all reads as no snapshot.
        std::fs::write(path_in(&dir, 2), b"{ not json").expect("written");
        assert!(load_in(&dir, 2).is_none());

        // And one for another drive id in the file's name is not resumed.
        save_in(&dir, &snapshot(3)).expect("written");
        let mut renamed = snapshot(4);
        renamed.drive_id = 3;
        save_in(&dir, &renamed).expect("written");
        assert!(load_in(&dir, 4).is_none());
    }

    /// Reloading a drive throws the snapshot away, and forgetting one object
    /// takes it out of the cached projection without disturbing the rest.
    #[test]
    fn invalidating_and_forgetting_change_what_the_next_launch_sees() {
        let dir = scratch("invalidate");
        save_in(&dir, &snapshot(5)).expect("written");

        forget_object_in(&dir, 5, "f:7");
        let after = load_in(&dir, 5).expect("still there");
        assert!(
            after.state.objects.is_empty(),
            "the deleted object is gone from the cache"
        );

        // Forgetting an object the state does not hold leaves the file alone.
        forget_object_in(&dir, 5, "f:999");
        assert!(load_in(&dir, 5).is_some());

        invalidate_in(&dir, 5);
        assert!(load_in(&dir, 5).is_none(), "a reload starts from the drive");
    }
}
