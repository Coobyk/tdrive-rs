use std::path::PathBuf;

use chrono::{DateTime, Utc};
use grammers_client::Client;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_session::types::PeerRef;

#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_id: i32,
    pub api_hash: String,
}

/// How a chat is grouped in the sidebar, mirroring the original's
/// "My Drive" / "Shared with me" split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// A one-to-one conversation or Saved Messages: the personal drive.
    Personal,
    /// A group or broadcast channel: a place shared with other people.
    Shared,
}

impl DialogKind {
    pub fn is_personal(self) -> bool {
        matches!(self, DialogKind::Personal)
    }
}

#[derive(Clone)]
pub struct DialogInfo {
    pub id: i64,
    pub title: String,
    pub kind: DialogKind,
    pub peer_ref: PeerRef,
}

/// Broad file category, used only to pick the row's leading glyph and label.
///
/// These are the families from the original `file-list/file-type.ts`: coarse
/// on purpose, because a reader does not need to tell ODT from RTF at list
/// speed, only a document from a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Folder,
    Document,
    Image,
    Video,
    Audio,
    Sheet,
    Code,
    Archive,
    Other,
}

impl FileKind {
    pub fn from_name_and_mime(name: &str, mime: Option<&str>) -> Self {
        if let Some(mime) = mime {
            if mime.starts_with("image/") {
                return FileKind::Image;
            }
            if mime.starts_with("video/") {
                return FileKind::Video;
            }
            if mime.starts_with("audio/") {
                return FileKind::Audio;
            }
            if mime.contains("zip") || mime.contains("tar") || mime.contains("rar") {
                return FileKind::Archive;
            }
            if mime.contains("sheet") || mime.contains("excel") || mime.contains("csv") {
                return FileKind::Sheet;
            }
            if mime.contains("pdf") || mime.starts_with("text/") || mime.contains("document") {
                return FileKind::Document;
            }
        }
        let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        match ext.as_str() {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "avif" | "bmp" | "tiff"
            | "tif" | "svg" | "ico" | "psd" => FileKind::Image,
            "mp4" | "mkv" | "mov" | "webm" | "avi" | "m4v" | "wmv" | "flv" | "mpg" | "mpeg"
            | "3gp" => FileKind::Video,
            "mp3" | "m4a" | "flac" | "wav" | "aac" | "ogg" | "opus" | "wma" | "aiff" | "mid" => {
                FileKind::Audio
            }
            "xls" | "xlsx" | "ods" | "csv" | "tsv" | "number" => FileKind::Sheet,
            "js" | "mjs" | "cjs" | "ts" | "jsx" | "tsx" | "svelte" | "vue" | "go" | "py" | "rs"
            | "java" | "kt" | "swift" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "rb" | "php"
            | "sh" | "zsh" | "bash" | "lua" | "sql" | "html" | "htm" | "css" | "scss" | "sass"
            | "less" | "json" | "yaml" | "yml" | "toml" | "xml" | "ini" => FileKind::Code,
            "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "iso" | "dmg" => {
                FileKind::Archive
            }
            "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "markdn" | "epub" | "pages"
            | "tex" => FileKind::Document,
            _ => FileKind::Other,
        }
    }

    /// The uppercase label a row's meta line leads with on the phone layout's
    /// single meta line ("PDF", "Folder").
    #[allow(dead_code)]
    pub fn label(self, name: &str) -> String {
        if self == FileKind::Folder {
            return "Folder".to_string();
        }
        let ext = name.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
        if ext.is_empty() || ext.len() > 6 {
            "File".to_string()
        } else {
            ext.to_uppercase()
        }
    }
}

#[derive(Debug, Clone)]
pub struct RemoteFile {
    pub message_id: i32,
    pub name: String,
    pub size: i64,
    pub kind: FileKind,
    pub date: Option<DateTime<Utc>>,
    pub local_state: LocalState,
}

/// Where a row is in the download lifecycle, so the list can show progress
/// rather than a static "downloaded" line in a status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalState {
    Remote,
    Downloading { done: i64 },
    Downloaded(PathBuf),
    Failed(String),
}

/// Which destination the main pane is showing. The sidebar's Photos and Trash
/// items are real destinations in the original shell -- the file list with the
/// chrome swapped around it -- rather than filters applied on top of one view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    /// A drive's own contents.
    Drive,
    /// The active drive, narrowed to images.
    Photos,
    /// Deleted items waiting to be restored or purged.
    Trash,
}

impl Default for ViewMode {
    fn default() -> Self {
        ViewMode::Drive
    }
}

/// How long a deleted item waits before the backend purges it. The original's
/// trash reports a countdown against this deadline, so it is the number the
/// port needs to reproduce the same wording.
pub const PURGE_WINDOW_DAYS: i64 = 30;

/// An item in the trash: the file it came from, where it came from, and when it
/// was deleted. [`TrashEntry::purge_after`] is derived so the countdown has a
/// single source.
#[derive(Debug, Clone)]
pub struct TrashEntry {
    pub file: RemoteFile,
    /// The title of the drive the file was deleted from.
    pub origin: String,
    pub deleted_at: DateTime<Utc>,
}

