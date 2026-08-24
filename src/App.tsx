import { useCallback, useEffect, useState } from "react";
import { getWorld, type WorldSummary } from "./ipc";
import { useRenderLoop } from "./map/useRenderLoop";
import { PALETTES, usePalette } from "./ui/usePalette";

function cssVar(name: string, fallback: string) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim() || fallback;
}

export default function App() {
  const { palette, setPalette } = usePalette();
  const [world, setWorld] = useState<WorldSummary | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    getWorld().then(setWorld).catch((e) => setError(String(e)));
  }, []);

  // M0: the loop runs and is instrumented, but there is nothing to draw yet.
  // Terrain, resource patches and creatures land at M1.
  const draw = useCallback((ctx: CanvasRenderingContext2D, w: number, h: number) => {
    ctx.fillStyle = cssVar("--void", "#0A1012");
    ctx.fillRect(0, 0, w, h);

    ctx.strokeStyle = cssVar("--line-soft", "#1A272A");
    ctx.lineWidth = 1;
    ctx.beginPath();
    for (let x = 0.5; x < w; x += 32) { ctx.moveTo(x, 0); ctx.lineTo(x, h); }
    for (let y = 0.5; y < h; y += 32) { ctx.moveTo(0, y); ctx.lineTo(w, y); }
    ctx.stroke();

    ctx.fillStyle = cssVar("--ink-3", "#677A79");
    ctx.font = `12px ${cssVar("--mono", "monospace")}`;
    ctx.fillText("NO WORLD GENERATED — M1", 18, 28);
  }, []);

  const { canvasRef, fps, frameMs } = useRenderLoop(draw);

  // The map renderer sizes its chunk cache off the viewport, so the live value
  // is worth surfacing rather than inferring from the window config.
  const [vp, setVp] = useState({ w: window.innerWidth, h: window.innerHeight,
                                 dpr: window.devicePixelRatio });
  useEffect(() => {
    const on = () => setVp({ w: window.innerWidth, h: window.innerHeight,
                             dpr: window.devicePixelRatio });
    window.addEventListener("resize", on);
    return () => window.removeEventListener("resize", on);
  }, []);

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
            {error && (
              <div className="readout"><span className="val err">{error}</span></div>
            )}
            {!world && !error && (
              <div className="readout"><span className="val">loading…</span></div>
            )}
            {world && (
              <dl className="kv">
                <dt>Name</dt><dd>{world.name}</dd>
                <dt>Id</dt><dd>#{world.id}</dd>
                <dt>Seed</dt><dd>{world.seed}</dd>
                <dt>Size</dt><dd>{world.config.map.width}×{world.config.map.height}</dd>
                <dt>Status</dt><dd>{world.status}</dd>
              </dl>
            )}
          </div>

          {world && (
            <div className="sec">
              <div className="sec-head"><span className="eyebrow">Deliberation</span></div>
              <dl className="kv">
                <dt>Model</dt><dd>{world.config.llm.model}</dd>
                <dt>Tier 2</dt>
                <dd>{world.config.features.llm ? "enabled" : "off · tier 1 only"}</dd>
                <dt>Budget</dt><dd>{world.config.deliberation.budget_observe} / tick</dd>
              </dl>
            </div>
          )}

          <div className="sec">
            <div className="sec-head"><span className="eyebrow">Render</span></div>
            <dl className="kv">
              <dt>Fps</dt><dd>{fps.toFixed(0)}</dd>
              <dt>Frame</dt><dd>{frameMs.toFixed(2)} ms</dd>
              <dt>Viewport</dt><dd>{vp.w}×{vp.h} @{vp.dpr}x</dd>
            </dl>
          </div>
        </aside>

        <main className="stage">
          <canvas ref={canvasRef} className="map-canvas" />
        </main>
      </div>
    </div>
  );
}
