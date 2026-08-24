//! The deliberation scheduler (PRD §5.3, §5.4, §5.5).
//!
//! Thinking is scarce twice over, and the two scarcities are deliberately the
//! same one seen from opposite sides.
//!
//! From the engine's side there is a per-tick budget, because a call costs
//! seconds of wall clock. From the creature's side thinking costs fatigue and
//! hunger, because a brain is expensive. §5.5 calls this the best structural
//! idea in the design and it is right: the engine's constraint stops being an
//! external scheduling hack and becomes something the creature has its own
//! reason to obey. Creature-side demand falls, so the engine-side cap binds
//! less often, so the creatures that genuinely need to think are likelier to be
//! served. The two mechanisms compose instead of fighting.
//!
//! Both have the same escape hatch. A starving creature can least afford to
//! think exactly when it most needs to, and that spiral must not be absorbing —
//! so a critical need buys one heavily discounted shallow deliberation, and
//! urgency largely bypasses the age weight. Panic overrides economy.

use crate::ai::ollama::Depth;
use crate::config::WorldConfig;
use crate::sim::creature::{Addresses, Creature, LifeStage};

/// Why a creature wants to think, split so the age weight can apply to some of
/// it and not the rest.
#[derive(Debug, Clone, Copy, Default)]
pub struct Pressure {
    /// A need has crossed into deficit, or health is falling. Only weakly
    /// weighted by age: otherwise a starving elder would rationally deliberate
    /// its way to death, which is both bad simulation and bad drama.
    pub urgency: f32,
    /// Novelty, staleness, social significance, narrative weight. Fully
    /// weighted by age — this is where §5.4's saving actually comes from.
    pub discretionary: f32,
    pub age_weight: f32,
    /// True when a need is past critical: buys a discounted call regardless of
    /// what the budget would otherwise allow.
    pub crisis: bool,
}

impl Pressure {
    /// What the scheduler ranks on.
    pub fn total(&self) -> f32 {
        // Urgency keeps most of its force at any age; everything else is
        // concentrated on the creatures whose choices move the simulation.
        self.urgency * (0.75 + 0.25 * self.age_weight) + self.discretionary * self.age_weight
    }
}

/// §5.4's frequency curve: peaking in early adulthood and tapering at both ends.
///
/// The peak is deliberately at 220–380 rather than mid-life. The
/// least-reversible decisions a creature makes — leave the household or stay,
/// farm or forage, court whom — all cluster in the first third of adult life,
/// and that is where thinking buys the most. A mature adult executing a plan it
/// settled on two hundred ticks ago needs far less thought per tick.
pub fn age_weight(c: &Creature, tick: i64, cfg: &WorldConfig) -> f32 {
    let d = &cfg.deliberation;
    if !cfg.features.age_weighting {
        return 1.0;
    }
    let age = c.age(tick) as f32;
    let infant_until = cfg.lifespan.infant_until_tick as f32;
    let elder_from = cfg.lifespan.elder_from_tick as f32;

    // The stage boundaries scale with infancy, so turning §13.1's first dial
    // does not silently leave the deliberation curve describing a different
    // creature than the one being simulated.
    let emerging_end = infant_until + 52.0;
    let prime_end = infant_until + 212.0;

    if age < infant_until {
        d.age_weight_infant
    } else if age < emerging_end {
        // Rising fast: this is when it leaves home, picks a livelihood, courts.
        let t = (age - infant_until) / (emerging_end - infant_until);
        d.age_weight_emerging + (d.age_weight_prime - d.age_weight_emerging) * t
    } else if age < prime_end {
        d.age_weight_prime
    } else if age < elder_from {
        let t = (age - prime_end) / (elder_from - prime_end).max(1.0);
        d.age_weight_prime + (d.age_weight_mature - d.age_weight_prime) * t
    } else {
        d.age_weight_elder
    }
}

