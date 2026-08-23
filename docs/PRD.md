# Life Zone — Product Requirements Document

**Status:** Draft v1
**Date:** 2026-08-23
**Owner:** Rob Righter

---

## 1. Summary

Life Zone is a locally-run desktop simulation of a community of living creatures on a large, scrollable top-down map. Creatures are autonomous agents whose decisions are made by a local LLM running on Ollama. They forage, farm, herd, build shelter, form families, and reproduce. Every creature has a short life — roughly four in-game weeks — so the interesting unit of play is not the individual but the **lineage**.

The player does not command creatures. The player builds the world, sets the rules, watches, and reads the record. The win condition belongs to the creatures: **carry a bloodline as far as possible.**

Everything runs on the user's machine. No network calls. All state — living, dead, and historical — persists in SQLite.

### 1.1 What makes this interesting

This is not a god game and it is not a colony sim. It's an **observation instrument for emergent LLM behavior under scarcity**. The compelling questions are things like: does a community that discovers wheat farming out-survive one that stays foraging? Do creatures learn to feed dependent children, or does generation 3 starve? Does a "share food with kin" norm emerge without being coded? The simulation exists to produce answers you can read out of the event log.

That framing drives two design commitments that run through this whole document:

1. **The log is the product.** Rendering is how you watch; the database is what you keep. Every decision, its prompt, its latency, and whether it was LLM-driven or a fallback gets recorded.
2. **The LLM must make decisions that matter, not decisions that are frequent.** See §5.

---

## 2. Goals and Non-Goals

### 2.1 Goals

- **G1** — A 512×512 tile world with water, forage, wood, wheat and sheep, generated from a seed and fully scrollable/zoomable at interactive frame rates.
- **G2** — Up to 500 concurrent creatures with individual persistent state: needs, inventory, family, home, relationships, memory.
- **G3** — Creature decision-making driven by a local Ollama model (`qwen3:8b`), with every decision recorded and auditable.
- **G4** — Complete lifecycle: birth → dependency → adulthood → courtship → reproduction → decline → death, with cause of death always recorded.
- **G5** — Survival pressure that actually bites: no food or no shelter measurably shortens life.
- **G6** — SQLite persistence of all creatures living and dead, all events, and all LLM decisions. Save/resume a world.
- **G7** — Knowledge as a first-class, fallible, transmissible resource: creatures explore, form beliefs, teach their young, and share with each other, so that knowledge can outlive its discoverer (§4.11).
- **G8** — A comprehensive reporting view: population curves, lineage trees, cause-of-death breakdowns, resource economy, per-creature life stories, and LLM performance stats.

### 2.2 Non-Goals for v1

- Combat, warfare, or inter-tribe conflict.
- Multiplayer or any networked feature.
- Direct player control of individual creatures (the player is an observer and world-setter, not a commander).
- 3D or isometric rendering.
- Modding APIs, Steam integration, distribution/packaging beyond a local build.
- Creature-to-creature **free-text** dialogue. Creatures do communicate in v1, but through structured social actions and typed belief transfer (§4.11) — not generated speech. Negotiation, persuasion, and deception all require language and are v2 candidates (§13).

### 2.3 Success criteria

v1 is done when all of the following hold on the developer's machine:

| # | Criterion | Measure |
|---|---|---|
| S1 | The world is watchable | ≥30 FPS panning a 512×512 map with 500 creatures rendered |
| S2 | Ticks are affordable | Median tick ≤ 2s in Observe mode; ≤ 50ms in Fast-Forward mode |
| S3 | Lineages actually persist | At least one seeded world reaches generation 5+ without manual intervention |
| S4 | Scarcity is real | Turning off wheat farming collapses lineage depth across 3 seeded runs — with no storable food, households cannot reach the reproduction reserve (§4.4) |
| S5 | Decisions are auditable | Any creature's full life — every decision, prompt, and outcome — is reconstructable from the DB |
| S6 | The LLM is load-bearing | Replacing LLM deliberation with the deterministic fallback produces visibly different, worse outcomes |
| S7 | Knowledge outlives its discoverer | In a gen-5 run, a measurable share of beliefs in circulation originate from creatures already dead |

**S6 is the one that matters most.** If a run with the LLM disabled performs the same as one with it enabled, the LLM is decorative and the design has failed. This should be measured continuously, not at the end.

---

## 3. Platform and Architecture

### 3.1 Stack

- **Shell:** Tauri 2 (Rust core + webview UI), packaged as a native desktop app.
- **Core / simulation:** Rust. Owns the tick loop, world state, SQLite, and the Ollama client pool.
- **Persistence:** SQLite via `rusqlite` (bundled), WAL mode.
- **LLM:** Ollama HTTP API at `http://localhost:11434`, model `qwen3:8b` (exact tag configurable; any local chat model with JSON-ish output works).
- **UI:** TypeScript + React for panels and reporting; Canvas2D (WebGL upgrade path) for the map.
- **IPC:** Tauri commands for queries/control; Tauri events for the push stream (`tick:complete`, `creature:died`, `decision:made`).

**Why Rust owns the sim:** 500 creatures × per-tick reflex updates × pathfinding × resource regrowth is real per-tick work, and it has to coexist with concurrent LLM I/O. Keeping it in Rust with the renderer as a thin consumer means the UI can never stall the simulation, and Fast-Forward mode (which runs thousands of ticks with no rendering and no LLM) stays cheap.

### 3.2 Repository layout

```
life-zone/
  docs/
    PRD.md
  src-tauri/
    src/
      main.rs
      sim/
        world.rs        world state, tiles, chunks
        worldgen.rs     seeded terrain + resource placement
        creature.rs     creature struct, needs, lifecycle
        tick.rs         the tick pipeline (§4.2)
        actions.rs      action definitions + execution
        pathfind.rs     A* over the tile grid, cached
        social.rs       households, relationships, courtship
        knowledge.rs    beliefs, confidence decay, transmission
        economy.rs      resource nodes, regrowth, inventories, spoilage
      ai/
        budget.rs       deliberation scheduler (§5.3)
        policy.rs       deterministic utility fallback (§5.2)
        prompt.rs       prompt assembly + local map view
        ollama.rs       client pool, retries, timeouts
        schema.rs       plan schema + response validation
        plan.rs         plan execution, horizon tracking, aborts
      db/
        schema.sql
        migrations/
        repo.rs         typed read/write helpers
      report/
        queries.rs      the reporting aggregations (§10)
  src/
    map/                canvas renderer, chunk cache, viewport
    panels/             creature inspector, world controls
    report/             charts + lineage tree
    ipc/                typed Tauri bindings
```

---

## 4. Simulation Model

### 4.1 Time

| Unit | Value |
|---|---|
| Tick | 1 in-game hour |
| Day | 24 ticks |
| Week | 168 ticks |
| Baseline lifespan | 4 weeks = **672 ticks** |

Day/night matters: night ticks (20:00–06:00) apply an exposure penalty to any creature that is neither sheltered nor beside a lit fire, and reduce forage yield. This is what makes shelter load-bearing rather than decorative — and what gives carried firewood (§4.4) its value on long journeys.

### 4.2 The tick pipeline

Each tick runs these phases in order. Phases 1–3 and 5–7 are deterministic and fast; phase 4 is the only one that touches the LLM.

