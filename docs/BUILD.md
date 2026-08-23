# Life Zone — Implementation Handoff

**For:** the agent building this application
**Target platform:** Windows 11 (Tauri 2 desktop app)
**Date:** 2026-08-23
**Status:** design complete, zero implementation

---

## 0. Read this first

You are building **Life Zone**, a locally-run desktop simulation in which a community of
creatures — each driven by a local LLM — forages, farms, forms families, and dies. Creatures
live about four in-game weeks, so the unit that matters is the **lineage**, not the individual.

Three documents govern this build. **Read all three before writing code.**

| Document | What it is | Authority |
|---|---|---|
| `docs/PRD.md` | The full product spec — mechanics, data model, success criteria | **Source of truth.** If this handoff and the PRD disagree, the PRD wins |
| `design/mockups/` | Working HTML mockups of all five screens | **Visual source of truth.** Match these, don't reinvent |
| `docs/BUILD.md` | This file — sequencing, platform setup, invariants | How to get from nothing to done |

The PRD is ~750 lines and dense. Do not skim it. Sections §4 (simulation model), §5 (the
decision system), and §7 (data model) are load-bearing and you will get the architecture wrong
if you guess at them.

### The one-paragraph version

A 512×512 tile world holds up to 500 creatures. Each tick is one in-game hour. Most creature
behaviour is deterministic and cheap; a small budgeted subset of creatures per tick get a call
to a local LLM (`qwen3:8b` on Ollama), which returns a **plan** — a short sequence of goals plus
a committed horizon in ticks. Everything that happens is written to SQLite: every creature living
and dead, every event, every decision with its full prompt. A reporting view reads that database
back. The player watches and reads; they never give orders.

### The single most important thing

**Success criterion S6: the LLM must be load-bearing.** If a run with the LLM disabled produces
the same outcomes as one with it enabled, the LLM is decorative and the project has failed. This
is measured continuously from M3 onward, not checked at the end. Every architectural decision in
this handoff exists to protect it.

---

## 1. Getting the repo onto Windows

This repository was authored in WSL2. **Copy it to a native Windows path before building** —
do not build in place over `\\wsl.localhost\...`. Cargo does an enormous amount of small-file
I/O, and over the WSL network redirector a clean build can take several times longer and
occasionally fails outright on file locking.

```powershell
# from Windows, with the WSL distro running
robocopy \\wsl.localhost\Ubuntu\home\robrighter\development\life-zone C:\dev\life-zone /E /XD .git target node_modules
cd C:\dev\life-zone
git init
git add -A
git commit -m "Design: PRD and mockups"
```

Everything you need is in `docs/` and `design/`. There is no source code yet — you are starting
from an empty implementation.

---

## 2. Viewing the mockups on Windows

The mockups are static files with no build step. From the repo root:

```powershell
cd design\mockups
python -m http.server 8731
# then open http://localhost:8731/
```

Or `npx serve design/mockups`. They must be served over HTTP, not opened as `file://` — the
canvas rendering reads image data and will be blocked by the file-origin policy.

Start at `index.html`, which explains the design direction and links the rest. `palettes.html`
shows the three palettes side by side. Any screen accepts `?palette=survey` or `?palette=strata`.

**The mockups are a reference, not a starting codebase.** `mock.js` generates fake data and draws
a fake map; none of it should be ported wholesale. What *should* carry over verbatim:

- `app.css` and `palettes.css` — the entire token system, component styles, and the three palettes
- The `.lifespan` component — this is the product's signature device (§7.3 below)
- The chart helpers' *rules* (thin marks, legend always present for ≥2 series, de-collided direct
  labels, grouped-not-stacked for comparisons) — reimplement these properly, don't copy the mock

---

## 3. Windows environment setup

### 3.1 Prerequisites

Install in this order. Verify each before continuing.

**1. Visual Studio Build Tools 2022** — Rust's MSVC toolchain needs these.
Install "Desktop development with C++" workload. This is a multi-GB download; start it first.

**2. Rust (MSVC toolchain)**
```powershell
winget install Rustlang.Rustup
rustup default stable-x86_64-pc-windows-msvc
rustc --version   # expect 1.7x or later
```
Use the **MSVC** toolchain, not GNU. Tauri's Windows bundler and WebView2 bindings expect it.

