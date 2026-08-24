-- Life Zone initial schema. Mirrors PRD §7.
-- Invariants encoded here:
--   * no per-creature-per-tick snapshot table (§7, invariant 5)
--   * lineage is derived from mother_id/father_id, never stored (invariant 6)
--   * every LLM call is recorded in full (invariant 4)

CREATE TABLE worlds (
    id            INTEGER PRIMARY KEY,
    name          TEXT    NOT NULL,
    seed          INTEGER NOT NULL,
    config_json   TEXT    NOT NULL,
    created_at    TEXT    NOT NULL,
    current_tick  INTEGER NOT NULL DEFAULT 0,
    status        TEXT    NOT NULL DEFAULT 'active'
);

-- Terrain in 32x32 chunks: the unit of both render caching and dirty-region
-- persistence (§4.3).
CREATE TABLE chunks (
    world_id     INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    cx           INTEGER NOT NULL,
    cy           INTEGER NOT NULL,
    terrain_blob BLOB    NOT NULL,
    PRIMARY KEY (world_id, cx, cy)
) WITHOUT ROWID;

CREATE TABLE resource_nodes (
    id           INTEGER PRIMARY KEY,
    world_id     INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    kind         TEXT    NOT NULL,
    x            INTEGER NOT NULL,
    y            INTEGER NOT NULL,
    quantity     REAL    NOT NULL,
    max_quantity REAL    NOT NULL,
    regen_rate   REAL    NOT NULL,
    state        TEXT    NOT NULL DEFAULT 'active'
);
CREATE INDEX idx_nodes_world_pos  ON resource_nodes(world_id, x, y);
CREATE INDEX idx_nodes_world_kind ON resource_nodes(world_id, kind);

CREATE TABLE households (
    id            INTEGER PRIMARY KEY,
    world_id      INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    shelter_id    INTEGER,
    founded_tick  INTEGER NOT NULL,
    dissolved_tick INTEGER,
    -- store_json holds {kind, quantity, harvested_tick} batches, not totals,
    -- so spoilage can expire the oldest first (§4.4, §7).
    store_json    TEXT    NOT NULL DEFAULT '[]'
);
CREATE INDEX idx_households_world ON households(world_id);

CREATE TABLE structures (
    id              INTEGER PRIMARY KEY,
    world_id        INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    kind            TEXT    NOT NULL,
    x               INTEGER NOT NULL,
    y               INTEGER NOT NULL,
    condition       REAL    NOT NULL DEFAULT 1.0,
    capacity        INTEGER NOT NULL DEFAULT 0,
    household_id    INTEGER REFERENCES households(id),
    built_tick      INTEGER NOT NULL,
    fuel_remaining  REAL    NOT NULL DEFAULT 0,
    lit_until_tick  INTEGER
);
CREATE INDEX idx_structures_world_pos ON structures(world_id, x, y);

CREATE TABLE creatures (
    id                     INTEGER PRIMARY KEY,
    world_id               INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    name                   TEXT    NOT NULL,
    sex                    TEXT    NOT NULL,
    generation             INTEGER NOT NULL DEFAULT 1,

    mother_id              INTEGER REFERENCES creatures(id),
    father_id              INTEGER REFERENCES creatures(id),
    household_id           INTEGER REFERENCES households(id),

    birth_tick             INTEGER NOT NULL,
    death_tick             INTEGER,
    death_cause            TEXT,

    x                      INTEGER NOT NULL,
    y                      INTEGER NOT NULL,
    life_stage             TEXT    NOT NULL DEFAULT 'INFANT',

    hunger                 REAL    NOT NULL DEFAULT 100,
    thirst                 REAL    NOT NULL DEFAULT 100,
    fatigue                REAL    NOT NULL DEFAULT 100,
    warmth                 REAL    NOT NULL DEFAULT 100,
    health                 REAL    NOT NULL DEFAULT 100,
    lifespan_modifier      REAL    NOT NULL DEFAULT 1.0,

    traits_json            TEXT    NOT NULL DEFAULT '{}',
    inventory_json         TEXT    NOT NULL DEFAULT '[]',

    current_plan_json      TEXT,
    plan_set_tick          INTEGER,
    plan_horizon           INTEGER,
    plan_ticks_remaining   INTEGER,
    plan_step_index        INTEGER,

    last_deliberation_tick INTEGER,
    deliberation_pressure  REAL    NOT NULL DEFAULT 0,
    lifetime_deliberations INTEGER NOT NULL DEFAULT 0,
    lifetime_think_fatigue REAL    NOT NULL DEFAULT 0,
    -- Denormalised from `decisions` on purpose: read every tick by elders and
    -- must not require a join (§7).
    habit_prior_json       TEXT
);
CREATE INDEX idx_creatures_world_alive ON creatures(world_id, death_tick);
CREATE INDEX idx_creatures_mother      ON creatures(mother_id);
CREATE INDEX idx_creatures_father      ON creatures(father_id);
CREATE INDEX idx_creatures_household   ON creatures(household_id);
CREATE INDEX idx_creatures_world_pos   ON creatures(world_id, x, y);

