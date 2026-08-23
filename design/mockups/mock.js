/* Life Zone — mockup data + rendering helpers.
   Everything here is fabricated sample state, shaped to match the PRD schema
   so the mockups argue about real layouts rather than lorem ipsum. */

/* ------------------------------------------------------------------ rng */

function rng(seed) {
  let s = seed >>> 0;
  return function () {
    s ^= s << 13; s >>>= 0;
    s ^= s >> 17;
    s ^= s << 5;  s >>>= 0;
    return s / 4294967296;
  };
}

/* value noise over a grid, bilinear-interpolated */
function noiseField(w, h, cells, seed) {
  const r = rng(seed);
  const gw = cells + 2, gh = cells + 2;
  const g = new Float32Array(gw * gh);
  for (let i = 0; i < g.length; i++) g[i] = r();
  const out = new Float32Array(w * h);
  const sx = cells / w, sy = cells / h;
  const sm = t => t * t * (3 - 2 * t);
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const fx = x * sx, fy = y * sy;
      const x0 = fx | 0, y0 = fy | 0;
      const tx = sm(fx - x0), ty = sm(fy - y0);
      const a = g[y0 * gw + x0], b = g[y0 * gw + x0 + 1];
      const c = g[(y0 + 1) * gw + x0], d = g[(y0 + 1) * gw + x0 + 1];
      out[y * w + x] = (a + (b - a) * tx) * (1 - ty) + (c + (d - c) * tx) * ty;
    }
  }
  return out;
}

function octaves(w, h, seed) {
  const a = noiseField(w, h, 4, seed);
  const b = noiseField(w, h, 9, seed + 7717);
  const c = noiseField(w, h, 20, seed + 3391);
  const out = new Float32Array(w * h);
  for (let i = 0; i < out.length; i++) out[i] = a[i] * 0.58 + b[i] * 0.29 + c[i] * 0.13;
  return out;
}

/* ------------------------------------------------------- world generation */

/* Terrain sits low in value on purpose — the map is a substrate the data reads
   against, not the subject. Creatures and nodes must always win the contrast.
   Canvas can't read CSS custom properties, so each palette carries its own set. */
const TERRAIN_SETS = {
  unlit: {
    deep: '#0C1A26', shallow: '#123043', sand: '#312D21',
    grass: '#232F1D', forest: '#182618', soil: '#2C2317', hill: '#232829'
  },
  survey: {
    deep: '#A9BECB', shallow: '#C2D4DC', sand: '#E4DECA',
    grass: '#D5DBC8', forest: '#BCCBB4', soil: '#DCD2BE', hill: '#D2D4D2'
  },
  strata: {
    deep: '#16202A', shallow: '#1E3340', sand: '#3A3122',
    grass: '#2B291A', forest: '#202417', soil: '#33251A', hill: '#2A2622'
  }
};

const TERRAIN = {
  deep:    { c: '', name: 'Deep water',    pass: false },
  shallow: { c: '', name: 'Shallow water', pass: true  },
  sand:    { c: '', name: 'Sand',          pass: true  },
  grass:   { c: '', name: 'Grass',         pass: true  },
  forest:  { c: '', name: 'Forest',        pass: true  },
  soil:    { c: '', name: 'Soil',          pass: true  },
  hill:    { c: '', name: 'Hills',         pass: true  }
};

/* Everything the canvas needs that isn't terrain: the void it fades into, the
   colour of the living, and how resource patches should be tinted. */
let PAL = 'unlit';
const PAL_CANVAS = {
  unlit:  { void: [10, 16, 18],    fade: [10, 16, 18],    quick: '#F0A93C',
            sheep: 'rgba(178,190,188,.72)', halo: 'rgba(6,10,12,.95)',
            res: { wheat: [0xc3,0x84,0x14], wood: [0x77,0x95,0x4d], forage: [0xb3,0x49,0x41] } },
  survey: { void: [237, 239, 236], fade: [199, 206, 205], quick: '#005E3E',
            sheep: 'rgba(90,104,108,.62)',  halo: 'rgba(237,239,236,.95)',
            res: { wheat: [0x9d,0x70,0x00], wood: [0x71,0x8b,0x42], forage: [0xaf,0x3e,0x30] } },
  strata: { void: [18, 14, 11],    fade: [18, 14, 11],    quick: '#E8823A',
            sheep: 'rgba(185,175,160,.7)',  halo: 'rgba(8,6,4,.95)',
            res: { wheat: [0xbf,0x89,0x0d], wood: [0xa1,0x93,0x41], forage: [0xb1,0x45,0x33] } }
};

