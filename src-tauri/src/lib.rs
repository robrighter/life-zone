//! Life Zone — a locally-run simulation of a community of creatures.
//!
//! M0 scaffold: the app opens, the database migrates, a world row exists, and
//! an empty render loop runs. The simulation itself lands at M1/M2.

pub mod config;
pub mod db;
pub mod logging;

use anyhow::Result;
use config::WorldConfig;
use db::repo::{self, World};
use rusqlite::Connection;
use serde::Serialize;
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
fn list_worlds(state: tauri::State<'_, AppState>) -> Result<Vec<World>, String> {
    let conn = state.conn.lock().map_err(|e| e.to_string())?;
    repo::list_worlds(&conn).map_err(|e| e.to_string())
}

/// Reuse the newest active world, or create one on first run. Worldgen itself
/// is M1; this only establishes the row and its config.
fn bootstrap_world(conn: &Connection) -> Result<World> {
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

            let conn = db::open(&db_path)?;
            let world = bootstrap_world(&conn)?;

            app.manage(AppState { conn: Mutex::new(conn), world_id: world.id });

            tracing::info!(world_id = world.id, "startup complete");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_world, list_worlds])
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