/// How much this creature wants to think right now.
#[allow(clippy::too_many_arguments)]
pub fn pressure_for(
    c: &Creature,
    tick: i64,
    cfg: &WorldConfig,
    has_plan: bool,
    plan_just_ended: bool,
    belief_was_wrong: bool,
    social_event: bool,
    under_inspection: bool,
) -> Pressure {
    let n = &cfg.needs;
    let mut urgency = 0.0;
    let mut crisis = false;

    for v in [c.hunger, c.thirst, c.warmth] {
        if v < n.critical_threshold {
            crisis = true;
            urgency += 1.4;
        } else if v < n.deficit_threshold {
            urgency += 0.5 * (n.deficit_threshold - v) / n.deficit_threshold;
        }
    }
    if c.health < 45.0 {
        urgency += 0.4;
    }

    let mut discretionary = 0.0;
    // Intent completion: the plan is finished, impossible, or gone.
    if !has_plan {
        discretionary += 1.0;
    }
    if plan_just_ended {
        discretionary += 0.5;
    }
    // Novelty: the world turned out not to be what the creature believed.
    if belief_was_wrong {
        discretionary += 0.8;
    }
    // Social significance: an offer, a birth, a death in the household.
    if social_event {
        discretionary += 0.7;
    }
    // Staleness. Carries over and compounds, so a creature that loses the
    // budget race repeatedly eventually wins it and nobody is starved of
    // deliberation indefinitely (§5.3).
    let since = c.last_deliberation_tick.map(|t| (tick - t).max(0)).unwrap_or(600);
    discretionary += (since as f32 / 120.0).min(2.0);
    // Narrative weight: founders, and whoever the player is looking at.
    if c.generation == 1 {
        discretionary += 0.15;
    }
    if under_inspection {
        discretionary += 1.5;
    }

    Pressure {
        urgency,
        discretionary,
        age_weight: age_weight(c, tick, cfg),
        crisis,
    }
}

/// §5.4's second knob: how hard to think, which is a real wall-clock lever on a
/// reasoning model and so compounds with the frequency saving.
pub fn depth_for(c: &Creature, tick: i64, cfg: &WorldConfig, crisis: bool) -> Depth {
    // Panic is shallow by construction: a creature about to die does not get a
    // long reflective pass, and could not afford one if it did (§5.5).
    if crisis {
        return Depth::Shallow;
    }
    if !cfg.features.age_weighting {
        return Depth::Standard;
    }
    match c.life_stage {
        LifeStage::Infant => Depth::Shallow,
        // Elders draw on habit rather than reasoning from scratch, so their
        // calls are cheap in both senses.
        LifeStage::Elder => Depth::Shallow,
        LifeStage::Adult => {
            if age_weight(c, tick, cfg) >= cfg.deliberation.age_weight_prime - 0.01 {
                Depth::Deep
            } else {
                Depth::Standard
            }
        }
    }
}

/// What a deliberation costs the creature (§5.5).
///
/// Flat per deliberation, never per tick planned. That is the whole incentive:
/// a creature that commits twenty ticks pays once and amortises, one that
/// re-thinks every tick pays twenty times and exhausts itself. Planning ahead
/// is rewarded without any rule saying "plan ahead".
pub fn cost_of(depth: Depth, c: &Creature, crisis: bool, cfg: &WorldConfig) -> (f32, f32) {
    let d = &cfg.deliberation;
    if !cfg.features.thinking_cost {
        return (0.0, 0.0);
    }
    let (mut fatigue, mut hunger) = match depth {
        Depth::Shallow => (d.fatigue_cost_shallow, d.hunger_cost_shallow),
        Depth::Standard => (d.fatigue_cost_standard, d.hunger_cost_standard),
        Depth::Deep => (d.fatigue_cost_deep, d.hunger_cost_deep),
    };
    // An elder is not reasoning from scratch; experience should read as
    // efficiency rather than only as diminished capacity, or elders get hit
    // twice — down-weighted by the scheduler *and* too tired to think.
    if c.life_stage == LifeStage::Elder && cfg.features.elder_habit_prior {
        fatigue *= d.elder_cost_discount;
        hunger *= d.elder_cost_discount;
    }
    if crisis {
        fatigue *= d.crisis_exemption_discount;
        hunger *= d.crisis_exemption_discount;
    }
    (fatigue, hunger)
}

