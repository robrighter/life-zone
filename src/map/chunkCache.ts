import type { CanvasPalette, RGB } from "./palette";
import type { ResourceNode, WorldMeta } from "../ipc";

/**
 * Per-chunk offscreen render cache (PRD §9.1).
 *
 * Each chunk is rasterised once at one pixel per tile and then blitted scaled,
 * so panning and zooming cost only a drawImage per visible chunk. Only chunks
 * whose tiles changed need re-rasterising, which is what makes resource
 * depletion cheap to reflect at M2.
 */
export class ChunkCache {
  private canvases = new Map<number, HTMLCanvasElement>();
  private dirty = new Set<number>();
  readonly chunksX: number;
  readonly chunksY: number;

  private nodes: ResourceNode[];

  constructor(
    private meta: WorldMeta,
    private terrain: Uint8Array,
    private palette: CanvasPalette,
    nodes: ResourceNode[] = [],
  ) {
    this.chunksX = Math.ceil(meta.width / meta.chunk_size);
    this.chunksY = Math.ceil(meta.height / meta.chunk_size);
    this.nodes = nodes;
    this.tint = this.buildResourceTint(nodes);
  }

  /**
   * Resource nodes are part of the ground, blended in before anything else —
   * a patch of wheat should look like a patch of ground that grows wheat, not a
   * marker sitting on top of it. Depleted nodes fade toward bare earth.
   */
  private tint: Float32Array;

  private buildResourceTint(nodes: ResourceNode[]): Float32Array {
    const { width: w, height: h } = this.meta;
    const tint = new Float32Array(w * h * 4);

    const put = (x: number, y: number, col: RGB, s: number) => {
      if (x < 0 || y < 0 || x >= w || y >= h) return;
      const i = (y * w + x) * 4;
      if (s <= tint[i + 3]) return; // strongest patch wins
      tint[i] = col[0]; tint[i + 1] = col[1]; tint[i + 2] = col[2]; tint[i + 3] = s;
    };

    // A smooth radial falloff, not a stepped manhattan diamond: at map zoom the
    // latter renders as a literal "+" glyph, which makes a resource read as a
    // marker stamped on the ground rather than as a patch of ground. Resources
    // are places (§7.2), so the edge has to dissolve.
    const R = 4;
    for (const n of nodes) {
      const col = (this.palette.res as Record<string, RGB>)[n.kind];
      if (!col) continue; // sheep move, so they are not ground
      const q = n.max_quantity > 0 ? n.quantity / n.max_quantity : 0;
      const core = 0.30 + q * 0.34; // depleted patches fade toward bare earth
      for (let dy = -R; dy <= R; dy++) {
        for (let dx = -R; dx <= R; dx++) {
          const d = Math.hypot(dx, dy) / R;
          if (d >= 1) continue;
          // smoothstep shoulder, so there is no visible rim
          const f = 1 - d * d * (3 - 2 * d) * 0.55 - d * 0.45;
          put(n.x + dx, n.y + dy, col, core * Math.max(0, f));
        }
      }
    }
    return tint;
  }

  setPalette(p: CanvasPalette) {
    this.palette = p;
    this.tint = this.buildResourceTint(this.nodes);
    this.invalidateAll();
  }

  setNodes(nodes: ResourceNode[]) {
    this.nodes = nodes;
    this.tint = this.buildResourceTint(nodes);
    this.invalidateAll();
  }

  invalidateAll() {
    this.dirty = new Set(this.canvases.keys());
  }

  /** Mark the chunk containing a tile for re-rasterisation. */
  invalidateTile(x: number, y: number) {
    const cs = this.meta.chunk_size;
    this.dirty.add(((y / cs) | 0) * this.chunksX + ((x / cs) | 0));
  }

  private key(cx: number, cy: number) {
    return cy * this.chunksX + cx;
  }

  /** The chunk's raster, building or refreshing it only when needed. */
  get(cx: number, cy: number): HTMLCanvasElement {
    const k = this.key(cx, cy);
    let c = this.canvases.get(k);
    if (c && !this.dirty.has(k)) return c;

    const cs = this.meta.chunk_size;
    if (!c) {
      c = document.createElement("canvas");
      c.width = cs;
      c.height = cs;
      this.canvases.set(k, c);
    }
    this.rasterise(c, cx, cy);
    this.dirty.delete(k);
    return c;
  }

  private rasterise(canvas: HTMLCanvasElement, cx: number, cy: number) {
    const ctx = canvas.getContext("2d")!;
    const cs = this.meta.chunk_size;
    const { width: w, height: h } = this.meta;
    const img = ctx.createImageData(cs, cs);
    const term = this.palette.terrain;

    for (let ty = 0; ty < cs; ty++) {
      for (let tx = 0; tx < cs; tx++) {
        const x = cx * cs + tx;
        const y = cy * cs + ty;
        const o = (ty * cs + tx) * 4;

        if (x >= w || y >= h) {
          img.data[o + 3] = 0; // off-map padding stays transparent
          continue;
        }

        const wi = y * w + x;
        const rgb = term[this.terrain[wi]] ?? term[0];
        let [r, g, b] = rgb;

        const a = this.tint[wi * 4 + 3];
        if (a > 0) {
          r = r + (this.tint[wi * 4] - r) * a;
          g = g + (this.tint[wi * 4 + 1] - g) * a;
          b = b + (this.tint[wi * 4 + 2] - b) * a;
        }

        img.data[o] = r; img.data[o + 1] = g; img.data[o + 2] = b; img.data[o + 3] = 255;
      }
    }
    ctx.putImageData(img, 0, 0);
  }

  /** Chunks currently held, for the diagnostics readout. */
  get size() {
    return this.canvases.size;
  }
}
