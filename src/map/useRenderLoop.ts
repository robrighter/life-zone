import { useEffect, useRef, useState } from "react";

/**
 * The render loop and its frame-time counter.
 *
 * M1's exit criterion is >=30 FPS with a counter proving it, so the counter is
 * here from M0 rather than bolted on later. `draw` is held in a ref so a new
 * closure each render does not restart the loop.
 */
export function useRenderLoop(
  draw: (ctx: CanvasRenderingContext2D, w: number, h: number) => void,
) {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const drawRef = useRef(draw);
  drawRef.current = draw;

  const [fps, setFps] = useState(0);
  const [frameMs, setFrameMs] = useState(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const ctx = canvas.getContext("2d");
    if (!ctx) return;

    let raf = 0;
    let frames = 0;
    let acc = 0;
    let windowStart = performance.now();

    const resize = () => {
      const dpr = window.devicePixelRatio || 1;
      const r = canvas.getBoundingClientRect();
      canvas.width = Math.max(1, Math.round(r.width * dpr));
      canvas.height = Math.max(1, Math.round(r.height * dpr));
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    };
    resize();
    const ro = new ResizeObserver(resize);
    ro.observe(canvas);

    const frame = () => {
      const t0 = performance.now();
      const dpr = window.devicePixelRatio || 1;
      drawRef.current(ctx, canvas.width / dpr, canvas.height / dpr);
      const t1 = performance.now();

      acc += t1 - t0;
      frames++;
      // Report over a ~500ms window so the number is readable, not jittery.
      if (t1 - windowStart >= 500) {
        setFps((frames * 1000) / (t1 - windowStart));
        setFrameMs(acc / frames);
        frames = 0; acc = 0; windowStart = t1;
      }
      raf = requestAnimationFrame(frame);
    };
    raf = requestAnimationFrame(frame);

    return () => { cancelAnimationFrame(raf); ro.disconnect(); };
  }, []);

  return { canvasRef, fps, frameMs };
}
