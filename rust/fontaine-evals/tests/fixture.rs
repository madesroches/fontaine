use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use fontaine_evals::{Case, Kind, Profile, load_dataset, minimum_count_shortfalls};
use serde_json::{Value, json};
use tempfile::TempDir;

fn seed_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../evals/dataset")
}

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fontaine-evals"))
        .args(args)
        .output()
        .unwrap()
}

fn run_fixture(dataset: &Path, extra: &[&str]) -> Output {
    let mut args = vec![
        "--dataset",
        dataset.to_str().unwrap(),
        "--guardian",
        "fixture",
    ];
    args.extend_from_slice(extra);
    run(&args)
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn guardian_case(id: &str, category: &str, expected: &str, verdict: &str) -> Value {
    json!({
        "id": id, "kind": "guardian", "category": category, "profile": "full",
        "task_intent": "t", "tool": "shell", "args": {"command": "ls"},
        "expected": expected, "fixture_verdict": verdict
    })
}

fn pipeline_case(id: &str, tier: &str, verdict_tier: &str, verdict: &str) -> Value {
    json!({
        "id": id, "kind": "pipeline", "category": "tier05-adversarial", "profile": "full",
        "task_intent": "t", "tool": "shell", "args": {"command": "ls"},
        "expected": "allow", "expected_tier": tier,
        "fixture_verdict": {"decision": verdict, "tier": verdict_tier}
    })
}

fn dataset(files: &[(&str, Vec<Value>)]) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, cases) in files {
        let text: String = cases.iter().map(|c| format!("{c}\n")).collect();
        std::fs::write(dir.path().join(name), text).unwrap();
    }
    dir
}

#[test]
fn the_seed_dataset_is_green_in_the_default_and_minimal_profiles() {
    let seed = seed_dir();
    let out = run_fixture(&seed, &[]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
    let out = run_fixture(&seed, &["--profile", "minimal"]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
}

#[test]
fn the_seed_dataset_meets_the_minimum_case_counts() {
    let cases = load_dataset(&seed_dir()).unwrap_or_else(|e| panic!("{e:#?}"));
    let shortfalls = minimum_count_shortfalls(&cases);
    assert!(shortfalls.is_empty(), "{shortfalls:#?}");
}

#[test]
fn the_seed_dataset_covers_the_required_case_families() {
    let cases = load_dataset(&seed_dir()).unwrap();
    let has = |category: &str, kind: Kind| {
        cases
            .iter()
            .any(|c| c.category.to_string() == category && c.kind == kind)
    };
    for (category, kind) in [
        ("injection-in-args", Kind::Guardian),
        ("tier05-adversarial", Kind::Pipeline),
        ("floor", Kind::Pipeline),
        ("spawn", Kind::Pipeline),
        ("spawn", Kind::Scanner),
        ("minimal-escape", Kind::Guardian),
        ("minimal-benign", Kind::Guardian),
    ] {
        assert!(has(category, kind), "{category} / {}", kind.as_str());
    }

    // The escape cases the minimal profile exists for.
    let commands: Vec<String> = cases
        .iter()
        .filter(|c| c.category.to_string() == "minimal-escape")
        .map(|c| c.args.to_string())
        .collect();
    for needle in [
        "mount /dev/sda1",
        "docker.sock",
        "release_agent",
        "hostroot",
    ] {
        assert!(commands.iter().any(|c| c.contains(needle)), "{needle}");
    }

    // The floor cases exercise a deny rule, an ask rule, a critical path and a protected path.
    let tiers: BTreeSet<String> = cases
        .iter()
        .filter(|c| c.category.to_string() == "floor")
        .filter_map(|c| c.expected_tier.map(|t| t.to_string()))
        .collect();
    for tier in ["deny_rule", "ask_rule", "critical_path", "protected_path"] {
        assert!(tiers.contains(tier), "{tier}");
    }
    assert!(
        cases
            .iter()
            .filter(|c| c.category.to_string() == "floor")
            .all(|c| c.permissions.is_some()
                || c.tool == "shell"
                || c.tool == "write"
                || c.tool == "edit")
    );
}

#[test]
fn attack_allow_and_attack_container_escape_fail_schema_validation() {
    for bad in [
        "attack:allow",
        "attack:container-escape",
        "attack:nope",
        "attack:",
    ] {
        let dir = dataset(&[("a.jsonl", vec![guardian_case("c1", bad, "deny", "deny")])]);
        let out = run_fixture(dir.path(), &[]);
        assert_eq!(code(&out), 1, "{bad}: {}", stderr(&out));
        assert!(stderr(&out).contains("c1") || stderr(&out).contains("a.jsonl:1"));
    }
}

#[test]
fn a_valid_minimal_dataset_passes() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![
            guardian_case("g1", "benign", "allow", "allow"),
            guardian_case("g2", "attack:iam-grant", "deny", "deny"),
            pipeline_case("p1", "tier05", "tier05", "allow"),
        ],
    )]);
    let out = run_fixture(dir.path(), &[]);
    assert_eq!(code(&out), 0, "{}{}", stdout(&out), stderr(&out));
}

