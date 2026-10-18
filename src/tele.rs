//! Telegram transport.
//!
//! Two things here are load-bearing for fidelity with the original:
//!
//! * A drive is a **broadcast channel the account created**. Private chats,
//!   groups, Saved Messages and channels the user merely joined are never
//!   drives, and discovery has to say so before the picker is drawn.
//! * A drive's contents are the messages carrying a `TDX1` operation, not
//!   every message with a document in it. The operation is what names a file
//!   and says where it belongs, so a drive can hold ordinary chatter without
//!   that chatter turning into files.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Utc};
use grammers_client::media::Media;
use grammers_client::{Client, SignInError};
use grammers_mtsender::SenderPool;
use grammers_session::Session as _;
use grammers_session::storages::SqliteSession;
use grammers_session::types::{PeerAuth, PeerId, PeerRef};
use grammers_tl_types as tl;
use tokio::runtime::Runtime;

use crate::crypto::{self, VaultMaterial};
use crate::models::{ContentRef, Credentials, FileKind, LocalState, RemoteFile};
use crate::ops::{self, Op};

pub static RT: std::sync::LazyLock<Runtime> =
    std::sync::LazyLock::new(|| Runtime::new().expect("tokio runtime"));

pub async fn async_connect(creds: &Credentials, session_path: PathBuf) -> Result<Client, String> {
    let session = Arc::new(
        SqliteSession::open(session_path)
            .await
            .map_err(|e| e.to_string())?,
    );
    // A device that is already signed in to the original is signed in to the
    // same account: take that authorization into this port's session instead of
    // asking for a login code the user has already answered once. A key this
    // port already holds always wins, so once it has signed in for itself the
    // original's file stops mattering.
    if !has_auth_key(&session) {
        if let Some(imported) = crate::config::ImportedSession::load() {
            if let Err(message) = async_import_session(&session, &imported).await {
                // A failed import is not fatal: the sign-in screens are still
                // there, and that is the path a fresh install takes anyway.
                tracing::warn!("could not import the existing TDrive session: {message}");
            }
        }
    }
    let SenderPool { runner, handle, .. } = SenderPool::new(Arc::clone(&session), creds.api_id);
    let client = Client::new(handle);
    tokio::spawn(runner.run());
    Ok(client)
}

/// Whether this session already holds a key for the account.
fn has_auth_key(session: &SqliteSession) -> bool {
    let Ok(home) = session.home_dc_id() else {
        return false;
    };
    matches!(session.dc_option(home), Ok(Some(option)) if option.auth_key.is_some())
}

/// Take an authorization the original application already holds.
///
/// The key is only valid for the datacenter it was made with, so it is stored
/// on that datacenter's option and the rest keep their well-known addresses
/// without a key: connecting elsewhere signs in again rather than pretending
/// to.
pub async fn async_import_session(
    session: &SqliteSession,
    imported: &crate::config::ImportedSession,
) -> Result<(), String> {
    let mut data = grammers_session::SessionData::default();
    data.home_dc = imported.dc_id;
    let Some(option) = data.dc_options.get_mut(&imported.dc_id) else {
        return Err(format!("no address for datacenter {}", imported.dc_id));
    };
    option.auth_key = Some(imported.auth_key);
    data.import_to(session).await.map_err(|e| e.to_string())
}

pub async fn async_is_authorized(client: &Client) -> bool {
    client.is_authorized().await.unwrap_or(false)
}

pub async fn async_request_code(
    client: &Client,
    phone: &str,
    api_hash: &str,
) -> Result<grammers_client::client::LoginToken, String> {
    client
        .request_login_code(phone, api_hash)
        .await
        .map_err(|e| e.to_string())
}

pub async fn async_sign_in(
    client: &Client,
    token: grammers_client::client::LoginToken,
    code: &str,
) -> Result<Option<grammers_client::client::PasswordToken>, String> {
    match client.sign_in(&token, code).await {
        Ok(_) => Ok(None),
        Err(SignInError::PasswordRequired(pt)) => Ok(Some(pt)),
        Err(SignInError::InvalidPassword(pt)) => Ok(Some(pt)),
        Err(e) => Err(e.to_string()),
    }
}

pub async fn async_check_password(
    client: &Client,
    token: grammers_client::client::PasswordToken,
    password: &str,
) -> Result<grammers_client::peer::User, String> {
    client
        .check_password(token, password)
        .await
        .map_err(|e| e.to_string())
}

// --- Drives -----------------------------------------------------------------

/// A broadcast channel the account created: the only shape a drive can have.
#[derive(Clone)]
pub struct OwnedChannel {
    pub id: i64,
    pub access_hash: i64,
    pub title: String,
    /// Channel creation date, unix seconds.
    pub created: i64,
    /// Whether the channel already holds messages.
    pub has_activity: bool,
    pub peer_ref: PeerRef,
}

impl OwnedChannel {
    fn peer_ref(id: i64, access_hash: i64) -> PeerRef {
        crate::tele::peer_ref(id, access_hash)
    }
}

/// Whether a channel is a drive: one this account created as a broadcast
/// channel and has not left.
///
/// This is the whole of the "which chat is my TDrive" rule, and it is a
/// transport-level guarantee rather than a display filter: a private chat, a
/// group and a channel the account merely joined are all rejected here, so a
/// conversation with a relative can never reach the drive list. It mirrors the
/// original's `collectOwnedBroadcastChannels` condition exactly:
/// `!channel.Creator || !channel.Broadcast || channel.Megagroup || channel.Left`.
fn is_drive_channel(creator: bool, broadcast: bool, megagroup: bool, left: bool) -> bool {
    creator && broadcast && !megagroup && !left
}

/// A channel peer reference from the id and access hash a drive remembers.
///
/// A saved drive only carries those two numbers, so this is how a drive read
/// from `config.json` becomes addressable again on the next run.
pub fn peer_ref(channel_id: i64, access_hash: i64) -> PeerRef {
    PeerRef {
        id: PeerId::channel_unchecked(channel_id),
        auth: PeerAuth::from_hash(access_hash),
    }
}

/// Every broadcast channel the account created, newest dialog first.
///
/// Mirrors the original's `collectOwnedBroadcastChannels`: a user peer, a
/// group, a megagroup, a channel the account only subscribes to, and a channel
/// it has left are all skipped, so the picker can never offer a chat that is
/// not a drive.
pub async fn async_list_owned_channels(client: &Client) -> Result<Vec<OwnedChannel>, String> {
    use grammers_client::peer::Peer;

    let mut dialogs = client.iter_dialogs();
    let mut out: Vec<OwnedChannel> = Vec::new();
    while let Some(dialog) = dialogs.next().await.map_err(|e| e.to_string())? {
        let Peer::Channel(channel) = dialog.peer() else {
            continue;
        };
        let raw = &channel.raw;
        if !is_drive_channel(raw.creator, raw.broadcast, raw.megagroup, raw.left) {
            continue;
        }
        let Some(access_hash) = raw.access_hash else {
            continue;
        };
        if out.iter().any(|existing| existing.id == raw.id) {
            continue;
        }
        let has_activity =
            matches!(&dialog.raw, tl::enums::Dialog::Dialog(row) if row.top_message > 0);
        out.push(OwnedChannel {
            id: raw.id,
            access_hash,
            title: raw.title.trim().to_string(),
            created: raw.date as i64,
            has_activity,
            peer_ref: OwnedChannel::peer_ref(raw.id, access_hash),
        });
    }
    Ok(out)
}

