//! The simulation thread (PRD §3.1, BUILD.md §5.1).
//!
//! ```text
//! ┌─ main thread ──────────┐   ┌─ sim thread ─────────────┐
//! │ Tauri, IPC, webview    │◄──┤ owns ALL world state     │
//! │ never touches sim state│   │ runs the tick pipeline   │
//! └────────────────────────┘   └──────────────────────────┘
//!                                        │ one tx per tick
//!                                    ┌────────┐
//!                                    │ SQLite │
//!                                    └────────┘
//! ```
//!
//! M0 and M1 kept the world in an `AppState` mutex that the UI locked directly,
//! with a comment on it saying M2 would replace that. This is the replacement,
//! and it is a rewrite rather than a tick loop bolted onto the mutex: the sim
//! thread owns the `Sim` and the `Connection` outright, the UI sends commands
//! down a channel, and state comes back as snapshots.
//!
//! **On the one lock that remains.** Snapshots are pushed to the webview as
//! Tauri events, which is the primary path, but a UI that starts late or
//! reloads needs to be able to *pull* the current state too. That slot is a
//! `Mutex<Arc<Snapshot>>`. It does not reintroduce shared world state: the sim
//! thread's critical section is a single pointer store, it holds the lock for
//! no I/O and no work, and a reader clones the `Arc` and leaves. The UI
//! therefore still cannot stall the tick loop, which is the property the
//! architecture exists to guarantee.

use crate::config::WorldConfig;
use crate::db::repo;
use crate::sim::creature::{Creature, DeathCause, ItemKind, LifeStage};
use crate::sim::economy::{self, StructureKind};
use crate::sim::knowledge::{self, NeedProfile};
use crate::sim::tick::{PhaseTimings, Sim, TickReport};
use crate::sim::world::{Founder, ResourceNode};
use serde::{Deserialize, Serialize};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SpeedMode {
    /// Unbounded deliberation. Meaningless until M3, kept so the control exists.
    Deep,
    /// The default watching experience.
    Observe,
    /// No LLM, no rendering pressure — skip ahead days.
    FastForward,
    /// The whole budget spent on one lineage. M3.
    Focus,
}

impl SpeedMode {
    /// How long a tick should take in wall clock, at M2.
    ///
    /// §5.6's targets are about how long *deliberation* takes, and at M2 there
    /// is none — every mode would otherwise run at Fast-Forward speed and
    /// Observe would be unwatchable. So Observe and Deep are paced here for
    /// human eyes, and take their real timings when the model lands at M3.
    fn tick_interval(self, deliberating: bool) -> Option<Duration> {
        match self {
            SpeedMode::Deep => Some(Duration::from_millis(if deliberating { 2_000 } else { 500 })),
            // §5.6 puts Observe at 1–2s per tick, and that number exists
            // because of the model. M2 paced it at 140ms — correct when there
            // was nothing to wait for, and wrong the moment there is: at seven
            // ticks a second a deliberation comes back 200 ticks after it was
            // asked, which is a third of a creature's life. A second a tick
            // brings the round trip down to single figures and is what the PRD
            // specified in the first place.
            SpeedMode::Observe => {
                Some(Duration::from_millis(if deliberating { 1_000 } else { 140 }))
            }
            SpeedMode::Focus => {
                Some(Duration::from_millis(if deliberating { 700 } else { 220 }))
            }
            // No LLM at all here, by definition (§5.6): Tier 1 and nothing else.
            SpeedMode::FastForward => None,
        }
    }
}

#[derive(Debug)]
pub enum SimCommand {
    Play,
    Pause,
    /// Advance exactly n ticks, then pause.
    Step(u32),
    SetMode(SpeedMode),
    Select(Option<i64>),
    Regenerate { seed: i64, creatures: u32 },
    /// Flush everything to SQLite and stop.
    Shutdown,
}

// ------------------------------------------------------------------ snapshots

/// One creature, as the map needs it. Deliberately tiny: this is serialised for
/// up to 500 creatures several times a second.
#[derive(Debug, Clone, Serialize)]
pub struct CreatureDot {
    pub id: i64,
    pub x: u32,
    pub y: u32,
    /// 0 infant, 1 adult, 2 elder.
    pub stage: u8,
    /// Bit flags: 1 hungry, 2 thirsty, 4 cold, 8 sheltered, 16 at a fire,
    /// 32 thinking (a call is in flight), 64 running a plan the model wrote.
    pub flags: u8,
}

