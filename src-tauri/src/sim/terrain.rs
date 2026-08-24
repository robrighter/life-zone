//! Terrain types (PRD §4.3).

use serde::{Deserialize, Serialize};

/// Stored as one byte per tile in `chunks.terrain_blob`. The discriminants are
/// part of the on-disk format: append new variants, never renumber existing ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[repr(u8)]
pub enum Terrain {
    DeepWater = 0,
    ShallowWater = 1,
    Sand = 2,
    Grass = 3,
    Forest = 4,
    Soil = 5,
    Hill = 6,
}

impl Terrain {
    pub fn from_u8(v: u8) -> Option<Self> {
        Some(match v {
            0 => Terrain::DeepWater,
            1 => Terrain::ShallowWater,
            2 => Terrain::Sand,
            3 => Terrain::Grass,
            4 => Terrain::Forest,
            5 => Terrain::Soil,
            6 => Terrain::Hill,
            _ => return None,
        })
    }

    /// Deep water is the only impassable terrain; it exists as a visual boundary.
    pub fn passable(self) -> bool {
        !matches!(self, Terrain::DeepWater)
    }

    /// Relative cost of entering this tile, for A* at M2. Forest, hills and
    /// shallow water are "passable (slow)" in §4.3.
    pub fn move_cost(self) -> f32 {
        match self {
            Terrain::DeepWater => f32::INFINITY,
            Terrain::ShallowWater => 2.2,
            Terrain::Forest | Terrain::Hill => 1.8,
            Terrain::Sand => 1.2,
            Terrain::Grass | Terrain::Soil => 1.0,
        }
    }

    /// Drinkable in place (§4.4).
    pub fn is_water(self) -> bool {
        matches!(self, Terrain::DeepWater | Terrain::ShallowWater)
    }

    pub fn is_fresh_water(self) -> bool {
        matches!(self, Terrain::ShallowWater)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discriminants_round_trip() {
        for v in 0u8..=6 {
            let t = Terrain::from_u8(v).expect("valid discriminant");
            assert_eq!(t as u8, v, "discriminant must be stable on disk");
        }
        assert!(Terrain::from_u8(7).is_none());
    }

    #[test]
    fn only_deep_water_blocks_movement() {
        assert!(!Terrain::DeepWater.passable());
        for t in [Terrain::ShallowWater, Terrain::Sand, Terrain::Grass,
                  Terrain::Forest, Terrain::Soil, Terrain::Hill] {
            assert!(t.passable(), "{t:?} should be passable");
            assert!(t.move_cost().is_finite());
        }
    }
}
