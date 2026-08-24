import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  FLAG_AT_FIRE, FLAG_COLD, FLAG_HUNGRY, FLAG_SHELTERED, FLAG_THIRSTY,
  type Palette, type ResourceNode, type Snapshot, type WorldMeta,
} from "../ipc";
import { ChunkCache } from "./chunkCache";
import { resolvePalette } from "./palette";
import { useRenderLoop } from "./useRenderLoop";

const MIN_SCALE = 0.5;   // whole 512 map ~ fits
const MAX_SCALE = 16;    // individual tiles are legible

export interface Overlays {
  nodes: boolean;
  creatures: boolean;
  structures: boolean;
  plans: boolean;
  /** Render the map as *known* rather than as it is (§9.1). */
  knowledge: boolean;
}

export interface BenchResult {
  frames: number;
  seconds: number;
  meanFps: number;
  p50Fps: number;
  p95FrameMs: number;
  worstFrameMs: number;
  chunksDrawn: number;
  scale: number;
}

export interface MapStats {
  fps: number;
  frameMs: number;
  scale: number;
  chunksDrawn: number;
  chunksCached: number;
}

interface Props {
  meta: WorldMeta;
  terrain: Uint8Array;
  nodes: ResourceNode[];
  palette: Palette;
  snapshot: Snapshot | null;
  overlays: Overlays;
  selectedId: number | null;
  onSelect?: (id: number | null) => void;
  onStats?: (s: MapStats) => void;
  benchFrames?: number | null;
  onBench?: (r: BenchResult) => void;
}

