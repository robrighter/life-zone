//! Tier 1 — the deterministic utility policy (PRD §5.2).
//!
//! **This is the experimental control for S6 and it is built to be genuinely
//! good.** There is a standing temptation to leave the fallback slightly weak
//! so that the LLM looks impressive at M3. That would not make the model
//! load-bearing; it would make the experiment worthless. If Tier 2 cannot beat
//! an honest Tier 1, the right answer is to learn that, not to arrange it.
//!
//! What the policy is, precisely: a scored decision over the goals that are
//! currently legal, given needs, inventory, beliefs and traits. It is competent
//! and myopic. It will feed a starving creature, send an idle one to the
//! nearest forage it believes in, chop wood before a night it expects to spend
//! outdoors, and take shelter when there is shelter to take.
//!
//! What it is *not*, and this is the S6 hypothesis stated mechanically: it
//! scores every option by discounted immediate return, so anything whose payoff
//! is far enough out is dominated by something nearer whenever a need is
//! pressing. Planting wheat is legal here, offered here, and scored honestly
//! here — with an explicit exponential discount over the ~72 ticks to harvest.
//! Whether that is enough for a Tier-1 population to farm is a measurement, not
//! an assumption, and M2 reports the answer rather than assuming it.
//!
//! **The policy reads beliefs, never ground truth.** A creature goes to where
//! it *believes* the berries are. If it is wrong it arrives at an empty
//! clearing, corrects the belief, and re-plans having wasted the trip. That is
//! the volatility §5.5 depends on to make horizon a real choice, and building
//! the fallback omniscient would have quietly removed it.

use crate::config::{NeedsConfig, WorldConfig};
use crate::sim::actions::{Goal, Step, Target};
use crate::sim::creature::{Addresses, Creature, ItemKind, LifeStage, Plan};
use crate::sim::economy::{self, NodeIndex, Structures};
use crate::sim::knowledge::{self, BeliefKind, NeedProfile};
use crate::sim::social::{Bystander, CreatureIndex, Courtships, Households, Relationships};
use crate::sim::perception::WorldCache;
use crate::sim::terrain::Terrain;
use crate::sim::world::{NodeKind, World};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

pub struct PolicyCtx<'a> {
    pub world: &'a World,
    pub structures: &'a Structures,
    pub cache: &'a WorldCache,
    pub nodes: &'a NodeIndex,
    pub people: &'a CreatureIndex,
    pub households: &'a Households,
    pub courtships: &'a Courtships,
    pub relationships: &'a Relationships,
    pub cfg: &'a WorldConfig,
    pub tick: i64,
    pub night: bool,
    /// The last few significant things that happened to *this* creature —
    /// §5.7 point 6, which is distinct from beliefs: those are about the world,
    /// these are about the self. Empty for Tier 1, which has no use for them.
    pub recent_events: &'a [String],
}

impl PolicyCtx<'_> {
    /// Where this creature's household keeps its store, if it has one.
    pub fn hearth_of(&self, c: &Creature) -> Option<(i64, u32, u32)> {
        self.hearth(c)
    }

    /// Where this creature would head if it went looking. Exposed so the
    /// action menu can offer the same destination Tier 1 would pick, rather
    /// than a second, differently-chosen one.
    pub fn explore_target_for(&self, c: &Creature) -> (u32, u32) {
        // A fixed stream: the menu must not consume the simulation's RNG, or
        // merely *offering* an option would change the world.
        let mut rng = ChaCha8Rng::seed_from_u64(
            (c.id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ self.tick as u64,
        );
        explore_target(c, self, &mut rng)
    }

    /// Whether this creature's hearth is *its own*, rather than one it grew up in.
    ///
    /// A child born into a household keeps its membership for life, so
    /// `hearth()` returns a roof for it forever and it never reads as homeless
    /// — which meant it never built a house, never founded a household, and
    /// never opened a store of its own. Measured over 800 ticks: living
    /// household sizes of 1, 3, 5, 8, 10, 11, 23, 38 and 46, against a shelter
    /// holding six. The household of 46 turned 40 of its members away every
    /// night, which is where 1,694 of 2,264 shelter refusals came from.
    ///
    /// `Household::size_cap` exists, is set to the shelter's capacity, is
    /// persisted, and is read by nothing.
    fn owns_hearth(&self, c: &Creature) -> bool {
        let Some(id) = c.household_id else { return false };
        let Some(h) = self.households.get(id) else { return false };
        h.founder_ids.0 == c.id || h.founder_ids.1 == Some(c.id)
    }

    /// A grown child with a mate needs a hearth of its own, not a bed in the
    /// house it was born in. This is what turns a lineage into households
    /// rather than one ever-growing dormitory (§4.6).
    fn wants_own_hearth(&self, c: &Creature) -> bool {
        self.hearth(c).is_none() || (c.is_paired() && !self.owns_hearth(c))
    }

    fn hearth(&self, c: &Creature) -> Option<(i64, u32, u32)> {
        let h = self.households.get(c.household_id?)?;
        let s = self.structures.get(h.shelter_id?)?;
        Some((h.id, s.x, s.y))
    }
}

/// How badly a need wants attention. Zero when satisfied, rising sharply once
/// it crosses into deficit, so a creature attends to the thing that is actually
/// going wrong rather than spreading effort evenly.
fn urgency(value: f32, cfg: &NeedsConfig) -> f32 {
    let d = cfg.deficit_threshold.max(1.0);
    if value >= d {
        // A mild background want, so a well-fed creature still prefers food to
        // nothing when it has nothing better to do.
        (1.0 - value / 100.0).max(0.0) * 0.25
    } else {
        0.25 + 3.0 * ((d - value) / d).powi(2)
    }
}

const MAX_URGENCY: f32 = 3.25;

/// How pressing a need is *given how long it takes to do something about it*.
///
/// This is the difference between a competent forager and a stat-watcher, and
/// it is why Tier 1 scores candidates with this rather than with `urgency`.
/// A creature thirty tiles from the river and a creature standing on its bank
/// have the same thirst reading and completely different problems: one has to
/// leave now, the other can finish picking berries. Scoring on the reading
/// alone makes both of them indifferent until the number is already low, by
/// which time the far one is a two-day walk from water with one day of thirst
/// left.
///
/// Measured before this existed: the population sat ~30 tiles from water in
/// permanent thirst, plans collapsed to a mean committed horizon of 3.2 ticks
/// because almost everyone was permanently in crisis, and 81% of all decisions
/// came out as EXPLORE because nothing else ever scored.
///
/// So: work out how many ticks of the need are left, subtract how many ticks
/// the errand costs, and let the *slack* decide. Slack near zero means go now.
fn pressure(value: f32, decay_per_tick: f32, errand_ticks: u32) -> f32 {
    let ticks_left = value / decay_per_tick.max(0.001);
    let slack = ticks_left - errand_ticks as f32;
    // A day's grace is the natural scale: beyond that it can wait.
    1.0 / (1.0 + slack.max(0.0) / 24.0)
}

/// Warmth only drains at night, so its effective rate depends on the hour.
fn warmth_decay(ctx: &PolicyCtx) -> f32 {
    let n = &ctx.cfg.needs;
    if ctx.night {
        n.warmth_decay_night
    } else {
        // Discounted by how soon dusk is, so a creature starts thinking about
        // the night before it is dark.
        (n.warmth_decay_night * night_proximity(ctx)).max(0.15)
    }
}

fn need_profile(c: &Creature, ctx: &PolicyCtx) -> NeedProfile {
    let n = &ctx.cfg.needs;
    let food_want = urgency(c.hunger, n).max(if c.inventory.food_value() < 12.0 { 0.5 } else { 0.0 });

    // Fuel is wanted for the night ahead, not in the abstract: a creature with
    // shelter in reach has little use for firewood, and one without has a great
    // deal. This is the coupling that keeps wood in demand after the shelter
    // stands (§4.4).
    //
    // Wood already in the pack satisfies the want. Without that term a creature
    // carrying a night's fuel still reads as short of it, goes looking for
    // wood it does not need, and — because it has no wood-node belief — ends up
    // exploring instead of lighting the fire it could light where it stands.
    // Having a *home*, not a bed for tonight. The same distinction that
    // decides whether a creature builds decides how much timber it wants: with
    // the occupancy test here, any shelter with a spare bed within twelve tiles
    // dropped the target from a shelter's worth of wood to a night's fire, so a
    // couple that wanted a house of their own never carried enough to raise
    // one. Measured: 77 courtships, 11 households.
    let sheltered_soon = !ctx.wants_own_hearth(c);
    let night_pressure = if ctx.night { 1.0 } else { night_proximity(ctx) };
    //
    // How much wood is enough depends on what it is *for*. With a roof in
    // reach, a creature needs a night's fire at most. With no roof anywhere, it
    // needs the timber to raise one — which is the demand that turns a
    // wandering population into a settled one.
    let night_len = (24 + ctx.cfg.actions.night_end_hour - ctx.cfg.actions.night_start_hour) % 24;
    let a_nights_fire =
        ctx.cfg.resources.fire_fuel_burn_per_tick * night_len as f32 + ctx.cfg.actions.fire_wood_cost;
    // A creature with no home needs the timber for one *and* the fire to
    // survive the nights until it is built. Aiming only at the shelter cost is
    // a trap: the night always comes first, the wood goes on the fire, and the
    // house is never started. Measured: 40 of 50 paired creatures homeless,
    // carrying a mean of 1.9 wood against a 12-wood shelter, indefinitely.
    let target = if sheltered_soon {
        a_nights_fire
    } else {
        ctx.cfg.actions.shelter_wood_cost + a_nights_fire
    };
    let short_by = ((target - c.inventory.total(ItemKind::Wood)) / target.max(1.0)).clamp(0.0, 1.0);

    // Wanted through the whole day, not only as it gets dark: a creature that
    // only wants firewood at dusk has no time left to go and get any. So fuel
    // is scored on the same time-to-satisfy footing as food and water — will I
    // be cold before I can fetch wood and light it? — using the *night* rate,
    // because that is the one that will apply when it matters.
    let cold_by_morning = pressure(c.warmth, n.warmth_decay_night, ticks_until_dusk(ctx));
    let fuel_want = (short_by * cold_by_morning.max(0.3))
        .max(urgency(c.warmth, n) / MAX_URGENCY);
    let _ = night_pressure;

    NeedProfile {
        food: (food_want / MAX_URGENCY).clamp(0.0, 1.0).max(0.08),
        water: (urgency(c.thirst, n) / MAX_URGENCY).clamp(0.0, 1.0).max(0.08),
        fuel: fuel_want.clamp(0.0, 1.0),
        shelter: (urgency(c.warmth, n).max(urgency(c.fatigue, n)) / MAX_URGENCY).clamp(0.0, 1.0),
    }
}

