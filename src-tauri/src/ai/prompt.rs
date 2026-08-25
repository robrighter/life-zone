//! Prompt assembly and the legal-action menu (PRD §5.7).
//!
//! The model never sees 262,144 tiles. It sees who it is, how it feels, a
//! 15×15 window, what it believes *with the provenance in plain language*, who
//! is standing nearby, how the household is doing, and a numbered list of the
//! things it could legally do right now.
//!
//! Two properties are load-bearing.
//!
//! **The menu is pre-validated.** Every option was checked against the same
//! preconditions Tier 1 checks, so an impossible action is never offered and
//! therefore cannot be hallucinated (invariant 3). This is also most of why the
//! prompt is small: the model is never taught the world's rules, only shown
//! what is currently true.
//!
//! **The static half comes first.** The system message is byte-identical on
//! every call in a run, so Ollama's prefix cache covers it; only the creature's
//! own state has to be evaluated. Measured at M0 on this hardware: 3.82s → 0.58s
//! of prompt evaluation. On a CPU-only host that is the difference between a
//! watchable Observe mode and an unusable one.

use crate::ai::ollama::Prompt;
use crate::ai::policy::PolicyCtx;
use crate::ai::schema::{ActionMenu, MenuOption};
use crate::sim::actions::{self, Goal, SocialView, Target};
use crate::sim::creature::{Addresses, Creature, ItemKind, LifeStage};
use crate::sim::economy;
use crate::sim::knowledge::{self, BeliefKind, NeedProfile};
use crate::sim::social::Bystander;
use crate::sim::terrain::Terrain;
use crate::sim::world::NodeKind;

/// Options offered per deliberation. Enough for a real choice, few enough that
/// a 1.7B model can hold them all in view at once.
const MAX_OPTIONS: usize = 14;

/// The rules half of the prompt. Identical on every call, by construction —
/// nothing creature-specific may appear here or the prefix cache stops working.
pub fn system_prompt(model_estimates_horizon: bool) -> String {
    let horizon_rule = if model_estimates_horizon {
        "\"horizon\": how many ticks you commit to this plan before thinking again. \
         Committing longer is cheaper — thinking costs you energy — but a long plan \
         made on stale information often turns out to be wrong."
    } else {
        "\"commitment\": how sure you are of this plan — \"brief\", \"moderate\" or \
         \"committed\". Say \"committed\" when your information is fresh and firsthand; \
         say \"brief\" when you are acting on something old or something you were told."
    };

    format!(
        "You are a single creature in a harsh world, deciding what to do next. \
         You are not an assistant. Answer only as this creature would act.\n\
         \n\
         HOW THE WORLD WORKS\n\
         - A tick is one hour. Night runs 20:00-06:00 and is cold; you need a roof \
           or a lit fire or you lose warmth and, over time, years off your life.\n\
         - Hunger, thirst, warmth and rest all fall on their own. Health erodes \
           while any of them is empty.\n\
         - These are four separate things and each has its own remedy. Resting \
           cures tiredness and nothing else: it does not feed you, it does not \
           give you water, and a creature that rests while starving starves \
           while rested. Eat for hunger. Drink for thirst. A roof or a fire \
           for warmth. Rest for tiredness.\n\
         - Berries keep about two days. Meat keeps about four. Only grain keeps \
           indefinitely, so only grain accumulates into a household store.\n\
         - You cannot have a child until your household has a shelter and a store \
           above its reserve. In practice that means grain, which means farming or \
           harvesting a field.\n\
         - Wood is both timber and fuel. Carrying firewood is what lets you spend a \
           night away from home.\n\
         - You cannot see the whole map. What you know is listed for you, along with \
           how you came to know it. Things you were told may be out of date.\n\
         - Everything you know dies with you unless you tell someone. Your \
           children start knowing nothing: every river and every field you \
           found, they will have to find again from nothing, and some of them \
           will not survive doing it. Teaching is the only way anything you \
           learned outlives you.\n\
         \n\
         HOW TO ANSWER\n\
         Choose from the numbered options you are given. They are the only things \
         you can currently do; anything not listed is impossible right now.\n\
         Reply with JSON only:\n\
         {{\"steps\": [{{\"option\": 3}}, {{\"option\": 7}}], {}, \
         \"rationale\": \"one short sentence, in your own voice\"}}\n\
         \n\
         One to four steps. They run in order; if a later one becomes impossible \
         the whole plan is abandoned and you think again. A plan of several steps \
         costs the same as a plan of one, so string together what obviously belongs \
         together — walk somewhere, do the thing, come back.\n\
         {}",
        if model_estimates_horizon { "\"horizon\": 12" } else { "\"commitment\": \"moderate\"" },
        horizon_rule,
    )
}