**3. Node.js 20 LTS or later**
```powershell
winget install OpenJS.NodeJS.LTS
node --version
```

**4. WebView2 Runtime** — preinstalled on Windows 11. Verify:
```powershell
Get-ItemProperty "HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}" -ErrorAction SilentlyContinue
```
If absent, install the Evergreen Bootstrapper from Microsoft.

**5. Ollama + the model**
```powershell
winget install Ollama.Ollama
ollama pull qwen3:8b
ollama list
```

### 3.2 Verify Ollama before writing any LLM code

Do this now. It is the riskiest external dependency and you want to know its real latency on
this machine before you design around it.

```powershell
$body = @{
  model  = 'qwen3:8b'
  prompt = 'Reply with only this JSON and nothing else: {"ok": true}'
  stream = $false
  format = 'json'
} | ConvertTo-Json

Measure-Command {
  $r = Invoke-RestMethod -Uri http://localhost:11434/api/generate `
                         -Method Post -Body $body -ContentType 'application/json'
  $r.response
}
```

**Record the observed latency.** The PRD assumes roughly 200ms for a shallow structured
completion and up to ~1.3s for one with reasoning. If your machine is materially slower, the
deliberation budget defaults in §5.4/§5.5 of the PRD need adjusting, and you should say so
rather than silently shipping a simulation that runs at a tenth of the intended speed.

Note that `qwen3` is a reasoning model. Use `format: "json"` for structured output, and control
reasoning depth per the model's documented mechanism — the PRD's §5.4 "depth" knob maps onto it.
Verify how thinking tokens appear in the response and strip them before parsing.

### 3.3 Two Windows-specific build annoyances

**Defender will make `cargo build` slow.** Add an exclusion for the repo and the cargo registry:
```powershell
Add-MpPreference -ExclusionPath "$PWD"
Add-MpPreference -ExclusionPath "$env:USERPROFILE\.cargo"
```

**Long paths.** Rust's target directory nests deeply. Enable long paths:
```powershell
New-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name "LongPathsEnabled" -Value 1 -PropertyType DWORD -Force
```

**Line endings.** A `.gitattributes` is already committed with `* text=auto eol=lf`, so the tree
will not show as wholly modified after a checkout on either side. Leave it in place.

### 3.4 Scaffold

```powershell
npm create tauri-app@latest
# name: life-zone   ·  frontend: TypeScript / React  ·  package manager: npm
```

Scaffold into a temp directory and merge, or scaffold in place — but **do not overwrite `docs/`
or `design/`**. Those are the spec.

---

## 4. Stack and crate choices

Fixed by the PRD (§3.1): Tauri 2, Rust core, SQLite via `rusqlite`, TypeScript + React frontend,
Canvas2D map, Ollama over HTTP.

Suggested crates — deviate if you have a concrete reason, and say why:

| Purpose | Crate | Note |
|---|---|---|
| SQLite | `rusqlite` with `features = ["bundled"]` | **`bundled` is not optional on Windows** — it compiles SQLite from source and avoids requiring a system library |
| Async runtime | `tokio` | Tauri 2 already depends on it |
| HTTP to Ollama | `reqwest` with `json` | |
| Serialisation | `serde`, `serde_json` | |
| Seeded RNG | `rand` + `rand_chacha` | `ChaCha8Rng::seed_from_u64` — **reproducible across platforms**, unlike `StdRng` |
| Noise | `noise` crate, or hand-roll value noise | The mockup's `octaves()` in `mock.js` is a working reference |
| Pathfinding | `pathfinding` crate, or hand-rolled A* | Hand-rolled is fine and probably faster to tune |
| Tracing | `tracing` + `tracing-subscriber` | Phase timings go into `tick_stats` |

**Frontend:** React + TypeScript, Canvas2D for the map. No charting library — the mockups hand-roll
SVG and the dataviz rules are specific enough that a general-purpose library will fight you. Port
the chart helpers as proper typed components.

---

## 5. Architecture

Follow the repository layout in PRD §3.2. The parts that matter most:

### 5.1 Threading

```
┌─ main thread ──────────┐   ┌─ sim thread ─────────────┐   ┌─ tokio pool ────┐
│ Tauri, IPC, webview    │◄──┤ owns ALL world state     ├──►│ Ollama calls    │
│ never touches sim state│   │ runs the tick pipeline   │   │ N concurrent    │
└────────────────────────┘   └──────────────────────────┘   └─────────────────┘
                                        │
                                        ▼  batched, one tx per tick
                                    ┌────────┐
                                    │ SQLite │  WAL mode
                                    └────────┘