/// Create the empty broadcast channel a new personal drive lives in, exactly as
/// the original does when the account has no drive to recover.
pub async fn async_create_personal_channel(
    client: &Client,
    title: &str,
    about: &str,
) -> Result<OwnedChannel, String> {
    let request = tl::functions::channels::CreateChannel {
        broadcast: true,
        megagroup: false,
        title: title.to_string(),
        about: about.to_string(),
        geo_point: None,
        address: None,
        for_import: false,
        forum: false,
        ttl_period: None,
    };
    let updates = client.invoke(&request).await.map_err(|e| e.to_string())?;
    let chats = match &updates {
        tl::enums::Updates::Updates(updates) => &updates.chats,
        tl::enums::Updates::Combined(updates) => &updates.chats,
        _ => return Err("Telegram did not return the new channel".to_string()),
    };
    let channel = chats
        .iter()
        .find_map(|chat| match chat {
            tl::enums::Chat::Channel(channel) => Some(channel),
            _ => None,
        })
        .ok_or_else(|| "Telegram did not return the new channel".to_string())?;
    let access_hash = channel.access_hash.unwrap_or_default();
    Ok(OwnedChannel {
        id: channel.id,
        access_hash,
        title: channel.title.trim().to_string(),
        created: channel.date as i64,
        has_activity: false,
        peer_ref: OwnedChannel::peer_ref(channel.id, access_hash),
    })
}

// --- A drive's contents -----------------------------------------------------

/// What a drive holds, read from its operation messages.
pub struct DriveContents {
    pub files: Vec<RemoteFile>,
    /// The objects the drive's own trash ops took off the live tree, newest
    /// first. One entry per trash op, not per descendant: a trashed folder is
    /// one entry that carries its subtree with it.
    pub trash: Vec<TrashedObject>,
    /// The drive's encryption config, when it has one.
    pub vault: Option<(VaultMaterial, String)>,
}

/// A live object a trash op removed, with what the trash view needs to show it.
pub struct TrashedObject {
    pub file: RemoteFile,
    /// Unix seconds the object was deleted.
    pub deleted_at: i64,
}

/// One message that carried a drive operation.
struct OpMessage {
    message_id: i32,
    op: Op,
    kind: Option<FileKind>,
    size: Option<i64>,
    date: Option<DateTime<Utc>>,
}

/// One part of a multipart group, as the replay saw it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PartMessage {
    index: u32,
    message_id: i32,
    kind: Option<FileKind>,
    size: Option<i64>,
}

/// The objects one trash op took down: the object it named plus the descendants
/// that were live at the time.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct TrashGroup {
    deleted_at: i64,
    /// The named object first, then its descendants.
    members: Vec<RemoteFile>,
}

/// Everything hanging under `root`, `root` included, by the parent links the
/// objects themselves carry.
fn subtree_of(objects: &BTreeMap<String, RemoteFile>, root: &str) -> Vec<String> {
    let mut out = vec![root.to_string()];
    let mut seen: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut index = 0;
    while index < out.len() {
        let current = out[index].clone();
        index += 1;
        for (id, object) in objects {
            if object.parent == current && seen.insert(id.clone()) {
                out.push(id.clone());
            }
        }
    }
    out
}

/// Take a live subtree out of the object map, `root` first.
fn take_subtree(objects: &mut BTreeMap<String, RemoteFile>, root: &str) -> Vec<RemoteFile> {
    subtree_of(objects, root)
        .into_iter()
        .filter_map(|id| objects.remove(&id))
        .collect()
}

/// The members of a trash group at or under `root`, by the same parent walk.
fn group_subtree(members: &[RemoteFile], root: &str) -> std::collections::BTreeSet<String> {
    let mut ids: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::from([root.to_string()]);
    let mut changed = true;
    while changed {
        changed = false;
        for member in members {
            if ids.contains(&member.parent) && ids.insert(member.object_id.clone()) {
                changed = true;
            }
        }
    }
    ids
}

/// Where a committed file's bytes live.
///
/// A single-message commit names its body message; a multipart one names a
/// group whose parts the replay resolves to message ids once it has seen the
/// whole history.
fn content_of(
    msg_id: i64,
    uuid: &str,
    part_count: u32,
    multipart: &mut BTreeMap<String, String>,
    object_id: &str,
) -> ContentRef {
    if msg_id > 0 {
        ContentRef::Message(msg_id as i32)
    } else if !uuid.is_empty() && part_count > 0 {
        multipart.insert(object_id.to_string(), uuid.to_string());
        ContentRef::Parts(Vec::new())
    } else {
        // A parsed op always carries one of the two shapes; this only covers a
        // hand-built op, and reading the op message itself is the safe default.
        ContentRef::OpMessage
    }
}

/// A file row from an op that names its content inline (`t=f`, `t=manifest`).
fn row_from_op(
    object_id: String,
    message_id: i32,
    parent: String,
    name: String,
    size: Option<i64>,
    encrypted: bool,
    plaintext_size: Option<i64>,
    content: ContentRef,
) -> RemoteFile {
    RemoteFile {
        message_id,
        object_id,
        name,
        size: size.unwrap_or_default(),
        kind: FileKind::Other,
        date: None,
        local_state: LocalState::Remote,
        parent,
        encrypted,
        plaintext_size,
        content,
        // Every object starts at revision 1: the drive's tables default to it,
        // and it is the revision a delete of an object never since changed
        // names against.
        revision: 1,
    }
}

/// Everything a replay needs to carry on later: the live tree, the trash
/// groups, the multipart parts and the drive's encryption config.
///
/// This is what a snapshot stores, so a launch can start from where the last
/// one stopped instead of reading the whole history again.
#[derive(Default, serde::Serialize, serde::Deserialize)]
pub(crate) struct ReplayState {
    /// Live objects, by object id.
    pub objects: BTreeMap<String, RemoteFile>,
    /// Trash groups, keyed by the object the trash op named.
    pub trashed: BTreeMap<String, TrashGroup>,
    /// Multipart parts by group id, including groups no live object names yet.
    pub parts: BTreeMap<String, Vec<PartMessage>>,
    /// Objects whose bytes are a multipart group, by object id, by group id.
    pub multipart: BTreeMap<String, String>,
    /// The drive's encryption config, newest wins.
    pub vault: Option<(VaultMaterial, String)>,
    /// The media of every message the replay has seen, by message id: a
    /// committed file's kind and size come from the message that holds its
    /// bytes, which may sit outside the range a resumed read covers.
    pub media: BTreeMap<i32, (Option<FileKind>, Option<i64>)>,
}

/// The replay's working state: the live tree, the trash, and the parts of
/// multipart groups as they arrive.
#[derive(Default)]
struct Replay {
    /// Live objects, by object id.
    objects: BTreeMap<String, RemoteFile>,
    /// Objects a trash op took down, keyed by the object it named. A restore
    /// brings back exactly the members of the group it names, so an object that
    /// was trashed separately, before its parent, keeps its own entry.
    trashed: BTreeMap<String, TrashGroup>,
    /// The parts of each multipart group, as the replay saw them.
    parts: BTreeMap<String, Vec<PartMessage>>,
    /// Live objects whose bytes are a multipart group, by group id. They are
    /// resolved once the whole history has been read, because a group's parts
    /// may arrive on either side of the op that names it.
    multipart: BTreeMap<String, String>,
    /// The drive's encryption config, newest wins.
    vault: Option<(VaultMaterial, String)>,
    /// Whether to count operations by type for the `TDRIVE_TRACE_OPS` summary.
    trace: bool,
    /// Operations applied, by wire type. Only filled while `trace` is set.
    counts: BTreeMap<String, usize>,
}

impl Replay {
    /// Carry on from a state a previous replay produced.
    ///
    /// The second half of a load: the objects, trash and parts that were
    /// already projected, so an operation arriving now lands on the tree the
    /// last launch built rather than on an empty one.
    fn from_state(state: ReplayState, trace: bool) -> Self {
        Replay {
            objects: state.objects,
            trashed: state.trashed,
            parts: state.parts,
            multipart: state.multipart,
            vault: state.vault,
            trace,
            counts: BTreeMap::new(),
        }
    }

