# M2 handoff

You are continuing **Life Zone**, a Tauri 2 desktop simulation. M0 and M1 are done and
committed. You are building **M2 — deterministic life**, which BUILD.md calls the most
important milestone in the project.

The repo is at `C:\dev\life-zone`. **Work there, not over `\\wsl.localhost\...`** — cargo does
enormous small-file I/O and the WSL redirector makes clean builds several times slower and can
fail on file locking.

## Start here, in this order

1. `docs/PRD.md` — the source of truth, ~750 lines. For M2 the load-bearing sections are
   **§4.2** (the tick pipeline), **§4.4** (resources, spoilage, fuel), **§4.5–4.7** (needs,
   lifespan, life stages), **§4.11** (beliefs), **§5.2** (decision tiers), **§6** (the action
   list) and **§7** (the data model). Do not skim §4.4 or §4.11 — both encode mechanics that
   look like flavour and are not.
2. `docs/BUILD.md` — §6 for the milestone definition, **§8 for the invariants**, §9 for the
   testing strategy.
3. The existing code. It is small and it will tell you more than a summary would:
   `src-tauri/src/sim/` (terrain, noise, world, worldgen), `src-tauri/src/db/`
   (schema + repo), `src-tauri/src/config.rs`, `src/map/` (renderer).
4. `git log` — the M0 and M1 commit messages record decisions and their reasons.

Run `cargo test --manifest-path src-tauri/Cargo.toml` before changing anything. 36 tests
should pass. If they don't, find out why before building on top.

## What already exists

- **Schema.** Every table in PRD §7 is already created by `db/migrations/001_initial.sql`,
  including `creatures`, `beliefs`, `events`, `tick_stats` and `creature_samples`. You should
  not need a migration for M2 — if you do, add `002_*.sql`; migrations are append-only and
  never edited once shipped.
- **Config.** `config.rs` already has `needs`, `lifespan`, `reproduction`, `resources`
  (including per-food spoilage and fire burn rate), `knowledge`, and the feature toggles.
  Read from these rather than introducing constants.
- **Worldgen.** Deterministic given a seed, with a viability check. `World::fingerprint()`
  exists for determinism assertions.
- **Terrain.** `Terrain::move_cost()` and `passable()` are already defined for A*.
- **Founders.** Worldgen produces `Vec<Founder>` (position + sex) but does **not** persist
  them. M2 turns them into rows in `creatures`, which is where they belong.

## What M2 is

Creatures, needs, life stages, lifespan, death with recorded cause. **Tier 0 and Tier 1 only —
no LLM at all, and no Ollama client.** Actions and A* pathfinding with caching. Food spoilage
and the fuel economy (§4.4). The belief substrate (§4.11): exploration, confidence decay, and
the knowledge overlay.

Implement the seven tick phases **exactly as ordered in PRD §4.2**, in `sim/tick.rs`, and write
per-phase timings into `tick_stats.phase_timings_json` **from the first commit**. "Which phase"
is the first question every time a tick is slow, and retrofitting the instrumentation once
you need it is miserable.

## Exit criterion — measure it, don't claim it

> 500 creatures forage, drink, shelter, and die of plausible causes across 2,000 ticks.
> Fast-Forward hits **<50ms per tick** measured over 1,000 ticks. Cause-of-death distribution
> is not degenerate — **no single cause above ~60%**.

Report the measured numbers: median and p95 tick time over a real 1,000-tick run, and the
actual cause-of-death histogram. Not "it passes".

## The one architectural change you must make

`AppState` currently holds `Mutex<Option<World>>`, and both M0 and M1 have comments saying M2
replaces it. Do it properly now rather than bolting a tick loop onto the mutex:

- **The simulation thread owns all world state exclusively.** No `Arc<Mutex<World>>` shared
  with the UI. The UI receives snapshots pushed via Tauri events and sends commands in over a
  channel. This is what guarantees the UI can never stall the tick loop, and it is why
  Fast-Forward can run thousands of ticks per second with the renderer idle.
- **One SQLite transaction per tick.** Never write per-entity. At 500 creatures × 2,000 ticks,
  per-entity writes will dominate your runtime.
- The existing `Mutex<Connection>` is fine for UI-side reads (WAL allows concurrent readers)
  but the tick loop's writes must not contend with it.

Getting this wrong is recoverable but expensive, and it gets more expensive every milestone.

## Invariants (BUILD.md §8) that bind at M2