function setTerrainPalette(id) {
  PAL = TERRAIN_SETS[id] ? id : 'unlit';
  const set = TERRAIN_SETS[PAL];
  for (const k in TERRAIN) TERRAIN[k].c = set[k];
  if (typeof RES_TINT !== 'undefined') rebuildResourceTint();
  if (typeof window !== 'undefined' && window.__lzRepaint) window.__lzRepaint();
}

const WORLD_W = 256, WORLD_H = 176;   // viewport window onto the 512x512 world

function buildWorld(seed) {
  const elev = octaves(WORLD_W, WORLD_H, seed);
  const moist = octaves(WORLD_W, WORLD_H, seed + 90210);
  const tiles = new Array(WORLD_W * WORLD_H);
  for (let i = 0; i < tiles.length; i++) {
    const e = elev[i], m = moist[i];
    let t;
    if (e < 0.34) t = 'deep';
    else if (e < 0.41) t = 'shallow';
    else if (e < 0.435) t = 'sand';
    else if (e > 0.755) t = 'hill';        // hills stay scarce, per §4.3
    else if (m > 0.60) t = 'forest';
    else if (m > 0.47 && e < 0.53) t = 'soil';
    else t = 'grass';
    tiles[i] = t;
  }
  return { tiles, elev, moist, w: WORLD_W, h: WORLD_H, seed };
}

const WORLD = buildWorld(44127);

/* resource nodes scattered on appropriate terrain */
function buildNodes(world) {
  const r = rng(world.seed + 555);
  const nodes = [];
  for (let i = 0; i < 260; i++) {
    const x = (r() * world.w) | 0, y = (r() * world.h) | 0;
    const t = world.tiles[y * world.w + x];
    let kind = null;
    if (t === 'forest') kind = r() > 0.45 ? 'wood' : 'forage';
    else if (t === 'grass' && r() > 0.72) kind = 'sheep';
    else if (t === 'soil' && r() > 0.55) kind = 'wheat';
    if (kind) nodes.push({ x, y, kind, q: 0.25 + r() * 0.75 });
  }
  return nodes;
}
const NODES = buildNodes(WORLD);

/* ------------------------------------------------------------- creatures */

const NAMES = ['Mira','Sev','Tolen','Ansa','Rook','Vell','Iska','Bren','Ottel','Nara',
  'Halu','Esk','Pell','Wren','Odd','Tash','Lume','Corr','Sira','Tave','Ferrin','Ase',
  'Nix','Dree','Mott','Selv','Yarrow','Kest','Onn','Vesk','Delle','Grath'];

function buildCreatures(world) {
  const r = rng(88121);
  const out = [];
  for (let i = 0; i < 118; i++) {
    let x, y, t, guard = 0;
    do {
      x = (r() * world.w) | 0; y = (r() * world.h) | 0;
      t = world.tiles[y * world.w + x];
    } while (!TERRAIN[t].pass && guard++ < 40);
    const age = (r() * 700) | 0;
    out.push({
      id: 100 + i,
      name: NAMES[(r() * NAMES.length) | 0],
      x, y, age,
      stage: age < 168 ? 'infant' : age < 588 ? 'adult' : 'elder',
      household: (r() * 9) | 0,
      gen: 1 + ((r() * 5) | 0)
    });
  }
  return out;
}
const CREATURES = buildCreatures(WORLD);

/* Household colouring borrows the palette's categorical set — cycled here only
   because nine households exceed six slots, which is exactly the case §10 says
   should fold into "Other" on a chart. On a map, repeated hues are tolerable
   because position disambiguates; on a chart they would not be. */
const HOUSEHOLD_SETS = {
  unlit:  ['#c38414','#875aab','#41a984','#b34941','#5b7ec8','#77954d','#c38414','#41a984','#875aab'],
  survey: ['#af3e30','#2459a5','#9d7000','#008668','#85356e','#718b42','#af3e30','#9d7000','#2459a5'],
  strata: ['#bf890d','#4370b2','#339d89','#b14533','#a19341','#9d538a','#bf890d','#339d89','#4370b2']
};
Object.defineProperty(globalThis, 'HOUSEHOLD_HUE', {
  get() { return HOUSEHOLD_SETS[PAL] || HOUSEHOLD_SETS.unlit; }
});

/* Resource kinds own the categorical palette. Creatures do NOT — by default every
   living thing is amber, the colour the whole product reserves for the quick.
   Two entity classes must never share a palette: if a mark's colour could mean
   either "this is a wheat field" or "this is one of Ottel's children", the map
   has stopped being readable. Household colouring is an opt-in overlay, and when
   it is on the resource layer dims to get out of its way.

   Sheep are deliberately absent here: they wander (§4.4), so they cannot be
   ground. They are drawn as small pale marks instead — moving, but subordinate
   to the creatures whose story this is. */
