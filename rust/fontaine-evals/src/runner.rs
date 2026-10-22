use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fontaine_core::config::PermissionMode;
use serde::Serialize;

use crate::baseline;
use crate::case::{Case, FixtureVerdict, Kind, Profile, load_dataset};
use crate::live::{self, Live, Scored};
use crate::none::{self, Outcome};
use crate::report::{self, Scorecard};
use crate::{EXIT_FAIL, EXIT_USAGE};

/// Where the inclusion-bar writer puts qualifying entries when `--models-file` is not given,
/// relative to the invocation directory `rust/`.
pub const DEFAULT_MODELS_FILE: &str = "fontaine-guardian/data/guardian_models.toml";

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProfileArg {
    Full,
    Minimal,
}

#[derive(Parser, Debug)]
#[command(
    name = "fontaine-evals",
    version,
    about = "Score guardian verdicts and the permission pipeline against the golden dataset",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Differential baselines against `claude -p`
    Baseline {
        #[command(subcommand)]
        action: BaselineAction,
    },
}

#[derive(Subcommand, Debug)]
enum BaselineAction {
    /// Record what `claude -p` does with each pipeline case
    Capture {
        /// Directory of `*.jsonl` cases
        #[arg(long, value_name = "DIR")]
        dataset: PathBuf,

        /// Restrict to a kind or a category glob
        #[arg(long, value_name = "KIND|CATEGORY-GLOB")]
        filter: Option<String>,

        /// Baseline file to write, normally under `../evals/baselines/`
        #[arg(long, value_name = "PATH")]
        out: PathBuf,

        /// The Claude Code executable
        #[arg(long, value_name = "PATH", default_value = "claude")]
        claude: String,
    },
    /// Report every case whose outcome differs between a baseline and a Fontaine run
    Diff {
        /// A baseline file written by `baseline capture`
        baseline: PathBuf,
        /// A `--guardian none --out` file
        run: PathBuf,
    },
}

#[derive(Args, Debug)]
struct RunArgs {
    /// Directory of `*.jsonl` cases
    #[arg(long, value_name = "DIR")]
    dataset: Option<PathBuf>,

    /// Which guardian scores the cases: `fixture` (canned verdicts), `none` (the real
    /// permission pipeline, no guardian) or `live:<account>/<model>` (a live guardian model)
    #[arg(long, value_name = "MODE")]
    guardian: Option<String>,

    /// Guardian profile under test
    #[arg(long, value_enum, default_value = "full")]
    profile: ProfileArg,

    /// Restrict scoring to a kind (`guardian`, `pipeline`, `scanner`) or a category glob
    #[arg(long, value_name = "KIND|CATEGORY-GLOB")]
    filter: Option<String>,

    /// Write per-case results and the summary as JSON
    #[arg(long, value_name = "PATH")]
    out: Option<PathBuf>,

    /// Where an unfiltered `--profile full` live run writes the guardian models that clear the
    /// inclusion bar
    #[arg(long, value_name = "PATH", default_value = DEFAULT_MODELS_FILE)]
    models_file: PathBuf,
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
    /// The permission mode a `none` run scored the case in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub pass: bool,
    pub latency_ms: u64,
    /// The guardian's own words, in a live run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
    /// The `class=<name>` tag of a `minimal-escape` verdict.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub escape_class: Option<String>,
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
    /// Catch rate per attack class.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub catch_by_class: BTreeMap<String, Option<f64>>,
    /// Catch rate per escape class, from the `class=<name>` tag in the verdict explanation.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub escape_classes: BTreeMap<String, Option<f64>>,
    /// The dataset the numbers are for, in a live run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dataset_version: Option<String>,
    pub latency_ms: BTreeMap<String, Latency>,
    /// Cases selected but not scored, with the reason.
    pub skipped: Vec<Skipped>,
}

#[derive(Debug, Serialize)]
pub struct Skipped {
    pub id: String,
    pub reason: String,
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
    match cli.command {
        Some(Command::Baseline { action }) => match action {
            BaselineAction::Capture {
                dataset,
                filter,
                out,
                claude,
            } => run_capture(&dataset, filter.as_deref(), &out, &claude),
            BaselineAction::Diff { baseline, run } => run_diff(&baseline, &run),
        },
        None => run_score(&cli.run),
    }
}

fn usage(message: &str) -> u8 {
    eprintln!("fontaine-evals: {message}");
    EXIT_USAGE
}