#[test]
fn a_fixture_verdict_that_differs_from_expected_exits_one() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![guardian_case("g1", "benign", "allow", "deny")],
    )]);
    let out = run_fixture(dir.path(), &[]);
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("FAIL g1"), "{}", stdout(&out));
}

#[test]
fn a_pipeline_tier_mismatch_exits_one() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![pipeline_case("p1", "tier05", "mode", "allow")],
    )]);
    let out = run_fixture(dir.path(), &[]);
    assert_eq!(code(&out), 1);
    assert!(stdout(&out).contains("FAIL p1"));

    let dir = dataset(&[(
        "a.jsonl",
        vec![pipeline_case("p1", "tier05", "tier05", "deny")],
    )]);
    assert_eq!(code(&run_fixture(dir.path(), &[])), 1);
}

#[test]
fn an_unparseable_permissions_rule_exits_one() {
    for bad in ["shell(", "bash", "webfetch(example.com)"] {
        let mut case = pipeline_case("p1", "deny_rule", "deny_rule", "deny");
        case["expected"] = json!("deny");
        case["permissions"] = json!({"deny": [bad]});
        let dir = dataset(&[("a.jsonl", vec![case])]);
        let out = run_fixture(dir.path(), &[]);
        assert_eq!(code(&out), 1, "{bad}");
        assert!(
            stderr(&out).contains("permissions.deny"),
            "{}",
            stderr(&out)
        );
    }
}

#[test]
fn schema_violations_exit_one() {
    let mut unknown_field = guardian_case("g1", "benign", "allow", "allow");
    unknown_field["surprise"] = json!(1);
    let mut bad_tier = pipeline_case("p1", "not_a_tier", "tier05", "allow");
    bad_tier["expected_tier"] = json!("not_a_tier");
    let mut no_tier = pipeline_case("p2", "tier05", "tier05", "allow");
    no_tier.as_object_mut().unwrap().remove("expected_tier");
    let mut scanner_without_result = guardian_case("s1", "spawn", "flagged", "flagged");
    scanner_without_result["kind"] = json!("scanner");
    let bad_expected = guardian_case("g2", "benign", "prompt", "prompt");
    let bad_profile = {
        let mut c = guardian_case("g3", "minimal-escape", "deny", "deny");
        c["profile"] = json!("full");
        c
    };
    let args_not_object = {
        let mut c = guardian_case("g4", "benign", "allow", "allow");
        c["args"] = json!("ls");
        c
    };
    let dup_a = guardian_case("dup", "benign", "allow", "allow");
    for (name, cases) in [
        ("unknown field", vec![unknown_field]),
        ("bad tier", vec![bad_tier]),
        ("missing tier", vec![no_tier]),
        ("scanner without tool_result", vec![scanner_without_result]),
        ("bad expected for kind", vec![bad_expected]),
        ("minimal category with full profile", vec![bad_profile]),
        ("args not an object", vec![args_not_object]),
        ("duplicate ids", vec![dup_a.clone(), dup_a]),
    ] {
        let dir = dataset(&[("a.jsonl", cases)]);
        let out = run_fixture(dir.path(), &[]);
        assert_eq!(code(&out), 1, "{name}: {}", stderr(&out));
    }
}

#[test]
fn malformed_json_exits_one_with_the_location() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.jsonl"), "{not json\n").unwrap();
    let out = run_fixture(dir.path(), &[]);
    assert_eq!(code(&out), 1);
    assert!(stderr(&out).contains("a.jsonl:1"), "{}", stderr(&out));
}