1. **World update** — resource regrowth, crop growth stages, sheep wander/breed, **food spoilage in inventories and household stores, fires consuming wood or guttering out** (§4.4), weather/season advance.
2. **Needs decay** — hunger, thirst, fatigue, warmth decrement per creature. Health responds to sustained deficits (§4.5).
3. **Plan expiry and interrupt detection** — decrement committed horizons; flag creatures whose plan ran out, whose plan became impossible, or whose situation changed enough to warrant rethinking (§5.3, §5.5).
4. **Deliberation** — the budgeted set of flagged creatures get LLM calls, concurrently, and pay the fatigue cost. Everyone else continues their committed plan or gets a fallback plan from the utility policy.
5. **Reflex + action execution** — every creature advances one step of its current plan: move along path, gather, eat, drink, build, plant, harvest, court, feed a child, deposit to household store.
6. **Resolution** — births, deaths, pairings, structure completion. Emit events.
7. **Persist** — write events, dirty creatures, and decisions to SQLite in one transaction.

**Phase 4 is the only phase whose cost is unbounded**, which is why it's the one under an explicit budget. Phases 1–3 and 5–7 must stay within the Fast-Forward performance target (S2) on their own.

### 4.3 Terrain

Seeded generation (simplex noise for elevation + moisture, then biome classification):

| Terrain | Passable | Notes |
|---|---|---|
| Deep water | No | Visual boundary; blocks movement |
| Shallow water | Yes (slow) | Drinkable; enables irrigation adjacency |
| Grass | Yes | Default; sheep graze here |
| Forest | Yes (slow) | Hosts wood nodes and forage |
| Soil | Yes | The only terrain wheat can be planted on |
| Hills / rock | Yes (slow) | Sparse; blocks farming. Deliberately *not* a stone resource — see §4.4 |
| Sand | Yes | Low yield, borders water |

Stored as 32×32 chunks — the unit for both render caching and dirty-region persistence.

### 4.4 Resources

| Resource | Form | Acquisition | Consumed for |
|---|---|---|---|
| **Water** | Terrain tile | Drink in place at shallow water | Thirst; wheat irrigation adjacency |
| **Forage** (berries) | Regenerating node | Gather; instant low food | Immediate hunger. Reliable, low ceiling, **spoils fast** |
| **Wood** | Regenerating node (trees) | Chop | **Dual use:** shelter construction/repair *and* fuel burned for warmth |
| **Wheat** | Crop | Plant on soil near water → grows over ~3 days → harvest → grain | Stored food. High yield, high latency, **the only food that keeps** |
| **Sheep** | Mobile entity | Herd into a pen; slaughter | Large food payload; breeds if penned and fed |

Five resources, deliberately. Each one has to earn its place by creating a decision no other resource creates — and the three food sources are not redundant, they're a **risk portfolio**: forage is instant/low/safe, wheat is delayed/high/fixed-in-place, sheep are capital/compounding/mobile.

#### Spoilage is what makes the portfolio real

If all food converted to one fungible "food unit," the optimal strategy would be monoculture on whichever source is most efficient, and the portfolio would collapse into a single number. So food has shelf life:

| Food | Keeps for | Consequence |
|---|---|---|
| Forage | ~2 days | Cannot be stockpiled. Feeds you today and only today |
| Meat | ~4 days | A slaughtered sheep must be eaten or shared before it rots — which quietly creates a *reason to feed the household* |
| **Grain** | Indefinitely | The only food that accumulates |

This one mechanic does most of the work in the design:

- **Reproduction requires agriculture.** §4.8 gates reproduction on a household store above a reserve threshold. Since only grain accumulates, a purely foraging household can never reach that threshold — it can survive, but it cannot breed. Foraging is a life; farming is a lineage.
- **It makes S4 mechanically inevitable** rather than merely hoped for. Disabling wheat doesn't just make food scarcer; it severs the path to reproduction, and lineages *must* collapse.
- **Meat's middle shelf life creates sharing pressure** with no rule about sharing. A creature that slaughters a sheep has four days of food it cannot possibly eat alone. Generosity becomes the rational move, discovered rather than imposed.

#### Wood is fuel as well as timber

Wood used only for building has a demand cliff: once the shelter stands, "go chop wood" stops being a live decision for the rest of a creature's life. Making it burnable keeps it in continuous demand and, more importantly, **makes warmth portable.**

Warmth would otherwise be purely positional — be home by dark — which hard-caps exploration at "how far can I get and still return before nightfall." That cap would throttle map discovery, and with it the entire knowledge and culture layer (§4.11). Carried firewood lifts it: a creature can camp out, which turns the explorer's inventory into a genuine tradeoff of **fuel weight against food weight**, and puts distant discoveries within reach of a lineage willing to fund the trip.

**The strategic gradient:** forage is safe and insufficient; wheat and sheep are what a lineage needs to survive past generation 2, but both require planning across more ticks than a hungry creature naturally tolerates. That tension is precisely the decision the LLM exists to make.

### 4.5 Creature needs and health

Four needs, each 0–100, each with a per-tick decay and a deficit threshold:

- **Hunger** — decays steadily; below threshold, health erodes. Zero for extended ticks → starvation death.
- **Thirst** — decays faster than hunger; cheap to satisfy near water, lethal away from it.
- **Fatigue** — restored by resting, faster in shelter. High fatigue slows movement and work. **Also the metabolic cost of thinking** (§5.5) — deliberation draws it down, which is what makes planning ahead worth something to the creature itself.
- **Warmth** — drains at night and in bad weather. Restored by being in a shelter, or by a **lit fire**, which burns wood and works anywhere (§4.4). This is what makes both shelter and carried fuel matter.

**Health** is the integrator: it drops while any need sits in deficit and regenerates slowly when all needs are satisfied. Health is *not* directly visible to the creature's prompt as a number — the prompt describes felt state ("you are weak and very hungry") so the model reasons in-character rather than optimizing a stat.

### 4.6 Lifespan

Baseline 672 ticks, adjusted continuously rather than at death:

- **Sustained malnutrition** — accelerates aging (up to ~2× rate).
- **Nights without shelter or fire** — each one permanently shaves expected lifespan. A fire is the cheaper, more fragile substitute: it costs wood every night and goes out if the wood runs out.
- **Consistently well-fed and sheltered** — extends toward a ~840-tick (5 week) ceiling.
- **Injury / illness** — discrete events (v1: rare accidents, sheep-related injury, and childbirth risk).

Death causes, always recorded: `STARVATION`, `DEHYDRATION`, `EXPOSURE`, `OLD_AGE`, `ILLNESS`, `ACCIDENT`, `CHILDBIRTH`.

### 4.7 Life stages

Compressed to fit the four-week span:

| Stage | Ticks | Capability |
|---|---|---|
| **Infant** | 0–168 (week 1) | Cannot gather or work. Must be fed by a parent or household member or it dies. Follows a guardian. |
| **Adult** | 168–588 | Full action set. Can court, reproduce, build, farm, herd. |
| **Elder** | 588–death | Reduced carry and speed. Can still tend crops, feed infants, and hold the household store. |

Deliberation is weighted across these stages rather than distributed flat — see §5.4, which subdivides adulthood further for that purpose.

