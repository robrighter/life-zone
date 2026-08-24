-- The society layer: pairing, pregnancy, guardianship and household membership.
--
-- Brought forward ahead of the LLM. The reason is recorded here because it is a
-- resequencing of the plan, not a normal increment: M2 measured a world of 500
-- individuals independently solving the same problem the same way, because the
-- mechanics that differentiate creatures from one another — households,
-- inheritance, transmission — all sat behind the milestone *after* the model.
-- Adding deliberation to that world would have given it very little to
-- differentiate on, and S6 is measured from M3 onward.
--
-- `mother_id` and `father_id` already exist and stay the only record of descent:
-- lineage is a recursive CTE over them, never a stored tree (invariant 6).
ALTER TABLE creatures ADD COLUMN mate_id INTEGER;
ALTER TABLE creatures ADD COLUMN paired_tick INTEGER;
-- The father and the tick the child is due. Gestation is §4.8's 48 ticks.
ALTER TABLE creatures ADD COLUMN pregnant_by INTEGER;
ALTER TABLE creatures ADD COLUMN due_tick INTEGER;
-- An infant cannot gather or work; it follows a guardian and is fed by one
-- (§4.7). Without a living guardian it starves, which is the dependency window
-- the PRD calls deliberately harsh.
ALTER TABLE creatures ADD COLUMN guardian_id INTEGER;
-- When the last child arrived, so §4.8's spacing between births can be
-- enforced: without it a household commits to three dependents at once the
-- moment its store first crosses the reserve.
ALTER TABLE creatures ADD COLUMN last_birth_tick INTEGER;
ALTER TABLE creatures ADD COLUMN children_born INTEGER NOT NULL DEFAULT 0;
-- Counters for the culture reports (§10). Denormalised on purpose: they are
-- read per creature in the inspector and would otherwise need an aggregate over
-- `transmissions` every time somebody clicked on somebody.
ALTER TABLE creatures ADD COLUMN taught_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE creatures ADD COLUMN shared_count INTEGER NOT NULL DEFAULT 0;

-- Household membership is derived from creatures.household_id, so the household
-- row carries only what it owns. `size_cap` is what limits *membership* rather
-- than tonight's occupancy: M2 gated shelter-building on whether a shelter had a
-- free bed, which is always true during the day — the only time a shelter can
-- be built — so 4,271 creatures built 35 shelters between them and exposure took
-- 23% of all deaths.
ALTER TABLE households ADD COLUMN size_cap INTEGER NOT NULL DEFAULT 6;

-- Kin lookups walk these constantly once lineage depth is a headline metric.
CREATE INDEX IF NOT EXISTS idx_creatures_mother ON creatures(world_id, mother_id);
CREATE INDEX IF NOT EXISTS idx_creatures_father ON creatures(world_id, father_id);
CREATE INDEX IF NOT EXISTS idx_creatures_household ON creatures(world_id, household_id);

-- Who founded a household. Needed to route an inheritance when its last member
-- dies: the store passes to a household of one of their children, and without
-- the founders there is no way to know whose children those are.
ALTER TABLE households ADD COLUMN founder_a INTEGER;
ALTER TABLE households ADD COLUMN founder_b INTEGER;

-- Courtship offers awaiting an answer.
--
-- These look ephemeral and are not: an offer stands for several ticks, and a
-- creature's next decision depends on whether one is outstanding. Dropping them
-- on reload makes a resumed run diverge from an uninterrupted one, which the
-- schema round-trip test in BUILD.md §9 exists to catch — and did.
CREATE TABLE courtship_offers (
    world_id      INTEGER NOT NULL REFERENCES worlds(id) ON DELETE CASCADE,
    from_creature INTEGER NOT NULL,
    to_creature   INTEGER NOT NULL,
    offered_tick  INTEGER NOT NULL,
    PRIMARY KEY (world_id, from_creature, to_creature)
) WITHOUT ROWID;
