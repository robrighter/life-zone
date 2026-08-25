# Life Zone

A locally-run desktop simulation in which a community of creatures — each driven by a local
LLM — forages, farms, forms families, and dies.

Life Zone is a Tauri 2 desktop app holding a 512×512 tile world and up to 500 creatures. One
tick is one in-game hour; a creature lives about four in-game weeks, so the unit that matters
is the **lineage**, not the individual. Creatures forage, drink, chop wood, plant and harvest
wheat, build shelter, court, form households, reproduce, teach their young, and pass beliefs
to each other — beliefs which decay in confidence, carry provenance, and can be wrong. Their
decisions come from a three-tier system: a Tier 0 reflex that advances the current plan, a
Tier 1 deterministic utility policy, and a Tier 2 call to a local model on Ollama that returns
a multi-step plan with a committed horizon. The player never gives orders; they build the
world, set the rules, watch, and read the record. **The log is the product** — every event and
every decision, with its full prompt, latency and outcome, is written to SQLite and read back
through a reporting layer.

- **Spec:** [`docs/PRD.md`](docs/PRD.md) — source of truth
- **Plan:** [`docs/BUILD.md`](docs/BUILD.md) — milestones, invariants, testing
- **Design:** [`design/mockups/`](design/mockups/) — visual source of truth

## Status

| Milestone | State |
|---|---|
| **M0 — Scaffold** | Done. Tauri app, SQLite migration runner, config, logging |
| **M1 — World** | Done. Seeded worldgen with viability check, chunked storage, Canvas2D map with pan/zoom |
| **M2 — Deterministic life** | Done. Needs, life stages, death with cause, actions, A* pathfinding, spoilage and fuel, the belief substrate, Tier 0/1. Exit criterion measured (below) |
| **M3 — Deliberation** | Done. Ollama client and dispatcher, prompt assembly, plan schema with strict validation, budget scheduler, thinking cost, full decision logging |
| **M4 — Society** | Done (brought forward ahead of M3 — see the commit history). Households, courtship, reproduction, infants, inheritance, knowledge transmission, age-weighted deliberation |
| **M5 — Reporting** | **In progress.** The Rust side is built: `report::queries` and `report::culture`, eighteen report commands, CSV export, and migration `004_reporting.sql` for the two aggregations the event log could not supply. The React reporting view is not built yet — `src/` has the map and inspector but no `report/` |
| **M6 — Tuning** | Not started. Balance passes against S3/S4/S6/S7, prompt iteration, Focus mode, overlays |

None of the seven success criteria in PRD §2.3 have been signed off on three seeds; that is
M6's exit. S6 has been measured once (below) and the result is encouraging rather than
conclusive.

## Architecture

```
┌─ main thread ──────────┐   ┌─ sim thread ─────────────┐   ┌─ dispatcher ────┐
│ Tauri, IPC, webview    │◄──┤ owns ALL world state     ├──►│ Ollama calls    │
│ never touches sim state│   │ runs the tick pipeline   │   │ off the tick    │
└────────────────────────┘   └──────────────────────────┘   └─────────────────┘
                                        │
                                        ▼  one transaction per tick
                                    ┌────────┐
                                    │ SQLite │  WAL mode
                                    └────────┘
```

**The simulation owns its state exclusively.** There is no `Arc<Mutex<World>>` shared with the
UI. The sim thread publishes snapshots that are pushed to the webview as `tick:complete` Tauri
events, and receives commands over an mpsc channel. The UI can therefore never stall the tick
loop, which is what lets Fast-Forward run thousands of ticks with the renderer idle. Reports
run on a second, read-only SQLite connection; WAL means opening a report while a run is going
cannot block a tick.

**One SQLite transaction per tick.** Never per entity — at 500 creatures across thousands of
ticks, per-entity writes dominate everything else.

**The seven-phase tick pipeline** (`src-tauri/src/sim/tick.rs`, PRD §4.2), in order:

1. **World update** — resource regrowth, crop stages, sheep, food spoilage, fires burning wood
2. **Needs decay** — hunger, thirst, fatigue, warmth; health integrates sustained deficits
3. **Plan expiry and interrupt detection** — decrement committed horizons, flag re-deliberation
4. **Deliberation** — the budgeted set get LLM calls and pay the fatigue cost; everyone else
   continues a committed plan or gets one from Tier 1. *This is the only unbounded phase*
