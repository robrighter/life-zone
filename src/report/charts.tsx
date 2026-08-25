import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";

/**
 * The chart kit (BUILD.md §7.4).
 *
 * The rules there are non-negotiable, so they are built into the components
 * rather than left as per-chart discipline:
 *
 * - **There is no dual-axis chart in this file.** Not "discouraged" — absent.
 *   Two measures of different scale are two `<LineChart>`s or an indexed one.
 * - `<BarChart>` is grouped. `<StackedBar>` exists separately and its prop is
 *   named `partsOfAWhole` so that stacking something that is not one requires
 *   writing down a claim that is false.
 * - `<Figure>` renders a legend whenever there are two or more series, and a
 *   table toggle for every chart without the caller asking.
 * - Direct labels are de-collided and clamped by `spread()`.
 * - Colour comes from `colorFor(namespace, entity)`, which is a registry keyed
 *   by the entity's own name. Filtering a series out cannot repaint the ones
 *   that remain, because nothing is keyed by position.
 */

// ------------------------------------------------------------------- colour

const SLOTS = ["--c1", "--c2", "--c3", "--c4", "--c5", "--c6"] as const;

/**
 * Entities that have a natural colour keep it everywhere. Seeded up front so a
 * cause of death is the same hue on the mortality chart and the life story, and
 * so the assignment does not depend on which report you opened first.
 */
const FIXED: Record<string, string> = {
  "cause:STARVATION": "--c1",
  "cause:DEHYDRATION": "--c5",
  "cause:EXPOSURE": "--c2",
  "cause:OLD_AGE": "--still",
  "cause:ILLNESS": "--c3",
  "cause:CHILDBIRTH": "--c4",
  "cause:ACCIDENT": "--c6",
  "tier:Tier 1": "--c5",
  "tier:Tier 2": "--c1",
  "res:grain": "--res-wheat",
  "res:wood": "--res-wood",
  "res:forage": "--res-forage",
  "flow:births": "--quick",
  "flow:deaths": "--still",
};

const assigned = new Map<string, string>(Object.entries(FIXED));
let nextSlot = 0;

/** A CSS variable reference for an entity. Stable for the life of the window. */
export function colorFor(namespace: string, entity: string): string {
  const key = `${namespace}:${entity}`;
  let slot = assigned.get(key);
  if (!slot) {
    slot = SLOTS[nextSlot++ % SLOTS.length];
    assigned.set(key, slot);
  }
  return `var(${slot})`;
}

// ------------------------------------------------------------------ geometry

const PAD = { top: 14, right: 92, bottom: 30, left: 52 };

function useWidth<T extends HTMLElement>() {
  const ref = useRef<T | null>(null);
  const [w, setW] = useState(640);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const ro = new ResizeObserver(([e]) => setW(Math.max(320, e.contentRect.width)));
    ro.observe(el);
    return () => ro.disconnect();
  }, []);
  return [ref, w] as const;
}

/**
 * Push labels apart so a cluster of series ending at nearly the same value is
 * still readable, and keep every one inside the plot (§7.4).
 *
 * A single pass of "sort, then walk down enforcing a minimum gap" is enough for
 * the handful of series any chart here has, and unlike an iterative relaxation
 * it always terminates in the same arrangement — which matters, because a
 * legend that reshuffles on re-render is worse than one that collides.
 */
function spread(items: { y: number }[], min: number, lo: number, hi: number): number[] {
  const order = items.map((it, i) => ({ i, y: it.y })).sort((a, b) => a.y - b.y);
  const out = new Array<number>(items.length);
  let last = -Infinity;
  for (const { i, y } of order) {
    const placed = Math.max(y, last + min);
    out[i] = placed;
    last = placed;
  }
  // If the stack overflowed the bottom, slide the whole run back up.
  const overflow = Math.max(0, (out.length ? Math.max(...out) : 0) - hi);
  return out.map((y) => Math.min(hi, Math.max(lo, y - overflow)));
}

