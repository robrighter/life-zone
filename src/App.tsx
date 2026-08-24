import { useCallback, useEffect, useState } from "react";
import {
  benchMode, getTerrain, getWorld, getWorldMeta, regenerateWorld, reportBench,
  type WorldMeta, type WorldSummary,
} from "./ipc";
import { MapView, type BenchResult, type MapStats } from "./map/MapView";
import { TERRAIN_NAMES, TERRAIN_SETS } from "./map/palette";
import { PALETTES, usePalette } from "./ui/usePalette";

export default function App() {
  const { palette, setPalette } = usePalette();
  const [world, setWorld] = useState<WorldSummary | null>(null);
  const [meta, setMeta] = useState<WorldMeta | null>(null);
  const [terrain, setTerrain] = useState<Uint8Array | null>(null);
  const [stats, setStats] = useState<MapStats | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [seedInput, setSeedInput] = useState("");
  const [benchFrames, setBenchFrames] = useState<number | null>(null);
  const [bench, setBench] = useState<BenchResult | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const [w, m, t] = await Promise.all([getWorld(), getWorldMeta(), getTerrain()]);
        setWorld(w); setMeta(m); setTerrain(t);
        setSeedInput(String(w.seed));
      } catch (e) {
        setError(String(e));
      }
    })();
  }, []);

  const regenerate = useCallback(async (seed: number) => {
    setBusy(true);
    setError(null);
    try {
      const m = await regenerateWorld(seed);
      const [w, t] = await Promise.all([getWorld(), getTerrain()]);
      setMeta(m); setWorld(w); setTerrain(t);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const onStats = useCallback((s: MapStats) => setStats(s), []);

  // 'b' runs the pan benchmark, so the M1 frame-rate measurement can be driven
  // without a mouse.
  useEffect(() => {
    const on = (e: KeyboardEvent) => {
      if (e.key.toLowerCase() === "b" && !e.repeat) {
        setBench(null);
        setBenchFrames(600);
      }
    };
    window.addEventListener("keydown", on);
    return () => window.removeEventListener("keydown", on);
  }, []);
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

  const counts = meta
    ? meta.nodes.reduce<Record<string, number>>((a, n) => {
        a[n.kind] = (a[n.kind] ?? 0) + 1;
        return a;
      }, {})
    : {};

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
            <span className="val">{world ? world.current_tick.toLocaleString() : "—"}</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Alive</span>
            <span className="val quick">0</span>
          </div>
          <div className="readout">
            <span className="eyebrow">Buried</span>
            <span className="val still">0</span>
          </div>
        </div>
      </header>

      <div className="body body-world">
        <aside className="panel">
          <div className="sec">
            <div className="sec-head"><span className="eyebrow">World</span></div>
            {error && <div className="readout"><span className="val err">{error}</span></div>}
            {world && (
              <dl className="kv">
                <dt>Name</dt><dd>{world.name}</dd>
                <dt>Seed</dt><dd>{world.seed}</dd>
                <dt>Size</dt>
                <dd>{meta ? `${meta.width}×${meta.height}` : "—"}</dd>
                <dt>Chunks</dt>
                <dd>
                  {meta
                    ? `${Math.ceil(meta.width / meta.chunk_size)}×${Math.ceil(meta.height / meta.chunk_size)} @ ${meta.chunk_size}`
                    : "—"}
                </dd>
                <dt>Founders</dt><dd>{meta?.founders.length ?? 0}</dd>
              </dl>
            )}
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">New world</span></div>
            <div className="btn-row" style={{ gap: 6 }}>
              <input
                className="seed-input"
                value={seedInput}
                onChange={(e) => setSeedInput(e.target.value.replace(/[^0-9]/g, ""))}
                placeholder="seed"
                aria-label="World seed"
              />
              <button
                className="btn"
                disabled={busy || !seedInput}
                onClick={() => regenerate(Number(seedInput))}
              >
                {busy ? "…" : "Generate"}
              </button>
            </div>
            <button
              className="btn"
              style={{ marginTop: 6, width: "100%" }}
              disabled={busy}
              onClick={() => {
                const s = Math.floor(Math.random() * 1_000_000);
                setSeedInput(String(s));
                regenerate(s);
              }}
            >
              Random seed
            </button>
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Ground</span></div>
            {/* Swatches read the same table the canvas rasterises from, so the
                legend cannot drift away from the map. */}
            <div className="legend legend-col">
              {TERRAIN_NAMES.map((n, i) => (
                <div className="item" key={n}>
                  <span className="sw" style={{ background: TERRAIN_SETS[palette][i] }} />
                  <span>{n}</span>
                </div>
              ))}
            </div>
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Grows there</span></div>
            {/* Resource hues are CSS tokens, read by both legend and canvas. */}
            <div className="legend legend-col">
              {(["WHEAT", "WOOD", "FORAGE"] as const).map((k) => (
                <div className="item" key={k}>
                  <span className="sw" style={{ background: `var(--res-${k.toLowerCase()})` }} />
                  <span>{k[0] + k.slice(1).toLowerCase()}</span>
                  <span className="legend-n num">{counts[k] ?? 0}</span>
                </div>
              ))}
              <div className="item">
                <span className="sw sw-sheep" />
                <span>Sheep</span>
                <span className="legend-n num">{counts.SHEEP ?? 0}</span>
              </div>
            </div>
          </div>

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Render</span></div>
            <dl className="kv">
              <dt>Fps</dt>
              <dd className={stats && stats.fps < 30 ? "val err" : undefined}>
                {stats ? stats.fps.toFixed(0) : "—"}
              </dd>
              <dt>Frame</dt><dd>{stats ? `${stats.frameMs.toFixed(2)} ms` : "—"}</dd>
              <dt>Zoom</dt><dd>{stats ? `${stats.scale.toFixed(2)} px/tile` : "—"}</dd>
              <dt>Chunks</dt>
              <dd>{stats ? `${stats.chunksDrawn} drawn · ${stats.chunksCached} cached` : "—"}</dd>
            </dl>
            <p className="hint">Drag or WASD to pan · scroll to zoom</p>
            <button
              className="btn"
              style={{ marginTop: 8, width: "100%" }}
              disabled={!!benchFrames || !meta}
              onClick={() => { setBench(null); setBenchFrames(600); }}
            >
              {benchFrames ? "measuring…" : "Measure pan FPS"}
            </button>
            {bench && (
              <dl className="kv" style={{ marginTop: 8 }}>
                <dt>Mean</dt>
                <dd className={bench.meanFps < 30 ? "val err" : undefined}>
                  {bench.meanFps.toFixed(1)} fps
                </dd>
                <dt>p50</dt><dd>{bench.p50Fps.toFixed(1)} fps</dd>
                <dt>p95</dt><dd>{bench.p95FrameMs.toFixed(2)} ms</dd>
                <dt>Worst</dt><dd>{bench.worstFrameMs.toFixed(2)} ms</dd>
                <dt>Frames</dt><dd>{bench.frames} · {bench.chunksDrawn} chunks</dd>
              </dl>
            )}
          </div>
        </aside>

        <main className="stage">
          {meta && terrain ? (
            <MapView
              meta={meta} terrain={terrain} palette={palette}
              onStats={onStats} benchFrames={benchFrames} onBench={onBench}
            />
          ) : (
            <div className="stage-empty">{error ?? "generating world…"}</div>
          )}
        </main>
      </div>
    </div>
  );
}
