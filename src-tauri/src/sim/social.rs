//! Households, relationships, courtship and reproduction (PRD §4.8–§4.10).
//!
//! Brought forward ahead of M3. M2 measured a world in which nothing
//! distinguished one creature from another: 500 individuals solving the same
//! problem the same way, 0.8% of whom ever built anything. Everything that
//! makes creatures *differ* — whose household they belong to, who their parents
//! were, what they were taught — lived behind the milestone after the model, so
//! the model would have arrived with almost nothing to differentiate on.
//!
//! The unit that matters here is the **household**: a shelter plus its members,
//! with a shared store. It is the unit of cooperation, the thing reproduction
//! gates on, and — because only grain keeps (§4.4) — the mechanism by which
//! farming becomes the precondition for a lineage rather than merely a good
//! idea.
//!
//! Lineage itself is *not* here. It is a recursive CTE over `mother_id` and
//! `father_id` (invariant 6); storing a tree would only create a second source
//! of truth to keep in sync.

use crate::config::WorldConfig;
use crate::sim::creature::{Creature, Inventory, ItemKind, LifeStage, Sex};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A shelter plus its members and their shared store (§4.10).
#[derive(Debug, Clone)]
pub struct Household {
    pub id: i64,
    pub shelter_id: Option<i64>,
    pub founded_tick: i64,
    pub dissolved_tick: Option<i64>,
    /// Batches, not totals, so spoilage expires the oldest first. A household
    /// store of grain accumulates; a store of berries quietly rots (§4.4).
    pub store: Inventory,
    /// How many creatures may belong. Limits *membership*, not tonight's beds.
    pub size_cap: u32,
    pub founder_ids: (i64, Option<i64>),
    pub dirty: bool,
}

impl Household {
    pub fn is_alive(&self) -> bool {
        self.dissolved_tick.is_none()
    }

    /// Food value held, which is what the reproduction gate is really about.
    pub fn stored_food(&self) -> f32 {
        self.store.total_food()
    }

    /// Only grain reaches the reserve in practice, because only grain keeps.
    /// This is the mechanism that makes S4 inevitable rather than hoped for:
    /// disabling wheat does not merely make food scarcer, it severs the path to
    /// reproduction entirely.
    pub fn grain(&self) -> f32 {
        self.store.total(ItemKind::Grain)
    }
}

#[derive(Debug, Default)]
pub struct Households {
    pub items: Vec<Household>,
    next_id: i64,
}

impl Households {
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: 1 }
    }

    pub fn with_next_id(next_id: i64) -> Self {
        Self { items: Vec::new(), next_id: next_id.max(1) }
    }

    pub fn next_id(&self) -> i64 {
        self.next_id
    }

    pub fn found(
        &mut self,
        shelter_id: Option<i64>,
        by: i64,
        with: Option<i64>,
        tick: i64,
        cfg: &WorldConfig,
    ) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.push(Household {
            id,
            shelter_id,
            founded_tick: tick,
            dissolved_tick: None,
            store: Inventory::default(),
            size_cap: cfg.actions.shelter_capacity,
            founder_ids: (by, with),
            dirty: true,
        });
        id
    }

    pub fn get(&self, id: i64) -> Option<&Household> {
        self.items.iter().find(|h| h.id == id && h.is_alive())
    }

    pub fn get_mut(&mut self, id: i64) -> Option<&mut Household> {
        self.items.iter_mut().find(|h| h.id == id && h.is_alive())
    }

    /// Dissolve households nobody belongs to any more, handing the store on.
    ///
    /// A store that simply vanished when the last member died would make death
    /// destroy grain, and grain is the whole reproduction economy. Instead it
    /// passes to the nearest surviving household of a child of the founders —
    /// which is inheritance, and it is what lets a lineage compound across
    /// generations rather than restarting from nothing every time.
    pub fn reap(&mut self, member_counts: &BTreeMap<i64, u32>, tick: i64) -> Vec<(i64, Inventory)> {
        let mut estates = Vec::new();
        for h in self.items.iter_mut() {
            if !h.is_alive() {
                continue;
            }
            if member_counts.get(&h.id).copied().unwrap_or(0) == 0 {
                h.dissolved_tick = Some(tick);
                h.dirty = true;
                if h.store.weight() > 0.0 {
                    estates.push((h.id, std::mem::take(&mut h.store)));
                }
            }
        }
        estates
    }
}

