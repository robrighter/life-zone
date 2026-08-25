//! Life Zone — a locally-run simulation of a community of creatures.
//!
//! M2: creatures, needs, life stages, lifespan and death; Tier 0/1 decisions;
//! actions and pathfinding; spoilage and the fuel economy; the belief
//! substrate. No LLM — that lands at M3 beside Tier 1, never in place of it.

pub mod ai;
pub mod config;
pub mod db;
pub mod logging;
pub mod report;
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
    /// Where the database lives; CSV exports go beside it.
    pub data_dir: std::path::PathBuf,
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

// ------------------------------------------------------------- reporting
//
// All of these run on the reader connection while the simulation writes on its
// own. WAL makes that safe and it means opening a report can never stall a
// tick — which matters more than it sounds, because the reporting view is
// exactly what you open when something looks wrong and the run is still going.

macro_rules! report_cmd {
    ($name:ident, $query:path $(, $arg:ident : $ty:ty)*) => {
        #[tauri::command]
        fn $name(
            state: tauri::State<'_, AppState>
            $(, $arg: $ty)*
        ) -> Result<serde_json::Value, String> {
            let conn = state.reader.lock().map_err(|e| e.to_string())?;
            let out = $query(&conn, state.world_id $(, $arg)*).map_err(|e| e.to_string())?;
            serde_json::to_value(out).map_err(|e| e.to_string())
        }
    };
}

report_cmd!(report_headline, report::queries::headline);
report_cmd!(report_population, report::queries::population_series, buckets: i64);
report_cmd!(report_causes, report::queries::cause_of_death_by_generation);
report_cmd!(report_age_at_death, report::queries::age_at_death, bucket: i64);
report_cmd!(report_lineages, report::queries::deepest_lineages, limit: i64);
report_cmd!(report_lineage_tree, report::queries::lineage_tree, founder: i64);
report_cmd!(report_generations, report::queries::by_generation);
report_cmd!(report_economy, report::queries::economy_series, buckets: i64);
report_cmd!(report_farming, report::queries::farming_adoption);
report_cmd!(report_actions, report::queries::action_distribution_by_tier);
report_cmd!(report_deliberation, report::queries::deliberation_series, buckets: i64);
report_cmd!(report_horizons, report::queries::horizon_gap);
report_cmd!(report_aborts, report::queries::abort_reasons);
report_cmd!(report_fallbacks, report::queries::fallback_reasons);
report_cmd!(report_transmission, report::queries::transmission_by_channel);
report_cmd!(report_beliefs, report::queries::belief_provenance);
report_cmd!(report_roster, report::queries::roster, limit: i64);

// The knowledge, planning and selection reports — the ones the success
// criteria are graded on rather than the ones that describe the run.
report_cmd!(report_coverage, report::culture::map_coverage);
report_cmd!(report_half_life, report::culture::knowledge_half_life);
report_cmd!(report_accuracy, report::culture::belief_accuracy);
report_cmd!(report_teaching, report::culture::teaching_vs_depth);
report_cmd!(report_graph, report::culture::transmission_graph, limit: i64);
report_cmd!(report_s6, report::culture::deliberation_vs_depth);
report_cmd!(report_planners, report::culture::horizon_vs_depth);
report_cmd!(report_survival, report::culture::lineage_survival);
report_cmd!(report_stage_compute, report::culture::compute_by_life_stage);
report_cmd!(report_elders, report::culture::elder_autonomy);
report_cmd!(report_pressure, report::culture::pressure_distribution);
report_cmd!(report_latency, report::culture::latency);
report_cmd!(report_horizon_gen, report::culture::horizon_by_generation);
report_cmd!(report_horizon_goal, report::culture::horizon_by_goal);
report_cmd!(report_roles, report::culture::roles);
report_cmd!(report_action_gen, report::culture::actions_by_generation);
report_cmd!(report_wealth, report::culture::household_wealth);
report_cmd!(report_wood, report::culture::wood_budget, buckets: i64);
report_cmd!(report_life, report::queries::life, id: i64);

