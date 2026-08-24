//! A* over the tile grid, cached (PRD §3.2).
//!
//! Two things make this affordable at 500 creatures:
//!
//! * **Scratch is reused across calls.** Clearing a 262,144-entry score array
//!   per search would cost more than the search. Instead every entry carries a
//!   generation stamp and is treated as unset unless the stamp matches the
//!   current search, so a search costs only what it actually visits.
//! * **Costs are integers.** Tile costs are held in 1/256ths, which keeps the
//!   open set orderable without wrapping floats, and — more importantly — makes
//!   the result bit-identical across runs and platforms. Float accumulation
//!   order would be a quiet source of divergence in the golden-run test.
//!
//! Searches are node-capped. A creature that asks for a path across the whole
//! map gets the best partial route found within the cap rather than stalling
//! the tick; walking most of the way and re-planning is both cheaper and a
//! better model of how a creature without a map actually travels.

use super::terrain::Terrain;
use super::world::World;
use std::collections::BinaryHeap;

/// Fixed-point scale for tile costs.
const SCALE: u32 = 256;
/// Diagonal step multiplier, sqrt(2) in the same fixed point.
const DIAG: u32 = 362;
/// Cheapest possible tile, used to keep the heuristic admissible.
const MIN_STEP: u32 = SCALE;

const NEIGHBOURS: [(i32, i32, bool); 8] = [
    (1, 0, false),
    (-1, 0, false),
    (0, 1, false),
    (0, -1, false),
    (1, 1, true),
    (1, -1, true),
    (-1, 1, true),
    (-1, -1, true),
];

#[inline]
fn step_cost(t: Terrain, diagonal: bool) -> u32 {
    let base = (t.move_cost() * SCALE as f32) as u32;
    if diagonal {
        (base as u64 * DIAG as u64 / SCALE as u64) as u32
    } else {
        base
    }
}

/// Total path cost of a route, in the same units a creature's speed is spent in.
pub fn path_cost(world: &World, path: &[(u32, u32)]) -> f32 {
    let mut total = 0.0;
    for w in path.windows(2) {
        let (ax, ay) = w[0];
        let (bx, by) = w[1];
        let diagonal = ax != bx && ay != by;
        total += step_cost(world.at(bx, by), diagonal) as f32 / SCALE as f32;
    }
    total
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Open {
    /// Negated for a max-heap so the cheapest comes out first; the tie-break on
    /// index keeps the expansion order total and therefore reproducible.
    f: u32,
    idx: u32,
}

impl Ord for Open {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.f.cmp(&self.f).then(other.idx.cmp(&self.idx))
    }
}
impl PartialOrd for Open {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct PathStats {
    pub searches: u64,
    pub cache_hits: u64,
    pub nodes_expanded: u64,
    pub capped: u64,
}

pub struct Pathfinder {
    width: u32,
    height: u32,
    g: Vec<u32>,
    came_from: Vec<u32>,
    stamp: Vec<u32>,
    generation: u32,
    open: BinaryHeap<Open>,
    /// Bounded (start, goal) cache. Eviction only decides whether a search is
    /// repeated, never what it returns, so it cannot affect determinism.
    cache: std::collections::HashMap<(u32, u32), Vec<(u32, u32)>>,
    cache_order: std::collections::VecDeque<(u32, u32)>,
    cache_capacity: usize,
    /// Cap on nodes expanded per search.
    node_cap: usize,
    pub stats: PathStats,
}

impl Pathfinder {
    pub fn new(world: &World) -> Self {
        let n = (world.width as usize) * (world.height as usize);
        Self {
            width: world.width,
            height: world.height,
            g: vec![0; n],
            came_from: vec![0; n],
            stamp: vec![0; n],
            generation: 0,
            open: BinaryHeap::with_capacity(1024),
            cache: std::collections::HashMap::with_capacity(2048),
            cache_order: std::collections::VecDeque::with_capacity(2048),
            cache_capacity: 2048,
            node_cap: 6_000,
            stats: PathStats::default(),
        }
    }