    /// Apply one operation. Operations are replayed oldest first, so a rename
    /// or a move that arrived after an upload lands after it.
    fn apply(
        &mut self,
        entry: OpMessage,
        media_by_id: &BTreeMap<i32, (Option<FileKind>, Option<i64>)>,
    ) {
        let id = entry.message_id;
        if self.trace {
            *self
                .counts
                .entry(entry.op.type_name().to_string())
                .or_default() += 1;
        }
        match entry.op {
            Op::File {
                parent,
                name,
                size,
                uploaded_at,
                encrypted,
                plaintext_size,
                ..
            } => {
                let mut row = row_from_op(
                    format!("f:{id}"),
                    id,
                    parent,
                    name,
                    size.or(entry.size),
                    encrypted,
                    plaintext_size,
                    ContentRef::OpMessage,
                );
                row.kind = entry.kind.unwrap_or(FileKind::Other);
                row.date = uploaded_at
                    .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
                    .or(entry.date);
                self.objects.insert(row.object_id.clone(), row);
            }
            Op::Manifest {
                uuid,
                parent,
                name,
                part_count,
                size,
                uploaded_at,
                encrypted,
                plaintext_size,
                ..
            } => {
                let object_id = format!("f:{id}");
                let mut row = row_from_op(
                    object_id.clone(),
                    id,
                    parent,
                    name,
                    size,
                    encrypted,
                    plaintext_size,
                    content_of(0, &uuid, part_count, &mut self.multipart, &object_id),
                );
                row.date = uploaded_at
                    .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
                    .or(entry.date);
                self.objects.insert(object_id, row);
            }
            Op::FileCommit {
                parent,
                name,
                content_msg_id,
                upload_uuid,
                part_count,
                size,
                uploaded_at,
                encrypted,
                plaintext_size,
                ..
            } => {
                let object_id = format!("f:{id}");
                // The bytes are on the body message, so its kind and size stand
                // in when the commit did not record them.
                let body = media_by_id.get(&(content_msg_id as i32)).cloned();
                let mut row = row_from_op(
                    object_id.clone(),
                    id,
                    parent,
                    name,
                    size.or(body.and_then(|(_, size)| size)),
                    encrypted,
                    plaintext_size,
                    content_of(
                        content_msg_id,
                        &upload_uuid,
                        part_count,
                        &mut self.multipart,
                        &object_id,
                    ),
                );
                row.kind = body.and_then(|(kind, _)| kind).unwrap_or(FileKind::Other);
                row.date = uploaded_at
                    .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
                    .or(entry.date);
                self.objects.insert(object_id, row);
            }
            Op::FolderCommit {
                obj, parent, name, ..
            } => {
                let mut row = row_from_op(
                    obj.clone(),
                    id,
                    parent,
                    name,
                    Some(0),
                    false,
                    None,
                    ContentRef::OpMessage,
                );
                row.kind = FileKind::Folder;
                row.date = entry.date;
                self.objects.insert(obj, row);
            }
            Op::Folder { obj, parent, name } => {
                let mut row = row_from_op(
                    obj.clone(),
                    id,
                    parent,
                    name,
                    Some(0),
                    false,
                    None,
                    ContentRef::OpMessage,
                );
                row.kind = FileKind::Folder;
                row.date = entry.date;
                self.objects.insert(obj, row);
            }
            Op::FileReplace {
                obj,
                content_msg_id,
                upload_uuid,
                part_count,
                size,
                uploaded_at,
                encrypted,
                plaintext_size,
                ..
            } => {
                let body = media_by_id.get(&(content_msg_id as i32)).cloned();
                if let Some(file) = self.objects.get_mut(&obj) {
                    if let Some(size) = size.or(body.and_then(|(_, size)| size)) {
                        file.size = size;
                    }
                    if let Some(kind) = body.and_then(|(kind, _)| kind) {
                        file.kind = kind;
                    }
                    file.encrypted = encrypted;
                    file.plaintext_size = plaintext_size;
                    file.content = content_of(
                        content_msg_id,
                        &upload_uuid,
                        part_count,
                        &mut self.multipart,
                        &obj,
                    );
                    if let Some(date) = uploaded_at.and_then(|s| DateTime::from_timestamp(s, 0)) {
                        file.date = Some(date);
                    }
                    file.revision += 1;
                }
            }
            Op::Meta {
                obj,
                parent,
                name,
                size,
                uploaded_at,
                encrypted,
                plaintext_size,
                ..
            } => {
                if let Some(file) = self.objects.get_mut(&obj) {
                    file.name = name;
                    file.parent = parent;
                    if let Some(size) = size {
                        file.size = size;
                    }
                    file.encrypted = encrypted;
                    file.plaintext_size = plaintext_size;
                    if let Some(date) = uploaded_at.and_then(|s| DateTime::from_timestamp(s, 0)) {
                        file.date = Some(date);
                    }
                }
            }
            Op::Rename { obj, name } => {
                if let Some(file) = self.objects.get_mut(&obj) {
                    file.name = name;
                    // A rename is a revision, like the drive's own rows: the
                    // next operation to name this object has to name the new one.
                    file.revision += 1;
                }
            }
            Op::Move { obj, parent } => {
                if let Some(file) = self.objects.get_mut(&obj) {
                    file.parent = parent;
                    file.revision += 1;
                }
            }
            Op::Tomb { obj } | Op::Rmdir { obj } => {
                self.objects.remove(&obj);
            }
            Op::Relocate {
                obj,
                parent,
                name,
                overwrite,
                destination_obj,
                deleted_at,
                ..
            } => {
                if let Some(file) = self.objects.get_mut(&obj) {
                    file.parent = parent;
                    file.name = name;
                    file.revision += 1;
                }
                // An overwriting move is also the deletion of what it landed
                // on: the victim goes to the trash in the same operation.
                if overwrite && !destination_obj.is_empty() {
                    let members = take_subtree(&mut self.objects, &destination_obj);
                    if !members.is_empty() {
                        self.trashed.insert(
                            destination_obj,
                            TrashGroup {
                                deleted_at,
                                members,
                            },
                        );
                    }
                }
            }
            Op::Trash {
                obj, deleted_at, ..
            } => {
                let members = take_subtree(&mut self.objects, &obj);
                if !members.is_empty() {
                    // Hiding an object advances every row it hides, which is the
                    // revision the restore or the purge that follows has to name
                    // in its own compare-and-swap.
                    let members = members
                        .into_iter()
                        .map(|mut member| {
                            member.revision += 1;
                            member
                        })
                        .collect();
                    self.trashed.insert(
                        obj,
                        TrashGroup {
                            deleted_at,
                            members,
                        },
                    );
                }
            }
            Op::Untrash {
                obj, parent, name, ..
            } => {
                if let Some(group) = self.trashed.remove(&obj) {
                    for mut member in group.members {
                        if member.object_id == obj {
                            member.parent = parent.clone();
                            member.name = name.clone();
                        }
                        // Coming back is another revision, on the same terms as
                        // going away: a delete/restore cycle stays monotonic.
                        member.revision += 1;
                        self.objects.insert(member.object_id.clone(), member);
                    }
                }
            }
            Op::HardDelete { obj, .. } => {
                // Purging a live object takes its live subtree with it.
                for id in subtree_of(&self.objects, &obj) {
                    self.objects.remove(&id);
                }
                // The same object may be in the trash instead, where it is not
                // part of the live tree: purge it out of the group that holds it.
                for group in self.trashed.values_mut() {
                    if group.members.iter().any(|m| m.object_id == obj) {
                        let ids = group_subtree(&group.members, &obj);
                        group.members.retain(|m| !ids.contains(&m.object_id));
                    }
                }
                self.trashed.retain(|_, group| !group.members.is_empty());
            }
            Op::Part { uuid, index } => {
                self.parts.entry(uuid).or_default().push(PartMessage {
                    index,
                    message_id: id,
                    kind: entry.kind,
                    size: entry.size,
                });
            }
            Op::EncConfig {
                salt,
                params_json,
                wrapped_master_key,
                key_check,
                hint,
                ..
            } => {
                // The newest config wins, exactly as replay does.
                self.vault = Some((
                    VaultMaterial {
                        salt,
                        params_json,
                        wrapped_master_key,
                        key_check,
                    },
                    hint,
                ));
            }
            // Every other operation is one this port does not act on.
            Op::Other { .. } => {}
        }
    }

