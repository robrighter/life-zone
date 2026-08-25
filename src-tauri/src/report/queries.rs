//! The reporting aggregations (PRD §10).
//!
//! Two rules run through all of this.
//!
//! **Lineage is derived, never stored** (invariant 6). Every ancestry question
//! is a recursive CTE over `mother_id`/`father_id`. There is no lineage table
//! to fall out of sync, and the tree is exactly as true as the creatures are.
//!
//! **Reads never touch the simulation.** These run on a second connection while
//! the sim thread writes on its own; WAL makes that safe, and it means opening
//! a report can never stall a tick.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// A numeric field out of an event payload, as a SQL expression.
///
/// `events.payload_json` is named for what it was going to be and holds what it
/// actually is: space-separated `key=value` text. That is a deliberate trade —
/// it is written on every event of every tick and parsed only when somebody
/// opens a report — but it means `json_extract` raises "malformed JSON" against
/// it, so the digging is done here instead.
///
/// Returns NULL rather than 0 when the key is absent, so a missing field is
/// distinguishable from a recorded zero and `SUM` skips it instead of
/// pretending the event carried no wood.
pub(crate) fn payload_num(key: &str) -> String {
    let k = format!("{key}=");
    let n = k.len();
    format!(
        "(CASE WHEN instr(payload_json, '{k}') = 0 THEN NULL ELSE CAST(
            substr(payload_json, instr(payload_json, '{k}') + {n},
                   CASE WHEN instr(substr(payload_json, instr(payload_json, '{k}') + {n}), ' ') = 0
                        THEN length(payload_json)
                        ELSE instr(substr(payload_json, instr(payload_json, '{k}') + {n}), ' ') - 1
                   END) AS REAL) END)"
    )
}

/// The four numbers at the top of the reporting view.
#[derive(Debug, Clone, Serialize, Default)]
pub struct Headline {
    /// The highest `creatures.generation` any creature reached — the number
    /// M4's exit criterion ("a lineage reaches generation 3") is graded on.
    ///
    /// Deliberately not the lineage *depth* from `deepest_lineages`, which is
    /// zero-based and counts steps below a founder. Reporting depth here made
    /// the headline read "generation 0" for a run whose founders were all
    /// generation 1, which is the kind of off-by-one that gets believed.
    pub deepest_generation: i64,
    pub deepest_founder: Option<String>,
    pub deepest_descendants: i64,
    pub median_life_ticks: i64,
    pub baseline_ticks: i64,
    pub infant_mortality: f64,
    pub infant_mortality_first_gen: f64,
    /// S7: the share of beliefs in circulation whose discoverer is dead.
    pub beliefs_outliving_finders: f64,
    pub total_born: i64,
    pub total_dead: i64,
    pub living: i64,
    pub through_tick: i64,
}