5. **Reflex and action execution** — every creature advances one step of its current plan
6. **Resolution** — births, deaths, pairings, structures completed; events emitted
7. **Persist** — events, dirty creatures and decisions, in one transaction

Every phase is timed and the timings go into `tick_stats.phase_timings_json`, because "which
phase" is the first question every time a tick is slow.

**Determinism.** Worldgen is reproducible from a seed. The deterministic layer is reproducible
tick-for-tick: creatures are visited in ascending id order, no traversal depends on `HashMap`
iteration, and every random draw comes from a ChaCha8 stream reseeded per tick from
`(seed, tick)` — so changing how many draws one phase makes cannot shift another's rolls. The
golden-run test (2,000 ticks with the LLM off must produce a byte-identical event log) is what
keeps this honest. LLM decisions are *not* deterministic, which is precisely why the
`decisions` table stores everything: replay-from-log is how a specific run is reproduced.

Lineage is derived, never stored — a recursive CTE over `mother_id`/`father_id`.

## The three decision tiers

**Tier 0 — Reflex.** Every creature, every tick, microseconds. Execute the current plan step.
No thinking and no metabolic cost beyond the action itself, which is what makes 500 creatures
affordable and a long committed horizon genuinely cheap for the creature.

**Tier 1 — Utility policy** (`src-tauri/src/ai/policy.rs`). A deterministic scored decision
over the goals that are currently legal, given needs, inventory, beliefs and traits. Competent
and myopic: it will feed a starving creature and send an idle one to the nearest forage it
believes in. It reads *beliefs, never ground truth* — a creature walks to where it thinks the
berries are, and if it is wrong it arrives at an empty clearing and re-plans having wasted the
trip.

**Tier 2 — LLM deliberation.** A budgeted call to Ollama. The model gets felt state, traits as
personality, a 15×15 local view, beliefs rendered with confidence and provenance in plain
language, nearby creatures, household status, and a numbered menu of goals the engine has
already validated as legal. It returns a **plan** — a short sequence of steps plus a committed
horizon — not a single action. One call buys many ticks of coherent behaviour.

**Tier 1 has to stay good, permanently.** It is the experimental control for success criterion
S6: *replacing LLM deliberation with the deterministic fallback must produce visibly different,
worse outcomes.* There is a standing temptation to leave the fallback slightly weak so the
model looks impressive. That would not make the model load-bearing; it would make the
experiment worthless. Tier 1 is also not scaffolding to be removed once Tier 2 works — it
guarantees no creature ever stalls waiting for a call, and it is what every LLM failure falls
through to.

## Running it

### Prerequisites

- **Rust** (stable). `rust-toolchain.toml` pins `stable` for the *host* triple deliberately —
  see Platform notes below. On Windows use the MSVC toolchain and install the Visual Studio
  2022 "Desktop development with C++" workload.
