//! Local metadata store.
//!
//! This is the persistence seam the port has not wired up yet: listings are
//! read from Telegram on demand and the trash lives in memory, so nothing
//! opens this connection today. The schema is here so the shape of the store
//! is decided once rather than reinvented when offline listings land.

use rusqlite::{Connection, Result};
use std::path::Path;

#[allow(dead_code)]
pub fn init(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        r#"
        PRAGMA journal_mode=WAL;
        CREATE TABLE IF NOT EXISTS dialogs (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS files (
            drive_id INTEGER NOT NULL,
            message_id INTEGER NOT NULL,
            name TEXT NOT NULL,
            size INTEGER NOT NULL,
            mime TEXT,
            PRIMARY KEY(drive_id, message_id)
        );
        "#,
    )?;
    Ok(conn)
}