pub fn headline(conn: &Connection, world: i64) -> Result<Headline> {
    let mut h = Headline::default();
    // From the world's own config, not a constant: every dial is tunable per
    // world (§11), so a hardcoded baseline would quietly lie on any run of M6's
    // sweep that moved it.
    h.baseline_ticks = conn
        .query_row(
            "SELECT json_extract(config_json, '$.lifespan.baseline_ticks')
               FROM worlds WHERE id = ?1",
            [world],
            |r| r.get(0),
        )
        .unwrap_or(672);

    h.through_tick = conn
        .query_row("SELECT current_tick FROM worlds WHERE id = ?1", [world], |r| r.get(0))
        .unwrap_or(0);
    h.total_born = count(conn, "SELECT COUNT(*) FROM creatures WHERE world_id = ?1", world)?;
    h.total_dead = count(
        conn,
        "SELECT COUNT(*) FROM creatures WHERE world_id = ?1 AND death_tick IS NOT NULL",
        world,
    )?;
    h.living = h.total_born - h.total_dead;

    h.deepest_generation = conn
        .query_row(
            "SELECT COALESCE(MAX(generation), 0) FROM creatures WHERE world_id = ?1",
            [world],
            |r| r.get(0),
        )
        .unwrap_or(0);

    if let Some(top) = deepest_lineages(conn, world, 1)?.first() {
        h.deepest_founder = Some(top.founder_name.clone());
        h.deepest_descendants = top.descendants;
    }

    // Median rather than mean: a run with an early massacre and a few
    // survivors has a mean that describes nobody.
    h.median_life_ticks = conn
        .query_row(
            "SELECT death_tick - birth_tick FROM creatures
              WHERE world_id = ?1 AND death_tick IS NOT NULL
              ORDER BY death_tick - birth_tick
              LIMIT 1 OFFSET (
                SELECT COUNT(*) / 2 FROM creatures
                 WHERE world_id = ?1 AND death_tick IS NOT NULL)",
            [world],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // §10 calls infant mortality the sharpest signal of whether households are
    // coping: the share of those born who did not live to work.
    h.infant_mortality = infant_mortality(conn, world, None)?;
    h.infant_mortality_first_gen = infant_mortality(conn, world, Some(2))?;
    h.beliefs_outliving_finders = beliefs_from_the_dead(conn, world)?;
    Ok(h)
}

fn count(conn: &Connection, sql: &str, world: i64) -> Result<i64> {
    Ok(conn.query_row(sql, [world], |r| r.get(0)).unwrap_or(0))
}

/// Share of creatures born into a generation that died before adulthood.
///
/// Only counts generations that have had time to grow up — a creature born ten
/// ticks ago is not an infant death, and counting it as a survivor is just as
/// wrong. Both are excluded rather than guessed at.
pub fn infant_mortality(conn: &Connection, world: i64, generation: Option<i64>) -> Result<f64> {
    let (born, died): (i64, i64) = conn.query_row(
        "SELECT
            SUM(CASE WHEN death_tick IS NOT NULL
                      OR (SELECT current_tick FROM worlds WHERE id = ?1) - birth_tick >= 168
                     THEN 1 ELSE 0 END),
            SUM(CASE WHEN death_tick IS NOT NULL AND death_tick - birth_tick < 168
                     THEN 1 ELSE 0 END)
         FROM creatures
         WHERE world_id = ?1 AND (?2 IS NULL OR generation = ?2)",
        rusqlite::params![world, generation],
        |r| Ok((r.get(0).unwrap_or(0), r.get(1).unwrap_or(0))),
    )?;
    Ok(if born == 0 { 0.0 } else { died as f64 / born as f64 })
}

/// S7, as one number: what share of the beliefs currently in circulation came
/// from somebody who is no longer alive to be asked.
/// S7: the share of beliefs **in circulation** whose discoverer is dead.
///
/// "In circulation" means held by somebody alive, which is the join this
/// originally lacked. A dead creature's belief rows stay in the table — that is
/// deliberate, and it is what lets a belief be traced to its origin long after
/// the finder is gone — but they are not in circulation, and counting them puts
/// the entire dead population into the denominator. On a run that ends empty
/// the measure then approaches zero by construction no matter how well
/// knowledge actually travelled while there was anyone left to carry it.
pub fn beliefs_from_the_dead(conn: &Connection, world: i64) -> Result<f64> {
    let (total, inherited): (i64, i64) = conn.query_row(
        "SELECT COUNT(*),
                SUM(CASE WHEN b.origin_creature_id IS NOT NULL
                          AND b.origin_creature_id <> b.creature_id
                          AND EXISTS (SELECT 1 FROM creatures o
                                       WHERE o.id = b.origin_creature_id
                                         AND o.death_tick IS NOT NULL)
                         THEN 1 ELSE 0 END)
         FROM beliefs b
         JOIN creatures h ON h.id = b.creature_id AND h.death_tick IS NULL
        WHERE b.world_id = ?1",
        [world],
        |r| Ok((r.get(0).unwrap_or(0), r.get(1).unwrap_or(0))),
    )?;
    Ok(if total == 0 { 0.0 } else { inherited as f64 / total as f64 })
}

// ------------------------------------------------------- population & survival

#[derive(Debug, Clone, Serialize)]
pub struct PopulationPoint {
    pub tick: i64,
    pub population: i64,
    pub births: i64,
    pub deaths: i64,
}

/// Population over time with births and deaths overlaid.
///
/// Bucketed rather than per-tick: a 20,000-tick run is 20,000 rows, and a chart
/// cannot show them. Births and deaths are summed within the bucket while
/// population is sampled at its end, because one is a rate and the other is a
/// level — averaging a level and summing a rate over the same window is how
/// bucketed charts usually go wrong.
pub fn population_series(conn: &Connection, world: i64, buckets: i64) -> Result<Vec<PopulationPoint>> {
    let last: i64 = conn
        .query_row("SELECT MAX(tick) FROM tick_stats WHERE world_id = ?1", [world], |r| r.get(0))
        .unwrap_or(0);
    let width = (last / buckets.max(1)).max(1);

    let mut stmt = conn.prepare(
        "SELECT (tick / ?2) * ?2 AS bucket,
                SUM(births), SUM(deaths),
                (SELECT population FROM tick_stats t2
                  WHERE t2.world_id = ?1 AND t2.tick / ?2 = t1.tick / ?2
                  ORDER BY t2.tick DESC LIMIT 1)
         FROM tick_stats t1
         WHERE world_id = ?1
         GROUP BY bucket ORDER BY bucket",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, width], |r| {
        Ok(PopulationPoint {
            tick: r.get(0)?,
            births: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
            deaths: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            population: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct CauseByGeneration {
    pub generation: i64,
    pub cause: String,
    pub deaths: i64,
}

/// §10 calls this *the* diagnostic for whether the difficulty curve is working:
/// starvation giving way to old age as a community learns to farm is the
/// clearest evidence it is learning anything at all.
pub fn cause_of_death_by_generation(conn: &Connection, world: i64) -> Result<Vec<CauseByGeneration>> {
    let mut stmt = conn.prepare(
        "SELECT generation, death_cause, COUNT(*)
         FROM creatures
         WHERE world_id = ?1 AND death_cause IS NOT NULL
         GROUP BY generation, death_cause
         ORDER BY generation, COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(CauseByGeneration { generation: r.get(0)?, cause: r.get(1)?, deaths: r.get(2)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct AgeBucket {
    pub from_ticks: i64,
    pub deaths: i64,
}

/// Age at death against the 672-tick baseline.
pub fn age_at_death(conn: &Connection, world: i64, bucket: i64) -> Result<Vec<AgeBucket>> {
    let mut stmt = conn.prepare(
        "SELECT ((death_tick - birth_tick) / ?2) * ?2 AS b, COUNT(*)
         FROM creatures WHERE world_id = ?1 AND death_tick IS NOT NULL
         GROUP BY b ORDER BY b",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, bucket.max(1)], |r| {
        Ok(AgeBucket { from_ticks: r.get(0)?, deaths: r.get(1)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------------- lineage

#[derive(Debug, Clone, Serialize)]
pub struct LineageRow {
    pub founder_id: i64,
    pub founder_name: String,
    pub depth: i64,
    pub descendants: i64,
    pub living_descendants: i64,
    pub founder_alive: bool,
}

/// The leaderboard, and the answer to "which founders still have living
/// descendants".
///
/// One recursive CTE over `mother_id`/`father_id` — invariant 6. Depth is
/// capped at 24 generations, which is more than a 672-tick lifespan can reach
/// in any run anybody will sit through, and stops a cycle from a corrupt
/// parent link turning a report into an infinite loop.
pub fn deepest_lineages(conn: &Connection, world: i64, limit: i64) -> Result<Vec<LineageRow>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE descent(founder, id, depth) AS (
             SELECT id, id, 0 FROM creatures WHERE world_id = ?1 AND generation = 1
             UNION
             SELECT d.founder, c.id, d.depth + 1
               FROM creatures c JOIN descent d
                 ON c.mother_id = d.id OR c.father_id = d.id
              WHERE c.world_id = ?1 AND d.depth < 24
         )
         SELECT d.founder,
                f.name,
                MAX(d.depth),
                COUNT(*) - 1,
                SUM(CASE WHEN c.death_tick IS NULL THEN 1 ELSE 0 END),
                MAX(CASE WHEN f.death_tick IS NULL THEN 1 ELSE 0 END)
           FROM descent d
           JOIN creatures f ON f.id = d.founder
           JOIN creatures c ON c.id = d.id
          GROUP BY d.founder
          ORDER BY MAX(d.depth) DESC, COUNT(*) DESC
          LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, limit], |r| {
        Ok(LineageRow {
            founder_id: r.get(0)?,
            founder_name: r.get(1)?,
            depth: r.get(2)?,
            descendants: r.get(3)?,
            living_descendants: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            founder_alive: r.get::<_, i64>(5)? == 1,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct TreeNode {
    pub id: i64,
    pub name: String,
    pub generation: i64,
    pub birth_tick: i64,
    pub death_tick: Option<i64>,
    pub death_cause: Option<String>,
    pub mother_id: Option<i64>,
    pub father_id: Option<i64>,
    pub children: i64,
}

/// Everybody descended from one creature, for the interactive tree.
pub fn lineage_tree(conn: &Connection, world: i64, founder: i64) -> Result<Vec<TreeNode>> {
    let mut stmt = conn.prepare(
        "WITH RECURSIVE descent(id, depth) AS (
             SELECT ?2, 0
             UNION
             SELECT c.id, d.depth + 1
               FROM creatures c JOIN descent d
                 ON c.mother_id = d.id OR c.father_id = d.id
              WHERE c.world_id = ?1 AND d.depth < 24
         )
         SELECT c.id, c.name, c.generation, c.birth_tick, c.death_tick, c.death_cause,
                c.mother_id, c.father_id,
                (SELECT COUNT(*) FROM creatures k
                  WHERE k.world_id = ?1 AND (k.mother_id = c.id OR k.father_id = c.id))
           FROM creatures c JOIN descent d ON d.id = c.id
          WHERE c.world_id = ?1
          ORDER BY c.generation, c.birth_tick, c.id",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, founder], |r| {
        Ok(TreeNode {
            id: r.get(0)?,
            name: r.get(1)?,
            generation: r.get(2)?,
            birth_tick: r.get(3)?,
            death_tick: r.get(4)?,
            death_cause: r.get(5)?,
            mother_id: r.get(6)?,
            father_id: r.get(7)?,
            children: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct GenerationRow {
    pub generation: i64,
    pub born: i64,
    pub living: i64,
    pub median_life: i64,
    pub reached_adulthood: f64,
    /// Trait drift (§4.9): the clearest evidence selection is operating.
    pub boldness: f64,
    pub industry: f64,
    pub sociability: f64,
    pub caution: f64,
}

/// One row per generation: survival and the trait means that drift across them.
///
/// Traits are stored as JSON on the creature, so they are extracted in SQL
/// rather than pulled into Rust — a run with 40,000 creatures should not have
/// to deserialise 40,000 blobs to draw four lines.
pub fn by_generation(conn: &Connection, world: i64) -> Result<Vec<GenerationRow>> {
    let mut stmt = conn.prepare(
        "SELECT generation,
                COUNT(*),
                SUM(CASE WHEN death_tick IS NULL THEN 1 ELSE 0 END),
                AVG(json_extract(traits_json, '$.boldness')),
                AVG(json_extract(traits_json, '$.industry')),
                AVG(json_extract(traits_json, '$.sociability')),
                AVG(json_extract(traits_json, '$.caution')),
                SUM(CASE WHEN death_tick IS NULL OR death_tick - birth_tick >= 168
                         THEN 1 ELSE 0 END)
         FROM creatures WHERE world_id = ?1
         GROUP BY generation ORDER BY generation",
    )?;
    let rows = stmt.query_map([world], |r| {
        let born: i64 = r.get(1)?;
        let grown: i64 = r.get(7)?;
        Ok(GenerationRow {
            generation: r.get(0)?,
            born,
            living: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            median_life: 0,
            reached_adulthood: if born == 0 { 0.0 } else { grown as f64 / born as f64 },
            boldness: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            industry: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            sociability: r.get::<_, Option<f64>>(5)?.unwrap_or(0.0),
            caution: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
        })
    })?;
    let mut out = rows.collect::<Result<Vec<_>, _>>()?;

    // Median needs its own pass; SQLite has no percentile function.
    for row in out.iter_mut() {
        row.median_life = conn
            .query_row(
                "SELECT death_tick - birth_tick FROM creatures
                  WHERE world_id = ?1 AND generation = ?2 AND death_tick IS NOT NULL
                  ORDER BY death_tick - birth_tick
                  LIMIT 1 OFFSET (SELECT COUNT(*) / 2 FROM creatures
                                   WHERE world_id = ?1 AND generation = ?2
                                     AND death_tick IS NOT NULL)",
                rusqlite::params![world, row.generation],
                |r| r.get(0),
            )
            .unwrap_or(0);
    }
    Ok(out)
}

// ------------------------------------------------------------------- economy

#[derive(Debug, Clone, Serialize)]
pub struct EconomyPoint {
    pub tick: i64,
    pub gathered: f64,
    pub harvested: f64,
    pub eaten: f64,
    pub spoiled: f64,
    pub planted: i64,
    pub chopped: f64,
}

/// Production against consumption, and what rotted on the way.
///
/// High forage waste means creatures are over-gathering perishables instead of
/// investing in crops (§10) — which, because only grain reaches the
/// reproduction reserve, is the same thing as a community with no future.
///
/// Quantities live in the event payload as `key=value` text, so they are pulled
/// out here rather than stored twice.
pub fn economy_series(conn: &Connection, world: i64, buckets: i64) -> Result<Vec<EconomyPoint>> {
    let last: i64 = conn
        .query_row("SELECT MAX(tick) FROM events WHERE world_id = ?1", [world], |r| r.get(0))
        .unwrap_or(0);
    let width = (last / buckets.max(1)).max(1);

    let qty = payload_num("qty");

    let sql = format!(
        "SELECT (tick / ?2) * ?2 AS bucket,
                SUM(CASE WHEN kind = 'GATHERED'  THEN {qty} ELSE 0 END),
                SUM(CASE WHEN kind = 'HARVESTED' THEN {qty} ELSE 0 END),
                SUM(CASE WHEN kind = 'ATE'       THEN {qty} ELSE 0 END),
                SUM(CASE WHEN kind = 'SPOILED'   THEN {qty} ELSE 0 END),
                SUM(CASE WHEN kind = 'PLANTED'   THEN 1 ELSE 0 END),
                SUM(CASE WHEN kind = 'CHOPPED'   THEN {qty} ELSE 0 END)
         FROM events
         WHERE world_id = ?1
           AND kind IN ('GATHERED','HARVESTED','ATE','SPOILED','PLANTED','CHOPPED')
         GROUP BY bucket ORDER BY bucket"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![world, width], |r| {
        Ok(EconomyPoint {
            tick: r.get(0)?,
            gathered: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            harvested: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            eaten: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
            spoiled: r.get::<_, Option<f64>>(4)?.unwrap_or(0.0),
            planted: r.get::<_, Option<i64>>(5)?.unwrap_or(0),
            chopped: r.get::<_, Option<f64>>(6)?.unwrap_or(0.0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct FarmingRow {
    pub generation: i64,
    pub creatures: i64,
    pub planted: i64,
    pub harvested: i64,
    pub share_who_farmed: f64,
}

/// Farming adoption by generation. With spoilage in play this is effectively a
/// reproduction forecast (§10): only grain reaches the household reserve, so a
/// generation that does not farm is a generation that cannot breed.
pub fn farming_adoption(conn: &Connection, world: i64) -> Result<Vec<FarmingRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.generation,
                COUNT(DISTINCT c.id),
                SUM(CASE WHEN e.kind = 'PLANTED'   THEN 1 ELSE 0 END),
                SUM(CASE WHEN e.kind = 'HARVESTED' THEN 1 ELSE 0 END),
                COUNT(DISTINCT CASE WHEN e.kind IN ('PLANTED','HARVESTED')
                                    THEN c.id END)
         FROM creatures c
         LEFT JOIN events e
                ON e.world_id = c.world_id AND e.actor_id = c.id
               AND e.kind IN ('PLANTED','HARVESTED')
         WHERE c.world_id = ?1
         GROUP BY c.generation ORDER BY c.generation",
    )?;
    let rows = stmt.query_map([world], |r| {
        let creatures: i64 = r.get(1)?;
        let farmers: i64 = r.get::<_, Option<i64>>(4)?.unwrap_or(0);
        Ok(FarmingRow {
            generation: r.get(0)?,
            creatures,
            planted: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
            harvested: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
            share_who_farmed: if creatures == 0 {
                0.0
            } else {
                farmers as f64 / creatures as f64
            },
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// -------------------------------------------------------- deliberation & S6

#[derive(Debug, Clone, Serialize)]
pub struct TierAction {
    pub goal: String,
    pub tier1: i64,
    pub tier2: i64,
}

/// **The single best early warning that the LLM has stopped mattering** (§10).
///
/// If these two distributions converge, S6 is failing: the model is producing
/// what the deterministic policy would have produced anyway, and the whole
/// premise of the project is decorative.
pub fn action_distribution_by_tier(conn: &Connection, world: i64) -> Result<Vec<TierAction>> {
    let mut stmt = conn.prepare(
        "SELECT json_extract(parsed_plan_json, '$.goal') AS goal,
                SUM(CASE WHEN tier = 1 THEN 1 ELSE 0 END),
                SUM(CASE WHEN tier = 2 AND fallback_used = 0 THEN 1 ELSE 0 END)
         FROM decisions
         WHERE world_id = ?1 AND goal IS NOT NULL AND goal <> 'FALLBACK'
         GROUP BY goal ORDER BY SUM(1) DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(TierAction {
            goal: r.get::<_, Option<String>>(0)?.unwrap_or_default(),
            tier1: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
            tier2: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct DeliberationPoint {
    pub tick: i64,
    pub calls: i64,
    pub fallbacks: i64,
    pub fallback_rate: f64,
    pub mean_latency_ms: f64,
}

/// Fallback rate over time. §5.8 and invariant 8 both say to treat a rise as a
/// defect rather than as noise.
pub fn deliberation_series(
    conn: &Connection,
    world: i64,
    buckets: i64,
) -> Result<Vec<DeliberationPoint>> {
    let last: i64 = conn
        .query_row("SELECT MAX(tick) FROM tick_stats WHERE world_id = ?1", [world], |r| r.get(0))
        .unwrap_or(0);
    let width = (last / buckets.max(1)).max(1);

    let mut stmt = conn.prepare(
        "SELECT (tick / ?2) * ?2 AS bucket, SUM(llm_calls), SUM(fallbacks),
                AVG(mean_latency_ms)
         FROM tick_stats WHERE world_id = ?1
         GROUP BY bucket ORDER BY bucket",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, width], |r| {
        let calls: i64 = r.get::<_, Option<i64>>(1)?.unwrap_or(0);
        let fallbacks: i64 = r.get::<_, Option<i64>>(2)?.unwrap_or(0);
        Ok(DeliberationPoint {
            tick: r.get(0)?,
            calls,
            fallbacks,
            fallback_rate: if calls == 0 { 0.0 } else { fallbacks as f64 / calls as f64 },
            mean_latency_ms: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct HorizonRow {
    pub tier: i64,
    pub committed: f64,
    pub actual: f64,
    pub plans: i64,
}

/// The abandonment gap (§5.5). A population that routinely commits to twenty
/// ticks and aborts at four is telling you the horizon estimate is bad, which
/// is a prompt problem with a clear fix.
pub fn horizon_gap(conn: &Connection, world: i64) -> Result<Vec<HorizonRow>> {
    let mut stmt = conn.prepare(
        "SELECT tier, AVG(horizon_committed), AVG(horizon_actual), COUNT(*)
         FROM decisions
         WHERE world_id = ?1 AND horizon_actual IS NOT NULL
         GROUP BY tier ORDER BY tier",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(HorizonRow {
            tier: r.get(0)?,
            committed: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            actual: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            plans: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct NamedCount {
    pub name: String,
    pub count: i64,
}

/// Why plans ended: "the world changed" against "the plan was bad" (§10).
pub fn abort_reasons(conn: &Connection, world: i64) -> Result<Vec<NamedCount>> {
    let mut stmt = conn.prepare(
        "SELECT abort_reason, COUNT(*) FROM decisions
         WHERE world_id = ?1 AND abort_reason IS NOT NULL
         GROUP BY abort_reason ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(NamedCount { name: r.get(0)?, count: r.get(1)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Why model answers were unusable. A rising total here is invariant 8's alarm.
pub fn fallback_reasons(conn: &Connection, world: i64) -> Result<Vec<NamedCount>> {
    let mut stmt = conn.prepare(
        "SELECT fallback_reason, COUNT(*) FROM decisions
         WHERE world_id = ?1 AND tier = 2 AND fallback_reason IS NOT NULL
         GROUP BY fallback_reason ORDER BY COUNT(*) DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(NamedCount { name: r.get(0)?, count: r.get(1)? })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------------- culture

#[derive(Debug, Clone, Serialize)]
pub struct TransmissionRow {
    pub channel: String,
    pub events: i64,
    pub beliefs: i64,
}

pub fn transmission_by_channel(conn: &Connection, world: i64) -> Result<Vec<TransmissionRow>> {
    let mut stmt = conn.prepare(
        "SELECT channel, COUNT(*), SUM(belief_count) FROM transmissions
         WHERE world_id = ?1 GROUP BY channel ORDER BY SUM(belief_count) DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(TransmissionRow {
            channel: r.get(0)?,
            events: r.get(1)?,
            beliefs: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct BeliefProvenance {
    pub hops: i64,
    pub beliefs: i64,
    pub mean_confidence: f64,
    pub from_the_dead: i64,
}

/// Belief provenance by hop count, and how much of it came from the dead.
///
/// The two columns together are S7: knowledge that outlives its discoverer has
/// to have travelled, and how far it travelled is how much it degraded.
pub fn belief_provenance(conn: &Connection, world: i64) -> Result<Vec<BeliefProvenance>> {
    let mut stmt = conn.prepare(
        "SELECT b.hops, COUNT(*), AVG(b.confidence),
                SUM(CASE WHEN b.origin_creature_id IS NOT NULL
                          AND b.origin_creature_id <> b.creature_id
                          AND EXISTS (SELECT 1 FROM creatures o
                                       WHERE o.id = b.origin_creature_id
                                         AND o.death_tick IS NOT NULL)
                         THEN 1 ELSE 0 END)
         FROM beliefs b WHERE b.world_id = ?1
         GROUP BY b.hops ORDER BY b.hops",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(BeliefProvenance {
            hops: r.get(0)?,
            beliefs: r.get(1)?,
            mean_confidence: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            from_the_dead: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------ one whole life

#[derive(Debug, Clone, Serialize)]
pub struct LifeEvent {
    pub tick: i64,
    pub kind: String,
    pub target_id: Option<i64>,
    pub x: Option<i64>,
    pub y: Option<i64>,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LifeDecision {
    pub tick: i64,
    pub tier: i64,
    pub goal: String,
    pub rationale: String,
    pub horizon_committed: Option<i64>,
    pub horizon_actual: Option<i64>,
    pub abort_reason: Option<String>,
    pub fallback_used: bool,
    pub fallback_reason: Option<String>,
    pub latency_ms: Option<i64>,
    pub prompt_text: Option<String>,
    pub raw_response: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LifeSample {
    pub tick: i64,
    pub hunger: f64,
    pub thirst: f64,
    pub fatigue: f64,
    pub warmth: f64,
    pub health: f64,
}

/// One creature's entire life, reconstructed from the database (S5).
///
/// This is the criterion, not an illustration of it: "any creature's full life —
/// every decision, prompt, and outcome — is reconstructable from the DB". If
/// this function cannot produce it, S5 fails, which is why it is a query rather
/// than something the UI assembles from whatever it happens to have in memory.
#[derive(Debug, Clone, Serialize)]
pub struct Life {
    pub id: i64,
    pub name: String,
    pub sex: String,
    pub generation: i64,
    pub birth_tick: i64,
    pub death_tick: Option<i64>,
    pub death_cause: Option<String>,
    pub mother: Option<(i64, String)>,
    pub father: Option<(i64, String)>,
    pub children: Vec<(i64, String)>,
    pub lifespan_modifier: f64,
    pub events: Vec<LifeEvent>,
    pub decisions: Vec<LifeDecision>,
    pub samples: Vec<LifeSample>,
    pub beliefs_found: i64,
    pub still_circulating: i64,
    pub taught_count: i64,
    pub shared_count: i64,
}

pub fn life(conn: &Connection, world: i64, id: i64) -> Result<Option<Life>> {
    let base = conn
        .query_row(
            "SELECT name, sex, generation, birth_tick, death_tick, death_cause,
                    mother_id, father_id, lifespan_modifier, taught_count, shared_count
             FROM creatures WHERE world_id = ?1 AND id = ?2",
            rusqlite::params![world, id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, Option<i64>>(6)?,
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, f64>(8)?,
                    r.get::<_, i64>(9)?,
                    r.get::<_, i64>(10)?,
                ))
            },
        )
        .ok();
    let Some((name, sex, generation, birth_tick, death_tick, death_cause, mother_id, father_id,
              lifespan_modifier, taught_count, shared_count)) = base
    else {
        return Ok(None);
    };

    let named = |who: Option<i64>| -> Option<(i64, String)> {
        let w = who?;
        conn.query_row(
            "SELECT id, name FROM creatures WHERE world_id = ?1 AND id = ?2",
            rusqlite::params![world, w],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    };

    let mut kids = conn.prepare(
        "SELECT id, name FROM creatures
         WHERE world_id = ?1 AND (mother_id = ?2 OR father_id = ?2)
         ORDER BY birth_tick",
    )?;
    let children: Vec<(i64, String)> = kids
        .query_map(rusqlite::params![world, id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut ev = conn.prepare(
        "SELECT tick, kind, target_id, x, y, payload_json FROM events
         WHERE world_id = ?1 AND actor_id = ?2 ORDER BY tick, id",
    )?;
    let events: Vec<LifeEvent> = ev
        .query_map(rusqlite::params![world, id], |r| {
            Ok(LifeEvent {
                tick: r.get(0)?,
                kind: r.get(1)?,
                target_id: r.get(2)?,
                x: r.get(3)?,
                y: r.get(4)?,
                payload: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Every decision, with its prompt and the raw response (§5.8, invariant 4).
    let mut de = conn.prepare(
        "SELECT tick, tier,
                COALESCE(json_extract(parsed_plan_json, '$.goal'), ''),
                COALESCE(json_extract(parsed_plan_json, '$.rationale'), ''),
                horizon_committed, horizon_actual, abort_reason,
                fallback_used, fallback_reason, latency_ms, prompt_text, raw_response
         FROM decisions WHERE world_id = ?1 AND creature_id = ?2 ORDER BY tick, id",
    )?;
    let decisions: Vec<LifeDecision> = de
        .query_map(rusqlite::params![world, id], |r| {
            Ok(LifeDecision {
                tick: r.get(0)?,
                tier: r.get(1)?,
                goal: r.get(2)?,
                rationale: r.get(3)?,
                horizon_committed: r.get(4)?,
                horizon_actual: r.get(5)?,
                abort_reason: r.get(6)?,
                fallback_used: r.get::<_, i64>(7)? == 1,
                fallback_reason: r.get(8)?,
                latency_ms: r.get(9)?,
                prompt_text: r.get(10)?,
                raw_response: r.get(11)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut sa = conn.prepare(
        "SELECT tick, hunger, thirst, fatigue, warmth, health FROM creature_samples
         WHERE world_id = ?1 AND creature_id = ?2 ORDER BY tick",
    )?;
    let samples: Vec<LifeSample> = sa
        .query_map(rusqlite::params![world, id], |r| {
            Ok(LifeSample {
                tick: r.get(0)?,
                hunger: r.get(1)?,
                thirst: r.get(2)?,
                fatigue: r.get(3)?,
                warmth: r.get(4)?,
                health: r.get(5)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // What this creature found, and how much of it is still known by anybody.
    // The second number is S7 at the scale of one life: did what they learned
    // outlive them?
    let beliefs_found: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT kind || ':' || x || ',' || y) FROM beliefs
             WHERE world_id = ?1 AND origin_creature_id = ?2",
            rusqlite::params![world, id],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let still_circulating: i64 = conn
        .query_row(
            "SELECT COUNT(DISTINCT b.kind || ':' || b.x || ',' || b.y)
             FROM beliefs b JOIN creatures c ON c.id = b.creature_id
             WHERE b.world_id = ?1 AND b.origin_creature_id = ?2
               AND b.creature_id <> ?2 AND c.death_tick IS NULL",
            rusqlite::params![world, id],
            |r| r.get(0),
        )
        .unwrap_or(0);

    Ok(Some(Life {
        id,
        name,
        sex,
        generation,
        birth_tick,
        death_tick,
        death_cause,
        mother: named(mother_id),
        father: named(father_id),
        children,
        lifespan_modifier,
        events,
        decisions,
        samples,
        beliefs_found,
        still_circulating,
        taught_count,
        shared_count,
    }))
}

/// Everybody, for the creature picker and the CSV export.
#[derive(Debug, Clone, Serialize)]
pub struct Roster {
    pub id: i64,
    pub name: String,
    pub generation: i64,
    pub birth_tick: i64,
    pub death_tick: Option<i64>,
    pub death_cause: Option<String>,
    pub children: i64,
}

pub fn roster(conn: &Connection, world: i64, limit: i64) -> Result<Vec<Roster>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.generation, c.birth_tick, c.death_tick, c.death_cause,
                (SELECT COUNT(*) FROM creatures k
                  WHERE k.world_id = ?1 AND (k.mother_id = c.id OR k.father_id = c.id))
         FROM creatures c WHERE c.world_id = ?1
         ORDER BY c.generation DESC, c.birth_tick DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, limit], |r| {
        Ok(Roster {
            id: r.get(0)?,
            name: r.get(1)?,
            generation: r.get(2)?,
            birth_tick: r.get(3)?,
            death_tick: r.get(4)?,
            death_cause: r.get(5)?,
            children: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorldConfig;
    use crate::sim::tick::{Sim, TickReport};

    /// A short run with persistence on, so the queries are exercised against a
    /// database a real simulation wrote rather than one a test hand-built.
    fn run(ticks: i64) -> (Connection, i64) {
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        // A floor, so there is a second generation to have a lineage.
        cfg.bench.maintain_population = Some(40);
        cfg.persistence.sample_interval_ticks = 12;

        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrate(&conn).unwrap();

        let world = crate::sim::worldgen::generate(44127, &cfg).world;
        crate::db::repo::create_world(&conn, "Report", 44127, &cfg).unwrap();
        crate::db::repo::save_world(&mut conn, 1, &world).unwrap();

        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_population(40);
        for _ in 0..ticks {
            let mut r = sim.step();
            sim.persist(&mut conn, &mut r, false).unwrap();
        }
        // A forced checkpoint, as a save would do, so nothing is only in RAM.
        let mut last = TickReport { tick: sim.tick, ..Default::default() };
        sim.persist(&mut conn, &mut last, true).unwrap();
        (conn, 1)
    }

    #[test]
    fn the_economy_records_what_was_eaten_and_not_only_what_was_gathered() {
        // This is the test that was missing. `economy_series` had a passing
        // test and returned `eaten = 0` for every tick of every run ever made,
        // because ATE is collapsed by `collapse_routine_events` and the
        // collapse discarded the quantity along with the row. A report that is
        // structurally incapable of being non-zero will never fail an
        // assertion about its shape — only one about its content.
        let (conn, w) = run(500);
        let econ = economy_series(&conn, w, 20).unwrap();
        assert!(!econ.is_empty());

        let eaten: f64 = econ.iter().map(|p| p.eaten).sum();
        let gathered: f64 = econ.iter().map(|p| p.gathered).sum();
        assert!(gathered > 0.0, "500 ticks with nothing gathered is not a run");
        assert!(
            eaten > 0.0,
            "creatures that never eat still starve; consumption cannot be zero \
             across {} buckets when {gathered:.1} units were gathered",
            econ.len()
        );
    }

    #[test]
    fn the_headline_reports_the_generation_reached_not_the_lineage_depth() {
        let (conn, w) = run(500);
        let h = headline(&conn, w).unwrap();
        let max_gen: i64 = conn
            .query_row(
                "SELECT MAX(generation) FROM creatures WHERE world_id = ?1",
                [w],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            h.deepest_generation, max_gen,
            "founders are generation 1, so a headline of 0 is an off-by-one \
             against the number M4 is graded on"
        );
        assert!(h.baseline_ticks > 0, "the baseline comes from the world's own config");
    }

    #[test]
    fn the_headline_numbers_describe_the_run_that_happened() {
        let (conn, w) = run(700);
        let h = headline(&conn, w).unwrap();

        assert!(h.total_born > 40, "settlers and births should both count");
        assert!(h.total_dead > 0, "700 ticks is longer than some creatures live");
        assert_eq!(h.living, h.total_born - h.total_dead);
        assert!(h.through_tick > 0);
        assert!(
            h.median_life_ticks > 0 && h.median_life_ticks < 900,
            "a median life of {} is not a life",
            h.median_life_ticks
        );
        assert!((0.0..=1.0).contains(&h.infant_mortality));
    }

    #[test]
    fn cause_of_death_is_broken_out_by_generation() {
        // §10 calls this *the* diagnostic for whether the difficulty curve is
        // working.
        let (conn, w) = run(700);
        let rows = cause_of_death_by_generation(&conn, w).unwrap();

        assert!(!rows.is_empty(), "nobody died in 700 ticks?");
        assert!(rows.iter().all(|r| r.deaths > 0));
        assert!(
            rows.iter().map(|r| &r.cause).collect::<std::collections::BTreeSet<_>>().len() > 1,
            "one cause of death is a degenerate simulation, not a report"
        );
        let total: i64 = rows.iter().map(|r| r.deaths).sum();
        let dead = count(
            &conn,
            "SELECT COUNT(*) FROM creatures WHERE world_id = ?1 AND death_cause IS NOT NULL",
            w,
        )
        .unwrap();
        assert_eq!(total, dead, "every death must appear exactly once");
    }

    #[test]
    fn lineage_is_derived_from_parentage_and_nothing_else() {
        // Invariant 6. The tree is a recursive CTE over mother_id/father_id;
        // there is no stored structure that could disagree with the creatures.
        let (conn, w) = run(900);
        let board = deepest_lineages(&conn, w, 10).unwrap();
        assert!(!board.is_empty(), "no founders?");

        for row in &board {
            assert!(row.depth >= 0);
            assert!(row.living_descendants <= row.descendants + 1);
        }
        // Sorted deepest first, which is what makes it a leaderboard.
        assert!(board.windows(2).all(|w| w[0].depth >= w[1].depth));

        // A founder with descendants must produce a tree containing them.
        if let Some(top) = board.iter().find(|r| r.descendants > 0) {
            let tree = lineage_tree(&conn, w, top.founder_id).unwrap();
            assert!(tree.len() > 1, "a lineage of one is not a lineage");
            assert_eq!(tree[0].id, top.founder_id, "the founder is the root");
            assert!(
                tree.iter().skip(1).all(|n| n.mother_id.is_some() || n.father_id.is_some()),
                "everybody below the root has a parent"
            );
        }
    }

    #[test]
    fn a_creature_with_no_descendants_is_a_lineage_of_one() {
        let (conn, w) = run(300);
        let board = deepest_lineages(&conn, w, 50).unwrap();
        for row in board.iter().filter(|r| r.descendants == 0) {
            assert_eq!(row.depth, 0, "no children means no depth");
        }
    }

    #[test]
    fn generations_carry_their_trait_means_for_the_drift_chart() {
        let (conn, w) = run(700);
        let gens = by_generation(&conn, w).unwrap();
        assert!(!gens.is_empty());
        for g in &gens {
            assert!(g.born > 0);
            assert!(
                (0.0..=1.0).contains(&g.boldness) && (0.0..=1.0).contains(&g.industry),
                "traits are 0..1, got boldness {} industry {}",
                g.boldness, g.industry
            );
            assert!((0.0..=1.0).contains(&g.reached_adulthood));
        }
    }

    #[test]
    fn population_births_and_deaths_line_up_with_the_creature_table() {
        let (conn, w) = run(600);
        let series = population_series(&conn, w, 20).unwrap();
        assert!(!series.is_empty());
        assert!(series.windows(2).all(|p| p[0].tick < p[1].tick), "buckets ascend");

        let charted: i64 = series.iter().map(|p| p.deaths).sum();
        let actual = count(
            &conn,
            "SELECT COUNT(*) FROM creatures WHERE world_id = ?1 AND death_tick IS NOT NULL",
            w,
        )
        .unwrap();
        assert_eq!(charted, actual, "the chart must not lose deaths in the buckets");
    }

    #[test]
    fn the_economy_reads_quantities_out_of_the_event_payloads() {
        let (conn, w) = run(600);
        let econ = economy_series(&conn, w, 15).unwrap();
        assert!(!econ.is_empty());
        assert!(
            econ.iter().any(|p| p.gathered > 0.0),
            "600 ticks with nothing gathered means the payload parsing is wrong"
        );
        assert!(econ.iter().all(|p| p.gathered >= 0.0 && p.eaten >= 0.0));
    }

    #[test]
    fn farming_adoption_is_reported_per_generation() {
        let (conn, w) = run(600);
        let rows = farming_adoption(&conn, w).unwrap();
        assert!(!rows.is_empty());
        for r in &rows {
            assert!((0.0..=1.0).contains(&r.share_who_farmed));
            assert!(r.creatures > 0);
        }
    }

    #[test]
    fn the_two_tiers_action_distributions_are_comparable() {
        // §10: if these converge, S6 is failing. The report has to be able to
        // show that, which means both columns must come from the same query.
        let (conn, w) = run(500);
        let rows = action_distribution_by_tier(&conn, w).unwrap();
        assert!(!rows.is_empty(), "no decisions were recorded at all");
        // With no model running, everything is tier 1 — and that is itself the
        // S6 control, so it must read correctly rather than as an empty chart.
        assert!(rows.iter().any(|r| r.tier1 > 0));
        assert!(rows.iter().all(|r| r.tier2 == 0), "no model was running");
    }

    #[test]
    fn plans_that_ended_early_are_attributable() {
        let (conn, w) = run(500);
        let reasons = abort_reasons(&conn, w).unwrap();
        assert!(!reasons.is_empty(), "no plan ended in 500 ticks?");
        assert!(reasons.windows(2).all(|r| r[0].count >= r[1].count), "ranked");

        let gap = horizon_gap(&conn, w).unwrap();
        assert!(!gap.is_empty());
        for row in &gap {
            assert!(row.plans > 0);
            assert!(row.committed > 0.0, "a plan with no horizon is not a commitment");
        }
    }

    #[test]
    fn belief_provenance_separates_firsthand_from_hearsay() {
        let (conn, w) = run(700);
        let rows = belief_provenance(&conn, w).unwrap();
        assert!(!rows.is_empty(), "nobody knows anything");
        assert!(rows.iter().any(|r| r.hops == 0), "somebody must have seen something");
        assert!(rows.iter().all(|r| (0.0..=1.0).contains(&r.mean_confidence)));
        assert!(rows.windows(2).all(|r| r[0].hops < r[1].hops), "ascending hops");
    }

    // ------------------------------------------------------------------- S5

    #[test]
    fn any_creatures_full_life_is_reconstructable_from_the_database() {
        // **This is success criterion S5**, and it has never been tested until
        // now. "Any creature's full life — every decision, prompt, and outcome —
        // is reconstructable from the DB."
        //
        // It is worth testing rather than assuming, because two decisions taken
        // for the sake of database size could plausibly have broken it: routine
        // events are collapsed into per-tick counts, and creature rows are
        // checkpointed rather than written every tick. §7 explicitly endorses
        // "events plus periodic sampling", so the question is whether what
        // survives is still a life.
        let (conn, w) = run(900);

        // Somebody who lived a while and then died: the hardest case, because
        // nothing about them is still in memory anywhere.
        let subject: i64 = conn
            .query_row(
                "SELECT id FROM creatures
                  WHERE world_id = ?1 AND death_tick IS NOT NULL
                    AND death_tick - birth_tick > 200
                  ORDER BY death_tick - birth_tick DESC LIMIT 1",
                [w],
                |r| r.get(0),
            )
            .expect("somebody should have lived more than 200 ticks");

        let life = life(&conn, w, subject).unwrap().expect("the life should be readable");

        assert_eq!(life.id, subject);
        assert!(!life.name.is_empty(), "a life needs a name");
        assert!(life.death_tick.is_some() && life.death_cause.is_some(),
                "how it ended is part of the record");

        // Every decision it ever made, with the reason it gave.
        assert!(
            !life.decisions.is_empty(),
            "a creature that lived 200+ ticks made decisions and none were kept"
        );
        assert!(
            life.decisions.iter().all(|d| !d.goal.is_empty()),
            "a decision with no goal is not reconstructable"
        );
        assert!(
            life.decisions.iter().any(|d| !d.rationale.is_empty()),
            "the reason a creature gave is what makes the record readable"
        );
        assert!(
            life.decisions.windows(2).all(|d| d[0].tick <= d[1].tick),
            "a life is in order"
        );

        // The state trajectory, from periodic sampling rather than per-tick
        // rows — the thing §7 trades away and S5 depends on surviving.
        assert!(
            life.samples.len() >= 5,
            "only {} samples across a {}-tick life: too coarse to be a trajectory",
            life.samples.len(),
            life.death_tick.unwrap() - life.birth_tick
        );

        // And the outcomes: what actually happened to it.
        assert!(!life.events.is_empty(), "nothing is recorded as having happened");
        assert!(
            life.events.iter().any(|e| e.kind == "BORN"),
            "a life starts with being born"
        );

        // Everything is within the lifetime, or the record belongs to somebody
        // else.
        let (from, to) = (life.birth_tick, life.death_tick.unwrap());
        assert!(
            life.decisions.iter().all(|d| d.tick >= from && d.tick <= to),
            "a decision outside the lifetime is somebody else's"
        );
        assert!(life.samples.iter().all(|s| s.tick >= from && s.tick <= to));
    }

    #[test]
    fn a_life_carries_its_kin_and_what_it_left_behind() {
        let (conn, w) = run(900);
        let with_kids: Option<i64> = conn
            .query_row(
                "SELECT mother_id FROM creatures
                  WHERE world_id = ?1 AND mother_id IS NOT NULL LIMIT 1",
                [w],
                |r| r.get(0),
            )
            .ok();

        if let Some(parent) = with_kids {
            let life = life(&conn, w, parent).unwrap().unwrap();
            assert!(!life.children.is_empty(), "a parent's children are part of their life");
        }

        // S7 at the scale of one creature: did what it found outlive it?
        let anyone: i64 = conn
            .query_row("SELECT id FROM creatures WHERE world_id = ?1 LIMIT 1", [w], |r| r.get(0))
            .unwrap();
        let life = life(&conn, w, anyone).unwrap().unwrap();
        assert!(life.still_circulating <= life.beliefs_found.max(0) + 1);
    }

    #[test]
    fn asking_about_a_creature_that_never_existed_is_not_an_error() {
        let (conn, w) = run(120);
        assert!(life(&conn, w, 999_999).unwrap().is_none());
    }

    #[test]
    fn the_roster_is_ordered_newest_generation_first() {
        let (conn, w) = run(600);
        let r = roster(&conn, w, 50).unwrap();
        assert!(!r.is_empty());
        assert!(r.windows(2).all(|x| x[0].generation >= x[1].generation));
    }
}