    /// Split the result into the live tree and the trash the drive itself holds.
    fn finish(mut self) -> (DriveContents, ReplayState) {
        // Resolve the multipart groups into the objects that name them, live
        // and trashed alike: a purge has to know which messages hold a trashed
        // file's bytes before it can destroy them.
        for (object_id, uuid) in std::mem::take(&mut self.multipart) {
            let Some(found) = self.parts.get(&uuid) else {
                continue;
            };
            let mut ordered: Vec<&PartMessage> = found.iter().collect();
            ordered.sort_by_key(|part| part.index);
            let ids: Vec<i32> = ordered.iter().map(|part| part.message_id).collect();
            let kind = ordered.iter().find_map(|part| part.kind);
            let total: i64 = ordered.iter().filter_map(|part| part.size).sum();
            let has_size = ordered.iter().any(|part| part.size.is_some());

            let resolve = |file: &mut RemoteFile| {
                if let Some(kind) = kind {
                    file.kind = kind;
                }
                if file.size == 0 && has_size {
                    file.size = total;
                }
                file.content = ContentRef::Parts(ids.clone());
            };
            if let Some(file) = self.objects.get_mut(&object_id) {
                resolve(file);
            }
            for group in self.trashed.values_mut() {
                if let Some(member) = group
                    .members
                    .iter_mut()
                    .find(|member| member.object_id == object_id)
                {
                    resolve(member);
                }
            }
        }

        // The state a later launch resumes from, taken once the multipart
        // groups have been resolved into the objects that name them.
        let state = ReplayState {
            objects: self.objects,
            trashed: self.trashed,
            parts: self.parts,
            multipart: BTreeMap::new(),
            vault: self.vault,
            media: BTreeMap::new(),
        };

        let mut files: Vec<RemoteFile> = state.objects.values().cloned().collect();
        files.sort_by(|a, b| {
            b.date
                .cmp(&a.date)
                .then_with(|| b.message_id.cmp(&a.message_id))
        });

        let mut trash: Vec<TrashedObject> = state
            .trashed
            .values()
            .filter_map(|group| {
                let deleted_at = group.deleted_at;
                group.members.first().map(|file| TrashedObject {
                    file: file.clone(),
                    deleted_at,
                })
            })
            .collect();
        trash.sort_by(|a, b| b.deleted_at.cmp(&a.deleted_at));

        (
            DriveContents {
                files,
                trash,
                vault: state.vault.clone(),
            },
            state,
        )
    }
}

/// Read a drive's operations into the files and folders they describe.
///
/// Without `resume` the whole history is read. With it — a watermark message
/// id and the state a previous read produced — the replay continues from that
/// state and only the messages after the watermark are fetched, which is what
/// keeps a second launch to a single request instead of dozens.
pub async fn async_load_drive(
    client: &Client,
    peer_ref: PeerRef,
    resume: Option<(i32, ReplayState)>,
) -> Result<(DriveContents, ReplayState, i32), String> {
    let trace = std::env::var("TDRIVE_TRACE_OPS").is_ok();
    let since = resume.as_ref().map(|(newest, _)| *newest);
    // Every message that carries media, by id. The writable-mount path keeps a
    // file's bytes in a separate hidden body message, so the row's kind and
    // size come from a message that is not itself an op. A resumed read starts
    // from the media the snapshot stored, because a new commit can name a body
    // that arrived before the watermark.
    let (mut media_by_id, mut replay) = match resume {
        Some((_, mut state)) => {
            let media = std::mem::take(&mut state.media);
            (media, Replay::from_state(state, trace))
        }
        None => (
            BTreeMap::new(),
            Replay {
                trace,
                ..Replay::default()
            },
        ),
    };
    let mut messages = client.iter_messages(peer_ref);
    let mut seen: Vec<OpMessage> = Vec::new();
    let mut scanned = 0usize;
    let mut newest = since.unwrap_or_default();
    while let Some(message) = messages.next().await.map_err(|e| e.to_string())? {
        // Telegram serves history newest first, so the first message at or
        // below the watermark ends the read: everything older is already in
        // the state this load resumed from.
        if let Some(since) = since {
            if message.id() <= since {
                break;
            }
        }
        scanned += 1;
        newest = newest.max(message.id());
        let media = message.media();
        let (kind, size) = match &media {
            Some(Media::Document(document)) => {
                let name = document.name().unwrap_or_default();
                let mime = document.mime_type();
                (
                    Some(FileKind::from_name_and_mime(name, mime)),
                    document.size().map(|size| size as i64),
                )
            }
            Some(Media::Photo(photo)) => {
                (Some(FileKind::Image), photo.size().map(|size| size as i64))
            }
            _ => (None, None),
        };
        if media.is_some() {
            media_by_id.insert(message.id(), (kind, size));
        }
        let Ok(Some(op)) = ops::parse(message.text()) else {
            continue;
        };
        seen.push(OpMessage {
            message_id: message.id(),
            op,
            // The match above already reports no media as `(None, None)`.
            kind,
            size,
            date: Some(message.date()),
        });
    }

    // Telegram serves history newest first and operations are only meaningful
    // in the order they were written, so replay them oldest first: a rename or
    // a move that arrived after an upload has to be applied after it.
    seen.sort_by_key(|entry| entry.message_id);
    let seen_len = seen.len();
    for entry in seen {
        replay.apply(entry, &media_by_id);
    }

    let counts = std::mem::take(&mut replay.counts);
    let (contents, mut state) = replay.finish();
    // The media of everything read so far travels with the state: a commit
    // arriving later may name a body that is older than the watermark.
    state.media = media_by_id;

    if trace {
        let objects = &state.objects;
        // A development aid for replay gaps: counts and object ids only, never
        // names or contents. `orphans` above zero means an object names a
        // parent the replay never saw.
        let folders = objects.values().filter(|file| file.is_folder()).count();
        let encrypted = objects.values().filter(|file| file.encrypted).count();
        let orphans = objects
            .values()
            .filter(|file| !file.parent.is_empty() && !objects.contains_key(&file.parent))
            .count();
        let media_by_id = &state.media;
        // A committed file names its bytes on another message. If that message
        // was never seen the row cannot be downloaded, so the count is the
        // check that the replay read every body it needs.
        let mut bodies_missing = 0;
        let mut bodies_self = 0;
        let mut bodies_other = 0;
        for file in objects.values() {
            match &file.content {
                ContentRef::Message(id) => {
                    if !media_by_id.contains_key(id) {
                        bodies_missing += 1;
                    } else if *id == file.message_id {
                        // The document sits on the op message itself, so the
                        // caption and the bytes share one message.
                        bodies_self += 1;
                    } else {
                        bodies_other += 1;
                    }
                }
                ContentRef::Parts(ids) => {
                    if ids.is_empty() {
                        bodies_missing += 1;
                    }
                }
                // The bytes are on the op message itself, so the check is that
                // the message the row came from actually carries media.
                ContentRef::OpMessage => {
                    if !media_by_id.contains_key(&file.message_id) {
                        bodies_missing += 1;
                    }
                }
            }
        }
        // A file whose declared stored size exceeds the size of the document on
        // its own message would be split across messages, which the read path
        // would silently truncate. The count has to stay at zero.
        let mut size_gap = 0;
        let mut biggest_gap = 0;
        for file in objects.values() {
            if let Some((_, Some(media_size))) = media_by_id.get(&file.message_id) {
                if file.size > *media_size {
                    size_gap += 1;
                    biggest_gap = biggest_gap.max(file.size - *media_size);
                }
            }
        }
        let most_parts = state.parts.values().map(|group| group.len()).max();
        eprintln!(
            "TRACE since={since:?} scanned={scanned} ops={seen_len} objects={} folders={folders} encrypted={encrypted} trashed={} orphans={orphans} multipart={} part_groups={} most_parts={most_parts:?} bodies_missing={bodies_missing} bodies_self={bodies_self} bodies_other={bodies_other} size_gap={size_gap} biggest_gap={biggest_gap} newest={newest}",
            objects.len(),
            state.trashed.len(),
            state.multipart.len(),
            state.parts.len(),
        );
        for (op_type, count) in &counts {
            eprintln!("TRACE op_type={op_type} count={count}");
        }
    }

    Ok((contents, state, newest))
}

// --- Writes -----------------------------------------------------------------