// ------------------------------------------------------------ relationships

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum RelKind {
    Kin,
    Mate,
    Household,
    Acquaintance,
}

impl RelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RelKind::Kin => "KIN",
            RelKind::Mate => "MATE",
            RelKind::Household => "HOUSEHOLD",
            RelKind::Acquaintance => "ACQUAINTANCE",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Edge {
    pub affinity: f32,
    pub kind: Option<RelKind>,
    pub updated_tick: i64,
}

/// Directed edges between creatures, updated by what they do to each other
/// (§4.10). A `BTreeMap` rather than a `HashMap`: this is iterated when
/// persisting and when a creature looks for someone to court, and iteration
/// order has to be reproducible (invariant 7).
#[derive(Debug, Default)]
pub struct Relationships {
    edges: BTreeMap<(i64, i64), Edge>,
}

impl Relationships {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, from: i64, to: i64) -> f32 {
        self.edges.get(&(from, to)).map(|e| e.affinity).unwrap_or(0.0)
    }

    pub fn kind(&self, from: i64, to: i64) -> Option<RelKind> {
        self.edges.get(&(from, to)).and_then(|e| e.kind)
    }

    pub fn edge(&self, from: i64, to: i64) -> Option<&Edge> {
        self.edges.get(&(from, to))
    }

    pub fn iter(&self) -> impl Iterator<Item = (&(i64, i64), &Edge)> {
        self.edges.iter()
    }

    pub fn len(&self) -> usize {
        self.edges.len()
    }

    pub fn is_empty(&self) -> bool {
        self.edges.is_empty()
    }

    /// Nudge one direction of a relationship. Affinity saturates rather than
    /// running away, so a hundred small kindnesses do not make a stranger
    /// family.
    pub fn adjust(&mut self, from: i64, to: i64, delta: f32, kind: Option<RelKind>, tick: i64) {
        let e = self.edges.entry((from, to)).or_insert(Edge {
            affinity: 0.0,
            kind: None,
            updated_tick: tick,
        });
        e.affinity = (e.affinity + delta).clamp(-1.0, 1.0);
        if kind.is_some() {
            e.kind = kind;
        }
        e.updated_tick = tick;
    }

    pub fn adjust_both(&mut self, a: i64, b: i64, delta: f32, kind: Option<RelKind>, tick: i64) {
        self.adjust(a, b, delta, kind, tick);
        self.adjust(b, a, delta, kind, tick);
    }

    /// Drop edges nobody can use any more. Without this the map grows for the
    /// whole run: every creature that ever stood near another leaves an entry,
    /// and the dead never stop being remembered.
    pub fn forget_dead(&mut self, living: &dyn Fn(i64) -> bool) {
        self.edges.retain(|(a, b), _| living(*a) && living(*b));
    }

    pub fn insert_raw(&mut self, from: i64, to: i64, e: Edge) {
        self.edges.insert((from, to), e);
    }
}

// ---------------------------------------------------------------- courtship

/// An offer of courtship, awaiting an answer.
///
/// §4.8 requires pairing to be mutual and rejection to be possible and
/// recorded. Modelling it as an offer the other creature answers — rather than
/// as a coincidence of two creatures happening to want each other on the same
/// tick — is what makes rejection a thing that happens *to* somebody, and gives
/// the utility policy (and later the model) a decision with a loser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Offer {
    pub from: i64,
    pub to: i64,
    pub tick: i64,
}

