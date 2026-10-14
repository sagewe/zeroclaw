use std::path::Path;

use anyhow::{Context, Result, ensure};
use parking_lot::Mutex;
use rusqlite::{Connection, OptionalExtension, params};

use super::{Checkpoint, PendingEvent};

/// Owns ingestion checkpoints and delivery admission only. SOP owns run state.
pub(super) struct Store(Mutex<Connection>);

impl Store {
    pub fn open(directory: &Path) -> Result<Self> {
        if let Ok(metadata) = std::fs::symlink_metadata(directory) {
            ensure!(
                metadata.is_dir() && !metadata.file_type().is_symlink(),
                "event store directory must not be a symlink"
            );
        }
        std::fs::create_dir_all(directory)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
        }
        // macOS /var is itself a symlink. Resolve trusted parent components
        // after refusing a symlink at our own directory boundary, then let
        // SQLite enforce NOFOLLOW on the database it actually opens.
        let path = directory.canonicalize()?.join("events.db");
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            ensure!(
                metadata.is_file() && !metadata.file_type().is_symlink(),
                "event store must be a regular file"
            );
        }
        let connection = Connection::open_with_flags(
            &path,
            rusqlite::OpenFlags::default() | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .context("open event watch store")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        }
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        connection.execute_batch("PRAGMA synchronous=FULL;
            CREATE TABLE IF NOT EXISTS checkpoint (id INTEGER PRIMARY KEY CHECK(id=1), data TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, revision TEXT NOT NULL, payload TEXT NOT NULL, status TEXT NOT NULL);
            CREATE INDEX IF NOT EXISTS events_status ON events(status);")?;
        // A crash between admission and recording its outcome is ambiguous.
        // Never repeat an autonomous side effect merely because its receipt was lost.
        connection.execute(
            "UPDATE events SET status='uncertain' WHERE status='dispatching'",
            [],
        )?;
        Ok(Self(Mutex::new(connection)))
    }

    pub fn load(&self) -> Result<Checkpoint> {
        let data: Option<String> = self
            .0
            .lock()
            .query_row("SELECT data FROM checkpoint WHERE id=1", [], |r| r.get(0))
            .optional()?;
        data.map(|s| serde_json::from_str(&s).context("decode event checkpoint"))
            .transpose()
            .map(|s| s.unwrap_or_default())
    }

    pub fn save(&self, checkpoint: &Checkpoint, events: &[PendingEvent]) -> Result<()> {
        let mut db = self.0.lock();
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE events SET status='superseded' WHERE status='queued' AND revision != ?1",
            [&checkpoint.revision],
        )?;
        let queued: i64 = tx.query_row(
            "SELECT COUNT(*) FROM events WHERE status IN ('queued','dispatching','uncertain')",
            [],
            |r| r.get(0),
        )?;
        ensure!(
            queued + i64::try_from(events.len())? <= 1000,
            "event watch outbox full; checkpoint not advanced"
        );
        for event in events {
            tx.execute("INSERT OR IGNORE INTO events(id,revision,payload,status) VALUES(?1,?2,?3,'queued')",
                params![event.id, checkpoint.revision, serde_json::to_string(event)?])?;
        }
        tx.execute("INSERT INTO checkpoint(id,data) VALUES(1,?1) ON CONFLICT(id) DO UPDATE SET data=excluded.data", [serde_json::to_string(checkpoint)?])?;
        tx.execute("DELETE FROM events WHERE status NOT IN ('queued','dispatching','uncertain') AND rowid NOT IN (SELECT rowid FROM events ORDER BY rowid DESC LIMIT 10000)", [])?;
        tx.commit()?;
        Ok(())
    }

    pub fn next(&self) -> Result<Option<PendingEvent>> {
        let data: Option<String> = self
            .0
            .lock()
            .query_row(
                "SELECT payload FROM events WHERE status='queued' ORDER BY rowid LIMIT 1",
                [],
                |r| r.get(0),
            )
            .optional()?;
        data.map(|s| serde_json::from_str(&s).context("decode queued event"))
            .transpose()
    }

    pub fn mark(&self, id: &str, status: &str) -> Result<()> {
        self.0.lock().execute(
            "UPDATE events SET status=?1 WHERE id=?2",
            params![status, id],
        )?;
        Ok(())
    }

    pub fn uncertain_count(&self) -> Result<i64> {
        Ok(self.0.lock().query_row(
            "SELECT COUNT(*) FROM events WHERE status='uncertain'",
            [],
            |r| r.get(0),
        )?)
    }
}
