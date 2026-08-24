import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { Palette, WorldMeta } from "../ipc";
import { ChunkCache } from "./chunkCache";
import { resolvePalette } from "./palette";
import { useRenderLoop } from "./useRenderLoop";

const MIN_SCALE = 0.5;   // whole 512 map ~ fits
const MAX_SCALE = 16;    // individual tiles are legible

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
  palette: Palette;
  onStats?: (s: MapStats) => void;
  /** Set to a frame count to run an automated pan sweep and measure it. */
  benchFrames?: number | null;
  onBench?: (r: BenchResult) => void;
}

export function MapView({ meta, terrain, palette, onStats, benchFrames, onBench }: Props) {
  const pal = useMemo(() => resolvePalette(palette), [palette]);
  const cache = useMemo(
    () => new ChunkCache(meta, terrain, pal),
    // A new world or terrain buffer means a wholly new cache.
    [meta, terrain],
  );

  useEffect(() => { cache.setPalette(pal); }, [cache, pal]);

  // Camera in world-tile coordinates: the tile at the viewport's top-left.
  const cam = useRef({ x: meta.width / 2, y: meta.height / 2, scale: 2 });
  const drag = useRef<{ px: number; py: number } | null>(null);
  // Frame the whole world on first paint; the viewport size is not known until
  // then, so this cannot be done when the camera is initialised.
  const fitted = useRef(false);
  const keys = useRef(new Set<string>());
  const [, force] = useState(0);
  const chunksDrawn = useRef(0);

  /**
   * Automated pan sweep. M1's exit criterion is a measured frame rate while
   * panning, so this drives the camera itself and records real rAF-to-rAF
   * intervals rather than trusting a number read off a moving display.
   */
  const bench = useRef<{ left: number; times: number[]; last: number } | null>(null);
  useEffect(() => {
    if (benchFrames) bench.current = { left: benchFrames, times: [], last: 0 };
  }, [benchFrames]);

  const draw = useCallback((ctx: CanvasRenderingContext2D, vw: number, vh: number) => {
    const c = cam.current;
    const cs = meta.chunk_size;

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

    // Snap to the DEVICE pixel grid. The context carries a devicePixelRatio
    // transform, so rounding to whole CSS pixels leaves chunk edges on half
    // device pixels, and the rasteriser antialiases them into visible seams.
    const dpr = window.devicePixelRatio || 1;
    const snap = (v: number) => Math.round(v * dpr) / dpr;

    let drawn = 0;
    const span = cs * c.scale;
    for (let cy = y0; cy <= y1; cy++) {
      for (let cx = x0; cx <= x1; cx++) {
        const img = cache.get(cx, cy);
        const dx = snap(originX + cx * span);
        const dy = snap(originY + cy * span);
        // Derive extent from the next chunk's origin so neighbours abut exactly.
        const dw = snap(originX + (cx + 1) * span) - dx;
        const dh = snap(originY + (cy + 1) * span) - dy;
        ctx.drawImage(img, dx, dy, dw, dh);
        drawn++;
      }
    }
    chunksDrawn.current = drawn;

    // Sheep: they move, so they are marks rather than ground — but pale, small
    // and un-haloed, so they never compete with creatures (§7.2).
    ctx.fillStyle = pal.sheep;
    for (const n of meta.nodes) {
      if (n.kind !== "SHEEP") continue;
      const sx = originX + (n.x + 0.5) * c.scale;
      const sy = originY + (n.y + 0.5) * c.scale;
      if (sx < -8 || sy < -8 || sx > vw + 8 || sy > vh + 8) continue;
      ctx.beginPath();
      ctx.arc(sx, sy, Math.max(1.2, c.scale * 0.5), 0, 6.284);
      ctx.fill();
    }

    // Founders, until M2 turns them into creatures. Amber, the colour the
    // product reserves for the quick, with a halo so they sit above the ground.
    if (meta.founders.length) {
      ctx.strokeStyle = pal.halo;
      ctx.fillStyle = pal.quick;
      ctx.lineWidth = Math.max(1.4, c.scale * 0.7);
      for (const f of meta.founders) {
        const sx = originX + (f.x + 0.5) * c.scale;
        const sy = originY + (f.y + 0.5) * c.scale;
        if (sx < -8 || sy < -8 || sx > vw + 8 || sy > vh + 8) continue;
        ctx.beginPath();
        ctx.arc(sx, sy, Math.max(2, c.scale * 0.92), 0, 6.284);
        ctx.stroke();
        ctx.fill();
      }
    }
  }, [cache, meta, pal]);

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
      if ("wasd".includes(k)) { keys.current.add(k); e.preventDefault(); }
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
    drag.current = { px: e.clientX, py: e.clientY };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onMove = (e: React.PointerEvent<HTMLCanvasElement>) => {
    if (!drag.current) return;
    const c = cam.current;
    c.x -= (e.clientX - drag.current.px) / c.scale;
    c.y -= (e.clientY - drag.current.py) / c.scale;
    drag.current = { px: e.clientX, py: e.clientY };
    const r = e.currentTarget.getBoundingClientRect();
    clampCamera(c, meta, r.width, r.height);
  };
  const onUp = () => { drag.current = null; };

  return (
    <canvas
      ref={canvasRef}
      className="map-canvas"
      onWheel={onWheel}
      onPointerDown={onDown}
      onPointerMove={onMove}
      onPointerUp={onUp}
      onPointerCancel={onUp}
      style={{ cursor: "grab", touchAction: "none" }}
    />
  );
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
