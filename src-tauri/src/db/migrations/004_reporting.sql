-- 004 — instrumentation that §10 needs and the event log cannot supply.
--
-- Two reports in §10 are not reconstructable from what M2–M4 recorded, and
-- both were found by trying to write the query rather than by reading the spec:
--
--  * "Collective known-map coverage over time." `collapse_routine_events`
--    folds per-creature DISCOVERED events into one per-tick count with no
--    coordinates, so the union of known tiles is unrecoverable after the fact.
--    Un-collapsing would restore ~150 events/tick, which was itself a measured
--    performance fix. Instead the sim samples the union periodically and writes
--    the size here. NULL means "not sampled on this tick", which is a different
--    statement from zero and the report has to be able to tell them apart.
--
--  * "Belief accuracy: share of acted-on beliefs that turned out stale, by hop
--    count." Aborts record TARGET_GONE, but nothing recorded how far the belief
--    that sent the creature there had travelled — which is the entire point of
--    the question, since §4.11's premise is that secondhand knowledge is worse.
ALTER TABLE tick_stats ADD COLUMN known_tiles INTEGER;
ALTER TABLE decisions  ADD COLUMN belief_hops INTEGER;
