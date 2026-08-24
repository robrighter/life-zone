import { invoke } from "@tauri-apps/api/core";
import type { WorldMeta, WorldSummary } from "./types";

export const getWorld = () => invoke<WorldSummary>("get_world");
export const listWorlds = () => invoke<WorldSummary[]>("list_worlds");
export const getWorldMeta = () => invoke<WorldMeta>("get_world_meta");
export const benchMode = () => invoke<boolean>("bench_mode");
export const reportBench = (result: unknown) => invoke("report_bench", { result });
export const regenerateWorld = (seed: number) =>
  invoke<WorldMeta>("regenerate_world", { seed });

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
