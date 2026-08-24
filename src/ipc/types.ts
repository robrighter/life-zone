// Mirrors the Rust types in src-tauri/src/lib.rs and config.rs.
// Kept hand-written and narrow: only what the UI actually reads.

export type Palette = "unlit" | "survey" | "strata";

export interface FeatureToggles {
  wheat: boolean; sheep: boolean; spoilage: boolean; fires: boolean;
  llm: boolean; age_weighting: boolean; elder_habit_prior: boolean;
  thinking_cost: boolean; multi_step_plans: boolean;
  knowledge_sharing: boolean; teaching: boolean;
}

export interface WorldConfig {
  map: { width: number; height: number; chunk_size: number; founder_count: number };
  lifespan: { baseline_ticks: number; infant_until_tick: number; elder_from_tick: number };
  llm: { model: string; endpoint: string; max_concurrent: number };
  deliberation: { budget_observe: number; observe_target_tick_ms: number };
  features: FeatureToggles;
}

export interface WorldSummary {
  id: number;
  name: string;
  seed: number;
  current_tick: number;
  status: string;
  created_at: string;
  config: WorldConfig;
}
