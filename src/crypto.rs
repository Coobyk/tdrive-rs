//! Password-derived key wrapping and TDE1 authenticated streaming encryption.
//!
//! This is a self-contained Rust port of the TDrive Go package
//! `backend/crypto` (`vault.go`, `stream.go`). The vault envelope and the
//! on-wire TDE1 container are byte-for-byte compatible with the Go
//! implementation, so metadata produced by Go unlocks here and streams written
//! here decrypt in Go.
//!
//! # Key hierarchy
//!
//! A user password is stretched with Argon2id into a key-encryption-key (KEK).
//! The KEK unwraps a random 32-byte master key with XChaCha20-Poly1305. A fixed
//! `tdrive-key-check-v1` marker, encrypted under the master key, lets us reject
//! a wrong password without touching a real file. Each encrypted file derives
//! its own 32-byte subkey from the master key and a random 16-byte per-file salt
//! with HKDF-SHA256.
//!
//! # TDE1 container
//!
//! ```text
//! offset  size  meaning
//! 0       4     ASCII "TDE1"
//! 4       1     reserved flags, must be zero
//! 5       1     plaintext chunk size exponent, must be 16 (64 KiB)
//! 6       16    random per-file HKDF salt
//! 22      20    random per-file nonce prefix
//! 42      8     plaintext size, unsigned little endian
//! 50      ...   one AEAD ciphertext + 16-byte tag per chunk
//! ```
//!
//! Every stream ends with an explicit final chunk: a partial last chunk carries
//! the final marker, while empty files and exact multiples of the chunk size get
//! an empty final chunk that still carries a 16-byte tag. The 20-byte nonce
//! prefix is followed by a big-endian `u32` chunk counter whose high bit marks
//! the final chunk. AEAD calls use no additional authenticated data.
//!
//! Note: the port brief listed `HEADER_LEN` as 48, but the fixed TDE1 header is
//! 50 bytes (4 + 1 + 1 + 16 + 20 + 8), matching Go's `streamHeaderLen` and the
//! `parse_header` contract that the first `HEADER_LEN` bytes include the magic.
//! This module uses 50 so the port stays interoperable.

// The UI wires these bindings in as they land; until then nothing in the binary
// references this module, so silence dead-code lints for this self-contained API.
#![allow(dead_code)]

use std::io::{ErrorKind, Read, Write};

use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce, aead::Aead};
use hkdf::Hkdf;
use sha2::Sha256;
use zeroize::Zeroize;

/// Error surface shared by vault and stream operations.
///
/// The vault path collapses "wrong password" and "tampered key material" into
/// [`CryptoError::WrongPassword`], exactly like the Go package, so callers never
/// learn which one happened.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CryptoError {
    /// The password did not unwrap the master key or failed the key check.
    #[error("crypto: wrong password")]
    WrongPassword,
    /// The KDF metadata is unknown, out of bounds or malformed.
    #[error("crypto: unsupported KDF")]
    UnsupportedKdf,
    /// Vault material or a TDE1 stream is structurally invalid or inauthentic.
    #[error("crypto: corrupt data")]
    Corrupt,
    /// The underlying reader, writer or random source failed.
    #[error("crypto: I/O error")]
    Io,
}

/// Vault material parsed from a channel's `encfg` operation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VaultMaterial {
    /// Raw KDF salt.
    pub salt: Vec<u8>,
    /// Argon2 parameters JSON, exactly as the Go `Params` type marshals it.
    pub params_json: String,
    /// Wrapped master key: `nonce(24) || ciphertext(32) || tag(16)`.
    pub wrapped_master_key: Vec<u8>,
    /// Encrypted `tdrive-key-check-v1` marker: `nonce(24) || ciphertext(19) || tag(16)`.
    pub key_check: Vec<u8>,
}

/// Parsed TDE1 header fields.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamHeader {
    /// Reserved flags; always zero for TDE1 v1.
    pub flags: u8,
    /// `log2` of the plaintext chunk size; always 16 for TDE1 v1.
    pub chunk_log2: u8,
    /// Per-file HKDF-SHA256 salt.
    pub salt: [u8; 16],
    /// Per-file nonce prefix; the per-chunk nonce is this followed by a
    /// big-endian `u32` counter.
    pub nonce_prefix: [u8; 20],
    /// Plaintext size declared by the header.
    pub plaintext_size: u64,
}

// ---------------------------------------------------------------------------
// Format constants
// ---------------------------------------------------------------------------

const STREAM_MAGIC: &[u8; 4] = b"TDE1";
/// Total fixed TDE1 header length: magic, flags, chunk exponent, salt, nonce
/// prefix and little-endian plaintext size.
pub const HEADER_LEN: usize = 50;
/// `log2` of the plaintext chunk size for TDE1 v1.
pub const CHUNK_LOG2: u8 = 16;
/// Plaintext chunk size in bytes (64 KiB).
pub const CHUNK_SIZE: usize = 1usize << CHUNK_LOG2;
const FILE_SALT_LEN: usize = 16;
const NONCE_PREFIX_LEN: usize = 20;
const TAG_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const CHUNK_CIPHER_SIZE: usize = CHUNK_SIZE + TAG_LEN;
const FINAL_CHUNK_BIT: u32 = 1 << 31;
/// Largest plaintext size representable by TDE1: `2^47 - 1` bytes.
const MAX_PLAINTEXT_SIZE: u64 = (FINAL_CHUNK_BIT as u64) * (CHUNK_SIZE as u64) - 1;
const HKDF_INFO: &[u8] = b"tdrive/file/v1";
const NO_PLAINTEXT: &[u8] = &[];

// Vault constants, mirroring the Go package.
const KDF_ARGON2ID: &str = "argon2id";
const MASTER_KEY_LEN: usize = 32;
const MIN_MEMORY_KIB: u32 = 16 * 1024;
const MAX_MEMORY_KIB: u32 = 256 * 1024;
const MIN_TIME: u32 = 1;
const MAX_TIME: u32 = 10;
const MIN_PARALLELISM: u8 = 1;
const MAX_PARALLELISM: u8 = 16;
const MIN_SALT_LEN: usize = 16;
const MAX_SALT_LEN: usize = 64;
const MAX_PARAMS_JSON_LEN: usize = 1024;
const WRAPPED_MASTER_KEY_LEN: usize = NONCE_LEN + MASTER_KEY_LEN + TAG_LEN;
const KEY_CHECK_PLAINTEXT: &[u8] = b"tdrive-key-check-v1";
const KEY_CHECK_LEN: usize = NONCE_LEN + KEY_CHECK_PLAINTEXT.len() + TAG_LEN;