/// Ticks until the sun comes up. A creature that has gone to the trouble of
/// getting under a roof should stay there until morning, not wake after eight
/// ticks and walk back out into the cold.
fn ticks_until_dawn(ctx: &PolicyCtx) -> u32 {
    let hour = economy::hour_of(ctx.tick);
    let end = ctx.cfg.actions.night_end_hour;
    if hour < end {
        end - hour
    } else {
        24 - hour + end
    }
}

/// Ticks until dark. Zero once it already is.
fn ticks_until_dusk(ctx: &PolicyCtx) -> u32 {
    if ctx.night {
        return 0;
    }
    let hour = economy::hour_of(ctx.tick);
    let start = ctx.cfg.actions.night_start_hour;
    if hour <= start { start - hour } else { 24 - hour + start }
}

/// 0 in broad daylight, 1 as dusk arrives. What makes a creature start walking
/// home, or start gathering firewood, *before* it is cold.
fn night_proximity(ctx: &PolicyCtx) -> f32 {
    let hour = economy::hour_of(ctx.tick) as i32;
    let start = ctx.cfg.actions.night_start_hour as i32;
    let until = if hour <= start { start - hour } else { 24 - hour + start };
    (1.0 - (until as f32 / 8.0)).clamp(0.0, 1.0)
}

/// Octile distance, the same metric the pathfinder's heuristic uses, so an
/// estimate here and the route actually taken do not disagree.
fn travel_ticks(c: &Creature, cfg: &WorldConfig, to: (u32, u32)) -> u32 {
    let dx = c.x.abs_diff(to.0) as f32;
    let dy = c.y.abs_diff(to.1) as f32;

    // Chebyshev, because a tick is a *tile* — not octile, and with no terrain
    // multiplier.
    //
    // Both of those were charging for costs the movement code does not levy.
    // `advance_move` spends a budget of `speed` along the path but takes the
    // next tile anyway when nothing has moved yet, so neither a diagonal step
    // (√2) nor rough ground (up to 2.2) can ever drop a creature below one tile
    // per tick. Measured over 19,805 arrivals: 1.08 ticks per tile, and for
    // paths of 8 tiles or more, ticks and tiles were equal to two decimal
    // places.
    //
    // The old octile × 1.25 therefore ran ~1.5× long (measured: est/tiles 1.60
    // overall, 1.47 on long paths). Since a plan's committed horizon is the sum
    // of its steps' estimates and 86% of all committed ticks sit under a
    // MOVE_TO, that single factor was most of the population-wide abandonment
    // gap — and Tier 1 is the control S6 is judged against, so an estimate that
    // is wrong by half is not a neutral inaccuracy.
    //
    // The 1.1 is for path bending around obstacles, which is the one real cost
    // the straight-line distance misses; it is what the residual 1.08 measures.
    let tiles = dx.max(dy) * 1.1;
    // Only a configured speed *above* one tile per tick can beat the floor, so
    // that is the only case where dividing is right.
    ((tiles / cfg.actions.move_speed.max(1.0)).ceil() as u32).max(1)
}

/// How long resting will actually take: the ticks needed to reach the fatigue
/// level at which `actions::rest` reports the step complete.
///
/// The estimate used to be a flat 6 or 8 regardless of how tired the creature
/// was. Rest ends the moment fatigue passes 99, so a creature that lies down at
/// 94 is up again on the next tick — measured across 13,457 rest plans, 98.9%
/// completed and the mean duration was 0.025 ticks against a committed 6.
fn rest_ticks(c: &Creature, cfg: &WorldConfig) -> u32 {
    let restore = if c.in_shelter.is_some() {
        cfg.actions.rest_restore_sheltered
    } else {
        cfg.actions.rest_restore
    };
    (((99.0 - c.fatigue) / restore.max(0.1)).ceil().max(1.0) as u32).min(24)
}

/// A scored option: what to do, why, and how good it looks.
struct Candidate {
    score: f32,
    steps: Vec<Step>,
    rationale: String,
    addresses: Addresses,
    /// Confidence in the belief this plan rests on. Drives how long the
    /// creature is willing to commit (§5.5) — commit long on what you saw
    /// yourself, stay tentative on hearsay.
    confidence: f32,
}

