//! The belief substrate (PRD §4.11).
//!
//! No creature sees the whole map. Each carries a private, incomplete and
//! possibly wrong model of the world, built from what it has personally seen.
//! Transmission between creatures lands at M4; what is here is the substrate
//! those channels will move things over — and, critically, the thing the Tier 1
//! policy navigates by. **Tier 1 reads beliefs, never ground truth.** If the
//! deterministic policy were omniscient, stale belief would stop punishing
//! over-commitment, the volatility §5.5 depends on would vanish, and the belief
//! layer would be decorative long before the LLM arrived to use it.

use crate::config::KnowledgeConfig;
use crate::sim::world::NodeKind;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BeliefKind {
    Water,
    ForageNode,
    WoodNode,
    SoilPatch,
    SheepFlock,
    Shelter,
    HouseholdTerritory,
    Danger,
    Person,
}

impl BeliefKind {
    pub fn as_str(self) -> &'static str {
        match self {
            BeliefKind::Water => "WATER",
            BeliefKind::ForageNode => "FORAGE_NODE",
            BeliefKind::WoodNode => "WOOD_NODE",
            BeliefKind::SoilPatch => "SOIL_PATCH",
            BeliefKind::SheepFlock => "SHEEP_FLOCK",
            BeliefKind::Shelter => "SHELTER",
            BeliefKind::HouseholdTerritory => "HOUSEHOLD_TERRITORY",
            BeliefKind::Danger => "DANGER",
            BeliefKind::Person => "PERSON",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "WATER" => BeliefKind::Water,
            "FORAGE_NODE" => BeliefKind::ForageNode,
            "WOOD_NODE" => BeliefKind::WoodNode,
            "SOIL_PATCH" => BeliefKind::SoilPatch,
            "SHEEP_FLOCK" => BeliefKind::SheepFlock,
            "SHELTER" => BeliefKind::Shelter,
            "HOUSEHOLD_TERRITORY" => BeliefKind::HouseholdTerritory,
            "DANGER" => BeliefKind::Danger,
            "PERSON" => BeliefKind::Person,
            _ => return None,
        })
    }

    /// How fast a belief of this kind goes stale, as a multiple of the base
    /// decay rate.
    ///
    /// §4.11 motivates confidence decay with a forage node that "may have been
    /// stripped since", and that is exactly right *for a forage node*. Applying
    /// the same rate to everything is not: a river is in the same place next
    /// month, and a creature that forgets where the water is because it has not
    /// been for a fortnight is not modelling fallible memory, it is modelling
    /// amnesia.
    ///
    /// Measured before this existed: creatures lost their water beliefs while
    /// exploring, stopped being able to plan a drink at all, and dehydration
    /// took 36% of all deaths. Landmarks persist; harvests do not.
    pub fn decay_scale(self) -> f32 {
        match self {
            // Terrain. It is where it was.
            BeliefKind::Water => 0.05,
            BeliefKind::SoilPatch => 0.1,
            // Built things last unless they fall down.
            BeliefKind::Shelter => 0.25,
            // A chopped stand of trees is still a place trees grow.
            BeliefKind::WoodNode => 0.6,
            // The PRD's own example, and the reference rate.
            BeliefKind::ForageNode => 1.0,
            // They wander off by themselves.
            BeliefKind::SheepFlock => 2.0,
            BeliefKind::Person | BeliefKind::Danger | BeliefKind::HouseholdTerritory => 1.0,
        }
    }

    pub fn of_node(kind: NodeKind) -> Self {
        match kind {
            NodeKind::Forage => BeliefKind::ForageNode,
            NodeKind::Wood => BeliefKind::WoodNode,
            NodeKind::Wheat => BeliefKind::SoilPatch,
            NodeKind::Sheep => BeliefKind::SheepFlock,
        }
    }
}

/// What the creature thinks is there. Deliberately coarse — a creature
/// remembers "plentiful" or "picked over", not a float, which is also what the
/// prompt will render at M3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Estimate {
    Empty,
    Sparse,
    Some,
    Plentiful,
}

