//! The v1 action set (PRD §6): preconditions, costs, and execution.
//!
//! Every action carries its preconditions with it, checked by the engine before
//! the action is ever offered. That is invariant 3 — at M3 the model chooses
//! from a menu the engine has already validated, so an impossible action cannot
//! be hallucinated. Tier 1 uses exactly the same menu, which is what makes the
//! two tiers comparable for S6.
//!
//! Social actions (`COURT`, `GIVE_FOOD`, `SHARE_KNOWLEDGE`, `TEACH`, the
//! household store verbs) are deliberately absent: they need households, which
//! land at M4. `HERD_SHEEP` and `BUILD_PEN` go with them.

use crate::config::WorldConfig;
use crate::sim::creature::{Creature, ItemKind, LifeStage};
use crate::sim::economy::{Structure, StructureKind, Structures};
use crate::sim::knowledge::BeliefKind;
use crate::sim::social::{Bystander, CreatureIndex, Courtships, Households, SocialIntent};
use crate::sim::event::{Event, EventKind};
use crate::sim::pathfind::Pathfinder;
use crate::sim::terrain::Terrain;
use crate::sim::world::{NodeKind, ResourceNode, World};
use rand::Rng as _;
use rand_chacha::ChaCha8Rng;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Goal {
    MoveTo,
    Drink,
    EatFromInventory,
    Rest,
    Shelter,
    BuildFire,
    FeedFire,
    GatherForage,
    ChopWood,
    HarvestWheat,
    PlantWheat,
    TendCrop,
    SlaughterSheep,
    BuildShelter,
    RepairShelter,
    Explore,
    Verify,

    // ---- society (§4.10). Every one of these reaches into another creature,
    // so they record an intent that resolution applies (see `SocialIntent`).
    Court,
    AcceptCourtship,
    RejectCourtship,
    GiveFood,
    FeedInfant,
    Follow,
    JoinHousehold,
    LeaveHousehold,
    DepositToStore,
    WithdrawFromStore,
    EatFromStore,

    // ---- knowledge transmission (§4.11)
    ShareKnowledge,
    Teach,
}

impl Goal {
    pub fn as_str(self) -> &'static str {
        match self {
            Goal::MoveTo => "MOVE_TO",
            Goal::Drink => "DRINK",
            Goal::EatFromInventory => "EAT_FROM_INVENTORY",
            Goal::Rest => "REST",
            Goal::Shelter => "SHELTER",
            Goal::BuildFire => "BUILD_FIRE",
            Goal::FeedFire => "FEED_FIRE",
            Goal::GatherForage => "GATHER_FORAGE",
            Goal::ChopWood => "CHOP_WOOD",
            Goal::HarvestWheat => "HARVEST_WHEAT",
            Goal::PlantWheat => "PLANT_WHEAT",
            Goal::TendCrop => "TEND_CROP",
            Goal::SlaughterSheep => "SLAUGHTER_SHEEP",
            Goal::BuildShelter => "BUILD_SHELTER",
            Goal::RepairShelter => "REPAIR_SHELTER",
            Goal::Explore => "EXPLORE",
            Goal::Verify => "VERIFY",
            Goal::Court => "COURT",
            Goal::AcceptCourtship => "ACCEPT_COURTSHIP",
            Goal::RejectCourtship => "REJECT_COURTSHIP",
            Goal::GiveFood => "GIVE_FOOD",
            Goal::FeedInfant => "FEED_INFANT",
            Goal::Follow => "FOLLOW",
            Goal::JoinHousehold => "JOIN_HOUSEHOLD",
            Goal::LeaveHousehold => "LEAVE_HOUSEHOLD",
            Goal::DepositToStore => "DEPOSIT_TO_STORE",
            Goal::WithdrawFromStore => "WITHDRAW_FROM_STORE",
            Goal::EatFromStore => "EAT_FROM_STORE",
            Goal::ShareKnowledge => "SHARE_KNOWLEDGE",
            Goal::Teach => "TEACH",
        }
    }

    /// Which horizon cap applies (§5.5). You cannot commit to a courtship
    /// twenty ticks in advance, and you cannot commit to anything at all while
    /// in crisis.
    pub fn horizon_cap(self, cfg: &WorldConfig) -> u32 {
        let d = &cfg.deliberation;
        match self {
            Goal::MoveTo | Goal::Explore | Goal::Verify => d.horizon_cap_travel,
            Goal::GatherForage | Goal::ChopWood | Goal::HarvestWheat | Goal::SlaughterSheep => {
                d.horizon_cap_gather
            }
            Goal::BuildShelter | Goal::RepairShelter | Goal::PlantWheat | Goal::TendCrop => {
                d.horizon_cap_construction
            }
            Goal::Drink | Goal::EatFromInventory | Goal::EatFromStore => d.horizon_cap_crisis,
            // "You cannot commit to a courtship twenty ticks in advance" —
            // anything that depends on another creature still being there, and
            // still willing, is capped hard (§5.5).
            Goal::Court
            | Goal::AcceptCourtship
            | Goal::RejectCourtship
            | Goal::GiveFood
            | Goal::FeedInfant
            | Goal::ShareKnowledge
            | Goal::Teach
            | Goal::JoinHousehold
            | Goal::LeaveHousehold => d.horizon_cap_social,
            Goal::Follow => d.horizon_cap_travel,
            _ => d.horizon_cap_gather,
        }
    }

    /// Work that can hurt you (§4.6). Swinging an axe and killing a sheep are
    /// the two places a creature can be injured at M2.
    pub fn is_hazardous(self) -> bool {
        matches!(self, Goal::ChopWood | Goal::SlaughterSheep | Goal::BuildShelter)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    None,
    Tile(u32, u32),
    /// Index into `World::nodes`. That Vec is append-only for the life of a run
    /// — depleted nodes go to quantity zero and are reused in place — so an
    /// index stays a valid handle for as long as a plan holds one.
    Node(u32),
    Structure(i64),
    Creature(i64),
    Household(i64),
}

impl Target {
    pub fn tile(self, world: &World) -> Option<(u32, u32)> {
        match self {
            Target::Tile(x, y) => Some((x, y)),
            Target::Node(i) => world.nodes.get(i as usize).map(|n| (n.x, n.y)),
            _ => None,
        }
    }

