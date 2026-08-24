//! Creatures: identity, needs, life stage, lifespan, and what they carry.
//!
//! Determinism note (invariant 7): nothing in this module iterates a HashMap.
//! Creatures live in a `Vec` held in ascending id order and every per-creature
//! loop walks it in that order, so a run is reproducible from its seed.

use crate::config::{LifespanConfig, NeedsConfig, WorldConfig};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Sex {
    Female,
    Male,
}

impl Sex {
    pub fn as_str(self) -> &'static str {
        match self {
            Sex::Female => "FEMALE",
            Sex::Male => "MALE",
        }
    }
    pub fn parse(s: &str) -> Self {
        if s == "MALE" { Sex::Male } else { Sex::Female }
    }
}

/// PRD §4.7. Adulthood is subdivided further for deliberation weighting at M4;
/// the three stages here are the ones that change what a creature *can do*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum LifeStage {
    Infant,
    Adult,
    Elder,
}

impl LifeStage {
    pub fn as_str(self) -> &'static str {
        match self {
            LifeStage::Infant => "INFANT",
            LifeStage::Adult => "ADULT",
            LifeStage::Elder => "ELDER",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "INFANT" => LifeStage::Infant,
            "ELDER" => LifeStage::Elder,
            _ => LifeStage::Adult,
        }
    }

    pub fn of(age: i64, cfg: &LifespanConfig) -> Self {
        if age < cfg.infant_until_tick as i64 {
            LifeStage::Infant
        } else if age >= cfg.elder_from_tick as i64 {
            LifeStage::Elder
        } else {
            LifeStage::Adult
        }
    }

    /// Infants cannot gather or work; they follow a guardian and are fed (§4.7).
    pub fn can_work(self) -> bool {
        !matches!(self, LifeStage::Infant)
    }

    /// Elders have reduced carry and speed (§4.7).
    pub fn work_rate(self) -> f32 {
        match self {
            LifeStage::Infant => 0.0,
            LifeStage::Adult => 1.0,
            LifeStage::Elder => 0.62,
        }
    }

    pub fn carry_scale(self) -> f32 {
        match self {
            LifeStage::Infant => 0.25,
            LifeStage::Adult => 1.0,
            LifeStage::Elder => 0.7,
        }
    }
}

/// Always recorded (§4.6). `Childbirth` is unreachable until M4 but is part of
/// the enum now so the stored strings never have to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum DeathCause {
    Starvation,
    Dehydration,
    Exposure,
    OldAge,
    Illness,
    Accident,
    Childbirth,
}

impl DeathCause {
    pub fn as_str(self) -> &'static str {
        match self {
            DeathCause::Starvation => "STARVATION",
            DeathCause::Dehydration => "DEHYDRATION",
            DeathCause::Exposure => "EXPOSURE",
            DeathCause::OldAge => "OLD_AGE",
            DeathCause::Illness => "ILLNESS",
            DeathCause::Accident => "ACCIDENT",
            DeathCause::Childbirth => "CHILDBIRTH",
        }
    }

    pub const ALL: [DeathCause; 7] = [
        DeathCause::Starvation,
        DeathCause::Dehydration,
        DeathCause::Exposure,
        DeathCause::OldAge,
        DeathCause::Illness,
        DeathCause::Accident,
        DeathCause::Childbirth,
    ];
}

/// Heritable personality (§4.9). At M2 these bias the utility policy's weights;
/// from M3 they are also rendered into the prompt as description rather than
/// applied as stat modifiers, so they shape decisions and not outcomes.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Traits {
    pub boldness: f32,
    pub industry: f32,
    pub sociability: f32,
    pub caution: f32,
}

impl Default for Traits {
    fn default() -> Self {
        Self { boldness: 0.5, industry: 0.5, sociability: 0.5, caution: 0.5 }
    }
}

