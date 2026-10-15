use std::path::{Path, PathBuf};
use std::sync::Arc;

use grammers_client::media::Media;
use grammers_client::{Client, SignInError};
use grammers_mtsender::SenderPool;
use grammers_session::storages::SqliteSession;
use grammers_session::types::PeerRef;
use tokio::runtime::Runtime;

use crate::models::{Credentials, DialogInfo, DialogKind, FileKind, LocalState, RemoteFile};

pub static RT: std::sync::LazyLock<Runtime> = std::sync::LazyLock::new(|| {
    Runtime::new().expect("tokio runtime")
});

pub async fn async_connect(creds: &Credentials, session_path: PathBuf) -> Result<Client, String> {
    let session = Arc::new(SqliteSession::open(session_path).await.map_err(|e| e.to_string())?);
    let SenderPool { runner, handle, .. } = SenderPool::new(Arc::clone(&session), creds.api_id);
    let client = Client::new(handle);
    tokio::spawn(runner.run());
    Ok(client)
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

pub async fn async_list_dialogs(client: &Client) -> Result<Vec<DialogInfo>, String> {
    use grammers_client::peer::Peer;

    let mut dialogs = client.iter_dialogs();
    let mut out = Vec::new();
    while let Some(d) = dialogs.next().await.map_err(|e| e.to_string())? {
        let peer = d.peer();
        let kind = match peer {
            Peer::User(_) => DialogKind::Personal,
            Peer::Group(_) | Peer::Channel(_) => DialogKind::Shared,
        };
        let Some(peer_ref) = peer.to_ref().await.map_err(|e| e.to_string())? else {
            continue;
        };
        out.push(DialogInfo {
            id: peer.id().bot_api_dialog_id_unchecked(),
            title: peer.name().unwrap_or("Saved Messages").to_string(),
            kind,
            peer_ref,
        });
        if out.len() >= 200 {
            break;
        }
    }
    // The original keeps its own drive ordering; peers arrive newest-first, so
    // sort by title to make the sidebar scannable.
    out.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()));
    Ok(out)
}

pub async fn async_list_files(client: &Client, peer_ref: PeerRef) -> Result<Vec<RemoteFile>, String> {
    let mut messages = client.iter_messages(peer_ref);
    let mut out = Vec::new();
    let mut seen = 0usize;
    while let Some(msg) = messages.next().await.map_err(|e| e.to_string())? {
        seen += 1;
        if let Some(media) = msg.media() {
            let (name, size, _mime, kind) = match &media {
                Media::Document(doc) => {
                    let name = doc.name().unwrap_or("file").to_string();
                    let mime = doc.mime_type().map(str::to_string);
                    let kind = FileKind::from_name_and_mime(&name, mime.as_deref());
                    (name, doc.size().unwrap_or(0) as i64, mime, kind)
                }
                Media::Photo(photo) => (
                    format!("photo_{}.jpg", msg.id()),
                    photo.size().unwrap_or(0) as i64,
                    Some("image/jpeg".to_string()),
                    FileKind::Image,
                ),
                _ => continue,
            };
            out.push(RemoteFile {
                message_id: msg.id(),
                name,
                size,
                kind,
                date: Some(msg.date()),
                local_state: LocalState::Remote,
            });
        }
        if seen > 500 {
            break;
        }
    }
    Ok(out)
}

pub async fn async_delete_file(
    client: &Client,
    peer_ref: PeerRef,
    message_id: i32,
) -> Result<(), String> {
    client
        .delete_messages(peer_ref, &[message_id])
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub async fn async_upload_file(
    client: &Client,
    peer_ref: PeerRef,
    path: &Path,
) -> Result<(), String> {
    let uploaded = client.upload_file(path).await.map_err(|e| e.to_string())?;
    let input = grammers_client::message::InputMessage::new().document(uploaded);
    client
        .send_message(peer_ref, input)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Fetch a single message's media so a download can start from a row's id
/// without re-listing the whole chat.
async fn find_media(client: &Client, peer_ref: PeerRef, message_id: i32) -> Result<Media, String> {
    let mut messages = client.iter_messages(peer_ref);
    while let Some(msg) = messages.next().await.map_err(|e| e.to_string())? {
        if msg.id() == message_id {
            return msg.media().ok_or_else(|| "no media".to_string());
        }
    }
    Err("message not found".to_string())
}

pub async fn async_download_file(
    client: &Client,
    peer_ref: PeerRef,
    message_id: i32,
    dest: &Path,
) -> Result<(), String> {
    let media = find_media(client, peer_ref, message_id).await?;
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    client
        .download_media(&media, dest)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
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
pub fn list_dialogs(client: &Client) -> Result<Vec<DialogInfo>, String> {
    RT.block_on(async_list_dialogs(client))
}
pub fn list_files(client: &Client, peer_ref: PeerRef) -> Result<Vec<RemoteFile>, String> {
    RT.block_on(async_list_files(client, peer_ref))
}
pub fn delete_file(client: &Client, peer_ref: PeerRef, message_id: i32) -> Result<(), String> {
    RT.block_on(async_delete_file(client, peer_ref, message_id))
}
pub fn upload_file(client: &Client, peer_ref: PeerRef, path: &Path) -> Result<(), String> {
    RT.block_on(async_upload_file(client, peer_ref, path))
}
pub fn download_file(
    client: &Client,
    peer_ref: PeerRef,
    message_id: i32,
    dest: &Path,
) -> Result<(), String> {
    RT.block_on(async_download_file(client, peer_ref, message_id, dest))
}
