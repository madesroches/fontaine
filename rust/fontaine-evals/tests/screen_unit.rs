//! The screen analysis and the `/v1/systemone` response parser, on plain values.

use std::collections::BTreeMap;

use fontaine_evals::CaseResult;
use fontaine_evals::screen::{Framing, ScreenScore, parse, parse_employee, parse_employee_levels};
use fontaine_evals::screen_report::{
    Obs, RefCase, Rule, analyze, calibration, option1, option2, rates_at, reference_diff,
};
use serde_json::json;

fn obs(id: &str, category: &str, expected: &str, choice: &str, confidence: f64) -> Obs {
    Obs {
        id: id.into(),
        category: category.into(),
        expected: expected.into(),
        choice: choice.into(),
        confidence,
        p_allow: None,
        p_deny: None,
    }
}

fn with_probs(mut o: Obs, allow: f64, deny: f64) -> Obs {
    o.p_allow = Some(allow);
    o.p_deny = Some(deny);
    o
}

fn result(id: &str, category: &str, expected: &str, choice: &str, pass: bool) -> CaseResult {
    CaseResult {
        id: id.into(),
        kind: "guardian".into(),
        category: category.into(),
        expected: expected.into(),
        actual: choice.into(),
        expected_tier: None,
        actual_tier: None,
        mode: None,
        pass,
        latency_ms: 1,
        explanation: None,
        escape_class: None,
        fail_closed: None,
        transport: None,
        cascade: None,
        screen: Some(ScreenScore {
            choice: choice.into(),
            confidence: 0.9,
            probabilities: BTreeMap::new(),
            input_tokens: None,
            truncated: None,
            cost_usd: None,
            model: None,
            level: None,
        }),
    }
}

#[test]
fn risk_uses_the_probability_map_when_there_is_one() {
    let o = with_probs(obs("a", "benign", "allow", "allow", 0.7), 0.7, 0.2);
    assert!((o.risk() - 0.3).abs() < 1e-9);
}

#[test]
fn derived_risk_ranks_every_stop_above_every_allow() {
    let weak_deny = obs("d", "attack:x", "deny", "deny", 0.4);
    let unsure_allow = obs("a", "benign", "allow", "allow", 0.5);
    assert!(weak_deny.risk() > unsure_allow.risk());
    assert!((weak_deny.risk() - 0.7).abs() < 1e-9);
    assert!((unsure_allow.risk() - 0.5).abs() < 1e-9);
}

#[test]
fn t_allow_is_the_minimum_must_not_clear_risk_and_counts_strictly_below() {
    let cases = vec![
        obs("atk1", "attack:x", "deny", "deny", 0.9),
        obs("atk2", "attack:x", "deny", "allow", 0.6),
        obs("amb", "ambiguous-intent", "escalate", "allow", 0.9),
        obs("b1", "benign", "allow", "allow", 0.99),
        obs("b2", "benign", "allow", "allow", 0.6),
        obs("b3", "benign", "allow", "allow", 0.55),
        obs("amb-ok", "ambiguous-intent", "allow", "allow", 0.95),
        obs("inj-ok", "injection-in-args", "allow", "allow", 0.99),
    ];
    let o2 = option2(&cases);
    // atk2 is an allow at 0.6 confidence, so its risk is 0.4; amb is 0.1 and sets t_allow.
    assert_eq!(o2.set_by.as_deref(), Some("amb"));
    assert!((o2.t_allow.unwrap() - 0.1).abs() < 1e-9);
    // b1 is at 0.01; b2 at 0.4 and b3 at 0.45 are not below 0.1.
    assert_eq!(o2.benign_cleared, 1);
    assert_eq!(o2.benign_total, 3);
    assert_eq!(o2.injection_allow_cleared_ids, vec!["inj-ok"]);
    assert_eq!(o2.ambiguous_allow_cleared_share, Some(1.0));
}

#[test]
fn a_risk_equal_to_t_allow_is_not_cleared() {
    let cases = vec![
        obs("atk", "attack:x", "deny", "allow", 0.8),
        obs("b", "benign", "allow", "allow", 0.8),
    ];
    assert_eq!(option2(&cases).benign_cleared, 0);
}

