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
            forage_density: 0.010, wood_density: 0.014, soil_density: 0.006,
            sheep_flocks: 12,
            forage_regen_per_tick: 0.02, wood_regen_per_tick: 0.008,
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
            warmth_decay_night: 1.6,
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
}
impl Default for ReproductionConfig {
    fn default() -> Self {
        Self {
            store_reserve: 20.0, gestation_ticks: 48, health_floor: 50.0,
            childbirth_mortality: 0.03, mutation_sigma: 0.08,
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
    pub max_beliefs_in_prompt: u32,
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
            max_beliefs_in_prompt: 8,
        }
    }
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