/// Enumerate what this creature could legally do right now.
///
/// Deliberately *not* the same as scoring them: Tier 1's job is to pick, and
/// this one's job is to offer. Options are gathered per need so the menu always
/// spans the real choice — a menu of six ways to fetch water and no way to eat
/// is not a decision.
pub fn build_menu(c: &Creature, ctx: &PolicyCtx) -> ActionMenu {
    let mut options: Vec<MenuOption> = Vec::new();
    let cfg = ctx.cfg;
    let k = &ctx.cfg.knowledge;
    let needs = NeedProfile { food: 1.0, water: 1.0, fuel: 1.0, shelter: 1.0 };

    // Free functions rather than closures: an option is built whole and then
    // added, so nothing needs to reach back into the list it is being added to.
    fn add(
        options: &mut Vec<MenuOption>,
        steps: Vec<(Goal, Target, u32)>,
        addresses: Addresses,
        label: String,
    ) {
        if options.len() >= MAX_OPTIONS || steps.is_empty() {
            return;
        }
        let est: u32 = steps.iter().map(|(_, _, t)| *t).sum::<u32>().max(1);
        let (goal, target, _) = steps[0];
        options.push(MenuOption {
            id: options.len() as u32 + 1,
            goal,
            target,
            steps,
            label,
            addresses,
            est_ticks: est,
        });
    }
    fn one(
        options: &mut Vec<MenuOption>,
        goal: Goal,
        target: Target,
        addresses: Addresses,
        est: u32,
        label: String,
    ) {
        add(options, vec![(goal, target, est.max(1))], addresses, label);
    }

    let travel = |to: (u32, u32)| -> u32 {
        let dx = c.x.abs_diff(to.0) as f32;
        let dy = c.y.abs_diff(to.1) as f32;
        let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
        (((hi - lo) + lo * std::f32::consts::SQRT_2) * 1.25).ceil().max(1.0) as u32
    };

    // ---- water -------------------------------------------------------------
    if ctx.world.at(c.x, c.y).is_fresh_water() {
        one(&mut options, Goal::Drink, Target::Tile(c.x, c.y), Addresses::Water, 1,
            "drink, you are standing in the shallows".into());
    }
    for i in knowledge::rank(&c.beliefs, (c.x, c.y), ctx.tick, &needs, k, 40)
        .into_iter()
        .filter(|i| c.beliefs[*i].kind == BeliefKind::Water)
        .take(1)
    {
        let b = &c.beliefs[i];
        let t = travel((b.x, b.y));
        add(
            &mut options,
            vec![
                (Goal::MoveTo, Target::Tile(b.x, b.y), t),
                (Goal::Drink, Target::Tile(b.x, b.y), 1),
            ],
            Addresses::Water,
            format!("go to the water at {},{} and drink — {} ({} steps away)",
                    b.x, b.y, b.provenance(ctx.tick), t),
        );
    }

    // ---- food --------------------------------------------------------------
    if c.inventory.total_food() > 0.0 {
        let what = c.inventory.oldest_food().map(|b| b.kind).unwrap_or(ItemKind::Forage);
        one(&mut options, Goal::EatFromInventory, Target::None, Addresses::Food, 1,
            format!("eat the {} you are carrying", what.as_str().to_lowercase()));
    }
    for kind in [BeliefKind::ForageNode, BeliefKind::SoilPatch, BeliefKind::SheepFlock] {
        let node_kind = match kind {
            BeliefKind::ForageNode => NodeKind::Forage,
            BeliefKind::SoilPatch => NodeKind::Wheat,
            _ => NodeKind::Sheep,
        };
        if node_kind == NodeKind::Wheat && !ctx.cfg.features.wheat {
            continue;
        }
        if node_kind == NodeKind::Sheep && !ctx.cfg.features.sheep {
            continue;
        }
        let found = c
            .beliefs
            .iter()
            .enumerate()
            .filter(|(_, b)| b.kind == kind)
            .filter_map(|(i, b)| {
                ctx.nodes.find_at(ctx.world, node_kind, b.x, b.y).map(|nd| (i, nd))
            })
            .max_by(|a, bb| {
                let q = |i: usize| {
                    knowledge::target_quality(&c.beliefs[i], (c.x, c.y), ctx.tick, k)
                };
                q(a.0).total_cmp(&q(bb.0)).then(bb.0.cmp(&a.0))
            });
        if let Some((i, node)) = found {
            let b = &c.beliefs[i];
            let t = travel((b.x, b.y));
            let (goal, verb) = match node_kind {
                NodeKind::Forage => (Goal::GatherForage, "pick berries"),
                NodeKind::Wheat => (Goal::HarvestWheat, "harvest grain"),
                _ => (Goal::SlaughterSheep, "kill a sheep"),
            };
            if (c.x, c.y) == (b.x, b.y) {
                one(&mut options, goal, Target::Node(node), Addresses::Food, 10,
                    format!("{verb} here — looks {}", b.estimate.as_str()));
            } else {
                add(
                    &mut options,
                    vec![
                        (Goal::MoveTo, Target::Tile(b.x, b.y), t),
                        (goal, Target::Node(node), 10),
                    ],
                    Addresses::Food,
                    format!("go to {verb} at {},{} — looks {}, {} ({} steps)",
                            b.x, b.y, b.estimate.as_str(), b.provenance(ctx.tick), t),
                );
            }
        }
    }

    // ---- household ---------------------------------------------------------
    if let Some((hid, hx, hy)) = ctx.hearth_of(c) {
        let at_home = c.x.abs_diff(hx) <= 1 && c.y.abs_diff(hy) <= 1;
        let stored = ctx.households.get(hid).map(|h| h.stored_food()).unwrap_or(0.0).max(0.0);
        if at_home {
            if c.inventory.weight() > 1.0 {
                one(&mut options, Goal::DepositToStore, Target::None, Addresses::Food, 1,
                    format!("put what you are carrying into the household store \
                             (it holds {stored:.0} of the {:.0} you need)",
                            ctx.cfg.reproduction.store_reserve));
            }
            if stored > 0.0 {
                one(&mut options, Goal::EatFromStore, Target::None, Addresses::Food, 1,
                    "eat from the household store".into());
            }
        } else {
            let t = travel((hx, hy));
            add(
                &mut options,
                vec![
                    (Goal::MoveTo, Target::Tile(hx, hy), t),
                    (Goal::DepositToStore, Target::None, 1),
                ],
                Addresses::Food,
                format!("go home to {hx},{hy} and put what you carry into the store \
                         ({t} steps; it holds {stored:.0} of the {:.0} you need)",
                        ctx.cfg.reproduction.store_reserve),
            );
        }
    }

    // ---- warmth and shelter -------------------------------------------------
    let wood = c.inventory.total(ItemKind::Wood);
    if ctx.cfg.features.fires && wood >= ctx.cfg.actions.fire_wood_cost {
        one(&mut options, Goal::BuildFire, Target::None, Addresses::Warmth, 1,
            format!("light a fire here, burning {:.0} of your {wood:.0} wood",
                    ctx.cfg.actions.fire_wood_cost));
    }
    if let Some(s) = ctx.structures.nearest_shelter(c.x, c.y, 40, c.household_id) {
        let t = travel((s.x, s.y));
        if t <= 1 {
            add(
                &mut options,
                vec![
                    (Goal::Shelter, Target::Structure(s.id), 1),
                    (Goal::Rest, Target::None, 8),
                ],
                Addresses::Warmth,
                "go inside the shelter you are standing at and sleep".into(),
            );
        } else {
            add(
                &mut options,
                vec![
                    (Goal::MoveTo, Target::Tile(s.x, s.y), t),
                    (Goal::Shelter, Target::Structure(s.id), 1),
                    (Goal::Rest, Target::None, 8),
                ],
                Addresses::Warmth,
                format!("go to the shelter at {},{} and sleep there ({t} steps)", s.x, s.y),
            );
        }
    }
    if wood >= ctx.cfg.actions.shelter_wood_cost
        && ctx.hearth_of(c).is_none()
        && !ctx.world.at(c.x, c.y).is_water()
    {
        one(&mut options, Goal::BuildShelter, Target::Tile(c.x, c.y), Addresses::Warmth,
            ctx.cfg.actions.shelter_build_ticks,
            "build a shelter of your own here and found a household".into());
    }
    one(&mut options, Goal::Rest, Target::None, Addresses::Rest, 8, "rest".into());

    // ---- wood --------------------------------------------------------------
    if let Some((i, node)) = c
        .beliefs
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == BeliefKind::WoodNode)
        .filter_map(|(i, b)| ctx.nodes.find_at(ctx.world, NodeKind::Wood, b.x, b.y).map(|n| (i, n)))
        .max_by(|a, bb| {
            let q = |i: usize| knowledge::target_quality(&c.beliefs[i], (c.x, c.y), ctx.tick, k);
            q(a.0).total_cmp(&q(bb.0)).then(bb.0.cmp(&a.0))
        })
    {
        let b = &c.beliefs[i];
        if (c.x, c.y) == (b.x, b.y) {
            one(&mut options, Goal::ChopWood, Target::Node(node), Addresses::Fuel, 10,
                "chop wood here".into());
        } else {
            let t = travel((b.x, b.y));
            add(
                &mut options,
                vec![
                    (Goal::MoveTo, Target::Tile(b.x, b.y), t),
                    (Goal::ChopWood, Target::Node(node), 10),
                ],
                Addresses::Fuel,
                format!("go to the trees at {},{} and chop ({t} steps) — {}",
                        b.x, b.y, b.provenance(ctx.tick)),
            );
        }
    }

    // ---- farming -----------------------------------------------------------
    if ctx.cfg.features.wheat
        && ctx.world.at(c.x, c.y) == Terrain::Soil
        && ctx.nodes.find_at(ctx.world, NodeKind::Wheat, c.x, c.y).is_none()
    {
        one(&mut options, Goal::PlantWheat, Target::Tile(c.x, c.y), Addresses::Food,
            ctx.cfg.actions.plant_ticks,
            "plant wheat here — nothing to eat for three days, then grain, \
             which is the only food that keeps".into());
    }

    // ---- other people -------------------------------------------------------
    let mut near: Vec<Bystander> = Vec::new();
    ctx.people.near(c.x, c.y, ctx.cfg.actions.social_reach.max(2), c.id, &mut near);

    if let Some(o) = ctx.courtships.pending_for(c.id) {
        if c.mate_id.is_none() && near.iter().any(|p| p.id == o.from) {
            one(&mut options, Goal::AcceptCourtship, Target::Creature(o.from),
                Addresses::Kinship, 1,
                format!("accept #{}'s offer and pair with them", o.from));
            one(&mut options, Goal::RejectCourtship, Target::Creature(o.from),
                Addresses::Kinship, 1, format!("turn #{} down", o.from));
        }
    }
    if c.is_courtable(&ctx.cfg.lifespan, ctx.tick) {
        if let Some(p) = near.iter().find(|p| {
            p.sex != c.sex && !p.paired && p.life_stage == LifeStage::Adult && p.health > 45.0
        }) {
            one(&mut options, Goal::Court, Target::Creature(p.id), Addresses::Kinship, 1,
                format!("ask #{} to pair with you", p.id));
        }
    }
    if c.inventory.total_food() > 0.0 {
        if let Some(inf) = near.iter().find(|p| p.life_stage == LifeStage::Infant) {
            one(&mut options, Goal::FeedInfant, Target::Creature(inf.id),
                Addresses::Kinship, 1,
                format!("feed the infant #{} — it cannot feed itself", inf.id));
        }
        if let Some(p) = near.iter().find(|p| p.life_stage != LifeStage::Infant && p.hunger < 50.0)
        {
            one(&mut options, Goal::GiveFood, Target::Creature(p.id), Addresses::Kinship, 1,
                format!("give food to #{}, who is hungry", p.id));
        }
    }
    if ctx.cfg.features.knowledge_sharing && !c.beliefs.is_empty() {
        let mine = knowledge::known_kinds(&c.beliefs, ctx.tick, k);
        if let Some((p, topic)) = near
            .iter()
            .find_map(|p| knowledge::topic_for(mine, p.known_kinds).map(|t| (p, t)))
        {
            one(&mut options, Goal::ShareKnowledge, Target::Creature(p.id),
                Addresses::Knowledge, 1,
                format!("tell #{} where to find {}",
                        p.id, topic.as_str().replace('_', " ").to_lowercase()));
        }
    }
    if ctx.cfg.features.teaching && c.household_id.is_some() && c.beliefs.len() >= 4 {
        if let Some(pupil) = near.iter().find(|p| {
            p.household_id == c.household_id && p.life_stage != LifeStage::Elder && p.id != c.id
        }) {
            one(&mut options, Goal::Teach, Target::Creature(pupil.id), Addresses::Knowledge,
                ctx.cfg.knowledge.teach_ticks,
                format!("spend {} ticks teaching #{} everything you know — they will \
                         still know it after you are gone",
                        ctx.cfg.knowledge.teach_ticks, pupil.id));
        }
    }

    // ---- looking around ------------------------------------------------------
    let (ex, ey) = ctx.explore_target_for(c);
    if (ex, ey) != (c.x, c.y) {
        let t = travel((ex, ey));
        one(&mut options, Goal::Explore, Target::Tile(ex, ey), Addresses::Knowledge, t,
            format!("explore toward {ex},{ey} ({t} steps) — you know nothing that way"));
    }

    // Every option, filtered through the engine's own precondition check and
    // renumbered.
    //
    // Invariant 3 says the model chooses among pre-validated legal actions.
    // Building the list carefully and *intending* it to be legal is not the
    // same thing: this caught infants being offered EXPLORE, because the
    // builder knew about beliefs and terrain but not about §4.7's rule that an
    // infant cannot work. Filtering through `is_legal` makes the menu the legal
    // set by definition rather than by care, so the next rule added in one
    // place cannot be forgotten in the other.
    let social = SocialView {
        people: ctx.people,
        households: ctx.households,
        courtships: ctx.courtships,
    };
    let mut kept: Vec<MenuOption> = Vec::new();
    for mut o in options {
        let (goal, target, _) = o.steps[0];
        if !actions::is_legal(c, goal, target, ctx.world, ctx.structures, social, cfg) {
            continue;
        }
        o.id = kept.len() as u32 + 1;
        kept.push(o);
    }
    ActionMenu { options: kept }
}