    pub fn creature(self) -> Option<i64> {
        match self {
            Target::Creature(id) => Some(id),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Step {
    pub goal: Goal,
    pub target: Target,
    pub est_ticks: u32,
    pub elapsed: u32,
    /// Total taken so far by a multi-tick gather, so the event can be written
    /// once when the step finishes instead of once per tick.
    pub progress: f32,
    /// What a SHARE_KNOWLEDGE step is about. §4.11 calls the topic filter half
    /// of what makes sharing a decision rather than a transfer.
    pub topic: Option<BeliefKind>,
    /// Remaining tiles of a movement, consumed one or more per tick according
    /// to speed. Computed once when the step is reached, not per tick.
    pub path: Vec<(u32, u32)>,
    /// How far along `path` the creature has walked. An index rather than
    /// draining from the front, which would be quadratic on a long route.
    pub path_pos: usize,
    pub path_ready: bool,
}

impl Step {
    pub fn new(goal: Goal, target: Target, est_ticks: u32) -> Self {
        Self { goal, target, est_ticks, elapsed: 0, progress: 0.0, topic: None,
               path: Vec::new(), path_pos: 0, path_ready: false }
    }

    pub fn about(mut self, topic: BeliefKind) -> Self {
        self.topic = Some(topic);
        self
    }

    pub fn describe(&self, world: &World) -> String {
        match self.target.tile(world) {
            Some((x, y)) => format!("{} {},{}", self.goal.as_str(), x, y),
            None => self.goal.as_str().to_string(),
        }
    }
}

/// Why a plan stopped. Recorded on every early abort so the abandonment
/// breakdown can distinguish "the world changed" from "the plan was bad" (§10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum AbortReason {
    Completed,
    HorizonExpired,
    TargetDepleted,
    TargetGone,
    Unreachable,
    PreconditionFailed,
    Encumbered,
    HungerCritical,
    ThirstCritical,
    WarmthCritical,
    Superseded,
    Died,
}

impl AbortReason {
    pub fn as_str(self) -> &'static str {
        match self {
            AbortReason::Completed => "COMPLETED",
            AbortReason::HorizonExpired => "HORIZON_EXPIRED",
            AbortReason::TargetDepleted => "TARGET_DEPLETED",
            AbortReason::TargetGone => "TARGET_GONE",
            AbortReason::Unreachable => "UNREACHABLE",
            AbortReason::PreconditionFailed => "PRECONDITION_FAILED",
            AbortReason::Encumbered => "ENCUMBERED",
            AbortReason::HungerCritical => "HUNGER_CRITICAL",
            AbortReason::ThirstCritical => "THIRST_CRITICAL",
            AbortReason::WarmthCritical => "WARMTH_CRITICAL",
            AbortReason::Superseded => "SUPERSEDED",
            AbortReason::Died => "DIED",
        }
    }

    /// A hard signal aborts a committed plan immediately; soft ones only
    /// accumulate as pressure until the horizon expires (§5.5).
    pub fn is_hard(self) -> bool {
        matches!(
            self,
            AbortReason::HungerCritical
                | AbortReason::ThirstCritical
                | AbortReason::WarmthCritical
                | AbortReason::TargetGone
                | AbortReason::Unreachable
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Still working; the step keeps the creature for another tick.
    Working,
    /// This step finished; advance to the next.
    StepComplete,
    /// The step cannot proceed; the plan aborts with this reason.
    Failed(AbortReason),
}

/// The read-only social surroundings: who else is here, which households
/// exist, and who has been asked what. Bundled rather than passed as five
/// arguments because every precondition below needs some of it.
#[derive(Clone, Copy)]
pub struct SocialView<'a> {
    pub people: &'a CreatureIndex,
    pub households: &'a Households,
    pub courtships: &'a Courtships,
}

impl<'a> SocialView<'a> {
    /// The other creature, if it is close enough to act on at all.
    pub fn within_reach(&self, c: &Creature, id: i64, reach: u32) -> Option<Bystander> {
        let p = self.people.get(id)?;
        if near(p.x, p.y, c.x, c.y, reach) { Some(*p) } else { None }
    }
}

/// Everything an action needs besides the creature itself. Held as separate
/// borrows so a creature can be mutated while the world it acts on is too.
pub struct ActionCtx<'a> {
    pub world: &'a mut World,
    pub structures: &'a mut Structures,
    pub pathfinder: &'a mut Pathfinder,
    pub cfg: &'a WorldConfig,
    pub tick: i64,
    pub rng: &'a mut ChaCha8Rng,
    pub events: &'a mut Vec<Event>,
    pub night: bool,
    /// Food that spoiled and work that was done, accumulated for `tick_stats`.
    pub gathered: f32,
    pub eaten: f32,

    // ---- society -------------------------------------------------------
    pub households: &'a mut Households,
    pub people: &'a CreatureIndex,
    pub courtships: &'a Courtships,
    /// Two-sided acts, applied in resolution where both parties are reachable.
    pub intents: &'a mut Vec<SocialIntent>,
}

impl ActionCtx<'_> {
    fn social(&self) -> SocialView<'_> {
        SocialView {
            people: self.people,
            households: self.households,
            courtships: self.courtships,
        }
    }
}

// --------------------------------------------------------------- preconditions

/// Whether a goal is legal for this creature right now (invariant 3).
pub fn is_legal(
    c: &Creature,
    goal: Goal,
    target: Target,
    world: &World,
    structures: &Structures,
    social: SocialView<'_>,
    cfg: &WorldConfig,
) -> bool {
    // Infants cannot gather or work; they follow a guardian and are fed (§4.7).
    // What they *can* do is eat, rest, take shelter and stay close to whoever
    // is keeping them alive.
    if !c.life_stage.can_work()
        && !matches!(
            goal,
            Goal::EatFromInventory | Goal::Rest | Goal::Shelter | Goal::MoveTo | Goal::Follow
        )
    {
        return false;
    }

    match goal {
        Goal::MoveTo | Goal::Explore | Goal::Verify => target
            .tile(world)
            .is_some_and(|(x, y)| world.in_bounds(x as i64, y as i64) && world.at(x, y).passable()),

        Goal::Drink => target
            .tile(world)
            .is_some_and(|(x, y)| world.at(x, y).is_fresh_water()),

        Goal::EatFromInventory => c.inventory.total_food() > 0.0,

        Goal::Rest => true,

        Goal::Shelter => structures
            .nearest_shelter(c.x, c.y, 1)
            .is_some_and(|s| s.has_room()),

        Goal::BuildFire => {
            cfg.features.fires && c.inventory.total(ItemKind::Wood) >= cfg.actions.fire_wood_cost
        }

        Goal::FeedFire => {
            cfg.features.fires
                && c.inventory.total(ItemKind::Wood) >= 1.0
                && structures
                    .items
                    .iter()
                    .any(|s| s.kind == StructureKind::Fire && near(s.x, s.y, c.x, c.y, 1))
        }

        Goal::GatherForage => node_has(world, target, NodeKind::Forage) && has_carry_room(c, cfg),
        Goal::ChopWood => node_has(world, target, NodeKind::Wood) && has_carry_room(c, cfg),
        Goal::HarvestWheat => {
            cfg.features.wheat && node_has(world, target, NodeKind::Wheat) && has_carry_room(c, cfg)
        }
        Goal::SlaughterSheep => {
            cfg.features.sheep && node_has(world, target, NodeKind::Sheep) && has_carry_room(c, cfg)
        }

        Goal::PlantWheat => {
            cfg.features.wheat
                && target.tile(world).is_some_and(|(x, y)| {
                    world.at(x, y) == Terrain::Soil
                        && !world.nodes.iter().any(|n| {
                            n.kind == NodeKind::Wheat && n.x == x && n.y == y && n.quantity > 0.0
                        })
                })
        }

        Goal::TendCrop => {
            cfg.features.wheat
                && matches!(target, Target::Node(i)
                    if world.nodes.get(i as usize).is_some_and(|n| {
                        n.kind == NodeKind::Wheat && n.quantity < n.max_quantity
                    }))
        }

        Goal::BuildShelter => {
            c.inventory.total(ItemKind::Wood) >= cfg.actions.shelter_wood_cost
                && target.tile(world).is_some_and(|(x, y)| {
                    let t = world.at(x, y);
                    t.passable() && !t.is_water()
                })
        }

        Goal::RepairShelter => {
            c.inventory.total(ItemKind::Wood) >= 1.0
                && matches!(target, Target::Structure(id)
                    if structures.get(id).is_some_and(|s| {
                        s.kind == StructureKind::Shelter && s.condition < 0.99
                            && near(s.x, s.y, c.x, c.y, 1)
                    }))
        }

        // ---- society ----------------------------------------------------
        //
        // Courting is free and does not require a home. §4.8 gates
        // *reproduction* on a shared shelter with capacity, not courtship —
        // which is the right way round: a couple with nowhere to live has a
        // reason to build one, whereas requiring the shelter first means
        // nobody ever pairs and nobody ever needs a shelter.
        Goal::Court => {
            let reach = cfg.actions.social_reach;
            c.is_courtable(&cfg.lifespan, 0)
                && target.creature().is_some_and(|id| {
                    social.within_reach(c, id, reach).is_some_and(|p| {
                        p.sex != c.sex
                            && !p.paired
                            && p.life_stage == LifeStage::Adult
                            && p.health > 40.0
                    })
                })
        }

        Goal::AcceptCourtship | Goal::RejectCourtship => {
            c.mate_id.is_none()
                && target.creature().is_some_and(|id| {
                    social
                        .courtships
                        .pending_for(c.id)
                        .is_some_and(|o| o.from == id)
                        && social.within_reach(c, id, cfg.actions.social_reach).is_some()
                })
        }

        Goal::GiveFood => {
            c.inventory.total_food() > 0.0
                && target
                    .creature()
                    .is_some_and(|id| social.within_reach(c, id, cfg.actions.social_reach).is_some())
        }

        // An infant must be fed by a parent or household member or it dies
        // (§4.7). Anyone with food who is standing next to a hungry infant may
        // do it; whether they choose to is the interesting part.
        Goal::FeedInfant => {
            c.inventory.total_food() > 0.0
                && target.creature().is_some_and(|id| {
                    social
                        .within_reach(c, id, cfg.actions.social_reach)
                        .is_some_and(|p| p.life_stage == LifeStage::Infant)
                })
        }

        Goal::Follow => target.creature().is_some_and(|id| social.people.get(id).is_some()),

        // Membership, not tonight's occupancy. This is what makes a household a
        // thing you belong to rather than a bed you happen to be in.
        Goal::JoinHousehold => {
            c.household_id.is_none()
                && matches!(target, Target::Household(id) if social.households.get(id).is_some())
        }
        Goal::LeaveHousehold => c.household_id.is_some(),

        Goal::DepositToStore => {
            c.inventory.weight() > 0.0 && at_own_hearth(c, social, structures, cfg)
        }
        Goal::WithdrawFromStore => {
            has_carry_room(c, cfg)
                && at_own_hearth(c, social, structures, cfg)
                && c.household_id
                    .and_then(|h| social.households.get(h))
                    .is_some_and(|h| h.store.weight() > 0.0)
        }
        Goal::EatFromStore => {
            at_own_hearth(c, social, structures, cfg)
                && c.household_id
                    .and_then(|h| social.households.get(h))
                    .is_some_and(|h| h.stored_food() > 0.0)
        }

        // ---- knowledge transmission (§4.11) ------------------------------
        Goal::ShareKnowledge => {
            cfg.features.knowledge_sharing
                && !c.beliefs.is_empty()
                && target
                    .creature()
                    .is_some_and(|id| social.within_reach(c, id, cfg.actions.social_reach).is_some())
        }

        // Household-only, adult to young. The restriction is what makes
        // teaching a thing a lineage does rather than a thing anybody does.
        Goal::Teach => {
            cfg.features.teaching
                && c.life_stage != LifeStage::Infant
                && !c.beliefs.is_empty()
                && c.household_id.is_some()
                && target.creature().is_some_and(|id| {
                    social
                        .within_reach(c, id, cfg.actions.social_reach.max(2))
                        .is_some_and(|p| {
                            p.household_id == c.household_id && p.life_stage != LifeStage::Elder
                        })
                })
        }
    }
}