/* Resource nodes are painted into the terrain itself — they are ground, not
   markers. A patch of wheat should look like a patch of ground that grows wheat. */
function buildResourceTint(world, nodes) {
  const cols = PAL_CANVAS[PAL].res;
  const tint = new Float32Array(world.w * world.h * 4);
  const put = (x, y, col, s) => {
    if (x < 0 || y < 0 || x >= world.w || y >= world.h) return;
    const i = (y * world.w + x) * 4;
    if (s <= tint[i + 3]) return;
    tint[i] = col[0]; tint[i + 1] = col[1]; tint[i + 2] = col[2]; tint[i + 3] = s;
  };
  nodes.forEach(n => {
    const col = cols[n.kind];
    if (!col) return;                             // sheep are not ground
    const core = 0.34 + n.q * 0.30;               // depleted nodes fade into the ground
    for (let dy = -2; dy <= 2; dy++)
      for (let dx = -2; dx <= 2; dx++) {
        const d = Math.abs(dx) + Math.abs(dy);
        if (d > 2) continue;
        put(n.x + dx, n.y + dy, col, core * (d === 0 ? 1 : d === 1 ? 0.55 : 0.26));
      }
  });
  return tint;
}
let RES_TINT = buildResourceTint(WORLD, NODES);
function rebuildResourceTint() { RES_TINT = buildResourceTint(WORLD, NODES); }
setTerrainPalette((typeof currentPalette === 'function' && currentPalette()) || 'unlit');

/* ------------------------------------------------------------ map render */

