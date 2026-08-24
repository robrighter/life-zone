//! Ollama integration, against a mock endpoint (BUILD.md §9).
//!
//! > Cover: malformed JSON, a plan referencing an illegal action, a plan with
//! > an out-of-range horizon, a timeout, and a response containing reasoning
//! > tokens. Every one must fall through to Tier 1 with a recorded reason and
//! > must never panic or stall the tick.
//!
//! These run against a socket rather than against the validator directly,
//! because the thing being tested is the whole path — client, worker thread,
//! adoption, fallback and the recorded reason. Validating a string in isolation
//! would not catch a worker that panics, a creature left without a plan, or a
//! tick that blocks for thirty seconds waiting on a dead endpoint.
//!
//! The suite is never gated on a live Ollama. One test at the bottom needs one
//! and is `#[ignore]`d.

use life_zone_lib::config::WorldConfig;
use life_zone_lib::sim::tick::Sim;
use life_zone_lib::sim::worldgen;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// What the mock does with each request.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Reply with this as the model's `message.content`.
    Content(&'static str),
    /// Accept the connection and never answer, so the client must time out.
    Hang,
    /// Answer with an HTTP error.
    ServerError,
}

struct Mock {
    port: u16,
    hits: Arc<AtomicUsize>,
}

impl Mock {
    fn start(behaviour: Behaviour) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind a mock endpoint");
        let port = listener.local_addr().unwrap().port();
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                counter.fetch_add(1, Ordering::SeqCst);
                if drain_request(&mut stream).is_err() {
                    continue;
                }
                match behaviour {
                    Behaviour::Hang => {
                        // Hold the connection open with no reply at all.
                        std::thread::sleep(Duration::from_secs(30));
                    }
                    Behaviour::ServerError => {
                        let _ = stream.write_all(
                            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n",
                        );
                    }
                    Behaviour::Content(text) => {
                        let payload = serde_json::json!({
                            "message": { "role": "assistant", "content": text },
                            "prompt_eval_count": 400,
                            "eval_count": 40,
                            "prompt_eval_duration": 500_000u64,
                        })
                        .to_string();
                        let _ = stream.write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                                 Content-Length: {}\r\n\r\n{}",
                                payload.len(),
                                payload
                            )
                            .as_bytes(),
                        );
                    }
                }
                let _ = stream.flush();
            }
        });

        Mock { port, hits }
    }

    fn endpoint(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
}

/// Read headers and body so the client is not left writing into a full buffer.
fn drain_request(stream: &mut TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut length = 0usize;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
            length = v.trim().parse().unwrap_or(0);
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
    }
    if length > 0 {
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body)?;
    }
    Ok(())
}

/// A run against a mock, and every decision it recorded.
///
/// Decisions are collected as they happen: `sim.decisions` is cleared at the
/// top of every tick, because it is a per-tick outbox for the persistence
/// layer, not a log.
struct Run {
    sim: Sim,
    decisions: Vec<life_zone_lib::sim::tick::DecisionRecord>,
}

impl Run {
    /// Every reason a model answer was not used.
    fn fallback_reasons(&self) -> Vec<String> {
        self.decisions
            .iter()
            .filter(|d| d.tier == 2)
            .filter_map(|d| d.fallback_reason.clone())
            .collect()
    }
}

/// A small world with the model pointed at a mock, run for a few ticks.
fn run_against(behaviour: Behaviour, ticks: i64) -> Run {
    let mock = Mock::start(behaviour);
    let mut cfg = WorldConfig::default();
    cfg.map.width = 128;
    cfg.map.height = 128;
    cfg.llm.endpoint = mock.endpoint();
    // Short, so a hung endpoint is a fast test rather than a slow one.
    cfg.llm.timeout_ms = 700;
    cfg.llm.max_concurrent = 2;
    cfg.deliberation.budget_observe = 4;

    let world = worldgen::generate(44127, &cfg).world;
    let mut sim = Sim::new(1, world, cfg, 44127);
    sim.spawn_population(30);
    sim.mode = life_zone_lib::sim::runner::SpeedMode::Observe;
    sim.enable_llm();

    let mut decisions = Vec::new();
    let step_and_check = |sim: &mut Sim, decisions: &mut Vec<_>| {
        let before = sim.alive() as u32;
        let started = Instant::now();
        let r = sim.step();

        // The two properties that matter on every single tick, whatever the
        // endpoint is doing. "Everybody acted" is the real form of "nobody
        // stalled": a creature whose plan finished this tick legitimately ends
        // the tick without one and is given a new one in phase 4 of the next,
        // which runs before phase 5 — so it never misses a turn.
        assert_eq!(
            r.acted, before,
            "a creature stalled waiting for the model at tick {}",
            r.tick
        );
        assert!(
            started.elapsed() < Duration::from_millis(400),
            "tick {} blocked for {:?} — deliberation must not sit on the tick loop",
            r.tick,
            started.elapsed()
        );
        decisions.extend(sim.decisions.iter().cloned());
    };

    for _ in 0..ticks {
        step_and_check(&mut sim, &mut decisions);
    }
    // Let anything outstanding land, then take delivery of it.
    std::thread::sleep(Duration::from_millis(900));
    for _ in 0..3 {
        step_and_check(&mut sim, &mut decisions);
    }
    let _ = mock.hits.load(Ordering::SeqCst);
    Run { sim, decisions }
}

