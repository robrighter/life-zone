//! The event log — the spine of reporting (PRD §7).
//!
//! Events record *occurrences*, never per-tick state. A row per creature per
//! tick would be invariant 5 wearing a different hat, so movement, need decay
//! and partial progress produce nothing; a completed gather, a death, a
//! discovery and an abandoned plan all do.
//!
//! The event stream is also what the golden-run test hashes, so its contents
//! and order are load-bearing for determinism, not just for reporting.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EventKind {
    Born,
    Died,
    Arrived,
    Gathered,
    Chopped,
    Harvested,
    Planted,
    Tended,
    Slaughtered,
    Drank,
    Ate,
    Rested,
    Sheltered,
    FireLit,
    FireFed,
    FireOut,
    ShelterBuilt,
    ShelterRepaired,
    Discovered,
    Verified,
    Forgot,
    PlanSet,
    PlanDone,
    PlanAbandoned,
    Spoiled,
    ExposedNight,
    Injured,
    FellIll,
    Settled,
}

impl EventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Born => "BORN",
            EventKind::Died => "DIED",
            EventKind::Arrived => "ARRIVED",
            EventKind::Gathered => "GATHERED",
            EventKind::Chopped => "CHOPPED",
            EventKind::Harvested => "HARVESTED",
            EventKind::Planted => "PLANTED",
            EventKind::Tended => "TENDED",
            EventKind::Slaughtered => "SLAUGHTERED",
            EventKind::Drank => "DRANK",
            EventKind::Ate => "ATE",
            EventKind::Rested => "RESTED",
            EventKind::Sheltered => "SHELTERED",
            EventKind::FireLit => "FIRE_LIT",
            EventKind::FireFed => "FIRE_FED",
            EventKind::FireOut => "FIRE_OUT",
            EventKind::ShelterBuilt => "SHELTER_BUILT",
            EventKind::ShelterRepaired => "SHELTER_REPAIRED",
            EventKind::Discovered => "DISCOVERED",
            EventKind::Verified => "VERIFIED",
            EventKind::Forgot => "FORGOT",
            EventKind::PlanSet => "PLAN_SET",
            EventKind::PlanDone => "PLAN_DONE",
            EventKind::PlanAbandoned => "PLAN_ABANDONED",
            EventKind::Spoiled => "SPOILED",
            EventKind::ExposedNight => "EXPOSED_NIGHT",
            EventKind::Injured => "INJURED",
            EventKind::FellIll => "FELL_ILL",
            EventKind::Settled => "SETTLED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub tick: i64,
    pub kind: EventKind,
    pub actor_id: Option<i64>,
    pub target_id: Option<i64>,
    pub x: Option<u32>,
    pub y: Option<u32>,
    /// Compact `key=value` pairs rather than JSON objects.
    ///
    /// The golden-run test hashes this text, so it has to serialise identically
    /// on every run. A hand-built string with fixed field order and fixed float
    /// precision does that by construction; `serde_json::Value` would depend on
    /// map ordering and on float formatting staying stable across versions.
    pub payload: String,
}

impl Event {
    pub fn new(tick: i64, kind: EventKind, actor_id: i64) -> Self {
        Self { tick, kind, actor_id: Some(actor_id), target_id: None, x: None, y: None,
               payload: String::new() }
    }

    pub fn at(mut self, x: u32, y: u32) -> Self {
        self.x = Some(x);
        self.y = Some(y);
        self
    }

    pub fn target(mut self, id: i64) -> Self {
        self.target_id = Some(id);
        self
    }

    pub fn with(mut self, key: &str, value: &str) -> Self {
        if !self.payload.is_empty() {
            self.payload.push(' ');
        }
        self.payload.push_str(key);
        self.payload.push('=');
        self.payload.push_str(value);
        self
    }

    /// Two decimal places, so a float never varies in its last digit between
    /// runs or platforms.
    pub fn with_num(self, key: &str, value: f32) -> Self {
        let v = format!("{:.2}", value);
        self.with(key, &v)
    }

    pub fn with_int(self, key: &str, value: i64) -> Self {
        let v = value.to_string();
        self.with(key, &v)
    }

    /// Stable line form, used by the golden-run digest.
    pub fn digest_line(&self) -> String {
        format!(
            "{}|{}|{}|{}|{}|{}|{}",
            self.tick,
            self.kind.as_str(),
            self.actor_id.map(|v| v.to_string()).unwrap_or_default(),
            self.target_id.map(|v| v.to_string()).unwrap_or_default(),
            self.x.map(|v| v.to_string()).unwrap_or_default(),
            self.y.map(|v| v.to_string()).unwrap_or_default(),
            self.payload,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payload_field_order_is_fixed_by_construction() {
        let a = Event::new(4, EventKind::Gathered, 7)
            .at(10, 12)
            .with("kind", "FORAGE")
            .with_num("qty", 1.5);
        let b = Event::new(4, EventKind::Gathered, 7)
            .at(10, 12)
            .with("kind", "FORAGE")
            .with_num("qty", 1.5);
        assert_eq!(a.digest_line(), b.digest_line());
        assert_eq!(a.payload, "kind=FORAGE qty=1.50");
    }

    #[test]
    fn floats_are_written_at_fixed_precision() {
        let e = Event::new(0, EventKind::Ate, 1).with_num("v", 1.0 / 3.0);
        assert_eq!(e.payload, "v=0.33", "no drifting last digit between runs");
    }

    #[test]
    fn absent_fields_do_not_shift_the_digest_columns() {
        let e = Event::new(9, EventKind::Died, 3);
        assert_eq!(e.digest_line(), "9|DIED|3||||");
    }
}