The infant dependency window is the core difficulty. A creature that reproduces without a food reserve has committed its household to feeding a non-productive mouth for a full week — a quarter of an adult lifetime. **This is deliberately harsh** and is the main dial to tune if early playtests show lineages can't get past generation 2.

### 4.8 Reproduction

Requirements, all of which must hold:

1. Two adult creatures, opposite sex, both above a health floor.
2. **Mutual pairing** — courtship is a two-sided action; both must choose it. Rejection is possible and is recorded.
3. A shared shelter with capacity.
4. A household food store above the reserve threshold (default: 20 food units) — this is the "certain amount of food" gate. **Because only grain keeps indefinitely (§4.4), in practice this means the household must farm or herd.** A foraging household can feed itself but can never accumulate enough to reproduce.

Then: gestation of 48 ticks (2 days), after which an infant is born with inherited traits (§4.9) and a small mutation. Childbirth carries a small mortality risk for the mother, which is what makes reproduction a genuine gamble rather than a free action.

### 4.9 Traits and inheritance

Each creature carries a small trait vector, inherited from parents with mutation. Traits are **injected into the prompt as personality description**, not applied as raw stat modifiers — so they shape decisions rather than outcomes directly:

- `boldness` — willingness to travel far from the household
- `industry` — preference for long-horizon work (farming) over immediate gain (foraging)
- `sociability` — tendency to share, court, and cooperate
- `caution` — weight given to needs before opportunity

This creates the selection pressure that makes lineage survival interesting: if industrious households out-survive bold ones, the trait distribution should drift measurably over generations. **That drift is a headline reporting metric** (§10) and one of the clearest signals that the simulation is doing something real.

### 4.10 Social structures

- **Household** — a shelter plus its members. Has a shared food/wood store. The unit of cooperation.
- **Relationships** — directed edges between creatures with a familiarity/affinity score, updated by shared actions (gave food, was refused, raised together, mated, rejected).
- **Kinship** — derived from `mother_id`/`father_id`; the lineage tree is a query, not a stored structure.
- **Roles** — emergent, not assigned. A creature that keeps choosing to farm is *labeled* a farmer by the reporting layer. Roles are an output of the simulation, never an input to it.

### 4.11 Knowledge, belief, and communication

No creature can see the whole map. Each one carries a private, incomplete, and **possibly wrong** model of the world, built from what it has personally seen and what others have told it.

#### Beliefs

Memory is a set of typed **beliefs** rather than an opaque blob:

```json
{
  "kind": "FORAGE_NODE",
  "at": [312, 88],
  "detail": { "est_quantity": "plentiful" },
  "confidence": 0.55,
  "learned_tick": 340,
  "last_verified_tick": 340,
  "source": { "from": "creature:88", "hops": 2 }
}
```