function drawMap(canvas, opts) {
  opts = opts || {};
  const world = WORLD;
  const ctx = canvas.getContext('2d');
  const dpr = window.devicePixelRatio || 1;
  const rect = canvas.getBoundingClientRect();
  canvas.width = Math.max(1, Math.round(rect.width * dpr));
  canvas.height = Math.max(1, Math.round(rect.height * dpr));

  const off = document.createElement('canvas');
  off.width = world.w; off.height = world.h;
  const octx = off.getContext('2d');
  const img = octx.createImageData(world.w, world.h);

  // knowledge field: 0 = unknown, 1 = firsthand & fresh
  const know = opts.knowledge || null;

  const resDim = opts.nodes === false ? 0 : (opts.dimNodes ? 0.42 : 1);

  for (let i = 0; i < world.tiles.length; i++) {
    const hex = TERRAIN[world.tiles[i]].c;
    let r = parseInt(hex.slice(1, 3), 16),
        g = parseInt(hex.slice(3, 5), 16),
        b = parseInt(hex.slice(5, 7), 16);

    // resource nodes are part of the ground, blended in before anything else
    const a = RES_TINT[i * 4 + 3] * resDim;
    if (a > 0) {
      r = r + (RES_TINT[i * 4] - r) * a;
      g = g + (RES_TINT[i * 4 + 1] - g) * a;
      b = b + (RES_TINT[i * 4 + 2] - b) * a;
    }

    if (know) {
      const k = know[i];
      // Never-seen ground falls to the void. Known-but-stale ground fades toward
      // a separate target instead, so the two are never the same colour. On dark
      // palettes those coincide; on paper they must not, or "faintly remembered"
      // becomes indistinguishable from "unprinted" and the gradient loses its range.
      const p = PAL_CANVAS[PAL];
      const [vr, vg, vb] = k > 0 ? p.fade : p.void;
      const grey = (r * 0.35 + g * 0.5 + b * 0.15);
      const sat = 0.42 + 0.58 * k;                 // confidence -> saturation
      r = r * sat + grey * (1 - sat);
      g = g * sat + grey * (1 - sat);
      b = b * sat + grey * (1 - sat);
      const lum = k <= 0 ? 0 : 0.44 + 0.56 * k;    // confidence -> luminance
      r = vr + (r - vr) * lum;
      g = vg + (g - vg) * lum;
      b = vb + (b - vb) * lum;
    }
    img.data[i * 4] = r; img.data[i * 4 + 1] = g; img.data[i * 4 + 2] = b; img.data[i * 4 + 3] = 255;
  }
  octx.putImageData(img, 0, 0);

  ctx.imageSmoothingEnabled = false;
  const scale = Math.max(canvas.width / world.w, canvas.height / world.h);
  const dw = world.w * scale, dh = world.h * scale;
  const ox = (canvas.width - dw) / 2, oy = (canvas.height - dh) / 2;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.drawImage(off, ox, oy, dw, dh);

  const px = (x, y) => [ox + (x + 0.5) * scale, oy + (y + 0.5) * scale];

  /* Sheep: they move, so they are marks rather than ground — but pale, small
     and un-haloed, so they never compete with the creatures. */
  if (opts.nodes !== false) {
    ctx.fillStyle = PAL_CANVAS[PAL].sheep;
    NODES.forEach(n => {
      if (n.kind !== 'sheep') return;
      const k = know ? know[n.y * world.w + n.x] : 1;
      if (k < 0.25) return;
      const [cx, cy] = px(n.x, n.y);
      ctx.globalAlpha = 0.4 + 0.6 * k;
      ctx.beginPath();
      ctx.arc(cx, cy, Math.max(1.2, scale * 0.5), 0, 6.284);
      ctx.fill();
    });
    ctx.globalAlpha = 1;
  }

  /* Creatures. One colour — amber, the living — unless household colouring is
     explicitly switched on. Shape carries life stage. Every mark gets a dark
     halo so it separates from whatever ground it is standing on. */
  if (opts.creatures !== false) {
    CREATURES.forEach(c => {
      const k = know ? know[c.y * world.w + c.x] : 1;
      if (know && k < 0.3) return;
      const [cx, cy] = px(c.x, c.y);
      const rad = c.stage === 'infant' ? scale * 0.62 : scale * 0.92;

      ctx.fillStyle = opts.byHousehold ? HOUSEHOLD_HUE[c.household] : PAL_CANVAS[PAL].quick;
      ctx.strokeStyle = PAL_CANVAS[PAL].halo;
      ctx.lineWidth = Math.max(1.4, scale * 0.7);
      ctx.beginPath();
      if (c.stage === 'elder') {
        // matched to the adult circle's area so elders don't read as heavier
        const s = rad * 0.886;
        ctx.rect(cx - s, cy - s, s * 2, s * 2);
      } else {
        ctx.arc(cx, cy, rad, 0, 6.284);
      }
      ctx.stroke(); ctx.fill();
    });
  }

  // selected creature ring + committed plan path
  if (opts.selected) {
    const s = opts.selected;
    const [cx, cy] = px(s.x, s.y);
    ctx.strokeStyle = PAL_CANVAS[PAL].quick;
    ctx.lineWidth = 2;
    ctx.beginPath(); ctx.arc(cx, cy, scale * 3.4, 0, 6.284); ctx.stroke();
    if (opts.path) {
      ctx.setLineDash([4, 4]);
      ctx.lineWidth = 1.5;
      ctx.globalAlpha = .85;
      ctx.beginPath();
      opts.path.forEach((p, i) => { const [a, b] = px(p[0], p[1]); i ? ctx.lineTo(a, b) : ctx.moveTo(a, b); });
      ctx.stroke();
      ctx.setLineDash([]);
      opts.path.slice(1).forEach(p => {
        const [a, b] = px(p[0], p[1]);
        ctx.fillStyle = PAL_CANVAS[PAL].quick;
        ctx.beginPath(); ctx.arc(a, b, 3, 0, 6.284); ctx.fill();
      });
      ctx.globalAlpha = 1;
    }
  }
}

/* knowledge fields: radial competence around visited sites, decayed by age */

/* Knowledge is patchy, not gaussian. Each visited site holds a plateau of solid
   knowledge that falls off quickly at the edge, and the edge is roughened by
   noise — because it was made by someone walking, not by a light source. */
function knowledgeField(sites) {
  const f = new Float32Array(WORLD_W * WORLD_H);
  const edge = noiseField(WORLD_W, WORLD_H, 26, 5150);
  sites.forEach(s => {
    const R = s.r;
    for (let y = Math.max(0, s.y - R); y < Math.min(WORLD_H, s.y + R); y++) {
      for (let x = Math.max(0, s.x - R); x < Math.min(WORLD_W, s.x + R); x++) {
        const i = y * WORLD_W + x;
        const d = Math.sqrt((x - s.x) ** 2 + (y - s.y) ** 2) / R;
        const wobble = 0.76 + edge[i] * 0.42;      // ragged boundary
        if (d > wobble) continue;
        // plateau out to 55% of the radius, then a fast falloff to the edge
        const t = d < 0.55 ? 1 : 1 - (d - 0.55) / (wobble - 0.55);
        const v = s.conf * Math.max(0, t) ** 0.75;
        if (v > f[i]) f[i] = v;
      }
    }
  });
  return f;
}

