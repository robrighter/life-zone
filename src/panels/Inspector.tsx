import type { CreatureDetail, WorldConfig } from "../ipc";
import { Lifespan } from "./Lifespan";

/// The reproduction reserve, for scaling the store bar. Read from config where
/// it is available; this is only the fallback for the bar's full mark.
const RESERVE_HINT = 20;

/**
 * The creature inspector (PRD §9.2).
 *
 * Everything about one creature: felt state, traits, what it is carrying, its
 * committed plan *and the reason it gave for it*, and what it believes about
 * the world with the provenance of each belief in plain language.
 *
 * Surfacing the reasoning is what turns a simulation into something you can
 * read. At M2 the rationale comes from the Tier 1 policy; at M3 the same field
 * carries the model's own words, which is why it is here now rather than being
 * added when there is a model to quote.
 */
export function Inspector({ d, config }: { d: CreatureDetail; config: WorldConfig }) {
  const l = config.lifespan;
  return (
    <>
      <div className="sec">
        <div style={{ display: "flex", alignItems: "baseline", gap: 8, marginBottom: 2 }}>
          <h2 style={{ fontSize: 20 }}>{d.name}</h2>
          <span className="chip alive"><span className="dot" />{d.felt_state}</span>
        </div>
        <div className="eyebrow" style={{ marginBottom: 10 }}>
          #{d.id} · generation {d.generation} · {d.sex.toLowerCase()} · {d.life_stage.toLowerCase()}
        </div>

        <Lifespan
          age={d.age}
          baseline={l.baseline_ticks}
          infantUntil={l.infant_until_tick}
          elderFrom={l.elder_from_tick}
          expected={d.expected_lifespan}
        />

        <dl className="kv" style={{ marginTop: 12 }}>
          <dt>Age</dt>
          <dd className="num">{d.age} ticks <span className="dim">· {(d.age / 168).toFixed(1)} weeks</span></dd>
          <dt>Expected</dt>
          <dd className="num">
            {d.expected_lifespan}
            {d.expected_lifespan < l.baseline_ticks && (
              <span className="dim"> · {l.baseline_ticks - d.expected_lifespan} short of baseline</span>
            )}
          </dd>
          <dt>Position</dt><dd className="num">{d.x}, {d.y}</dd>
          <dt>Shelter</dt>
          <dd>{d.sheltered ? "under a roof" : <span className="dim">in the open</span>}</dd>
        </dl>
      </div>

      <div className="sec">
        <div className="sec-head">
          <span className="eyebrow">Felt state</span>
          <span className="dim" style={{ fontSize: 11 }}>{d.felt_state}</span>
        </div>
        <div className="needs">
          <Need label="Hunger" v={d.hunger} />
          <Need label="Thirst" v={d.thirst} />
          <Need label="Fatigue" v={d.fatigue} />
          <Need label="Warmth" v={d.warmth} />
          <Need label="Health" v={d.health} />
        </div>
      </div>

      <div className="sec">
        <div className="sec-head">
          <span className="eyebrow">Traits</span>
          <span className="dim" style={{ fontSize: 11 }}>heritable</span>
        </div>
        <div className="needs">
          <Trait label="Boldness" v={d.traits.boldness} />
          <Trait label="Industry" v={d.traits.industry} />
          <Trait label="Sociability" v={d.traits.sociability} />
          <Trait label="Caution" v={d.traits.caution} />
        </div>
      </div>

      <div className="sec">
        <div className="sec-head">
          <span className="eyebrow">Household and kin</span>
          {d.household_id != null && (
            <span className="dim" style={{ fontSize: 11 }}>
              #{d.household_id} · {d.household_members} member
              {d.household_members === 1 ? "" : "s"}
            </span>
          )}
        </div>
        {d.household_id == null ? (
          <p className="hint">No household. Nowhere to keep anything, and — until
            there is — no children (§4.8).</p>
        ) : (
          <>
            {/* Only grain keeps, so the store is really a grain store. Showing
                both makes it obvious when a household is rich in food that is
                about to rot and still cannot have a child. */}
            <div className="needs" style={{ marginBottom: 8 }}>
              <div className="need">
                <span className="lbl">Store</span>
                <div className="track">
                  <div
                    className="fill"
                    style={{
                      width: `${Math.min(100, (d.household_store / RESERVE_HINT) * 100)}%`,
                      background: d.household_store >= RESERVE_HINT
                        ? "var(--st-good)" : "var(--c2)",
                    }}
                  />
                </div>
                <span className="v">{d.household_store.toFixed(0)}</span>
              </div>
              <div className="need">
                <span className="lbl">of which grain</span>
                <div className="track">
                  <div
                    className="fill"
                    style={{
                      width: `${Math.min(100, (d.household_grain / RESERVE_HINT) * 100)}%`,
                      background: "var(--res-wheat)",
                    }}
                  />
                </div>
                <span className="v">{d.household_grain.toFixed(0)}</span>
              </div>
            </div>
          </>
        )}

        <dl className="kv">
          <dt>Mate</dt>
          <dd>
            {d.mate ? (
              <>{d.mate[1]} <span className="dim">· #{d.mate[0]}</span></>
            ) : (
              <span className="dim">unattached</span>
            )}
          </dd>
          <dt>Parents</dt>
          <dd>
            {d.mother || d.father ? (
              <>
                {d.mother ? d.mother[1] : "—"}
                {" · "}
                {d.father ? d.father[1] : "—"}
              </>
            ) : (
              <span className="dim">a founder</span>
            )}
          </dd>
          <dt>Children</dt><dd className="num">{d.children_born}</dd>
          {d.expecting_in != null && (
            <>
              <dt>Expecting</dt>
              <dd className="num quick">in {Math.max(0, d.expecting_in)} ticks</dd>
            </>
          )}
          {d.cannot_yet && (
            <>
              <dt>Not yet</dt>
              <dd className="dim">{d.cannot_yet}</dd>
            </>
          )}
        </dl>
      </div>

      <div className="sec">
        <div className="sec-head">
          <span className="eyebrow">Committed plan</span>
          <span className="badge">tier {d.plan_tier} · {d.plan_addresses.toLowerCase()}</span>
        </div>
        {d.steps.length === 0 ? (
          <p className="hint">Deciding.</p>
        ) : (
          <div className="plan">
            <ol className="plan-steps">
              {d.steps.map((s, i) => (
                <li key={i} className={s.done ? "done" : s.current ? "now" : undefined}>
                  <span className="idx">{s.done ? "" : s.current ? "▸" : i + 1}</span>
                  <span>{s.label}</span>
                  <span className="cost">{s.est_ticks}t</span>
                </li>
              ))}
            </ol>
            <div className="horizon">
              <span className="eyebrow">Horizon</span>
              <div className="track">
                <div
                  className="fill"
                  style={{
                    width: `${d.plan_horizon ? (d.plan_remaining / d.plan_horizon) * 100 : 0}%`,
                  }}
                />
              </div>
              <span className="num" style={{ fontSize: 11 }}>
                {d.plan_remaining}<span className="dim">/{d.plan_horizon}</span>
              </span>
            </div>
            {d.plan_rationale && <p className="rationale">“{d.plan_rationale}”</p>}
          </div>
        )}
      </div>

      <div className="sec">
        <div className="sec-head">
          <span className="eyebrow">Beliefs</span>
          <span className="dim" style={{ fontSize: 11 }}>
            {d.belief_count} held · ranked by relevance
          </span>
        </div>
        {d.beliefs.length === 0 ? (
          <p className="hint">Knows nothing yet.</p>
        ) : (
          <div className="beliefs">
            {d.beliefs.slice(0, 10).map((b, i) => (
              <div className="belief" key={i}>
                <span className="conf-bar" style={{ background: confColour(b.confidence) }} />
                <div>
                  <div className="what">
                    {b.kind.replace(/_/g, " ").toLowerCase()} · {b.x},{b.y}
                  </div>
                  <div className="src">{b.provenance}. Looked {b.estimate}.</div>
                </div>
                <div className="meta">
                  {b.hops === 0 ? "firsthand" : `${b.hops} hop${b.hops > 1 ? "s" : ""}`}
                  <br />
                  {b.confidence.toFixed(2)}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>

      <div className="sec" style={{ borderBottom: 0 }}>
        <div className="sec-head"><span className="eyebrow">Carrying</span></div>
        {d.carrying.length === 0 ? (
          <p className="hint">Empty-handed.</p>
        ) : (
          <dl className="kv">
            {d.carrying.map(([kind, qty, spoils]) => (
              <FragmentRow key={kind} kind={kind} qty={qty} spoils={spoils} />
            ))}
          </dl>
        )}
        <dl className="kv" style={{ marginTop: 10 }}>
          <dt>Decisions</dt>
          <dd className="num">{d.lifetime_deliberations} <span className="dim">lifetime</span></dd>
          <dt>Taught</dt>
          <dd className="num">{d.taught_count} <span className="dim">times</span></dd>
          <dt>Told</dt>
          <dd className="num">{d.shared_count} <span className="dim">times</span></dd>
          {/* S7 at the level of one creature: how much of what it knows came
              from somebody who is no longer alive to be asked. */}
          <dt>Knows secondhand</dt>
          <dd className="num">
            {d.inherited_beliefs}
            {d.from_the_dead > 0 && (
              <span className="dim"> · {d.from_the_dead} from the dead</span>
            )}
          </dd>
        </dl>
      </div>
    </>
  );
}

function FragmentRow({ kind, qty, spoils }: { kind: string; qty: number; spoils: number | null }) {
  // Only grain keeps indefinitely (§4.4), and the difference is the whole
  // reason the resource portfolio exists — so it is shown, not implied.
  const urgent = spoils != null && spoils < 12;
  return (
    <>
      <dt>{kind.toLowerCase()}</dt>
      <dd className="num">
        {qty.toFixed(1)}{" "}
        {spoils == null ? (
          <span className="dim">· keeps</span>
        ) : (
          <span style={{ color: urgent ? "var(--st-serious)" : undefined }} className="dim">
            · spoils in {Math.max(0, spoils)}t
          </span>
        )}
      </dd>
    </>
  );
}

export function Need({ label, v }: { label: string; v: number }) {
  const cls = v > 60 ? "" : v > 35 ? "warn" : v > 15 ? "serious" : "critical";
  return (
    <div className="need">
      <span className="lbl">{label}</span>
      <div className="track"><div className={`fill ${cls}`} style={{ width: `${v}%` }} /></div>
      <span className="v">{v.toFixed(0)}</span>
    </div>
  );
}

function Trait({ label, v }: { label: string; v: number }) {
  return (
    <div className="need">
      <span className="lbl">{label}</span>
      <div className="track">
        <div className="fill" style={{ width: `${v * 100}%`, background: "var(--c2)" }} />
      </div>
      <span className="v">{v.toFixed(2)}</span>
    </div>
  );
}

/** The sequential ramp: hearsay at the dim end, firsthand at the bright one. */
function confColour(c: number) {
  return c > 0.8 ? "var(--seq-5)"
    : c > 0.6 ? "var(--seq-4)"
    : c > 0.4 ? "var(--seq-3)"
    : c > 0.2 ? "var(--seq-2)"
    : "var(--seq-1)";
}