export function MapView({
  meta, terrain, nodes, palette, snapshot, overlays, selectedId,
  onSelect, onStats, benchFrames, onBench,
}: Props) {
  const pal = useMemo(() => resolvePalette(palette), [palette]);
  const cache = useMemo(
    // A new world or terrain buffer means a wholly new cache.
    () => new ChunkCache(meta, terrain, pal, nodes),
    [meta, terrain],
  );

  useEffect(() => { cache.setPalette(pal); }, [cache, pal]);
  useEffect(() => { cache.setNodes(nodes); }, [cache, nodes]);

  // Camera in world-tile coordinates: the tile at the viewport's centre.
  const cam = useRef({ x: meta.width / 2, y: meta.height / 2, scale: 2 });
  const drag = useRef<{ px: number; py: number; moved: boolean } | null>(null);
  // Frame the whole world on first paint; the viewport size is not known until
  // then, so this cannot be done when the camera is initialised.
  const fitted = useRef(false);
  const keys = useRef(new Set<string>());
  const [, force] = useState(0);
  const chunksDrawn = useRef(0);

  // The latest snapshot, held in a ref so a 15Hz push stream does not re-run
  // the draw callback's dependencies 15 times a second.
  const snap = useRef<Snapshot | null>(snapshot);
  snap.current = snapshot;
  const ov = useRef(overlays);
  ov.current = overlays;
  const sel = useRef(selectedId);
  sel.current = selectedId;

  /**
   * Automated pan sweep. The frame-rate criterion is a measured number, so this
   * drives the camera itself and records real rAF-to-rAF intervals rather than
   * trusting a figure read off a moving display.
   */
  const bench = useRef<{ left: number; times: number[]; last: number } | null>(null);
  useEffect(() => {
    if (benchFrames) bench.current = { left: benchFrames, times: [], last: 0 };
  }, [benchFrames]);

  const draw = useCallback((ctx: CanvasRenderingContext2D, vw: number, vh: number) => {
    const c = cam.current;
    const cs = meta.chunk_size;
    const s = snap.current;
    const o = ov.current;

    if (!fitted.current && vw > 0 && vh > 0) {
      c.scale = clamp(Math.min(vw / meta.width, vh / meta.height), MIN_SCALE, MAX_SCALE);
      c.x = meta.width / 2;
      c.y = meta.height / 2;
      fitted.current = true;
    }

    if (bench.current) {
      const b = bench.current;
      const now = performance.now();
      if (b.last) b.times.push(now - b.last);
      b.last = now;
      // Sweep diagonally so new chunks enter the viewport every frame; a
      // stationary camera would measure a cache that never misses.
      c.x += 2.5 / c.scale * 4;
      c.y += 1.5 / c.scale * 4;
      if (c.x > meta.width) c.x = 0;
      if (c.y > meta.height) c.y = 0;
      if (--b.left <= 0) {
        const t = b.times.slice().sort((p, q) => p - q);
        const total = b.times.reduce((a, v) => a + v, 0);
        onBench?.({
          frames: b.times.length,
          seconds: total / 1000,
          meanFps: (b.times.length * 1000) / total,
          p50Fps: 1000 / t[Math.floor(t.length * 0.5)],
          p95FrameMs: t[Math.floor(t.length * 0.95)],
          worstFrameMs: t[t.length - 1],
          chunksDrawn: chunksDrawn.current,
          scale: c.scale,
        });
        bench.current = null;
      }
    }

    // WASD panning, framerate-independent enough at these speeds.
    if (keys.current.size) {
      const step = 8 / c.scale;
      if (keys.current.has("w")) c.y -= step;
      if (keys.current.has("s")) c.y += step;
      if (keys.current.has("a")) c.x -= step;
      if (keys.current.has("d")) c.x += step;
      clampCamera(c, meta, vw, vh);
    }

    ctx.fillStyle = `rgb(${pal.void.join(",")})`;
    ctx.fillRect(0, 0, vw, vh);
    ctx.imageSmoothingEnabled = false;

    // Viewport culling: cost scales with what is on screen, not world size.
    const tilesW = vw / c.scale;
    const tilesH = vh / c.scale;
    const x0 = Math.max(0, Math.floor((c.x - tilesW / 2) / cs));
    const y0 = Math.max(0, Math.floor((c.y - tilesH / 2) / cs));
    const x1 = Math.min(cache.chunksX - 1, Math.ceil((c.x + tilesW / 2) / cs));
    const y1 = Math.min(cache.chunksY - 1, Math.ceil((c.y + tilesH / 2) / cs));

    const originX = vw / 2 - c.x * c.scale;
    const originY = vh / 2 - c.y * c.scale;
    const sx = (wx: number) => originX + wx * c.scale;
    const sy = (wy: number) => originY + wy * c.scale;

    // Snap to the DEVICE pixel grid. The context carries a devicePixelRatio
    // transform, so rounding to whole CSS pixels leaves chunk edges on half
    // device pixels, and the rasteriser antialiases them into visible seams.
    const dpr = window.devicePixelRatio || 1;
    const snapPx = (v: number) => Math.round(v * dpr) / dpr;

    let drawn = 0;
    if (o.nodes || !o.knowledge) {
      const span = cs * c.scale;
      for (let cy = y0; cy <= y1; cy++) {
        for (let cx = x0; cx <= x1; cx++) {
          const img = cache.get(cx, cy);
          const dx = snapPx(originX + cx * span);
          const dy = snapPx(originY + cy * span);
          // Derive extent from the next chunk's origin so neighbours abut exactly.
          const dw = snapPx(originX + (cx + 1) * span) - dx;
          const dh = snapPx(originY + (cy + 1) * span) - dy;
          ctx.drawImage(img, dx, dy, dw, dh);
          drawn++;
        }
      }
    }
    chunksDrawn.current = drawn;

    // ---- the knowledge overlay (§9.1) ------------------------------------
    // The map as the community *believes* it, not as it is. Ground nobody
    // knows about is painted out entirely; what is known is lit in proportion
    // to how confident the best-informed creature is about it, so stale
    // knowledge reads as dim and unvisited ground reads as dark.
    if (o.knowledge && s && s.known.length > 0) {
      const dim = s.known_dim;
      const cell = s.known_cell;

      // Black the world out, then let what is known glow back through it.
      // Painting flat swatches over a dimmed map does not work: the muted
      // sequential colours land at almost exactly the value of the dimmed
      // terrain underneath and the overlay reads as a uniform grey wash.
      // Compositing additively makes brightness *mean* confidence, which is
      // what the design asks for — dim ground is remembered but stale.
      ctx.fillStyle = `rgba(${pal.void.join(",")},0.94)`;
      ctx.fillRect(0, 0, vw, vh);

      const ramp = confidenceRamp();
      const span = cell * c.scale;
      ctx.globalCompositeOperation = "lighter";
      for (let gy = 0; gy < dim; gy++) {
        const py = sy(gy * cell);
        if (py > vh || py + span < 0) continue;
        for (let gx = 0; gx < dim; gx++) {
          const v = s.known[gy * dim + gx];
          if (v === 0) continue;
          const px = sx(gx * cell);
          if (px > vw || px + span < 0) continue;
          const t = v / 255;
          const [r, g, b] = ramp[t > 0.8 ? 4 : t > 0.6 ? 3 : t > 0.4 ? 2 : t > 0.2 ? 1 : 0];
          // Scaled by confidence as well as bucketed by it, so the ramp reads
          // as a gradient rather than five bands.
          const k = 0.25 + t * 0.75;
          ctx.fillStyle = `rgb(${(r * k) | 0},${(g * k) | 0},${(b * k) | 0})`;
          ctx.fillRect(px, py, span + 1, span + 1);
        }
      }
      ctx.globalCompositeOperation = "source-over";
    }

    // ---- sheep -----------------------------------------------------------
    // They move, so they are marks rather than ground — but pale, small and
    // un-haloed, so they never compete with creatures (§7.2).
    if (o.nodes && !o.knowledge) {
      ctx.fillStyle = pal.sheep;
      for (const n of nodes) {
        if (n.kind !== "SHEEP" || n.quantity <= 0) continue;
        const px = sx(n.x + 0.5);
        const py = sy(n.y + 0.5);
        if (px < -8 || py < -8 || px > vw + 8 || py > vh + 8) continue;
        ctx.beginPath();
        ctx.arc(px, py, Math.max(1.2, c.scale * 0.5), 0, 6.284);
        ctx.fill();
      }
    }

    // ---- structures ------------------------------------------------------
    if (o.structures && s) {
      for (const st of s.structures) {
        const px = sx(st.x + 0.5);
        const py = sy(st.y + 0.5);
        if (px < -10 || py < -10 || px > vw + 10 || py > vh + 10) continue;
        const r = Math.max(2, c.scale * 0.8);
        if (st.kind === "FIRE") {
          if (!st.lit) continue;
          // A lit fire is the one warm thing on a cold map at night.
          const g = ctx.createRadialGradient(px, py, 0, px, py, r * 4);
          g.addColorStop(0, "rgba(240,169,60,.85)");
          g.addColorStop(1, "rgba(240,169,60,0)");
          ctx.fillStyle = g;
          ctx.beginPath();
          ctx.arc(px, py, r * 4, 0, 6.284);
          ctx.fill();
        } else {
          // Shelters are built things: square, so they never read as creatures.
          ctx.fillStyle = pal.halo;
          ctx.fillRect(px - r, py - r, r * 2, r * 2);
          ctx.fillStyle = `rgba(157,175,173,${0.35 + st.condition * 0.5})`;
          ctx.fillRect(px - r * 0.7, py - r * 0.7, r * 1.4, r * 1.4);
        }
      }
    }

    // ---- committed plan paths -------------------------------------------
    // Where the selected creature has bound itself to go, and how far it has
    // left to walk.
    if (o.plans && s?.selected) {
      const d = s.selected;
      ctx.strokeStyle = "rgba(240,169,60,.5)";
      ctx.lineWidth = Math.max(1, c.scale * 0.2);
      ctx.setLineDash([4, 3]);
      let from: [number, number] = [d.x, d.y];
      for (const step of d.steps) {
        const m = /(\d+),(\d+)$/.exec(step.label);
        if (!m) continue;
        const to: [number, number] = [Number(m[1]), Number(m[2])];
        ctx.beginPath();
        ctx.moveTo(sx(from[0] + 0.5), sy(from[1] + 0.5));
        ctx.lineTo(sx(to[0] + 0.5), sy(to[1] + 0.5));
        ctx.stroke();
        from = to;
      }
      ctx.setLineDash([]);
    }

    // ---- creatures -------------------------------------------------------
    // Crisp marks in a single colour with a halo, so they always sit on top of
    // the ground and never compete with the resource layer (§7.2).
    if (o.creatures && s) {
      const r = Math.max(1.6, c.scale * 0.85);
      ctx.lineWidth = Math.max(1.1, c.scale * 0.5);
      for (const cr of s.creatures) {
        const px = sx(cr.x + 0.5);
        const py = sy(cr.y + 0.5);
        if (px < -8 || py < -8 || px > vw + 8 || py > vh + 8) continue;

        // Life stage is carried by size, not by hue: colour is reserved.
        const size = cr.stage === 0 ? r * 0.6 : cr.stage === 2 ? r * 0.85 : r;
        ctx.strokeStyle = pal.halo;
        ctx.fillStyle = cr.flags & FLAG_SHELTERED
          ? "rgba(240,169,60,.45)"
          : pal.quick;

        ctx.beginPath();
        if (cr.stage === 2) {
          // Elders are drawn as a ring: present, and visibly not the same.
          ctx.arc(px, py, size, 0, 6.284);
          ctx.stroke();
          ctx.stroke();
        } else {
          ctx.arc(px, py, size, 0, 6.284);
          ctx.stroke();
          ctx.fill();
        }

        // A creature in trouble gets a small tell, not a second colour.
        if (cr.flags & (FLAG_HUNGRY | FLAG_THIRSTY | FLAG_COLD)) {
          ctx.strokeStyle = "rgba(198,93,79,.9)";
          ctx.lineWidth = Math.max(1, c.scale * 0.25);
          ctx.beginPath();
          ctx.arc(px, py, size * 1.9, 0, 6.284);
          ctx.stroke();
          ctx.lineWidth = Math.max(1.1, c.scale * 0.5);
        }
        if (cr.flags & FLAG_AT_FIRE) {
          ctx.fillStyle = "rgba(240,169,60,.35)";
          ctx.beginPath();
          ctx.arc(px, py, size * 2.4, 0, 6.284);
          ctx.fill();
        }
      }

      // The selection ring last, so nothing draws over it.
      if (sel.current != null) {
        const me = s.creatures.find((k) => k.id === sel.current);
        if (me) {
          const px = sx(me.x + 0.5);
          const py = sy(me.y + 0.5);
          ctx.strokeStyle = pal.quick;
          ctx.lineWidth = 1.5;
          ctx.beginPath();
          ctx.arc(px, py, Math.max(7, r * 3), 0, 6.284);
          ctx.stroke();
        }
      }
    }
  }, [cache, meta, nodes, pal, onBench]);

  const { canvasRef, fps, frameMs } = useRenderLoop(draw);

  useEffect(() => {
    onStats?.({
      fps, frameMs,
      scale: cam.current.scale,
      chunksDrawn: chunksDrawn.current,
      chunksCached: cache.size,
    });
  }, [fps, frameMs, onStats, cache]);

  // --- input ---------------------------------------------------------------
  useEffect(() => {
    const down = (e: KeyboardEvent) => {
      const k = e.key.toLowerCase();
      const typing = (e.target as HTMLElement)?.tagName === "INPUT";
      if (!typing && "wasd".includes(k)) { keys.current.add(k); e.preventDefault(); }
    };
    const up = (e: KeyboardEvent) => keys.current.delete(e.key.toLowerCase());
    window.addEventListener("keydown", down);
    window.addEventListener("keyup", up);
    return () => {
      window.removeEventListener("keydown", down);
      window.removeEventListener("keyup", up);
    };
  }, []);

  const onWheel = useCallback((e: React.WheelEvent<HTMLCanvasElement>) => {
    const c = cam.current;
    const rect = e.currentTarget.getBoundingClientRect();
    const mx = e.clientX - rect.left;
    const my = e.clientY - rect.top;

    // Zoom about the cursor, so the tile under the pointer stays put.
    const before = screenToWorld(c, mx, my, rect.width, rect.height);
    const next = clamp(c.scale * (e.deltaY < 0 ? 1.15 : 1 / 1.15), MIN_SCALE, MAX_SCALE);
    c.scale = next;
    const after = screenToWorld(c, mx, my, rect.width, rect.height);
    c.x += before.x - after.x;
    c.y += before.y - after.y;
    clampCamera(c, meta, rect.width, rect.height);
    force((n) => n + 1);
  }, [meta]);

  const onDown = (e: React.PointerEvent<HTMLCanvasElement>) => {
    drag.current = { px: e.clientX, py: e.clientY, moved: false };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drag.current) return;
    const c = cam.current;
    const dx = e.clientX - drag.current.px;
    const dy = e.clientY - drag.current.py;
    if (Math.abs(dx) + Math.abs(dy) > 2) drag.current.moved = true;
    c.x -= dx / c.scale;
    c.y -= dy / c.scale;
    drag.current = { px: e.clientX, py: e.clientY, moved: drag.current.moved };
    const r = e.currentTarget.getBoundingClientRect();
    clampCamera(c, meta, r.width, r.height);
  };
  const onUp = (e: React.PointerEvent<HTMLCanvasElement>) => {
    const d = drag.current;
    drag.current = null;
    // A click, not the end of a drag: pick whatever is under the pointer.
    if (!d || d.moved || !onSelect) return;
    const rect = e.currentTarget.getBoundingClientRect();
    const w = screenToWorld(
      cam.current, e.clientX - rect.left, e.clientY - rect.top, rect.width, rect.height,
    );
    const s = snap.current;
    if (!s) return;
    // Generous in world units at low zoom, tight when zoomed in.
    const reach = Math.max(1.5, 8 / cam.current.scale);
    let best: { id: number; d: number } | null = null;
    for (const cr of s.creatures) {
      const dd = Math.hypot(cr.x + 0.5 - w.x, cr.y + 0.5 - w.y);
      if (dd <= reach && (!best || dd < best.d)) best = { id: cr.id, d: dd };
    }
    onSelect(best ? best.id : null);
  };

  return (
    <canvas
      ref={canvasRef}
      className="map-canvas"
      onWheel={onWheel}
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={() => { drag.current = null; }}
      style={{ cursor: "grab", touchAction: "none" }}
    />
  );
}