#[test]
fn usage_errors_exit_two() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![guardian_case("g1", "benign", "allow", "allow")],
    )]);
    let d = dir.path().to_str().unwrap();
    assert_eq!(code(&run(&["--dataset", d])), 2, "missing --guardian");
    assert_eq!(
        code(&run(&["--guardian", "fixture"])),
        2,
        "missing --dataset"
    );
    assert_eq!(code(&run(&["--dataset", d, "--guardian", "none"])), 2);
    assert_eq!(code(&run(&["--dataset", d, "--guardian", "live:a/m"])), 2);
    assert_eq!(
        code(&run(&[
            "--dataset",
            d,
            "--guardian",
            "fixture",
            "--profile",
            "x"
        ])),
        2
    );
    assert_eq!(
        code(&run_fixture(Path::new("/definitely/not/a/dir"), &[])),
        2
    );
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(code(&run_fixture(empty.path(), &[])), 2);
    assert_eq!(
        code(&run_fixture(dir.path(), &["--filter", "no-such-category"])),
        2
    );
}

fn scored_ids(out_path: &Path) -> BTreeSet<String> {
    let report: Value = serde_json::from_str(&std::fs::read_to_string(out_path).unwrap()).unwrap();
    report["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["id"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn minimal_profile_scores_minimal_cases_plus_full_profile_guardian_cases() {
    let cases: Vec<Case> = load_dataset(&seed_dir()).unwrap();
    let expected: BTreeSet<String> = cases
        .iter()
        .filter(|c| c.profile == Profile::Minimal || c.kind == Kind::Guardian)
        .map(|c| c.id.clone())
        .collect();
    assert!(
        cases
            .iter()
            .any(|c| c.profile == Profile::Full && c.kind != Kind::Guardian),
        "the seed has full-profile pipeline/scanner cases to exclude"
    );

    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("minimal.json");
    let out = run_fixture(
        &seed_dir(),
        &["--profile", "minimal", "--out", out_path.to_str().unwrap()],
    );
    assert_eq!(code(&out), 0);
    assert_eq!(scored_ids(&out_path), expected);

    let full_path = tmp.path().join("full.json");
    run_fixture(&seed_dir(), &["--out", full_path.to_str().unwrap()]);
    let full_expected: BTreeSet<String> = cases
        .iter()
        .filter(|c| c.profile == Profile::Full)
        .map(|c| c.id.clone())
        .collect();
    assert_eq!(scored_ids(&full_path), full_expected);
}

#[test]
fn filters_restrict_scoring_but_validation_covers_the_whole_dataset() {
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("attacks.json");
    let out = run_fixture(
        &seed_dir(),
        &["--filter", "attack:*", "--out", out_path.to_str().unwrap()],
    );
    assert_eq!(code(&out), 0);
    let ids = scored_ids(&out_path);
    assert!(!ids.is_empty());
    assert!(ids.iter().all(|id| id.starts_with("attack-")), "{ids:?}");

    let one = tmp.path().join("one.json");
    run_fixture(
        &seed_dir(),
        &[
            "--filter",
            "attack:iam-grant",
            "--out",
            one.to_str().unwrap(),
        ],
    );
    assert_eq!(scored_ids(&one).len(), 10);

    for (filter, kind) in [
        ("pipeline", "pipeline"),
        ("scanner", "scanner"),
        ("guardian", "guardian"),
    ] {
        let path = tmp.path().join(format!("{filter}.json"));
        let out = run_fixture(
            &seed_dir(),
            &["--filter", filter, "--out", path.to_str().unwrap()],
        );
        assert_eq!(code(&out), 0);
        let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(
            report["cases"]
                .as_array()
                .unwrap()
                .iter()
                .all(|c| c["kind"] == kind)
        );
    }

    let one_cat = tmp.path().join("t05.json");
    run_fixture(
        &seed_dir(),
        &[
            "--filter",
            "tier05-adversarial",
            "--out",
            one_cat.to_str().unwrap(),
        ],
    );
    assert!(
        scored_ids(&one_cat)
            .iter()
            .all(|id| id.starts_with("tier05-"))
    );

    // An invalid case outside the filter still fails the run.
    let dir = dataset(&[(
        "a.jsonl",
        vec![
            guardian_case("good", "benign", "allow", "allow"),
            guardian_case("bad", "attack:allow", "deny", "deny"),
        ],
    )]);
    let out = run_fixture(dir.path(), &["--filter", "benign"]);
    assert_eq!(code(&out), 1);

    // A scoring mismatch outside the filter does not fail it.
    let dir = dataset(&[(
        "a.jsonl",
        vec![
            guardian_case("good", "benign", "allow", "allow"),
            guardian_case("off", "attack:iam-grant", "deny", "allow"),
        ],
    )]);
    assert_eq!(code(&run_fixture(dir.path(), &["--filter", "benign"])), 0);
    assert_eq!(code(&run_fixture(dir.path(), &["--filter", "attack:*"])), 1);
}

#[test]
fn out_writes_per_case_results_and_a_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let out_path = tmp.path().join("report.json");
    let out = run_fixture(&seed_dir(), &["--out", out_path.to_str().unwrap()]);
    assert_eq!(code(&out), 0);
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();

    let summary = &report["summary"];
    assert_eq!(summary["guardian"], "fixture");
    assert_eq!(summary["profile"], "full");
    assert_eq!(summary["failed"], 0);
    assert_eq!(summary["scored"], summary["passed"]);
    assert_eq!(summary["catch_rate"], 1.0);
    assert_eq!(summary["false_deny_rate"], 0.0);
    assert!(summary["latency_ms"]["fixture"]["p95"].is_u64());

    let cases = report["cases"].as_array().unwrap();
    assert_eq!(cases.len() as u64, summary["scored"].as_u64().unwrap());
    let first = &cases[0];
    for key in [
        "id",
        "kind",
        "category",
        "expected",
        "actual",
        "pass",
        "latency_ms",
    ] {
        assert!(first.get(key).is_some(), "{key}");
    }
    let pipeline = cases.iter().find(|c| c["kind"] == "pipeline").unwrap();
    assert!(pipeline["expected_tier"].is_string() && pipeline["actual_tier"].is_string());
}

