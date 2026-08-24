//! Life Zone — a locally-run simulation of a community of creatures.
//!
//! M1: a seeded world generates, persists in chunks, and renders. The tick
//! pipeline and creatures land at M2.

pub mod config;
pub mod db;
pub mod logging;
pub mod sim;

use anyhow::Result;
use config::WorldConfig;
use db::repo::{self, WorldRow};
use rusqlite::Connection;
use serde::Serialize;
use sim::world::{Founder, ResourceNode, World};
use std::sync::Mutex;
use tauri::Manager;

/// Application state.
///
/// The connection sits behind a Mutex for M0 only. From M2 the simulation
/// thread owns all world state exclusively and the UI gets snapshots pushed to
/// it (PRD §3.1, BUILD.md §5.1) — the UI must never be able to stall the tick
/// loop, so this Mutex does not survive into the tick pipeline.
pub struct AppState {
    pub conn: Mutex<Connection>,
    pub world_id: i64,
    /// The generated world. Behind a Mutex for M1, where only the UI reads it.
    /// At M2 the sim thread takes exclusive ownership and this becomes a
    /// snapshot channel instead.
    pub world: Mutex<Option<World>>,
}

#[derive(Serialize)]
pub struct WorldSummary {
    pub id: i64,
    pub name: String,
    pub seed: i64,
    pub current_tick: i64,
    pub status: String,
    pub created_at: String,
    pub config: WorldConfig,
}

#[tauri::command]
fn get_world(state: tauri::State<'_, AppState>) -> Result<WorldSummary, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;

    let world = repo::load_world(&conn, state.world_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("world {} not found", state.world_id))?;
    let config = repo::load_world_config(&conn, state.world_id)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    Ok(WorldSummary {
        id: world.id,
        name: world.name,
        seed: world.seed,
        current_tick: world.current_tick,
        status: world.status,
        created_at: world.created_at,
        config,
    })
}

#[tauri::command]
fn list_worlds(state: tauri::State<'_, AppState>) -> Result<Vec<WorldRow>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    repo::list_worlds(&conn).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct WorldMeta {
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
    pub seed: i64,
    pub nodes: Vec<ResourceNode>,
    pub founders: Vec<Founder>,
}

/// Terrain as raw bytes, one per tile, row-major. At 512x512 that is 256KB;
/// serialising it as a JSON array of numbers instead would be several megabytes
/// of text for the webview to parse on every load.
#[tauri::command]
fn get_terrain(state: tauri::State<'_, AppState>) -> Result<tauri::ipc::Response, String> {
    let world = state.world.lock().map_err(|e| e.to_string())?;
    let w = world.as_ref().ok_or("no world generated")?;
    Ok(tauri::ipc::Response::new(w.terrain_bytes()))
}

#[tauri::command]
fn get_world_meta(state: tauri::State<'_, AppState>) -> Result<WorldMeta, String> {
    let world = state.world.lock().map_err(|e| e.to_string())?;
    let w = world.as_ref().ok_or("no world generated")?;
    Ok(WorldMeta {
        width: w.width, height: w.height, chunk_size: w.chunk_size,
        seed: w.seed as i64,
        nodes: w.nodes.clone(),
        founders: w.founders.clone(),
    })
}

/// Generate a fresh world from a seed, replacing the current one.
#[tauri::command]
fn regenerate_world(
    state: tauri::State<'_, AppState>,
    seed: i64,
) -> Result<WorldMeta, String> {
    let mut conn = state.conn.lock().map_err(|e| e.to_string())?;
    let config = repo::load_world_config(&conn, state.world_id)
        .map_err(|e| e.to_string())?
        .unwrap_or_default();

    let t0 = std::time::Instant::now();
    let out = sim::worldgen::generate(seed as u64, &config);
    let gen_ms = t0.elapsed().as_millis();

    repo::save_world(&mut conn, state.world_id, &out.world).map_err(|e| e.to_string())?;
    conn.execute("UPDATE worlds SET seed = ?2 WHERE id = ?1",
                 rusqlite::params![state.world_id, seed])
        .map_err(|e| e.to_string())?;

    let meta = WorldMeta {
        width: out.world.width, height: out.world.height,
        chunk_size: out.world.chunk_size, seed,
        nodes: out.world.nodes.clone(), founders: out.world.founders.clone(),
    };
    tracing::info!(seed, gen_ms, rejected = out.rejected, "regenerated world");

    *state.world.lock().map_err(|e| e.to_string())? = Some(out.world);
    Ok(meta)
}

/// Whether to run the render benchmark automatically on load. Off unless
/// LIFE_ZONE_BENCH is set, so it never intrudes on normal use.
#[tauri::command]
fn bench_mode() -> bool {
    std::env::var("LIFE_ZONE_BENCH").is_ok()
}