/**
 * The sequential ramp from the palette tokens: hearsay at the dim end,
 * firsthand at the bright one. Read from CSS so the map and the legend beside
 * it cannot drift apart, and resolved once per frame rather than once per cell
 * — `getComputedStyle` in a 4,096-iteration loop is not free.
 */
function confidenceRamp(): [number, number, number][] {
  const css = getComputedStyle(document.documentElement);
  const fallback = ["#2b3a3f", "#3c5359", "#4f6f72", "#6d8f86", "#9dbfa8"];
  return [1, 2, 3, 4, 5].map((i, k) => {
    const hex = (css.getPropertyValue(`--seq-${i}`).trim() || fallback[k]).replace("#", "");
    const full = hex.length === 3 ? hex.split("").map((ch) => ch + ch).join("") : hex;
    return [
      parseInt(full.slice(0, 2), 16) || 0,
      parseInt(full.slice(2, 4), 16) || 0,
      parseInt(full.slice(4, 6), 16) || 0,
    ] as [number, number, number];
  });
}

function clamp(v: number, lo: number, hi: number) {
  return Math.min(hi, Math.max(lo, v));
}

function screenToWorld(
  c: { x: number; y: number; scale: number },
  sx: number, sy: number, vw: number, vh: number,
) {
  return { x: c.x + (sx - vw / 2) / c.scale, y: c.y + (sy - vh / 2) / c.scale };
}

/** Keep at least a little of the map on screen at all times. */
function clampCamera(
  c: { x: number; y: number; scale: number },
  meta: WorldMeta, vw: number, vh: number,
) {
  const halfW = vw / c.scale / 2;
  const halfH = vh / c.scale / 2;
  c.x = clamp(c.x, -halfW / 2, meta.width + halfW / 2);
  c.y = clamp(c.y, -halfH / 2, meta.height + halfH / 2);
}