impl Traits {
    pub fn random(rng: &mut impl rand::Rng) -> Self {
        // Centred rather than uniform: a population of extremists is less
        // interesting than one with a middle and tails.
        let mut d = || (rng.gen::<f32>() + rng.gen::<f32>() + rng.gen::<f32>()) / 3.0;
        Self { boldness: d(), industry: d(), sociability: d(), caution: d() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum ItemKind {
    Forage,
    Meat,
    Grain,
    Wood,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemKind::Forage => "FORAGE",
            ItemKind::Meat => "MEAT",
            ItemKind::Grain => "GRAIN",
            ItemKind::Wood => "WOOD",
        }
    }

    pub fn is_food(self) -> bool {
        !matches!(self, ItemKind::Wood)
    }

    /// Food value per unit when eaten. Grain is dense; forage is not, which is
    /// half of why foraging is a life and farming is a lineage (§4.4).
    pub fn nutrition(self) -> f32 {
        match self {
            ItemKind::Forage => 6.0,
            ItemKind::Meat => 14.0,
            ItemKind::Grain => 11.0,
            ItemKind::Wood => 0.0,
        }
    }
}

/// One batch of one food, with the tick it was acquired. Inventories track
/// batches rather than totals so spoilage can expire the oldest first — a
/// single food integer would make shelf life unrepresentable and collapse the
/// resource portfolio into one fungible number (§7).
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Batch {
    pub kind: ItemKind,
    pub quantity: f32,
    pub harvested_tick: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Inventory {
    pub batches: Vec<Batch>,
}

impl Inventory {
    pub fn total(&self, kind: ItemKind) -> f32 {
        self.batches.iter().filter(|b| b.kind == kind).map(|b| b.quantity).sum()
    }

    pub fn total_food(&self) -> f32 {
        self.batches.iter().filter(|b| b.kind.is_food()).map(|b| b.quantity).sum()
    }

    /// Nutrition currently carried, which is what actually matters to a hungry
    /// creature — 4 grain and 4 forage are not the same meal.
    pub fn food_value(&self) -> f32 {
        self.batches
            .iter()
            .filter(|b| b.kind.is_food())
            .map(|b| b.quantity * b.kind.nutrition())
            .sum()
    }

    pub fn weight(&self) -> f32 {
        self.batches.iter().map(|b| b.quantity).sum()
    }

    pub fn add(&mut self, kind: ItemKind, quantity: f32, tick: i64) {
        if quantity <= 0.0 {
            return;
        }
        // Merge into a batch acquired the same tick so a creature gathering for
        // six consecutive ticks ends with one batch, not six.
        if let Some(b) = self
            .batches
            .iter_mut()
            .find(|b| b.kind == kind && b.harvested_tick == tick)
        {
            b.quantity += quantity;
            return;
        }
        self.batches.push(Batch { kind, quantity, harvested_tick: tick });
    }

    /// Remove up to `want`, oldest batch first, and return how much was taken.
    /// Oldest-first is what makes carrying perishables a real cost: the food
    /// about to rot is the food you eat.
    pub fn take(&mut self, kind: ItemKind, want: f32) -> f32 {
        let mut left = want;
        // Ascending harvest tick, then by position, so the order never depends
        // on how the batches happened to be appended.
        let mut order: Vec<usize> = (0..self.batches.len())
            .filter(|&i| self.batches[i].kind == kind)
            .collect();
        order.sort_by(|&a, &b| {
            self.batches[a]
                .harvested_tick
                .cmp(&self.batches[b].harvested_tick)
                .then(a.cmp(&b))
        });

        for i in order {
            if left <= 0.0 {
                break;
            }
            let take = left.min(self.batches[i].quantity);
            self.batches[i].quantity -= take;
            left -= take;
        }
        self.batches.retain(|b| b.quantity > 1e-4);
        want - left
    }

    /// The oldest food batch and how many ticks until it spoils, for the UI and
    /// for the policy's "eat it before it rots" preference.
    pub fn oldest_food(&self) -> Option<&Batch> {
        self.batches
            .iter()
            .filter(|b| b.kind.is_food())
            .min_by_key(|b| b.harvested_tick)
    }
}

/// What a plan is *for*.
///
/// §5.5 says a need going critical is a hard signal that aborts a committed
/// plan immediately. Taken literally that is self-defeating: a creature dying
/// of thirst, whose plan is a two-day walk to the only water it knows, has that
/// plan cancelled every tick by the very thirst it is on its way to fix — and
/// then re-plans the same walk, aborts it again, and dies where it stood.
///
/// Measured before this existed: 50% of all plans abandoned, creatures
/// re-deciding every 2.5 ticks, and 80% of decisions coming out as EXPLORE
/// because no journey ever survived long enough to arrive.
///
/// So a crisis interrupts a plan unless the plan is already the response to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Addresses {
    Food,
    Water,
    Warmth,
    Rest,
    Fuel,
    Knowledge,
    /// Anything to do with other creatures: courting, feeding, joining,
    /// giving. Never a crisis, so it is always interruptible by one.
    Kinship,
    Nothing,
}

/// A creature's committed plan (§5.5). At M2 every plan comes from the Tier 1
/// utility policy; at M3 the same struct is what the model returns, which is
/// why the horizon and abort fields exist now rather than being retrofitted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Plan {
    pub steps: Vec<crate::sim::actions::Step>,
    pub step_index: usize,
    pub horizon: u32,
    pub ticks_remaining: u32,
    pub set_tick: i64,
    /// Filled in by the deterministic policy; at M3 this is the model's own
    /// stated reasoning, which is what makes the inspector readable.
    pub rationale: String,
    pub tier: u8,
    /// The need this plan exists to satisfy, so a crisis does not cancel the
    /// journey that was on its way to end it.
    pub addresses: Addresses,
}

