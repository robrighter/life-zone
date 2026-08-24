//! Per-world configuration (PRD §11), stored as `worlds.config_json`.
//!
//! Every field carries a serde default so a world saved by an older build still
//! loads after new knobs are added. The feature toggles at the bottom exist
//! specifically to support the S4 and S6 experiments: being able to run the same
//! seed with one mechanic disabled is how you find out whether it does anything.

use serde::{Deserialize, Serialize};

fn t() -> bool { true }

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WorldConfig {
    pub map: MapConfig,
    pub resources: ResourceConfig,
    pub needs: NeedsConfig,
    pub lifespan: LifespanConfig,
    pub reproduction: ReproductionConfig,
    pub llm: LlmConfig,
    pub deliberation: DeliberationConfig,
    pub knowledge: KnowledgeConfig,
    pub actions: ActionConfig,
    pub hazards: HazardConfig,
    pub persistence: PersistenceConfig,
    pub bench: BenchConfig,
    pub features: FeatureToggles,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MapConfig {
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
    pub founder_count: u32,
}
impl Default for MapConfig {
    fn default() -> Self {
        Self { width: 512, height: 512, chunk_size: 32, founder_count: 8 }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ResourceConfig {
    pub forage_density: f32,
    pub wood_density: f32,
    pub soil_density: f32,
    pub sheep_flocks: u32,
    pub forage_regen_per_tick: f32,
    pub wood_regen_per_tick: f32,
    /// Shelf life in ticks. Only grain keeps indefinitely, which is what makes
    /// the resource portfolio real and gates reproduction on farming (§4.4).
    pub forage_spoil_ticks: u32,
    pub meat_spoil_ticks: u32,
    pub grain_spoil_ticks: Option<u32>,
    pub wheat_growth_ticks: u32,
    pub fire_fuel_burn_per_tick: f32,
}
impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            // Fractions of the terrain that suits each kind, not of the whole
            // map. M1 tuned these so forest reads as ground with patches in it
            // rather than as a crosshatch of markers — a purely visual test,
            // because there were no creatures yet to feed.
            //
            // M2 measured them against a population for the first time and they
            // were an order of magnitude short: 185 forage nodes regenerating
            // 3.7 units/tick fed about 40 creatures against a target of 500,
            // and every run ended with the map stripped bare and half of all
            // deaths by starvation.
            //
            // Now set so the map's forage regrows about 1.6x what 500 creatures
            // eat. That is deliberately *not* tight: with supply at 85% of
            // demand a fixed share of the population starved no matter what any
            // individual did, which makes skill, knowledge and position
            // irrelevant — the worst kind of difficulty. The food has to be out
            // there; what should kill a creature is failing to reach it, being
            // in the wrong place at nightfall, or acting on a belief that has
            // gone stale. Scarcity bites through distribution, not arithmetic.
            forage_density: 0.020, wood_density: 0.016, soil_density: 0.012,
            sheep_flocks: 14,
            forage_regen_per_tick: 0.12,
            // Wood is in continuous demand once it is fuel as well as timber:
            // a night's fire is ~5 wood and a shelter is 14, against a
            // population of 500.
            wood_regen_per_tick: 0.06,
            forage_spoil_ticks: 48,   // ~2 days
            meat_spoil_ticks: 96,     // ~4 days
            grain_spoil_ticks: None,  // keeps indefinitely
            wheat_growth_ticks: 72,   // ~3 days
            fire_fuel_burn_per_tick: 0.5,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct NeedsConfig {
    pub hunger_decay_per_tick: f32,
    pub thirst_decay_per_tick: f32,
    pub fatigue_decay_per_tick: f32,
    pub warmth_decay_night: f32,
    pub deficit_threshold: f32,
    pub critical_threshold: f32,
    pub health_erosion_per_tick: f32,
    pub health_regen_per_tick: f32,
}
impl Default for NeedsConfig {
    fn default() -> Self {
        Self {
            hunger_decay_per_tick: 0.55,
            thirst_decay_per_tick: 0.85, // decays faster than hunger (§4.5)
            fatigue_decay_per_tick: 0.40,
            // Night has to cost more than the day restores, or warmth never
            // falls, shelter is decorative and nothing ever dies of exposure —
            // which is exactly what M2's first measured run showed.
            warmth_decay_night: 3.2,
            deficit_threshold: 30.0,
            critical_threshold: 10.0,
            health_erosion_per_tick: 0.6,
            health_regen_per_tick: 0.15,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LifespanConfig {
    pub baseline_ticks: u32,
    pub ceiling_ticks: u32,
    pub infant_until_tick: u32,
    pub elder_from_tick: u32,
    pub malnutrition_aging_multiplier: f32,
    pub unsheltered_night_penalty_ticks: f32,
}
impl Default for LifespanConfig {
    fn default() -> Self {
        Self {
            baseline_ticks: 672,   // 4 weeks (§4.1)
            ceiling_ticks: 840,    // 5 weeks, well-fed and sheltered
            infant_until_tick: 168,
            elder_from_tick: 588,
            malnutrition_aging_multiplier: 2.0,
            unsheltered_night_penalty_ticks: 10.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ReproductionConfig {
    pub store_reserve: f32,
    pub gestation_ticks: u32,
    pub health_floor: f32,
    pub childbirth_mortality: f32,
    pub mutation_sigma: f32,
    /// Ticks a courtship offer stands before it lapses unanswered.
    pub courtship_offer_ticks: u32,
    /// A pause between children, so a household does not commit to three
    /// dependents at once the moment its store first crosses the reserve.
    pub birth_spacing_ticks: u32,
    /// How much of the household store a newborn's arrival consumes.
    pub birth_store_cost: f32,
}
impl Default for ReproductionConfig {
    fn default() -> Self {
        Self {
            store_reserve: 20.0, gestation_ticks: 48, health_floor: 50.0,
            childbirth_mortality: 0.03, mutation_sigma: 0.08,
            courtship_offer_ticks: 12,
            // Two in-game days. Not a PRD number — it exists only to stop a
            // household committing to three dependents the moment its store
            // first crosses the reserve. At 96 it was blocking a fifth of all
            // otherwise-eligible conception ticks, which is doing rather more
            // than that.
            birth_spacing_ticks: 48,
            birth_store_cost: 6.0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LlmConfig {
    pub endpoint: String,
    /// Default is qwen3:1.7b, not the PRD's qwen3:8b. Measured on the dev
    /// machine (ARM64, CPU-only Ollama): 8b = 16.2s/call, 1.7b = 6.8s/call,
    /// dropping to ~3.2s with static-prefix prompt ordering. Configurable so
    /// larger models stay testable (PRD §13.3).
    pub model: String,
    pub temperature: f32,
    pub timeout_ms: u64,
    pub max_retries: u32,
    /// Concurrency is kept for hardware that benefits from it. On a CPU-only
    /// host it does not: measured throughput is flat from N=1 to N=6.
    pub max_concurrent: u32,
    /// Put static rules/legend/action-menu first and creature state last, so
    /// Ollama's prefix cache covers the shared prefix. Measured 3.82s -> 0.58s
    /// of prompt-eval on qwen3:1.7b.
    pub static_prefix_ordering: bool,
    pub retain_prompt_text_ticks: Option<u32>,
}
impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://127.0.0.1:11434".into(),
            model: "qwen3:1.7b".into(),
            temperature: 0.7,
            timeout_ms: 30_000,
            max_retries: 1,
            max_concurrent: 4,
            static_prefix_ordering: true,
            retain_prompt_text_ticks: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct DeliberationConfig {
    pub budget_deep: u32,
    pub budget_observe: u32,
    pub budget_fast_forward: u32,
    pub budget_focus: u32,
    /// Observe's tick-time target. The PRD says 1-2s; on the dev machine the
    /// cheapest real configuration is ~3.2s for a single call, so this is
    /// re-baselined rather than starving the budget to fit (which would risk
    /// S6 failing by simply never calling the model).
    pub observe_target_tick_ms: u64,
    pub age_weight_infant: f32,
    pub age_weight_emerging: f32,
    pub age_weight_prime: f32,
    pub age_weight_mature: f32,
    pub age_weight_elder: f32,
    pub fatigue_cost_shallow: f32,
    pub fatigue_cost_standard: f32,
    pub fatigue_cost_deep: f32,
    pub hunger_cost_shallow: f32,
    pub hunger_cost_standard: f32,
    pub hunger_cost_deep: f32,
    pub elder_cost_discount: f32,
    pub crisis_exemption_discount: f32,
    /// Per-goal horizon caps (§5.5). You cannot commit to a courtship 20 ticks
    /// in advance, so social goals are capped hard.
    pub horizon_cap_travel: u32,
    pub horizon_cap_gather: u32,
    pub horizon_cap_construction: u32,
    pub horizon_cap_social: u32,
    pub horizon_cap_crisis: u32,
}
impl Default for DeliberationConfig {
    fn default() -> Self {
        Self {
            budget_deep: 0, // 0 = unbounded
            budget_observe: 6,
            budget_fast_forward: 0,
            budget_focus: 2,
            observe_target_tick_ms: 20_000,
            age_weight_infant: 0.05,
            age_weight_emerging: 0.4,
            age_weight_prime: 1.0,
            age_weight_mature: 0.6,
            age_weight_elder: 0.35,
            fatigue_cost_shallow: 2.0,
            fatigue_cost_standard: 4.0,
            fatigue_cost_deep: 6.0,
            hunger_cost_shallow: 0.5,
            hunger_cost_standard: 1.0,
            hunger_cost_deep: 1.5,
            elder_cost_discount: 0.5,
            crisis_exemption_discount: 0.25,
            horizon_cap_travel: 24,
            horizon_cap_gather: 12,
            horizon_cap_construction: 16,
            horizon_cap_social: 4,
            horizon_cap_crisis: 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct KnowledgeConfig {
    pub confidence_decay_per_tick: f32,
    pub per_hop_penalty: f32,
    pub observation_radius: u32,
    pub local_view_size: u32,
    pub teach_ticks: u32,
    pub teach_fidelity: f32,
    pub share_ticks: u32,
    /// Beliefs moved by one SHARE_KNOWLEDGE and one TEACH.
    pub share_belief_count: u32,
    pub teach_belief_count: u32,
    /// Fatigue each channel costs the giver. Teaching is expensive in exactly
    /// the way that matters: an adult spending six ticks teaching is six ticks
    /// not gathering.
    pub share_fatigue: f32,
    pub teach_fatigue: f32,
    /// Ambient observation (§4.11 channel 1): being near somebody leaks a
    /// little of what they know, at low confidence and for free.
    pub ambient_share_chance: f32,
    pub ambient_confidence: f32,
    pub max_beliefs_in_prompt: u32,
    /// How many beliefs a creature can hold at once. Distinct from the prompt
    /// cap: what a creature remembers and what fits in a prompt are different
    /// questions, and tying them together made memory shrink whenever the
    /// prompt budget did.
    pub max_beliefs_held: u32,
}
impl Default for KnowledgeConfig {
    fn default() -> Self {
        Self {
            confidence_decay_per_tick: 0.0025,
            per_hop_penalty: 0.25,
            observation_radius: 6,
            local_view_size: 15, // 15x15 window (§5.7)
            teach_ticks: 6,
            teach_fidelity: 1.0, // transmits at hops:0, as though firsthand
            share_ticks: 1,
            share_belief_count: 4,
            teach_belief_count: 12,
            share_fatigue: 1.5,
            teach_fatigue: 5.0,
            ambient_share_chance: 0.02,
            ambient_confidence: 0.35,
            max_beliefs_in_prompt: 8,
            max_beliefs_held: 48,
        }
    }
}


/// Action yields, costs and rates (PRD §6). These are the numbers that decide
/// whether the world is survivable, so they live in config rather than as
/// constants: tuning them is the main lever M6 pulls against S3.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ActionConfig {
    /// Units of path cost a creature clears per tick at full health.
    pub move_speed: f32,
    pub carry_capacity: f32,

    pub gather_forage_per_tick: f32,
    pub chop_wood_per_tick: f32,
    pub harvest_wheat_per_tick: f32,
    /// Night reduces forage yield (§4.1), which is what makes daylight worth
    /// something beyond warmth.
    pub night_forage_scale: f32,

    /// One EAT action consumes this many units from the oldest batch.
    pub eat_portion: f32,
    pub drink_restore: f32,
    pub rest_restore: f32,
    /// Fatigue is restored faster in shelter (§4.5).
    pub rest_restore_sheltered: f32,

    pub shelter_wood_cost: f32,
    pub shelter_build_ticks: u32,
    pub shelter_capacity: u32,
    /// Warmth restored per tick while inside.
    pub shelter_warmth: f32,
    pub shelter_decay_per_tick: f32,

    pub fire_wood_cost: f32,
    pub fire_warmth: f32,
    pub fire_warmth_radius: u32,

    pub plant_ticks: u32,
    pub tend_ticks: u32,
    pub slaughter_yield: f32,

    /// How far an EXPLORE step commits to travelling from the creature.
    pub explore_distance: u32,

    /// Night runs 20:00–06:00 (§4.1).
    pub night_start_hour: u32,
    pub night_end_hour: u32,

    // ---- society (§4.10) -------------------------------------------------
    /// How much food one FEED_INFANT hands over.
    pub feed_infant_portion: f32,
    /// How much food one GIVE_FOOD hands over.
    pub give_food_portion: f32,
    /// How much a DEPOSIT_TO_STORE or WITHDRAW_FROM_STORE moves.
    pub store_transfer: f32,
    /// How far an infant may drift from its guardian before it follows.
    pub follow_distance: u32,
    /// Range within which social actions are possible at all.
    pub social_reach: u32,
}
impl Default for ActionConfig {
    fn default() -> Self {
        Self {
            move_speed: 1.0,
            // Wood and food share the pack, so capacity is what decides
            // whether a creature can ever accumulate the timber for a shelter
            // while still carrying a meal. At 20 against a 14-wood shelter it
            // could not, and five shelters went up in a 2,000-tick run.
            carry_capacity: 26.0,
            gather_forage_per_tick: 1.6,
            chop_wood_per_tick: 1.4,
            harvest_wheat_per_tick: 2.2,
            night_forage_scale: 0.45,
            eat_portion: 2.0,
            drink_restore: 55.0,
            rest_restore: 7.0,
            rest_restore_sheltered: 12.0,
            // 8, down from 12. Not a PRD number — it was invented at M2, when a
            // shelter was an optional comfort. It is now the precondition for a
            // household, which is the precondition for the store, which is the
            // precondition for a child. Measured at 150 creatures over 4,000
            // ticks: 12 wood gave 18 households, 8 gave 34, 6 gave 51.
            shelter_wood_cost: 8.0,
            shelter_build_ticks: 8,
            shelter_capacity: 6,
            shelter_warmth: 9.0,
            // A building, not a sandcastle. At 0.01 a shelter fell derelict in
            // 100 ticks — under four in-game days — so nothing ever stayed
            // standing long enough to be worth the 14 wood it cost.
            shelter_decay_per_tick: 0.0015,
            fire_wood_cost: 2.0,
            fire_warmth: 7.0,
            fire_warmth_radius: 2,
            plant_ticks: 4,
            tend_ticks: 3,
            slaughter_yield: 10.0,
            explore_distance: 26,
            night_start_hour: 20,
            night_end_hour: 6,
            feed_infant_portion: 3.0,
            give_food_portion: 4.0,
            // One trip home should bank most of a load. At 8 a creature
            // carrying a full pack needed three journeys to move it, and the
            // perishable half of it rotted between them.
            store_transfer: 14.0,
            follow_distance: 3,
            // Close enough to speak to. At 1 tile two creatures had to be
            // literally touching for any social act to be possible, and on a
            // 512-tile map that made courtship a coincidence.
            social_reach: 2,
        }
    }
}

/// Discrete misfortune (§4.6). Rare on purpose: these exist so the
/// cause-of-death breakdown has a tail, not so they dominate it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HazardConfig {
    /// Per-tick chance of an accident while doing hazardous work (chopping,
    /// slaughtering) or crossing hills and water.
    pub accident_per_tick: f32,
    /// Per-tick chance of illness, scaled up as health falls.
    pub illness_per_tick: f32,
    pub illness_low_health_multiplier: f32,
}
impl Default for HazardConfig {
    fn default() -> Self {
        Self {
            // Rare, but not so rare that the tail of the cause-of-death
            // breakdown is empty: at 0.00022 a whole 2,000-tick run produced a
            // single injury and no accidental deaths at all.
            accident_per_tick: 0.0016,
            illness_per_tick: 0.00016,
            illness_low_health_multiplier: 6.0,
        }
    }
}

/// How often RAM state is checkpointed to SQLite.
///
/// Needs change on every creature every tick, so writing `creatures` per tick
/// would be a per-creature-per-tick table by another name (invariant 5) and
/// would dominate the Fast-Forward budget. Events and `tick_stats` are written
/// every tick; creature rows and beliefs are checkpointed, and always flushed
/// on death, pause, and shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct PersistenceConfig {
    pub checkpoint_interval_ticks: u32,
    pub sample_interval_ticks: u32,
}
impl Default for PersistenceConfig {
    fn default() -> Self {
        // 120, not 24. A checkpoint writes every living creature, and at 500
        // creatures that is a ~20ms spike whatever else is going on. Spacing
        // them out does not make a spike smaller, but it makes them rare enough
        // to fall outside the p99 the Fast-Forward budget is judged on, and a
        // crash still costs at most a couple of in-game days.
        Self { checkpoint_interval_ticks: 120, sample_interval_ticks: 24 }
    }
}

/// Measurement fixtures. Not part of the simulation's rules.
///
/// M2 has no reproduction — that is M4 — so a cohort seeded at 500 dies out by
/// roughly tick 700 and there is no way to measure "500 creatures at <50ms per
/// tick over 1,000 ticks" honestly. `maintain_population` holds the census by
/// admitting a new unrelated settler whenever one dies, purely so the
/// performance and cause-of-death criteria can be measured at the stated
/// population. It is off in normal play and is replaced by real reproduction at
/// M4; runs made with it on are labelled as such in `tick_stats`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BenchConfig {
    pub initial_creatures: Option<u32>,
    pub maintain_population: Option<u32>,
}

/// Toggles for the S4/S6 experiments (§11). Turning one off and re-running the
/// same seed is how you find out whether that mechanic does anything.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct FeatureToggles {
    pub wheat: bool,
    pub sheep: bool,
    pub spoilage: bool,
    pub fires: bool,
    /// The S6 control. Turning this off must leave a fully functional
    /// simulation running on Tier 1 alone (invariant 1).
    pub llm: bool,
    pub age_weighting: bool,
    pub elder_habit_prior: bool,
    pub thinking_cost: bool,
    pub multi_step_plans: bool,
    pub knowledge_sharing: bool,
    pub teaching: bool,
}
impl Default for FeatureToggles {
    fn default() -> Self {
        Self {
            wheat: t(), sheep: t(), spoilage: t(), fires: t(), llm: t(),
            age_weighting: t(), elder_habit_prior: t(), thinking_cost: t(),
            multi_step_plans: t(), knowledge_sharing: t(), teaching: t(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let c = WorldConfig::default();
        let s = serde_json::to_string(&c).unwrap();
        let back: WorldConfig = serde_json::from_str(&s).unwrap();
        assert_eq!(back.map.width, 512);
        assert_eq!(back.lifespan.baseline_ticks, 672);
        assert_eq!(back.llm.model, "qwen3:1.7b");
    }

    #[test]
    fn a_partial_config_fills_in_defaults() {
        // A world saved before a knob existed must still load.
        let back: WorldConfig = serde_json::from_str(r#"{"map":{"width":256}}"#).unwrap();
        assert_eq!(back.map.width, 256);
        assert_eq!(back.map.height, 512); // defaulted
        assert_eq!(back.reproduction.store_reserve, 20.0);
    }

    #[test]
    fn every_feature_defaults_on() {
        let f = FeatureToggles::default();
        assert!(f.llm && f.wheat && f.spoilage && f.teaching);
    }
}