/// Choose a plan. Deterministic given the creature, the world and the RNG
/// stream; `rng` is used only to break ties between near-equal options and to
/// pick an exploration bearing.
pub fn decide(c: &Creature, ctx: &PolicyCtx, rng: &mut ChaCha8Rng) -> Plan {
    let n = &ctx.cfg.needs;
    let needs = need_profile(c, ctx);
    let mut best: Option<Candidate> = None;

    let mut offer = |mut cand: Candidate| {
        if cand.score <= 0.0 || cand.steps.is_empty() {
            return;
        }
        // §5.4's elder habit prior. An elder leans toward the kind of plan that
        // has worked for it before, which is what lets §13.10 ask whether
        // elders need deliberation at all.
        //
        // This multiplier was written, unit-tested, and never called from
        // anywhere: `Creature::habit` was initialised to zeros, serialised, and
        // never incremented. That left Tier 1 weaker than specified — and Tier
        // 1 is the control S6 is judged against, so an unwired improvement to
        // it is a thumb on the scale in the model's favour.
        cand.score *= crate::ai::budget::habit_bonus(c, cand.addresses, ctx.cfg);
        if best.as_ref().is_none_or(|b| cand.score > b.score) {
            best = Some(cand);
        }
    };

    // Infants cannot work. They stay put, eat what they are given, and rest —
    // the household feeding them is M4's problem, and until then an infant with
    // nothing in its pack is genuinely helpless, which is the dependency window
    // §4.7 says is deliberately harsh.
    if c.life_stage == LifeStage::Infant {
        let mut steps = Vec::new();
        if c.inventory.total_food() > 0.0 && c.hunger < 70.0 {
            steps.push(Step::new(Goal::EatFromInventory, Target::None, 1));
        }
        // Staying with whoever is keeping you alive is the only meaningful
        // thing an infant can do. If the guardian is gone it has nothing left
        // but to wait, which is the dependency window doing what §4.7 says it
        // does.
        if let Some(g) = c.guardian_id.filter(|g| ctx.people.get(*g).is_some()) {
            steps.push(Step::new(Goal::Follow, Target::Creature(g), 12));
        } else {
            steps.push(Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg)));
        }
        return finish(
            c, ctx, steps,
            if c.guardian_id.is_some() {
                "Stay close.".into()
            } else {
                "Nobody is coming.".into()
            },
            1.0, Addresses::Rest,
        );
    }

    // ---- eat what you already carry: instant, free, and usually right -------
    if c.inventory.total_food() > 0.0 && c.hunger < 85.0 {
        let spoiling = c
            .inventory
            .oldest_food()
            .and_then(|b| economy::ticks_until_spoiled(b, ctx.tick, ctx.cfg))
            .is_some_and(|t| t < 12);
        offer(Candidate {
            score: pressure(c.hunger, n.hunger_decay_per_tick, 0) * 1.5
                + if spoiling { 0.6 } else { 0.0 },
            steps: vec![Step::new(Goal::EatFromInventory, Target::None, 1)],
            addresses: Addresses::Food,
            rationale: if spoiling {
                "Eat it before it turns.".into()
            } else {
                "There is food in the pack.".into()
            },
            confidence: 1.0,
        });
    }

    // ---- drink -------------------------------------------------------------
    if ctx.world.at(c.x, c.y).is_fresh_water() {
        offer(Candidate {
            score: pressure(c.thirst, n.thirst_decay_per_tick, 0) * 1.9,
            steps: vec![Step::new(Goal::Drink, Target::Tile(c.x, c.y), 1)],
            addresses: Addresses::Water,
            rationale: "Standing in the shallows.".into(),
            confidence: 1.0,
        });
    } else if let Some(i) =
        knowledge::best_of(&c.beliefs, &[BeliefKind::Water], (c.x, c.y), ctx.tick, &needs, &ctx.cfg.knowledge)
    {
        let b = &c.beliefs[i];
        let conf = b.confidence_at(ctx.tick, &ctx.cfg.knowledge);
        let quality = knowledge::target_quality(b, (c.x, c.y), ctx.tick, &ctx.cfg.knowledge);
        let t = travel_ticks(c, ctx.cfg, (b.x, b.y));
        offer(Candidate {
            score: pressure(c.thirst, n.thirst_decay_per_tick, t) * quality * 2.6,
            steps: vec![
                Step::new(Goal::MoveTo, Target::Tile(b.x, b.y), t),
                Step::new(Goal::Drink, Target::Tile(b.x, b.y), 1),
            ],
            addresses: Addresses::Water,
            rationale: format!("Water at {},{} — {}.", b.x, b.y, b.provenance(ctx.tick)),
            confidence: conf,
        });
    }

    // ---- go and get food ---------------------------------------------------
    if let Some(cand) = food_run(c, ctx) {
        offer(cand);
    }

    // ---- fuel: wood is timber and warmth both (§4.4) -----------------------
    let wood_held = c.inventory.total(ItemKind::Wood);
    let want_wood = wood_held < ctx.cfg.actions.shelter_wood_cost;
    if want_wood {
        if let Some(i) = knowledge::best_of(
            &c.beliefs, &[BeliefKind::WoodNode], (c.x, c.y), ctx.tick, &needs, &ctx.cfg.knowledge,
        ) {
            let b = &c.beliefs[i];
            let conf = b.confidence_at(ctx.tick, &ctx.cfg.knowledge);
            let quality = knowledge::target_quality(b, (c.x, c.y), ctx.tick, &ctx.cfg.knowledge);
            if let Some(node) = find_node(ctx, NodeKind::Wood, b.x, b.y) {
                let t = travel_ticks(c, ctx.cfg, (b.x, b.y));
                // Industry is a taste for long-horizon work over immediate gain.
                // Wanting timber is not the same as wanting fuel. A homeless
                // creature is gathering to *build*, and a homeless paired one
                // has §4.8 telling it that nothing else it wants is possible
                // until it does.
                let building = match (ctx.hearth(c).is_none(), c.is_paired()) {
                    (true, true) => 2.4,
                    (true, false) => 1.5,
                    _ => 1.0,
                };
                let drive = (0.55 + 0.75 * c.traits.industry) * building;
                offer(Candidate {
                    score: needs.fuel * quality * 2.6 * drive,
                    steps: vec![
                        Step::new(Goal::MoveTo, Target::Tile(b.x, b.y), t),
                        // Long enough to come home with the timber for a
                        // shelter, not just a night's fire. At six ticks a trip
                        // yielded 8.4 wood against a 12-wood shelter, so no
                        // creature ever crossed the threshold to build one and
                        // the whole population slept outdoors for the entire
                        // run — exposure was the leading cause of death.
                        Step::new(Goal::ChopWood, Target::Node(node), 10),
                    ],
                    addresses: Addresses::Fuel,
                    rationale: format!("Wood at {},{} — {}.", b.x, b.y, b.provenance(ctx.tick)),
                    confidence: conf,
                });
            }
        }
    }

    // ---- warmth: shelter, or a fire, or a cold night -----------------------
    let lit_fire_near = ctx
        .structures
        .fire_near(c.x, c.y, ctx.cfg.actions.fire_warmth_radius, ctx.tick)
        .is_some();

    // Going home to sleep. A creature with a hearth of its own goes to *that*
    // one, and puts down what it is carrying on the way in.
    //
    // This is the change that makes households work. Provisioning as a separate
    // errand loses to survival every time — measured at 98 deposits across 51
    // households over 2,500 ticks, with stores averaging 0.8 against a reserve
    // of 10 — because a creature deciding between "walk home and put the
    // berries away" and "eat, drink, or get warm" will always have something
    // more pressing. Coming home to sleep is not a competing errand; it is
    // something the creature already does every night. Putting the food away on
    // the way in costs one tick of a journey it was making anyway.
    let own = ctx.hearth(c);
    let bed = own
        .map(|(_, hx, hy)| (hx, hy, None))
        .or_else(|| {
            ctx.structures
                .nearest_shelter(c.x, c.y, 40)
                .map(|s| (s.x, s.y, Some(s.id)))
        });
    if let Some((sx, sy, sid)) = bed {
        let t = travel_ticks(c, ctx.cfg, (sx, sy));
        let pull = pressure(c.warmth, warmth_decay(ctx), t)
            .max(if ctx.night { 0.5 } else { night_proximity(ctx) * 0.55 });

        let mut steps = vec![Step::new(Goal::MoveTo, Target::Tile(sx, sy), t)];
        let bringing_something = own.is_some() && c.inventory.weight() > 7.0;
        if bringing_something {
            steps.push(Step::new(Goal::DepositToStore, Target::None, 1));
        }
        if let Some(id) = sid {
            steps.push(Step::new(Goal::Shelter, Target::Structure(id), 1));
        } else {
            steps.push(Step::new(Goal::Shelter, Target::None, 1));
        }
        steps.push(Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg)));

        // Home pulls harder than any shelter that merely has a spare bed:
        // walking twenty tiles to sleep somewhere else is worse than lighting a
        // fire where you stand, but walking twenty tiles to your own hearth
        // with a full pack is how a household gets fed.
        let homeward = if own.is_some() { 1.35 } else { 1.0 };
        offer(Candidate {
            score: pull * 2.4 * homeward / (1.0 + t as f32 / 12.0),
            steps,
            addresses: Addresses::Warmth,
            rationale: if bringing_something {
                "Home before dark, and put this away.".into()
            } else {
                "Under a roof before dark.".into()
            },
            confidence: 1.0,
        });
    }

    let roof_in_reach = ctx.structures.nearest_shelter(c.x, c.y, 8).is_some();
    if ctx.cfg.features.fires
        && (ctx.night || night_proximity(ctx) > 0.6)
        && !lit_fire_near
        && !roof_in_reach
    {
        if wood_held >= ctx.cfg.actions.fire_wood_cost {
            offer(Candidate {
                score: pressure(c.warmth, warmth_decay(ctx), 1).max(0.4) * 2.2,
                steps: vec![
                    Step::new(Goal::BuildFire, Target::None, 1),
                    Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg)),
                ],
                addresses: Addresses::Warmth,
                rationale: "No roof in reach. Burn what I carry.".into(),
                confidence: 1.0,
            });
        }
    } else if ctx.cfg.features.fires && lit_fire_near && wood_held >= 1.0 && ctx.night {
        offer(Candidate {
            score: 0.9,
            steps: vec![
                Step::new(Goal::FeedFire, Target::None, 1),
                Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg)),
            ],
            addresses: Addresses::Warmth,
            rationale: "Keep the fire in.".into(),
            confidence: 1.0,
        });
    }

    // ---- rest --------------------------------------------------------------
    if c.fatigue < 55.0 {
        offer(Candidate {
            score: pressure(c.fatigue, n.fatigue_decay_per_tick, 0) * 1.0,
            steps: vec![Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg))],
            addresses: Addresses::Rest,
            rationale: "Worn out.".into(),
            confidence: 1.0,
        });
    }

    // ---- build a shelter of one's own --------------------------------------
    // A creature builds when it has nowhere of its *own* — not when there
    // happens to be no free bed nearby.
    //
    // M2 gated this on `nearest_shelter(10).is_none()`, which tests tonight's
    // occupancy. Occupancy is zero all day, and daytime is the only time a
    // shelter can be built, so the answer was always "somewhere nearby has
    // room" and 4,271 creatures built 35 shelters between them. Homelessness is
    // a persistent fact about a creature; a free bed is not.
    let homeless = ctx.wants_own_hearth(c);
    if homeless
        && wood_held >= ctx.cfg.actions.shelter_wood_cost
        && ctx.world.at(c.x, c.y).passable()
        && !ctx.world.at(c.x, c.y).is_water()
    {
        // Worth more the closer it is to water, because a shelter a creature
        // cannot drink near is a shelter it will not sleep in. This is what
        // makes settlements form along the rivers rather than anywhere the
        // fourteenth log happened to be cut.
        let near_water = ctx
            .cache
            .water_within(ctx.world, c.x, c.y, 14)
            .map(|_| 1.5)
            .unwrap_or(0.7);
        // Weighted up for a paired creature: §4.8 will not let them have a
        // child until they have somewhere to keep the food.
        let for_a_family = if c.is_paired() { 1.8 } else { 1.0 };
        offer(Candidate {
            score: 1.15 * near_water * for_a_family
                * (0.5 + c.traits.industry)
                * (1.0 + night_proximity(ctx)),
            steps: vec![Step::new(
                Goal::BuildShelter,
                Target::Tile(c.x, c.y),
                ctx.cfg.actions.shelter_build_ticks,
            )],
            addresses: Addresses::Warmth,
            rationale: "Nowhere to sleep here. Build one.".into(),
            confidence: 1.0,
        });
    }

    // ---- farming, scored honestly and discounted honestly -------------------
    if let Some(cand) = plant_run(c, ctx, &needs) {
        offer(cand);
    }

    // ---- verify something doubtful ----------------------------------------
    if let Some(cand) = verify_run(c, ctx, &needs) {
        offer(cand);
    }

    // ---- the household: eat from it, stock it, go back to it ---------------
    if let Some((_hid, hx, hy)) = ctx.hearth(c) {
        let at_home = c.x.abs_diff(hx) <= 1 && c.y.abs_diff(hy) <= 1;
        let store = ctx.households.get(c.household_id.unwrap());
        let stored = store.map(|h| h.stored_food()).unwrap_or(0.0);
        let t = travel_ticks(c, ctx.cfg, (hx, hy));

        // Eating from the store is the cheapest meal there is when you are
        // standing in front of it — but what is *below* the reserve is not
        // food, it is the household's future.
        //
        // Without this distinction the store is a larder rather than a reserve:
        // it is eaten down as fast as it is filled, never crosses §4.8's
        // threshold, and no household ever has a child. Measured on a founders
        // run before the rule existed: 91.5% of all blocked conception ticks
        // were "store below the reserve", with the households in question
        // holding food the whole time.
        //
        // So above the reserve, eat freely. Below it, only when genuinely
        // hungry and carrying nothing else — a household starving beside its
        // seed corn is not thrift, it is stupidity.
        let reserve = ctx.cfg.reproduction.store_reserve;
        if at_home && stored > 0.0 {
            let spare = stored - reserve;
            let desperate = c.hunger < n.deficit_threshold && c.inventory.total_food() < 1.0;
            if spare > 0.0 && c.hunger < 70.0 {
                offer(Candidate {
                    score: pressure(c.hunger, n.hunger_decay_per_tick, 0) * 1.45,
                    steps: vec![Step::new(Goal::EatFromStore, Target::None, 1)],
                    addresses: Addresses::Food,
                    rationale: "Eat from what the store can spare.".into(),
                    confidence: 1.0,
                });
            } else if desperate {
                offer(Candidate {
                    score: pressure(c.hunger, n.hunger_decay_per_tick, 0) * 1.6,
                    steps: vec![Step::new(Goal::EatFromStore, Target::None, 1)],
                    addresses: Addresses::Food,
                    rationale: "Nothing left but the reserve.".into(),
                    confidence: 1.0,
                });
            }
        }

        // Banking the harvest. This is the single most consequential thing a
        // Tier 1 creature can do for its lineage: only grain keeps, and only a
        // household store above the reserve permits a child (§4.4, §4.8). It
        // is scored on how much of the reserve is still missing, so a household
        // that has enough stops hoarding and goes back to living.
        // Only when there is genuinely something spare *and* nothing pressing.
        // A creature that banks its food and then starves on the walk back has
        // helped nobody, and its household still has no reserve.
        // "Can I spare this?" is a question about the pack, not about the
        // stomach. Gating on hunger alone had creatures walking around with 73
        // nutrition in hand and an empty household store, because hunger sits
        // just under any fixed threshold you pick while food keeps arriving.
        let surplus = c.inventory.weight() - 6.0;
        // Carrying several meals *is* being able to spare one. Folding hunger
        // into the test made it fail perpetually, because hunger sits wherever
        // the food supply pins it and food kept arriving into a pack that never
        // got emptied into the store.
        let can_spare =
            (c.inventory.food_value() > 28.0 || c.hunger > 55.0) && c.thirst > 30.0;
        if surplus > 1.0 && can_spare {
            let short = ((ctx.cfg.reproduction.store_reserve - stored)
                / ctx.cfg.reproduction.store_reserve.max(1.0))
            .clamp(0.0, 1.0);
            let grain = c.inventory.total(ItemKind::Grain);
            // Grain is what the reserve is actually made of, so carrying grain
            // makes the trip home worth more than carrying berries.
            let worth = 0.7 + short * 1.9 + (grain / 8.0).min(1.0) * 0.8;
            let mut steps = Vec::new();
            if !at_home {
                steps.push(Step::new(Goal::MoveTo, Target::Tile(hx, hy), t));
            }
            steps.push(Step::new(Goal::DepositToStore, Target::None, 1));
            offer(Candidate {
                // Tolerant of distance. Provisioning a household is worth a
                // walk: at a /10 falloff a creature foraging twenty tiles out
                // never went home, and its household held a mean of 3 food
                // against a reserve of 20.
                score: worth * 0.85 * (0.6 + 0.8 * c.traits.industry)
                    / (1.0 + t as f32 / 20.0),
                steps,
                addresses: Addresses::Food,
                rationale: if short > 0.0 {
                    format!("Store it — {:.0} of {:.0} toward the reserve.",
                            stored, ctx.cfg.reproduction.store_reserve)
                } else {
                    "Store the surplus.".into()
                },
                confidence: 1.0,
            });
        }

        // Take provisions out before setting off. Only possible at the hearth.
        if at_home && stored > 8.0 && c.inventory.food_value() < 12.0 {
            offer(Candidate {
                score: 0.55,
                steps: vec![Step::new(Goal::WithdrawFromStore, Target::None, 1)],
                addresses: Addresses::Food,
                rationale: "Take provisions.".into(),
                confidence: 1.0,
            });
        }
    }

    // ---- neighbours --------------------------------------------------------
    if let Some(cand) = social_run(c, ctx) {
        offer(cand);
    }

    // ---- go where the people are -------------------------------------------
    //
    // Courtship was purely opportunistic: it fired only when somebody suitable
    // happened to be standing next to you. On a 512-tile map with a scattered
    // population that is a coincidence, not a decision, and the measured result
    // was fourteen pairings among sixty creatures over three thousand ticks.
    //
    // Creatures have no PERSON beliefs yet — they cannot remember where anybody
    // *lives* — so what they can do is head for the one place people reliably
    // are: somebody's hearth. That is what makes a settlement a settlement
    // rather than a scatter of individuals who happen to share a map.
    if c.life_stage == LifeStage::Adult
        && c.mate_id.is_none()
        && c.hunger > 40.0
        && c.thirst > 40.0
    {
        if let Some(s) = ctx.structures.nearest_shelter(c.x, c.y, 60) {
            let t = travel_ticks(c, ctx.cfg, (s.x, s.y));
            if t > 1 {
                offer(Candidate {
                    score: 0.55 * (0.4 + 1.2 * c.traits.sociability) / (1.0 + t as f32 / 24.0),
                    steps: vec![Step::new(Goal::MoveTo, Target::Tile(s.x, s.y), t)],
                    addresses: Addresses::Kinship,
                    rationale: "Somewhere there are other people.".into(),
                    confidence: 0.8,
                });
            }
        }
    }

    // ---- explore -----------------------------------------------------------
    // Weighted up when a pressing need has nothing to serve it: a thirsty
    // creature with no water belief has no option but to go and look.
    let (unserved, looking_for) = unserved_pressure(c, ctx, &needs);
    let (ex, ey) = explore_target(c, ctx, rng);
    let t = travel_ticks(c, ctx.cfg, (ex, ey));
    offer(Candidate {
        // A modest nudge, not a dominating one.
        //
        // This term was 1.4x and it wrecked the simulation: a creature that
        // wanted firewood and held a perfectly good belief about a stand of
        // trees scored "go and look for wood somewhere" above "go to the wood",
        // because the unserved bonus outweighed the whole chop plan. Measured:
        // 67% of all plans aimed at fuel, 75% of them were EXPLORE, and the
        // population carried a mean of 0.3 wood while 9,900 wood stood
        // untouched on the map.
        //
        // The competition does not need the help. When a creature has no belief
        // that serves a need, the candidate for that need is simply never
        // offered, and exploring wins on its own because nothing else scored.
        score: 0.16 * (0.4 + 1.2 * c.traits.boldness) + unserved * 0.35,
        steps: vec![Step::new(Goal::Explore, Target::Tile(ex, ey), t)],
        // A search prompted by thirst *is* the response to thirst.
        addresses: if unserved > 0.0 { looking_for } else { Addresses::Knowledge },
        rationale: match looking_for {
            Addresses::Water if unserved > 0.0 => "No water I know of. Go and find some.".into(),
            Addresses::Food if unserved > 0.0 => "Nothing left to pick here. Go and look.".into(),
            Addresses::Fuel if unserved > 0.0 => "No wood I know of. Go and look.".into(),
            _ => "See what is out that way.".into(),
        },
        confidence: 0.6,
    });

    match best {
        Some(b) => finish(c, ctx, b.steps, b.rationale, b.confidence, b.addresses),
        // Every branch above can decline; resting is always available and
        // always defensible, so a creature can never end a tick without a plan.
        None => finish(c, ctx, vec![Step::new(Goal::Rest, Target::None, rest_ticks(c, ctx.cfg))],
                       "Nothing to be done.".into(), 1.0, Addresses::Rest),
    }
}

