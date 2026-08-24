//! The tick pipeline (PRD §4.2) — the seven phases, in order.
//!
//! Phases 1-3 and 5-7 are deterministic and fast. Phase 4 is the only one that
//! will ever touch the LLM; at M2 it runs the Tier 1 policy and nothing else,
//! and Tier 1 keeps running there forever after M3 lands beside it.
//!
//! Every phase is timed and the timings go into `tick_stats.phase_timings_json`
//! from this first commit, because "which phase" is the first question every
//! time a tick is slow and reconstructing it later is guesswork.
//!
//! Determinism (invariant 7): the whole simulation is deterministic at M2
//! because there is no model in it. Creatures are visited in ascending id
//! order, no traversal depends on a `HashMap`, and every random draw comes from
//! one seeded ChaCha8 stream consumed in that same fixed order. The golden-run
//! test is what holds this honest.

use crate::ai::policy::{self, PolicyCtx};
use crate::config::WorldConfig;
use crate::sim::actions::{self, AbortReason, ActionCtx, Outcome};
use crate::sim::creature::{
    Addresses, Creature, DeathCause, Inventory, ItemKind, LifeStage, Sex, Traits,
};
use crate::sim::economy::{self, NodeIndex, Structures};
use crate::sim::event::{Event, EventKind};
use crate::sim::knowledge;
use crate::sim::pathfind::Pathfinder;
use crate::sim::knowledge::Channel;
use crate::sim::perception::{self, WorldCache};
use crate::sim::social::{
    self, Bystander, CreatureIndex, Courtships, Households, RelKind, Relationships, SocialIntent,
};
use crate::sim::world::World;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};
use std::time::Instant;

/// The random stream for one tick. Distinct from worldgen's, so changing how
/// many draws generation makes cannot shift the simulation's rolls.
fn tick_rng(seed: u64, tick: i64) -> ChaCha8Rng {
    ChaCha8Rng::seed_from_u64(
        (seed ^ 0x51_4E_47_00).wrapping_add((tick as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)),
    )
}

/// Per-phase wall-clock cost of one tick, in microseconds.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct PhaseTimings {
    pub world: u64,
    pub needs: u64,
    pub plans: u64,
    pub deliberate: u64,
    pub act: u64,
    pub resolve: u64,
    pub persist: u64,
}

impl PhaseTimings {
    pub fn total(&self) -> u64 {
        self.world + self.needs + self.plans + self.deliberate + self.act + self.resolve
            + self.persist
    }
}

/// What one tick did, handed to the persistence layer and the UI.
#[derive(Debug, Default)]
pub struct TickReport {
    pub tick: i64,
    pub population: u32,
    pub births: u32,
    pub deaths: u32,
    pub deliberations: u32,
    pub plans_abandoned: u32,
    pub food_gathered: f32,
    pub food_eaten: f32,
    pub food_spoiled: f32,
    pub discoveries: u32,
    pub pairings: u32,
    pub rejections: u32,
    pub conceptions: u32,
    pub beliefs_shared: u32,
    pub beliefs_taught: u32,
    pub beliefs_overheard: u32,
    pub households_founded: u32,
    /// Arrivals from the measurement fixture, counted separately from births.
    /// Folding them into `births` made a held run look like a fertile one —
    /// 4,733 "births" in a run with zero conceptions — which is exactly the
    /// confusion the fixture's labelling exists to prevent.
    pub settlers: u32,
    /// Which of §4.8's requirements stopped a conception this tick, tallied by
    /// blocker. Without this, "nobody is reproducing" is a dead end: the four
    /// requirements fail for completely different reasons and only one of them
    /// is ever the real one.
    pub conception_blocked: [u32; 7],
    /// Creatures that executed a step this tick. §5.2 promises Tier 0 runs for
    /// every creature every tick, so this must always equal the population that
    /// entered phase 5 — if it does not, somebody stalled waiting for a
    /// decision, which is the exact failure Tier 1 exists to prevent.
    pub acted: u32,
    pub timings: PhaseTimings,
}

/// A decision to be recorded (PRD §7). At M2 every row is tier 1; the LLM
/// columns exist now so M3 fills them in rather than migrating the table.
#[derive(Debug, Clone)]
pub struct DecisionRecord {
    pub tick: i64,
    pub creature_id: i64,
    pub tier: u8,
    pub age_ticks: i64,
    pub life_stage: LifeStage,
    pub goal: String,
    /// Which need the plan exists to serve. The first step's goal is a poor
    /// proxy — every errand starts with MOVE_TO — so intent is recorded
    /// directly.
    pub addresses: Addresses,
    pub rationale: String,
    pub horizon_committed: u32,
    pub fallback_reason: Option<&'static str>,
}

/// One act of transmission, for the culture reports (§10): the transmission
/// graph, teaching rate by household, and whether information hubs emerge.
#[derive(Debug, Clone, Copy)]
pub struct TransmissionRecord {
    pub tick: i64,
    pub from: i64,
    pub to: i64,
    pub channel: Channel,
    pub count: u32,
}

/// A plan that ended, so `horizon_actual` and `abort_reason` can be backfilled.
/// The gap between committed and actual is plan-abandonment, the early-warning
/// metric for the horizon mechanic failing (§5.5).
#[derive(Debug, Clone, Copy)]
pub struct PlanOutcome {
    pub creature_id: i64,
    pub set_tick: i64,
    pub horizon_actual: u32,
    pub reason: AbortReason,
}

/// The simulation. Owns all world state exclusively (PRD §3.1): nothing here is
/// shared with the UI thread, which sees only snapshots.
pub struct Sim {
    pub world_id: i64,
    pub cfg: WorldConfig,
    pub tick: i64,

    pub world: World,
    pub creatures: Vec<Creature>,
    pub structures: Structures,
    pub households: Households,
    pub relationships: Relationships,
    pub courtships: Courtships,
    /// Rebuilt once at the top of every tick, so deliberation and action both
    /// see the same population. Positions are therefore end-of-previous-tick
    /// throughout a tick, which is a deliberate one-tick lag: it costs a
    /// creature nothing that matters and it means adjacency cannot change
    /// underneath a decision that was made on it.
    pub people: CreatureIndex,

    pub cache: WorldCache,
    pub node_index: NodeIndex,
    pub pathfinder: Pathfinder,

    /// Reseeded from `(seed, tick)` at the start of every tick rather than
    /// carried forward. Two consequences, both wanted:
    ///
    /// * a run resumed from a checkpoint draws exactly what an uninterrupted
    ///   one would, without the stream position having to be persisted, which
    ///   is what makes the schema round-trip test in BUILD.md §9 hold;
    /// * a tick's randomness does not depend on how many draws every earlier
    ///   tick happened to make, so adding a call site somewhere cannot silently
    ///   shift the whole future of a run.
    rng: ChaCha8Rng,
    seed: u64,
    next_creature_id: i64,

    /// Buffers reused across ticks so a tick allocates almost nothing.
    scratch: Vec<u32>,
    bystanders: Vec<Bystander>,
    flagged: Vec<usize>,
    intents: Vec<SocialIntent>,

    pub events: Vec<Event>,
    pub decisions: Vec<DecisionRecord>,
    pub plan_outcomes: Vec<PlanOutcome>,
    pub transmissions: Vec<TransmissionRecord>,
    pub deaths_by_cause: [u32; 7],
    pub total_births: u64,
    pub total_deaths: u64,
    /// Creatures that died this tick, held until phase 7 has written their
    /// final row. A creature's death is the one moment its state *must* reach
    /// the database, because it will never be checkpointed again.
    pending_dead: Vec<Creature>,
    /// Creatures born since the last write. Their rows go in on the very next
    /// tick rather than waiting for a checkpoint: `beliefs.creature_id` is a
    /// foreign key into `creatures`, so a creature that has beliefs but no row
    /// yet fails the constraint — and more simply, a creature should exist in
    /// the record from the moment it is born.
    pending_born: Vec<i64>,
    /// Set when the population is being held by the benchmark fixture rather
    /// than by reproduction, so a run made that way is never mistaken for one
    /// that sustained itself.
    pub population_maintained: bool,
}

impl Sim {
    pub fn new(world_id: i64, world: World, cfg: WorldConfig, seed: u64) -> Self {
        let cache = WorldCache::build(&world);
        let node_index = NodeIndex::new(&world, 8);
        let pathfinder = Pathfinder::new(&world);
        Self {
            world_id,
            cfg,
            tick: 0,
            people: CreatureIndex::new(world.width, world.height, 8),
            world,
            creatures: Vec::new(),
            structures: Structures::new(),
            households: Households::new(),
            relationships: Relationships::new(),
            courtships: Courtships::new(),
            cache,
            node_index,
            pathfinder,
            // A stream distinct from worldgen's, so changing how many draws
            // generation makes cannot shift the simulation's rolls.
            rng: tick_rng(seed, 0),
            seed,
            next_creature_id: 1,
            scratch: Vec::new(),
            bystanders: Vec::new(),
            flagged: Vec::new(),
            intents: Vec::new(),
            events: Vec::new(),
            decisions: Vec::new(),
            plan_outcomes: Vec::new(),
            transmissions: Vec::new(),
            deaths_by_cause: [0; 7],
            total_births: 0,
            total_deaths: 0,
            pending_dead: Vec::new(),
            pending_born: Vec::new(),
            population_maintained: false,
        }
    }

    pub fn alive(&self) -> usize {
        self.creatures.len()
    }

    pub fn set_next_creature_id(&mut self, id: i64) {
        self.next_creature_id = id.max(1);
    }

    pub fn next_creature_id(&self) -> i64 {
        self.next_creature_id
    }

    pub fn creature(&self, id: i64) -> Option<&Creature> {
        self.creatures.iter().find(|c| c.id == id)
    }

    /// Turn worldgen's founders into `creatures` rows — the handover M1 left
    /// deliberately undone.
    pub fn spawn_founders(&mut self) {
        let founders = self.world.founders.clone();
        for f in founders {
            let sex = if f.female { Sex::Female } else { Sex::Male };
            // Founders start as established adults rather than at tick zero of
            // adulthood: they are the people who were already here.
            let age = self.rng.gen_range(180..320);
            self.spawn_at(f.x, f.y, sex, age, 1);
        }
    }

