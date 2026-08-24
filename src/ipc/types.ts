// Mirrors the Rust types in src-tauri/src/lib.rs, config.rs and sim/runner.rs.
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
  lifespan: {
    baseline_ticks: number; ceiling_ticks: number;
    infant_until_tick: number; elder_from_tick: number;
  };
  llm: { model: string; endpoint: string; max_concurrent: number };
  deliberation: { budget_observe: number; observe_target_tick_ms: number };
  bench: { initial_creatures: number | null; maintain_population: number | null };
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

export type NodeKind = "FORAGE" | "WOOD" | "WHEAT" | "SHEEP";

export interface ResourceNode {
  kind: NodeKind;
  x: number;
  y: number;
  quantity: number;
  max_quantity: number;
  regen_rate: number;
}

export interface Founder { x: number; y: number; female: boolean }

export interface WorldMeta {
  width: number;
  height: number;
  chunk_size: number;
  seed: number;
  founders: Founder[];
}

export interface NodesSnapshot { version: number; nodes: ResourceNode[] }

export type SpeedMode = "DEEP" | "OBSERVE" | "FAST_FORWARD" | "FOCUS";

/** Bit flags on CreatureDot.flags — must match sim/runner.rs. */
export const FLAG_HUNGRY = 1;
export const FLAG_THIRSTY = 2;
export const FLAG_COLD = 4;
export const FLAG_SHELTERED = 8;
export const FLAG_AT_FIRE = 16;
/** A deliberation is in flight for this creature (§9.1's heatmap). */
export const FLAG_THINKING = 32;
/** Currently running a plan the model wrote, rather than Tier 1's. */
export const FLAG_MODEL_PLAN = 64;

export interface CreatureDot {
  id: number;
  x: number;
  y: number;
  /** 0 infant, 1 adult, 2 elder. */
  stage: number;
  flags: number;
}

export interface StructureDot {
  id: number; x: number; y: number;
  kind: "SHELTER" | "FIRE" | "PEN";
  lit: boolean;
  condition: number;
}

export interface TickerLine {
  tick: number;
  kind: string;
  text: string;
  tone: string;
}

export interface BeliefView {
  kind: string;
  x: number;
  y: number;
  estimate: string;
  confidence: number;
  hops: number;
  provenance: string;
}

export interface PlanStepView {
  goal: string;
  label: string;
  done: boolean;
  current: boolean;
  est_ticks: number;
}

export interface Traits {
  boldness: number; industry: number; sociability: number; caution: number;
}

export interface CreatureDetail {
  id: number;
  name: string;
  sex: string;
  generation: number;
  age: number;
  expected_lifespan: number;
  life_stage: string;
  x: number;
  y: number;
  felt_state: string;
  hunger: number; thirst: number; fatigue: number; warmth: number; health: number;
  traits: Traits;
  /** [kind, quantity, ticks until it spoils or null if it keeps] */
  carrying: [string, number, number | null][];
  plan_rationale: string;
  plan_addresses: string;
  plan_horizon: number;
  plan_remaining: number;
  plan_tier: number;
  steps: PlanStepView[];
  beliefs: BeliefView[];
  belief_count: number;
  lifetime_deliberations: number;
  sheltered: boolean;

  household_id: number | null;
  household_store: number;
  household_grain: number;
  household_members: number;
  /** [id, name] */
  mate: [number, string] | null;
  mother: [number, string] | null;
  father: [number, string] | null;
  children_born: number;
  taught_count: number;
  shared_count: number;
  expecting_in: number | null;
  /** Which of §4.8's requirements is missing, in plain language. */
  cannot_yet: string | null;
  inherited_beliefs: number;
  from_the_dead: number;
}

export interface PhaseTimings {
  world: number; needs: number; plans: number; deliberate: number;
  act: number; resolve: number; persist: number;
}

export interface Snapshot {
  tick: number;
  day: number;
  hour: number;
  night: boolean;
  running: boolean;
  mode: SpeedMode;

  population: number;
  born: number;
  died: number;
  infants: number;
  adults: number;
  elders: number;
  structures_standing: number;
  shelters: number;
  fires_lit: number;

  deaths_by_cause: [string, number][];
  tick_ms: number;
  timings: PhaseTimings;
  ticks_per_second: number;
  population_maintained: boolean;

  households: number;
  households_at_reserve: number;
  mean_store: number;
  paired: number;
  expecting: number;
  deepest_generation: number;
  beliefs_taught: number;
  beliefs_shared: number;

  llm_enabled: boolean;
  llm_model: string;
  llm_dispatched: number;
  llm_accepted: number;
  llm_in_flight: number;
  /** Invariant 8: a rising fallback rate is the LLM ceasing to matter. */
  fallback_rate: number;
  mean_latency_ms: number;
  cache_hit_rate: number;
  on_model_plans: number;

  creatures: CreatureDot[];
  structures: StructureDot[];
  events: TickerLine[];
  selected: CreatureDetail | null;
  nodes_version: number;

  /**
   * The community's collective map, one byte per cell: the strongest
   * confidence anyone holds about anything in that cell. Beliefs live in RAM
   * on the sim thread and there are tens of thousands of them, so this coarse
   * reduction is what makes the knowledge overlay affordable to push.
   */
  known: number[];
  known_dim: number;
  known_cell: number;
}

export type UiCommand =
  | { kind: "play" }
  | { kind: "pause" }
  | { kind: "step"; ticks: number }
  | { kind: "set_mode"; mode: SpeedMode }
  | { kind: "select"; id: number | null }
  | { kind: "regenerate"; seed: number; creatures: number };
