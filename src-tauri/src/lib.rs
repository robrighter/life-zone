//! Life Zone — a locally-run simulation of a community of creatures.
//!
//! M2: creatures, needs, life stages, lifespan and death; Tier 0/1 decisions;
//! actions and pathfinding; spoilage and the fuel economy; the belief
//! substrate. No LLM — that lands at M3 beside Tier 1, never in place of it.

pub mod ai;
pub mod config;
pub mod db;
pub mod logging;
pub mod sim;

use anyhow::Result;
use config::WorldConfig;
use db::repo::{self, WorldRow};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sim::runner::{
    self, NodesSnapshot, SimCommand, SimHandle, Snapshot, SnapshotSink, SpeedMode,
};
use sim::tick::Sim;
use sim::world::Founder;
use std::sync::{Arc, Mutex};
use tauri::{Emitter, Manager};

/// Application state.
///
/// The simulation is **not** in here. It lives on its own thread which owns the
/// world, the creatures and the writing connection outright; this struct holds
/// only a command channel out to it and the snapshot slots it publishes into
/// (PRD §3.1, BUILD.md §5.1). M0 and M1 each carried a `Mutex<Option<World>>`
/// with a comment saying M2 would remove it — this is that removal.
///
/// `reader` is a second SQLite connection, read-only in practice. WAL mode
/// allows readers concurrent with the sim thread's writer, so a report query
/// never blocks a tick.
pub struct AppState {
    pub world_id: i64,
    pub sim: SimHandle,
    pub reader: Mutex<Connection>,
}

/// Pushes each snapshot to the webview as a Tauri event (`tick:complete`).
struct EventSink {
    app: tauri::AppHandle,
}

