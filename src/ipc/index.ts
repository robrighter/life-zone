import { invoke } from "@tauri-apps/api/core";
import type { WorldSummary } from "./types";

export const getWorld = () => invoke<WorldSummary>("get_world");
export const listWorlds = () => invoke<WorldSummary[]>("list_worlds");
export type * from "./types";
