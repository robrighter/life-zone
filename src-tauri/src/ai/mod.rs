//! Decision-making. At M2 only Tier 1 exists; `ollama.rs`, `prompt.rs`,
//! `budget.rs` and `schema.rs` land at M3 alongside it, never replacing it.

pub mod policy;
pub mod budget;
pub mod ollama;
pub mod prompt;
pub mod schema;