Belief kinds: `WATER`, `FORAGE_NODE`, `WOOD_NODE`, `SOIL_PATCH`, `SHEEP_FLOCK`, `SHELTER`, `HOUSEHOLD_TERRITORY`, `DANGER`, `PERSON` (who someone is, where they live, what they're like).

Three properties do all the work:

- **Confidence decays with time since last verified.** A forage node reported 100 ticks ago may have been stripped since.
- **Provenance is tracked.** `hops` counts how far the belief has travelled from firsthand observation; each transmission degrades confidence. Secondhand knowledge is genuinely worse than firsthand.
- **Beliefs can be false.** Not through lying (v1) but through staleness — the world moved on. A creature acting on a stale belief walks to an empty clearing.

**This closes the open hole in §5.5.** That section flagged a hard dependency: the horizon mechanic only works if the world punishes over-commitment, and random resource depletion was a thin source of that volatility. Stale secondhand belief is a far better one, and it's *legible* — when a creature commits 20 ticks to a wood node its cousin mentioned 80 ticks ago and finds nothing, that's a story, not a dice roll. Confidence becomes something the creature can actually reason about when choosing a horizon: **commit long on what you saw yourself, stay tentative on hearsay.**

#### How knowledge moves

Three channels, in increasing cost and fidelity:

Note that the reach of firsthand discovery is set by the fuel economy (§4.4): carried firewood is what lets a creature survive a night away from home, so **how far the collective map can grow is a function of how much wood the community can spare for expeditions.** Knowledge has a material cost, paid in advance, by someone who may not live to use what they find.

**1. Observation (passive, free).** Simply being near another creature leaks information: seeing someone carry wheat implies wheat exists; watching where a neighbour walks each morning suggests something is over there. Low confidence, no action required, always on. This gives the world an ambient information hum without any decision-making.

**2. `SHARE_KNOWLEDGE` (one tick, small fatigue).** An adjacent creature receives a selected subset of beliefs. **The interesting part is the selection** — the sharer chooses a topic filter and a recipient, and both are real decisions. Do you tell a stranger where your household's soil patch is? Telling them costs you nothing directly, but a rival household that learns to farm is a rival household that survives.

**3. `TEACH` (multi-tick, higher fatigue, household-only).** A bulk, high-fidelity transfer from an adult to a young creature in the same household — transmission at `hops: 0`, as though firsthand. Expensive in exactly the way that matters: an adult spending 6 ticks teaching is 6 ticks not gathering.

**Crucially, none of this costs an LLM call.** Sharing is a plan step (§5.5) chosen inside a deliberation the creature was going to make anyway — `{"goal": "SHARE_KNOWLEDGE", "target": "creature:412", "topic": "WATER"}`. Beliefs are structured data, so the transfer itself is a set merge. Communication is nearly free in the budget that matters.

#### Why this matters more here than in most simulations

A creature lives 672 ticks. **Everything it learns dies with it unless it is transmitted.** That single fact turns knowledge transmission from a nice feature into the central mechanism of the entire game:

> Does this community accumulate knowledge across generations, or does every generation rediscover water from scratch?

That is *culture*, it emerges rather than being scripted, and it is directly measurable (§10). It also serves the stated goal better than anything else in the design — the hypothesis that **lineages which teach out-survive lineages which don't** is precisely the kind of claim this simulation exists to test, and it's a cleaner S6 validation than most: teaching only pays off across a timescale longer than any individual's life, so a creature that chooses to teach is doing something the deterministic utility policy would never invent.

#### Trust gates sharing

Willingness to share should track affinity (§4.10) and the `sociability` trait. Kin and household members get generous sharing; strangers get little. This isn't enforced as a rule — it's expressed in the prompt as the creature's disposition, leaving the model free to be unexpectedly generous or unexpectedly guarded. Whether open households out-survive closed ones is then an experimental result rather than a design decision.

---

## 5. The Decision System

This is the heart of the product and the hardest engineering problem in it.

### 5.1 The constraint, stated honestly

The original concept was one LLM call per creature per tick. At 500 creatures that is 500 calls per tick. `qwen3:8b` on a single consumer GPU serves roughly 1–3 short structured completions per second. **That is 3–8 minutes per in-game hour, or roughly three real-time months per creature lifetime.** Not viable.

The hybrid approach reduces this but does not solve it. If a creature re-deliberates every ~12 ticks on average, 500 creatures produce **~40 pending decisions per tick** — still 15–40 seconds per tick. Better by an order of magnitude, still far from the 2-second target in S2.

So the budget is not an optimization to add later. It is the load-bearing mechanic, and it's designed in from the start.

### 5.2 Three tiers of decision

**Tier 0 — Reflex (every creature, every tick, deterministic, ~microseconds).**
Execute the current plan step: advance along a path, swing the axe, eat what's in hand, flee immediate danger. No thinking, and — importantly — **no metabolic cost beyond the action itself** (§5.5). This is what makes 500 creatures affordable, and what makes a long committed horizon genuinely cheap for the creature.

**Tier 1 — Utility policy (deterministic fallback, ~microseconds).**
A scored decision over the available goals given the creature's needs, inventory, beliefs (§4.11), and traits. Competent and boring: it will feed a starving creature and send an idle one to the nearest forage node. It will not invent farming, prioritize a sibling's infant, or take a risk that pays off in 60 ticks.

Tier 1 exists for two reasons: it guarantees no creature ever stalls waiting for the LLM, and it's the **experimental control** for S6.

**Tier 2 — LLM deliberation (budgeted, ~0.3–1s each).**
The model receives the creature's felt state, personality, local map view, beliefs with their confidence and provenance (§4.11), nearby creatures and relationships, household status, and recent personal events. It returns a structured **plan** — a short sequence of goals plus a committed horizon (§5.5) — that then governs Tier 0/1 behavior until the horizon expires or a hard interrupt fires.

The plan, not the action, is the LLM's output. This is the key move: **one call buys many ticks of coherent behavior.**

### 5.3 Deliberation pressure and the budget

Each tick, every creature gets a **deliberation pressure** score:

| Component | Raises pressure when |
|---|---|
| **Intent completion** | The current goal is achieved, impossible, or invalidated |
| **Urgency** | A need has crossed into deficit; health is falling |
| **Novelty** | The local situation differs meaningfully from the one that produced the current plan |
| **Social significance** | A courtship offer, a birth, a death in the household, a refusal |
| **Staleness** | Many ticks since this creature last deliberated |
| **Narrative weight** | The creature is a lineage founder, an elder, or currently under player inspection |

The summed score is then scaled by an **age weight** (§5.4), which is what concentrates thinking on creatures whose decisions matter most.

The top **N** creatures by pressure get LLM calls, where N is the per-tick budget. Everyone else falls to Tier 1. Pressure carries over and compounds, so a creature that loses the budget race repeatedly will eventually win it — no one is starved of deliberation indefinitely.

This makes the quality/speed tradeoff a single tunable number, and it means the simulation **degrades gracefully rather than stalling** under any population or hardware.

### 5.4 Deliberation across the lifespan

Deliberation is weighted by age, peaking in early adulthood and tapering at both ends. This is both a fidelity choice and the single largest efficiency win available in the budget system.

**Why this is more than flavor.** At steady state roughly a quarter of the population is infant, and infants *cannot take meaningful actions* — they follow a guardian and are fed. Every LLM call spent on one is pure waste. Weighting by age converts an accident of the population pyramid into deliberate allocation: thinking goes to the creatures whose choices actually move the simulation.

Two things scale with age, and they're independent knobs:

**Frequency** — an age multiplier on deliberation pressure (§5.3), which determines how often a creature wins the budget:

| Stage | Ticks | Weight | Behavior |
|---|---|---|---|
| **Infant** | 0–168 | 0.05 | Effectively never deliberates. Tier 0/1 only; the guardian's decisions govern it |
| **Emerging adult** | 168–220 | 0.4 → 1.0 (ramp) | Rising fast — this is when it leaves home, picks a livelihood, and courts |
| **Prime** | 220–380 | **1.0** (peak) | Full deliberation. The productive core of the community |
| **Mature** | 380–588 | 1.0 → 0.6 (decay) | Established habits; re-thinks less often |
| **Elder** | 588+ | 0.35 | Falls back on habit (see below) |

**Depth** — the reasoning-token budget per call. `qwen3:8b` is a reasoning model, so this maps naturally onto thinking budget: prime adults get full reasoning, emerging and mature adults get a reduced budget, elders get minimal. A shallow call is genuinely cheaper in wall-clock time, so this compounds with the frequency saving rather than just duplicating it.

**Peak at early adulthood, not mid-life.** Your instinct is right and it's supported by the mechanics: the highest-stakes, least-reversible decisions a creature makes — leave the household or stay, farm or forage, court whom — all cluster in the first third of adult life. That's exactly where deliberation buys the most. A mature adult executing a farming strategy it settled on 200 ticks ago needs far less thought per tick than the young adult choosing that strategy in the first place.

**Elders fall back on habit, not stupidity.** Rather than dropping elders to the generic Tier-1 utility policy, their fallback is seeded from *their own historical successful intents* — a cheap prior over what has worked for this creature before. Mechanically it's nearly free (a query over their own decision history). Thematically it's the right model of aging: elders deliberate less because they've already solved most of what they encounter, and they draw on crystallized experience rather than fresh reasoning. It also makes elders genuinely valuable to a household instead of a drain, which matters for whether multi-generational households are worth forming at all.

**Urgency partially bypasses the age weight.** The multiplier applies to the *discretionary* pressure components — novelty, staleness, social significance — but only weakly to urgency. Otherwise a starving elder would rationally deliberate its way to death, which is both bad simulation and bad drama. An old creature in crisis still gets to think.

**Expected effect on the budget.** Assuming a uniform age distribution (a rough approximation — real distributions skew young), the weights above cut aggregate deliberation demand by **roughly 40%**, and shift prime adults' share of the budget from ~31% to ~50% of all calls. That is a larger win than any prompt optimization on the table, and it comes with better simulation fidelity rather than at its expense.

There's a second-order effect worth watching: this couples compute allocation to demographics. A baby boom concentrates the budget onto a shrinking set of prime adults, making each of them think harder exactly when the community is under the most strain. An aging population spends less total. Neither was designed in — both fall out of the weighting — and both should show up in the reporting (§10).

### 5.5 Plans, horizons, and the cost of thinking

A deliberation does not produce a single action. It produces a **plan**: a short sequence of steps plus a **committed horizon** — the number of ticks the creature binds itself to executing it before thinking again.

```json
{
  "steps": [
    { "goal": "MOVE_TO", "target": [312, 88], "est_ticks": 8 },
    { "goal": "CHOP_WOOD", "target": "node:4471", "est_ticks": 6 },
    { "goal": "MOVE_TO", "target": "home", "est_ticks": 8 }
  ],
  "horizon": 22,
  "abort_if": ["HUNGER_CRITICAL", "TARGET_DEPLETED", "THREAT"],
  "rationale": "The near forage is picked over. Wood first, then home before dark."
}
```

Step 1 is validated hard at issue time; later steps are re-validated when reached, and a failed re-validation aborts the plan rather than silently no-oping. Horizons are capped per goal type — travel and exploration can commit 24 ticks, gathering ~12, construction ~16, anything social 1–4 (you cannot commit to a courtship 20 ticks in advance), crisis responses 1.

**Why multi-step and not just one goal with a duration:** the expensive thing is the call, not the tokens. A three-step plan costs the same call as a one-step plan and buys roughly three times the coherent behavior. This is the highest-leverage change available to the budget math.

#### Thinking costs the creature

Deliberation draws down **fatigue**, plus a smaller amount of **hunger**. Cost scales with depth — the reasoning-token budget from §5.4 — so a deep deliberation is expensive in-world exactly as it is expensive in wall-clock.

| | Fatigue | Hunger |
|---|---|---|
| Shallow deliberation | ~2 | ~0.5 |
| Standard | ~4 | ~1.0 |
| Deep | ~6 | ~1.5 |

The cost is **flat per deliberation, not per tick planned**. That is the whole incentive: a creature that commits 20 ticks pays once and amortizes; a creature that re-thinks every tick pays twenty times and exhausts itself. Planning ahead is rewarded without any rule that says "plan ahead."

**This is the best structural idea in the design, because it makes the engine's constraint diegetic.** Until now the LLM budget (§5.3) was an external scheduling hack — the engine deciding who gets to think, for reasons that exist nowhere in the fiction. With a metabolic cost, the creature has its *own* reason to think less, and it happens to be the same reason the engine has. The two mechanisms compose rather than fight: creature-side demand for deliberation falls, so the engine-side cap binds less often, so the creatures that genuinely need to think are more likely to be served. A scarce resource became a believable one.

It is also simply true. The brain is roughly a fifth of resting metabolism. Thinking is expensive for real creatures too.

#### The crisis exemption

A starving creature can least afford to think, precisely when it most needs to. Left alone this is a death spiral: hungry → can't afford deliberation → poor choices → hungrier.

That spiral is realistic (decision quality genuinely degrades under deprivation) and dramatically good, but it must not be absorbing. So: **when any need crosses its critical threshold, the creature gets one heavily discounted shallow deliberation.** Panic overrides economy. This is the same principle as urgency bypassing the age weight in §5.4, and the two should share an implementation.

**Elders pay less.** An elder drawing on its habit prior (§5.4) isn't reasoning from scratch, so its deliberations are discounted. Without this, elders would be hit twice — down-weighted by the scheduler *and* too tired to think — and would decay into uselessness. Experience should read as efficiency, not just as diminished capacity.

#### What stops everyone from always planning maximum horizon

If a flat cost is the only pressure, the dominant strategy is trivially "always commit the maximum." The counter-pressure has to be **the risk of being wrong**: a 20-tick plan in a changing world gets aborted partway (the call is wasted) or, worse, executed to completion into a situation that no longer exists — a creature arriving at a forage node another household stripped 15 ticks ago.

This gives the mechanic a hard dependency worth stating plainly: **it only works if the world is volatile enough to punish over-commitment.** Resource depletion, competition between households, weather, and sheep that wander are not just flavor — they are what make horizon choice a genuine decision instead of a solved one.

The largest source of that volatility is **fallible knowledge** (§4.11). A creature's beliefs carry confidence and provenance, so the horizon decision has a natural anchor: commit long on what you verified yourself, stay short on what someone told you eighty ticks ago. That converts horizon choice from a guess about world dynamics into a judgment about the reliability of one's own information — a far better decision to hand a language model, and one it should be genuinely good at. If the world is too static, every creature maxes its horizon, deliberation collapses to near-zero, and the LLM stops being load-bearing. That is an S6 failure arriving through the back door, and **plan-abandonment rate is the metric that catches it** (§10).

#### Interaction with interrupts

§5.3's interrupt detection must now respect commitment:

- **Soft signals** — novelty, staleness, mild need decay — cannot break a committed plan. They accumulate as pressure that applies the moment the horizon expires.
- **Hard signals** — the plan became impossible, a need went critical, immediate danger, a death or birth in the household, a courtship offer — abort the plan immediately.

Every early abort is recorded with its cause. A population that routinely commits to 20 ticks and aborts at 4 is telling you the model is bad at estimating horizons, which is a prompt problem with a clear fix: show it the abandonment history.

#### Traits pick horizon strategy

`caution` and `industry` (§4.9) both plausibly drive horizon length — a cautious creature re-checks often and pays more; an industrious one commits. Since traits are heritable and horizon strategy affects survival, **selection should act on planning style directly.**

That makes one of the more interesting experiments this simulation can run: *does evolution favor planners or reactors, and does the answer change with world volatility?* Horizon length by generation belongs in the trait-drift report.

### 5.6 Simulation speed modes

| Mode | Budget/tick | Target tick time | Use |
|---|---|---|---|
| **Deep** | Unbounded | 15–60s | Studying a small population closely |
| **Observe** | 4–8 | ~1–2s | The default watching experience |
| **Fast-Forward** | 0 (Tier 1 only) | <50ms | Skipping ahead days or weeks |
| **Focus** | 1–2, spent on a chosen lineage | ~0.5s | Following one family closely while the rest of the world runs cheap |

**Focus mode is the one to get right.** It matches how the product is actually used — you care about one bloodline at a time — and it concentrates the entire LLM budget where the player is looking, which is the best possible use of a scarce resource.

### 5.7 Prompt construction

The model never sees 262,144 tiles. It sees:

1. **Identity and felt state** — name, age in weeks, life stage, personality from traits, needs in qualitative language.
2. **Local view** — a 15×15 tile window around the creature, rendered as a compact legend-keyed grid.
3. **Beliefs** (§4.11) — a short list from memory, **rendered with confidence and provenance in plain language**: "water at NW, ~20 steps — you drank there yourself, recently"; "wood in the eastern forest — your cousin Mira mentioned it a while back, she may be out of date." This phrasing matters more than it looks: it's what lets the model reason about how far to commit (§5.5) rather than treating all knowledge as equally solid.
4. **Nearby creatures** — names, relationships, apparent state.
5. **Household** — members, stores, shelter condition, infants needing care.
6. **Recent personal events** — the last few significant things that happened to this creature (distinct from beliefs, which are about the world rather than the self).
7. **The action menu** — the exact set of currently-legal goals, with preconditions already checked.

That last point matters: **the model chooses among options the engine has pre-validated as legal.** It cannot hallucinate an impossible action, because impossible actions are never offered. This eliminates an entire class of failure and shrinks the prompt considerably.

### 5.8 Response handling

Response is JSON, validated against a strict schema (goal enum, optional target, short rationale). On failure: one retry with a repair instruction, then fall through to Tier 1 and record `fallback_used = true` with the reason.

**Fallback rate is a monitored production metric, not a debug detail.** A rising fallback rate means the LLM is quietly stopping being load-bearing — exactly the S6 failure — and it should be visible on the reporting dashboard at all times.

Every call records: prompt hash, full prompt text, raw response, parsed plan, committed horizon, metabolic cost, latency, model tag, and fallback status. Storage is cheap; being unable to explain why generation 4 starved is not.

---

## 6. Actions

The complete v1 action set. Each has explicit preconditions (checked by the engine before being offered) and a tick cost.

**Survival:** `MOVE_TO`, `DRINK`, `EAT_FROM_INVENTORY`, `EAT_FROM_STORE`, `REST`, `SHELTER`, `BUILD_FIRE`, `FEED_FIRE`
**Resource:** `GATHER_FORAGE`, `CHOP_WOOD`, `PLANT_WHEAT`, `TEND_CROP`, `HARVEST_WHEAT`, `HERD_SHEEP`, `SLAUGHTER_SHEEP`
**Construction:** `BUILD_SHELTER`, `REPAIR_SHELTER`, `BUILD_PEN`
**Social:** `COURT`, `ACCEPT_COURTSHIP`, `REJECT_COURTSHIP`, `GIVE_FOOD`, `REQUEST_FOOD`, `FEED_INFANT`, `DEPOSIT_TO_STORE`, `WITHDRAW_FROM_STORE`, `FOLLOW`, `JOIN_HOUSEHOLD`, `LEAVE_HOUSEHOLD`
**Knowledge:** `EXPLORE` (directed wander that writes firsthand beliefs into memory), `SHARE_KNOWLEDGE` (topic-filtered transfer to an adjacent creature), `TEACH` (multi-tick bulk transfer to a young household member), `VERIFY` (revisit a low-confidence belief to refresh it firsthand)

---

## 7. Data Model

SQLite, WAL mode. Abbreviated — full DDL lives in `src-tauri/src/db/schema.sql`.

```sql
worlds          id, name, seed, config_json, created_at, current_tick, status
chunks          world_id, cx, cy, terrain_blob, PRIMARY KEY (world_id, cx, cy)
resource_nodes  id, world_id, kind, x, y, quantity, max_quantity, regen_rate, state

creatures       id, world_id, name, sex, generation,
                mother_id, father_id, household_id,
                birth_tick, death_tick, death_cause,
                x, y, life_stage,
                hunger, thirst, fatigue, warmth, health,
                lifespan_modifier,
                traits_json, inventory_json,
                current_plan_json, plan_set_tick,
                plan_horizon, plan_ticks_remaining, plan_step_index,
                last_deliberation_tick, deliberation_pressure,
                lifetime_deliberations, lifetime_think_fatigue,
                habit_prior_json

beliefs         id, world_id, creature_id, kind, x, y, detail_json,
                confidence, learned_tick, last_verified_tick,
                source_creature_id, hops, origin_creature_id, origin_tick
transmissions   id, world_id, tick, from_creature, to_creature,
                channel, belief_count, kinds_json

households      id, world_id, shelter_id, founded_tick, dissolved_tick, store_json
structures      id, world_id, kind, x, y, condition, capacity, household_id, built_tick,
                fuel_remaining, lit_until_tick
relationships   world_id, from_creature, to_creature, affinity, kind, updated_tick

events          id, world_id, tick, kind, actor_id, target_id, x, y, payload_json
decisions       id, world_id, tick, creature_id, tier,
                creature_age_ticks, life_stage, age_weight, think_budget,
                prompt_hash, prompt_text, raw_response,
                parsed_plan_json, horizon_committed, horizon_actual,
                abort_reason, fatigue_cost, hunger_cost, crisis_exempt,
                latency_ms, model,
                fallback_used, fallback_reason
tick_stats      world_id, tick, population, births, deaths,
                llm_calls, fallbacks, mean_latency_ms, phase_timings_json
```

**Notes on the schema shape:**

- **No per-creature-per-tick snapshot table.** 500 creatures × 672 ticks × many generations is hundreds of millions of rows for data nobody queries. State history is reconstructed from `events` plus periodic sampled snapshots (configurable, default every 24 ticks).
- **`events` is the spine of reporting.** Nearly every report in §10 is a query over it. It should be indexed on `(world_id, tick)`, `(world_id, actor_id)`, and `(world_id, kind)`.
- **Inventories and stores track food as batches, not totals.** `inventory_json` and `store_json` hold `{kind, quantity, harvested_tick}` entries so spoilage (§4.4) can expire the oldest first. A single food integer would make shelf life unrepresentable, which would collapse the whole resource portfolio back into one fungible number.
- **`beliefs` lives in RAM during a run.** The sim holds it in memory (§3.1) and flushes dirty rows periodically; the table is the persistence and reporting layer, never the per-tick read path. At ~30 beliefs × 500 creatures it's a small working set.
- **`origin_creature_id` / `origin_tick` are what make the culture reports possible.** They survive every retransmission, so a belief can always be traced to the creature that first saw the thing — even long after that creature is dead. This is the column that answers S7, and it costs nothing to carry.
- **`transmissions` is deliberately coarse** — counts and kinds, not one row per belief. Per-belief rows would be the largest table in the database by a wide margin and would answer questions nobody is asking; provenance already lives on the belief itself.
- **`horizon_committed` vs `horizon_actual`** is the key pair in the whole table. Committed is written when the plan is issued; actual is backfilled when it ends, along with `abort_reason`. The gap between them is plan-abandonment, which §5.5 identifies as the early-warning metric for the mechanic failing. Backfilling means a plan row is written twice — accept it; the alternative is deriving abandonment from the event log at query time, which is far more expensive.
- **`decisions` stores the age context of every call** — age, stage, the age weight applied, and the thinking budget granted. Without these, the §10 question "did creatures who got more thinking in early adulthood found deeper lineages?" is unanswerable after the fact.
- **`habit_prior_json` is the elder fallback** (§5.4) — a compact summary of a creature's historically successful intents, recomputed occasionally rather than every tick. Denormalized from `decisions` on purpose: it's read every tick by elders and must not require a join.
- **`decisions` retains full prompt text.** Verbose by design — it's what makes prompt iteration and post-hoc explanation possible. Add a retention setting (e.g. keep full text for the last N ticks, hash-only before that) if it becomes a problem.
- **Lineage is derived, never stored.** A recursive CTE over `mother_id`/`father_id` gives ancestry, descendants, and lineage depth. Storing it would only create a second source of truth to keep in sync.

---

## 8. World Generation

Seeded and fully reproducible. Given a seed, terrain and initial resource placement are identical every time — which is what makes the S4 comparison ("same world, wheat disabled") a valid experiment.

1. Elevation and moisture via layered simplex noise → biome classification.
2. Water bodies from the elevation floor; ensure a minimum count of distinct fresh sources with reasonable spatial spread.
3. Forest clusters seeded by moisture; wood and forage nodes placed within them.
4. Soil regions placed adjacent to water (farmable land is intentionally scarce and contested).
5. Sheep flocks spawned on grassland.
6. Founder placement: N adult creatures (default 8, mixed sex) near a viable start — water access, forage in range, soil within reach.

A **viability check** runs post-generation: if founders can't reach water and food within a survivable number of ticks, reject the seed and regenerate. Nothing wastes more playtest time than discovering at tick 200 that the world was unwinnable from tick 0.

LLM decisions are *not* deterministic, so a seed reproduces the world, not the history. This is why `decisions` stores everything — replay-from-log is the mechanism for reproducing a specific run.

---

## 9. User Interface

### 9.1 Map view

Canvas2D with per-chunk offscreen caching; only dirty chunks re-render. Viewport culling means cost scales with what's on screen, not world size.

- Pan (drag / WASD / edge scroll), zoom across ~4 levels.
- Terrain, resource nodes with depletion state, structures, sheep, creatures.
- Creature rendering: color by household, marker by life stage, small state indicators (hungry, pregnant, carrying).
- Selection → creature inspector.
- **Overlays** (toggleable): food density, household territory, lineage highlight, **committed-plan paths** (showing where each creature has bound itself to go, and how many ticks remain), a **deliberation heatmap** showing who the LLM is actually spending attention on. and a **knowledge overlay** (§4.11). The deliberation heatmap is the debugging tool for §5.3 and is worth building early.

**The knowledge overlay is the most interesting view in the product.** It renders the map as *known* rather than as it is, in two modes: what a single selected creature believes (with stale beliefs drawn faded and wrong ones visibly misplaced), and what the community collectively knows. Watching the collective map expand across generations — then contract when a well-travelled elder dies before teaching anyone — is the clearest possible picture of culture forming and being lost.

### 9.2 Creature inspector

Everything about one creature: needs, traits, inventory, family, relationships, home. Current plan — its steps, ticks remaining on the committed horizon, **and the rationale the model gave for it**. A scrollable life-story timeline built from its events. Full decision history with prompts, showing which plans ran to completion and which were abandoned, and why.

Surfacing the model's own stated reasoning in the UI is what turns a simulation into something you can actually read.

### 9.3 World controls

Play/pause, speed mode (§5.6), tick counter and in-game clock, population readout, LLM budget slider with live tick-time feedback, save/load, new world from seed.

### 9.4 Reporting view

See §10. A separate full-window view, not a side panel.

---

## 10. Reporting

Backed by SQL over `events`, `creatures`, and `tick_stats`.

**Population & survival**
- Population over time, with births and deaths overlaid
- Cause-of-death breakdown by generation — *the* diagnostic for whether the difficulty curve is working
- Age-at-death distribution vs. the 672-tick baseline
- Infant mortality rate (the sharpest signal of whether households are coping)

**Lineage** *(the headline — this is the stated goal of the game)*
- Leaderboard: deepest lineages by generation count and total descendants
- Interactive lineage tree, navigable to any creature
- Founder outcomes: which of the original 8 still have living descendants
- Lineage survival curves — how deep does a bloodline typically get before extinction

**Economy**
- Resource stock over time by type; consumption vs. production
- **Farming adoption rate over generations** — with spoilage in play (§4.4) this is effectively a *reproduction* forecast, since only grain reaches the household reserve
- **Food spoiled vs. food eaten**, by source. High forage waste means creatures are over-gathering perishables instead of investing in crops
- **Wood budget split** — timber vs. fuel vs. fuel carried on expeditions. The last of these is the community's actual spend on exploration, and should correlate with map coverage in the culture reports
- Household wealth distribution

**Knowledge & culture** *(§4.11 — the most novel reports in the product)*
- **Collective known-map coverage over time** — what fraction of the world the community collectively knows, generation by generation. Expect a ragged expansion that stalls or collapses when a knowledgeable lineage dies out
- **Belief survival past the discoverer's death** — the S7 metric. Does gen 5 still know what gen 1 found?
- **Knowledge half-life** — how long a belief stays in circulation before everyone holding it dies without teaching it
- Teaching rate by household, cross-referenced against household lineage depth. **This is the direct test of "do lineages that teach out-survive those that don't"**
- Belief accuracy: share of acted-on beliefs that turned out stale, by hop count
- Transmission graph — who informs whom, revealing whether information hubs emerge

**Behavior & traits**
- Action frequency distribution by generation
- **Trait drift across generations** (§4.9) — the clearest evidence of selection actually operating
- Emergent role classification: what fraction of creatures behave as farmers, foragers, shepherds, caretakers

**LLM performance**
- Calls per tick, latency distribution (p50/p95), tokens/sec
- **Fallback rate over time** (§5.8) — treat a rise as a defect
- Deliberation pressure distribution — who is getting attention and who is being starved of it
- **Compute spent per life stage** — confirms the §5.4 weighting is actually landing where intended
- **Lifetime deliberation count vs. lineage depth** — does a creature that got more thinking in early adulthood found a deeper bloodline? If yes, that is direct evidence for S6
- **Elder habit-prior hit rate** — how often the elder fallback produces a sensible plan without a call

**Planning** *(§5.5)*
- **Committed vs. actual horizon** — the abandonment gap. A widening gap means the model is over-committing and the prompt needs the abandonment history fed back into it
- Abort-reason breakdown — distinguishes "the world changed" from "the plan was bad"
- Horizon length distribution by generation, and **horizon length vs. lineage depth** — the direct test of whether planners out-survive reactors
- Fatigue spent on thinking as a share of total fatigue, by life stage — confirms the metabolic cost is biting without being crippling
- Crisis-exemption invocation rate — a high rate means creatures are routinely thinking their way into starvation and the base cost is too high
- Action distribution: LLM-chosen vs. Tier-1 fallback. **If these two distributions converge, S6 is failing.** This chart is the single best early warning that the LLM has stopped mattering.

All reports exportable to CSV.

---

## 11. Configuration

A single editable config per world (stored as `worlds.config_json`), covering: map size and seed, resource density and regen rates, **per-food spoilage rates and fire fuel-burn rate** (§4.4), need decay rates, lifespan baseline and modifiers, reproduction thresholds, infant dependency duration, LLM model tag / budget / temperature / timeout, the **age-weight curve and per-stage thinking budgets** (§5.4), **deliberation fatigue/hunger costs, per-goal horizon caps, and the crisis-exemption threshold** (§5.5), **belief confidence-decay rates, per-hop confidence penalty, observation radius, and teaching cost/fidelity** (§4.11), and feature toggles (wheat on/off, sheep on/off, **spoilage on/off, fires on/off**, LLM on/off, age-weighting on/off, elder habit-prior on/off, thinking-cost on/off, multi-step plans on/off, knowledge-sharing on/off, teaching on/off).

The toggles exist specifically to support the S4 and S6 experiments. Being able to run the same seed with one mechanic disabled is how you find out whether that mechanic does anything.

---

## 12. Milestones

**M0 — Scaffold.** Tauri app, SQLite with migrations, empty render loop, config load. *Done when:* the app opens, creates a world row, and closes cleanly.

**M1 — World.** Worldgen, chunk storage, map renderer with pan/zoom, resource nodes. *Done when:* a seeded 512×512 world renders and scrolls at 30+ FPS.

**M2 — Deterministic life.** Creatures, needs, Tier 0/1 decision system, actions, pathfinding, death, **spoilage and the fuel economy** (§4.4), and the **belief substrate** (§4.11) — exploration, confidence decay, and the knowledge overlay. Transmission comes later, but the deterministic policy needs beliefs to navigate by, so the substrate belongs here. **No LLM at all.** *Done when:* 500 creatures survive, forage, and die of plausible causes; Fast-Forward hits <50ms/tick.

M2 is the most important milestone. **If the simulation isn't interesting to watch with the deterministic policy alone, adding an LLM will not save it** — it will just make a boring simulation slow. Everything after this point is enhancement of a working system.

**M3 — Deliberation.** Ollama client pool, prompt assembly, plan schema with committed horizons (§5.5), thinking cost, budget scheduler, decision logging. *Done when:* creatures visibly act on model-chosen multi-tick plans, every call is in the DB, and plan-abandonment rate is on a chart. Prototype both horizon-estimation approaches from §13.9 here.

**M4 — Society.** Households, shelter, courtship, reproduction, infants, food sharing, inheritance. Knowledge **transmission** — observation, `SHARE_KNOWLEDGE`, `TEACH` — lands here alongside them, since teaching is fundamentally a household act. Age-weighted deliberation and the elder habit prior (§5.4) also land here, since they only become measurable once there are real life stages to weight across. *Done when:* a lineage reaches generation 3 unaided, beliefs demonstrably survive their discoverer's death (S7), and the age curve is instrumented against founder outcomes (§13.7).

**M5 — Reporting.** All of §10, plus CSV export. *Done when:* S5 holds — any creature's full life is reconstructable.

**M6 — Tuning.** Balance passes against S3/S4/S6, prompt iteration, Focus mode, overlays, performance. *Done when:* all success criteria pass on three different seeds.

---

## 13. Open Questions and v2 Candidates

**Open questions for v1:**

1. **Is a 4-week lifespan with a 1-week infancy survivable at all?** A quarter of adult life spent as a dependent is brutal, and spoilage (§4.4) sharpens it further by making the reproduction reserve reachable only through farming. If M4 shows lineages consistently dying at generation 2, the dials in order are: infant duration, the reserve threshold, then grain yield per harvest.
2. **Is the wheat gate too absolute?** Spoilage means a foraging household literally cannot reproduce — clean, legible, and possibly too binary. If M4 runs show every lineage either farming or extinct with nothing in between, soften it by giving meat a longer shelf life, so herding becomes a genuine second path rather than a stopgap.
3. **How much does model choice matter?** `qwen3:8b` is the target, but the budget math changes substantially with a smaller model. Worth benchmarking a 3B against it at M3 — a faster model that deliberates 3× more often may beat a smarter one that rarely gets the budget.
4. **How are beliefs selected for the prompt?** A well-travelled creature accumulates far more beliefs than fit in a prompt. Naive truncation loses the wheat field; naive retention blows up the context. Needs a relevance ranking over confidence, distance, recency, and current need — built at M2 with the substrate, tuned at M3 when real prompts exist.
5. **Will creatures actually choose to teach?** Teaching costs ticks now and pays off only after the teacher is dead. That is a genuinely hard ask of an agent optimizing its own survival, and it may simply not happen — in which case knowledge dies with every generation and S7 fails. Mitigations in rough order of preference: make the prompt surface what the creature owes its lineage; give teaching a small immediate affinity reward; make it partly automatic between parent and infant. **Resist the last one** — if teaching has to be hardcoded, the simulation isn't discovering culture, it's performing it.
6. **Does the deliberation budget produce visible unfairness?** If Tier-1 creatures behave noticeably worse, the population may split into a "smart" observed class and a "dumb" background class. Watch for this in M6.
7. **Where exactly does the age curve peak?** §5.4 puts it at 220–380 ticks on the argument that early adulthood holds the least-reversible decisions. But if playtests show creatures making poor *first* choices — bad mate, bad livelihood — right at 168–220 while still on the ramp, the peak needs to shift earlier and start steeper. This is worth instrumenting at M4 specifically: compare lineage outcomes against how much deliberation the founder got in its first 50 adult ticks.
8. **Is the world volatile enough to make horizon a real choice?** §5.5 depends on it entirely. If early runs show creatures converging on maximum horizon with low abandonment, the world is too predictable and resource competition needs sharpening. Measure from M3, before the society layer complicates the signal.
9. **Can the model estimate horizons at all?** Asking a creature to predict how long a plan will take is a genuinely hard judgment, and `qwen3:8b` may simply be bad at it. If abandonment stays high regardless of world tuning, the fallback is to let the engine derive the horizon from the plan's estimated step costs and only let the model choose a coarse commitment level (brief / moderate / committed). Worth prototyping both at M3.
10. **Do elders need any deliberation at all?** If the habit prior performs well, elder weight could drop toward zero and free more budget for the prime. If it performs badly, elders will visibly decay into uselessness and the weight needs raising. Either outcome is informative; the metric is the habit-prior hit rate in §10.

**v2 candidates:** seasons and famine cycles; predators; disease spread; tribes and territory; a narrative chronicle generated from the event log; and three that all depend on adding language:

- **Free-text speech**, enabling negotiation and persuasion rather than fixed social actions.
- **Information trade** — "I'll tell you where the water is if you share your grain." Requires an offer/counter-offer exchange that structured actions can't express.
- **Deception.** Once a creature can share a belief, it can share a *false* one — sending a rival household toward an empty clearing. This is probably the single most interesting behavior this simulation could ever produce, and the infrastructure for it is already in v1: beliefs carry provenance, so a lie would be traceable and trust could collapse in response. It is deliberately held back because verifying that a model is lying *strategically* rather than hallucinating is genuinely hard, and getting that wrong would poison the knowledge metrics that S7 depends on.

---

## 14. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| **LLM too slow even hybrid** | High | Budget scheduler (§5.3) is designed for exactly this; age weighting (§5.4) cuts demand ~40% on top; Fast-Forward and Focus modes give usable play at any speed |
| **Age weighting starves young adults at the worst moment** | Medium | Peak is placed at early adulthood precisely to avoid this; the ramp shape is configurable and instrumented at M4 (§13.7) |
| **World too static, so max horizon always wins** | **High** | This is an S6 failure via the back door (§5.5). Resource depletion, household competition, and above all fallible secondhand belief (§4.11) must genuinely punish over-commitment; plan-abandonment rate is the detector, watched from M3 |
| **Creatures never choose to teach, so knowledge dies each generation** | **High** | S7 fails and the culture layer is inert. Prompt framing first, small affinity reward second; hardcoding parent→infant transfer only as a last resort, since it fakes the result (§13.5) |
| **Thinking cost creates an inescapable starvation spiral** | Medium | Crisis exemption grants a discounted shallow deliberation at critical need; exemption rate is a monitored metric and the cost is fully configurable |
| **LLM output unparseable** | Medium | Strict schema, pre-validated action menu, one repair retry, Tier-1 fallback, monitored fallback rate |
| **LLM adds nothing over Tier 1** | **High** | S6 is a first-class criterion measured from M3 onward, not a final check. If it fails, the fix is richer prompts and higher-stakes decisions — not more calls |
| **Difficulty makes lineages non-viable** | Medium | Everything in §11 is tunable; M4's exit criterion is specifically generation 3 |
| **DB growth** | Low | Events + sampled snapshots instead of per-tick state; prompt-text retention policy |
| **Render perf at 500 creatures** | Medium | Chunk caching, viewport culling, WebGL upgrade path held in reserve |

---

## Appendix A — Rejected alternatives

**One LLM call per creature per tick (the original concept).** Rejected on arithmetic (§5.1). Preserved in spirit by Deep mode on a small population, where it is genuinely affordable and worth seeing.

**Batching multiple creatures into one prompt.** Tempting — it amortizes call overhead. Rejected because it entangles creatures' reasoning: the model starts coordinating them as a group mind, which destroys the independent-agent premise that makes emergent cooperation meaningful. Cooperation has to be *achieved* through social actions, not assumed by the prompt structure.

**Caching decisions by situation hash.** Kept as a possible optimization, not a v1 dependency. Creature state is high-dimensional enough that hit rates are likely poor, and identical responses to similar situations would flatten exactly the behavioral variety the project exists to observe.