fn load_or_report(dir: &std::path::Path) -> Result<Vec<Case>, u8> {
    // Every case is validated, whatever the profile or filter selects.
    load_dataset(dir).map_err(|errors| {
        for e in &errors {
            eprintln!("{e}");
        }
        let unreadable = errors
            .iter()
            .any(|e| e.starts_with("cannot read dataset directory") || e.starts_with("no *.jsonl"));
        eprintln!("fontaine-evals: {} dataset problem(s)", errors.len());
        if unreadable { EXIT_USAGE } else { EXIT_FAIL }
    })
}

fn runtime() -> Result<tokio::runtime::Runtime, u8> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| usage(&format!("cannot start the async runtime: {e}")))
}

fn run_score(args: &RunArgs) -> u8 {
    let (Some(dataset), Some(guardian)) = (&args.dataset, &args.guardian) else {
        return usage("`--dataset` and `--guardian` are required (or use a subcommand: baseline)");
    };
    let live_spec = guardian.strip_prefix("live:");
    if !matches!(guardian.as_str(), "fixture" | "none") && live_spec.is_none() {
        return usage(&format!(
            "unsupported --guardian `{guardian}`; supported modes are `fixture`, `none` and `live:<account>/<model>`"
        ));
    }
    let cases = match load_or_report(dataset) {
        Ok(cases) => cases,
        Err(code) => return code,
    };

    let profile = match args.profile {
        ProfileArg::Full => Profile::Full,
        ProfileArg::Minimal => Profile::Minimal,
    };
    let mut selected: Vec<&Case> = cases
        .iter()
        .filter(|c| in_profile(c, profile))
        .filter(|c| args.filter.as_deref().is_none_or(|f| matches_filter(c, f)))
        .collect();
    if selected.is_empty() {
        return usage("no cases selected by --profile/--filter");
    }

    let mut skipped = Vec::new();
    let mut live_run = None;
    let results: Vec<CaseResult> = if let Some(spec) = live_spec {
        let connected = match live::connect(
            spec,
            match profile {
                Profile::Full => fontaine_guardian::Profile::Full,
                Profile::Minimal => fontaine_guardian::Profile::Minimal,
            },
            &|name| std::env::var_os(name),
        ) {
            Ok(connected) => connected,
            Err(e) => return usage(&e),
        };
        let rt = match runtime() {
            Ok(rt) => rt,
            Err(code) => return code,
        };
        let (results, scored, missed) = match rt.block_on(score_live(&selected, &connected)) {
            Ok(done) => done,
            Err(e) => return usage(&e),
        };
        skipped = missed;
        selected = scored;
        live_run = Some(connected);
        results
    } else if guardian == "none" {
        // The guardian and the scanner do not exist in this mode, and `auto` needs the guardian.
        selected.retain(|c| c.kind == Kind::Pipeline);
        if selected.is_empty() {
            return usage("`--guardian none` scores pipeline cases; none were selected");
        }
        let rt = match runtime() {
            Ok(rt) => rt,
            Err(code) => return code,
        };
        let mut results = Vec::new();
        let mut scored = Vec::new();
        for case in &selected {
            if case.mode == Some(PermissionMode::Auto) {
                skipped.push(Skipped {
                    id: case.id.clone(),
                    reason: "auto mode needs the guardian".into(),
                });
                continue;
            }
            match rt.block_on(none::decide(case, None)) {
                Ok(Outcome::Decided {
                    decision,
                    tier,
                    latency_ms,
                }) => {
                    results.push(score_pipeline(case, decision, tier, latency_ms));
                    scored.push(*case);
                }
                Ok(Outcome::Skipped(reason)) => skipped.push(Skipped {
                    id: case.id.clone(),
                    reason,
                }),
                Err(e) => return usage(&format!("case {}: {e}", case.id)),
            }
        }
        selected = scored;
        results
    } else {
        selected.iter().map(|c| score_fixture(c)).collect()
    };
    let mut summary = summarize(
        guardian,
        args.filter.clone(),
        profile,
        cases.len(),
        &selected,
        &results,
        skipped,
        live_run.is_some(),
    );

    if let Some(live) = &live_run {
        match dataset_version_and_models(args, dataset, live, profile, &summary) {
            Ok(version) => summary.dataset_version = Some(version),
            Err(e) => return usage(&e),
        }
    }
    print_summary(&summary, &results);
    if let Some(out) = &args.out {
        let report = Report {
            summary: &summary,
            cases: &results,
        };
        let json = serde_json::to_string_pretty(&report).expect("report serializes");
        if let Err(e) = std::fs::write(out, json + "\n") {
            return usage(&format!("cannot write {}: {e}", out.display()));
        }
    }
    // A live run reports and exits 0 whatever it scored: it is a measurement, not a gate.
    if live_run.is_none() && summary.failed > 0 {
        EXIT_FAIL
    } else {
        0
    }
}

