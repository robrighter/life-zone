//! The M2 measurement harness.
//!
//! BUILD.md §6 asks for the exit criterion *measured*, not claimed, so the
//! numbers in the milestone report come from here rather than from a guess.
//!
//!   measure world [seed]                       -- world composition and food balance
//!   measure run [seed] [creatures] [ticks]     -- the M2 exit criterion
//!
//! Build with --release: the <50ms budget is about the binary that ships, and a
//! debug build is several times slower for reasons that have nothing to do with
//! the simulation.

use life_zone_lib::config::WorldConfig;
use life_zone_lib::sim::creature::{DeathCause, ItemKind, LifeStage};
use life_zone_lib::sim::economy;
use life_zone_lib::sim::knowledge::BeliefKind;
use life_zone_lib::sim::terrain::Terrain;
use life_zone_lib::sim::tick::Sim;
use life_zone_lib::sim::world::{NodeKind, World};
use life_zone_lib::sim::worldgen;
use std::collections::BTreeMap;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("run");
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(44127);

    match cmd {
        "world" => world_report(seed),
        "llm" => {
            let ticks: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(60);
            let calls: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(6);
            llm_report(seed, ticks, calls);
        }
        "s6" => {
            let ticks: i64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(150);
            let calls: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(30);
            s6_report(seed, ticks, calls);
        }
        "run" => {
            let creatures: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let ticks: i64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2000);
            run_report(seed, creatures, ticks);
        }
        // measure report <path-to-sqlite>
        "report" => report_dump(args.get(2).map(|s| s.as_str()).unwrap_or("")),
        other => eprintln!("unknown command {other}"),
    }
}

/// Run every §10 aggregation against a database a real run wrote.
///
/// The unit tests exercise these against 700-tick fixtures on a 128×128 map,
/// which is enough to prove the SQL is valid and the arithmetic holds. It is
/// not enough to know whether the reports say anything on a full-size run —
/// whether the survival curve has more than one point, whether coverage
/// actually stalls, whether any band of the S6 chart has enough creatures in it
/// to be worth drawing. That is what this is for.
fn report_dump(path: &str) {
    use life_zone_lib::report::{culture, queries};

    if path.is_empty() {
        eprintln!("usage: measure report <path-to-sqlite>");
        return;
    }
    let conn = match rusqlite::Connection::open(path) {
        Ok(c) => c,
        Err(e) => return eprintln!("cannot open {path}: {e}"),
    };
    let w: i64 = conn
        .query_row("SELECT id FROM worlds ORDER BY id DESC LIMIT 1", [], |r| r.get(0))
        .unwrap_or(1);

    macro_rules! show {
        ($title:literal, $call:expr) => {
            print!("\n-- {} ", $title);
            println!("{}", "-".repeat(62usize.saturating_sub($title.len())));
            match $call {
                Ok(v) => {
                    let json = serde_json::to_value(&v).unwrap_or(serde_json::Value::Null);
                    match &json {
                        serde_json::Value::Array(rows) if rows.is_empty() => {
                            println!("  (no rows — this report has nothing to say about this run)")
                        }
                        serde_json::Value::Array(rows) => {
                            for row in rows.iter().take(14) {
                                println!("  {}", brief(row));
                            }
                            if rows.len() > 14 {
                                println!("  … {} more", rows.len() - 14);
                            }
                        }
                        other => println!("  {}", brief(other)),
                    }
                }
                Err(e) => println!("  FAILED: {e}"),
            }
        };
    }

    println!("== REPORTS over {path} (world {w}) ==");
    show!("headline", queries::headline(&conn, w));
    show!("population", queries::population_series(&conn, w, 10));
    show!("cause of death by generation", queries::cause_of_death_by_generation(&conn, w));
    show!("age at death", queries::age_at_death(&conn, w, 96));
    show!("deepest lineages", queries::deepest_lineages(&conn, w, 8));
    show!("by generation", queries::by_generation(&conn, w));
    show!("lineage survival", culture::lineage_survival(&conn, w));
    show!("economy", queries::economy_series(&conn, w, 8));
    show!("wood budget", culture::wood_budget(&conn, w, 8));
    show!("farming adoption", queries::farming_adoption(&conn, w));
    show!("household wealth", culture::household_wealth(&conn, w));
    show!("map coverage", culture::map_coverage(&conn, w));
    show!("knowledge half-life", culture::knowledge_half_life(&conn, w));
    show!("belief accuracy by hops", culture::belief_accuracy(&conn, w));
    show!("belief provenance", queries::belief_provenance(&conn, w));
    show!("transmission", queries::transmission_by_channel(&conn, w));
    show!("teaching vs depth", culture::teaching_vs_depth(&conn, w));
    show!("roles", culture::roles(&conn, w));
    show!("action by tier", queries::action_distribution_by_tier(&conn, w));
    show!("deliberation vs lineage depth (S6)", culture::deliberation_vs_depth(&conn, w));
    show!("horizon vs lineage depth", culture::horizon_vs_depth(&conn, w));
    show!("compute by life stage", culture::compute_by_life_stage(&conn, w));
    show!("elder autonomy", culture::elder_autonomy(&conn, w));
    show!("pressure distribution", culture::pressure_distribution(&conn, w));
    show!("latency", culture::latency(&conn, w));
    show!("horizon gap", queries::horizon_gap(&conn, w));
    show!("horizon by goal", culture::horizon_by_goal(&conn, w));
    show!("abort reasons", queries::abort_reasons(&conn, w));
    show!("fallback reasons", queries::fallback_reasons(&conn, w));

    // S5, against the longest life in the run rather than a chosen one.
    let subject: Option<i64> = conn
        .query_row(
            "SELECT id FROM creatures WHERE world_id = ?1 AND death_tick IS NOT NULL
              ORDER BY death_tick - birth_tick DESC LIMIT 1",
            [w],
            |r| r.get(0),
        )
        .ok();
    println!("\n-- S5: one whole life {}", "-".repeat(44));
    match subject.and_then(|id| queries::life(&conn, w, id).ok().flatten()) {
        Some(l) => println!(
            "  {} (g{}) lived {} ticks, died of {}\n  \
             {} events, {} decisions, {} need samples, {} beliefs found ({} still circulating)\n  \
             mother {}, father {}, {} children",
            l.name,
            l.generation,
            l.death_tick.unwrap_or(0) - l.birth_tick,
            l.death_cause.clone().unwrap_or_else(|| "?".into()),
            l.events.len(),
            l.decisions.len(),
            l.samples.len(),
            l.beliefs_found,
            l.still_circulating,
            l.mother.map(|m| m.1).unwrap_or_else(|| "—".into()),
            l.father.map(|f| f.1).unwrap_or_else(|| "—".into()),
            l.children.len(),
        ),
        None => println!("  nobody has died yet"),
    }
}