impl Estimate {
    pub fn of(quantity: f32, max: f32) -> Self {
        let f = if max > 0.0 { quantity / max } else { 0.0 };
        if f <= 0.02 {
            Estimate::Empty
        } else if f < 0.25 {
            Estimate::Sparse
        } else if f < 0.65 {
            Estimate::Some
        } else {
            Estimate::Plentiful
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Estimate::Empty => "empty",
            Estimate::Sparse => "picked over",
            Estimate::Some => "some",
            Estimate::Plentiful => "plentiful",
        }
    }

    /// How much a creature expects to get out of going there.
    pub fn expected_value(self) -> f32 {
        match self {
            Estimate::Empty => 0.0,
            Estimate::Sparse => 0.28,
            Estimate::Some => 0.65,
            Estimate::Plentiful => 1.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Belief {
    pub kind: BeliefKind,
    pub x: u32,
    pub y: u32,
    pub estimate: Estimate,
    /// Confidence at `last_verified_tick`. Current confidence is this decayed
    /// by elapsed time — see `confidence_at`, which is the only correct way to
    /// read it. Storing the decayed value would mean touching every belief on
    /// every tick for no gain.
    pub confidence: f32,
    pub learned_tick: i64,
    pub last_verified_tick: i64,
    pub source_creature_id: Option<i64>,
    /// How far this has travelled from firsthand observation. Each transmission
    /// degrades confidence, so secondhand knowledge is genuinely worse.
    pub hops: u8,
    /// Survives every retransmission, so a belief can always be traced to the
    /// creature that first saw the thing — even long after it is dead. This is
    /// the pair that answers S7 (§7).
    pub origin_creature_id: Option<i64>,
    pub origin_tick: i64,
}

impl Belief {
    /// Confidence decays with time since last verified (§4.11). A forage node
    /// reported 100 ticks ago may have been stripped since.
    pub fn confidence_at(&self, tick: i64, cfg: &KnowledgeConfig) -> f32 {
        let elapsed = (tick - self.last_verified_tick).max(0) as f32;
        let rate = cfg.confidence_decay_per_tick * self.kind.decay_scale();
        (self.confidence - elapsed * rate).clamp(0.0, 1.0)
    }

    pub fn is_firsthand(&self) -> bool {
        self.hops == 0
    }

    /// Plain-language provenance. Used by the inspector now and by the prompt at
    /// M3, where the phrasing is what lets the model reason about how far to
    /// commit rather than treating all knowledge as equally solid (§5.7).
    pub fn provenance(&self, tick: i64) -> String {
        let age = tick - self.last_verified_tick;
        let when = if age < 24 {
            "recently".to_string()
        } else if age < 168 {
            format!("{} days ago", (age / 24).max(1))
        } else {
            format!("{} weeks ago", age / 168)
        };
        if self.is_firsthand() {
            format!("you saw it yourself, {when}")
        } else if self.hops == 1 {
            format!("someone told you, {when}")
        } else {
            format!("{} hops from anyone who saw it, {when}", self.hops)
        }
    }
}

/// What a creature currently wants, used to weight belief relevance. Values are
/// 0..1 where 1 means "this need is the pressing one".
#[derive(Debug, Clone, Copy, Default)]
pub struct NeedProfile {
    pub food: f32,
    pub water: f32,
    pub fuel: f32,
    pub shelter: f32,
}

impl NeedProfile {
    /// How well a belief of this kind serves what the creature wants.
    pub fn match_for(&self, kind: BeliefKind) -> f32 {
        match kind {
            BeliefKind::Water => self.water,
            BeliefKind::ForageNode => self.food,
            BeliefKind::SheepFlock => self.food * 0.8,
            // Soil is both a wheat source to harvest and the place to plant.
            BeliefKind::SoilPatch => self.food * 0.7,
            BeliefKind::WoodNode => self.fuel,
            BeliefKind::Shelter => self.shelter,
            BeliefKind::Danger => 0.0,
            _ => 0.15,
        }
    }
}

/// Relevance ranking over a creature's beliefs.
///
/// **This answers PRD §13.4.** A well-travelled creature accumulates far more
/// beliefs than fit in a prompt; naive truncation loses the wheat field and
/// naive retention blows up the context. The PRD asks for a ranking over
/// confidence, distance, recency and current need, built at M2 and tuned at M3.
///
/// It is deliberately built here rather than at M3 because the Tier 1 policy
/// needs exactly the same question answered — "which of the things I believe is
/// worth acting on right now?" — so the ranking is exercised and tuned by a
/// working simulation long before a prompt ever consumes it. If it were built
/// at M3 its first real test would be inside the component hardest to debug.
///
/// The four terms are kept separate rather than folded together because they
/// fail differently:
///
/// * **confidence** — do I believe this is true? Already decayed for elapsed
///   time, and penalised per hop at transmission.
/// * **recency** — how long since anyone checked? Correlated with confidence
///   but not identical: a 3-hop belief heard 10 ticks ago and a firsthand one
///   from 100 ticks ago can carry equal confidence and very different staleness
///   risk. Keeping it separate is what lets a creature prefer the fresher of
///   two equally-credible options.
/// * **distance** — what will acting on this cost me? Hyperbolic, not linear,
///   so a node twice as far is worth appreciably less than half as much.
/// * **need** — does it help with what is actually pressing?
///
/// Multiplicative, so any term near zero rules the belief out: a plentiful
/// forage node I no longer believe in is not a plan, and neither is a certain
/// one on the far side of the map when I am about to die of thirst.
pub fn relevance(
    belief: &Belief,
    from: (u32, u32),
    tick: i64,
    needs: &NeedProfile,
    cfg: &KnowledgeConfig,
) -> f32 {
    let need = needs.match_for(belief.kind);
    if need <= 0.0 {
        return 0.0;
    }
    target_quality(belief, from, tick, cfg) * need
}

/// How good a target this belief is, independent of what the creature wants —
/// credibility, reachability, freshness and expected yield.
///
/// Kept separate from `relevance` because the two have different callers.
/// Ranking beliefs (for a prompt, or to choose between a water source and a
/// wood node) must weight by current need. *Scoring* a candidate plan must not:
/// the policy has already multiplied by that need's urgency, and folding it in
/// twice squashes every concrete option flat and leaves a creature wandering
/// instead of eating.
pub fn target_quality(
    belief: &Belief,
    from: (u32, u32),
    tick: i64,
    cfg: &KnowledgeConfig,
) -> f32 {
    let confidence = belief.confidence_at(tick, cfg);
    if confidence <= 0.0 {
        return 0.0;
    }

    let dx = belief.x as f32 - from.0 as f32;
    let dy = belief.y as f32 - from.1 as f32;
    let dist = (dx * dx + dy * dy).sqrt();
    // 24 tiles is roughly a day's travel there and back, which is the natural
    // scale for "is this near me".
    let distance_term = 1.0 / (1.0 + dist / 24.0);

    let elapsed = (tick - belief.last_verified_tick).max(0) as f32;
    let recency_term = 1.0 / (1.0 + elapsed / 168.0);

    confidence * distance_term * recency_term * belief.estimate.expected_value().max(0.05)
}

/// Insert or refresh a belief, keeping the better of the two.
///
/// Firsthand observation always wins over hearsay about the same place, which
/// is what makes walking somewhere and looking worth doing.
pub fn upsert(
    beliefs: &mut Vec<Belief>,
    incoming: Belief,
    at: (u32, u32),
    cfg: &KnowledgeConfig,
    max_beliefs: usize,
    tick: i64,
) -> bool {
    if let Some(existing) = beliefs
        .iter_mut()
        .find(|b| b.kind == incoming.kind && b.x == incoming.x && b.y == incoming.y)
    {
        let take = incoming.hops < existing.hops
            || incoming.last_verified_tick > existing.last_verified_tick;
        if take {
            // Provenance of the original discovery survives: the creature who
            // first saw the thing keeps the credit however often it is re-seen.
            let origin_creature_id = existing.origin_creature_id.or(incoming.origin_creature_id);
            let origin_tick = existing.origin_tick.min(incoming.origin_tick);
            let learned_tick = existing.learned_tick.min(incoming.learned_tick);
            *existing = Belief { origin_creature_id, origin_tick, learned_tick, ..incoming };
        }
        return false;
    }

    beliefs.push(incoming);

    // Memory is finite. When it overflows, the least relevant belief is what
    // gets forgotten — with no need pressure, so forgetting is about credibility
    // and reachability rather than about what is urgent this minute.
    if beliefs.len() > max_beliefs {
        let worst = least_worth_keeping(beliefs, tick, cfg);
        beliefs.remove(worst);
    }
    let _ = at;
    true
}

/// Which belief to forget when memory is full.
///
/// Deliberately *not* distance-weighted, and this is the subtlety that matters.
/// Distance belongs to deciding what to act on: a water source thirty tiles
/// away is a poor plan right now. It does not belong to deciding what to
/// remember, because a creature that walked away from the only water it knows
/// would evict that memory precisely because it walked away — and then die of
/// thirst a hundred tiles out with no idea where to turn back to.
///
/// Measured: with distance in the rule, dehydration rose from 9% to 34% of all
/// deaths the moment there were enough forage nodes to fill memory, because
/// berries near the creature crowded out the river behind it.
///
/// What survives instead is what is worth knowing: credible, fresh, and not
/// known to be empty — with a strong bonus for being the last thing a creature
/// knows about its kind, so nobody forgets water to remember a fourth berry
/// bush.
fn least_worth_keeping(beliefs: &[Belief], tick: i64, cfg: &KnowledgeConfig) -> usize {
    let mut worst = 0usize;
    let mut worst_score = f32::MAX;
    for (i, b) in beliefs.iter().enumerate() {
        let elapsed = (tick - b.last_verified_tick).max(0) as f32;
        let recency = 1.0 / (1.0 + elapsed / 168.0);
        let only_one_of_its_kind = !beliefs
            .iter()
            .enumerate()
            .any(|(j, o)| j != i && o.kind == b.kind);
        let scarcity = if only_one_of_its_kind { 4.0 } else { 1.0 };

        let s = b.confidence_at(tick, cfg)
            * recency
            * b.estimate.expected_value().max(0.05)
            * scarcity;
        if s < worst_score {
            worst_score = s;
            worst = i;
        }
    }
    worst
}

/// Drop beliefs that have decayed to nothing. A creature genuinely forgets:
/// the clearing it heard about three weeks ago stops being a place it knows.
pub fn forget_expired(beliefs: &mut Vec<Belief>, tick: i64, cfg: &KnowledgeConfig) -> usize {
    let before = beliefs.len();
    beliefs.retain(|b| b.confidence_at(tick, cfg) > 0.02);
    before - beliefs.len()
}

/// The best belief of the given kinds by relevance, and its score.
pub fn best_of(
    beliefs: &[Belief],
    kinds: &[BeliefKind],
    from: (u32, u32),
    tick: i64,
    needs: &NeedProfile,
    cfg: &KnowledgeConfig,
) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, b) in beliefs.iter().enumerate() {
        if !kinds.contains(&b.kind) {
            continue;
        }
        let s = relevance(b, from, tick, needs, cfg);
        if s <= 0.0 {
            continue;
        }
        // Ties break on index, never on float equality, so the choice is stable.
        if best.is_none_or(|(_, bs)| s > bs) {
            best = Some((i, s));
        }
    }
    best.map(|(i, _)| i)
}

/// The top `n` beliefs by relevance. This is the function M3's prompt assembly
/// calls; Tier 1 uses `best_of` for a single target.
pub fn rank(
    beliefs: &[Belief],
    from: (u32, u32),
    tick: i64,
    needs: &NeedProfile,
    cfg: &KnowledgeConfig,
    n: usize,
) -> Vec<usize> {
    let mut scored: Vec<(usize, f32)> = beliefs
        .iter()
        .enumerate()
        .map(|(i, b)| (i, relevance(b, from, tick, needs, cfg)))
        .filter(|(_, s)| *s > 0.0)
        .collect();
    // Descending score, index ascending as the tiebreak: total and stable.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    scored.into_iter().take(n).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> KnowledgeConfig {
        KnowledgeConfig::default()
    }

