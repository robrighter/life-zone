//! Typed read/write helpers over the schema (PRD §3.2).

use crate::config::WorldConfig;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldRow {
    pub id: i64,
    pub name: String,
    pub seed: i64,
    pub created_at: String,
    pub current_tick: i64,
    pub status: String,
}

/// Create a world row and return it. `config` is stored verbatim as
/// `config_json`, which is the single source of truth for this world's rules.
pub fn create_world(
    conn: &Connection,
    name: &str,
    seed: i64,
    config: &WorldConfig,
) -> Result<WorldRow> {
    let config_json = serde_json::to_string(config).context("serialising world config")?;
    let created_at = now_iso8601();

    conn.execute(
        "INSERT INTO worlds (name, seed, config_json, created_at, current_tick, status)
         VALUES (?1, ?2, ?3, ?4, 0, 'active')",
        rusqlite::params![name, seed, config_json, created_at],
    )
    .context("inserting world row")?;

    let id = conn.last_insert_rowid();
    tracing::info!(world_id = id, name, seed, "created world");

    Ok(WorldRow {
        id,
        name: name.to_string(),
        seed,
        created_at,
        current_tick: 0,
        status: "active".into(),
    })
}

pub fn load_world(conn: &Connection, id: i64) -> Result<Option<WorldRow>> {
    let world = conn
        .query_row(
            "SELECT id, name, seed, created_at, current_tick, status FROM worlds WHERE id = ?1",
            [id],
            row_to_world,
        )
        .optional()?;
    Ok(world)
}

/// The config as stored for this world, with any fields absent from an older
/// save filled in from defaults.
pub fn load_world_config(conn: &Connection, id: i64) -> Result<Option<WorldConfig>> {
    let json: Option<String> = conn
        .query_row("SELECT config_json FROM worlds WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;

    match json {
        None => Ok(None),
        Some(j) => Ok(Some(
            serde_json::from_str(&j).context("parsing stored world config")?,
        )),
    }
}

pub fn list_worlds(conn: &Connection) -> Result<Vec<WorldRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, seed, created_at, current_tick, status
         FROM worlds ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], row_to_world)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Find the newest active world, if there is one.
pub fn latest_active_world(conn: &Connection) -> Result<Option<WorldRow>> {
    let world = conn
        .query_row(
            "SELECT id, name, seed, created_at, current_tick, status
             FROM worlds WHERE status = 'active' ORDER BY id DESC LIMIT 1",
            [],
            row_to_world,
        )
        .optional()?;
    Ok(world)
}

pub fn set_current_tick(conn: &Connection, world_id: i64, tick: i64) -> Result<()> {
    conn.execute(
        "UPDATE worlds SET current_tick = ?2 WHERE id = ?1",
        rusqlite::params![world_id, tick],
    )?;
    Ok(())
}

fn row_to_world(r: &rusqlite::Row) -> rusqlite::Result<WorldRow> {
    Ok(WorldRow {
        id: r.get(0)?,
        name: r.get(1)?,
        seed: r.get(2)?,
        created_at: r.get(3)?,
        current_tick: r.get(4)?,
        status: r.get(5)?,
    })
}

fn now_iso8601() -> String {
    use time::format_description::well_known::Rfc3339;
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "unknown".into())
}

// ---------------------------------------------------------------- worldgen

use crate::sim::terrain::Terrain;
use crate::sim::world::{NodeKind, ResourceNode, World};

/// Persist terrain and resource nodes. One transaction for the whole world:
/// 256 chunk inserts done individually would dominate generation time.
pub fn save_world(conn: &mut Connection, world_id: i64, world: &World) -> Result<()> {
    let tx = conn.transaction()?;

    tx.execute("DELETE FROM chunks WHERE world_id = ?1", [world_id])?;
    tx.execute("DELETE FROM resource_nodes WHERE world_id = ?1", [world_id])?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO chunks (world_id, cx, cy, terrain_blob) VALUES (?1, ?2, ?3, ?4)",
        )?;
        for cy in 0..world.chunks_y() {
            for cx in 0..world.chunks_x() {
                stmt.execute(rusqlite::params![world_id, cx, cy, world.chunk_blob(cx, cy)])?;
            }
        }
    }
    {
        let mut stmt = tx.prepare(
            "INSERT INTO resource_nodes
               (world_id, kind, x, y, quantity, max_quantity, regen_rate, state)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active')",
        )?;
        for n in &world.nodes {
            stmt.execute(rusqlite::params![
                world_id, n.kind.as_str(), n.x, n.y,
                n.quantity, n.max_quantity, n.regen_rate
            ])?;
        }
    }

    tx.commit()?;
    tracing::info!(world_id, chunks = world.chunks_x() * world.chunks_y(),
                   nodes = world.nodes.len(), "world persisted");
    Ok(())
}