/// One JSON object on one line, without the braces and quoting that make a
/// terminal dump unreadable.
fn brief(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::Object(m) => m
            .iter()
            .map(|(k, val)| {
                let s = match val {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n
                        .as_f64()
                        .map(|f| if f.fract() == 0.0 { format!("{f:.0}") } else { format!("{f:.3}") })
                        .unwrap_or_else(|| n.to_string()),
                    serde_json::Value::Null => "—".into(),
                    other => other.to_string(),
                };
                format!("{k}={s}")
            })
            .collect::<Vec<_>>()
            .join("  "),
        other => other.to_string(),
    }
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 { 0.0 } else { n as f64 * 100.0 / total as f64 }
}

// ---------------------------------------------------------------- S6 report

/// Does the model choose differently from the deterministic policy?
///
/// S6 is the criterion that matters most: "replacing LLM deliberation with the
/// deterministic fallback produces visibly different, worse outcomes." §10 says
/// the sharpest early warning is the action distribution of the two tiers — "if
/// these two distributions converge, S6 is failing."
///
/// Both are asked about the same creature, in the same state, from the same
/// pre-validated menu. Any difference is therefore the decision and not the
/// situation.
fn s6_report(seed: u64, ticks: i64, calls: usize) {
    use life_zone_lib::ai::ollama::{Client, Depth};
    use life_zone_lib::ai::schema;

    let mut cfg = WorldConfig::default();
    cfg.map.width = 256;
    cfg.map.height = 256;
    cfg.bench.maintain_population = Some(80);

    let world = worldgen::generate(seed, &cfg).world;
    let mut sim = Sim::new(1, world, cfg.clone(), seed);
    sim.spawn_population(80);
    for _ in 0..ticks {
        sim.step();
    }

    println!("== S6: the model against Tier 1, same creature, same menu ==");
    println!("model {}, world warmed {ticks} ticks, {} alive\n",
             cfg.llm.model, sim.alive());

    let client = Client::new(cfg.llm.clone());
    let sc = schema::response_schema(cfg.deliberation.model_estimates_horizon);

    let mut tier1: BTreeMap<String, usize> = BTreeMap::new();
    let mut model: BTreeMap<String, usize> = BTreeMap::new();
    let mut tier1_intent: BTreeMap<String, usize> = BTreeMap::new();
    let mut model_intent: BTreeMap<String, usize> = BTreeMap::new();
    let mut agreed = 0usize;
    let mut compared = 0usize;
    let mut horizons: (u32, u32, usize) = (0, 0, 0);
    let mut steps_per_plan = (0usize, 0usize);
    let mut rejected = 0usize;
    let mut examples: Vec<String> = Vec::new();

    let ids: Vec<i64> = sim.creatures.iter().map(|c| c.id).collect();
    let started = Instant::now();

    for id in ids.into_iter().take(calls) {
        let Some((prompt, menu)) = sim.deliberation_for(id) else { continue };
        let Some((t1_goal, t1_intent)) = sim.tier1_choice(id) else { continue };

        let Ok(r) = client.chat(&prompt, &sc, Depth::Standard) else {
            rejected += 1;
            continue;
        };
        let Ok(v) = schema::validate(&r.raw, &menu, &cfg) else {
            rejected += 1;
            continue;
        };

        let m_goal = v.steps[0].goal.as_str().to_string();
        let m_intent = format!("{:?}", v.addresses);
        let t1_intent_s = format!("{t1_intent:?}");

        compared += 1;
        if m_goal == t1_goal {
            agreed += 1;
        } else if examples.len() < 6 {
            let c = sim.creature(id).unwrap();
            examples.push(format!(
                "  #{} ({}, {}): tier 1 said {t1_goal}; the model said {m_goal} — \"{}\"",
                id, c.name, c.felt_state(&cfg.needs), v.rationale,
            ));
        }
        *tier1.entry(t1_goal).or_default() += 1;
        *model.entry(m_goal).or_default() += 1;
        *tier1_intent.entry(t1_intent_s).or_default() += 1;
        *model_intent.entry(m_intent).or_default() += 1;
        horizons.1 += v.horizon;
        horizons.2 += 1;
        steps_per_plan.0 += v.steps.len();
        steps_per_plan.1 += 1;
    }

    if compared == 0 {
        println!("nothing was comparable — is ollama running?");
        return;
    }

    println!("compared {compared} decisions in {:.0}s ({rejected} unusable)\n",
             started.elapsed().as_secs_f64());
    println!("-- agreement --");
    println!("  the two tiers chose the same first action {agreed}/{compared} times \
              ({:.0}%)", pct(agreed, compared));
    println!("  they differed {:.0}% of the time", pct(compared - agreed, compared));
    println!("  §10: if these distributions converge, S6 is failing.\n");

    let show = |name: &str, a: &BTreeMap<String, usize>, b: &BTreeMap<String, usize>| {
        println!("-- {name} --");
        let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
        keys.sort();
        keys.dedup();
        println!("  {:<22} {:>10} {:>10}", "", "tier 1", "model");
        for k in keys {
            let (x, y) = (a.get(k).copied().unwrap_or(0), b.get(k).copied().unwrap_or(0));
            println!("  {k:<22} {:>9.0}% {:>9.0}%", pct(x, compared), pct(y, compared));
        }
        println!();
    };
    show("first action chosen", &tier1, &model);
    show("what the plan was for", &tier1_intent, &model_intent);

    println!("-- the model's plans --");
    println!("  mean horizon {:.1} ticks", horizons.1 as f64 / horizons.2.max(1) as f64);
    println!("  mean steps per plan {:.2} (Tier 1 rarely exceeds 2)",
             steps_per_plan.0 as f64 / steps_per_plan.1.max(1) as f64);

    if !examples.is_empty() {
        println!("\n-- where they disagreed --");
        for e in &examples {
            println!("{e}");
        }
    }
}

