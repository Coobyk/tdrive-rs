//! TDX1 caption wire format: parse and format TDrive drive operations.
//!
//! Every drive object in TDrive is a Telegram message whose caption begins with
//! a pipe-separated header, `TDX1|t=<op>|key=value|...`. This is a
//! self-contained Rust port of `backend/projection/wire.go` (with `types.go`
//! for the op types and the `f:`/`d:` id prefixes). The Go source is the format
//! authority; when the two disagree, Go is right.
//!
//! Only the first line of the caption is the header -- anything after the first
//! `\n` is a human-readable comment. `n` (name), `kdf` and `hint` are
//! percent-encoded with Go's `url.QueryEscape`, so `+` means a space, and must
//! be unescaped. `salt`, `wrap` and `check` use base64 **RawURLEncoding**
//! (unpadded, URL-safe alphabet). Timestamps are unix seconds.
//!
//! # Messages that are not drive ops
//!
//! Ordinary chat messages have no caption, or a caption that is not a TDX1
//! header. [`parse`] reports those as [`Ok(None)`](parse) -- the common case --
//! rather than an error, so a caller can walk a history page and skip them.
//! [`is_header_candidate`] mirrors the Go caller's quick check
//! (`strings.HasPrefix(strings.TrimSpace(header), "TDX1")`).
//!
//! # Deliberate divergences from Go
//!
//! * Go returns `ErrWireBadVersion` for *any* header whose first field is not
//!   `TDX1`. Here a field that looks like a versioned TDX header (`TDX` followed
//!   by a digit, such as `TDX2`) is [`OpError::BadVersion`], while anything else
//!   is not a drive message at all ([`Ok(None)`](parse)).
//! * Go distinguishes bad object ids (`ErrWireBadObject`) from bad parents
//!   (`ErrWireBadParent`); both collapse into [`OpError::Malformed`], which is
//!   the one variant the public error type has for a malformed known op.
//! * A non-empty `t` this port does not act on parses as [`Op::Other`] instead
//!   of Go's `ErrWireBadOpType`, so the format stays forward-compatible. An
//!   empty `t=` is [`OpError::BadOpType`], matching Go's fall-through.
//! * `ev`, `cv`, `pix` and `pc` are `u32` in this API. Go accepts the full
//!   signed `int` range, so negative values and values above `u32::MAX` are
//!   rejected here as malformed.
//! * `n`, `kdf` and `hint` must be valid UTF-8 after unescaping because the
//!   ported API uses `String`; Go tolerates arbitrary bytes.
//! * `rend` (image rendition) payloads are not modelled. The op-type guard is
//!   mirrored (`rend` on anything but `part` is malformed), but the payload is
//!   not decoded or validated.
//! * [`Op::Part`] carries no size, so `sz` on a part message is validated and
//!   then dropped; formatting a part never emits `sz`.
//! * `encfg` has no explicit `enabled` key on the wire. Go's `applyEncConfig`
//!   writes a config with `Enabled: true` for every `encfg` op, so a parsed
//!   [`Op::EncConfig`] is always `enabled: true` rather than inventing a key.

// The UI wires these bindings in as they land; until then nothing in the binary
// references this module, so silence dead-code lints for this self-contained API.
#![allow(dead_code)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// The wire version every recognised header starts with.
pub const HEADER_PREFIX: &str = "TDX1";

/// The root parent id. An absent or empty `p=` attribute means the drive root.
pub const ROOT_PARENT: &str = "";

/// Folder object ids carry this prefix.
pub const FOLDER_ID_PREFIX: &str = "d:";

/// File object ids carry this prefix.
pub const FILE_ID_PREFIX: &str = "f:";

/// SQLite's upper bound for a unix timestamp: `9999-12-31T23:59:59Z`.
const MAX_WIRE_UNIX_TIMESTAMP: i64 = 253_402_300_799;

/// One decoded drive operation.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    /// `t=f`: a file uploaded as one message with its own document/photo content.
    File {
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Stored size in bytes; `None` when the wire omitted `sz`. A `sz=0`
        /// attribute is `Some(0)` here, but Go cannot tell it from absent.
        size: Option<i64>,
        /// Unix seconds; `None` when the wire omitted `ts`.
        uploaded_at: Option<i64>,
        /// Whether the `enc=1` attribute is present.
        encrypted: bool,
        /// Cipher-format version: `0` when neither `enc` nor `ev` is carried,
        /// `1` when only `enc=1` is, otherwise the raw `ev` value.
        encryption_version: u32,
        /// Plaintext size (`psz`) for encrypted files; independent of `enc` in
        /// the wire format, exactly as in Go.
        plaintext_size: Option<i64>,
    },
    /// `t=mkdir`: create a folder.
    Folder {
        /// Folder object id (`d:` prefix required).
        obj: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
    },
    /// `t=meta`: name/parent/size update for an existing object.
    Meta {
        /// File object id (`f:` prefix required).
        obj: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Stored size, or `None` when `sz` is absent.
        size: Option<i64>,
        /// Unix seconds, or `None` when `ts` is absent.
        uploaded_at: Option<i64>,
        /// Whether the `enc=1` attribute is present.
        encrypted: bool,
        /// Cipher-format version; see [`Op::File`].
        encryption_version: u32,
        /// Plaintext size (`psz`), or `None` when absent.
        plaintext_size: Option<i64>,
    },
    /// `t=rename`: change an object's name.
    Rename {
        /// File or folder object id.
        obj: String,
        /// Decoded display name.
        name: String,
    },
    /// `t=move`: change an object's parent.
    Move {
        /// File or folder object id.
        obj: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
    },
    /// `t=tomb`: file removal.
    Tomb {
        /// File object id (`f:` prefix required).
        obj: String,
    },
    /// `t=rmdir`: folder removal.
    Rmdir {
        /// Folder object id (`d:` prefix required).
        obj: String,
    },
    /// `t=manifest`: a multipart file's logical identity. The message id of
    /// this op is the file.
    Manifest {
        /// Multipart group id.
        uuid: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Total number of parts; always at least one.
        part_count: u32,
        /// Logical (whole-file) size, or `None` when `sz` is absent.
        size: Option<i64>,
        /// Unix seconds, or `None` when `ts` is absent.
        uploaded_at: Option<i64>,
        /// Whether the `enc=1` attribute is present.
        encrypted: bool,
        /// Cipher-format version; see [`Op::File`].
        encryption_version: u32,
        /// Plaintext size (`psz`), or `None` when absent.
        plaintext_size: Option<i64>,
    },
    /// `t=part`: one hidden multipart body message.
    Part {
        /// Multipart group id.
        uuid: String,
        /// Zero-based position of this part.
        index: u32,
    },
    /// `t=fcommit`: commit a new logical file (writable-mount path).
    ///
    /// The message id of this op is the file's identity, `f:<message id>`,
    /// exactly as a `t=f` upload is. The content it names is *not* this
    /// message: it lives in the hidden body message `cmid` points at, or in
    /// the multipart group `u`/`pc` names.
    FileCommit {
        /// Decoded `oid`: the durable operation id, unique per commit.
        op_id: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Message id of the hidden content body, or `0` for multipart.
        content_msg_id: i64,
        /// Multipart group id, empty for a single-message commit.
        upload_uuid: String,
        /// Number of parts; `0` for a single-message commit.
        part_count: u32,
        /// Content hash the committer recorded, or `""`.
        content_hash: String,
        /// Stored size, or `None` when `sz` is absent.
        size: Option<i64>,
        /// Unix seconds, or `None` when `ts` is absent.
        uploaded_at: Option<i64>,
        /// Whether the `enc=1` attribute is present.
        encrypted: bool,
        /// Cipher-format version; see [`Op::File`].
        encryption_version: u32,
        /// Plaintext size (`psz`), or `None` when absent.
        plaintext_size: Option<i64>,
    },
    /// `t=freplace`: replace a committed file's content, bumping its revision.
    FileReplace {
        /// Decoded `oid`.
        op_id: String,
        /// File object id (`f:` prefix required).
        obj: String,
        /// Revision the writer saw; always positive.
        expected_revision: i64,
        /// Message id of the hidden content body, or `0` for multipart.
        content_msg_id: i64,
        /// Multipart group id, empty for a single-message commit.
        upload_uuid: String,
        /// Number of parts; `0` for a single-message commit.
        part_count: u32,
        /// Content hash the committer recorded, or `""`.
        content_hash: String,
        /// Unix seconds the replaced body is retained until; always positive.
        retained_until: i64,
        /// Stored size, or `None` when `sz` is absent.
        size: Option<i64>,
        /// Unix seconds, or `None` when `ts` is absent.
        uploaded_at: Option<i64>,
        /// Whether the `enc=1` attribute is present.
        encrypted: bool,
        /// Cipher-format version; see [`Op::File`].
        encryption_version: u32,
        /// Plaintext size (`psz`), or `None` when absent.
        plaintext_size: Option<i64>,
    },
    /// `t=dcommit`: create a folder (writable-mount path). Unlike `t=mkdir`,
    /// the object id is a durable uuid chosen by the writer, not a message id.
    FolderCommit {
        /// Decoded `oid`.
        op_id: String,
        /// Folder object id (`d:` prefix required).
        obj: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
    },
    /// `t=relocate`: move/rename an object, optionally overwriting the
    /// destination (which is trashed as a side effect).
    Relocate {
        /// Decoded `oid`.
        op_id: String,
        /// File or folder object id.
        obj: String,
        /// New parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Revision the writer saw; always positive.
        expected_revision: i64,
        /// Whether `ow=1` asked to overwrite a live destination.
        overwrite: bool,
        /// Destination object id, empty when no destination was named.
        destination_obj: String,
        /// Expected destination revision, or `0` when absent.
        expected_destination_revision: i64,
        /// Trash timestamp for the overwritten destination, or `0`.
        deleted_at: i64,
        /// Purge deadline for the overwritten destination, or `0`.
        purge_after: i64,
    },
    /// `t=trash`: move an object (and its live subtree) to the trash.
    Trash {
        /// Decoded `oid`.
        op_id: String,
        /// File or folder object id.
        obj: String,
        /// Revision the writer saw; always positive.
        expected_revision: i64,
        /// Unix seconds the object was deleted; always positive.
        deleted_at: i64,
        /// Unix seconds after which it may be purged; always positive.
        purge_after: i64,
    },
    /// `t=harddel`: purge an object (and its subtree) for good.
    HardDelete {
        /// Decoded `oid`.
        op_id: String,
        /// File or folder object id.
        obj: String,
        /// Revision the writer saw; always positive.
        expected_revision: i64,
    },
    /// `t=untrash`: restore a trashed object to an explicit parent and name.
    Untrash {
        /// Decoded `oid`.
        op_id: String,
        /// File or folder object id.
        obj: String,
        /// Parent folder id, or `""` for the drive root.
        parent: String,
        /// Decoded display name.
        name: String,
        /// Revision the writer saw; always positive.
        expected_revision: i64,
    },
    /// `t=encfg`: the drive's encryption config (salt/kdf/wrap/check/hint).
    EncConfig {
        /// Raw KDF salt.
        salt: Vec<u8>,
        /// Argon2 parameters JSON, URL-unescaped.
        params_json: String,
        /// Master key wrapped under the password-derived key.
        wrapped_master_key: Vec<u8>,
        /// Marker that verifies the unwrapped master key.
        key_check: Vec<u8>,
        /// Password hint, URL-unescaped and **not** trimmed (Go trims when it
        /// applies the op).
        hint: String,
        /// Config version; defaults to `1` when `cv` is absent.
        config_version: u32,
        /// Always `true`: the wire has no `enabled` key, but Go only ever
        /// applies an `encfg` op by enabling the config. See the module docs.
        enabled: bool,
    },
    /// A TDX1 header for an op this port does not act on. Callers ignore these.
    ///
    /// This covers both op types that do not exist yet and any type the port
    /// has never heard of. The versioned writable-mount ops (`fcommit`,
    /// `freplace`, `dcommit`, `relocate`, `trash`, `harddel`, `untrash`) have
    /// their own variants: a drive written through TDrive's mounted-write path
    /// stores its real structure in them.
    Other {
        /// The raw `t` value.
        op_type: String,
    },
}