pub fn world_has_terrain(conn: &Connection, world_id: i64) -> Result<bool> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM chunks WHERE world_id = ?1", [world_id], |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Reassemble the tile grid from its chunks.
pub fn load_terrain(
    conn: &Connection, world_id: i64, width: u32, height: u32, chunk_size: u32,
) -> Result<Option<Vec<Terrain>>> {
    if !world_has_terrain(conn, world_id)? {
        return Ok(None);
    }
    let mut tiles = vec![Terrain::DeepWater; (width as usize) * (height as usize)];

    let mut stmt = conn.prepare(
        "SELECT cx, cy, terrain_blob FROM chunks WHERE world_id = ?1",
    )?;
    let mut rows = stmt.query([world_id])?;
    while let Some(row) = rows.next()? {
        let cx: u32 = row.get(0)?;
        let cy: u32 = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;

        for ty in 0..chunk_size {
            for tx in 0..chunk_size {
                let (x, y) = (cx * chunk_size + tx, cy * chunk_size + ty);
                if x >= width || y >= height {
                    continue; // padding on an edge chunk
                }
                let b = blob[(ty * chunk_size + tx) as usize];
                let t = Terrain::from_u8(b)
                    .ok_or_else(|| anyhow::anyhow!("unknown terrain byte {b} at {x},{y}"))?;
                tiles[(y as usize) * (width as usize) + (x as usize)] = t;
            }
        }
    }
    Ok(Some(tiles))
}

pub fn load_resource_nodes(conn: &Connection, world_id: i64) -> Result<Vec<ResourceNode>> {
    let mut stmt = conn.prepare(
        "SELECT kind, x, y, quantity, max_quantity, regen_rate
         FROM resource_nodes WHERE world_id = ?1 AND state = 'active' ORDER BY id",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        let kind: String = r.get(0)?;
        Ok(ResourceNode {
            kind: match kind.as_str() {
                "FORAGE" => NodeKind::Forage,
                "WOOD" => NodeKind::Wood,
                "WHEAT" => NodeKind::Wheat,
                _ => NodeKind::Sheep,
            },
            x: r.get(1)?, y: r.get(2)?,
            quantity: r.get(3)?, max_quantity: r.get(4)?, regen_rate: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}


// ------------------------------------------------------------ M2: the sim

use crate::sim::actions::AbortReason;
use crate::sim::creature::{
    Creature, DeathCause, Inventory, LifeStage, Plan, Pregnancy, Sex, Traits,
};
use crate::sim::economy::{Structure, StructureKind, Structures};
use crate::sim::social::{Courtships, Edge, Household, Households, Offer, RelKind, Relationships};
use crate::sim::tick::TransmissionRecord;
use crate::sim::event::Event;
use crate::sim::knowledge::{Belief, BeliefKind, Estimate};
use crate::sim::tick::{DecisionRecord, PlanOutcome, TickReport};
use rusqlite::Transaction;

/// Append this tick's events.
///
/// Events are outcomes, not state: nothing here fires for movement, need decay
/// or partial progress, because a row per creature per tick is invariant 5
/// wearing a different hat.
pub fn insert_events(tx: &Transaction, world_id: i64, events: &[Event]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO events (world_id, tick, kind, actor_id, target_id, x, y, payload_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for e in events {
        stmt.execute(rusqlite::params![
            world_id,
            e.tick,
            e.kind.as_str(),
            e.actor_id.filter(|id| *id != 0),
            e.target_id,
            e.x,
            e.y,
            e.payload,
        ])?;
    }
    Ok(())
}

/// Record every decision. At M2 they are all tier 1; the LLM columns stay null
/// until M3 fills them, which is why they exist now rather than being migrated
/// in later (invariant 4).
pub fn insert_decisions(tx: &Transaction, world_id: i64, rows: &[DecisionRecord]) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO decisions
           (world_id, tick, creature_id, tier, creature_age_ticks, life_stage,
            age_weight, think_budget, prompt_hash, prompt_text, raw_response,
            parsed_plan_json, horizon_committed,
            fatigue_cost, hunger_cost, crisis_exempt,
            latency_ms, model, fallback_used, fallback_reason)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                 ?14, ?15, ?16, ?17, ?18, ?19, ?20)",
    )?;
    for d in rows {
        // Compact rather than a full plan blob: at M2 the goal, the intent and
        // the rationale are the whole decision, and there are hundreds of
        // thousands of them in a run.
        let plan = format!(
            "{{\"goal\":\"{}\",\"addresses\":\"{:?}\",\"rationale\":{}}}",
            d.goal,
            d.addresses,
            serde_json::to_string(&d.rationale).unwrap_or_else(|_| "\"\"".into()),
        );
        stmt.execute(rusqlite::params![
            world_id,
            d.tick,
            d.creature_id,
            d.tier as i64,
            d.age_ticks,
            d.life_stage.as_str(),
            d.age_weight,
            d.think_budget,
            d.prompt_hash,
            d.prompt_text,
            d.raw_response,
            plan,
            d.horizon_committed as i64,
            d.fatigue_cost,
            d.hunger_cost,
            d.crisis_exempt as i64,
            d.latency_ms.map(|v| v as i64),
            d.model,
            d.fallback_used as i64,
            d.fallback_reason,
        ])?;
    }
    Ok(())
}

/// Backfill `horizon_actual` and `abort_reason` onto the decision that issued
/// the plan. The gap between committed and actual is plan-abandonment, which
/// §5.5 calls the early-warning metric for the horizon mechanic failing — and
/// it can only be known once the plan has ended.
pub fn backfill_plan_outcomes(
    tx: &Transaction,
    world_id: i64,
    outcomes: &[PlanOutcome],
) -> Result<()> {
    if outcomes.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "UPDATE decisions SET horizon_actual = ?4, abort_reason = ?5
         WHERE world_id = ?1 AND creature_id = ?2 AND tick = ?3",
    )?;
    for o in outcomes {
        stmt.execute(rusqlite::params![
            world_id,
            o.creature_id,
            o.set_tick,
            o.horizon_actual as i64,
            o.reason.as_str(),
        ])?;
    }
    Ok(())
}