// -------------------------------------------------------------- llm report

/// Measure what a deliberation actually costs on this machine.
///
/// Everything about the budget in §5.3 and the speed modes in §5.6 depends on
/// this number, and the PRD's figures assume a GPU. Prompts are built by the
/// simulation itself rather than by hand, so what is timed is what is sent.
fn llm_report(seed: u64, ticks: i64, calls: usize) {
    use life_zone_lib::ai::ollama::{Client, Depth, Prompt};
    use life_zone_lib::ai::schema;

    let mut cfg = WorldConfig::default();
    cfg.map.width = 256;
    cfg.map.height = 256;
    // §13.3: "how much does model choice matter? qwen3:8b is the target, but
    // the budget maths changes substantially with a smaller model. A faster
    // model that deliberates 3x more often may beat a smarter one that rarely
    // gets the budget." That is a question about two numbers this harness
    // already prints, so it only needed a way to change the model.
    if let Ok(m) = std::env::var("LZ_MODEL") {
        cfg.llm.model = m;
    }
    if let Some(v) = std::env::var("LZ_PREDICT").ok().and_then(|v| v.parse::<u32>().ok()) {
        cfg.llm.num_predict_override = Some(v);
    }
    let world = worldgen::generate(seed, &cfg).world;
    let mut sim = Sim::new(1, world, cfg.clone(), seed);
    sim.spawn_population(80);
    for _ in 0..ticks {
        sim.step();
    }

    println!("== LLM cost, model {} ==", cfg.llm.model);
    println!("warmed the world for {ticks} ticks, {} creatures alive\n", sim.alive());

    let ids: Vec<i64> = sim.creatures.iter().map(|c| c.id).take(calls).collect();
    let mut prompts: Vec<(i64, Prompt, schema::ActionMenu)> = Vec::new();
    for id in ids {
        if let Some((p, m)) = sim.deliberation_for(id) {
            prompts.push((id, p, m));
        }
    }
    if prompts.is_empty() {
        println!("no creature had a legal menu — nothing to measure");
        return;
    }

    let example = &prompts[0].1;
    println!("-- prompt shape --");
    println!("  system {} chars (identical every call — the cached prefix)",
             example.system.len());
    println!("  user   {} chars (this creature, this tick)", example.user.len());
    println!("  menu   {} options", prompts[0].2.options.len());
    println!("\n----- example prompt -----\n{}\n{}\n--------------------------\n",
             example.system, example.user);

    let sc = schema::response_schema(cfg.deliberation.model_estimates_horizon);
    let client = Client::new(cfg.llm.clone());

    for depth in [Depth::Shallow, Depth::Standard] {
        let mut lat: Vec<u64> = Vec::new();
        let mut ok = 0usize;
        let mut rejected: BTreeMap<&str, usize> = BTreeMap::new();
        let mut ptok = 0u32;
        let mut cached = 0u32;
        let mut rtok = 0u32;
        let started = Instant::now();

        for (id, p, menu) in &prompts {
            match client.chat(p, &sc, depth) {
                Ok(r) => {
                    lat.push(r.latency_ms);
                    ptok += r.prompt_tokens;
                    cached += r.cached_tokens;
                    rtok += r.response_tokens;
                    match schema::validate(&r.raw, menu, &cfg) {
                        Ok(v) => {
                            ok += 1;
                            if ok == 1 {
                                println!("-- first accepted plan (creature #{id}) --");
                                for st in &v.steps {
                                    println!("   {} {}", st.goal.as_str(),
                                             st.describe(&sim.world));
                                }
                                println!("   horizon {} — \"{}\"\n", v.horizon, v.rationale);
                            }
                        }
                        Err(e) => {
                            *rejected.entry(e.as_str()).or_default() += 1;
                            if rejected.len() == 1 {
                                println!("-- first rejected response ({}) --\n{}\n",
                                         e.as_str(), r.raw.chars().take(300)
                                             .collect::<String>());
                            }
                        }
                    }
                }
                Err(e) => {
                    *rejected.entry(e.as_str()).or_default() += 1;
                }
            }
        }

        if lat.is_empty() {
            println!("-- {} -- no calls completed (is ollama running?)", depth.as_str());
            continue;
        }
        lat.sort_unstable();
        let n = lat.len();
        let mean = lat.iter().sum::<u64>() as f64 / n as f64;
        println!("-- {} --", depth.as_str());
        println!("  {n} calls in {:.1}s: mean {:.0}ms, p50 {}ms, max {}ms",
                 started.elapsed().as_secs_f64(), mean, lat[n / 2], lat[n - 1]);
        println!("  accepted {ok}/{n}  ({:.0}% usable first time)", pct(ok, n));
        println!("  prompt tokens {ptok} of which {cached} came from cache ({:.0}%)",
                 pct(cached as usize, ptok as usize));
        println!("  response tokens {rtok}");
        for (why, k) in &rejected {
            println!("    rejected: {why} x{k}");
        }
        // What the budget can actually be, given the mode targets in §5.6.
        println!("  => at {:.0}ms a call, one tick of Observe affords {:.1} calls \
                  in {:.0}s",
                 mean,
                 cfg.deliberation.observe_target_tick_ms as f64 / mean,
                 cfg.deliberation.observe_target_tick_ms as f64 / 1000.0);
        println!();
    }
}

// ------------------------------------------------------------ world report