#[test]
fn malformed_json_falls_through_to_tier_one_with_a_reason() {
    let run = run_against(Behaviour::Content("{\"steps\": [ {\"option\":"), 14);
    assert!(run.sim.llm_stats.dispatched > 0, "nothing was ever asked");
    assert!(run.sim.llm_stats.rejected > 0, "a broken answer should be rejected");
    assert_eq!(run.sim.llm_stats.accepted, 0);
    assert!(
        !run.fallback_reasons().is_empty(),
        "and the reason must be recorded, not merely counted"
    );
}

#[test]
fn a_plan_naming_an_action_that_was_never_offered_is_refused() {
    // Invariant 3 from the other side: the menu makes an illegal action
    // unnameable, so naming one is a rejection rather than an exploit.
    let run = run_against(
        Behaviour::Content(
            "{\"steps\":[{\"option\":9999}],\"commitment\":\"moderate\",\"rationale\":\"x\"}",
        ),
        14,
    );
    assert!(run.sim.llm_stats.rejected > 0);
    assert_eq!(run.sim.llm_stats.accepted, 0);
    assert!(
        run.fallback_reasons().iter().any(|r| r.contains("OPTION")),
        "the reason should say what was wrong: {:?}",
        run.fallback_reasons()
    );
}

#[test]
fn an_out_of_range_horizon_is_refused() {
    let mut cfg = WorldConfig::default();
    cfg.deliberation.model_estimates_horizon = true;
    // Exercised through the validator with the same config the sim would use;
    // the transport path is covered by the other cases here.
    let menu = life_zone_lib::ai::schema::ActionMenu::default();
    let out = life_zone_lib::ai::schema::validate(
        "{\"steps\":[{\"option\":1}],\"horizon\":99999,\"rationale\":\"forever\"}",
        &menu,
        &cfg,
    );
    assert!(out.is_err());
}

#[test]
fn a_timeout_is_survivable_and_never_blocks_a_tick() {
    // The property being tested is in `run_against`: every tick must finish
    // quickly and every creature must have acted, while the endpoint holds
    // every connection open and answers nothing.
    let run = run_against(Behaviour::Hang, 16);
    assert!(run.sim.llm_stats.dispatched > 0);
    assert!(
        run.sim.llm_stats.failed > 0,
        "a hung endpoint should be recorded as a failure, not silently ignored"
    );
    assert!(
        run.fallback_reasons().iter().any(|r| r.contains("TIMEOUT") || r.contains("OLLAMA")),
        "with a reason: {:?}",
        run.fallback_reasons()
    );
}

#[test]
fn an_http_error_is_survivable() {
    let run = run_against(Behaviour::ServerError, 12);
    assert!(run.sim.llm_stats.failed > 0);
    assert!(!run.fallback_reasons().is_empty());
}

#[test]
fn reasoning_tokens_around_a_good_plan_are_stripped_and_the_plan_used() {
    // qwen3 is a reasoning model. A `<think>` block is not a failure.
    let run = run_against(
        Behaviour::Content(
            "<think>It is thirsty. Option 1 goes to the water. I will pick {that}.</think>\n\
             ```json\n{\"steps\":[{\"option\":1}],\"commitment\":\"brief\",\
             \"rationale\":\"Water first.\"}\n```",
        ),
        16,
    );
    assert!(run.sim.llm_stats.accepted > 0, "a good plan wrapped in reasoning should be used");
    assert!(
        run.decisions.iter().any(|d| d.tier == 2 && !d.fallback_used),
        "and it should be recorded as an accepted tier-2 decision"
    );
    assert!(
        run.decisions
            .iter()
            .any(|d| d.tier == 2 && d.rationale.contains("Water first")),
        "with the model's own words kept, which is what makes the inspector readable"
    );
}

#[test]
fn an_endpoint_that_is_not_there_at_all_changes_nothing() {
    // The commonest real case: the user has not started Ollama. Tier 1 is
    // fully functional forever (invariant 1), so this must be unremarkable.
    let mut cfg = WorldConfig::default();
    cfg.map.width = 128;
    cfg.map.height = 128;
    cfg.llm.endpoint = "http://127.0.0.1:1".into();
    cfg.llm.timeout_ms = 400;

    let world = worldgen::generate(44127, &cfg).world;
    let mut sim = Sim::new(1, world, cfg, 44127);
    sim.spawn_population(25);
    sim.enable_llm();

    for _ in 0..20 {
        let before = sim.alive() as u32;
        let r = sim.step();
        assert_eq!(r.acted, before, "Tier 1 carries the whole simulation on its own");
    }
    assert_eq!(sim.llm_stats.accepted, 0);
    assert!(sim.alive() > 0);
}