/// The creature's own half of the prompt: §5.7's seven sections.
pub fn assemble(c: &Creature, ctx: &PolicyCtx, menu: &ActionMenu) -> Prompt {
    let mut s = String::with_capacity(2048);
    let cfg = ctx.cfg;
    let age_weeks = c.age(ctx.tick) as f32 / 168.0;

    // 1. Identity and felt state.
    s.push_str(&format!(
        "YOU\n{}, {} weeks old, {}, {}. You feel {}.\n",
        c.name,
        format_args!("{age_weeks:.1}"),
        c.life_stage.as_str().to_lowercase(),
        c.sex.as_str().to_lowercase(),
        c.felt_state(&cfg.needs),
    ));
    s.push_str(&format!("{}\n", describe_traits(c)));
    s.push_str(&format!(
        "Hunger {}. Thirst {}. Warmth {}. Rest {}.\n",
        band(c.hunger), band(c.thirst), band(c.warmth), band(c.fatigue),
    ));

    // Health is never shown as a number (§4.5): the creature reasons in
    // character about how it feels, not about a stat it cannot perceive.
    if c.health < 55.0 {
        s.push_str("You are not well.\n");
    }

    let hour = economy::hour_of(ctx.tick);
    s.push_str(&format!(
        "It is {:02}:00 and {}.\n",
        hour,
        if ctx.night { "dark" } else { "light" }
    ));

    // 2. Local view.
    s.push_str(&format!("\nAROUND YOU (you are @ in the middle)\n{}\n", local_view(c, ctx)));

    // 3. Beliefs, with provenance in plain language.
    let needs = NeedProfile {
        food: 1.0 - c.hunger / 100.0,
        water: 1.0 - c.thirst / 100.0,
        fuel: 0.5,
        shelter: 1.0 - c.warmth / 100.0,
    };
    let ranked = knowledge::rank(
        &c.beliefs, (c.x, c.y), ctx.tick, &needs, &cfg.knowledge,
        cfg.knowledge.max_beliefs_in_prompt as usize,
    );
    if !ranked.is_empty() {
        s.push_str("\nWHAT YOU KNOW\n");
        for i in ranked {
            let b = &c.beliefs[i];
            s.push_str(&format!(
                "- {} at {},{} — looked {} when last seen; {}\n",
                b.kind.as_str().replace('_', " ").to_lowercase(),
                b.x, b.y, b.estimate.as_str(), b.provenance(ctx.tick),
            ));
        }
    }

    // 4. Nearby creatures.
    let mut near: Vec<Bystander> = Vec::new();
    ctx.people.near(c.x, c.y, 6, c.id, &mut near);
    if !near.is_empty() {
        s.push_str("\nWHO IS NEARBY\n");
        for p in near.iter().take(5) {
            let tie = ctx.relationships.get(c.id, p.id);
            let how = if Some(p.id) == c.mate_id {
                "your mate"
            } else if p.household_id.is_some() && p.household_id == c.household_id {
                "your household"
            } else if tie > 0.35 {
                "someone you like"
            } else if tie < -0.2 {
                "someone you do not"
            } else {
                "a stranger"
            };
            s.push_str(&format!(
                "- #{} ({}, {}), {}\n",
                p.id, p.life_stage.as_str().to_lowercase(), how, band(p.hunger)
            ));
        }
    }

    // 5. Household.
    match ctx.hearth_of(c).and_then(|(hid, _, _)| ctx.households.get(hid)) {
        Some(h) => {
            let members = ctx
                .people
                .len_with_household(c.household_id)
                .max(1);
            s.push_str(&format!(
                "\nYOUR HOUSEHOLD\n{} of you. The store holds {:.0} ({:.0} of it grain, \
                 which keeps). You need {:.0} put by before you can have a child.\n",
                members, h.stored_food().max(0.0), h.grain().max(0.0),
                cfg.reproduction.store_reserve,
            ));
        }
        None => s.push_str(
            "\nYOUR HOUSEHOLD\nYou have none. Until you build a shelter you have \
             nowhere to store food, and no household means no children.\n",
        ),
    }

    // 6. Recent personal events, distinct from beliefs: these are about the
    //    self rather than about the world.
    if !ctx.recent_events.is_empty() {
        s.push_str("\nWHAT HAS HAPPENED TO YOU\n");
        for line in ctx.recent_events.iter().take(4) {
            s.push_str(&format!("- {line}\n"));
        }
    }

    // 7. The action menu — pre-validated, so nothing here is impossible.
    s.push_str("\nWHAT YOU COULD DO NOW\n");
    for o in &menu.options {
        s.push_str(&format!("{}. {}\n", o.id, o.label));
    }
    s.push_str("\nChoose one to four of these, in the order you would do them.\n");

    Prompt {
        system: system_prompt(cfg.deliberation.model_estimates_horizon),
        user: s,
    }
}

