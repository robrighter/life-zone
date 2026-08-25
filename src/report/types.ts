/**
 * Mirrors of the `Serialize` structs in `src-tauri/src/report/`.
 *
 * serde uses the field names verbatim — no rename attributes anywhere in the
 * report modules — so these are snake_case on purpose and match the Rust
 * one-for-one. Keeping the names identical is what lets the CSV export and the
 * table view share a column order without a translation layer to drift.
 */

export interface Headline {
  deepest_generation: number;
  deepest_founder: string | null;
  deepest_descendants: number;
  median_life_ticks: number;
  baseline_ticks: number;
  infant_mortality: number;
  infant_mortality_first_gen: number;
  beliefs_outliving_finders: number;
  total_born: number;
  total_dead: number;
  living: number;
  through_tick: number;
}

export interface PopulationPoint {
  tick: number; population: number; births: number; deaths: number;
}
export interface CauseByGeneration { generation: number; cause: string; deaths: number }
export interface AgeBucket { from_ticks: number; deaths: number }

export interface LineageRow {
  founder_id: number; founder_name: string; depth: number;
  descendants: number; living_descendants: number; founder_alive: boolean;
}
export interface TreeNode {
  id: number; name: string; generation: number; birth_tick: number;
  death_tick: number | null; death_cause: string | null;
  mother_id: number | null; father_id: number | null; children: number;
}
export interface GenerationRow {
  generation: number; born: number; living: number; median_life: number;
  reached_adulthood: number;
  boldness: number; industry: number; sociability: number; caution: number;
}
export interface SurvivalPoint {
  generation: number; share_surviving: number; lineages: number;
}

export interface EconomyPoint {
  tick: number; gathered: number; harvested: number; eaten: number;
  spoiled: number; planted: number; chopped: number;
}
export interface FarmingRow {
  generation: number; creatures: number; planted: number; harvested: number;
  share_who_farmed: number;
}
export interface WoodSplit { tick: number; chopped: number; timber: number; fuel: number }
export interface WealthRow {
  household_id: number; members: number; grain: number; wood: number;
  other: number; grain_per_member: number;
}

export interface CoveragePoint {
  tick: number; known_sites: number; population: number; per_capita: number;
}
export interface HalfLifeRow {
  kind: string; median_ticks: number; p90_ticks: number;
  still_alive: number; extinguished: number;
}
export interface AccuracyRow {
  hops: number; acted_on: number; stale: number; stale_rate: number;
}
export interface TeachingRow {
  household_id: number; members: number; teaching_events: number;
  beliefs_taught: number; per_member: number;
  lineage_depth: number; living_descendants: number;
}
export interface TransmissionEdge {
  from_id: number; from_name: string; to_id: number; to_name: string;
  channel: string; beliefs: number; events: number;
}
export interface TransmissionRow { channel: string; events: number; beliefs: number }
export interface BeliefProvenance {
  hops: number; beliefs: number; mean_confidence: number; from_the_dead: number;
}

export interface RoleRow {
  generation: number; role: string; creatures: number; share: number;
}
export interface ActionByGeneration {
  generation: number; kind: string; count: number; per_creature: number;
}

export interface TierAction { goal: string; tier1: number; tier2: number }
export interface DeliberationPoint {
  tick: number; calls: number; fallbacks: number;
  fallback_rate: number; mean_latency_ms: number;
}
export interface LatencyRow {
  model: string; calls: number; p50_ms: number; p95_ms: number;
  p99_ms: number; max_ms: number;
}
export interface StageCompute {
  life_stage: string; calls: number; share_of_calls: number; creatures: number;
  mean_age_weight: number; calls_per_creature: number;
  think_fatigue: number; crisis_exempt: number;
}
export interface ElderRow {
  life_stage: string; creatures: number; plans: number;
  completion_rate: number; call_share: number;
}
export interface PressureBand {
  band: string; creatures: number; calls_per_100_ticks: number;
}

export interface HorizonRow {
  tier: number; committed: number; actual: number; plans: number;
}
export interface HorizonByGeneration {
  generation: number; mean_committed: number; mean_actual: number; plans: number;
}
export interface NamedCount { name: string; count: number }

export interface DepthBand {
  band: string; creatures: number; mean: number; median: number;
  lineage_depth: number; living_descendants: number;
}

export interface LifeEvent {
  tick: number; kind: string; target_id: number | null;
  x: number | null; y: number | null; payload: string;
}
export interface LifeDecision {
  tick: number; tier: number; goal: string; rationale: string;
  horizon_committed: number | null; horizon_actual: number | null;
  abort_reason: string | null; fallback_used: boolean;
  fallback_reason: string | null; latency_ms: number | null;
  prompt_text: string | null; raw_response: string | null;
}
export interface LifeSample {
  tick: number; hunger: number; thirst: number; fatigue: number;
  warmth: number; health: number;
}
export interface Life {
  id: number; name: string; sex: string; generation: number;
  birth_tick: number; death_tick: number | null; death_cause: string | null;
  mother: [number, string] | null;
  father: [number, string] | null;
  children: [number, string][];
  lifespan_modifier: number;
  events: LifeEvent[]; decisions: LifeDecision[]; samples: LifeSample[];
  beliefs_found: number; still_circulating: number;
  taught_count: number; shared_count: number;
}
export interface Roster {
  id: number; name: string; generation: number; birth_tick: number;
  death_tick: number | null; death_cause: string | null; children: number;
}
