import { invoke } from "@tauri-apps/api/core";
import type * as R from "./types";

/**
 * The reporting commands (PRD §10).
 *
 * Every one of these runs on the reader connection while the simulation writes
 * on its own, so opening a report can never stall a tick — which matters
 * because the reporting view is exactly what you open when something looks
 * wrong and the run is still going.
 */

// population & survival
export const headline = () => invoke<R.Headline>("report_headline");
export const population = (buckets = 240) =>
  invoke<R.PopulationPoint[]>("report_population", { buckets });
export const causes = () => invoke<R.CauseByGeneration[]>("report_causes");
export const ageAtDeath = (bucket = 48) =>
  invoke<R.AgeBucket[]>("report_age_at_death", { bucket });

// lineage
export const lineages = (limit = 40) => invoke<R.LineageRow[]>("report_lineages", { limit });
export const lineageTree = (founder: number) =>
  invoke<R.TreeNode[]>("report_lineage_tree", { founder });
export const generations = () => invoke<R.GenerationRow[]>("report_generations");
export const survival = () => invoke<R.SurvivalPoint[]>("report_survival");

// economy
export const economy = (buckets = 240) =>
  invoke<R.EconomyPoint[]>("report_economy", { buckets });
export const farming = () => invoke<R.FarmingRow[]>("report_farming");
export const wood = (buckets = 240) => invoke<R.WoodSplit[]>("report_wood", { buckets });
export const wealth = () => invoke<R.WealthRow[]>("report_wealth");

// knowledge & culture
export const coverage = () => invoke<R.CoveragePoint[]>("report_coverage");
export const halfLife = () => invoke<R.HalfLifeRow[]>("report_half_life");
export const accuracy = () => invoke<R.AccuracyRow[]>("report_accuracy");
export const teaching = () => invoke<R.TeachingRow[]>("report_teaching");
export const graph = (limit = 300) => invoke<R.TransmissionEdge[]>("report_graph", { limit });
export const beliefs = () => invoke<R.BeliefProvenance[]>("report_beliefs");
export const transmission = () => invoke<R.TransmissionRow[]>("report_transmission");

// behaviour & traits
export const roles = () => invoke<R.RoleRow[]>("report_roles");
export const actionsByGeneration = () =>
  invoke<R.ActionByGeneration[]>("report_action_gen");

// deliberation
export const actions = () => invoke<R.TierAction[]>("report_actions");
export const deliberation = (buckets = 240) =>
  invoke<R.DeliberationPoint[]>("report_deliberation", { buckets });
export const latency = () => invoke<R.LatencyRow[]>("report_latency");
export const stageCompute = () => invoke<R.StageCompute[]>("report_stage_compute");
export const elders = () => invoke<R.ElderRow[]>("report_elders");
export const pressure = () => invoke<R.PressureBand[]>("report_pressure");

// planning
export const horizons = () => invoke<R.HorizonRow[]>("report_horizons");
export const horizonByGeneration = () =>
  invoke<R.HorizonByGeneration[]>("report_horizon_gen");
export const aborts = () => invoke<R.NamedCount[]>("report_aborts");
export const fallbacks = () => invoke<R.NamedCount[]>("report_fallbacks");

// the two S6 / §5.5 correlations
export const s6 = () => invoke<R.DepthBand[]>("report_s6");
export const planners = () => invoke<R.DepthBand[]>("report_planners");

// a life
export const roster = (limit = 400) => invoke<R.Roster[]>("report_roster", { limit });
export const life = (id: number) => invoke<R.Life | null>("report_life", { id });

/** Writes every report beside the database and returns the directory. */
export const exportCsv = () => invoke<string>("export_reports_csv");
