//! The simulation core. Owns all world state exclusively (PRD §3.1).

pub mod actions;
pub mod creature;
pub mod economy;
pub mod event;
pub mod knowledge;
pub mod noise;
pub mod pathfind;
pub mod perception;
pub mod runner;
pub mod terrain;
pub mod tick;
pub mod world;
pub mod worldgen;
