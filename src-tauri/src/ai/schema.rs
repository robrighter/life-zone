//! The plan schema and response validation (PRD §5.5, §5.8).
//!
//! **Invariant 3 is enforced structurally here, not by checking afterwards.**
//! The engine enumerates the goals that are legal for this creature right now,
//! numbers them, and the model picks numbers. An impossible action is not
//! rejected — it is unrepresentable, because it was never given an id. That
//! removes an entire class of failure, and it shrinks the prompt a great deal:
//! a small model does not have to be taught the world's preconditions, only to
//! choose between things that are already true.
//!
//! It also makes the two horizon experiments of §13.9 cheap to run side by
//! side: the same response can carry an explicit `horizon` or a coarse
//! `commitment`, and config decides which one is authoritative.

use crate::config::WorldConfig;
use crate::sim::actions::{Goal, Step, Target};
use crate::sim::creature::Addresses;
use serde::{Deserialize, Serialize};

/// One legal thing this creature could do, with an id the model can name.
#[derive(Debug, Clone)]
pub struct MenuOption {
    pub id: u32,
    /// The first thing this option does. Kept separate because the horizon cap
    /// and the plan's `addresses` are decided by what the option is *for*.
    pub goal: Goal,
    pub target: Target,
    /// Everything the option does, in order — usually the walk and then the
    /// work.
    ///
    /// An option is a thing a creature could do, not an engine step. Offering
    /// only the walk made the model produce plans of three consecutive
    /// journeys, because "go to the berries" and "pick the berries" were
    /// separate menu entries and it could only afford to name the first of
    /// each. Bundling them is also what makes a multi-step plan worth the call
    /// (§5.5): one call, three useful ticks of behaviour rather than three
    /// arrivals.
    pub steps: Vec<(Goal, Target, u32)>,
    /// What the option looks like to the model: "gather forage at 312,88 —
    /// plentiful, you saw it yourself two days ago".
    pub label: String,
    /// The need this option serves, carried through to the plan so a crisis
    /// does not cancel the plan that answers it.
    pub addresses: Addresses,
    /// The engine's own estimate of how long this step takes, used when the
    /// engine derives the horizon rather than the model (§13.9 approach B).
    pub est_ticks: u32,
}

/// The set of options offered for one deliberation.
#[derive(Debug, Clone, Default)]
pub struct ActionMenu {
    pub options: Vec<MenuOption>,
}

impl ActionMenu {
    pub fn get(&self, id: u32) -> Option<&MenuOption> {
        self.options.iter().find(|o| o.id == id)
    }

    pub fn is_empty(&self) -> bool {
        self.options.is_empty()
    }
}

/// How committed the creature says it is, when the engine derives the number.
///
/// §13.9: asking a small model to predict how many ticks a plan will take is a
/// genuinely hard judgment and it may simply be bad at it. The fallback is to
/// let the engine derive the horizon from step costs and let the model choose
/// only a coarse commitment level — which is a much easier question, and one it
/// has real information about, since it can see how stale its own beliefs are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Commitment {
    /// Check again soon; this rests on something I am unsure of.
    Brief,
    #[default]
    Moderate,
    /// I know what I am doing and I will see it through.
    Committed,
}