pub fn insert_tick_stats(tx: &Transaction, world_id: i64, r: &TickReport) -> Result<()> {
    let timings = serde_json::to_string(&r.timings)?;
    tx.prepare_cached(
        "INSERT OR REPLACE INTO tick_stats
           (world_id, tick, population, births, deaths, llm_calls, fallbacks,
            mean_latency_ms, phase_timings_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )?
    .execute(rusqlite::params![
        world_id,
        r.tick,
        r.population as i64,
        r.births as i64,
        r.deaths as i64,
        r.llm_dispatched as i64,
        // Invariant 8: the fallback rate is a production metric, so it is a
        // column rather than something reconstructed from the decision log.
        (r.llm_rejected + r.llm_failed) as i64,
        r.mean_latency_ms,
        timings,
    ])?;
    Ok(())
}

/// Write creature rows.
///
/// Deliberately *not* every tick. Needs change on every creature on every tick,
/// so a per-tick write would be 500 UPDATEs a tick and would dominate the
/// Fast-Forward budget for data nobody reads at that resolution. Current state
/// lives in RAM; this is the checkpoint, and it also runs on death, on pause
/// and at shutdown so nothing that matters is ever only in memory.
pub fn upsert_creatures<'a>(
    tx: &Transaction,
    world_id: i64,
    creatures: impl IntoIterator<Item = &'a Creature>,
) -> Result<()> {
    // `INSERT OR REPLACE` would be the obvious way to write this and it is
    // actively wrong here. REPLACE deletes the conflicting row before
    // inserting, and `beliefs.creature_id` is declared
    // `REFERENCES creatures(id) ON DELETE CASCADE` — so every checkpoint threw
    // away the belief history of every living creature, and then paid for it: 
    // the foreign key has no usable index on `creature_id` alone, so each of
    // the 500 deletions scanned the whole `beliefs` table. Measured at 6
    // seconds a checkpoint, and phase 7 at 99% of the tick.
    //
    // An upsert updates in place. Nothing is deleted, so nothing cascades.
    let mut stmt = tx.prepare_cached(
        "INSERT INTO creatures
           (id, world_id, name, sex, generation, mother_id, father_id, household_id,
            birth_tick, death_tick, death_cause, x, y, life_stage,
            hunger, thirst, fatigue, warmth, health, lifespan_modifier,
            traits_json, inventory_json, current_plan_json, plan_set_tick,
            plan_horizon, plan_ticks_remaining, plan_step_index,
            last_deliberation_tick, deliberation_pressure,
            lifetime_deliberations, lifetime_think_fatigue, habit_prior_json,
            wear, exposed_ticks, in_shelter, trauma_cause, trauma_tick,
            mate_id, paired_tick, pregnant_by, due_tick, last_birth_tick,
            children_born, guardian_id, taught_count, shared_count)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27,
                 ?28, ?29, ?30, ?31, ?46, ?32, ?33, ?34, ?35, ?36,
                 ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45)
         ON CONFLICT(id) DO UPDATE SET
            household_id = excluded.household_id,
            death_tick = excluded.death_tick,
            death_cause = excluded.death_cause,
            x = excluded.x, y = excluded.y,
            life_stage = excluded.life_stage,
            hunger = excluded.hunger, thirst = excluded.thirst,
            fatigue = excluded.fatigue, warmth = excluded.warmth,
            health = excluded.health,
            lifespan_modifier = excluded.lifespan_modifier,
            inventory_json = excluded.inventory_json,
            current_plan_json = excluded.current_plan_json,
            plan_set_tick = excluded.plan_set_tick,
            plan_horizon = excluded.plan_horizon,
            plan_ticks_remaining = excluded.plan_ticks_remaining,
            plan_step_index = excluded.plan_step_index,
            last_deliberation_tick = excluded.last_deliberation_tick,
            deliberation_pressure = excluded.deliberation_pressure,
            lifetime_deliberations = excluded.lifetime_deliberations,
            lifetime_think_fatigue = excluded.lifetime_think_fatigue,
            wear = excluded.wear,
            exposed_ticks = excluded.exposed_ticks,
            in_shelter = excluded.in_shelter,
            trauma_cause = excluded.trauma_cause,
            trauma_tick = excluded.trauma_tick,
            mate_id = excluded.mate_id,
            paired_tick = excluded.paired_tick,
            pregnant_by = excluded.pregnant_by,
            due_tick = excluded.due_tick,
            last_birth_tick = excluded.last_birth_tick,
            children_born = excluded.children_born,
            guardian_id = excluded.guardian_id,
            taught_count = excluded.taught_count,
            shared_count = excluded.shared_count,
            habit_prior_json = excluded.habit_prior_json",
    )?;
    for c in creatures {
        let plan_json = c.plan.as_ref().map(serde_json::to_string).transpose()?;
        stmt.execute(rusqlite::params![
            c.id,
            world_id,
            c.name,
            c.sex.as_str(),
            c.generation,
            c.mother_id,
            c.father_id,
            c.household_id,
            c.birth_tick,
            c.death_tick,
            c.death_cause.map(|d| d.as_str()),
            c.x,
            c.y,
            c.life_stage.as_str(),
            c.hunger,
            c.thirst,
            c.fatigue,
            c.warmth,
            c.health,
            c.lifespan_ticks,
            serde_json::to_string(&c.traits)?,
            serde_json::to_string(&c.inventory)?,
            plan_json,
            c.plan.as_ref().map(|p| p.set_tick),
            c.plan.as_ref().map(|p| p.horizon as i64),
            c.plan.as_ref().map(|p| p.ticks_remaining as i64),
            c.plan.as_ref().map(|p| p.step_index as i64),
            c.last_deliberation_tick,
            c.deliberation_pressure,
            c.lifetime_deliberations,
            c.lifetime_think_fatigue,
            c.wear,
            c.exposed_ticks as i64,
            c.in_shelter,
            c.trauma.map(|(cause, _)| cause.as_str()),
            c.trauma.map(|(_, at)| at),
            c.mate_id,
            c.paired_tick,
            c.pregnancy.map(|p| p.father_id),
            c.pregnancy.map(|p| p.due_tick),
            c.last_birth_tick,
            c.children_born,
            c.guardian_id,
            c.taught_count,
            c.shared_count,
            serde_json::to_string(&c.habit)?,
        ])?;
    }
    Ok(())
}