#[derive(Debug, Clone, Serialize)]
pub struct StructureDot {
    pub id: i64,
    pub x: u32,
    pub y: u32,
    pub kind: &'static str,
    pub lit: bool,
    pub condition: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct TickerLine {
    pub tick: i64,
    pub kind: &'static str,
    pub text: String,
    /// "death", "birth", or "" — the ticker's emphasis classes.
    pub tone: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct BeliefView {
    pub kind: &'static str,
    pub x: u32,
    pub y: u32,
    pub estimate: &'static str,
    pub confidence: f32,
    pub hops: u8,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlanStepView {
    pub goal: &'static str,
    pub label: String,
    pub done: bool,
    pub current: bool,
    pub est_ticks: u32,
}

/// Everything the inspector shows about one creature (§9.2).
#[derive(Debug, Clone, Serialize)]
pub struct CreatureDetail {
    pub id: i64,
    pub name: String,
    pub sex: &'static str,
    pub generation: i32,
    pub age: i64,
    pub expected_lifespan: u32,
    pub life_stage: &'static str,
    pub x: u32,
    pub y: u32,
    pub felt_state: &'static str,
    pub hunger: f32,
    pub thirst: f32,
    pub fatigue: f32,
    pub warmth: f32,
    pub health: f32,
    pub traits: crate::sim::creature::Traits,
    pub carrying: Vec<(String, f32, Option<i64>)>,
    pub plan_rationale: String,
    pub plan_addresses: String,
    pub plan_horizon: u32,
    pub plan_remaining: u32,
    pub plan_tier: u8,
    pub steps: Vec<PlanStepView>,
    pub beliefs: Vec<BeliefView>,
    pub belief_count: usize,
    pub lifetime_deliberations: i64,
    pub sheltered: bool,

    // ---- society ---------------------------------------------------------
    pub household_id: Option<i64>,
    pub household_store: f32,
    pub household_grain: f32,
    pub household_members: u32,
    pub mate: Option<(i64, String)>,
    pub mother: Option<(i64, String)>,
    pub father: Option<(i64, String)>,
    pub children_born: i32,
    pub taught_count: i32,
    pub shared_count: i32,
    /// Ticks until the child arrives, if there is one on the way.
    pub expecting_in: Option<i64>,
    /// Which of §4.8's requirements is currently missing, in plain language.
    /// "needs 6 more grain" is a story; "cannot reproduce" is not.
    pub cannot_yet: Option<String>,
    /// Beliefs this creature holds that somebody else discovered, and how many
    /// of those discoverers are already dead — S7, per creature.
    pub inherited_beliefs: u32,
    pub from_the_dead: u32,
}

/// Terrain and the things that only change when the world does. Published once
/// and on regeneration, never per tick — it is a quarter of a megabyte.
pub struct TerrainSnapshot {
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
    pub seed: i64,
    pub bytes: Vec<u8>,
    pub founders: Vec<Founder>,
}

/// Resource nodes, republished on an interval because crops are planted and
/// patches are stripped as the run goes on.
#[derive(Debug, Clone, Serialize)]
pub struct NodesSnapshot {
    pub version: u64,
    pub nodes: Vec<ResourceNode>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub tick: i64,
    pub day: i64,
    pub hour: u32,
    pub night: bool,
    pub running: bool,
    pub mode: SpeedMode,

    pub population: u32,
    pub born: u64,
    pub died: u64,
    pub infants: u32,
    pub adults: u32,
    pub elders: u32,
    pub structures_standing: u32,
    pub shelters: u32,
    pub fires_lit: u32,

    pub deaths_by_cause: Vec<(&'static str, u32)>,
    pub tick_ms: f32,
    pub timings: PhaseTimings,
    pub ticks_per_second: f32,
    /// True when the census is being held by the M2 benchmark fixture rather
    /// than by reproduction, so a held run is never read as a self-sustaining
    /// one.
    pub population_maintained: bool,

    // ---- society ---------------------------------------------------------
    pub households: u32,
    pub households_at_reserve: u32,
    pub mean_store: f32,
    pub paired: u32,
    pub expecting: u32,
    pub deepest_generation: i32,
    pub beliefs_taught: u64,
    pub beliefs_shared: u64,

    /// §5.8 and invariant 8: a rising fallback rate is the LLM quietly ceasing
    /// to matter, and that is the S6 failure. It belongs on the dashboard.
    pub llm_enabled: bool,
    pub llm_model: String,
    pub llm_dispatched: u64,
    pub llm_accepted: u64,
    pub llm_in_flight: u32,
    pub fallback_rate: f32,
    pub mean_latency_ms: f32,
    pub cache_hit_rate: f32,
    /// Share of live creatures currently running a plan the model wrote.
    pub on_model_plans: f32,

    pub creatures: Vec<CreatureDot>,
    pub structures: Vec<StructureDot>,
    pub events: Vec<TickerLine>,
    pub selected: Option<CreatureDetail>,
    pub nodes_version: u64,

    /// The community's collective map, as a coarse coverage grid.
    ///
    /// §9.1 calls the knowledge overlay the most interesting view in the
    /// product, and it is the one view that cannot be assembled from anything
    /// else the UI receives — beliefs live in RAM on the sim thread and there
    /// are ~20,000 of them. So it is reduced here to one byte per cell, the
    /// strongest confidence anyone holds about anything in that cell, which is
    /// exactly the "brightness is confidence" reading the design asks for.
    pub known: Vec<u8>,
    /// Side of the coverage grid, in cells.
    pub known_dim: u32,
    /// World tiles per coverage cell.
    pub known_cell: u32,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            tick: 0, day: 0, hour: 0, night: false, running: false,
            mode: SpeedMode::Observe,
            population: 0, born: 0, died: 0, infants: 0, adults: 0, elders: 0,
            structures_standing: 0, shelters: 0, fires_lit: 0,
            deaths_by_cause: Vec::new(),
            tick_ms: 0.0, timings: PhaseTimings::default(), ticks_per_second: 0.0,
            population_maintained: false,
            households: 0, households_at_reserve: 0, mean_store: 0.0,
            paired: 0, expecting: 0, deepest_generation: 1,
            beliefs_taught: 0, beliefs_shared: 0,
            llm_enabled: false, llm_model: String::new(),
            llm_dispatched: 0, llm_accepted: 0, llm_in_flight: 0,
            fallback_rate: 0.0, mean_latency_ms: 0.0, cache_hit_rate: 0.0,
            on_model_plans: 0.0,
            creatures: Vec::new(), structures: Vec::new(), events: Vec::new(),
            selected: None, nodes_version: 0,
            known: Vec::new(), known_dim: 0, known_cell: 8,
        }
    }
}

/// The slots the UI reads. See the module note on why a mutex here does not
/// reintroduce shared world state.
pub struct Shared {
    pub snapshot: Mutex<Arc<Snapshot>>,
    pub terrain: Mutex<Arc<TerrainSnapshot>>,
    pub nodes: Mutex<Arc<NodesSnapshot>>,
}

impl Shared {
    /// The latest published snapshot. Clones an `Arc` and releases the lock
    /// immediately — a reader can never hold up the sim thread.
    pub fn latest(&self) -> Arc<Snapshot> {
        match self.snapshot.lock() {
            Ok(g) => g.clone(),
            // A poisoned lock means a previous holder panicked while doing a
            // pointer store, which cannot leave torn state. An empty snapshot
            // is a better answer than taking the UI down with it.
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// The UI's handle on the simulation: a channel out, snapshots back.
pub struct SimHandle {
    pub shared: Arc<Shared>,
    pub commands: Sender<SimCommand>,
}

impl SimHandle {
    pub fn send(&self, cmd: SimCommand) {
        // A closed channel means the sim thread is gone, which happens only at
        // shutdown; dropping the command is correct there.
        let _ = self.commands.send(cmd);
    }
}

/// What the sim thread does with a finished tick. Kept as a trait object so the
/// loop can be driven in a test without a Tauri app around it.
pub trait SnapshotSink: Send {
    fn publish(&self, snapshot: Arc<Snapshot>);
}

pub struct NullSink;
impl SnapshotSink for NullSink {
    fn publish(&self, _snapshot: Arc<Snapshot>) {}
}

// ----------------------------------------------------------------- the loop

pub struct SimThread {
    sim: Sim,
    conn: rusqlite::Connection,
    shared: Arc<Shared>,
    commands: Receiver<SimCommand>,
    sink: Box<dyn SnapshotSink>,

    running: bool,
    mode: SpeedMode,
    steps_left: u32,
    selected: Option<i64>,
    ticker: std::collections::VecDeque<TickerLine>,
    nodes_version: u64,
    last_emit: Instant,
    taught_total: u64,
    shared_total: u64,
    last_nodes_publish: Instant,
    recent_tick_us: std::collections::VecDeque<u64>,
}

impl SimThread {
    pub fn new(
        sim: Sim,
        conn: rusqlite::Connection,
        shared: Arc<Shared>,
        commands: Receiver<SimCommand>,
        sink: Box<dyn SnapshotSink>,
    ) -> Self {
        Self {
            sim,
            conn,
            shared,
            commands,
            sink,
            // A simulation you open to watch should be running. The player is
            // an observer, not an operator who has to start the world.
            running: true,
            mode: SpeedMode::Observe,
            steps_left: 0,
            selected: None,
            ticker: std::collections::VecDeque::with_capacity(64),
            nodes_version: 1,
            last_emit: Instant::now(),
            taught_total: 0,
            shared_total: 0,
            last_nodes_publish: Instant::now(),
            recent_tick_us: std::collections::VecDeque::with_capacity(64),
        }
    }

    /// Run until told to shut down. Owns the world for its whole life.
    pub fn run(mut self) {
        self.sim.mode = self.mode;
        // Start the model here rather than in `Sim::new`: every test builds a
        // simulation, and none of them should spawn worker threads or need
        // Ollama to be running.
        self.sim.enable_llm();
        self.publish_terrain();
        self.publish_nodes();
        self.emit(&TickReport::default(), true);

        loop {
            match self.drain_commands() {
                Flow::Stop => break,
                Flow::Continue => {}
            }

            let should_tick = self.steps_left > 0 || self.running;
            if !should_tick {
                // Idle: block on the channel rather than spinning, so a paused
                // simulation costs nothing.
                match self.commands.recv_timeout(Duration::from_millis(120)) {
                    Ok(cmd) => {
                        if matches!(self.apply(cmd), Flow::Stop) {
                            break;
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                    Err(_) => break,
                }
                continue;
            }

            let started = Instant::now();
            let mut report = self.sim.step();
            if let Err(e) = self.sim.persist(&mut self.conn, &mut report, false) {
                tracing::error!(tick = report.tick, error = %e, "persist failed");
            }
            let elapsed = started.elapsed();

            // Tell the simulation how fast it is actually being run: whether a
            // deliberation is worth dispatching depends on how many ticks pass
            // while the model thinks, not on how many seconds.
            self.sim.ticks_per_second = if elapsed.as_secs_f32() > 0.0 {
                let paced = self
                    .mode
                    .tick_interval(self.sim.llm.is_some())
                    .map(|i| i.as_secs_f32().max(elapsed.as_secs_f32()))
                    .unwrap_or(elapsed.as_secs_f32());
                1.0 / paced.max(0.001)
            } else {
                self.sim.ticks_per_second
            };
            self.taught_total += report.beliefs_taught as u64;
            self.shared_total += report.beliefs_shared as u64;
            self.recent_tick_us.push_back(elapsed.as_micros() as u64);
            if self.recent_tick_us.len() > 64 {
                self.recent_tick_us.pop_front();
            }
            self.collect_ticker();

            if self.steps_left > 0 {
                self.steps_left -= 1;
                if self.steps_left == 0 {
                    self.running = false;
                }
            }

            // Snapshot emission is throttled by wall clock, not by tick count.
            // In Fast-Forward the loop can run hundreds of ticks a second and
            // pushing every one of them would drown the webview in JSON for
            // frames it will never draw.
            let force = !self.running;
            self.emit(&report, force);

            if self.last_nodes_publish.elapsed() > Duration::from_millis(900) {
                self.publish_nodes();
            }

            if let Some(interval) = self.mode.tick_interval(self.sim.llm.is_some()) {
                if let Some(rest) = interval.checked_sub(elapsed) {
                    std::thread::sleep(rest);
                }
            }
        }

        self.shutdown();
    }

    fn shutdown(mut self) {
        tracing::info!(tick = self.sim.tick, "sim thread stopping; flushing state");
        let mut report = TickReport { tick: self.sim.tick, ..Default::default() };
        // A forced checkpoint, so a resume picks up exactly here.
        if let Err(e) = self.sim.persist(&mut self.conn, &mut report, true) {
            tracing::error!(error = %e, "final flush failed");
        }
    }

    fn drain_commands(&mut self) -> Flow {
        loop {
            match self.commands.try_recv() {
                Ok(cmd) => {
                    if matches!(self.apply(cmd), Flow::Stop) {
                        return Flow::Stop;
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return Flow::Continue,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return Flow::Stop,
            }
        }
    }

    fn apply(&mut self, cmd: SimCommand) -> Flow {
        match cmd {
            SimCommand::Play => self.running = true,
            SimCommand::Pause => {
                self.running = false;
                self.steps_left = 0;
                // Pausing is a natural checkpoint: flush so a crash while
                // paused loses nothing.
                let mut report = TickReport { tick: self.sim.tick, ..Default::default() };
                let _ = self.sim.persist(&mut self.conn, &mut report, true);
            }
            SimCommand::Step(n) => {
                self.steps_left = n.max(1);
                self.running = false;
            }
            SimCommand::SetMode(m) => {
                self.mode = m;
                // The budget is a simulation fact, not a rendering one (§5.6).
                self.sim.mode = m;
            }
            SimCommand::Select(id) => {
                self.selected = id;
                // §5.3's narrative weight: whoever is being watched is worth
                // thinking about.
                self.sim.selected = id;
                let report = TickReport { tick: self.sim.tick, ..Default::default() };
                self.emit(&report, true);
            }
            SimCommand::Regenerate { seed, creatures } => {
                self.regenerate(seed, creatures);
                self.sim.mode = self.mode;
                self.sim.enable_llm();
            }
            SimCommand::Shutdown => return Flow::Stop,
        }
        Flow::Continue
    }

    fn regenerate(&mut self, seed: i64, creatures: u32) {
        tracing::info!(seed, creatures, "regenerating world");
        // Take the *current* defaults, not the config this world was created
        // with. Regenerating is "start over", and a world made before M6's
        // balance pass carries dials — soil at 0.012, a 20-grain reserve, a
        // 168-tick infancy — under which no lineage reaches generation 2. A
        // player who regenerates to get a fresh start and silently keeps the
        // old economy has no way to tell why nothing is happening.
        let cfg = crate::config::WorldConfig::default();
        let out = crate::sim::worldgen::generate(seed as u64, &cfg);

        let world_id = self.sim.world_id;
        // A new world means the old one's history is not this world's history.
        for table in [
            "creatures", "beliefs", "events", "decisions", "tick_stats",
            "creature_samples", "structures", "transmissions", "relationships",
            "households",
        ] {
            let _ = self.conn.execute(
                &format!("DELETE FROM {table} WHERE world_id = ?1"),
                rusqlite::params![world_id],
            );
        }
        let _ = repo::save_world(&mut self.conn, world_id, &out.world);
        let _ = self.conn.execute(
            "UPDATE worlds SET seed = ?2, current_tick = 0, config_json = ?3 WHERE id = ?1",
            rusqlite::params![
                world_id,
                seed,
                serde_json::to_string(&cfg).unwrap_or_default()
            ],
        );

        let mut sim = Sim::new(world_id, out.world, cfg, seed as u64);
        if creatures > 0 {
            sim.spawn_population(creatures);
        } else {
            sim.spawn_founders();
        }
        self.sim = sim;
        self.ticker.clear();
        self.nodes_version += 1;
        self.publish_terrain();
        self.publish_nodes();
        let report = TickReport { tick: 0, ..Default::default() };
        self.emit(&report, true);
    }

    fn collect_ticker(&mut self) {
        use crate::sim::event::EventKind as K;
        for e in &self.sim.events {
            // The ticker is for things a person would notice. Discovery and
            // routine upkeep are the bulk of the log and would bury everything.
            let (tone, text) = match e.kind {
                K::Died => ("death", format!(
                    "#{} died — {}",
                    e.actor_id.unwrap_or(0),
                    e.payload.split_whitespace().next().unwrap_or("").replace("cause=", "")
                )),
                K::Born => ("birth", format!("#{} was born", e.actor_id.unwrap_or(0))),
                K::Settled => ("birth", format!("#{} arrived as a settler", e.actor_id.unwrap_or(0))),
                K::ShelterBuilt => ("", format!(
                    "#{} raised a shelter at {},{}",
                    e.actor_id.unwrap_or(0), e.x.unwrap_or(0), e.y.unwrap_or(0)
                )),
                K::Planted => ("", format!(
                    "#{} planted wheat at {},{}",
                    e.actor_id.unwrap_or(0), e.x.unwrap_or(0), e.y.unwrap_or(0)
                )),
                K::Slaughtered => ("", format!("#{} slaughtered a sheep", e.actor_id.unwrap_or(0))),
                K::FireLit => ("", format!(
                    "#{} lit a fire at {},{}",
                    e.actor_id.unwrap_or(0), e.x.unwrap_or(0), e.y.unwrap_or(0)
                )),
                K::Injured => ("death", format!("#{} was hurt working", e.actor_id.unwrap_or(0))),
                K::FellIll => ("death", format!("#{} fell ill", e.actor_id.unwrap_or(0))),
                _ => continue,
            };
            self.ticker.push_front(TickerLine { tick: e.tick, kind: e.kind.as_str(), text, tone });
        }
        while self.ticker.len() > 40 {
            self.ticker.pop_back();
        }
    }

    fn emit(&mut self, report: &TickReport, force: bool) {
        if !force && self.last_emit.elapsed() < Duration::from_millis(66) {
            return;
        }
        self.last_emit = Instant::now();

        let cfg = &self.sim.cfg;
        let tick = self.sim.tick;
        let night = economy::is_night(tick, cfg);
        let n = &cfg.needs;

        let thinking = self.sim.pending_ids();
        let mut infants = 0;
        let mut adults = 0;
        let mut elders = 0;
        let creatures: Vec<CreatureDot> = self
            .sim
            .creatures
            .iter()
            .map(|c| {
                let stage = match c.life_stage {
                    LifeStage::Infant => { infants += 1; 0 }
                    LifeStage::Adult => { adults += 1; 1 }
                    LifeStage::Elder => { elders += 1; 2 }
                };
                let mut flags = 0u8;
                if c.hunger < n.deficit_threshold { flags |= 1; }
                if c.thirst < n.deficit_threshold { flags |= 2; }
                if c.warmth < n.deficit_threshold { flags |= 4; }
                if c.in_shelter.is_some() { flags |= 8; }
                if c.at_fire { flags |= 16; }
                // §9.1's deliberation heatmap: who the model is actually
                // spending attention on. The debugging tool for §5.3, and the
                // thing that makes an invisible budget visible.
                if thinking.contains(&c.id) { flags |= 32; }
                if c.plan.as_ref().is_some_and(|p| p.tier == 2) { flags |= 64; }
                CreatureDot { id: c.id, x: c.x, y: c.y, stage, flags }
            })
            .collect();

        let mut shelters = 0;
        let mut fires_lit = 0;
        let structures: Vec<StructureDot> = self
            .sim
            .structures
            .items
            .iter()
            .map(|s| {
                let lit = s.is_lit(tick);
                match s.kind {
                    StructureKind::Shelter => shelters += 1,
                    StructureKind::Fire if lit => fires_lit += 1,
                    _ => {}
                }
                StructureDot {
                    id: s.id, x: s.x, y: s.y,
                    kind: s.kind.as_str(), lit, condition: s.condition,
                }
            })
            .collect();

        let deaths_by_cause: Vec<(&'static str, u32)> = DeathCause::ALL
            .iter()
            .map(|c| (c.as_str(), self.sim.deaths_by_cause[*c as usize]))
            .filter(|(_, n)| *n > 0)
            .collect();

        let mean_us = if self.recent_tick_us.is_empty() {
            0.0
        } else {
            self.recent_tick_us.iter().sum::<u64>() as f32 / self.recent_tick_us.len() as f32
        };

        let selected = self.selected.and_then(|id| self.detail_for(id));
        let (known, known_dim, known_cell) = self.collective_map();

        let reserve = cfg.reproduction.store_reserve;
        let live: Vec<&crate::sim::social::Household> =
            self.sim.households.items.iter().filter(|h| h.is_alive()).collect();
        let live_households = live.len();
        let at_reserve = live.iter().filter(|h| h.stored_food() >= reserve).count() as u32;
        let mean_store = if live_households == 0 {
            0.0
        } else {
            live.iter().map(|h| h.stored_food()).sum::<f32>() / live_households as f32
        };
        let st = self.sim.llm_stats;
        let paired = self.sim.creatures.iter().filter(|c| c.mate_id.is_some()).count() as u32;
        let expecting =
            self.sim.creatures.iter().filter(|c| c.pregnancy.is_some()).count() as u32;
        let deepest_generation =
            self.sim.creatures.iter().map(|c| c.generation).max().unwrap_or(1);

        let snapshot = Arc::new(Snapshot {
            tick,
            day: economy::day_of(tick),
            hour: economy::hour_of(tick),
            night,
            running: self.running || self.steps_left > 0,
            mode: self.mode,
            population: self.sim.creatures.len() as u32,
            born: self.sim.total_births,
            died: self.sim.total_deaths,
            infants,
            adults,
            elders,
            structures_standing: structures.len() as u32,
            shelters,
            fires_lit,
            deaths_by_cause,
            tick_ms: mean_us / 1000.0,
            timings: report.timings,
            ticks_per_second: if mean_us > 0.0 { 1_000_000.0 / mean_us } else { 0.0 },
            population_maintained: self.sim.population_maintained,
            households: live_households as u32,
            households_at_reserve: at_reserve,
            mean_store,
            paired,
            expecting,
            deepest_generation,
            beliefs_taught: self.taught_total,
            beliefs_shared: self.shared_total,
            llm_enabled: self.sim.llm.is_some(),
            llm_model: self.sim.cfg.llm.model.clone(),
            llm_dispatched: st.dispatched,
            llm_accepted: st.accepted,
            llm_in_flight: self.sim.llm.as_ref().map(|d| d.outstanding()).unwrap_or(0) as u32,
            fallback_rate: st.fallback_rate(),
            mean_latency_ms: st.mean_latency_ms(),
            cache_hit_rate: if st.tokens_prompt == 0 {
                0.0
            } else {
                st.tokens_cached as f32 / st.tokens_prompt as f32
            },
            on_model_plans: if self.sim.creatures.is_empty() {
                0.0
            } else {
                self.sim.creatures.iter().filter(|c| {
                    c.plan.as_ref().is_some_and(|p| p.tier == 2)
                }).count() as f32
                    / self.sim.creatures.len() as f32
            },
            creatures,
            structures,
            events: self.ticker.iter().cloned().collect(),
            selected,
            nodes_version: self.nodes_version,
            known,
            known_dim,
            known_cell,
        });

        if let Ok(mut slot) = self.shared.snapshot.lock() {
            *slot = snapshot.clone();
        }
        self.sink.publish(snapshot);
    }

    /// What the community collectively knows, reduced to a coverage grid.
    ///
    /// Watching this expand across a run — and contract when a well-travelled
    /// creature dies without passing anything on — is the clearest picture of
    /// culture forming and being lost (§9.1). At M2 there is no transmission
    /// yet, so what it shows is the reach of firsthand exploration; the same
    /// view answers S7 once teaching lands at M4.
    fn collective_map(&self) -> (Vec<u8>, u32, u32) {
        const CELL: u32 = 8;
        let dim = self.sim.world.width.div_ceil(CELL);
        let mut grid = vec![0u8; (dim * dim) as usize];
        let k = &self.sim.cfg.knowledge;
        let tick = self.sim.tick;

        for c in &self.sim.creatures {
            for b in &c.beliefs {
                let cx = (b.x / CELL).min(dim - 1);
                let cy = (b.y / CELL).min(dim - 1);
                let i = (cy * dim + cx) as usize;
                let v = (b.confidence_at(tick, k) * 255.0) as u8;
                if v > grid[i] {
                    grid[i] = v;
                }
            }
        }
        (grid, dim, CELL)
    }

    /// The inspector's view of one creature (§9.2). Computed only when somebody
    /// is looking at it.
    fn detail_for(&self, id: i64) -> Option<CreatureDetail> {
        let c = self.sim.creatures.iter().find(|c| c.id == id)?;
        let cfg = &self.sim.cfg;
        let tick = self.sim.tick;

        let carrying: Vec<(String, f32, Option<i64>)> = {
            let mut totals: Vec<(ItemKind, f32, Option<i64>)> = Vec::new();
            for b in &c.inventory.batches {
                let spoils = economy::ticks_until_spoiled(b, tick, cfg);
                match totals.iter_mut().find(|(k, _, _)| *k == b.kind) {
                    Some(e) => {
                        e.1 += b.quantity;
                        // Show the batch that will turn first.
                        e.2 = match (e.2, spoils) {
                            (Some(a), Some(b)) => Some(a.min(b)),
                            (None, s) => s,
                            (s, None) => s,
                        };
                    }
                    None => totals.push((b.kind, b.quantity, spoils)),
                }
            }
            totals.into_iter().map(|(k, q, s)| (k.as_str().to_string(), q, s)).collect()
        };

        // Beliefs ranked exactly as the policy ranks them — the §13.4 ordering.
        // The inspector shows what the creature would actually act on, which is
        // also what M3's prompt will carry.
        let needs = NeedProfile {
            food: 1.0 - (c.hunger / 100.0),
            water: 1.0 - (c.thirst / 100.0),
            fuel: 0.5,
            shelter: 1.0 - (c.warmth / 100.0),
        };
        // Ranked, but not truncated to the prompt budget: the inspector shows
        // everything the creature holds, most relevant first. The prompt cap is
        // about what fits in a context window, which is a different question.
        let order = knowledge::rank(
            &c.beliefs, (c.x, c.y), tick, &needs, &cfg.knowledge, c.beliefs.len(),
        );
        let beliefs: Vec<BeliefView> = order
            .into_iter()
            .map(|i| {
                let b = &c.beliefs[i];
                BeliefView {
                    kind: b.kind.as_str(),
                    x: b.x,
                    y: b.y,
                    estimate: b.estimate.as_str(),
                    confidence: b.confidence_at(tick, &cfg.knowledge),
                    hops: b.hops,
                    provenance: b.provenance(tick),
                }
            })
            .collect();

        let (steps, rationale, addresses, horizon, remaining, tier) = match &c.plan {
            Some(p) => (
                p.steps
                    .iter()
                    .enumerate()
                    .map(|(i, s)| PlanStepView {
                        goal: s.goal.as_str(),
                        label: s.describe(&self.sim.world),
                        done: i < p.step_index,
                        current: i == p.step_index,
                        est_ticks: s.est_ticks,
                    })
                    .collect(),
                p.rationale.clone(),
                format!("{:?}", p.addresses),
                p.horizon,
                p.ticks_remaining,
                p.tier,
            ),
            None => (Vec::new(), String::new(), "None".into(), 0, 0, 1),
        };

        let name_of = |id: i64| {
            self.sim.creatures.iter().find(|o| o.id == id).map(|o| (id, o.name.clone()))
        };
        let household = c.household_id.and_then(|h| self.sim.households.get(h));
        let members = c
            .household_id
            .map(|h| self.sim.creatures.iter().filter(|o| o.household_id == Some(h)).count())
            .unwrap_or(0) as u32;

        // Why not yet? Only meaningful for a paired female; for anyone else the
        // answer is a different question and saying "not paired" to a bachelor
        // is not information.
        let cannot_yet = c.mate_id.and_then(|mate| {
            let partner = self.sim.creatures.iter().find(|o| o.id == mate)?;
            let (mother, father) = if c.sex == crate::sim::creature::Sex::Female {
                (c, partner)
            } else {
                (partner, c)
            };
            let blocker = crate::sim::social::conception_blocker(
                mother, father, household, cfg, tick,
            )?;
            Some(match blocker {
                crate::sim::social::Blocker::StoreShort => {
                    let short = cfg.reproduction.store_reserve
                        - household.map(|h| h.stored_food()).unwrap_or(0.0);
                    format!("needs {:.0} more put by", short.max(0.0))
                }
                other => other.as_str().to_string(),
            })
        });

        let dead = |id: i64| !self.sim.creatures.iter().any(|o| o.id == id);
        let inherited_beliefs = c
            .beliefs
            .iter()
            .filter(|b| b.origin_creature_id.is_some_and(|o| o != c.id))
            .count() as u32;
        let from_the_dead = c
            .beliefs
            .iter()
            .filter(|b| b.origin_creature_id.is_some_and(|o| o != c.id && dead(o)))
            .count() as u32;

        Some(CreatureDetail {
            id: c.id,
            name: c.name.clone(),
            sex: c.sex.as_str(),
            generation: c.generation,
            age: c.age(tick),
            expected_lifespan: c.lifespan_ticks as u32,
            life_stage: c.life_stage.as_str(),
            x: c.x,
            y: c.y,
            felt_state: c.felt_state(&cfg.needs),
            hunger: c.hunger,
            thirst: c.thirst,
            fatigue: c.fatigue,
            warmth: c.warmth,
            health: c.health,
            traits: c.traits,
            carrying,
            plan_rationale: rationale,
            plan_addresses: addresses,
            plan_horizon: horizon,
            plan_remaining: remaining,
            plan_tier: tier,
            steps,
            beliefs,
            belief_count: c.beliefs.len(),
            lifetime_deliberations: c.lifetime_deliberations,
            sheltered: c.in_shelter.is_some(),

            household_id: c.household_id,
            household_store: household.map(|h| h.stored_food()).unwrap_or(0.0),
            household_grain: household.map(|h| h.grain()).unwrap_or(0.0),
            household_members: members,
            mate: c.mate_id.and_then(name_of),
            mother: c.mother_id.and_then(name_of),
            father: c.father_id.and_then(name_of),
            children_born: c.children_born,
            taught_count: c.taught_count,
            shared_count: c.shared_count,
            expecting_in: c.pregnancy.map(|p| p.due_tick - tick),
            cannot_yet,
            inherited_beliefs,
            from_the_dead,
        })
    }

    fn publish_terrain(&self) {
        let w = &self.sim.world;
        let snap = Arc::new(TerrainSnapshot {
            width: w.width,
            height: w.height,
            chunk_size: w.chunk_size,
            seed: w.seed as i64,
            bytes: w.terrain_bytes(),
            founders: w.founders.clone(),
        });
        if let Ok(mut slot) = self.shared.terrain.lock() {
            *slot = snap;
        }
    }

    fn publish_nodes(&mut self) {
        self.last_nodes_publish = Instant::now();
        self.nodes_version += 1;
        let snap = Arc::new(NodesSnapshot {
            version: self.nodes_version,
            nodes: self.sim.world.nodes.clone(),
        });
        if let Ok(mut slot) = self.shared.nodes.lock() {
            *slot = snap;
        }
    }
}

enum Flow {
    Continue,
    Stop,
}

/// Build the shared slots for a freshly created simulation.
pub fn shared_for(sim: &Sim) -> Arc<Shared> {
    let w = &sim.world;
    Arc::new(Shared {
        snapshot: Mutex::new(Arc::new(Snapshot::default())),
        terrain: Mutex::new(Arc::new(TerrainSnapshot {
            width: w.width,
            height: w.height,
            chunk_size: w.chunk_size,
            seed: w.seed as i64,
            bytes: w.terrain_bytes(),
            founders: w.founders.clone(),
        })),
        nodes: Mutex::new(Arc::new(NodesSnapshot { version: 0, nodes: w.nodes.clone() })),
    })
}

/// Spawn the simulation thread and hand back the UI's handle to it.
pub fn spawn(
    sim: Sim,
    conn: rusqlite::Connection,
    sink: Box<dyn SnapshotSink>,
) -> (SimHandle, std::thread::JoinHandle<()>) {
    let shared = shared_for(&sim);
    let (tx, rx) = std::sync::mpsc::channel();
    let thread_shared = shared.clone();

    let handle = std::thread::Builder::new()
        .name("life-zone-sim".into())
        .spawn(move || {
            SimThread::new(sim, conn, thread_shared, rx, sink).run();
        })
        .expect("spawning the simulation thread");

    (SimHandle { shared, commands: tx }, handle)
}

/// Guard against a config that would make the UI unusable.
pub fn creature_budget(cfg: &WorldConfig) -> u32 {
    cfg.bench.initial_creatures.unwrap_or(cfg.map.founder_count)
}

/// Unused at M2; kept so the type is exercised and the import does not rot.
pub fn creature_is_alive(c: &Creature) -> bool {
    c.is_alive()
}
