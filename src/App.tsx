import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  benchMode, getNodes, getSnapshot, getTerrain, getWorld, getWorldMeta, onTick,
  reportBench, simControl,
  type ResourceNode, type Snapshot, type SpeedMode, type WorldMeta, type WorldSummary,
} from "./ipc";
import { MapView, type BenchResult, type MapStats, type Overlays } from "./map/MapView";
import { TERRAIN_NAMES, TERRAIN_SETS } from "./map/palette";
import { Inspector } from "./panels/Inspector";
import { PALETTES, usePalette } from "./ui/usePalette";

const MODES: { id: SpeedMode; name: string; sub: string }[] = [
  { id: "OBSERVE", name: "Observe", sub: "watchable — no LLM until M3" },
  { id: "FAST_FORWARD", name: "Fast-forward", sub: "no LLM · target <50ms/tick" },
  { id: "DEEP", name: "Deep", sub: "slow, for reading one tick" },
  { id: "FOCUS", name: "Focus", sub: "follows one lineage — M3" },
];

export default function App() {
  const { palette, setPalette } = usePalette();
  const [world, setWorld] = useState<WorldSummary | null>(null);
  const [meta, setMeta] = useState<WorldMeta | null>(null);
  const [terrain, setTerrain] = useState<Uint8Array | null>(null);
  const [nodes, setNodes] = useState<ResourceNode[]>([]);
  const [snap, setSnap] = useState<Snapshot | null>(null);
  const [stats, setStats] = useState<MapStats | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [seedInput, setSeedInput] = useState("");
  const [popInput, setPopInput] = useState("300");
  const [selectedId, setSelectedId] = useState<number | null>(null);
  const [benchFrames, setBenchFrames] = useState<number | null>(null);
  const [bench, setBench] = useState<BenchResult | null>(null);
  const [overlays, setOverlays] = useState<Overlays>({
    nodes: true, creatures: true, structures: true, plans: true, knowledge: false,
  });

  const nodesVersion = useRef(-1);

  useEffect(() => {
    (async () => {
      try {
        const [w, m, t, s] = await Promise.all([
          getWorld(), getWorldMeta(), getTerrain(), getSnapshot(),
        ]);
        setWorld(w); setMeta(m); setTerrain(t); setSnap(s);
        setSeedInput(String(w.seed));
        if (w.config.bench.initial_creatures) {
          setPopInput(String(w.config.bench.initial_creatures));
        }
        const n = await getNodes();
        nodesVersion.current = n.version;
        setNodes(n.nodes);
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  // Snapshots are pushed by the simulation thread, never polled. The UI cannot
  // stall a tick because it never holds anything the tick loop needs.
  useEffect(() => {
    const un = onTick((s) => setSnap(s));
    return () => { un.then((f) => f()); };
  }, []);


  // Resource nodes change as crops are planted and patches are stripped, but
  // far more slowly than creatures move, so they come over separately when the
  // sim says they have changed.
  useEffect(() => {
    if (!snap || snap.nodes_version === nodesVersion.current) return;
    nodesVersion.current = snap.nodes_version;
    getNodes().then((n) => setNodes(n.nodes)).catch(() => {});
  }, [snap?.nodes_version]);

  const select = useCallback((id: number | null) => {
    setSelectedId(id);
    void simControl({ kind: "select", id });
  }, []);

  const onStats = useCallback((s: MapStats) => setStats(s), []);
  const onBench = useCallback((r: BenchResult) => {
    setBench(r);
    setBenchFrames(null);
    // To the Rust log as well, so a measured result survives the window closing.
    void reportBench(r);
  }, []);

  // Under LIFE_ZONE_BENCH, measure automatically once the map is up. Frame
  // intervals can only be observed in the renderer, so the measurement has to
  // originate here even though the result belongs in the log.
  useEffect(() => {
    if (!meta || !terrain) return;
    let cancelled = false;
    benchMode().then((on) => {
      if (on && !cancelled) setTimeout(() => setBenchFrames(600), 1200);
    });
    return () => { cancelled = true; };
  }, [meta, terrain]);

  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if ((e.target as HTMLElement)?.tagName === "INPUT") return;
      if (e.key === " ") {
        e.preventDefault();
        void simControl({ kind: snap?.running ? "pause" : "play" });
      }
      if (e.key === ".") void simControl({ kind: "step", ticks: 1 });
      if (e.key.toLowerCase() === "k") {
        setOverlays((o) => ({ ...o, knowledge: !o.knowledge }));
      }
      if (e.key.toLowerCase() === "b" && !e.repeat) {
        setBench(null);
        setBenchFrames(600);
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, [snap?.running]);

  const counts = useMemo(
    () => nodes.reduce<Record<string, number>>((a, n) => {
      if (n.quantity > 0) a[n.kind] = (a[n.kind] ?? 0) + 1;
      return a;
    }, {}),
    [nodes],
  );

  const clock = snap
    ? `D${snap.day} · ${String(snap.hour).padStart(2, "0")}:00`
    : "—";
  const totalDeaths = snap?.deaths_by_cause.reduce((a, [, n]) => a + n, 0) ?? 0;

  return (
    <div className="app">
      <header className="topbar">
        <span className="brand">Life Zone</span>

        <div className="topbar-right">
          <div className="pal-switch">
            <span className="eyebrow">Palette</span>
            {PALETTES.map((p) => (
              <button
                key={p.id}
                data-pal={p.id}
                style={{ ["--sw" as string]: p.sw }}
                aria-pressed={palette === p.id}
                onClick={() => setPalette(p.id)}
              >
                {p.name}
              </button>
            ))}
          </div>

          <div className="readout">
            <span className="eyebrow">World</span>
            <span className="val">{world ? `${world.name} · ${world.seed}` : "—"}</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Tick</span>
            <span className="val">{snap ? snap.tick.toLocaleString() : "—"}</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Day / hour</span>
            <span className="val">{clock}{snap?.night ? " ·  night" : ""}</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Alive</span>
            <span className="val quick">{snap?.population ?? 0}</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Buried</span>
            <span className="val still">{snap?.died ?? 0}</span>
          </div>
          {/* The stated goal of the game is lineage depth, so it goes where the
              population count goes rather than three panels down. */}
          <div className="readout">
            <span className="eyebrow">Generation</span>
            <span className="val quick">{snap?.deepest_generation ?? 1}</span>
          </div>
        </div>
      </header>

      <div className="body" style={{ gridTemplateColumns: "228px 1fr 330px" }}>
        {/* ------------------------------------------------------ left rail */}
        <aside className="panel">
          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Run</span></div>
            {error && <div className="readout"><span className="val err">{error}</span></div>}
            <div className="btn-row" style={{ marginBottom: 10 }}>
              <button
                className="btn"
                aria-pressed={snap?.running ?? false}
                style={{ flex: 1, textAlign: "center" }}
                onClick={() => void simControl({ kind: snap?.running ? "pause" : "play" })}
              >
                {snap?.running ? "Pause" : "Play"}
              </button>
              <button
                className="btn"
                style={{ flex: 1, textAlign: "center" }}
                onClick={() => void simControl({ kind: "step", ticks: 1 })}
              >
                Step 1
              </button>
            </div>
            <div className="stack">
              {MODES.map((m) => (
                <button
                  key={m.id}
                  className="btn mode"
                  aria-pressed={snap?.mode === m.id}
                  onClick={() => void simControl({ kind: "set_mode", mode: m.id })}
                >
                  <span>{m.name}</span>
                  <span className="sub">{m.sub}</span>
                </button>
              ))}
            </div>
            <p className="hint" style={{ marginTop: 8 }}>
              Space play/pause · . step · K knowledge
            </p>
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">This tick</span></div>
            <dl className="kv">
              <dt>Tick time</dt>
              <dd className={snap && snap.tick_ms > 50 ? "val err" : "num"}>
                {snap ? `${snap.tick_ms.toFixed(2)} ms` : "—"}
              </dd>
              <dt>Rate</dt>
              <dd className="num">{snap ? `${snap.ticks_per_second.toFixed(0)}/s` : "—"}</dd>
              <dt>Decisions</dt><dd className="num">{snap?.timings ? "tier 1" : "—"}</dd>
            </dl>
            {/* "Which phase" is the first question every time a tick is slow,
                so the breakdown is here rather than only in tick_stats. */}
            {snap && (
              <dl className="kv" style={{ marginTop: 8 }}>
                {([
                  ["1 world", snap.timings.world],
                  ["2 needs", snap.timings.needs],
                  ["3 plans", snap.timings.plans],
                  ["4 think", snap.timings.deliberate],
                  ["5 act", snap.timings.act],
                  ["6 resolve", snap.timings.resolve],
                  ["7 persist", snap.timings.persist],
                ] as const).map(([label, us]) => (
                  <FragmentTiming key={label} label={label} us={us} />
                ))}
              </dl>
            )}
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Overlays</span></div>
            {([
              ["creatures", "Creatures"],
              ["nodes", "Resource patches"],
              ["structures", "Shelters and fires"],
              ["plans", "Committed plan path"],
              ["knowledge", "What is known"],
            ] as const).map(([k, label]) => (
              <label className="toggle" key={k}>
                <input
                  type="checkbox"
                  checked={overlays[k]}
                  onChange={(e) => setOverlays((o) => ({ ...o, [k]: e.target.checked }))}
                />
                {" "}{label}
                {k === "creatures" && (
                  <span className="swatch" style={{ background: "var(--quick)" }} />
                )}
              </label>
            ))}
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Population</span></div>
            <dl className="kv">
              <dt>Infants</dt><dd className="num">{snap?.infants ?? 0}</dd>
              <dt>Adults</dt><dd className="num">{snap?.adults ?? 0}</dd>
              <dt>Elders</dt><dd className="num">{snap?.elders ?? 0}</dd>
              <dt>Shelters</dt><dd className="num">{snap?.shelters ?? 0}</dd>
              <dt>Fires lit</dt><dd className="num">{snap?.fires_lit ?? 0}</dd>
            </dl>
            <dl className="kv" style={{ marginTop: 8 }}>
              <dt>Paired</dt><dd className="num">{snap?.paired ?? 0}</dd>
              <dt>Expecting</dt>
              <dd className="num quick">{snap?.expecting ?? 0}</dd>
              <dt>Households</dt><dd className="num">{snap?.households ?? 0}</dd>
              {/* Only grain keeps, so only a household above the reserve can
                  have a child. This is the number that predicts births. */}
              <dt>At reserve</dt>
              <dd className="num">
                {snap?.households_at_reserve ?? 0}
                <span className="dim"> · mean store {(snap?.mean_store ?? 0).toFixed(0)}</span>
              </dd>
              <dt>Taught</dt>
              <dd className="num">
                {snap?.beliefs_taught ?? 0}
                <span className="dim"> · told {snap?.beliefs_shared ?? 0}</span>
              </dd>
            </dl>
            {snap?.population_maintained && (
              /* Honesty about the fixture: reproduction is M4, so this census
                 is held rather than sustained, and a reader must not mistake
                 the two. */
              <p className="hint" style={{ marginTop: 8 }}>
                Census held by the M2 fixture — settlers replace the dead.
                Reproduction lands at M4.
              </p>
            )}
          </div>

          <div className="sec" style={{ borderBottom: 0 }}>
            <div className="sec-head">
              <span className="eyebrow">Cause of death</span>
              <span className="num" style={{ fontSize: 12 }}>{totalDeaths}</span>
            </div>
            {totalDeaths === 0 ? (
              <p className="hint">Nobody has died yet.</p>
            ) : (
              <div className="needs">
                {snap!.deaths_by_cause.map(([cause, n]) => (
                  <div className="need" key={cause}>
                    <span className="lbl">{cause.replace("_", " ").toLowerCase()}</span>
                    <div className="track">
                      <div
                        className="fill"
                        style={{
                          width: `${(n / totalDeaths) * 100}%`,
                          background: "var(--c3)",
                        }}
                      />
                    </div>
                    <span className="v">{((n / totalDeaths) * 100).toFixed(0)}%</span>
                  </div>
                ))}
              </div>
            )}
            <div className="btn-row" style={{ gap: 6, marginTop: 12 }}>
              <input
                className="seed-input"
                value={seedInput}
                onChange={(e) => setSeedInput(e.target.value.replace(/[^0-9]/g, ""))}
                placeholder="seed"
                aria-label="World seed"
              />
              <input
                className="seed-input"
                style={{ width: 58 }}
                value={popInput}
                onChange={(e) => setPopInput(e.target.value.replace(/[^0-9]/g, ""))}
                placeholder="pop"
                aria-label="Starting population"
              />
            </div>
            <button
              className="btn"
              style={{ marginTop: 6, width: "100%" }}
              disabled={!seedInput}
              onClick={() => {
                select(null);
                void simControl({
                  kind: "regenerate",
                  seed: Number(seedInput),
                  creatures: Number(popInput) || 0,
                });
              }}
            >
              New world
            </button>
          </div>
        </aside>

        {/* ------------------------------------------------------------ map */}
        <main className="map-wrap">
          {meta && terrain ? (
            <MapView
              meta={meta}
              terrain={terrain}
              nodes={nodes}
              palette={palette}
              snapshot={snap}
              overlays={overlays}
              selectedId={selectedId}
              onSelect={select}
              onStats={onStats}
              benchFrames={benchFrames}
              onBench={onBench}
            />
          ) : (
            <div className="stage-empty">{error ?? "generating world…"}</div>
          )}

          <div className="map-legend">
            {overlays.knowledge ? (
              <>
                <div className="leg-group">Confidence</div>
                <div className="row">
                  <span className="sw" style={{ background: "var(--seq-5)" }} /> Verified recently
                </div>
                <div className="row">
                  <span className="sw" style={{ background: "var(--seq-3)" }} /> Getting old
                </div>
                <div className="row">
                  <span className="sw" style={{ background: "var(--seq-1)" }} /> Stale, may be wrong
                </div>
                <div className="row">
                  <span className="sw" style={{ background: "var(--void)", border: "1px solid var(--line)" }} />
                  {" "}Never seen
                </div>
              </>
            ) : (
              <>
                <div className="leg-group">Ground</div>
                {TERRAIN_NAMES.map((n, i) => (
                  <div className="row" key={n}>
                    <span className="sw" style={{ background: TERRAIN_SETS[palette][i] }} />
                    {" "}{n}
                  </div>
                ))}
                <div className="leg-group">Grows there</div>
                {(["WHEAT", "WOOD", "FORAGE"] as const).map((k) => (
                  <div className="row" key={k}>
                    <span className="sw patch" style={{ background: `var(--res-${k.toLowerCase()})` }} />
                    {" "}{k[0] + k.slice(1).toLowerCase()}
                    <span className="legend-n num"> {counts[k] ?? 0}</span>
                  </div>
                ))}
                <div className="leg-group">Moving</div>
                <div className="row"><span className="mk mk-adult" /> Adult</div>
                <div className="row"><span className="mk mk-infant" /> Infant</div>
                <div className="row"><span className="mk mk-elder" /> Elder</div>
                <div className="row"><span className="mk mk-sheep" /> Sheep ({counts.SHEEP ?? 0})</div>
              </>
            )}
          </div>

          <div className="scalebar">
            <span className="bar" />
            {stats ? `${stats.scale.toFixed(2)} px/tile · ${stats.fps.toFixed(0)} fps` : "—"}
            {bench && ` · measured ${bench.meanFps.toFixed(1)} fps mean, p95 ${bench.p95FrameMs.toFixed(2)}ms`}
          </div>
        </main>

        {/* ------------------------------------------------------ inspector */}
        <aside className="panel right">
          {snap?.selected && world ? (
            <Inspector d={snap.selected} config={world.config} />
          ) : (
            <div className="sec" style={{ borderBottom: 0 }}>
              <div className="sec-head"><span className="eyebrow">Inspector</span></div>
              <p className="hint">
                Click a creature to read its felt state, its committed plan and the
                reason it gave for it, and everything it believes about the world.
              </p>
            </div>
          )}
        </aside>
      </div>

      {/* --------------------------------------------------------- ticker */}
      <footer className="ticker">
        <span className="ticker-label eyebrow">Events</span>
        <div className="ticker-stream live">
          {(snap?.events ?? []).map((e, i) => (
            <span className={`ev ${e.tone}`} key={`${e.tick}-${i}`}>
              <span className="t">{e.tick}</span>{e.text}
            </span>
          ))}
        </div>
      </footer>
    </div>
  );
}

function FragmentTiming({ label, us }: { label: string; us: number }) {
  const ms = us / 1000;
  return (
    <>
      <dt>{label}</dt>
      <dd className="num" style={{ color: ms > 20 ? "var(--st-warn)" : undefined }}>
        {ms < 0.01 ? "—" : `${ms.toFixed(2)} ms`}
      </dd>
    </>
  );
}