    #[inline]
    fn idx(&self, x: u32, y: u32) -> u32 {
        y * self.width + x
    }

    #[inline]
    fn xy(&self, i: u32) -> (u32, u32) {
        (i % self.width, i / self.width)
    }

    /// Admissible octile heuristic: the cheapest conceivable remaining cost.
    #[inline]
    fn heuristic(&self, i: u32, goal: u32) -> u32 {
        let (ax, ay) = self.xy(i);
        let (bx, by) = self.xy(goal);
        let dx = ax.abs_diff(bx);
        let dy = ay.abs_diff(by);
        let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
        // (hi - lo) straight steps plus lo diagonal ones, all at the cheapest rate.
        (hi - lo) * MIN_STEP + lo * DIAG
    }

    /// Invalidate the cache. Called when terrain changes; resource depletion
    /// does not affect routes, so it does not invalidate.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        self.cache_order.clear();
    }

    /// A path from `start` to `goal`, inclusive of both, or `None` if the goal
    /// is unreachable. The returned path excludes the start tile so it can be
    /// consumed one step per tick.
    pub fn find(
        &mut self,
        world: &World,
        start: (u32, u32),
        goal: (u32, u32),
    ) -> Option<Vec<(u32, u32)>> {
        if start == goal {
            return Some(Vec::new());
        }
        if !world.in_bounds(goal.0 as i64, goal.1 as i64) || !world.at(goal.0, goal.1).passable() {
            return None;
        }

        let key = (self.idx(start.0, start.1), self.idx(goal.0, goal.1));
        if let Some(hit) = self.cache.get(&key) {
            self.stats.cache_hits += 1;
            return Some(hit.clone());
        }

        let path = self.search(world, start, goal);

        if let Some(ref p) = path {
            if self.cache_order.len() >= self.cache_capacity {
                if let Some(old) = self.cache_order.pop_front() {
                    self.cache.remove(&old);
                }
            }
            self.cache.insert(key, p.clone());
            self.cache_order.push_back(key);
        }
        path
    }