/// Everything that involves another creature standing next to you.
///
/// One candidate function rather than eight, because the options are mutually
/// exclusive in practice — a creature does one social thing per plan — and
/// because they all need the same expensive thing: the list of who is actually
/// in reach.
///
/// **On teaching, and why it is scored honestly.** §13.5 asks whether creatures
/// will ever choose to teach, given that it costs six ticks now and pays off
/// only after the teacher is dead. It would be easy to make parent-to-child
/// transfer automatic and declare culture achieved; the PRD explicitly says to
/// resist that, because a simulation that hardcodes the result is performing
/// culture rather than discovering it. So TEACH is offered here with an honest
/// utility — the immediate affinity it buys, weighted by sociability — and
/// whether a Tier 1 population teaches enough for knowledge to outlive its
/// discoverer is a measurement, reported rather than assumed.
fn social_run(c: &Creature, ctx: &PolicyCtx) -> Option<Candidate> {
    if c.life_stage == LifeStage::Infant {
        return None;
    }
    let reach = ctx.cfg.actions.social_reach.max(2);
    let mut near: Vec<Bystander> = Vec::new();
    ctx.people.near(c.x, c.y, reach, c.id, &mut near);
    if near.is_empty() {
        return None;
    }

    let n = &ctx.cfg.needs;
    let mut best: Option<Candidate> = None;
    let mut offer = |mut cand: Candidate| {
        cand.score *= crate::ai::budget::habit_bonus(c, cand.addresses, ctx.cfg);
        if cand.score > 0.0 && best.as_ref().is_none_or(|b| cand.score > b.score) {
            best = Some(cand);
        }
    };

    // ---- answer anybody who has asked ------------------------------------
    // Left standing, an offer lapses and costs both parties something, so a
    // creature answers before it does anything else sociable.
    if let Some(o) = ctx.courtships.pending_for(c.id) {
        if c.mate_id.is_none() && near.iter().any(|p| p.id == o.from) {
            let suitor = near.iter().find(|p| p.id == o.from).unwrap();
            let affinity = ctx.relationships.get(c.id, o.from);
            // Health, and how much this creature already likes them. A cautious
            // creature wants more convincing.
            let appeal = 0.35 + affinity * 0.5 + (suitor.health / 100.0) * 0.35
                - c.traits.caution * 0.25;
            if appeal > 0.35 {
                offer(Candidate {
                    score: 1.9,
                    steps: vec![Step::new(Goal::AcceptCourtship, Target::Creature(o.from), 1)],
                    addresses: Addresses::Kinship,
                    rationale: format!("Accept #{}.", o.from),
                    confidence: 1.0,
                });
            } else {
                offer(Candidate {
                    score: 1.2,
                    steps: vec![Step::new(Goal::RejectCourtship, Target::Creature(o.from), 1)],
                    addresses: Addresses::Kinship,
                    rationale: format!("Turn #{} down.", o.from),
                    confidence: 1.0,
                });
            }
        }
    }

    // ---- a hungry infant in reach ----------------------------------------
    // Whoever is standing there with food. An infant cannot feed itself, and
    // this is the only thing that stops the dependency window killing it.
    if c.inventory.total_food() > 0.0 {
        let mine = near
            .iter()
            .filter(|p| p.life_stage == LifeStage::Infant && p.hunger < 65.0)
            .min_by(|a, b| {
                // The household's own first, then the hungriest.
                let own = |p: &Bystander| p.household_id.is_some() && p.household_id == c.household_id;
                own(b).cmp(&own(a)).then(a.hunger.total_cmp(&b.hunger))
            });
        if let Some(inf) = mine {
            let own = inf.household_id.is_some() && inf.household_id == c.household_id;
            let kin = ctx.relationships.get(c.id, inf.id);
            let urgency = 1.0 - (inf.hunger / 100.0);
            offer(Candidate {
                score: urgency * (if own { 2.4 } else { 0.7 })
                    * (0.5 + c.traits.sociability + kin.max(0.0)),
                steps: vec![Step::new(Goal::FeedInfant, Target::Creature(inf.id), 1)],
                addresses: Addresses::Kinship,
                rationale: format!("Feed #{}.", inf.id),
                confidence: 1.0,
            });
        }
    }

    // ---- meat that will not keep ------------------------------------------
    // §4.4: a slaughtered sheep is four days of food nobody can eat alone.
    // Generosity becomes the rational move, discovered rather than imposed —
    // so this is scored on the surplus, not on a rule about sharing.
    let perishing = c
        .inventory
        .batches
        .iter()
        .filter(|b| b.kind.is_food())
        .filter(|b| {
            economy::ticks_until_spoiled(b, ctx.tick, ctx.cfg).is_some_and(|t| t < 36)
        })
        .map(|b| b.quantity)
        .sum::<f32>();
    if perishing > 8.0 {
        if let Some(p) = near
            .iter()
            .filter(|p| p.life_stage != LifeStage::Infant && p.hunger < 70.0)
            .min_by(|a, b| a.hunger.total_cmp(&b.hunger))
        {
            let kin = ctx.relationships.get(c.id, p.id).max(0.0);
            offer(Candidate {
                score: (perishing / 20.0).min(1.2)
                    * (0.4 + c.traits.sociability + kin)
                    * 0.9,
                steps: vec![Step::new(Goal::GiveFood, Target::Creature(p.id), 1)],
                addresses: Addresses::Kinship,
                rationale: "This will spoil before I can eat it.".into(),
                confidence: 1.0,
            });
        }
    }

    // ---- court -------------------------------------------------------------
    if c.is_courtable(&ctx.cfg.lifespan, ctx.tick) && c.hunger > 35.0 && c.thirst > 35.0 {
        let suitor = near
            .iter()
            .filter(|p| {
                p.sex != c.sex
                    && !p.paired
                    && p.life_stage == LifeStage::Adult
                    && p.health > 45.0
            })
            .max_by(|a, b| {
                let score = |p: &Bystander| {
                    ctx.relationships.get(c.id, p.id) + p.health / 200.0
                };
                score(a).total_cmp(&score(b)).then(b.id.cmp(&a.id))
            });
        if let Some(p) = suitor {
            let affinity = ctx.relationships.get(c.id, p.id);
            // Boldness is willingness to make the approach; caution is what
            // holds it back until the ground is firmer.
            let nerve = 0.45 + c.traits.boldness * 0.7 + affinity * 0.6
                - c.traits.caution * 0.3;
            offer(Candidate {
                score: nerve.max(0.0) * 1.35,
                steps: vec![Step::new(Goal::Court, Target::Creature(p.id), 1)],
                addresses: Addresses::Kinship,
                rationale: format!("Ask #{}.", p.id),
                confidence: 1.0,
            });
        }
    }

    // ---- join a household --------------------------------------------------
    //
    // A household you have a claim on: your mate's, or your parents'. Not
    // whichever roof happens to be nearest.
    //
    // Letting anybody join anybody filled a dozen households with unrelated
    // adults who each drew on the store and none of whom then founded a home of
    // their own — 150 creatures, 78 courtships, 12 households, and every store
    // eaten flat by people who were not raising anything. Belonging somewhere
    // has to mean something, or nobody ever builds.
    if c.household_id.is_none() {
        let claim = near
            .iter()
            .filter(|p| p.household_id.is_some())
            .filter(|p| {
                ctx.households
                    .get(p.household_id.unwrap())
                    .is_some_and(|hh| hh.shelter_id.is_some())
            })
            .filter(|p| {
                // Kin or mate: somebody this creature actually belongs with.
                Some(p.id) == c.mate_id || ctx.relationships.get(c.id, p.id) > 0.35
            })
            .max_by(|a, b| {
                ctx.relationships
                    .get(c.id, a.id)
                    .total_cmp(&ctx.relationships.get(c.id, b.id))
                    .then(b.id.cmp(&a.id))
            });
        if let Some(p) = claim {
            let h = p.household_id.unwrap();
            let kin = ctx.relationships.get(c.id, p.id);
            offer(Candidate {
                score: (0.6 + kin * 1.4) * (0.4 + c.traits.sociability),
                steps: vec![Step::new(Goal::JoinHousehold, Target::Household(h), 1)],
                addresses: Addresses::Kinship,
                rationale: format!("Join household {h}."),
                confidence: 1.0,
            });
        }
    }

    // ---- tell somebody something ------------------------------------------
    //
    // Only if there is something to tell. Without that test a creature shares
    // with whoever is standing there, over and over, because each share nudges
    // affinity up and makes the next one look better still — measured at 37% of
    // all decisions in a founders run, while the same creatures gathered 365
    // units of food between them and starved.
    //
    // Trust gates *whether* (§4.11): kin and household get generous sharing,
    // strangers get little. That is a disposition rather than a rule, and the
    // model is free to override it in either direction.
    if ctx.cfg.features.knowledge_sharing && !c.beliefs.is_empty() {
        let mine = knowledge::known_kinds(&c.beliefs, ctx.tick, &ctx.cfg.knowledge);
        let listener = near.iter().find_map(|p| {
            let topic = knowledge::topic_for(mine, p.known_kinds)?;
            Some((p, topic))
        });
        if let Some((p, topic)) = listener {
            let household = p.household_id.is_some() && p.household_id == c.household_id;
            let trust = ctx.relationships.get(c.id, p.id) + if household { 0.5 } else { 0.0 };
            let willing = c.traits.sociability * 0.8 + trust * 0.6;
            if willing > 0.35 {
                offer(Candidate {
                    score: willing * 0.30,
                    steps: vec![
                        Step::new(Goal::ShareKnowledge, Target::Creature(p.id), 1).about(topic)
                    ],
                    addresses: Addresses::Knowledge,
                    rationale: format!(
                        "Tell #{} about {}.",
                        p.id,
                        topic.as_str().replace('_', " ").to_lowercase()
                    ),
                    confidence: 1.0,
                });
            }
        }
    }

    // ---- teach the young ---------------------------------------------------
    if ctx.cfg.features.teaching && c.household_id.is_some() && c.beliefs.len() >= 4 {
        if let Some(pupil) = near
            .iter()
            .filter(|p| {
                p.household_id == c.household_id && p.life_stage != LifeStage::Elder
                    && p.id != c.id
            })
            .min_by_key(|p| p.id)
        {
            // What it is worth *now*, which is all a myopic policy can see: the
            // affinity it buys, weighted by how sociable this creature is. The
            // thing that would actually justify it — a lineage that still knows
            // where the water is in eighty years — is invisible from here. That
            // is the gap the model is supposed to close.
            let kin = ctx.relationships.get(c.id, pupil.id).max(0.0);
            let comfortable = c.hunger > 55.0 && c.thirst > 55.0 && c.fatigue > 40.0;
            if comfortable {
                offer(Candidate {
                    score: (0.30 + kin * 0.35) * (0.3 + c.traits.sociability * 0.9),
                    steps: vec![Step::new(
                        Goal::Teach,
                        Target::Creature(pupil.id),
                        ctx.cfg.knowledge.teach_ticks,
                    )],
                    addresses: Addresses::Knowledge,
                    rationale: format!("Teach #{} what I know.", pupil.id),
                    confidence: 1.0,
                });
            }
        }
    }

    let _ = n;
    best
}