    fn belief(kind: BeliefKind, x: u32, y: u32, verified: i64) -> Belief {
        Belief {
            kind,
            x,
            y,
            estimate: Estimate::Plentiful,
            confidence: 1.0,
            learned_tick: verified,
            last_verified_tick: verified,
            source_creature_id: None,
            hops: 0,
            origin_creature_id: Some(1),
            origin_tick: verified,
        }
    }

    #[test]
    fn a_river_is_remembered_far_longer_than_a_berry_patch() {
        let c = cfg();
        let water = belief(BeliefKind::Water, 10, 10, 0);
        let forage = belief(BeliefKind::ForageNode, 10, 10, 0);
        let sheep = belief(BeliefKind::SheepFlock, 10, 10, 0);

        assert!(water.confidence_at(600, &c) > 0.8, "the river has not moved");
        assert!(forage.confidence_at(600, &c) < 0.2, "the clearing may well be bare");
        assert!(
            sheep.confidence_at(300, &c) < forage.confidence_at(300, &c),
            "a flock walks away faster than a bush is picked"
        );
    }

    #[test]
    fn confidence_decays_and_never_rises_without_a_verify() {
        let b = belief(BeliefKind::ForageNode, 10, 10, 0);
        let c = cfg();
        let mut last = f32::MAX;
        for t in [0, 10, 50, 100, 200, 400] {
            let now = b.confidence_at(t, &c);
            assert!(now <= last, "confidence must be monotonically non-increasing");
            last = now;
        }
        assert_eq!(b.confidence_at(10_000, &c), 0.0, "clamped at zero, never negative");
    }