/// Periodic sampled state, in place of a per-creature-per-tick table (§7).
pub fn insert_creature_samples(
    tx: &Transaction,
    world_id: i64,
    tick: i64,
    creatures: &[Creature],
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO creature_samples
           (world_id, tick, creature_id, x, y, hunger, thirst, fatigue, warmth, health, life_stage)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for c in creatures {
        stmt.execute(rusqlite::params![
            world_id, tick, c.id, c.x, c.y,
            c.hunger, c.thirst, c.fatigue, c.warmth, c.health,
            c.life_stage.as_str(),
        ])?;
    }
    Ok(())
}

/// Replace the stored beliefs of these creatures.
///
/// Beliefs live in RAM during a run (§7); this table is the persistence and
/// reporting layer and is never on the per-tick read path.
pub fn flush_beliefs<'a>(
    tx: &Transaction,
    world_id: i64,
    creatures: impl IntoIterator<Item = &'a Creature>,
) -> Result<()> {
    let mut del = tx.prepare_cached(
        "DELETE FROM beliefs WHERE world_id = ?1 AND creature_id = ?2",
    )?;
    let mut ins = tx.prepare_cached(
        "INSERT INTO beliefs
           (world_id, creature_id, kind, x, y, detail_json, confidence,
            learned_tick, last_verified_tick, source_creature_id, hops,
            origin_creature_id, origin_tick)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for c in creatures {
        del.execute(rusqlite::params![world_id, c.id])?;
        for b in &c.beliefs {
            ins.execute(rusqlite::params![
                world_id,
                c.id,
                b.kind.as_str(),
                b.x,
                b.y,
                format!("{{\"est\":\"{}\"}}", b.estimate.as_str()),
                b.confidence,
                b.learned_tick,
                b.last_verified_tick,
                b.source_creature_id,
                b.hops as i64,
                b.origin_creature_id,
                b.origin_tick,
            ])?;
        }
    }
    Ok(())
}

pub fn upsert_structures(tx: &Transaction, world_id: i64, structures: &[Structure]) -> Result<()> {
    if structures.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT OR REPLACE INTO structures
           (id, world_id, kind, x, y, condition, capacity, household_id, built_tick,
            fuel_remaining, lit_until_tick)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?;
    for s in structures {
        stmt.execute(rusqlite::params![
            s.id, world_id, s.kind.as_str(), s.x, s.y, s.condition,
            s.capacity as i64, s.household_id, s.built_tick,
            s.fuel_remaining, s.lit_until_tick,
        ])?;
    }
    Ok(())
}

/// Household rows. Written before creatures every tick, because
/// `creatures.household_id` is a foreign key into this table and a creature
/// that founded a home this tick has nowhere to point until its household
/// exists.
pub fn upsert_households(
    tx: &Transaction,
    world_id: i64,
    households: &[Household],
) -> Result<()> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO households
           (id, world_id, shelter_id, founded_tick, dissolved_tick, store_json, size_cap,
            founder_a, founder_b)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(id) DO UPDATE SET
            shelter_id = excluded.shelter_id,
            dissolved_tick = excluded.dissolved_tick,
            store_json = excluded.store_json,
            size_cap = excluded.size_cap",
    )?;
    for h in households {
        stmt.execute(rusqlite::params![
            h.id,
            world_id,
            h.shelter_id,
            h.founded_tick,
            h.dissolved_tick,
            serde_json::to_string(&h.store)?,
            h.size_cap as i64,
            h.founder_ids.0,
            h.founder_ids.1,
        ])?;
    }
    Ok(())
}

/// Directed affinity edges (§4.10).
///
/// Rewritten wholesale on the slow cadence rather than incrementally: the set
/// churns constantly as creatures meet and die, and a diff would cost more to
/// compute than the write it saves.
pub fn save_relationships(
    tx: &Transaction,
    world_id: i64,
    rels: &Relationships,
) -> Result<()> {
    tx.execute("DELETE FROM relationships WHERE world_id = ?1", [world_id])?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO relationships
           (world_id, from_creature, to_creature, affinity, kind, updated_tick)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for ((from, to), e) in rels.iter() {
        stmt.execute(rusqlite::params![
            world_id,
            from,
            to,
            e.affinity,
            e.kind.map(|k| k.as_str()),
            e.updated_tick,
        ])?;
    }
    Ok(())
}

