//! Seeded world generation (PRD §8).
//!
//! Invariant 7: deterministic given a seed. Everything here draws from a single
//! ChaCha8 stream in a fixed order, and iteration order is never dependent on a
//! hash map. `World::fingerprint` is the check.

use super::terrain::Terrain;
use super::world::{Founder, NodeKind, ResourceNode, World};
use crate::config::WorldConfig;
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha8Rng;
use std::collections::VecDeque;

/// How far from fresh water a tile may be and still be farmable. Soil is
/// deliberately scarce and contested (§8.4).
const SOIL_WATER_REACH: u32 = 6;
/// A start is viable only if founders can reach water and forage within this
/// many tiles of travel.
const VIABLE_WATER_DIST: u32 = 22;
const VIABLE_FORAGE_DIST: u32 = 26;
/// Distinct fresh-water bodies a world must have to be worth playing (§8.2).
const MIN_WATER_BODIES: usize = 3;
const MIN_WATER_BODY_TILES: usize = 40;

#[derive(Debug)]
pub struct GenOutcome {
    pub world: World,
    /// How many seeds were rejected by the viability check before this one.
    pub rejected: u32,
}

/// Generate a viable world. If the requested seed produces an unplayable world,
/// derive a new one and retry rather than handing back a world that is
/// unwinnable from tick 0.
pub fn generate(seed: u64, config: &WorldConfig) -> GenOutcome {
    let mut attempt_seed = seed;
    for rejected in 0..24 {
        let world = generate_once(attempt_seed, config);
        if let Err(reason) = check_viability(&world) {
            tracing::warn!(seed = attempt_seed, %reason, "rejected seed, regenerating");
            attempt_seed = attempt_seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            continue;
        }
        tracing::info!(
            seed = attempt_seed, rejected,
            nodes = world.nodes.len(), founders = world.founders.len(),
            "world generated"
        );
        return GenOutcome { world, rejected };
    }
    // Every seed in the chain failed, which means the thresholds are wrong
    // rather than the seed. Hand back the last one rather than looping forever.
    tracing::error!("no viable world after 24 attempts; returning last");
    GenOutcome { world: generate_once(attempt_seed, config), rejected: 24 }
}

fn generate_once(seed: u64, config: &WorldConfig) -> World {
    let (w, h) = (config.map.width as usize, config.map.height as usize);

    // 1. Elevation and moisture.
    let elev = super::noise::octaves(w, h, seed);
    let moist = super::noise::octaves(w, h, seed.wrapping_add(90210));

    // 2. Biome classification. Thresholds carried over from the mockup, which
    //    is the visual reference the map is matched against.
    let mut tiles = vec![Terrain::Grass; w * h];
    for i in 0..w * h {
        let (e, m) = (elev[i], moist[i]);
        tiles[i] = if e < 0.34 {
            Terrain::DeepWater
        } else if e < 0.41 {
            Terrain::ShallowWater
        } else if e < 0.435 {
            Terrain::Sand
        } else if e > 0.755 {
            Terrain::Hill // hills stay scarce, per §4.3
        } else if m > 0.60 {
            Terrain::Forest
        } else if m > 0.47 && e < 0.53 {
            Terrain::Soil
        } else {
            Terrain::Grass
        };
    }

    let mut world = World {
        width: config.map.width,
        height: config.map.height,
        chunk_size: config.map.chunk_size,
        seed,
        tiles,
        nodes: Vec::new(),
        founders: Vec::new(),
    };

    // 4. Farmable land must border water. Soil out of reach reverts to grass,
    //    which is what makes farming a contested position rather than a default.
    restrict_soil_to_water(&mut world);

    let mut rng = ChaCha8Rng::seed_from_u64(seed.wrapping_add(555));

    // 3 & 5. Resource nodes on terrain that suits them.
    place_nodes(&mut world, config, &mut rng);

    // 6. Founders near a viable start.
    place_founders(&mut world, config, &mut rng);

    world
}

/// Distance in tiles to the nearest tile satisfying `pred`, by multi-source BFS.
/// Returns `u32::MAX` where unreachable. Used for both soil placement and the
/// viability check, so they agree on what "within reach" means.
fn distance_field(world: &World, pred: impl Fn(Terrain) -> bool) -> Vec<u32> {
    let (w, h) = (world.width as usize, world.height as usize);
    let mut dist = vec![u32::MAX; w * h];
    let mut q = VecDeque::new();

    for y in 0..world.height {
        for x in 0..world.width {
            let i = world.idx(x, y);
            if pred(world.tiles[i]) {
                dist[i] = 0;
                q.push_back((x, y));
            }
        }
    }

    while let Some((x, y)) = q.pop_front() {
        let d = dist[world.idx(x, y)];
        for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if !world.in_bounds(nx, ny) {
                continue;
            }
            let (nx, ny) = (nx as u32, ny as u32);
            let ni = world.idx(nx, ny);
            // Travel is over land; deep water blocks.
            if !world.tiles[ni].passable() || dist[ni] != u32::MAX {
                continue;
            }
            dist[ni] = d + 1;
            q.push_back((nx, ny));
        }
    }
    dist
}