    #[test]
    fn verifying_restores_confidence() {
        let mut beliefs = vec![belief(BeliefKind::ForageNode, 5, 5, 0)];
        let c = cfg();
        assert!(beliefs[0].confidence_at(300, &c) < 0.3);

        upsert(&mut beliefs, belief(BeliefKind::ForageNode, 5, 5, 300), (5, 5), &c, 48, 300);

        assert_eq!(beliefs.len(), 1, "same place is a refresh, not a second belief");
        assert!(beliefs[0].confidence_at(300, &c) > 0.9);
    }

    #[test]
    fn firsthand_observation_displaces_hearsay_about_the_same_place() {
        let c = cfg();
        let mut hearsay = belief(BeliefKind::WoodNode, 9, 9, 100);
        hearsay.hops = 3;
        hearsay.confidence = 0.4;
        hearsay.origin_creature_id = Some(77);
        hearsay.origin_tick = 40;
        let mut beliefs = vec![hearsay];

        upsert(&mut beliefs, belief(BeliefKind::WoodNode, 9, 9, 120), (9, 9), &c, 48, 120);

        assert_eq!(beliefs.len(), 1);
        assert_eq!(beliefs[0].hops, 0, "seeing it yourself beats being told");
        assert_eq!(
            beliefs[0].origin_creature_id,
            Some(77),
            "but the original discoverer keeps the credit — this is the S7 column"
        );
        assert_eq!(beliefs[0].origin_tick, 40);
    }