    fn search(
        &mut self,
        world: &World,
        start: (u32, u32),
        goal: (u32, u32),
    ) -> Option<Vec<(u32, u32)>> {
        self.stats.searches += 1;
        self.generation = self.generation.wrapping_add(1);
        if self.generation == 0 {
            // Wrapped: the only moment the arrays genuinely need clearing.
            self.stamp.iter_mut().for_each(|s| *s = 0);
            self.generation = 1;
        }
        let gen = self.generation;
        self.open.clear();

        let start_i = self.idx(start.0, start.1);
        let goal_i = self.idx(goal.0, goal.1);

        self.g[start_i as usize] = 0;
        self.came_from[start_i as usize] = start_i;
        self.stamp[start_i as usize] = gen;
        self.open.push(Open { f: self.heuristic(start_i, goal_i), idx: start_i });

        let mut expanded = 0usize;
        // Best partial result, so a capped search still makes progress.
        let mut closest = (start_i, self.heuristic(start_i, goal_i));
        let mut found = false;
        let mut capped = false;

        while let Some(Open { f, idx }) = self.open.pop() {
            if idx == goal_i {
                found = true;
                break;
            }
            // Stale heap entry from a since-improved node.
            if self.stamp[idx as usize] != gen {
                continue;
            }
            let g_here = self.g[idx as usize];
            if f < g_here {
                continue;
            }

            expanded += 1;
            if expanded > self.node_cap {
                self.stats.capped += 1;
                capped = true;
                break;
            }

            let h = self.heuristic(idx, goal_i);
            if h < closest.1 {
                closest = (idx, h);
            }

            let (x, y) = self.xy(idx);
            for (dx, dy, diagonal) in NEIGHBOURS {
                let nx = x as i64 + dx as i64;
                let ny = y as i64 + dy as i64;
                if nx < 0 || ny < 0 || nx >= self.width as i64 || ny >= self.height as i64 {
                    continue;
                }
                let (nx, ny) = (nx as u32, ny as u32);
                let t = world.at(nx, ny);
                if !t.passable() {
                    continue;
                }
                // No cutting the corner of an impassable tile: a creature cannot
                // slip diagonally between two deep-water tiles.
                if diagonal
                    && (!world.at(x, ny).passable() || !world.at(nx, y).passable())
                {
                    continue;
                }

                let ni = self.idx(nx, ny);
                let tentative = g_here + step_cost(t, diagonal);
                let unset = self.stamp[ni as usize] != gen;
                if unset || tentative < self.g[ni as usize] {
                    self.stamp[ni as usize] = gen;
                    self.g[ni as usize] = tentative;
                    self.came_from[ni as usize] = idx;
                    self.open.push(Open { f: tentative + self.heuristic(ni, goal_i), idx: ni });
                }
            }
        }

        self.stats.nodes_expanded += expanded as u64;

        let end = if found {
            goal_i
        } else if capped && closest.0 != start_i {
            // The search ran out of budget rather than out of map: hand back the
            // best partial route and let the creature re-plan when it gets
            // there. An *exhausted* open set means the goal is genuinely
            // unreachable, and saying so is what stops a creature walking into
            // a wall for the rest of its life.
            closest.0
        } else {
            return None;
        };

        let mut path = Vec::new();
        let mut cur = end;
        while cur != start_i {
            let (cx, cy) = self.xy(cur);
            path.push((cx, cy));
            let prev = self.came_from[cur as usize];
            if prev == cur {
                break;
            }
            cur = prev;
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::world::World;

    fn flat_world(w: u32, h: u32, t: Terrain) -> World {
        World {
            width: w,
            height: h,
            chunk_size: 32,
            seed: 1,
            tiles: vec![t; (w * h) as usize],
            nodes: Vec::new(),
            founders: Vec::new(),
        }
    }

    #[test]
    fn a_straight_run_across_open_grass_is_direct() {
        let world = flat_world(32, 32, Terrain::Grass);
        let mut pf = Pathfinder::new(&world);
        let path = pf.find(&world, (2, 2), (10, 2)).unwrap();
        assert_eq!(path.len(), 8, "eight tiles east, no detour");
        assert_eq!(*path.last().unwrap(), (10, 2));
    }

    #[test]
    fn diagonals_are_used_and_cost_more_than_straight_steps() {
        let world = flat_world(32, 32, Terrain::Grass);
        let mut pf = Pathfinder::new(&world);
        let path = pf.find(&world, (2, 2), (10, 10)).unwrap();
        assert_eq!(path.len(), 8, "a pure diagonal is eight steps, not sixteen");
        let cost = path_cost(&world, &[(2, 2)].into_iter().chain(path).collect::<Vec<_>>());
        assert!(cost > 8.0 && cost < 12.0, "diagonal steps cost ~1.41 each, got {cost}");
    }

    #[test]
    fn deep_water_is_routed_around() {
        let mut world = flat_world(32, 32, Terrain::Grass);
        // A wall with a gap at the bottom.
        for y in 0..28 {
            let i = world.idx(16, y);
            world.tiles[i] = Terrain::DeepWater;
        }
        let mut pf = Pathfinder::new(&world);
        let path = pf.find(&world, (4, 4), (28, 4)).expect("a way round exists");

        assert!(path.iter().all(|&(x, y)| world.at(x, y).passable()));
        assert!(path.len() > 24, "it has to go the long way round, got {}", path.len());
        assert_eq!(*path.last().unwrap(), (28, 4));
    }

    #[test]
    fn a_walled_off_goal_is_unreachable() {
        let mut world = flat_world(24, 24, Terrain::Grass);
        for y in 0..24 {
            let i = world.idx(12, y);
            world.tiles[i] = Terrain::DeepWater;
        }
        let mut pf = Pathfinder::new(&world);
        assert!(pf.find(&world, (4, 4), (20, 4)).is_none());
    }

    #[test]
    fn a_goal_in_deep_water_is_refused_rather_than_searched_for() {
        let mut world = flat_world(16, 16, Terrain::Grass);
        let i = world.idx(8, 8);
        world.tiles[i] = Terrain::DeepWater;
        let mut pf = Pathfinder::new(&world);
        assert!(pf.find(&world, (2, 2), (8, 8)).is_none());
        assert_eq!(pf.stats.searches, 0, "rejected before any search work");
    }

    #[test]
    fn expensive_terrain_is_avoided_when_a_cheap_detour_exists() {
        let mut world = flat_world(32, 8, Terrain::Grass);
        // A band of shallow water (cost 2.2) straight ahead, clear above.
        for x in 8..24 {
            let i = world.idx(x, 4);
            world.tiles[i] = Terrain::ShallowWater;
        }
        let mut pf = Pathfinder::new(&world);
        let path = pf.find(&world, (2, 4), (30, 4)).unwrap();
        let waded = path.iter().filter(|&&(x, y)| world.at(x, y) == Terrain::ShallowWater).count();
        assert!(waded <= 2, "should step around the water, waded {waded} tiles");
    }

    #[test]
    fn the_same_query_twice_comes_from_cache_and_matches() {
        let world = flat_world(64, 64, Terrain::Grass);
        let mut pf = Pathfinder::new(&world);
        let a = pf.find(&world, (1, 1), (40, 30)).unwrap();
        let searches = pf.stats.searches;
        let b = pf.find(&world, (1, 1), (40, 30)).unwrap();

        assert_eq!(a, b);
        assert_eq!(pf.stats.searches, searches, "second query did no search work");
        assert_eq!(pf.stats.cache_hits, 1);
    }

    #[test]
    fn a_path_to_where_you_already_stand_is_empty_not_none() {
        let world = flat_world(16, 16, Terrain::Grass);
        let mut pf = Pathfinder::new(&world);
        assert_eq!(pf.find(&world, (5, 5), (5, 5)), Some(Vec::new()));
    }

    #[test]
    fn repeated_searches_stay_correct_after_the_generation_stamp_wraps() {
        let world = flat_world(24, 24, Terrain::Grass);
        let mut pf = Pathfinder::new(&world);
        let expected = pf.find(&world, (1, 1), (20, 20)).unwrap();

        // Force the stamp counter to the edge of wrapping.
        pf.generation = u32::MAX - 1;
        pf.clear_cache();
        let a = pf.find(&world, (1, 1), (20, 20)).unwrap();
        pf.clear_cache();
        let b = pf.find(&world, (1, 1), (20, 20)).unwrap();

        assert_eq!(a, expected);
        assert_eq!(b, expected, "results must survive the wrap");
    }

    #[test]
    fn the_same_search_is_identical_across_pathfinder_instances() {
        // Determinism (invariant 7): two fresh pathfinders must agree exactly,
        // or the golden-run test will drift the moment a creature travels.
        let world = flat_world(64, 64, Terrain::Forest);
        let a = Pathfinder::new(&world).find(&world, (3, 60), (60, 3));
        let b = Pathfinder::new(&world).find(&world, (3, 60), (60, 3));
        assert_eq!(a, b);
    }

    #[test]
    fn a_capped_search_still_makes_progress_toward_the_goal() {
        let world = flat_world(512, 512, Terrain::Forest);
        let mut pf = Pathfinder::new(&world);
        pf.node_cap = 200;
        let path = pf.find(&world, (5, 5), (500, 500)).expect("best effort, not nothing");

        assert_eq!(pf.stats.capped, 1);
        assert!(!path.is_empty());
        let end = *path.last().unwrap();
        let before = 5i64.abs_diff(500) + 5i64.abs_diff(500);
        let after = (end.0 as i64).abs_diff(500) + (end.1 as i64).abs_diff(500);
        assert!(after < before, "the partial route must close the gap");
    }
}
