//! The golden-run and schema round-trip tests (BUILD.md §9).
//!
//! > with the LLM disabled and a fixed seed, 2,000 ticks must produce a
//! > byte-identical event log. This makes any accidental non-determinism in the
//! > deterministic layer immediately visible, and it is the single
//! > highest-value test in the suite.
//!
//! At M2 there is no LLM at all, so the *whole* simulation is deterministic and
//! this test covers everything. That will not be true again after M3 — which is
//! exactly why it is worth having now, while a violation can only mean a real
//! bug rather than the model being the model.
//!
//! The usual culprit is `HashMap` iteration order, so the assertions below
//! compare the full event stream rather than a summary: a hash of counts would
//! pass happily while two creatures swapped places.

use life_zone_lib::config::WorldConfig;
use life_zone_lib::sim::tick::Sim;
use life_zone_lib::sim::worldgen;

fn config(width: u32) -> WorldConfig {
    let mut cfg = WorldConfig::default();
    cfg.map.width = width;
    cfg.map.height = width;
    // Reproduction is M4; holding the census keeps a population in the world
    // for the whole run so the log has something in it to compare.
    cfg
}

/// Run and return (full event log, final state digest, per-tick digests).
fn run(seed: u64, creatures: u32, ticks: i64, cfg: WorldConfig) -> (String, u64, Vec<u64>) {
    let world = worldgen::generate(seed, &cfg).world;
    let mut sim = Sim::new(1, world, cfg, seed);
    sim.spawn_population(creatures);

    let mut log = String::with_capacity(1 << 20);
    let mut per_tick = Vec::with_capacity(ticks as usize);
    for _ in 0..ticks {
        sim.step();
        for e in &sim.events {
            log.push_str(&e.digest_line());
            log.push('\n');
        }
        per_tick.push(sim.state_digest());
    }
    (log, sim.state_digest(), per_tick)
}

#[test]
fn two_thousand_ticks_from_one_seed_produce_an_identical_event_log() {
    let mut cfg = config(256);
    cfg.bench.maintain_population = Some(120);

    let (log_a, digest_a, ticks_a) = run(44127, 120, 2_000, cfg.clone());
    let (log_b, digest_b, ticks_b) = run(44127, 120, 2_000, cfg);

    assert!(!log_a.is_empty(), "a run that logs nothing proves nothing");
    assert!(log_a.lines().count() > 10_000, "only {} lines", log_a.lines().count());

    // Report the first divergence rather than "the strings differ", because
    // finding the tick is most of the work of fixing it.
    if log_a != log_b {
        let first = log_a
            .lines()
            .zip(log_b.lines())
            .enumerate()
            .find(|(_, (a, b))| a != b);
        match first {
            Some((i, (a, b))) => panic!("event logs diverge at line {i}:\n  A: {a}\n  B: {b}"),
            None => panic!(
                "event logs have different lengths: {} vs {}",
                log_a.lines().count(),
                log_b.lines().count()
            ),
        }
    }

    let diverged = ticks_a.iter().zip(&ticks_b).position(|(a, b)| a != b);
    assert_eq!(diverged, None, "world state diverged at tick {diverged:?}");
    assert_eq!(digest_a, digest_b);
}

#[test]
fn a_different_seed_is_a_different_history() {
    let cfg = config(256);
    let (log_a, _, _) = run(44127, 60, 300, cfg.clone());
    let (log_b, _, _) = run(9_001, 60, 300, cfg);
    assert_ne!(log_a, log_b, "the seed has to matter");
}

/// The full-scale version of the golden run: the real map, the real population.
///
/// Ignored by default because it runs the simulation twice at 500 creatures and
/// dominates the suite's runtime. Run it with
/// `cargo test --release -- --ignored --nocapture` when touching anything in
/// the tick pipeline.
#[test]
#[ignore = "slow: 2 x 2,000 ticks at 500 creatures on the full map"]
fn the_full_size_golden_run_is_deterministic() {
    let mut cfg = config(512);
    cfg.bench.maintain_population = Some(500);

    let (log_a, digest_a, _) = run(44127, 500, 2_000, cfg.clone());
    let (log_b, digest_b, _) = run(44127, 500, 2_000, cfg);

    assert_eq!(log_a.len(), log_b.len(), "event log lengths differ");
    assert!(log_a == log_b, "the full-size event log is not reproducible");
    assert_eq!(digest_a, digest_b);
    println!(
        "golden run: {} event lines, digest {:016x}",
        log_a.lines().count(),
        digest_a
    );
}

// ------------------------------------------------------- schema round-trip

/// BUILD.md §9: "save a world mid-run, reload, continue; the resumed run must
/// match an uninterrupted one tick-for-tick with the LLM off."
///
/// This is the test that proves persistence is real rather than decorative. It
/// caught two things worth having caught: that a creature's accumulated ageing
/// and its shelter occupancy were both being dropped on reload, and that a
/// carried-forward RNG stream cannot be resumed without storing its position —
/// which is why the sim reseeds per tick from `(seed, tick)` instead.
#[test]
fn a_reloaded_world_continues_exactly_where_it_left_off() {
    use life_zone_lib::db;

    let mut cfg = config(192);
    cfg.bench.maintain_population = Some(80);
    let seed = 44127u64;
    let (halt, total) = (240i64, 480i64);

    // --- the control: one run, straight through ---------------------------
    let world = worldgen::generate(seed, &cfg).world;
    let mut straight = Sim::new(1, world, cfg.clone(), seed);
    straight.spawn_population(80);
    for _ in 0..total {
        straight.step();
    }

    // --- the same run, interrupted and resumed ----------------------------
    let dir = std::env::temp_dir().join(format!("life-zone-roundtrip-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("round-trip.sqlite3");
    let mut conn = db::open(&path).expect("open database");

    let world = worldgen::generate(seed, &cfg).world;
    db::repo::create_world(&conn, "RoundTrip", seed as i64, &cfg).expect("world row");
    db::repo::save_world(&mut conn, 1, &world).expect("save world");

    let mut first = Sim::new(1, world.clone(), cfg.clone(), seed);
    first.spawn_population(80);
    for _ in 0..halt {
        let mut report = first.step();
        first.persist(&mut conn, &mut report, false).expect("persist");
    }
    // Pausing forces a checkpoint, which is what a save actually is.
    let mut final_report = life_zone_lib::sim::tick::TickReport {
        tick: first.tick,
        ..Default::default()
    };
    first.persist(&mut conn, &mut final_report, true).expect("checkpoint");
    let halted_digest = first.state_digest();
    drop(first);

    let mut resumed = Sim::new(1, world, cfg, seed);
    resumed.load_from(&conn, halt).expect("load");

    assert_eq!(
        resumed.state_digest(),
        halted_digest,
        "reloading must reproduce the state that was saved, before anything else can hold"
    );

    for _ in halt..total {
        resumed.step();
    }

    assert_eq!(
        resumed.state_digest(),
        straight.state_digest(),
        "a resumed run must match an uninterrupted one tick for tick"
    );
    assert_eq!(resumed.alive(), straight.alive());
    assert_eq!(resumed.deaths_by_cause, straight.deaths_by_cause);

    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);
}