/// Argon2id parameters, mirroring the Go `Params` type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Params {
    memory: u32,
    time: u32,
    parallelism: u8,
    key_len: u32,
    salt_len: usize,
}

const DEFAULT_PARAMS: Params = Params {
    memory: 64 * 1024,
    time: 3,
    parallelism: 4,
    key_len: MASTER_KEY_LEN as u32,
    salt_len: MIN_SALT_LEN,
};

// ---------------------------------------------------------------------------
// Vault: password → KEK → master key
// ---------------------------------------------------------------------------

/// Full unlock: derive the KEK, unwrap the master key, verify the key check.
///
/// A wrong password and tampered key material both return
/// [`CryptoError::WrongPassword`], exactly like the Go code. Structural damage
/// (bad salt length, truncated envelopes, unsupported KDF metadata) returns
/// [`CryptoError::Corrupt`] or [`CryptoError::UnsupportedKdf`].
pub fn unlock(material: &VaultMaterial, password: &str) -> Result<[u8; 32], CryptoError> {
    let params = parse_params(&material.params_json)?;
    if material.salt.len() != params.salt_len
        || !(MIN_SALT_LEN..=MAX_SALT_LEN).contains(&material.salt.len())
    {
        return Err(CryptoError::Corrupt);
    }
    if material.wrapped_master_key.len() != WRAPPED_MASTER_KEY_LEN
        || material.key_check.len() != KEY_CHECK_LEN
    {
        return Err(CryptoError::Corrupt);
    }

    let mut kek = derive_kek(password.as_bytes(), &material.salt, &params)?;
    let unwrapped = unwrap_master_key(&material.wrapped_master_key, &kek);
    kek.zeroize();
    let mut master = unwrapped?;

    if verify_key_check(&master, &material.key_check).is_err() {
        master.zeroize();
        return Err(CryptoError::WrongPassword);
    }
    Ok(master)
}

/// Create fresh vault material for a new password.
///
/// `hint` is user-facing metadata only: like Go's `EncryptionConfig.Hint` it is
/// persisted by the caller and never enters the cryptographic material.
pub fn create_vault(password: &str, hint: &str) -> Result<(VaultMaterial, [u8; 32]), CryptoError> {
    let _ = hint;
    create_vault_with_params(password, DEFAULT_PARAMS)
}

/// Best-effort wipe of a key. Clears the bytes in place.
pub fn wipe(key: &mut [u8; 32]) {
    key.zeroize();
}

fn create_vault_with_params(
    password: &str,
    params: Params,
) -> Result<(VaultMaterial, [u8; 32]), CryptoError> {
    validate_params(&params)?;

    let mut salt = vec![0u8; params.salt_len];
    fill_random(&mut salt)?;

    let mut master = [0u8; MASTER_KEY_LEN];
    fill_random(&mut master)?;

    let mut kek = derive_kek(password.as_bytes(), &salt, &params)?;
    let wrapped_master_key = wrap_master_key(&master, &kek);
    kek.zeroize();
    let wrapped_master_key = wrapped_master_key?;

    let key_check = encode_key_check(&master)?;
    let params_json = marshal_params(&params);

    Ok((
        VaultMaterial {
            salt,
            params_json,
            wrapped_master_key,
            key_check,
        },
        master,
    ))
}

/// Derive the input for Argon2id, i.e. the KEK, from password and salt.
fn derive_kek(password: &[u8], salt: &[u8], params: &Params) -> Result<[u8; 32], CryptoError> {
    validate_params(params)?;
    if salt.len() != params.salt_len || salt.len() < MIN_SALT_LEN || salt.len() > MAX_SALT_LEN {
        return Err(CryptoError::UnsupportedKdf);
    }

    let argon_params = argon2::Params::new(
        params.memory,
        params.time,
        params.parallelism as u32,
        Some(params.key_len as usize),
    )
    .map_err(|_| CryptoError::UnsupportedKdf)?;
    let argon = argon2::Argon2::new(
        argon2::Algorithm::Argon2id,
        argon2::Version::V0x13,
        argon_params,
    );

    let mut kek = [0u8; MASTER_KEY_LEN];
    argon
        .hash_password_into(password, salt, &mut kek)
        .map_err(|_| CryptoError::UnsupportedKdf)?;
    Ok(kek)
}

fn validate_params(params: &Params) -> Result<(), CryptoError> {
    let in_range = params.memory >= MIN_MEMORY_KIB
        && params.memory <= MAX_MEMORY_KIB
        && params.time >= MIN_TIME
        && params.time <= MAX_TIME
        && params.parallelism >= MIN_PARALLELISM
        && params.parallelism <= MAX_PARALLELISM
        && params.key_len == MASTER_KEY_LEN as u32
        && params.salt_len >= MIN_SALT_LEN
        && params.salt_len <= MAX_SALT_LEN;
    if in_range {
        Ok(())
    } else {
        Err(CryptoError::UnsupportedKdf)
    }
}

/// Encrypt `master` under `kek`. Layout: `nonce(24) || ciphertext(32) || tag(16)`.
fn wrap_master_key(master: &[u8; MASTER_KEY_LEN], kek: &[u8; 32]) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new_from_slice(kek).map_err(|_| CryptoError::Corrupt)?;
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), &master[..])
        .map_err(|_| CryptoError::Corrupt)?;

    let mut out = Vec::with_capacity(WRAPPED_MASTER_KEY_LEN);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Reverse [`wrap_master_key`]. An AEAD failure means the wrong KEK, so it maps