/// Standing at the shelter of the household this creature belongs to.
fn at_own_hearth(
    c: &Creature,
    social: SocialView<'_>,
    structures: &Structures,
    cfg: &WorldConfig,
) -> bool {
    let Some(h) = c.household_id.and_then(|id| social.households.get(id)) else {
        return false;
    };
    let Some(shelter_id) = h.shelter_id else {
        return false;
    };
    structures
        .get(shelter_id)
        .is_some_and(|s| near(s.x, s.y, c.x, c.y, cfg.actions.social_reach.max(1)))
}

fn node_has(world: &World, target: Target, kind: NodeKind) -> bool {
    matches!(target, Target::Node(i)
        if world.nodes.get(i as usize).is_some_and(|n| n.kind == kind && n.quantity > 0.01))
}

fn has_carry_room(c: &Creature, cfg: &WorldConfig) -> bool {
    c.inventory.weight() < c.carry_capacity(cfg) - 0.01
}

#[inline]
fn near(ax: u32, ay: u32, bx: u32, by: u32, r: u32) -> bool {
    ax.abs_diff(bx) <= r && ay.abs_diff(by) <= r
}

// ------------------------------------------------------------------- execution

/// Advance one tick of the creature's current step.
pub fn execute(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    step.elapsed += 1;

    // Anything that is not standing still means leaving shelter.
    if !matches!(step.goal, Goal::Rest | Goal::Shelter | Goal::EatFromInventory) {
        release_shelter(c, ctx.structures);
    }

    let out = match step.goal {
        Goal::MoveTo | Goal::Explore | Goal::Verify => advance_move(c, step, ctx),
        Goal::Drink => drink(c, step, ctx),
        Goal::EatFromInventory => eat(c, ctx),
        Goal::Rest => rest(c, step, ctx),
        Goal::Shelter => shelter(c, ctx),
        Goal::BuildFire => build_fire(c, ctx),
        Goal::FeedFire => feed_fire(c, ctx),
        Goal::GatherForage => harvest_node(c, step, ctx, NodeKind::Forage),
        Goal::ChopWood => harvest_node(c, step, ctx, NodeKind::Wood),
        Goal::HarvestWheat => harvest_node(c, step, ctx, NodeKind::Wheat),
        Goal::SlaughterSheep => slaughter(c, step, ctx),
        Goal::PlantWheat => plant(c, step, ctx),
        Goal::TendCrop => tend(c, step, ctx),
        Goal::BuildShelter => build_shelter(c, step, ctx),
        Goal::RepairShelter => repair_shelter(c, step, ctx),

        Goal::Court => court(c, step, ctx),
        Goal::AcceptCourtship => answer_courtship(c, step, ctx, true),
        Goal::RejectCourtship => answer_courtship(c, step, ctx, false),
        Goal::GiveFood => hand_over(c, step, ctx, false),
        Goal::FeedInfant => hand_over(c, step, ctx, true),
        Goal::Follow => follow(c, step, ctx),
        Goal::JoinHousehold => join_household(c, step, ctx),
        Goal::LeaveHousehold => leave_household(c, ctx),
        Goal::DepositToStore => deposit(c, ctx),
        Goal::WithdrawFromStore => withdraw(c, ctx),
        Goal::EatFromStore => eat_from_store(c, ctx),
        Goal::ShareKnowledge => share_knowledge(c, step, ctx),
        Goal::Teach => teach(c, step, ctx),
    };

    if step.goal.is_hazardous() && matches!(out, Outcome::Working | Outcome::StepComplete) {
        maybe_injure(c, ctx);
    }
    out
}

fn release_shelter(c: &mut Creature, structures: &mut Structures) {
    if let Some(id) = c.in_shelter.take() {
        if let Some(s) = structures.get_mut(id) {
            s.occupants = s.occupants.saturating_sub(1);
            s.dirty = true;
        }
    }
}

/// Movement along a committed path. The route is computed once when the step is
/// first reached rather than per tick — a creature that re-pathed every tick
/// would spend the whole Fast-Forward budget in A*.
fn advance_move(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Some(goal) = step.target.tile(ctx.world) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if (c.x, c.y) == goal {
        return arrive(c, step, ctx);
    }

    if !step.path_ready {
        step.path_ready = true;
        step.path_pos = 0;
        match ctx.pathfinder.find(ctx.world, (c.x, c.y), goal) {
            Some(p) if !p.is_empty() => step.path = p,
            Some(_) => return arrive(c, step, ctx),
            None => return Outcome::Failed(AbortReason::Unreachable),
        }
    }

    // Spend this tick's movement budget along the path. Fatigue slows a
    // creature down, which is what makes rest worth the ticks it costs.
    let mut budget = c.speed(ctx.cfg);
    let mut moved = false;
    while budget > 0.0 && step.path_pos < step.path.len() {
        let (nx, ny) = step.path[step.path_pos];
        let diagonal = nx != c.x && ny != c.y;
        let cost = ctx.world.at(nx, ny).move_cost()
            * if diagonal { std::f32::consts::SQRT_2 } else { 1.0 };
        if cost > budget && moved {
            break;
        }
        budget -= cost;
        c.x = nx;
        c.y = ny;
        moved = true;
        step.path_pos += 1;
    }

    if step.path_pos >= step.path.len() {
        // A capped search returns a partial route; re-path from where it ended
        // rather than declaring arrival somewhere the creature is not.
        if (c.x, c.y) != goal {
            step.path_ready = false;
            if step.elapsed >= step.est_ticks.saturating_mul(3).max(48) {
                return Outcome::Failed(AbortReason::Unreachable);
            }
            return Outcome::Working;
        }
        return arrive(c, step, ctx);
    }
    Outcome::Working
}

fn arrive(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    // The looking itself happens in the observation phase, which runs every
    // tick for every creature; arriving is what puts the creature in a position
    // to see. If the belief was wrong, that is where it gets corrected — and
    // where the creature learns its informant was out of date.
    if step.goal == Goal::Verify {
        ctx.events.push(Event::new(ctx.tick, EventKind::Verified, c.id).at(c.x, c.y));
    }
    Outcome::StepComplete
}