/// Scores `selected` against the live guardian, one case at a time so each latency is its own.
/// Returns the results, the cases they belong to and the cases that could not be scored.
async fn score_live<'a>(
    selected: &[&'a Case],
    live: &Live,
) -> Result<(Vec<CaseResult>, Vec<&'a Case>, Vec<Skipped>), String> {
    let (mut results, mut scored, mut skipped) = (Vec::new(), Vec::new(), Vec::new());
    for case in selected {
        match case.kind {
            Kind::Guardian => match live::adjudicate(case, live).await? {
                Scored::Result(result) => {
                    results.push(*result);
                    scored.push(*case);
                }
                Scored::Skipped(reason) => skipped.push(Skipped {
                    id: case.id.clone(),
                    reason,
                }),
            },
            Kind::Pipeline => match none::decide(case, Some(live.guardian.clone())).await {
                Ok(Outcome::Decided {
                    decision,
                    tier,
                    latency_ms,
                }) => {
                    results.push(score_pipeline(case, decision, tier, latency_ms));
                    scored.push(*case);
                }
                Ok(Outcome::Skipped(reason)) => skipped.push(Skipped {
                    id: case.id.clone(),
                    reason,
                }),
                Err(e) => return Err(format!("case {}: {e}", case.id)),
            },
            Kind::Scanner => skipped.push(Skipped {
                id: case.id.clone(),
                reason: "scanner cases are scored once the tool-result scanner exists".into(),
            }),
        }
    }
    Ok((results, scored, skipped))
}

/// The dataset version of this run, and, on an unfiltered full-profile run, the update of the
/// models file.
fn dataset_version_and_models(
    args: &RunArgs,
    dataset: &std::path::Path,
    live: &Live,
    profile: Profile,
    summary: &Summary,
) -> Result<String, String> {
    let version = report::dataset_version(dataset)
        .map_err(|e| format!("cannot read the dataset {}: {e}", dataset.display()))?;
    if profile != Profile::Full || args.filter.is_some() {
        return Ok(version);
    }
    let p95 = summary
        .latency_ms
        .get(&summary.guardian)
        .map_or(0, |latency| latency.p95);
    let rates = report::Rates {
        catch_rate: summary.catch_rate,
        false_deny_rate: summary.false_deny_rate,
        by_class: summary.catch_by_class.clone(),
    };
    let today = report::today();
    let card = Scorecard {
        kind: live.kind,
        model: &live.model,
        rates: &rates,
        p95_ms: p95,
        scored_on: &today,
        dataset_version: &version,
    };
    let existing = report::read(&args.models_file)?;
    let merged = report::merge(existing, &card);
    for line in &merged.dropped {
        eprintln!("fontaine-evals: dropped from the models file: {line}");
    }
    report::write(&args.models_file, &merged.file)?;
    let kept = merged
        .file
        .models
        .iter()
        .any(|m| m.kind == live.kind && m.model == live.model);
    println!(
        "{}: {} the inclusion bar ({} models in {})",
        live.label,
        if kept { "clears" } else { "does not clear" },
        merged.file.models.len(),
        args.models_file.display()
    );
    Ok(version)
}

fn score_pipeline(case: &Case, decision: String, tier: String, latency_ms: u64) -> CaseResult {
    let expected_tier = case.expected_tier.map(|t| t.to_string());
    CaseResult {
        id: case.id.clone(),
        kind: case.kind.as_str(),
        category: case.category.to_string(),
        pass: decision == case.expected && expected_tier.as_deref() == Some(tier.as_str()),
        expected: case.expected.clone(),
        actual: decision,
        expected_tier,
        actual_tier: Some(tier),
        mode: Some(
            case.mode
                .unwrap_or(PermissionMode::Manual)
                .as_str()
                .to_string(),
        ),
        latency_ms,
        explanation: None,
        escape_class: None,
    }
}