fn restrict_soil_to_water(world: &mut World) {
    let near_water = distance_field(world, |t| t.is_fresh_water());
    for (tile, &d) in world.tiles.iter_mut().zip(near_water.iter()) {
        if *tile == Terrain::Soil && d > SOIL_WATER_REACH {
            *tile = Terrain::Grass;
        }
    }
}

fn place_nodes(world: &mut World, config: &WorldConfig, rng: &mut ChaCha8Rng) {
    let r = &config.resources;

    // Collect eligible tiles per kind first, so densities mean "this fraction of
    // the terrain that suits it" rather than "this fraction of the whole map".
    // Scattering by whole-map density puts node counts at the mercy of how much
    // water a seed happens to generate.
    let (mut forest, mut soil, mut grass) = (Vec::new(), Vec::new(), Vec::new());
    for y in 0..world.height {
        for x in 0..world.width {
            match world.at(x, y) {
                Terrain::Forest => forest.push((x, y)),
                Terrain::Soil => soil.push((x, y)),
                Terrain::Grass => grass.push((x, y)),
                _ => {}
            }
        }
    }

    let scatter = |kind: NodeKind, pool: &[(u32, u32)], density: f32,
                       max: f32, regen: f32, out: &mut Vec<ResourceNode>,
                       rng: &mut ChaCha8Rng| {
        let n = (pool.len() as f32 * density) as usize;
        for _ in 0..n {
            let (x, y) = pool[rng.gen_range(0..pool.len())];
            let q: f32 = 0.25 + rng.gen::<f32>() * 0.75;
            out.push(ResourceNode {
                kind, x, y, quantity: max * q, max_quantity: max, regen_rate: regen,
            });
        }
    };

    let mut nodes = Vec::new();
    if !forest.is_empty() {
        scatter(NodeKind::Wood, &forest, r.wood_density, 40.0,
                r.wood_regen_per_tick, &mut nodes, rng);
        scatter(NodeKind::Forage, &forest, r.forage_density, 12.0,
                r.forage_regen_per_tick, &mut nodes, rng);
    }
    if !soil.is_empty() && config.features.wheat {
        scatter(NodeKind::Wheat, &soil, r.soil_density, 24.0, 0.0, &mut nodes, rng);
    }

    // Sheep come in flocks, not as a per-tile sprinkle. A sheep on every third
    // grass tile reads as visual noise across the whole map and makes herding
    // meaningless — the point of a flock is that it is somewhere in particular.
    if config.features.sheep && !grass.is_empty() {
        for _ in 0..r.sheep_flocks {
            let (hx, hy) = grass[rng.gen_range(0..grass.len())];
            let size = rng.gen_range(3..=6);
            for _ in 0..size {
                let dx = rng.gen_range(-3i64..=3);
                let dy = rng.gen_range(-3i64..=3);
                let (x, y) = (hx as i64 + dx, hy as i64 + dy);
                if !world.in_bounds(x, y) { continue; }
                let (x, y) = (x as u32, y as u32);
                if world.at(x, y) != Terrain::Grass { continue; }
                nodes.push(ResourceNode {
                    kind: NodeKind::Sheep, x, y,
                    quantity: 1.0, max_quantity: 6.0, regen_rate: 0.0,
                });
            }
        }
    }

    world.nodes = nodes;
}