    #[test]
    fn a_stale_belief_is_forgotten_entirely() {
        let c = cfg();
        let mut beliefs = vec![belief(BeliefKind::ForageNode, 1, 1, 0)];
        assert_eq!(forget_expired(&mut beliefs, 100, &c), 0);
        assert_eq!(forget_expired(&mut beliefs, 5_000, &c), 1);
        assert!(beliefs.is_empty());
    }

    #[test]
    fn relevance_prefers_the_near_certain_thing_over_the_far_rumour() {
        let c = cfg();
        let needs = NeedProfile { food: 1.0, ..Default::default() };

        let near = belief(BeliefKind::ForageNode, 12, 10, 100);
        let mut far = belief(BeliefKind::ForageNode, 200, 180, 100);
        far.hops = 2;
        far.confidence = 0.5;

        let rn = relevance(&near, (10, 10), 100, &needs, &c);
        let rf = relevance(&far, (10, 10), 100, &needs, &c);
        assert!(rn > rf, "near firsthand {rn} should beat far hearsay {rf}");
    }

    #[test]
    fn relevance_follows_the_need_not_just_the_distance() {
        let c = cfg();
        let water = belief(BeliefKind::Water, 40, 40, 100);
        let forage = belief(BeliefKind::ForageNode, 12, 10, 100);

        let thirsty = NeedProfile { water: 1.0, food: 0.1, ..Default::default() };
        assert!(
            relevance(&water, (10, 10), 100, &thirsty, &c)
                > relevance(&forage, (10, 10), 100, &thirsty, &c),
            "a thirsty creature should rank distant water above near berries"
        );

        let hungry = NeedProfile { water: 0.1, food: 1.0, ..Default::default() };
        assert!(
            relevance(&forage, (10, 10), 100, &hungry, &c)
                > relevance(&water, (10, 10), 100, &hungry, &c)
        );
    }