/// One row per act of transmission — who told whom, over which channel, how
/// much. This is what the culture reports read: the transmission graph, the
/// teaching rate by household, and whether information hubs emerge (§10).
pub fn insert_transmissions(
    tx: &Transaction,
    world_id: i64,
    rows: &[TransmissionRecord],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare_cached(
        "INSERT INTO transmissions
           (world_id, tick, from_creature, to_creature, channel, belief_count, kinds_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for r in rows {
        stmt.execute(rusqlite::params![
            world_id,
            r.tick,
            r.from,
            r.to,
            r.channel.as_str(),
            r.count as i64,
            "[]",
        ])?;
    }
    Ok(())
}

pub fn load_households(conn: &Connection, world_id: i64) -> Result<Households> {
    let mut stmt = conn.prepare(
        "SELECT id, shelter_id, founded_tick, dissolved_tick, store_json, size_cap,
                founder_a, founder_b
         FROM households WHERE world_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        Ok(Household {
            id: r.get(0)?,
            shelter_id: r.get(1)?,
            founded_tick: r.get(2)?,
            dissolved_tick: r.get(3)?,
            store: serde_json::from_str(&r.get::<_, String>(4)?).unwrap_or_default(),
            size_cap: r.get::<_, i64>(5)? as u32,
            founder_ids: (r.get::<_, Option<i64>>(6)?.unwrap_or(0), r.get(7)?),
            dirty: false,
        })
    })?;
    let items: Vec<Household> = rows.collect::<Result<Vec<_>, _>>()?;
    let next = items.iter().map(|h| h.id).max().unwrap_or(0) + 1;
    let mut hs = Households::with_next_id(next);
    hs.items = items;
    Ok(hs)
}

/// Courtship offers awaiting an answer. Persisted because a creature's next
/// decision depends on whether one is standing, so losing them makes a resumed
/// run diverge.
pub fn save_courtships(tx: &Transaction, world_id: i64, c: &Courtships) -> Result<()> {
    tx.execute("DELETE FROM courtship_offers WHERE world_id = ?1", [world_id])?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO courtship_offers (world_id, from_creature, to_creature, offered_tick)
         VALUES (?1, ?2, ?3, ?4)",
    )?;
    for o in &c.offers {
        stmt.execute(rusqlite::params![world_id, o.from, o.to, o.tick])?;
    }
    Ok(())
}

pub fn load_courtships(conn: &Connection, world_id: i64) -> Result<Courtships> {
    let mut stmt = conn.prepare(
        "SELECT from_creature, to_creature, offered_tick
         FROM courtship_offers WHERE world_id = ?1 ORDER BY from_creature, to_creature",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        Ok(Offer { from: r.get(0)?, to: r.get(1)?, tick: r.get(2)? })
    })?;
    let mut c = Courtships::new();
    c.offers = rows.collect::<Result<Vec<_>, _>>()?;
    Ok(c)
}

pub fn load_relationships(conn: &Connection, world_id: i64) -> Result<Relationships> {
    let mut stmt = conn.prepare(
        "SELECT from_creature, to_creature, affinity, kind, updated_tick
         FROM relationships WHERE world_id = ?1 ORDER BY from_creature, to_creature",
    )?;
    let mut rels = Relationships::new();
    let mut rows = stmt.query([world_id])?;
    while let Some(row) = rows.next()? {
        let kind = row.get::<_, Option<String>>(3)?.and_then(|k| match k.as_str() {
            "KIN" => Some(RelKind::Kin),
            "MATE" => Some(RelKind::Mate),
            "HOUSEHOLD" => Some(RelKind::Household),
            "ACQUAINTANCE" => Some(RelKind::Acquaintance),
            _ => None,
        });
        rels.insert_raw(
            row.get(0)?,
            row.get(1)?,
            Edge { affinity: row.get(2)?, kind, updated_tick: row.get(4)? },
        );
    }
    Ok(rels)
}