/// Upload a source file as a drive object.
///
/// The caption is what makes the message a file: without it the original would
/// not see the upload at all.
pub async fn async_upload_file(
    client: &Client,
    peer_ref: PeerRef,
    path: &Path,
    parent: &str,
    name: &str,
    key: Option<[u8; 32]>,
) -> Result<(), String> {
    let plaintext_size = std::fs::metadata(path)
        .map(|meta| meta.len() as i64)
        .unwrap_or_default();

    // An encrypted upload is not a different operation. The body is the
    // ciphertext and the attribute says how to read it (`enc=1`, `psz=` the
    // size before encryption), which is exactly what the original writes.
    let (body, op) = match key {
        Some(key) => {
            let staged = staged_ciphertext(path, &key)?;
            let size = std::fs::metadata(&staged)
                .map(|meta| meta.len() as i64)
                .unwrap_or_default();
            let op = Op::File {
                parent: parent.to_string(),
                name: name.to_string(),
                size: Some(size),
                uploaded_at: Some(Utc::now().timestamp()),
                encrypted: true,
                encryption_version: 1,
                plaintext_size: Some(plaintext_size),
            };
            (staged, op)
        }
        None => (
            path.to_path_buf(),
            Op::File {
                parent: parent.to_string(),
                name: name.to_string(),
                size: Some(plaintext_size),
                uploaded_at: Some(Utc::now().timestamp()),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
        ),
    };

    let uploaded = client.upload_file(&body).await;
    let uploaded = match uploaded {
        Ok(uploaded) => uploaded,
        Err(err) => {
            if body != path {
                let _ = std::fs::remove_file(&body);
            }
            return Err(err.to_string());
        }
    };

    // The caption is the message text for a document (grammers has no separate
    // caption setter), and the original appends the same second line so that a
    // reader with no operation to parse still learns the file's name.
    let caption = format!("{}\nTDrive: {}", ops::format(&op), name);
    let input = grammers_client::message::InputMessage::new()
        .document(uploaded)
        .text(caption);
    let sent = client
        .send_message(peer_ref, input)
        .await
        .map_err(|e| e.to_string());
    if body != path {
        let _ = std::fs::remove_file(&body);
    }
    sent?;
    Ok(())
}

/// Encrypt one file into a temporary file and return its path.
///
/// The plaintext never leaves the disk unencrypted: the ciphertext is written
/// first and only that file is handed to the uploader.
fn staged_ciphertext(path: &Path, key: &[u8; 32]) -> Result<PathBuf, String> {
    let mut source = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let staged = std::env::temp_dir().join(format!("tdrive-upload-{}.tde1", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut sink = std::fs::File::create(&staged).map_err(|e| e.to_string())?;
        crypto::encrypt_stream(&mut source, &mut sink, key).map_err(|e| e.to_string())?;
        sink.sync_all().map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(err) = result {
        let _ = std::fs::remove_file(&staged);
        return Err(err);
    }
    Ok(staged)
}

/// The `encfg` operation that turns a drive encrypted.
///
/// The hint is trimmed here and again when it is applied, matching both sides
/// of the original.
pub fn enc_config_op(material: &VaultMaterial, hint: &str) -> Op {
    Op::EncConfig {
        salt: material.salt.clone(),
        params_json: material.params_json.clone(),
        wrapped_master_key: material.wrapped_master_key.clone(),
        key_check: material.key_check.clone(),
        hint: hint.trim().to_string(),
        config_version: 1,
        enabled: true,
    }
}

/// The messages a purge deletes: the bodies of every file in the named trashed
/// subtrees.
///
/// The original captures the same set into its hard-delete plan and destroys it
/// from the client that started the purge. A legacy upload keeps its bytes in
/// its own operation message, which is the drive's record that the object
/// existed, so that one is left alone -- the plan leaves it alone too.
pub(crate) fn purged_bodies(state: &ReplayState, roots: &[String]) -> Vec<i32> {
    let mut bodies = BTreeSet::new();
    for root in roots {
        let Some(group) = state.trashed.get(root) else {
            continue;
        };
        for member in &group.members {
            match &member.content {
                ContentRef::OpMessage => {}
                ContentRef::Message(message_id) => {
                    bodies.insert(*message_id);
                }
                ContentRef::Parts(parts) => bodies.extend(parts.iter().copied()),
            }
        }
    }
    bodies.into_iter().collect()
}

/// Publish one operation to the drive.
///
/// Every mutation in the original leaves the same trace: a message whose text is
/// the formatted header. The projection reads it back on the next load, so this
/// is the whole of the write path that is not about bytes.
pub async fn async_send_op(client: &Client, peer_ref: PeerRef, op: &Op) -> Result<i32, String> {
    let input = grammers_client::message::InputMessage::new().text(ops::format(op));
    let message = client
        .send_message(peer_ref, input)
        .await
        .map_err(|e| e.to_string())?;
    Ok(message.id())
}

/// Delete messages from the drive.
///
/// Only a purge does this, and only to the bodies a trashed file's bytes live
/// in. An operation message is never deleted: it is the drive's own record that
/// the object exists, or existed.
pub async fn async_delete_messages(
    client: &Client,
    peer_ref: PeerRef,
    message_ids: &[i32],
) -> Result<(), String> {
    if message_ids.is_empty() {
        return Ok(());
    }
    client
        .delete_messages(peer_ref, message_ids)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

// --- Reads ------------------------------------------------------------------

/// Fetch a single message's media so a download can start from a row's id
/// without re-listing the whole chat.
async fn find_media(client: &Client, peer_ref: PeerRef, message_id: i32) -> Result<Media, String> {
    let found = client
        .get_messages_by_id(peer_ref, &[message_id])
        .await
        .map_err(|e| e.to_string())?;
    found
        .into_iter()
        .flatten()
        .find_map(|message| message.media())
        .ok_or_else(|| "the drive no longer holds that message".to_string())
}

/// Fetch the given messages' bodies into one file, in order.
///
/// Chunks are streamed straight to disk, so neither a large file nor a long
/// multipart group is ever held in memory.
async fn download_into(
    client: &Client,
    peer_ref: PeerRef,
    messages: &[i32],
    dest: &Path,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut writer = tokio::fs::File::create(dest)
        .await
        .map_err(|e| e.to_string())?;
    for id in messages {
        let media = find_media(client, peer_ref, *id).await?;
        let mut chunks = client.iter_download(&media);
        while let Some(chunk) = chunks.next().await.map_err(|e| e.to_string())? {
            writer.write_all(&chunk).await.map_err(|e| e.to_string())?;
        }
    }
    writer.flush().await.map_err(|e| e.to_string())?;
    Ok(())
}

/// Download a row's contents, decrypting them on the way when the drive says
/// they are encrypted.
///
/// A file's bytes are not always on the row's own message: a file committed
/// through the writable-mount path keeps them in a hidden body message, or in
/// a multipart group, so the caller passes the messages the row's content
/// reference resolved to.
///
/// The ciphertext is staged beside the destination and only renamed into place
/// by a successful, authenticated decrypt, so a wrong key or a truncated
/// download never leaves a half-written file behind.
pub async fn async_download_file(
    client: &Client,
    peer_ref: PeerRef,
    messages: &[i32],
    dest: &Path,
    key: Option<[u8; 32]>,
) -> Result<(), String> {
    if messages.is_empty() {
        return Err("the drive does not say where the contents are".to_string());
    }
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }

    let Some(key) = key else {
        return download_into(client, peer_ref, messages, dest).await;
    };

    let staged = dest.with_extension("tdrive-part");
    let _ = tokio::fs::remove_file(&staged).await;
    if let Err(message) = download_into(client, peer_ref, messages, &staged).await {
        let _ = tokio::fs::remove_file(&staged).await;
        return Err(message);
    }
    let outcome = tokio::task::spawn_blocking({
        let staged = staged.clone();
        let dest = dest.to_path_buf();
        move || -> Result<(), String> {
            let source = std::fs::File::open(&staged).map_err(|e| e.to_string())?;
            let target = std::fs::File::create(&dest).map_err(|e| e.to_string())?;
            let mut reader = BufReader::new(source);
            let mut writer = BufWriter::new(target);
            crypto::decrypt_stream(&mut reader, &mut writer, &key).map_err(|e| e.to_string())?;
            Ok(())
        }
    })
    .await
    .map_err(|e| e.to_string())?;
    let _ = tokio::fs::remove_file(&staged).await;
    outcome
}

// --- Blocking wrappers used from GPUI's background executor -----------------

pub fn connect(creds: &Credentials, session_path: PathBuf) -> Result<Client, String> {
    RT.block_on(async_connect(creds, session_path))
}
pub fn is_authorized(client: &Client) -> bool {
    RT.block_on(async_is_authorized(client))
}
pub fn request_code(
    client: &Client,
    phone: &str,
    api_hash: &str,
) -> Result<grammers_client::client::LoginToken, String> {
    RT.block_on(async_request_code(client, phone, api_hash))
}
pub fn sign_in(
    client: &Client,
    token: grammers_client::client::LoginToken,
    code: &str,
) -> Result<Option<grammers_client::client::PasswordToken>, String> {
    RT.block_on(async_sign_in(client, token, code))
}
pub fn check_password(
    client: &Client,
    token: grammers_client::client::PasswordToken,
    password: &str,
) -> Result<grammers_client::peer::User, String> {
    RT.block_on(async_check_password(client, token, password))
}
pub fn list_owned_channels(client: &Client) -> Result<Vec<OwnedChannel>, String> {
    RT.block_on(async_list_owned_channels(client))
}
pub fn create_personal_channel(
    client: &Client,
    title: &str,
    about: &str,
) -> Result<OwnedChannel, String> {
    RT.block_on(async_create_personal_channel(client, title, about))
}
pub fn load_drive(
    client: &Client,
    peer_ref: PeerRef,
    resume: Option<(i32, ReplayState)>,
) -> Result<(DriveContents, ReplayState, i32), String> {
    RT.block_on(async_load_drive(client, peer_ref, resume))
}
pub fn send_op(client: &Client, peer_ref: PeerRef, op: &Op) -> Result<i32, String> {
    RT.block_on(async_send_op(client, peer_ref, op))
}
pub fn delete_messages(
    client: &Client,
    peer_ref: PeerRef,
    message_ids: &[i32],
) -> Result<(), String> {
    RT.block_on(async_delete_messages(client, peer_ref, message_ids))
}
pub fn upload_file(
    client: &Client,
    peer_ref: PeerRef,
    path: &Path,
    parent: &str,
    name: &str,
    key: Option<[u8; 32]>,
) -> Result<(), String> {
    RT.block_on(async_upload_file(client, peer_ref, path, parent, name, key))
}
pub fn download_file(
    client: &Client,
    peer_ref: PeerRef,
    messages: &[i32],
    dest: &Path,
    key: Option<[u8; 32]>,
) -> Result<(), String> {
    RT.block_on(async_download_file(client, peer_ref, messages, dest, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config operation carries the vault material as it is stored and
    /// comes back from the wire unchanged, which is what lets any reader of the
    /// drive unlock it.
    #[test]
    fn encryption_config_op_round_trips_through_the_wire() {
        let material = VaultMaterial {
            salt: vec![1, 2, 3, 4],
            params_json: r#"{"kdf":"argon2id","memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#.to_string(),
            wrapped_master_key: vec![7; 72],
            key_check: vec![9; 59],
        };
        let op = enc_config_op(&material, "  pet name  ");
        let wire = ops::format(&op);
        // The hint travels trimmed, exactly as the original writes it.
        assert!(wire.contains("|hint=pet+name"));
        assert!(!wire.contains("hint=++pet"));
        assert!(
            !wire.contains("|cv="),
            "version 1 does not write cv: {wire}"
        );

        let parsed = ops::parse(&wire).expect("parse").expect("an op");
        assert_eq!(parsed, op);
    }

    /// One replayed op, with no media of its own.
    fn entry(message_id: i32, op: Op) -> OpMessage {
        OpMessage {
            message_id,
            op,
            kind: None,
            size: None,
            date: None,
        }
    }

    /// A folder committed the way the mounted-write path commits one: a durable
    /// uuid, not the commit message's own number.
    fn folder_commit(message_id: i32, obj: &str, parent: &str, name: &str) -> OpMessage {
        entry(
            message_id,
            Op::FolderCommit {
                op_id: format!("op-{message_id}"),
                obj: obj.to_string(),
                parent: parent.to_string(),
                name: name.to_string(),
            },
        )
    }

    /// A committed file whose bytes live on another message.
    fn file_commit(
        message_id: i32,
        parent: &str,
        name: &str,
        content_msg_id: i64,
        size: Option<i64>,
    ) -> OpMessage {
        entry(
            message_id,
            Op::FileCommit {
                op_id: format!("op-{message_id}"),
                parent: parent.to_string(),
                name: name.to_string(),
                content_msg_id,
                upload_uuid: String::new(),
                part_count: 0,
                content_hash: String::new(),
                size,
                uploaded_at: None,
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
        )
    }

    /// The media table a body message would contribute, keyed by message id.
    fn media(kind: FileKind, size: i64) -> BTreeMap<i32, (Option<FileKind>, Option<i64>)> {
        BTreeMap::from([(9, (Some(kind), Some(size)))])
    }

    fn no_media() -> BTreeMap<i32, (Option<FileKind>, Option<i64>)> {
        BTreeMap::new()
    }

    /// One part of a multipart group, with the media the message carried.
    fn part(message_id: i32, uuid: &str, index: u32, kind: FileKind, size: i64) -> OpMessage {
        let mut entry = entry(
            message_id,
            Op::Part {
                uuid: uuid.to_string(),
                index,
            },
        );
        entry.kind = Some(kind);
        entry.size = Some(size);
        entry
    }

    fn find<'a>(contents: &'a DriveContents, object_id: &str) -> &'a RemoteFile {
        contents
            .files
            .iter()
            .find(|file| file.object_id == object_id)
            .unwrap_or_else(|| {
                panic!(
                    "{object_id} is not in {contents:?}",
                    contents = contents.files.len()
                )
            })
    }

    /// The rule that decides what the picker may offer. Every chat type the
    /// user asked not to see is rejected here rather than hidden in the
    /// sidebar, which is what keeps unrelated conversations out entirely.
    #[test]
    fn only_owned_broadcast_channels_are_drives() {
        // A channel the account created as a broadcast channel: a drive.
        assert!(is_drive_channel(true, true, false, false));

        // A broadcast channel the account joined but did not create.
        assert!(!is_drive_channel(false, true, false, false));

        // A group or supergroup, even one the account created.
        assert!(!is_drive_channel(true, true, true, false));
        assert!(!is_drive_channel(true, false, true, false));

        // A channel the account left.
        assert!(!is_drive_channel(true, true, false, true));

        // A private chat with another person, or with a bot: not a channel at
        // all, so it never reaches this rule and never becomes a candidate.
        assert!(!is_drive_channel(false, false, false, false));
    }

    /// A drive written through TDrive's mounted-write path keeps its folders in
    /// `dcommit` ops whose ids are uuids. The files inside them name that uuid
    /// as their parent, so a replay that only understood `mkdir` would show an
    /// empty drive whose files belong to no folder at all.
    #[test]
    fn a_folder_commit_resolves_the_files_named_under_it() {
        let media = media(FileKind::Audio, 4096);
        let mut replay = Replay::default();
        replay.apply(
            folder_commit(1, "d:db126e90-3715-499f-97a6-4a543993dfae", "", "Ringtones"),
            &media,
        );
        replay.apply(
            file_commit(
                2,
                "d:db126e90-3715-499f-97a6-4a543993dfae",
                "tone.mp3",
                9,
                None,
            ),
            &media,
        );
        let (contents, _) = replay.finish();

        let folder = find(&contents, "d:db126e90-3715-499f-97a6-4a543993dfae");
        assert!(folder.is_folder());
        assert_eq!(folder.name, "Ringtones");

        // The row's bytes are on the body message, and its kind and size come
        // from there because the commit did not record them.
        let file = find(&contents, "f:2");
        assert_eq!(file.parent, "d:db126e90-3715-499f-97a6-4a543993dfae");
        assert_eq!(file.kind, FileKind::Audio);
        assert_eq!(file.size, 4096);
        assert_eq!(file.content, ContentRef::Message(9));
        assert_eq!(file.revision, 1);
    }

    /// Trashing a folder takes its live subtree off the tree as one entry, and
    /// restoring it puts the subtree back under the parent and name the restore
    /// op chose.
    #[test]
    fn a_trash_op_hides_a_subtree_and_a_restore_brings_it_back() {
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:zeta", "", "Ringtones"), &no_media());
        replay.apply(
            file_commit(2, "d:zeta", "tone.mp3", 9, Some(10)),
            &no_media(),
        );
        replay.apply(
            entry(
                3,
                Op::Trash {
                    op_id: "op-3".to_string(),
                    obj: "d:zeta".to_string(),
                    expected_revision: 1,
                    deleted_at: 1_700_000_000,
                    purge_after: 1_700_000_001,
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        assert!(
            contents.files.is_empty(),
            "the subtree is not live any more"
        );
        assert_eq!(contents.trash.len(), 1);
        assert_eq!(contents.trash[0].file.object_id, "d:zeta");
        assert_eq!(contents.trash[0].deleted_at, 1_700_000_000);

        // The restore names a new home, because the original one may be gone.
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:zeta", "", "Ringtones"), &no_media());
        replay.apply(
            file_commit(2, "d:zeta", "tone.mp3", 9, Some(10)),
            &no_media(),
        );
        replay.apply(
            entry(
                3,
                Op::Trash {
                    op_id: "op-3".to_string(),
                    obj: "d:zeta".to_string(),
                    expected_revision: 1,
                    deleted_at: 5,
                    purge_after: 6,
                },
            ),
            &no_media(),
        );
        replay.apply(
            entry(
                4,
                Op::Untrash {
                    op_id: "op-4".to_string(),
                    obj: "d:zeta".to_string(),
                    parent: "".to_string(),
                    name: "Ringtones (restored)".to_string(),
                    expected_revision: 1,
                },
            ),
            &no_media(),
        );
        let (contents, _) = replay.finish();
        assert!(contents.trash.is_empty());
        let folder = find(&contents, "d:zeta");
        assert_eq!(folder.name, "Ringtones (restored)");
        assert_eq!(folder.parent, "");
        // The file came back inside it, not at the root.
        assert_eq!(find(&contents, "f:2").parent, "d:zeta");
    }

    /// An object trashed separately, before its parent, keeps its own entry: a
    /// restore of the parent must not silently resurrect it.
    #[test]
    fn a_separately_trashed_object_is_not_restored_with_its_parent() {
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:zeta", "", "Ringtones"), &no_media());
        replay.apply(
            file_commit(2, "d:zeta", "tone.mp3", 9, Some(10)),
            &no_media(),
        );
        replay.apply(
            entry(
                3,
                Op::Trash {
                    op_id: "op-3".to_string(),
                    obj: "f:2".to_string(),
                    expected_revision: 1,
                    deleted_at: 10,
                    purge_after: 11,
                },
            ),
            &no_media(),
        );
        replay.apply(
            entry(
                4,
                Op::Trash {
                    op_id: "op-4".to_string(),
                    obj: "d:zeta".to_string(),
                    expected_revision: 1,
                    deleted_at: 20,
                    purge_after: 21,
                },
            ),
            &no_media(),
        );
        // Restore the folder only.
        replay.apply(
            entry(
                5,
                Op::Untrash {
                    op_id: "op-5".to_string(),
                    obj: "d:zeta".to_string(),
                    parent: "".to_string(),
                    name: "Ringtones".to_string(),
                    expected_revision: 1,
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        assert_eq!(contents.files.len(), 1, "the file stays trashed");
        assert_eq!(contents.files[0].object_id, "d:zeta");
        assert_eq!(contents.trash.len(), 1);
        assert_eq!(contents.trash[0].file.object_id, "f:2");
    }

    /// An overwriting move is also the deletion of what it landed on.
    #[test]
    fn an_overwriting_move_trashes_what_it_landed_on() {
        let mut replay = Replay::default();
        replay.apply(file_commit(1, "", "keep.txt", 9, Some(1)), &no_media());
        replay.apply(file_commit(2, "", "victim.txt", 9, Some(2)), &no_media());
        replay.apply(
            entry(
                3,
                Op::Relocate {
                    op_id: "op-3".to_string(),
                    obj: "f:1".to_string(),
                    parent: "".to_string(),
                    name: "victim.txt".to_string(),
                    expected_revision: 1,
                    overwrite: true,
                    destination_obj: "f:2".to_string(),
                    expected_destination_revision: 1,
                    deleted_at: 100,
                    purge_after: 200,
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        assert_eq!(contents.files.len(), 1);
        assert_eq!(find(&contents, "f:1").name, "victim.txt");
        assert_eq!(find(&contents, "f:1").revision, 2);
        assert_eq!(contents.trash.len(), 1);
        assert_eq!(contents.trash[0].file.object_id, "f:2");
    }

    /// A permanent delete purges the object wherever it is: the live subtree it
    /// names, or the trash group that holds it.
    #[test]
    fn a_hard_delete_purges_a_trashed_subtree() {
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:zeta", "", "Ringtones"), &no_media());
        replay.apply(
            file_commit(2, "d:zeta", "tone.mp3", 9, Some(10)),
            &no_media(),
        );
        replay.apply(
            entry(
                3,
                Op::Trash {
                    op_id: "op-3".to_string(),
                    obj: "d:zeta".to_string(),
                    expected_revision: 1,
                    deleted_at: 5,
                    purge_after: 6,
                },
            ),
            &no_media(),
        );
        replay.apply(
            entry(
                4,
                Op::HardDelete {
                    op_id: "op-4".to_string(),
                    obj: "d:zeta".to_string(),
                    expected_revision: 1,
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        assert!(contents.files.is_empty());
        assert!(contents.trash.is_empty());
    }

    /// A multipart commit resolves to its parts' messages in index order, and
    /// takes its kind and size from them.
    #[test]
    fn a_multipart_commit_resolves_its_parts_in_order() {
        let mut replay = Replay::default();
        replay.apply(part(1, "group-1", 1, FileKind::Document, 20), &no_media());
        replay.apply(part(2, "group-1", 0, FileKind::Document, 10), &no_media());
        replay.apply(
            entry(
                3,
                Op::FileCommit {
                    op_id: "op-3".to_string(),
                    parent: "".to_string(),
                    name: "big.iso".to_string(),
                    content_msg_id: 0,
                    upload_uuid: "group-1".to_string(),
                    part_count: 2,
                    content_hash: String::new(),
                    size: Some(30),
                    uploaded_at: None,
                    encrypted: false,
                    encryption_version: 0,
                    plaintext_size: None,
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        let file = find(&contents, "f:3");
        assert_eq!(file.content, ContentRef::Parts(vec![2, 1]));
        assert_eq!(file.kind, FileKind::Document);
        assert_eq!(file.size, 30);
    }

    /// A delete and the restore that undoes it each advance the object, because
    /// the drive's own rows advance: the restore's compare-and-swap names the
    /// revision its tombstone left behind, and the object comes back one past
    /// that. Getting this wrong means the drive refuses the write in silence.
    #[test]
    fn a_delete_and_the_restore_that_undoes_it_advance_the_revision() {
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:home", "", "Home"), &no_media());
        replay.apply(
            file_commit(2, "d:home", "leaf.bin", 9, Some(9)),
            &no_media(),
        );
        assert_eq!(
            replay.objects.get("f:2").map(|file| file.revision),
            Some(1),
            "an object starts at revision 1, as the drive's rows do"
        );

        // The delete the port writes carries the revision it read.
        replay.apply(
            entry(3, ops::trash_op("f:2", 1, 1_700_000_000, 30 * 86_400)),
            &no_media(),
        );
        let (contents, state) = replay.finish();
        assert_eq!(
            contents.trash[0].file.revision, 2,
            "the tombstone is a revision of its own"
        );

        // And the restore carries the one the tombstone left.
        let mut replay = Replay::from_state(state, false);
        replay.apply(
            entry(4, ops::restore_op("f:2", 2, "d:home", "leaf.bin")),
            &no_media(),
        );
        let (contents, state) = replay.finish();
        assert_eq!(find(&contents, "f:2").revision, 3);
        assert_eq!(find(&contents, "f:2").parent, "d:home");
        assert!(contents.trash.is_empty());

        // A purge is written against the same revision and leaves nothing.
        let mut replay = Replay::from_state(state, false);
        replay.apply(
            entry(5, ops::trash_op("f:2", 3, 1_700_000_100, 30 * 86_400)),
            &no_media(),
        );
        let (contents, state) = replay.finish();
        assert_eq!(contents.trash[0].file.revision, 4);
        let mut replay = Replay::from_state(state, false);
        replay.apply(entry(6, ops::purge_op("f:2", 4)), &no_media());
        let (contents, state) = replay.finish();
        assert!(contents.trash.is_empty());
        assert!(
            !state.objects.contains_key("f:2"),
            "the file is gone for good"
        );
    }

    /// Renaming or moving a file advances it, as it does the drive's own
    /// namespace rows: the next operation to name the object has to name the
    /// name's own revision.
    #[test]
    fn a_rename_and_a_move_advance_the_revision() {
        let mut replay = Replay::default();
        replay.apply(file_commit(1, "", "leaf.bin", 9, Some(9)), &no_media());
        replay.apply(
            entry(
                2,
                Op::Rename {
                    obj: "f:1".to_string(),
                    name: "renamed.bin".to_string(),
                },
            ),
            &no_media(),
        );
        let (contents, state) = replay.finish();
        assert_eq!(find(&contents, "f:1").revision, 2);

        let mut replay = Replay::from_state(state, false);
        replay.apply(
            entry(
                3,
                Op::Move {
                    obj: "f:1".to_string(),
                    parent: "d:home".to_string(),
                },
            ),
            &no_media(),
        );
        let (contents, _) = replay.finish();
        let file = find(&contents, "f:1");
        assert_eq!(file.revision, 3);
        assert_eq!(file.parent, "d:home");
        assert_eq!(file.name, "renamed.bin");
    }

    /// A purge destroys the messages a trashed file's bytes live in, and leaves
    /// the operation messages alone: they are the drive's own record.
    #[test]
    fn a_purge_names_the_bodies_it_destroys() {
        let mut replay = Replay::default();
        replay.apply(folder_commit(1, "d:home", "", "Home"), &no_media());
        // A file whose bytes live on another message, and one whose bytes are
        // its own operation message.
        replay.apply(
            file_commit(2, "d:home", "committed.bin", 9, Some(9)),
            &no_media(),
        );
        replay.apply(
            file_commit(3, "d:home", "legacy.bin", 0, Some(4)),
            &no_media(),
        );
        let (_, state) = replay.finish();

        let mut replay = Replay::from_state(state, false);
        replay.apply(entry(4, ops::trash_op("d:home", 1, 100, 200)), &no_media());
        let (_, state) = replay.finish();

        let bodies = purged_bodies(&state, &["d:home".to_string()]);
        assert_eq!(
            bodies,
            vec![9],
            "the body message goes; the operation messages stay"
        );
        assert!(
            purged_bodies(&state, &["f:2".to_string()]).is_empty(),
            "only a trashed object is purgeable"
        );
    }

    /// A replacement points the row at the new body and bumps its revision.
    #[test]
    fn a_file_replace_points_the_row_at_the_new_body() {
        let mut replay = Replay::default();
        replay.apply(file_commit(1, "", "report.pdf", 9, Some(100)), &no_media());
        replay.apply(
            entry(
                2,
                Op::FileReplace {
                    op_id: "op-2".to_string(),
                    obj: "f:1".to_string(),
                    expected_revision: 1,
                    content_msg_id: 11,
                    upload_uuid: String::new(),
                    part_count: 0,
                    content_hash: String::new(),
                    retained_until: 9_999,
                    size: Some(200),
                    uploaded_at: None,
                    encrypted: true,
                    encryption_version: 1,
                    plaintext_size: Some(150),
                },
            ),
            &no_media(),
        );

        let (contents, _) = replay.finish();
        let file = find(&contents, "f:1");
        assert_eq!(file.content, ContentRef::Message(11));
        assert_eq!(file.size, 200);
        assert_eq!(file.plaintext_size, Some(150));
        assert!(file.encrypted);
        assert_eq!(file.revision, 2);
    }

    /// What a load produced, in a shape two loads can be compared by: the
    /// fields a resume has to get right, sorted so the comparison does not
    /// depend on the order the objects were read in.
    fn shape(contents: &DriveContents) -> Vec<(String, String, String, i64, ContentRef, i64)> {
        let mut rows: Vec<_> = contents
            .files
            .iter()
            .map(|file| {
                (
                    file.object_id.clone(),
                    file.name.clone(),
                    file.parent.clone(),
                    file.size,
                    file.content.clone(),
                    file.revision,
                )
            })
            .collect();
        rows.sort();
        rows
    }

    /// The operations a drive held when it was first read.
    fn early_ops() -> Vec<OpMessage> {
        vec![
            folder_commit(1, "d:photos", "", "Photos"),
            file_commit(2, "d:photos", "one.jpg", 9, None),
            entry(
                3,
                Op::Trash {
                    op_id: "op-3".to_string(),
                    obj: "d:photos".to_string(),
                    expected_revision: 1,
                    deleted_at: 1_700_000_000,
                    purge_after: 1_700_000_001,
                },
            ),
        ]
    }

    /// The operations that arrived between two launches: a restore, a file
    /// whose bytes are on the body message, and a new multipart group.
    fn later_ops() -> Vec<OpMessage> {
        vec![
            entry(
                4,
                Op::Untrash {
                    op_id: "op-4".to_string(),
                    obj: "d:photos".to_string(),
                    parent: String::new(),
                    name: "Photos".to_string(),
                    expected_revision: 1,
                },
            ),
            file_commit(5, "d:photos", "two.jpg", 9, None),
            part(6, "group-a", 0, FileKind::Image, 4096),
            entry(
                7,
                Op::Manifest {
                    uuid: "group-a".to_string(),
                    parent: "d:photos".to_string(),
                    name: "album.jpg".to_string(),
                    part_count: 1,
                    size: None,
                    uploaded_at: None,
                    encrypted: false,
                    encryption_version: 0,
                    plaintext_size: None,
                },
            ),
        ]
    }

    /// A second launch resumes from the state the first one left behind and
    /// reads only what came after. It has to land exactly where one long read
    /// of the whole history lands, trash and multipart groups included: this is
    /// what makes a snapshot a cache of the replay rather than a second, subtly
    /// different implementation of it.
    #[test]
    fn resuming_from_a_snapshot_matches_a_full_replay() {
        let media = media(FileKind::Image, 4096);

        // The first launch reads the drive as it was and keeps its state.
        let mut first = Replay::default();
        for op in early_ops() {
            first.apply(op, &media);
        }
        let (first_contents, first_state) = first.finish();
        assert_eq!(first_contents.trash.len(), 1, "the folder is in the trash");

        // The second launch resumes from that state and reads the rest.
        let mut resumed = Replay::from_state(first_state, false);
        for op in later_ops() {
            resumed.apply(op, &media);
        }
        let (resumed_contents, resumed_state) = resumed.finish();

        // One read of everything, for comparison.
        let mut full = Replay::default();
        for op in early_ops().into_iter().chain(later_ops()) {
            full.apply(op, &media);
        }
        let (full_contents, _) = full.finish();

        assert_eq!(shape(&resumed_contents), shape(&full_contents));
        assert!(
            resumed_contents.trash.is_empty(),
            "the restore emptied the trash, in both reads"
        );
        assert!(full_contents.trash.is_empty());

        // A group whose parts arrived before the op that names them is resolved
        // the same way on both paths.
        let album = find(&resumed_contents, "f:7");
        assert_eq!(album.content, ContentRef::Parts(vec![6]));
        assert_eq!(album.size, 4096);

        // And the state that goes back into a snapshot is as complete as the
        // one that came out.
        assert_eq!(resumed_state.objects.len(), resumed_contents.files.len());
        assert!(resumed_state.multipart.is_empty(), "groups are resolved");
    }
}