impl TrashEntry {
    pub fn purge_after(&self) -> DateTime<Utc> {
        self.deleted_at + chrono::Duration::days(PURGE_WINDOW_DAYS)
    }
}

/// Which column the list is ordered by, and which way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Date,
    Size,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub key: SortKey,
    pub ascending: bool,
}

impl Default for SortState {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            ascending: true,
        }
    }
}

impl SortState {
    /// Clicking the active column flips direction; a new column starts ascending.
    pub fn toggle(&mut self, key: SortKey) {
        if self.key == key {
            self.ascending = !self.ascending;
        } else {
            self.key = key;
            self.ascending = true;
        }
    }

    pub fn is_active(&self, key: SortKey) -> bool {
        self.key == key
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginStep {
    NeedCredentials,
    NeedPhone,
    NeedCode,
    NeedPassword,
    LoggedIn,
}

impl Default for LoginStep {
    fn default() -> Self {
        LoginStep::NeedCredentials
    }
}

pub struct LoginFlow {
    pub step: LoginStep,
    pub phone: String,
    pub login_token: Option<LoginToken>,
    pub password_token: Option<PasswordToken>,
    pub error: Option<String>,
    pub working: Option<String>,
}

impl Default for LoginFlow {
    fn default() -> Self {
        Self {
            step: LoginStep::NeedCredentials,
            phone: String::new(),
            login_token: None,
            password_token: None,
            error: None,
            working: None,
        }
    }
}

pub struct AppState {
    pub creds: Option<Credentials>,
    pub session_path: PathBuf,
    pub login: LoginFlow,
    pub tele: Option<Client>,

    pub dialogs: Vec<DialogInfo>,
    pub dialogs_loading: bool,
    pub active_drive: Option<usize>,

    pub files: Vec<RemoteFile>,
    pub files_loading: bool,
    pub selected_file: Option<usize>,

    pub query: String,
    /// Sort applied to the visible rows, mirroring the original's sortable
    /// Name / Date / Size headers.
    pub sort: SortState,
    /// The sidebar destination the main pane is showing.
    pub view: ViewMode,
    /// True when the shell is running on [`AppState::seed_demo`] data rather
    /// than a live Telegram session (`TDRIVE_DEMO=1`). Mutating actions take a
    /// local path in this mode so the flows stay walkable without credentials.
    pub demo: bool,
    /// Items deleted from a drive this session, oldest purge deadline first.
    pub trash: Vec<TrashEntry>,
    /// The message id whose live row is asking for delete confirmation.
    pub pending_delete: Option<i32>,
    /// The trash index whose row is asking for purge confirmation.
    pub pending_purge: Option<usize>,
    /// Whether the "Empty trash" control has been armed by a first click.
    pub confirm_empty_trash: bool,
    pub status: String,
    pub error: Option<String>,
}

impl Default for AppState {
    fn default() -> Self {
        let data_dir = directories::ProjectDirs::from("io", "TDrive", "tdrive-rs")
            .map(|d| d.data_dir().to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let _ = std::fs::create_dir_all(&data_dir);
        Self {
            creds: None,
            session_path: data_dir.join("tdrive.session"),
            login: LoginFlow::default(),
            tele: None,
            dialogs: Vec::new(),
            dialogs_loading: false,
            active_drive: None,
            files: Vec::new(),
            files_loading: false,
            selected_file: None,
            query: String::new(),
            sort: SortState::default(),
            view: ViewMode::default(),
            demo: false,
            trash: Vec::new(),
            pending_delete: None,
            pending_purge: None,
            confirm_empty_trash: false,
            status: "Ready".to_string(),
            error: None,
        }
    }
}

impl AppState {
    pub fn active_dialog(&self) -> Option<&DialogInfo> {
        self.active_drive.and_then(|i| self.dialogs.get(i))
    }

    pub fn photos_only(&self) -> bool {
        self.view == ViewMode::Photos
    }

    pub fn trash_mode(&self) -> bool {
        self.view == ViewMode::Trash
    }

    /// Rows the list should draw for the current search box, sort and view.
    /// Trash mode has its own set, so this returns nothing for it.
    pub fn visible_files(&self) -> Vec<usize> {
        if self.trash_mode() {
            return Vec::new();
        }
        let mut indices = self.order(&self.files, self.photos_only());
        indices.shrink_to_fit();
        indices
    }

    /// Trash rows for the current search and sort. The Date column reports the
    /// purge deadline in trash mode, so the sort follows that deadline rather
    /// than the day the file was first uploaded.
    pub fn visible_trash(&self) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        let mut indices: Vec<usize> = self
            .trash
            .iter()
            .enumerate()
            .filter(|(_, e)| query.is_empty() || e.file.name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect();

        let ascending = self.sort.ascending;
        indices.sort_by(|&a, &b| {
            let ea = &self.trash[a];
            let eb = &self.trash[b];
            let ordering = match self.sort.key {
                SortKey::Name => ea
                    .file
                    .name
                    .to_lowercase()
                    .cmp(&eb.file.name.to_lowercase()),
                SortKey::Size => ea.file.size.cmp(&eb.file.size),
                SortKey::Date => ea.purge_after().cmp(&eb.purge_after()),
            };
            if ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
        indices
    }

    /// Filter and order a set of files, keeping the original ordering when the
    /// search box and Photos mode are both off.
    fn order(&self, files: &[RemoteFile], images_only: bool) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        let mut indices: Vec<usize> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| !images_only || f.kind == FileKind::Image)
            .filter(|(_, f)| query.is_empty() || f.name.to_lowercase().contains(&query))
            .map(|(i, _)| i)
            .collect();

        let ascending = self.sort.ascending;
        indices.sort_by(|&a, &b| {
            let fa = &files[a];
            let fb = &files[b];
            let ordering = match self.sort.key {
                SortKey::Name => fa.name.to_lowercase().cmp(&fb.name.to_lowercase()),
                SortKey::Size => fa.size.cmp(&fb.size),
                SortKey::Date => fa.date.cmp(&fb.date),
            };
            if ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
        indices
    }

    pub fn total_size(&self) -> i64 {
        self.files.iter().map(|f| f.size.max(0)).sum()
    }

    /// Move a live file into the trash, remembering where it came from so the
    /// row can say so. The origin is passed in rather than read from the active
    /// drive, because a drive switch while a delete is in flight must not
    /// relabel where the file came from. Returns the entry that was added.
    pub fn trash_file(&mut self, index: usize, origin: &str) -> Option<TrashEntry> {
        if index >= self.files.len() {
            return None;
        }
        let file = self.files.remove(index);
        let entry = TrashEntry {
            file,
            origin: origin.to_string(),
            deleted_at: Utc::now(),
        };
        self.trash.push(entry.clone());
        Some(entry)
    }

    /// Populate representative data so the shell can be built, run and
    /// screenshotted without a live Telegram session (`TDRIVE_DEMO=1`).
    pub fn seed_demo(&mut self) {
        use chrono::Duration;
        use grammers_session::types::PeerAuth;

        let peer = |id: i64| PeerRef {
            id: grammers_session::types::PeerId::user_unchecked(id),
            auth: PeerAuth::default(),
        };

        let now = Utc::now();
        let dialog = |id: i64, title: &str, kind: DialogKind| DialogInfo {
            id,
            title: title.to_string(),
            kind,
            peer_ref: peer(id),
        };

        self.dialogs = vec![
            dialog(1, "Saved Messages", DialogKind::Personal),
            dialog(2, "Notes & Ideas", DialogKind::Personal),
            dialog(3, "Design Team", DialogKind::Shared),
            dialog(4, "Family Photos", DialogKind::Shared),
            dialog(5, "Release Archives", DialogKind::Shared),
        ];
        self.status = format!("{} drives", self.dialogs.len());

        let file =
            |message_id: i32, name: &str, size: i64, kind: FileKind, hours: i64| -> RemoteFile {
                RemoteFile {
                    message_id,
                    name: name.to_string(),
                    size,
                    kind,
                    date: Some(now - Duration::hours(hours)),
                    local_state: LocalState::Remote,
                }
            };

        self.files = vec![
            file(99, "Design assets", 0, FileKind::Folder, 1),
            file(101, "Q3 roadmap.pdf", 2_412_544, FileKind::Document, 2),
            file(102, "budget-2026.xlsx", 184_320, FileKind::Sheet, 5),
            file(103, "hero-shot.png", 4_981_504, FileKind::Image, 9),
            file(104, "product-demo.mp4", 268_435_456, FileKind::Video, 26),
            file(105, "team-photo.jpg", 3_145_728, FileKind::Image, 30),
            file(106, "voice-note.m4a", 512_000, FileKind::Audio, 33),
            file(107, "assets.zip", 1_073_741_824, FileKind::Archive, 50),
            file(108, "release-notes.md", 9_216, FileKind::Document, 74),
            file(109, "screenshot-01.png", 745_472, FileKind::Image, 96),
            file(110, "invoice-042.pdf", 122_880, FileKind::Document, 120),
            file(111, "main.rs", 41_984, FileKind::Code, 200),
        ];

        self.active_drive = Some(0);
        self.view = ViewMode::Drive;
        self.demo = true;
        self.query.clear();
        self.selected_file = None;
        self.pending_delete = None;
        self.pending_purge = None;
        self.confirm_empty_trash = false;
        self.error = None;
        self.trash = vec![
            TrashEntry {
                file: file(201, "old-draft.docx", 87_040, FileKind::Document, 30),
                origin: "Saved Messages".to_string(),
                deleted_at: now - Duration::days(2),
            },
            TrashEntry {
                file: file(202, "broken-export.zip", 5_242_880, FileKind::Archive, 44),
                origin: "Release Archives".to_string(),
                deleted_at: now - Duration::days(9),
            },
            TrashEntry {
                file: file(203, "tmp-render.png", 1_310_720, FileKind::Image, 51),
                origin: "Design Team".to_string(),
                deleted_at: now - Duration::days(21),
            },
        ];
        self.login.step = LoginStep::LoggedIn;
    }
}