/// to [`CryptoError::WrongPassword`].
fn unwrap_master_key(wrapped: &[u8], kek: &[u8; 32]) -> Result<[u8; MASTER_KEY_LEN], CryptoError> {
    if wrapped.len() != WRAPPED_MASTER_KEY_LEN {
        return Err(CryptoError::Corrupt);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(kek).map_err(|_| CryptoError::Corrupt)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&wrapped[..NONCE_LEN]);
    let mut plaintext = cipher
        .decrypt(&XNonce::from(nonce), &wrapped[NONCE_LEN..])
        .map_err(|_| CryptoError::WrongPassword)?;

    if plaintext.len() != MASTER_KEY_LEN {
        plaintext.zeroize();
        return Err(CryptoError::Corrupt);
    }
    let mut master = [0u8; MASTER_KEY_LEN];
    master.copy_from_slice(&plaintext);
    plaintext.zeroize();
    Ok(master)
}

/// Encrypt the fixed key-check marker under the master key.
fn encode_key_check(master: &[u8; MASTER_KEY_LEN]) -> Result<Vec<u8>, CryptoError> {
    let cipher = XChaCha20Poly1305::new_from_slice(master).map_err(|_| CryptoError::Corrupt)?;
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let ciphertext = cipher
        .encrypt(&XNonce::from(nonce), KEY_CHECK_PLAINTEXT)
        .map_err(|_| CryptoError::Corrupt)?;

    let mut out = Vec::with_capacity(KEY_CHECK_LEN);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    Ok(out)
}

/// Decrypt and compare the key-check marker.
fn verify_key_check(master: &[u8; MASTER_KEY_LEN], blob: &[u8]) -> Result<(), CryptoError> {
    if blob.len() != KEY_CHECK_LEN {
        return Err(CryptoError::Corrupt);
    }
    let cipher = XChaCha20Poly1305::new_from_slice(master).map_err(|_| CryptoError::Corrupt)?;
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&blob[..NONCE_LEN]);
    let mut plaintext = cipher
        .decrypt(&XNonce::from(nonce), &blob[NONCE_LEN..])
        .map_err(|_| CryptoError::WrongPassword)?;

    let matches = plaintext.as_slice() == KEY_CHECK_PLAINTEXT;
    plaintext.zeroize();
    if matches {
        Ok(())
    } else {
        Err(CryptoError::Corrupt)
    }
}

/// Serialise parameters exactly as the Go `Params` type does.
fn marshal_params(params: &Params) -> String {
    format!(
        r#"{{"kdf":"{KDF_ARGON2ID}","memory":{},"time":{},"parallelism":{},"key_len":{},"salt_len":{}}}"#,
        params.memory, params.time, params.parallelism, params.key_len, params.salt_len
    )
}

/// Decode parameters previously written by [`marshal_params`], or by Go.
///
/// Unknown fields, malformed numbers, trailing documents, `null`/empty and
/// unknown KDF identifiers and out-of-bounds costs all map to
/// [`CryptoError::UnsupportedKdf`]. A missing `kdf` field selects Argon2id.
fn parse_params(json: &str) -> Result<Params, CryptoError> {
    if json.is_empty() || json.len() > MAX_PARAMS_JSON_LEN {
        return Err(CryptoError::UnsupportedKdf);
    }
    let value: serde_json::Value =
        serde_json::from_str(json).map_err(|_| CryptoError::UnsupportedKdf)?;
    let object = value.as_object().ok_or(CryptoError::UnsupportedKdf)?;

    for key in object.keys() {
        if !matches!(
            key.as_str(),
            "kdf" | "memory" | "time" | "parallelism" | "key_len" | "salt_len"
        ) {
            return Err(CryptoError::UnsupportedKdf);
        }
    }

    match object.get("kdf") {
        None => {}
        Some(serde_json::Value::String(kdf)) if kdf == KDF_ARGON2ID => {}
        Some(_) => return Err(CryptoError::UnsupportedKdf),
    }

    let memory = unsigned_field(object, "memory", u32::MAX as u64)?;
    let time = unsigned_field(object, "time", u32::MAX as u64)?;
    let parallelism = unsigned_field(object, "parallelism", u8::MAX as u64)? as u8;
    let key_len = unsigned_field(object, "key_len", u32::MAX as u64)?;
    let salt_len = signed_field(object, "salt_len")?;

    let Ok(salt_len) = usize::try_from(salt_len) else {
        return Err(CryptoError::UnsupportedKdf);
    };
    let params = Params {
        memory,
        time,
        parallelism,
        key_len,
        salt_len,
    };
    validate_params(&params)?;
    Ok(params)
}

/// Read an unsigned JSON field, returning `0` when absent (matching Go's zero
/// value) and rejecting non-integer or out-of-range values.
fn unsigned_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    max: u64,
) -> Result<u32, CryptoError> {
    match object.get(key) {
        None => Ok(0),
        Some(serde_json::Value::Number(number)) => match number.as_u64() {
            Some(value) if value <= max => Ok(value as u32),
            _ => Err(CryptoError::UnsupportedKdf),
        },
        Some(_) => Err(CryptoError::UnsupportedKdf),
    }
}

fn signed_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> Result<i64, CryptoError> {
    match object.get(key) {
        None => Ok(0),
        Some(serde_json::Value::Number(number)) => {
            number.as_i64().ok_or(CryptoError::UnsupportedKdf)
        }
        Some(_) => Err(CryptoError::UnsupportedKdf),
    }
}

// ---------------------------------------------------------------------------
// TDE1 streams
// ---------------------------------------------------------------------------

/// Parse the first [`HEADER_LEN`] bytes of a TDE1 container.
///
/// Rejects a bad magic, nonzero flags, a chunk exponent other than
/// [`CHUNK_LOG2`], a short buffer and a declared plaintext size beyond the
/// format capacity.
pub fn parse_header(bytes: &[u8]) -> Result<StreamHeader, CryptoError> {
    if bytes.len() < HEADER_LEN {
        return Err(CryptoError::Corrupt);
    }
    if &bytes[0..4] != STREAM_MAGIC {
        return Err(CryptoError::Corrupt);
    }
    let flags = bytes[4];
    let chunk_log2 = bytes[5];
    if flags != 0 || chunk_log2 != CHUNK_LOG2 {
        return Err(CryptoError::Corrupt);
    }

    let mut salt = [0u8; FILE_SALT_LEN];
    salt.copy_from_slice(&bytes[6..6 + FILE_SALT_LEN]);
    let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
    nonce_prefix.copy_from_slice(&bytes[22..22 + NONCE_PREFIX_LEN]);
    let mut size_bytes = [0u8; 8];
    size_bytes.copy_from_slice(&bytes[42..50]);
    let plaintext_size = u64::from_le_bytes(size_bytes);
    if plaintext_size > MAX_PLAINTEXT_SIZE {
        return Err(CryptoError::Corrupt);
    }

    Ok(StreamHeader {
        flags,
        chunk_log2,
        salt,
        nonce_prefix,
        plaintext_size,
    })
}