/// Fetch food from wherever the creature believes food is: berries, a wheat
/// field, or a sheep. All three route through the same candidate so they
/// compete on their merits rather than by a hardcoded preference order.
fn food_run(c: &Creature, ctx: &PolicyCtx) -> Option<Candidate> {
    let n = &ctx.cfg.needs;
    if c.inventory.weight() >= c.carry_capacity(ctx.cfg) - 0.5 {
        return None;
    }

    let kinds = [BeliefKind::ForageNode, BeliefKind::SoilPatch, BeliefKind::SheepFlock];
    let mut best: Option<Candidate> = None;

    for kind in kinds {
        let node_kind_probe = node_kind_for(kind);
        if !ctx.cfg.features.wheat && node_kind_probe == NodeKind::Wheat {
            continue;
        }
        if !ctx.cfg.features.sheep && node_kind_probe == NodeKind::Sheep {
            continue;
        }

        // Rank, then take the first belief that resolves to something actually
        // standing there — not simply the best-ranked one.
        //
        // `SOIL_PATCH` covers both a wheat node and a bare patch of farmable
        // ground, because from a creature's point of view they are the same
        // kind of place. Taking only the single best belief therefore kept
        // picking bare soil, failing to find a crop on it, and abandoning the
        // whole wheat option — while the creature stood there knowing perfectly
        // well where a field was. Since grain is the only food that keeps, and
        // the household reserve is what gates reproduction, that one line was
        // quietly severing the entire path from foraging to a lineage.
        // Ranked *within this kind*, then the first that resolves to something
        // actually standing there. Ranking across all kinds and filtering
        // afterwards is the same mistake in a different place: the top of a
        // creature's overall ranking is nearly always water and berries, so no
        // soil patch ever survived the filter and grain vanished again.
        let mut ranked: Vec<(usize, f32)> = c
            .beliefs
            .iter()
            .enumerate()
            .filter(|(_, b)| b.kind == kind)
            .map(|(i, b)| {
                (i, knowledge::target_quality(b, (c.x, c.y), ctx.tick, &ctx.cfg.knowledge))
            })
            .filter(|(_, q)| *q > 0.0)
            .collect();
        ranked.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));

        let Some((i, node)) = ranked.into_iter().find_map(|(i, _)| {
            let b = &c.beliefs[i];
            find_node(ctx, node_kind_probe, b.x, b.y).map(|nd| (i, nd))
        }) else {
            continue;
        };
        let b = &c.beliefs[i];
        let conf = b.confidence_at(ctx.tick, &ctx.cfg.knowledge);
        let mut quality = knowledge::target_quality(b, (c.x, c.y), ctx.tick, &ctx.cfg.knowledge);
        let t = travel_ticks(c, ctx.cfg, (b.x, b.y));

        // A creature with a home forages around it. Not a rule — a weighting —
        // but it is what turns a household from an address into a base, and
        // what puts the walk back with a full pack within reach of being worth
        // making. Without it creatures drift outward from their own hearth and
        // the store stays empty however much they gather.
        if let Some((_, hx, hy)) = ctx.hearth(c) {
            let dx = b.x as f32 - hx as f32;
            let dy = b.y as f32 - hy as f32;
            let from_home = (dx * dx + dy * dy).sqrt();
            quality *= 1.0 + 0.6 / (1.0 + from_home / 18.0);
        }

        // Long enough that the trip pays for itself. At five ticks of work
        // against fifteen of walking, three quarters of every foraging journey
        // was travel, and a household never accumulated a surplus to store —
        // the same defect that stopped anybody building a shelter at M2, in a
        // different place.
        let node_kind = node_kind_for(kind);
        let (goal, work, yield_bias) = match kind {
            BeliefKind::ForageNode => (Goal::GatherForage, 10, 1.0),
            // Wheat is worth more per unit and it keeps, so a known field beats
            // berries even at a distance — which is the whole point of grain.
            BeliefKind::SoilPatch => (Goal::HarvestWheat, 10, 2.4),
            _ => (Goal::SlaughterSheep, 1, 1.5),
        };
        if !ctx.cfg.features.wheat && node_kind == NodeKind::Wheat {
            continue;
        }
        if !ctx.cfg.features.sheep && node_kind == NodeKind::Sheep {
            continue;
        }

        let mut steps = vec![
            Step::new(Goal::MoveTo, Target::Tile(b.x, b.y), t),
            Step::new(goal, Target::Node(node), work),
        ];
        // One call buys many ticks of coherent behaviour (§5.5): fetching and
        // then eating is one plan, not two decisions.
        if c.hunger < 45.0 {
            steps.push(Step::new(Goal::EatFromInventory, Target::None, 1));
        }

        // The errand is the round trip plus the work, not just the walk out.
        let errand = t + work;
        let drive = pressure(
            c.hunger + c.inventory.food_value().min(60.0),
            n.hunger_decay_per_tick,
            errand,
        );
        let cand = Candidate {
            score: drive * quality * 2.4 * yield_bias,
            steps,
            addresses: Addresses::Food,
            rationale: format!(
                "{} at {},{} — {}.",
                kind.as_str().replace('_', " ").to_lowercase(),
                b.x, b.y,
                b.provenance(ctx.tick)
            ),
            confidence: conf,
        };
        if best.as_ref().is_none_or(|x| cand.score > x.score) {
            best = Some(cand);
        }
    }
    best
}

