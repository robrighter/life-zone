//! The knowledge, planning and selection reports (PRD §10).
//!
//! `queries.rs` answers "what happened". This answers "did the design work" —
//! the reports §10 calls "the most novel in the product", plus the ones the
//! success criteria are actually graded on:
//!
//! * **S6** — `deliberation_vs_lineage_depth`. If creatures who got more
//!   thinking early do not found deeper bloodlines, the LLM is decoration.
//! * **S7** — `knowledge_half_life` and `map_coverage`. Does what gen 1 found
//!   still get gen 5 to water?
//! * **§5.5** — `horizon_vs_lineage_depth`. Do planners out-survive reactors?
//!
//! Each of these is a correlation, not a proof, and none of them is worth
//! reading below a few hundred subjects. Where the sample is thin the row
//! carries its own `n` so the chart can say so rather than drawing a
//! confident-looking line through four points.

use anyhow::Result;
use rusqlite::Connection;
use serde::Serialize;

/// Ancestry as a CTE, reused by every lineage-depth correlation below.
///
/// Invariant 6: lineage is derived, never stored. Depth is the generation
/// distance from a creature down to its furthest descendant — the thing the
/// game is scored on (§1) — so it has to walk *down* the tree, not up.
const DESCENDANTS: &str = "
WITH RECURSIVE line(root, id, depth) AS (
    SELECT c.id, c.id, 0 FROM creatures c WHERE c.world_id = ?1
    UNION ALL
    SELECT l.root, k.id, l.depth + 1
      FROM creatures k JOIN line l
        ON k.mother_id = l.id OR k.father_id = l.id
     WHERE k.world_id = ?1 AND l.depth < 24
),
depth_of AS (
    SELECT root AS id, MAX(depth) AS depth, COUNT(*) - 1 AS descendants
      FROM line GROUP BY root
)";

// ------------------------------------------------------------------ knowledge

#[derive(Debug, Clone, Serialize)]
pub struct CoveragePoint {
    pub tick: i64,
    pub known_tiles: i64,
    pub share_of_world: f64,
    pub population: i64,
    /// Tiles known per living creature. A rising total with a falling ratio is
    /// a community coasting on what a few well-travelled elders remember.
    pub per_capita: f64,
}