1. **Tier 1 stays fully functional forever.** It is the experimental control for success
   criterion S6, not scaffolding to delete once the LLM works. Build it to be genuinely
   competent — it must feed a starving creature and send an idle one to forage — and never
   make it conditional on the LLM being off.
2. **No per-creature-per-tick snapshot table.** Events plus periodic sampling into
   `creature_samples` (default every 24 ticks). The naive version is hundreds of millions of
   rows nobody queries.
3. **Lineage is derived, never stored.** Recursive CTE over `mother_id`/`father_id`.
4. **Worldgen is deterministic given a seed**, and at M2 the *whole simulation* is, because
   there is no LLM yet. Protect that — see the golden-run test below.

## Testing

The highest-value test in the suite belongs to this milestone:

> **Golden run.** With a fixed seed and the LLM disabled, 2,000 ticks must produce a
> byte-identical event log. Any accidental non-determinism in the deterministic layer becomes
> immediately visible.

Build it early, not at the end. Iterating over a `HashMap` is the usual culprit; so is
anything that depends on wall-clock time or thread scheduling order. Also cover: needs decay,
spoilage expiry (oldest batch first), lifespan modifiers, confidence decay, and the property
that a creature with all needs satisfied never dies of anything but old age.

## Platform notes and gotchas that will otherwise cost you hours

- The machine is **ARM64 (Snapdragon X Elite), CPU-only**. `rust-toolchain.toml` pins `stable`
  for the host triple so you get a native build — don't override it to x86_64, the M2 tick
  budget cannot afford emulation.
- **`cmd.exe` cannot `cd` from a WSL working directory** (UNC paths unsupported). Use
  `cmd.exe /c "cd /d C:\dev\life-zone && ..."`.
- **WebView2 ignores synthetic mouse clicks**, so you cannot drive the UI by injecting input.
  There is already a pattern for this: `LIFE_ZONE_BENCH=1` makes the frontend run its render
  benchmark on load and report the result through a Tauri command into the Rust log. Do the
  same for anything you need to measure from inside the webview.
- **Screenshots lie when downscaled.** Both M1 rendering bugs — resource patches drawing as
  literal `+` glyphs, and chunk seams from CSS-pixel rounding under a 1.5× DPR transform —
  were invisible until the PNG was cropped and inspected at 1:1. When you change the renderer,
  look at actual pixels.
- Capturing the window needs `SetProcessDpiAwarenessContext(-4)` first, or `GetWindowRect`
  returns DPI-virtualised coordinates and you capture two-thirds of the window.
- **`app.css` and `palettes.css` are the mockup design system carried over verbatim.** Several
  things in them look like arbitrary styling and are not. Add new rules; do not "clean up"
  existing components. Reuse `.kv`, `.sec`, `.legend`, `.readout`, `.btn` rather than inventing
  parallel ones — that mistake was already made once at M0 and had to be reverted.
- Two distinct `World` types exist: `repo::WorldRow` (the `worlds` table row) and
  `sim::world::World` (the tile grid). Keep them straight.

## Two things that matter more than they look

**If the simulation is not interesting to watch at M2, adding an LLM will not save it.** It
will only make a boring simulation slow. If you hit the exit criterion and the result is
lifeless — creatures milling about, deaths that read as arbitrary, nothing you'd want to
follow — **stop and say so** rather than proceeding to M3. The fix is in the mechanics, not
the model. This is an explicit instruction in both the PRD and BUILD.md, and it is the single
most useful judgement you can offer on this milestone.

**Tier 1 is the control for S6, so build it honestly.** There is a temptation to make the
deterministic policy slightly bad so the LLM looks good later. Resist it completely. A weak
Tier 1 doesn't make the LLM load-bearing, it makes the experiment worthless — and S6 is the
criterion the whole project is judged on.

## How to work

- One milestone. Do not start M3, and do not add an Ollama client "while you're in there".
- The PRD is a draft written before any code existed. Where it is ambiguous or wrong, **say so
  and propose a fix** rather than silently picking an interpretation. Two known-soft spots you
  will hit: §13.4 (how beliefs are selected for a prompt — the substrate is built here and
  wants a relevance ranking over confidence, distance, recency and current need), and the
  interaction between `SOIL_WATER_REACH` and `reproduction.store_reserve`, which jointly decide
  whether any lineage can ever reach the reproduction gate.
- **Stop and report at the end.** Give the measured exit criterion, anything you deviated from
  and why, any PRD §13 open question your implementation answered, and your honest read on
  whether the thing is interesting to watch.
