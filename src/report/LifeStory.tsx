import { useEffect, useMemo, useState } from "react";
import * as api from "./api";
import { Figure, LineChart, Stat, fmt } from "./charts";
import type { Life, Roster } from "./types";

/**
 * One creature's whole life, read back out of the database (§10, criterion S5).
 *
 * S5 says any creature's full life must be reconstructable from the DB, and
 * this view is what that criterion means in practice: birth, parents, children,
 * every event, every decision with its full prompt and the model's own words,
 * and the sampled needs curve underneath it all.
 *
 * The needs chart is drawn from `creature_samples`, which is written every 12–24
 * ticks rather than every tick (§7). That is a deliberate trade and the caption
 * says so, because a needs curve that looks smooth is making a promise about
 * resolution that the record does not keep.
 */
export function LifeStory({ initial }: { initial: number | null }) {
  const [roster, setRoster] = useState<Roster[]>([]);
  const [id, setId] = useState<number | null>(initial);
  const [life, setLife] = useState<Life | null>(null);
  const [filter, setFilter] = useState("");
  const [openPrompt, setOpenPrompt] = useState<number | null>(null);
  const [err, setErr] = useState<string | null>(null);

  useEffect(() => {
    api.roster(500).then(setRoster).catch((e) => setErr(String(e)));
  }, []);
  useEffect(() => {
    setId(initial);
  }, [initial]);
  useEffect(() => {
    if (id == null) return;
    api.life(id).then(setLife).catch((e) => setErr(String(e)));
  }, [id]);

  const shown = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return roster.slice(0, 200);
    return roster
      .filter(
        (r) =>
          r.name.toLowerCase().includes(q) ||
          String(r.id) === q ||
          (r.death_cause ?? "").toLowerCase().includes(q),
      )
      .slice(0, 200);
  }, [roster, filter]);

  if (err) return <p className="fig-empty">{err}</p>;

  return (
    <div className="life">
      <aside className="life-roster">
        <input
          placeholder="name, id, or cause of death"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
        />
        <ul>
          {shown.map((r) => (
            <li key={r.id}>
              <button className={id === r.id ? "on" : ""} onClick={() => setId(r.id)}>
                <span className={r.death_tick == null ? "quick" : "still"}>{r.name}</span>
                <span className="life-roster-meta">
                  g{r.generation} · {r.children} children
                </span>
              </button>
            </li>
          ))}
        </ul>
      </aside>

      <div className="life-body">
        {!life ? (
          <p className="fig-empty">Pick somebody.</p>
        ) : (
          <>
            <header className="life-head">
              <h2>{life.name}</h2>
              <p>
                {life.sex.toLowerCase()} · generation {life.generation} · born tick{" "}
                {life.birth_tick}
                {life.death_tick != null
                  ? ` · died tick ${life.death_tick} of ${(life.death_cause ?? "unknown").toLowerCase().replace(/_/g, " ")}`
                  : " · still alive"}
              </p>
              <p className="life-kin">
                {life.mother && (
                  <>
                    mother <b>{life.mother[1]}</b>{" "}
                  </>
                )}
                {life.father && (
                  <>
                    father <b>{life.father[1]}</b>{" "}
                  </>
                )}
                {life.children.length > 0 && (
                  <>
                    · children {life.children.map((c) => c[1]).join(", ")}
                  </>
                )}
              </p>
            </header>

            <div className="stats">
              <Stat
                label="Lived"
                value={
                  life.death_tick != null
                    ? `${life.death_tick - life.birth_tick} ticks`
                    : "—"
                }
                sub={`lifespan modifier ${life.lifespan_modifier.toFixed(2)}×`}
              />
              <Stat label="Decisions" value={fmt(life.decisions.length)} sub="logged in full" />
              <Stat
                label="Beliefs found"
                value={fmt(life.beliefs_found)}
                sub={`${fmt(life.still_circulating)} still circulating`}
              />
              <Stat
                label="Passed on"
                value={fmt(life.taught_count + life.shared_count)}
                sub={`${fmt(life.taught_count)} taught · ${fmt(life.shared_count)} shared`}
              />
            </div>

            <Figure
              title="Needs over a life"
              note="Sampled every few ticks, not continuously — the line between two points is interpolation, not record."
              rows={life.samples}
              series={[
                { key: "hunger", label: "hunger", value: (d) => d.hunger },
                { key: "thirst", label: "thirst", value: (d) => d.thirst },
                { key: "warmth", label: "warmth", value: (d) => d.warmth },
                { key: "fatigue", label: "fatigue", value: (d) => d.fatigue },
                { key: "health", label: "health", value: (d) => d.health },
              ]}
            >
              <LineChart
                rows={life.samples}
                x={(d) => d.tick}
                xLabel="tick"
                height={190}
                series={[
                  { key: "hunger", label: "hunger", value: (d) => d.hunger },
                  { key: "thirst", label: "thirst", value: (d) => d.thirst },
                  { key: "warmth", label: "warmth", value: (d) => d.warmth },
                  { key: "fatigue", label: "fatigue", value: (d) => d.fatigue },
                  { key: "health", label: "health", value: (d) => d.health },
                ]}
              />
            </Figure>

            <section className="life-cols">
              <div>
                <h3>What happened</h3>
                <ol className="log">
                  {life.events.map((e, i) => (
                    <li key={i}>
                      <span className="log-tick">{e.tick}</span>
                      <span className="log-kind">{e.kind.toLowerCase().replace(/_/g, " ")}</span>
                      {e.payload && <span className="log-payload">{e.payload}</span>}
                    </li>
                  ))}
                </ol>
              </div>

              <div>
                <h3>What it decided</h3>
                <ol className="log">
                  {life.decisions.map((d, i) => (
                    <li key={i} className={d.fallback_used ? "fell-back" : ""}>
                      <span className="log-tick">{d.tick}</span>
                      <span className="log-kind">
                        T{d.tier} {d.goal.toLowerCase().replace(/_/g, " ")}
                      </span>
                      {d.rationale && <span className="log-why">“{d.rationale}”</span>}
                      <span className="log-payload">
                        {d.horizon_committed != null && (
                          <>
                            committed {d.horizon_committed}, ran {d.horizon_actual ?? "—"}
                          </>
                        )}
                        {d.abort_reason && ` · ${d.abort_reason.toLowerCase().replace(/_/g, " ")}`}
                        {d.fallback_reason && ` · fell back: ${d.fallback_reason.toLowerCase().replace(/_/g, " ")}`}
                        {d.latency_ms != null && ` · ${d.latency_ms}ms`}
                      </span>
                      {/* Invariant 4: every call is recorded in full, so the
                          exact prompt and the model's raw answer are always
                          one click away rather than summarised. */}
                      {(d.prompt_text || d.raw_response) && (
                        <button className="log-open" onClick={() => setOpenPrompt(openPrompt === i ? null : i)}>
                          {openPrompt === i ? "hide" : "prompt"}
                        </button>
                      )}
                      {openPrompt === i && (
                        <pre className="log-prompt">
                          {d.prompt_text ?? "(prompt not retained)"}
                          {d.raw_response ? `\n\n--- response ---\n${d.raw_response}` : ""}
                        </pre>
                      )}
                    </li>
                  ))}
                </ol>
              </div>
            </section>
          </>
        )}
      </div>
    </div>
  );
}