- **Node.js 20 LTS or later.**
- **WebView2 runtime** — preinstalled on Windows 11.
- **Ollama**, only if you want Tier 2. The simulation runs fully without it; every creature
  falls to Tier 1 and the run stays deterministic. Default model tag is `qwen3:1.7b` (not the
  PRD's `qwen3:8b` — see Platform notes).

```
npm install
npm run tauri dev
```

Tests:

```
cargo test --manifest-path src-tauri/Cargo.toml
```

Around 230 tests. Nothing in the suite requires a live Ollama; the LLM integration tests mock
the endpoint, and the one test that needs a real server is `#[ignore]` by default.

Lint:

```
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
```

### The measurement harness

BUILD.md §6 asks for each milestone's exit criterion *measured*, not claimed, so the numbers
below come from `src-tauri/src/bin/measure.rs` rather than from a guess. Build it in release —
the <50ms tick budget is about the binary that ships.

```
cargo run --release --manifest-path src-tauri/Cargo.toml --bin measure -- <subcommand>
```

| Subcommand | Arguments (with defaults) | What it does |
|---|---|---|
| `world` | `[seed=44127]` | World composition and food balance for a seed |
| `run` | `[seed=44127] [creatures=500] [ticks=2000] [hold]` | The tick-time and survival run, persisting to SQLite for real |
| `llm` | `[seed=44127] [ticks=60] [calls=6]` | End-to-end deliberation against a live Ollama |
| `s6` | `[seed=44127] [ticks=150] [calls=30]` | Asks both tiers about the same creature in the same state and compares |

`creatures=0` on `run` spawns founders instead of a fixed population. The trailing literal
`hold` floors the census with a benchmark fixture (settlers replace the dead) so a performance
measurement is not confounded by the population collapsing; without it the population is
emergent.

`run` accepts environment-variable dial overrides so the PRD §13.1 balance questions can be
answered by experiment rather than by rebuilding:

| Variable | Overrides |
|---|---|
| `LZ_RESERVE` | Household grain reserve required to reproduce |
| `LZ_INFANT` | Infant dependency duration in ticks |
| `LZ_SPACING` | Minimum ticks between births |
| `LZ_WHEAT_YIELD` | Grain harvested per tick |
| `LZ_SOIL` | Soil density in worldgen |
| `LZ_ELDER` | Tick at which a creature becomes an elder |
| `LZ_GESTATION` | Gestation length in ticks |
| `LZ_SHELTER_COST` | Wood cost of a shelter |

```
LZ_INFANT=96 LZ_RESERVE=12 cargo run --release --manifest-path src-tauri/Cargo.toml \
  --bin measure -- run 44127 0 2000
```

### Design mockups

They must be served over HTTP, not opened as `file://` — the canvas rendering reads image data
and the file-origin policy blocks it.

```
cd design/mockups && python -m http.server 8731
```

## Repository layout

```
src-tauri/src/
  lib.rs            app state, Tauri commands, the report commands, CSV export
  config.rs         WorldConfig — every dial in PRD §11
  sim/
    tick.rs         the seven-phase pipeline; per-tick RNG; decision records
    world.rs        world state, tiles, chunks, resource nodes
    worldgen.rs     seeded terrain, resource placement, viability check
    terrain.rs      biome classification and chunk storage
    noise.rs        layered value noise for elevation and moisture
    creature.rs     creature struct, needs, traits, life stages, plans
    actions.rs      the v1 action set with preconditions and costs
    pathfind.rs     A* over the tile grid, cached
    perception.rs   the local view and world cache
    knowledge.rs    beliefs, confidence decay, provenance, transmission
    social.rs       households, relationships, courtship, reproduction
    economy.rs      nodes, regrowth, inventories, spoilage, structures, fire
    event.rs        the event log spine
    runner.rs       the sim thread, speed modes, snapshots, commands
  ai/
    policy.rs       Tier 1 — the deterministic utility policy (the S6 control)
    budget.rs       deliberation pressure, age weighting, thinking cost
    prompt.rs       prompt assembly and the pre-validated action menu
    schema.rs       plan schema and response validation
    ollama.rs       the client and the dispatcher that keeps calls off the tick
  db/
    mod.rs          connection, WAL, migration runner
    repo.rs         typed read/write helpers
    migrations/     001_initial, 002_sim_state, 003_society, 004_reporting
  report/
    queries.rs      population, lineage, economy, actions, planning aggregations
    culture.rs      the knowledge/culture reports and the S6/S7 correlations
  bin/
    measure.rs      the measurement harness

src/
  App.tsx           shell, world controls, speed modes, palette switcher
  map/              Canvas2D renderer, chunk cache, viewport, palette
  panels/           creature inspector, the lifespan track component
  ipc/              typed Tauri bindings
  ui/               palette hook
  styles/           app.css and palettes.css — the token system and three palettes
  assets/fonts/     Bricolage Grotesque and IBM Plex, bundled (no CDN)

docs/               PRD.md, BUILD.md, M2-PROMPT.md
design/mockups/     the five screens as working HTML
tools/              PowerShell screenshot and crop helpers
```

## Measured results so far

These are measurements with their conditions, not claims about the design.

**M2 exit criterion — met.** Over 1,000 ticks at 500 creatures with persistence on (phase 7
writing to SQLite for real, since a tick-time measurement that skips the write is not a
measurement of the thing the criterion is about):

- p50 **4.32 ms/tick**, p95 **20.03 ms**, p99 **29.22 ms** against a 50 ms budget
- **0.30%** of ticks over budget
- Largest single cause of death **38.5%**, against the "no cause above ~60%" non-degeneracy bar

**M3 / S6 — the two tiers do diverge.** Asked about the same creature, in the same state, from
the same pre-validated menu:

- The tiers choose differently on **25%** of decisions
- The model writes **3.64-step** plans against Tier 1's ~2
- **23%** fallback rate
- **~4.5 s** per call, and a **13-tick** end-to-end round trip from asking to taking delivery,
  on ARM64 CPU-only Ollama

**M4 open finding — at PRD default dials, Tier 1 cannot sustain lineages.** The arithmetic:
setup to a first child takes roughly 300 ticks, a second needs gestation plus birth spacing on
top of that, and the adult window is only 588 − 168 = **420 ticks**. Two children per couple is
therefore not merely unlikely, it is impossible. This is the failure mode PRD §13.1 predicted
and it is a tuning problem, not a bug. **No design numbers were changed** — retuning is M6's
job, and infant duration is the dominant dial per §13.1's stated order (infant duration, then
the reserve threshold, then grain yield). The `LZ_*` overrides above exist so that pass can be
run as an experiment.