const SITES_SELF = [
  { x: 96,  y: 84, r: 34, conf: 1.00 },
  { x: 128, y: 70, r: 20, conf: 0.78 },
  { x: 70,  y: 108, r: 17, conf: 0.55 },
  { x: 150, y: 100, r: 13, conf: 0.34 }
];
const SITES_ALL = SITES_SELF.concat([
  { x: 176, y: 56, r: 30, conf: 0.86 },
  { x: 44,  y: 60, r: 27, conf: 0.72 },
  { x: 206, y: 116, r: 24, conf: 0.63 },
  { x: 30,  y: 132, r: 20, conf: 0.48 },
  { x: 224, y: 34, r: 16, conf: 0.40 },
  { x: 118, y: 148, r: 22, conf: 0.58 },
  { x: 190, y: 150, r: 15, conf: 0.30 }
]);

/* -------------------------------------------------------------- lifespan */

const BASELINE = 672;

function lifespanHTML(age, dead, deathTick) {
  const a = Math.min(1, age / BASELINE);
  const cls = dead ? 'lifespan is-dead' : 'lifespan';
  const end = dead ? `<div class="zone-div" style="left:${(deathTick / BASELINE) * 100}%"></div>` : '';
  return `<div class="lifespan-wrap">
    <div class="${cls}" style="--age:${a}" role="img"
         aria-label="${dead ? 'Died at tick ' + deathTick : 'Age ' + age} of ${BASELINE} baseline">
      <div class="zone-div" style="left:25%"></div>
      <div class="zone-div" style="left:87.5%"></div>${end}
    </div>
    <div class="lifespan-legend"><span>infant</span><span>adult</span><span>elder</span><span>672</span></div>
  </div>`;
}

/* ---------------------------------------------------------------- charts */

const NS = 'http://www.w3.org/2000/svg';
function el(n, attrs, parent) {
  const e = document.createElementNS(NS, n);
  for (const k in attrs) e.setAttribute(k, attrs[k]);
  if (parent) parent.appendChild(e);
  return e;
}

let TIP;
function tip() {
  if (!TIP) { TIP = document.createElement('div'); TIP.className = 'tip'; document.body.appendChild(TIP); }
  return TIP;
}
function showTip(evt, html) {
  const t = tip();
  t.innerHTML = html;
  t.classList.add('on');
  const pad = 14;
  let x = evt.clientX + pad, y = evt.clientY + pad;
  const r = t.getBoundingClientRect();
  if (x + r.width > innerWidth - 8) x = evt.clientX - r.width - pad;
  if (y + r.height > innerHeight - 8) y = evt.clientY - r.height - pad;
  t.style.left = x + 'px'; t.style.top = y + 'px';
}
function hideTip() { if (TIP) TIP.classList.remove('on'); }

const CAT = ['var(--c1)','var(--c2)','var(--c3)','var(--c4)','var(--c5)','var(--c6)'];