/// Record a render benchmark from the frontend. The renderer is the only place
/// that can measure real frame intervals, but the log is where a measured
/// result belongs.
#[tauri::command]
fn report_bench(result: serde_json::Value) {
    tracing::info!(target: "bench", result = %result, "render benchmark");
}

/// Reuse the newest active world, or create one on first run.
fn bootstrap_world(conn: &Connection) -> Result<WorldRow> {
    if let Some(existing) = repo::latest_active_world(conn)? {
        tracing::info!(world_id = existing.id, name = %existing.name,
                       tick = existing.current_tick, "resuming world");
        return Ok(existing);
    }

    // A fixed default seed keeps first-run behaviour reproducible; the UI will
    // offer new-world-from-seed at M1.
    let world = repo::create_world(conn, "Ashfen", 44127, &WorldConfig::default())?;
    Ok(world)
}

/// Reload the persisted grid, or generate and save it on first run.
///
/// Founders are deliberately not reloaded: they are a worldgen output that M2
/// turns into rows in `creatures`, which is where they will persist. At M1
/// there are no creatures yet, so an empty list after a restart is correct.
fn load_or_generate_world(
    conn: &mut Connection,
    world_row: &WorldRow,
    config: &WorldConfig,
) -> Result<World> {
    let (w, h, cs) = (config.map.width, config.map.height, config.map.chunk_size);

    if let Some(tiles) = repo::load_terrain(conn, world_row.id, w, h, cs)? {
        let nodes = repo::load_resource_nodes(conn, world_row.id)?;
        tracing::info!(world_id = world_row.id, tiles = tiles.len(), nodes = nodes.len(),
                       "loaded persisted world");
        return Ok(World {
            width: w, height: h, chunk_size: cs,
            seed: world_row.seed as u64,
            tiles, nodes, founders: Vec::new(),
        });
    }

    let t0 = std::time::Instant::now();
    let out = sim::worldgen::generate(world_row.seed as u64, config);
    let gen_ms = t0.elapsed().as_millis();

    repo::save_world(conn, world_row.id, &out.world)?;
    tracing::info!(world_id = world_row.id, gen_ms, rejected = out.rejected,
                   "generated and persisted world");
    Ok(out.world)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let data_dir = app.path().app_data_dir()?;
            let guard = logging::init(&data_dir.join("logs"))?;
            // Held for the process lifetime so buffered log lines are not lost.
            app.manage(guard);

            let db_path = data_dir.join("life-zone.sqlite3");
            tracing::info!(path = %db_path.display(), "opening database");

            let mut conn = db::open(&db_path)?;
            let world_row = bootstrap_world(&conn)?;
            let config = repo::load_world_config(&conn, world_row.id)?.unwrap_or_default();
            let world = load_or_generate_world(&mut conn, &world_row, &config)?;

            app.manage(AppState {
                conn: Mutex::new(conn),
                world_id: world_row.id,
                world: Mutex::new(Some(world)),
            });

            tracing::info!(world_id = world_row.id, "startup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_world, list_worlds, get_world_meta, get_terrain, regenerate_world,
            bench_mode, report_bench
        ])
        .run(tauri::generate_context!())
        .expect("error while running Life Zone");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("db/migrations/001_initial.sql")).unwrap();
        conn
    }

    #[test]
    fn first_run_creates_exactly_one_world() {
        let conn = db();
        let w = bootstrap_world(&conn).unwrap();

        assert_eq!(w.name, "Ashfen");
        assert_eq!(w.current_tick, 0);
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM worlds", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn a_second_run_resumes_rather_than_creating_a_duplicate() {
        let conn = db();
        let first = bootstrap_world(&conn).unwrap();
        repo::set_current_tick(&conn, first.id, 4118).unwrap();

        let second = bootstrap_world(&conn).unwrap();

        assert_eq!(second.id, first.id, "should resume the existing world");
        assert_eq!(second.current_tick, 4118, "resumed at the stored tick");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM worlds", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 1, "must not create a second world");
    }

    #[test]
    fn an_archived_world_is_not_resumed() {
        let conn = db();
        let first = bootstrap_world(&conn).unwrap();
        conn.execute("UPDATE worlds SET status = 'archived' WHERE id = ?1", [first.id]).unwrap();

        let second = bootstrap_world(&conn).unwrap();

        assert_ne!(second.id, first.id, "archived worlds are not resumed");
        let n: i64 = conn.query_row("SELECT COUNT(*) FROM worlds", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn the_bootstrapped_world_stores_a_loadable_config() {
        let conn = db();
        let w = bootstrap_world(&conn).unwrap();
        let cfg = repo::load_world_config(&conn, w.id).unwrap().unwrap();

        assert_eq!(cfg.map.width, 512);
        assert_eq!(cfg.lifespan.baseline_ticks, 672);
        // Tier 1 must remain the control for S6, so the LLM toggle ships on but
        // the deterministic policy is never conditional on it.
        assert!(cfg.features.llm);
    }
}
