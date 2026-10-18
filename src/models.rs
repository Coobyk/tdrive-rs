use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use grammers_client::Client;
use grammers_client::client::{LoginToken, PasswordToken};
use grammers_session::types::PeerRef;

use crate::crypto::{self, VaultMaterial};

#[derive(Debug, Clone)]
pub struct Credentials {
    pub api_id: i32,
    pub api_hash: String,
}

/// How a drive is grouped in the sidebar, mirroring the original's
/// "My Drive" / "Shared with me" split. The value is the `kind` column the
/// original stores for each channel row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    /// My Drive: the one personal drive the account keeps its files in.
    Personal,
    /// A drive the user created for other people, or joined by invite link.
    Shared,
}

impl DriveKind {
    pub fn is_personal(self) -> bool {
        matches!(self, DriveKind::Personal)
    }

    /// The wire/file spelling, matching `projection.KindPersonal` and
    /// `projection.KindShared`.
    pub fn as_str(self) -> &'static str {
        match self {
            DriveKind::Personal => "personal",
            DriveKind::Shared => "shared",
        }
    }

    pub fn from_str(raw: &str) -> Self {
        if raw == "shared" {
            DriveKind::Shared
        } else {
            DriveKind::Personal
        }
    }
}

/// A place holdable files: a Telegram channel.
///
/// A drive is always a channel. The original never treats a private chat, a
/// group, or Saved Messages as a drive, which is why the sidebar cannot be
/// filled by listing every conversation the account is in.
#[derive(Clone)]
pub struct Drive {
    pub id: i64,
    pub title: String,
    pub kind: DriveKind,
    pub peer_ref: PeerRef,
}

/// One entry in the "Choose your TDrive" picker: a broadcast channel the
/// account created, which is the only shape a TDrive channel can have.
#[derive(Clone)]
pub struct DriveCandidate {
    pub id: i64,
    pub title: String,
    /// Channel creation date, unix seconds.
    pub created: i64,
    /// Whether the channel already holds anything, which is what makes it a
    /// likelier drive than an empty channel of the same age.
    pub has_activity: bool,
    /// The channel a TDrive-created drive is named after, when it leads the
    /// list. Only ever set on one candidate.
    pub recommended: bool,
    pub peer_ref: PeerRef,
}

/// Broad file category, used only to pick the row's leading glyph and label.
///
/// These are the families from the original `file-list/file-type.ts`: coarse
/// on purpose, because a reader does not need to tell ODT from RTF at list
/// speed, only a document from a video.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RemoteFile {
    /// The message this row was read from: the op message for a `t=f` upload,
    /// the commit message for an `fcommit` file.
    pub message_id: i32,
    /// The drive's own object id for this row (`f:<id>` or `d:<id>`).
    ///
    /// It is not always derivable from `message_id`: a folder created through
    /// TDrive's writable-mount path is `d:<uuid>`, a durable id its commit
    /// message chose rather than the message's own number.
    pub object_id: String,
    pub name: String,
    pub size: i64,
    pub kind: FileKind,
    pub date: Option<DateTime<Utc>>,
    pub local_state: LocalState,
    /// The folder this object sits in: `""` for the drive root, otherwise the
    /// `d:<...>` id of its parent folder.
    pub parent: String,
    /// True when the drive's own metadata says the contents are encrypted
    /// (`enc=1`), which is what makes a row carry a lock and a download ask
    /// for the vault key.
    pub encrypted: bool,
    /// The size before encryption, when the metadata carries it. A locked row
    /// shows this rather than the ciphertext size.
    pub plaintext_size: Option<i64>,
    /// Which message, or messages, hold the bytes; see [`ContentRef`].
    pub content: ContentRef,
    /// The writable revision of this object, or `0` for the unversioned ops.
    pub revision: i64,
}

/// Where a file's bytes actually live.
///
/// A plain `t=f` upload is its own content: the document and the caption are
/// one message. The writable-mount path splits them, so a row has to remember
/// which message to fetch: an `fcommit` names a hidden body message, or a
/// multipart group whose parts have to be fetched in order.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum ContentRef {
    /// The op message itself carries the bytes (`t=f` uploads).
    OpMessage,
    /// Another message carries the whole content (`fcommit`/`freplace`).
    Message(i32),
    /// The content is these messages' bodies, in order.
    Parts(Vec<i32>),
}

impl Default for ContentRef {
    fn default() -> Self {
        ContentRef::OpMessage
    }
}

impl RemoteFile {
    /// A folder is an object with no contents of its own.
    pub fn is_folder(&self) -> bool {
        self.kind == FileKind::Folder
    }

    /// The size a row should show. An encrypted file's metadata reports the
    /// ciphertext length, so the plaintext length is preferred when known.
    pub fn display_size(&self) -> i64 {
        self.plaintext_size.unwrap_or(self.size)
    }
}

/// Where a row is in the download lifecycle, so the list can show progress
/// rather than a static "downloaded" line in a status bar.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

/// How many matches the search list draws per kind. The original's
/// `projection.Search` clamps its limit the same way, folders and files each
/// counted separately.
pub const SEARCH_LIMIT: usize = 200;

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
    /// Signed in, but no drive is saved on this device yet: the original shows
    /// the "Choose your TDrive" picker before any file list exists.
    ChooseDrive,
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

/// The drive's encryption config, and the key once the user has unlocked it.
///
/// The key lives here for the app session only; nothing about it is written to
/// disk, matching the original's "files stay unlocked until you lock, sign out
/// or close the app".
pub struct Vault {
    pub material: VaultMaterial,
    pub hint: String,
    pub key: Option<[u8; 32]>,
    pub prompt: Option<VaultPrompt>,
    pub error: Option<String>,
    pub working: bool,
}

/// Why the encryption prompt is on screen. Only one prompt exists at a time, so
/// a newer request replaces an older one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultPrompt {
    /// The encrypted row that asked for the key, when a file was being opened.
    pub message_id: Option<i32>,
}

