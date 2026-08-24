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
    fn tick_interval(self) -> Option<Duration> {
        match self {
            SpeedMode::Deep => Some(Duration::from_millis(500)),
            SpeedMode::Observe => Some(Duration::from_millis(140)),
            SpeedMode::Focus => Some(Duration::from_millis(220)),
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
    /// Bit flags: 1 hungry, 2 thirsty, 4 cold, 8 sheltered, 16 at a fire.
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
            last_nodes_publish: Instant::now(),
            recent_tick_us: std::collections::VecDeque::with_capacity(64),
        }
    }

    /// Run until told to shut down. Owns the world for its whole life.
    pub fn run(mut self) {
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

            if let Some(interval) = self.mode.tick_interval() {
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
            SimCommand::SetMode(m) => self.mode = m,
            SimCommand::Select(id) => {
                self.selected = id;
                let report = TickReport { tick: self.sim.tick, ..Default::default() };
                self.emit(&report, true);
            }
            SimCommand::Regenerate { seed, creatures } => {
                self.regenerate(seed, creatures);
            }
            SimCommand::Shutdown => return Flow::Stop,
        }
        Flow::Continue
    }

    fn regenerate(&mut self, seed: i64, creatures: u32) {
        tracing::info!(seed, creatures, "regenerating world");
        let cfg = self.sim.cfg.clone();
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
            "UPDATE worlds SET seed = ?2, current_tick = 0 WHERE id = ?1",
            rusqlite::params![world_id, seed],
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