CREATE TABLE beliefs (
    id                 INTEGER PRIMARY KEY,
    world_id           INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    creature_id        INTEGER NOT NULL REFERENCES creatures(id) ON DELETE CASCADE,
    kind               TEXT    NOT NULL,
    x                  INTEGER,
    y                  INTEGER,
    detail_json        TEXT    NOT NULL DEFAULT '{}',
    confidence         REAL    NOT NULL,
    learned_tick       INTEGER NOT NULL,
    last_verified_tick INTEGER,
    source_creature_id INTEGER,
    hops               INTEGER NOT NULL DEFAULT 0,
    -- origin_* survive every retransmission: this is the pair that answers S7.
    origin_creature_id INTEGER,
    origin_tick        INTEGER
);
CREATE INDEX idx_beliefs_creature ON beliefs(world_id, creature_id);
CREATE INDEX idx_beliefs_origin   ON beliefs(world_id, origin_creature_id);
CREATE INDEX idx_beliefs_kind     ON beliefs(world_id, kind);

-- Deliberately coarse: counts and kinds, not one row per belief (§7).
CREATE TABLE transmissions (
    id            INTEGER PRIMARY KEY,
    world_id      INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    tick          INTEGER NOT NULL,
    from_creature INTEGER NOT NULL,
    to_creature   INTEGER NOT NULL,
    channel       TEXT    NOT NULL,
    belief_count  INTEGER NOT NULL,
    kinds_json    TEXT    NOT NULL DEFAULT '[]'
);
CREATE INDEX idx_transmissions_world_tick ON transmissions(world_id, tick);

CREATE TABLE relationships (
    world_id     INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    from_creature INTEGER NOT NULL,
    to_creature   INTEGER NOT NULL,
    affinity      REAL    NOT NULL DEFAULT 0,
    kind          TEXT,
    updated_tick  INTEGER NOT NULL,
    PRIMARY KEY (world_id, from_creature, to_creature)
) WITHOUT ROWID;

-- The spine of reporting: nearly every report in §10 is a query over this.
CREATE TABLE events (
    id           INTEGER PRIMARY KEY,
    world_id     INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    tick         INTEGER NOT NULL,
    kind         TEXT    NOT NULL,
    actor_id     INTEGER,
    target_id    INTEGER,
    x            INTEGER,
    y            INTEGER,
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX idx_events_world_tick  ON events(world_id, tick);
CREATE INDEX idx_events_world_actor ON events(world_id, actor_id);
CREATE INDEX idx_events_world_kind  ON events(world_id, kind);

CREATE TABLE decisions (
    id                INTEGER PRIMARY KEY,
    world_id          INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    tick              INTEGER NOT NULL,
    creature_id       INTEGER NOT NULL,
    tier              INTEGER NOT NULL,

    -- Age context of every call: without these the §10 question "did creatures
    -- who got more thinking in early adulthood found deeper lineages?" is
    -- unanswerable after the fact.
    creature_age_ticks INTEGER,
    life_stage        TEXT,
    age_weight        REAL,
    think_budget      TEXT,

    prompt_hash       TEXT,
    prompt_text       TEXT,
    raw_response      TEXT,
    parsed_plan_json  TEXT,

    -- The key pair in the table: the gap between them is plan-abandonment,
    -- the early-warning metric for the horizon mechanic failing (§5.5).
    horizon_committed INTEGER,
    horizon_actual    INTEGER,
    abort_reason      TEXT,

    fatigue_cost      REAL,
    hunger_cost       REAL,
    crisis_exempt     INTEGER NOT NULL DEFAULT 0,

    latency_ms        INTEGER,
    model             TEXT,
    fallback_used     INTEGER NOT NULL DEFAULT 0,
    fallback_reason   TEXT
);
CREATE INDEX idx_decisions_world_tick     ON decisions(world_id, tick);
CREATE INDEX idx_decisions_world_creature ON decisions(world_id, creature_id);
CREATE INDEX idx_decisions_fallback       ON decisions(world_id, fallback_used);

CREATE TABLE tick_stats (
    world_id          INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    tick              INTEGER NOT NULL,
    population        INTEGER NOT NULL DEFAULT 0,
    births            INTEGER NOT NULL DEFAULT 0,
    deaths            INTEGER NOT NULL DEFAULT 0,
    llm_calls         INTEGER NOT NULL DEFAULT 0,
    fallbacks         INTEGER NOT NULL DEFAULT 0,
    mean_latency_ms   REAL,
    phase_timings_json TEXT NOT NULL DEFAULT '{}',
    PRIMARY KEY (world_id, tick)
) WITHOUT ROWID;

-- State history is reconstructed from events plus periodic sampling
-- (default every 24 ticks) rather than a per-tick snapshot table.
CREATE TABLE creature_samples (
    world_id    INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    tick        INTEGER NOT NULL,
    creature_id INTEGER NOT NULL,
    x           INTEGER NOT NULL,
    y           INTEGER NOT NULL,
    hunger      REAL NOT NULL,
    thirst      REAL NOT NULL,
    fatigue     REAL NOT NULL,
    warmth      REAL NOT NULL,
    health      REAL NOT NULL,
    life_stage  TEXT NOT NULL,
    PRIMARY KEY (world_id, tick, creature_id)
) WITHOUT ROWID;
