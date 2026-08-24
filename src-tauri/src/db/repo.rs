//! Typed read/write helpers over the schema (PRD §3.2).

use crate::config::WorldConfig;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldRow {
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
) -> Result<WorldRow> {
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

    Ok(WorldRow {
        id,
        name: name.to_string(),
        seed,
        created_at,
        current_tick: 0,
        status: "active".into(),
    })
}

pub fn load_world(conn: &Connection, id: i64) -> Result<Option<WorldRow>> {
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

pub fn list_worlds(conn: &Connection) -> Result<Vec<WorldRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, seed, created_at, current_tick, status
         FROM worlds ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], row_to_world)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Find the newest active world, if there is one.
pub fn latest_active_world(conn: &Connection) -> Result<Option<WorldRow>> {
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

fn row_to_world(r: &rusqlite::Row) -> rusqlite::Result<WorldRow> {
    Ok(WorldRow {
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

// ---------------------------------------------------------------- worldgen

use crate::sim::terrain::Terrain;
use crate::sim::world::{NodeKind, ResourceNode, World};

/// Persist terrain and resource nodes. One transaction for the whole world:
/// 256 chunk inserts done individually would dominate generation time.
pub fn save_world(conn: &mut Connection, world_id: i64, world: &World) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute("DELETE FROM chunks WHERE world_id = ?1", [world_id])?;
    tx.execute("DELETE FROM resource_nodes WHERE world_id = ?1", [world_id])?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO chunks (world_id, cx, cy, terrain_blob) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for cy in 0..world.chunks_y() {
            for cx in 0..world.chunks_x() {
                stmt.execute(rusqlite::params![world_id, cx, cy, world.chunk_blob(cx, cy)])?;
            }
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO resource_nodes
               (world_id, kind, x, y, quantity, max_quantity, regen_rate, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active')",
        )?;
        for n in &world.nodes {
            stmt.execute(rusqlite::params![
                world_id, n.kind.as_str(), n.x, n.y,
                n.quantity, n.max_quantity, n.regen_rate
            ])?;
        }
    }

    tx.commit()?;
    tracing::info!(world_id, chunks = world.chunks_x() * world.chunks_y(),
                   nodes = world.nodes.len(), "world persisted");
    Ok(())
}

pub fn world_has_terrain(conn: &Connection, world_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE world_id = ?1", [world_id], |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Reassemble the tile grid from its chunks.
pub fn load_terrain(
    conn: &Connection, world_id: i64, width: u32, height: u32, chunk_size: u32,
) -> Result<Option<Vec<Terrain>>> {
    if !world_has_terrain(conn, world_id)? {
        return Ok(None);
    }
    let mut tiles = vec![Terrain::DeepWater; (width as usize) * (height as usize)];

    let mut stmt = conn.prepare(
        "SELECT cx, cy, terrain_blob FROM chunks WHERE world_id = ?1",
    )?;
    let mut rows = stmt.query([world_id])?;
    while let Some(row) = rows.next()? {
        let cx: u32 = row.get(0)?;
        let cy: u32 = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;

        for ty in 0..chunk_size {
            for tx in 0..chunk_size {
                let (x, y) = (cx * chunk_size + tx, cy * chunk_size + ty);
                if x >= width || y >= height {
                    continue; // padding on an edge chunk
                }
                let b = blob[(ty * chunk_size + tx) as usize];
                let t = Terrain::from_u8(b)
                    .ok_or_else(|| anyhow::anyhow!("unknown terrain byte {b} at {x},{y}"))?;
                tiles[(y as usize) * (width as usize) + (x as usize)] = t;
            }
        }
    }
    Ok(Some(tiles))
}

pub fn load_resource_nodes(conn: &Connection, world_id: i64) -> Result<Vec<ResourceNode>> {
    let mut stmt = conn.prepare(
        "SELECT kind, x, y, quantity, max_quantity, regen_rate
         FROM resource_nodes WHERE world_id = ?1 AND state = 'active' ORDER BY id",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        let kind: String = r.get(0)?;
        Ok(ResourceNode {
            kind: match kind.as_str() {
                "FORAGE" => NodeKind::Forage,
                "WOOD" => NodeKind::Wood,
                "WHEAT" => NodeKind::Wheat,
                _ => NodeKind::Sheep,
            },
            x: r.get(1)?, y: r.get(2)?,
            quantity: r.get(3)?, max_quantity: r.get(4)?, regen_rate: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
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
    fn terrain_survives_a_save_load_round_trip() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 44127, &cfg).unwrap();

        let generated = crate::sim::worldgen::generate(44127, &cfg).world;
        save_world(&mut conn, w.id, &generated).unwrap();

        let loaded = load_terrain(&conn, w.id, 128, 128, cfg.map.chunk_size)
            .unwrap()
            .expect("terrain should be present");

        assert_eq!(loaded, generated.tiles, "tiles must round-trip byte-identically");
    }

    #[test]
    fn resource_nodes_survive_a_round_trip() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 7, &cfg).unwrap();

        let generated = crate::sim::worldgen::generate(7, &cfg).world;
        save_world(&mut conn, w.id, &generated).unwrap();
        let loaded = load_resource_nodes(&conn, w.id).unwrap();

        assert_eq!(loaded.len(), generated.nodes.len());
        for (a, b) in loaded.iter().zip(generated.nodes.iter()) {
            assert_eq!(a.kind, b.kind);
            assert_eq!((a.x, a.y), (b.x, b.y));
            assert!((a.quantity - b.quantity).abs() < 1e-4);
        }
    }

    #[test]
    fn saving_twice_replaces_rather_than_duplicates() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 1, &cfg).unwrap();
        let generated = crate::sim::worldgen::generate(1, &cfg).world;

        save_world(&mut conn, w.id, &generated).unwrap();
        save_world(&mut conn, w.id, &generated).unwrap();

        let chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE world_id = ?1", [w.id], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks as u32, generated.chunks_x() * generated.chunks_y());
    }

    #[test]
    fn terrain_reads_as_none_before_anything_is_saved() {
        let conn = db();
        let w = create_world(&conn, "empty", 1, &WorldConfig::default()).unwrap();
        assert!(!world_has_terrain(&conn, w.id).unwrap());
        assert!(load_terrain(&conn, w.id, 512, 512, 32).unwrap().is_none());
    }

    #[test]
    fn current_tick_advances() {
        let conn = db();
        let w = create_world(&conn, "Ashfen", 1, &WorldConfig::default()).unwrap();
        set_current_tick(&conn, w.id, 4118).unwrap();
        assert_eq!(load_world(&conn, w.id).unwrap().unwrap().current_tick, 4118);
    }
}