impl Commitment {
    /// Multiplier on the engine's own estimate of the plan's length.
    pub fn scale(self) -> f32 {
        match self {
            Commitment::Brief => 0.5,
            Commitment::Moderate => 1.0,
            Commitment::Committed => 1.6,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Commitment::Brief => "brief",
            Commitment::Moderate => "moderate",
            Commitment::Committed => "committed",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RawStep {
    /// The id of a menu option. The only way to name an action.
    pub option: u32,
}

/// Exactly what the model is asked to return.
#[derive(Debug, Clone, Deserialize)]
pub struct RawPlan {
    pub steps: Vec<RawStep>,
    /// §13.9 approach A: the model names the number of ticks.
    #[serde(default)]
    pub horizon: Option<u32>,
    /// §13.9 approach B: the model names how sure it is and the engine does
    /// the arithmetic.
    #[serde(default)]
    pub commitment: Option<Commitment>,
    #[serde(default)]
    pub rationale: String,
}

/// Why a response could not be used. Every one is recorded against the call, so
/// a rising fallback rate can be attributed rather than merely noticed
/// (invariant 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reject {
    NoJson,
    BadJson,
    NoSteps,
    TooManySteps,
    UnknownOption,
    HorizonOutOfRange,
    EmptyMenu,
}

impl Reject {
    pub fn as_str(self) -> &'static str {
        match self {
            Reject::NoJson => "NO_JSON_IN_RESPONSE",
            Reject::BadJson => "MALFORMED_JSON",
            Reject::NoSteps => "PLAN_HAD_NO_STEPS",
            Reject::TooManySteps => "TOO_MANY_STEPS",
            Reject::UnknownOption => "OPTION_NOT_ON_THE_MENU",
            Reject::HorizonOutOfRange => "HORIZON_OUT_OF_RANGE",
            Reject::EmptyMenu => "NOTHING_WAS_LEGAL",
        }
    }

    /// Whether a repair retry is worth the second call (§5.8 allows exactly
    /// one). Retrying a structurally sound plan that named a bad option is
    /// worthwhile; retrying an empty menu is not, because the fault is ours.
    pub fn worth_repairing(self) -> bool {
        !matches!(self, Reject::EmptyMenu)
    }
}

/// Pull the JSON object out of whatever the model actually said.
///
/// qwen3 is a reasoning model: even with thinking disabled it will sometimes
/// wrap output in a fence, prefix it with a sentence, or leave a `<think>`
/// block in. None of that is a failure worth burning a retry on, so it is
/// stripped here rather than rejected.
pub fn extract_json(raw: &str) -> Option<&str> {
    // Drop any reasoning block first: it can itself contain braces.
    let body = match raw.find("</think>") {
        Some(i) => &raw[i + "</think>".len()..],
        None => raw,
    };
    let start = body.find('{')?;
    // Scan for the matching close rather than taking the last brace in the
    // string, which would swallow trailing commentary.
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (i, &b) in bytes.iter().enumerate().skip(start) {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&body[start..=i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// The result of turning a model response into something the engine can run.
#[derive(Debug, Clone, PartialEq)]
pub struct Validated {
    pub steps: Vec<Step>,
    pub horizon: u32,
    pub rationale: String,
    pub addresses: Addresses,
    /// What the model said about commitment, for the §13.9 comparison.
    pub commitment: Option<Commitment>,
    pub model_horizon: Option<u32>,
}

/// Validate a raw response against the menu it was offered.
///
/// §5.5: step 1 is validated hard at issue time; later steps are re-validated
/// when reached. Here every step is checked against the menu — which is what
/// makes step 1 legal by construction — and the later ones are checked again by
/// the action executor when the plan reaches them, where a failure aborts the
/// plan rather than silently no-oping.
pub fn validate(
    raw: &str,
    menu: &ActionMenu,
    cfg: &WorldConfig,
) -> Result<Validated, Reject> {
    if menu.is_empty() {
        return Err(Reject::EmptyMenu);
    }
    let json = extract_json(raw).ok_or(Reject::NoJson)?;
    let plan: RawPlan = serde_json::from_str(json).map_err(|_| Reject::BadJson)?;

    if plan.steps.is_empty() {
        return Err(Reject::NoSteps);
    }
    // A long plan is not better than a short one and a small model will happily
    // emit twenty steps of nonsense.
    if plan.steps.len() > 4 {
        return Err(Reject::TooManySteps);
    }

    let mut steps = Vec::with_capacity(plan.steps.len());
    let mut implied = 0u32;
    let mut cap = 0u32;
    let mut addresses = Addresses::Nothing;

    for (i, rs) in plan.steps.iter().enumerate() {
        let opt = menu.get(rs.option).ok_or(Reject::UnknownOption)?;
        if i == 0 {
            addresses = opt.addresses;
        }
        for (goal, target, est) in &opt.steps {
            implied += est;
            cap += est.min(&goal.horizon_cap(cfg));
            steps.push(Step::new(*goal, *target, *est));
        }
    }
    // A plan of four options can expand into rather more engine steps; that is
    // fine, but it must not run away.
    if steps.len() > 10 {
        return Err(Reject::TooManySteps);
    }

    // Either the model named the horizon, or it named how sure it is and the
    // engine does the arithmetic (§13.9). Both are clamped to the per-goal caps
    // of §5.5 — the model may not commit a creature to a courtship twenty ticks
    // in advance, however confident it sounds.
    let cap = cap.max(1);
    let horizon = if cfg.deliberation.model_estimates_horizon {
        let h = plan.horizon.ok_or(Reject::HorizonOutOfRange)?;
        if h == 0 || h > cfg.deliberation.horizon_cap_travel * 4 {
            return Err(Reject::HorizonOutOfRange);
        }
        h.min(cap)
    } else {
        let scale = plan.commitment.unwrap_or_default().scale();
        (((implied.max(1)) as f32 * scale).round() as u32).clamp(1, cap)
    };

    let rationale = plan.rationale.trim();
    Ok(Validated {
        steps,
        horizon,
        // Truncated rather than rejected: an over-long rationale is the model
        // being chatty, not the model being wrong, and the inspector has to
        // render it.
        rationale: rationale.chars().take(240).collect(),
        addresses,
        commitment: plan.commitment,
        model_horizon: plan.horizon,
    })
}

/// The JSON schema handed to Ollama's structured-output mode, so the model is
/// constrained at generation time rather than corrected afterwards.
pub fn response_schema(model_estimates_horizon: bool) -> serde_json::Value {
    let horizon_field = if model_estimates_horizon {
        serde_json::json!({
            "horizon": { "type": "integer", "minimum": 1, "maximum": 96 }
        })
    } else {
        serde_json::json!({
            "commitment": { "type": "string", "enum": ["brief", "moderate", "committed"] }
        })
    };
    let mut properties = serde_json::json!({
        "steps": {
            "type": "array",
            "minItems": 1,
            "maxItems": 4,
            "items": {
                "type": "object",
                "properties": { "option": { "type": "integer" } },
                "required": ["option"]
            }
        },
        "rationale": { "type": "string" }
    });
    if let (Some(p), Some(h)) = (properties.as_object_mut(), horizon_field.as_object()) {
        for (k, v) in h {
            p.insert(k.clone(), v.clone());
        }
    }
    let required = if model_estimates_horizon {
        serde_json::json!(["steps", "horizon", "rationale"])
    } else {
        serde_json::json!(["steps", "commitment", "rationale"])
    };
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn menu() -> ActionMenu {
        ActionMenu {
            options: vec![
                MenuOption {
                    id: 1, goal: Goal::MoveTo, target: Target::Tile(10, 10),
                    steps: vec![(Goal::MoveTo, Target::Tile(10, 10), 8)],
                    label: "walk to the water".into(), addresses: Addresses::Water,
                    est_ticks: 8,
                },
                MenuOption {
                    id: 2, goal: Goal::Drink, target: Target::Tile(10, 10),
                    steps: vec![(Goal::Drink, Target::Tile(10, 10), 1)],
                    label: "drink".into(), addresses: Addresses::Water, est_ticks: 1,
                },
                MenuOption {
                    id: 3, goal: Goal::Court, target: Target::Creature(9),
                    steps: vec![(Goal::Court, Target::Creature(9), 1)],
                    label: "ask Mira".into(), addresses: Addresses::Kinship, est_ticks: 1,
                },
            ],
        }
    }

    fn cfg() -> WorldConfig {
        WorldConfig::default()
    }

    #[test]
    fn a_well_formed_plan_becomes_runnable_steps() {
        let c = cfg();
        let raw = r#"{"steps":[{"option":1},{"option":2}],"commitment":"moderate",
                      "rationale":"Thirsty. The pool is close."}"#;
        let v = validate(raw, &menu(), &c).unwrap();

        assert_eq!(v.steps.len(), 2);
        assert_eq!(v.steps[0].goal, Goal::MoveTo);
        assert_eq!(v.steps[1].goal, Goal::Drink);
        assert_eq!(v.addresses, Addresses::Water, "the plan knows what it is for");
        assert!(v.rationale.contains("Thirsty"));
    }

    #[test]
    fn one_option_can_expand_into_the_walk_and_the_work() {
        // §5.5: the expensive thing is the call, not the tokens. An option that
        // is only the journey wastes most of what a call buys.
        let c = cfg();
        let mut m = menu();
        m.options.push(MenuOption {
            id: 4,
            goal: Goal::MoveTo,
            target: Target::Tile(40, 40),
            steps: vec![
                (Goal::MoveTo, Target::Tile(40, 40), 12),
                (Goal::GatherForage, Target::Node(3), 10),
            ],
            label: "go and pick the berries at 40,40".into(),
            addresses: Addresses::Food,
            est_ticks: 22,
        });
        let raw = r#"{"steps":[{"option":4}],"commitment":"moderate","rationale":"Hungry."}"#;
        let v = validate(raw, &m, &c).unwrap();

        assert_eq!(v.steps.len(), 2, "one choice, two things done");
        assert_eq!(v.steps[0].goal, Goal::MoveTo);
        assert_eq!(v.steps[1].goal, Goal::GatherForage);
        assert_eq!(v.addresses, Addresses::Food);
    }

    #[test]
    fn an_action_that_was_never_offered_cannot_be_named() {
        // Invariant 3. The engine checks preconditions before offering, so an
        // impossible action has no id and simply cannot be expressed.
        let c = cfg();
        let raw = r#"{"steps":[{"option":99}],"commitment":"brief","rationale":"x"}"#;
        assert_eq!(validate(raw, &menu(), &c), Err(Reject::UnknownOption));
    }

    #[test]
    fn reasoning_tokens_and_fences_are_stripped_rather_than_rejected() {
        let c = cfg();
        let raw = "<think>The creature is thirsty, so it should drink. I will pick 2.\
                   Maybe {not} this.</think>\n```json\n\
                   {\"steps\":[{\"option\":2}],\"commitment\":\"brief\",\"rationale\":\"Drink.\"}\
                   \n```\nHope that helps!";
        let v = validate(raw, &menu(), &c).unwrap();
        assert_eq!(v.steps.len(), 1);
        assert_eq!(v.steps[0].goal, Goal::Drink);
    }

    #[test]
    fn a_brace_inside_a_string_does_not_end_the_object() {
        let raw = r#"prefix {"steps":[{"option":1}],"rationale":"a } brace"} suffix"#;
        let json = extract_json(raw).unwrap();
        assert!(json.ends_with('}'));
        assert!(json.contains("a } brace"));
        assert!(!json.contains("suffix"));
    }

    #[test]
    fn malformed_json_is_rejected_with_a_reason_worth_retrying() {
        let c = cfg();
        assert_eq!(
            validate("{\"steps\": [ {\"option\": ", &menu(), &c),
            Err(Reject::NoJson)
        );
        assert_eq!(
            validate("{\"steps\": \"not an array\"}", &menu(), &c),
            Err(Reject::BadJson)
        );
        assert!(Reject::BadJson.worth_repairing());
    }

    #[test]
    fn a_plan_with_no_steps_is_not_a_plan() {
        let c = cfg();
        let raw = r#"{"steps":[],"commitment":"brief","rationale":"nothing"}"#;
        assert_eq!(validate(raw, &menu(), &c), Err(Reject::NoSteps));
    }

    #[test]
    fn an_absurdly_long_plan_is_refused() {
        let c = cfg();
        let raw = r#"{"steps":[{"option":1},{"option":2},{"option":1},{"option":2},
                     {"option":1}],"commitment":"committed","rationale":"x"}"#;
        assert_eq!(validate(raw, &menu(), &c), Err(Reject::TooManySteps));
    }

    #[test]
    fn an_empty_menu_is_our_fault_and_not_worth_a_retry() {
        let c = cfg();
        let raw = r#"{"steps":[{"option":1}],"commitment":"brief","rationale":"x"}"#;
        assert_eq!(
            validate(raw, &ActionMenu::default(), &c),
            Err(Reject::EmptyMenu)
        );
        assert!(!Reject::EmptyMenu.worth_repairing());
    }

    #[test]
    fn the_model_may_not_commit_a_creature_beyond_the_per_goal_caps() {
        // §5.5: you cannot commit to a courtship twenty ticks in advance,
        // however confident the model sounds.
        let mut c = cfg();
        c.deliberation.model_estimates_horizon = true;
        let raw = r#"{"steps":[{"option":3}],"horizon":90,"rationale":"very sure"}"#;
        let v = validate(raw, &menu(), &c).unwrap();
        assert!(
            v.horizon <= c.deliberation.horizon_cap_social,
            "social horizon {} exceeded the cap {}",
            v.horizon, c.deliberation.horizon_cap_social
        );
    }

    #[test]
    fn an_out_of_range_horizon_is_rejected_rather_than_clamped() {
        let mut c = cfg();
        c.deliberation.model_estimates_horizon = true;
        let raw = r#"{"steps":[{"option":1}],"horizon":100000,"rationale":"forever"}"#;
        assert_eq!(validate(raw, &menu(), &c), Err(Reject::HorizonOutOfRange));

        let zero = r#"{"steps":[{"option":1}],"horizon":0,"rationale":"none"}"#;
        assert_eq!(validate(zero, &menu(), &c), Err(Reject::HorizonOutOfRange));
    }

    #[test]
    fn commitment_level_scales_the_engines_own_estimate() {
        // §13.9 approach B: the model says how sure it is, the engine does the
        // arithmetic. A much easier question to ask a small model.
        let c = cfg();
        let horizon_for = |level: &str| {
            let raw = format!(
                r#"{{"steps":[{{"option":1}}],"commitment":"{level}","rationale":"x"}}"#
            );
            validate(&raw, &menu(), &c).unwrap().horizon
        };
        assert!(horizon_for("brief") < horizon_for("committed"));
    }

    #[test]
    fn a_missing_commitment_falls_back_to_moderate_rather_than_failing() {
        let c = cfg();
        let raw = r#"{"steps":[{"option":1}],"rationale":"forgot to say"}"#;
        let v = validate(raw, &menu(), &c).unwrap();
        assert!(v.horizon >= 1);
    }

    #[test]
    fn the_schema_asks_for_whichever_horizon_approach_is_configured() {
        let a = response_schema(true);
        assert!(a["properties"]["horizon"].is_object());
        assert!(a["required"].as_array().unwrap().iter().any(|v| v == "horizon"));

        let b = response_schema(false);
        assert!(b["properties"]["commitment"].is_object());
        assert!(b["required"].as_array().unwrap().iter().any(|v| v == "commitment"));
    }
}
