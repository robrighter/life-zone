import { useEffect, useState } from "react";
import type { Palette } from "../ipc";

/** Matches design/mockups/theme.js: `unlit` is the :root default and carries no
 *  attribute, so setting one would override tokens that are already correct. */
export const PALETTES: { id: Palette; name: string; sw: string }[] = [
  { id: "unlit", name: "Unlit", sw: "#F0A93C" },
  { id: "survey", name: "Survey", sw: "#005E3E" },
  { id: "strata", name: "Strata", sw: "#E8823A" },
];

export function usePalette() {
  const [palette, setPalette] = useState<Palette>(() => {
    const stored = localStorage.getItem("lz-palette") as Palette | null;
    return stored && PALETTES.some((p) => p.id === stored) ? stored : "unlit";
  });

  useEffect(() => {
    if (palette === "unlit") document.documentElement.removeAttribute("data-palette");
    else document.documentElement.setAttribute("data-palette", palette);
    localStorage.setItem("lz-palette", palette);
    // Terrain lives in canvas, not CSS, so the map has to be told separately.
    document.dispatchEvent(new CustomEvent("palettechange", { detail: palette }));
  }, [palette]);

  return { palette, setPalette };
}
