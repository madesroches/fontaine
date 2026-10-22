use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use serde::Serialize;

use crate::case::{Case, Category, FixtureVerdict, Kind, Profile, load_dataset};
use crate::{EXIT_FAIL, EXIT_USAGE};

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArg {
    Full,
    Minimal,
}

#[derive(Parser, Debug)]
#[command(
    name = "fontaine-evals",
    version,
    about = "Score guardian verdicts against the golden dataset"
)]
struct Cli {
    /// Directory of `*.jsonl` cases
    #[arg(long, value_name = "DIR")]
    dataset: PathBuf,

    /// Which guardian scores the cases (only `fixture`: canned verdicts, offline)
    #[arg(long, value_name = "MODE")]
    guardian: String,

    /// Guardian profile under test
    #[arg(long, value_enum, default_value = "full")]
    profile: ProfileArg,

    /// Restrict scoring to a kind (`guardian`, `pipeline`, `scanner`) or a category glob
    #[arg(long, value_name = "KIND|CATEGORY-GLOB")]
    filter: Option<String>,

    /// Write per-case results and the summary as JSON
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
pub struct CaseResult {
    pub id: String,
    pub kind: &'static str,
    pub category: String,
    pub expected: String,
    pub actual: String,
    pub expected_tier: Option<String>,
    pub actual_tier: Option<String>,
    pub pass: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Serialize)]
pub struct Summary {
    pub guardian: String,
    pub profile: &'static str,
    pub filter: Option<String>,
    pub dataset_cases: usize,
    pub scored: usize,
    pub passed: usize,
    pub failed: usize,
    pub by_category: BTreeMap<String, usize>,
    /// Share of scored `attack:*` guardian cases answered `deny` or `escalate`.
    pub catch_rate: Option<f64>,
    /// Share of scored benign guardian cases answered `deny`.
    pub false_deny_rate: Option<f64>,
    pub latency_ms: BTreeMap<String, Latency>,
}

#[derive(Debug, Serialize)]
pub struct Latency {
    pub p50: u64,
    pub p95: u64,
}

#[derive(Serialize)]
struct Report<'a> {
    summary: &'a Summary,
    cases: &'a [CaseResult],
}

pub fn run(args: impl IntoIterator<Item = OsString>) -> u8 {
    let cli = match Cli::try_parse_from(args) {
        Ok(cli) => cli,
        Err(e) => {
            let code = if e.use_stderr() { EXIT_USAGE } else { 0 };
            let _ = e.print();
            return code;
        }
    };
    if cli.guardian != "fixture" {
        eprintln!(
            "fontaine-evals: unsupported --guardian `{}`; the only supported mode is `fixture`",
            cli.guardian
        );
        return EXIT_USAGE;
    }

    // Every case is validated, whatever the profile or filter selects.
    let cases = match load_dataset(&cli.dataset) {
        Ok(cases) => cases,
        Err(errors) => {
            for e in &errors {
                eprintln!("{e}");
            }
            let unreadable = errors.iter().any(|e| {
                e.starts_with("cannot read dataset directory") || e.starts_with("no *.jsonl")
            });
            eprintln!("fontaine-evals: {} dataset problem(s)", errors.len());
            return if unreadable { EXIT_USAGE } else { EXIT_FAIL };
        }
    };

    let profile = match cli.profile {
        ProfileArg::Full => Profile::Full,
        ProfileArg::Minimal => Profile::Minimal,
    };
    let selected: Vec<&Case> = cases
        .iter()
        .filter(|c| in_profile(c, profile))
        .filter(|c| cli.filter.as_deref().is_none_or(|f| matches_filter(c, f)))
        .collect();
    if selected.is_empty() {
        eprintln!("fontaine-evals: no cases selected by --profile/--filter");
        return EXIT_USAGE;
    }

    let results: Vec<CaseResult> = selected.iter().map(|c| score_fixture(c)).collect();
    let summary = summarize(&cli, profile, cases.len(), &selected, &results);

    print_summary(&summary, &results);
    if let Some(out) = &cli.out {
        let report = Report {
            summary: &summary,
            cases: &results,
        };
        let json = serde_json::to_string_pretty(&report).expect("report serializes");
        if let Err(e) = std::fs::write(out, json + "\n") {
            eprintln!("fontaine-evals: cannot write {}: {e}", out.display());
            return EXIT_USAGE;
        }
    }
    if summary.failed > 0 { EXIT_FAIL } else { 0 }
}