/// Lineage depth and descendant counts, by a recursive CTE over
/// `mother_id`/`father_id`.
///
/// Invariant 6: lineage is derived, never stored. This is that derivation — the
/// only place the tree exists — and it is why there is no `lineage` table to
/// keep in sync with reality.
pub fn deepest_lineages(
    conn: &Connection,
    world_id: i64,
    limit: i64,
) -> Result<Vec<(i64, String, i32, i64)>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE descent(founder, id, depth) AS (
             SELECT id, id, 0 FROM creatures
              WHERE world_id = ?1 AND generation = 1
             UNION ALL
             SELECT d.founder, c.id, d.depth + 1
               FROM creatures c JOIN descent d
                 ON c.mother_id = d.id OR c.father_id = d.id
              WHERE c.world_id = ?1 AND d.depth < 24
         )
         SELECT d.founder, f.name, MAX(d.depth) AS depth, COUNT(*) - 1 AS descendants
           FROM descent d JOIN creatures f ON f.id = d.founder
          GROUP BY d.founder
          ORDER BY depth DESC, descendants DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![world_id, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Structures that no longer exist in RAM (a fire burnt to cold ash) are
/// removed, so a resume does not resurrect them.
pub fn prune_structures(tx: &Transaction, world_id: i64, alive: &[Structure]) -> Result<()> {
    let ids: Vec<String> = alive.iter().map(|s| s.id.to_string()).collect();
    let sql = if ids.is_empty() {
        "DELETE FROM structures WHERE world_id = ?1".to_string()
    } else {
        format!(
            "DELETE FROM structures WHERE world_id = ?1 AND id NOT IN ({})",
            ids.join(",")
        )
    };
    tx.execute(&sql, rusqlite::params![world_id])?;
    Ok(())
}

/// Update the live resource stock. Nodes are rewritten wholesale because
/// planting appends to the list and quantities change everywhere every tick.
pub fn save_resource_nodes(tx: &Transaction, world_id: i64, world: &World) -> Result<()> {
    tx.execute("DELETE FROM resource_nodes WHERE world_id = ?1", [world_id])?;
    let mut stmt = tx.prepare_cached(
        "INSERT INTO resource_nodes
           (world_id, kind, x, y, quantity, max_quantity, regen_rate, state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'active')",
    )?;
    for n in &world.nodes {
        stmt.execute(rusqlite::params![
            world_id, n.kind.as_str(), n.x, n.y, n.quantity, n.max_quantity, n.regen_rate
        ])?;
    }
    Ok(())
}

// ------------------------------------------------------------------ reading

/// Living creatures, in ascending id order — the order the simulation iterates
/// in, so a resumed run visits them exactly as an uninterrupted one would.
pub fn load_living_creatures(conn: &Connection, world_id: i64) -> Result<Vec<Creature>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, sex, generation, mother_id, father_id, household_id,
                birth_tick, death_tick, death_cause, x, y, life_stage,
                hunger, thirst, fatigue, warmth, health, lifespan_modifier,
                traits_json, inventory_json, current_plan_json,
                last_deliberation_tick, deliberation_pressure,
                lifetime_deliberations, lifetime_think_fatigue,
                wear, exposed_ticks, in_shelter, trauma_cause, trauma_tick,
                mate_id, paired_tick, pregnant_by, due_tick, last_birth_tick,
                children_born, guardian_id, taught_count, shared_count, habit_prior_json
         FROM creatures WHERE world_id = ?1 AND death_tick IS NULL ORDER BY id",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        let traits: Traits = serde_json::from_str::<Traits>(&r.get::<_, String>(19)?)
            .unwrap_or_default();
        let inventory: Inventory = serde_json::from_str(&r.get::<_, String>(20)?)
            .unwrap_or_default();
        let plan: Option<Plan> = r
            .get::<_, Option<String>>(21)?
            .and_then(|s| serde_json::from_str(&s).ok());
        Ok(Creature {
            id: r.get(0)?,
            name: r.get(1)?,
            sex: Sex::parse(&r.get::<_, String>(2)?),
            generation: r.get(3)?,
            mother_id: r.get(4)?,
            father_id: r.get(5)?,
            household_id: r.get(6)?,
            birth_tick: r.get(7)?,
            death_tick: r.get(8)?,
            death_cause: None,
            x: r.get(10)?,
            y: r.get(11)?,
            life_stage: LifeStage::parse(&r.get::<_, String>(12)?),
            hunger: r.get(13)?,
            thirst: r.get(14)?,
            fatigue: r.get(15)?,
            warmth: r.get(16)?,
            health: r.get(17)?,
            lifespan_ticks: r.get(18)?,
            wear: r.get(26)?,
            traits,
            inventory,
            plan,
            beliefs: Vec::new(),
            last_deliberation_tick: r.get(22)?,
            deliberation_pressure: r.get(23)?,
            lifetime_deliberations: r.get(24)?,
            lifetime_think_fatigue: r.get(25)?,
            in_shelter: r.get(28)?,
            exposed_ticks: r.get::<_, i64>(27)? as u32,
            at_fire: false,
            trauma: match (r.get::<_, Option<String>>(29)?, r.get::<_, Option<i64>>(30)?) {
                (Some(cause), Some(at)) => death_cause_from_str(&cause).map(|c| (c, at)),
                _ => None,
            },
            mate_id: r.get(31)?,
            paired_tick: r.get(32)?,
            pregnancy: match (r.get::<_, Option<i64>>(33)?, r.get::<_, Option<i64>>(34)?) {
                (Some(father_id), Some(due_tick)) => Some(Pregnancy { father_id, due_tick }),
                _ => None,
            },
            last_birth_tick: r.get(35)?,
            children_born: r.get(36)?,
            guardian_id: r.get(37)?,
            taught_count: r.get(38)?,
            shared_count: r.get(39)?,
            // Habit is a denormalised summary of this creature's own history
            // (§7 says exactly that about `habit_prior_json`), so it is stored
            // as one blob rather than eight columns.
            habit: r
                .get::<_, Option<String>>(40)?
                .and_then(|j| serde_json::from_str(&j).ok())
                .unwrap_or([0; crate::ai::budget::HABITS]),
            dirty: false,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Death tallies as `[count; 7]` indexed by `DeathCause`, so a resumed run
/// continues the same running totals rather than restarting them.
pub fn death_tallies(conn: &Connection, world_id: i64) -> Result<([u32; 7], u64, u64)> {
    let mut tallies = [0u32; 7];
    for (cause, n) in deaths_by_cause(conn, world_id)? {
        if let Some(c) = death_cause_from_str(&cause) {
            tallies[c as usize] = n as u32;
        }
    }
    let born: i64 = conn.query_row(
        "SELECT COUNT(*) FROM creatures WHERE world_id = ?1", [world_id], |r| r.get(0),
    )?;
    let died: i64 = conn.query_row(
        "SELECT COUNT(*) FROM creatures WHERE world_id = ?1 AND death_tick IS NOT NULL",
        [world_id], |r| r.get(0),
    )?;
    Ok((tallies, born as u64, died as u64))
}

/// Attach stored beliefs to creatures already loaded, matching by id.
pub fn load_beliefs_into(conn: &Connection, world_id: i64, creatures: &mut [Creature]) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT creature_id, kind, x, y, detail_json, confidence, learned_tick,
                last_verified_tick, source_creature_id, hops, origin_creature_id, origin_tick
         FROM beliefs WHERE world_id = ?1 ORDER BY creature_id, id",
    )?;
    let mut rows = stmt.query([world_id])?;
    while let Some(row) = rows.next()? {
        let creature_id: i64 = row.get(0)?;
        let Some(kind) = BeliefKind::parse(&row.get::<_, String>(1)?) else {
            continue;
        };
        let detail: String = row.get(4)?;
        let estimate = if detail.contains("plentiful") {
            Estimate::Plentiful
        } else if detail.contains("picked over") {
            Estimate::Sparse
        } else if detail.contains("empty") {
            Estimate::Empty
        } else {
            Estimate::Some
        };
        // Binary search: both sides are in ascending id order.
        let Ok(idx) = creatures.binary_search_by_key(&creature_id, |c| c.id) else {
            continue;
        };
        creatures[idx].beliefs.push(Belief {
            kind,
            x: row.get(2)?,
            y: row.get(3)?,
            estimate,
            confidence: row.get(5)?,
            learned_tick: row.get(6)?,
            last_verified_tick: row.get(7)?,
            source_creature_id: row.get(8)?,
            hops: row.get::<_, i64>(9)? as u8,
            origin_creature_id: row.get(10)?,
            origin_tick: row.get(11)?,
        });
    }
    Ok(())
}

pub fn load_structures(conn: &Connection, world_id: i64) -> Result<Structures> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, x, y, condition, capacity, household_id, built_tick,
                fuel_remaining, lit_until_tick
         FROM structures WHERE world_id = ?1 ORDER BY id",
    )?;
    let rows = stmt.query_map([world_id], |r| {
        Ok(Structure {
            id: r.get(0)?,
            kind: StructureKind::parse(&r.get::<_, String>(1)?),
            x: r.get(2)?,
            y: r.get(3)?,
            condition: r.get(4)?,
            capacity: r.get::<_, i64>(5)? as u32,
            occupants: 0,
            household_id: r.get(6)?,
            built_tick: r.get(7)?,
            fuel_remaining: r.get(8)?,
            lit_until_tick: r.get(9)?,
            dirty: false,
        })
    })?;
    let items: Vec<Structure> = rows.collect::<Result<Vec<_>, _>>()?;
    let next = items.iter().map(|s| s.id).max().unwrap_or(0) + 1;
    let mut st = Structures::with_next_id(next);
    st.items = items;
    Ok(st)
}