#[derive(Debug, Default)]
pub struct Courtships {
    pub offers: Vec<Offer>,
}

impl Courtships {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn offer(&mut self, from: i64, to: i64, tick: i64) {
        if let Some(o) = self.offers.iter_mut().find(|o| o.from == from && o.to == to) {
            o.tick = tick;
            return;
        }
        self.offers.push(Offer { from, to, tick });
    }

    /// The oldest standing offer to this creature, so nobody is left waiting
    /// forever while newer suitors jump the queue.
    pub fn pending_for(&self, to: i64) -> Option<Offer> {
        self.offers
            .iter()
            .filter(|o| o.to == to)
            .min_by_key(|o| (o.tick, o.from))
            .copied()
    }

    pub fn remove_between(&mut self, a: i64, b: i64) {
        self.offers.retain(|o| !((o.from == a && o.to == b) || (o.from == b && o.to == a)));
    }

    pub fn remove_all_for(&mut self, id: i64) {
        self.offers.retain(|o| o.from != id && o.to != id);
    }

    /// Offers that have gone unanswered long enough to lapse.
    pub fn expire(&mut self, tick: i64, ttl: u32) -> Vec<Offer> {
        let mut lapsed = Vec::new();
        self.offers.retain(|o| {
            if tick - o.tick > ttl as i64 {
                lapsed.push(*o);
                false
            } else {
                true
            }
        });
        lapsed
    }
}

// ------------------------------------------------------------ reproduction

/// Why a pair cannot conceive right now. Returned rather than a bool so the
/// inspector can say *which* requirement is missing — "needs 6 more grain" is a
/// story, "cannot reproduce" is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Blocker {
    NotPaired,
    NotAdult,
    Unwell,
    NoHousehold,
    StoreShort,
    AlreadyExpecting,
    TooSoon,
}

impl Blocker {
    pub const ALL: [Blocker; 7] = [
        Blocker::NotPaired,
        Blocker::NotAdult,
        Blocker::Unwell,
        Blocker::NoHousehold,
        Blocker::StoreShort,
        Blocker::AlreadyExpecting,
        Blocker::TooSoon,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Blocker::NotPaired => "not paired",
            Blocker::NotAdult => "not of age",
            Blocker::Unwell => "not well enough",
            Blocker::NoHousehold => "no household",
            Blocker::StoreShort => "store below the reserve",
            Blocker::AlreadyExpecting => "already expecting",
            Blocker::TooSoon => "too soon after the last",
        }
    }
}

/// The four requirements of §4.8, checked in order of how cheap they are to
/// test and how likely they are to be the blocker worth reporting.
pub fn conception_blocker(
    mother: &Creature,
    father: &Creature,
    household: Option<&Household>,
    cfg: &WorldConfig,
    tick: i64,
) -> Option<Blocker> {
    let r = &cfg.reproduction;

    if mother.sex != Sex::Female || father.sex != Sex::Male {
        return Some(Blocker::NotPaired);
    }
    if mother.mate_id != Some(father.id) || father.mate_id != Some(mother.id) {
        return Some(Blocker::NotPaired);
    }
    if mother.pregnancy.is_some() {
        return Some(Blocker::AlreadyExpecting);
    }
    if mother.life_stage != LifeStage::Adult || father.life_stage != LifeStage::Adult {
        return Some(Blocker::NotAdult);
    }
    if mother.health < r.health_floor || father.health < r.health_floor {
        return Some(Blocker::Unwell);
    }
    // A pause between children, so a household does not commit itself to three
    // simultaneous dependents the moment its store first crosses the line.
    if let Some(last) = mother.last_birth_tick {
        if tick - last < r.birth_spacing_ticks as i64 {
            return Some(Blocker::TooSoon);
        }
    }

    // 3 and 4: a shared shelter with capacity, and a store above the reserve.
    let Some(h) = household else {
        return Some(Blocker::NoHousehold);
    };
    if h.shelter_id.is_none() {
        return Some(Blocker::NoHousehold);
    }
    if h.stored_food() < r.store_reserve {
        return Some(Blocker::StoreShort);
    }
    None
}