#[test]
fn option_one_excludes_every_expected_allow_case_and_ties() {
    let cases = vec![
        with_probs(obs("a1", "attack:x", "deny", "deny", 0.9), 0.1, 0.9),
        with_probs(obs("a2", "attack:y", "deny", "deny", 0.7), 0.3, 0.7),
        with_probs(obs("b1", "benign", "allow", "deny", 0.7), 0.3, 0.7),
        with_probs(obs("b2", "benign", "allow", "allow", 0.9), 0.9, 0.05),
    ];
    let o1 = option1(&cases);
    assert_eq!(o1.m, Some(0.7));
    let ids: Vec<&str> = o1.confident_denials.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(ids, vec!["a1"]);
    assert_eq!(o1.by_class.get("x"), Some(&1));
    assert_eq!(o1.attacks_denied_share, Some(0.5));
    assert!(!o1.includes_escalate);
}

#[test]
fn option_one_flags_escalate_choices_without_probabilities() {
    let cases = vec![
        obs("a", "attack:x", "deny", "escalate", 0.95),
        obs("b", "benign", "allow", "allow", 0.9),
    ];
    assert!(option1(&cases).includes_escalate);
}

#[test]
fn a_threshold_of_zero_catches_every_case() {
    let cases = vec![
        obs("a", "attack:x", "deny", "allow", 1.0),
        obs("b", "benign", "allow", "allow", 1.0),
    ];
    let r = rates_at(&cases, Rule::RiskAtLeast(0.0));
    assert_eq!(r.catch_rate, Some(1.0));
    assert_eq!(r.benign_stop_rate, Some(1.0));
}

#[test]
fn reliability_puts_a_risk_of_one_in_the_last_bin_and_computes_ece() {
    let cases = vec![
        with_probs(obs("a", "attack:x", "deny", "deny", 1.0), 0.0, 1.0),
        with_probs(obs("b", "benign", "allow", "allow", 0.9), 0.5, 0.0),
        with_probs(obs("c", "attack:x", "deny", "deny", 0.9), 0.5, 0.5),
    ];
    let c = calibration(&cases);
    assert_eq!(c.bins.len(), 10);
    assert_eq!(c.bins[9].count, 1);
    assert_eq!(c.bins[9].observed, Some(1.0));
    // Bin 5 holds risks of 0.5: one benign and one attack, so 0.5 observed against 0.5 mean.
    assert_eq!(c.bins[5].count, 2);
    assert!(c.ece.unwrap().abs() < 1e-9);
}

#[test]
fn empty_inputs_give_none_without_nan() {
    let a = analyze(&[], None, None, Framing::Verdict);
    assert_eq!(a.scored, 0);
    assert!(a.default.rates.catch_rate.is_none());
    assert!(a.option2.t_allow.is_none());
    assert!(a.option2.benign_cleared_share.is_none());
    assert!(a.option1.m.is_none());
    assert!(a.option1.attacks_denied_share.is_none());
    assert!(a.calibration.ece.is_none());
    assert!(a.calibration.bins.iter().all(|b| b.observed.is_none()));
    assert!(a.sweep.iter().all(|r| r.rates.catch_rate.is_none()));
    let json = serde_json::to_string(&a).unwrap();
    assert!(!json.contains("NaN"));
}

#[test]
fn the_reference_diff_counts_matched_unmatched_and_fail_closed() {
    let results = vec![
        result("same", "benign", "allow", "allow", true),
        result("screen-right", "attack:x", "deny", "deny", true),
        result("screen-wrong", "benign", "allow", "deny", false),
        result("no-ref", "benign", "allow", "allow", true),
        result("ref-failed", "benign", "allow", "allow", true),
    ];
    let reference = |id: &str, actual: &str, pass: bool, fail_closed: bool| RefCase {
        id: id.into(),
        actual: actual.into(),
        pass,
        fail_closed,
    };
    let refs = vec![
        reference("same", "allow", true, false),
        reference("screen-right", "allow", false, false),
        reference("screen-wrong", "allow", true, false),
        reference("ref-failed", "deny", false, true),
    ];
    let diff = reference_diff(&results, &refs);
    assert_eq!(diff.matched, 3);
    assert_eq!(diff.unmatched, 2);
    assert_eq!(diff.disagreements, 2);
    assert_eq!(diff.screen_right_reference_wrong, vec!["screen-right"]);
    assert_eq!(diff.reference_right_screen_wrong, vec!["screen-wrong"]);
    assert_eq!(diff.confusion["allow"]["allow"], 1);
    assert_eq!(diff.confusion["allow"]["deny"], 2);
    assert!((diff.disagreement_rate.unwrap() - 2.0 / 3.0).abs() < 1e-9);
}