/// The "How would you like to upload?" choice, while it is on screen.
///
/// The original asks this for every upload batch and starts on plain every
/// time: encryption is chosen per batch rather than remembered from the last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadOptions {
    /// How many files the batch holds; the modal's summary names the count.
    pub count: usize,
    /// The choice being made, starting on plain.
    pub encrypt: bool,
}

/// A chosen file waiting for the upload modal, the encryption password, or the
/// encryption setup to be settled before it is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingUpload {
    pub path: PathBuf,
    /// Whether this batch is to be encrypted before it is sent.
    pub encrypt: bool,
}

/// First-time encryption setup, while the modal is open.
#[derive(Debug, Clone, Default)]
pub struct EncryptionSetup {
    pub error: Option<String>,
    pub working: bool,
}

impl Vault {
    pub fn new(material: VaultMaterial, hint: String) -> Self {
        Self {
            material,
            hint,
            key: None,
            prompt: None,
            error: None,
            working: false,
        }
    }

    pub fn unlocked(&self) -> bool {
        self.key.is_some()
    }

    /// Forget the key and wipe the copy that was held.
    pub fn lock(&mut self) {
        if let Some(key) = self.key.as_mut() {
            crypto::wipe(key);
        }
        self.key = None;
        self.prompt = None;
        self.error = None;
        self.working = false;
    }
}

impl Drop for Vault {
    fn drop(&mut self) {
        // Never let key material reach the allocator unzeroed, even when the
        // vault is dropped without an explicit `lock`.
        if let Some(key) = self.key.as_mut() {
            crypto::wipe(key);
        }
    }
}

/// One step of the path from the drive root to the folder on screen. The root
/// is the first crumb, with an empty id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub id: String,
    pub name: String,
}

impl Crumb {
    pub fn root(title: &str) -> Self {
        Self {
            id: String::new(),
            name: title.to_string(),
        }
    }
}

pub struct AppState {
    pub creds: Option<Credentials>,
    pub session_path: PathBuf,
    pub login: LoginFlow,
    pub tele: Option<Client>,

    /// Drives known on this device: My Drive first, then shared ones.
    pub drives: Vec<Drive>,
    /// True while the drive list is being read from the saved config.
    pub drives_loading: bool,
    pub active_drive: Option<usize>,
    /// The owned broadcast channels offered by the "Choose your TDrive"
    /// picker, in the order the original lists them.
    pub candidates: Vec<DriveCandidate>,
    pub candidates_loading: bool,
    pub candidates_error: Option<String>,
    pub selected_candidate: Option<usize>,
    /// True while a chosen channel is being registered as the personal drive.
    pub selecting_drive: bool,
    /// True once "Create New TDrive" has been pressed and the empty channel is
    /// being confirmed, because creating one is a real change to the account.
    pub confirming_create: bool,

    pub files: Vec<RemoteFile>,
    pub files_loading: bool,
    pub selected_file: Option<usize>,
    /// The folder path from the drive root to the folder on screen.
    pub folder: Vec<Crumb>,

