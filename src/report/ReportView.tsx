import { useCallback, useEffect, useState } from "react";
import * as api from "./api";
import { BarChart, Figure, LineChart, StackedBar, Stat, colorFor, fmt } from "./charts";
import { LineageTree } from "./LineageTree";
import { LifeStory } from "./LifeStory";
import type * as R from "./types";

/**
 * The reporting view (PRD §9.4, §10).
 *
 * A separate full-window view, not a side panel — §9.4 is explicit about that,
 * and it is right: these are read while thinking about a run, not while
 * watching one.
 *
 * Tabs load on first open and are then kept, because several of the lineage
 * queries are recursive CTEs over every creature ever born and re-running them
 * on every tab switch would make the view feel broken during a long run.
 */

const TABS = [
  "Overview", "Lineage", "Mortality", "Economy",
  "Knowledge", "Behaviour", "Deliberation", "Planning", "A life",
] as const;
type Tab = (typeof TABS)[number];

/** Loads once, remembers the error, and never leaves the caller guessing. */
function useReport<T>(load: () => Promise<T>, active: boolean) {
  const [data, setData] = useState<T | null>(null);
  const [err, setErr] = useState<string | null>(null);
  useEffect(() => {
    if (!active || data !== null) return;
    load().then(setData).catch((e) => setErr(String(e)));
    // `load` is a fresh closure every render; depending on it would re-fetch
    // forever. The tab becoming active is the only trigger that matters.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [active]);
  return { data, err };
}

export function ReportView({ onClose }: { onClose: () => void }) {
  const [tab, setTab] = useState<Tab>("Overview");
  const [founder, setFounder] = useState<number | null>(null);
  const [subject, setSubject] = useState<number | null>(null);
  const [exported, setExported] = useState<string | null>(null);

  const exportAll = useCallback(() => {
    api.exportCsv().then(setExported).catch((e) => setExported(String(e)));
  }, []);

  const goToLife = (id: number) => {
    setSubject(id);
    setTab("A life");
  };

  return (
    <div className="report">
      <header className="report-bar">
        <h1>The record</h1>
        <nav>
          {TABS.map((t) => (
            <button key={t} className={t === tab ? "on" : ""} onClick={() => setTab(t)}>
              {t}
            </button>
          ))}
        </nav>
        <div className="report-actions">
          {exported && <span className="exported">{exported}</span>}
          <button onClick={exportAll}>Export CSV</button>
          <button onClick={onClose}>Back to the world</button>
        </div>
      </header>

      <main className="report-body">
        {tab === "Overview" && <Overview onFounder={(id) => { setFounder(id); setTab("Lineage"); }} />}
        {tab === "Lineage" && <Lineage founder={founder} setFounder={setFounder} onLife={goToLife} />}
        {tab === "Mortality" && <Mortality />}
        {tab === "Economy" && <Economy />}
        {tab === "Knowledge" && <Knowledge />}
        {tab === "Behaviour" && <Behaviour />}
        {tab === "Deliberation" && <Deliberation />}
        {tab === "Planning" && <Planning />}
        {tab === "A life" && <LifeStory initial={subject} />}
      </main>
    </div>
  );
}

// ------------------------------------------------------------------ overview

function Overview({ onFounder }: { onFounder: (id: number) => void }) {
  const { data: h, err } = useReport(api.headline, true);
  const { data: pop } = useReport(() => api.population(240), true);
  const { data: lines } = useReport(() => api.lineages(10), true);

  if (err) return <p className="fig-empty">{err}</p>;
  if (!h) return <p className="fig-empty">Reading…</p>;

  return (
    <>
      <div className="stats wide">
        <Stat
          label="Deepest lineage"
          value={`generation ${h.deepest_generation}`}
          sub={h.deepest_founder ? `${h.deepest_founder} · ${fmt(h.deepest_descendants)} descendants` : "no lineage yet"}
        />
        <Stat
          label="Median life"
          value={`${fmt(h.median_life_ticks)} ticks`}
          sub={`against a ${fmt(h.baseline_ticks)}-tick baseline`}
        />
        <Stat
          label="Infant mortality"
          value={`${(h.infant_mortality * 100).toFixed(0)}%`}
          sub={`${(h.infant_mortality_first_gen * 100).toFixed(0)}% in the founding generation`}
        />
        <Stat
          label="Beliefs outliving their finder"
          value={`${(h.beliefs_outliving_finders * 100).toFixed(0)}%`}
          sub="criterion S7"
        />
        <Stat
          label="Alive now"
          value={fmt(h.living)}
          sub={`${fmt(h.total_born)} born · ${fmt(h.total_dead)} dead · through tick ${fmt(h.through_tick)}`}
        />
      </div>

      <Figure
        title="Population over time"
        note="Births and deaths are drawn against the same count axis as the population they move — they are the same unit, so this is one chart and not two."
        rows={pop ?? []}
        series={[
          { key: "population", label: "population", value: (d: R.PopulationPoint) => d.population },
          { key: "births", label: "births", value: (d: R.PopulationPoint) => d.births, color: "var(--quick)" },
          { key: "deaths", label: "deaths", value: (d: R.PopulationPoint) => d.deaths, color: "var(--still)" },
        ]}
        columns={[
          { key: "tick", label: "tick", get: (d: R.PopulationPoint) => d.tick },
          { key: "population", label: "population", get: (d: R.PopulationPoint) => d.population },
          { key: "births", label: "births", get: (d: R.PopulationPoint) => d.births },
          { key: "deaths", label: "deaths", get: (d: R.PopulationPoint) => d.deaths },
        ]}
      >
        <LineChart
          rows={pop ?? []}
          x={(d: R.PopulationPoint) => d.tick}
          xLabel="tick"
          height={230}
          series={[
            { key: "population", label: "population", value: (d: R.PopulationPoint) => d.population },
            { key: "births", label: "births", value: (d: R.PopulationPoint) => d.births, color: "var(--quick)" },
            { key: "deaths", label: "deaths", value: (d: R.PopulationPoint) => d.deaths, color: "var(--still)" },
          ]}
        />
      </Figure>

      <section>
        <h3>Deepest lineages</h3>
        <table className="fig-table clickable">
          <thead>
            <tr>
              <th>founder</th><th>generations</th><th>descendants</th>
              <th>living</th><th>founder</th>
            </tr>
          </thead>
          <tbody>
            {(lines ?? []).map((l) => (
              <tr key={l.founder_id} onClick={() => onFounder(l.founder_id)}>
                <td>{l.founder_name}</td>
                <td>{l.depth}</td>
                <td>{fmt(l.descendants)}</td>
                <td>{fmt(l.living_descendants)}</td>
                <td className={l.founder_alive ? "quick" : "still"}>
                  {l.founder_alive ? "alive" : "dead"}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </section>
    </>
  );
}

// ------------------------------------------------------------------- lineage

function Lineage({
  founder, setFounder, onLife,
}: {
  founder: number | null;
  setFounder: (id: number) => void;
  onLife: (id: number) => void;
}) {
  const { data: lines } = useReport(() => api.lineages(40), true);
  const { data: gens } = useReport(api.generations, true);
  const { data: surv } = useReport(api.survival, true);
  const [tree, setTree] = useState<R.TreeNode[]>([]);

  useEffect(() => {
    const id = founder ?? lines?.[0]?.founder_id ?? null;
    if (id == null) return;
    if (founder == null) setFounder(id);
    api.lineageTree(id).then(setTree).catch(() => setTree([]));
  }, [founder, lines, setFounder]);

  return (
    <>
      <div className="split">
        <section>
          <h3>Founders</h3>
          <div className="fig-scroll tall">
            <table className="fig-table clickable">
              <thead>
                <tr><th>founder</th><th>gens</th><th>desc.</th><th>living</th></tr>
              </thead>
              <tbody>
                {(lines ?? []).map((l) => (
                  <tr
                    key={l.founder_id}
                    className={l.founder_id === founder ? "on" : ""}
                    onClick={() => setFounder(l.founder_id)}
                  >
                    <td>{l.founder_name}</td>
                    <td>{l.depth}</td>
                    <td>{fmt(l.descendants)}</td>
                    <td>{fmt(l.living_descendants)}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </section>

        <section>
          <h3>The bloodline</h3>
          <p className="fig-note">
            Click anyone to read their life. A filled mark is alive, a hollow one is dead;
            a faint edge is the second parent, who is in the lineage too.
          </p>
          <LineageTree nodes={tree} selected={null} onSelect={onLife} />
        </section>
      </div>

      <Figure
        title="Lineage survival"
        note="Share of founders whose line was still producing creatures at each generation. Starts at 1.0 by construction."
        rows={surv ?? []}
        series={[{ key: "share_surviving", label: "surviving", value: (d: R.SurvivalPoint) => d.share_surviving }]}
        columns={[
          { key: "generation", label: "generation", get: (d: R.SurvivalPoint) => d.generation },
          { key: "share", label: "share surviving", get: (d: R.SurvivalPoint) => `${(d.share_surviving * 100).toFixed(0)}%` },
          { key: "lineages", label: "lineages", get: (d: R.SurvivalPoint) => d.lineages },
        ]}
      >
        <BarChart
          rows={surv ?? []}
          label={(d: R.SurvivalPoint) => `g${d.generation}`}
          share
          height={190}
          series={[{ key: "share_surviving", label: "surviving", value: (d: R.SurvivalPoint) => d.share_surviving }]}
        />
      </Figure>

      <Figure
        title="Trait drift across generations"
        note="§4.9's clearest evidence that selection is operating: traits are inherited with noise, so a drift means something is being selected for."
        rows={gens ?? []}
        series={[
          { key: "boldness", label: "boldness", value: (d: R.GenerationRow) => d.boldness },
          { key: "industry", label: "industry", value: (d: R.GenerationRow) => d.industry },
          { key: "sociability", label: "sociability", value: (d: R.GenerationRow) => d.sociability },
          { key: "caution", label: "caution", value: (d: R.GenerationRow) => d.caution },
        ]}
      >
        <LineChart
          rows={gens ?? []}
          x={(d: R.GenerationRow) => d.generation}
          xLabel="generation"
          zero={false}
          height={200}
          series={[
            { key: "boldness", label: "boldness", value: (d: R.GenerationRow) => d.boldness },
            { key: "industry", label: "industry", value: (d: R.GenerationRow) => d.industry },
            { key: "sociability", label: "sociability", value: (d: R.GenerationRow) => d.sociability },
            { key: "caution", label: "caution", value: (d: R.GenerationRow) => d.caution },
          ]}
        />
      </Figure>

      <Figure
        title="Each generation"
        note="Born, still living, and the share that reached adulthood at all."
        rows={gens ?? []}
        series={[
          { key: "born", label: "born", value: (d: R.GenerationRow) => d.born },
          { key: "living", label: "living", value: (d: R.GenerationRow) => d.living },
        ]}
        columns={[
          { key: "generation", label: "generation", get: (d: R.GenerationRow) => d.generation },
          { key: "born", label: "born", get: (d: R.GenerationRow) => d.born },
          { key: "living", label: "living", get: (d: R.GenerationRow) => d.living },
          { key: "median_life", label: "median life", get: (d: R.GenerationRow) => d.median_life },
          { key: "adult", label: "reached adulthood", get: (d: R.GenerationRow) => `${(d.reached_adulthood * 100).toFixed(0)}%` },
        ]}
      >
        <BarChart
          rows={gens ?? []}
          label={(d: R.GenerationRow) => `g${d.generation}`}
          height={200}
          series={[
            { key: "born", label: "born", value: (d: R.GenerationRow) => d.born },
            { key: "living", label: "living", value: (d: R.GenerationRow) => d.living },
          ]}
        />
      </Figure>
    </>
  );
}

// ----------------------------------------------------------------- mortality

function Mortality() {
  const { data: causes } = useReport(api.causes, true);
  const { data: ages } = useReport(() => api.ageAtDeath(48), true);
  const { data: h } = useReport(api.headline, true);

  // Pivot to one row per generation so the causes can be compared within it.
  const kinds = [...new Set((causes ?? []).map((c) => c.cause))].sort();
  const byGen = [...new Set((causes ?? []).map((c) => c.generation))]
    .sort((a, b) => a - b)
    .map((g) => {
      const row: Record<string, number> & { generation: number } = { generation: g };
      for (const k of kinds) {
        row[k] = (causes ?? []).find((c) => c.generation === g && c.cause === k)?.deaths ?? 0;
      }
      return row;
    });

  const series = kinds.map((k) => ({
    key: k,
    label: k.toLowerCase().replace(/_/g, " "),
    value: (d: (typeof byGen)[number]) => d[k] ?? 0,
    color: colorFor("cause", k),
  }));

  return (
    <>
      <Figure
        title="Cause of death by generation"
        note="§10 calls this the diagnostic for whether the difficulty curve is working. Stacked, because every death has exactly one cause — these really are parts of one whole."
        rows={byGen}
        series={series}
        columns={[
          { key: "generation", label: "generation", get: (d) => `g${d.generation}` },
          ...kinds.map((k) => ({ key: k, label: k.toLowerCase().replace(/_/g, " "), get: (d: (typeof byGen)[number]) => d[k] ?? 0 })),
        ]}
      >
        <StackedBar
          rows={byGen}
          series={series}
          label={(d) => `g${d.generation}`}
          height={240}
          partsOfAWhole
        />
      </Figure>

      <Figure
        title="Age at death"
        note="Against the lifespan baseline. A distribution that piles up far to the left of the rule means almost nobody is dying of old age."
        rows={ages ?? []}
        series={[{ key: "deaths", label: "deaths", value: (d: R.AgeBucket) => d.deaths }]}
        columns={[
          { key: "from", label: "age (ticks)", get: (d: R.AgeBucket) => d.from_ticks },
          { key: "deaths", label: "deaths", get: (d: R.AgeBucket) => d.deaths },
        ]}
      >
        <LineChart
          rows={ages ?? []}
          x={(d: R.AgeBucket) => d.from_ticks}
          xLabel="age at death (ticks)"
          height={200}
          rule={h ? { at: 0, label: `baseline ${h.baseline_ticks} ticks` } : undefined}
          series={[{ key: "deaths", label: "deaths", value: (d: R.AgeBucket) => d.deaths }]}
        />
      </Figure>
    </>
  );
}

// ------------------------------------------------------------------- economy

function Economy() {
  const { data: econ } = useReport(() => api.economy(240), true);
  const { data: farm } = useReport(api.farming, true);
  const { data: wood } = useReport(() => api.wood(240), true);
  const { data: wealth } = useReport(api.wealth, true);

  return (
    <>
      <Figure
        title="Food: produced, eaten, rotted"
        note="High forage waste means the community is over-gathering perishables instead of investing in crops — and since only grain reaches the reproduction reserve, that is the same thing as a community with no future."
        rows={econ ?? []}
        series={[
          { key: "gathered", label: "gathered", value: (d: R.EconomyPoint) => d.gathered, color: "var(--res-forage)" },
          { key: "harvested", label: "harvested", value: (d: R.EconomyPoint) => d.harvested, color: "var(--res-wheat)" },
          { key: "eaten", label: "eaten", value: (d: R.EconomyPoint) => d.eaten },
          { key: "spoiled", label: "spoiled", value: (d: R.EconomyPoint) => d.spoiled, color: "var(--st-critical)" },
        ]}
      >
        <LineChart
          rows={econ ?? []}
          x={(d: R.EconomyPoint) => d.tick}
          xLabel="tick"
          height={220}
          series={[
            { key: "gathered", label: "gathered", value: (d: R.EconomyPoint) => d.gathered, color: "var(--res-forage)" },
            { key: "harvested", label: "harvested", value: (d: R.EconomyPoint) => d.harvested, color: "var(--res-wheat)" },
            { key: "eaten", label: "eaten", value: (d: R.EconomyPoint) => d.eaten },
            { key: "spoiled", label: "spoiled", value: (d: R.EconomyPoint) => d.spoiled, color: "var(--st-critical)" },
          ]}
        />
      </Figure>

      <Figure
        title="Wood: cut, built with, burned"
        note="Grouped, never stacked: wood cut this tick may be burned twenty ticks later, so these three do not sum to anything. Fuel is the community's real spend on staying warm and going further."
        rows={wood ?? []}
        series={[
          { key: "chopped", label: "chopped", value: (d: R.WoodSplit) => d.chopped, color: "var(--res-wood)" },
          { key: "timber", label: "timber", value: (d: R.WoodSplit) => d.timber },
          { key: "fuel", label: "fuel", value: (d: R.WoodSplit) => d.fuel, color: "var(--quick)" },
        ]}
      >
        <LineChart
          rows={wood ?? []}
          x={(d: R.WoodSplit) => d.tick}
          xLabel="tick"
          height={200}
          series={[
            { key: "chopped", label: "chopped", value: (d: R.WoodSplit) => d.chopped, color: "var(--res-wood)" },
            { key: "timber", label: "timber", value: (d: R.WoodSplit) => d.timber },
            { key: "fuel", label: "fuel", value: (d: R.WoodSplit) => d.fuel, color: "var(--quick)" },
          ]}
        />
      </Figure>

      <Figure
        title="Farming adoption by generation"
        note="With spoilage in play this is effectively a reproduction forecast: only grain reaches the household reserve."
        rows={farm ?? []}
        series={[{ key: "share_who_farmed", label: "farmed", value: (d: R.FarmingRow) => d.share_who_farmed }]}
        columns={[
          { key: "generation", label: "generation", get: (d: R.FarmingRow) => `g${d.generation}` },
          { key: "creatures", label: "creatures", get: (d: R.FarmingRow) => d.creatures },
          { key: "planted", label: "planted", get: (d: R.FarmingRow) => d.planted },
          { key: "harvested", label: "harvested", get: (d: R.FarmingRow) => d.harvested },
          { key: "share", label: "share who farmed", get: (d: R.FarmingRow) => `${(d.share_who_farmed * 100).toFixed(0)}%` },
        ]}
      >
        <BarChart
          rows={farm ?? []}
          label={(d: R.FarmingRow) => `g${d.generation}`}
          share
          height={190}
          series={[{ key: "share_who_farmed", label: "farmed", value: (d: R.FarmingRow) => d.share_who_farmed, color: "var(--res-wheat)" }]}
        />
      </Figure>

      <Figure
        title="Household wealth"
        note="Grain is broken out because it is the only food that reaches the reproduction reserve — a household rich in berries is not wealthy in any sense that affects whether it has children."
        rows={(wealth ?? []).slice(0, 24)}
        series={[
          { key: "grain", label: "grain", value: (d: R.WealthRow) => d.grain, color: "var(--res-wheat)" },
          { key: "wood", label: "wood", value: (d: R.WealthRow) => d.wood, color: "var(--res-wood)" },
          { key: "other", label: "other food", value: (d: R.WealthRow) => d.other, color: "var(--res-forage)" },
        ]}
      >
        <BarChart
          rows={(wealth ?? []).slice(0, 24)}
          label={(d: R.WealthRow) => `#${d.household_id}`}
          height={200}
          series={[
            { key: "grain", label: "grain", value: (d: R.WealthRow) => d.grain, color: "var(--res-wheat)" },
            { key: "wood", label: "wood", value: (d: R.WealthRow) => d.wood, color: "var(--res-wood)" },
            { key: "other", label: "other food", value: (d: R.WealthRow) => d.other, color: "var(--res-forage)" },
          ]}
        />
      </Figure>
    </>
  );
}

// ----------------------------------------------------------------- knowledge

function Knowledge() {
  const { data: cov } = useReport(api.coverage, true);
  const { data: half } = useReport(api.halfLife, true);
  const { data: acc } = useReport(api.accuracy, true);
  const { data: teach } = useReport(api.teaching, true);
  const { data: prov } = useReport(api.beliefs, true);
  const { data: chan } = useReport(api.transmission, true);
  const { data: edges } = useReport(() => api.graph(200), true);

  const hubs = [...(edges ?? [])]
    .reduce((m, e) => m.set(e.from_name, (m.get(e.from_name) ?? 0) + e.beliefs), new Map<string, number>());
  const topHubs = [...hubs.entries()]
    .sort((a, b) => b[1] - a[1]).slice(0, 12)
    .map(([name, beliefs]) => ({ name, beliefs }));

  return (
    <>
      <Figure
        title="What the community collectively knows"
        note="Sampled, so gaps in the line are gaps in sampling. Expect a ragged expansion that stalls or collapses when a knowledgeable lineage dies out."
        rows={cov ?? []}
        series={[{ key: "known_tiles", label: "tiles known", value: (d: R.CoveragePoint) => d.known_tiles }]}
        columns={[
          { key: "tick", label: "tick", get: (d: R.CoveragePoint) => d.tick },
          { key: "tiles", label: "tiles known", get: (d: R.CoveragePoint) => d.known_tiles },
          { key: "share", label: "share of world", get: (d: R.CoveragePoint) => `${(d.share_of_world * 100).toFixed(2)}%` },
          { key: "per", label: "per creature", get: (d: R.CoveragePoint) => d.per_capita.toFixed(1) },
        ]}
      >
        <LineChart
          rows={cov ?? []}
          x={(d: R.CoveragePoint) => d.tick}
          xLabel="tick"
          height={200}
          series={[{ key: "known_tiles", label: "tiles known", value: (d: R.CoveragePoint) => d.known_tiles }]}
        />
      </Figure>

      <Figure
        title="Knowledge per creature"
        note="Separate from the chart above rather than a second axis on it. A rising total with a falling ratio is a community coasting on what a few well-travelled elders remember."
        rows={cov ?? []}
        series={[{ key: "per_capita", label: "tiles per creature", value: (d: R.CoveragePoint) => d.per_capita }]}
      >
        <LineChart
          rows={cov ?? []}
          x={(d: R.CoveragePoint) => d.tick}
          xLabel="tick"
          height={170}
          series={[{ key: "per_capita", label: "tiles per creature", value: (d: R.CoveragePoint) => d.per_capita }]}
        />
      </Figure>

      <Figure
        title="Belief accuracy by hop count"
        note="§4.11's premise is that secondhand knowledge is genuinely worse. If this comes back flat, hop count is decorative and transmission is free."
        rows={acc ?? []}
        series={[{ key: "stale_rate", label: "turned out stale", value: (d: R.AccuracyRow) => d.stale_rate }]}
        columns={[
          { key: "hops", label: "hops", get: (d: R.AccuracyRow) => d.hops },
          { key: "acted", label: "acted on", get: (d: R.AccuracyRow) => d.acted_on },
          { key: "stale", label: "stale", get: (d: R.AccuracyRow) => d.stale },
          { key: "rate", label: "stale rate", get: (d: R.AccuracyRow) => `${(d.stale_rate * 100).toFixed(0)}%` },
        ]}
        thin={
          (acc ?? []).some((a) => a.acted_on < 30)
            ? "Some hop counts have fewer than 30 acted-on beliefs behind them; read those bars as an indication, not a rate."
            : undefined
        }
      >
        <BarChart
          rows={acc ?? []}
          label={(d: R.AccuracyRow) => `${d.hops} hop${d.hops === 1 ? "" : "s"}`}
          share
          height={180}
          series={[{ key: "stale_rate", label: "turned out stale", value: (d: R.AccuracyRow) => d.stale_rate }]}
        />
      </Figure>

      <Figure
        title="Belief provenance"
        note="How many beliefs are held at each remove, and how much of what circulates was found by somebody now dead — the S7 number."
        rows={prov ?? []}
        series={[
          { key: "beliefs", label: "beliefs held", value: (d: R.BeliefProvenance) => d.beliefs },
          { key: "from_the_dead", label: "found by the dead", value: (d: R.BeliefProvenance) => d.from_the_dead, color: "var(--still)" },
        ]}
        columns={[
          { key: "hops", label: "hops", get: (d: R.BeliefProvenance) => d.hops },
          { key: "beliefs", label: "beliefs", get: (d: R.BeliefProvenance) => d.beliefs },
          { key: "conf", label: "mean confidence", get: (d: R.BeliefProvenance) => d.mean_confidence.toFixed(2) },
          { key: "dead", label: "from the dead", get: (d: R.BeliefProvenance) => d.from_the_dead },
        ]}
      >
        <BarChart
          rows={prov ?? []}
          label={(d: R.BeliefProvenance) => `${d.hops}`}
          height={190}
          series={[
            { key: "beliefs", label: "beliefs held", value: (d: R.BeliefProvenance) => d.beliefs },
            { key: "from_the_dead", label: "found by the dead", value: (d: R.BeliefProvenance) => d.from_the_dead, color: "var(--still)" },
          ]}
        />
      </Figure>

      <Figure
        title="Knowledge half-life"
        note="Ticks from a fact being discovered to the last creature holding it dying. Beliefs still in circulation are excluded, since counting them could only bias this downward."
        rows={half ?? []}
        series={[
          { key: "median_ticks", label: "median", value: (d: R.HalfLifeRow) => d.median_ticks },
          { key: "p90_ticks", label: "p90", value: (d: R.HalfLifeRow) => d.p90_ticks },
        ]}
        columns={[
          { key: "kind", label: "kind", get: (d: R.HalfLifeRow) => d.kind.toLowerCase().replace(/_/g, " ") },
          { key: "median", label: "median ticks", get: (d: R.HalfLifeRow) => d.median_ticks },
          { key: "p90", label: "p90 ticks", get: (d: R.HalfLifeRow) => d.p90_ticks },
          { key: "alive", label: "still circulating", get: (d: R.HalfLifeRow) => d.still_alive },
          { key: "gone", label: "extinguished", get: (d: R.HalfLifeRow) => d.extinguished },
        ]}
      >
        <BarChart
          rows={half ?? []}
          label={(d: R.HalfLifeRow) => d.kind.toLowerCase().replace(/_/g, " ").slice(0, 10)}
          height={200}
          series={[
            { key: "median_ticks", label: "median", value: (d: R.HalfLifeRow) => d.median_ticks },
            { key: "p90_ticks", label: "p90", value: (d: R.HalfLifeRow) => d.p90_ticks },
          ]}
        />
      </Figure>

      <Figure
        title="How knowledge moves"
        note="Teaching costs ticks and pays off only after the teacher is dead (§13.5). If this is all overhearing, nobody is choosing to pass anything on."
        rows={chan ?? []}
        series={[{ key: "beliefs", label: "beliefs", value: (d: R.TransmissionRow) => d.beliefs }]}
        columns={[
          { key: "channel", label: "channel", get: (d: R.TransmissionRow) => d.channel.toLowerCase() },
          { key: "events", label: "events", get: (d: R.TransmissionRow) => d.events },
          { key: "beliefs", label: "beliefs", get: (d: R.TransmissionRow) => d.beliefs },
        ]}
      >
        <BarChart
          rows={chan ?? []}
          label={(d: R.TransmissionRow) => d.channel.toLowerCase()}
          height={180}
          series={[{ key: "beliefs", label: "beliefs", value: (d: R.TransmissionRow) => d.beliefs }]}
        />
      </Figure>

      <div className="split">
        <Figure
          title="Information hubs"
          note="Who informs whom, summed over the heaviest edges. A few tall bars means hubs emerged; a flat field means everyone is telling everyone."
          rows={topHubs}
          series={[{ key: "beliefs", label: "beliefs passed on", value: (d) => d.beliefs }]}
        >
          <BarChart
            rows={topHubs}
            label={(d) => d.name.slice(0, 8)}
            height={190}
            series={[{ key: "beliefs", label: "beliefs passed on", value: (d) => d.beliefs }]}
          />
        </Figure>

        <Figure
          title="Teaching against lineage depth"
          note="§10's direct test of whether lineages that teach out-survive those that don't. Per-member, because a large household out-teaches a small one on volume alone."
          rows={(teach ?? []).slice(0, 24)}
          series={[
            { key: "per_member", label: "teaching per member", value: (d: R.TeachingRow) => d.per_member },
            { key: "lineage_depth", label: "lineage depth", value: (d: R.TeachingRow) => d.lineage_depth },
          ]}
          columns={[
            { key: "hh", label: "household", get: (d: R.TeachingRow) => `#${d.household_id}` },
            { key: "members", label: "members", get: (d: R.TeachingRow) => d.members },
            { key: "acts", label: "teaching acts", get: (d: R.TeachingRow) => d.teaching_events },
            { key: "per", label: "per member", get: (d: R.TeachingRow) => d.per_member.toFixed(2) },
            { key: "depth", label: "lineage depth", get: (d: R.TeachingRow) => d.lineage_depth },
          ]}
        >
          <BarChart
            rows={(teach ?? []).slice(0, 24)}
            label={(d: R.TeachingRow) => `#${d.household_id}`}
            height={190}
            series={[
              { key: "per_member", label: "teaching per member", value: (d: R.TeachingRow) => d.per_member },
              { key: "lineage_depth", label: "lineage depth", value: (d: R.TeachingRow) => d.lineage_depth },
            ]}
          />
        </Figure>
      </div>
    </>
  );
}

// ----------------------------------------------------------------- behaviour

function Behaviour() {
  const { data: roles } = useReport(api.roles, true);
  const { data: acts } = useReport(api.actionsByGeneration, true);

  const roleKinds = [...new Set((roles ?? []).map((r) => r.role))].sort();
  const byGen = [...new Set((roles ?? []).map((r) => r.generation))]
    .sort((a, b) => a - b)
    .map((g) => {
      const row: Record<string, number> & { generation: number } = { generation: g };
      for (const k of roleKinds) {
        row[k] = (roles ?? []).find((r) => r.generation === g && r.role === k)?.share ?? 0;
      }
      return row;
    });

  const topActs = [...new Set((acts ?? []).map((a) => a.kind))]
    .map((k) => ({ k, n: (acts ?? []).filter((a) => a.kind === k).reduce((s, a) => s + a.count, 0) }))
    .sort((a, b) => b.n - a.n)
    .slice(0, 7)
    .map((a) => a.k);
  const actGens = [...new Set((acts ?? []).map((a) => a.generation))]
    .sort((a, b) => a - b)
    .map((g) => {
      const row: Record<string, number> & { generation: number } = { generation: g };
      for (const k of topActs) {
        row[k] = (acts ?? []).find((a) => a.generation === g && a.kind === k)?.per_creature ?? 0;
      }
      return row;
    });

  return (
    <>
      <Figure
        title="Emergent roles by generation"
        note="Nothing in the simulation has a job title — a creature's role is whichever livelihood act it did most of, classified after the fact. If the mix shifts, the population specialised on its own."
        rows={byGen}
        series={roleKinds.map((k) => ({
          key: k, label: k, value: (d: (typeof byGen)[number]) => d[k] ?? 0, color: colorFor("role", k),
        }))}
        columns={[
          { key: "generation", label: "generation", get: (d) => `g${d.generation}` },
          ...roleKinds.map((k) => ({ key: k, label: k, get: (d: (typeof byGen)[number]) => `${((d[k] ?? 0) * 100).toFixed(0)}%` })),
        ]}
      >
        <StackedBar
          rows={byGen}
          label={(d) => `g${d.generation}`}
          height={230}
          partsOfAWhole
          series={roleKinds.map((k) => ({
            key: k, label: k, value: (d: (typeof byGen)[number]) => d[k] ?? 0, color: colorFor("role", k),
          }))}
        />
      </Figure>

      <Figure
        title="What each generation actually did"
        note="Per creature, because generation sizes differ by an order of magnitude and raw counts would only say 'generation 2 was large'."
        rows={actGens}
        series={topActs.map((k) => ({
          key: k, label: k.toLowerCase().replace(/_/g, " "),
          value: (d: (typeof actGens)[number]) => d[k] ?? 0, color: colorFor("action", k),
        }))}
      >
        <BarChart
          rows={actGens}
          label={(d) => `g${d.generation}`}
          height={230}
          series={topActs.map((k) => ({
            key: k, label: k.toLowerCase().replace(/_/g, " "),
            value: (d: (typeof actGens)[number]) => d[k] ?? 0, color: colorFor("action", k),
          }))}
        />
      </Figure>
    </>
  );
}

// -------------------------------------------------------------- deliberation

function Deliberation() {
  const { data: tiers } = useReport(api.actions, true);
  const { data: series } = useReport(() => api.deliberation(240), true);
  const { data: lat } = useReport(api.latency, true);
  const { data: stage } = useReport(api.stageCompute, true);
  const { data: elders } = useReport(api.elders, true);
  const { data: press } = useReport(api.pressure, true);
  const { data: s6 } = useReport(api.s6, true);
  const { data: fb } = useReport(api.fallbacks, true);

  return (
    <>
      <Figure
        title="What each tier chose"
        note="The single best early warning that the LLM has stopped mattering. If these two distributions converge, S6 is failing — the model is producing what the deterministic policy would have produced anyway. Grouped, never stacked: these are two separate populations of choices, not parts of one."
        rows={tiers ?? []}
        series={[
          { key: "Tier 1", label: "Tier 1 (policy)", value: (d: R.TierAction) => d.tier1, color: colorFor("tier", "Tier 1") },
          { key: "Tier 2", label: "Tier 2 (model)", value: (d: R.TierAction) => d.tier2, color: colorFor("tier", "Tier 2") },
        ]}
        columns={[
          { key: "goal", label: "goal", get: (d: R.TierAction) => d.goal.toLowerCase().replace(/_/g, " ") },
          { key: "t1", label: "Tier 1", get: (d: R.TierAction) => d.tier1 },
          { key: "t2", label: "Tier 2", get: (d: R.TierAction) => d.tier2 },
        ]}
      >
        <BarChart
          rows={tiers ?? []}
          label={(d: R.TierAction) => d.goal.toLowerCase().replace(/_/g, " ").slice(0, 9)}
          height={230}
          series={[
            { key: "Tier 1", label: "Tier 1 (policy)", value: (d: R.TierAction) => d.tier1, color: colorFor("tier", "Tier 1") },
            { key: "Tier 2", label: "Tier 2 (model)", value: (d: R.TierAction) => d.tier2, color: colorFor("tier", "Tier 2") },
          ]}
        />
      </Figure>

      <Figure
        title="Lifetime thinking against lineage depth"
        note="Direct evidence for S6: did creatures who got more deliberation found deeper bloodlines? Restricted to those who reached adulthood — otherwise this only measures that the dead do not reproduce."
        rows={s6 ?? []}
        series={[
          { key: "lineage_depth", label: "lineage depth", value: (d: R.DepthBand) => d.lineage_depth },
          { key: "living_descendants", label: "living descendants", value: (d: R.DepthBand) => d.living_descendants },
        ]}
        columns={[
          { key: "band", label: "deliberations", get: (d: R.DepthBand) => d.band },
          { key: "n", label: "creatures", get: (d: R.DepthBand) => d.creatures },
          { key: "depth", label: "mean lineage depth", get: (d: R.DepthBand) => d.lineage_depth.toFixed(2) },
          { key: "desc", label: "mean living descendants", get: (d: R.DepthBand) => d.living_descendants.toFixed(2) },
        ]}
        thin={
          (s6 ?? []).some((b) => b.creatures > 0 && b.creatures < 30)
            ? "At least one band holds fewer than 30 creatures. This is a correlation on a thin sample, not a result."
            : undefined
        }
      >
        <BarChart
          rows={s6 ?? []}
          label={(d: R.DepthBand) => d.band}
          height={200}
          series={[
            { key: "lineage_depth", label: "lineage depth", value: (d: R.DepthBand) => d.lineage_depth },
            { key: "living_descendants", label: "living descendants", value: (d: R.DepthBand) => d.living_descendants },
          ]}
        />
      </Figure>

      <Figure
        title="Fallback rate over time"
        note="Invariant 8 treats a rise here as a defect, not as weather."
        rows={series ?? []}
        series={[{ key: "fallback_rate", label: "fallback rate", value: (d: R.DeliberationPoint) => d.fallback_rate, color: "var(--st-serious)" }]}
        columns={[
          { key: "tick", label: "tick", get: (d: R.DeliberationPoint) => d.tick },
          { key: "calls", label: "calls", get: (d: R.DeliberationPoint) => d.calls },
          { key: "fallbacks", label: "fallbacks", get: (d: R.DeliberationPoint) => d.fallbacks },
          { key: "rate", label: "rate", get: (d: R.DeliberationPoint) => `${(d.fallback_rate * 100).toFixed(1)}%` },
          { key: "lat", label: "mean latency", get: (d: R.DeliberationPoint) => `${d.mean_latency_ms.toFixed(0)}ms` },
        ]}
      >
        <LineChart
          rows={series ?? []}
          x={(d: R.DeliberationPoint) => d.tick}
          xLabel="tick"
          height={180}
          series={[{ key: "fallback_rate", label: "fallback rate", value: (d: R.DeliberationPoint) => d.fallback_rate, color: "var(--st-serious)" }]}
        />
      </Figure>

      <div className="split">
        <Figure
          title="Why it fell back"
          rows={fb ?? []}
          series={[{ key: "count", label: "occurrences", value: (d: R.NamedCount) => d.count }]}
          columns={[
            { key: "name", label: "reason", get: (d: R.NamedCount) => d.name.toLowerCase().replace(/_/g, " ") },
            { key: "count", label: "occurrences", get: (d: R.NamedCount) => d.count },
          ]}
        >
          <BarChart
            rows={fb ?? []}
            label={(d: R.NamedCount) => d.name.toLowerCase().replace(/_/g, " ").slice(0, 10)}
            height={190}
            series={[{ key: "count", label: "occurrences", value: (d: R.NamedCount) => d.count }]}
          />
        </Figure>

        <Figure
          title="Latency"
          note="Percentiles, never a mean: the tail is the whole story, and an average hides the calls that came back after their creature had died."
          rows={lat ?? []}
          series={[
            { key: "p50_ms", label: "p50", value: (d: R.LatencyRow) => d.p50_ms },
            { key: "p95_ms", label: "p95", value: (d: R.LatencyRow) => d.p95_ms },
            { key: "p99_ms", label: "p99", value: (d: R.LatencyRow) => d.p99_ms },
          ]}
          columns={[
            { key: "model", label: "model", get: (d: R.LatencyRow) => d.model },
            { key: "calls", label: "calls", get: (d: R.LatencyRow) => d.calls },
            { key: "p50", label: "p50", get: (d: R.LatencyRow) => `${d.p50_ms}ms` },
            { key: "p95", label: "p95", get: (d: R.LatencyRow) => `${d.p95_ms}ms` },
            { key: "p99", label: "p99", get: (d: R.LatencyRow) => `${d.p99_ms}ms` },
            { key: "max", label: "worst", get: (d: R.LatencyRow) => `${d.max_ms}ms` },
          ]}
        >
          <BarChart
            rows={lat ?? []}
            label={(d: R.LatencyRow) => d.model.slice(0, 12)}
            height={190}
            series={[
              { key: "p50_ms", label: "p50", value: (d: R.LatencyRow) => d.p50_ms },
              { key: "p95_ms", label: "p95", value: (d: R.LatencyRow) => d.p95_ms },
              { key: "p99_ms", label: "p99", value: (d: R.LatencyRow) => d.p99_ms },
            ]}
          />
        </Figure>
      </div>

      <Figure
        title="Compute spent per life stage"
        note="§5.4 weights deliberation toward early adulthood, where the least reversible decisions are made. This is the check that the weighting is landing where it was aimed."
        rows={stage ?? []}
        series={[
          { key: "calls_per_creature", label: "calls per creature", value: (d: R.StageCompute) => d.calls_per_creature },
          { key: "mean_age_weight", label: "mean age weight", value: (d: R.StageCompute) => d.mean_age_weight },
        ]}
        columns={[
          { key: "stage", label: "life stage", get: (d: R.StageCompute) => d.life_stage.toLowerCase() },
          { key: "calls", label: "calls", get: (d: R.StageCompute) => d.calls },
          { key: "share", label: "share of calls", get: (d: R.StageCompute) => `${(d.share_of_calls * 100).toFixed(0)}%` },
          { key: "per", label: "per creature", get: (d: R.StageCompute) => d.calls_per_creature.toFixed(2) },
          { key: "weight", label: "mean age weight", get: (d: R.StageCompute) => d.mean_age_weight.toFixed(2) },
          { key: "fat", label: "think fatigue", get: (d: R.StageCompute) => d.think_fatigue.toFixed(0) },
          { key: "crisis", label: "crisis exemptions", get: (d: R.StageCompute) => d.crisis_exempt },
        ]}
      >
        <BarChart
          rows={stage ?? []}
          label={(d: R.StageCompute) => d.life_stage.toLowerCase()}
          height={190}
          series={[
            { key: "calls_per_creature", label: "calls per creature", value: (d: R.StageCompute) => d.calls_per_creature },
            { key: "mean_age_weight", label: "mean age weight", value: (d: R.StageCompute) => d.mean_age_weight },
          ]}
        />
      </Figure>

      <div className="split">
        <Figure
          title="Do elders need to think?"
          note="§13.10. Elders run on Tier 1 far more than adults do; if their plans complete at a comparable rate, the policy is already carrying them. Note that ai::budget::habit_bonus is written but never called, so there is no habit-prior path to measure a hit rate against."
          rows={elders ?? []}
          series={[
            { key: "completion_rate", label: "plans completed", value: (d: R.ElderRow) => d.completion_rate },
            { key: "call_share", label: "share from a call", value: (d: R.ElderRow) => d.call_share },
          ]}
          columns={[
            { key: "stage", label: "life stage", get: (d: R.ElderRow) => d.life_stage.toLowerCase() },
            { key: "n", label: "creatures", get: (d: R.ElderRow) => d.creatures },
            { key: "plans", label: "plans", get: (d: R.ElderRow) => d.plans },
            { key: "done", label: "completed", get: (d: R.ElderRow) => `${(d.completion_rate * 100).toFixed(0)}%` },
            { key: "call", label: "from a call", get: (d: R.ElderRow) => `${(d.call_share * 100).toFixed(0)}%` },
          ]}
        >
          <BarChart
            rows={elders ?? []}
            label={(d: R.ElderRow) => d.life_stage.toLowerCase()}
            share
            height={190}
            series={[
              { key: "completion_rate", label: "plans completed", value: (d: R.ElderRow) => d.completion_rate },
              { key: "call_share", label: "share from a call", value: (d: R.ElderRow) => d.call_share },
            ]}
          />
        </Figure>

        <Figure
          title="Who gets the attention"
          note="§13.6 worries the population splits into an observed class and a background one. That failure is a bimodal shape here long before it is visible on the map."
          rows={press ?? []}
          series={[{ key: "creatures", label: "creatures", value: (d: R.PressureBand) => d.creatures }]}
          columns={[
            { key: "band", label: "pressure", get: (d: R.PressureBand) => d.band },
            { key: "n", label: "creatures", get: (d: R.PressureBand) => d.creatures },
            { key: "rate", label: "calls / 100 ticks", get: (d: R.PressureBand) => d.calls_per_100_ticks.toFixed(2) },
          ]}
        >
          <BarChart
            rows={press ?? []}
            label={(d: R.PressureBand) => d.band}
            height={190}
            series={[{ key: "creatures", label: "creatures", value: (d: R.PressureBand) => d.creatures }]}
          />
        </Figure>
      </div>
    </>
  );
}

// ------------------------------------------------------------------ planning

function Planning() {
  const { data: horizons } = useReport(api.horizons, true);
  const { data: byGen } = useReport(api.horizonByGeneration, true);
  const { data: aborts } = useReport(api.aborts, true);
  const { data: planners } = useReport(api.planners, true);

  return (
    <>
      <Figure
        title="Committed against actual horizon"
        note="The abandonment gap. A widening gap means the model is over-committing and the prompt needs the abandonment history fed back into it (§5.5)."
        rows={horizons ?? []}
        series={[
          { key: "committed", label: "committed", value: (d: R.HorizonRow) => d.committed },
          { key: "actual", label: "actually ran", value: (d: R.HorizonRow) => d.actual },
        ]}
        columns={[
          { key: "tier", label: "tier", get: (d: R.HorizonRow) => `Tier ${d.tier}` },
          { key: "committed", label: "committed", get: (d: R.HorizonRow) => d.committed.toFixed(2) },
          { key: "actual", label: "actually ran", get: (d: R.HorizonRow) => d.actual.toFixed(2) },
          { key: "plans", label: "plans", get: (d: R.HorizonRow) => d.plans },
        ]}
      >
        <BarChart
          rows={horizons ?? []}
          label={(d: R.HorizonRow) => `Tier ${d.tier}`}
          height={190}
          series={[
            { key: "committed", label: "committed", value: (d: R.HorizonRow) => d.committed },
            { key: "actual", label: "actually ran", value: (d: R.HorizonRow) => d.actual },
          ]}
        />
      </Figure>

      <Figure
        title="Horizon by generation"
        note="Rising commitment with a stable gap would mean planning is being selected for. A widening gap means the opposite: creatures learning to over-promise."
        rows={byGen ?? []}
        series={[
          { key: "mean_committed", label: "committed", value: (d: R.HorizonByGeneration) => d.mean_committed },
          { key: "mean_actual", label: "actually ran", value: (d: R.HorizonByGeneration) => d.mean_actual },
        ]}
      >
        <LineChart
          rows={byGen ?? []}
          x={(d: R.HorizonByGeneration) => d.generation}
          xLabel="generation"
          height={190}
          series={[
            { key: "mean_committed", label: "committed", value: (d: R.HorizonByGeneration) => d.mean_committed },
            { key: "mean_actual", label: "actually ran", value: (d: R.HorizonByGeneration) => d.mean_actual },
          ]}
        />
      </Figure>

      <Figure
        title="Do planners out-survive reactors?"
        note="§5.5's direct test. If longer horizons do not buy deeper lineages, the horizon mechanic is costing complexity and returning nothing."
        rows={planners ?? []}
        series={[
          { key: "lineage_depth", label: "lineage depth", value: (d: R.DepthBand) => d.lineage_depth },
          { key: "living_descendants", label: "living descendants", value: (d: R.DepthBand) => d.living_descendants },
        ]}
        columns={[
          { key: "band", label: "mean committed horizon", get: (d: R.DepthBand) => d.band },
          { key: "n", label: "creatures", get: (d: R.DepthBand) => d.creatures },
          { key: "depth", label: "mean lineage depth", get: (d: R.DepthBand) => d.lineage_depth.toFixed(2) },
          { key: "desc", label: "mean living descendants", get: (d: R.DepthBand) => d.living_descendants.toFixed(2) },
        ]}
        thin={
          (planners ?? []).some((b) => b.creatures > 0 && b.creatures < 30)
            ? "At least one band holds fewer than 30 creatures; treat the shape as a hint, not a finding."
            : undefined
        }
      >
        <BarChart
          rows={planners ?? []}
          label={(d: R.DepthBand) => d.band}
          height={200}
          series={[
            { key: "lineage_depth", label: "lineage depth", value: (d: R.DepthBand) => d.lineage_depth },
            { key: "living_descendants", label: "living descendants", value: (d: R.DepthBand) => d.living_descendants },
          ]}
        />
      </Figure>

      <Figure
        title="Why plans ended"
        note="Distinguishes 'the world changed' from 'the plan was bad'. If the world never invalidates a plan, it is too predictable for horizon to be a real choice (§13.8)."
        rows={aborts ?? []}
        series={[{ key: "count", label: "plans", value: (d: R.NamedCount) => d.count }]}
        columns={[
          { key: "name", label: "reason", get: (d: R.NamedCount) => d.name.toLowerCase().replace(/_/g, " ") },
          { key: "count", label: "plans", get: (d: R.NamedCount) => d.count },
        ]}
      >
        <BarChart
          rows={aborts ?? []}
          label={(d: R.NamedCount) => d.name.toLowerCase().replace(/_/g, " ").slice(0, 10)}
          height={200}
          series={[{ key: "count", label: "plans", value: (d: R.NamedCount) => d.count }]}
        />
      </Figure>
    </>
  );
}