/// Score every candidate tile by how survivable a start there is, then drop the
/// founders around the best one. Placement is clustered on purpose: eight
/// founders scattered across a 512-tile map would never meet.
fn place_founders(world: &mut World, config: &WorldConfig, rng: &mut ChaCha8Rng) {
    let water = distance_field(world, |t| t.is_fresh_water());
    let forage: Vec<u32> = {
        let mut f = vec![u32::MAX; world.tiles.len()];
        let mut q = VecDeque::new();
        for n in &world.nodes {
            if n.kind == NodeKind::Forage {
                let i = world.idx(n.x, n.y);
                f[i] = 0;
                q.push_back((n.x, n.y));
            }
        }
        while let Some((x, y)) = q.pop_front() {
            let d = f[world.idx(x, y)];
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if !world.in_bounds(nx, ny) { continue; }
                let (nx, ny) = (nx as u32, ny as u32);
                let ni = world.idx(nx, ny);
                if !world.tiles[ni].passable() || f[ni] != u32::MAX { continue; }
                f[ni] = d + 1;
                q.push_back((nx, ny));
            }
        }
        f
    };
    let soil = distance_field(world, |t| t == Terrain::Soil);

    let mut best: Option<(u32, u32, u32)> = None; // (score, x, y) - lower is better
    for y in 0..world.height {
        for x in 0..world.width {
            let i = world.idx(x, y);
            if !world.tiles[i].passable() || world.tiles[i].is_water() {
                continue;
            }
            let (dw, df, ds) = (water[i], forage[i], soil[i]);
            if dw > VIABLE_WATER_DIST || df > VIABLE_FORAGE_DIST {
                continue;
            }
            // Water matters most, then forage, then soil within reach.
            let score = dw * 3 + df * 2 + ds.min(60);
            if best.is_none_or(|(b, _, _)| score < b) {
                best = Some((score, x, y));
            }
        }
    }

    let Some((_, hx, hy)) = best else { return };

    let n = config.map.founder_count;
    for k in 0..n {
        // Spiral outward from the hearth until a passable land tile is free.
        let mut placed = false;
        for radius in 0..24u32 {
            for _ in 0..12 {
                let dx = rng.gen_range(-(radius as i64 + 1)..=(radius as i64 + 1));
                let dy = rng.gen_range(-(radius as i64 + 1)..=(radius as i64 + 1));
                let (x, y) = (hx as i64 + dx, hy as i64 + dy);
                if !world.in_bounds(x, y) { continue; }
                let (x, y) = (x as u32, y as u32);
                let t = world.at(x, y);
                if !t.passable() || t.is_water() { continue; }
                world.founders.push(Founder { x, y, female: k % 2 == 0 });
                placed = true;
                break;
            }
            if placed { break; }
        }
    }
}

/// Reject worlds the founders cannot survive from tick 0 (§8).
fn check_viability(world: &World) -> Result<(), String> {
    if world.founders.is_empty() {
        return Err("no viable founder placement".into());
    }
    if world.founders.len() < 2 {
        return Err(format!("only {} founders placed", world.founders.len()));
    }

    let bodies = count_water_bodies(world);
    if bodies < MIN_WATER_BODIES {
        return Err(format!("only {bodies} distinct fresh water bodies"));
    }

    let water = distance_field(world, |t| t.is_fresh_water());
    let has_forage = world.nodes.iter().any(|n| n.kind == NodeKind::Forage);
    if !has_forage {
        return Err("no forage nodes".into());
    }
    let has_soil = world.tiles.contains(&Terrain::Soil);
    if !has_soil {
        return Err("no farmable soil".into());
    }

    for f in &world.founders {
        let i = world.idx(f.x, f.y);
        if water[i] > VIABLE_WATER_DIST {
            return Err(format!("founder at {},{} is {} tiles from water", f.x, f.y, water[i]));
        }
    }
    Ok(())
}

