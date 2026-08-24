//! SQLite persistence. WAL mode, one transaction per tick (PRD §3.1, §7).

pub mod repo;

use anyhow::{Context, Result};
use rusqlite::Connection;
use std::path::Path;

/// Migrations are embedded so a packaged build has no external file dependency.
/// Append-only: never edit a migration that has shipped.
const MIGRATIONS: &[(i32, &str, &str)] = &[(
    1,
    "001_initial",
    include_str!("migrations/001_initial.sql"),
)];

/// Open the database, apply pragmas, and bring the schema up to date.
pub fn open(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }

    let conn = Connection::open(path)
        .with_context(|| format!("opening database at {}", path.display()))?;

    // WAL lets the reporting layer read while the sim thread writes.
    conn.pragma_update(None, "journal_mode", "WAL")?;
    // NORMAL is the right durability trade for a simulation we can re-run:
    // it survives process crashes, only risking the last tick on power loss.
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;

    migrate(&conn)?;
    Ok(conn)
}

/// Apply any migrations newer than the recorded `user_version`.
fn migrate(conn: &Connection) -> Result<()> {
    let current: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let target = MIGRATIONS.last().map(|m| m.0).unwrap_or(0);

    if current > target {
        anyhow::bail!(
            "database schema version {current} is newer than this build understands ({target}); \
             refusing to run rather than corrupt it"
        );
    }
    if current == target {
        tracing::info!(version = current, "schema up to date");
        return Ok(());
    }

    for (version, name, sql) in MIGRATIONS.iter().filter(|m| m.0 > current) {
        tracing::info!(version, name, "applying migration");
        conn.execute_batch(sql)
            .with_context(|| format!("applying migration {name}"))?;
        // execute_batch cannot bind parameters, and user_version rejects them.
        conn.execute_batch(&format!("PRAGMA user_version = {version}"))?;
    }

    tracing::info!(from = current, to = target, "schema migrated");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        migrate(&conn).unwrap();
        conn
    }

    #[test]
    fn migrations_apply_from_empty() {
        let conn = mem();
        let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn migrations_are_idempotent() {
        let conn = mem();
        // Running again must be a no-op rather than an error.
        migrate(&conn).unwrap();
        let v: i32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, 1);
    }

    #[test]
    fn every_expected_table_exists() {
        let conn = mem();
        for table in [
            "worlds", "chunks", "resource_nodes", "creatures", "beliefs",
            "transmissions", "households", "structures", "relationships",
            "events", "decisions", "tick_stats", "creature_samples",
        ] {
            let n: i32 = conn
                .query_row(
                    "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [table],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(n, 1, "table {table} missing");
        }
    }

    #[test]
    fn refuses_a_newer_schema_than_it_understands() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA user_version = 99").unwrap();
        assert!(migrate(&conn).is_err());
    }
}
