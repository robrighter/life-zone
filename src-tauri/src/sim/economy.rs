//! Resource nodes, regrowth, structures, spoilage, and the fuel economy
//! (PRD §4.4).
//!
//! Two mechanics here look like flavour and are not:
//!
//! * **Spoilage** is what stops the three food sources collapsing into one
//!   fungible number. Forage feeds you today; meat must be eaten or shared
//!   within four days; only grain accumulates. Because §4.8 gates reproduction
//!   on a stored reserve, and only grain reaches it, foraging is a life and
//!   farming is a lineage.
//! * **Wood is fuel as well as timber.** Without that, "go chop wood" stops
//!   being a live decision once the shelter stands, and warmth becomes purely
//!   positional — be home by dark — which caps exploration at the round trip a
//!   creature can make in one day and throttles the whole knowledge layer.
//!   Carried firewood is what puts distant discoveries in reach.

use crate::config::WorldConfig;
use crate::sim::creature::{Batch, Inventory, ItemKind};
use crate::sim::world::{NodeKind, ResourceNode, World};
use serde::{Deserialize, Serialize};

/// Whether this tick falls in the night window (§4.1). Night applies an
/// exposure penalty to anyone neither sheltered nor beside a lit fire, and
/// reduces forage yield.
pub fn is_night(tick: i64, cfg: &WorldConfig) -> bool {
    let hour = hour_of(tick);
    let (start, end) = (cfg.actions.night_start_hour, cfg.actions.night_end_hour);
    if start <= end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

pub fn hour_of(tick: i64) -> u32 {
    tick.rem_euclid(24) as u32
}

pub fn day_of(tick: i64) -> i64 {
    tick.div_euclid(24)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum StructureKind {
    Shelter,
    Fire,
    Pen,
}

impl StructureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            StructureKind::Shelter => "SHELTER",
            StructureKind::Fire => "FIRE",
            StructureKind::Pen => "PEN",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "FIRE" => StructureKind::Fire,
            "PEN" => StructureKind::Pen,
            _ => StructureKind::Shelter,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Structure {
    pub id: i64,
    pub kind: StructureKind,
    pub x: u32,
    pub y: u32,
    /// 0..1. A shelter decays and stops keeping the cold out.
    pub condition: f32,
    pub capacity: u32,
    pub occupants: u32,
    pub household_id: Option<i64>,
    pub built_tick: i64,
    /// Wood remaining in a fire. A fire is the cheaper, more fragile substitute
    /// for a shelter: it costs wood every night and goes out when the wood runs
    /// out (§4.6).
    pub fuel_remaining: f32,
    pub lit_until_tick: Option<i64>,
    pub dirty: bool,
}

impl Structure {
    pub fn is_lit(&self, tick: i64) -> bool {
        self.kind == StructureKind::Fire
            && self.fuel_remaining > 0.0
            && self.lit_until_tick.is_none_or(|t| tick <= t)
    }

    pub fn shelters(&self) -> bool {
        self.kind == StructureKind::Shelter && self.condition > 0.15
    }

    pub fn has_room(&self) -> bool {
        self.occupants < self.capacity
    }
}

/// All structures in the world, kept in a Vec in ascending id order so every
/// traversal is deterministic.
#[derive(Debug, Default)]
pub struct Structures {
    pub items: Vec<Structure>,
    next_id: i64,
}

impl Structures {
    pub fn new() -> Self {
        Self { items: Vec::new(), next_id: 1 }
    }

    pub fn with_next_id(next_id: i64) -> Self {
        Self { items: Vec::new(), next_id: next_id.max(1) }
    }

    pub fn add(&mut self, mut s: Structure) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        s.id = id;
        s.dirty = true;
        self.items.push(s);
        id
    }

    pub fn get(&self, id: i64) -> Option<&Structure> {
        self.items.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: i64) -> Option<&mut Structure> {
        self.items.iter_mut().find(|s| s.id == id)
    }

    pub fn next_id(&self) -> i64 {
        self.next_id
    }

    /// The nearest usable shelter with room, by squared distance. Ties break on
    /// id so the choice never depends on insertion order.
    pub fn nearest_shelter(&self, x: u32, y: u32, max_dist: u32) -> Option<&Structure> {
        let max2 = (max_dist as i64).pow(2);
        self.items
            .iter()
            .filter(|s| s.shelters() && s.has_room())
            .map(|s| (dist2(s.x, s.y, x, y), s))
            .filter(|(d, _)| *d <= max2)
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)))
            .map(|(_, s)| s)
    }

    /// A lit fire within warming range of a tile.
    pub fn fire_near(&self, x: u32, y: u32, radius: u32, tick: i64) -> Option<&Structure> {
        let r2 = (radius as i64).pow(2);
        self.items
            .iter()
            .filter(|s| s.is_lit(tick))
            .map(|s| (dist2(s.x, s.y, x, y), s))
            .filter(|(d, _)| *d <= r2)
            .min_by(|a, b| a.0.cmp(&b.0).then(a.1.id.cmp(&b.1.id)))
            .map(|(_, s)| s)
    }
}