    /// Seed a population directly, for the M2 measurement. Placed by spreading
    /// out from the founders' hearth so they start as a community rather than
    /// scattered across the map.
    pub fn spawn_population(&mut self, n: u32) {
        let (hx, hy) = self
            .world
            .founders
            .first()
            .map(|f| (f.x, f.y))
            .unwrap_or((self.world.width / 2, self.world.height / 2));

        for _ in 0..n {
            let (x, y) = self.scatter_near(hx, hy, 34);
            let sex = if self.rng.gen::<bool>() { Sex::Female } else { Sex::Male };
            // Staggered ages, so a seeded cohort does not all reach old age in
            // the same fortnight and produce a population cliff.
            let age = self.rng.gen_range(0..560);
            self.spawn_at(x, y, sex, age, 1);
        }
    }

    fn scatter_near(&mut self, hx: u32, hy: u32, radius: u32) -> (u32, u32) {
        for _ in 0..64 {
            let dx = self.rng.gen_range(-(radius as i64)..=(radius as i64));
            let dy = self.rng.gen_range(-(radius as i64)..=(radius as i64));
            let (x, y) = (hx as i64 + dx, hy as i64 + dy);
            if !self.world.in_bounds(x, y) {
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            let t = self.world.at(x, y);
            if t.passable() && !t.is_water() {
                return (x, y);
            }
        }
        (hx, hy)
    }

    pub fn spawn_at(&mut self, x: u32, y: u32, sex: Sex, age: i64, generation: i32) -> i64 {
        let id = self.next_creature_id;
        self.next_creature_id += 1;

        let traits = Traits::random(&mut self.rng);
        let name = crate::sim::creature::name_for(&mut self.rng);
        let stage = LifeStage::of(age, &self.cfg.lifespan);
        // A little spread in constitution, so identical circumstances do not
        // produce identical deaths.
        let span = self.cfg.lifespan.baseline_ticks as f32
            * (0.9 + self.rng.gen::<f32>() * 0.2);

        let mut c = Creature {
            id,
            name,
            sex,
            generation,
            mother_id: None,
            father_id: None,
            household_id: None,
            birth_tick: self.tick - age,
            death_tick: None,
            death_cause: None,
            x,
            y,
            life_stage: stage,
            hunger: 70.0 + self.rng.gen::<f32>() * 30.0,
            thirst: 70.0 + self.rng.gen::<f32>() * 30.0,
            fatigue: 70.0 + self.rng.gen::<f32>() * 30.0,
            warmth: 70.0 + self.rng.gen::<f32>() * 30.0,
            health: 100.0,
            lifespan_ticks: span,
            wear: 0.0,
            traits,
            inventory: Inventory::default(),
            plan: None,
            beliefs: Vec::new(),
            last_deliberation_tick: None,
            deliberation_pressure: 0.0,
            lifetime_deliberations: 0,
            lifetime_think_fatigue: 0.0,
            in_shelter: None,
            exposed_ticks: 0,
            at_fire: false,
            trauma: None,
            mate_id: None,
            paired_tick: None,
            pregnancy: None,
            last_birth_tick: None,
            children_born: 0,
            guardian_id: None,
            taught_count: 0,
            shared_count: 0,
            dirty: true,
        };

        perception::seed_local_knowledge(
            &mut c,
            &self.world,
            &self.cache,
            &self.node_index,
            &mut self.scratch,
            &self.cfg,
            self.tick,
        );

        self.events.push(
            Event::new(self.tick, EventKind::Born, id)
                .at(x, y)
                .with("sex", sex.as_str())
                .with_int("age", age),
        );
        self.creatures.push(c);
        self.pending_born.push(id);
        self.total_births += 1;
        id
    }

    // ================================================================== tick

    /// Run one tick: the seven phases of §4.2, in order.
    pub fn step(&mut self) -> TickReport {
        self.tick += 1;
        self.rng = tick_rng(self.seed, self.tick);
        self.events.clear();
        self.decisions.clear();
        self.plan_outcomes.clear();
        self.transmissions.clear();

        let mut report = TickReport { tick: self.tick, ..Default::default() };

        let t = Instant::now();
        self.phase_world(&mut report);
        report.timings.world = t.elapsed().as_micros() as u64;

        let t = Instant::now();
        self.phase_needs(&mut report);
        report.timings.needs = t.elapsed().as_micros() as u64;

        let t = Instant::now();
        self.phase_plans(&mut report);
        report.timings.plans = t.elapsed().as_micros() as u64;

        let t = Instant::now();
        self.phase_deliberate(&mut report);
        report.timings.deliberate = t.elapsed().as_micros() as u64;

        let t = Instant::now();
        self.phase_act(&mut report);
        report.timings.act = t.elapsed().as_micros() as u64;

        let t = Instant::now();
        self.phase_resolve(&mut report);
        report.timings.resolve = t.elapsed().as_micros() as u64;

        self.collapse_routine_events();

        report.population = self.creatures.len() as u32;
        report
    }

    /// Phase 1 — world update: regrowth, crops, sheep, spoilage, fires.
    fn phase_world(&mut self, report: &mut TickReport) {
        economy::regrow(&mut self.world, &self.cfg);
        economy::sheep_tick(&mut self.world, &self.cfg, &mut self.rng);

        let guttered = economy::burn_and_decay(&mut self.structures, &self.cfg, self.tick);
        if guttered > 0 {
            self.events.push(
                Event::new(self.tick, EventKind::FireOut, 0).with_int("count", guttered as i64),
            );
        }

        // Spoilage in inventories. Household stores land with households at M4.
        let mut spoiled_total = 0.0;
        for c in self.creatures.iter_mut() {
            let lost = economy::spoil(&mut c.inventory, self.tick, &self.cfg);
            if lost > 0.0 {
                spoiled_total += lost;
                c.dirty = true;
            }
        }
        if spoiled_total > 0.0 {
            self.events.push(
                Event::new(self.tick, EventKind::Spoiled, 0).with_num("qty", spoiled_total),
            );
        }
        // Household stores rot too — that is the whole point of them holding
        // batches rather than totals. A store of berries is not a reserve.
        for h in self.households.items.iter_mut() {
            if !h.is_alive() {
                continue;
            }
            let lost = economy::spoil(&mut h.store, self.tick, &self.cfg);
            if lost > 0.0 {
                spoiled_total += lost;
                h.dirty = true;
            }
        }
        report.food_spoiled = spoiled_total;

        self.node_index.rebuild(&self.world);
        self.people.rebuild(self.creatures.iter(), self.tick, &self.cfg.knowledge);
    }

    /// Phase 2 — needs decay, and health's response to sustained deficit (§4.5).
    fn phase_needs(&mut self, _report: &mut TickReport) {
        let night = economy::is_night(self.tick, &self.cfg);
        let n = &self.cfg.needs;
        let a = &self.cfg.actions;
        let l = &self.cfg.lifespan;
        let dawn = economy::hour_of(self.tick) == a.night_end_hour;

        for c in self.creatures.iter_mut() {
            c.hunger = (c.hunger - n.hunger_decay_per_tick).max(0.0);
            c.thirst = (c.thirst - n.thirst_decay_per_tick).max(0.0);
            c.fatigue = (c.fatigue - n.fatigue_decay_per_tick).max(0.0);

            // Warmth: a roof, a fire, or the cold (§4.4, §4.5).
            let sheltered = c
                .in_shelter
                .and_then(|id| self.structures.get(id))
                .filter(|s| s.shelters());
            let at_fire = self
                .structures
                .fire_near(c.x, c.y, a.fire_warmth_radius, self.tick)
                .is_some();
            c.at_fire = at_fire;

            if let Some(s) = sheltered {
                c.warmth = (c.warmth + a.shelter_warmth * s.condition).min(100.0);
                c.exposed_ticks = 0;
            } else if at_fire {
                c.warmth = (c.warmth + a.fire_warmth).min(100.0);
                c.exposed_ticks = 0;
            } else if night {
                c.warmth = (c.warmth - n.warmth_decay_night).max(0.0);
                c.exposed_ticks += 1;
            } else {
                // Daylight warms, but by less than the night takes. A creature
                // that never finds a roof or lights a fire cools over days
                // rather than recovering each morning.
                c.warmth = (c.warmth + 1.1).min(100.0);
            }

            // Health integrates sustained deficit; thirst bites hardest.
            let mut erosion = 0.0;
            for (v, w) in [(c.hunger, 1.0), (c.thirst, 1.35), (c.warmth, 1.1)] {
                if v < n.deficit_threshold {
                    erosion += w * ((n.deficit_threshold - v) / n.deficit_threshold);
                }
            }
            if erosion > 0.0 {
                c.health = (c.health - n.health_erosion_per_tick * erosion).max(0.0);
            } else if c.fatigue > n.deficit_threshold {
                c.health = (c.health + n.health_regen_per_tick).min(100.0);
            }

            // Sustained malnutrition accelerates ageing, up to ~2x (§4.6).
            if c.hunger < n.deficit_threshold {
                let depth = (n.deficit_threshold - c.hunger) / n.deficit_threshold;
                c.wear += (l.malnutrition_aging_multiplier - 1.0) * depth;
            }

            c.life_stage = LifeStage::of(c.age(self.tick), l);
            c.dirty = true;
        }

        // At dawn, settle up for the night just past. Each night spent with
        // neither roof nor fire permanently shaves expected lifespan; a run of
        // good ones extends it toward the ceiling.
        if dawn {
            let night_len = (24 + a.night_end_hour - a.night_start_hour) % 24;
            let threshold = (night_len / 2).max(1);
            let mut exposed_events = Vec::new();
            for c in self.creatures.iter_mut() {
                if c.exposed_ticks >= threshold {
                    c.lifespan_ticks =
                        (c.lifespan_ticks - l.unsheltered_night_penalty_ticks).max(24.0);
                    exposed_events.push(
                        Event::new(self.tick, EventKind::ExposedNight, c.id)
                            .at(c.x, c.y)
                            .with_int("ticks", c.exposed_ticks as i64),
                    );
                } else if c.hunger > 60.0 && c.thirst > 60.0 && c.warmth > 60.0 {
                    c.lifespan_ticks = (c.lifespan_ticks + 2.0).min(l.ceiling_ticks as f32);
                }
                c.exposed_ticks = 0;
            }
            self.events.append(&mut exposed_events);
        }

        // Illness: rare, and much less rare when already weak (§4.6).
        let h = &self.cfg.hazards;
        for i in 0..self.creatures.len() {
            let weak = self.creatures[i].health < 45.0;
            let chance =
                h.illness_per_tick * if weak { h.illness_low_health_multiplier } else { 1.0 };
            if self.rng.gen::<f32>() < chance {
                let severity = 14.0 + self.rng.gen::<f32>() * 40.0;
                let c = &mut self.creatures[i];
                c.health = (c.health - severity).max(0.0);
                c.trauma = Some((DeathCause::Illness, self.tick));
                self.events.push(
                    Event::new(self.tick, EventKind::FellIll, c.id)
                        .at(c.x, c.y)
                        .with_num("severity", severity),
                );
            }
        }
    }

    /// Phase 3 — plan expiry and interrupt detection (§5.3, §5.5).
    ///
    /// Soft signals accumulate as pressure and cannot break a commitment; hard
    /// ones abort immediately. Without that distinction the horizon mechanic
    /// does nothing, because every plan would be interrupted by ordinary need
    /// decay within a few ticks.
    fn phase_plans(&mut self, report: &mut TickReport) {
        self.flagged.clear();
        let crit = self.cfg.needs.critical_threshold;

        for i in 0..self.creatures.len() {
            let c = &mut self.creatures[i];

            // *Every* need currently in crisis, not just the first one found.
            //
            // Checking only the deepest livelocks a creature that is critical
            // in two needs at once: on its way to water with its hunger also
            // critical, the hunger cancels the water plan, the policy re-issues
            // the water plan because thirst is more pressing, and the hunger
            // cancels it again — every tick, forever. Measured: plans lasting a
            // mean of 2.2 ticks against a committed 4.5, and three quarters of
            // all decisions coming out as EXPLORE because no plan lived long
            // enough to do anything.
            //
            // A creature acting on any of its crises is doing the right thing.
            let mut crises: [Option<Addresses>; 3] = [None; 3];
            let mut worst: Option<AbortReason> = None;
            if c.thirst < crit {
                crises[0] = Some(Addresses::Water);
                worst = Some(AbortReason::ThirstCritical);
            }
            if c.hunger < crit {
                crises[1] = Some(Addresses::Food);
                worst = worst.or(Some(AbortReason::HungerCritical));
            }
            if c.warmth < crit {
                crises[2] = Some(Addresses::Warmth);
                worst = worst.or(Some(AbortReason::WarmthCritical));
            }
            let hard = worst.map(|r| (r, crises));

            let Some(plan) = c.plan.as_mut() else {
                self.flagged.push(i);
                continue;
            };

            plan.ticks_remaining = plan.ticks_remaining.saturating_sub(1);

            let ended = if let Some((reason, crises)) = hard {
                // A crisis interrupts a plan unless the plan is already the
                // answer to one of them. Without this a creature dying of
                // thirst cancels its walk to the water on every tick of it.
                //
                // A plan also gets a floor of two ticks before any hard signal
                // can touch it, so a creature that has just decided something
                // at least takes a step before reconsidering.
                // Fetching firewood is how a creature answers being cold, so a
                // fuel run counts as a response to a warmth crisis. Without
                // this every wood plan is cancelled by the very cold it was
                // going to fix, and nobody ever accumulates the timber for a
                // shelter — measured at a mean of 0.3 wood carried, population
                // wide, across a whole run.
                let answering = crises.iter().flatten().any(|a| {
                    *a == plan.addresses
                        || (*a == Addresses::Warmth && plan.addresses == Addresses::Fuel)
                });
                let just_set = self.tick - plan.set_tick < 2;
                if answering || just_set {
                    None
                } else {
                    Some(reason)
                }
            } else if plan.is_done() {
                Some(AbortReason::Completed)
            } else if plan.ticks_remaining == 0 {
                Some(AbortReason::HorizonExpired)
            } else {
                None
            };

            if let Some(reason) = ended {
                let actual = (self.tick - plan.set_tick).max(0) as u32;
                let committed = plan.horizon;
                self.plan_outcomes.push(PlanOutcome {
                    creature_id: c.id,
                    set_tick: plan.set_tick,
                    horizon_actual: actual,
                    reason,
                });
                if reason != AbortReason::Completed && reason != AbortReason::HorizonExpired {
                    report.plans_abandoned += 1;
                }
                let _ = committed;
                c.plan = None;
                self.flagged.push(i);
            } else {
                // Soft pressure builds while committed, so a creature that has
                // been running the same plan for a long time is first in line
                // when the horizon does expire (§5.3).
                c.deliberation_pressure += 0.02;
            }
        }
    }

    /// Phase 4 — deliberation.
    ///
    /// At M2 this is Tier 1 for everyone who needs a plan; there is no budget
    /// to spend and no model to call. At M3 the budget scheduler picks the top
    /// N by pressure and everyone else still arrives here, which is why the
    /// shape of this phase does not change.
    fn phase_deliberate(&mut self, report: &mut TickReport) {
        let night = economy::is_night(self.tick, &self.cfg);
        let flagged = std::mem::take(&mut self.flagged);

        for &i in &flagged {
            let plan = {
                let ctx = PolicyCtx {
                    world: &self.world,
                    structures: &self.structures,
                    cache: &self.cache,
                    nodes: &self.node_index,
                    people: &self.people,
                    households: &self.households,
                    courtships: &self.courtships,
                    relationships: &self.relationships,
                    cfg: &self.cfg,
                    tick: self.tick,
                    night,
                };
                policy::decide(&self.creatures[i], &ctx, &mut self.rng)
            };

            let c = &mut self.creatures[i];
            let goal = plan.steps.first().map(|s| s.goal.as_str()).unwrap_or("NONE");

            self.decisions.push(DecisionRecord {
                tick: self.tick,
                creature_id: c.id,
                tier: 1,
                age_ticks: c.age(self.tick),
                life_stage: c.life_stage,
                goal: goal.to_string(),
                addresses: plan.addresses,
                rationale: plan.rationale.clone(),
                horizon_committed: plan.horizon,
                // Not a failure: at M2 there is no model to fall back *from*.
                fallback_reason: Some("LLM_NOT_ENABLED_AT_M2"),
            });
            // No PLAN_SET event: `decisions` already holds the tick, the
            // creature, the goal and the committed horizon. Writing it twice
            // was 338,000 rows per run of pure duplication.

            c.last_deliberation_tick = Some(self.tick);
            c.lifetime_deliberations += 1;
            c.deliberation_pressure = 0.0;
            c.plan = Some(plan);
            c.dirty = true;
            report.deliberations += 1;
        }
        self.flagged = flagged;
    }

    /// Phase 5 — reflex and action execution, then observation.
    ///
    /// Observation runs *after* acting so a creature that arrives somewhere
    /// this tick sees it this tick. That ordering is what makes a stale belief
    /// legible: the creature walks to the clearing it remembers, looks, and its
    /// belief is corrected on the spot — then next tick the gather it planned
    /// fails with `TARGET_DEPLETED` and the plan is abandoned with a reason.
    fn phase_act(&mut self, report: &mut TickReport) {
        let night = economy::is_night(self.tick, &self.cfg);
        let mut gathered = 0.0;
        let mut eaten = 0.0;
        let present = self.creatures.len() as u32;

        for i in 0..self.creatures.len() {
            // The plan is taken out for the duration so the creature and the
            // world it acts on can both be borrowed mutably.
            let Some(mut plan) = self.creatures[i].plan.take() else {
                continue;
            };
            report.acted += 1;

            let mut ctx = ActionCtx {
                world: &mut self.world,
                structures: &mut self.structures,
                pathfinder: &mut self.pathfinder,
                cfg: &self.cfg,
                tick: self.tick,
                rng: &mut self.rng,
                events: &mut self.events,
                night,
                gathered: 0.0,
                eaten: 0.0,
                households: &mut self.households,
                people: &self.people,
                courtships: &self.courtships,
                intents: &mut self.intents,
            };

            let c = &mut self.creatures[i];
            let mut failure = None;

            if let Some(step) = plan.steps.get_mut(plan.step_index) {
                match actions::execute(c, step, &mut ctx) {
                    Outcome::Working => {}
                    Outcome::StepComplete => plan.step_index += 1,
                    Outcome::Failed(reason) => failure = Some(reason),
                }
            } else {
                failure = Some(AbortReason::Completed);
            }

            gathered += ctx.gathered;
            eaten += ctx.eaten;

            match failure {
                Some(reason) if reason != AbortReason::Completed => {
                    let actual = (self.tick - plan.set_tick).max(0) as u32;
                    self.plan_outcomes.push(PlanOutcome {
                        creature_id: c.id,
                        set_tick: plan.set_tick,
                        horizon_actual: actual,
                        reason,
                    });
                    report.plans_abandoned += 1;
                    // No plan next tick means phase 3 flags it and phase 4
                    // gives it a new one, which is a full tick of standing
                    // still — the cost of having been wrong.
                }
                _ => {
                    if !plan.is_done() {
                        self.creatures[i].plan = Some(plan);
                    }
                }
            }

            // Look around from wherever the creature now stands.
            let c = &mut self.creatures[i];
            report.discoveries += perception::observe(
                c,
                &self.world,
                &self.cache,
                &self.node_index,
                &mut self.scratch,
                &self.cfg,
                self.tick,
                &mut self.events,
            );
            // Forgetting is silent: it is a consequence of confidence decay
            // that the belief table already records, and an event per creature
            // per tick for it would dwarf everything worth reading.
            knowledge::forget_expired(&mut c.beliefs, self.tick, &self.cfg.knowledge);
        }

        report.food_gathered = gathered;
        report.food_eaten = eaten;
        debug_assert_eq!(
            report.acted, present,
            "every creature acts every tick (§5.2); {} of {present} did not",
            present - report.acted
        );
    }

    /// Phase 6 — resolution: births, deaths, pairings, and everything else
    /// that takes two creatures to settle.
    ///
    /// Order matters and is deliberate. Social acts land first, because a
    /// creature fed this tick should not starve this tick. Then conception and
    /// birth, then death, then the consequences of death — widowhood,
    /// orphaning, and the inheritance of what a dissolved household held.
    fn phase_resolve(&mut self, report: &mut TickReport) {
        self.apply_social_intents(report);
        self.ambient_observation(report);
        self.expire_courtships();
        self.conceive(report);
        self.give_birth(report);

        let mut dying: Vec<(usize, DeathCause)> = Vec::new();

        for (i, c) in self.creatures.iter().enumerate() {
            if c.biological_age(self.tick) >= c.lifespan_ticks {
                dying.push((i, DeathCause::OldAge));
            } else if c.health <= 0.0 {
                // An accident or illness in the recent past is what killed a
                // creature, not whichever need happened to be lowest when it
                // finally gave out.
                let cause = match c.trauma {
                    Some((cause, at)) if self.tick - at <= 48 => cause,
                    _ => c.worst_need_cause(),
                };
                dying.push((i, cause));
            }
        }

        for &(i, cause) in dying.iter() {
            let c = &mut self.creatures[i];
            c.death_tick = Some(self.tick);
            c.death_cause = Some(cause);
            c.dirty = true;

            self.deaths_by_cause[cause as usize] += 1;
            self.total_deaths += 1;
            report.deaths += 1;

            self.events.push(
                Event::new(self.tick, EventKind::Died, c.id)
                    .at(c.x, c.y)
                    .with("cause", cause.as_str())
                    .with_int("age", c.age(self.tick))
                    .with_int("expected", c.lifespan_ticks as i64),
            );

            if let Some(id) = c.in_shelter.take() {
                if let Some(s) = self.structures.get_mut(id) {
                    s.occupants = s.occupants.saturating_sub(1);
                    s.dirty = true;
                }
            }
        }

        // Dead creatures leave the live set, but their final state is staged
        // for phase 7 first: this is the last chance to record it.
        if !dying.is_empty() {
            let (dead, alive): (Vec<Creature>, Vec<Creature>) = std::mem::take(&mut self.creatures)
                .into_iter()
                .partition(|c| c.death_tick.is_some());
            self.creatures = alive;
            self.pending_dead.extend(dead);
        }

        if !dying.is_empty() {
            self.settle_estates(report);
        }

        // The measurement fixture (see `BenchConfig`): hold the census so the
        // performance and cause-of-death criteria can be measured at the stated
        // population before reproduction exists at M4.
        if let Some(target) = self.cfg.bench.maintain_population {
            self.population_maintained = true;
            let (hx, hy) = self
                .world
                .founders
                .first()
                .map(|f| (f.x, f.y))
                .unwrap_or((self.world.width / 2, self.world.height / 2));
            while (self.creatures.len() as u32) < target {
                let (x, y) = self.scatter_near(hx, hy, 34);
                let sex = if self.rng.gen::<bool>() { Sex::Female } else { Sex::Male };
                let age = self.cfg.lifespan.infant_until_tick as i64
                    + self.rng.gen_range(0..40);
                let id = self.spawn_at(x, y, sex, age, 1);
                report.settlers += 1;
                if let Some(last) = self.events.last_mut() {
                    if last.actor_id == Some(id) {
                        last.kind = EventKind::Settled;
                    }
                }
            }
        }
    }

    fn index_of(&self, id: i64) -> Option<usize> {
        self.creatures.binary_search_by_key(&id, |c| c.id).ok()
    }

    /// Apply every two-sided act recorded during phase 5.
    fn apply_social_intents(&mut self, report: &mut TickReport) {
        let intents = std::mem::take(&mut self.intents);
        for intent in &intents {
            match *intent {
                SocialIntent::Court { from, to } => {
                    self.courtships.offer(from, to, self.tick);
                    // Asking is itself a small social act: it registers.
                    self.relationships.adjust(to, from, 0.05, None, self.tick);
                }

                SocialIntent::Accept { from, to } => self.pair_up(from, to, report),
                SocialIntent::Reject { from, to } => {
                    self.courtships.remove_between(from, to);
                    // Rejection is recorded (§4.8) and it costs something on
                    // both sides — which is what makes courting a risk.
                    self.relationships.adjust(from, to, -0.3, None, self.tick);
                    self.relationships.adjust(to, from, -0.1, None, self.tick);
                    report.rejections += 1;
                    self.events.push(
                        Event::new(self.tick, EventKind::Rejected, from).target(to),
                    );
                }

                SocialIntent::GiveFood { from, to, quantity } => {
                    self.transfer_food(from, to, quantity, false, report);
                }
                SocialIntent::FeedInfant { from, to, quantity } => {
                    self.transfer_food(from, to, quantity, true, report);
                }

                SocialIntent::JoinHousehold { creature, household } => {
                    self.join_household(creature, household);
                }
                SocialIntent::LeaveHousehold { creature } => {
                    if let Some(i) = self.index_of(creature) {
                        if let Some(old) = self.creatures[i].household_id.take() {
                            self.creatures[i].dirty = true;
                            self.events.push(
                                Event::new(self.tick, EventKind::HouseholdLeft, creature)
                                    .target(old),
                            );
                        }
                    }
                }

                SocialIntent::Share { from, to, topic } => {
                    let n = self.transfer_beliefs(from, to, Channel::Share, topic);
                    if n > 0 {
                        self.transmissions.push(TransmissionRecord {
                            tick: self.tick, from, to, channel: Channel::Share, count: n as u32,
                        });
                        report.beliefs_shared += n as u32;
                        if let Some(i) = self.index_of(from) {
                            self.creatures[i].shared_count += 1;
                        }
                        // Being told something you needed is a kindness.
                        self.relationships.adjust(to, from, 0.08, None, self.tick);
                        self.events.push(
                            Event::new(self.tick, EventKind::Shared, from)
                                .target(to)
                                .with_int("beliefs", n as i64),
                        );
                    }
                }

                SocialIntent::Teach { from, to } => {
                    let n = self.transfer_beliefs(from, to, Channel::Teach, None);
                    if n > 0 {
                        self.transmissions.push(TransmissionRecord {
                            tick: self.tick, from, to, channel: Channel::Teach, count: n as u32,
                        });
                        report.beliefs_taught += n as u32;
                        if let Some(i) = self.index_of(from) {
                            self.creatures[i].taught_count += 1;
                        }
                        self.relationships.adjust_both(from, to, 0.15, None, self.tick);
                        self.events.push(
                            Event::new(self.tick, EventKind::Taught, from)
                                .target(to)
                                .with_int("beliefs", n as i64),
                        );
                    }
                }
            }
        }
        self.intents = intents;
        self.intents.clear();
    }

    /// Two creatures accept each other. §4.8's mutual pairing.
    fn pair_up(&mut self, a: i64, b: i64, report: &mut TickReport) {
        let (Some(ia), Some(ib)) = (self.index_of(a), self.index_of(b)) else {
            return;
        };
        if self.creatures[ia].mate_id.is_some() || self.creatures[ib].mate_id.is_some() {
            return; // somebody was quicker
        }

        self.creatures[ia].mate_id = Some(b);
        self.creatures[ia].paired_tick = Some(self.tick);
        self.creatures[ia].dirty = true;
        self.creatures[ib].mate_id = Some(a);
        self.creatures[ib].paired_tick = Some(self.tick);
        self.creatures[ib].dirty = true;

        self.courtships.remove_all_for(a);
        self.courtships.remove_all_for(b);
        self.relationships.adjust_both(a, b, 0.6, Some(RelKind::Mate), self.tick);

        // A couple shares one household. If either already has one the other
        // joins it; if neither does they will have to build.
        let ha = self.creatures[ia].household_id;
        let hb = self.creatures[ib].household_id;
        match (ha, hb) {
            (Some(h), None) => self.join_household(b, h),
            (None, Some(h)) => self.join_household(a, h),
            _ => {}
        }

        report.pairings += 1;
        let (x, y) = (self.creatures[ia].x, self.creatures[ia].y);
        self.events.push(Event::new(self.tick, EventKind::Paired, a).at(x, y).target(b));
    }

    fn join_household(&mut self, creature: i64, household: i64) {
        let Some(i) = self.index_of(creature) else { return };
        if self.creatures[i].household_id == Some(household) {
            return;
        }
        let members = self.household_size(household);
        let Some(h) = self.households.get(household) else { return };
        if members >= h.size_cap {
            return;
        }
        self.creatures[i].household_id = Some(household);
        self.creatures[i].dirty = true;
        self.events.push(
            Event::new(self.tick, EventKind::HouseholdJoined, creature).target(household),
        );
    }

    fn household_size(&self, id: i64) -> u32 {
        self.creatures.iter().filter(|c| c.household_id == Some(id)).count() as u32
    }

    /// Move food between two creatures. Done here rather than in the action so
    /// nothing is ever taken from a giver whose recipient has since died.
    fn transfer_food(
        &mut self,
        from: i64,
        to: i64,
        quantity: f32,
        to_infant: bool,
        report: &mut TickReport,
    ) {
        let (Some(fi), Some(ti)) = (self.index_of(from), self.index_of(to)) else {
            return;
        };
        // Oldest first, so what changes hands is what was about to spoil —
        // which is exactly the surplus §4.4 says creates sharing pressure.
        let Some(oldest) = self.creatures[fi].inventory.oldest_food().copied() else {
            return;
        };
        let want = quantity.min(oldest.quantity);
        let got = self.creatures[fi].inventory.take(oldest.kind, want);
        if got <= 0.0 {
            return;
        }
        self.creatures[fi].dirty = true;

        if to_infant {
            // An infant cannot carry a pack around; it is fed directly.
            self.creatures[ti].hunger =
                (self.creatures[ti].hunger + got * oldest.kind.nutrition()).min(100.0);
        } else {
            self.creatures[ti].inventory.add(oldest.kind, got, oldest.harvested_tick);
        }
        self.creatures[ti].dirty = true;

        self.relationships.adjust(to, from, 0.2, None, self.tick);
        self.relationships.adjust(from, to, 0.05, None, self.tick);

        let (x, y) = (self.creatures[fi].x, self.creatures[fi].y);
        self.events.push(
            Event::new(
                self.tick,
                if to_infant { EventKind::FedInfant } else { EventKind::GaveFood },
                from,
            )
            .at(x, y)
            .target(to)
            .with("kind", oldest.kind.as_str())
            .with_num("qty", got),
        );
        let _ = report;
    }

    /// Copy beliefs from one creature to another over the given channel.
    ///
    /// Reads and writes are separate indexing operations rather than a
    /// simultaneous borrow of two creatures, which is why this lives on `Sim`
    /// and not in the action.
    fn transfer_beliefs(
        &mut self,
        from: i64,
        to: i64,
        channel: Channel,
        topic: Option<crate::sim::knowledge::BeliefKind>,
    ) -> usize {
        let (Some(fi), Some(ti)) = (self.index_of(from), self.index_of(to)) else {
            return 0;
        };
        let k = &self.cfg.knowledge;
        let n = match channel {
            Channel::Teach => k.teach_belief_count,
            Channel::Share => k.share_belief_count,
            Channel::Observation => 1,
        } as usize;

        let (x, y) = (self.creatures[fi].x, self.creatures[fi].y);
        let picked = knowledge::select_for_sharing(
            &self.creatures[fi].beliefs, topic, (x, y), self.tick, k, n,
        );
        if picked.is_empty() {
            return 0;
        }

        let cap = k.max_beliefs_held.max(8) as usize;
        let mut moved = 0;
        for i in picked {
            let incoming = knowledge::transmit(
                &self.creatures[fi].beliefs[i], from, channel, k, self.tick,
            );
            if incoming.confidence <= 0.02 {
                continue;
            }
            let (tx, ty) = (self.creatures[ti].x, self.creatures[ti].y);
            knowledge::upsert(
                &mut self.creatures[ti].beliefs, incoming, (tx, ty), k, cap, self.tick,
            );
            moved += 1;
        }
        if moved > 0 {
            self.creatures[ti].dirty = true;
        }
        moved
    }

    /// Channel 1 of §4.11: being near somebody leaks a little of what they
    /// know. No decision, no cost, always on — an ambient information hum that
    /// gives the world a floor of shared knowledge without anybody choosing to
    /// communicate.
    fn ambient_observation(&mut self, report: &mut TickReport) {
        if !self.cfg.features.knowledge_sharing {
            return;
        }
        let chance = self.cfg.knowledge.ambient_share_chance;
        if chance <= 0.0 {
            return;
        }
        let radius = self.cfg.knowledge.observation_radius.min(4);

        let mut pairs: Vec<(i64, i64)> = Vec::new();
        for c in &self.creatures {
            if self.rng.gen::<f32>() >= chance {
                continue;
            }
            self.people.near(c.x, c.y, radius, c.id, &mut self.bystanders);
            if self.bystanders.is_empty() {
                continue;
            }
            // One neighbour, chosen from the deterministic ascending-id list.
            let pick = self.rng.gen_range(0..self.bystanders.len());
            pairs.push((self.bystanders[pick].id, c.id));
        }

        for (from, to) in pairs {
            let n = self.transfer_beliefs(from, to, Channel::Observation, None);
            if n > 0 {
                report.beliefs_overheard += n as u32;
            }
        }
    }

    fn expire_courtships(&mut self) {
        let ttl = self.cfg.reproduction.courtship_offer_ticks;
        for lapsed in self.courtships.expire(self.tick, ttl) {
            // Being ignored is not the same as being turned down, but it is not
            // nothing either.
            self.relationships.adjust(lapsed.from, lapsed.to, -0.05, None, self.tick);
        }
    }

    /// The four requirements of §4.8, checked for every paired female.
    fn conceive(&mut self, report: &mut TickReport) {
        let candidates: Vec<(usize, usize, i64)> = self
            .creatures
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                c.sex == Sex::Female && c.pregnancy.is_none() && c.mate_id.is_some()
            })
            .filter_map(|(i, c)| {
                let mate = c.mate_id?;
                let j = self.index_of(mate)?;
                Some((i, j, c.household_id?))
            })
            .collect();