/// Exact number of bytes an encrypted stream occupies for a given plaintext
/// size: the fixed header plus a 16-byte tag per 64 KiB chunk, including the
/// always-present final chunk. Saturates to [`u64::MAX`] above TDE1 capacity.
pub fn ciphertext_size(plaintext_size: u64) -> u64 {
    if plaintext_size > MAX_PLAINTEXT_SIZE {
        return u64::MAX;
    }
    let chunks = plaintext_size / CHUNK_SIZE as u64 + 1;
    let Some(base) = (HEADER_LEN as u64).checked_add(plaintext_size) else {
        return u64::MAX;
    };
    let max_tags = (u64::MAX - base) / TAG_LEN as u64;
    if chunks > max_tags {
        return u64::MAX;
    }
    base + chunks * TAG_LEN as u64
}

/// Check that `ciphertext_size` is the exact stored size for `plaintext_size`.
///
/// Returns `Some(plaintext_size)` when the pair is a valid TDE1 container and
/// `None` when the plaintext size exceeds the format capacity or the two sizes
/// disagree.
pub fn validate_plaintext_size(ciphertext_size: u64, plaintext_size: u64) -> Option<u64> {
    if plaintext_size > MAX_PLAINTEXT_SIZE {
        return None;
    }
    if self::ciphertext_size(plaintext_size) != ciphertext_size {
        return None;
    }
    Some(plaintext_size)
}

/// Encrypt a plaintext stream into a TDE1 container, returning the number of
/// plaintext bytes written.
///
/// The TDE1 header records the total plaintext size, and the writer is only a
/// `Write` sink with no seek, so this reads the whole reader into memory first.
/// That is a deliberate difference from Go's streaming `EncryptStream`; the
/// container bytes are identical.
pub fn encrypt_stream(
    reader: &mut impl Read,
    writer: &mut impl Write,
    master: &[u8; 32],
) -> Result<u64, CryptoError> {
    let mut plaintext = Vec::new();
    reader
        .read_to_end(&mut plaintext)
        .map_err(|_| CryptoError::Io)?;
    let size = plaintext.len() as u64;
    if size > MAX_PLAINTEXT_SIZE {
        plaintext.zeroize();
        return Err(CryptoError::Corrupt);
    }
    let mut source = plaintext.as_slice();
    let result = encrypt_stream_sized(&mut source, writer, master, size);
    plaintext.zeroize();
    result
}

/// Encrypt exactly `plaintext_size` bytes from `reader`, mirroring the Go
/// `EncryptStream` length bookkeeping.
fn encrypt_stream_sized(
    reader: &mut impl Read,
    writer: &mut impl Write,
    master: &[u8; 32],
    plaintext_size: u64,
) -> Result<u64, CryptoError> {
    if plaintext_size > MAX_PLAINTEXT_SIZE {
        return Err(CryptoError::Corrupt);
    }

    let mut salt = [0u8; FILE_SALT_LEN];
    fill_random(&mut salt)?;
    let mut nonce_prefix = [0u8; NONCE_PREFIX_LEN];
    fill_random(&mut nonce_prefix)?;

    let mut subkey = derive_subkey(master, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&subkey).map_err(|_| CryptoError::Corrupt)?;

    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(STREAM_MAGIC);
    header[4] = 0;
    header[5] = CHUNK_LOG2;
    header[6..6 + FILE_SALT_LEN].copy_from_slice(&salt);
    header[22..22 + NONCE_PREFIX_LEN].copy_from_slice(&nonce_prefix);
    header[42..50].copy_from_slice(&plaintext_size.to_le_bytes());
    writer.write_all(&header).map_err(|_| CryptoError::Io)?;

    let mut buffer = vec![0u8; CHUNK_SIZE];
    let mut counter: u32 = 0;
    let mut remaining = plaintext_size;
    while remaining > 0 {
        let want = remaining.min(CHUNK_SIZE as u64) as usize;
        read_exact_mapped(reader, &mut buffer[..want])?;
        remaining -= want as u64;

        let is_final = remaining == 0 && want < CHUNK_SIZE;
        if is_final {
            ensure_no_extra(reader)?;
        }

        let nonce = build_nonce(&nonce_prefix, counter, is_final);
        let ciphertext = cipher
            .encrypt(&nonce, &buffer[..want])
            .map_err(|_| CryptoError::Corrupt)?;
        writer.write_all(&ciphertext).map_err(|_| CryptoError::Io)?;
        counter += 1;

        if is_final {
            zeroize_buffer(&mut buffer);
            subkey.zeroize();
            return Ok(plaintext_size);
        }
    }

    ensure_no_extra(reader)?;
    let nonce = build_nonce(&nonce_prefix, counter, true);
    let ciphertext = cipher
        .encrypt(&nonce, NO_PLAINTEXT)
        .map_err(|_| CryptoError::Corrupt)?;
    writer.write_all(&ciphertext).map_err(|_| CryptoError::Io)?;

    zeroize_buffer(&mut buffer);
    subkey.zeroize();
    Ok(plaintext_size)
}