#[inline]
fn dist2(ax: u32, ay: u32, bx: u32, by: u32) -> i64 {
    let dx = ax as i64 - bx as i64;
    let dy = ay as i64 - by as i64;
    dx * dx + dy * dy
}

/// Shelf life in ticks, or `None` for food that keeps indefinitely.
///
/// Grain is the only one that accumulates, and that single fact is what makes
/// the resource portfolio real (§4.4).
pub fn shelf_life(kind: ItemKind, cfg: &WorldConfig) -> Option<u32> {
    if !cfg.features.spoilage {
        return None;
    }
    match kind {
        ItemKind::Forage => Some(cfg.resources.forage_spoil_ticks),
        ItemKind::Meat => Some(cfg.resources.meat_spoil_ticks),
        ItemKind::Grain => cfg.resources.grain_spoil_ticks,
        ItemKind::Wood => None,
    }
}

/// Ticks until this batch spoils, or `None` if it keeps.
pub fn ticks_until_spoiled(b: &Batch, tick: i64, cfg: &WorldConfig) -> Option<i64> {
    shelf_life(b.kind, cfg).map(|life| b.harvested_tick + life as i64 - tick)
}

/// Expire spoiled batches. Returns what was lost, for the economy reports —
/// "food spoiled vs food eaten" is how you see a community over-gathering
/// perishables instead of investing in crops (§10).
pub fn spoil(inventory: &mut Inventory, tick: i64, cfg: &WorldConfig) -> f32 {
    if !cfg.features.spoilage {
        return 0.0;
    }
    let mut lost = 0.0;
    inventory.batches.retain(|b| {
        match shelf_life(b.kind, cfg) {
            Some(life) if tick - b.harvested_tick >= life as i64 => {
                lost += b.quantity;
                false
            }
            _ => true,
        }
    });
    lost
}

/// Regrow nodes toward their maximum and advance crops.
///
/// Wheat is the exception: it does not regenerate, it *grows* — a planted node
/// starts at zero and matures over `wheat_growth_ticks`, which is what gives
/// farming its latency and makes it a decision no other resource creates.
pub fn regrow(world: &mut World, _cfg: &WorldConfig) {
    for node in world.nodes.iter_mut() {
        if node.regen_rate <= 0.0 {
            continue;
        }
        if node.quantity < node.max_quantity {
            node.quantity = (node.quantity + node.regen_rate).min(node.max_quantity);
        }
    }
}