/* line / area chart over time */
function lineChart(mount, cfg) {
  const W = cfg.width || 560, H = cfg.height || 200;
  const m = { t: 14, r: 16, b: 24, l: 38 };
  const iw = W - m.l - m.r, ih = H - m.t - m.b;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);

  const xs = cfg.x, maxY = cfg.maxY || Math.max(...cfg.series.flatMap(s => s.v)) * 1.1;
  const X = i => m.l + (i / (xs.length - 1)) * iw;
  const Y = v => m.t + ih - (v / maxY) * ih;

  for (let g = 0; g <= 4; g++) {
    const y = m.t + (g / 4) * ih;
    el('line', { class: 'grid-line', x1: m.l, x2: m.l + iw, y1: y, y2: y }, svg);
    el('text', { class: 'tick-label', x: m.l - 7, y: y + 3, 'text-anchor': 'end' }, svg)
      .textContent = Math.round(maxY * (1 - g / 4));
  }
  el('line', { class: 'axis-line', x1: m.l, x2: m.l + iw, y1: m.t + ih, y2: m.t + ih }, svg);

  cfg.xTicks.forEach(ti => {
    el('text', { class: 'tick-label', x: X(ti.i), y: H - 7, 'text-anchor': 'middle' }, svg).textContent = ti.l;
  });

  const labels = [];
  cfg.series.forEach((s, si) => {
    const col = s.color || CAT[si];
    if (s.area) {
      const d = s.v.map((v, i) => `${i ? 'L' : 'M'}${X(i)},${Y(v)}`).join('') +
                `L${X(s.v.length - 1)},${m.t + ih}L${X(0)},${m.t + ih}Z`;
      el('path', { d, fill: col, opacity: .13 }, svg);
    }
    const d = s.v.map((v, i) => `${i ? 'L' : 'M'}${X(i)},${Y(v)}`).join('');
    el('path', { class: 'series-line', d, stroke: col, 'stroke-dasharray': s.dash || 'none' }, svg);
    if (cfg.directLabel !== false && cfg.series.length <= 4)
      labels.push({ y: Y(s.v[s.v.length - 1]) - 7, name: s.name, col });
  });

  /* de-collide direct labels: series that end close together would otherwise
     stack on top of each other, which is worse than no label at all */
  labels.sort((a, b) => a.y - b.y);
  const MIN = 13;
  for (let i = 1; i < labels.length; i++)
    if (labels[i].y - labels[i - 1].y < MIN) labels[i].y = labels[i - 1].y + MIN;
  const overflow = labels.length && labels[labels.length - 1].y - (m.t + ih);
  if (overflow > 0) labels.forEach(l => l.y -= overflow);
  labels.forEach(l => {
    el('text', { class: 'direct-label', x: m.l + iw - 2, y: Math.max(m.t + 9, l.y), fill: l.col, 'text-anchor': 'end' }, svg)
      .textContent = l.name;
  });

  // crosshair + tooltip
  const cross = el('line', { class: 'axis-line', y1: m.t, y2: m.t + ih, opacity: 0, stroke: 'var(--ink-3)' }, svg);
  const hit = el('rect', { x: m.l, y: m.t, width: iw, height: ih, fill: 'transparent' }, svg);
  hit.style.cursor = 'crosshair';
  hit.addEventListener('mousemove', e => {
    const b = svg.getBoundingClientRect();
    const rel = ((e.clientX - b.left) / b.width) * W;
    const i = Math.max(0, Math.min(xs.length - 1, Math.round(((rel - m.l) / iw) * (xs.length - 1))));
    cross.setAttribute('x1', X(i)); cross.setAttribute('x2', X(i)); cross.setAttribute('opacity', .5);
    showTip(e, `<span class="tip-t">${cfg.xLabel || 'tick'} ${xs[i]}</span>` +
      cfg.series.map((s, si) => `<div class="tip-r"><span class="sw" style="background:${s.color || CAT[si]}"></span>${s.name}<span class="v">${s.v[i]}${cfg.unit || ''}</span></div>`).join(''));
  });
  hit.addEventListener('mouseleave', () => { cross.setAttribute('opacity', 0); hideTip(); });
  return svg;
}

/* stacked bars — categories across an ordinal axis */
function stackedBars(mount, cfg) {
  const W = cfg.width || 560, H = cfg.height || 210;
  const m = { t: 14, r: 16, b: 26, l: 38 };
  const iw = W - m.l - m.r, ih = H - m.t - m.b;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);

  const totals = cfg.groups.map(g => g.v.reduce((a, b) => a + b, 0));
  const maxY = cfg.maxY || Math.max(...totals) * 1.12;
  const bw = iw / cfg.groups.length;
  const bar = Math.min(46, bw * 0.62);

  for (let g = 0; g <= 4; g++) {
    const y = m.t + (g / 4) * ih;
    el('line', { class: 'grid-line', x1: m.l, x2: m.l + iw, y1: y, y2: y }, svg);
    el('text', { class: 'tick-label', x: m.l - 7, y: y + 3, 'text-anchor': 'end' }, svg)
      .textContent = Math.round(maxY * (1 - g / 4));
  }
  el('line', { class: 'axis-line', x1: m.l, x2: m.l + iw, y1: m.t + ih, y2: m.t + ih }, svg);

  cfg.groups.forEach((grp, gi) => {
    const cx = m.l + bw * (gi + 0.5);
    let acc = 0;
    grp.v.forEach((v, si) => {
      if (v <= 0) return;
      const h = (v / maxY) * ih;
      const y = m.t + ih - (acc / maxY) * ih - h;
      const rect = el('rect', {
        class: 'bar-seg', x: cx - bar / 2, y, width: bar, height: Math.max(1, h),
        fill: CAT[si], rx: 1
      }, svg);
      rect.addEventListener('mousemove', e => showTip(e,
        `<span class="tip-t">${grp.name}</span><div class="tip-r"><span class="sw" style="background:${CAT[si]}"></span>${cfg.keys[si]}<span class="v">${v}</span></div>`));
      rect.addEventListener('mouseleave', hideTip);
      acc += v;
    });
    el('text', { class: 'tick-label', x: cx, y: H - 8, 'text-anchor': 'middle' }, svg).textContent = grp.name;
  });
  return svg;
}

/* grouped bars — two measures compared side by side, never stacked
   (stacking would imply they sum to something, and they don't) */