        for (mi, fi, hid) in candidates {
            let household = self.households.get(hid);
            if let Some(blocker) = social::conception_blocker(
                &self.creatures[mi], &self.creatures[fi], household, &self.cfg, self.tick,
            ) {
                report.conception_blocked[blocker as usize] += 1;
                continue;
            }

            let due = self.tick + self.cfg.reproduction.gestation_ticks as i64;
            let father_id = self.creatures[fi].id;
            self.creatures[mi].pregnancy =
                Some(crate::sim::creature::Pregnancy { father_id, due_tick: due });
            self.creatures[mi].dirty = true;
            report.conceptions += 1;

            let (x, y) = (self.creatures[mi].x, self.creatures[mi].y);
            self.events.push(
                Event::new(self.tick, EventKind::Conceived, self.creatures[mi].id)
                    .at(x, y)
                    .target(father_id)
                    .with_int("due", due),
            );
        }
    }

    /// Gestation ends. An infant is born with inherited traits and a small
    /// mutation, and childbirth carries a real risk to the mother — which is
    /// what makes reproduction a gamble rather than a free action (§4.8).
    fn give_birth(&mut self, report: &mut TickReport) {
        let due: Vec<usize> = self
            .creatures
            .iter()
            .enumerate()
            .filter(|(_, c)| c.pregnancy.is_some_and(|p| p.due_tick <= self.tick))
            .map(|(i, _)| i)
            .collect();

        for mi in due {
            let Some(p) = self.creatures[mi].pregnancy else { continue };
            let mother_id = self.creatures[mi].id;
            let (x, y) = (self.creatures[mi].x, self.creatures[mi].y);
            let household = self.creatures[mi].household_id;
            let generation = self.creatures[mi].generation;
            let mother_traits = self.creatures[mi].traits;

            self.creatures[mi].pregnancy = None;
            self.creatures[mi].last_birth_tick = Some(self.tick);
            self.creatures[mi].dirty = true;

            let father_traits = self
                .index_of(p.father_id)
                .map(|fi| self.creatures[fi].traits)
                .unwrap_or(mother_traits);

            // A birth draws on the household store. Feeding a non-productive
            // mouth for a week is the cost §4.7 calls deliberately harsh, and
            // it starts on day one.
            if let Some(h) = household.and_then(|id| self.households.get_mut(id)) {
                let want = self.cfg.reproduction.birth_store_cost;
                for kind in [ItemKind::Grain, ItemKind::Meat, ItemKind::Forage] {
                    if h.store.take(kind, want) > 0.0 {
                        break;
                    }
                }
                h.dirty = true;
            }

            let sigma = self.cfg.reproduction.mutation_sigma;
            let traits = Traits {
                boldness: social::inherit(mother_traits.boldness, father_traits.boldness, sigma, &mut self.rng),
                industry: social::inherit(mother_traits.industry, father_traits.industry, sigma, &mut self.rng),
                sociability: social::inherit(mother_traits.sociability, father_traits.sociability, sigma, &mut self.rng),
                caution: social::inherit(mother_traits.caution, father_traits.caution, sigma, &mut self.rng),
            };

            let sex = self.coin_flip_sex();
            let child = self.spawn_at(x, y, sex, 0, generation + 1);
            if let Some(ci) = self.index_of(child) {
                self.creatures[ci].mother_id = Some(mother_id);
                self.creatures[ci].father_id = Some(p.father_id);
                self.creatures[ci].household_id = household;
                self.creatures[ci].guardian_id = Some(mother_id);
                self.creatures[ci].traits = traits;
                // A newborn has seen nothing. Whatever it comes to know, it
                // will be taught or it will find out — which is the whole
                // question the culture layer exists to ask.
                self.creatures[ci].beliefs.clear();
            }
            self.creatures[mi].children_born += 1;

            self.relationships.adjust_both(mother_id, child, 0.8, Some(RelKind::Kin), self.tick);
            self.relationships.adjust_both(p.father_id, child, 0.7, Some(RelKind::Kin), self.tick);
            report.births += 1;

            // Childbirth mortality. Applied after the child exists, so a
            // mother who does not survive still leaves one behind.
            if self.rng.gen::<f32>() < self.cfg.reproduction.childbirth_mortality {
                self.creatures[mi].health = 0.0;
                self.creatures[mi].trauma = Some((DeathCause::Childbirth, self.tick));
            }
        }
    }

    fn coin_flip_sex(&mut self) -> Sex {
        if self.rng.gen::<bool>() { Sex::Female } else { Sex::Male }
    }

    /// The consequences of death: widowhood, orphaning, and what a household
    /// leaves behind when its last member is gone.
    fn settle_estates(&mut self, report: &mut TickReport) {
        let dead: Vec<i64> = self.pending_dead.iter().map(|c| c.id).collect();

        for id in &dead {
            self.courtships.remove_all_for(*id);
            for i in 0..self.creatures.len() {
                if self.creatures[i].mate_id == Some(*id) {
                    self.creatures[i].mate_id = None;
                    self.creatures[i].dirty = true;
                }
                if self.creatures[i].guardian_id == Some(*id) {
                    // An orphan needs somebody, or the dependency window kills
                    // it. The household is the natural place to look — which is
                    // exactly what a household is for.
                    let hid = self.creatures[i].household_id;
                    let heir = hid.and_then(|h| {
                        self.creatures
                            .iter()
                            .filter(|o| {
                                o.household_id == Some(h)
                                    && o.life_stage != LifeStage::Infant
                                    && o.id != self.creatures[i].id
                            })
                            .map(|o| o.id)
                            .min()
                    });
                    self.creatures[i].guardian_id = heir;
                    self.creatures[i].dirty = true;
                    let orphan = self.creatures[i].id;
                    self.events.push(
                        Event::new(self.tick, EventKind::Orphaned, orphan)
                            .with_int("guardian", heir.unwrap_or(0)),
                    );
                }
            }
        }

        // Forget the dead. Without this the relationship set only ever grows:
        // every creature that ever stood near another leaves an edge behind,
        // and nobody stops being remembered. It was reaching 7,000+ edges in a
        // 2,000-tick run, and the checkpoint that rewrites them wholesale was
        // the tick-time spike that pushed p99 over the Fast-Forward budget.
        {
            let living: std::collections::BTreeSet<i64> =
                self.creatures.iter().map(|c| c.id).collect();
            self.relationships.forget_dead(&|id| living.contains(&id));
        }

        // Households nobody belongs to any more hand on what they held.
        let mut counts: std::collections::BTreeMap<i64, u32> = Default::default();
        for c in &self.creatures {
            if let Some(h) = c.household_id {
                *counts.entry(h).or_default() += 1;
            }
        }
        for h in self.households.items.iter() {
            counts.entry(h.id).or_insert(0);
        }

        for (hid, estate) in self.households.reap(&counts, self.tick) {
            self.events.push(
                Event::new(self.tick, EventKind::HouseholdDissolved, 0).target(hid),
            );
            // Inheritance: the store passes to the household of a child of the
            // founders, if one survives. This is what lets a lineage compound
            // rather than starting from nothing every generation.
            let founders = self
                .households
                .items
                .iter()
                .find(|h| h.id == hid)
                .map(|h| h.founder_ids);
            let heir_household = founders.and_then(|(a, b)| {
                self.creatures
                    .iter()
                    .filter(|c| {
                        let parent = |p: Option<i64>| p == Some(a) || (b.is_some() && p == b);
                        parent(c.mother_id) || parent(c.father_id)
                    })
                    .filter_map(|c| c.household_id)
                    .filter(|h| *h != hid)
                    .min()
            });

            if let Some(target) = heir_household {
                if let Some(h) = self.households.get_mut(target) {
                    for b in &estate.batches {
                        h.store.add(b.kind, b.quantity, b.harvested_tick);
                    }
                    h.dirty = true;
                    self.events.push(
                        Event::new(self.tick, EventKind::Inherited, 0)
                            .target(target)
                            .with_num("qty", estate.weight()),
                    );
                }
            }
        }
        let _ = report;
    }

    /// Fold the routine into counts.
    ///
    /// Drinking, eating, resting, taking shelter and noticing a bush are things
    /// that happen to most creatures most ticks. Written one row each they came
    /// to 177 events a tick — 354,000 rows in a 2,000-tick run, and phase 7
    /// grew to 99% of the tick, with a p99 of six seconds. That is a
    /// per-creature-per-tick table wearing a different hat, which invariant 5
    /// exists to forbid.
    ///
    /// Nothing that answers a question in §10 is lost. Production and
    /// consumption survive as their own rows (a completed gather, a harvest, a
    /// slaughter). Belief provenance was never in the discovery event — it
    /// lives on the belief, in `origin_creature_id`, which is the column S7 is
    /// answered from. What collapses is the routine, into one summary row per
    /// tick carrying the counts.
    fn collapse_routine_events(&mut self) {
        use EventKind as K;
        const ROUTINE: [EventKind; 5] =
            [K::Discovered, K::Drank, K::Ate, K::Rested, K::Sheltered];

        let mut counts = [0u32; 5];
        let mut exposed = 0u32;
        self.events.retain(|e| {
            if let Some(i) = ROUTINE.iter().position(|k| *k == e.kind) {
                counts[i] += 1;
                return false;
            }
            if e.kind == K::ExposedNight {
                exposed += 1;
                return false;
            }
            true
        });

        if counts.iter().any(|c| *c > 0) {
            let mut ev = Event::new(self.tick, K::Discovered, 0);
            for (i, kind) in ROUTINE.iter().enumerate() {
                ev = ev.with_int(&kind.as_str().to_lowercase(), counts[i] as i64);
            }
            self.events.push(ev);
        }
        if exposed > 0 {
            // The per-creature consequence is already recorded where it
            // matters: on the creature's own expected lifespan.
            self.events.push(
                Event::new(self.tick, K::ExposedNight, 0).with_int("creatures", exposed as i64),
            );
        }
    }

    /// Phase 7 — persist.
    ///
    /// **One transaction per tick** (PRD §3.1, BUILD.md §5.1). Per-entity writes
    /// would dominate the runtime at 500 creatures across thousands of ticks,
    /// so everything this tick produced goes in together.
    ///
    /// What goes in every tick is small and append-only: this tick's events,
    /// this tick's decisions, the backfill of any plan that ended, and one row
    /// of `tick_stats` carrying the phase timings. What does *not* go in every
    /// tick is creature state — needs change on every creature every tick, so
    /// writing them would be invariant 5 by another name. Those are
    /// checkpointed on an interval, and always on death, on pause and at
    /// shutdown, so nothing that matters is ever only in memory.
    pub fn persist(
        &mut self,
        conn: &mut rusqlite::Connection,
        report: &mut TickReport,
        force_checkpoint: bool,
    ) -> anyhow::Result<()> {
        let t = Instant::now();
        let p = &self.cfg.persistence;
        let checkpoint = force_checkpoint
            || p.checkpoint_interval_ticks == 0
            || self.tick % p.checkpoint_interval_ticks as i64 == 0;
        // Offset from the checkpoint so the two never land on the same tick:
        // 500 creature upserts and 500 sample rows together are one spike
        // instead of two smaller ones.
        let sample = p.sample_interval_ticks > 0
            && self.tick % p.sample_interval_ticks as i64 == (p.sample_interval_ticks / 2) as i64;
        // Beliefs are flushed on a rolling basis: a slice of the population
        // each tick rather than all of it at once. Writing ~19,500 belief rows
        // in one go was a 164ms tick, and beliefs live in RAM during a run
        // (§7) — the table is the reporting and resume layer, so it only has
        // to be exactly current when a run is actually saved, which is what
        // `force_checkpoint` is for.
        const BELIEF_STRIDE: i64 = 120;

        let tx = conn.transaction()?;

        crate::db::repo::insert_events(&tx, self.world_id, &self.events)?;
        crate::db::repo::insert_decisions(&tx, self.world_id, &self.decisions)?;
        crate::db::repo::backfill_plan_outcomes(&tx, self.world_id, &self.plan_outcomes)?;
        crate::db::repo::insert_tick_stats(&tx, self.world_id, report)?;
        crate::db::repo::insert_transmissions(&tx, self.world_id, &self.transmissions)?;

        // Households first, every tick they change. `creatures.household_id` is
        // a foreign key into them, so a creature that founded a home this tick
        // has nothing to point at until its household has a row.
        let dirty_households: Vec<_> = self
            .households
            .items
            .iter()
            .filter(|h| h.dirty)
            .cloned()
            .collect();
        if !dirty_households.is_empty() {
            crate::db::repo::upsert_households(&tx, self.world_id, &dirty_households)?;
            for h in self.households.items.iter_mut() {
                h.dirty = false;
            }
        }

        // The newly born, always: they need a row before anything can
        // reference them, and they are only a handful a tick.
        if !self.pending_born.is_empty() {
            let ids = std::mem::take(&mut self.pending_born);
            crate::db::repo::upsert_creatures(
                &tx,
                self.world_id,
                self.creatures.iter().filter(|c| ids.contains(&c.id)),
            )?;
        }

        // The dead, always: there is no later checkpoint for them.
        if !self.pending_dead.is_empty() {
            crate::db::repo::upsert_creatures(&tx, self.world_id, &self.pending_dead)?;
            crate::db::repo::flush_beliefs(&tx, self.world_id, &self.pending_dead)?;
        }

        if checkpoint {
            crate::db::repo::upsert_creatures(&tx, self.world_id, &self.creatures)?;
            crate::db::repo::upsert_structures(&tx, self.world_id, &self.structures.items)?;
            crate::db::repo::prune_structures(&tx, self.world_id, &self.structures.items)?;
            tx.execute(
                "UPDATE worlds SET current_tick = ?2 WHERE id = ?1",
                rusqlite::params![self.world_id, self.tick],
            )?;
        }
        if sample {
            crate::db::repo::insert_creature_samples(
                &tx, self.world_id, self.tick, &self.creatures,
            )?;
        }
        if force_checkpoint {
            crate::db::repo::flush_beliefs(&tx, self.world_id, &self.creatures)?;
        } else {
            let slot = self.tick.rem_euclid(BELIEF_STRIDE);
            crate::db::repo::flush_beliefs(
                &tx,
                self.world_id,
                self.creatures.iter().filter(|c| c.id.rem_euclid(BELIEF_STRIDE) == slot),
            )?;
        }
        if checkpoint || force_checkpoint {
            // Resource stock is rewritten wholesale, so it rides the slow
            // cadence: 600 rows deleted and reinserted every 24 ticks was pure
            // spike for state that only has to be right when a run is resumed.
            crate::db::repo::save_resource_nodes(&tx, self.world_id, &self.world)?;
            crate::db::repo::save_relationships(&tx, self.world_id, &self.relationships)?;
            crate::db::repo::save_courtships(&tx, self.world_id, &self.courtships)?;
            crate::db::repo::upsert_households(&tx, self.world_id, &self.households.items)?;
        }

        tx.commit()?;
        self.pending_dead.clear();

        report.timings.persist = t.elapsed().as_micros() as u64;
        Ok(())
    }

    /// Restore a run from the database. Used on resume and by the schema
    /// round-trip test, which requires a resumed run to match an uninterrupted
    /// one tick for tick (BUILD.md §9).
    pub fn load_from(
        &mut self,
        conn: &rusqlite::Connection,
        tick: i64,
    ) -> anyhow::Result<()> {
        // Resource stock first: the world a run resumes into is the one it
        // left, not the one worldgen produced. Without this a reload silently
        // restores every stripped patch and every crop to its starting value —
        // caught by the round-trip test, which is exactly what it is for.
        self.world.nodes = crate::db::repo::load_resource_nodes(conn, self.world_id)?;

        let mut creatures = crate::db::repo::load_living_creatures(conn, self.world_id)?;
        crate::db::repo::load_beliefs_into(conn, self.world_id, &mut creatures)?;
        self.creatures = creatures;
        self.structures = crate::db::repo::load_structures(conn, self.world_id)?;
        self.households = crate::db::repo::load_households(conn, self.world_id)?;
        self.relationships = crate::db::repo::load_relationships(conn, self.world_id)?;
        self.courtships = crate::db::repo::load_courtships(conn, self.world_id)?;
        self.next_creature_id = crate::db::repo::next_creature_id(conn, self.world_id)?;

        // Occupancy is derived rather than stored: it is a fact about who is
        // where, and keeping a second copy of it would be a second thing to get
        // out of sync.
        for c in &self.creatures {
            if let Some(id) = c.in_shelter {
                if let Some(s) = self.structures.get_mut(id) {
                    s.occupants += 1;
                }
            }
        }

        let (tallies, born, died) =
            crate::db::repo::death_tallies(conn, self.world_id)?;
        self.deaths_by_cause = tallies;
        self.total_births = born;
        self.total_deaths = died;

        self.tick = tick;
        self.rng = tick_rng(self.seed, tick);
        self.node_index.rebuild(&self.world);
        self.people.rebuild(self.creatures.iter(), tick, &self.cfg.knowledge);
        Ok(())
    }

    /// A stable digest of everything the simulation has produced. Used by the
    /// golden-run test: two runs from the same seed must agree exactly.
    pub fn state_digest(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.tick.hash(&mut h);
        self.creatures.len().hash(&mut h);
        for c in &self.creatures {
            c.id.hash(&mut h);
            c.x.hash(&mut h);
            c.y.hash(&mut h);
            c.hunger.to_bits().hash(&mut h);
            c.thirst.to_bits().hash(&mut h);
            c.fatigue.to_bits().hash(&mut h);
            c.warmth.to_bits().hash(&mut h);
            c.health.to_bits().hash(&mut h);
            c.beliefs.len().hash(&mut h);
            c.inventory.weight().to_bits().hash(&mut h);
        }
        for c in &self.creatures {
            c.household_id.hash(&mut h);
            c.mate_id.hash(&mut h);
            c.generation.hash(&mut h);
            c.pregnancy.map(|p| (p.father_id, p.due_tick)).hash(&mut h);
        }
        for n in &self.world.nodes {
            n.x.hash(&mut h);
            n.y.hash(&mut h);
            n.quantity.to_bits().hash(&mut h);
        }
        for hh in &self.households.items {
            hh.id.hash(&mut h);
            hh.dissolved_tick.hash(&mut h);
            hh.store.weight().to_bits().hash(&mut h);
        }
        for ((a, b), e) in self.relationships.iter() {
            a.hash(&mut h);
            b.hash(&mut h);
            e.affinity.to_bits().hash(&mut h);
        }
        self.deaths_by_cause.hash(&mut h);
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::worldgen;

    fn small_cfg() -> WorldConfig {
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        cfg
    }

    fn sim_with(seed: u64, creatures: u32, cfg: WorldConfig) -> Sim {
        let world = worldgen::generate(seed, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, seed);
        sim.spawn_population(creatures);
        sim
    }

    #[test]
    fn founders_become_creatures() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let founders = world.founders.len();
        let mut sim = Sim::new(1, world, cfg, 44127);

        sim.spawn_founders();

        assert_eq!(sim.alive(), founders, "every founder becomes a row in creatures");
        assert!(sim.creatures.iter().all(|c| c.generation == 1));
        assert!(
            sim.creatures.iter().any(|c| c.sex == Sex::Female)
                && sim.creatures.iter().any(|c| c.sex == Sex::Male),
            "mixed sex, as §8.6 requires"
        );
    }

    #[test]
    fn a_founder_starts_life_knowing_something() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_founders();
        assert!(
            sim.creatures.iter().all(|c| !c.beliefs.is_empty()),
            "an adult who lives here is not born blind"
        );
    }

    #[test]
    fn a_tick_advances_the_clock_and_reports_its_phases() {
        let mut sim = sim_with(44127, 20, small_cfg());
        let r = sim.step();

        assert_eq!(r.tick, 1);
        assert_eq!(sim.tick, 1);
        assert_eq!(r.population, 20);
        assert!(r.timings.total() > 0, "phase timings are recorded from the first tick");
    }

    #[test]
    fn needs_decay_every_tick() {
        let mut sim = sim_with(44127, 8, small_cfg());
        let before: Vec<f32> = sim.creatures.iter().map(|c| c.thirst).collect();
        sim.step();
        let after: Vec<f32> = sim.creatures.iter().map(|c| c.thirst).collect();
        assert!(
            before.iter().zip(&after).all(|(b, a)| a < b || *a >= 99.9),
            "thirst must fall unless the creature drank"
        );
    }

    #[test]
    fn no_creature_is_ever_left_without_a_decision() {
        // §5.2: Tier 1 guarantees nobody stalls. A plan that ends in phase 5 is
        // detected in phase 3 and replaced in phase 4 of the *next* tick, both
        // of which run before phase 5 — so a creature never misses a turn, and
        // ending a tick planless is the normal, costless state between plans.
        let mut sim = sim_with(44127, 40, small_cfg());
        for _ in 0..60 {
            let before = sim.alive() as u32;
            let r = sim.step();
            assert_eq!(r.acted, before, "every creature acted at tick {}", r.tick);
        }
    }

    #[test]
    fn creatures_act_on_the_world_rather_than_standing_still() {
        let mut sim = sim_with(44127, 60, small_cfg());
        let start: Vec<(u32, u32)> = sim.creatures.iter().map(|c| (c.x, c.y)).collect();
        for _ in 0..30 {
            sim.step();
        }
        let moved = sim
            .creatures
            .iter()
            .zip(&start)
            .filter(|(c, s)| (c.x, c.y) != **s)
            .count();
        assert!(moved > sim.alive() / 2, "only {moved} of {} moved", sim.alive());
    }

    #[test]
    fn a_run_produces_deaths_with_recorded_causes() {
        let mut cfg = small_cfg();
        cfg.bench.maintain_population = Some(60);
        let mut sim = sim_with(44127, 60, cfg);
        for _ in 0..900 {
            sim.step();
        }

        let total: u32 = sim.deaths_by_cause.iter().sum();
        assert!(total > 0, "nobody died in 900 ticks, which is not a simulation of life");
        assert_eq!(total as u64, sim.total_deaths);
        assert!(
            sim.deaths_by_cause[DeathCause::OldAge as usize] > 0,
            "with the population held steady, some creatures should reach old age"
        );
    }

    #[test]
    fn the_same_seed_produces_a_byte_identical_run() {
        // The golden-run property in miniature — the full 2,000-tick version is
        // in tests/golden_run.rs. This is the fast one that runs every build.
        let run = |seed: u64| {
            let mut sim = sim_with(seed, 40, small_cfg());
            let mut log = String::new();
            for _ in 0..120 {
                sim.step();
                for e in &sim.events {
                    log.push_str(&e.digest_line());
                    log.push('\n');
                }
            }
            (log, sim.state_digest())
        };

        let (log_a, digest_a) = run(44127);
        let (log_b, digest_b) = run(44127);

        assert_eq!(log_a, log_b, "the event log must be byte-identical");
        assert_eq!(digest_a, digest_b);
        assert!(!log_a.is_empty(), "a run that logs nothing proves nothing");
    }

    #[test]
    fn different_seeds_produce_different_runs() {
        let run = |seed: u64| {
            let mut sim = sim_with(seed, 40, small_cfg());
            for _ in 0..120 {
                sim.step();
            }
            sim.state_digest()
        };
        assert_ne!(run(44127), run(9001));
    }

    #[test]
    fn a_creature_whose_needs_are_all_met_dies_only_of_old_age() {
        // The §9 property test. Needs are pinned full every tick, so nothing
        // but the clock can end this creature.
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_at(20, 20, Sex::Female, 400, 1);
        sim.cfg.hazards.accident_per_tick = 0.0;
        sim.cfg.hazards.illness_per_tick = 0.0;

        for _ in 0..900 {
            for c in sim.creatures.iter_mut() {
                c.hunger = 100.0;
                c.thirst = 100.0;
                c.fatigue = 100.0;
                c.warmth = 100.0;
                c.health = 100.0;
            }
            sim.step();
            if sim.alive() == 0 {
                break;
            }
        }

        assert_eq!(sim.alive(), 0, "it should still die eventually");
        assert_eq!(sim.deaths_by_cause[DeathCause::OldAge as usize], 1);
        for (i, n) in sim.deaths_by_cause.iter().enumerate() {
            if i != DeathCause::OldAge as usize {
                assert_eq!(*n, 0, "cause {i} should not occur when every need is met");
            }
        }
    }

    #[test]
    fn thirst_kills_a_creature_kept_from_water() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_at(20, 20, Sex::Female, 200, 1);

        for _ in 0..600 {
            for c in sim.creatures.iter_mut() {
                c.thirst = 0.0; // never allowed to drink
                c.hunger = 100.0;
                c.warmth = 100.0;
            }
            sim.step();
            if sim.alive() == 0 {
                break;
            }
        }
        assert_eq!(sim.deaths_by_cause[DeathCause::Dehydration as usize], 1);
    }

    #[test]
    fn beliefs_accumulate_as_creatures_walk_around() {
        let mut sim = sim_with(44127, 30, small_cfg());
        let before: usize = sim.creatures.iter().map(|c| c.beliefs.len()).sum();
        for _ in 0..120 {
            sim.step();
        }
        let after: usize = sim.creatures.iter().map(|c| c.beliefs.len()).sum();
        assert!(after > before, "exploring should teach a creature something: {before} -> {after}");
    }

    #[test]
    fn plans_are_abandoned_and_the_reason_is_recorded() {
        let mut cfg = small_cfg();
        cfg.bench.maintain_population = Some(80);
        let mut sim = sim_with(44127, 80, cfg);
        let mut reasons: std::collections::BTreeSet<String> = Default::default();
        for _ in 0..400 {
            sim.step();
            for o in &sim.plan_outcomes {
                if o.reason != AbortReason::Completed {
                    reasons.insert(o.reason.as_str().to_string());
                }
            }
        }
        assert!(
            reasons.len() >= 2,
            "a world that never invalidates a plan is too static to make horizon a real \
             choice (§5.5, §13.8); saw {reasons:?}"
        );
    }

    // ------------------------------------------------------------- society

    /// Put two healthy adults next to each other with a stocked household, so
    /// the four requirements of §4.8 can be exercised one at a time.
    fn couple_at_home(sim: &mut Sim, food: f32) -> (i64, i64, i64) {
        let a = sim.spawn_at(20, 20, Sex::Female, 200, 1);
        let b = sim.spawn_at(20, 21, Sex::Male, 200, 1);
        let shelter = sim.structures.add(crate::sim::economy::Structure {
            id: 0,
            kind: crate::sim::economy::StructureKind::Shelter,
            x: 20, y: 20, condition: 1.0, capacity: 6, occupants: 0,
            household_id: None, built_tick: 0, fuel_remaining: 0.0,
            lit_until_tick: None, dirty: false,
        });
        let cfg = sim.cfg.clone();
        let h = sim.households.found(Some(shelter), a, Some(b), 0, &cfg);
        sim.households.get_mut(h).unwrap().store.add(ItemKind::Grain, food, 0);
        for id in [a, b] {
            let i = sim.index_of(id).unwrap();
            sim.creatures[i].household_id = Some(h);
            sim.creatures[i].health = 100.0;
        }
        (a, b, h)
    }

    fn marry(sim: &mut Sim, a: i64, b: i64) {
        let (ia, ib) = (sim.index_of(a).unwrap(), sim.index_of(b).unwrap());
        sim.creatures[ia].mate_id = Some(b);
        sim.creatures[ib].mate_id = Some(a);
    }

    #[test]
    fn a_paired_couple_with_a_stocked_household_conceives_and_gives_birth() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg.clone(), 44127);
        let (a, b, _) = couple_at_home(&mut sim, cfg.reproduction.store_reserve + 20.0);
        marry(&mut sim, a, b);

        let before = sim.alive();
        let mut conceived = false;
        let mut born_blank = None;
        for _ in 0..(cfg.reproduction.gestation_ticks + 40) {
            // Keep them fed and watered; this test is about §4.8, not survival.
            for c in sim.creatures.iter_mut() {
                c.hunger = 100.0;
                c.thirst = 100.0;
                c.warmth = 100.0;
                c.health = 100.0;
            }
            let r = sim.step();
            conceived |= r.conceptions > 0;
            if born_blank.is_none() {
                if let Some(child) = sim.creatures.iter().find(|c| c.generation == 2) {
                    // Checked on the tick it arrives: from the next one it is
                    // already picking things up ambiently, which is §4.11
                    // channel 1 working as intended.
                    born_blank = Some(child.beliefs.is_empty());
                }
            }
        }

        assert!(conceived, "the four requirements were met and nothing happened");
        assert!(sim.alive() > before, "a child should have arrived");
        let child = sim.creatures.iter().find(|c| c.generation == 2).expect("a second generation");
        assert_eq!(child.mother_id, Some(a));
        assert_eq!(child.father_id, Some(b));
        assert_eq!(child.life_stage, LifeStage::Infant);
        // Nothing is inherited but traits. Knowledge has to be taught or found,
        // or it dies with whoever held it — which is the whole reason the
        // culture layer exists.
        assert_eq!(born_blank, Some(true), "a newborn is born knowing nothing at all");
        assert_eq!(child.guardian_id, Some(a), "and has somebody to follow");
    }

    #[test]
    fn an_empty_store_is_the_thing_that_stops_a_lineage() {
        // §4.4 and §4.8 together: only grain keeps, and only a household above
        // the reserve may have a child. A couple with everything else in place
        // and nothing put by is the case this whole economy is built around.
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg.clone(), 44127);
        let (a, b, _) = couple_at_home(&mut sim, 0.0);
        marry(&mut sim, a, b);

        let mut blocked = 0;
        for _ in 0..60 {
            for c in sim.creatures.iter_mut() {
                c.hunger = 100.0;
                c.thirst = 100.0;
                c.health = 100.0;
            }
            let r = sim.step();
            assert_eq!(r.conceptions, 0, "no store, no child");
            blocked += r.conception_blocked[social::Blocker::StoreShort as usize];
        }
        assert!(blocked > 0, "and the reason is recorded, not merely implied");
    }

    #[test]
    fn a_child_inherits_from_both_parents() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg.clone(), 44127);
        let (a, b, _) = couple_at_home(&mut sim, 400.0);
        marry(&mut sim, a, b);

        // Push the parents to opposite extremes so inheritance is visible.
        let ia = sim.index_of(a).unwrap();
        sim.creatures[ia].traits.industry = 0.05;
        let ib = sim.index_of(b).unwrap();
        sim.creatures[ib].traits.industry = 0.95;

        for _ in 0..(cfg.reproduction.gestation_ticks + 40) {
            for c in sim.creatures.iter_mut() {
                c.hunger = 100.0;
                c.thirst = 100.0;
                c.warmth = 100.0;
                c.health = 100.0;
            }
            sim.step();
        }

        let child = sim.creatures.iter().find(|c| c.generation == 2).expect("a child");
        assert!(
            (0.2..0.8).contains(&child.traits.industry),
            "should land between its parents, got {}",
            child.traits.industry
        );
    }

    #[test]
    fn an_infant_with_no_guardian_and_nobody_to_feed_it_dies() {
        // The dependency window §4.7 calls deliberately harsh. An infant cannot
        // gather; without somebody feeding it, it starves.
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_at(20, 20, Sex::Female, 0, 2);

        for _ in 0..400 {
            sim.step();
            if sim.alive() == 0 {
                break;
            }
        }
        assert_eq!(sim.alive(), 0, "an unfed infant cannot survive alone");
        // Thirst decays faster than hunger (§4.5), and an infant can no more
        // fetch water than it can forage — so it is usually thirst that takes
        // it first. Either way it is neglect, and either way it is recorded.
        let neglect = sim.deaths_by_cause[DeathCause::Starvation as usize]
            + sim.deaths_by_cause[DeathCause::Dehydration as usize];
        assert!(neglect > 0, "cause of death: {:?}", sim.deaths_by_cause);
    }

    #[test]
    fn teaching_hands_over_knowledge_as_though_the_pupil_had_seen_it() {
        use crate::sim::knowledge::{Belief, BeliefKind, Estimate};
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        let teacher = sim.spawn_at(20, 20, Sex::Female, 300, 1);
        let pupil = sim.spawn_at(20, 21, Sex::Male, 170, 2);

        let ti = sim.index_of(teacher).unwrap();
        sim.creatures[ti].beliefs.push(Belief {
            kind: BeliefKind::SoilPatch,
            x: 44, y: 44,
            estimate: Estimate::Plentiful,
            confidence: 1.0,
            learned_tick: 0,
            last_verified_tick: 0,
            source_creature_id: None,
            hops: 0,
            origin_creature_id: Some(teacher),
            origin_tick: 0,
        });
        let pi = sim.index_of(pupil).unwrap();
        sim.creatures[pi].beliefs.clear();

        let n = sim.transfer_beliefs(teacher, pupil, Channel::Teach, None);

        assert!(n > 0, "nothing was handed over");
        let learned = &sim.creatures[sim.index_of(pupil).unwrap()].beliefs;
        let b = learned.iter().find(|b| b.kind == BeliefKind::SoilPatch).expect("taught it");
        assert_eq!(b.hops, 0, "taught knowledge arrives as though firsthand");
        assert_eq!(
            b.origin_creature_id,
            Some(teacher),
            "and still credits whoever actually found the place — this is S7's column"
        );
    }

    #[test]
    fn a_household_that_loses_everybody_passes_its_store_to_a_child() {
        // Inheritance (§4.10). Without it, death destroys grain — and grain is
        // the entire reproduction economy.
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg.clone(), 44127);
        let (a, b, parents_home) = couple_at_home(&mut sim, 50.0);

        // A grown child with a household of its own.
        let child = sim.spawn_at(30, 30, Sex::Female, 300, 2);
        let ci = sim.index_of(child).unwrap();
        sim.creatures[ci].mother_id = Some(a);
        sim.creatures[ci].father_id = Some(b);
        let childs_home = sim.households.found(Some(1), child, None, 0, &cfg);
        sim.creatures[ci].household_id = Some(childs_home);

        // Both parents die.
        for id in [a, b] {
            let i = sim.index_of(id).unwrap();
            sim.creatures[i].health = 0.0;
            sim.creatures[i].hunger = 0.0;
        }
        let mut report = TickReport::default();
        sim.phase_resolve(&mut report);

        assert!(sim.households.get(parents_home).is_none(), "the old household is gone");
        let inherited = sim.households.get(childs_home).unwrap().stored_food();
        assert!(inherited > 0.0, "the store should have passed on, got {inherited}");
    }

    #[test]
    fn a_courtship_can_be_refused_and_the_refusal_is_recorded() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg, 44127);
        let a = sim.spawn_at(20, 20, Sex::Female, 200, 1);
        let b = sim.spawn_at(20, 21, Sex::Male, 200, 1);

        sim.intents.push(SocialIntent::Court { from: b, to: a });
        let mut report = TickReport::default();
        sim.apply_social_intents(&mut report);
        assert!(sim.courtships.pending_for(a).is_some(), "the question was put");

        sim.intents.push(SocialIntent::Reject { from: b, to: a });
        let mut report = TickReport::default();
        sim.apply_social_intents(&mut report);

        assert_eq!(report.rejections, 1);
        assert!(sim.courtships.pending_for(a).is_none());
        assert!(sim.relationships.get(b, a) < 0.0, "being turned down costs something");
        assert!(sim.creature(a).unwrap().mate_id.is_none());
    }

    #[test]
    fn pairing_is_mutual_and_puts_both_in_one_household() {
        let cfg = small_cfg();
        let world = worldgen::generate(44127, &cfg).world;
        let mut sim = Sim::new(1, world, cfg.clone(), 44127);
        let a = sim.spawn_at(20, 20, Sex::Female, 200, 1);
        let b = sim.spawn_at(20, 21, Sex::Male, 200, 1);
        let h = sim.households.found(Some(1), a, None, 0, &cfg);
        let ia = sim.index_of(a).unwrap();
        sim.creatures[ia].household_id = Some(h);

        sim.intents.push(SocialIntent::Accept { from: a, to: b });
        let mut report = TickReport::default();
        sim.apply_social_intents(&mut report);

        assert_eq!(report.pairings, 1);
        assert_eq!(sim.creature(a).unwrap().mate_id, Some(b));
        assert_eq!(sim.creature(b).unwrap().mate_id, Some(a), "pairing is two-sided");
        assert_eq!(
            sim.creature(b).unwrap().household_id,
            Some(h),
            "a couple shares one household"
        );
    }

    #[test]
    fn the_population_fixture_is_a_floor_not_a_cap() {
        // It exists so performance can be measured at a stated population. Now
        // that creatures reproduce it must not *stop* them: it tops the census
        // up when death outpaces birth and otherwise gets out of the way.
        let mut cfg = small_cfg();
        cfg.bench.maintain_population = Some(50);
        let mut sim = sim_with(44127, 50, cfg);
        for _ in 0..300 {
            sim.step();
        }
        assert!(sim.alive() >= 50, "the floor holds: {}", sim.alive());
        assert!(sim.population_maintained, "a held run must be labelled as one");
    }
}