    #[test]
    fn an_emptied_node_stops_being_worth_walking_to() {
        let c = cfg();
        let needs = NeedProfile { food: 1.0, ..Default::default() };
        let mut b = belief(BeliefKind::ForageNode, 12, 10, 100);
        let full = relevance(&b, (10, 10), 100, &needs, &c);
        b.estimate = Estimate::Empty;
        let empty = relevance(&b, (10, 10), 100, &needs, &c);
        assert!(empty < full * 0.1, "a clearing known to be bare should not attract");
    }

    #[test]
    fn memory_is_capped_and_forgets_the_least_useful() {
        let c = cfg();
        let mut beliefs = Vec::new();
        upsert(&mut beliefs, belief(BeliefKind::Water, 100, 100, 500), (100, 100), &c, 4, 500);
        for i in 0..8u32 {
            upsert(
                &mut beliefs,
                belief(BeliefKind::ForageNode, 400 + i, 400, 500),
                (100, 100),
                &c,
                4,
                500,
            );
        }
        assert_eq!(beliefs.len(), 4, "capped");
        assert!(
            beliefs.iter().any(|b| b.kind == BeliefKind::Water),
            "the only water a creature knows must survive the cull"
        );
    }

    #[test]
    fn walking_away_from_the_river_does_not_erase_it() {
        // The regression that cost a third of the population to thirst: memory
        // must not be pruned by how far away the thing is.
        let c = cfg();
        let mut beliefs = vec![belief(BeliefKind::Water, 10, 10, 500)];
        // Fill memory with berries clustered where the creature now stands,
        // far from the water it drank at.
        for i in 0..12u32 {
            upsert(
                &mut beliefs,
                belief(BeliefKind::ForageNode, 300 + i, 300, 500),
                (300, 300),
                &c,
                6,
                500,
            );
        }
        assert_eq!(beliefs.len(), 6);
        assert!(
            beliefs.iter().any(|b| b.kind == BeliefKind::Water),
            "the creature must still know where the river is"
        );
    }

    #[test]
    fn what_is_known_to_be_empty_is_forgotten_first() {
        let c = cfg();
        let mut beliefs = Vec::new();
        for i in 0..4u32 {
            let mut b = belief(BeliefKind::ForageNode, 10 + i, 10, 500);
            if i == 2 {
                b.estimate = Estimate::Empty;
            }
            beliefs.push(b);
        }
        assert_eq!(least_worth_keeping(&beliefs, 500, &c), 2);
    }

    #[test]
    fn ranking_is_stable_and_ordered() {
        let c = cfg();
        let needs = NeedProfile { food: 1.0, water: 1.0, ..Default::default() };
        let beliefs = vec![
            belief(BeliefKind::ForageNode, 200, 200, 100),
            belief(BeliefKind::Water, 11, 10, 100),
            belief(BeliefKind::ForageNode, 14, 10, 100),
        ];
        let top = rank(&beliefs, (10, 10), 100, &needs, &c, 2);
        assert_eq!(top, vec![1, 2], "nearest first, and the far one drops out");
        assert_eq!(top, rank(&beliefs, (10, 10), 100, &needs, &c, 2), "stable");
    }

    #[test]
    fn provenance_reads_as_language_not_numbers() {
        let mut b = belief(BeliefKind::Water, 1, 1, 100);
        assert!(b.provenance(110).contains("yourself"));
        b.hops = 1;
        assert!(b.provenance(300).contains("told you"));
        b.hops = 3;
        assert!(b.provenance(300).contains("3 hops"));
    }
}