function groupedBars(mount, cfg) {
  const W = cfg.width || 560, H = cfg.height || 210;
  const m = { t: 14, r: 16, b: 30, l: 38 };
  const iw = W - m.l - m.r, ih = H - m.t - m.b;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);
  const maxY = cfg.maxY || Math.max(...cfg.groups.flatMap(g => g.v)) * 1.14;
  const gw = iw / cfg.groups.length;
  const n = cfg.keys.length;
  const bw = Math.min(20, (gw * 0.66) / n);

  for (let g = 0; g <= 4; g++) {
    const y = m.t + (g / 4) * ih;
    el('line', { class: 'grid-line', x1: m.l, x2: m.l + iw, y1: y, y2: y }, svg);
    el('text', { class: 'tick-label', x: m.l - 7, y: y + 3, 'text-anchor': 'end' }, svg)
      .textContent = Math.round(maxY * (1 - g / 4)) + (cfg.unit || '');
  }
  el('line', { class: 'axis-line', x1: m.l, x2: m.l + iw, y1: m.t + ih, y2: m.t + ih }, svg);

  cfg.groups.forEach((grp, gi) => {
    const cx = m.l + gw * (gi + 0.5);
    grp.v.forEach((v, si) => {
      const h = (v / maxY) * ih;
      const x = cx - (n * bw + (n - 1) * 2) / 2 + si * (bw + 2);
      const col = cfg.colors ? cfg.colors[si] : CAT[si];
      const rect = el('rect', {
        x, y: m.t + ih - h, width: bw, height: Math.max(1, h), fill: col, rx: 1
      }, svg);
      rect.addEventListener('mousemove', e => showTip(e,
        `<span class="tip-t">${grp.name}</span><div class="tip-r"><span class="sw" style="background:${col}"></span>${cfg.keys[si]}<span class="v">${v}${cfg.unit || ''}</span></div>`));
      rect.addEventListener('mouseleave', hideTip);
    });
    el('text', { class: 'tick-label', x: cx, y: H - 10, 'text-anchor': 'middle' }, svg).textContent = grp.name;
  });
  return svg;
}

/* horizontal ranked bars */
function rankedBars(mount, cfg) {
  const rowH = 26, W = cfg.width || 560;
  const H = cfg.rows.length * rowH + 18;
  const labelW = cfg.labelW || 96;
  const m = { t: 6, r: 44, l: labelW };
  const iw = W - m.l - m.r;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);
  const max = Math.max(...cfg.rows.map(r => r.v));

  cfg.rows.forEach((r, i) => {
    const y = m.t + i * rowH;
    el('text', { class: 'tick-label', x: m.l - 10, y: y + 13, 'text-anchor': 'end', style: 'font-size:10px;fill:var(--ink-2)' }, svg)
      .textContent = r.name;
    el('rect', { x: m.l, y: y + 4, width: iw, height: 9, fill: 'var(--raised)', rx: 1 }, svg);
    const w = Math.max(2, (r.v / max) * iw);
    const bar = el('rect', { x: m.l, y: y + 4, width: w, height: 9, fill: r.color || CAT[0], rx: 1 }, svg);
    bar.addEventListener('mousemove', e => showTip(e, `<span class="tip-t">${r.name}</span>${cfg.unit || ''} <b>${r.v}</b>${r.note ? '<br>' + r.note : ''}`));
    bar.addEventListener('mouseleave', hideTip);
    el('text', { class: 'direct-label', x: m.l + w + 7, y: y + 13, fill: 'var(--ink-2)' }, svg).textContent = r.v + (cfg.suffix || '');
  });
  return svg;
}

