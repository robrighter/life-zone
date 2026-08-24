//! Turning the world into beliefs (PRD §4.11).
//!
//! Observation is passive and free: a creature simply sees what is within its
//! observation radius and forms firsthand beliefs about it. This is the only
//! way a belief enters the world at M2 — the three transmission channels
//! (ambient observation of *others*, `SHARE_KNOWLEDGE`, `TEACH`) land at M4.
//!
//! Seeing is also how a wrong belief gets corrected. A creature that walks to a
//! remembered clearing and finds it stripped overwrites `plentiful` with
//! `empty` on the same tick, which is what turns a stale belief from a silent
//! inefficiency into a legible disappointment.

use crate::config::WorldConfig;
use crate::sim::creature::Creature;
use crate::sim::economy::NodeIndex;
use crate::sim::event::{Event, EventKind};
use crate::sim::knowledge::{self, Belief, BeliefKind, Estimate};
use crate::sim::terrain::Terrain;
use crate::sim::world::{NodeKind, World};
use std::collections::VecDeque;

/// Precomputed fields that never change while the terrain does not.
///
/// Both are per-tile and cost 1MB each at 512x512, which buys an O(1) answer to
/// "is there water within sight, and where" — the alternative is scanning a
/// 13x13 window per creature per tick, or 84,500 tile reads.
pub struct WorldCache {
    pub water_dist: Vec<u32>,
    pub nearest_water: Vec<u32>,
}

impl WorldCache {
    pub fn build(world: &World) -> Self {
        let n = world.tiles.len();
        let mut water_dist = vec![u32::MAX; n];
        let mut nearest_water = vec![u32::MAX; n];
        let mut q = VecDeque::new();

        for y in 0..world.height {
            for x in 0..world.width {
                let i = world.idx(x, y);
                if world.tiles[i].is_fresh_water() {
                    water_dist[i] = 0;
                    nearest_water[i] = i as u32;
                    q.push_back((x, y));
                }
            }
        }

        // Multi-source BFS carrying the source with it, so every tile learns
        // both how far the water is and which tile it is.
        while let Some((x, y)) = q.pop_front() {
            let i = world.idx(x, y);
            let (d, src) = (water_dist[i], nearest_water[i]);
            for (dx, dy) in [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)] {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if !world.in_bounds(nx, ny) {
                    continue;
                }
                let ni = world.idx(nx as u32, ny as u32);
                if water_dist[ni] != u32::MAX {
                    continue;
                }
                water_dist[ni] = d + 1;
                nearest_water[ni] = src;
                q.push_back((nx as u32, ny as u32));
            }
        }

        Self { water_dist, nearest_water }
    }

    pub fn water_within(&self, world: &World, x: u32, y: u32, radius: u32) -> Option<(u32, u32)> {
        let i = world.idx(x, y);
        if self.water_dist[i] > radius {
            return None;
        }
        let src = self.nearest_water[i];
        if src == u32::MAX {
            return None;
        }
        Some((src % world.width, src / world.width))
    }
}