impl Op {
    /// The wire `t` value this op was read from, for diagnostics and for
    /// reporting which operations a drive actually uses.
    pub fn type_name(&self) -> &str {
        match self {
            Op::File { .. } => "f",
            Op::Folder { .. } => "mkdir",
            Op::Meta { .. } => "meta",
            Op::Rename { .. } => "rename",
            Op::Move { .. } => "move",
            Op::Tomb { .. } => "tomb",
            Op::Rmdir { .. } => "rmdir",
            Op::Manifest { .. } => "manifest",
            Op::Part { .. } => "part",
            Op::FileCommit { .. } => "fcommit",
            Op::FileReplace { .. } => "freplace",
            Op::FolderCommit { .. } => "dcommit",
            Op::Relocate { .. } => "relocate",
            Op::Trash { .. } => "trash",
            Op::HardDelete { .. } => "harddel",
            Op::Untrash { .. } => "untrash",
            Op::EncConfig { .. } => "encfg",
            Op::Other { op_type } => op_type,
        }
    }
}

/// Why a caption could not be read as a drive op.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum OpError {
    /// The message carries no header line at all.
    ///
    /// [`parse`] does not return this: a missing header is the ordinary
    /// "not a drive message" case and is reported as `Ok(None)`. The variant
    /// is kept for API parity with Go's `ErrWireMissingHeader`.
    #[error("wire: header missing")]
    MissingHeader,
    /// The header is a TDX drive header, but not version 1.
    #[error("wire: unknown version")]
    BadVersion,
    /// The `t` attribute is present but empty.
    #[error("wire: unknown op type")]
    BadOpType,
    /// A known op is missing a required attribute or carries a bad value.
    #[error("wire: malformed header")]
    Malformed,
}

/// The header line only: everything before the first `'\n'`, trimmed.
///
/// Mirrors Go's `ExtractHeaderLine` plus the `strings.TrimSpace` that `Parse`
/// applies (so a trailing `\r` from a `\r\n` caption is dropped).
pub fn header_line(raw: &str) -> &str {
    let head = match raw.find('\n') {
        Some(end) => &raw[..end],
        None => raw,
    };
    head.trim()
}

/// Whether a caption looks like a TDX drive header at all.
///
/// Mirrors `sync.parse.tdriveHeaderCandidate`: the trimmed first line must start
/// with `TDX1`. Callers use this to decide whether a caption is a drive op that
/// failed to parse or an ordinary attachment that may be adopted.
pub fn is_header_candidate(raw: &str) -> bool {
    header_line(raw).starts_with(HEADER_PREFIX)
}

/// Parse a message caption into a drive operation.
///
/// `Ok(None)` means the message is not a drive op at all -- no caption, or a
/// caption that is not a TDX1 header. That is the common case for ordinary chat
/// messages and must not be treated as an error.
///
/// A TDX1 header with an unknown (non-empty) `t` becomes [`Op::Other`] so the
/// format stays forward-compatible. A malformed recognised op is
/// [`OpError::Malformed`].
pub fn parse(raw: &str) -> Result<Option<Op>, OpError> {
    let header = header_line(raw);
    if header.is_empty() {
        return Ok(None);
    }

    let mut fields = header.split('|');
    let version = fields.next().unwrap_or("");
    if version != HEADER_PREFIX {
        return if looks_like_drive_header(version) {
            Err(OpError::BadVersion)
        } else {
            Ok(None)
        };
    }

    // Go rejects a header with no attributes (`len(parts) < 2`) and any
    // attribute that lacks `=` or starts with one (`IndexByte(p, '=') <= 0`).
    let mut kv: Vec<(&str, &str)> = Vec::new();
    for field in fields {
        let Some(eq) = field.find('=') else {
            return Err(OpError::Malformed);
        };
        if eq == 0 {
            return Err(OpError::Malformed);
        }
        kv.push((&field[..eq], &field[eq + 1..]));
    }
    if kv.is_empty() {
        return Err(OpError::Malformed);
    }

    let Some(op_type) = kv_get(&kv, "t") else {
        return Err(OpError::Malformed);
    };
    if op_type.is_empty() {
        // Go's switch falls through to ErrWireBadOpType for a present-but-empty
        // `t=`; only a non-empty unknown type is forward-compatible.
        return Err(OpError::BadOpType);
    }

    let op = match op_type {
        "f" => {
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let attrs = parse_common_attrs(&kv, op_type)?;
            Op::File {
                parent,
                name,
                size: attrs.size,
                uploaded_at: attrs.uploaded_at,
                encrypted: attrs.encrypted,
                encryption_version: attrs.encryption_version,
                plaintext_size: attrs.plaintext_size,
            }
        }
        "mkdir" => {
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FOLDER_ID_PREFIX)?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Folder { obj, parent, name }
        }
        "meta" => {
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FILE_ID_PREFIX)?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let attrs = parse_common_attrs(&kv, op_type)?;
            Op::Meta {
                obj,
                parent,
                name,
                size: attrs.size,
                uploaded_at: attrs.uploaded_at,
                encrypted: attrs.encrypted,
                encryption_version: attrs.encryption_version,
                plaintext_size: attrs.plaintext_size,
            }
        }
        "rename" => {
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Rename { obj, name }
        }
        "move" => {
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Move { obj, parent }
        }
        "tomb" => {
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FILE_ID_PREFIX)?;
            parse_common_attrs(&kv, op_type)?;
            Op::Tomb { obj }
        }
        "rmdir" => {
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FOLDER_ID_PREFIX)?;
            parse_common_attrs(&kv, op_type)?;
            Op::Rmdir { obj }
        }
        "manifest" => {
            let uuid = parse_required_uuid(kv_get(&kv, "u").unwrap_or(""))?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let part_count = parse_part_count(kv_get(&kv, "pc").unwrap_or(""))?;
            let attrs = parse_common_attrs(&kv, op_type)?;
            Op::Manifest {
                uuid,
                parent,
                name,
                part_count,
                size: attrs.size,
                uploaded_at: attrs.uploaded_at,
                encrypted: attrs.encrypted,
                encryption_version: attrs.encryption_version,
                plaintext_size: attrs.plaintext_size,
            }
        }
        "part" => {
            let uuid = parse_required_uuid(kv_get(&kv, "u").unwrap_or(""))?;
            let index = parse_u32(kv_get(&kv, "pix").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Part { uuid, index }
        }
        "fcommit" => {
            let op_id = parse_writable_envelope(&kv)?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let content = parse_content_reference(&kv)?;
            let attrs = parse_common_attrs(&kv, op_type)?;
            Op::FileCommit {
                op_id,
                parent,
                name,
                content_msg_id: content.msg_id,
                upload_uuid: content.uuid,
                part_count: content.part_count,
                content_hash: content.hash,
                size: attrs.size,
                uploaded_at: attrs.uploaded_at,
                encrypted: attrs.encrypted,
                encryption_version: attrs.encryption_version,
                plaintext_size: attrs.plaintext_size,
            }
        }
        "freplace" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FILE_ID_PREFIX)?;
            let expected_revision = parse_positive_i64(kv_get(&kv, "rev").unwrap_or(""))?;
            let content = parse_content_reference(&kv)?;
            let retained_until = parse_positive_i64(kv_get(&kv, "retain").unwrap_or(""))?;
            let attrs = parse_common_attrs(&kv, op_type)?;
            Op::FileReplace {
                op_id,
                obj,
                expected_revision,
                content_msg_id: content.msg_id,
                upload_uuid: content.uuid,
                part_count: content.part_count,
                content_hash: content.hash,
                retained_until,
                size: attrs.size,
                uploaded_at: attrs.uploaded_at,
                encrypted: attrs.encrypted,
                encryption_version: attrs.encryption_version,
                plaintext_size: attrs.plaintext_size,
            }
        }
        "dcommit" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj(kv_get(&kv, "obj").unwrap_or(""), FOLDER_ID_PREFIX)?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::FolderCommit {
                op_id,
                obj,
                parent,
                name,
            }
        }
        "relocate" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let expected_revision = parse_positive_i64(kv_get(&kv, "rev").unwrap_or(""))?;
            // `ow` is a flag: exactly "1" means overwrite.
            let overwrite = kv_get(&kv, "ow") == Some("1");
            let destination_obj = match kv_get(&kv, "dst") {
                Some(raw) if !raw.is_empty() => parse_obj_any(raw)?,
                _ => String::new(),
            };
            let expected_destination_revision = match kv_get(&kv, "drev") {
                Some(raw) if !raw.is_empty() => parse_positive_i64(raw)?,
                _ => 0,
            };
            let deleted_at = match kv_get(&kv, "del") {
                Some(raw) if !raw.is_empty() => parse_positive_i64(raw)?,
                _ => 0,
            };
            let purge_after = match kv_get(&kv, "purge") {
                Some(raw) if !raw.is_empty() => parse_positive_i64(raw)?,
                _ => 0,
            };
            parse_common_attrs(&kv, op_type)?;
            Op::Relocate {
                op_id,
                obj,
                parent,
                name,
                expected_revision,
                overwrite,
                destination_obj,
                expected_destination_revision,
                deleted_at,
                purge_after,
            }
        }
        "trash" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let expected_revision = parse_positive_i64(kv_get(&kv, "rev").unwrap_or(""))?;
            let deleted_at = parse_positive_i64(kv_get(&kv, "del").unwrap_or(""))?;
            let purge_after = parse_positive_i64(kv_get(&kv, "purge").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Trash {
                op_id,
                obj,
                expected_revision,
                deleted_at,
                purge_after,
            }
        }
        "harddel" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let expected_revision = parse_positive_i64(kv_get(&kv, "rev").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::HardDelete {
                op_id,
                obj,
                expected_revision,
            }
        }
        "untrash" => {
            let op_id = parse_writable_envelope(&kv)?;
            let obj = parse_obj_any(kv_get(&kv, "obj").unwrap_or(""))?;
            let parent = parse_parent(kv_get(&kv, "p").unwrap_or(""))?;
            let name = parse_required_text(kv_get(&kv, "n").unwrap_or(""))?;
            let expected_revision = parse_positive_i64(kv_get(&kv, "rev").unwrap_or(""))?;
            parse_common_attrs(&kv, op_type)?;
            Op::Untrash {
                op_id,
                obj,
                parent,
                name,
                expected_revision,
            }
        }
        "encfg" => {
            let salt = parse_b64(kv_get(&kv, "salt").unwrap_or(""))?;
            let params_json = parse_required_text(kv_get(&kv, "kdf").unwrap_or(""))?;
            let wrapped_master_key = parse_b64(kv_get(&kv, "wrap").unwrap_or(""))?;
            let key_check = parse_b64(kv_get(&kv, "check").unwrap_or(""))?;
            // Go unescapes `hint` inline, so an empty `hint=` is a valid empty
            // hint (unlike `n`/`kdf`, which reject an empty value).
            let hint = match kv_get(&kv, "hint") {
                Some(raw) => unescape_optional(raw)?,
                None => String::new(),
            };
            let config_version = match kv_get(&kv, "cv") {
                Some(raw) => parse_u32(raw)?,
                None => 1,
            };
            parse_common_attrs(&kv, op_type)?;
            Op::EncConfig {
                salt,
                params_json,
                wrapped_master_key,
                key_check,
                hint,
                config_version,
                enabled: true,
            }
        }
        // Not an op this port acts on, and deliberately not validated: a
        // future op type must never make an old reader fail.
        _ => {
            return Ok(Some(Op::Other {
                op_type: op_type.to_string(),
            }));
        }
    };

    Ok(Some(op))
}

