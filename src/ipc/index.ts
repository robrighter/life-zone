import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { NodesSnapshot, Snapshot, UiCommand, WorldMeta, WorldSummary } from "./types";

export const getWorld = () => invoke<WorldSummary>("get_world");
export const listWorlds = () => invoke<WorldSummary[]>("list_worlds");
export const getWorldMeta = () => invoke<WorldMeta>("get_world_meta");
export const getNodes = () => invoke<NodesSnapshot>("get_nodes");
export const getSnapshot = () => invoke<Snapshot>("get_snapshot");
export const getDeathsByCause = () => invoke<[string, number][]>("get_deaths_by_cause");
export const benchMode = () => invoke<boolean>("bench_mode");
export const reportBench = (result: unknown) => invoke("report_bench", { result });

/**
 * The only way the UI changes anything. Commands go down a channel to the
 * simulation thread, which owns all world state; nothing here can block a tick.
 */
export const simControl = (command: UiCommand) => invoke("sim_control", { command });

/**
 * Snapshots are pushed, not polled. The sim thread throttles emission by wall
 * clock, so Fast-Forward running hundreds of ticks a second still delivers a
 * readable stream rather than drowning the webview.
 */
export const onTick = (fn: (s: Snapshot) => void) =>
  listen<Snapshot>("tick:complete", (e) => fn(e.payload));

/**
 * Terrain arrives as raw bytes, one per tile, row-major. Tauri hands an
 * ArrayBuffer straight through for commands that return `ipc::Response`, so the
 * 256KB grid never becomes JSON.
 */
export async function getTerrain(): Promise<Uint8Array> {
  const buf = await invoke<ArrayBuffer>("get_terrain");
  return new Uint8Array(buf);
}

export type * from "./types";
// The flag constants are values, not types, so they need a value re-export.
export { FLAG_HUNGRY, FLAG_THIRSTY, FLAG_COLD, FLAG_SHELTERED, FLAG_AT_FIRE } from "./types";
