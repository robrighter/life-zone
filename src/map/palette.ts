import type { Palette } from "../ipc";

/**
 * Terrain sits low in value on purpose — the map is a substrate the data reads
 * against, not the subject. Creatures and nodes must always win the contrast.
 *
 * These have no CSS tokens (the mockups keep them in JS), so they live here.
 * Resource hues are the opposite case: they ARE tokens, and are read from CSS
 * below so the legend and the canvas can never drift apart (BUILD.md §7.2).
 */
export const TERRAIN_SETS: Record<Palette, string[]> = {
  // indexed by the Rust Terrain discriminant: deep, shallow, sand, grass, forest, soil, hill
  unlit:  ["#0C1A26", "#123043", "#312D21", "#232F1D", "#182618", "#2C2317", "#232829"],
  survey: ["#A9BECB", "#C2D4DC", "#E4DECA", "#D5DBC8", "#BCCBB4", "#DCD2BE", "#D2D4D2"],
  strata: ["#16202A", "#1E3340", "#3A3122", "#2B291A", "#202417", "#33251A", "#2A2622"],
};

export const TERRAIN_NAMES = [
  "Deep water", "Shallow water", "Sand", "Grass", "Forest", "Soil", "Hills",
];

/** Marks that are not ground. Alpha is baked in, so these stay JS-side. */
export const MARK_SETS: Record<Palette, { sheep: string; halo: string }> = {
  unlit:  { sheep: "rgba(178,190,188,.72)", halo: "rgba(6,10,12,.95)" },
  survey: { sheep: "rgba(90,104,108,.62)",  halo: "rgba(237,239,236,.95)" },
  strata: { sheep: "rgba(185,175,160,.7)",  halo: "rgba(8,6,4,.95)" },
};

export type RGB = [number, number, number];

function readVar(name: string, fallback: string): string {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

export function hexToRgb(hex: string): RGB {
  const h = hex.replace("#", "").trim();
  const full = h.length === 3 ? h.split("").map((c) => c + c).join("") : h;
  return [
    parseInt(full.slice(0, 2), 16),
    parseInt(full.slice(2, 4), 16),
    parseInt(full.slice(4, 6), 16),
  ];
}

/**
 * Snapshot of everything the canvas needs, resolved at palette-change time.
 * Resource and void colours come from CSS so there is exactly one source of
 * truth shared with the legend; terrain and marks come from the tables above.
 */
export interface CanvasPalette {
  terrain: RGB[];
  res: { WHEAT: RGB; WOOD: RGB; FORAGE: RGB };
  void: RGB;
  quick: string;
  sheep: string;
  halo: string;
}

export function resolvePalette(p: Palette): CanvasPalette {
  return {
    terrain: TERRAIN_SETS[p].map(hexToRgb),
    res: {
      WHEAT: hexToRgb(readVar("--res-wheat", "#c38414")),
      WOOD: hexToRgb(readVar("--res-wood", "#77954d")),
      FORAGE: hexToRgb(readVar("--res-forage", "#b34941")),
    },
    void: hexToRgb(readVar("--void", "#0A1012")),
    quick: readVar("--quick", "#F0A93C"),
    sheep: MARK_SETS[p].sheep,
    halo: MARK_SETS[p].halo,
  };
}