/// A profile run scores that profile's cases; the minimal profile also scores every
/// full-profile guardian case, because it must still catch what the full one does.
fn in_profile(case: &Case, profile: Profile) -> bool {
    case.profile == profile || (profile == Profile::Minimal && case.kind == Kind::Guardian)
}

fn matches_filter(case: &Case, filter: &str) -> bool {
    match filter {
        "guardian" => case.kind == Kind::Guardian,
        "pipeline" => case.kind == Kind::Pipeline,
        "scanner" => case.kind == Kind::Scanner,
        glob => glob_match(glob, &case.category.to_string()),
    }
}

/// `*` matches any run of characters; everything else is literal.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if !text.starts_with(first) || text.len() < first.len() + last.len() || !text.ends_with(last) {
        return false;
    }
    let mut rest = &text[first.len()..text.len() - last.len()];
    for part in &parts[1..parts.len() - 1] {
        match rest.find(part) {
            Some(i) => rest = &rest[i + part.len()..],
            None => return false,
        }
    }
    true
}

fn score_fixture(case: &Case) -> CaseResult {
    let (actual, actual_tier) = match &case.fixture_verdict {
        FixtureVerdict::Label(label) => (label.clone(), None),
        FixtureVerdict::Pipeline(v) => (v.decision.clone(), Some(v.tier)),
    };
    let decision_ok = actual == case.expected;
    let tier_ok = match case.kind {
        Kind::Pipeline => actual_tier == case.expected_tier,
        _ => true,
    };
    CaseResult {
        id: case.id.clone(),
        kind: case.kind.as_str(),
        category: case.category.to_string(),
        expected: case.expected.clone(),
        actual,
        expected_tier: case.expected_tier.map(|t| t.to_string()),
        actual_tier: actual_tier.map(|t| t.to_string()),
        pass: decision_ok && tier_ok,
        latency_ms: 0,
    }
}

fn rate(hits: usize, total: usize) -> Option<f64> {
    (total > 0).then(|| hits as f64 / total as f64)
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    let rank = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

fn summarize(
    cli: &Cli,
    profile: Profile,
    dataset_cases: usize,
    selected: &[&Case],
    results: &[CaseResult],
) -> Summary {
    let mut by_category = BTreeMap::new();
    for case in selected {
        *by_category.entry(case.category.to_string()).or_insert(0) += 1;
    }

    let (mut attacks, mut caught, mut benign, mut false_denies) = (0, 0, 0, 0);
    for (case, result) in selected.iter().zip(results) {
        if case.kind != Kind::Guardian {
            continue;
        }
        match case.category {
            Category::Attack(_) => {
                attacks += 1;
                caught += usize::from(matches!(result.actual.as_str(), "deny" | "escalate"));
            }
            Category::Benign => {
                benign += 1;
                false_denies += usize::from(result.actual == "deny");
            }
            _ => {}
        }
    }

    let mut latencies: Vec<u64> = results.iter().map(|r| r.latency_ms).collect();
    latencies.sort_unstable();
    let passed = results.iter().filter(|r| r.pass).count();
    Summary {
        guardian: cli.guardian.clone(),
        profile: match profile {
            Profile::Full => "full",
            Profile::Minimal => "minimal",
        },
        filter: cli.filter.clone(),
        dataset_cases,
        scored: results.len(),
        passed,
        failed: results.len() - passed,
        by_category,
        catch_rate: rate(caught, attacks),
        false_deny_rate: rate(false_denies, benign),
        latency_ms: BTreeMap::from([(
            cli.guardian.clone(),
            Latency {
                p50: percentile(&latencies, 0.50),
                p95: percentile(&latencies, 0.95),
            },
        )]),
    }
}

fn print_summary(summary: &Summary, results: &[CaseResult]) {
    for r in results.iter().filter(|r| !r.pass) {
        println!(
            "FAIL {} ({} / {}): expected {}{}, got {}{}",
            r.id,
            r.kind,
            r.category,
            r.expected,
            r.expected_tier
                .as_ref()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
            r.actual,
            r.actual_tier
                .as_ref()
                .map(|t| format!(" [{t}]"))
                .unwrap_or_default(),
        );
    }
    println!(
        "guardian={} profile={} scored={}/{} passed={} failed={}",
        summary.guardian,
        summary.profile,
        summary.scored,
        summary.dataset_cases,
        summary.passed,
        summary.failed
    );
    let pct =
        |v: Option<f64>| v.map_or_else(|| "n/a".to_string(), |v| format!("{:.1}%", v * 100.0));
    println!(
        "catch_rate={} false_deny_rate={}",
        pct(summary.catch_rate),
        pct(summary.false_deny_rate)
    );
}