    pub query: String,
    /// Sort applied to the visible rows, mirroring the original's sortable
    /// Name / Date / Size headers.
    pub sort: SortState,
    /// The sidebar destination the main pane is showing.
    pub view: ViewMode,
    /// Bumped by every change that can change which rows are visible or their
    /// order. The view caches the ordered row indices against it so a render
    /// does not re-sort the folder (which the original memoises for the same
    /// reason: a folder can hold six figures of entries).
    pub rows_version: u64,
    /// Message ids whose thumbnail is currently being fetched for the gallery,
    /// so the tile is not asked to download the same image twice.
    pub gallery_downloads: std::collections::HashSet<i32>,
    /// The drive's encryption config and session key, when it has one.
    pub vault: Option<Vault>,
    /// First-time encryption setup, while its modal is open.
    pub encryption_setup: Option<EncryptionSetup>,
    /// The upload options modal, while it is open.
    pub upload_options: Option<UploadOptions>,
    /// The file a modal or a password prompt is standing in for.
    pub pending_upload: Option<PendingUpload>,
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
        // The session lives beside the original's own files rather than in the
        // platform's data directory: `datadir.Dir()` is the one place TDrive
        // keeps state, and the SQLite session file it writes is this port's own
        // format, so it neither reads nor clashes with the original's
        // `session.json`.
        let data_dir = crate::config::data_dir();
        let _ = std::fs::create_dir_all(&data_dir);
        Self {
            creds: None,
            session_path: data_dir.join("tdrive.session"),
            login: LoginFlow::default(),
            tele: None,
            drives: Vec::new(),
            drives_loading: false,
            active_drive: None,
            candidates: Vec::new(),
            candidates_loading: false,
            candidates_error: None,
            selected_candidate: None,
            selecting_drive: false,
            confirming_create: false,
            files: Vec::new(),
            files_loading: false,
            selected_file: None,
            folder: vec![Crumb::root("My Drive")],
            query: String::new(),
            sort: SortState::default(),
            view: ViewMode::default(),
            rows_version: 0,
            gallery_downloads: std::collections::HashSet::new(),
            vault: None,
            encryption_setup: None,
            upload_options: None,
            pending_upload: None,
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
    pub fn active_dialog(&self) -> Option<&Drive> {
        self.active_drive.and_then(|i| self.drives.get(i))
    }

    pub fn photos_only(&self) -> bool {
        self.view == ViewMode::Photos
    }

    pub fn trash_mode(&self) -> bool {
        self.view == ViewMode::Trash
    }

    /// The folder the main pane is showing.
    pub fn current_folder(&self) -> &str {
        self.folder
            .last()
            .map(|crumb| crumb.id.as_str())
            .unwrap_or("")
    }

    /// Something that can change the visible rows changed: drop the row cache.
    pub fn touch(&mut self) {
        self.rows_version = self.rows_version.wrapping_add(1);
    }

    pub fn set_query(&mut self, query: String) {
        if self.query != query {
            self.query = query;
            // A search spans the whole drive, so it is a file-list view.
            // Leaving Photos or the trash is what the original's
            // `runGlobalSearch` does before it draws results, rather than
            // filtering a gallery or drawing live files under a trash heading.
            if self.searching() && self.view != ViewMode::Drive {
                self.view = ViewMode::Drive;
            }
            self.touch();
        }
    }

    /// Whether the search box holds a query, which is what turns the list from
    /// one folder's contents into the drive's matches.
    pub fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn toggle_sort(&mut self, key: SortKey) {
        self.sort.toggle(key);
        self.touch();
    }

    pub fn set_view(&mut self, view: ViewMode) {
        if self.view != view {
            self.view = view;
            self.touch();
        }
    }

    /// Show the drive root, as selecting a drive or leaving a folder does.
    pub fn go_to_root(&mut self, title: &str) {
        self.folder = vec![Crumb::root(title)];
        self.selected_file = None;
        self.query.clear();
        self.touch();
    }

    pub fn enter_folder(&mut self, id: String, name: String) {
        self.folder.push(Crumb { id, name });
        self.selected_file = None;
        self.query.clear();
        self.touch();
    }

    /// Walk back to the crumb at `index`, which is what clicking a breadcrumb
    /// does.
    pub fn go_to_crumb(&mut self, index: usize) {
        if index + 1 < self.folder.len() {
            self.folder.truncate(index + 1);
            self.selected_file = None;
            self.query.clear();
            self.touch();
        }
    }

    pub fn open_drive(&mut self, index: usize) {
        self.active_drive = Some(index);
        let title = self
            .drives
            .get(index)
            .map(|drive| drive.title.clone())
            .unwrap_or_else(|| "My Drive".to_string());
        self.go_to_root(&title);
    }

    /// Install the encryption config a drive reported.
    ///
    /// Re-opening the same drive keeps whatever key is already in memory, so
    /// the password is asked for once per session rather than once per visit,
    /// matching the original's "files stay unlocked until you lock or sign
    /// out".
    pub fn set_vault(&mut self, material: Option<(VaultMaterial, String)>) {
        match material {
            // A drive with no encryption config at all: whatever key was in
            // memory belonged to the drive that was open before.
            None => {
                if let Some(vault) = self.vault.as_mut() {
                    vault.lock();
                }
                self.vault = None;
            }
            Some((material, hint)) => {
                let unchanged = self
                    .vault
                    .as_ref()
                    .is_some_and(|vault| vault.material == material);
                if !unchanged {
                    if let Some(vault) = self.vault.as_mut() {
                        vault.lock();
                    }
                    self.vault = Some(Vault::new(material, hint));
                }
            }
        }
    }

    /// Raise the unlock prompt, optionally remembering which row asked for it
    /// so the action can resume once the key is known.
    pub fn request_vault(&mut self, message_id: Option<i32>) {
        if let Some(vault) = self.vault.as_mut() {
            vault.prompt = Some(VaultPrompt { message_id });
            vault.error = None;
        }
    }

    /// Take a chosen file to the upload options modal.
    pub fn begin_upload(&mut self, path: PathBuf, count: usize) {
        self.pending_upload = Some(PendingUpload {
            path,
            encrypt: false,
        });
        self.upload_options = Some(UploadOptions {
            count,
            encrypt: false,
        });
    }

    /// Keep the modal's radio choice while it is open.
    pub fn set_upload_encrypt(&mut self, encrypt: bool) {
        if let Some(options) = self.upload_options.as_mut() {
            options.encrypt = encrypt;
        }
    }

    /// Close the upload options modal, whoever closed it.
    pub fn close_upload_options(&mut self) {
        self.upload_options = None;
    }

    /// Accept the modal's choice for the file that is waiting, and report
    /// whether it is the encrypt choice. The radio is only a preference until
    /// this runs, so a cancelled modal changes nothing.
    pub fn confirm_upload_choice(&mut self) -> bool {
        let encrypt = self
            .upload_options
            .as_ref()
            .is_some_and(|options| options.encrypt);
        self.upload_options = None;
        if let Some(pending) = self.pending_upload.as_mut() {
            pending.encrypt = encrypt;
        }
        encrypt
    }

    pub fn cancel_upload(&mut self) {
        self.close_upload_options();
        self.pending_upload = None;
    }

    /// Open the first-time encryption setup.
    pub fn open_encryption_setup(&mut self) {
        self.encryption_setup = Some(EncryptionSetup::default());
        self.error = None;
    }

    pub fn close_encryption_setup(&mut self) {
        self.encryption_setup = None;
    }

    /// Hold a key that was just created for `material`.
    ///
    /// Creating the config sends an `encfg` op and re-reads the drive, which
    /// installs the new material as a locked vault. The key the user just
    /// chose is theirs to keep, so it is put back once that material lands.
    pub fn remember_vault_key(&mut self, material: &VaultMaterial, key: [u8; 32]) {
        if let Some(vault) = self.vault.as_mut() {
            if vault.material == *material {
                vault.key = Some(key);
            }
        }
    }

    pub fn close_vault_prompt(&mut self) {
        if let Some(vault) = self.vault.as_mut() {
            vault.prompt = None;
            vault.error = None;
            vault.working = false;
        }
    }

    /// Dismiss the passphrase prompt, and with it any upload that was waiting
    /// for the key: cancelling the prompt is how the user says no to it.
    pub fn cancel_vault_prompt(&mut self) {
        let was_upload = self
            .vault
            .as_ref()
            .and_then(|vault| vault.prompt.as_ref())
            .is_some_and(|prompt| prompt.message_id.is_none());
        self.close_vault_prompt();
        if was_upload {
            self.pending_upload = None;
        }
    }

    /// Rows the list should draw for the current search box, sort and view.
    /// Trash mode has its own set, so this returns nothing for it.
    pub fn visible_files(&self) -> Vec<usize> {
        if self.trash_mode() {
            return Vec::new();
        }
        if self.searching() {
            return self.search_results();
        }
        let folder = if self.photos_only() {
            None
        } else {
            Some(self.current_folder())
        };
        self.order(&self.files, folder, self.photos_only())
    }

    /// Every entry in the drive whose name matches the search box, in the
    /// original's shape: matching folders first, then matching files, each
    /// capped the way its SQL queries cap theirs.
    ///
    /// The original searches its projection, which spans the whole channel
    /// rather than the folder on screen, so a search here does too: a result
    /// can live anywhere, and the list shows where in place of its date. The
    /// order within each half is the list's own sort rather than the fixed
    /// order the original's two queries happen to return, so the column
    /// headers keep working while a search is up.
    fn search_results(&self) -> Vec<usize> {
        let ordered = self.order(&self.files, None, false);
        let split = ordered.partition_point(|&index| self.files[index].is_folder());
        let mut results: Vec<usize> = ordered[..split]
            .iter()
            .copied()
            .take(SEARCH_LIMIT)
            .collect();
        results.extend(ordered[split..].iter().copied().take(SEARCH_LIMIT));
        results
    }

    /// Folders by object id, as parent and name, which is what a search
    /// result's path is built from. One pass over the drive, so building it
    /// along with the rows is cheap even at six figures of files.
    pub fn folder_index(&self) -> BTreeMap<String, (String, String)> {
        self.files
            .iter()
            .filter(|file| file.is_folder())
            .map(|file| {
                (
                    file.object_id.clone(),
                    (file.parent.clone(), file.name.clone()),
                )
            })
            .collect()
    }

    /// The path a search result shows in place of its date: the chain of
    /// folders above it under the drive's name, as the original's
    /// `buildFolderPathLazy` writes it ("My Drive / Photos / 2026").
    ///
    /// A result directly in the drive is "My Drive", and a missing or looping
    /// parent link ends the walk rather than running on.
    pub fn folder_path(folders: &BTreeMap<String, (String, String)>, folder_id: &str) -> String {
        let mut names: Vec<&str> = Vec::new();
        let mut visited: BTreeSet<&str> = BTreeSet::new();
        let mut current = folder_id;
        while !current.is_empty() && visited.insert(current) {
            let Some((parent, name)) = folders.get(current) else {
                break;
            };
            if !name.trim().is_empty() {
                names.push(name.as_str());
            }
            current = parent.as_str();
        }
        if names.is_empty() {
            return "My Drive".to_string();
        }
        names.reverse();
        format!("My Drive / {}", names.join(" / "))
    }

    /// Select the row for one object in whatever list is on screen now.
    ///
    /// Opening a search result lands in the folder it lives in, where the row's
    /// index is not the one the result had: this is how the file that was
    /// double-clicked stays marked after the list changes under it.
    pub fn select_object(&mut self, object_id: &str) {
        let rows = self.visible_files();
        self.selected_file = rows
            .into_iter()
            .find(|&index| self.files[index].object_id == object_id);
    }

    /// Leaving a search by opening one of its results: the folder can be
    /// anywhere in the drive, so the breadcrumb is rebuilt from the parent
    /// links rather than walked forward from the root. The result's own folder
    /// becomes the one on screen, exactly as the original's
    /// `setFolderPathAbsolute` does.
    pub fn reveal_folder(&mut self, folder_id: &str, title: &str) {
        let index = self.folder_index();
        let mut chain: Vec<(String, String)> = Vec::new();
        let mut visited: BTreeSet<String> = BTreeSet::new();
        let mut current = folder_id.to_string();
        while !current.is_empty() && visited.insert(current.clone()) {
            let Some((parent, name)) = index.get(&current) else {
                break;
            };
            chain.push((current.clone(), name.clone()));
            current = parent.clone();
        }
        chain.reverse();

        self.folder = vec![Crumb::root(title)];
        for (id, name) in chain {
            self.folder.push(Crumb { id, name });
        }
        self.query.clear();
        self.selected_file = None;
        self.touch();
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

    /// Filter and order the entries the list draws, keeping the original
    /// ordering when the search box and Photos mode are both off.
    ///
    /// `folder` is the folder whose entries are wanted, or `None` for a search,
    /// which spans the drive. Folders lead files, as they do in the original,
    /// and the whole pass is one allocation per row rather than one per
    /// comparison: a folder can hold a hundred thousand entries and this runs
    /// on every keystroke in the search box.
    fn order(&self, files: &[RemoteFile], folder: Option<&str>, media_only: bool) -> Vec<usize> {
        let query = self.query.trim().to_lowercase();
        let mut keyed: Vec<(bool, String, usize)> = files
            .iter()
            .enumerate()
            .filter(|(_, f)| folder.is_none_or(|id| f.parent == id))
            .filter(|(_, f)| !media_only || f.kind == FileKind::Image || f.kind == FileKind::Video)
            .filter(|(_, f)| query.is_empty() || f.name.to_lowercase().contains(&query))
            .map(|(i, f)| (!f.is_folder(), f.name.to_lowercase(), i))
            .collect();

        let ascending = self.sort.ascending;
        let key = self.sort.key;
        keyed.sort_by(|a, b| {
            let ordering = match key {
                SortKey::Name => a.1.cmp(&b.1),
                SortKey::Size => files[a.2].display_size().cmp(&files[b.2].display_size()),
                SortKey::Date => files[a.2].date.cmp(&files[b.2].date),
            };
            // Folders stay above files regardless of direction, the way the
            // original's sort partitions before comparing.
            match a.0.cmp(&b.0) {
                std::cmp::Ordering::Equal => {
                    if ascending {
                        ordering.then_with(|| a.1.cmp(&b.1))
                    } else {
                        ordering.reverse().then_with(|| a.1.cmp(&b.1))
                    }
                }
                other => other,
            }
        });
        keyed.into_iter().map(|(_, _, i)| i).collect()
    }

    /// The whole drive's stored bytes, which is what the sidebar footer
    /// reports.
    ///
    /// This is the original's `StorageUsed`: a channel-wide sum over the stored
    /// (ciphertext, for encrypted rows) sizes of the files that are not
    /// tombstoned, not the size of the folder on screen. It walks every file,
    /// so callers that redraw often should use the memoised copy instead.
    pub fn total_size(&self) -> i64 {
        self.files
            .iter()
            .filter(|file| !file.is_folder())
            .map(|file| file.size.max(0))
            .sum()
    }

    /// Move a live file into the trash, remembering where it came from so the
    /// row can say so. The origin is passed in rather than read from the active
    /// drive, because a drive switch while a delete is in flight must not
    /// relabel where the file came from. Returns the entry that was added.
    /// A name that is free to use under a folder, as the original's
    /// `NextFreeSiblingName` picks one: the name itself, or `name (2).ext` and
    /// so on, keeping a file's extension.
    ///
    /// The original compares canonical name keys -- NFC-normalised and
    /// case-folded -- so its unique sibling index cannot be violated by a
    /// restore. This compares names lowercased, which answers the same way for
    /// every name that differs only in case; a name that folds in some other
    /// way would be refused by the drive's own guard rather than accepted twice.
    pub fn free_sibling_name(&self, parent: &str, name: &str, is_folder: bool) -> String {
        let taken: Vec<String> = self
            .files
            .iter()
            .filter(|file| file.parent == parent)
            .map(|file| file.name.to_lowercase())
            .collect();
        let free = |candidate: &str| !taken.iter().any(|taken| *taken == candidate.to_lowercase());
        if free(name) {
            return name.to_string();
        }

        let (stem, extension) = if is_folder {
            (name, "")
        } else {
            split_extension(name)
        };
        for attempt in 2..=1000 {
            let candidate = format!("{stem} ({attempt}){extension}");
            if free(&candidate) {
                return candidate;
            }
        }
        name.to_string()
    }

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
        self.touch();
        Some(entry)
    }

    /// Populate representative data so the shell can be built, run and
    /// screenshotted without a live Telegram session (`TDRIVE_DEMO=1`).
    pub fn seed_demo(&mut self) {
        use chrono::Duration;
        use grammers_session::types::PeerAuth;

        let peer = |id: i64| PeerRef {
            id: grammers_session::types::PeerId::channel_unchecked(id),
            auth: PeerAuth::from_hash(id * 7),
        };

        let drive = |id: i64, title: &str, kind: DriveKind| Drive {
            id,
            title: title.to_string(),
            kind,
            peer_ref: peer(id),
        };

        self.drives = vec![
            drive(1, "TDrive", DriveKind::Personal),
            drive(3, "Design Team", DriveKind::Shared),
            drive(4, "Family Photos", DriveKind::Shared),
            drive(5, "Release Archives", DriveKind::Shared),
        ];
        self.status = format!("{} drives", self.drives.len());

        let now = Utc::now();
        let file = |message_id: i32,
                    name: &str,
                    size: i64,
                    kind: FileKind,
                    hours: i64,
                    parent: &str|
         -> RemoteFile {
            RemoteFile {
                message_id,
                object_id: if kind == FileKind::Folder {
                    format!("d:{message_id}")
                } else {
                    format!("f:{message_id}")
                },
                name: name.to_string(),
                size,
                kind,
                date: Some(now - Duration::hours(hours)),
                local_state: LocalState::Remote,
                parent: parent.to_string(),
                encrypted: false,
                plaintext_size: None,
                content: ContentRef::default(),
                revision: 0,
            }
        };

        let mut files = vec![
            file(99, "Design assets", 0, FileKind::Folder, 1, ""),
            file(101, "Q3 roadmap.pdf", 2_412_544, FileKind::Document, 2, ""),
            file(102, "budget-2026.xlsx", 184_320, FileKind::Sheet, 5, ""),
            file(103, "hero-shot.png", 4_981_504, FileKind::Image, 9, ""),
            file(
                104,
                "product-demo.mp4",
                268_435_456,
                FileKind::Video,
                26,
                "",
            ),
            file(105, "team-photo.jpg", 3_145_728, FileKind::Image, 30, ""),
            file(106, "voice-note.m4a", 512_000, FileKind::Audio, 33, ""),
            file(107, "assets.zip", 1_073_741_824, FileKind::Archive, 50, ""),
            file(108, "release-notes.md", 9_216, FileKind::Document, 74, ""),
            file(109, "screenshot-01.png", 745_472, FileKind::Image, 96, ""),
            file(110, "invoice-042.pdf", 122_880, FileKind::Document, 120, ""),
            file(111, "main.rs", 41_984, FileKind::Code, 200, ""),
            // Inside "Design assets" (d:99), so the breadcrumb and folder
            // navigation have somewhere to go.
            file(112, "logo.svg", 12_288, FileKind::Image, 4, "d:99"),
            file(113, "palette.png", 2_048_000, FileKind::Image, 6, "d:99"),
            file(114, "brief.docx", 64_512, FileKind::Document, 7, "d:99"),
        ];

        // One encrypted row, so the lock badge and the unlock prompt are both
        // reachable in demo mode.
        if let Some(encrypted) = files.iter_mut().find(|row| row.name == "invoice-042.pdf") {
            encrypted.encrypted = true;
            encrypted.plaintext_size = Some(118_784);
        }

        // `TDRIVE_DEMO_BULK=N` grows the drive to N files. The original's row
        // window exists for drives this size, so this is how the port's own
        // windowing and scroll cost can be looked at rather than assumed.
        if let Some(count) = std::env::var("TDRIVE_DEMO_BULK")
            .ok()
            .and_then(|raw| raw.parse::<usize>().ok())
        {
            files.reserve(count);
            for index in 0..count {
                files.push(file(
                    10_000 + index as i32,
                    &format!("bulk-{index:06}.dat"),
                    4_096 + (index as i64 % 977) * 512,
                    FileKind::Document,
                    (index % 900) as i64,
                    "",
                ));
            }
        }

        self.files = files;
        self.active_drive = Some(0);
        self.view = ViewMode::Drive;
        self.folder = vec![Crumb::root("TDrive")];
        self.demo = true;
        self.query.clear();
        self.selected_file = None;
        self.pending_delete = None;
        self.pending_purge = None;
        self.confirm_empty_trash = false;
        self.error = None;
        self.rows_version = self.rows_version.wrapping_add(1);
        self.trash = vec![
            TrashEntry {
                file: file(201, "old-draft.docx", 87_040, FileKind::Document, 30, ""),
                origin: "TDrive".to_string(),
                deleted_at: now - Duration::days(2),
            },
            TrashEntry {
                file: file(
                    202,
                    "broken-export.zip",
                    5_242_880,
                    FileKind::Archive,
                    44,
                    "",
                ),
                origin: "Release Archives".to_string(),
                deleted_at: now - Duration::days(9),
            },
            TrashEntry {
                file: file(203, "tmp-render.png", 1_310_720, FileKind::Image, 51, ""),
                origin: "Design Team".to_string(),
                deleted_at: now - Duration::days(21),
            },
        ];

        // A real vault, locked, with the password "demo". Demo only: it is
        // built at startup so the unlock prompt can be walked end to end with
        // the same code the live path uses.
        if let Ok((material, mut key)) = crypto::create_vault("demo", "the demo password is demo") {
            crypto::wipe(&mut key);
            self.vault = Some(Vault::new(
                material,
                "the demo password is demo".to_string(),
            ));
        }

        self.login.step = LoginStep::LoggedIn;

        // `TDRIVE_DEMO_PICKER=1` opens on "Choose your TDrive" instead of the
        // dashboard, so that screen can be looked at and driven without a live
        // account. The candidates are shaped like real ones: an owned channel
        // with activity, an empty one, and a channel this account merely joined
        // — which the picker can never be given in the first place, so it is
        // left out here too.
        if std::env::var("TDRIVE_DEMO_PICKER").is_ok() {
            self.login.step = LoginStep::ChooseDrive;
            self.active_drive = None;
            self.files.clear();
            self.selected_candidate = None;
            let now = Utc::now().timestamp();
            self.candidates = vec![
                DriveCandidate {
                    id: 4_309_652_849,
                    title: "TDrive".to_string(),
                    created: now - 86_400 * 96,
                    has_activity: true,
                    recommended: true,
                    peer_ref: peer(4_309_652_849),
                },
                DriveCandidate {
                    id: 4_281_004_411,
                    title: "TDrive backup".to_string(),
                    created: now - 86_400 * 61,
                    has_activity: false,
                    recommended: false,
                    peer_ref: peer(4_281_004_411),
                },
                DriveCandidate {
                    id: 4_190_882_003,
                    title: "Release archives".to_string(),
                    created: now - 86_400 * 210,
                    has_activity: true,
                    recommended: false,
                    peer_ref: peer(4_190_882_003),
                },
            ];
            self.status = "3 channels found".to_string();
        }
    }
}

/// Split a name into its stem and extension the way the original's
/// `splitLegacyFileExtension` does: on the last dot, which is ignored when it
/// leads the name or when what follows is longer than 32 bytes.
fn split_extension(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(dot) if dot > 0 && name.len() - dot <= 32 => (&name[..dot], &name[dot..]),
        _ => (name, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The upload flow's bookkeeping: a chosen file waits behind the options
    /// modal, the choice is kept per batch, and leaving the flow drops it.
    #[test]
    fn upload_options_track_the_batch_and_start_plain() {
        let mut state = AppState::default();
        state.begin_upload(PathBuf::from("/tmp/report.pdf"), 1);
        assert_eq!(
            state.upload_options,
            Some(UploadOptions {
                count: 1,
                encrypt: false
            })
        );
        assert_eq!(
            state.pending_upload.as_ref().map(|p| p.encrypt),
            Some(false)
        );

        state.set_upload_encrypt(true);
        assert!(state.upload_options.is_some());
        // Only accepting the choice passes it on to the file; a cancelled
        // modal would leave the pending upload plain.
        assert!(state.confirm_upload_choice());
        assert!(state.upload_options.is_none());
        assert_eq!(state.pending_upload.as_ref().map(|p| p.encrypt), Some(true));

        state.cancel_upload();
        assert!(state.pending_upload.is_none());
    }

    /// Cancelling the passphrase prompt drops the upload that was waiting for
    /// it, but leaves a download (a prompt that names a row) alone.
    #[test]
    fn cancelling_a_vault_prompt_drops_only_a_waiting_upload() {
        let material = VaultMaterial {
            salt: vec![0; 16],
            params_json: "{}".to_string(),
            wrapped_master_key: vec![0; 72],
            key_check: vec![0; 59],
        };
        let mut state = AppState::default();
        state.set_vault(Some((material.clone(), "hint".to_string())));
        state.pending_upload = Some(PendingUpload {
            path: PathBuf::from("/tmp/report.pdf"),
            encrypt: true,
        });
        state.request_vault(None);
        state.cancel_vault_prompt();
        assert!(state.pending_upload.is_none());

        state.pending_upload = Some(PendingUpload {
            path: PathBuf::from("/tmp/report.pdf"),
            encrypt: true,
        });
        state.request_vault(Some(42));
        state.cancel_vault_prompt();
        assert!(state.pending_upload.is_some());
    }

    fn file(message_id: i32, name: &str, size: i64, parent: &str) -> RemoteFile {
        RemoteFile {
            message_id,
            object_id: format!("f:{message_id}"),
            name: name.to_string(),
            size,
            kind: FileKind::Document,
            date: None,
            local_state: LocalState::Remote,
            parent: parent.to_string(),
            encrypted: false,
            plaintext_size: None,
            content: ContentRef::default(),
            revision: 0,
        }
    }

    fn folder(message_id: i32, name: &str, parent: &str) -> RemoteFile {
        RemoteFile {
            object_id: format!("d:{message_id}"),
            kind: FileKind::Folder,
            ..file(message_id, name, 0, parent)
        }
    }

    /// The footer reports what the drive stores, which the original computes by
    /// summing `files.size` over the whole channel: the ciphertext length for
    /// an encrypted row, and including files that are not in the folder on
    /// screen.
    #[test]
    fn storage_usage_counts_stored_bytes_for_the_whole_drive() {
        let mut state = AppState {
            files: vec![
                file(1, "root.bin", 100, ""),
                folder(2, "nested", ""),
                file(3, "inside.bin", 2_000, "d:2"),
                RemoteFile {
                    encrypted: true,
                    plaintext_size: Some(40),
                    ..file(4, "locked.bin", 64, "")
                },
            ],
            ..AppState::default()
        };
        state.folder = vec![Crumb::root("TDrive")];

        // 100 + 2000 + 64, the stored total; the 40-byte plaintext size and the
        // 64-byte folder row do not stand in for it.
        assert_eq!(state.total_size(), 2_164);

        state.enter_folder("d:2".to_string(), "nested".to_string());
        assert_eq!(state.total_size(), 2_164, "the footer is drive-wide");
    }

    /// A row shows the plaintext size when the metadata carries one, which is
    /// separate from what the drive stores.
    #[test]
    fn a_locked_row_shows_its_plaintext_size() {
        let locked = RemoteFile {
            encrypted: true,
            plaintext_size: Some(40),
            ..file(1, "locked.bin", 64, "")
        };
        assert_eq!(locked.display_size(), 40);
        assert_eq!(file(2, "plain.bin", 64, "").display_size(), 64);
    }

    #[test]
    fn the_root_lists_folders_before_files() {
        let state = AppState {
            files: vec![
                file(1, "b.txt", 1, ""),
                folder(2, "a", ""),
                file(3, "c.txt", 1, ""),
            ],
            ..AppState::default()
        };
        assert_eq!(state.visible_files(), vec![1, 0, 2]);
    }

    #[test]
    fn a_search_filters_by_name_and_a_sort_direction_flips() {
        let mut state = AppState {
            files: vec![
                file(1, "alpha.txt", 3, ""),
                file(2, "beta.txt", 1, ""),
                file(3, "gamma.txt", 2, ""),
            ],
            ..AppState::default()
        };
        assert_eq!(state.visible_files(), vec![0, 1, 2]);

        state.set_query("a.txt".to_string());
        assert_eq!(state.visible_files(), vec![0, 1, 2], "alpha and beta match");

        state.set_query("gamma".to_string());
        assert_eq!(state.visible_files(), vec![2]);

        state.set_query(String::new());
        state.toggle_sort(SortKey::Size);
        assert_eq!(state.visible_files(), vec![1, 2, 0], "ascending by size");
        state.toggle_sort(SortKey::Size);
        assert_eq!(state.visible_files(), vec![0, 2, 1], "descending by size");
    }

    /// A search is not a filter of the folder on screen: it covers the drive,
    /// because that is what the original's projection query covers. The folder
    /// on the breadcrumb does not restrict it.
    #[test]
    fn a_search_covers_the_whole_drive_not_only_the_open_folder() {
        let mut state = AppState {
            files: vec![
                folder(1, "Photos", ""),
                folder(2, "2026", "d:1"),
                file(3, "holiday.jpg", 10, "d:2"),
                file(4, "notes.txt", 10, ""),
            ],
            ..AppState::default()
        };
        // Stand inside the folder and search from there.
        state.enter_folder("d:1".to_string(), "Photos".to_string());
        state.enter_folder("d:2".to_string(), "2026".to_string());
        assert_eq!(state.visible_files(), vec![2], "the folder's own entry");

        state.set_query("holiday".to_string());
        assert_eq!(
            state.visible_files(),
            vec![2],
            "a search finds the file whether or not it is on screen"
        );

        // And one that lives nowhere near the open folder is found just as well.
        state.set_query("notes".to_string());
        assert_eq!(state.visible_files(), vec![3]);
    }

    /// Folders lead files in the results, as they do in the original's two
    /// queries, and each half stops at the limit that query clamps to.
    #[test]
    fn search_results_lead_with_folders_and_are_capped_per_kind() {
        let mut files: Vec<RemoteFile> = Vec::new();
        for index in 0..(SEARCH_LIMIT + 5) {
            files.push(file(
                index as i32 + 1,
                &format!("match-{index:04}.txt"),
                1,
                "",
            ));
        }
        files.push(folder(9001, "matches", ""));
        let mut state = AppState {
            files,
            ..AppState::default()
        };

        state.set_query("match".to_string());
        let rows = state.visible_files();
        assert_eq!(rows.len(), SEARCH_LIMIT + 1, "one folder, then the cap");
        assert!(
            state.files[rows[0]].is_folder(),
            "the folder leads the files"
        );
        assert!(
            rows[1..]
                .iter()
                .all(|&index| !state.files[index].is_folder()),
            "and the files follow it"
        );
    }

    /// The path a result shows is the chain of folders above it, as the
    /// original's `buildFolderPathLazy` writes it, and a result in the drive
    /// itself is just the drive.
    #[test]
    fn a_search_result_reports_where_it_lives() {
        let state = AppState {
            files: vec![
                folder(1, "Photos", ""),
                folder(2, "2026", "d:1"),
                file(3, "holiday.jpg", 10, "d:2"),
                file(4, "notes.txt", 10, ""),
                // A folder whose parent link points at itself must not loop.
                folder(5, "loop", "d:5"),
            ],
            ..AppState::default()
        };
        let folders = state.folder_index();

        assert_eq!(
            AppState::folder_path(&folders, "d:2"),
            "My Drive / Photos / 2026"
        );
        assert_eq!(AppState::folder_path(&folders, ""), "My Drive");
        assert_eq!(AppState::folder_path(&folders, "d:1"), "My Drive / Photos");
        assert_eq!(
            AppState::folder_path(&folders, "d:5"),
            "My Drive / loop",
            "a folder that names itself as its parent ends the walk"
        );
        assert_eq!(
            AppState::folder_path(&folders, "d:404"),
            "My Drive",
            "a missing folder is not a path"
        );
    }

    /// Opening a result goes where it lives: the breadcrumb is rebuilt from the
    /// parent links rather than walked forward from the root, the search is
    /// over, and the file that was opened is the selected row.
    #[test]
    fn opening_a_result_rebuilds_the_breadcrumb_and_ends_the_search() {
        let mut state = AppState {
            files: vec![
                folder(1, "Photos", ""),
                folder(2, "2026", "d:1"),
                file(3, "holiday.jpg", 10, "d:2"),
                file(4, "notes.txt", 10, ""),
            ],
            ..AppState::default()
        };
        state.go_to_root("TDrive");
        state.set_query("holiday".to_string());
        assert_eq!(state.visible_files(), vec![2]);

        // Double-clicking the result's folder: the crumb is rebuilt, the query
        // is gone and the folder's own entries are on screen.
        state.reveal_folder("d:2", "TDrive");
        assert_eq!(
            state
                .folder
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            ["TDrive", "Photos", "2026"]
        );
        assert!(!state.searching());
        assert_eq!(state.visible_files(), vec![2], "the folder's own file");

        // Double-clicking a file: the row it names is selected once the list
        // has changed under it.
        state.set_query("notes".to_string());
        state.reveal_folder("", "TDrive");
        state.select_object("f:4");
        assert_eq!(
            state.selected_file,
            Some(3),
            "the selected row is the file, not its position in the list"
        );
        assert_eq!(
            state.visible_files(),
            vec![0, 3],
            "the folder, then the file"
        );
    }

    /// A search is a file-list view, so it leaves Photos mode rather than
    /// filtering the gallery, and the column reports the location.
    #[test]
    fn a_search_leaves_the_photos_view() {
        let mut state = AppState {
            files: vec![file(1, "holiday.jpg", 10, "")],
            ..AppState::default()
        };
        state.set_view(ViewMode::Photos);
        assert!(state.photos_only());

        state.set_query("holiday".to_string());
        assert_eq!(state.view, ViewMode::Drive);
        assert!(!state.photos_only());
        assert_eq!(state.visible_files(), vec![0]);
    }

    /// A restore that lands on a name someone else has takes the next free one,
    /// and a file keeps its extension when it moves over, as the original's
    /// `NextFreeSiblingName` writes it.
    #[test]
    fn a_taken_name_moves_over_for_a_restore() {
        let state = AppState {
            files: vec![
                file(1, "Report.PDF", 10, ""),
                file(2, "Notes.txt", 10, "d:1"),
                file(3, "Notes (2).txt", 10, "d:1"),
                folder(4, "Docs", "d:1"),
            ],
            ..AppState::default()
        };

        // A free name is used as it is, whatever case it is in.
        assert_eq!(
            state.free_sibling_name("", "Holiday.jpg", false),
            "Holiday.jpg"
        );
        assert_eq!(
            state.free_sibling_name("d:1", "Notes.txt", false),
            "Notes (3).txt",
            "the number is the first that is free, not the second"
        );
        assert_eq!(
            state.free_sibling_name("", "report.pdf", false),
            "report (2).pdf",
            "a name taken in another case is still taken"
        );
        assert_eq!(
            state.free_sibling_name("d:1", "Docs", true),
            "Docs (2)",
            "a folder keeps its whole name"
        );

        // The extension is the last dot, and a name that is all dots is not
        // split at all, as the original's splitter reads it. Both need the name
        // to be taken first, or there is nothing to move over.
        let long = format!("archive.{}", "e".repeat(33));
        let mut state = state;
        state.files.push(file(5, ".gitignore", 10, "d:1"));
        state.files.push(file(6, &long, 10, "d:1"));
        assert_eq!(
            state.free_sibling_name("d:1", ".gitignore", false),
            ".gitignore (2)",
            "a leading dot is part of the stem, not an extension"
        );
        assert_eq!(
            state.free_sibling_name("d:1", &long, false),
            format!("{long} (2)"),
            "a suffix too long to be an extension stays part of the stem"
        );
    }

    /// The key is kept when the same drive is reopened and dropped when a drive
    /// with no encryption config replaces it, so a stale key cannot decrypt the
    /// wrong drive's files.
    #[test]
    fn a_drive_switch_forgets_the_key_of_the_previous_drive() {
        let (material, key) = crypto::create_vault("demo", "hint").expect("vault");
        let mut state = AppState::default();

        state.set_vault(Some((material.clone(), "hint".to_string())));
        state.vault.as_mut().expect("vault").key = Some(key);
        assert!(state.vault.as_ref().expect("vault").unlocked());

        // Reopening the same drive keeps the key in memory.
        state.set_vault(Some((material.clone(), "hint".to_string())));
        assert!(
            state.vault.as_ref().expect("vault").unlocked(),
            "the same drive keeps its key"
        );

        // A drive without encryption config drops it, so nothing is left that
        // could decrypt this drive's files while another one is open.
        state.set_vault(None);
        assert!(state.vault.is_none());

        // A different drive's config starts locked rather than inheriting the
        // key that was in memory.
        state.set_vault(Some((material, "hint".to_string())));
        assert!(!state.vault.as_ref().expect("vault").unlocked());
    }

    #[test]
    fn the_unlock_prompt_remembers_the_row_that_asked_and_clears_on_close() {
        let (material, _key) = crypto::create_vault("demo", "hint").expect("vault");
        let mut state = AppState::default();
        state.set_vault(Some((material, "hint".to_string())));

        state.request_vault(Some(77));
        let prompt = state
            .vault
            .as_ref()
            .and_then(|vault| vault.prompt.as_ref())
            .expect("prompt");
        assert_eq!(prompt.message_id, Some(77));

        state.close_vault_prompt();
        assert!(state.vault.as_ref().expect("vault").prompt.is_none());
    }
}