/// Planting, discounted for the ~72 ticks before there is anything to eat.
///
/// The discount is the honest expression of what a utility policy *is*: value
/// arriving later is worth less now. At the default rate a harvest three days
/// out is worth roughly a third of the same food in hand, which is why a
/// creature with any pressing need will always do something else first. Whether
/// a well-fed one ever gets round to it is the measurement M2 reports.
fn plant_run(c: &Creature, ctx: &PolicyCtx, needs: &NeedProfile) -> Option<Candidate> {
    if !ctx.cfg.features.wheat || c.life_stage == LifeStage::Infant {
        return None;
    }
    let growth = ctx.cfg.resources.wheat_growth_ticks as f32;
    // Per-tick discount: a creature that lives 672 ticks and might die tomorrow
    // does not value next week at par.
    let discount = 0.985f32.powf(growth);

    let here = ctx.world.at(c.x, c.y) == Terrain::Soil;
    let (tx, ty, travel) = if here {
        (c.x, c.y, 0)
    } else {
        let i = knowledge::best_of(
            &c.beliefs, &[BeliefKind::SoilPatch], (c.x, c.y), ctx.tick, needs, &ctx.cfg.knowledge,
        )?;
        let b = &c.beliefs[i];
        if ctx.world.at(b.x, b.y) != Terrain::Soil {
            return None;
        }
        (b.x, b.y, travel_ticks(c, ctx.cfg, (b.x, b.y)))
    };
    if ctx.world.nodes.iter().any(|nd| {
        nd.kind == NodeKind::Wheat && nd.x == tx && nd.y == ty && nd.quantity > 0.0
    }) {
        return None;
    }

    // 24 grain at ~11 nutrition is a large payoff; the discount is what makes
    // it lose to a handful of berries available now.
    let payoff = 24.0 * ItemKind::Grain.nutrition() / 100.0;
    let urgency_penalty = 1.0 - needs.food.max(needs.water).max(needs.shelter);
    let score = payoff * discount * (0.4 + 1.1 * c.traits.industry) * urgency_penalty.max(0.0)
        / (1.0 + travel as f32 / 8.0);

    let mut steps = Vec::new();
    if travel > 0 {
        steps.push(Step::new(Goal::MoveTo, Target::Tile(tx, ty), travel));
    }
    steps.push(Step::new(Goal::PlantWheat, Target::Tile(tx, ty), ctx.cfg.actions.plant_ticks));

    Some(Candidate {
        score,
        steps,
        addresses: Addresses::Food,
        rationale: format!("Soil at {tx},{ty}. Something to come back to."),
        confidence: 0.9,
    })
}

/// Revisit something doubtful. Cautious creatures check before they commit.
fn verify_run(c: &Creature, ctx: &PolicyCtx, needs: &NeedProfile) -> Option<Candidate> {
    let k = &ctx.cfg.knowledge;
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in c.beliefs.iter().enumerate() {
        let conf = b.confidence_at(ctx.tick, k);
        if !(0.05..0.55).contains(&conf) {
            continue;
        }
        let t = travel_ticks(c, ctx.cfg, (b.x, b.y));
        if t > 14 {
            continue;
        }
        // Worth checking in proportion to how much it would matter if true.
        let worth = needs.match_for(b.kind) * (1.0 - conf) / (1.0 + t as f32 / 6.0);
        if best.is_none_or(|(_, w)| worth > w) {
            best = Some((i, worth));
        }
    }
    let (i, worth) = best?;
    let b = &c.beliefs[i];
    let t = travel_ticks(c, ctx.cfg, (b.x, b.y));
    Some(Candidate {
        score: worth * 0.85 * (0.4 + 1.0 * c.traits.caution),
        steps: vec![Step::new(Goal::Verify, Target::Tile(b.x, b.y), t)],
        addresses: Addresses::Knowledge,
        rationale: format!("Check whether {} at {},{} is still there.",
                           b.kind.as_str().to_lowercase(), b.x, b.y),
        confidence: 0.5,
    })
}

fn target_quality_of(b: &crate::sim::knowledge::Belief, c: &Creature, ctx: &PolicyCtx) -> f32 {
    knowledge::target_quality(b, (c.x, c.y), ctx.tick, &ctx.cfg.knowledge)
}

/// Pressure from needs that no belief can currently serve — the thing that
/// turns exploration from a hobby into a necessity.
fn unserved_pressure(c: &Creature, ctx: &PolicyCtx, needs: &NeedProfile) -> (f32, Addresses) {
    let k = &ctx.cfg.knowledge;
    let mut total = 0.0;
    let mut worst = (0.0f32, Addresses::Knowledge);
    let has_fuel = c.inventory.total(ItemKind::Wood) >= ctx.cfg.actions.fire_wood_cost;
    let checks: [(f32, Addresses, &[BeliefKind]); 3] = [
        (needs.water, Addresses::Water, &[BeliefKind::Water]),
        (needs.food, Addresses::Food,
         &[BeliefKind::ForageNode, BeliefKind::SoilPatch, BeliefKind::SheepFlock]),
        (if has_fuel { 0.0 } else { needs.fuel }, Addresses::Fuel, &[BeliefKind::WoodNode]),
    ];
    for (want, tag, kinds) in checks {
        // A low gate on purpose: a creature should start looking for water when
        // it notices it is thirsty, not when it is already dying of it.
        if want < 0.1 {
            continue;
        }
        let served = knowledge::best_of(&c.beliefs, kinds, (c.x, c.y), ctx.tick, needs, k)
            .is_some_and(|i| target_quality_of(&c.beliefs[i], c, ctx) > 0.08);
        if !served {
            total += want;
            if want > worst.0 {
                worst = (want, tag);
            }
        }
    }
    (total, worst.1)
}

/// Where to go looking.
///
/// A creature heads for the compass direction it knows least about, with a
/// little jitter so a whole cohort does not walk the same line. Cheap, and it
/// produces the ragged outward expansion of the collective map that §10 expects
/// to see — rather than the aimless drift a pure random walk would give.
/// How far a creature can afford to wander.
///
/// Water cannot be carried. Firewood makes *warmth* portable (§4.4) and that is
/// the whole reason fuel exists, but thirst stays positional — so the river a
/// creature drinks at is a tether, and how long that tether is depends on how
/// recently it drank.
///
/// Without this, exploration is a monotonic outward walk: the least-known
/// direction is always further out, so creatures drift away from water and
/// never come back. Measured: the population sat a mean of 30 tiles from the
/// nearest water for an entire 2,000-tick run, spent most of its time walking
/// to a drink and back, and starved doing it — 50% of deaths — while food it
/// knew about sat unpicked.
///
/// Half the remaining thirst goes out and half comes back, with the distance
/// already spent getting away from water charged against the allowance.
fn explore_reach(c: &Creature, ctx: &PolicyCtx) -> i32 {
    let ticks_left = c.thirst / ctx.cfg.needs.thirst_decay_per_tick.max(0.01);
    let one_way_tiles = ticks_left * 0.45 * ctx.cfg.actions.move_speed / 1.25;

    let from_water = if ctx.world.at(c.x, c.y).is_fresh_water() {
        0.0
    } else {
        c.beliefs
            .iter()
            .filter(|b| b.kind == BeliefKind::Water)
            .map(|b| {
                let dx = (b.x as f32) - c.x as f32;
                let dy = (b.y as f32) - c.y as f32;
                (dx * dx + dy * dy).sqrt()
            })
            .fold(f32::MAX, f32::min)
    };
    // A creature that knows of no water at all is not tethered to anything and
    // has to go looking; the allowance is then its thirst alone.
    let spent = if from_water == f32::MAX { 0.0 } else { from_water };

    ((one_way_tiles - spent).max(3.0) as i32).min(ctx.cfg.actions.explore_distance as i32)
}