fn world_report(seed: u64) {
    let cfg = WorldConfig::default();
    let t0 = Instant::now();
    let out = worldgen::generate(seed, &cfg);
    let world = out.world;
    let gen_ms = t0.elapsed().as_millis();

    println!("== WORLD {} ({}x{}) ==", seed, world.width, world.height);
    println!("generated in {gen_ms}ms, {} seeds rejected", out.rejected);
    println!("fingerprint {:016x}\n", world.fingerprint());

    let total = world.tiles.len();
    let mut terrain: BTreeMap<&str, usize> = BTreeMap::new();
    for t in &world.tiles {
        let name = match t {
            Terrain::DeepWater => "deep water",
            Terrain::ShallowWater => "shallow water",
            Terrain::Sand => "sand",
            Terrain::Grass => "grass",
            Terrain::Forest => "forest",
            Terrain::Soil => "soil",
            Terrain::Hill => "hill",
        };
        *terrain.entry(name).or_default() += 1;
    }
    println!("-- terrain --");
    for (name, n) in &terrain {
        println!("  {name:15} {n:>7}  {:5.2}%", pct(*n, total));
    }

    let mut nodes: BTreeMap<&str, (usize, f32, f32)> = BTreeMap::new();
    for n in &world.nodes {
        let e = nodes.entry(n.kind.as_str()).or_insert((0, 0.0, 0.0));
        e.0 += 1;
        e.1 += n.quantity;
        e.2 += n.regen_rate;
    }
    println!("\n-- resource nodes --");
    for (kind, (n, qty, regen)) in &nodes {
        println!("  {kind:8} {n:>5} nodes   stock {qty:>9.1}   regen {regen:>7.3}/tick");
    }

    // The food balance: what the map produces against what a population eats.

    let forage_regen: f32 = world
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Forage)
        .map(|n| n.regen_rate)
        .sum();
    let nutrition_per_tick = forage_regen * ItemKind::Forage.nutrition();
    let hunger_per_creature = cfg.needs.hunger_decay_per_tick;

    println!("\n-- food balance (forage only, the resource a lineage starts on) --");
    println!("  forage regen        {forage_regen:>8.3} units/tick");
    println!("  nutrition regen     {nutrition_per_tick:>8.2} /tick");
    println!("  one creature eats   {hunger_per_creature:>8.2} /tick");
    println!(
        "  carrying capacity   {:>8.0} creatures on forage alone",
        nutrition_per_tick / hunger_per_creature
    );

    // §13: SOIL_WATER_REACH vs reproduction.store_reserve. Farmable land is
    // restricted to within a few tiles of fresh water, and only grain reaches
    // the reproduction reserve, so these two numbers jointly decide whether any
    // lineage can ever breed.
    let soil = *terrain.get("soil").unwrap_or(&0);
    let wheat_nodes = nodes.get("WHEAT").map(|e| e.0).unwrap_or(0);
    let wheat_stock = nodes.get("WHEAT").map(|e| e.1).unwrap_or(0.0);
    let reserve = cfg.reproduction.store_reserve;

    println!("\n-- the farming gate (PRD §4.8 x §8.4) --");
    println!("  soil tiles          {soil:>8}  ({:.2}% of map)", pct(soil, total));
    println!("  wild wheat nodes    {wheat_nodes:>8}  holding {wheat_stock:.0} grain");
    println!("  reproduction reserve{reserve:>8.0} grain per household");
    println!(
        "  reserves in the standing wild crop: {:.0}",
        wheat_stock / reserve.max(1.0)
    );
    if let Some(f) = world.founders.first() {
        let d = nearest_kind_distance(&world, f.x, f.y, Terrain::Soil);
        println!("  hearth -> nearest soil          {d} tiles");
        let w = nearest_water_distance(&world, f.x, f.y);
        println!("  hearth -> nearest fresh water   {w} tiles");
    }
    println!("  founders            {:>8}", world.founders.len());
}

/// How far this tile is from fresh water, for the diagnostic above.
fn water_distance(sim: &Sim, x: u32, y: u32) -> u32 {
    sim.cache.water_dist[sim.world.idx(x, y)].min(9999)
}

fn nearest_kind_distance(world: &World, x: u32, y: u32, kind: Terrain) -> u32 {
    let mut best = u32::MAX;
    for yy in 0..world.height {
        for xx in 0..world.width {
            if world.at(xx, yy) == kind {
                let d = xx.abs_diff(x).max(yy.abs_diff(y));
                best = best.min(d);
            }
        }
    }
    best
}

fn nearest_water_distance(world: &World, x: u32, y: u32) -> u32 {
    let mut best = u32::MAX;
    for yy in 0..world.height {
        for xx in 0..world.width {
            if world.at(xx, yy).is_fresh_water() {
                let d = xx.abs_diff(x).max(yy.abs_diff(y));
                best = best.min(d);
            }
        }
    }
    best
}

// -------------------------------------------------------------- run report