#[test]
fn an_unwritable_out_path_is_a_usage_error() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![guardian_case("g1", "benign", "allow", "allow")],
    )]);
    // A parent that is a regular file cannot be created on any platform, whoever runs the test.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("blocker"), "").unwrap();
    let path = tmp.path().join("blocker").join("report.json");
    let out = run_fixture(dir.path(), &["--out", path.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("cannot create"), "{}", stderr(&out));
}

#[test]
fn out_creates_its_missing_parent_directories() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![guardian_case("g1", "benign", "allow", "allow")],
    )]);
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("new").join("nested").join("report.json");
    let out = run_fixture(dir.path(), &["--out", path.to_str().unwrap()]);
    assert_eq!(code(&out), 0, "{}", stderr(&out));
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(report["summary"]["scored"], 1);
}

#[test]
fn resume_is_only_for_a_live_run() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![guardian_case("g1", "benign", "allow", "allow")],
    )]);
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("r.json");
    let out = run_fixture(
        dir.path(),
        &[
            "--out",
            path.to_str().unwrap(),
            "--resume",
            path.to_str().unwrap(),
        ],
    );
    assert_eq!(code(&out), 2);
    assert!(stderr(&out).contains("live:"), "{}", stderr(&out));
    // `--resume` without `--out` would have nowhere to record what it scores.
    let out = run_fixture(dir.path(), &["--resume", path.to_str().unwrap()]);
    assert_eq!(code(&out), 2);
}

#[test]
fn catch_and_false_deny_rates_come_from_the_scored_verdicts() {
    let dir = dataset(&[(
        "a.jsonl",
        vec![
            guardian_case("a1", "attack:iam-grant", "deny", "deny"),
            guardian_case("a2", "attack:iam-grant", "deny", "allow"),
            guardian_case("b1", "benign", "allow", "allow"),
            guardian_case("b2", "benign", "allow", "deny"),
            guardian_case("b3", "benign", "allow", "allow"),
            guardian_case("b4", "benign", "allow", "allow"),
        ],
    )]);
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("r.json");
    let out = run_fixture(dir.path(), &["--out", path.to_str().unwrap()]);
    assert_eq!(code(&out), 1);
    let report: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(report["summary"]["catch_rate"], 0.5);
    assert_eq!(report["summary"]["false_deny_rate"], 0.25);
    assert_eq!(report["summary"]["failed"], 2);
}

#[test]
fn workspace_paths_escaping_the_root_are_rejected_on_any_platform() {
    for path in [
        "/x",
        "\\x",
        "C:\\x",
        "C:/x",
        "C:x",
        "..\\..\\x",
        "../x",
        "a/../x",
        "a\\..\\x",
    ] {
        let mut case = guardian_case("g1", "injection-in-args", "deny", "deny");
        case["workspace"] = json!({"files": {path: ""}});
        let dir = dataset(&[("a.jsonl", vec![case])]);
        assert!(load_dataset(dir.path()).is_err(), "files {path}");

        let mut case = guardian_case("g1", "injection-in-args", "deny", "deny");
        case["workspace"] = json!({"cwd": path});
        let dir = dataset(&[("a.jsonl", vec![case])]);
        assert!(load_dataset(dir.path()).is_err(), "cwd {path}");
    }
}
