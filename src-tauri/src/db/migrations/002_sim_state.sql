-- M2: the remainder of a creature's state, so a resumed run continues exactly
-- where it left off rather than approximately.
--
-- BUILD.md §9 requires that a world saved mid-run, reloaded and continued
-- matches an uninterrupted run tick for tick. That is only true if everything
-- the tick pipeline reads is restored, and these four fields are read every
-- tick by phases 2, 5 and 6.
ALTER TABLE creatures ADD COLUMN wear REAL NOT NULL DEFAULT 0;
ALTER TABLE creatures ADD COLUMN exposed_ticks INTEGER NOT NULL DEFAULT 0;
ALTER TABLE creatures ADD COLUMN in_shelter INTEGER;
-- A recent injury or illness and when it happened: if health runs out soon
-- after, that is the recorded cause rather than whichever need was lowest.
ALTER TABLE creatures ADD COLUMN trauma_cause TEXT;
ALTER TABLE creatures ADD COLUMN trauma_tick INTEGER;

-- `horizon_actual` and `abort_reason` are backfilled onto the decision that
-- issued the plan, keyed on all three columns. Without the tick in the index
-- every backfill scans every decision that creature has ever made, which grows
-- without bound over a run.
CREATE INDEX idx_decisions_backfill ON decisions(world_id, creature_id, tick);

-- `beliefs.creature_id` carries an ON DELETE CASCADE foreign key. The existing
-- index leads with world_id, so SQLite cannot use it to enforce that key and
-- falls back to a full scan of `beliefs` for every creature row deleted. This
-- is the index the foreign key actually needs.
CREATE INDEX idx_beliefs_creature_fk ON beliefs(creature_id);

-- Creature positions change every tick, so this index is rewritten for every
-- creature at every checkpoint — and nothing reads it. The simulation answers
-- "who is near here" from memory, and no report in §10 asks for creatures by
-- coordinate. It cost more to maintain than it ever saved.
DROP INDEX IF EXISTS idx_creatures_world_pos;
