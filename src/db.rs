//! SQLite persistence for nonces and submissions.
//!
//! Times are stored as Unix epoch seconds (INTEGER). All operations take the
//! "current time" as an input parameter so that they can be tested without
//! touching the clock.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

const SCHEMA: &str = include_str!("schema.sql");

pub struct Db {
    conn: Connection,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ConsumeError {
    NotFound,
    Expired,
    AlreadyUsed,
}

#[derive(Debug, PartialEq, Eq)]
pub struct NonceRecord {
    pub nonce: String,
    pub difficulty: u32,
    pub expires_at: i64,
}

impl Db {
    pub fn open<P: AsRef<Path>>(path: P) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(SCHEMA)?;
        Ok(Db { conn })
    }

    pub fn insert_nonce(
        &self,
        nonce: &str,
        difficulty: u32,
        now: i64,
        expires_at: i64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "INSERT INTO nonces (nonce, difficulty, created_at, expires_at) VALUES (?, ?, ?, ?)",
            params![nonce, difficulty, now, expires_at],
        )?;
        Ok(())
    }

    pub fn get_nonce(&self, nonce: &str) -> rusqlite::Result<Option<(u32, i64, Option<i64>)>> {
        self.conn
            .query_row(
                "SELECT difficulty, expires_at, used_at FROM nonces WHERE nonce = ?",
                params![nonce],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()
    }

    /// Atomically check-and-mark a nonce as used. Returns the difficulty on
    /// success so callers can verify the PoW against it.
    pub fn consume_nonce(&mut self, nonce: &str, now: i64) -> Result<u32, ConsumeError> {
        let tx = self
            .conn
            .transaction()
            .map_err(|_| ConsumeError::NotFound)?;
        let row: Option<(u32, i64, Option<i64>)> = tx
            .query_row(
                "SELECT difficulty, expires_at, used_at FROM nonces WHERE nonce = ?",
                params![nonce],
                |row| {
                    Ok((
                        row.get::<_, u32>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, Option<i64>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|_| ConsumeError::NotFound)?;
        let (difficulty, expires_at, used_at) = match row {
            Some(r) => r,
            None => return Err(ConsumeError::NotFound),
        };
        if used_at.is_some() {
            return Err(ConsumeError::AlreadyUsed);
        }
        if expires_at <= now {
            return Err(ConsumeError::Expired);
        }
        tx.execute(
            "UPDATE nonces SET used_at = ? WHERE nonce = ?",
            params![now, nonce],
        )
        .map_err(|_| ConsumeError::NotFound)?;
        tx.commit().map_err(|_| ConsumeError::NotFound)?;
        Ok(difficulty)
    }

    pub fn insert_submission(&self, nonce: &str, data: &str, now: i64) -> rusqlite::Result<i64> {
        self.conn.execute(
            "INSERT INTO submissions (nonce, data, submitted_at) VALUES (?, ?, ?)",
            params![nonce, data, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Deletes nonces whose expires_at is in the past. Returns the number of
    /// nonces removed.
    pub fn prune_expired(&self, now: i64) -> rusqlite::Result<usize> {
        let n = self
            .conn
            .execute("DELETE FROM nonces WHERE expires_at <= ?", params![now])?;
        Ok(n)
    }

    pub fn count_nonces(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM nonces", [], |row| row.get(0))
    }

    pub fn count_submissions(&self) -> rusqlite::Result<i64> {
        self.conn
            .query_row("SELECT COUNT(*) FROM submissions", [], |row| row.get(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_initializes() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.count_nonces().unwrap(), 0);
        assert_eq!(db.count_submissions().unwrap(), 0);
    }

    #[test]
    fn insert_and_get_nonce() {
        let db = Db::open_in_memory().unwrap();
        db.insert_nonce("abcd", 10, 100, 200).unwrap();
        let got = db.get_nonce("abcd").unwrap().unwrap();
        assert_eq!(got, (10, 200, None));
    }

    #[test]
    fn get_missing_nonce_returns_none() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(db.get_nonce("nope").unwrap(), None);
    }

    #[test]
    fn consume_nonce_happy_path() {
        let mut db = Db::open_in_memory().unwrap();
        db.insert_nonce("abcd", 12, 100, 200).unwrap();
        let d = db.consume_nonce("abcd", 150).unwrap();
        assert_eq!(d, 12);
        let got = db.get_nonce("abcd").unwrap().unwrap();
        assert_eq!(got.2, Some(150));
    }

    #[test]
    fn consume_nonce_missing() {
        let mut db = Db::open_in_memory().unwrap();
        assert_eq!(db.consume_nonce("nope", 10), Err(ConsumeError::NotFound));
    }

    #[test]
    fn consume_nonce_expired() {
        let mut db = Db::open_in_memory().unwrap();
        db.insert_nonce("abcd", 1, 100, 200).unwrap();
        assert_eq!(db.consume_nonce("abcd", 200), Err(ConsumeError::Expired));
        assert_eq!(db.consume_nonce("abcd", 201), Err(ConsumeError::Expired));
    }

    #[test]
    fn consume_nonce_already_used() {
        let mut db = Db::open_in_memory().unwrap();
        db.insert_nonce("abcd", 1, 100, 200).unwrap();
        db.consume_nonce("abcd", 150).unwrap();
        assert_eq!(
            db.consume_nonce("abcd", 160),
            Err(ConsumeError::AlreadyUsed)
        );
    }

    #[test]
    fn insert_submission_records_row() {
        let db = Db::open_in_memory().unwrap();
        let id = db.insert_submission("abcd", "k=v&x=y", 42).unwrap();
        assert!(id > 0);
        assert_eq!(db.count_submissions().unwrap(), 1);
    }

    #[test]
    fn prune_removes_expired_only() {
        let db = Db::open_in_memory().unwrap();
        db.insert_nonce("a", 1, 0, 50).unwrap();
        db.insert_nonce("b", 1, 0, 100).unwrap();
        db.insert_nonce("c", 1, 0, 200).unwrap();
        let n = db.prune_expired(100).unwrap();
        assert_eq!(n, 2); // a and b (expires_at <= 100)
        assert!(db.get_nonce("a").unwrap().is_none());
        assert!(db.get_nonce("b").unwrap().is_none());
        assert!(db.get_nonce("c").unwrap().is_some());
    }
}