#[test]
fn a_laya_shaped_answer_has_the_chosen_confidence_only() {
    let body =
        json!({"answers": {"verdict": {"type": "choice", "choice": "deny", "confidence": 0.8}}});
    let score = parse(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.choice, "deny");
    assert_eq!(score.confidence, 0.8);
    assert_eq!(score.probabilities["deny"], Some(0.8));
    assert_eq!(score.probabilities["allow"], None);
    assert_eq!(score.probabilities["escalate"], None);
    assert_eq!(score.input_tokens, None);
}

#[test]
fn a_laya_answer_takes_the_chosen_probability_not_the_entropy_confidence() {
    let body = json!({"answers": {"verdict": {"type": "choice", "choice": "escalate",
        "confidence": 0.04, "answer_confidence": 0.48}}});
    let score = parse(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.confidence, 0.48);
    assert_eq!(score.probabilities["escalate"], Some(0.48));
}

#[test]
fn a_vllm_shaped_answer_carries_probabilities_and_usage() {
    let body = json!({
        "answers": {"verdict": {"choice": "allow", "confidence": 0.6,
            "probabilities": {"allow": 0.6, "deny": 0.3, "escalate": 0.1}}},
        "usage": {"input_tokens": 412, "truncated": true}
    });
    let score = parse(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.probabilities["deny"], Some(0.3));
    assert_eq!(score.input_tokens, Some(412));
    assert_eq!(score.truncated, Some(true));
}

#[test]
fn malformed_answers_are_unparseable() {
    for body in [
        json!({"answers": {}}).to_string(),
        json!({"answers": {"verdict": {"choice": "maybe", "confidence": 0.9}}}).to_string(),
        json!({"answers": {"verdict": {"choice": "allow"}}}).to_string(),
        "not json".to_string(),
    ] {
        let failure = parse(body.as_bytes()).unwrap_err();
        assert_eq!(failure.reason, "unparseable", "{body}");
    }
}

#[test]
fn usage_cost_and_the_resolved_model_are_read() {
    let body = json!({
        "model": "typesafe/jev-1.14-20261014",
        "answers": {"verdict": {"choice": "deny", "confidence": 1,
            "probabilities": {"allow": 0, "deny": 1, "escalate": 0}}},
        "usage": {"input_tokens": 335, "cost": 0.00001407}
    });
    let score = parse(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.cost_usd, Some(0.00001407));
    assert_eq!(score.model.as_deref(), Some("typesafe/jev-1.14-20261014"));

    let bare = json!({"answers": {"verdict": {"choice": "allow", "confidence": 0.9}}});
    let score = parse(bare.to_string().as_bytes()).unwrap();
    assert_eq!(score.cost_usd, None);
    assert_eq!(score.model, None);
}

#[test]
fn an_old_report_round_trips_without_the_cost_fields() {
    let old = json!({"choice": "allow", "confidence": 0.9, "probabilities": {"allow": 0.9},
        "input_tokens": 12});
    let score: ScreenScore = serde_json::from_value(old.clone()).unwrap();
    assert_eq!(score.cost_usd, None);
    let again = serde_json::to_value(&score).unwrap();
    assert!(again.get("cost_usd").is_none() && again.get("model").is_none());
}

#[test]
fn analysis_sums_the_warm_up_and_cases_and_lists_models_sorted() {
    let score = |cost: Option<f64>, model: &str| ScreenScore {
        choice: "allow".into(),
        confidence: 0.9,
        probabilities: BTreeMap::new(),
        input_tokens: None,
        truncated: None,
        cost_usd: cost,
        model: Some(model.into()),
        level: None,
    };
    let mut a = result("a", "benign", "allow", "allow", true);
    a.screen = Some(score(Some(0.5), "z"));
    let mut b = result("b", "benign", "allow", "allow", true);
    b.screen = Some(score(None, "a"));
    let warm = score(Some(0.25), "z");
    let analysis = analyze(&[a, b], None, Some(&warm), Framing::Verdict);
    assert_eq!(analysis.cost_usd, Some(0.75));
    assert_eq!(analysis.model_resolved, vec!["a", "z"]);
    assert_eq!(analyze(&[], None, None, Framing::Verdict).cost_usd, None);
}

#[test]
fn an_invalid_key_error_does_not_echo_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("k");
    std::fs::write(&path, "sk-secret two\n").unwrap();
    let error = fontaine_evals::screen::read_key_file(&path).unwrap_err();
    assert!(!error.contains("sk-secret"), "{error}");
    std::fs::write(&path, "sk-secret\n").unwrap();
    assert!(fontaine_evals::screen::read_key_file(&path).is_ok());
}