/* histogram with a reference marker (the 672 baseline) */
function histogram(mount, cfg) {
  const W = cfg.width || 560, H = cfg.height || 190;
  const m = { t: 14, r: 16, b: 26, l: 38 };
  const iw = W - m.l - m.r, ih = H - m.t - m.b;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);
  const max = Math.max(...cfg.bins.map(b => b.v)) * 1.1;
  const bw = iw / cfg.bins.length;

  for (let g = 0; g <= 3; g++) {
    const y = m.t + (g / 3) * ih;
    el('line', { class: 'grid-line', x1: m.l, x2: m.l + iw, y1: y, y2: y }, svg);
    el('text', { class: 'tick-label', x: m.l - 7, y: y + 3, 'text-anchor': 'end' }, svg)
      .textContent = Math.round(max * (1 - g / 3));
  }
  cfg.bins.forEach((b, i) => {
    const h = (b.v / max) * ih;
    const x = m.l + i * bw;
    const rect = el('rect', {
      class: 'bar-seg', x: x + 1.5, y: m.t + ih - h, width: bw - 3, height: Math.max(1, h),
      fill: b.color || 'var(--c1)', rx: 1
    }, svg);
    rect.addEventListener('mousemove', e => showTip(e, `<span class="tip-t">${b.name} ticks</span><b>${b.v}</b> creatures`));
    rect.addEventListener('mouseleave', hideTip);
    if (i % cfg.labelEvery === 0)
      el('text', { class: 'tick-label', x: x + bw / 2, y: H - 8, 'text-anchor': 'middle' }, svg).textContent = b.name;
  });
  el('line', { class: 'axis-line', x1: m.l, x2: m.l + iw, y1: m.t + ih, y2: m.t + ih }, svg);

  if (cfg.marker != null) {
    const mx = m.l + (cfg.marker / cfg.bins.length) * iw;
    el('line', { x1: mx, x2: mx, y1: m.t - 2, y2: m.t + ih, stroke: 'var(--still)', 'stroke-width': 1.5, 'stroke-dasharray': '3 3' }, svg);
    el('text', { class: 'direct-label', x: mx - 5, y: m.t + 8, fill: 'var(--still)', 'text-anchor': 'end' }, svg)
      .textContent = cfg.markerLabel;
  }
  return svg;
}

/* scatter. maxY defaults to maxX; the y=x reference line is opt-in and only
   meaningful when both axes carry the same unit. */
function scatter(mount, cfg) {
  const W = cfg.width || 400, H = cfg.height || 260;
  const m = { t: 14, r: 14, b: 34, l: 40 };
  const iw = W - m.l - m.r, ih = H - m.t - m.b;
  const svg = el('svg', { viewBox: `0 0 ${W} ${H}`, width: '100%', role: 'img', 'aria-label': cfg.title }, mount);
  const maxX = cfg.maxX, maxY = cfg.maxY || cfg.maxX;
  const X = v => m.l + (v / maxX) * iw, Y = v => m.t + ih - (v / maxY) * ih;

  for (let g = 0; g <= 4; g++) {
    const y = m.t + (g / 4) * ih;
    el('line', { class: 'grid-line', x1: m.l, x2: m.l + iw, y1: y, y2: y }, svg);
    el('text', { class: 'tick-label', x: m.l - 7, y: y + 3, 'text-anchor': 'end' }, svg)
      .textContent = Math.round(maxY * (1 - g / 4));
  }
  el('line', { class: 'axis-line', x1: m.l, x2: m.l + iw, y1: m.t + ih, y2: m.t + ih }, svg);

  if (cfg.diagonal) {
    el('line', { x1: X(0), y1: Y(0), x2: X(maxX), y2: Y(maxY), stroke: 'var(--ink-3)', 'stroke-width': 1, 'stroke-dasharray': '4 4' }, svg);
    el('text', { class: 'direct-label', x: X(maxX) - 4, y: Y(maxY) + 15, fill: 'var(--ink-3)', 'text-anchor': 'end' }, svg)
      .textContent = cfg.diagonalLabel || '';
  }

  cfg.points.forEach(p => {
    const col = cfg.colorFn ? cfg.colorFn(p) : 'var(--c3)';
    const c = el('circle', { cx: X(p[0]), cy: Y(p[1]), r: p[2] || 4, fill: col, opacity: .78, stroke: 'var(--panel)', 'stroke-width': 1.5 }, svg);
    c.addEventListener('mousemove', e => showTip(e, cfg.tipFn ? cfg.tipFn(p) : `<b>${p[0]}</b>, <b>${p[1]}</b>`));
    c.addEventListener('mouseleave', hideTip);
  });
  el('text', { class: 'tick-label', x: m.l + iw / 2, y: H - 8, 'text-anchor': 'middle' }, svg).textContent = cfg.xLabel;
  if (cfg.yLabel)
    el('text', { class: 'tick-label', x: -(m.t + ih / 2), y: 11, 'text-anchor': 'middle', transform: 'rotate(-90)' }, svg)
      .textContent = cfg.yLabel;
  return svg;
}

/* ------------------------------------------------------------------ misc */

function need(label, v) {
  const cls = v > 60 ? '' : v > 35 ? 'warn' : v > 15 ? 'serious' : 'critical';
  return `<div class="need"><span class="lbl">${label}</span>
    <div class="track"><div class="fill ${cls}" style="width:${v}%"></div></div>
    <span class="v">${v}</span></div>`;
}

function confColor(c) {
  return c > .8 ? 'var(--seq-5)' : c > .6 ? 'var(--seq-4)' : c > .4 ? 'var(--seq-3)' : c > .2 ? 'var(--seq-2)' : 'var(--seq-1)';
}
