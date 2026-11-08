#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use fontaine_evals::cache_bench::Scorecard;
use fontaine_evals::run;
use serde_json::{Value, json};

fn model_call(creation: u64) -> String {
    json!({"type": "model_call", "input_tokens": 1, "output_tokens": 1,
           "cache_read_input_tokens": 10, "cache_creation_input_tokens": creation,
           "calls_tool": true})
    .to_string()
}

fn assistant(id: &str, creation: u64) -> String {
    json!({"type": "assistant", "message": {"id": id, "content": [],
        "usage": {"input_tokens": 1, "output_tokens": 1, "cache_read_input_tokens": 10,
                  "cache_creation_input_tokens": creation}}})
    .to_string()
}

/// A script that records its argv and prints `stdout`; `--version` prints a version.
fn stub(dir: &Path, name: &str, stdout: &str) -> String {
    let argv = dir.join(format!("{name}.argv"));
    let body = dir.join(format!("{name}.out"));
    std::fs::write(&body, stdout).unwrap();
    let script = dir.join(name);
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo '9.9.9 (stub)'; exit 0; fi\n\
             for a in \"$@\"; do printf '%s\\n' \"$a\"; done > '{}'\ncat '{}'\n",
            argv.display(),
            body.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    script.display().to_string()
}

fn bench(dir: &Path, fontaine_creations: &[u64], claude_creations: &[u64], extra: &[&str]) -> u8 {
    bench_with(
        dir,
        fontaine_creations,
        claude_creations,
        "acct/sonnet",
        "4",
        extra,
    )
}

fn bench_with(
    dir: &Path,
    fontaine_creations: &[u64],
    claude_creations: &[u64],
    model: &str,
    files: &str,
    extra: &[&str],
) -> u8 {
    let fontaine_out = format!(
        "{}\n{}\n",
        json!({"type": "init", "model": "acct/sonnet"}),
        fontaine_creations
            .iter()
            .map(|c| model_call(*c))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let claude_out = format!(
        "{}\n{}\n",
        json!({"type": "system", "subtype": "init", "model": "claude-sonnet-x"}),
        claude_creations
            .iter()
            .enumerate()
            .map(|(i, c)| assistant(&format!("m{i}"), *c))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let fontaine = stub(dir, "fontaine", &fontaine_out);
    let claude = stub(dir, "claude", &claude_out);
    let mut args: Vec<String> = [
        "fontaine-evals",
        "cache-bench",
        "--model",
        model,
        "--files",
        files,
        "--fontaine",
        &fontaine,
        "--claude",
        &claude,
        "--out",
        dir.join("card.json").to_str().unwrap(),
    ]
    .map(String::from)
    .to_vec();
    args.extend(extra.iter().map(|s| s.to_string()));
    run(args.into_iter().map(Into::into))
}

fn argv(dir: &Path, name: &str) -> Vec<String> {
    std::fs::read_to_string(dir.join(format!("{name}.argv")))
        .unwrap()
        .lines()
        .map(String::from)
        .collect()
}

#[test]
fn an_unflagged_run_writes_the_scorecard_and_passes_the_shared_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let code = bench(
        dir.path(),
        &[900, 50, 220, 220, 50],
        &[900, 200, 200, 200, 50],
        &[],
    );
    assert_eq!(code, 0);
    let card: Scorecard =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("card.json")).unwrap())
            .unwrap();
    assert_eq!(card.summary.ratio, 1.1);
    assert!(!card.summary.flagged);
    assert_eq!(card.claude_version, "9.9.9 (stub)");
    assert_eq!(card.claude_served_model.as_deref(), Some("claude-sonnet-x"));
    assert_eq!(card.fontaine_served_model.as_deref(), Some("acct/sonnet"));

    let fontaine = argv(dir.path(), "fontaine");
    let claude = argv(dir.path(), "claude");
    assert_eq!(fontaine[0], "-p");
    assert_eq!(fontaine[1], claude[1], "both sides get the same prompt");
    assert!(fontaine[1].contains("f1.txt") && fontaine[1].contains("f4.txt"));
    let pair = |args: &[String], flag: &str| -> String {
        let at = args.iter().position(|a| a == flag).unwrap();
        args[at + 1].clone()
    };
    assert_eq!(pair(&fontaine, "--output-format"), "stream-json");
    assert_eq!(pair(&fontaine, "--model"), "acct/sonnet");
    assert_eq!(pair(&fontaine, "--allowed-tools"), "read");
    assert_eq!(pair(&claude, "--output-format"), "stream-json");
    assert_eq!(pair(&claude, "--model"), "sonnet");
    assert_eq!(pair(&claude, "--allowed-tools"), "Read");
    assert!(claude.contains(&"--verbose".to_string()));
    assert!(claude.contains(&"--no-session-persistence".to_string()));
}

#[test]
fn a_ratio_above_the_threshold_exits_one_and_still_writes_the_scorecard() {
    let dir = tempfile::tempdir().unwrap();
    let code = bench(
        dir.path(),
        &[900, 50, 600, 600, 50],
        &[900, 200, 200, 200, 50],
        &[],
    );
    assert_eq!(code, 1);
    let card: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.path().join("card.json")).unwrap())
            .unwrap();
    assert_eq!(card["summary"]["flagged"], true);
}

#[test]
fn the_threshold_flag_moves_the_verdict() {
    let dir = tempfile::tempdir().unwrap();
    let code = bench(
        dir.path(),
        &[900, 50, 600, 600, 50],
        &[900, 200, 200, 200, 50],
        &["--threshold", "3.5"],
    );
    assert_eq!(code, 0);
}

#[test]
fn a_side_with_too_few_steps_fails_the_bench() {
    let dir = tempfile::tempdir().unwrap();
    let code = bench(
        dir.path(),
        &[900, 300, 300, 50],
        &[900, 200, 200, 200, 50],
        &[],
    );
    assert_eq!(code, 1);
    assert!(!dir.path().join("card.json").exists());
}

#[test]
fn bad_flags_are_usage_errors() {
    let dir = tempfile::tempdir().unwrap();
    let steps = [1, 1, 1, 1, 1];
    assert_eq!(bench(dir.path(), &steps, &steps, &["--threshold", "0"]), 2);
    assert_eq!(
        bench_with(dir.path(), &steps, &steps, "acct/sonnet", "3", &[]),
        2
    );
    assert_eq!(
        bench_with(dir.path(), &steps, &steps, "nosplit", "4", &[]),
        2
    );
}