/// Sheep wander and, slowly, breed.
///
/// **Deviation from PRD §4.4, stated plainly:** the PRD breeds sheep only when
/// penned and fed, and pens are M4. Applied literally at M2 the ~60 sheep a
/// world generates are a one-time consumable that 500 creatures strip inside a
/// few hundred ticks, after which meat — and with it the entire four-day shelf
/// life that creates sharing pressure — never appears in a run again. So wild
/// flocks breed here at a slow rate against a cap. Penned breeding stays the
/// fast path and lands with pens at M4.
pub fn sheep_tick(world: &mut World, cfg: &WorldConfig, rng: &mut impl rand::Rng) {
    if !cfg.features.sheep {
        return;
    }
    let cap = (cfg.resources.sheep_flocks as usize) * 8;
    let mut live = 0usize;
    let mut births: Vec<(u32, u32)> = Vec::new();

    for i in 0..world.nodes.len() {
        if world.nodes[i].kind != NodeKind::Sheep || world.nodes[i].quantity <= 0.0 {
            continue;
        }
        live += 1;

        // Wander, but only onto grass, so flocks stay on grazing land.
        if rng.gen::<f32>() < 0.08 {
            let dx = rng.gen_range(-1i64..=1);
            let dy = rng.gen_range(-1i64..=1);
            let (nx, ny) = (world.nodes[i].x as i64 + dx, world.nodes[i].y as i64 + dy);
            if world.in_bounds(nx, ny)
                && world.at(nx as u32, ny as u32) == crate::sim::terrain::Terrain::Grass
            {
                world.nodes[i].x = nx as u32;
                world.nodes[i].y = ny as u32;
            }
        }

        if rng.gen::<f32>() < 0.0016 {
            births.push((world.nodes[i].x, world.nodes[i].y));
        }
    }

    for (x, y) in births {
        if live >= cap {
            break;
        }
        live += 1;
        // Reuse a slaughtered node before appending: node indices are the
        // stable handle a plan step holds, so the Vec is append-only and
        // entries are never removed mid-run.
        if let Some(slot) = world
            .nodes
            .iter_mut()
            .find(|n| n.kind == NodeKind::Sheep && n.quantity <= 0.0)
        {
            slot.x = x;
            slot.y = y;
            slot.quantity = 1.0;
        } else {
            world.nodes.push(ResourceNode {
                kind: NodeKind::Sheep,
                x,
                y,
                quantity: 1.0,
                max_quantity: 6.0,
                regen_rate: 0.0,
            });
        }
    }
}

/// Burn fuel in lit fires, gutter the ones that run out, and decay shelters.
/// Returns the number of fires that went out this tick.
pub fn burn_and_decay(structures: &mut Structures, cfg: &WorldConfig, tick: i64) -> u32 {
    let mut guttered = 0;
    // Cold ashes are cleared. Left in place they accumulate for the whole run —
    // one measured run ended with 615 of them — and every shelter and fire
    // lookup scans the list, so they cost real time for the rest of the game.
    structures.items.retain(|s| {
        !(s.kind == StructureKind::Fire
            && s.fuel_remaining <= 0.0
            && s.lit_until_tick.is_some_and(|t| tick - t > 24))
    });
    for s in structures.items.iter_mut() {
        match s.kind {
            StructureKind::Fire => {
                if s.fuel_remaining > 0.0 {
                    s.fuel_remaining =
                        (s.fuel_remaining - cfg.resources.fire_fuel_burn_per_tick).max(0.0);
                    s.dirty = true;
                    if s.fuel_remaining <= 0.0 {
                        s.lit_until_tick = Some(tick);
                        guttered += 1;
                    }
                }
            }
            StructureKind::Shelter | StructureKind::Pen => {
                if s.condition > 0.0 {
                    s.condition = (s.condition - cfg.actions.shelter_decay_per_tick).max(0.0);
                    s.dirty = true;
                }
            }
        }
    }
    guttered
}

/// A spatial index over resource nodes, rebuilt each tick.
///
/// Observation and target-seeking both need "what is near this tile", and
/// scanning ~700 nodes per creature per tick is 350,000 comparisons that a
/// coarse grid turns into a handful. Rebuilt rather than maintained because
/// sheep move: rebuilding 700 entries is cheaper than tracking their moves.
pub struct NodeIndex {
    cell: u32,
    cols: u32,
    rows: u32,
    cells: Vec<Vec<u32>>,
}

impl NodeIndex {
    pub fn new(world: &World, cell: u32) -> Self {
        let cols = world.width.div_ceil(cell);
        let rows = world.height.div_ceil(cell);
        let mut me = Self { cell, cols, rows, cells: vec![Vec::new(); (cols * rows) as usize] };
        me.rebuild(world);
        me
    }

    pub fn rebuild(&mut self, world: &World) {
        for c in self.cells.iter_mut() {
            c.clear();
        }
        for (i, n) in world.nodes.iter().enumerate() {
            if n.quantity <= 0.0 && n.kind == NodeKind::Sheep {
                continue;
            }
            let cx = (n.x / self.cell).min(self.cols - 1);
            let cy = (n.y / self.cell).min(self.rows - 1);
            self.cells[(cy * self.cols + cx) as usize].push(i as u32);
        }
    }