/// Format an operation as a caption header, mirroring Go's `Format`.
///
/// Like Go, only size/time/encryption attributes with a meaningful value are
/// written, so `Some(0)` sizes are omitted and an encrypted file always reports
/// `enc=1` with `ev` only for versions above 1.
pub fn format(op: &Op) -> String {
    let mut out = String::from(HEADER_PREFIX);
    out.push_str("|t=");

    match op {
        Op::File {
            parent,
            name,
            size,
            uploaded_at,
            encrypted,
            encryption_version,
            plaintext_size,
        } => {
            out.push_str("f|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            append_file_attrs(
                &mut out,
                *size,
                *uploaded_at,
                *encrypted,
                *encryption_version,
                *plaintext_size,
            );
        }
        Op::Folder { obj, parent, name } => {
            out.push_str("mkdir|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
        }
        Op::Meta {
            obj,
            parent,
            name,
            size,
            uploaded_at,
            encrypted,
            encryption_version,
            plaintext_size,
        } => {
            out.push_str("meta|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            append_file_attrs(
                &mut out,
                *size,
                *uploaded_at,
                *encrypted,
                *encryption_version,
                *plaintext_size,
            );
        }
        Op::Rename { obj, name } => {
            out.push_str("rename|obj=");
            out.push_str(obj);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
        }
        Op::Move { obj, parent } => {
            out.push_str("move|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
        }
        Op::Tomb { obj } => {
            out.push_str("tomb|obj=");
            out.push_str(obj);
        }
        Op::Rmdir { obj } => {
            out.push_str("rmdir|obj=");
            out.push_str(obj);
        }
        Op::Manifest {
            uuid,
            parent,
            name,
            part_count,
            size,
            uploaded_at,
            encrypted,
            encryption_version,
            plaintext_size,
        } => {
            out.push_str("manifest|u=");
            out.push_str(uuid);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            out.push_str("|pc=");
            out.push_str(&part_count.to_string());
            append_file_attrs(
                &mut out,
                *size,
                *uploaded_at,
                *encrypted,
                *encryption_version,
                *plaintext_size,
            );
        }
        // Go also writes `sz` here when the part carries a size; this port does
        // not retain it, so a formatted part never includes one.
        Op::Part { uuid, index } => {
            out.push_str("part|u=");
            out.push_str(uuid);
            out.push_str("|pix=");
            out.push_str(&index.to_string());
        }
        Op::FileCommit {
            op_id,
            parent,
            name,
            content_msg_id,
            upload_uuid,
            part_count,
            content_hash,
            size,
            uploaded_at,
            encrypted,
            encryption_version,
            plaintext_size,
        } => {
            out.push_str("fcommit");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            append_content_reference(
                &mut out,
                *content_msg_id,
                upload_uuid,
                *part_count,
                content_hash,
            );
            append_file_attrs(
                &mut out,
                *size,
                *uploaded_at,
                *encrypted,
                *encryption_version,
                *plaintext_size,
            );
        }
        Op::FileReplace {
            op_id,
            obj,
            expected_revision,
            content_msg_id,
            upload_uuid,
            part_count,
            content_hash,
            retained_until,
            size,
            uploaded_at,
            encrypted,
            encryption_version,
            plaintext_size,
        } => {
            out.push_str("freplace");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|rev=");
            out.push_str(&expected_revision.to_string());
            append_content_reference(
                &mut out,
                *content_msg_id,
                upload_uuid,
                *part_count,
                content_hash,
            );
            out.push_str("|retain=");
            out.push_str(&retained_until.to_string());
            append_file_attrs(
                &mut out,
                *size,
                *uploaded_at,
                *encrypted,
                *encryption_version,
                *plaintext_size,
            );
        }
        Op::FolderCommit {
            op_id,
            obj,
            parent,
            name,
        } => {
            out.push_str("dcommit");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
        }
        Op::Relocate {
            op_id,
            obj,
            parent,
            name,
            expected_revision,
            overwrite,
            destination_obj,
            expected_destination_revision,
            deleted_at,
            purge_after,
        } => {
            out.push_str("relocate");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            out.push_str("|rev=");
            out.push_str(&expected_revision.to_string());
            if *overwrite {
                out.push_str("|ow=1");
            }
            if !destination_obj.is_empty() {
                out.push_str("|dst=");
                out.push_str(destination_obj);
            }
            if *expected_destination_revision > 0 {
                out.push_str("|drev=");
                out.push_str(&expected_destination_revision.to_string());
            }
            if *deleted_at > 0 {
                out.push_str("|del=");
                out.push_str(&deleted_at.to_string());
            }
            if *purge_after > 0 {
                out.push_str("|purge=");
                out.push_str(&purge_after.to_string());
            }
        }
        Op::Trash {
            op_id,
            obj,
            expected_revision,
            deleted_at,
            purge_after,
        } => {
            out.push_str("trash");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|rev=");
            out.push_str(&expected_revision.to_string());
            out.push_str("|del=");
            out.push_str(&deleted_at.to_string());
            out.push_str("|purge=");
            out.push_str(&purge_after.to_string());
        }
        Op::HardDelete {
            op_id,
            obj,
            expected_revision,
        } => {
            out.push_str("harddel");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|rev=");
            out.push_str(&expected_revision.to_string());
        }
        Op::Untrash {
            op_id,
            obj,
            parent,
            name,
            expected_revision,
        } => {
            out.push_str("untrash");
            append_writable_envelope(&mut out, op_id);
            out.push_str("|obj=");
            out.push_str(obj);
            out.push_str("|p=");
            out.push_str(parent);
            out.push_str("|n=");
            out.push_str(&query_escape(name));
            out.push_str("|rev=");
            out.push_str(&expected_revision.to_string());
        }
        Op::EncConfig {
            salt,
            params_json,
            wrapped_master_key,
            key_check,
            hint,
            config_version,
            // No `enabled` key exists on the wire; see the module docs.
            ..
        } => {
            out.push_str("encfg|salt=");
            out.push_str(&b64_encode(salt));
            out.push_str("|kdf=");
            out.push_str(&query_escape(params_json));
            out.push_str("|wrap=");
            out.push_str(&b64_encode(wrapped_master_key));
            out.push_str("|check=");
            out.push_str(&b64_encode(key_check));
            if !hint.is_empty() {
                out.push_str("|hint=");
                out.push_str(&query_escape(hint));
            }
            if *config_version > 1 {
                out.push_str("|cv=");
                out.push_str(&config_version.to_string());
            }
        }
        Op::Other { op_type } => {
            out.push_str(op_type);
        }
    }

    out
}

/// The `t=f` caption for a plain (unencrypted) single-message upload.
///
/// A convenience over [`format`] for the common write path. `size` and
/// `uploaded_at` are omitted from the header when they are not positive, just
/// as Go's `appendFileAttrs` does.
pub fn format_file(parent: &str, name: &str, size: i64, uploaded_at: i64) -> String {
    format(&Op::File {
        parent: parent.to_string(),
        name: name.to_string(),
        size: Some(size),
        uploaded_at: Some(uploaded_at),
        encrypted: false,
        encryption_version: 0,
        plaintext_size: None,
    })
}

/// The op id the original's `DeterministicOpID` writes for a trash, restore or
/// purge: the purpose, a hyphen, and the first 16 bytes of a SHA-256 over a
/// domain-separated identity.
///
/// The identity includes the revision the writer saw, so a delete is a
/// different operation from the restore that follows it, and re-running one
/// produces the same id rather than a second entry.
pub fn deterministic_op_id(purpose: &str, object_id: &str, revision: i64) -> String {
    use sha2::{Digest, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(b"tdrive.trash.v1\0");
    hasher.update(purpose.as_bytes());
    hasher.update(b"\0");
    hasher.update(object_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(revision.to_string().as_bytes());
    let digest = hasher.finalize();

    let mut out = String::with_capacity(purpose.len() + 1 + 32);
    out.push_str(purpose);
    out.push('-');
    for byte in &digest[..16] {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// A delete: the original's `projection.TrashOp`, which hides the object and
/// its subtree without touching Telegram.
///
/// `revision` is the revision the writer saw, which the drive compares against
/// its own before it applies anything.
pub fn trash_op(object_id: &str, revision: i64, now: i64, retention_secs: i64) -> Op {
    Op::Trash {
        op_id: deterministic_op_id("trash", object_id, revision),
        obj: object_id.to_string(),
        expected_revision: revision,
        deleted_at: now,
        purge_after: now + retention_secs,
    }
}

/// A restore: the original's `projection.RestoreOp`, which reopens a trashed
/// object under an explicit parent and name. The original home may be gone, and
/// the name may be taken by the time it comes back, so both are written out
/// rather than inferred.
pub fn restore_op(object_id: &str, revision: i64, parent: &str, name: &str) -> Op {
    Op::Untrash {
        op_id: deterministic_op_id("restore", object_id, revision),
        obj: object_id.to_string(),
        parent: parent.to_string(),
        name: name.to_string(),
        expected_revision: revision,
    }
}

/// A purge: the original's `projection.HardDeleteOp`, which removes a trashed
/// object for good. Its bodies are deleted separately, because the marker only
/// describes the namespace.
pub fn purge_op(object_id: &str, revision: i64) -> Op {
    Op::HardDelete {
        op_id: deterministic_op_id("purge", object_id, revision),
        obj: object_id.to_string(),
        expected_revision: revision,
    }
}

/// `f:<v>`: build a file object id, matching Go's `FileIDPrefix`.
pub fn file_obj_id(v: &str) -> String {
    format!("{FILE_ID_PREFIX}{v}")
}

/// `d:<v>`: build a folder object id, matching Go's `FolderIDPrefix`.
pub fn folder_obj_id(v: &str) -> String {
    format!("{FOLDER_ID_PREFIX}{v}")
}

/// Whether a string is a file object id (`f:` prefix plus a non-empty id).
pub fn is_file_obj(obj: &str) -> bool {
    obj.len() > FILE_ID_PREFIX.len() && obj.starts_with(FILE_ID_PREFIX)
}

/// Whether a string is a folder object id (`d:` prefix plus a non-empty id).
pub fn is_folder_obj(obj: &str) -> bool {
    obj.len() > FOLDER_ID_PREFIX.len() && obj.starts_with(FOLDER_ID_PREFIX)
}

/// Whether a first header field names a TDX version this port does not know:
/// `TDX` followed by at least one digit (`TDX2`, `TDX1x`). A caption such as
/// `TDX` or `TDXfoo` is an ordinary message instead.
fn looks_like_drive_header(version: &str) -> bool {
    version
        .strip_prefix("TDX")
        .and_then(|rest| rest.as_bytes().first())
        .is_some_and(u8::is_ascii_digit)
}

/// Wire attributes shared by the file-bearing ops.
struct CommonAttrs {
    size: Option<i64>,
    uploaded_at: Option<i64>,
    encrypted: bool,
    encryption_version: u32,
    plaintext_size: Option<i64>,
}

/// Read the last occurrence of a `key=value` attribute.
///
/// Go stores attributes in a map, where a repeated key keeps the last value.
fn kv_get<'a>(kv: &[(&'a str, &'a str)], key: &str) -> Option<&'a str> {
    kv.iter()
        .rev()
        .find(|(candidate, _)| *candidate == key)
        .map(|(_, value)| *value)
}

/// Parse `sz`/`ts`/`enc`/`ev`/`psz`/`rend` for any op this port acts on.
///
/// Go validates these for every recognised op, including ones that do not keep
/// them (a bad `sz` on a `mkdir` is still an error), so the same validation runs
/// here and the result is dropped where the variant has no field.
fn parse_common_attrs(kv: &[(&str, &str)], op_type: &str) -> Result<CommonAttrs, OpError> {
    let size = match kv_get(kv, "sz") {
        Some(raw) => Some(parse_non_negative_i64(raw)?),
        None => None,
    };
    let uploaded_at = match kv_get(kv, "ts") {
        Some(raw) => {
            let n = parse_i64(raw)?;
            if n < 0 || n > MAX_WIRE_UNIX_TIMESTAMP {
                return Err(OpError::Malformed);
            }
            Some(n)
        }
        None => None,
    };

    let mut encrypted = false;
    let mut encryption_version = 0;
    // `enc` is a flag: any value other than exactly "1" is ignored, not an error.
    if kv_get(kv, "enc") == Some("1") {
        encrypted = true;
        encryption_version = 1;
    }
    if let Some(raw) = kv_get(kv, "ev") {
        encryption_version = parse_u32(raw)?;
    }
    let plaintext_size = match kv_get(kv, "psz") {
        Some(raw) => Some(parse_non_negative_i64(raw)?),
        None => None,
    };

    // `rend` only decorates hidden part messages. Go additionally decodes and
    // validates the payload; this port mirrors only the op-type guard.
    if kv_get(kv, "rend").is_some() && op_type != "part" {
        return Err(OpError::Malformed);
    }

    Ok(CommonAttrs {
        size,
        uploaded_at,
        encrypted,
        encryption_version,
        plaintext_size,
    })
}

/// Go's `setParent`: empty/missing means the drive root, otherwise it must
/// already carry the folder id prefix.
fn parse_parent(raw: &str) -> Result<String, OpError> {
    if raw.is_empty() {
        return Ok(ROOT_PARENT.to_string());
    }
    if !raw.starts_with(FOLDER_ID_PREFIX) {
        return Err(OpError::Malformed);
    }
    Ok(raw.to_string())
}

/// Go's `setObj`: required, and it must carry the given id prefix.
fn parse_obj(raw: &str, required_prefix: &str) -> Result<String, OpError> {
    if raw.is_empty() || !raw.starts_with(required_prefix) {
        return Err(OpError::Malformed);
    }
    Ok(raw.to_string())
}

/// Go's `setObjAny`: required, and it must carry either id prefix.
fn parse_obj_any(raw: &str) -> Result<String, OpError> {
    if raw.is_empty() || !(raw.starts_with(FOLDER_ID_PREFIX) || raw.starts_with(FILE_ID_PREFIX)) {
        return Err(OpError::Malformed);
    }
    Ok(raw.to_string())
}

/// Go's `setName` and `setRequiredRaw`: required and percent-encoded.
fn parse_required_text(raw: &str) -> Result<String, OpError> {
    if raw.is_empty() {
        return Err(OpError::Malformed);
    }
    unescape_optional(raw)
}

/// Go's `setUUID`: required, and used verbatim (uuids are not escaped).
fn parse_required_uuid(raw: &str) -> Result<String, OpError> {
    if raw.is_empty() {
        return Err(OpError::Malformed);
    }
    Ok(raw.to_string())
}

/// Go's `setB64`: required, non-empty, base64 RawURLEncoding.
fn parse_b64(raw: &str) -> Result<Vec<u8>, OpError> {
    if raw.is_empty() {
        return Err(OpError::Malformed);
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(raw)
        .map_err(|_| OpError::Malformed)?;
    if decoded.is_empty() {
        return Err(OpError::Malformed);
    }
    Ok(decoded)
}

/// Go's `setPartCount`: a positive count.
fn parse_part_count(raw: &str) -> Result<u32, OpError> {
    let count = parse_u32(raw)?;
    if count == 0 {
        return Err(OpError::Malformed);
    }
    Ok(count)
}

/// Go's `setPositiveInt64`: strictly positive, used by the revision and
/// timestamp attributes of the versioned writable ops.
fn parse_positive_i64(raw: &str) -> Result<i64, OpError> {
    let n = parse_i64(raw)?;
    if n <= 0 {
        return Err(OpError::Malformed);
    }
    Ok(n)
}

/// Go's `setWritableEnvelope`: the `v=1` protocol gate plus a decoded `oid`.
fn parse_writable_envelope(kv: &[(&str, &str)]) -> Result<String, OpError> {
    let version = parse_i64(kv_get(kv, "v").unwrap_or(""))?;
    if version != 1 {
        return Err(OpError::Malformed);
    }
    let raw = kv_get(kv, "oid").unwrap_or("");
    if raw.is_empty() {
        return Err(OpError::Malformed);
    }
    let op_id = unescape_optional(raw)?;
    if op_id.is_empty() {
        return Err(OpError::Malformed);
    }
    Ok(op_id)
}

/// How a committed file's bytes are addressed. Go's `setContentReference`
/// accepts exactly two shapes: one whole-content message, or a multipart group.
struct ContentReference {
    msg_id: i64,
    uuid: String,
    part_count: u32,
    hash: String,
}

/// Go's `setContentReference`.
fn parse_content_reference(kv: &[(&str, &str)]) -> Result<ContentReference, OpError> {
    let msg_id = match kv_get(kv, "cmid") {
        Some(raw) if !raw.is_empty() => parse_positive_i64(raw)?,
        _ => 0,
    };
    let uuid = match kv_get(kv, "u") {
        Some(raw) if !raw.is_empty() => parse_required_uuid(raw)?,
        _ => String::new(),
    };
    let part_count = match kv_get(kv, "pc") {
        Some(raw) if !raw.is_empty() => parse_part_count(raw)?,
        _ => 0,
    };
    let hash = match kv_get(kv, "hash") {
        Some(raw) if !raw.is_empty() => unescape_optional(raw)?,
        _ => String::new(),
    };
    // Exactly one of the two shapes, and a multipart reference needs both its
    // group id and a positive part count.
    if msg_id <= 0 && (uuid.is_empty() || part_count == 0) {
        return Err(OpError::Malformed);
    }
    if msg_id > 0 && (!uuid.is_empty() || part_count != 0) {
        return Err(OpError::Malformed);
    }
    Ok(ContentReference {
        msg_id,
        uuid,
        part_count,
        hash,
    })
}

/// Go's `strconv.ParseInt(raw, 10, 64)`.
fn parse_i64(raw: &str) -> Result<i64, OpError> {
    raw.parse::<i64>().map_err(|_| OpError::Malformed)
}

/// A non-negative `int64` value (`sz`, `psz`).
fn parse_non_negative_i64(raw: &str) -> Result<i64, OpError> {
    let n = parse_i64(raw)?;
    if n < 0 {
        return Err(OpError::Malformed);
    }
    Ok(n)
}

/// Go's `strconv.Atoi` narrowed to the `u32` fields of this API.
fn parse_u32(raw: &str) -> Result<u32, OpError> {
    let n = parse_i64(raw)?;
    if n < 0 || n > u32::MAX as i64 {
        return Err(OpError::Malformed);
    }
    Ok(n as u32)
}

/// Go's `url.QueryUnescape`, returning `Err` on a bad escape or non-UTF-8 text.
fn unescape_optional(raw: &str) -> Result<String, OpError> {
    query_unescape(raw).ok_or(OpError::Malformed)
}

/// Go's `url.QueryUnescape`: `+` means space, `%XX` decodes one byte.
///
/// Returns `None` for a truncated or non-hex escape, and for byte sequences
/// that are not valid UTF-8 (Go's `string` can hold those; ours cannot).
fn query_unescape(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                if i + 2 >= bytes.len() {
                    return None;
                }
                let high = hex_value(bytes[i + 1])?;
                let low = hex_value(bytes[i + 2])?;
                out.push((high << 4) | low);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Go's `url.QueryEscape`: unreserved characters stay literal, space becomes
/// `+`, and every other byte becomes `%XX` with uppercase hex.
fn query_escape(raw: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(raw.len());
    for &byte in raw.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

/// One hex digit of a `%XX` escape.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// base64 RawURLEncoding without padding.
fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Go's `appendWritableEnvelope`. The port only ever writes protocol version 1.
fn append_writable_envelope(out: &mut String, op_id: &str) {
    out.push_str("|v=1|oid=");
    out.push_str(&query_escape(op_id));
}

/// Go's `appendContentReference`: one whole-content message, or a multipart
/// group, followed by the hash when the committer recorded one.
fn append_content_reference(
    out: &mut String,
    msg_id: i64,
    uuid: &str,
    part_count: u32,
    hash: &str,
) {
    if msg_id > 0 {
        out.push_str("|cmid=");
        out.push_str(&msg_id.to_string());
    } else {
        out.push_str("|u=");
        out.push_str(uuid);
        out.push_str("|pc=");
        out.push_str(&part_count.to_string());
    }
    if !hash.is_empty() {
        out.push_str("|hash=");
        out.push_str(&query_escape(hash));
    }
}

/// Go's `appendFileAttrs`: size, time and the encryption block.
fn append_file_attrs(
    out: &mut String,
    size: Option<i64>,
    uploaded_at: Option<i64>,
    encrypted: bool,
    encryption_version: u32,
    plaintext_size: Option<i64>,
) {
    if let Some(size) = size {
        if size > 0 {
            out.push_str("|sz=");
            out.push_str(&size.to_string());
        }
    }
    if let Some(uploaded_at) = uploaded_at {
        if uploaded_at > 0 {
            out.push_str("|ts=");
            out.push_str(&uploaded_at.to_string());
        }
    }
    if encrypted {
        out.push_str("|enc=1");
        let version = if encryption_version == 0 {
            1
        } else {
            encryption_version
        };
        if version != 1 {
            out.push_str("|ev=");
            out.push_str(&version.to_string());
        }
        if let Some(plaintext_size) = plaintext_size {
            if plaintext_size > 0 {
                out.push_str("|psz=");
                out.push_str(&plaintext_size.to_string());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Parse a caption that must be a drive op.
    fn op(raw: &str) -> Op {
        match parse(raw) {
            Ok(Some(op)) => op,
            other => panic!("parse({raw:?}) = {other:?}, want an op"),
        }
    }

    /// Parse a caption that must be malformed.
    fn malformed(raw: &str) {
        assert_eq!(parse(raw), Err(OpError::Malformed), "parse({raw:?})");
    }

    fn plain_file(name: &str) -> Op {
        Op::File {
            parent: ROOT_PARENT.to_string(),
            name: name.to_string(),
            size: None,
            uploaded_at: None,
            encrypted: false,
            encryption_version: 0,
            plaintext_size: None,
        }
    }

    #[test]
    fn legacy_encrypted_upload_vector() {
        // Vector from backend/media/raw_drive_test.go.
        assert_eq!(
            op("TDX1|t=f|p=|n=notes.md|sz=20|ts=1|enc=1|psz=4|ev=1"),
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "notes.md".to_string(),
                size: Some(20),
                uploaded_at: Some(1),
                encrypted: true,
                encryption_version: 1,
                plaintext_size: Some(4),
            }
        );
    }

    #[test]
    fn root_and_nested_parents() {
        // From wire_test.go TestParseFileUploadRoot / TestParseFileUpload.
        assert_eq!(
            op("TDX1|t=f|p=|n=hello.txt"),
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "hello.txt".to_string(),
                size: None,
                uploaded_at: None,
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
        let nested = op("TDX1|t=f|p=d:abc|n=sunset.jpg|sz=4321|ts=99\nTDrive: sunset.jpg");
        assert_eq!(
            nested,
            Op::File {
                parent: "d:abc".to_string(),
                name: "sunset.jpg".to_string(),
                size: Some(4321),
                uploaded_at: Some(99),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
        // A nested parent that never carried `p` is still the root.
        assert_eq!(
            op("TDX1|t=mkdir|obj=d:goa-uuid|n=Goa"),
            Op::Folder {
                obj: "d:goa-uuid".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "Goa".to_string(),
            }
        );
        // `d:` alone is a valid (if odd) parent id, exactly as Go's HasPrefix.
        assert_eq!(
            op("TDX1|t=move|obj=f:7|p=d:"),
            Op::Move {
                obj: "f:7".to_string(),
                parent: "d:".to_string(),
            }
        );
    }

    #[test]
    fn format_file_matches_the_write_path() {
        assert_eq!(
            format_file("d:abc", "sunset.jpg", 4321, 99),
            "TDX1|t=f|p=d:abc|n=sunset.jpg|sz=4321|ts=99"
        );
        // A root upload, with a size/time that carry no information, omits them.
        assert_eq!(
            format_file("", "hello.txt", 0, 0),
            "TDX1|t=f|p=|n=hello.txt"
        );
        assert_eq!(
            op(&format_file("", "Goa Trip.md", 7, 8)),
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "Goa Trip.md".to_string(),
                size: Some(7),
                uploaded_at: Some(8),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
    }

    #[test]
    fn names_round_trip_through_query_escape() {
        // Go's net/url QueryEscape output, verified against the Go standard library.
        let cases = [
            ("Goa Trip", "Goa+Trip"),
            ("spaces here.txt", "spaces+here.txt"),
            ("a+b", "a%2Bb"),
            ("100%", "100%25"),
            ("x!y'z(w)*", "x%21y%27z%28w%29%2A"),
            ("file#1[2]{3}.txt", "file%231%5B2%5D%7B3%7D.txt"),
            ("café & tea ☕.txt", "caf%C3%A9+%26+tea+%E2%98%95.txt"),
            ("~-_.", "~-_."),
        ];
        for (name, escaped) in cases {
            let raw = format(&plain_file(name));
            assert_eq!(raw, format!("TDX1|t=f|p=|n={escaped}"), "format({name:?})");
            assert_eq!(op(&raw), plain_file(name), "round trip {name:?}");
        }
        // `+` is a space, but `%2B` is a literal plus.
        assert_eq!(
            op("TDX1|t=f|p=|n=spaces+here.txt"),
            plain_file("spaces here.txt")
        );
        assert_eq!(op("TDX1|t=f|p=|n=a%2Bb"), plain_file("a+b"));
        // Invalid UTF-8 cannot be represented by String, so it is malformed
        // here even though Go would accept the raw byte.
        malformed("TDX1|t=f|p=|n=%C3");
    }

    #[test]
    fn non_drive_captions_parse_as_none() {
        assert_eq!(parse(""), Ok(None));
        assert_eq!(parse("\nbody only"), Ok(None));
        assert_eq!(parse("   "), Ok(None));
        assert_eq!(parse("hello world"), Ok(None));
        assert_eq!(parse("TDrive: sunset.jpg"), Ok(None));
        assert_eq!(parse("just|pipes|and|stuff"), Ok(None));
        assert_eq!(parse("TDX|t=f|p=|n=x"), Ok(None));
        assert_eq!(parse("TDXfoo|t=f|p=|n=x"), Ok(None));
        assert!(!is_header_candidate("hello world"));
        assert!(is_header_candidate("  TDX1|t=f\nbody"));
    }

    #[test]
    fn header_without_op_type_is_malformed() {
        assert_eq!(parse("TDX1"), Err(OpError::Malformed));
        assert_eq!(parse("TDX1|"), Err(OpError::Malformed));
        assert_eq!(parse("TDX1|p=|n=x"), Err(OpError::Malformed));
        assert_eq!(parse("TDX1|t=f|p=|n=x|=bad"), Err(OpError::Malformed));
        // A trailing or bare field with no `=` is malformed, matching Go.
        assert_eq!(parse("TDX1|t=f|p=|n=x|"), Err(OpError::Malformed));
        assert_eq!(parse("TDX1|t=f|p=|n=x|bare"), Err(OpError::Malformed));
    }

    #[test]
    fn parses_every_supported_op_type() {
        let upload = op("TDX1|t=f|p=d:abc|n=sunset.jpg|sz=4321|ts=99\ncomment");
        assert!(matches!(upload, Op::File { .. }));

        assert_eq!(
            op("TDX1|t=mkdir|obj=d:goa-uuid|p=|n=Goa%20Trip"),
            Op::Folder {
                obj: "d:goa-uuid".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "Goa Trip".to_string(),
            }
        );
        assert_eq!(
            op("TDX1|t=meta|obj=f:9|p=|n=legacy.bin|sz=2048|ts=88"),
            Op::Meta {
                obj: "f:9".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "legacy.bin".to_string(),
                size: Some(2048),
                uploaded_at: Some(88),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
        assert_eq!(
            op("TDX1|t=rename|obj=f:42|n=new.png"),
            Op::Rename {
                obj: "f:42".to_string(),
                name: "new.png".to_string(),
            }
        );
        assert_eq!(
            op("TDX1|t=rename|obj=d:abc|n=Goa"),
            Op::Rename {
                obj: "d:abc".to_string(),
                name: "Goa".to_string(),
            }
        );
        assert_eq!(
            op("TDX1|t=move|obj=f:7|p=d:abc"),
            Op::Move {
                obj: "f:7".to_string(),
                parent: "d:abc".to_string(),
            }
        );
        assert_eq!(
            op("TDX1|t=tomb|obj=f:99"),
            Op::Tomb {
                obj: "f:99".to_string(),
            }
        );
        assert_eq!(
            op("TDX1|t=rmdir|obj=d:gone"),
            Op::Rmdir {
                obj: "d:gone".to_string(),
            }
        );
        assert_eq!(
            op(
                "TDX1|t=manifest|u=abc-123|p=d:folder|n=My+File.mkv|pc=3|sz=5000|ts=1700|enc=1|psz=4096"
            ),
            Op::Manifest {
                uuid: "abc-123".to_string(),
                parent: "d:folder".to_string(),
                name: "My File.mkv".to_string(),
                part_count: 3,
                size: Some(5000),
                uploaded_at: Some(1700),
                encrypted: true,
                encryption_version: 1,
                plaintext_size: Some(4096),
            }
        );
        // From backend/projection/raw_attachment_test.go: a hidden part message.
        assert_eq!(
            op("TDX1|t=part|u=upload|pix=0|sz=12"),
            Op::Part {
                uuid: "upload".to_string(),
                index: 0,
            }
        );
        assert!(matches!(
            op("TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s"),
            Op::EncConfig { .. }
        ));
    }

    #[test]
    fn unknown_op_type_is_other() {
        // From wire_test.go TestParseUnknownOpType; here forward-compatible.
        assert_eq!(
            op("TDX1|t=quux|obj=d:x|p=|n=foo"),
            Op::Other {
                op_type: "quux".to_string(),
            }
        );
        assert_eq!(
            format(&Op::Other {
                op_type: "quux".to_string(),
            }),
            "TDX1|t=quux"
        );
    }

    /// The mounted-write path is not a future op any more: a drive written
    /// through it stores its real structure in these headers, so each one has
    /// to parse into its own variant.
    #[test]
    fn writable_mount_ops_are_modelled() {
        assert!(matches!(
            op("TDX1|t=fcommit|v=1|oid=op|p=|n=a|cmid=1"),
            Op::FileCommit { .. }
        ));
        assert!(matches!(
            op("TDX1|t=dcommit|v=1|oid=op|obj=d:x|p=|n=a"),
            Op::FolderCommit { .. }
        ));
        assert!(matches!(
            op("TDX1|t=relocate|v=1|oid=op|obj=f:1|p=|n=a|rev=1"),
            Op::Relocate { .. }
        ));
        assert!(matches!(
            op("TDX1|t=trash|v=1|oid=op|obj=f:1|rev=1|del=2|purge=3"),
            Op::Trash { .. }
        ));
        assert!(matches!(
            op("TDX1|t=harddel|v=1|oid=op|obj=f:1|rev=1"),
            Op::HardDelete { .. }
        ));
        assert!(matches!(
            op("TDX1|t=untrash|v=1|oid=op|obj=f:1|p=|n=a|rev=1"),
            Op::Untrash { .. }
        ));
        assert!(matches!(
            op("TDX1|t=freplace|v=1|oid=op|obj=f:1|rev=1|cmid=2|retain=3"),
            Op::FileReplace { .. }
        ));
    }

    #[test]
    fn unknown_op_type_skips_attribute_validation() {
        // A future op type must never fail an old reader, even with a value this
        // port would reject for a known op.
        assert_eq!(
            parse("TDX1|t=quux|sz=-1|ts=-1|rend=x"),
            Ok(Some(Op::Other {
                op_type: "quux".to_string(),
            }))
        );
    }

    #[test]
    fn unsupported_version_is_reported() {
        // From wire_test.go TestParseUnknownVersion.
        assert_eq!(
            parse("TDX2|t=mkdir|obj=d:x|p=|n=foo"),
            Err(OpError::BadVersion)
        );
        assert_eq!(parse("TDX1x|t=f"), Err(OpError::BadVersion));
    }

    #[test]
    fn empty_op_type_is_bad_op_type() {
        // Go's switch falls through to ErrWireBadOpType for a present, empty `t`.
        assert_eq!(parse("TDX1|t="), Err(OpError::BadOpType));
        assert_eq!(parse("TDX1|t=|p=|n=x"), Err(OpError::BadOpType));
    }

    #[test]
    fn encryption_config_round_trip() {
        // Mirrors wire_test.go TestParseEncryptionConfig through Format/Parse.
        let want = Op::EncConfig {
            salt: b"salt".to_vec(),
            params_json: r#"{"memory":65536,"time":3}"#.to_string(),
            wrapped_master_key: b"wrapped".to_vec(),
            key_check: b"check".to_vec(),
            hint: "pet name".to_string(),
            config_version: 1,
            enabled: true,
        };
        let raw = format(&want);
        assert_eq!(
            raw,
            "TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22memory%22%3A65536%2C%22time%22%3A3%7D|\
             wrap=d3JhcHBlZA|check=Y2hlY2s|hint=pet+name"
        );
        assert_eq!(op(&raw), want);
        // Unknown attributes on an encfg are ignored, as in Go's map lookup.
        assert_eq!(
            op(
                "TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s|p=raw|n="
            ),
            Op::EncConfig {
                salt: b"salt".to_vec(),
                params_json: r#"{"m":1}"#.to_string(),
                wrapped_master_key: b"wrapped".to_vec(),
                key_check: b"check".to_vec(),
                hint: String::new(),
                config_version: 1,
                enabled: true,
            }
        );
    }

    #[test]
    fn encryption_config_uses_raw_url_base64() {
        assert_eq!(
            op("TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s"),
            Op::EncConfig {
                salt: b"salt".to_vec(),
                params_json: r#"{"m":1}"#.to_string(),
                wrapped_master_key: b"wrapped".to_vec(),
                key_check: b"check".to_vec(),
                hint: String::new(),
                config_version: 1,
                enabled: true,
            }
        );
        // A value whose raw bytes span all 64 symbols with no padding still decodes.
        let bytes: Vec<u8> = (0..=255u8).collect();
        assert!(!b64_encode(&bytes).contains('='));
        assert_eq!(parse_b64(&b64_encode(&bytes)), Ok(bytes));
        // Padded base64 is not RawURLEncoding.
        malformed("TDX1|t=encfg|salt=c2FsdA==|kdf=%7B%7D|wrap=d3JhcHBlZA|check=Y2hlY2s");
        // Standard-alphabet symbols are not accepted either.
        malformed("TDX1|t=encfg|salt=c2+s/A|kdf=%7B%7D|wrap=d3JhcHBlZA|check=Y2hlY2s");
        // Empty material is required to be present.
        malformed("TDX1|t=encfg|salt=|kdf=%7B%7D|wrap=d3JhcHBlZA|check=Y2hlY2s");
        malformed("TDX1|t=encfg|salt=c2FsdA|kdf=|wrap=d3JhcHBlZA|check=Y2hlY2s");
        malformed("TDX1|t=encfg|salt=c2FsdA|kdf=%7B%7D|wrap=|check=Y2hlY2s");
        malformed("TDX1|t=encfg|salt=c2FsdA|kdf=%7B%7D|wrap=d3JhcHBlZA");
    }

    #[test]
    fn encryption_config_hint_is_unescaped() {
        let parsed = op(
            "TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s|hint=pet+name",
        );
        assert_eq!(
            parsed,
            Op::EncConfig {
                salt: b"salt".to_vec(),
                params_json: r#"{"m":1}"#.to_string(),
                wrapped_master_key: b"wrapped".to_vec(),
                key_check: b"check".to_vec(),
                hint: "pet name".to_string(),
                config_version: 1,
                enabled: true,
            }
        );
        // An empty hint is valid and stays empty; the hint is not trimmed here.
        let empty = op(
            "TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s|hint=",
        );
        assert_eq!(
            empty,
            Op::EncConfig {
                salt: b"salt".to_vec(),
                params_json: r#"{"m":1}"#.to_string(),
                wrapped_master_key: b"wrapped".to_vec(),
                key_check: b"check".to_vec(),
                hint: String::new(),
                config_version: 1,
                enabled: true,
            }
        );
        // A bad escape in a present hint is malformed.
        malformed("TDX1|t=encfg|salt=c2FsdA|kdf=%7B%7D|wrap=d3JhcHBlZA|check=Y2hlY2s|hint=%zz");
    }

    #[test]
    fn encryption_config_version_defaults_to_one() {
        let base = "TDX1|t=encfg|salt=c2FsdA|kdf=%7B%22m%22%3A1%7D|wrap=d3JhcHBlZA|check=Y2hlY2s";
        assert!(matches!(
            op(base),
            Op::EncConfig {
                config_version: 1,
                ..
            }
        ));
        assert!(matches!(
            op(&format!("{base}|cv=2")),
            Op::EncConfig {
                config_version: 2,
                ..
            }
        ));
        // Only versions above 1 are written back, matching Go's Format.
        assert!(!format(&op(base)).contains("|cv="));
        assert!(format(&op(&format!("{base}|cv=2"))).ends_with("|cv=2"));
        malformed(&format!("{base}|cv="));
    }

    #[test]
    fn encryption_attributes_with_and_without_psz_and_ev() {
        let file = |raw: &str| match op(raw) {
            Op::File {
                encrypted,
                encryption_version,
                plaintext_size,
                ..
            } => (encrypted, encryption_version, plaintext_size),
            other => panic!("expected a file op, got {other:?}"),
        };

        // `enc=1` alone implies version 1 and no plaintext size.
        assert_eq!(file("TDX1|t=f|p=|n=x|enc=1"), (true, 1, None));
        // `ev` overrides the implied version and `psz` carries the plaintext size.
        assert_eq!(file("TDX1|t=f|p=|n=x|enc=1|ev=2|psz=7"), (true, 2, Some(7)));
        // `ev`/`psz` are independent of `enc`: they set the fields but not the flag.
        assert_eq!(file("TDX1|t=f|p=|n=x|ev=1"), (false, 1, None));
        assert_eq!(file("TDX1|t=f|p=|n=x|psz=12"), (false, 0, Some(12)));
        // A non-"1" `enc` value is ignored, not an error.
        assert_eq!(file("TDX1|t=f|p=|n=x|enc=0"), (false, 0, None));
        assert_eq!(file("TDX1|t=f|p=|n=x|enc=yes"), (false, 0, None));
        // Formatting an encrypted file with version 0 promotes it to 1 and
        // drops the zero version, exactly like Go.
        let encrypted = format(&Op::File {
            parent: ROOT_PARENT.to_string(),
            name: "x".to_string(),
            size: None,
            uploaded_at: None,
            encrypted: true,
            encryption_version: 0,
            plaintext_size: None,
        });
        assert_eq!(encrypted, "TDX1|t=f|p=|n=x|enc=1");
        // A version above 1 is written as `ev=`.
        let v2 = format(&Op::File {
            parent: ROOT_PARENT.to_string(),
            name: "x".to_string(),
            size: None,
            uploaded_at: None,
            encrypted: true,
            encryption_version: 2,
            plaintext_size: Some(3),
        });
        assert_eq!(v2, "TDX1|t=f|p=|n=x|enc=1|ev=2|psz=3");
    }

    #[test]
    fn manifest_and_part_vectors() {
        // From backend/projection/multipart_test.go TestMultipartWireRoundTrip.
        assert_eq!(
            format(&Op::Part {
                uuid: "abc-123".to_string(),
                index: 5,
            }),
            "TDX1|t=part|u=abc-123|pix=5"
        );
        let manifest = Op::Manifest {
            uuid: "abc-123".to_string(),
            parent: "d:folder".to_string(),
            name: "My File.mkv".to_string(),
            part_count: 3,
            size: Some(5000),
            uploaded_at: Some(1700),
            encrypted: true,
            encryption_version: 1,
            plaintext_size: Some(4096),
        };
        assert_eq!(
            format(&manifest),
            "TDX1|t=manifest|u=abc-123|p=d:folder|n=My+File.mkv|pc=3|sz=5000|ts=1700|enc=1|psz=4096"
        );
        assert_eq!(op(&format(&manifest)), manifest);
        // A part's `sz` is validated but not retained; `pc` must be positive.
        assert_eq!(
            op("TDX1|t=part|u=abc-123|pix=5|sz=2048"),
            Op::Part {
                uuid: "abc-123".to_string(),
                index: 5,
            }
        );
        malformed("TDX1|t=part|u=abc-123|pix=5|sz=-1");
        malformed("TDX1|t=manifest|u=abc-123|p=d:x|n=f|pc=0");
        malformed("TDX1|t=manifest|u=abc-123|p=d:x|n=f");
        malformed("TDX1|t=part|u=abc-123");
        malformed("TDX1|t=part|pix=0");
        malformed("TDX1|t=part|u=abc-123|pix=-1");
    }

    #[test]
    fn known_ops_reject_malformed_headers() {
        // Object id prefixes, from wire_test.go.
        malformed("TDX1|t=mkdir|obj=raw|p=|n=Goa");
        malformed("TDX1|t=tomb|obj=d:x");
        malformed("TDX1|t=rmdir|obj=f:x");
        malformed("TDX1|t=meta|obj=d:x|p=|n=x");
        malformed("TDX1|t=rename|obj=raw|n=x");
        malformed("TDX1|t=move|obj=raw|p=");
        // A non-folder parent, from TestParseRejectsNonFolderParent.
        malformed("TDX1|t=f|p=raw|n=x");
        // A missing or empty name.
        malformed("TDX1|t=f|p=");
        malformed("TDX1|t=f|p=|n=");
        malformed("TDX1|t=rename|obj=f:1|n=");
        // Bad escapes in the name.
        malformed("TDX1|t=f|p=|n=%zz");
        malformed("TDX1|t=f|p=|n=trailing%");
        // Negative and out-of-range numbers, from TestParseRejectsNegativeFileSizes
        // and TestParseRejectsUploadTimeOutsideSQLiteRange.
        malformed("TDX1|t=f|p=|n=x|sz=-1");
        malformed("TDX1|t=f|p=|n=x|ts=-1");
        malformed("TDX1|t=f|p=|n=x|enc=1|psz=-1");
        malformed("TDX1|t=f|p=|n=x|ts=253402300800");
        malformed("TDX1|t=f|p=|n=x|sz=abc");
        malformed("TDX1|t=f|p=|n=x|ev=two");
        malformed("TDX1|t=f|p=|n=x|psz=abc");
        // The timestamp upper bound itself is accepted.
        assert_eq!(
            op("TDX1|t=f|p=|n=x|ts=253402300799"),
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "x".to_string(),
                size: None,
                uploaded_at: Some(253_402_300_799),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
        // Attributes are validated for ops that do not keep them.
        malformed("TDX1|t=mkdir|obj=d:x|p=|n=Goa|sz=-1");
        malformed("TDX1|t=rmdir|obj=d:x|ts=-1");
        // The last value of a repeated attribute wins, as with Go's map.
        assert_eq!(
            op("TDX1|t=f|p=|n=x|sz=5|sz=6"),
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "x".to_string(),
                size: Some(6),
                uploaded_at: None,
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            }
        );
    }

    #[test]
    fn rendition_only_allowed_on_part() {
        malformed("TDX1|t=f|p=|n=x|rend=abc");
        malformed("TDX1|t=mkdir|obj=d:x|p=|n=x|rend=abc");
        // The payload is not decoded, but the op-type guard still admits it.
        assert_eq!(
            op("TDX1|t=part|u=u|pix=0|rend=not-base64"),
            Op::Part {
                uuid: "u".to_string(),
                index: 0,
            }
        );
    }

    #[test]
    fn header_line_takes_the_first_line_trimmed() {
        // From wire_test.go TestExtractHeaderLineStripsBody.
        assert_eq!(
            header_line("TDX1|t=mkdir|obj=d:x|p=|n=foo\nhuman comment"),
            "TDX1|t=mkdir|obj=d:x|p=|n=foo"
        );
        assert_eq!(header_line("  TDX1|t=f  \nbody"), "TDX1|t=f");
        assert_eq!(header_line("TDX1|t=f\r\nbody"), "TDX1|t=f");
        assert_eq!(header_line("no newline"), "no newline");
        assert_eq!(header_line(""), "");
    }

    #[test]
    fn object_id_helpers() {
        assert_eq!(file_obj_id("42"), "f:42");
        assert_eq!(folder_obj_id("abc"), "d:abc");
        assert!(is_file_obj("f:42"));
        assert!(!is_file_obj("f:"));
        assert!(!is_file_obj("d:42"));
        assert!(!is_file_obj("f"));
        assert!(!is_file_obj(""));
        assert!(is_folder_obj("d:abc"));
        assert!(!is_folder_obj("d:"));
        assert!(!is_folder_obj("f:abc"));
        assert!(!is_folder_obj(""));
    }

    #[test]
    fn go_round_trip_table() {
        // Adapted from wire_test.go TestRoundTripFormatParse.
        let cases = vec![
            Op::File {
                parent: ROOT_PARENT.to_string(),
                name: "alpha.png".to_string(),
                size: Some(123),
                uploaded_at: Some(456),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
            Op::File {
                parent: "d:abc".to_string(),
                name: "spaces here.txt".to_string(),
                size: Some(789),
                uploaded_at: Some(1000),
                encrypted: true,
                encryption_version: 1,
                plaintext_size: Some(7),
            },
            Op::Meta {
                obj: "f:9".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "legacy.bin".to_string(),
                size: Some(2048),
                uploaded_at: Some(88),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
            Op::Folder {
                obj: "d:abc".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "Photos".to_string(),
            },
            Op::Rename {
                obj: "f:5".to_string(),
                name: "new.png".to_string(),
            },
            Op::Rename {
                obj: "d:abc".to_string(),
                name: "Renamed".to_string(),
            },
            Op::Move {
                obj: "f:5".to_string(),
                parent: "d:abc".to_string(),
            },
            Op::Move {
                obj: "d:abc".to_string(),
                parent: ROOT_PARENT.to_string(),
            },
            Op::Tomb {
                obj: "f:42".to_string(),
            },
            Op::Rmdir {
                obj: "d:abc".to_string(),
            },
            Op::Manifest {
                uuid: "abc-123".to_string(),
                parent: "d:folder".to_string(),
                name: "My File.mkv".to_string(),
                part_count: 3,
                size: Some(5000),
                uploaded_at: Some(1700),
                encrypted: true,
                encryption_version: 2,
                plaintext_size: Some(4096),
            },
            Op::Part {
                uuid: "abc-123".to_string(),
                index: 5,
            },
            Op::EncConfig {
                salt: b"salt".to_vec(),
                params_json: r#"{"memory":65536,"time":3}"#.to_string(),
                wrapped_master_key: b"wrapped".to_vec(),
                key_check: b"check".to_vec(),
                hint: "pet name".to_string(),
                config_version: 2,
                enabled: true,
            },
            // The writable-mount ops, as a drive written through the mounted
            // path stores its real structure.
            Op::FolderCommit {
                op_id: "op-1".to_string(),
                obj: "d:db126e90-3715-499f-97a6-4a543993dfae".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "Ringtones".to_string(),
            },
            Op::FileCommit {
                op_id: "op-2".to_string(),
                parent: "d:db126e90-3715-499f-97a6-4a543993dfae".to_string(),
                name: "tone.mp3".to_string(),
                content_msg_id: 4242,
                upload_uuid: String::new(),
                part_count: 0,
                content_hash: String::new(),
                size: Some(4096),
                uploaded_at: Some(1700),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
            Op::FileCommit {
                op_id: "op-3".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "big movie.mkv".to_string(),
                content_msg_id: 0,
                upload_uuid: "abc-123".to_string(),
                part_count: 3,
                content_hash: "sha256:beef".to_string(),
                size: Some(900_000),
                uploaded_at: Some(1800),
                encrypted: true,
                encryption_version: 2,
                plaintext_size: Some(880_000),
            },
            Op::FileReplace {
                op_id: "op-4".to_string(),
                obj: "f:4242".to_string(),
                expected_revision: 2,
                content_msg_id: 4300,
                upload_uuid: String::new(),
                part_count: 0,
                content_hash: String::new(),
                retained_until: 9999,
                size: Some(2048),
                uploaded_at: Some(1900),
                encrypted: false,
                encryption_version: 0,
                plaintext_size: None,
            },
            Op::Relocate {
                op_id: "op-5".to_string(),
                obj: "f:4242".to_string(),
                parent: "d:target".to_string(),
                name: "moved.mp3".to_string(),
                expected_revision: 1,
                overwrite: true,
                destination_obj: "f:7".to_string(),
                expected_destination_revision: 3,
                deleted_at: 1000,
                purge_after: 2000,
            },
            Op::Trash {
                op_id: "op-6".to_string(),
                obj: "d:db126e90-3715-499f-97a6-4a543993dfae".to_string(),
                expected_revision: 1,
                deleted_at: 1000,
                purge_after: 2000,
            },
            Op::HardDelete {
                op_id: "op-7".to_string(),
                obj: "f:4242".to_string(),
                expected_revision: 1,
            },
            Op::Untrash {
                op_id: "op-8".to_string(),
                obj: "f:4242".to_string(),
                parent: ROOT_PARENT.to_string(),
                name: "restored.mp3".to_string(),
                expected_revision: 1,
            },
            Op::Other {
                op_type: "future-op".to_string(),
            },
        ];

        for want in cases {
            let raw = format(&want);
            assert!(raw.starts_with("TDX1|t="), "format({want:?}) = {raw:?}");
            assert_eq!(op(&raw), want, "round trip of {raw:?}");
        }
    }

    /// The writable ops are the ones the mounted-write path uses, so their
    /// validation has to match Go's `setWritableEnvelope` and
    /// `setContentReference` rather than be lenient.
    #[test]
    fn writable_ops_reject_malformed_headers() {
        for raw in [
            // No protocol version, or one this reader does not implement.
            "TDX1|t=fcommit|oid=op|p=|n=a|cmid=1",
            "TDX1|t=fcommit|v=2|oid=op|p=|n=a|cmid=1",
            // No operation id.
            "TDX1|t=fcommit|v=1|oid=|p=|n=a|cmid=1",
            // Both content shapes at once, and neither of them.
            "TDX1|t=fcommit|v=1|oid=op|p=|n=a|cmid=1|u=u|pc=2",
            "TDX1|t=fcommit|v=1|oid=op|p=|n=a",
            "TDX1|t=fcommit|v=1|oid=op|p=|n=a|u=u",
            "TDX1|t=fcommit|v=1|oid=op|p=|n=a|pc=2",
            // A folder commit has to name a folder.
            "TDX1|t=dcommit|v=1|oid=op|obj=f:1|p=|n=a",
            // Revisions and trash timestamps are strictly positive.
            "TDX1|t=relocate|v=1|oid=op|obj=f:1|p=|n=a|rev=0",
            "TDX1|t=trash|v=1|oid=op|obj=f:1|rev=1|del=1",
            "TDX1|t=untrash|v=1|oid=op|obj=f:1|p=|n=a",
            // An object id has to be addressed as a file or a folder.
            "TDX1|t=harddel|v=1|oid=op|obj=z:1|rev=1",
        ] {
            malformed(raw);
        }
    }

    /// A relocate without an overwrite target is the ordinary move: the fields
    /// that describe the victim simply stay empty rather than failing.
    #[test]
    fn relocate_without_an_overwrite_parses_as_a_plain_move() {
        let parsed = op("TDX1|t=relocate|v=1|oid=op-9|obj=f:5|p=d:abc|n=moved.txt|rev=4");
        assert_eq!(
            parsed,
            Op::Relocate {
                op_id: "op-9".to_string(),
                obj: "f:5".to_string(),
                parent: "d:abc".to_string(),
                name: "moved.txt".to_string(),
                expected_revision: 4,
                overwrite: false,
                destination_obj: String::new(),
                expected_destination_revision: 0,
                deleted_at: 0,
                purge_after: 0,
            }
        );
    }

    /// A replacement addresses a file, so a folder id is malformed.
    #[test]
    fn file_replace_only_targets_files() {
        malformed("TDX1|t=freplace|v=1|oid=op|obj=d:1|rev=1|cmid=2|retain=3");
    }

    /// The op ids a delete, a restore and a purge carry are the original's own
    /// values for the same inputs, down to the byte: the digest is over a
    /// domain-separated identity, and the golden values here were computed from
    /// the original's `DeterministicOpID` rather than from this code.
    #[test]
    fn a_trash_op_id_is_the_one_the_original_writes() {
        assert_eq!(
            deterministic_op_id("trash", "f:1", 1),
            "trash-56eac129eb0efaa44de19bc662fff63c"
        );
        assert_eq!(
            deterministic_op_id("restore", "d:zeta", 3),
            "restore-edeb90fc68b4eabd8840e562d871fb7a"
        );
        assert_eq!(
            deterministic_op_id("purge", "f:42", 7),
            "purge-0f8c0318044033a48bcde27e1e5804c8"
        );
        // The revision is part of the identity: the delete and the restore that
        // undoes it are never the same operation.
        assert_ne!(
            deterministic_op_id("trash", "f:1", 1),
            deterministic_op_id("trash", "f:1", 2)
        );
    }

    /// A delete carries the retention window it was written with, and the
    /// restore and purge that follow name the revision they expect to find.
    #[test]
    fn the_delete_restore_and_purge_operations_round_trip() {
        let trash = trash_op("f:1", 2, 1_700_000_000, 30 * 86_400);
        assert_eq!(trash, op(&format(&trash)));
        match &trash {
            Op::Trash {
                op_id,
                obj,
                expected_revision,
                deleted_at,
                purge_after,
            } => {
                assert_eq!(op_id, &deterministic_op_id("trash", "f:1", 2));
                assert_eq!(obj, "f:1");
                assert_eq!(*expected_revision, 2);
                assert_eq!(*deleted_at, 1_700_000_000);
                assert_eq!(*purge_after, 1_700_000_000 + 30 * 86_400);
            }
            other => panic!("trash_op built {other:?}"),
        }

        let restore = restore_op("d:zeta", 3, "d:home", "Ringtones (2)");
        assert_eq!(restore, op(&format(&restore)));
        match &restore {
            Op::Untrash {
                obj,
                parent,
                name,
                expected_revision,
                ..
            } => {
                assert_eq!(obj, "d:zeta");
                assert_eq!(parent, "d:home");
                assert_eq!(name, "Ringtones (2)");
                assert_eq!(*expected_revision, 3);
            }
            other => panic!("restore_op built {other:?}"),
        }

        let purge = purge_op("f:42", 7);
        assert_eq!(purge, op(&format(&purge)));
        match &purge {
            Op::HardDelete {
                obj,
                expected_revision,
                ..
            } => {
                assert_eq!(obj, "f:42");
                assert_eq!(*expected_revision, 7);
            }
            other => panic!("purge_op built {other:?}"),
        }
    }

    /// The headers a delete, a restore and a purge put in the drive, pinned as
    /// text: the drive's own reader is the only consumer, and a field it does
    /// not expect is a write that does nothing.
    #[test]
    fn the_management_headers_are_written_the_way_the_drive_reads_them() {
        assert_eq!(
            format(&trash_op("f:2", 1, 1_700_000_000, 30 * 86_400)),
            "TDX1|t=trash|v=1|oid=trash-c9908488a3b8821148497b9817de1505|obj=f:2|rev=1|del=1700000000|purge=1702592000"
        );
        assert_eq!(
            format(&restore_op("f:2", 2, "", "leaf.bin")),
            "TDX1|t=untrash|v=1|oid=restore-3f7ed584569b532d578f0e7e98de45c4|obj=f:2|p=|n=leaf.bin|rev=2"
        );
        assert_eq!(
            format(&purge_op("f:2", 4)),
            "TDX1|t=harddel|v=1|oid=purge-fe467995d69f3f41ae7b23eb64968cc4|obj=f:2|rev=4"
        );
    }
}