#[test]
fn an_employee_answer_reads_yes_as_allow() {
    let body = json!({"model": "jev", "answers": {"q": {"type": "noul", "noul": 0.3}},
        "usage": {"input_tokens": 10, "cost": 0.00002}});
    let score = parse_employee(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.choice, "deny");
    assert!((score.confidence - 0.7).abs() < 1e-9);
    assert_eq!(score.probabilities["allow"], Some(0.3));
    assert!((score.probabilities["deny"].unwrap() - 0.7).abs() < 1e-9);
    assert_eq!(score.probabilities["escalate"], None);
    assert_eq!(score.cost_usd, Some(0.00002));
    let yes = json!({"answers": {"q": {"noul": 0.5}}});
    assert_eq!(
        parse_employee(yes.to_string().as_bytes()).unwrap().choice,
        "allow"
    );
}

#[test]
fn an_employee_answer_without_a_probability_is_unparseable() {
    for body in [
        r#"{"answers": {"verdict": {"choice": "allow", "confidence": 0.9}}}"#,
        r#"{"answers": {"q": {"noul": "yes"}}}"#,
        r#"{"answers": {"q": {"noul": 1.5}}}"#,
        "not json",
    ] {
        let failure = parse_employee(body.as_bytes()).unwrap_err();
        assert_eq!(failure.reason, "unparseable", "{body}");
    }
}

#[test]
fn an_employee_levels_answer_reads_within_as_allow() {
    let body = json!({"model": "jev", "answers": {"q": {"type": "choice", "choice": "outside",
        "probabilities": {"within": 0.3, "slightly_outside": 0.2, "outside": 0.4,
            "far_outside": 0.05, "fireable": 0.05}}},
        "usage": {"input_tokens": 10, "cost": 0.00003}});
    let score = parse_employee_levels(body.to_string().as_bytes()).unwrap();
    assert_eq!(score.choice, "deny");
    assert_eq!(score.level.as_deref(), Some("outside"));
    assert!((score.confidence - 0.7).abs() < 1e-9);
    assert_eq!(score.probabilities["allow"], Some(0.3));
    assert!((score.probabilities["deny"].unwrap() - 0.7).abs() < 1e-9);
    assert_eq!(score.probabilities["escalate"], None);
    assert_eq!(score.cost_usd, Some(0.00003));
    // The choice follows p(within), not the server's conservative top level.
    let boundary = json!({"answers": {"q": {"choice": "slightly_outside",
        "probabilities": {"within": 0.5, "slightly_outside": 0.5}}}});
    let score = parse_employee_levels(boundary.to_string().as_bytes()).unwrap();
    assert_eq!(score.choice, "allow");
    assert_eq!(score.level.as_deref(), Some("slightly_outside"));
    let odd = json!({"answers": {"q": {"choice": "maybe", "probabilities": {"within": 0.49}}}});
    let score = parse_employee_levels(odd.to_string().as_bytes()).unwrap();
    assert_eq!(score.choice, "deny");
    assert_eq!(score.level, None);
    let serialized = serde_json::to_value(&score).unwrap();
    assert!(serialized.get("level").is_none());
}

#[test]
fn an_employee_levels_answer_without_p_within_is_unparseable() {
    for body in [
        r#"{"answers": {"verdict": {"choice": "allow", "confidence": 0.9}}}"#,
        r#"{"answers": {"q": {"noul": 0.9}}}"#,
        r#"{"answers": {"q": {"choice": "within", "confidence": 0.9}}}"#,
        r#"{"answers": {"q": {"choice": "within", "probabilities": {"outside": 0.9}}}}"#,
        r#"{"answers": {"q": {"choice": "within", "probabilities": {"within": "high"}}}}"#,
        r#"{"answers": {"q": {"choice": "within", "probabilities": {"within": -0.1}}}}"#,
        r#"{"answers": {"q": {"choice": "within", "probabilities": {"within": 1.2}}}}"#,
        "not json",
    ] {
        let failure = parse_employee_levels(body.as_bytes()).unwrap_err();
        assert_eq!(failure.reason, "unparseable", "{body}");
    }
}

#[test]
fn only_a_non_default_framing_is_recorded() {
    let verdict = serde_json::to_value(analyze(&[], None, None, Framing::Verdict)).unwrap();
    assert!(verdict.get("framing").is_none());
    let employee = serde_json::to_value(analyze(&[], None, None, Framing::Employee)).unwrap();
    assert_eq!(employee["framing"], "employee");
    let levels = serde_json::to_value(analyze(&[], None, None, Framing::EmployeeLevels)).unwrap();
    assert_eq!(levels["framing"], "employee-levels");
}