fn run_capture(
    dataset: &std::path::Path,
    filter: Option<&str>,
    out: &std::path::Path,
    claude: &str,
) -> u8 {
    let cases = match load_or_report(dataset) {
        Ok(cases) => cases,
        Err(code) => return code,
    };
    let selected: Vec<&Case> = cases
        .iter()
        .filter(|c| c.kind == Kind::Pipeline)
        .filter(|c| filter.is_none_or(|f| matches_filter(c, f)))
        .collect();
    if selected.is_empty() {
        return usage("no pipeline cases selected by --filter");
    }
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let report = match rt.block_on(baseline::capture(claude, &selected)) {
        Ok(report) => report,
        Err(e) => return usage(&e),
    };
    if let Err(e) = baseline::write_records(out, &report.records) {
        return usage(&format!("cannot write {}: {e}", out.display()));
    }
    for (id, why) in &report.skipped {
        println!("SKIP {id}: {why}");
    }
    for (id, why) in &report.failed {
        println!("FAIL {id}: {why}");
    }
    println!(
        "captured={} skipped={} failed={} out={}",
        report.records.len(),
        report.skipped.len(),
        report.failed.len(),
        out.display()
    );
    if report.failed.is_empty() {
        0
    } else {
        EXIT_FAIL
    }
}

fn run_diff(baseline_path: &std::path::Path, run_path: &std::path::Path) -> u8 {
    let records = match baseline::read_records(baseline_path) {
        Ok(records) => records,
        Err(e) => return usage(&e),
    };
    let run_json = match std::fs::read_to_string(run_path) {
        Ok(text) => text,
        Err(e) => return usage(&format!("{}: {e}", run_path.display())),
    };
    let report = match baseline::diff(&records, &run_json) {
        Ok(report) => report,
        Err(e) => return usage(&e),
    };
    for d in &report.divergences {
        println!(
            "DIFF {} [{}]: fontaine {} ({}), claude {}",
            d.case_id,
            d.mode,
            d.fontaine,
            baseline::fontaine_outcome(&d.fontaine).as_str(),
            d.claude.as_str()
        );
    }
    for id in &report.unmatched {
        println!("UNMATCHED {id}: not in the Fontaine run");
    }
    println!(
        "compared={} differing={} unmatched={}",
        report.compared,
        report.divergences.len(),
        report.unmatched.len()
    );
    if report.divergences.is_empty() {
        0
    } else {
        EXIT_FAIL
    }
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
        mode: None,
        latency_ms: 0,
        explanation: None,
        escape_class: None,
    }
}

fn percentile(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64) * p).ceil() as usize;
    sorted[rank.clamp(1, sorted.len()) - 1]
}

#[allow(clippy::too_many_arguments)]
fn summarize(
    guardian: &str,
    filter: Option<String>,
    profile: Profile,
    dataset_cases: usize,
    selected: &[&Case],
    results: &[CaseResult],
    skipped: Vec<Skipped>,
    live: bool,
) -> Summary {
    let mut by_category = BTreeMap::new();
    for case in selected {
        *by_category.entry(case.category.to_string()).or_insert(0) += 1;
    }
    let rates = report::rates(selected, results);

    // The latency a live scorecard reports is the guardian's: the cases that were adjudicated.
    let mut latencies: Vec<u64> = selected
        .iter()
        .zip(results)
        .filter(|(case, _)| !live || case.kind == Kind::Guardian)
        .map(|(_, r)| r.latency_ms)
        .collect();
    latencies.sort_unstable();
    let passed = results.iter().filter(|r| r.pass).count();
    Summary {
        guardian: guardian.to_string(),
        profile: match profile {
            Profile::Full => "full",
            Profile::Minimal => "minimal",
        },
        filter,
        dataset_cases,
        scored: results.len(),
        passed,
        failed: results.len() - passed,
        by_category,
        catch_rate: rates.catch_rate,
        false_deny_rate: rates.false_deny_rate,
        catch_by_class: rates.by_class,
        escape_classes: report::escape_classes(selected, results),
        dataset_version: None,
        latency_ms: BTreeMap::from([(
            guardian.to_string(),
            Latency {
                p50: percentile(&latencies, 0.50),
                p95: percentile(&latencies, 0.95),
            },
        )]),
        skipped,
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
    for s in &summary.skipped {
        println!("SKIP {}: {}", s.id, s.reason);
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
    if let Some(latency) = summary.latency_ms.get(&summary.guardian)
        && summary.guardian.starts_with("live:")
    {
        println!("latency_ms p50={} p95={}", latency.p50, latency.p95);
    }
    for (class, rate) in &summary.catch_by_class {
        println!("  attack:{class} catch_rate={}", pct(*rate));
    }
    for (class, rate) in &summary.escape_classes {
        println!("  escape:{class} catch_rate={}", pct(*rate));
    }
}