/// Inherit a trait from two parents, with a small mutation (§4.9).
///
/// The midpoint plus gaussian noise, rather than picking one parent's value:
/// blending is what lets a population drift as a population, which is what
/// makes trait drift across generations a readable signal rather than a random
/// walk between two founding values.
pub fn inherit(a: f32, b: f32, sigma: f32, rng: &mut impl rand::Rng) -> f32 {
    // Box-Muller, so mutation is normally distributed rather than uniform:
    // most children resemble their parents and a few genuinely do not.
    let u1: f32 = rng.gen::<f32>().max(1e-6);
    let u2: f32 = rng.gen::<f32>();
    let z = (-2.0 * u1.ln()).sqrt() * (std::f32::consts::TAU * u2).cos();
    ((a + b) / 2.0 + z * sigma).clamp(0.0, 1.0)
}

// ------------------------------------------------------- who else is here

/// What one creature can see about another without inspecting it.
#[derive(Debug, Clone, Copy)]
pub struct Bystander {
    pub id: i64,
    pub x: u32,
    pub y: u32,
    pub sex: Sex,
    pub life_stage: LifeStage,
    pub household_id: Option<i64>,
    pub paired: bool,
    pub guardian_id: Option<i64>,
    pub health: f32,
    pub hunger: f32,
    /// Which belief kinds this creature knows anything credible about, as a
    /// bitmask. Carried here so a creature can tell whether it has anything
    /// worth saying without reaching into another creature's memory.
    pub known_kinds: u16,
}

/// A spatial index over creatures, rebuilt each tick.
///
/// Every social action needs "who is next to me", and every ambient
/// observation needs "who is near me". Scanning the whole population per
/// creature is 250,000 comparisons a tick at 500 creatures; a coarse grid makes
/// it a handful. Rebuilt rather than maintained because creatures move every
/// tick, which is precisely the case where maintaining costs more than
/// rebuilding.
pub struct CreatureIndex {
    cell: u32,
    cols: u32,
    rows: u32,
    cells: Vec<Vec<u32>>,
    people: Vec<Bystander>,
    by_id: BTreeMap<i64, usize>,
}

impl CreatureIndex {
    pub fn new(width: u32, height: u32, cell: u32) -> Self {
        let cols = width.div_ceil(cell);
        let rows = height.div_ceil(cell);
        Self {
            cell,
            cols,
            rows,
            cells: vec![Vec::new(); (cols * rows) as usize],
            people: Vec::new(),
            by_id: BTreeMap::new(),
        }
    }

