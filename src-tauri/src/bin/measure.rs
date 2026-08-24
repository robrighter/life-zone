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
        "run" => {
            let creatures: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(500);
            let ticks: i64 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2000);
            run_report(seed, creatures, ticks);
        }
        other => eprintln!("unknown command {other}"),
    }
}

fn pct(n: usize, total: usize) -> f64 {
    if total == 0 { 0.0 } else { n as f64 * 100.0 / total as f64 }
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
    // Reproduction is M4. Holding the census is the only way to measure the
    // stated criterion — 500 creatures, <50ms/tick — before it exists.
    cfg.bench.maintain_population = Some(creatures);

    let world = worldgen::generate(seed, &cfg).world;

    // Persist for real. Phase 7 is part of a tick, so a tick-time measurement
    // that skips SQLite is not a measurement of the thing the criterion is
    // about.
    let dir = std::env::temp_dir().join(format!("life-zone-measure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let db_path = dir.join("measure.sqlite3");
    let mut conn = life_zone_lib::db::open(&db_path).expect("open database");
    life_zone_lib::db::repo::create_world(&conn, "Measure", seed as i64, &cfg)
        .expect("world row");
    life_zone_lib::db::repo::save_world(&mut conn, 1, &world).expect("save world");

    let mut sim = Sim::new(1, world, cfg.clone(), seed);
    sim.spawn_population(creatures);

    println!("== RUN seed {seed}, {creatures} creatures, {ticks} ticks ==");
    println!("population held by the M2 benchmark fixture (no reproduction until M4)");
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
                life_zone_lib::sim::event::EventKind::Planted => planted += 1,
                life_zone_lib::sim::event::EventKind::ShelterBuilt => shelters_built += 1,
                life_zone_lib::sim::event::EventKind::FireLit => fires_lit += 1,
                _ => {}
            }
        }

        if t % 100 == 0 {
            pop_curve.push((t, sim.alive()));
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
    let _ = std::fs::remove_dir_all(&dir);
}