/// The next free creature id, so a resumed run never reuses one.
pub fn next_creature_id(conn: &Connection, world_id: i64) -> Result<i64> {
    let n: Option<i64> = conn.query_row(
        "SELECT MAX(id) FROM creatures WHERE world_id = ?1",
        [world_id],
        |r| r.get(0),
    )?;
    Ok(n.unwrap_or(0) + 1)
}

/// Cause-of-death tallies — the M2 exit criterion, read straight from the DB.
pub fn deaths_by_cause(conn: &Connection, world_id: i64) -> Result<Vec<(String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT death_cause, COUNT(*) FROM creatures
         WHERE world_id = ?1 AND death_cause IS NOT NULL
         GROUP BY death_cause ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([world_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Recent events, newest first — the ticker.
pub fn recent_events(conn: &Connection, world_id: i64, limit: i64) -> Result<Vec<Event>> {
    let mut stmt = conn.prepare(
        "SELECT tick, kind, actor_id, target_id, x, y, payload_json
         FROM events WHERE world_id = ?1 ORDER BY id DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![world_id, limit], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, Option<i64>>(2)?,
            r.get::<_, Option<i64>>(3)?,
            r.get::<_, Option<u32>>(4)?,
            r.get::<_, Option<u32>>(5)?,
            r.get::<_, String>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (tick, kind, actor, target, x, y, payload) = row?;
        // Kinds are round-tripped by name; an unknown one is skipped rather
        // than crashing a reader against a newer writer.
        let Some(kind) = event_kind_from_str(&kind) else { continue };
        out.push(Event { tick, kind, actor_id: actor, target_id: target, x, y, payload });
    }
    Ok(out)
}

fn event_kind_from_str(s: &str) -> Option<crate::sim::event::EventKind> {
    use crate::sim::event::EventKind as K;
    Some(match s {
        "BORN" => K::Born, "DIED" => K::Died, "ARRIVED" => K::Arrived,
        "GATHERED" => K::Gathered, "CHOPPED" => K::Chopped, "HARVESTED" => K::Harvested,
        "PLANTED" => K::Planted, "TENDED" => K::Tended, "SLAUGHTERED" => K::Slaughtered,
        "DRANK" => K::Drank, "ATE" => K::Ate, "RESTED" => K::Rested,
        "SHELTERED" => K::Sheltered, "FIRE_LIT" => K::FireLit, "FIRE_FED" => K::FireFed,
        "FIRE_OUT" => K::FireOut, "SHELTER_BUILT" => K::ShelterBuilt,
        "SHELTER_REPAIRED" => K::ShelterRepaired, "DISCOVERED" => K::Discovered,
        "VERIFIED" => K::Verified, "FORGOT" => K::Forgot, "PLAN_SET" => K::PlanSet,
        "PLAN_DONE" => K::PlanDone, "PLAN_ABANDONED" => K::PlanAbandoned,
        "SPOILED" => K::Spoiled, "EXPOSED_NIGHT" => K::ExposedNight,
        "INJURED" => K::Injured, "FELL_ILL" => K::FellIll, "SETTLED" => K::Settled,
        _ => return None,
    })
}

/// Unused at M2 but part of the typed surface the reporting layer needs; kept
/// here so the enum round-trip has one home.
pub fn death_cause_from_str(s: &str) -> Option<DeathCause> {
    DeathCause::ALL.into_iter().find(|c| c.as_str() == s)
}

pub fn abort_reason_str(r: AbortReason) -> &'static str {
    r.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch(include_str!("migrations/001_initial.sql")).unwrap();
        conn
    }

    #[test]
    fn creates_and_reads_back_a_world() {
        let conn = db();
        let cfg = WorldConfig::default();
        let w = create_world(&conn, "Ashfen", 44127, &cfg).unwrap();

        assert_eq!(w.name, "Ashfen");
        assert_eq!(w.seed, 44127);
        assert_eq!(w.current_tick, 0);
        assert_eq!(w.status, "active");

        let loaded = load_world(&conn, w.id).unwrap().unwrap();
        assert_eq!(loaded.id, w.id);
        assert_eq!(loaded.seed, 44127);
    }

    #[test]
    fn config_survives_the_round_trip_through_the_row() {
        let conn = db();
        let mut cfg = WorldConfig::default();
        cfg.features.wheat = false; // the S4 experiment
        cfg.llm.model = "qwen3:4b".into();

        let w = create_world(&conn, "NoWheat", 7, &cfg).unwrap();
        let back = load_world_config(&conn, w.id).unwrap().unwrap();

        assert!(!back.features.wheat);
        assert_eq!(back.llm.model, "qwen3:4b");
        assert_eq!(back.map.width, 512);
    }

    #[test]
    fn missing_world_reads_as_none() {
        let conn = db();
        assert!(load_world(&conn, 999).unwrap().is_none());
        assert!(load_world_config(&conn, 999).unwrap().is_none());
    }

    #[test]
    fn latest_active_world_picks_the_newest() {
        let conn = db();
        let cfg = WorldConfig::default();
        create_world(&conn, "first", 1, &cfg).unwrap();
        let second = create_world(&conn, "second", 2, &cfg).unwrap();

        assert_eq!(latest_active_world(&conn).unwrap().unwrap().id, second.id);
        assert_eq!(list_worlds(&conn).unwrap().len(), 2);
    }

    #[test]
    fn terrain_survives_a_save_load_round_trip() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 44127, &cfg).unwrap();

        let generated = crate::sim::worldgen::generate(44127, &cfg).world;
        save_world(&mut conn, w.id, &generated).unwrap();

        let loaded = load_terrain(&conn, w.id, 128, 128, cfg.map.chunk_size)
            .unwrap()
            .expect("terrain should be present");

        assert_eq!(loaded, generated.tiles, "tiles must round-trip byte-identically");
    }

    #[test]
    fn resource_nodes_survive_a_round_trip() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 7, &cfg).unwrap();

        let generated = crate::sim::worldgen::generate(7, &cfg).world;
        save_world(&mut conn, w.id, &generated).unwrap();
        let loaded = load_resource_nodes(&conn, w.id).unwrap();

        assert_eq!(loaded.len(), generated.nodes.len());
        for (a, b) in loaded.iter().zip(generated.nodes.iter()) {
            assert_eq!(a.kind, b.kind);
            assert_eq!((a.x, a.y), (b.x, b.y));
            assert!((a.quantity - b.quantity).abs() < 1e-4);
        }
    }

    #[test]
    fn saving_twice_replaces_rather_than_duplicates() {
        let mut conn = db();
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        let w = create_world(&conn, "Ashfen", 1, &cfg).unwrap();
        let generated = crate::sim::worldgen::generate(1, &cfg).world;

        save_world(&mut conn, w.id, &generated).unwrap();
        save_world(&mut conn, w.id, &generated).unwrap();

        let chunks: i64 = conn
            .query_row("SELECT COUNT(*) FROM chunks WHERE world_id = ?1", [w.id], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks as u32, generated.chunks_x() * generated.chunks_y());
    }

    #[test]
    fn terrain_reads_as_none_before_anything_is_saved() {
        let conn = db();
        let w = create_world(&conn, "empty", 1, &WorldConfig::default()).unwrap();
        assert!(!world_has_terrain(&conn, w.id).unwrap());
        assert!(load_terrain(&conn, w.id, 512, 512, 32).unwrap().is_none());
    }

    #[test]
    fn current_tick_advances() {
        let conn = db();
        let w = create_world(&conn, "Ashfen", 1, &WorldConfig::default()).unwrap();
        set_current_tick(&conn, w.id, 4118).unwrap();
        assert_eq!(load_world(&conn, w.id).unwrap().unwrap().current_tick, 4118);
    }
}