```

- **The simulation owns its state exclusively.** No `Arc<Mutex<World>>` shared with the UI. The
  UI gets snapshots pushed to it via Tauri events, and sends commands in via a channel. This is
  what guarantees the UI can never stall the tick loop, and it's why Fast-Forward mode can run
  thousands of ticks per second with the renderer idle.
- **LLM calls are the only await point in the tick.** Phase 4 dispatches the budgeted set
  concurrently and joins; every other phase is synchronous and fast.
- **One SQLite transaction per tick.** Never write per-entity. At 500 creatures × 4000 ticks,
  per-entity writes will dominate your runtime.

### 5.2 The tick pipeline

Implement exactly the seven phases in PRD §4.2, in order, in `sim/tick.rs`. Instrument each phase
and write the timings into `tick_stats.phase_timings_json` from day one — you will need them to
diagnose why a tick is slow, and "which phase" is the first question every time.

### 5.3 The decision system

This is the hardest part of the build and the part most likely to be got wrong. Read PRD §5 in
full. The three tiers:

- **Tier 0 (reflex)** — every creature, every tick, microseconds. Advance the current plan step.
- **Tier 1 (utility policy)** — deterministic fallback. Competent and boring. **This is the
  experimental control for S6** and must remain fully functional forever; it is not scaffolding
  to be removed once the LLM works.
- **Tier 2 (LLM deliberation)** — budgeted. Returns a plan, not an action.

The budget scheduler (`ai/budget.rs`) ranks creatures by deliberation pressure × age weight and
serves the top N. Everyone else falls to Tier 1. Pressure compounds so nobody starves indefinitely.

---

## 6. Milestones

Build in this order. **The ordering is deliberate and you should not resequence it.**

Each milestone has an exit criterion. Do not start the next one until the current one's criterion
demonstrably holds — not "looks about right", but verified and, where it's a number, measured.

---

### M0 — Scaffold

Tauri app opens and closes cleanly. SQLite database created with a migration runner. Config loaded
from `worlds.config_json`. Empty render loop. Logging to file.

**Exit:** `npm run tauri dev` opens a window, creates a `worlds` row, closes without error.

---

### M1 — World

Seeded worldgen (PRD §8) with the viability check. Chunked terrain storage. Canvas map renderer
with pan and zoom and per-chunk offscreen caching. Resource nodes painted into the terrain (see
§7.2 below — they are ground, not markers).

**Exit:** a seeded 512×512 world renders and pans at **≥30 FPS** with a frame-time counter proving
it. Same seed produces a byte-identical world twice.

---

### M2 — Deterministic life ← *the most important milestone*

Creatures, needs, life stages, lifespan, death with recorded cause. Tier 0 and Tier 1 only.
Actions, A* pathfinding with caching. Food spoilage and the fuel economy (§4.4). The belief
substrate (§4.11) — exploration, confidence decay, and the knowledge overlay. **No LLM at all.**

**Exit:** 500 creatures forage, drink, shelter, and die of plausible causes across 2,000 ticks.
Fast-Forward hits **<50ms per tick** measured over 1,000 ticks. Cause-of-death distribution is
not degenerate — no single cause above ~60%.

> **If the simulation is not interesting to watch at M2, adding an LLM will not save it.** It will
> only make a boring simulation slow. If you reach the exit criterion and the result is lifeless,
> stop and report that rather than proceeding — the fix is in the mechanics, not the model.

---

### M3 — Deliberation

Ollama client pool with timeouts and retries. Prompt assembly (§5.7) including belief relevance
ranking. Plan schema with committed horizons, strict validation, one repair retry, Tier 1
fallthrough. The budget scheduler. Thinking cost in fatigue and hunger. Full decision logging.

**Exit:** creatures visibly act on model-chosen multi-tick plans; every call is in `decisions`
with its prompt, latency, and outcome; **plan-abandonment rate is on a chart**; median tick in
Observe mode is ≤2s. Prototype both horizon-estimation approaches from PRD §13.9 and report which
performed better.

**Start measuring S6 here.** Log the Tier-1 action distribution alongside the Tier-2 one on every
tick. If they converge, escalate immediately — do not carry on and hope.

---

### M4 — Society

Households, shelter, courtship, reproduction, infants and dependency, food sharing, trait
inheritance. Knowledge *transmission* — observation, `SHARE_KNOWLEDGE`, `TEACH`. Age-weighted
deliberation and the elder habit prior (§5.4).

**Exit:** a lineage reaches **generation 3** with no manual intervention. Beliefs demonstrably
survive their discoverer's death (criterion S7). The age curve is instrumented against founder
outcomes (§13.7).

> Watch PRD §13.5 here: creatures may simply never choose to teach, because it costs ticks now and
> pays off only after they are dead. If that happens, try prompt framing first, then a small
> immediate affinity reward. **Do not hardcode parent→infant transfer** except as a last resort —
> it fakes the result the simulation exists to discover.

---

### M5 — Reporting

Everything in PRD §10, plus CSV export. Population, lineage, knowledge/culture, economy, and the
model-performance reports. Lineage tree via recursive CTE.

**Exit:** criterion S5 — any creature's full life, including every decision and prompt, is
reconstructable from the database alone.

---

### M6 — Tuning

Balance passes against S3/S4/S6/S7. Prompt iteration. Focus mode. Overlays. Performance.

**Exit:** all seven success criteria in PRD §2.3 pass on **three different seeds**.

---

## 7. Design system — what to port and what to preserve

The mockups encode decisions that took several iterations to get right. Some of them look like
arbitrary styling and are not. These are the ones that will break if you "clean them up":

### 7.1 The three palettes

`palettes.css` holds all three (Unlit, Survey, Strata) as `data-palette` attribute overrides on
`:root`. Ship all three with a switcher — the user has not made a final choice, and the switcher
costs nothing now and is expensive to retrofit.

**Every categorical palette is computationally validated** — lightness band, chroma floor,
colourblind separation, normal-vision floor, and contrast against its own surface. Do not add,
substitute, or "brighten" a categorical hue without re-running that validation. If you need a
seventh series, the answer is to fold into "Other", facet, or use small multiples — never to
generate a new hue.

### 7.2 Two entity classes must never share a palette

The map has three visual classes and they are separated by **form**, not just colour:

| Class | How it's drawn | Why |
|---|---|---|
| **Terrain** | Flat tile colour, low value | It's the substrate; data reads against it |
| **Resources** | Blended *into* the terrain as soft patches | They are *places*. Depletion fades them toward bare ground |
| **Creatures** | Crisp marks, single colour (`--quick`), dark/paper halo | They are *actors*. They sit on top |
| **Sheep** | Small pale un-haloed marks | They move, so they can't be ground — but they're subordinate to creatures |

This was originally got wrong: household colours reused the resource palette and both drew as
circles, making the map unreadable. **Creatures are one colour by default** — the palette's
`--quick`. Colouring by household is an opt-in overlay, and when it's on, the resource layer dims.

Resource hues are their own tokens (`--res-wheat`, `--res-wood`, `--res-forage`), read by both the
legend and the canvas, because each palette assigns them to different categorical slots and they
drifted apart when they were hardcoded.

### 7.3 The lifespan track

A 672-tick horizontal track showing infant/adult/elder zones, life spent, and a marker for *now*
— or, in the still colour, where a life actually ended against the baseline it was owed.

**This is the product's signature device.** It appears wherever a creature is named: inspector,
record, lists, lineage tree. Build it once as a component. It is the single most information-dense
element in the interface and it does more than any chart to make the simulation legible.

### 7.4 Chart rules

Non-negotiable, from the dataviz methodology:

- **Never a dual-axis chart.** Two measures of different scale → two charts or index to a base.
- **Grouped bars for comparisons, stacked only for parts of a whole.** "Share of population" vs
  "share of LLM calls" are not parts of a whole; stacking them is a lie.
- **Legend always present for ≥2 series**, plus selective direct labels — never a number on every
  point. Identity is never conveyed by colour alone.
- **De-collide direct labels.** Series that end close together will pile up; push them apart and
  clamp to the plot area.
- **Colour follows the entity, never its rank.** A filter that changes the series count must not
  repaint the survivors.
- Thin marks, recessive grid and axes, text in ink tokens never series colours, table view
  available for every chart.

### 7.5 Typography

Bricolage Grotesque (display, sparingly), IBM Plex Sans (UI), IBM Plex Mono (all data). **Bundle
the fonts locally** — do not link Google Fonts. This is an offline desktop app and a font request
to a CDN is both a broken dependency and a privacy leak.

---

## 8. Invariants — things that must stay true

Violating any of these will quietly break the project's purpose rather than producing an obvious bug.

1. **Tier 1 stays fully functional forever.** It is the control for S6, not scaffolding.
2. **Never batch multiple creatures into one prompt.** It entangles their reasoning into a group
   mind and destroys the independent-agent premise. See PRD Appendix A.
3. **The model chooses among pre-validated legal actions only.** The engine checks preconditions
   before offering the menu, so an impossible action can never be hallucinated.
4. **Every LLM call is recorded in full** — prompt, response, latency, cost, outcome. Storage is
   cheap; an unexplainable extinction is not.
5. **No per-creature-per-tick snapshot table.** Events plus periodic sampling. The naive version
   is hundreds of millions of rows nobody queries.
6. **Lineage is derived, never stored.** A recursive CTE over `mother_id`/`father_id`. Storing it
   creates a second source of truth to keep in sync.
7. **Worldgen is deterministic given a seed.** LLM decisions are not, which is exactly why the
   `decisions` table exists — replay-from-log is how a specific run is reproduced.
8. **Fallback rate and plan-abandonment rate are production metrics**, visible on the dashboard,
   not debug counters. A rising fallback rate means the LLM is quietly ceasing to matter.

---

## 9. Testing

- **Unit** — needs decay, spoilage expiry, lifespan modifiers, confidence decay, belief merge on
  transmission, plan validation and abort conditions, the pressure/age-weight calculation.
- **Property** — worldgen determinism (same seed → identical world); a creature with all needs
  satisfied never dies of anything but old age; belief confidence is monotonically non-increasing
  without a verify.
- **Golden-run** — with the LLM disabled and a fixed seed, 2,000 ticks must produce a
  byte-identical event log. This makes any accidental non-determinism in the deterministic layer
  immediately visible, and it is the single highest-value test in the suite.
- **Schema round-trip** — save a world mid-run, reload, continue; the resumed run must match an
  uninterrupted one tick-for-tick with the LLM off.
- **LLM integration** — mock the Ollama endpoint. Cover: malformed JSON, a plan referencing an
  illegal action, a plan with an out-of-range horizon, a timeout, and a response containing
  reasoning tokens. Every one must fall through to Tier 1 with a recorded reason and must never
  panic or stall the tick.

Do not gate the whole suite behind a live Ollama instance. One integration test may require it;
mark it `#[ignore]` by default.