impl Plan {
    pub fn current(&self) -> Option<&crate::sim::actions::Step> {
        self.steps.get(self.step_index)
    }
    pub fn current_mut(&mut self) -> Option<&mut crate::sim::actions::Step> {
        self.steps.get_mut(self.step_index)
    }
    pub fn is_done(&self) -> bool {
        self.step_index >= self.steps.len()
    }
}

#[derive(Debug, Clone)]
pub struct Creature {
    pub id: i64,
    pub name: String,
    pub sex: Sex,
    pub generation: i32,
    pub mother_id: Option<i64>,
    pub father_id: Option<i64>,
    pub household_id: Option<i64>,

    pub birth_tick: i64,
    pub death_tick: Option<i64>,
    pub death_cause: Option<DeathCause>,

    pub x: u32,
    pub y: u32,
    pub life_stage: LifeStage,

    pub hunger: f32,
    pub thirst: f32,
    pub fatigue: f32,
    pub warmth: f32,
    pub health: f32,

    /// Expected lifespan in ticks, shaved by hard nights and extended by good
    /// ones (§4.6). Stored as a multiple of the baseline in `lifespan_modifier`.
    pub lifespan_ticks: f32,
    /// Extra ageing accumulated through sustained malnutrition — the "up to ~2x
    /// rate" in §4.6, applied continuously rather than at death.
    pub wear: f32,

    pub traits: Traits,
    pub inventory: Inventory,
    pub plan: Option<Plan>,

    pub beliefs: Vec<crate::sim::knowledge::Belief>,

    pub last_deliberation_tick: Option<i64>,
    pub deliberation_pressure: f32,
    pub lifetime_deliberations: i64,
    pub lifetime_think_fatigue: f32,

    /// Shelter the creature is currently inside, if any.
    pub in_shelter: Option<i64>,
    /// Ticks spent this night without shelter or fire, for the lifespan penalty.
    pub exposed_ticks: u32,
    /// True while the creature is at a lit fire, for rendering and warmth.
    pub at_fire: bool,
    /// A recent injury or illness and the tick it happened. If health runs out
    /// soon afterwards, that is what killed the creature rather than whichever
    /// need happened to be lowest — otherwise every accident would be recorded
    /// as starvation and the cause-of-death breakdown would lie.
    pub trauma: Option<(DeathCause, i64)>,
    // ---- society (§4.8–§4.10) --------------------------------------------
    /// The other half of a mutual pairing. Set on both sides or on neither.
    pub mate_id: Option<i64>,
    pub paired_tick: Option<i64>,
    /// Who fathered the child, and the tick it is due.
    pub pregnancy: Option<Pregnancy>,
    pub last_birth_tick: Option<i64>,
    pub children_born: i32,
    /// An infant follows this creature and is fed by it (§4.7). Without a
    /// living guardian an infant cannot feed itself and dies — the dependency
    /// window the PRD calls deliberately harsh.
    pub guardian_id: Option<i64>,
    pub taught_count: i32,
    pub shared_count: i32,

    /// Set when the row differs from what is in SQLite.
    pub dirty: bool,
}

/// A pregnancy in progress (§4.8). Gestation is 48 ticks; childbirth carries a
/// small mortality risk for the mother, which is what makes reproduction a
/// genuine gamble rather than a free action.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Pregnancy {
    pub father_id: i64,
    pub due_tick: i64,
}

impl Creature {
    pub fn age(&self, tick: i64) -> i64 {
        tick - self.birth_tick
    }

    pub fn is_alive(&self) -> bool {
        self.death_tick.is_none()
    }

    /// Biological age: chronological age plus the wear malnutrition added.
    /// Life stage uses chronological age; death by old age uses this.
    pub fn biological_age(&self, tick: i64) -> f32 {
        self.age(tick) as f32 + self.wear
    }

