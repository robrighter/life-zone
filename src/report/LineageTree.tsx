import { useMemo, useState } from "react";
import type { TreeNode } from "./types";

/**
 * The interactive lineage tree (§10 — "the headline; this is the stated goal of
 * the game").
 *
 * Laid out by generation left-to-right rather than as a dendrogram, because the
 * question the tree exists to answer is *how deep did this bloodline get* — so
 * depth is the axis you read along, and a founder whose line stopped at
 * generation 2 should be visibly short next to one that reached 5.
 *
 * Both parents are recorded, so this is a DAG, not a tree: a child of two
 * members of the same lineage would otherwise be drawn twice. Each creature is
 * placed once, under whichever parent is already in the lineage, and the second
 * parent is drawn as a faint edge so the pairing is still visible.
 */

interface Props {
  nodes: TreeNode[];
  selected: number | null;
  onSelect: (id: number) => void;
}

const ROW = 22;
const COL = 132;
const PAD = 26;

export function LineageTree({ nodes, selected, onSelect }: Props) {
  const [collapsed, setCollapsed] = useState<Set<number>>(new Set());

  const layout = useMemo(() => {
    const byId = new Map(nodes.map((n) => [n.id, n]));
    const kids = new Map<number, TreeNode[]>();
    const placed = new Set<number>();
    const roots: TreeNode[] = [];

    // Ascending id, so siblings appear in birth order and the drawing is the
    // same every time it is opened.
    const sorted = [...nodes].sort((a, b) => a.id - b.id);
    for (const n of sorted) {
      const parent =
        (n.mother_id != null && byId.has(n.mother_id) && n.mother_id) ||
        (n.father_id != null && byId.has(n.father_id) && n.father_id) ||
        null;
      if (parent == null) {
        roots.push(n);
      } else {
        const list = kids.get(parent) ?? [];
        list.push(n);
        kids.set(parent, list);
      }
    }

    const out: { n: TreeNode; x: number; y: number; hidden: number }[] = [];
    const edges: { x1: number; y1: number; x2: number; y2: number; faint?: boolean }[] = [];
    const at = new Map<number, { x: number; y: number }>();
    let row = 0;

    const walk = (n: TreeNode, depth: number) => {
      if (placed.has(n.id)) return;
      placed.add(n.id);
      const pos = { x: PAD + depth * COL, y: PAD + row * ROW };
      row += 1;
      at.set(n.id, pos);
      const children = kids.get(n.id) ?? [];
      const isCollapsed = collapsed.has(n.id);
      out.push({ n, ...pos, hidden: isCollapsed ? children.length : 0 });
      if (!isCollapsed) for (const k of children) walk(k, depth + 1);
    };
    for (const r of roots) walk(r, 0);

    for (const { n } of out) {
      const me = at.get(n.id)!;
      for (const p of [n.mother_id, n.father_id]) {
        if (p == null) continue;
        const pp = at.get(p);
        if (!pp) continue;
        // The edge that placed this creature is solid; the other parent's is
        // faint, so a pairing inside the lineage reads as a pairing and not as
        // the same child appearing twice.
        const primary = pp.x === me.x - COL && placed.has(p);
        edges.push({ x1: pp.x, y1: pp.y, x2: me.x, y2: me.y, faint: !primary });
      }
    }
    const depth = out.reduce((m, o) => Math.max(m, o.x), 0);
    return { out, edges, w: depth + 200, h: PAD * 2 + row * ROW };
  }, [nodes, collapsed]);

  if (nodes.length === 0) {
    return <p className="fig-empty">Pick a founder from the leaderboard.</p>;
  }

  return (
    <div className="tree-scroll">
      <svg width={layout.w} height={layout.h} role="img" aria-label="Lineage tree">
        {layout.edges.map((e, i) => (
          <path
            key={i}
            className={e.faint ? "tree-edge faint" : "tree-edge"}
            d={`M${e.x1} ${e.y1} C${e.x1 + COL / 2} ${e.y1}, ${e.x2 - COL / 2} ${e.y2}, ${e.x2} ${e.y2}`}
            fill="none"
          />
        ))}
        {layout.out.map(({ n, x, y, hidden }) => {
          const dead = n.death_tick != null;
          return (
            <g
              key={n.id}
              className={`tree-node${dead ? " dead" : ""}${selected === n.id ? " on" : ""}`}
              transform={`translate(${x},${y})`}
              onClick={() => onSelect(n.id)}
            >
              {/* Alive is a filled mark, dead is hollow — the amber/bone
                  duality the rest of the app runs on, and readable without
                  relying on hue alone. */}
              <circle r={4} />
              <text x={9} y={4}>
                {n.name}
              </text>
              <text className="tree-meta" x={9} y={4} dx={n.name.length * 6.6 + 8}>
                g{n.generation}
                {dead ? ` · ${n.death_cause ?? "died"}` : ""}
              </text>
              {n.children > 0 && (
                <text
                  className="tree-toggle"
                  x={-13}
                  y={4}
                  onClick={(e) => {
                    e.stopPropagation();
                    setCollapsed((prev) => {
                      const next = new Set(prev);
                      next.has(n.id) ? next.delete(n.id) : next.add(n.id);
                      return next;
                    });
                  }}
                >
                  {hidden ? `+${hidden}` : "−"}
                </text>
              )}
            </g>
          );
        })}
      </svg>
    </div>
  );
}