/// Decrypt a TDE1 container, authenticating every chunk.
///
/// Returns the number of plaintext bytes written. Any tampering, truncation,
/// wrong key or declared-size mismatch fails closed with [`CryptoError::Corrupt`];
/// only authenticated bytes are ever written.
pub fn decrypt_stream(
    reader: &mut impl Read,
    writer: &mut impl Write,
    master: &[u8; 32],
) -> Result<u64, CryptoError> {
    let mut header_bytes = [0u8; HEADER_LEN];
    let (header_read, _) = read_full_or_eof(reader, &mut header_bytes)?;
    if header_read != HEADER_LEN {
        return Err(CryptoError::Corrupt);
    }
    let header = parse_header(&header_bytes)?;

    let mut subkey = derive_subkey(master, &header.salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&subkey).map_err(|_| CryptoError::Corrupt)?;

    let mut buffer = vec![0u8; CHUNK_CIPHER_SIZE];
    let mut counter: u32 = 0;
    let mut written: u64 = 0;
    loop {
        let (read, eof) = read_full_or_eof(reader, &mut buffer)?;
        if read == 0 {
            // EOF at a chunk boundary with no explicit final chunk.
            zeroize_buffer(&mut buffer);
            subkey.zeroize();
            return Err(CryptoError::Corrupt);
        }
        let is_final = eof;

        let nonce = build_nonce(&header.nonce_prefix, counter, is_final);
        let mut plaintext = cipher
            .decrypt(&nonce, &buffer[..read])
            .map_err(|_| CryptoError::Corrupt)?;
        let write_result = writer.write_all(&plaintext);
        written += plaintext.len() as u64;
        plaintext.zeroize();
        write_result.map_err(|_| CryptoError::Io)?;
        counter += 1;

        if is_final {
            zeroize_buffer(&mut buffer);
            subkey.zeroize();
            if written != header.plaintext_size {
                return Err(CryptoError::Corrupt);
            }
            return Ok(header.plaintext_size);
        }
    }
}

/// Derive the per-file subkey: HKDF-SHA256 over the master key with the
/// per-file salt and info `tdrive/file/v1`.
fn derive_subkey(master: &[u8; 32], salt: &[u8]) -> Result<[u8; 32], CryptoError> {
    let hkdf = Hkdf::<Sha256>::new(Some(salt), &master[..]);
    let mut subkey = [0u8; 32];
    hkdf.expand(HKDF_INFO, &mut subkey)
        .map_err(|_| CryptoError::Corrupt)?;
    Ok(subkey)
}

/// Build a per-chunk nonce: 20-byte prefix followed by the big-endian counter,
/// whose high bit marks the final chunk.
fn build_nonce(prefix: &[u8; NONCE_PREFIX_LEN], counter: u32, is_final: bool) -> XNonce {
    let mut nonce = [0u8; NONCE_LEN];
    nonce[..NONCE_PREFIX_LEN].copy_from_slice(prefix);
    let value = if is_final {
        counter | FINAL_CHUNK_BIT
    } else {
        counter
    };
    nonce[NONCE_PREFIX_LEN..].copy_from_slice(&value.to_be_bytes());
    XNonce::from(nonce)
}

/// Fill `buffer` with cryptographically secure random bytes.
fn fill_random(buffer: &mut [u8]) -> Result<(), CryptoError> {
    getrandom::fill(buffer).map_err(|_| CryptoError::Io)
}

/// Read until `buffer` is full or EOF, returning the byte count and whether EOF
/// was reached first.
fn read_full_or_eof(
    reader: &mut impl Read,
    buffer: &mut [u8],
) -> Result<(usize, bool), CryptoError> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        match reader.read(&mut buffer[filled..]) {
            Ok(0) => return Ok((filled, true)),
            Ok(read) => filled += read,
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(CryptoError::Io),
        }
    }
    Ok((filled, false))
}

/// Read exactly `buffer.len()` bytes; a short read is a size mismatch, which the
/// caller reports as [`CryptoError::Corrupt`].
fn read_exact_mapped(reader: &mut impl Read, buffer: &mut [u8]) -> Result<(), CryptoError> {
    match reader.read_exact(buffer) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Err(CryptoError::Corrupt),
        Err(_) => Err(CryptoError::Io),
    }
}

/// Confirm the plaintext source produced no bytes beyond the declared size.
fn ensure_no_extra(reader: &mut impl Read) -> Result<(), CryptoError> {
    let mut byte = [0u8; 1];
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(()),
            Ok(_) => return Err(CryptoError::Corrupt),
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(CryptoError::Io),
        }
    }
}

