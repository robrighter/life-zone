//! World state: the tile grid and what sits on it.

use super::terrain::Terrain;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum NodeKind {
    Forage,
    Wood,
    Wheat,
    Sheep,
}

impl NodeKind {
    pub fn as_str(self) -> &'static str {
        match self {
            NodeKind::Forage => "FORAGE",
            NodeKind::Wood => "WOOD",
            NodeKind::Wheat => "WHEAT",
            NodeKind::Sheep => "SHEEP",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceNode {
    pub kind: NodeKind,
    pub x: u32,
    pub y: u32,
    pub quantity: f32,
    pub max_quantity: f32,
    pub regen_rate: f32,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Founder {
    pub x: u32,
    pub y: u32,
    pub female: bool,
}

#[derive(Debug, Clone)]
pub struct World {
    pub width: u32,
    pub height: u32,
    pub chunk_size: u32,
    pub seed: u64,
    /// Row-major, `width * height`. One byte per tile on disk.
    pub tiles: Vec<Terrain>,
    pub nodes: Vec<ResourceNode>,
    pub founders: Vec<Founder>,
}

impl World {
    #[inline]
    pub fn idx(&self, x: u32, y: u32) -> usize {
        (y as usize) * (self.width as usize) + (x as usize)
    }

    #[inline]
    pub fn at(&self, x: u32, y: u32) -> Terrain {
        self.tiles[self.idx(x, y)]
    }

    pub fn in_bounds(&self, x: i64, y: i64) -> bool {
        x >= 0 && y >= 0 && x < self.width as i64 && y < self.height as i64
    }

    pub fn chunks_x(&self) -> u32 {
        self.width.div_ceil(self.chunk_size)
    }

    pub fn chunks_y(&self) -> u32 {
        self.height.div_ceil(self.chunk_size)
    }

    /// One chunk's tiles as raw bytes, row-major within the chunk. This is both
    /// the persistence unit and the render-cache unit (§4.3).
    pub fn chunk_blob(&self, cx: u32, cy: u32) -> Vec<u8> {
        let cs = self.chunk_size;
        let mut out = Vec::with_capacity((cs * cs) as usize);
        for ty in 0..cs {
            for tx in 0..cs {
                let (x, y) = (cx * cs + tx, cy * cs + ty);
                // Edge chunks on a non-multiple map size pad with deep water.
                let t = if x < self.width && y < self.height {
                    self.at(x, y)
                } else {
                    Terrain::DeepWater
                };
                out.push(t as u8);
            }
        }
        out
    }

    /// The whole grid as bytes, for a single transfer to the renderer.
    pub fn terrain_bytes(&self) -> Vec<u8> {
        self.tiles.iter().map(|t| *t as u8).collect()
    }

    /// Stable fingerprint of everything worldgen produced. Used to prove
    /// invariant 7 (same seed -> identical world) without comparing megabytes.
    pub fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();

        self.width.hash(&mut h);
        self.height.hash(&mut h);
        self.seed.hash(&mut h);
        self.tiles.hash(&mut h);

        for n in &self.nodes {
            n.kind.hash(&mut h);
            n.x.hash(&mut h);
            n.y.hash(&mut h);
            // Floats have no Hash; their bit patterns are what must match.
            n.quantity.to_bits().hash(&mut h);
            n.max_quantity.to_bits().hash(&mut h);
            n.regen_rate.to_bits().hash(&mut h);
        }
        for f in &self.founders {
            f.x.hash(&mut h);
            f.y.hash(&mut h);
            f.female.hash(&mut h);
        }
        h.finish()
    }
}