    pub fn rebuild<'a>(
        &mut self,
        creatures: impl Iterator<Item = &'a Creature>,
        tick: i64,
        kcfg: &crate::config::KnowledgeConfig,
    ) {
        for c in self.cells.iter_mut() {
            c.clear();
        }
        self.people.clear();
        self.by_id.clear();

        for c in creatures {
            let idx = self.people.len();
            self.people.push(Bystander {
                id: c.id,
                x: c.x,
                y: c.y,
                sex: c.sex,
                life_stage: c.life_stage,
                household_id: c.household_id,
                paired: c.mate_id.is_some(),
                guardian_id: c.guardian_id,
                health: c.health,
                hunger: c.hunger,
                known_kinds: crate::sim::knowledge::known_kinds(&c.beliefs, tick, kcfg),
            });
            self.by_id.insert(c.id, idx);
            let cx = (c.x / self.cell).min(self.cols - 1);
            let cy = (c.y / self.cell).min(self.rows - 1);
            self.cells[(cy * self.cols + cx) as usize].push(idx as u32);
        }
    }

    pub fn get(&self, id: i64) -> Option<&Bystander> {
        self.by_id.get(&id).map(|i| &self.people[*i])
    }

    pub fn len(&self) -> usize {
        self.people.len()
    }

    /// How many creatures belong to a household. Cheap because the index
    /// already holds everybody's membership.
    pub fn len_with_household(&self, household: Option<i64>) -> usize {
        match household {
            None => 0,
            Some(h) => self.people.iter().filter(|p| p.household_id == Some(h)).count(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.people.is_empty()
    }

    /// Everyone within `radius` of a tile, excluding `self_id`, in ascending
    /// creature-id order so the traversal never depends on grid layout.
    pub fn near(&self, x: u32, y: u32, radius: u32, self_id: i64, out: &mut Vec<Bystander>) {
        out.clear();
        let x0 = x.saturating_sub(radius) / self.cell;
        let y0 = y.saturating_sub(radius) / self.cell;
        let x1 = ((x + radius) / self.cell).min(self.cols - 1);
        let y1 = ((y + radius) / self.cell).min(self.rows - 1);
        let r2 = (radius as i64).pow(2);

        for cy in y0..=y1 {
            for cx in x0..=x1 {
                for &i in &self.cells[(cy * self.cols + cx) as usize] {
                    let p = self.people[i as usize];
                    if p.id == self_id {
                        continue;
                    }
                    if dist2(p.x, p.y, x, y) <= r2 {
                        out.push(p);
                    }
                }
            }
        }
        out.sort_unstable_by_key(|p| p.id);
    }
}

// ---------------------------------------------------------- social intents

/// A social act, recorded during action execution and applied in resolution.
///
/// Social actions are the one kind that reaches into another creature, and the
/// action executor holds an exclusive borrow of the actor — so it cannot also
/// hold one on the target. Rather than splitting the population or reaching for
/// interior mutability, an action states its intent and phase 6 applies it with
/// access to everybody. That also puts every two-sided outcome in one place:
/// pairing, rejection, feeding and teaching all resolve together, in creature
/// order, which is what keeps them reproducible.
#[derive(Debug, Clone, Copy)]
pub enum SocialIntent {
    Court { from: i64, to: i64 },
    Accept { from: i64, to: i64 },
    Reject { from: i64, to: i64 },
    GiveFood { from: i64, to: i64, quantity: f32 },
    FeedInfant { from: i64, to: i64, quantity: f32 },
    JoinHousehold { creature: i64, household: i64 },
    LeaveHousehold { creature: i64 },
    Share { from: i64, to: i64, topic: Option<crate::sim::knowledge::BeliefKind> },
    Teach { from: i64, to: i64 },
}

#[inline]
fn dist2(ax: u32, ay: u32, bx: u32, by: u32) -> i64 {
    let dx = ax as i64 - bx as i64;
    let dy = ay as i64 - by as i64;
    dx * dx + dy * dy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::creature::testing::test_creature;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn cfg() -> WorldConfig {
        WorldConfig::default()
    }

    fn pair() -> (Creature, Creature) {
        let mut mother = test_creature();
        mother.id = 1;
        mother.sex = Sex::Female;
        mother.mate_id = Some(2);
        let mut father = test_creature();
        father.id = 2;
        father.sex = Sex::Male;
        father.mate_id = Some(1);
        (mother, father)
    }

    fn household_with(food: f32, cfg: &WorldConfig) -> Household {
        let mut h = Household {
            id: 1,
            shelter_id: Some(7),
            founded_tick: 0,
            dissolved_tick: None,
            store: Inventory::default(),
            size_cap: cfg.actions.shelter_capacity,
            founder_ids: (1, Some(2)),
            dirty: false,
        };
        h.store.add(ItemKind::Grain, food, 0);
        h
    }

    #[test]
    fn a_paired_couple_with_a_home_and_a_full_store_can_conceive() {
        let c = cfg();
        let (m, f) = pair();
        let h = household_with(c.reproduction.store_reserve + 1.0, &c);
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, 400), None);
    }

    #[test]
    fn the_store_reserve_is_the_gate_that_makes_farming_necessary() {
        // §4.4: only grain keeps, so a household that never farms can feed
        // itself and still never breed. This is the mechanism S4 rests on.
        let c = cfg();
        let (m, f) = pair();
        let short = household_with(c.reproduction.store_reserve - 1.0, &c);
        assert_eq!(
            conception_blocker(&m, &f, Some(&short), &c, 400),
            Some(Blocker::StoreShort)
        );
    }

    #[test]
    fn a_household_of_perishables_cannot_reach_the_reserve_for_long() {
        let c = cfg();
        let (m, f) = pair();
        let mut h = household_with(0.0, &c);
        h.store.add(ItemKind::Forage, c.reproduction.store_reserve + 5.0, 0);

        // Berries clear the bar on the day they are picked...
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, 10), None);
        // ...and then they rot, which grain never does.
        crate::sim::economy::spoil(&mut h.store, 100, &c);
        assert_eq!(h.stored_food(), 0.0);
        assert_eq!(
            conception_blocker(&m, &f, Some(&h), &c, 100),
            Some(Blocker::StoreShort)
        );
    }

    #[test]
    fn every_requirement_of_section_4_8_is_actually_checked() {
        let c = cfg();
        let h = household_with(999.0, &c);

        let (mut m, f) = pair();
        m.mate_id = None;
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, 400), Some(Blocker::NotPaired));

        let (mut m, f) = pair();
        m.life_stage = LifeStage::Elder;
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, 400), Some(Blocker::NotAdult));

        let (mut m, f) = pair();
        m.health = c.reproduction.health_floor - 1.0;
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, 400), Some(Blocker::Unwell));

        let (m, f) = pair();
        assert_eq!(conception_blocker(&m, &f, None, &c, 400), Some(Blocker::NoHousehold));

        let (m, f) = pair();
        let mut homeless = household_with(999.0, &c);
        homeless.shelter_id = None;
        assert_eq!(
            conception_blocker(&m, &f, Some(&homeless), &c, 400),
            Some(Blocker::NoHousehold)
        );
    }

    #[test]
    fn children_are_spaced() {
        let c = cfg();
        let (mut m, f) = pair();
        let h = household_with(999.0, &c);
        m.last_birth_tick = Some(400);
        assert_eq!(
            conception_blocker(&m, &f, Some(&h), &c, 401),
            Some(Blocker::TooSoon)
        );
        let later = 400 + c.reproduction.birth_spacing_ticks as i64 + 1;
        assert_eq!(conception_blocker(&m, &f, Some(&h), &c, later), None);
    }

    #[test]
    fn affinity_saturates_rather_than_running_away() {
        let mut r = Relationships::new();
        for t in 0..500 {
            r.adjust(1, 2, 0.05, None, t);
        }
        assert!(r.get(1, 2) <= 1.0);
        for t in 0..2000 {
            r.adjust(1, 2, -0.05, None, t);
        }
        assert!(r.get(1, 2) >= -1.0);
    }

    #[test]
    fn relationships_are_directed() {
        let mut r = Relationships::new();
        r.adjust(1, 2, 0.4, Some(RelKind::Acquaintance), 10);
        assert_eq!(r.get(1, 2), 0.4);
        assert_eq!(r.get(2, 1), 0.0, "being liked is not the same as liking");
    }

    #[test]
    fn the_dead_are_forgotten() {
        let mut r = Relationships::new();
        r.adjust_both(1, 2, 0.5, None, 0);
        r.adjust_both(1, 3, 0.5, None, 0);
        r.forget_dead(&|id| id != 3);
        assert_eq!(r.len(), 2, "only the pair of living edges survives");
        assert_eq!(r.get(1, 3), 0.0);
    }

    #[test]
    fn the_oldest_offer_is_answered_first() {
        let mut c = Courtships::new();
        c.offer(5, 1, 100);
        c.offer(4, 1, 90);
        c.offer(6, 1, 110);
        assert_eq!(c.pending_for(1).unwrap().from, 4);
        c.remove_between(4, 1);
        assert_eq!(c.pending_for(1).unwrap().from, 5);
    }

    #[test]
    fn offers_lapse_if_nobody_answers() {
        let mut c = Courtships::new();
        c.offer(2, 1, 100);
        assert!(c.expire(110, 20).is_empty());
        let lapsed = c.expire(130, 20);
        assert_eq!(lapsed.len(), 1);
        assert!(c.pending_for(1).is_none());
    }

    #[test]
    fn a_dissolved_household_hands_on_what_it_held() {
        let c = cfg();
        let mut hs = Households::new();
        let id = hs.found(Some(1), 1, Some(2), 0, &c);
        hs.get_mut(id).unwrap().store.add(ItemKind::Grain, 30.0, 0);

        let mut counts = BTreeMap::new();
        counts.insert(id, 0u32);
        let estates = hs.reap(&counts, 500);

        assert_eq!(estates.len(), 1);
        assert_eq!(estates[0].1.total(ItemKind::Grain), 30.0, "grain is not destroyed by death");
        assert!(hs.get(id).is_none(), "the household is gone");
    }

    #[test]
    fn a_household_with_members_is_not_reaped() {
        let c = cfg();
        let mut hs = Households::new();
        let id = hs.found(Some(1), 1, Some(2), 0, &c);
        let mut counts = BTreeMap::new();
        counts.insert(id, 2u32);
        assert!(hs.reap(&counts, 500).is_empty());
        assert!(hs.get(id).is_some());
    }

    #[test]
    fn the_creature_index_finds_neighbours_and_excludes_the_asker() {
        let mut ix = CreatureIndex::new(128, 128, 8);
        let mut people = Vec::new();
        for (i, (x, y)) in [(10u32, 10u32), (12, 11), (100, 100)].into_iter().enumerate() {
            let mut c = test_creature();
            c.id = i as i64 + 1;
            (c.x, c.y) = (x, y);
            people.push(c);
        }
        ix.rebuild(people.iter(), 0, &crate::config::KnowledgeConfig::default());

        let mut out = Vec::new();
        ix.near(10, 10, 6, 1, &mut out);
        assert_eq!(out.iter().map(|p| p.id).collect::<Vec<_>>(), vec![2],
                   "the far one is out of range and the asker is not its own neighbour");

        ix.near(10, 10, 200, 99, &mut out);
        assert_eq!(out.len(), 3, "nobody is excluded when the asker is not present");
        assert!(out.windows(2).all(|w| w[0].id < w[1].id), "ascending id, always");
    }

    #[test]
    fn a_child_resembles_its_parents_and_occasionally_does_not() {
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let sigma = cfg().reproduction.mutation_sigma;
        let kids: Vec<f32> = (0..400).map(|_| inherit(0.8, 0.6, sigma, &mut rng)).collect();

        let mean = kids.iter().sum::<f32>() / kids.len() as f32;
        assert!((mean - 0.7).abs() < 0.03, "should centre on the parents' midpoint, got {mean}");
        assert!(kids.iter().all(|v| (0.0..=1.0).contains(v)), "always a legal trait value");
        assert!(
            kids.iter().any(|v| (v - 0.7).abs() > sigma * 1.5),
            "and the tail has to exist, or nothing ever drifts"
        );
    }

    #[test]
    fn inheritance_is_deterministic_for_a_given_stream() {
        let sigma = cfg().reproduction.mutation_sigma;
        let a = inherit(0.5, 0.5, sigma, &mut ChaCha8Rng::seed_from_u64(1));
        let b = inherit(0.5, 0.5, sigma, &mut ChaCha8Rng::seed_from_u64(1));
        assert_eq!(a, b);
    }
}