    /// The live node of this kind standing on exactly this tile, if any.
    ///
    /// Scanning every node in the world for this was the single hottest line in
    /// the policy: three kinds looked up per decision, hundreds of thousands of
    /// decisions per run, against a node list that grows as crops are planted.
    pub fn find_at(&self, world: &World, kind: NodeKind, x: u32, y: u32) -> Option<u32> {
        if x >= world.width || y >= world.height {
            return None;
        }
        let cx = (x / self.cell).min(self.cols - 1);
        let cy = (y / self.cell).min(self.rows - 1);
        self.cells[(cy * self.cols + cx) as usize]
            .iter()
            .copied()
            .find(|&i| {
                let n = &world.nodes[i as usize];
                n.kind == kind && n.x == x && n.y == y && n.quantity > 0.01
            })
    }

    /// Node indices within `radius` tiles of (x, y), in ascending index order.
    pub fn near(&self, world: &World, x: u32, y: u32, radius: u32, out: &mut Vec<u32>) {
        out.clear();
        let x0 = x.saturating_sub(radius) / self.cell;
        let y0 = y.saturating_sub(radius) / self.cell;
        let x1 = ((x + radius) / self.cell).min(self.cols - 1);
        let y1 = ((y + radius) / self.cell).min(self.rows - 1);
        let r2 = (radius as i64).pow(2);

        for cy in y0..=y1 {
            for cx in x0..=x1 {
                for &i in &self.cells[(cy * self.cols + cx) as usize] {
                    let n = &world.nodes[i as usize];
                    if dist2(n.x, n.y, x, y) <= r2 {
                        out.push(i);
                    }
                }
            }
        }
        // Ascending index: the traversal order must not depend on cell layout.
        out.sort_unstable();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorldConfig;

    #[test]
    fn night_spans_midnight() {
        let cfg = WorldConfig::default();
        assert!(!is_night(12, &cfg), "noon is day");
        assert!(is_night(20, &cfg), "20:00 is night");
        assert!(is_night(23, &cfg));
        assert!(is_night(24 + 2, &cfg), "02:00 the next day is still night");
        assert!(!is_night(24 + 6, &cfg), "06:00 is dawn");
        assert!(!is_night(24 + 19, &cfg));
    }

    #[test]
    fn forage_spoils_meat_lasts_longer_and_grain_keeps() {
        let cfg = WorldConfig::default();
        let mut inv = Inventory::default();
        inv.add(ItemKind::Forage, 5.0, 0);
        inv.add(ItemKind::Meat, 5.0, 0);
        inv.add(ItemKind::Grain, 5.0, 0);

        assert_eq!(spoil(&mut inv, 47, &cfg), 0.0, "nothing has expired yet");

        let lost = spoil(&mut inv, 48, &cfg);
        assert_eq!(lost, 5.0, "forage goes at ~2 days");
        assert_eq!(inv.total(ItemKind::Forage), 0.0);
        assert_eq!(inv.total(ItemKind::Meat), 5.0);

        spoil(&mut inv, 96, &cfg);
        assert_eq!(inv.total(ItemKind::Meat), 0.0, "meat goes at ~4 days");

        spoil(&mut inv, 100_000, &cfg);
        assert_eq!(inv.total(ItemKind::Grain), 5.0, "grain is the only food that keeps");
    }

    #[test]
    fn disabling_spoilage_makes_everything_keep() {
        let mut cfg = WorldConfig::default();
        cfg.features.spoilage = false;
        let mut inv = Inventory::default();
        inv.add(ItemKind::Forage, 5.0, 0);
        assert_eq!(spoil(&mut inv, 10_000, &cfg), 0.0);
        assert_eq!(inv.total(ItemKind::Forage), 5.0);
    }

    #[test]
    fn a_fire_burns_its_wood_then_goes_out() {
        let cfg = WorldConfig::default();
        let mut st = Structures::new();
        let id = st.add(Structure {
            id: 0,
            kind: StructureKind::Fire,
            x: 5,
            y: 5,
            condition: 1.0,
            capacity: 0,
            occupants: 0,
            household_id: None,
            built_tick: 0,
            fuel_remaining: 2.0,
            lit_until_tick: None,
            dirty: false,
        });

        assert!(st.get(id).unwrap().is_lit(0));
        // 0.5 wood per tick: four ticks of light out of two wood.
        for t in 0..3 {
            assert_eq!(burn_and_decay(&mut st, &cfg, t), 0);
            assert!(st.get(id).unwrap().is_lit(t));
        }
        assert_eq!(burn_and_decay(&mut st, &cfg, 3), 1, "the fourth tick guts it");
        assert!(!st.get(id).unwrap().is_lit(4), "no wood, no fire");
    }

    #[test]
    fn a_lit_fire_warms_only_what_is_near_it() {
        let mut st = Structures::new();
        st.add(Structure {
            id: 0,
            kind: StructureKind::Fire,
            x: 20,
            y: 20,
            condition: 1.0,
            capacity: 0,
            occupants: 0,
            household_id: None,
            built_tick: 0,
            fuel_remaining: 5.0,
            lit_until_tick: None,
            dirty: false,
        });
        assert!(st.fire_near(21, 20, 2, 0).is_some());
        assert!(st.fire_near(28, 20, 2, 0).is_none(), "warmth does not carry across the map");
    }

    #[test]
    fn a_full_shelter_is_not_offered() {
        let mut st = Structures::new();
        let id = st.add(Structure {
            id: 0,
            kind: StructureKind::Shelter,
            x: 10,
            y: 10,
            condition: 1.0,
            capacity: 2,
            occupants: 0,
            household_id: None,
            built_tick: 0,
            fuel_remaining: 0.0,
            lit_until_tick: None,
            dirty: false,
        });
        assert!(st.nearest_shelter(11, 10, 20).is_some());
        st.get_mut(id).unwrap().occupants = 2;
        assert!(st.nearest_shelter(11, 10, 20).is_none());
    }

    #[test]
    fn a_derelict_shelter_stops_sheltering() {
        let mut st = Structures::new();
        let id = st.add(Structure {
            id: 0,
            kind: StructureKind::Shelter,
            x: 10,
            y: 10,
            condition: 1.0,
            capacity: 4,
            occupants: 0,
            household_id: None,
            built_tick: 0,
            fuel_remaining: 0.0,
            lit_until_tick: None,
            dirty: false,
        });
        st.get_mut(id).unwrap().condition = 0.05;
        assert!(st.nearest_shelter(10, 10, 5).is_none());
    }

    #[test]
    fn the_node_index_finds_what_is_near_and_ignores_what_is_not() {
        let mut world = World {
            width: 128,
            height: 128,
            chunk_size: 32,
            seed: 1,
            tiles: vec![crate::sim::terrain::Terrain::Grass; 128 * 128],
            nodes: Vec::new(),
            founders: Vec::new(),
        };
        for (x, y) in [(10u32, 10u32), (12, 11), (100, 100)] {
            world.nodes.push(ResourceNode {
                kind: NodeKind::Forage,
                x,
                y,
                quantity: 5.0,
                max_quantity: 12.0,
                regen_rate: 0.02,
            });
        }
        let idx = NodeIndex::new(&world, 8);
        let mut out = Vec::new();
        idx.near(&world, 10, 10, 6, &mut out);
        assert_eq!(out, vec![0, 1], "ascending index, far node excluded");

        idx.near(&world, 60, 60, 4, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn regrowth_is_capped_at_the_node_maximum() {
        let cfg = WorldConfig::default();
        let mut world = World {
            width: 8,
            height: 8,
            chunk_size: 8,
            seed: 1,
            tiles: vec![crate::sim::terrain::Terrain::Grass; 64],
            nodes: vec![ResourceNode {
                kind: NodeKind::Forage,
                x: 1,
                y: 1,
                quantity: 11.99,
                max_quantity: 12.0,
                regen_rate: 0.5,
            }],
            founders: Vec::new(),
        };
        regrow(&mut world, &cfg);
        assert_eq!(world.nodes[0].quantity, 12.0);
        regrow(&mut world, &cfg);
        assert_eq!(world.nodes[0].quantity, 12.0, "never exceeds max");
    }
}
