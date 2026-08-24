/**
 * The lifespan track (BUILD.md §7.3).
 *
 * A horizontal track over the baseline lifespan showing the infant/adult/elder
 * zones, the life spent so far, and a marker for *now* — or, in the still
 * colour, where a life actually ended against the baseline it was owed.
 *
 * This is the product's signature device and it appears wherever a creature is
 * named. It is built once, here, precisely so that it stays one thing.
 *
 * The zone divisions are derived from config rather than hardcoded at 25% and
 * 87.5% the way the mockup has them: infancy and old age are the two dials
 * §4.7 says get tuned first, and a track that disagreed with the simulation
 * would be worse than no track.
 */
interface Props {
  /** Chronological age in ticks. */
  age: number;
  baseline: number;
  infantUntil: number;
  elderFrom: number;
  /** What this creature is actually expected to reach, after a hard life. */
  expected?: number;
  dead?: boolean;
  mini?: boolean;
}

export function Lifespan({
  age, baseline, infantUntil, elderFrom, expected, dead, mini,
}: Props) {
  const span = Math.max(baseline, expected ?? 0, age);
  const pct = (t: number) => `${Math.min(100, (t / span) * 100)}%`;

  return (
    <div className="lifespan-wrap">
      <div
        className={`lifespan${dead ? " is-dead" : ""}${mini ? " mini" : ""}`}
        style={{ ["--age" as string]: Math.min(1, age / span) }}
        role="img"
        aria-label={
          dead
            ? `Died at tick ${age} against a baseline of ${baseline}`
            : `Age ${age} of an expected ${expected ?? baseline}`
        }
      >
        <div className="zone-div" style={{ left: pct(infantUntil) }} />
        <div className="zone-div" style={{ left: pct(elderFrom) }} />
        {expected != null && expected < span && (
          // Where this life is now expected to end, against the baseline it was
          // owed. Nights without shelter shorten it; good ones push it out.
          <div
            className="zone-div"
            style={{ left: pct(expected), background: "var(--still)", opacity: 0.8 }}
          />
        )}
      </div>
      {!mini && (
        <div className="lifespan-legend">
          <span>infant</span>
          <span>adult</span>
          <span>elder</span>
          <span className="num">{Math.round(span)}</span>
        </div>
      )}
    </div>
  );
}