/// Everything one creature can see this tick, folded into its beliefs.
///
/// Returns the number of genuinely new discoveries, which is what the culture
/// reports count and what makes an explorer's trip legible in the event log.
#[allow(clippy::too_many_arguments)]
pub fn observe(
    c: &mut Creature,
    world: &World,
    cache: &WorldCache,
    nodes: &NodeIndex,
    scratch: &mut Vec<u32>,
    cfg: &WorldConfig,
    tick: i64,
    events: &mut Vec<Event>,
) -> u32 {
    let k = &cfg.knowledge;
    let radius = k.observation_radius;
    let cap = k.max_beliefs_held.max(8) as usize;
    let mut discovered = 0;
    let mut last: Option<(u32, u32, BeliefKind)> = None;

    // Water. Seen at a distance because a lake is a large visible thing; a
    // berry bush is not, which is why nodes use the same radius but must be
    // physically near.
    if let Some((wx, wy)) = cache.water_within(world, c.x, c.y, radius) {
        if knowledge::upsert(
            &mut c.beliefs,
            Belief {
                kind: BeliefKind::Water,
                x: wx,
                y: wy,
                estimate: Estimate::Plentiful,
                confidence: 1.0,
                learned_tick: tick,
                last_verified_tick: tick,
                source_creature_id: None,
                hops: 0,
                origin_creature_id: Some(c.id),
                origin_tick: tick,
            },
            (c.x, c.y),
            k,
            cap,
            tick,
        ) {
            discovered += 1;
        }
    }

    // Resource nodes in sight.
    nodes.near(world, c.x, c.y, radius, scratch);
    for &ni in scratch.iter() {
        let n = &world.nodes[ni as usize];
        if n.kind == NodeKind::Sheep && n.quantity <= 0.0 {
            continue;
        }
        let kind = BeliefKind::of_node(n.kind);
        let estimate = Estimate::of(n.quantity, n.max_quantity);

        if knowledge::upsert(
            &mut c.beliefs,
            Belief {
                kind,
                x: n.x,
                y: n.y,
                estimate,
                confidence: 1.0,
                learned_tick: tick,
                last_verified_tick: tick,
                source_creature_id: None,
                hops: 0,
                origin_creature_id: Some(c.id),
                origin_tick: tick,
            },
            (c.x, c.y),
            k,
            cap,
            tick,
        ) {
            discovered += 1;
            last = Some((n.x, n.y, kind));
        }
    }

    // Farmable ground underfoot. Soil only exists near water (worldgen §8.4),
    // so noticing it is noticing somewhere a crop could go.
    if world.at(c.x, c.y) == Terrain::Soil {
        knowledge::upsert(
            &mut c.beliefs,
            Belief {
                kind: BeliefKind::SoilPatch,
                x: c.x,
                y: c.y,
                estimate: Estimate::Some,
                confidence: 1.0,
                learned_tick: tick,
                last_verified_tick: tick,
                source_creature_id: None,
                hops: 0,
                origin_creature_id: Some(c.id),
                origin_tick: tick,
            },
            (c.x, c.y),
            k,
            cap,
            tick,
        );
    }

    if discovered > 0 {
        let (x, y, kind) = last.unwrap_or((c.x, c.y, BeliefKind::Water));
        events.push(
            Event::new(tick, EventKind::Discovered, c.id)
                .at(x, y)
                .with("kind", kind.as_str())
                .with_int("count", discovered as i64),
        );
    }
    discovered
}