/// Write every report to CSV and return where they went.
///
/// §10: "All reports exportable to CSV." Files rather than a download, because
/// this is an offline desktop app and the interesting thing to do with these is
/// open them in something else.
#[tauri::command]
fn export_reports_csv(state: tauri::State<'_, AppState>) -> Result<String, String> {
    let conn = state.reader.lock().map_err(|e| e.to_string())?;
    let w = state.world_id;
    let dir = state.data_dir.join(format!("reports-world-{w}"));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;

    let mut wrote = 0usize;
    macro_rules! dump {
        ($file:literal, $value:expr) => {
            let v = serde_json::to_value($value.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?;
            write_csv(&dir.join(concat!($file, ".csv")), &v).map_err(|e| e.to_string())?;
            wrote += 1;
        };
    }
    dump!("headline", report::queries::headline(&conn, w));
    dump!("population", report::queries::population_series(&conn, w, 400));
    dump!("cause_of_death_by_generation", report::queries::cause_of_death_by_generation(&conn, w));
    dump!("age_at_death", report::queries::age_at_death(&conn, w, 48));
    dump!("lineages", report::queries::deepest_lineages(&conn, w, 500));
    dump!("generations", report::queries::by_generation(&conn, w));
    dump!("economy", report::queries::economy_series(&conn, w, 400));
    dump!("farming_adoption", report::queries::farming_adoption(&conn, w));
    dump!("action_distribution_by_tier", report::queries::action_distribution_by_tier(&conn, w));
    dump!("deliberation", report::queries::deliberation_series(&conn, w, 400));
    dump!("horizon_gap", report::queries::horizon_gap(&conn, w));
    dump!("abort_reasons", report::queries::abort_reasons(&conn, w));
    dump!("fallback_reasons", report::queries::fallback_reasons(&conn, w));
    dump!("transmission", report::queries::transmission_by_channel(&conn, w));
    dump!("belief_provenance", report::queries::belief_provenance(&conn, w));
    dump!("roster", report::queries::roster(&conn, w, 100_000));
    dump!("map_coverage", report::culture::map_coverage(&conn, w));
    dump!("knowledge_half_life", report::culture::knowledge_half_life(&conn, w));
    dump!("belief_accuracy", report::culture::belief_accuracy(&conn, w));
    dump!("teaching_vs_depth", report::culture::teaching_vs_depth(&conn, w));
    dump!("transmission_graph", report::culture::transmission_graph(&conn, w, 5_000));
    dump!("deliberation_vs_depth", report::culture::deliberation_vs_depth(&conn, w));
    dump!("horizon_vs_depth", report::culture::horizon_vs_depth(&conn, w));
    dump!("lineage_survival", report::culture::lineage_survival(&conn, w));
    dump!("compute_by_life_stage", report::culture::compute_by_life_stage(&conn, w));
    dump!("elder_autonomy", report::culture::elder_autonomy(&conn, w));
    dump!("pressure_distribution", report::culture::pressure_distribution(&conn, w));
    dump!("latency", report::culture::latency(&conn, w));
    dump!("horizon_by_generation", report::culture::horizon_by_generation(&conn, w));
    dump!("horizon_by_goal", report::culture::horizon_by_goal(&conn, w));
    dump!("roles", report::culture::roles(&conn, w));
    dump!("actions_by_generation", report::culture::actions_by_generation(&conn, w));
    dump!("household_wealth", report::culture::household_wealth(&conn, w));
    dump!("wood_budget", report::culture::wood_budget(&conn, w, 400));

    tracing::info!(path = %dir.display(), files = wrote, "exported reports");
    Ok(dir.display().to_string())
}

/// Serialise a report to CSV.
///
/// Generic over the report types by going through `serde_json::Value`: the
/// alternative is sixteen bespoke writers that all drift apart, and the column
/// order then comes from the struct definition, which is the one place it is
/// already documented.
fn write_csv(path: &std::path::Path, value: &serde_json::Value) -> anyhow::Result<()> {
    use std::io::Write;
    let rows: Vec<&serde_json::Value> = match value {
        serde_json::Value::Array(a) => a.iter().collect(),
        other => vec![other],
    };
    let mut out = std::fs::File::create(path)?;

    let Some(first) = rows.first().and_then(|r| r.as_object()) else {
        // An empty report is still a file: a missing one reads as a failure.
        writeln!(out, "(no rows)")?;
        return Ok(());
    };
    let headers: Vec<&String> = first.keys().collect();
    writeln!(out, "{}", headers.iter().map(|h| h.as_str()).collect::<Vec<_>>().join(","))?;

    for row in &rows {
        let Some(obj) = row.as_object() else { continue };
        let cells: Vec<String> = headers
            .iter()
            .map(|h| csv_cell(obj.get(*h).unwrap_or(&serde_json::Value::Null)))
            .collect();
        writeln!(out, "{}", cells.join(","))?;
    }
    Ok(())
}

fn csv_cell(v: &serde_json::Value) -> String {
    let raw = match v {
        serde_json::Value::Null => String::new(),
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    if raw.contains([',', '"', '\n']) {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw
    }
}

/// Reuse the newest active world, or create one on first run.
fn bootstrap_world(conn: &Connection) -> Result<WorldRow> {
    if let Some(existing) = repo::latest_active_world(conn)? {
        tracing::info!(world_id = existing.id, name = %existing.name,
                       tick = existing.current_tick, "resuming world");
        return Ok(existing);
    }

    let mut config = WorldConfig::default();
    // A settlement rather than eight founders, because eight people on a
    // 512-tile map is not much to look at — but the census is *not* held any
    // more. It was, back when there was no reproduction and a world dwindled
    // to nothing within a few hundred ticks; holding it now would hide the
    // thing the game is about, since every settler arrives as its own
    // generation-1 founder and a held run shows a busy map with no lineage in
    // it. Since M6 the population sustains itself: measured across three seeds
    // at 6,000 ticks, generation 17 to 28 with no fixture at all.
    config.bench.initial_creatures = Some(300);

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
                data_dir,
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
            get_snapshot, sim_control, get_deaths_by_cause, bench_mode, report_bench,
            report_headline, report_population, report_causes, report_age_at_death,
            report_lineages, report_lineage_tree, report_generations, report_economy,
            report_farming, report_actions, report_deliberation, report_horizons,
            report_aborts, report_fallbacks, report_transmission, report_beliefs,
            report_roster, report_life, export_reports_csv,
            report_coverage, report_half_life, report_accuracy, report_teaching,
            report_graph, report_s6, report_planners, report_survival,
            report_stage_compute, report_elders, report_pressure, report_latency,
            report_horizon_gen, report_horizon_goal, report_roles, report_action_gen, report_wealth,
            report_wood
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
    fn a_new_world_starts_as_a_settlement_and_holds_nothing_up() {
        // A settlement rather than eight founders, because eight people on a
        // 512-tile map is nothing to look at — and whatever the world is
        // seeded with has to be recorded in its own config rather than hidden
        // in the binary.
        //
        // The census fixture is gone. It existed when there was no
        // reproduction; keeping it now would hide the thing the game is about,
        // because every settler arrives as its own generation-1 founder and a
        // held run shows a busy map with no lineage in it.
        let conn = db();
        let w = bootstrap_world(&conn).unwrap();
        let cfg = repo::load_world_config(&conn, w.id).unwrap().unwrap();

        assert_eq!(cfg.bench.initial_creatures, Some(300));
        assert_eq!(
            cfg.bench.maintain_population, None,
            "a world that tops itself up cannot show a lineage"
        );
    }
}