/// The one test that needs a live Ollama.
#[test]
#[ignore = "needs a running ollama with the configured model"]
fn a_real_model_produces_plans_creatures_can_run() {
    let mut cfg = WorldConfig::default();
    cfg.map.width = 192;
    cfg.map.height = 192;
    cfg.deliberation.budget_observe = 4;
    // Hold the census. Without it the population ages out long before enough
    // calls have come back to say anything, and the test measures an empty
    // world rather than a model — which is exactly what it did first time.
    cfg.bench.maintain_population = Some(60);

    let world = worldgen::generate(44127, &cfg).world;
    let mut sim = Sim::new(1, world, cfg, 44127);
    sim.spawn_population(60);
    sim.mode = life_zone_lib::sim::runner::SpeedMode::Observe;
    // The pace the runner would set for Observe, so the round-trip latency is
    // measured in the same ticks the product would see.
    sim.ticks_per_second = 1.0; // Observe with the model on (§5.6)
    // Warm the world so creatures have beliefs worth reasoning about.
    for _ in 0..120 {
        sim.step();
    }
    sim.enable_llm();

    // Keep ticking until the model has answered everything it was asked, or a
    // generous deadline passes. A CPU-only host takes seconds per call and the
    // dispatch queue is deliberately shallow, so this is a wait on the actual
    // condition rather than on a guess about how long it takes.
    // Paced like Observe actually is (§5.6), not as fast as the loop will go:
    // how stale a dispatched plan gets is a function of wall-clock latency
    // against tick rate, so running the ticks faster than the product does
    // would measure a staleness the player never sees.
    let mut reasons: Vec<String> = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(150);
    while Instant::now() < deadline {
        sim.step();
        reasons.extend(
            sim.decisions
                .iter()
                .filter(|d| d.tier == 2)
                .filter_map(|d| d.fallback_reason.clone()),
        );
        std::thread::sleep(Duration::from_millis(1000));
        if sim.llm_stats.dispatched >= 10 && sim.llm_outstanding() == 0 {
            break;
        }
    }
    for _ in 0..5 {
        sim.step();
        reasons.extend(
            sim.decisions
                .iter()
                .filter(|d| d.tier == 2)
                .filter_map(|d| d.fallback_reason.clone()),
        );
    }

    println!(
        "dispatched {} accepted {} rejected {} failed {} — fallback {:.0}%, mean {:.0}ms",
        sim.llm_stats.dispatched,
        sim.llm_stats.accepted,
        sim.llm_stats.rejected,
        sim.llm_stats.failed,
        sim.llm_stats.fallback_rate() * 100.0,
        sim.llm_stats.mean_latency_ms(),
    );
    println!(
        "  round trip {:.0} ticks end to end (call {:.0}ms + time queued)",
        sim.llm_stats.mean_round_trip_ticks(),
        sim.llm_stats.mean_latency_ms(),
    );
    let mut why: std::collections::BTreeMap<String, usize> = Default::default();
    for r in &reasons {
        *why.entry(r.clone()).or_default() += 1;
    }
    for (r, n) in &why {
        println!("  {r}: {n}");
    }
    assert!(sim.llm_stats.accepted > 0, "a live model produced no usable plan");
    assert!(
        sim.llm_stats.fallback_rate() < 0.5,
        "more than half of all calls were unusable"
    );
}

/// Isolate the dispatcher from the simulation: one request, one answer.
#[test]
#[ignore = "needs a running ollama with the configured model"]
fn the_dispatcher_gets_an_answer_back_from_a_real_model() {
    use life_zone_lib::ai::ollama::{Depth, Dispatcher, Prompt, Request};
    use life_zone_lib::ai::schema::{response_schema, ActionMenu};

    let cfg = WorldConfig::default();
    let schema = response_schema(cfg.deliberation.model_estimates_horizon);
    let d = Dispatcher::new(&cfg.llm, schema.clone());

    let sent = d.dispatch(Request {
        creature_id: 1,
        issued_tick: 0,
        prompt: Prompt {
            system: "Reply with JSON only.".into(),
            user: "{\"steps\":[{\"option\":1}],\"commitment\":\"brief\",\"rationale\":\"x\"} \
                   — repeat that object back.".into(),
        },
        menu: ActionMenu::default(),
        depth: Depth::Shallow,
        schema,
        crisis_exempt: false,
    });
    assert!(sent, "the queue refused a single request");
    assert_eq!(d.outstanding(), 1);

    let got = d
        .wait_one(Duration::from_secs(60))
        .expect("no answer came back from the worker in 60s");
    assert_eq!(got.creature_id, 1);
    match &got.result {
        Ok(r) => println!("answered in {}ms: {}", r.latency_ms, r.raw.chars().take(120)
            .collect::<String>()),
        Err(e) => panic!("worker returned an error: {e:?}"),
    }
    assert_eq!(d.outstanding(), 0, "the slot must be released");
}