fn explore_target(c: &Creature, ctx: &PolicyCtx, rng: &mut ChaCha8Rng) -> (u32, u32) {
    const DIRS: [(i32, i32); 8] = [
        (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1), (0, -1), (1, -1),
    ];
    let reach = explore_reach(c, ctx);

    let mut best = (0usize, f32::MAX);
    for (di, (dx, dy)) in DIRS.iter().enumerate() {
        // How much is already believed to lie this way.
        let mut known = 0.0;
        for b in &c.beliefs {
            let bx = b.x as i32 - c.x as i32;
            let by = b.y as i32 - c.y as i32;
            if bx * dx + by * dy > 0 {
                known += 1.0;
            }
        }
        let jitter = rng.gen::<f32>() * 1.5;
        let score = known + jitter;
        if score < best.1 {
            best = (di, score);
        }
    }

    let (dx, dy) = DIRS[best.0];
    // Walk out until the ground stops being walkable, so the target is somewhere
    // a creature could actually stand.
    let mut target = (c.x, c.y);
    for step in 1..=reach {
        let nx = c.x as i64 + (dx * step) as i64;
        let ny = c.y as i64 + (dy * step) as i64;
        if !ctx.world.in_bounds(nx, ny) {
            break;
        }
        let (nx, ny) = (nx as u32, ny as u32);
        if !ctx.world.at(nx, ny).passable() {
            break;
        }
        target = (nx, ny);
    }
    target
}

/// Which resource actually stands on the kind of place a belief describes.
fn node_kind_for(kind: BeliefKind) -> NodeKind {
    match kind {
        BeliefKind::ForageNode => NodeKind::Forage,
        BeliefKind::SoilPatch => NodeKind::Wheat,
        BeliefKind::WoodNode => NodeKind::Wood,
        _ => NodeKind::Sheep,
    }
}

fn find_node(ctx: &PolicyCtx, kind: NodeKind, x: u32, y: u32) -> Option<u32> {
    ctx.nodes.find_at(ctx.world, kind, x, y)
}