/// Beliefs a creature starts life holding.
///
/// A founder is an adult who has lived here; it is not born knowing nothing
/// about the ground it is standing on. Without this a founder spawns blind and
/// has to stumble onto water before thirst kills it at tick 118 — which makes
/// early death a coin flip on the exploration RNG rather than a consequence of
/// anything. Seeding is limited to what is genuinely local: the water it draws
/// from and the nodes within a short walk.
pub fn seed_local_knowledge(
    c: &mut Creature,
    world: &World,
    cache: &WorldCache,
    nodes: &NodeIndex,
    scratch: &mut Vec<u32>,
    cfg: &WorldConfig,
    tick: i64,
) {
    let k = &cfg.knowledge;
    let cap = k.max_beliefs_held.max(8) as usize;
    let home_radius = k.observation_radius * 4;

    if let Some((wx, wy)) = cache.water_within(world, c.x, c.y, home_radius) {
        knowledge::upsert(
            &mut c.beliefs,
            Belief {
                kind: BeliefKind::Water,
                x: wx,
                y: wy,
                estimate: Estimate::Plentiful,
                confidence: 1.0,
                learned_tick: tick,
                last_verified_tick: tick,
                source_creature_id: None,
                hops: 0,
                origin_creature_id: Some(c.id),
                origin_tick: tick,
            },
            (c.x, c.y),
            k,
            cap,
            tick,
        );
    }

    nodes.near(world, c.x, c.y, home_radius, scratch);
    for &ni in scratch.iter() {
        let n = &world.nodes[ni as usize];
        if n.kind == NodeKind::Sheep && n.quantity <= 0.0 {
            continue;
        }
        knowledge::upsert(
            &mut c.beliefs,
            Belief {
                kind: BeliefKind::of_node(n.kind),
                x: n.x,
                y: n.y,
                estimate: Estimate::of(n.quantity, n.max_quantity),
                confidence: 0.85,
                learned_tick: tick,
                last_verified_tick: tick,
                source_creature_id: None,
                hops: 0,
                origin_creature_id: Some(c.id),
                origin_tick: tick,
            },
            (c.x, c.y),
            k,
            cap,
            tick,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::creature::testing::test_creature;
    use crate::sim::world::ResourceNode;

    fn world_with_water() -> World {
        let mut w = World {
            width: 64,
            height: 64,
            chunk_size: 32,
            seed: 1,
            tiles: vec![Terrain::Grass; 64 * 64],
            nodes: Vec::new(),
            founders: Vec::new(),
        };
        for y in 0..64 {
            let i = w.idx(60, y);
            w.tiles[i] = Terrain::ShallowWater;
        }
        w
    }

    #[test]
    fn the_water_field_knows_both_how_far_and_which_way() {
        let w = world_with_water();
        let c = WorldCache::build(&w);

        assert_eq!(c.water_dist[w.idx(59, 10)], 1);
        assert_eq!(c.water_within(&w, 59, 10, 6), Some((60, 10)));
        assert!(c.water_within(&w, 10, 10, 6).is_none(), "too far to see");
    }

    #[test]
    fn a_creature_forms_a_firsthand_belief_about_what_it_can_see() {
        let mut w = world_with_water();
        w.nodes.push(ResourceNode {
            kind: NodeKind::Forage,
            x: 20,
            y: 20,
            quantity: 12.0,
            max_quantity: 12.0,
            regen_rate: 0.0,
        });
        let cache = WorldCache::build(&w);
        let idx = NodeIndex::new(&w, 8);
        let cfg = WorldConfig::default();
        let mut scratch = Vec::new();
        let mut events = Vec::new();

        let mut c = test_creature();
        (c.x, c.y) = (22, 20);

        let found = observe(&mut c, &w, &cache, &idx, &mut scratch, &cfg, 10, &mut events);

        assert_eq!(found, 1);
        assert_eq!(c.beliefs.len(), 1);
        assert_eq!(c.beliefs[0].kind, BeliefKind::ForageNode);
        assert!(c.beliefs[0].is_firsthand());
        assert_eq!(c.beliefs[0].estimate, Estimate::Plentiful);
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn seeing_a_stripped_node_corrects_the_belief_that_led_you_there() {
        let mut w = world_with_water();
        w.nodes.push(ResourceNode {
            kind: NodeKind::Forage, x: 20, y: 20,
            quantity: 12.0, max_quantity: 12.0, regen_rate: 0.0,
        });
        let cache = WorldCache::build(&w);
        let mut idx = NodeIndex::new(&w, 8);
        let cfg = WorldConfig::default();
        let (mut scratch, mut events) = (Vec::new(), Vec::new());

        let mut c = test_creature();
        (c.x, c.y) = (20, 20);
        observe(&mut c, &w, &cache, &idx, &mut scratch, &cfg, 10, &mut events);
        assert_eq!(c.beliefs[0].estimate, Estimate::Plentiful);

        // Somebody else strips it while this creature is away.
        w.nodes[0].quantity = 0.0;
        idx.rebuild(&w);
        observe(&mut c, &w, &cache, &idx, &mut scratch, &cfg, 90, &mut events);

        assert_eq!(c.beliefs.len(), 1, "still one belief about that clearing");
        assert_eq!(c.beliefs[0].estimate, Estimate::Empty, "now known to be bare");
        assert_eq!(c.beliefs[0].last_verified_tick, 90);
    }

    #[test]
    fn nothing_out_of_sight_is_believed() {
        let w = world_with_water();
        let cache = WorldCache::build(&w);
        let idx = NodeIndex::new(&w, 8);
        let cfg = WorldConfig::default();
        let (mut scratch, mut events) = (Vec::new(), Vec::new());

        let mut c = test_creature();
        (c.x, c.y) = (5, 5);
        assert_eq!(observe(&mut c, &w, &cache, &idx, &mut scratch, &cfg, 1, &mut events), 0);
        assert!(c.beliefs.is_empty(), "no creature sees the whole map");
    }

    #[test]
    fn a_founder_starts_knowing_its_own_water() {
        let mut w = world_with_water();
        w.nodes.push(ResourceNode {
            kind: NodeKind::Wood, x: 45, y: 30,
            quantity: 40.0, max_quantity: 40.0, regen_rate: 0.0,
        });
        let cache = WorldCache::build(&w);
        let idx = NodeIndex::new(&w, 8);
        let cfg = WorldConfig::default();
        let mut scratch = Vec::new();

        let mut c = test_creature();
        (c.x, c.y) = (44, 30);
        seed_local_knowledge(&mut c, &w, &cache, &idx, &mut scratch, &cfg, 0);

        assert!(
            c.beliefs.iter().any(|b| b.kind == BeliefKind::Water),
            "an adult who lives here knows where the water is"
        );
        assert!(c.beliefs.iter().any(|b| b.kind == BeliefKind::WoodNode));
    }
}