---

## 10. Reporting back

When you finish a milestone, report: the exit criterion with its **measured** value (not a claim
that it passes), anything you deviated from and why, and any PRD open question (§13) that your
implementation has now answered. Several of those questions — whether creatures will teach,
whether the model can estimate horizons, whether the world is volatile enough to make horizon a
real choice — can only be settled by running the thing, and the answers should change the design.

If you hit something where the PRD is genuinely ambiguous or wrong, say so and propose a fix
rather than silently picking an interpretation. The PRD is a draft written before any code
existed; it is expected to be wrong somewhere.

---

## Appendix — quick reference

```powershell
npm run tauri dev              # dev with hot reload
npm run tauri build            # release bundle (MSI/NSIS)
cargo test --manifest-path src-tauri/Cargo.toml
cargo clippy --manifest-path src-tauri/Cargo.toml -- -D warnings
cd design\mockups; python -m http.server 8731    # view the design reference
ollama list                    # confirm qwen3:8b is present
```

**Key PRD sections by topic**

| Topic | Section |
|---|---|
| Tick pipeline | §4.2 |
| Resources, spoilage, fuel | §4.4 |
| Needs, lifespan, life stages | §4.5–4.7 |
| Reproduction and traits | §4.8–4.9 |
| Knowledge and beliefs | §4.11 |
| Decision tiers and budget | §5.2–5.3 |
| Age-weighted deliberation | §5.4 |
| Plans, horizons, thinking cost | §5.5 |
| Prompt construction | §5.7 |
| Action list | §6 |
| Database schema | §7 |
| Worldgen | §8 |
| Reports | §10 |
| Open questions | §13 |
