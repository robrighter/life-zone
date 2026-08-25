//! Reading the record back (PRD §10).
//!
//! "The log is the product" (§1.1). Everything the simulation does is already
//! in SQLite; this is the half that makes it legible. Every aggregation here is
//! SQL over `creatures`, `events`, `decisions`, `tick_stats` and `beliefs` —
//! no state is kept, nothing is cached, and nothing here can affect a run.

pub mod culture;
pub mod queries;