/// Flood-fill distinct bodies of shallow water, ignoring puddles.
fn count_water_bodies(world: &World) -> usize {
    let mut seen = vec![false; world.tiles.len()];
    let mut bodies = 0;

    for y in 0..world.height {
        for x in 0..world.width {
            let start = world.idx(x, y);
            if seen[start] || !world.tiles[start].is_fresh_water() {
                continue;
            }
            let mut size = 0;
            let mut q = VecDeque::from([(x, y)]);
            seen[start] = true;
            while let Some((cx, cy)) = q.pop_front() {
                size += 1;
                for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                    let (nx, ny) = (cx as i64 + dx, cy as i64 + dy);
                    if !world.in_bounds(nx, ny) { continue; }
                    let (nx, ny) = (nx as u32, ny as u32);
                    let ni = world.idx(nx, ny);
                    if seen[ni] || !world.tiles[ni].is_fresh_water() { continue; }
                    seen[ni] = true;
                    q.push_back((nx, ny));
                }
            }
            if size >= MIN_WATER_BODY_TILES {
                bodies += 1;
            }
        }
    }
    bodies
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(size: u32) -> WorldConfig {
        let mut c = WorldConfig::default();
        c.map.width = size;
        c.map.height = size;
        c
    }

    /// Invariant 7 and half of M1's exit criterion.
    #[test]
    fn same_seed_produces_an_identical_world() {
        let c = cfg(256);
        let a = generate(44127, &c).world;
        let b = generate(44127, &c).world;

        assert_eq!(a.fingerprint(), b.fingerprint(), "same seed must be identical");
        assert_eq!(a.terrain_bytes(), b.terrain_bytes(), "terrain must be byte-identical");
        assert_eq!(a.nodes.len(), b.nodes.len());
        assert_eq!(a.founders.len(), b.founders.len());
    }

    #[test]
    fn different_seeds_produce_different_worlds() {
        let c = cfg(256);
        let a = generate(1, &c).world;
        let b = generate(2, &c).world;
        assert_ne!(a.fingerprint(), b.fingerprint());
    }

    #[test]
    fn a_generated_world_is_viable() {
        let c = cfg(256);
        for seed in [1u64, 44127, 90210, 7] {
            let out = generate(seed, &c);
            check_viability(&out.world)
                .unwrap_or_else(|e| panic!("seed {seed} produced an unviable world: {e}"));
        }
    }

    #[test]
    fn every_terrain_type_appears() {
        let w = generate(44127, &cfg(256)).world;
        for t in [Terrain::DeepWater, Terrain::ShallowWater, Terrain::Grass,
                  Terrain::Forest, Terrain::Soil] {
            assert!(w.tiles.contains(&t), "{t:?} never generated");
        }
    }

    #[test]
    fn soil_only_survives_near_water() {
        let w = generate(44127, &cfg(256)).world;
        let near = distance_field(&w, |t| t.is_fresh_water());
        for (i, (tile, &d)) in w.tiles.iter().zip(near.iter()).enumerate() {
            if *tile == Terrain::Soil {
                assert!(d <= SOIL_WATER_REACH, "soil at index {i} is {d} tiles from water");
            }
        }
    }

    #[test]
    fn nodes_sit_on_terrain_that_suits_them() {
        let w = generate(44127, &cfg(256)).world;
        assert!(!w.nodes.is_empty());
        for n in &w.nodes {
            let t = w.at(n.x, n.y);
            let ok = match n.kind {
                NodeKind::Wood | NodeKind::Forage => t == Terrain::Forest,
                NodeKind::Wheat => t == Terrain::Soil,
                NodeKind::Sheep => t == Terrain::Grass,
            };
            assert!(ok, "{:?} node on {:?}", n.kind, t);
            assert!(n.quantity <= n.max_quantity);
        }
    }

    #[test]
    fn founders_land_on_passable_dry_ground_near_each_other() {
        let c = cfg(256);
        let w = generate(44127, &c).world;
        assert_eq!(w.founders.len(), c.map.founder_count as usize);

        for f in &w.founders {
            let t = w.at(f.x, f.y);
            assert!(t.passable() && !t.is_water(), "founder on {t:?}");
        }
        // Clustered: eight founders scattered across the map would never meet.
        let (x0, y0) = (w.founders[0].x as i64, w.founders[0].y as i64);
        for f in &w.founders {
            let d = (f.x as i64 - x0).abs() + (f.y as i64 - y0).abs();
            assert!(d < 80, "founder {d} tiles from the first one");
        }
        // Mixed sex, or there is no second generation.
        assert!(w.founders.iter().any(|f| f.female));
        assert!(w.founders.iter().any(|f| !f.female));
    }

    #[test]
    fn wheat_toggle_removes_wheat_for_the_s4_experiment() {
        let mut c = cfg(256);
        c.features.wheat = false;
        let w = generate(44127, &c).world;
        assert!(!w.nodes.iter().any(|n| n.kind == NodeKind::Wheat));
    }

    #[test]
    fn chunk_blobs_tile_the_whole_map() {
        let w = generate(44127, &cfg(256)).world;
        assert_eq!(w.chunks_x(), 8);
        assert_eq!(w.chunks_y(), 8);

        for cy in 0..w.chunks_y() {
            for cx in 0..w.chunks_x() {
                let blob = w.chunk_blob(cx, cy);
                assert_eq!(blob.len(), (w.chunk_size * w.chunk_size) as usize);
                // Spot-check the corner tile matches the grid it came from.
                let expected = w.at(cx * w.chunk_size, cy * w.chunk_size) as u8;
                assert_eq!(blob[0], expected);
            }
        }
    }

    #[test]
    fn a_full_size_world_is_deterministic_and_viable() {
        // The actual M1 target: 512x512.
        let c = WorldConfig::default();
        let a = generate(44127, &c);
        let b = generate(44127, &c);
        assert_eq!(a.world.fingerprint(), b.world.fingerprint());
        assert_eq!(a.world.tiles.len(), 512 * 512);
        check_viability(&a.world).expect("default seed must be viable");
    }
}