/// Whether a creature can currently afford to think at all.
///
/// The crisis exemption is what stops this being an absorbing state: hungry →
/// cannot afford to deliberate → poor choices → hungrier is realistic and
/// dramatically good, but it must have a floor.
pub fn can_afford(c: &Creature, depth: Depth, crisis: bool, cfg: &WorldConfig) -> bool {
    if crisis {
        return true;
    }
    let (fatigue, _) = cost_of(depth, c, crisis, cfg);
    c.fatigue > fatigue + 4.0
}

/// Roughly how many ticks this creature has before something kills it.
///
/// Two clocks, whichever runs out first: the need that empties soonest, and old
/// age. Leaving age out was not a small omission — most deaths in a settled
/// population are old age, so a scheduler that only watched needs kept asking
/// the model about creatures who were about to die of nothing in particular.
///
/// Health is not in it, because a creature whose needs are met recovers.
pub fn ticks_of_life_left(c: &Creature, tick: i64, cfg: &WorldConfig) -> f32 {
    let n = &cfg.needs;
    let hunger = c.hunger / n.hunger_decay_per_tick.max(0.001);
    let thirst = c.thirst / n.thirst_decay_per_tick.max(0.001);
    // Warmth only falls at night, so it is not a reliable clock by day.
    let needs = hunger.min(thirst);
    let of_age = (c.lifespan_ticks - c.biological_age(tick)).max(0.0);
    needs.min(of_age)
}

/// Whether it is worth asking the model about this creature at all.
///
/// **This is where the asynchronous dispatch of `ai::ollama` shows its price.**
/// §5.5's crisis exemption exists so a starving creature can still think —
/// and it assumes the answer arrives now. On hardware where a call takes
/// seconds, the answer arrives dozens of ticks later, by which time the most
/// desperate creatures are dead. Measured on a live run before this check
/// existed: 39 of 47 model calls came back for a creature that had already
/// died, an 83% waste rate, precisely *because* pressure ranks the desperate
/// first.
///
/// So a creature that will not outlive the round trip is not asked. It is not
/// abandoned — it gets a Tier 1 plan in the same tick, which is instant and
/// competent, and which is the whole reason Tier 1 exists (§5.2). The model's
/// scarce attention goes to creatures who will still be there, and still care,
/// when it answers.
pub fn worth_asking(c: &Creature, tick: i64, cfg: &WorldConfig, latency_ticks: f32) -> bool {
    // A generous margin: the plan has to be worth adopting when it lands, not
    // merely land before the funeral.
    ticks_of_life_left(c, tick, cfg) > latency_ticks * 1.5
}

/// The per-tick budget for a speed mode (§5.6).
pub fn budget_for(mode: crate::sim::runner::SpeedMode, cfg: &WorldConfig) -> u32 {
    use crate::sim::runner::SpeedMode as M;
    let d = &cfg.deliberation;
    match mode {
        M::Deep => d.budget_deep,
        M::Observe => d.budget_observe,
        M::FastForward => d.budget_fast_forward,
        M::Focus => d.budget_focus,
    }
}

/// A creature's habit prior: what has actually worked for it before (§5.4).
///
/// Elders fall back on habit rather than on the generic utility policy —
/// crystallised experience rather than fresh reasoning. Mechanically it is
/// nearly free, thematically it is the right model of ageing, and it is what
/// makes an elder valuable to a household instead of a drain, which matters for
/// whether multi-generational households are worth forming at all.
pub const HABITS: usize = 8;

pub fn habit_index(a: Addresses) -> usize {
    match a {
        Addresses::Food => 0,
        Addresses::Water => 1,
        Addresses::Warmth => 2,
        Addresses::Rest => 3,
        Addresses::Fuel => 4,
        Addresses::Knowledge => 5,
        Addresses::Kinship => 6,
        Addresses::Nothing => 7,
    }
}