fn run_report(seed: u64, creatures: u32, ticks: i64) {
    let mut cfg = WorldConfig::default();
    // Creatures reproduce now, so the census is emergent and the fixture is
    // only a floor for the performance measurement — and only when asked for:
    //   measure run <seed> <creatures> <ticks> hold
    let hold = std::env::args().nth(5).is_some_and(|a| a == "hold");
    if hold {
        cfg.bench.maintain_population = Some(creatures);
    }

    // Dial overrides, so the balance questions §13.1 lists can be answered by
    // experiment rather than by rebuilding: "if M4 shows lineages consistently
    // dying at generation 2, the dials in order are infant duration, the
    // reserve threshold, then grain yield per harvest."
    let dial = |name: &str| std::env::var(name).ok().and_then(|v| v.parse::<f32>().ok());
    if let Some(v) = dial("LZ_RESERVE") {
        cfg.reproduction.store_reserve = v;
    }
    if let Some(v) = dial("LZ_INFANT") {
        cfg.lifespan.infant_until_tick = v as u32;
    }
    if let Some(v) = dial("LZ_SPACING") {
        cfg.reproduction.birth_spacing_ticks = v as u32;
    }
    if let Some(v) = dial("LZ_WHEAT_YIELD") {
        cfg.actions.harvest_wheat_per_tick = v;
    }
    if let Some(v) = dial("LZ_SOIL") {
        cfg.resources.soil_density = v;
    }
    if let Some(v) = dial("LZ_ELDER") {
        cfg.lifespan.elder_from_tick = v as u32;
    }
    // §13.1 lists three dials and lifespan is not among them, because the
    // 4-week life is a design premise rather than a knob. It becomes a knob the
    // moment the other three are exhausted and still cannot reach generation 5.
    if let Some(v) = dial("LZ_LIFESPAN") {
        cfg.lifespan.baseline_ticks = v as u32;
    }
    // S4: "turning off wheat farming collapses lineage depth across 3 seeded
    // runs". §11 says the toggles exist precisely to run the same seed with one
    // mechanic disabled, which is how you find out whether it does anything.
    if let Some(v) = dial("LZ_WHEAT") {
        cfg.features.wheat = v != 0.0;
    }
    if let Some(v) = dial("LZ_SHELTER_COST") {
        cfg.actions.shelter_wood_cost = v;
    }
    if let Some(v) = dial("LZ_GESTATION") {
        cfg.reproduction.gestation_ticks = v as u32;
    }
    println!(
        "dials: reserve {:.0}, infancy {}, spacing {}, wheat yield {:.1}, soil {:.3}, \
         elder {}",
        cfg.reproduction.store_reserve,
        cfg.lifespan.infant_until_tick,
        cfg.reproduction.birth_spacing_ticks,
        cfg.actions.harvest_wheat_per_tick,
        cfg.resources.soil_density,
        cfg.lifespan.elder_from_tick,
    );

    let world = worldgen::generate(seed, &cfg).world;

    // Persist for real. Phase 7 is part of a tick, so a tick-time measurement
    // that skips SQLite is not a measurement of the thing the criterion is
    // about.
    // `LZ_DB=<path>` keeps the database somewhere nameable, which is what makes
    // `measure report <path>` usable afterwards — the pid-stamped temp
    // directory is fine for a throwaway timing run and useless for anything you
    // want to look at twice.
    let db_path = match std::env::var("LZ_DB") {
        Ok(p) => {
            let p = std::path::PathBuf::from(p);
            if let Some(parent) = p.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::remove_file(&p);
            p
        }
        Err(_) => {
            let dir =
                std::env::temp_dir().join(format!("life-zone-measure-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            dir.join("measure.sqlite3")
        }
    };
    let mut conn = life_zone_lib::db::open(&db_path).expect("open database");
    life_zone_lib::db::repo::create_world(&conn, "Measure", seed as i64, &cfg)
        .expect("world row");
    life_zone_lib::db::repo::save_world(&mut conn, 1, &world).expect("save world");

    let mut sim = Sim::new(1, world, cfg.clone(), seed);
    if creatures == 0 {
        sim.spawn_founders();
    } else {
        sim.spawn_population(creatures);
    }

    println!("== RUN seed {seed}, {} creatures, {ticks} ticks ==",
             if creatures == 0 { sim.alive() as u32 } else { creatures });
    if hold {
        println!("census floored by the benchmark fixture (settlers replace the dead)");
    } else {
        println!("population is emergent — no fixture");
    }
    println!("persisting to {}\n", db_path.display());

    let mut tick_us: Vec<u64> = Vec::with_capacity(ticks as usize);
    let mut phase = [0u64; 7];
    let mut pop_curve: Vec<(i64, usize)> = Vec::new();
    let mut abandoned = 0u32;
    let mut deliberations = 0u64;
    let mut gathered = 0.0f64;
    let mut eaten = 0.0f64;
    let mut spoiled = 0.0f64;
    let mut discoveries = 0u64;
    let mut events_total = 0u64;
    let mut goal_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut abort_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut intent_counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut event_counts: BTreeMap<&str, u64> = BTreeMap::new();
    let mut horizon_committed = 0u64;
    let mut horizon_actual = 0u64;
    let mut planted = 0u64;
    let mut pairings = 0u64;
    let mut rejections = 0u64;
    let mut conceptions = 0u64;
    let mut births = 0u64;
    let mut settlers = 0u64;
    let mut shared = 0u64;
    let mut taught = 0u64;
    let mut overheard = 0u64;
    let mut households_founded = 0u64;
    let mut blocked = [0u64; 7];
    let mut grain_gathered = 0.0f64;
    let mut forage_gathered = 0.0f64;
    let mut max_generation = 0i32;
    // Everybody who has ever lived, so S7 can ask whether the beliefs still in
    // circulation came from somebody who is dead.
    let mut ever_lived: std::collections::BTreeSet<i64> = Default::default();
    let mut gen_curve: Vec<(i64, i32, usize, usize)> = Vec::new();
    let mut shelters_built = 0u64;
    let mut fires_lit = 0u64;

    println!("-- the living population as the run goes on --");
    let start = Instant::now();
    for t in 0..ticks {
        let t0 = Instant::now();
        let mut r = sim.step();
        sim.persist(&mut conn, &mut r, false).expect("persist");
        tick_us.push(t0.elapsed().as_micros() as u64);

        phase[0] += r.timings.world;
        phase[1] += r.timings.needs;
        phase[2] += r.timings.plans;
        phase[3] += r.timings.deliberate;
        phase[4] += r.timings.act;
        phase[5] += r.timings.resolve;
        phase[6] += r.timings.persist;

        abandoned += r.plans_abandoned;
        pairings += r.pairings as u64;
        rejections += r.rejections as u64;
        conceptions += r.conceptions as u64;
        births += r.births as u64;
        settlers += r.settlers as u64;
        for (i, n) in r.conception_blocked.iter().enumerate() {
            blocked[i] += *n as u64;
        }
        shared += r.beliefs_shared as u64;
        taught += r.beliefs_taught as u64;
        overheard += r.beliefs_overheard as u64;
        for e in &sim.events {
            if e.kind == life_zone_lib::sim::event::EventKind::HouseholdFounded {
                households_founded += 1;
            }
        }
        for c in &sim.creatures {
            ever_lived.insert(c.id);
            max_generation = max_generation.max(c.generation);
        }
        deliberations += r.deliberations as u64;
        gathered += r.food_gathered as f64;
        eaten += r.food_eaten as f64;
        spoiled += r.food_spoiled as f64;
        discoveries += r.discoveries as u64;
        events_total += sim.events.len() as u64;

        for d in &sim.decisions {
            *goal_counts.entry(d.goal.clone()).or_default() += 1;
            horizon_committed += d.horizon_committed as u64;
            *intent_counts.entry(format!("{:?}", d.addresses)).or_default() += 1;
        }
        for o in &sim.plan_outcomes {
            *abort_counts.entry(o.reason.as_str()).or_default() += 1;
            horizon_actual += o.horizon_actual as u64;
        }
        for e in &sim.events {
            *event_counts.entry(e.kind.as_str()).or_default() += 1;
            match e.kind {
                life_zone_lib::sim::event::EventKind::Harvested => {
                    if let Some(q) = e.payload.split("qty=").nth(1)
                        .and_then(|t| t.split_whitespace().next())
                        .and_then(|t| t.parse::<f64>().ok())
                    {
                        grain_gathered += q;
                    }
                }
                life_zone_lib::sim::event::EventKind::Gathered => {
                    if let Some(q) = e.payload.split("qty=").nth(1)
                        .and_then(|t| t.split_whitespace().next())
                        .and_then(|t| t.parse::<f64>().ok())
                    {
                        forage_gathered += q;
                    }
                }
                life_zone_lib::sim::event::EventKind::Planted => planted += 1,
                life_zone_lib::sim::event::EventKind::ShelterBuilt => shelters_built += 1,
                life_zone_lib::sim::event::EventKind::FireLit => fires_lit += 1,
                _ => {}
            }
        }

        if t % 100 == 0 {
            pop_curve.push((t, sim.alive()));
        }
        if t % 200 == 0 {
            let households = sim.households.items.iter().filter(|h| h.is_alive()).count();
            gen_curve.push((t, max_generation, sim.alive(), households));
        }
        if t % 250 == 0 && t > 0 {
            let n = sim.alive().max(1) as f32;
            let mean = |f: fn(&life_zone_lib::sim::creature::Creature) -> f32| {
                sim.creatures.iter().map(f).sum::<f32>() / n
            };
            let with_water = sim
                .creatures
                .iter()
                .filter(|c| c.beliefs.iter().any(|b| b.kind == BeliefKind::Water))
                .count();
            let with_food = sim
                .creatures
                .iter()
                .filter(|c| {
                    c.beliefs.iter().any(|b| {
                        matches!(b.kind, BeliefKind::ForageNode | BeliefKind::SoilPatch)
                    })
                })
                .count();
            let carrying = mean(|c| c.inventory.food_value());
            let wood = mean(|c| c.inventory.total(life_zone_lib::sim::creature::ItemKind::Wood));
            let dist_to_water = sim
                .creatures
                .iter()
                .map(|c| water_distance(&sim, c.x, c.y))
                .sum::<u32>() as f32
                / n;
            let stores: Vec<f32> = sim.households.items.iter()
                .filter(|h| h.is_alive()).map(|h| h.stored_food()).collect();
            let grain: f32 = sim.households.items.iter()
                .filter(|h| h.is_alive())
                .map(|h| h.store.total(ItemKind::Grain)).sum();
            if !stores.is_empty() {
                println!(
                    "         households {:3}  store mean {:5.1}  grain held {:6.1}  \
                     at reserve {}",
                    stores.len(),
                    stores.iter().sum::<f32>() / stores.len() as f32,
                    grain,
                    stores.iter().filter(|s| **s >= cfg.reproduction.store_reserve).count(),
                );
            }
            let paired = sim.creatures.iter().filter(|c| c.mate_id.is_some()).count();
            let homeless_paired: Vec<&life_zone_lib::sim::creature::Creature> = sim
                .creatures
                .iter()
                .filter(|c| c.mate_id.is_some() && c.household_id.is_none())
                .collect();
            let wood_hp = if homeless_paired.is_empty() {
                0.0
            } else {
                homeless_paired
                    .iter()
                    .map(|c| c.inventory.total(ItemKind::Wood))
                    .sum::<f32>()
                    / homeless_paired.len() as f32
            };
            println!(
                "         paired {paired:3}  of whom homeless {:3}  carrying {wood_hp:4.1} wood \
                 (a shelter costs {:.0})",
                homeless_paired.len(),
                cfg.actions.shelter_wood_cost,
            );
            let stock = |k: NodeKind| -> f32 {
                sim.world.nodes.iter().filter(|n| n.kind == k).map(|n| n.quantity).sum()
            };
            println!(
                "         standing stock: forage {:7.0}  wood {:7.0}  wheat {:7.0}  sheep {:4.0}",
                stock(NodeKind::Forage), stock(NodeKind::Wood),
                stock(NodeKind::Wheat), stock(NodeKind::Sheep),
            );
            println!(
                "  t{t:<5} hunger {:5.1} thirst {:5.1} warmth {:5.1} health {:5.1} | \
                 knows water {:3.0}% food {:3.0}% | carries {:5.1} nutrition | \
                 {:4.1} tiles from water | wood {:4.1}",
                mean(|c| c.hunger), mean(|c| c.thirst), mean(|c| c.warmth), mean(|c| c.health),
                pct(with_water, sim.alive()), pct(with_food, sim.alive()),
                carrying, dist_to_water, wood,
            );
        }
    }
    let wall = start.elapsed();

    // ---- tick time ---------------------------------------------------------
    let mut sorted = tick_us.clone();
    sorted.sort_unstable();
    let p = |q: f64| sorted[((sorted.len() as f64 - 1.0) * q) as usize] as f64 / 1000.0;
    let mean = tick_us.iter().sum::<u64>() as f64 / tick_us.len() as f64 / 1000.0;

    println!("\n-- tick time over {ticks} ticks (ms) --");
    println!("  mean {mean:6.2}   p50 {:6.2}   p90 {:6.2}   p95 {:6.2}   p99 {:6.2}   max {:6.2}",
             p(0.50), p(0.90), p(0.95), p(0.99), p(1.0));
    println!("  wall clock {:.2}s for {ticks} ticks", wall.as_secs_f64());
    let over = tick_us.iter().filter(|&&u| u > 50_000).count();
    println!("  ticks over the 50ms budget: {over} ({:.2}%)", pct(over, tick_us.len()));

    println!("\n-- histogram of tick time --");
    let buckets: [(u64, u64); 8] = [
        (0, 2_000), (2_000, 5_000), (5_000, 10_000), (10_000, 20_000),
        (20_000, 35_000), (35_000, 50_000), (50_000, 100_000), (100_000, u64::MAX),
    ];
    for (lo, hi) in buckets {
        let n = tick_us.iter().filter(|&&u| u >= lo && u < hi).count();
        if n == 0 {
            continue;
        }
        let label = if hi == u64::MAX {
            format!(">{:.0}ms", lo as f64 / 1000.0)
        } else {
            format!("{:.0}-{:.0}ms", lo as f64 / 1000.0, hi as f64 / 1000.0)
        };
        let bar = "#".repeat(((n as f64 / tick_us.len() as f64) * 60.0).ceil() as usize);
        println!("  {label:>10} {n:>6} {:5.1}%  {bar}", pct(n, tick_us.len()));
    }

    println!("\n-- where the time goes (total ms per phase) --");
    let names = ["1 world", "2 needs", "3 plans", "4 deliberate", "5 act", "6 resolve", "7 persist"];
    let phase_total: u64 = phase.iter().sum();
    for (i, name) in names.iter().enumerate() {
        println!("  {name:14} {:8.1}ms  {:5.1}%",
                 phase[i] as f64 / 1000.0, pct(phase[i] as usize, phase_total as usize));
    }

    // ---- cause of death ----------------------------------------------------
    let total_deaths: u32 = sim.deaths_by_cause.iter().sum();
    println!("\n-- cause of death ({total_deaths} deaths) --");
    let mut rows: Vec<(&str, u32)> = DeathCause::ALL
        .iter()
        .map(|c| (c.as_str(), sim.deaths_by_cause[*c as usize]))
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let mut worst = 0.0;
    for (name, n) in &rows {
        let share = pct(*n as usize, total_deaths as usize);
        worst = f64::max(worst, share);
        let bar = "#".repeat((share / 2.0).ceil().max(0.0) as usize);
        println!("  {name:12} {n:>6}  {share:5.1}%  {bar}");
    }
    println!("  largest single cause: {worst:.1}% (criterion: no cause above ~60%)");

    // ---- economy and behaviour --------------------------------------------
    println!("\n-- economy over the run --");
    println!("  food gathered {gathered:12.0}");
    println!("  food eaten    {eaten:12.0}");
    println!("  food spoiled  {spoiled:12.0}  ({:.1}% of what was gathered)",
             if gathered > 0.0 { spoiled * 100.0 / gathered } else { 0.0 });
    println!("  grain harvested {grain_gathered:10.0}   forage picked {forage_gathered:.0}");
    println!("  wheat planted {planted:12}   shelters {shelters_built}   fires lit {fires_lit}");
    println!("  structures standing at the end: {}", sim.structures.items.len());

    println!("\n-- deliberation (all Tier 1 at M2) --");
    println!("  decisions          {deliberations}");
    println!("  per creature-tick  {:.4}", deliberations as f64 / (ticks as f64 * creatures as f64));
    println!("  plans abandoned    {abandoned}  ({:.1}% of plans set)",
             pct(abandoned as usize, deliberations as usize));
    println!("  events written     {events_total}  ({:.1}/tick)",
             events_total as f64 / ticks as f64);

    println!("\n-- how plans ended (§5.5 abandonment) --");
    let ended: u64 = abort_counts.values().sum();
    let mut aborts: Vec<(&&str, &u64)> = abort_counts.iter().collect();
    aborts.sort_by(|a, b| b.1.cmp(a.1));
    for (reason, n) in aborts {
        println!("  {reason:22} {n:>8}  {:5.1}%", pct(*n as usize, ended as usize));
    }
    println!("  mean horizon committed {:.1}, actual {:.1}  (the abandonment gap)",
             horizon_committed as f64 / deliberations.max(1) as f64,
             horizon_actual as f64 / ended.max(1) as f64);

    println!("\n-- the event log, by kind ({:.1}/tick) --", events_total as f64 / ticks as f64);
    let mut evs: Vec<(&&str, &u64)> = event_counts.iter().collect();
    evs.sort_by(|a, b| b.1.cmp(a.1));
    for (kind, n) in &evs {
        println!("  {kind:20} {n:>8}  {:5.1}%", pct(**n as usize, events_total as usize));
    }

    println!("\n-- what Tier 1 was trying to achieve --");
    let mut intents: Vec<(&String, &u64)> = intent_counts.iter().collect();
    intents.sort_by(|a, b| b.1.cmp(a.1));
    for (intent, n) in &intents {
        println!("  {intent:20} {n:>8}  {:5.1}%", pct(**n as usize, deliberations as usize));
    }

    println!("\n-- what the living population knows, by kind --");
    let mut by_kind: BTreeMap<&str, usize> = BTreeMap::new();
    let mut holders: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &sim.creatures {
        let mut seen: Vec<&str> = Vec::new();
        for b in &c.beliefs {
            *by_kind.entry(b.kind.as_str()).or_default() += 1;
            if !seen.contains(&b.kind.as_str()) {
                seen.push(b.kind.as_str());
                *holders.entry(b.kind.as_str()).or_default() += 1;
            }
        }
    }
    for (kind, n) in &by_kind {
        println!("  {kind:20} {n:>7} beliefs, held by {:5.1}% of creatures",
                 pct(*holders.get(kind).unwrap_or(&0), sim.alive()));
    }

    println!("\n-- first step of the chosen plan --");
    let mut goals: Vec<(&String, &u64)> = goal_counts.iter().collect();
    goals.sort_by(|a, b| b.1.cmp(a.1));
    for (goal, n) in goals.iter().take(12) {
        println!("  {goal:20} {n:>8}  {:5.1}%", pct(**n as usize, deliberations as usize));
    }

    // ---- knowledge ---------------------------------------------------------
    let beliefs: usize = sim.creatures.iter().map(|c| c.beliefs.len()).sum();
    let firsthand: usize = sim
        .creatures
        .iter()
        .flat_map(|c| c.beliefs.iter())
        .filter(|b| b.is_firsthand())
        .count();
    // ---- society -----------------------------------------------------------
    println!("\n-- society over the run --");
    println!("  households founded {households_founded}");
    println!("  standing at the end {}",
             sim.households.items.iter().filter(|h| h.is_alive()).count());
    println!("  courtships accepted {pairings}, refused {rejections}  ({:.0}% refused)",
             pct(rejections as usize, (pairings + rejections) as usize));
    println!("  conceptions {conceptions}, births {births}");

    // How often teaching is even *possible*, as distinct from how often it is
    // chosen. Tier 1 is deliberately myopic about teaching (§13.5 and the note
    // in `policy::social_run`), so the model is meant to be the one that does
    // it — but that only matters if the opportunity exists. If this is near
    // zero, no tier can teach and the culture layer is inert whatever the
    // prompt says.
    {
        use life_zone_lib::sim::creature::LifeStage;
        let reach = cfg.actions.social_reach.max(2);
        let (mut eligible, mut with_pupil) = (0u64, 0u64);
        for c in sim.creatures.iter().filter(|c| c.is_alive()) {
            if c.life_stage == LifeStage::Infant || c.beliefs.is_empty() || c.household_id.is_none()
            {
                continue;
            }
            eligible += 1;
            let has_pupil = sim.creatures.iter().any(|p| {
                p.id != c.id
                    && p.is_alive()
                    && p.household_id == c.household_id
                    && p.life_stage != LifeStage::Elder
                    && c.x.abs_diff(p.x) <= reach
                    && c.y.abs_diff(p.y) <= reach
            });
            if has_pupil {
                with_pupil += 1;
            }
        }
        println!(
            "  teaching: {eligible} could teach, {with_pupil} have a pupil in reach              ({:.1}% of the living)",
            pct(with_pupil as usize, sim.alive().max(1))
        );
    }
    if settlers > 0 {
        println!("  settlers admitted by the fixture {settlers} (not births)");
    }
    let blocked_total: u64 = blocked.iter().sum();
    if blocked_total > 0 {
        println!("  what stopped the rest ({blocked_total} paired-couple ticks):");
        let mut rows: Vec<(&str, u64)> = life_zone_lib::sim::social::Blocker::ALL
            .iter()
            .map(|b| (b.as_str(), blocked[*b as usize]))
            .filter(|(_, n)| *n > 0)
            .collect();
        rows.sort_by(|a, b| b.1.cmp(&a.1));
        for (name, n) in rows {
            println!("    {name:28} {n:>8}  {:5.1}%", pct(n as usize, blocked_total as usize));
        }
    }
    println!("  relationships tracked {}", sim.relationships.len());

    println!("\n-- lineage (the point of the whole thing) --");
    println!("  deepest generation reached: {max_generation}");
    let mut by_gen: BTreeMap<i32, usize> = BTreeMap::new();
    for c in &sim.creatures {
        *by_gen.entry(c.generation).or_default() += 1;
    }
    for (g, n) in &by_gen {
        println!("    generation {g:>2}  {n:>5} alive");
    }
    let stores: Vec<f32> = sim
        .households
        .items
        .iter()
        .filter(|h| h.is_alive())
        .map(|h| h.stored_food())
        .collect();
    if !stores.is_empty() {
        let mean = stores.iter().sum::<f32>() / stores.len() as f32;
        let at_reserve = stores.iter().filter(|s| **s >= cfg.reproduction.store_reserve).count();
        println!("  household stores: mean {mean:.1}, {at_reserve} of {} at or above the \
                  {:.0} reserve", stores.len(), cfg.reproduction.store_reserve);
    }

    println!("\n-- transmission (§4.11) --");
    println!("  beliefs taught     {taught}");
    println!("  beliefs shared     {shared}");
    println!("  beliefs overheard  {overheard}");
    let teachers = sim.creatures.iter().filter(|c| c.taught_count > 0).count();
    let sharers = sim.creatures.iter().filter(|c| c.shared_count > 0).count();
    println!("  of the living, {:.0}% have taught somebody, {:.0}% have shared",
             pct(teachers, sim.alive()), pct(sharers, sim.alive()));

    // S7: does knowledge outlive the creature that found it?
    let living: std::collections::BTreeSet<i64> = sim.creatures.iter().map(|c| c.id).collect();
    let (mut inherited, mut total_beliefs) = (0usize, 0usize);
    for c in &sim.creatures {
        for b in &c.beliefs {
            total_beliefs += 1;
            if let Some(origin) = b.origin_creature_id {
                if origin != c.id && !living.contains(&origin) {
                    inherited += 1;
                }
            }
        }
    }
    println!("\n-- S7: knowledge outliving its discoverer --");
    println!("  beliefs in circulation      {total_beliefs}");
    println!("  originating with the dead   {inherited}  ({:.1}%)",
             pct(inherited, total_beliefs));

    println!("\n-- generation and household curve --");
    for (t, g, alive, h) in gen_curve.iter().step_by(4) {
        println!("  tick {t:>6}  gen {g:>2}  alive {alive:>5}  households {h:>4}");
    }

    println!("\n-- knowledge at the end --");
    println!("  beliefs held      {beliefs} across {} living creatures", sim.alive());
    println!("  mean per creature {:.1}", beliefs as f64 / sim.alive().max(1) as f64);
    println!("  firsthand         {firsthand} ({:.0}%)", pct(firsthand, beliefs));
    println!("  discoveries made  {discoveries} over the run");

    // ---- population --------------------------------------------------------
    println!("\n-- population --");
    for (t, n) in pop_curve.iter().step_by(4) {
        println!("  tick {t:>6}  {n:>5}");
    }
    println!("  ended at {} alive, {} died in total", sim.alive(), sim.total_deaths);

    // ---- life stages -------------------------------------------------------
    let mut stages: BTreeMap<&str, usize> = BTreeMap::new();
    for c in &sim.creatures {
        *stages.entry(c.life_stage.as_str()).or_default() += 1;
    }
    // What the run actually cost on disk — DB growth is a listed risk (§14).
    let bytes: u64 = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0)
        + std::fs::metadata(db_path.with_extension("sqlite3-wal"))
            .map(|m| m.len())
            .unwrap_or(0);
    println!("\n-- what it cost on disk --");
    println!("  database {:.1} MB after {ticks} ticks ({:.2} KB/tick)",
             bytes as f64 / 1_048_576.0, bytes as f64 / 1024.0 / ticks as f64);
    let rows = |t: &str| -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| r.get(0)).unwrap_or(-1)
    };
    for t in ["events", "decisions", "creatures", "beliefs", "tick_stats", "creature_samples"] {
        println!("  {t:20} {:>9} rows", rows(t));
    }

    println!("\n-- living population by stage --");
    for (s, n) in &stages {
        println!("  {s:8} {n:>5}  {:5.1}%", pct(*n, sim.alive()));
    }
    let _ = LifeStage::Adult;
    let _ = economy::is_night(0, &cfg);

    drop(conn);
    // A named database is the point of naming it — only the throwaway
    // pid-stamped directory gets cleaned up. This deletion is why every earlier
    // run left nothing behind to open a report against.
    if std::env::var("LZ_DB").is_err() {
        if let Some(dir) = db_path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    } else {
        println!("\ndatabase kept at {}", db_path.display());
    }
}
