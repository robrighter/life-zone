//! Deterministic value noise.
//!
//! Hand-rolled rather than pulled from a crate so the output is guaranteed
//! stable across platforms and dependency updates — worldgen determinism is
//! invariant 7, and a silent change to a noise implementation would break the
//! golden-run test in a way that is very hard to diagnose.
//!
//! This is the same construction as `octaves()` in design/mockups/mock.js:
//! bilinear-interpolated value noise with a smoothstep fade, summed over three
//! octaves. Cell counts scale with map width so feature size in *tiles* stays
//! constant regardless of map size.

use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;

/// One octave: a coarse grid of random values, bilinearly interpolated up to
/// the full map with a smoothstep fade.
fn field(w: usize, h: usize, cells: usize, seed: u64) -> Vec<f32> {
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let (gw, gh) = (cells + 2, cells + 2);
    let grid: Vec<f32> = (0..gw * gh).map(|_| rng.gen::<f32>()).collect();

    let mut out = vec![0.0f32; w * h];
    let (sx, sy) = (cells as f32 / w as f32, cells as f32 / h as f32);
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);

    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 * sx, y as f32 * sy);
            let (x0, y0) = (fx as usize, fy as usize);
            let (tx, ty) = (smooth(fx - x0 as f32), smooth(fy - y0 as f32));

            let a = grid[y0 * gw + x0];
            let b = grid[y0 * gw + x0 + 1];
            let c = grid[(y0 + 1) * gw + x0];
            let d = grid[(y0 + 1) * gw + x0 + 1];

            out[y * w + x] = (a + (b - a) * tx) * (1.0 - ty) + (c + (d - c) * tx) * ty;
        }
    }
    out
}

/// Three octaves summed at 0.58 / 0.29 / 0.13. Output is in roughly 0..1.
pub fn octaves(w: usize, h: usize, seed: u64) -> Vec<f32> {
    // Ratios chosen so a 256-wide map reproduces the mockup's 4 / 9 / 20.
    let c1 = (w / 64).max(2);
    let c2 = (w * 9 / 256).max(3);
    let c3 = (w * 20 / 256).max(4);

    let a = field(w, h, c1, seed);
    let b = field(w, h, c2, seed.wrapping_add(7717));
    let c = field(w, h, c3, seed.wrapping_add(3391));

    (0..w * h)
        .map(|i| a[i] * 0.58 + b[i] * 0.29 + c[i] * 0.13)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_deterministic_for_a_seed() {
        let a = octaves(128, 128, 44127);
        let b = octaves(128, 128, 44127);
        assert_eq!(a, b, "same seed must produce identical noise");
    }

    #[test]
    fn different_seeds_differ() {
        let a = octaves(128, 128, 1);
        let b = octaves(128, 128, 2);
        assert_ne!(a, b);
    }

    #[test]
    fn stays_in_unit_range() {
        let f = octaves(128, 128, 9);
        for v in f {
            assert!((0.0..=1.0).contains(&v), "noise out of range: {v}");
        }
    }

    #[test]
    fn is_continuous_rather_than_white_noise() {
        // Adjacent tiles should be close; this is what makes coherent terrain.
        let w = 128;
        let f = octaves(w, w, 3);
        let mut jumps = 0;
        for y in 0..w {
            for x in 1..w {
                if (f[y * w + x] - f[y * w + x - 1]).abs() > 0.1 { jumps += 1; }
            }
        }
        assert!(jumps < 40, "noise is not smooth: {jumps} large jumps");
    }
}