function niceTicks(min: number, max: number, count = 4): number[] {
  if (!isFinite(min) || !isFinite(max) || max === min) return [min];
  const raw = (max - min) / count;
  const mag = Math.pow(10, Math.floor(Math.log10(raw)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? mag * 10;
  const out: number[] = [];
  for (let v = Math.ceil(min / step) * step; v <= max + 1e-9; v += step) out.push(v);
  return out;
}

const fmt = (n: number): string => {
  if (!isFinite(n)) return "—";
  const a = Math.abs(n);
  if (a >= 10_000) return `${(n / 1000).toFixed(a >= 100_000 ? 0 : 1)}k`;
  if (a >= 100) return n.toFixed(0);
  if (a >= 1) return n.toFixed(a >= 10 ? 1 : 2).replace(/\.?0+$/, "");
  if (a === 0) return "0";
  return n.toFixed(3).replace(/0+$/, "");
};

// -------------------------------------------------------------------- figure

export interface Series<T> {
  /** The entity's own name — this is what colour is keyed by, never the index. */
  key: string;
  label: string;
  value: (d: T) => number;
  /** Overrides the registry for series with a meaning of their own. */
  color?: string;
}

interface FigureProps<T> {
  title: string;
  /** What the chart is for. One sentence, in ink, never in a series colour. */
  note?: string;
  /** Shown instead of the chart when the sample is too thin to read. */
  thin?: string;
  rows: T[];
  series: Series<T>[];
  columns?: { key: string; label: string; get: (d: T) => string | number }[];
  children: ReactNode;
}

/**
 * Frame, legend, and the table view that §7.4 requires for every chart.
 *
 * The table is not a fallback — it is the same numbers without the encoding,
 * and it is the only honest way to read a chart whose whole point is a
 * difference of two percent.
 */
export function Figure<T>({ title, note, thin, rows, series, columns, children }: FigureProps<T>) {
  const [asTable, setAsTable] = useState(false);
  const cols =
    columns ??
    series.map((s) => ({ key: s.key, label: s.label, get: (d: T) => fmt(s.value(d)) }));

  return (
    <figure className="fig">
      <div className="fig-head">
        <div>
          <h3>{title}</h3>
          {note && <p className="fig-note">{note}</p>}
        </div>
        <button
          className="fig-toggle"
          onClick={() => setAsTable((v) => !v)}
          aria-pressed={asTable}
        >
          {asTable ? "chart" : "table"}
        </button>
      </div>

      {rows.length === 0 ? (
        <p className="fig-empty">Nothing recorded yet.</p>
      ) : asTable ? (
        <div className="fig-scroll">
          <table className="fig-table">
            <thead>
              <tr>
                {cols.map((c) => (
                  <th key={c.key}>{c.label}</th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r, i) => (
                <tr key={i}>
                  {cols.map((c) => (
                    <td key={c.key}>{c.get(r)}</td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <>
          {children}
          {/* Identity is never conveyed by colour alone: the legend carries the
              name, and the swatch is only a second cue (§7.4). */}
          {series.length >= 2 && (
            <ul className="legend">
              {series.map((s) => (
                <li key={s.key}>
                  <span
                    className="swatch"
                    style={{ background: s.color ?? colorFor("series", s.key) }}
                  />
                  {s.label}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      {thin && <figcaption className="fig-thin">{thin}</figcaption>}
    </figure>
  );
}

// ---------------------------------------------------------------- line chart

interface LineProps<T> {
  rows: T[];
  series: Series<T>[];
  x: (d: T) => number;
  xLabel?: string;
  height?: number;
  /** Draw from zero rather than from the data's own floor. */
  zero?: boolean;
  /** A horizontal reference, e.g. the 672-tick lifespan baseline. */
  rule?: { at: number; label: string };
}

export function LineChart<T>({
  rows, series, x, xLabel, height = 200, zero = true, rule,
}: LineProps<T>) {
  const [ref, w] = useWidth<HTMLDivElement>();
  const h = height;
  const iw = Math.max(40, w - PAD.left - PAD.right);
  const ih = h - PAD.top - PAD.bottom;

  const geom = useMemo(() => {
    const xs = rows.map(x);
    const x0 = Math.min(...xs), x1 = Math.max(...xs);
    const all = series.flatMap((s) => rows.map(s.value)).filter(isFinite);
    if (rule) all.push(rule.at);
    let y0 = zero ? 0 : Math.min(...all);
    let y1 = Math.max(...all);
    if (y1 === y0) y1 = y0 + 1;
    const px = (v: number) => PAD.left + (x1 === x0 ? iw / 2 : ((v - x0) / (x1 - x0)) * iw);
    const py = (v: number) => PAD.top + ih - ((v - y0) / (y1 - y0)) * ih;
    return { px, py, y0, y1, x0, x1 };
  }, [rows, series, x, iw, ih, zero, rule]);

  const ends = series.map((s) => ({
    s,
    y: geom.py(rows.length ? s.value(rows[rows.length - 1]) : 0),
  }));
  const labelled = spread(ends, 13, PAD.top, PAD.top + ih);

  return (
    <div ref={ref} className="chart">
      <svg width="100%" height={h} viewBox={`0 0 ${w} ${h}`} role="img">
        {niceTicks(geom.y0, geom.y1).map((t) => (
          <g key={t}>
            <line className="grid" x1={PAD.left} x2={PAD.left + iw} y1={geom.py(t)} y2={geom.py(t)} />
            <text className="tick" x={PAD.left - 8} y={geom.py(t) + 4} textAnchor="end">
              {fmt(t)}
            </text>
          </g>
        ))}
        {rule && (
          <g>
            <line
              className="rule" x1={PAD.left} x2={PAD.left + iw}
              y1={geom.py(rule.at)} y2={geom.py(rule.at)}
            />
            <text className="rule-label" x={PAD.left + 4} y={geom.py(rule.at) - 5}>
              {rule.label}
            </text>
          </g>
        )}

        {series.map((s) => {
          const d = rows
            .map((r, i) => `${i ? "L" : "M"}${geom.px(x(r)).toFixed(1)} ${geom.py(s.value(r)).toFixed(1)}`)
            .join(" ");
          return (
            <path
              key={s.key} d={d} fill="none"
              stroke={s.color ?? colorFor("series", s.key)}
              strokeWidth={1.25} strokeLinejoin="round"
            />
          );
        })}

        {/* One label per series at its end — never a number on every point. */}
        {ends.map((e, i) => (
          <text
            key={e.s.key} className="direct" x={PAD.left + iw + 6} y={labelled[i] + 3}
            fill={e.s.color ?? colorFor("series", e.s.key)}
          >
            {e.s.label}
          </text>
        ))}

        <line className="axis" x1={PAD.left} x2={PAD.left + iw} y1={PAD.top + ih} y2={PAD.top + ih} />
        <text className="tick" x={PAD.left} y={h - 8}>{fmt(geom.x0)}</text>
        <text className="tick" x={PAD.left + iw} y={h - 8} textAnchor="end">{fmt(geom.x1)}</text>
        {xLabel && (
          <text className="axis-label" x={PAD.left + iw / 2} y={h - 8} textAnchor="middle">
            {xLabel}
          </text>
        )}
      </svg>
    </div>
  );
}

// ----------------------------------------------------------- bars (grouped)

interface BarProps<T> {
  rows: T[];
  series: Series<T>[];
  label: (d: T) => string;
  height?: number;
  /** Percentages rather than counts on the axis. */
  share?: boolean;
}

/**
 * Grouped bars, always (§7.4).
 *
 * "Share of population" and "share of LLM calls" are not parts of one whole and
 * stacking them would be a lie, so this component cannot stack. If the data
 * genuinely is a decomposition, that is what `<StackedBar>` is for.
 */
export function BarChart<T>({ rows, series, label, height = 220, share }: BarProps<T>) {
  const [ref, w] = useWidth<HTMLDivElement>();
  const h = height;
  const iw = Math.max(40, w - PAD.left - PAD.right);
  const ih = h - PAD.top - PAD.bottom;

  const max = Math.max(1e-9, ...series.flatMap((s) => rows.map(s.value)));
  const band = iw / Math.max(1, rows.length);
  const bw = Math.max(2, (band * 0.72) / series.length);
  const py = (v: number) => PAD.top + ih - (v / max) * ih;

  return (
    <div ref={ref} className="chart">
      <svg width="100%" height={h} viewBox={`0 0 ${w} ${h}`} role="img">
        {niceTicks(0, max).map((t) => (
          <g key={t}>
            <line className="grid" x1={PAD.left} x2={PAD.left + iw} y1={py(t)} y2={py(t)} />
            <text className="tick" x={PAD.left - 8} y={py(t) + 4} textAnchor="end">
              {share ? `${(t * 100).toFixed(0)}%` : fmt(t)}
            </text>
          </g>
        ))}
        {rows.map((r, i) => (
          <g key={i}>
            {series.map((s, j) => {
              const v = s.value(r);
              const x0 = PAD.left + i * band + band * 0.14 + j * bw;
              return (
                <rect
                  key={s.key} x={x0} y={py(v)} width={bw} height={Math.max(0, PAD.top + ih - py(v))}
                  fill={s.color ?? colorFor("series", s.key)}
                />
              );
            })}
            <text
              className="tick" x={PAD.left + i * band + band / 2} y={h - 12} textAnchor="middle"
            >
              {label(r)}
            </text>
          </g>
        ))}
        <line className="axis" x1={PAD.left} x2={PAD.left + iw} y1={PAD.top + ih} y2={PAD.top + ih} />
      </svg>
    </div>
  );
}

interface StackProps<T> {
  rows: T[];
  series: Series<T>[];
  label: (d: T) => string;
  height?: number;
  /**
   * Required, and required to be true. Stacking asserts the parts sum to the
   * whole; making that a written claim is the point (§7.4).
   */
  partsOfAWhole: true;
}

/** Stacked bars — only ever for a decomposition of one quantity. */
export function StackedBar<T>({ rows, series, label, height = 220 }: StackProps<T>) {
  const [ref, w] = useWidth<HTMLDivElement>();
  const h = height;
  const iw = Math.max(40, w - PAD.left - PAD.right);
  const ih = h - PAD.top - PAD.bottom;

  const totals = rows.map((r) => series.reduce((a, s) => a + s.value(r), 0));
  const max = Math.max(1e-9, ...totals);
  const band = iw / Math.max(1, rows.length);
  const bw = Math.max(3, band * 0.68);

  return (
    <div ref={ref} className="chart">
      <svg width="100%" height={h} viewBox={`0 0 ${w} ${h}`} role="img">
        {niceTicks(0, max).map((t) => (
          <g key={t}>
            <line
              className="grid" x1={PAD.left} x2={PAD.left + iw}
              y1={PAD.top + ih - (t / max) * ih} y2={PAD.top + ih - (t / max) * ih}
            />
            <text className="tick" x={PAD.left - 8} y={PAD.top + ih - (t / max) * ih + 4} textAnchor="end">
              {fmt(t)}
            </text>
          </g>
        ))}
        {rows.map((r, i) => {
          let acc = 0;
          return (
            <g key={i}>
              {series.map((s) => {
                const v = s.value(r);
                const y0 = PAD.top + ih - ((acc + v) / max) * ih;
                const y1 = PAD.top + ih - (acc / max) * ih;
                acc += v;
                return (
                  <rect
                    key={s.key} x={PAD.left + i * band + (band - bw) / 2} y={y0}
                    width={bw} height={Math.max(0, y1 - y0)}
                    fill={s.color ?? colorFor("series", s.key)}
                  />
                );
              })}
              <text className="tick" x={PAD.left + i * band + band / 2} y={h - 12} textAnchor="middle">
                {label(r)}
              </text>
            </g>
          );
        })}
        <line className="axis" x1={PAD.left} x2={PAD.left + iw} y1={PAD.top + ih} y2={PAD.top + ih} />
      </svg>
    </div>
  );
}

// ------------------------------------------------------------------ numbers

export function Stat({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="stat">
      <div className="stat-label">{label}</div>
      <div className="stat-value">{value}</div>
      {sub && <div className="stat-sub">{sub}</div>}
    </div>
  );
}

export { fmt };