/// Wrap the chosen steps in a committed plan.
///
/// The horizon is what a creature binds itself to before thinking again (§5.5),
/// and it is set from three things: the work the plan actually implies, the
/// creature's appetite for commitment, and — the interesting one — how much it
/// trusts the belief the plan rests on. Commit long on what you saw yourself;
/// stay tentative on hearsay.
fn finish(
    c: &Creature,
    ctx: &PolicyCtx,
    steps: Vec<Step>,
    rationale: String,
    confidence: f32,
    addresses: Addresses,
) -> Plan {
    let implied: u32 = steps.iter().map(|s| s.est_ticks).sum::<u32>().max(1);

    // Cautious creatures re-check often and pay for it; industrious ones commit.
    //
    // Both factors are centred on 1.0 rather than sitting below it. They were
    // 0.75+0.5i-0.35c and 0.55+0.45conf, whose product is under 1 for almost
    // every creature — so a horizon was routinely *shorter than the plan's own
    // work*, and a creature that set out on a 35-tick errand had its commitment
    // expire at tick 27, mid-swing, every time. Plans have to be able to finish
    // themselves; the traits decide how much further than that a creature is
    // willing to bind itself.
    let appetite = 0.9 + 0.5 * c.traits.industry - 0.3 * c.traits.caution;
    let trust = 0.7 + 0.5 * confidence.clamp(0.0, 1.0);
    // Per-goal caps bound each *step*, and the plan's cap is their sum.
    //
    // Taking the minimum instead would be the obvious reading of §5.5 and it is
    // wrong: a fetch-then-eat plan would inherit the one-tick cap on eating and
    // collapse to a single tick, which destroys multi-step plans — the thing
    // §5.5 calls the highest-leverage change available to the budget maths. A
    // step can still never commit beyond its own kind's limit.
    let cap: u32 = steps
        .iter()
        .map(|s| s.est_ticks.min(s.goal.horizon_cap(ctx.cfg)))
        .sum::<u32>()
        .max(1);

    // A creature in crisis commits to nothing: it does the next thing and looks
    // again (§5.5).
    let in_crisis = [c.hunger, c.thirst, c.warmth]
        .iter()
        .any(|&v| v < ctx.cfg.needs.critical_threshold);

    let horizon = if in_crisis {
        // §5.5 caps crisis responses at one tick, which is right for a
        // response that *is* one tick — eat what you carry, drink where you
        // stand. It is wrong for a crisis whose answer is a journey: clamping
        // to 1 makes a creature re-decide the same walk on every step of it,
        // and it never gets anywhere. Panic commits to exactly the errand and
        // no further, rather than to nothing at all.
        let immediate = steps.iter().all(|s| s.goal.horizon_cap(ctx.cfg) <= 1);
        if immediate {
            ctx.cfg.deliberation.horizon_cap_crisis.max(1)
        } else {
            implied.min(cap).max(1)
        }
    } else {
        ((implied as f32 * appetite * trust).round() as u32).clamp(1, cap.max(1))
    };

    Plan {
        steps,
        step_index: 0,
        horizon,
        ticks_remaining: horizon,
        set_tick: ctx.tick,
        rationale,
        tier: 1,
        addresses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::creature::testing::test_creature;
    use crate::sim::knowledge::{Belief, Estimate};
    use crate::sim::world::ResourceNode;
    use rand::SeedableRng;

    struct Fixture {
        world: World,
        structures: Structures,
        cache: WorldCache,
        nodes: NodeIndex,
        people: CreatureIndex,
        households: Households,
        courtships: Courtships,
        relationships: Relationships,
        cfg: WorldConfig,
    }

    impl Fixture {
        fn new() -> Self {
            let mut world = World {
                width: 64,
                height: 64,
                chunk_size: 32,
                seed: 1,
                tiles: vec![Terrain::Grass; 64 * 64],
                nodes: Vec::new(),
                founders: Vec::new(),
            };
            // A river down the east edge.
            for y in 0..64 {
                let i = world.idx(62, y);
                world.tiles[i] = Terrain::ShallowWater;
            }
            let cache = WorldCache::build(&world);
            let nodes = NodeIndex::new(&world, 8);
            let people = CreatureIndex::new(world.width, world.height, 8);
            Self {
                world,
                structures: Structures::new(),
                cache,
                nodes,
                people,
                households: Households::new(),
                courtships: Courtships::new(),
                relationships: Relationships::new(),
                cfg: WorldConfig::default(),
            }
        }

        /// Put these creatures on the map so the social candidates can see them.
        fn populate(&mut self, creatures: &[Creature]) {
            self.people.rebuild(creatures.iter(), 0, &self.cfg.knowledge);
        }

        fn rebuild_index(&mut self) {
            self.nodes.rebuild(&self.world);
        }

        fn ctx(&self, tick: i64, night: bool) -> PolicyCtx<'_> {
            PolicyCtx {
                world: &self.world,
                structures: &self.structures,
                cache: &self.cache,
                nodes: &self.nodes,
                people: &self.people,
                households: &self.households,
                courtships: &self.courtships,
                relationships: &self.relationships,
                cfg: &self.cfg,
                tick,
                night,
                recent_events: &[],
            }
        }

        fn rebuild_cache(&mut self) {
            self.cache = WorldCache::build(&self.world);
            self.nodes.rebuild(&self.world);
        }
    }

    fn belief(kind: BeliefKind, x: u32, y: u32, tick: i64) -> Belief {
        Belief {
            kind, x, y,
            estimate: Estimate::Plentiful,
            confidence: 1.0,
            learned_tick: tick,
            last_verified_tick: tick,
            source_creature_id: None,
            hops: 0,
            origin_creature_id: Some(1),
            origin_tick: tick,
        }
    }

    fn rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(99)
    }

    #[test]
    fn a_thirsty_creature_with_water_in_mind_goes_and_drinks() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.thirst = 8.0;
        c.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());

        assert_eq!(plan.steps.last().unwrap().goal, Goal::Drink);
        assert_eq!(plan.tier, 1, "Tier 1 is what produced this");
    }

    #[test]
    fn a_starving_creature_eats_what_it_is_already_carrying() {
        let f = Fixture::new();
        let mut c = test_creature();
        c.hunger = 6.0;
        c.inventory.add(ItemKind::Grain, 5.0, 0);

        let plan = decide(&c, &f.ctx(10, false), &mut rng());

        assert_eq!(plan.steps[0].goal, Goal::EatFromInventory,
                   "food in hand beats any trip");
    }

    #[test]
    fn hunger_beats_a_mild_want_of_firewood() {
        let mut f = Fixture::new();
        f.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 34, y: 30,
            quantity: 12.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        f.world.nodes.push(ResourceNode {
            kind: NodeKind::Wood, x: 33, y: 30,
            quantity: 40.0, max_quantity: 40.0, regen_rate: 0.0,
        });
        f.rebuild_index();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.hunger = 9.0;
        c.beliefs.push(belief(BeliefKind::ForageNode, 34, 30, 0));
        c.beliefs.push(belief(BeliefKind::WoodNode, 33, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());

        assert!(plan.steps.iter().any(|s| s.goal == Goal::GatherForage),
                "got {:?}", plan.steps.iter().map(|s| s.goal).collect::<Vec<_>>());
    }

    #[test]
    fn the_policy_acts_on_belief_and_not_on_what_is_actually_there() {
        // The forage is real, but this creature does not know about it. An
        // omniscient fallback would walk straight to it; this one must not.
        let mut f = Fixture::new();
        f.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 40, y: 40,
            quantity: 12.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        f.rebuild_index();
        let mut c = test_creature();
        (c.x, c.y) = (10, 10);
        c.hunger = 12.0;

        let plan = decide(&c, &f.ctx(10, false), &mut rng());

        assert!(!plan.steps.iter().any(|s| s.goal == Goal::GatherForage),
                "Tier 1 must not see through the fog");
        assert_eq!(plan.steps[0].goal, Goal::Explore, "it has to go and look");
    }

    #[test]
    fn a_stale_rumour_is_committed_to_less_deeply_than_a_firsthand_sighting() {
        // §5.5: commit long on what you saw yourself, stay tentative on hearsay.
        let f = Fixture::new();
        let c = test_creature();
        let steps = || {
            vec![
                Step::new(Goal::MoveTo, Target::Tile(44, 30), 14),
                Step::new(Goal::GatherForage, Target::Node(0), 5),
            ]
        };

        let firsthand =
            finish(&c, &f.ctx(200, false), steps(), String::new(), 1.0, Addresses::Food).horizon;
        let hearsay =
            finish(&c, &f.ctx(200, false), steps(), String::new(), 0.2, Addresses::Food).horizon;

        assert!(hearsay < firsthand,
                "hearsay {hearsay} should buy less commitment than firsthand {firsthand}");
    }

    #[test]
    fn a_multi_step_plan_may_commit_beyond_its_shortest_step() {
        // One decision has to buy many ticks of coherent behaviour, so a
        // one-tick eat on the end must not collapse the whole horizon.
        let f = Fixture::new();
        let mut c = test_creature();
        c.traits.industry = 0.9;
        c.traits.caution = 0.1;
        let plan = finish(
            &c,
            &f.ctx(10, false),
            vec![
                Step::new(Goal::MoveTo, Target::Tile(44, 30), 14),
                Step::new(Goal::GatherForage, Target::Node(0), 5),
                Step::new(Goal::EatFromInventory, Target::None, 1),
            ],
            String::new(),
            1.0,
            Addresses::Food,
        );
        assert!(plan.horizon > 5, "collapsed to {}", plan.horizon);
    }

    #[test]
    fn a_crisis_answered_on_the_spot_commits_to_a_single_tick() {
        // Standing in the shallows, dying of thirst: drink, then look again.
        let mut f = Fixture::new();
        let i = f.world.idx(30, 30);
        f.world.tiles[i] = Terrain::ShallowWater;
        f.rebuild_cache();

        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.thirst = 3.0;

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert_eq!(plan.steps[0].goal, Goal::Drink);
        assert_eq!(plan.horizon, f.cfg.deliberation.horizon_cap_crisis,
                   "panic does the next thing and looks again");
    }

    #[test]
    fn a_crisis_answered_by_a_journey_commits_to_the_journey() {
        // §5.5 caps crisis responses at one tick. Applied to a plan whose whole
        // answer is a two-day walk, that cancels the walk on every step of it.
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.thirst = 3.0;
        c.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert!(plan.horizon > 1, "a creature must be able to reach the water it set out for");
        assert!(
            plan.horizon <= plan.steps.iter().map(|s| s.est_ticks).sum::<u32>(),
            "but it commits to the errand and not a tick further"
        );
    }

    #[test]
    fn night_without_a_roof_gets_a_fire_out_of_the_wood_being_carried() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.warmth = 25.0;
        c.inventory.add(ItemKind::Wood, 8.0, 0);

        let plan = decide(&c, &f.ctx(21, true), &mut rng());

        assert_eq!(plan.steps[0].goal, Goal::BuildFire,
                   "carried fuel is what makes warmth portable (§4.4)");
    }

    #[test]
    fn a_roof_nearby_is_preferred_to_burning_the_wood() {
        let mut f = Fixture::new();
        f.structures.add(crate::sim::economy::Structure {
            id: 0, kind: crate::sim::economy::StructureKind::Shelter, x: 31, y: 30,
            condition: 1.0, capacity: 4, occupants: 0, household_id: None,
            built_tick: 0, fuel_remaining: 0.0, lit_until_tick: None, dirty: false,
        });
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.warmth = 25.0;
        c.inventory.add(ItemKind::Wood, 8.0, 0);

        let plan = decide(&c, &f.ctx(21, true), &mut rng());
        assert!(plan.steps.iter().any(|s| s.goal == Goal::Shelter),
                "got {:?}", plan.steps.iter().map(|s| s.goal).collect::<Vec<_>>());
    }

    #[test]
    fn a_well_fed_creature_on_soil_will_consider_planting() {
        let mut f = Fixture::new();
        for y in 28..34 {
            for x in 28..34 {
                let i = f.world.idx(x, y);
                f.world.tiles[i] = Terrain::Soil;
            }
        }
        f.rebuild_cache();

        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.traits.industry = 0.95;
        c.inventory.add(ItemKind::Grain, 8.0, 0);

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert!(plan.steps.iter().any(|s| s.goal == Goal::PlantWheat),
                "a safe, industrious creature standing on farmland has the option; \
                 got {:?}", plan.steps.iter().map(|s| s.goal).collect::<Vec<_>>());
    }

    #[test]
    fn a_hungry_creature_never_plants() {
        // The S6 hypothesis in miniature: the discount on a three-day payoff is
        // what makes a myopic policy myopic.
        let mut f = Fixture::new();
        for y in 28..34 {
            for x in 28..34 {
                let i = f.world.idx(x, y);
                f.world.tiles[i] = Terrain::Soil;
            }
        }
        f.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 32, y: 30,
            quantity: 12.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        f.rebuild_cache();

        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.traits.industry = 0.95;
        c.hunger = 14.0;
        c.beliefs.push(belief(BeliefKind::ForageNode, 32, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert!(!plan.steps.iter().any(|s| s.goal == Goal::PlantWheat),
                "food three days out loses to food now");
    }

    #[test]
    fn infants_only_eat_and_rest() {
        let f = Fixture::new();
        let mut c = test_creature();
        c.life_stage = LifeStage::Infant;
        c.hunger = 10.0;
        c.beliefs.push(belief(BeliefKind::ForageNode, 40, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert!(plan.steps.iter().all(|s| matches!(s.goal, Goal::Rest | Goal::EatFromInventory)),
                "an infant cannot work its way out of hunger");
    }

    #[test]
    fn the_same_situation_always_produces_the_same_plan() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.thirst = 20.0;
        c.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        let a = decide(&c, &f.ctx(10, false), &mut rng());
        let b = decide(&c, &f.ctx(10, false), &mut rng());

        assert_eq!(a.horizon, b.horizon);
        assert_eq!(a.rationale, b.rationale);
        assert_eq!(
            a.steps.iter().map(|s| s.goal).collect::<Vec<_>>(),
            b.steps.iter().map(|s| s.goal).collect::<Vec<_>>(),
        );
    }

    #[test]
    fn every_plan_carries_a_rationale_the_inspector_can_show() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        for thirst in [5.0, 50.0, 95.0] {
            c.thirst = thirst;
            let plan = decide(&c, &f.ctx(10, false), &mut rng());
            assert!(!plan.rationale.is_empty(), "a plan with no reason is unreadable");
            assert!(!plan.steps.is_empty(), "a creature is never left without a plan");
        }
    }

    #[test]
    fn a_creature_with_no_wood_and_a_stand_of_trees_in_mind_goes_and_chops() {
        // Wood is timber and fuel both (§4.4), so a creature holding neither a
        // night's fire nor a shelter's worth should fetch some.
        let mut f = Fixture::new();
        f.world.nodes.push(ResourceNode {
            kind: NodeKind::Wood, x: 40, y: 30,
            quantity: 40.0, max_quantity: 40.0, regen_rate: 0.06,
        });
        f.rebuild_index();

        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.warmth = 45.0;
        c.thirst = 80.0;
        c.hunger = 70.0;
        c.inventory.add(ItemKind::Forage, 3.0, 0);
        c.beliefs.push(belief(BeliefKind::WoodNode, 40, 30, 0));
        c.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        let plan = decide(&c, &f.ctx(10, false), &mut rng());
        assert!(
            plan.steps.iter().any(|s| s.goal == Goal::ChopWood),
            "got {:?} ({})",
            plan.steps.iter().map(|s| s.goal).collect::<Vec<_>>(),
            plan.rationale
        );
    }

    #[test]
    fn an_unattached_adult_beside_a_suitable_stranger_asks() {
        // §4.8's pairing is mutual and two-sided, so somebody has to put the
        // question first.
        let mut f = Fixture::new();
        let mut a = test_creature();
        a.id = 1;
        (a.x, a.y) = (30, 30);
        a.traits.boldness = 0.9;
        a.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        let mut b = test_creature();
        b.id = 2;
        (b.x, b.y) = (31, 30);
        b.sex = crate::sim::creature::Sex::Male;
        f.populate(&[a.clone(), b]);

        let plan = decide(&a, &f.ctx(10, false), &mut rng());
        assert!(
            plan.steps.iter().any(|s| s.goal == Goal::Court),
            "got {:?} ({})",
            plan.steps.iter().map(|s| s.goal).collect::<Vec<_>>(),
            plan.rationale
        );
    }

    #[test]
    fn nobody_courts_while_starving() {
        // Courtship is what a creature does when it is not busy dying.
        let mut f = Fixture::new();
        let mut a = test_creature();
        a.id = 1;
        (a.x, a.y) = (30, 30);
        a.hunger = 8.0;
        let mut b = test_creature();
        b.id = 2;
        (b.x, b.y) = (31, 30);
        b.sex = crate::sim::creature::Sex::Male;
        f.populate(&[a.clone(), b]);

        let plan = decide(&a, &f.ctx(10, false), &mut rng());
        assert!(!plan.steps.iter().any(|s| s.goal == Goal::Court));
    }

    #[test]
    fn a_thirsty_creature_far_from_water_does_not_wander_further() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (30, 30);
        c.beliefs.push(belief(BeliefKind::Water, 62, 30, 0));

        c.thirst = 100.0;
        let roaming = explore_reach(&c, &f.ctx(10, false));
        c.thirst = 30.0;
        let tethered = explore_reach(&c, &f.ctx(10, false));

        assert!(roaming > tethered,
                "a recent drink buys range: {roaming} should exceed {tethered}");
        assert!(tethered <= 6, "nearly dry and 32 tiles out, it should stay put: {tethered}");
    }

    #[test]
    fn exploration_heads_for_the_least_known_quarter() {
        let f = Fixture::new();
        let mut c = test_creature();
        (c.x, c.y) = (32, 32);
        // Everything known lies to the east.
        for dy in 0..6u32 {
            c.beliefs.push(belief(BeliefKind::ForageNode, 40 + dy, 32, 0));
        }
        let (tx, _ty) = explore_target(&c, &f.ctx(10, false), &mut rng());
        assert!(tx <= 32, "should not head into the quarter it already knows, went to {tx}");
    }

    #[test]
    fn urgency_rises_as_a_need_empties() {
        let n = NeedsConfig::default();
        let mut last = -1.0;
        for v in [100.0, 60.0, 30.0, 15.0, 0.0] {
            let u = urgency(v, &n);
            assert!(u > last, "urgency must rise monotonically as the need empties");
            last = u;
        }
    }
}