fn zeroize_buffer(buffer: &mut [u8]) {
    buffer.zeroize();
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    const SMALL_PARAMS: Params = Params {
        memory: MIN_MEMORY_KIB,
        time: 1,
        parallelism: 1,
        key_len: MASTER_KEY_LEN as u32,
        salt_len: MIN_SALT_LEN,
    };

    fn patterned(size: usize) -> Vec<u8> {
        (0..size)
            .map(|index| ((index * 31 + 17) % 251) as u8)
            .collect()
    }

    fn stream_encrypt(plaintext: &[u8], master: &[u8; 32]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut source = plaintext;
        encrypt_stream(&mut source, &mut output, master).expect("encrypt_stream");
        output
    }

    fn stream_decrypt(ciphertext: &[u8], master: &[u8; 32]) -> Result<(u64, Vec<u8>), CryptoError> {
        let mut output = Vec::new();
        let mut source = ciphertext;
        let size = decrypt_stream(&mut source, &mut output, master)?;
        Ok((size, output))
    }

    fn decode_base64(text: &str) -> Vec<u8> {
        base64::engine::general_purpose::STANDARD
            .decode(text)
            .expect("valid base64")
    }

    // -- vault ---------------------------------------------------------------

    #[test]
    fn create_vault_then_unlock_round_trip() {
        let (material, master) =
            create_vault("correct horse battery staple", "pet name").expect("create_vault");

        assert_eq!(material.salt.len(), MIN_SALT_LEN);
        assert_eq!(material.wrapped_master_key.len(), WRAPPED_MASTER_KEY_LEN);
        assert_eq!(material.key_check.len(), KEY_CHECK_LEN);
        assert_eq!(material.params_json, marshal_params(&DEFAULT_PARAMS));
        assert_eq!(
            parse_params(&material.params_json).expect("parse params"),
            DEFAULT_PARAMS
        );

        let unlocked = unlock(&material, "correct horse battery staple").expect("unlock");
        assert_eq!(unlocked, master);
    }

    #[test]
    fn unlock_rejects_wrong_password() {
        let (material, _) =
            create_vault_with_params("right password", SMALL_PARAMS).expect("create_vault");
        assert_eq!(
            unlock(&material, "wrong password"),
            Err(CryptoError::WrongPassword)
        );
    }

    #[test]
    fn unlock_rejects_tampered_wrapped_key() {
        let (mut material, _) =
            create_vault_with_params("password", SMALL_PARAMS).expect("create_vault");
        let last = material.wrapped_master_key.len() - 1;
        material.wrapped_master_key[last] ^= 0x01;
        assert_eq!(
            unlock(&material, "password"),
            Err(CryptoError::WrongPassword)
        );
    }

    #[test]
    fn unlock_rejects_tampered_key_check() {
        let (mut material, _) =
            create_vault_with_params("password", SMALL_PARAMS).expect("create_vault");
        let last = material.key_check.len() - 1;
        material.key_check[last] ^= 0x01;
        assert_eq!(
            unlock(&material, "password"),
            Err(CryptoError::WrongPassword)
        );
    }

    #[test]
    fn unlock_rejects_corrupt_envelope_sizes() {
        let (material, _) =
            create_vault_with_params("password", SMALL_PARAMS).expect("create_vault");

        let mut short_wrap = material.clone();
        short_wrap.wrapped_master_key.pop();
        assert_eq!(unlock(&short_wrap, "password"), Err(CryptoError::Corrupt));

        let mut long_wrap = material.clone();
        long_wrap.wrapped_master_key.push(0);
        assert_eq!(unlock(&long_wrap, "password"), Err(CryptoError::Corrupt));

        let mut short_check = material.clone();
        short_check.key_check.pop();
        assert_eq!(unlock(&short_check, "password"), Err(CryptoError::Corrupt));

        let mut short_salt = material;
        short_salt.salt.pop();
        assert_eq!(unlock(&short_salt, "password"), Err(CryptoError::Corrupt));
    }

    #[test]
    fn params_json_matches_go_field_names() {
        assert_eq!(
            marshal_params(&DEFAULT_PARAMS),
            r#"{"kdf":"argon2id","memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#
        );
    }

    #[test]
    fn params_json_round_trips_and_accepts_legacy() {
        let encoded = marshal_params(&DEFAULT_PARAMS);
        assert_eq!(parse_params(&encoded).expect("round trip"), DEFAULT_PARAMS);

        // Legacy records without a `kdf` field select Argon2id.
        let legacy = r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#;
        assert_eq!(parse_params(legacy).expect("legacy"), DEFAULT_PARAMS);
    }

    #[test]
    fn parse_params_rejects_untrusted_metadata() {
        let cases = [
            r#"{"memory":0,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"memory":4294967295,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":0,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":4294967295,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":0,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":255,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":0,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":4294967295,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":0}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":2147483647}"#,
            r#"{"kdf":"scrypt","memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"kdf":"","memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"kdf":null,"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16,"surprise":true}"#,
            r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}{}"#,
            r#"{"memory":65536.5,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#,
            "{}",
            "not json",
        ];
        for case in cases {
            assert_eq!(
                parse_params(case),
                Err(CryptoError::UnsupportedKdf),
                "case: {case}"
            );
        }

        let oversized = r#"{"memory":65536,"time":3,"parallelism":4,"key_len":32,"salt_len":16}"#
            .to_string()
            + &" ".repeat(1024);
        assert_eq!(parse_params(&oversized), Err(CryptoError::UnsupportedKdf));
    }

    #[test]
    fn derive_kek_rejects_dangerous_params_before_argon2() {
        let salt = vec![7u8; MIN_SALT_LEN];
        let cases = [
            Params {
                memory: 0,
                ..DEFAULT_PARAMS
            },
            Params {
                memory: u32::MAX,
                ..DEFAULT_PARAMS
            },
            Params {
                time: u32::MAX,
                ..DEFAULT_PARAMS
            },
            Params {
                parallelism: u8::MAX,
                ..DEFAULT_PARAMS
            },
            Params {
                key_len: u32::MAX,
                ..DEFAULT_PARAMS
            },
            Params {
                salt_len: MAX_SALT_LEN + 1,
                ..DEFAULT_PARAMS
            },
        ];
        for params in cases {
            assert_eq!(
                derive_kek(b"password", &salt, &params),
                Err(CryptoError::UnsupportedKdf)
            );
        }
        // Salt length must match the parameters.
        assert_eq!(
            derive_kek(b"password", &salt[..MIN_SALT_LEN - 1], &DEFAULT_PARAMS),
            Err(CryptoError::UnsupportedKdf)
        );
    }

    #[test]
    fn wipe_zeroizes_key() {
        let mut key = [0xABu8; 32];
        wipe(&mut key);
        assert_eq!(key, [0u8; 32]);
    }

    // -- header --------------------------------------------------------------

    #[test]
    fn parse_header_accepts_valid_header() {
        let master = [0x11u8; 32];
        let ciphertext = stream_encrypt(&patterned(10), &master);
        let header = parse_header(&ciphertext[..HEADER_LEN]).expect("header");
        assert_eq!(header.flags, 0);
        assert_eq!(header.chunk_log2, CHUNK_LOG2);
        assert_eq!(header.plaintext_size, 10);
    }

    #[test]
    fn parse_header_rejects_bad_magic() {
        let master = [0x12u8; 32];
        let mut ciphertext = stream_encrypt(&patterned(10), &master);
        ciphertext[0] ^= 0xff;
        assert_eq!(
            parse_header(&ciphertext[..HEADER_LEN]),
            Err(CryptoError::Corrupt)
        );
    }

    #[test]
    fn parse_header_rejects_impossible_chunk_log2() {
        let master = [0x13u8; 32];
        let ciphertext = stream_encrypt(&patterned(10), &master);
        for bad in [0u8, CHUNK_LOG2 - 1, CHUNK_LOG2 + 1, 31, 255] {
            let mut header = ciphertext[..HEADER_LEN].to_vec();
            header[5] = bad;
            assert_eq!(
                parse_header(&header),
                Err(CryptoError::Corrupt),
                "chunk_log2 {bad}"
            );
        }
    }

    #[test]
    fn parse_header_rejects_nonzero_flags_and_short_buffers() {
        let master = [0x14u8; 32];
        let ciphertext = stream_encrypt(&patterned(10), &master);

        let mut flagged = ciphertext[..HEADER_LEN].to_vec();
        flagged[4] = 1;
        assert_eq!(parse_header(&flagged), Err(CryptoError::Corrupt));

        for len in [0usize, 1, HEADER_LEN - 1] {
            assert_eq!(
                parse_header(&ciphertext[..len]),
                Err(CryptoError::Corrupt),
                "len {len}"
            );
        }
    }

    #[test]
    fn parse_header_rejects_impossible_plaintext_size() {
        let master = [0x15u8; 32];
        let mut header = stream_encrypt(&patterned(10), &master)[..HEADER_LEN].to_vec();
        header[42..50].copy_from_slice(&(MAX_PLAINTEXT_SIZE + 1).to_le_bytes());
        assert_eq!(parse_header(&header), Err(CryptoError::Corrupt));
    }

    // -- ciphertext size -----------------------------------------------------

    #[test]
    fn ciphertext_size_matches_encrypt_stream() {
        let master = [0x21u8; 32];
        let sizes = [
            0u64,
            1,
            1000,
            65535,
            65536,
            65537,
            131072,
            200000,
            3 * CHUNK_SIZE as u64 + 7,
        ];
        for size in sizes {
            let plaintext = patterned(size as usize);
            let ciphertext = stream_encrypt(&plaintext, &master);
            assert_eq!(
                ciphertext.len() as u64,
                ciphertext_size(size),
                "size {size}"
            );
        }
    }

    #[test]
    fn ciphertext_size_saturates_on_overflow() {
        assert_eq!(ciphertext_size(u64::MAX), u64::MAX);
        assert_eq!(ciphertext_size(MAX_PLAINTEXT_SIZE + 1), u64::MAX);
        // Documented constants: an empty file is 66 bytes, one full chunk 65618.
        assert_eq!(ciphertext_size(0), 66);
        assert_eq!(ciphertext_size(65536), 65618);
    }

    #[test]
    fn validate_plaintext_size_matches_go_expectations() {
        assert_eq!(MAX_PLAINTEXT_SIZE, (1u64 << 47) - 1);
        for size in [0u64, 1, 65535, 65536, 65537, 131072, 200000, 5_000_000] {
            assert_eq!(
                validate_plaintext_size(ciphertext_size(size), size),
                Some(size),
                "size {size}"
            );
        }
        assert_eq!(
            validate_plaintext_size(ciphertext_size(MAX_PLAINTEXT_SIZE), MAX_PLAINTEXT_SIZE),
            Some(MAX_PLAINTEXT_SIZE)
        );
        assert_eq!(
            validate_plaintext_size(
                ciphertext_size(MAX_PLAINTEXT_SIZE + 1),
                MAX_PLAINTEXT_SIZE + 1
            ),
            None
        );
        assert_eq!(validate_plaintext_size(ciphertext_size(0) + 1, 0), None);
        assert_eq!(validate_plaintext_size(ciphertext_size(0) - 1, 0), None);
        assert_eq!(validate_plaintext_size(ciphertext_size(1), 0), None);
    }

    // -- streams -------------------------------------------------------------

    #[test]
    fn stream_round_trip_across_chunk_sizes() {
        let master = [0x31u8; 32];
        let sizes = [
            0usize,
            1,
            CHUNK_SIZE - 1,
            CHUNK_SIZE,
            CHUNK_SIZE + 1,
            3 * CHUNK_SIZE + 7,
        ];
        for size in sizes {
            let plaintext = patterned(size);
            let ciphertext = stream_encrypt(&plaintext, &master);
            let (declared, decrypted) = stream_decrypt(&ciphertext, &master).expect("decrypt");
            assert_eq!(declared, size as u64, "size {size}");
            assert_eq!(decrypted, plaintext, "size {size}");
            assert_eq!(ciphertext.len() as u64, ciphertext_size(size as u64));
        }
    }

    #[test]
    fn stream_exact_multiple_uses_empty_final_chunk() {
        let master = [0x32u8; 32];
        let plaintext = patterned(CHUNK_SIZE);
        let ciphertext = stream_encrypt(&plaintext, &master);

        // header + one full chunk with tag + one empty final chunk with tag.
        assert_eq!(
            ciphertext.len(),
            HEADER_LEN + CHUNK_SIZE + TAG_LEN + TAG_LEN
        );

        // Removing the empty final chunk truncates the stream.
        assert_eq!(
            stream_decrypt(&ciphertext[..ciphertext.len() - TAG_LEN], &master),
            Err(CryptoError::Corrupt)
        );
        // Damaging the final tag is detected.
        let mut tampered = ciphertext.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 0x80;
        assert_eq!(
            stream_decrypt(&tampered, &master),
            Err(CryptoError::Corrupt)
        );
    }

    #[test]
    fn decrypt_rejects_tampered_ciphertext_without_output() {
        let master = [0x33u8; 32];
        let mut ciphertext = stream_encrypt(&patterned(100), &master);
        ciphertext[HEADER_LEN + 1] ^= 0x80;
        assert_eq!(
            stream_decrypt(&ciphertext, &master),
            Err(CryptoError::Corrupt)
        );

        // A later-chunk tamper fails when that chunk is reached.
        let mut multi = stream_encrypt(&patterned(2 * CHUNK_SIZE + 5), &master);
        multi[HEADER_LEN + CHUNK_SIZE + TAG_LEN + 9] ^= 0x40;
        assert_eq!(stream_decrypt(&multi, &master), Err(CryptoError::Corrupt));
    }

    #[test]
    fn decrypt_rejects_truncated_container() {
        let master = [0x34u8; 32];
        let ciphertext = stream_encrypt(&patterned(3 * CHUNK_SIZE), &master);

        // Chop off part of the final chunk.
        assert_eq!(
            stream_decrypt(&ciphertext[..ciphertext.len() - 32], &master),
            Err(CryptoError::Corrupt)
        );
        // A header-only stream has no final chunk.
        assert_eq!(
            stream_decrypt(&ciphertext[..HEADER_LEN], &master),
            Err(CryptoError::Corrupt)
        );
        assert_eq!(
            stream_decrypt(&ciphertext[..HEADER_LEN - 1], &master),
            Err(CryptoError::Corrupt)
        );
    }

    #[test]
    fn decrypt_rejects_wrong_master_key() {
        let ciphertext = stream_encrypt(&patterned(1000), &[0x41u8; 32]);
        assert_eq!(
            stream_decrypt(&ciphertext, &[0x42u8; 32]),
            Err(CryptoError::Corrupt)
        );
    }

    #[test]
    fn decrypt_rejects_unsupported_flags_and_impossible_size() {
        let master = [0x51u8; 32];
        let ciphertext = stream_encrypt(&patterned(10), &master);

        let mut flags = ciphertext.clone();
        flags[4] = 1;
        assert_eq!(stream_decrypt(&flags, &master), Err(CryptoError::Corrupt));

        let mut max_size = ciphertext.clone();
        max_size[42..50].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            stream_decrypt(&max_size, &master),
            Err(CryptoError::Corrupt)
        );

        let mut capacity = ciphertext.clone();
        capacity[42..50].copy_from_slice(&(MAX_PLAINTEXT_SIZE + 1).to_le_bytes());
        assert_eq!(
            stream_decrypt(&capacity, &master),
            Err(CryptoError::Corrupt)
        );

        let mut declared = ciphertext;
        declared[42..50].copy_from_slice(&11u64.to_le_bytes());
        assert_eq!(
            stream_decrypt(&declared, &master),
            Err(CryptoError::Corrupt)
        );
    }

    #[test]
    fn decrypt_never_writes_unauthenticated_bytes() {
        let master = [0x61u8; 32];
        let ciphertext = stream_encrypt(&patterned(100), &master);
        let mut tampered = ciphertext;
        tampered[HEADER_LEN + 3] ^= 0x01;

        let mut output = Vec::new();
        let mut source = tampered.as_slice();
        let result = decrypt_stream(&mut source, &mut output, &master);
        assert_eq!(result, Err(CryptoError::Corrupt));
        assert!(output.is_empty());
    }

    #[test]
    fn encrypt_stream_rejects_declared_size_mismatch() {
        // Mirrors Go's EncryptStream length bookkeeping through the private
        // sized entry point, which the public API always calls consistently.
        let master = [0x71u8; 32];
        let mut output = Vec::new();

        let short = patterned(CHUNK_SIZE - 1);
        let mut source = short.as_slice();
        assert_eq!(
            encrypt_stream_sized(&mut source, &mut output, &master, CHUNK_SIZE as u64),
            Err(CryptoError::Corrupt)
        );

        output.clear();
        let long = patterned(CHUNK_SIZE + 1);
        let mut source = long.as_slice();
        assert_eq!(
            encrypt_stream_sized(&mut source, &mut output, &master, CHUNK_SIZE as u64),
            Err(CryptoError::Corrupt)
        );
    }

    // -- cross-language vectors ---------------------------------------------
    //
    // These fixtures were produced by running the Go package's exported
    // primitives (see the port notes). They pin Argon2id, the vault envelope,
    // the params JSON and the TDE1 stream layout to the Go format authority.

    #[test]
    fn unlock_matches_go_produced_vault_material() {
        // Produced by Go crypto.DeriveKEK/WrapMasterKey/EncodeKeyCheck with a
        // fixed salt, master, password "vector-password" and params
        // {"kdf":"argon2id","memory":16384,"time":1,"parallelism":1,"key_len":32,"salt_len":16}.
        let material = VaultMaterial {
            salt: decode_base64("AAECAwQFBgcICQoLDA0ODw=="),
            params_json: r#"{"kdf":"argon2id","memory":16384,"time":1,"parallelism":1,"key_len":32,"salt_len":16}"#
                .to_string(),
            wrapped_master_key: decode_base64(
                "TZRBFHG6HML/v/5sTxy59CP2ZNOZw+ifCovwkb4bQ0Hr1tl7M90ZpV+Bb8NZftJU/OI8iyFGn1VeWurer8NpChxQNzzGMCTe",
            ),
            key_check: decode_base64(
                "GReVCXWmpBvZcgC34BJDlt4Ma2FGZ1kDF+OlF2MtzFE2y3q6VOV6190IBxSQcKOdVxTGGHjIktXwHK0=",
            ),
        };
        let master = unlock(&material, "vector-password").expect("unlock Go material");
        assert_eq!(
            master.as_slice(),
            decode_base64("QEFCQ0RFRkdISUpLTE1OT1BRUlNUVVZXWFlaW1xdXl8=").as_slice()
        );
    }

    #[test]
    fn decrypt_stream_accepts_go_produced_ciphertext() {
        // Produced by Go crypto.EncryptStream with a fixed master and a
        // 137-byte patterned plaintext.
        let master: [u8; 32] = decode_base64("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=")
            .try_into()
            .expect("32-byte master");
        let ciphertext = decode_base64(
            "VERFMQAQHfJODuHnLIFoB0sROUnftDy7IyB1O7C74fqjAvELE9KRAJU6iQAAAAAAAADwmp+smMwQngJxoRf4+tH44svdfl70frPWN1ZEsIDNV6fPG+cBWKvutKUYDzvLVjVTlQR/vF7N2iCnL8NibFjX1HU93/7nvvdJuSu7WoN6DlB6gOh8BcZu9JQQt+qh9j0A+HIWafKeZ/EidWZ3JpbInM1HeWYBQ86SKqHO3YVeRlEoA+yrH4H+IiQslYDtqwkZ+jSuzTrgLxc=",
        );
        let expected = decode_base64(
            "ETBPbo2sy+oOLUxriqnI5wsqSWiHpsXkCCdGZYSjwuEFJENigaC/3gIhQF9+nbzb+h49XHuaudj3GzpZeJe21fQYN1Z1lLPS8RU0U3KRsM/uEjFQb46tzOsPLk1si6rJ6AwrSmmIp8blCShHZoWkw+IGJURjgqHA3wMiQWB/nr3cAB8+XXybutk=",
        );
        let (size, plaintext) = stream_decrypt(&ciphertext, &master).expect("decrypt Go stream");
        assert_eq!(size, expected.len() as u64);
        assert_eq!(plaintext, expected);
        assert_eq!(ciphertext.len() as u64, ciphertext_size(size));
    }
}