    pub fn lifespan_modifier(&self, cfg: &LifespanConfig) -> f32 {
        self.lifespan_ticks / cfg.baseline_ticks as f32
    }

    /// How much this creature can carry. Elders carry less (§4.7).
    pub fn carry_capacity(&self, cfg: &WorldConfig) -> f32 {
        cfg.actions.carry_capacity * self.life_stage.carry_scale()
    }

    /// Movement speed in tiles of path cost per tick. High fatigue slows
    /// movement and work (§4.5), which is what makes rest worth taking.
    pub fn speed(&self, cfg: &WorldConfig) -> f32 {
        let fatigue_factor = if self.fatigue < cfg.needs.deficit_threshold {
            0.55 + 0.45 * (self.fatigue / cfg.needs.deficit_threshold).clamp(0.0, 1.0)
        } else {
            1.0
        };
        cfg.actions.move_speed * self.life_stage.work_rate().max(0.35) * fatigue_factor
    }

    /// Qualitative description of the worst need, for the ticker and — at M3 —
    /// for the prompt. Health is never shown to the creature as a number (§4.5).
    pub fn felt_state(&self, cfg: &NeedsConfig) -> &'static str {
        let worst = [
            (self.hunger, "hungry"),
            (self.thirst, "thirsty"),
            (self.fatigue, "exhausted"),
            (self.warmth, "freezing"),
        ]
        .into_iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap();

        if worst.0 < cfg.critical_threshold {
            match worst.1 {
                "hungry" => "starving",
                "thirsty" => "parched",
                "exhausted" => "spent",
                _ => "freezing",
            }
        } else if worst.0 < cfg.deficit_threshold {
            worst.1
        } else if self.health < 60.0 {
            "weak"
        } else {
            "well"
        }
    }

    pub fn is_paired(&self) -> bool {
        self.mate_id.is_some()
    }

    /// Eligible to be courted or to court: an unattached adult in reasonable
    /// health. Elders are not excluded from company, only from reproduction.
    pub fn is_courtable(&self, cfg: &LifespanConfig, tick: i64) -> bool {
        let _ = cfg;
        let _ = tick;
        self.life_stage == LifeStage::Adult && self.mate_id.is_none() && self.health > 40.0
    }

    /// The need in the deepest deficit, which is what decides the cause when
    /// health finally runs out.
    pub fn worst_need_cause(&self) -> DeathCause {
        let mut worst = (self.hunger, DeathCause::Starvation);
        if self.thirst < worst.0 {
            worst = (self.thirst, DeathCause::Dehydration);
        }
        if self.warmth < worst.0 {
            worst = (self.warmth, DeathCause::Exposure);
        }
        // Fatigue alone does not kill; exhaustion shows up as one of the others.
        worst.1
    }
}