## Known limitations and loose ends

- **Prompt-text retention is unimplemented.** `true_for_recent` in `src-tauri/src/sim/tick.rs`
  is a stub that unconditionally returns `true`, so `llm.retain_prompt_text_ticks` has no
  effect and every prompt is stored forever. That is roughly **105 MB per 2,000 ticks**. PRD §7
  anticipated this ("add a retention setting if it becomes a problem"); the config field exists
  and the policy behind it does not.
- **Focus mode is a budget number, not a focus.** `SpeedMode::Focus` currently just sets a
  smaller per-tick budget (`budget_focus = 2`) and a tick pacing. It does not follow a chosen
  lineage or concentrate the budget on it, which is the thing PRD §5.6 calls "the one to get
  right". It lands at M6.
- **`HERD_SHEEP`, `BUILD_PEN` and `REQUEST_FOOD` are deferred.** Sheep exist in the world and
  in worldgen, but the herding path is not implemented, so the "capital/compounding" leg of the
  three-food risk portfolio (PRD §4.4) is not yet exercisable.
- **The reporting view is backend-only.** All eighteen report queries and CSV export are
  reachable over IPC, but there is no React reporting screen yet. M5's exit criterion (S5 —
  any creature's full life reconstructable) is satisfiable from the database and the commands,
  not from the UI.

## Platform notes

`rust-toolchain.toml` pins `stable` for the **host** triple. The primary dev machine is ARM64
(Snapdragon X Elite); an x86_64 build there would run under Prism emulation, which the M2 tick
budget cannot afford.

Ollama on that machine is **CPU-only**, which makes deliberation far more expensive than
PRD §5.1 assumes. Measured with a ~760-token prompt:

| model | median call | calls/sec |
|---|---|---|
| qwen3:8b (the PRD's target) | 16.2s | 0.06 |
| qwen3:4b | 11.0s | 0.09 |
| qwen3:1.7b (**current default**) | 6.8s | 0.15 |

Two consequences are baked into the config defaults:

- **Concurrency buys nothing** on a CPU-only host — throughput is flat from 1 to 6 concurrent
  calls, because it is compute-bound rather than I/O-bound.
- **Prompt ingestion dominates**, and Ollama's prefix cache is exploitable: ordering the prompt
  static-first and creature-specific-last cut prompt-eval from 3.82s to 0.58s. This is
  `llm.static_prefix_ordering` and it is worth more than model choice.

Fonts are bundled in `src/assets/fonts/`. The app makes no network requests except to Ollama
on localhost, and the webview CSP enforces that.

On Windows, add Defender exclusions for the repo and `~/.cargo` before building, and enable
long paths — `cargo build` is otherwise slow enough to be misleading. BUILD.md §3.3 has the
commands.

## License

No license file is present in this repository. The project is unlicensed — all rights reserved
by default until one is added.
