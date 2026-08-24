//! Typed read/write helpers over the schema (PRD §3.2).

use crate::config::WorldConfig;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct World {
    pub id: i64,
    pub name: String,
    pub seed: i64,
    pub created_at: String,
    pub current_tick: i64,
    pub status: String,
}

/// Create a world row and return it. `config` is stored verbatim as
/// `config_json`, which is the single source of truth for this world's rules.
pub fn create_world(
    conn: &Connection,
    name: &str,
    seed: i64,
    config: &WorldConfig,
) -> Result<World> {
    let config_json = serde_json::to_string(config).context("serialising world config")?;
    let created_at = now_iso8601();

    conn.execute(
        "INSERT INTO worlds (name, seed, config_json, created_at, current_tick, status)
         VALUES (?1, ?2, ?3, ?4, 0, 'active')",
        rusqlite::params![name, seed, config_json, created_at],
    )
    .context("inserting world row")?;

    let id = conn.last_insert_rowid();
    tracing::info!(world_id = id, name, seed, "created world");

    Ok(World {
        id,
        name: name.to_string(),
        seed,
        created_at,
        current_tick: 0,
        status: "active".into(),
    })
}

pub fn load_world(conn: &Connection, id: i64) -> Result<Option<World>> {
    let world = conn
        .query_row(
            "SELECT id, name, seed, created_at, current_tick, status FROM worlds WHERE id = ?1",
            [id],
            row_to_world,
        )
        .optional()?;
    Ok(world)
}

/// The config as stored for this world, with any fields absent from an older
/// save filled in from defaults.
pub fn load_world_config(conn: &Connection, id: i64) -> Result<Option<WorldConfig>> {
    let json: Option<String> = conn
        .query_row("SELECT config_json FROM worlds WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;

    match json {
        None => Ok(None),
        Some(j) => Ok(Some(
            serde_json::from_str(&j).context("parsing stored world config")?,
        )),
    }
}

pub fn list_worlds(conn: &Connection) -> Result<Vec<World>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, seed, created_at, current_tick, status
         FROM worlds ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], row_to_world)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Find the newest active world, if there is one.
pub fn latest_active_world(conn: &Connection) -> Result<Option<World>> {
    let world = conn
        .query_row(
            "SELECT id, name, seed, created_at, current_tick, status
             FROM worlds WHERE status = 'active' ORDER BY id DESC LIMIT 1",
            [],
            row_to_world,
        )
        .optional()?;
    Ok(world)
}

pub fn set_current_tick(conn: &Connection, world_id: i64, tick: i64) -> Result<()> {
    conn.execute(
        "UPDATE worlds SET current_tick = ?2 WHERE id = ?1",
        rusqlite::params![world_id, tick],
    )?;
    Ok(())
}

fn row_to_world(r: &rusqlite::Row) -> rusqlite::Result<World> {
    Ok(World {
        id: r.get(0)?,
        name: r.get(1)?,
        seed: r.get(2)?,
        created_at: r.get(3)?,
        current_tick: r.get(4)?,
        status: r.get(5)?,
    })
}

fn now_iso8601() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("migrations/001_initial.sql")).unwrap();
        conn
    }

    #[test]
    fn creates_and_reads_back_a_world() {
        let conn = db();
        let cfg = WorldConfig::default();
        let w = create_world(&conn, "Ashfen", 44127, &cfg).unwrap();

        assert_eq!(w.name, "Ashfen");
        assert_eq!(w.seed, 44127);
        assert_eq!(w.current_tick, 0);
        assert_eq!(w.status, "active");

        let loaded = load_world(&conn, w.id).unwrap().unwrap();
        assert_eq!(loaded.id, w.id);
        assert_eq!(loaded.seed, 44127);
    }

    #[test]
    fn config_survives_the_round_trip_through_the_row() {
        let conn = db();
        let mut cfg = WorldConfig::default();
        cfg.features.wheat = false; // the S4 experiment
        cfg.llm.model = "qwen3:4b".into();

        let w = create_world(&conn, "NoWheat", 7, &cfg).unwrap();
        let back = load_world_config(&conn, w.id).unwrap().unwrap();

        assert!(!back.features.wheat);
        assert_eq!(back.llm.model, "qwen3:4b");
        assert_eq!(back.map.width, 512);
    }

    #[test]
    fn missing_world_reads_as_none() {
        let conn = db();
        assert!(load_world(&conn, 999).unwrap().is_none());
        assert!(load_world_config(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn latest_active_world_picks_the_newest() {
        let conn = db();
        let cfg = WorldConfig::default();
        create_world(&conn, "first", 1, &cfg).unwrap();
        let second = create_world(&conn, "second", 2, &cfg).unwrap();

        assert_eq!(latest_active_world(&conn).unwrap().unwrap().id, second.id);
        assert_eq!(list_worlds(&conn).unwrap().len(), 2);
    }

    #[test]
    fn current_tick_advances() {
        let conn = db();
        let w = create_world(&conn, "Ashfen", 1, &WorldConfig::default()).unwrap();
        set_current_tick(&conn, w.id, 4118).unwrap();
        assert_eq!(load_world(&conn, w.id).unwrap().unwrap().current_tick, 4118);
    }
}