/// Deterministic name generation. Names are drawn from the sim's single RNG
/// stream, so the same seed produces the same roll of names.
pub fn name_for(rng: &mut impl rand::Rng) -> String {
    const HEAD: [&str; 24] = [
        "An", "Bre", "Cor", "Da", "Es", "Fal", "Gar", "Hal", "Is", "Jor", "Kel", "Lo",
        "Mi", "Na", "Ot", "Pel", "Ru", "Sev", "Tol", "Ur", "Ve", "Wre", "Ys", "Zan",
    ];
    const TAIL: [&str; 16] = [
        "sa", "n", "el", "ra", "ka", "th", "ven", "lin", "mar", "ok", "is", "en",
        "wyn", "da", "ric", "ul",
    ];
    let h = HEAD[rng.gen_range(0..HEAD.len())];
    let t = TAIL[rng.gen_range(0..TAIL.len())];
    format!("{h}{t}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorldConfig;

    fn cfg() -> WorldConfig {
        WorldConfig::default()
    }

    #[test]
    fn life_stage_boundaries_match_the_prd_table() {
        let l = &cfg().lifespan;
        assert_eq!(LifeStage::of(0, l), LifeStage::Infant);
        assert_eq!(LifeStage::of(167, l), LifeStage::Infant);
        assert_eq!(LifeStage::of(168, l), LifeStage::Adult, "adulthood starts at 168");
        assert_eq!(LifeStage::of(587, l), LifeStage::Adult);
        assert_eq!(LifeStage::of(588, l), LifeStage::Elder, "elder from 588");
    }

    #[test]
    fn infants_cannot_work() {
        assert!(!LifeStage::Infant.can_work());
        assert!(LifeStage::Adult.can_work());
        assert!(LifeStage::Elder.can_work());
        assert!(LifeStage::Elder.work_rate() < LifeStage::Adult.work_rate());
    }

    #[test]
    fn inventory_spends_the_oldest_batch_first() {
        let mut inv = Inventory::default();
        inv.add(ItemKind::Forage, 3.0, 100);
        inv.add(ItemKind::Forage, 5.0, 140);

        let got = inv.take(ItemKind::Forage, 4.0);

        assert_eq!(got, 4.0);
        // The tick-100 batch is gone entirely; one unit came out of the newer.
        assert_eq!(inv.batches.len(), 1);
        assert_eq!(inv.batches[0].harvested_tick, 140);
        assert!((inv.batches[0].quantity - 4.0).abs() < 1e-5);
    }

    #[test]
    fn taking_more_than_held_returns_only_what_was_there() {
        let mut inv = Inventory::default();
        inv.add(ItemKind::Grain, 2.0, 1);
        assert_eq!(inv.take(ItemKind::Grain, 9.0), 2.0);
        assert_eq!(inv.total(ItemKind::Grain), 0.0);
        assert!(inv.batches.is_empty(), "emptied batches are dropped");
    }

    #[test]
    fn same_tick_gathers_merge_into_one_batch() {
        let mut inv = Inventory::default();
        inv.add(ItemKind::Forage, 1.0, 50);
        inv.add(ItemKind::Forage, 1.0, 50);
        inv.add(ItemKind::Forage, 1.0, 51);
        assert_eq!(inv.batches.len(), 2, "one batch per acquisition tick");
        assert_eq!(inv.total(ItemKind::Forage), 3.0);
    }

    #[test]
    fn food_value_weighs_grain_above_forage() {
        let mut a = Inventory::default();
        a.add(ItemKind::Forage, 4.0, 0);
        let mut b = Inventory::default();
        b.add(ItemKind::Grain, 4.0, 0);
        assert!(b.food_value() > a.food_value(), "same count, different meal");
        assert_eq!(a.weight(), b.weight());
    }

    #[test]
    fn wood_is_not_food() {
        let mut inv = Inventory::default();
        inv.add(ItemKind::Wood, 6.0, 0);
        assert_eq!(inv.total_food(), 0.0);
        assert_eq!(inv.food_value(), 0.0);
        assert_eq!(inv.weight(), 6.0, "but it still takes up carry weight");
    }

    #[test]
    fn the_deepest_deficit_names_the_cause() {
        let mut c = test_creature();
        c.hunger = 40.0;
        c.thirst = 2.0;
        c.warmth = 30.0;
        assert_eq!(c.worst_need_cause(), DeathCause::Dehydration);
        c.warmth = 0.5;
        assert_eq!(c.worst_need_cause(), DeathCause::Exposure);
    }

    #[test]
    fn felt_state_is_qualitative_and_escalates() {
        let n = cfg().needs;
        let mut c = test_creature();
        assert_eq!(c.felt_state(&n), "well");
        c.hunger = 25.0;
        assert_eq!(c.felt_state(&n), "hungry");
        c.hunger = 4.0;
        assert_eq!(c.felt_state(&n), "starving");
    }

    #[test]
    fn malnutrition_wear_brings_old_age_forward() {
        let mut c = test_creature();
        c.birth_tick = 0;
        assert_eq!(c.biological_age(300), 300.0);
        c.wear = 120.0;
        assert_eq!(c.biological_age(300), 420.0, "wear ages a creature early");
    }

    fn test_creature() -> Creature {
        super::testing::test_creature()
    }
}

/// A default adult, for tests in this crate that need a creature to act on.
#[cfg(test)]
pub mod testing {
    use super::*;

    pub fn test_creature() -> Creature {
        Creature {
            id: 1,
            name: "Test".into(),
            sex: Sex::Female,
            generation: 1,
            mother_id: None,
            father_id: None,
            household_id: None,
            birth_tick: 0,
            death_tick: None,
            death_cause: None,
            x: 10,
            y: 10,
            life_stage: LifeStage::Adult,
            hunger: 100.0,
            thirst: 100.0,
            fatigue: 100.0,
            warmth: 100.0,
            health: 100.0,
            lifespan_ticks: 672.0,
            wear: 0.0,
            traits: Traits::default(),
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
        }
    }
}