fn drink(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    if !ctx.world.at(c.x, c.y).is_fresh_water() {
        // Standing where the water was believed to be and finding dry ground.
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    c.thirst = (c.thirst + ctx.cfg.actions.drink_restore).min(100.0);
    ctx.events.push(Event::new(ctx.tick, EventKind::Drank, c.id).at(c.x, c.y));
    let _ = step;
    Outcome::StepComplete
}

fn eat(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    // Oldest batch first, so a creature eats what is about to rot. That single
    // rule is most of what makes perishables behave differently from grain.
    let Some(oldest) = c.inventory.oldest_food().copied() else {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    };
    let want = ctx.cfg.actions.eat_portion.min(oldest.quantity);
    let got = c.inventory.take(oldest.kind, want);
    if got <= 0.0 {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }

    c.hunger = (c.hunger + got * oldest.kind.nutrition()).min(100.0);
    ctx.eaten += got;
    ctx.events.push(
        Event::new(ctx.tick, EventKind::Ate, c.id)
            .at(c.x, c.y)
            .with("kind", oldest.kind.as_str())
            .with_num("qty", got),
    );
    Outcome::StepComplete
}

fn rest(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let sheltered = c.in_shelter.is_some();
    let restore = if sheltered {
        ctx.cfg.actions.rest_restore_sheltered
    } else {
        ctx.cfg.actions.rest_restore
    };
    c.fatigue = (c.fatigue + restore).min(100.0);

    if c.fatigue >= 99.0 || step.elapsed >= step.est_ticks {
        ctx.events.push(
            Event::new(ctx.tick, EventKind::Rested, c.id)
                .at(c.x, c.y)
                .with_int("ticks", step.elapsed as i64),
        );
        return Outcome::StepComplete;
    }
    Outcome::Working
}

fn shelter(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    if c.in_shelter.is_some() {
        return Outcome::StepComplete;
    }
    let Some(id) = ctx.structures.nearest_shelter(c.x, c.y, 1).map(|s| s.id) else {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    };
    if let Some(s) = ctx.structures.get_mut(id) {
        s.occupants += 1;
        s.dirty = true;
    }
    c.in_shelter = Some(id);
    ctx.events.push(Event::new(ctx.tick, EventKind::Sheltered, c.id).at(c.x, c.y).target(id));
    Outcome::StepComplete
}

fn build_fire(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    if !ctx.cfg.features.fires {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    let cost = ctx.cfg.actions.fire_wood_cost;
    if c.inventory.total(ItemKind::Wood) < cost {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    let spent = c.inventory.take(ItemKind::Wood, cost);

    let id = ctx.structures.add(Structure {
        id: 0,
        kind: StructureKind::Fire,
        x: c.x,
        y: c.y,
        condition: 1.0,
        capacity: 0,
        occupants: 0,
        household_id: None,
        built_tick: ctx.tick,
        fuel_remaining: spent,
        lit_until_tick: None,
        dirty: true,
    });
    ctx.events.push(
        Event::new(ctx.tick, EventKind::FireLit, c.id)
            .at(c.x, c.y)
            .target(id)
            .with_num("wood", spent),
    );
    Outcome::StepComplete
}

fn feed_fire(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    let Some(id) = ctx
        .structures
        .items
        .iter()
        .filter(|s| s.kind == StructureKind::Fire && near(s.x, s.y, c.x, c.y, 1))
        .map(|s| s.id)
        .min()
    else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let wood = c.inventory.take(ItemKind::Wood, 2.0);
    if wood <= 0.0 {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    if let Some(s) = ctx.structures.get_mut(id) {
        s.fuel_remaining += wood;
        s.lit_until_tick = None;
        s.dirty = true;
    }
    ctx.events.push(
        Event::new(ctx.tick, EventKind::FireFed, c.id).at(c.x, c.y).target(id).with_num("wood", wood),
    );
    Outcome::StepComplete
}

/// Gathering, chopping and harvesting are the same loop with different rates,
/// yields and item kinds.
fn harvest_node(
    c: &mut Creature,
    step: &mut Step,
    ctx: &mut ActionCtx,
    kind: NodeKind,
) -> Outcome {
    let Target::Node(i) = step.target else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let Some(node) = ctx.world.nodes.get(i as usize) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if node.kind != kind {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    if (c.x, c.y) != (node.x, node.y) {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    // The stale-belief payoff: the creature walked here on what it believed and
    // found the clearing stripped. Its belief gets corrected by observation on
    // the same tick, so the disappointment is recorded rather than merely felt.
    if node.quantity <= 0.01 {
        return Outcome::Failed(AbortReason::TargetDepleted);
    }
    if !has_carry_room(c, ctx.cfg) {
        return Outcome::Failed(AbortReason::Encumbered);
    }

    let a = &ctx.cfg.actions;
    let (rate, item, ev) = match kind {
        NodeKind::Forage => (
            a.gather_forage_per_tick * if ctx.night { a.night_forage_scale } else { 1.0 },
            ItemKind::Forage,
            EventKind::Gathered,
        ),
        NodeKind::Wood => (a.chop_wood_per_tick, ItemKind::Wood, EventKind::Chopped),
        NodeKind::Wheat => (a.harvest_wheat_per_tick, ItemKind::Grain, EventKind::Harvested),
        NodeKind::Sheep => return slaughter(c, step, ctx),
    };

    let room = c.carry_capacity(ctx.cfg) - c.inventory.weight();
    let take = (rate * c.life_stage.work_rate())
        .min(ctx.world.nodes[i as usize].quantity)
        .min(room)
        .max(0.0);

    ctx.world.nodes[i as usize].quantity -= take;
    c.inventory.add(item, take, ctx.tick);
    ctx.gathered += take;
    step.progress += take;

    let node_empty = ctx.world.nodes[i as usize].quantity <= 0.01;
    if node_empty || !has_carry_room(c, ctx.cfg) || step.elapsed >= step.est_ticks {
        ctx.events.push(
            Event::new(ctx.tick, ev, c.id)
                .at(c.x, c.y)
                .with("kind", item.as_str())
                .with_num("qty", step.progress)
                .with("left", if node_empty { "none" } else { "some" }),
        );
        return Outcome::StepComplete;
    }
    Outcome::Working
}

fn slaughter(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let Target::Node(i) = step.target else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let Some(node) = ctx.world.nodes.get_mut(i as usize) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if node.kind != NodeKind::Sheep || node.quantity <= 0.0 {
        return Outcome::Failed(AbortReason::TargetDepleted);
    }
    if !near(node.x, node.y, c.x, c.y, 1) {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    node.quantity = 0.0;

    // A slaughtered sheep is more meat than one creature can eat before it
    // rots. §4.4 expects that to create a reason to feed the household with no
    // rule about sharing; the sharing itself needs households, so at M2 the
    // surplus simply spoils — and the waste shows up in the economy report,
    // which is the honest version of the same signal.
    let yield_ = ctx.cfg.actions.slaughter_yield;
    let room = (c.carry_capacity(ctx.cfg) - c.inventory.weight()).max(0.0);
    let kept = yield_.min(room);
    c.inventory.add(ItemKind::Meat, kept, ctx.tick);
    ctx.gathered += kept;

    ctx.events.push(
        Event::new(ctx.tick, EventKind::Slaughtered, c.id)
            .at(c.x, c.y)
            .with_num("meat", kept)
            .with_num("wasted", yield_ - kept),
    );
    Outcome::StepComplete
}

fn plant(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Some((x, y)) = step.target.tile(ctx.world) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if (c.x, c.y) != (x, y) || ctx.world.at(x, y) != Terrain::Soil {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    if step.elapsed < ctx.cfg.actions.plant_ticks {
        return Outcome::Working;
    }

    // A planted crop starts at nothing and matures over ~3 days. Reusing the
    // regrowth rate for growth is what gives wheat its latency without a
    // separate growth-stage machine.
    let max = 24.0;
    let node = ResourceNode {
        kind: NodeKind::Wheat,
        x,
        y,
        quantity: 0.0,
        max_quantity: max,
        regen_rate: max / ctx.cfg.resources.wheat_growth_ticks.max(1) as f32,
    };
    if let Some(slot) = ctx
        .world
        .nodes
        .iter_mut()
        .find(|n| n.kind == NodeKind::Wheat && n.x == x && n.y == y)
    {
        *slot = node;
    } else {
        ctx.world.nodes.push(node);
    }

    ctx.events.push(Event::new(ctx.tick, EventKind::Planted, c.id).at(x, y));
    Outcome::StepComplete
}

fn tend(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Target::Node(i) = step.target else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let Some(node) = ctx.world.nodes.get_mut(i as usize) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if node.kind != NodeKind::Wheat {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    if !near(node.x, node.y, c.x, c.y, 1) {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    // Tending brings the harvest forward rather than raising the ceiling.
    node.quantity = (node.quantity + node.regen_rate * 2.0).min(node.max_quantity);

    if step.elapsed >= ctx.cfg.actions.tend_ticks {
        ctx.events.push(Event::new(ctx.tick, EventKind::Tended, c.id).at(node.x, node.y));
        return Outcome::StepComplete;
    }
    Outcome::Working
}

fn build_shelter(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Some((x, y)) = step.target.tile(ctx.world) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if (c.x, c.y) != (x, y) {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    if c.inventory.total(ItemKind::Wood) < ctx.cfg.actions.shelter_wood_cost {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    if step.elapsed < ctx.cfg.actions.shelter_build_ticks {
        return Outcome::Working;
    }

    let spent = c.inventory.take(ItemKind::Wood, ctx.cfg.actions.shelter_wood_cost);

    // Raising a roof founds a household, or gives an existing one its home.
    //
    // A shelter without a household is just a building: nothing can be stored
    // in it, nobody belongs to it, and §4.8's reproduction gate — which reads a
    // *household* store — can never be satisfied. So the two are created
    // together, which is also what §4.10 means by "a shelter plus its members".
    let household_id = match c.household_id {
        Some(h) => {
            if let Some(existing) = ctx.households.get_mut(h) {
                if existing.shelter_id.is_none() {
                    existing.shelter_id = None; // filled in below
                    existing.dirty = true;
                }
            }
            h
        }
        None => {
            let h = ctx.households.found(None, c.id, c.mate_id, ctx.tick, ctx.cfg);
            c.household_id = Some(h);
            c.dirty = true;
            ctx.events.push(
                Event::new(ctx.tick, EventKind::HouseholdFounded, c.id).at(x, y).target(h),
            );
            // A mate is a member of the same household by definition.
            if let Some(mate) = c.mate_id {
                ctx.intents
                    .push(SocialIntent::JoinHousehold { creature: mate, household: h });
            }
            h
        }
    };

    let id = ctx.structures.add(Structure {
        id: 0,
        kind: StructureKind::Shelter,
        x,
        y,
        condition: 1.0,
        capacity: ctx.cfg.actions.shelter_capacity,
        occupants: 0,
        household_id: Some(household_id),
        built_tick: ctx.tick,
        fuel_remaining: 0.0,
        lit_until_tick: None,
        dirty: true,
    });
    if let Some(h) = ctx.households.get_mut(household_id) {
        h.shelter_id = Some(id);
        h.dirty = true;
    }

    ctx.events.push(
        Event::new(ctx.tick, EventKind::ShelterBuilt, c.id)
            .at(x, y)
            .target(id)
            .with_num("wood", spent),
    );
    Outcome::StepComplete
}

fn repair_shelter(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let Target::Structure(id) = step.target else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let wood = c.inventory.take(ItemKind::Wood, 2.0);
    if wood <= 0.0 {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    let Some(s) = ctx.structures.get_mut(id) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    s.condition = (s.condition + wood * 0.15).min(1.0);
    s.dirty = true;
    ctx.events.push(
        Event::new(ctx.tick, EventKind::ShelterRepaired, c.id).at(s.x, s.y).target(id),
    );
    Outcome::StepComplete
}

// ------------------------------------------------------------------ society

/// Offer courtship. The answer belongs to the other creature (§4.8), so all
/// this does is put the question.
fn court(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let Some(to) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if ctx.social().within_reach(c, to, ctx.cfg.actions.social_reach).is_none() {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    c.fatigue = (c.fatigue - 1.0).max(0.0);
    ctx.intents.push(SocialIntent::Court { from: c.id, to });
    ctx.events.push(
        Event::new(ctx.tick, EventKind::Courted, c.id).at(c.x, c.y).target(to),
    );
    Outcome::StepComplete
}

fn answer_courtship(c: &mut Creature, step: &Step, ctx: &mut ActionCtx, accept: bool) -> Outcome {
    let Some(from) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    // The offer may have lapsed, or the suitor may have paired with somebody
    // else while this creature was thinking about it.
    if ctx.courtships.pending_for(c.id).is_none_or(|o| o.from != from) {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    ctx.intents.push(if accept {
        SocialIntent::Accept { from, to: c.id }
    } else {
        SocialIntent::Reject { from, to: c.id }
    });
    Outcome::StepComplete
}

/// Give food to somebody, or feed an infant.
///
/// The transfer itself happens in resolution, where both inventories are
/// reachable — so food is never in limbo between a giver and a recipient who
/// has since walked away or died.
fn hand_over(c: &mut Creature, step: &Step, ctx: &mut ActionCtx, to_infant: bool) -> Outcome {
    let Some(to) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if ctx.social().within_reach(c, to, ctx.cfg.actions.social_reach).is_none() {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    if c.inventory.total_food() <= 0.0 {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    let a = &ctx.cfg.actions;
    let quantity = if to_infant { a.feed_infant_portion } else { a.give_food_portion };
    ctx.intents.push(if to_infant {
        SocialIntent::FeedInfant { from: c.id, to, quantity }
    } else {
        SocialIntent::GiveFood { from: c.id, to, quantity }
    });
    Outcome::StepComplete
}

/// Stay near a guardian. An infant's whole locomotion (§4.7).
fn follow(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Some(id) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    let Some(p) = ctx.people.get(id).copied() else {
        // The guardian died. For an infant this is usually fatal, and it should
        // be legible as that rather than as a creature standing still.
        return Outcome::Failed(AbortReason::TargetGone);
    };

    let reach = ctx.cfg.actions.follow_distance;
    if near(p.x, p.y, c.x, c.y, reach) {
        return if step.elapsed >= step.est_ticks {
            Outcome::StepComplete
        } else {
            Outcome::Working
        };
    }

    // Step directly toward them rather than pathfinding: a guardian moves every
    // tick, so any route computed now is stale before it is walked, and an
    // infant is never more than a few tiles behind.
    let dx = (p.x as i64 - c.x as i64).signum();
    let dy = (p.y as i64 - c.y as i64).signum();
    let (nx, ny) = (c.x as i64 + dx, c.y as i64 + dy);
    if ctx.world.in_bounds(nx, ny) && ctx.world.at(nx as u32, ny as u32).passable() {
        c.x = nx as u32;
        c.y = ny as u32;
    }
    Outcome::Working
}

fn join_household(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let Target::Household(id) = step.target else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if ctx.households.get(id).is_none() {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    ctx.intents.push(SocialIntent::JoinHousehold { creature: c.id, household: id });
    Outcome::StepComplete
}

fn leave_household(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    if c.household_id.is_none() {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    ctx.intents.push(SocialIntent::LeaveHousehold { creature: c.id });
    Outcome::StepComplete
}

/// Put what you are carrying into the household store.
///
/// This is the act that makes a household more than a shared roof: the store is
/// what the reproduction gate reads, and because only grain keeps, depositing
/// grain is how a lineage becomes possible at all (§4.4, §4.8).
fn deposit(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    let Some(hid) = c.household_id else {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    };
    let want = ctx.cfg.actions.store_transfer;

    // Grain first: it is the only thing that will still be there next week, and
    // therefore the only thing a reserve can actually be made of.
    //
    // A creature keeps a meal in hand — banking everything and then starving on
    // the walk to the woods helps nobody — but that meal is made of what will
    // spoil anyway, not of the grain. Keeping four units of *every* kind meant
    // grain was withheld from the store alongside the berries, which is exactly
    // backwards: the berries are what you eat on the road and the grain is what
    // the household is trying to accumulate.
    let mut moved = 0.0;
    let order = [ItemKind::Grain, ItemKind::Wood, ItemKind::Meat, ItemKind::Forage];
    let perishable_in_hand = c.inventory.total(ItemKind::Forage) + c.inventory.total(ItemKind::Meat);
    let mut taken: Vec<(ItemKind, f32, i64)> = Vec::new();
    for kind in order {
        if moved >= want {
            break;
        }
        // Hold back a meal, made of whatever is closest to spoiling. If there
        // are no perishables to keep, hold back a little grain instead.
        let keep = match kind {
            ItemKind::Forage | ItemKind::Meat => 4.0,
            ItemKind::Grain if perishable_in_hand < 4.0 => 4.0 - perishable_in_hand,
            _ => 0.0,
        };
        let held = c.inventory.total(kind);
        let spare = (held - keep).min(want - moved);
        if spare <= 0.01 {
            continue;
        }
        let oldest = c
            .inventory
            .batches
            .iter()
            .filter(|b| b.kind == kind)
            .map(|b| b.harvested_tick)
            .min()
            .unwrap_or(ctx.tick);
        let got = c.inventory.take(kind, spare);
        if got > 0.0 {
            taken.push((kind, got, oldest));
            moved += got;
        }
    }

    if taken.is_empty() {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    }
    let Some(h) = ctx.households.get_mut(hid) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    for (kind, qty, harvested) in &taken {
        // The batch keeps its original harvest tick, so depositing does not
        // reset the clock on something that was already going off.
        h.store.add(*kind, *qty, *harvested);
    }
    h.dirty = true;

    ctx.events.push(
        Event::new(ctx.tick, EventKind::Deposited, c.id)
            .at(c.x, c.y)
            .target(hid)
            .with_num("qty", moved),
    );
    Outcome::StepComplete
}

fn withdraw(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    let Some(hid) = c.household_id else {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    };
    let room = (c.carry_capacity(ctx.cfg) - c.inventory.weight()).max(0.0);
    let want = ctx.cfg.actions.store_transfer.min(room);
    if want <= 0.01 {
        return Outcome::Failed(AbortReason::Encumbered);
    }
    let Some(h) = ctx.households.get_mut(hid) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };

    // Perishables out first: what is about to rot is what should be walking
    // around in somebody's pack, not sitting in the store.
    let mut moved = 0.0;
    for kind in [ItemKind::Forage, ItemKind::Meat, ItemKind::Grain] {
        if moved >= want {
            break;
        }
        let oldest = h
            .store
            .batches
            .iter()
            .filter(|b| b.kind == kind)
            .map(|b| b.harvested_tick)
            .min();
        let got = h.store.take(kind, want - moved);
        if got > 0.0 {
            c.inventory.add(kind, got, oldest.unwrap_or(ctx.tick));
            moved += got;
        }
    }
    h.dirty = true;

    if moved <= 0.0 {
        return Outcome::Failed(AbortReason::TargetDepleted);
    }
    ctx.events.push(
        Event::new(ctx.tick, EventKind::Withdrew, c.id)
            .at(c.x, c.y)
            .target(hid)
            .with_num("qty", moved),
    );
    Outcome::StepComplete
}

fn eat_from_store(c: &mut Creature, ctx: &mut ActionCtx) -> Outcome {
    let Some(hid) = c.household_id else {
        return Outcome::Failed(AbortReason::PreconditionFailed);
    };
    let portion = ctx.cfg.actions.eat_portion;
    let Some(h) = ctx.households.get_mut(hid) else {
        return Outcome::Failed(AbortReason::TargetGone);
    };

    let Some(oldest) = h.store.oldest_food().copied() else {
        return Outcome::Failed(AbortReason::TargetDepleted);
    };
    let got = h.store.take(oldest.kind, portion.min(oldest.quantity));
    if got <= 0.0 {
        return Outcome::Failed(AbortReason::TargetDepleted);
    }
    h.dirty = true;

    c.hunger = (c.hunger + got * oldest.kind.nutrition()).min(100.0);
    ctx.eaten += got;
    ctx.events.push(
        Event::new(ctx.tick, EventKind::Ate, c.id)
            .at(c.x, c.y)
            .with("kind", oldest.kind.as_str())
            .with("from", "store")
            .with_num("qty", got),
    );
    Outcome::StepComplete
}

// ------------------------------------------------ knowledge transmission

/// Tell somebody something. One tick, small fatigue, topic-filtered (§4.11).
///
/// None of this costs an LLM call at M3 either: the topic and the recipient are
/// chosen inside a deliberation the creature was going to make anyway, and the
/// transfer itself is a set merge. Communication is nearly free in the budget
/// that actually matters.
fn share_knowledge(c: &mut Creature, step: &Step, ctx: &mut ActionCtx) -> Outcome {
    let Some(to) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if ctx.social().within_reach(c, to, ctx.cfg.actions.social_reach).is_none() {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    let topic = step.topic;
    c.fatigue = (c.fatigue - ctx.cfg.knowledge.share_fatigue).max(0.0);
    ctx.intents.push(SocialIntent::Share { from: c.id, to, topic });
    Outcome::StepComplete
}

/// A bulk, high-fidelity transfer to a young household member.
///
/// Expensive in exactly the way that matters: an adult spending six ticks
/// teaching is six ticks not gathering, and the payoff arrives after the
/// teacher is dead. Whether a creature ever chooses to do it is §13.5's open
/// question, and the answer is a measurement rather than a design decision —
/// which is why nothing here makes it automatic between parent and child.
fn teach(c: &mut Creature, step: &mut Step, ctx: &mut ActionCtx) -> Outcome {
    let Some(to) = step.target.creature() else {
        return Outcome::Failed(AbortReason::TargetGone);
    };
    if ctx.social().within_reach(c, to, ctx.cfg.actions.social_reach.max(2)).is_none() {
        return Outcome::Failed(AbortReason::TargetGone);
    }
    if step.elapsed < ctx.cfg.knowledge.teach_ticks {
        return Outcome::Working;
    }
    c.fatigue = (c.fatigue - ctx.cfg.knowledge.teach_fatigue).max(0.0);
    ctx.intents.push(SocialIntent::Teach { from: c.id, to });
    Outcome::StepComplete
}

/// Rare misfortune while doing dangerous work (§4.6). Injury costs health
/// rather than killing outright, so an accident is usually survivable and
/// occasionally is not.
fn maybe_injure(c: &mut Creature, ctx: &mut ActionCtx) {
    if ctx.rng.gen::<f32>() >= ctx.cfg.hazards.accident_per_tick {
        return;
    }
    // Heavy-tailed: most mishaps are a bruise and a lost afternoon, a few are
    // an axe through a foot. A uniform range produced injuries that were always
    // survivable, so ACCIDENT never appeared in the cause-of-death breakdown at
    // all and the tail of that distribution was empty.
    let roll = ctx.rng.gen::<f32>();
    let severity = 6.0 + roll * roll * roll * 88.0;
    c.health = (c.health - severity).max(0.0);
    ctx.events.push(
        Event::new(ctx.tick, EventKind::Injured, c.id).at(c.x, c.y).with_num("severity", severity),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::creature::testing::test_creature;
    use crate::sim::creature::LifeStage;
    use rand::SeedableRng;

    fn world_of(t: Terrain) -> World {
        World {
            width: 32,
            height: 32,
            chunk_size: 32,
            seed: 1,
            tiles: vec![t; 32 * 32],
            nodes: Vec::new(),
            founders: Vec::new(),
        }
    }

    struct Harness {
        world: World,
        structures: Structures,
        pathfinder: Pathfinder,
        cfg: WorldConfig,
        rng: ChaCha8Rng,
        events: Vec<Event>,
        households: Households,
        people: CreatureIndex,
        courtships: Courtships,
        intents: Vec<SocialIntent>,
    }

    impl Harness {
        fn new(t: Terrain) -> Self {
            let world = world_of(t);
            let pathfinder = Pathfinder::new(&world);
            let people = CreatureIndex::new(world.width, world.height, 8);
            Self {
                world,
                structures: Structures::new(),
                pathfinder,
                cfg: WorldConfig::default(),
                rng: ChaCha8Rng::seed_from_u64(7),
                events: Vec::new(),
                households: Households::new(),
                people,
                courtships: Courtships::new(),
                intents: Vec::new(),
            }
        }

        /// Put these creatures on the map so social actions can find them.
        fn populate(&mut self, creatures: &[Creature]) {
            self.people.rebuild(creatures.iter(), 0, &self.cfg.knowledge);
        }

        fn social(&self) -> SocialView<'_> {
            SocialView {
                people: &self.people,
                households: &self.households,
                courtships: &self.courtships,
            }
        }

        fn ctx(&mut self, tick: i64, night: bool) -> ActionCtx<'_> {
            ActionCtx {
                world: &mut self.world,
                structures: &mut self.structures,
                pathfinder: &mut self.pathfinder,
                cfg: &self.cfg,
                tick,
                rng: &mut self.rng,
                events: &mut self.events,
                night,
                gathered: 0.0,
                eaten: 0.0,
                households: &mut self.households,
                people: &self.people,
                courtships: &self.courtships,
                intents: &mut self.intents,
            }
        }
    }

    #[test]
    fn moving_walks_the_path_and_reports_arrival() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        c.x = 2;
        c.y = 2;
        let mut step = Step::new(Goal::MoveTo, Target::Tile(9, 2), 8);

        let mut guard = 0;
        loop {
            guard += 1;
            assert!(guard < 50, "should arrive well inside 50 ticks");
            match execute(&mut c, &mut step, &mut h.ctx(guard, false)) {
                Outcome::Working => continue,
                Outcome::StepComplete => break,
                Outcome::Failed(r) => panic!("unexpected failure {r:?}"),
            }
        }
        assert_eq!((c.x, c.y), (9, 2));
    }

    #[test]
    fn an_unreachable_target_fails_rather_than_looping() {
        let mut h = Harness::new(Terrain::Grass);
        for y in 0..32 {
            let i = h.world.idx(16, y);
            h.world.tiles[i] = Terrain::DeepWater;
        }
        let mut c = test_creature();
        c.x = 2;
        c.y = 2;
        let mut step = Step::new(Goal::MoveTo, Target::Tile(28, 2), 20);

        let out = execute(&mut c, &mut step, &mut h.ctx(1, false));
        assert_eq!(out, Outcome::Failed(AbortReason::Unreachable));
    }

    #[test]
    fn drinking_at_water_restores_thirst_but_not_on_dry_land() {
        let mut h = Harness::new(Terrain::ShallowWater);
        let mut c = test_creature();
        c.thirst = 10.0;
        let mut step = Step::new(Goal::Drink, Target::Tile(c.x, c.y), 1);

        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(1, false)), Outcome::StepComplete);
        assert!(c.thirst > 60.0);

        let mut dry = Harness::new(Terrain::Grass);
        let mut c2 = test_creature();
        c2.thirst = 10.0;
        let mut step2 = Step::new(Goal::Drink, Target::Tile(c2.x, c2.y), 1);
        assert_eq!(
            execute(&mut c2, &mut step2, &mut dry.ctx(1, false)),
            Outcome::Failed(AbortReason::PreconditionFailed),
            "walking to remembered water and finding dry ground must fail loudly"
        );
        assert_eq!(c2.thirst, 10.0);
    }

    #[test]
    fn eating_spends_the_batch_closest_to_spoiling() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        c.hunger = 40.0;
        c.inventory.add(ItemKind::Grain, 4.0, 200);
        c.inventory.add(ItemKind::Forage, 4.0, 100);

        let mut step = Step::new(Goal::EatFromInventory, Target::None, 1);
        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(210, false)), Outcome::StepComplete);

        assert!(c.hunger > 40.0);
        assert_eq!(c.inventory.total(ItemKind::Grain), 4.0, "the keeping food is left alone");
        assert!(c.inventory.total(ItemKind::Forage) < 4.0, "the perishable is eaten first");
    }

    #[test]
    fn gathering_moves_quantity_from_the_node_into_the_pack() {
        let mut h = Harness::new(Terrain::Forest);
        h.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage,
            x: 10,
            y: 10,
            quantity: 12.0,
            max_quantity: 12.0,
            regen_rate: 0.02,
        });
        let mut c = test_creature();
        c.x = 10;
        c.y = 10;
        let mut step = Step::new(Goal::GatherForage, Target::Node(0), 4);

        execute(&mut c, &mut step, &mut h.ctx(1, false));

        assert!(c.inventory.total(ItemKind::Forage) > 0.0);
        assert!(h.world.nodes[0].quantity < 12.0);
        assert!((c.inventory.total(ItemKind::Forage) + h.world.nodes[0].quantity - 12.0).abs() < 1e-3,
                "nothing is created or destroyed in the transfer");
    }

    #[test]
    fn night_gathering_yields_less() {
        let mk = |night: bool| {
            let mut h = Harness::new(Terrain::Forest);
            h.world.nodes.push(ResourceNode {
                kind: NodeKind::Forage, x: 10, y: 10,
                quantity: 12.0, max_quantity: 12.0, regen_rate: 0.0,
            });
            let mut c = test_creature();
            (c.x, c.y) = (10, 10);
            let mut step = Step::new(Goal::GatherForage, Target::Node(0), 1);
            execute(&mut c, &mut step, &mut h.ctx(1, night));
            c.inventory.total(ItemKind::Forage)
        };
        assert!(mk(true) < mk(false), "the dark is a worse time to pick berries");
    }

    #[test]
    fn arriving_at_a_stripped_node_fails_with_target_depleted() {
        // The core stale-belief payoff: the creature acted on what it believed
        // and the world had moved on.
        let mut h = Harness::new(Terrain::Forest);
        h.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 10, y: 10,
            quantity: 0.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        let mut c = test_creature();
        (c.x, c.y) = (10, 10);
        let mut step = Step::new(Goal::GatherForage, Target::Node(0), 4);

        assert_eq!(
            execute(&mut c, &mut step, &mut h.ctx(1, false)),
            Outcome::Failed(AbortReason::TargetDepleted)
        );
    }

    #[test]
    fn a_full_pack_stops_gathering() {
        let mut h = Harness::new(Terrain::Forest);
        h.world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 10, y: 10,
            quantity: 99.0, max_quantity: 99.0, regen_rate: 0.0,
        });
        let mut c = test_creature();
        (c.x, c.y) = (10, 10);
        // Filled to capacity, whatever capacity is configured to be.
        let cap = c.carry_capacity(&h.cfg);
        c.inventory.add(ItemKind::Grain, cap, 0);
        let mut step = Step::new(Goal::GatherForage, Target::Node(0), 8);

        assert_eq!(
            execute(&mut c, &mut step, &mut h.ctx(1, false)),
            Outcome::Failed(AbortReason::Encumbered)
        );
    }

    #[test]
    fn a_fire_costs_wood_and_burns_it() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        c.inventory.add(ItemKind::Wood, 6.0, 0);
        let mut step = Step::new(Goal::BuildFire, Target::None, 1);

        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(5, true)), Outcome::StepComplete);
        assert_eq!(c.inventory.total(ItemKind::Wood), 4.0, "two wood went into the fire");
        assert_eq!(h.structures.items.len(), 1);
        assert!(h.structures.items[0].is_lit(5));
    }

    #[test]
    fn a_fire_cannot_be_lit_with_no_wood() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        let mut step = Step::new(Goal::BuildFire, Target::None, 1);
        assert!(matches!(
            execute(&mut c, &mut step, &mut h.ctx(5, true)),
            Outcome::Failed(_)
        ));
        assert!(h.structures.items.is_empty());
    }

    #[test]
    fn building_a_shelter_takes_time_and_timber() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        let carried = h.cfg.actions.shelter_wood_cost + 6.0;
        c.inventory.add(ItemKind::Wood, carried, 0);
        let build_ticks = h.cfg.actions.shelter_build_ticks;
        let mut step = Step::new(Goal::BuildShelter, Target::Tile(c.x, c.y), build_ticks);

        for t in 1..build_ticks as i64 {
            assert_eq!(
                execute(&mut c, &mut step, &mut h.ctx(t, false)),
                Outcome::Working,
                "a shelter is not raised in one tick"
            );
        }
        assert_eq!(
            execute(&mut c, &mut step, &mut h.ctx(build_ticks as i64, false)),
            Outcome::StepComplete
        );
        assert_eq!(h.structures.items.len(), 1);
        assert_eq!(c.inventory.total(ItemKind::Wood), 6.0, "the timber is spent, the rest kept");
    }

    #[test]
    fn planting_creates_a_crop_that_starts_empty_and_grows() {
        let mut h = Harness::new(Terrain::Soil);
        let mut c = test_creature();
        let mut step = Step::new(Goal::PlantWheat, Target::Tile(c.x, c.y), 4);

        let mut out = Outcome::Working;
        for t in 1..=4 {
            out = execute(&mut c, &mut step, &mut h.ctx(t, false));
        }
        assert_eq!(out, Outcome::StepComplete);

        let node = h.world.nodes.iter().find(|n| n.kind == NodeKind::Wheat).expect("crop planted");
        assert_eq!(node.quantity, 0.0, "a seed is not a harvest");
        assert!(node.regen_rate > 0.0, "it grows toward maturity");

        // Three in-game days of growth reach the full ear.
        for _ in 0..h.cfg.resources.wheat_growth_ticks {
            crate::sim::economy::regrow(&mut h.world, &h.cfg);
        }
        let node = h.world.nodes.iter().find(|n| n.kind == NodeKind::Wheat).unwrap();
        assert!((node.quantity - node.max_quantity).abs() < 0.1);
    }

    #[test]
    fn infants_are_offered_nothing_but_eating_resting_and_following() {
        let world = world_of(Terrain::Grass);
        let st = Structures::new();
        let cfg = WorldConfig::default();
        let (hh, ppl, cs) = (Households::new(), CreatureIndex::new(32, 32, 8), Courtships::new());
        let social = SocialView { people: &ppl, households: &hh, courtships: &cs };
        let mut c = test_creature();
        c.life_stage = LifeStage::Infant;
        c.inventory.add(ItemKind::Wood, 20.0, 0);

        assert!(!is_legal(&c, Goal::ChopWood, Target::Node(0), &world, &st, social, &cfg));
        assert!(!is_legal(&c, Goal::BuildShelter, Target::Tile(1, 1), &world, &st, social, &cfg));
        c.inventory.add(ItemKind::Forage, 2.0, 0);
        assert!(is_legal(&c, Goal::EatFromInventory, Target::None, &world, &st, social, &cfg));
        assert!(is_legal(&c, Goal::Rest, Target::None, &world, &st, social, &cfg));
    }

    #[test]
    fn illegal_goals_are_never_offered() {
        let mut world = world_of(Terrain::Grass);
        world.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 4, y: 4,
            quantity: 0.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        let st = Structures::new();
        let cfg = WorldConfig::default();
        let (hh, ppl, cs) = (Households::new(), CreatureIndex::new(32, 32, 8), Courtships::new());
        let social = SocialView { people: &ppl, households: &hh, courtships: &cs };
        let c = test_creature();

        assert!(!is_legal(&c, Goal::GatherForage, Target::Node(0), &world, &st, social, &cfg),
                "an empty node is not a legal gather");
        assert!(!is_legal(&c, Goal::Drink, Target::Tile(1, 1), &world, &st, social, &cfg),
                "dry land is not a legal drink");
        assert!(!is_legal(&c, Goal::BuildFire, Target::None, &world, &st, social, &cfg),
                "no wood, no fire");
        assert!(!is_legal(&c, Goal::EatFromInventory, Target::None, &world, &st, social, &cfg),
                "an empty pack is not a meal");
    }

    #[test]
    fn wheat_actions_vanish_when_wheat_is_disabled_for_the_s4_experiment() {
        let mut world = world_of(Terrain::Soil);
        world.nodes.push(ResourceNode {
            kind: NodeKind::Wheat, x: 4, y: 4,
            quantity: 20.0, max_quantity: 24.0, regen_rate: 0.0,
        });
        let st = Structures::new();
        let mut cfg = WorldConfig::default();
        let (hh, ppl, cs) = (Households::new(), CreatureIndex::new(32, 32, 8), Courtships::new());
        let social = SocialView { people: &ppl, households: &hh, courtships: &cs };
        let c = test_creature();

        assert!(is_legal(&c, Goal::HarvestWheat, Target::Node(0), &world, &st, social, &cfg));
        cfg.features.wheat = false;
        assert!(!is_legal(&c, Goal::HarvestWheat, Target::Node(0), &world, &st, social, &cfg));
        assert!(!is_legal(&c, Goal::PlantWheat, Target::Tile(5, 5), &world, &st, social, &cfg));
    }

    #[test]
    fn resting_in_shelter_recovers_faster_than_resting_in_the_open() {
        let restore = |sheltered: bool| {
            let mut h = Harness::new(Terrain::Grass);
            let mut c = test_creature();
            c.fatigue = 10.0;
            if sheltered {
                let id = h.structures.add(Structure {
                    id: 0, kind: StructureKind::Shelter, x: c.x, y: c.y,
                    condition: 1.0, capacity: 4, occupants: 1, household_id: None,
                    built_tick: 0, fuel_remaining: 0.0, lit_until_tick: None, dirty: false,
                });
                c.in_shelter = Some(id);
            }
            let mut step = Step::new(Goal::Rest, Target::None, 6);
            execute(&mut c, &mut step, &mut h.ctx(1, true));
            c.fatigue
        };
        assert!(restore(true) > restore(false), "shelter is worth sleeping in");
    }

    /// A creature with a household, standing at its shelter.
    fn householder(h: &mut Harness) -> Creature {
        let mut c = test_creature();
        (c.x, c.y) = (10, 10);
        let shelter = h.structures.add(Structure {
            id: 0, kind: StructureKind::Shelter, x: 10, y: 10,
            condition: 1.0, capacity: 6, occupants: 0, household_id: None,
            built_tick: 0, fuel_remaining: 0.0, lit_until_tick: None, dirty: false,
        });
        let hid = h.households.found(Some(shelter), c.id, None, 0, &h.cfg);
        c.household_id = Some(hid);
        c
    }

    #[test]
    fn a_social_action_needs_somebody_within_reach() {
        // Every social act reaches into another creature, so "is anybody
        // actually there" is the first precondition all of them share.
        let mut h = Harness::new(Terrain::Grass);
        let mut a = test_creature();
        a.id = 1;
        (a.x, a.y) = (10, 10);
        a.inventory.add(ItemKind::Forage, 5.0, 0);
        let mut far = test_creature();
        far.id = 2;
        (far.x, far.y) = (25, 25);

        h.populate(&[a.clone(), far]);
        assert!(
            !is_legal(&a, Goal::GiveFood, Target::Creature(2), &h.world, &h.structures,
                      h.social(), &h.cfg),
            "you cannot hand food to somebody fifteen tiles away"
        );

        let mut near = test_creature();
        near.id = 2;
        (near.x, near.y) = (11, 10);
        h.populate(&[a.clone(), near]);
        assert!(is_legal(&a, Goal::GiveFood, Target::Creature(2), &h.world, &h.structures,
                         h.social(), &h.cfg));
    }

    #[test]
    fn depositing_moves_food_from_the_pack_into_the_household_store() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = householder(&mut h);
        c.inventory.add(ItemKind::Grain, 16.0, 0);

        let mut step = Step::new(Goal::DepositToStore, Target::None, 1);
        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(5, false)), Outcome::StepComplete);

        let hid = c.household_id.unwrap();
        let stored = h.households.get(hid).unwrap().store.total(ItemKind::Grain);
        assert!(stored > 0.0, "nothing reached the store");
        assert!(
            (stored + c.inventory.total(ItemKind::Grain) - 16.0).abs() < 1e-3,
            "nothing is created or destroyed: {stored} stored, {} kept",
            c.inventory.total(ItemKind::Grain)
        );
        assert!(
            c.inventory.total(ItemKind::Grain) >= 4.0,
            "a creature keeps a meal in hand rather than banking everything and starving"
        );
    }

    #[test]
    fn a_deposit_moves_a_useful_share_of_a_full_pack() {
        // The reproduction gate reads a household store of 20 (§4.8). If one
        // trip home moves a trickle, the reserve is never reached and no
        // household ever has a child — which is exactly what a measured run
        // showed before `store_transfer` was raised.
        let mut h = Harness::new(Terrain::Grass);
        let mut c = householder(&mut h);
        c.inventory.add(ItemKind::Grain, 18.0, 0);

        let mut step = Step::new(Goal::DepositToStore, Target::None, 1);
        execute(&mut c, &mut step, &mut h.ctx(5, false));

        let stored = h.households.get(c.household_id.unwrap()).unwrap().stored_food();
        assert!(stored >= h.cfg.actions.store_transfer - 0.01,
                "one trip should bank a load, moved {stored}");
    }

    #[test]
    fn grain_is_banked_before_anything_that_will_rot() {
        // Only grain keeps, so grain is what the reserve is actually made of.
        let mut h = Harness::new(Terrain::Grass);
        let mut c = householder(&mut h);
        c.inventory.add(ItemKind::Forage, 20.0, 0);
        c.inventory.add(ItemKind::Grain, 10.0, 0);

        let mut step = Step::new(Goal::DepositToStore, Target::None, 1);
        execute(&mut c, &mut step, &mut h.ctx(5, false));

        let store = &h.households.get(c.household_id.unwrap()).unwrap().store;
        assert!(store.total(ItemKind::Grain) > store.total(ItemKind::Forage),
                "grain {} should outrank forage {}",
                store.total(ItemKind::Grain), store.total(ItemKind::Forage));
    }

    #[test]
    fn eating_from_the_store_feeds_the_creature_and_empties_the_store() {
        let mut h = Harness::new(Terrain::Grass);
        let mut c = householder(&mut h);
        c.hunger = 30.0;
        let hid = c.household_id.unwrap();
        h.households.get_mut(hid).unwrap().store.add(ItemKind::Grain, 10.0, 0);

        let mut step = Step::new(Goal::EatFromStore, Target::None, 1);
        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(5, false)), Outcome::StepComplete);

        assert!(c.hunger > 30.0);
        assert!(h.households.get(hid).unwrap().store.total(ItemKind::Grain) < 10.0);
    }

    #[test]
    fn raising_a_shelter_founds_a_household_to_go_with_it() {
        // §4.10: a household is a shelter *plus its members*. A shelter with no
        // household has nowhere to put anything, and §4.8's gate reads a
        // household store — so the two have to arrive together.
        let mut h = Harness::new(Terrain::Grass);
        let mut c = test_creature();
        c.inventory.add(ItemKind::Wood, 30.0, 0);
        let ticks = h.cfg.actions.shelter_build_ticks as i64;
        let mut step = Step::new(Goal::BuildShelter, Target::Tile(c.x, c.y), ticks as u32);
        for t in 1..=ticks {
            execute(&mut c, &mut step, &mut h.ctx(t, false));
        }

        let hid = c.household_id.expect("building a home should found a household");
        let household = h.households.get(hid).expect("household exists");
        assert!(household.shelter_id.is_some(), "and it knows where it lives");
    }

    #[test]
    fn slaughtering_a_sheep_yields_more_meat_than_one_creature_can_keep() {
        let mut h = Harness::new(Terrain::Grass);
        h.world.nodes.push(ResourceNode {
            kind: NodeKind::Sheep, x: 10, y: 10,
            quantity: 1.0, max_quantity: 6.0, regen_rate: 0.0,
        });
        let mut c = test_creature();
        (c.x, c.y) = (10, 10);
        // Leave room for less than one sheep, whatever the yield is set to.
        let room = h.cfg.actions.slaughter_yield - 4.0;
        c.inventory.add(ItemKind::Grain, c.carry_capacity(&h.cfg) - room, 0);
        let mut step = Step::new(Goal::SlaughterSheep, Target::Node(0), 1);

        assert_eq!(execute(&mut c, &mut step, &mut h.ctx(1, false)), Outcome::StepComplete);
        assert_eq!(h.world.nodes[0].quantity, 0.0, "the sheep is gone");
        assert!((c.inventory.total(ItemKind::Meat) - room).abs() < 1e-3);

        let waste = h.events.iter().find(|e| e.kind == EventKind::Slaughtered).unwrap();
        assert!(waste.payload.contains("wasted=4.00"),
                "the surplus is recorded — it is the sharing pressure §4.4 describes");
    }
}