/// Collective known-map coverage over time (§10).
///
/// > "Expect a ragged expansion that stalls or collapses when a knowledgeable
/// > lineage dies out."
///
/// Sampled, not continuous: `tick_stats.known_tiles` is NULL on unsampled ticks
/// (see migration 004), and those rows are dropped here rather than drawn as
/// zero — a gap in sampling is not a collapse in knowledge, and plotting it as
/// one would manufacture exactly the signal this chart exists to detect.
pub fn map_coverage(conn: &Connection, world: i64) -> Result<Vec<CoveragePoint>> {
    let tiles = super::queries::world_tiles(conn, world);

    let mut stmt = conn.prepare(
        "SELECT tick, known_tiles, population
           FROM tick_stats
          WHERE world_id = ?1 AND known_tiles IS NOT NULL
          ORDER BY tick",
    )?;
    let rows = stmt.query_map([world], |r| {
        let known: i64 = r.get(1)?;
        let pop: i64 = r.get(2)?;
        Ok(CoveragePoint {
            tick: r.get(0)?,
            known_tiles: known,
            share_of_world: known as f64 / tiles,
            population: pop,
            per_capita: if pop > 0 { known as f64 / pop as f64 } else { 0.0 },
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct HalfLifeRow {
    pub kind: String,
    /// Median ticks from first discovery to the last holder's death.
    pub median_ticks: i64,
    pub p90_ticks: i64,
    /// Belief lineages still in circulation — excluded from the median, since
    /// including them would only ever bias it downward.
    pub still_alive: i64,
    pub extinguished: i64,
}

/// Knowledge half-life (§10): how long a belief stays in circulation before
/// everyone holding it dies without teaching it.
///
/// A "belief lineage" is `(origin_creature_id, origin_tick, kind, x, y)` — the
/// pair §7 keeps precisely so that a fact can be followed across every
/// retelling. Dead creatures keep their belief rows, so a lineage is
/// extinguished when every holder is dead, not when the discoverer is.
pub fn knowledge_half_life(conn: &Connection, world: i64) -> Result<Vec<HalfLifeRow>> {
    let mut stmt = conn.prepare(
        "SELECT b.kind,
                b.origin_tick,
                MAX(CASE WHEN c.death_tick IS NULL THEN 1 ELSE 0 END) AS any_living,
                MAX(COALESCE(c.death_tick, 0))                        AS last_death
           FROM beliefs b
           JOIN creatures c ON c.id = b.creature_id
          WHERE b.world_id = ?1 AND b.origin_creature_id IS NOT NULL
          GROUP BY b.origin_creature_id, b.origin_tick, b.kind, b.x, b.y",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)? == 1,
            r.get::<_, i64>(3)?,
        ))
    })?;

    use std::collections::BTreeMap;
    let mut spans: BTreeMap<String, (Vec<i64>, i64)> = BTreeMap::new();
    for row in rows {
        let (kind, origin, living, last_death) = row?;
        let e = spans.entry(kind).or_insert_with(|| (Vec::new(), 0));
        if living {
            e.1 += 1;
        } else {
            e.0.push((last_death - origin).max(0));
        }
    }

    Ok(spans
        .into_iter()
        .map(|(kind, (mut lives, still_alive))| {
            lives.sort_unstable();
            let pick = |q: f64| -> i64 {
                if lives.is_empty() {
                    0
                } else {
                    lives[(((lives.len() - 1) as f64) * q).round() as usize]
                }
            };
            HalfLifeRow {
                kind,
                median_ticks: pick(0.5),
                p90_ticks: pick(0.9),
                still_alive,
                extinguished: lives.len() as i64,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct AccuracyRow {
    pub hops: i64,
    pub acted_on: i64,
    pub stale: i64,
    pub stale_rate: f64,
}

/// Belief accuracy by hop count (§10, §4.11).
///
/// The premise of the belief substrate is that secondhand knowledge is
/// genuinely worse, not just decorated with a lower number. If this comes back
/// flat, hop count is cosmetic and transmission is free — which would make the
/// culture layer a story the UI tells rather than a mechanic.
///
/// Reads `decisions.belief_hops` (migration 004), which is populated only on
/// aborts where the world contradicted a belief.
pub fn belief_accuracy(conn: &Connection, world: i64) -> Result<Vec<AccuracyRow>> {
    let mut stmt = conn.prepare(
        "SELECT belief_hops,
                COUNT(*),
                SUM(CASE WHEN abort_reason IN ('TARGET_GONE','TARGET_DEPLETED')
                         THEN 1 ELSE 0 END)
           FROM decisions
          WHERE world_id = ?1 AND belief_hops IS NOT NULL
          GROUP BY belief_hops ORDER BY belief_hops",
    )?;
    let rows = stmt.query_map([world], |r| {
        let acted: i64 = r.get(1)?;
        let stale: i64 = r.get(2)?;
        Ok(AccuracyRow {
            hops: r.get(0)?,
            acted_on: acted,
            stale,
            stale_rate: if acted > 0 { stale as f64 / acted as f64 } else { 0.0 },
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct TeachingRow {
    pub household_id: i64,
    pub members: i64,
    pub teaching_events: i64,
    pub beliefs_taught: i64,
    /// Teaching acts per member — the comparable number, since a large
    /// household will out-teach a small one on volume alone.
    pub per_member: f64,
    pub lineage_depth: i64,
    pub living_descendants: i64,
}

/// Teaching rate by household against lineage depth (§10).
///
/// > "This is the direct test of 'do lineages that teach out-survive those that
/// > don't'."
pub fn teaching_vs_depth(conn: &Connection, world: i64) -> Result<Vec<TeachingRow>> {
    let sql = format!(
        "{DESCENDANTS}
         SELECT c.household_id,
                COUNT(DISTINCT c.id),
                COALESCE(SUM(t.acts), 0),
                COALESCE(SUM(t.beliefs), 0),
                MAX(d.depth),
                SUM(CASE WHEN c.death_tick IS NULL THEN 1 ELSE 0 END)
           FROM creatures c
           LEFT JOIN depth_of d ON d.id = c.id
           LEFT JOIN (SELECT from_creature AS id,
                             COUNT(*)          AS acts,
                             SUM(belief_count) AS beliefs
                        FROM transmissions
                       WHERE world_id = ?1 AND channel = 'TEACH'
                       GROUP BY from_creature) t ON t.id = c.id
          WHERE c.world_id = ?1 AND c.household_id IS NOT NULL
          GROUP BY c.household_id
          ORDER BY 3 DESC"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([world], |r| {
        let members: i64 = r.get(1)?;
        let acts: i64 = r.get(2)?;
        Ok(TeachingRow {
            household_id: r.get(0)?,
            members,
            teaching_events: acts,
            beliefs_taught: r.get(3)?,
            per_member: if members > 0 { acts as f64 / members as f64 } else { 0.0 },
            lineage_depth: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            living_descendants: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[derive(Debug, Clone, Serialize)]
pub struct TransmissionEdge {
    pub from_id: i64,
    pub from_name: String,
    pub to_id: i64,
    pub to_name: String,
    pub channel: String,
    pub beliefs: i64,
    pub events: i64,
}

/// The transmission graph (§10) — "revealing whether information hubs emerge".
///
/// Capped, and sorted so the cap keeps the heaviest edges: a graph of every
/// overheard remark in a 2,000-tick run is tens of thousands of edges and
/// renders as a solid disc, which reveals nothing at all.
pub fn transmission_graph(conn: &Connection, world: i64, limit: i64) -> Result<Vec<TransmissionEdge>> {
    let mut stmt = conn.prepare(
        "SELECT t.from_creature, COALESCE(f.name, '?'),
                t.to_creature,   COALESCE(g.name, '?'),
                t.channel, SUM(t.belief_count), COUNT(*)
           FROM transmissions t
           LEFT JOIN creatures f ON f.id = t.from_creature
           LEFT JOIN creatures g ON g.id = t.to_creature
          WHERE t.world_id = ?1
          GROUP BY t.from_creature, t.to_creature, t.channel
          ORDER BY 6 DESC LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![world, limit], |r| {
        Ok(TransmissionEdge {
            from_id: r.get(0)?,
            from_name: r.get(1)?,
            to_id: r.get(2)?,
            to_name: r.get(3)?,
            channel: r.get(4)?,
            beliefs: r.get(5)?,
            events: r.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// -------------------------------------------------------- selection evidence

#[derive(Debug, Clone, Serialize)]
pub struct DepthBand {
    /// Bucketed rather than per-creature: the question is whether the trend
    /// exists, and 4,000 dots in a scatter answer that worse than six points.
    pub band: String,
    pub creatures: i64,
    pub mean: f64,
    pub median: f64,
    pub lineage_depth: f64,
    pub living_descendants: f64,
}

/// Lifetime deliberation count against lineage depth (§10) — **the S6 chart**.
///
/// > "Does a creature that got more thinking in early adulthood found a deeper
/// > bloodline? If yes, that is direct evidence for S6."
///
/// Restricted to creatures who reached adulthood, because otherwise this
/// measures infant mortality: an infant that died at tick 30 got no thinking
/// and founded nothing, and a thousand of those would fake a strong correlation
/// out of nothing but "the dead do not reproduce".
pub fn deliberation_vs_depth(conn: &Connection, world: i64) -> Result<Vec<DepthBand>> {
    banded(
        conn,
        world,
        "c.lifetime_deliberations",
        &[(0.0, "none"), (1.0, "1–4"), (5.0, "5–14"), (15.0, "15–39"), (40.0, "40+")],
    )
}

/// Committed horizon length against lineage depth (§5.5, §10).
///
/// > "The direct test of whether planners out-survive reactors."
pub fn horizon_vs_depth(conn: &Connection, world: i64) -> Result<Vec<DepthBand>> {
    banded(
        conn,
        world,
        "(SELECT AVG(horizon_committed) FROM decisions d
           WHERE d.world_id = ?1 AND d.creature_id = c.id
             AND d.horizon_committed IS NOT NULL)",
        &[(0.0, "1–3 ticks"), (4.0, "4–7"), (8.0, "8–15"), (16.0, "16+")],
    )
}

/// Shared machinery for the two correlation charts above.
fn banded(
    conn: &Connection,
    world: i64,
    measure: &str,
    bands: &[(f64, &str)],
) -> Result<Vec<DepthBand>> {
    let sql = format!(
        "{DESCENDANTS}
         SELECT {measure} AS m, COALESCE(d.depth, 0), COALESCE(d.descendants, 0),
                CASE WHEN c.death_tick IS NULL THEN 0 ELSE 1 END
           FROM creatures c
           LEFT JOIN depth_of d ON d.id = c.id
          WHERE c.world_id = ?1
            AND c.life_stage IN ('ADULT', 'ELDER')
            AND {measure} IS NOT NULL"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([world], |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)? as f64, r.get::<_, i64>(2)? as f64))
    })?;

    let mut buckets: Vec<(Vec<f64>, f64, f64)> =
        bands.iter().map(|_| (Vec::new(), 0.0, 0.0)).collect();
    for row in rows {
        let (m, depth, desc) = row?;
        // Last band whose floor the measure clears, so the table reads in the
        // order it is declared and an out-of-range high value lands in the top
        // band rather than being silently dropped.
        let idx = bands.iter().rposition(|(floor, _)| m >= *floor).unwrap_or(0);
        buckets[idx].0.push(m);
        buckets[idx].1 += depth;
        buckets[idx].2 += desc;
    }

    Ok(bands
        .iter()
        .zip(buckets)
        .map(|((_, label), (mut ms, depth, desc))| {
            ms.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            let n = ms.len().max(1) as f64;
            DepthBand {
                band: (*label).to_string(),
                creatures: ms.len() as i64,
                mean: ms.iter().sum::<f64>() / n,
                median: ms.get(ms.len() / 2).copied().unwrap_or(0.0),
                lineage_depth: depth / n,
                living_descendants: desc / n,
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct SurvivalPoint {
    pub generation: i64,
    /// Share of founding lineages still producing creatures at this depth.
    pub share_surviving: f64,
    pub lineages: i64,
}

/// Lineage survival curve (§10): how deep a bloodline typically gets before
/// extinction. Denominated in founders, so it starts at 1.0 by construction.
pub fn lineage_survival(conn: &Connection, world: i64) -> Result<Vec<SurvivalPoint>> {
    let sql = format!(
        "{DESCENDANTS}
         SELECT d.depth, COUNT(*)
           FROM depth_of d JOIN creatures c ON c.id = d.id
          WHERE c.world_id = ?1 AND c.mother_id IS NULL AND c.father_id IS NULL
          GROUP BY d.depth ORDER BY d.depth"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows: Vec<(i64, i64)> = stmt
        .query_map([world], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;

    let founders: i64 = rows.iter().map(|(_, n)| n).sum();
    if founders == 0 {
        return Ok(Vec::new());
    }
    let deepest = rows.iter().map(|(d, _)| *d).max().unwrap_or(0);
    Ok((0..=deepest)
        .map(|g| {
            // Survival at depth g = founders who reached *at least* g.
            let reached: i64 = rows.iter().filter(|(d, _)| *d >= g).map(|(_, n)| n).sum();
            SurvivalPoint {
                generation: g,
                share_surviving: reached as f64 / founders as f64,
                lineages: reached,
            }
        })
        .collect())
}

// ---------------------------------------------------------------- deliberation

#[derive(Debug, Clone, Serialize)]
pub struct StageCompute {
    pub life_stage: String,
    pub calls: i64,
    pub share_of_calls: f64,
    pub creatures: i64,
    pub mean_age_weight: f64,
    pub calls_per_creature: f64,
    /// Fatigue spent thinking as a share of that stage's total think-fatigue
    /// budget — §10's check that "the metabolic cost is biting without being
    /// crippling".
    pub think_fatigue: f64,
    pub crisis_exempt: i64,
}

/// Compute spent per life stage (§10, §5.4).
///
/// > "Confirms the §5.4 weighting is actually landing where intended."
pub fn compute_by_life_stage(conn: &Connection, world: i64) -> Result<Vec<StageCompute>> {
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM decisions WHERE world_id = ?1 AND tier = 2",
        [world],
        |r| r.get(0),
    )?;
    let total = total.max(1) as f64;

    let mut stmt = conn.prepare(
        "SELECT COALESCE(life_stage, 'UNKNOWN'),
                COUNT(*), COUNT(DISTINCT creature_id),
                AVG(COALESCE(age_weight, 0)), SUM(COALESCE(fatigue_cost, 0)),
                SUM(crisis_exempt)
           FROM decisions
          WHERE world_id = ?1 AND tier = 2
          GROUP BY life_stage",
    )?;
    let mut out: Vec<StageCompute> = stmt
        .query_map([world], |r| {
            let calls: i64 = r.get(1)?;
            let people: i64 = r.get(2)?;
            Ok(StageCompute {
                life_stage: r.get(0)?,
                calls,
                share_of_calls: calls as f64 / total,
                creatures: people,
                mean_age_weight: r.get(3)?,
                calls_per_creature: if people > 0 { calls as f64 / people as f64 } else { 0.0 },
                think_fatigue: r.get(4)?,
                crisis_exempt: r.get(5)?,
            })
        })?
        .collect::<Result<_, _>>()?;

    // Life order, not alphabetical: a stage chart that reads INFANT, ADULT,
    // ELDER, CHILD is unreadable regardless of what the numbers say.
    const ORDER: [&str; 4] = ["INFANT", "CHILD", "ADULT", "ELDER"];
    out.sort_by_key(|s| ORDER.iter().position(|o| *o == s.life_stage).unwrap_or(9));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct ElderRow {
    pub life_stage: String,
    pub creatures: i64,
    pub plans: i64,
    /// Share of plans that ran to completion rather than being abandoned.
    pub completion_rate: f64,
    /// Share of this stage's plans that came from a model call.
    pub call_share: f64,
}

/// §13.10 — do elders need any deliberation at all?
///
/// **This is not the "elder habit-prior hit rate" §10 asks for, and it cannot
/// be.** Writing that query is what turned up the reason: `ai::budget::
/// habit_bonus` exists, is tested, and is never called from anywhere, and
/// `Creature::habit` is initialised to zeros, serialised, and never
/// incremented. The elder habit prior is written but not wired in, so there is
/// no "plan produced without a call" path to measure a hit rate against.
///
/// What is measurable today is the question underneath it: elders run on Tier 1
/// far more than adults do, so if their plans complete at a comparable rate the
/// deterministic policy is already carrying them and elder weight can drop. If
/// they complete far worse, elders are decaying into uselessness and the prior
/// is not optional. Either reading is what §13.10 says it wants.
pub fn elder_autonomy(conn: &Connection, world: i64) -> Result<Vec<ElderRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(d.life_stage, 'UNKNOWN'),
                COUNT(DISTINCT d.creature_id), COUNT(*),
                AVG(CASE WHEN d.abort_reason = 'COMPLETED' THEN 1.0
                         WHEN d.abort_reason IS NULL       THEN NULL
                         ELSE 0.0 END),
                AVG(CASE WHEN d.tier = 2 THEN 1.0 ELSE 0.0 END)
           FROM decisions d
          WHERE d.world_id = ?1
          GROUP BY d.life_stage",
    )?;
    let mut out: Vec<ElderRow> = stmt
        .query_map([world], |r| {
            Ok(ElderRow {
                life_stage: r.get(0)?,
                creatures: r.get(1)?,
                plans: r.get(2)?,
                completion_rate: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
                call_share: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    const ORDER: [&str; 4] = ["INFANT", "CHILD", "ADULT", "ELDER"];
    out.sort_by_key(|s| ORDER.iter().position(|o| *o == s.life_stage).unwrap_or(9));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct PressureBand {
    pub band: String,
    pub creatures: i64,
    /// Calls per 100 ticks alive, so a creature that has been waiting a long
    /// time and one that just arrived are comparable.
    pub calls_per_100_ticks: f64,
}

/// Deliberation pressure distribution (§10, §13.6).
///
/// > "Who is getting attention and who is being starved of it."
///
/// §13.6 is worried the population splits into an observed "smart" class and a
/// background "dumb" one. That failure looks like a bimodal distribution here
/// long before it is visible on the map.
pub fn pressure_distribution(conn: &Connection, world: i64) -> Result<Vec<PressureBand>> {
    const BANDS: [(f64, &str); 5] =
        [(0.0, "0.0–0.2"), (0.2, "0.2–0.4"), (0.4, "0.4–0.6"), (0.6, "0.6–0.8"), (0.8, "0.8+")];

    let mut stmt = conn.prepare(
        "SELECT deliberation_pressure, lifetime_deliberations,
                COALESCE(death_tick, (SELECT MAX(tick) FROM tick_stats WHERE world_id = ?1))
                  - birth_tick
           FROM creatures
          WHERE world_id = ?1 AND life_stage IN ('ADULT', 'ELDER')",
    )?;
    let mut acc = vec![(0i64, 0f64); BANDS.len()];
    let rows = stmt.query_map([world], |r| {
        Ok((r.get::<_, f64>(0)?, r.get::<_, i64>(1)? as f64, r.get::<_, i64>(2)?.max(1) as f64))
    })?;
    for row in rows {
        let (p, calls, ticks) = row?;
        let i = BANDS.iter().rposition(|(f, _)| p >= *f).unwrap_or(0);
        acc[i].0 += 1;
        acc[i].1 += calls / ticks * 100.0;
    }
    Ok(BANDS
        .iter()
        .zip(acc)
        .map(|((_, label), (n, rate))| PressureBand {
            band: (*label).to_string(),
            creatures: n,
            calls_per_100_ticks: if n > 0 { rate / n as f64 } else { 0.0 },
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyRow {
    pub model: String,
    pub calls: i64,
    pub p50_ms: i64,
    pub p95_ms: i64,
    pub p99_ms: i64,
    pub max_ms: i64,
}

/// Latency distribution (§10). Percentiles, never a mean: on this hardware the
/// tail is the entire story, and an average latency hides the calls that came
/// back after their creature had already died.
pub fn latency(conn: &Connection, world: i64) -> Result<Vec<LatencyRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(model, 'unknown'), latency_ms
           FROM decisions
          WHERE world_id = ?1 AND latency_ms IS NOT NULL
          ORDER BY 1, 2",
    )?;
    let rows = stmt.query_map([world], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;

    use std::collections::BTreeMap;
    let mut by_model: BTreeMap<String, Vec<i64>> = BTreeMap::new();
    for row in rows {
        let (m, ms) = row?;
        by_model.entry(m).or_default().push(ms);
    }
    Ok(by_model
        .into_iter()
        .map(|(model, v)| {
            // Already sorted by the ORDER BY, per model.
            let at = |q: f64| v[(((v.len() - 1) as f64) * q).round() as usize];
            LatencyRow {
                model,
                calls: v.len() as i64,
                p50_ms: at(0.5),
                p95_ms: at(0.95),
                p99_ms: at(0.99),
                max_ms: *v.last().unwrap_or(&0),
            }
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct HorizonByGeneration {
    pub generation: i64,
    pub mean_committed: f64,
    pub mean_actual: f64,
    pub plans: i64,
}

/// Horizon length by generation (§10). Rising commitment across generations
/// with a stable gap is the shape that would mean planning is being selected
/// for; a widening gap means the model is learning to over-promise.
pub fn horizon_by_generation(conn: &Connection, world: i64) -> Result<Vec<HorizonByGeneration>> {
    let mut stmt = conn.prepare(
        "SELECT c.generation, AVG(d.horizon_committed), AVG(d.horizon_actual), COUNT(*)
           FROM decisions d JOIN creatures c ON c.id = d.creature_id
          WHERE d.world_id = ?1 AND d.horizon_committed IS NOT NULL
                AND d.horizon_actual IS NOT NULL
          GROUP BY c.generation ORDER BY c.generation",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(HorizonByGeneration {
            generation: r.get(0)?,
            mean_committed: r.get(1)?,
            mean_actual: r.get(2)?,
            plans: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// ------------------------------------------------------------------- behaviour

#[derive(Debug, Clone, Serialize)]
pub struct RoleRow {
    pub generation: i64,
    pub role: String,
    pub creatures: i64,
    pub share: f64,
}

/// Emergent role classification (§10).
///
/// A creature's role is whichever livelihood act it did most of over its life —
/// classified from the event log after the fact, never assigned. The point of
/// the chart is that nothing in the simulation has a job title: if the mix
/// shifts across generations, the population specialised on its own.
///
/// Creatures with no livelihood acts at all are `unspecialised` rather than
/// dropped, because "most of gen 4 never got as far as working" is the finding
/// on a run where the difficulty curve is wrong.
pub fn roles(conn: &Connection, world: i64) -> Result<Vec<RoleRow>> {
    let mut stmt = conn.prepare(
        "SELECT c.generation, c.id, e.kind, COUNT(*)
           FROM creatures c
           LEFT JOIN events e
             ON e.world_id = ?1 AND e.actor_id = c.id
            AND e.kind IN ('HARVESTED','PLANTED','TENDED','GATHERED','CHOPPED',
                           'SLAUGHTERED','FED_INFANT','TAUGHT','SHARED')
          WHERE c.world_id = ?1
          GROUP BY c.id, e.kind",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<String>>(2)?, r.get::<_, i64>(3)?))
    })?;

    use std::collections::BTreeMap;
    let mut per: BTreeMap<(i64, i64), BTreeMap<&'static str, i64>> = BTreeMap::new();
    for row in rows {
        let (gen, id, kind, n) = row?;
        let slot = per.entry((gen, id)).or_default();
        let role = match kind.as_deref() {
            Some("HARVESTED") | Some("PLANTED") | Some("TENDED") => "farmer",
            Some("GATHERED") => "forager",
            Some("CHOPPED") => "woodcutter",
            Some("SLAUGHTERED") => "shepherd",
            Some("FED_INFANT") => "caretaker",
            Some("TAUGHT") | Some("SHARED") => "teacher",
            _ => {
                slot.entry("unspecialised").or_insert(0);
                continue;
            }
        };
        *slot.entry(role).or_insert(0) += n;
    }

    let mut counts: BTreeMap<(i64, &'static str), i64> = BTreeMap::new();
    let mut totals: BTreeMap<i64, i64> = BTreeMap::new();
    for ((gen, _), acts) in per {
        let role = acts
            .iter()
            .filter(|(r, _)| **r != "unspecialised")
            .max_by_key(|(_, n)| **n)
            .map(|(r, _)| *r)
            .unwrap_or("unspecialised");
        *counts.entry((gen, role)).or_insert(0) += 1;
        *totals.entry(gen).or_insert(0) += 1;
    }

    Ok(counts
        .into_iter()
        .map(|((generation, role), creatures)| RoleRow {
            generation,
            role: role.to_string(),
            creatures,
            share: creatures as f64 / (*totals.get(&generation).unwrap_or(&1)).max(1) as f64,
        })
        .collect())
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionByGeneration {
    pub generation: i64,
    pub kind: String,
    pub count: i64,
    pub per_creature: f64,
}

/// Action frequency distribution by generation (§10). Per-creature as well as
/// raw, because generation sizes differ by an order of magnitude and the raw
/// counts alone say nothing but "gen 2 was large".
pub fn actions_by_generation(conn: &Connection, world: i64) -> Result<Vec<ActionByGeneration>> {
    let mut stmt = conn.prepare(
        "SELECT c.generation, e.kind, COUNT(*),
                COUNT(*) * 1.0 / MAX(1, (SELECT COUNT(*) FROM creatures k
                                          WHERE k.world_id = ?1
                                            AND k.generation = c.generation))
           FROM events e JOIN creatures c ON c.id = e.actor_id
          WHERE e.world_id = ?1 AND e.actor_id IS NOT NULL AND e.actor_id != 0
          GROUP BY c.generation, e.kind
          HAVING COUNT(*) > 0
          ORDER BY c.generation, 3 DESC",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok(ActionByGeneration {
            generation: r.get(0)?,
            kind: r.get(1)?,
            count: r.get(2)?,
            per_creature: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

// -------------------------------------------------------------------- economy

#[derive(Debug, Clone, Serialize)]
pub struct WealthRow {
    pub household_id: i64,
    pub members: i64,
    pub grain: f64,
    pub wood: f64,
    pub other: f64,
    pub grain_per_member: f64,
}

/// Household wealth distribution (§10).
///
/// Grain is broken out from everything else on purpose: §4.4 makes it the only
/// food that reaches the reproduction reserve, so a household rich in berries
/// is not wealthy in any sense that affects whether it has children.
pub fn household_wealth(conn: &Connection, world: i64) -> Result<Vec<WealthRow>> {
    let mut stmt = conn.prepare(
        "SELECT h.id, h.store_json,
                (SELECT COUNT(*) FROM creatures c
                  WHERE c.household_id = h.id AND c.death_tick IS NULL)
           FROM households h
          WHERE h.world_id = ?1 AND h.dissolved_tick IS NULL",
    )?;
    let rows = stmt.query_map([world], |r| {
        Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
    })?;

    let mut out = Vec::new();
    for row in rows {
        let (id, store, members) = row?;
        let (mut grain, mut wood, mut other) = (0.0, 0.0, 0.0);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&store) {
            for item in v.as_array().into_iter().flatten() {
                // Batches, not totals: `store_json` holds one entry per
                // harvest so spoilage can expire the oldest first (§4.4), and
                // the field is `quantity`.
                let qty = item.get("quantity").and_then(|q| q.as_f64()).unwrap_or(0.0);
                match item.get("kind").and_then(|k| k.as_str()) {
                    Some("GRAIN") => grain += qty,
                    Some("WOOD") => wood += qty,
                    _ => other += qty,
                }
            }
        }
        out.push(WealthRow {
            household_id: id,
            members,
            grain,
            wood,
            other,
            grain_per_member: if members > 0 { grain / members as f64 } else { grain },
        });
    }
    out.sort_by(|a, b| b.grain.partial_cmp(&a.grain).unwrap_or(std::cmp::Ordering::Equal));
    Ok(out)
}

#[derive(Debug, Clone, Serialize)]
pub struct WoodSplit {
    pub tick: i64,
    pub chopped: f64,
    pub timber: f64,
    pub fuel: f64,
}

/// The wood budget split (§10).
///
/// > "Timber vs. fuel vs. fuel carried on expeditions. The last of these is the
/// > community's actual spend on exploration."
///
/// Grouped, never stacked (BUILD.md §7.4): production and the two spends are
/// not parts of one whole — wood chopped this tick may be burned twenty ticks
/// later, and stacking them would assert an accounting identity that does not
/// hold.
pub fn wood_budget(conn: &Connection, world: i64, buckets: i64) -> Result<Vec<WoodSplit>> {
    let span: (i64, i64) = conn.query_row(
        "SELECT COALESCE(MIN(tick), 0), COALESCE(MAX(tick), 0)
           FROM tick_stats WHERE world_id = ?1",
        [world],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let width = (((span.1 - span.0 + 1) as f64) / buckets.max(1) as f64).ceil().max(1.0) as i64;

    let wood = super::queries::payload_num("wood");
    let qty = super::queries::payload_num("qty");
    let sql = format!(
        "SELECT (tick / ?2) * ?2,
                SUM(CASE WHEN kind = 'CHOPPED' THEN {qty} ELSE 0 END),
                SUM(CASE WHEN kind IN ('SHELTER_BUILT','SHELTER_REPAIRED')
                              THEN {wood} ELSE 0 END),
                SUM(CASE WHEN kind IN ('FIRE_LIT','FIRE_FED') THEN {wood} ELSE 0 END)
           FROM events
          WHERE world_id = ?1
            AND kind IN ('CHOPPED','SHELTER_BUILT','SHELTER_REPAIRED',
                         'FIRE_LIT','FIRE_FED')
          GROUP BY 1 ORDER BY 1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(rusqlite::params![world, width], |r| {
        Ok(WoodSplit {
            tick: r.get(0)?,
            chopped: r.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            timber: r.get::<_, Option<f64>>(2)?.unwrap_or(0.0),
            fuel: r.get::<_, Option<f64>>(3)?.unwrap_or(0.0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::WorldConfig;
    use crate::sim::tick::{Sim, TickReport};

    /// The same fixture `queries.rs` uses: a real run, persisted, so every
    /// query below is exercised against a database the simulation wrote.
    fn run(ticks: i64) -> (Connection, i64) {
        let mut cfg = WorldConfig::default();
        cfg.map.width = 128;
        cfg.map.height = 128;
        cfg.bench.maintain_population = Some(40);
        cfg.persistence.sample_interval_ticks = 12;

        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        crate::db::migrate(&conn).unwrap();

        let world = crate::sim::worldgen::generate(44127, &cfg).world;
        crate::db::repo::create_world(&conn, "Culture", 44127, &cfg).unwrap();
        crate::db::repo::save_world(&mut conn, 1, &world).unwrap();

        let mut sim = Sim::new(1, world, cfg, 44127);
        sim.spawn_population(40);
        for _ in 0..ticks {
            let mut r = sim.step();
            sim.persist(&mut conn, &mut r, false).unwrap();
        }
        let mut last = TickReport { tick: sim.tick, ..Default::default() };
        sim.persist(&mut conn, &mut last, true).unwrap();
        (conn, 1)
    }

    #[test]
    fn map_coverage_is_sampled_and_never_reports_a_gap_as_a_collapse() {
        let (conn, w) = run(600);
        let cov = map_coverage(&conn, w).unwrap();
        assert!(!cov.is_empty(), "migration 004 should be recording known_tiles");
        assert!(
            cov.iter().all(|p| p.known_tiles > 0),
            "an unsampled tick must be absent, not drawn as zero — that would \
             invent the collapse this chart exists to detect"
        );
        assert!(
            cov.iter().all(|p| p.share_of_world > 0.0 && p.share_of_world <= 1.0),
            "coverage is a share of the map and cannot exceed it"
        );
        let first = cov.first().unwrap().known_tiles;
        let peak = cov.iter().map(|p| p.known_tiles).max().unwrap();
        assert!(peak > first, "600 ticks of exploration should widen what is known");
    }

    #[test]
    fn a_belief_lineage_is_counted_from_its_origin_not_its_holder() {
        let (conn, w) = run(700);
        let rows = knowledge_half_life(&conn, w).unwrap();
        assert!(!rows.is_empty(), "the population should know something");
        for r in &rows {
            assert!(r.median_ticks >= 0, "{} has a negative half-life", r.kind);
            assert!(r.p90_ticks >= r.median_ticks, "p90 cannot sit below the median");
            assert!(
                r.still_alive + r.extinguished > 0,
                "{} was returned with no belief lineages behind it",
                r.kind
            );
        }
    }

    #[test]
    fn the_lineage_survival_curve_starts_whole_and_never_rises() {
        let (conn, w) = run(700);
        let curve = lineage_survival(&conn, w).unwrap();
        assert!(!curve.is_empty(), "there are founders, so there is a curve");
        assert_eq!(curve[0].generation, 0);
        assert!(
            (curve[0].share_surviving - 1.0).abs() < 1e-9,
            "every founder trivially reaches its own generation"
        );
        for pair in curve.windows(2) {
            assert!(
                pair[1].share_surviving <= pair[0].share_surviving + 1e-9,
                "a survival curve that rises means the query is counting \
                 something other than survival: {:?}",
                curve
            );
        }
    }

    #[test]
    fn selection_bands_cover_every_adult_exactly_once() {
        let (conn, w) = run(700);
        let bands = deliberation_vs_depth(&conn, w).unwrap();
        let banded: i64 = bands.iter().map(|b| b.creatures).sum();
        let adults: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM creatures
                  WHERE world_id = ?1 AND life_stage IN ('ADULT','ELDER')",
                [w],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            banded, adults,
            "a creature dropped between bands is a creature silently excluded \
             from the S6 evidence"
        );
    }

    #[test]
    fn roles_are_classified_from_the_log_and_sum_to_the_generation() {
        let (conn, w) = run(700);
        let rows = roles(&conn, w).unwrap();
        assert!(!rows.is_empty());

        use std::collections::BTreeMap;
        let mut shares: BTreeMap<i64, f64> = BTreeMap::new();
        for r in &rows {
            *shares.entry(r.generation).or_insert(0.0) += r.share;
        }
        for (gen, total) in shares {
            assert!(
                (total - 1.0).abs() < 1e-6,
                "generation {gen}'s roles sum to {total}, so somebody has two \
                 jobs or none"
            );
        }
    }

    #[test]
    fn every_report_runs_against_a_real_database() {
        // Cheap, and it is the failure that actually happens: a column renamed
        // in a migration turns one of these into a runtime error that nothing
        // notices until the view is opened.
        let (conn, w) = run(400);
        belief_accuracy(&conn, w).unwrap();
        teaching_vs_depth(&conn, w).unwrap();
        transmission_graph(&conn, w, 200).unwrap();
        horizon_vs_depth(&conn, w).unwrap();
        compute_by_life_stage(&conn, w).unwrap();
        elder_autonomy(&conn, w).unwrap();
        pressure_distribution(&conn, w).unwrap();
        latency(&conn, w).unwrap();
        horizon_by_generation(&conn, w).unwrap();
        actions_by_generation(&conn, w).unwrap();
        household_wealth(&conn, w).unwrap();
        wood_budget(&conn, w, 40).unwrap();
    }

    #[test]
    fn household_wealth_reads_the_batched_store_not_a_total() {
        let (conn, w) = run(700);
        let rows = household_wealth(&conn, w).unwrap();
        if rows.is_empty() {
            return; // no households formed in this window; not this test's claim
        }
        assert!(
            rows.iter().any(|r| r.grain + r.wood + r.other > 0.0),
            "every household store parsed as empty, which is what reading the \
             wrong JSON field looks like"
        );
        for pair in rows.windows(2) {
            assert!(pair[0].grain >= pair[1].grain, "rows should be richest-first");
        }
    }
}