/// How much this creature's history favours a given kind of plan, as a
/// multiplier around 1.0. Saturating, so a long life does not turn into a
/// single obsession.
pub fn habit_bonus(c: &Creature, a: Addresses, cfg: &WorldConfig) -> f32 {
    if !cfg.features.elder_habit_prior || c.life_stage != LifeStage::Elder {
        return 1.0;
    }
    let total: u32 = c.habit.iter().map(|v| *v as u32).sum();
    if total < 6 {
        return 1.0;
    }
    let mine = c.habit[habit_index(a)] as f32;
    let share = mine / total as f32;
    // A plan of a kind that has worked before is worth up to 40% more to an
    // elder; one that never has is worth a little less.
    0.85 + share * 1.65
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::creature::testing::test_creature;

    fn cfg() -> WorldConfig {
        WorldConfig::default()
    }

    fn at_age(age: i64) -> Creature {
        let mut c = test_creature();
        c.birth_tick = 0;
        c.life_stage = LifeStage::of(age, &cfg().lifespan);
        c
    }

    #[test]
    fn thinking_peaks_in_early_adulthood_and_tapers_at_both_ends() {
        let c = cfg();
        let w = |age: i64| age_weight(&at_age(age), age, &c);

        assert!(w(50) < 0.1, "infants effectively never deliberate");
        assert!(w(180) < w(230), "the ramp rises through emerging adulthood");
        assert!((w(300) - c.deliberation.age_weight_prime).abs() < 0.01, "prime is the peak");
        assert!(w(500) < w(300), "mature adults re-think less often");
        assert!(w(620) < w(500), "and elders less again");
        assert!(w(620) > 0.2, "but not to nothing — an old creature still thinks");
    }

    #[test]
    fn the_age_curve_follows_the_infancy_dial() {
        // §13.1 names infant duration as the first balance dial. If the
        // deliberation curve does not move with it, turning it leaves the
        // scheduler describing a creature that no longer exists.
        let mut short = cfg();
        short.lifespan.infant_until_tick = 96;
        let c = at_age(120);
        assert!(
            age_weight(&c, 120, &short) > age_weight(&c, 120, &cfg()),
            "a creature that is already an adult should be thinking like one"
        );
    }

    #[test]
    fn urgency_largely_bypasses_the_age_weight() {
        // §5.4: otherwise a starving elder would rationally deliberate its way
        // to death, which is both bad simulation and bad drama.
        let c = cfg();
        let mut elder = at_age(620);
        elder.hunger = 2.0;
        let p = pressure_for(&elder, 620, &c, true, false, false, false, false);

        assert!(p.crisis);
        assert!(p.age_weight < 0.4, "the elder is heavily down-weighted");
        assert!(
            p.total() > p.discretionary * p.age_weight + 1.0,
            "yet its crisis still carries real weight"
        );
    }

    #[test]
    fn a_creature_without_a_plan_wants_to_think_more_than_one_with_one() {
        let c = cfg();
        let adult = at_age(300);
        let idle = pressure_for(&adult, 300, &c, false, true, false, false, false);
        let busy = pressure_for(&adult, 300, &c, true, false, false, false, false);
        assert!(idle.total() > busy.total());
    }

    #[test]
    fn staleness_compounds_so_nobody_is_starved_of_deliberation() {
        // §5.3: a creature that loses the budget race repeatedly eventually
        // wins it.
        let c = cfg();
        let mut recent = at_age(300);
        recent.last_deliberation_tick = Some(295);
        let mut neglected = at_age(300);
        neglected.last_deliberation_tick = Some(60);

        let a = pressure_for(&recent, 300, &c, true, false, false, false, false);
        let b = pressure_for(&neglected, 300, &c, true, false, false, false, false);
        assert!(b.total() > a.total());
    }

    #[test]
    fn being_looked_at_is_worth_thinking_about() {
        // §5.3's narrative weight: the player is watching this one.
        let c = cfg();
        let adult = at_age(300);
        let watched = pressure_for(&adult, 300, &c, true, false, false, false, true);
        let ignored = pressure_for(&adult, 300, &c, true, false, false, false, false);
        assert!(watched.total() > ignored.total());
    }

    #[test]
    fn thinking_costs_more_the_deeper_it_goes_and_less_when_old() {
        let c = cfg();
        let adult = at_age(300);
        let elder = at_age(620);

        let (shallow, _) = cost_of(Depth::Shallow, &adult, false, &c);
        let (deep, _) = cost_of(Depth::Deep, &adult, false, &c);
        assert!(deep > shallow);

        let (elder_cost, _) = cost_of(Depth::Standard, &elder, false, &c);
        let (adult_cost, _) = cost_of(Depth::Standard, &adult, false, &c);
        assert!(elder_cost < adult_cost, "experience reads as efficiency");
    }

    #[test]
    fn panic_is_cheap_and_shallow_and_always_affordable() {
        // §5.5's crisis exemption. The spiral is realistic; it must not be
        // absorbing.
        let c = cfg();
        let mut spent = at_age(300);
        spent.fatigue = 0.5;
        spent.hunger = 1.0;

        assert!(!can_afford(&spent, Depth::Standard, false, &c), "too tired to think");
        assert!(can_afford(&spent, Depth::Standard, true, &c), "but panic always can");
        assert_eq!(depth_for(&spent, 300, &c, true), Depth::Shallow);

        let (panic_cost, _) = cost_of(Depth::Shallow, &spent, true, &c);
        let (calm_cost, _) = cost_of(Depth::Shallow, &spent, false, &c);
        assert!(panic_cost < calm_cost);
    }

    #[test]
    fn a_creature_that_will_not_outlive_the_round_trip_is_not_asked() {
        // The cost of dispatching rather than awaiting: an answer that arrives
        // after the funeral is a wasted call, and pressure ranks the dying
        // first, so without this the budget is spent almost entirely on them.
        let c = cfg();
        let mut dying = at_age(300);
        dying.hunger = 3.0;
        dying.thirst = 3.0;
        let mut healthy = at_age(300);
        healthy.hunger = 90.0;
        healthy.thirst = 90.0;

        assert!(!worth_asking(&dying, 300, &c, 30.0), "it will be dead before the answer");
        assert!(worth_asking(&healthy, 300, &c, 30.0));
        // And on a machine where the answer is immediate, the dying are asked.
        assert!(worth_asking(&dying, 300, &c, 0.5));

        // Old age is the other clock, and in a settled population it is the one
        // that usually runs out first.
        let mut ancient = at_age(300);
        ancient.hunger = 95.0;
        ancient.thirst = 95.0;
        ancient.lifespan_ticks = 310.0;
        assert!(
            !worth_asking(&ancient, 300, &c, 30.0),
            "ten ticks from the end of its life is not worth a call"
        );
    }

    #[test]
    fn turning_the_thinking_cost_off_makes_it_free() {
        // The S6 toggles have to actually toggle (§11).
        let mut c = cfg();
        c.features.thinking_cost = false;
        assert_eq!(cost_of(Depth::Deep, &at_age(300), false, &c), (0.0, 0.0));
    }

    #[test]
    fn an_elder_leans_on_what_has_worked_for_it_before() {
        let c = cfg();
        let mut elder = at_age(620);
        elder.habit[habit_index(Addresses::Food)] = 20;
        elder.habit[habit_index(Addresses::Knowledge)] = 1;

        let food = habit_bonus(&elder, Addresses::Food, &c);
        let knowledge = habit_bonus(&elder, Addresses::Knowledge, &c);
        assert!(food > 1.0 && knowledge < 1.0, "food {food}, knowledge {knowledge}");

        let mut adult = at_age(300);
        adult.habit = elder.habit;
        assert_eq!(
            habit_bonus(&adult, Addresses::Food, &c),
            1.0,
            "only elders fall back on habit"
        );
    }

    #[test]
    fn a_young_elder_with_no_history_is_not_biased_by_noise() {
        let c = cfg();
        let mut elder = at_age(620);
        elder.habit[habit_index(Addresses::Water)] = 2;
        assert_eq!(habit_bonus(&elder, Addresses::Water, &c), 1.0);
    }
}