impl SnapshotSink for EventSink {
    fn publish(&self, snapshot: Arc<Snapshot>) {
        // Emission is already throttled by wall clock on the sim thread, so
        // Fast-Forward does not drown the webview in frames it cannot draw.
        if let Err(e) = self.app.emit("tick:complete", snapshot.as_ref()) {
            tracing::debug!(error = %e, "snapshot emit failed (window closing?)");
        }
    }
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
    let conn = state.reader.lock().map_err(|e| e.to_string())?;
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
    let conn = state.reader.lock().map_err(|e| e.to_string())?;
    repo::list_worlds(&conn).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct WorldMeta {
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
    pub seed: i64,
    pub founders: Vec<Founder>,
}

#[tauri::command]
fn get_world_meta(state: tauri::State<'_, AppState>) -> WorldMeta {
    let t = state.sim.shared.terrain.lock().expect("terrain slot").clone();
    WorldMeta {
        width: t.width,
        height: t.height,
        chunk_size: t.chunk_size,
        seed: t.seed,
        founders: t.founders.clone(),
    }
}

/// Terrain as raw bytes, one per tile, row-major. At 512x512 that is 256KB;
/// serialising it as a JSON array of numbers instead would be several megabytes
/// of text for the webview to parse on every load.
#[tauri::command]
fn get_terrain(state: tauri::State<'_, AppState>) -> tauri::ipc::Response {
    let t = state.sim.shared.terrain.lock().expect("terrain slot").clone();
    tauri::ipc::Response::new(t.bytes.clone())
}

/// Resource nodes. Republished by the sim thread roughly once a second, because
/// crops are planted and patches are stripped as a run goes on.
#[tauri::command]
fn get_nodes(state: tauri::State<'_, AppState>) -> Arc<NodesSnapshot> {
    state.sim.shared.nodes.lock().expect("nodes slot").clone()
}

/// The latest snapshot, for a UI that has just loaded and has not yet received
/// a pushed one.
#[tauri::command]
fn get_snapshot(state: tauri::State<'_, AppState>) -> Arc<Snapshot> {
    state.sim.shared.latest()
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UiCommand {
    Play,
    Pause,
    Step { ticks: u32 },
    SetMode { mode: SpeedMode },
    Select { id: Option<i64> },
    Regenerate { seed: i64, creatures: u32 },
}

/// The only way the UI changes anything. Commands go down a channel; nothing
/// here touches simulation state.
#[tauri::command]
fn sim_control(state: tauri::State<'_, AppState>, command: UiCommand) {
    let cmd = match command {
        UiCommand::Play => SimCommand::Play,
        UiCommand::Pause => SimCommand::Pause,
        UiCommand::Step { ticks } => SimCommand::Step(ticks),
        UiCommand::SetMode { mode } => SimCommand::SetMode(mode),
        UiCommand::Select { id } => SimCommand::Select(id),
        UiCommand::Regenerate { seed, creatures } => SimCommand::Regenerate { seed, creatures },
    };
    state.sim.send(cmd);
}

/// Cause-of-death tallies over the whole run, read from the database rather
/// than from memory — this is the M2 exit criterion, and it should be checkable
/// from the artefact that survives the process.
#[tauri::command]
fn get_deaths_by_cause(state: tauri::State<'_, AppState>) -> Result<Vec<(String, i64)>, String> {
    let conn = state.reader.lock().map_err(|e| e.to_string())?;
    repo::deaths_by_cause(&conn, state.world_id).map_err(|e| e.to_string())
}

/// Whether to run the benchmark automatically on load. Off unless
/// LIFE_ZONE_BENCH is set, so it never intrudes on normal use.
#[tauri::command]
fn bench_mode() -> bool {
    std::env::var("LIFE_ZONE_BENCH").is_ok()
}

/// Record a benchmark from the frontend. The renderer is the only place that
/// can measure real frame intervals, but the log is where a measured result
/// belongs.
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

    let mut config = WorldConfig::default();
    // M2 has no reproduction — that is M4 — so a world seeded with founders
    // alone dwindles to nothing within a few hundred ticks and there is
    // nothing to watch. A new world therefore starts as a settlement with its
    // census held by the measurement fixture, which the UI labels plainly so a
    // held run is never mistaken for a self-sustaining one. Both knobs are in
    // `worlds.config_json` and can be turned off.
    config.bench.initial_creatures = Some(300);
    config.bench.maintain_population = Some(300);

    let world = repo::create_world(conn, "Ashfen", 44127, &config)?;
    Ok(world)
}

/// Reload the persisted grid, or generate and save it on first run.
fn load_or_generate_world(
    conn: &mut Connection,
    world_row: &WorldRow,
    config: &WorldConfig,
) -> Result<sim::world::World> {
    let (w, h, cs) = (config.map.width, config.map.height, config.map.chunk_size);

    if let Some(tiles) = repo::load_terrain(conn, world_row.id, w, h, cs)? {
        let nodes = repo::load_resource_nodes(conn, world_row.id)?;
        tracing::info!(world_id = world_row.id, tiles = tiles.len(), nodes = nodes.len(),
                       "loaded persisted world");

        // Founder *placements* are a worldgen output rather than stored state —
        // from M2 the founders themselves live in `creatures`. But a world can
        // legitimately exist with its terrain saved and no creatures yet: the
        // world row and its chunks are written before the simulation is built,
        // so a process that dies in between leaves exactly that. Reloading with
        // an empty founder list then produced a world that could never have
        // anybody in it.
        //
        // Worldgen is deterministic (invariant 7), so re-deriving the
        // placements from the same seed costs ~25ms at startup and gives back
        // precisely the ones that were generated the first time.
        let founders = sim::worldgen::generate(world_row.seed as u64, config).world.founders;

        return Ok(sim::world::World {
            width: w, height: h, chunk_size: cs,
            seed: world_row.seed as u64,
            tiles, nodes, founders,
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

/// Build the simulation: either resumed from the database, or freshly populated.
fn build_sim(conn: &Connection, world_row: &WorldRow, config: WorldConfig,
             world: sim::world::World) -> Result<Sim> {
    let mut sim = Sim::new(world_row.id, world, config.clone(), world_row.seed as u64);

    // A world with creatures in it is one to resume, whatever its tick. Testing
    // the tick alone treats a saved world paused at tick 0 as a fresh one.
    let has_creatures: i64 = conn.query_row(
        "SELECT COUNT(*) FROM creatures WHERE world_id = ?1 AND death_tick IS NULL",
        [world_row.id],
        |r| r.get(0),
    )?;
    let resuming = has_creatures > 0;
    if resuming {
        sim.load_from(conn, world_row.current_tick)?;
        tracing::info!(tick = sim.tick, alive = sim.alive(), "resumed simulation state");
        return Ok(sim);
    }

    match config.bench.initial_creatures {
        Some(n) => {
            sim.spawn_population(n);
            tracing::info!(n, "seeded a settlement (M2 measurement fixture)");
        }
        None => {
            sim.spawn_founders();
            tracing::info!(n = sim.alive(), "spawned worldgen founders");
        }
    }
    if sim.alive() == 0 {
        tracing::error!("world has no creatures and none could be placed");
    }
    Ok(sim)
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
            let sim = build_sim(&conn, &world_row, config, world)?;

            // A second connection for reads. WAL lets it run concurrently with
            // the sim thread's writer, so a report query cannot stall a tick.
            let reader = db::open(&db_path)?;

            let sink = Box::new(EventSink { app: app.handle().clone() });
            let (handle, _join) = runner::spawn(sim, conn, sink);

            app.manage(AppState {
                world_id: world_row.id,
                sim: handle,
                reader: Mutex::new(reader),
            });

            tracing::info!(world_id = world_row.id, "startup complete");
            Ok(())
        })
        .on_window_event(|window, event| {
            // Give the sim thread a chance to flush before the process goes.
            if let tauri::WindowEvent::CloseRequested { .. } = event {
                if let Some(state) = window.try_state::<AppState>() {
                    state.sim.send(SimCommand::Shutdown);
                    std::thread::sleep(std::time::Duration::from_millis(220));
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_world, list_worlds, get_world_meta, get_terrain, get_nodes,
            get_snapshot, sim_control, get_deaths_by_cause, bench_mode, report_bench
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

    #[test]
    fn a_new_world_starts_as_a_settlement_and_says_so() {
        // M2 has no reproduction, so founders alone would leave nothing to
        // watch. The fixture that fills the gap must be recorded in the world's
        // own config rather than hidden in the binary.
        let conn = db();
        let w = bootstrap_world(&conn).unwrap();
        let cfg = repo::load_world_config(&conn, w.id).unwrap().unwrap();

        assert_eq!(cfg.bench.initial_creatures, Some(300));
        assert_eq!(cfg.bench.maintain_population, Some(300));
    }
}