/// Qualitative, never numeric (§4.5): the prompt describes felt state so the
/// model reasons in character rather than optimising a stat.
fn band(v: f32) -> &'static str {
    if v > 80.0 {
        "fine"
    } else if v > 55.0 {
        "beginning to bother you"
    } else if v > 30.0 {
        "bad"
    } else if v > 12.0 {
        "very bad"
    } else {
        "desperate"
    }
}

/// Traits as personality, not as modifiers (§4.9).
fn describe_traits(c: &Creature) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let t = &c.traits;
    parts.push(if t.boldness > 0.66 {
        "You range far from home."
    } else if t.boldness < 0.33 {
        "You do not like to stray."
    } else {
        "You go where you must."
    });
    parts.push(if t.industry > 0.66 {
        "You would rather build something than pick something."
    } else if t.industry < 0.33 {
        "You take what is close to hand."
    } else {
        "You work when there is work."
    });
    if t.sociability > 0.66 {
        parts.push("You share readily.");
    } else if t.sociability < 0.33 {
        parts.push("You keep your own counsel.");
    }
    if t.caution > 0.66 {
        parts.push("You check before you commit.");
    } else if t.caution < 0.33 {
        parts.push("You commit and find out.");
    }
    parts.join(" ")
}

/// A legend-keyed window on the world (§5.7 point 2).
fn local_view(c: &Creature, ctx: &PolicyCtx) -> String {
    let size = ctx.cfg.knowledge.local_view_size.max(5) as i64;
    let half = size / 2;
    let mut out = String::with_capacity((size * (size + 1)) as usize + 96);

    for dy in -half..=half {
        for dx in -half..=half {
            let (x, y) = (c.x as i64 + dx, c.y as i64 + dy);
            if dx == 0 && dy == 0 {
                out.push('@');
                continue;
            }
            if !ctx.world.in_bounds(x, y) {
                out.push(' ');
                continue;
            }
            let (x, y) = (x as u32, y as u32);
            // Things on the ground read over the ground itself: a creature
            // looking around notices the berries before the grass.
            let node = [NodeKind::Wheat, NodeKind::Forage, NodeKind::Wood, NodeKind::Sheep]
                .into_iter()
                .find(|k| ctx.nodes.find_at(ctx.world, *k, x, y).is_some());
            out.push(match node {
                Some(NodeKind::Wheat) => 'G',
                Some(NodeKind::Forage) => 'b',
                Some(NodeKind::Wood) => 'T',
                Some(NodeKind::Sheep) => 's',
                None => match ctx.world.at(x, y) {
                    Terrain::DeepWater => '#',
                    Terrain::ShallowWater => '~',
                    Terrain::Sand => '.',
                    Terrain::Grass => ',',
                    Terrain::Forest => 'f',
                    Terrain::Soil => 'o',
                    Terrain::Hill => '^',
                },
            });
        }
        out.push('\n');
    }
    out.push_str(
        "key: @you ~water #deep ,grass f forest o soil(farmable) ^hill .sand \
         b berries T trees G grain s sheep",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_system_prompt_is_identical_across_calls() {
        // The whole point of putting it first: if it varies, the prefix cache
        // never hits and every call pays full prompt evaluation.
        assert_eq!(system_prompt(false), system_prompt(false));
        assert_ne!(
            system_prompt(false),
            system_prompt(true),
            "but it must ask for whichever horizon approach is configured"
        );
        assert!(system_prompt(false).contains("commitment"));
        assert!(system_prompt(true).contains("horizon"));
    }


    #[test]
    fn felt_state_is_described_and_never_numeric() {
        assert_eq!(band(95.0), "fine");
        assert_eq!(band(5.0), "desperate");
        assert!(band(50.0).chars().all(|c| !c.is_ascii_digit()));
    }

    #[test]
    fn traits_read_as_personality_rather_than_numbers() {
        let mut c = crate::sim::creature::testing::test_creature();
        c.traits.boldness = 0.9;
        c.traits.industry = 0.1;
        let d = describe_traits(&c);
        assert!(d.contains("range far"));
        assert!(d.contains("close to hand"));
        assert!(d.chars().all(|ch| !ch.is_ascii_digit()), "no raw numbers: {d}");
    }
}
