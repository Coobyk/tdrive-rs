//! The TDrive engine: the drive's format, its Telegram transport and its
//! cryptography, with no user interface attached.
//!
//! The desktop shell in this package draws with it, and the phone app in
//! `Coobyk/tdrive-android` speaks to it over JNI. Both applications read and
//! write the same drive in the same way, because they are the same code: the
//! same operation format, the same encryption, and the same small pictures
//! published beside a file.

pub mod cache;
pub mod config;
pub mod crypto;
pub mod format;
pub mod models;
pub mod ops;
pub mod palettes;
pub mod rendition;
pub mod tele;
pub mod themes;
