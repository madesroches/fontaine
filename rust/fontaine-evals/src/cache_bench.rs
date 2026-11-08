//! Live prompt-cache bench: one fixed multi-step read task through `fontaine -p` on a
//! claude-subscription account and through `claude -p`, compared by per-step cache write.
//! Manual and nightly only; never run from tests or CI.

use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::baseline::{claude_version, events};

const TIMEOUT: Duration = Duration::from_secs(300);
const WORDS_PER_FILE: usize = 1200;
const WORDS_PER_LINE: usize = 12;
// `write_fixture` pads each word to 6 characters so files are equal-sized; none may be longer.
const WORDS: [&str; 32] = [
    "amber", "birch", "cedar", "delta", "ember", "fjord", "glade", "harbor", "ivory", "jewel",
    "karst", "lunar", "maple", "nectar", "ocean", "prism", "quartz", "river", "stone", "tulip",
    "umber", "vigor", "willow", "xenon", "yarrow", "zenith", "anvil", "basil", "coral", "dune",
    "elm", "flint",
];

/// One model call as the provider billed it, in tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Step {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub output: u64,
    pub calls_tool: bool,
}

/// What one side's stream held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Stream {
    /// The model the side reports having served, from its `init` event.
    pub model: Option<String>,
    pub steps: Vec<Step>,
}

fn count(usage: &Value, key: &str) -> u64 {
    usage.get(key).and_then(Value::as_u64).unwrap_or(0)
}

/// Steps from `fontaine -p --output-format stream-json`: one per `model_call` event.
pub fn parse_fontaine_stream(output: &str) -> Result<Stream, String> {
    let mut stream = Stream::default();
    for event in events(output)? {
        match event.get("type").and_then(Value::as_str) {
            Some("init") => stream.model = event["model"].as_str().map(str::to_string),
            Some("model_call") => {
                let cache_read = count(&event, "cache_read_input_tokens");
                let cache_creation = count(&event, "cache_creation_input_tokens");
                stream.steps.push(Step {
                    // Fontaine's input total includes the cached parts; claude's does not.
                    input: count(&event, "input_tokens")
                        .saturating_sub(cache_read + cache_creation),
                    cache_read,
                    cache_creation,
                    output: count(&event, "output_tokens"),
                    calls_tool: event["calls_tool"].as_bool().unwrap_or(false),
                });
            }
            _ => {}
        }
    }
    Ok(stream)
}

/// Steps from `claude -p --output-format stream-json --verbose`: one per distinct
/// `message.id`, keeping the last usage seen, since one message can span several events.
pub fn parse_claude_stream(output: &str) -> Result<Stream, String> {
    let mut stream = Stream::default();
    let mut ids: Vec<String> = Vec::new();
    for event in events(output)? {
        match event.get("type").and_then(Value::as_str) {
            Some("system") if event["subtype"] == "init" => {
                stream.model = event["model"].as_str().map(str::to_string);
            }
            Some("assistant") => {
                let message = &event["message"];
                let Some(id) = message["id"].as_str() else {
                    continue;
                };
                let usage = &message["usage"];
                let calls_tool = message["content"]
                    .as_array()
                    .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "tool_use"));
                let step = Step {
                    input: count(usage, "input_tokens"),
                    cache_read: count(usage, "cache_read_input_tokens"),
                    cache_creation: count(usage, "cache_creation_input_tokens"),
                    output: count(usage, "output_tokens"),
                    calls_tool,
                };
                match ids.iter().position(|seen| seen == id) {
                    // A later event of the message may carry only the text block.
                    Some(at) => {
                        let kept = stream.steps[at].calls_tool;
                        stream.steps[at] = Step {
                            calls_tool: kept || calls_tool,
                            ..step
                        };
                    }
                    None => {
                        ids.push(id.to_string());
                        stream.steps.push(step);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(stream)
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub input: u64,
    pub cache_read: u64,
    pub cache_creation: u64,
    pub output: u64,
}

pub fn totals(steps: &[Step]) -> Totals {
    steps.iter().fold(
        Totals {
            input: 0,
            cache_read: 0,
            cache_creation: 0,
            output: 0,
        },
        |t, s| Totals {
            input: t.input + s.input,
            cache_read: t.cache_read + s.cache_read,
            cache_creation: t.cache_creation + s.cache_creation,
            output: t.output + s.output,
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    /// Step 1's read plus creation: the context before any tool result.
    pub fontaine_base_context: u64,
    pub claude_base_context: u64,
    pub fontaine_mean_creation: f64,
    pub claude_mean_creation: f64,
    pub ratio: f64,
    pub flagged: bool,
}

fn mean_middle_creation(steps: &[Step]) -> f64 {
    let middle = &steps[1..steps.len() - 1];
    middle.iter().map(|s| s.cache_creation as f64).sum::<f64>() / middle.len() as f64
}

/// The ratio compares the mean creation of every step but the first (which differs by base
/// context) and the last (an answer-only turn), Fontaine over Claude Code.
pub fn summarize(fontaine: &[Step], claude: &[Step], threshold: f64) -> Result<Summary, String> {
    for (side, steps) in [("fontaine", fontaine), ("claude", claude)] {
        if steps.len() < 3 {
            return Err(format!(
                "{side} reported {} model call(s); at least 3 are needed to compare the middle steps",
                steps.len()
            ));
        }
    }
    let fontaine_mean = mean_middle_creation(fontaine);
    let claude_mean = mean_middle_creation(claude);
    if claude_mean <= 0.0 {
        return Err(
            "claude wrote no cache in its middle steps, so there is nothing to compare \
                    against (is prompt caching off?)"
                .into(),
        );
    }
    let ratio = fontaine_mean / claude_mean;
    let base = |steps: &[Step]| steps[0].cache_read + steps[0].cache_creation;
    Ok(Summary {
        fontaine_base_context: base(fontaine),
        claude_base_context: base(claude),
        fontaine_mean_creation: fontaine_mean,
        claude_mean_creation: claude_mean,
        ratio,
        flagged: ratio > threshold,
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Scorecard {
    pub date: String,
    pub model: String,
    pub claude_model: String,
    pub files: usize,
    pub threshold: f64,
    pub claude_version: String,
    pub fontaine_served_model: Option<String>,
    pub claude_served_model: Option<String>,
    pub fontaine_steps: Vec<Step>,
    pub claude_steps: Vec<Step>,
    pub fontaine_totals: Totals,
    pub claude_totals: Totals,
    pub summary: Summary,
}

impl Scorecard {
    /// The per-step table and the verdict, as printed.
    pub fn table(&self) -> String {
        let mut out =
            String::from("step  fontaine(read/write/in/out)         claude(read/write/in/out)\n");
        let rows = self.fontaine_steps.len().max(self.claude_steps.len());
        let cell = |s: Option<&Step>| match s {
            Some(s) => format!(
                "{}/{}/{}/{}",
                s.cache_read, s.cache_creation, s.input, s.output
            ),
            None => "-".to_string(),
        };
        for i in 0..rows {
            out.push_str(&format!(
                "{:<5} {:<35} {}\n",
                i + 1,
                cell(self.fontaine_steps.get(i)),
                cell(self.claude_steps.get(i))
            ));
        }
        let s = &self.summary;
        out.push_str(&format!(
            "base context: fontaine={} claude={}\nmean write (middle steps): fontaine={:.0} claude={:.0}\nratio={:.2} threshold={:.2} {}\n",
            s.fontaine_base_context,
            s.claude_base_context,
            s.fontaine_mean_creation,
            s.claude_mean_creation,
            s.ratio,
            self.threshold,
            if s.flagged { "FLAGGED" } else { "ok" }
        ));
        out
    }
}

pub fn prompt(files: usize) -> String {
    let names: Vec<String> = (1..=files).map(|i| format!("f{i}.txt")).collect();
    format!(
        "Read these files one at a time, in order, each with its own Read call: {}. \
         After the last one, reply with only DONE.",
        names.join(", ")
    )
}

/// `files` text files of identical size and distinct content.
pub fn write_fixture(dir: &Path, files: usize) -> std::io::Result<()> {
    for file in 1..=files {
        let mut state = file as u64 * 2_654_435_761;
        let mut text = String::new();
        for line in 0..WORDS_PER_FILE / WORDS_PER_LINE {
            text.push_str(&format!("{line:03}:"));
            for _ in 0..WORDS_PER_LINE {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let word = WORDS[(state >> 33) as usize % WORDS.len()];
                text.push(' ');
                text.push_str(&format!("{word:<6}"));
            }
            text.push('\n');
        }
        std::fs::write(dir.join(format!("f{file}.txt")), text)?;
    }
    Ok(())
}

pub struct Options {
    pub model: String,
    pub claude_model: String,
    pub files: usize,
    pub threshold: f64,
    pub fontaine: String,
    pub claude: String,
}

/// Runs the command in `cwd` and returns its stdout, failing on a timeout or a non-zero exit.
async fn run_side(program: &str, args: &[&str], cwd: &Path) -> Result<String, String> {
    let child = tokio::process::Command::new(program)
        .args(args)
        .current_dir(cwd)
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(TIMEOUT, child)
        .await
        .map_err(|_| format!("`{program}` timed out after {} s", TIMEOUT.as_secs()))?
        .map_err(|e| format!("cannot run `{program}`: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "`{program}` exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub async fn run_bench(options: &Options, date: String) -> Result<Scorecard, String> {
    let version = claude_version(&options.claude).await?;
    let prompt = prompt(options.files);
    let scratch = tempfile::tempdir().map_err(|e| e.to_string())?;
    let mut dirs = Vec::new();
    for side in ["fontaine", "claude"] {
        let dir = scratch.path().join(side);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        write_fixture(&dir, options.files).map_err(|e| e.to_string())?;
        dirs.push(dir);
    }
    let fontaine_out = run_side(
        &options.fontaine,
        &[
            "-p",
            &prompt,
            "--model",
            &options.model,
            "--output-format",
            "stream-json",
            "--allowed-tools",
            "read",
        ],
        &dirs[0],
    )
    .await?;
    let claude_out = run_side(
        &options.claude,
        &[
            "-p",
            &prompt,
            "--model",
            &options.claude_model,
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--allowed-tools",
            "Read",
        ],
        &dirs[1],
    )
    .await?;
    let fontaine = parse_fontaine_stream(&fontaine_out)?;
    let claude = parse_claude_stream(&claude_out)?;
    let summary = summarize(&fontaine.steps, &claude.steps, options.threshold)?;
    Ok(Scorecard {
        date,
        model: options.model.clone(),
        claude_model: options.claude_model.clone(),
        files: options.files,
        threshold: options.threshold,
        claude_version: version,
        fontaine_served_model: fontaine.model,
        claude_served_model: claude.model,
        fontaine_totals: totals(&fontaine.steps),
        claude_totals: totals(&claude.steps),
        fontaine_steps: fontaine.steps,
        claude_steps: claude.steps,
        summary,
    })
}

pub fn write_scorecard(path: &Path, card: &Scorecard) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(card).expect("scorecard serializes");
    text.push('\n');
    std::fs::write(path, text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn step(creation: u64) -> Step {
        Step {
            cache_creation: creation,
            ..Step::default()
        }
    }

    fn steps(creations: &[u64]) -> Vec<Step> {
        creations.iter().map(|c| step(*c)).collect()
    }

    fn lines(values: &[Value]) -> String {
        values.iter().map(|v| format!("{v}\n")).collect()
    }

    #[test]
    fn fontaine_steps_come_from_model_calls_in_order() {
        let out = lines(&[
            json!({"type": "init", "model": "acct/sonnet"}),
            json!({"type": "model_call", "input_tokens": 25, "output_tokens": 5,
                   "cache_read_input_tokens": 7, "cache_creation_input_tokens": 11,
                   "calls_tool": true}),
            json!({"type": "tool_call"}),
            json!({"type": "model_call", "input_tokens": 5, "output_tokens": 2,
                   "cache_read_input_tokens": 0, "cache_creation_input_tokens": 4,
                   "calls_tool": false}),
        ]);
        let stream = parse_fontaine_stream(&out).unwrap();
        assert_eq!(stream.model.as_deref(), Some("acct/sonnet"));
        assert_eq!(
            stream.steps,
            [
                Step {
                    input: 7,
                    cache_read: 7,
                    cache_creation: 11,
                    output: 5,
                    calls_tool: true
                },
                Step {
                    input: 1,
                    cache_read: 0,
                    cache_creation: 4,
                    output: 2,
                    calls_tool: false
                },
            ]
        );
    }

    #[test]
    fn claude_steps_dedupe_by_message_id_and_the_last_usage_wins() {
        let assistant = |id: &str, creation: u64, output: u64, block: Value| {
            json!({"type": "assistant", "message": {"id": id, "content": [block],
                "usage": {"input_tokens": 1, "cache_read_input_tokens": 9,
                          "cache_creation_input_tokens": creation, "output_tokens": output}}})
        };
        let out = lines(&[
            json!({"type": "system", "subtype": "init", "model": "claude-sonnet-x"}),
            assistant("m1", 100, 1, json!({"type": "text", "text": "ok"})),
            assistant("m1", 100, 40, json!({"type": "tool_use", "name": "Read"})),
            json!({"type": "user"}),
            assistant("m2", 200, 3, json!({"type": "text", "text": "DONE"})),
        ]);
        let stream = parse_claude_stream(&out).unwrap();
        assert_eq!(stream.model.as_deref(), Some("claude-sonnet-x"));
        assert_eq!(stream.steps.len(), 2);
        assert_eq!(stream.steps[0].output, 40);
        assert_eq!(stream.steps[0].cache_creation, 100);
        assert!(stream.steps[0].calls_tool);
        assert!(!stream.steps[1].calls_tool);
    }

    #[test]
    fn the_ratio_covers_the_middle_steps_only() {
        // The huge first and last steps must not move the ratio.
        let fontaine = steps(&[99_999, 400, 600, 99_999]);
        let claude = steps(&[1, 100, 300, 1]);
        let summary = summarize(&fontaine, &claude, 2.0).unwrap();
        assert_eq!(summary.fontaine_mean_creation, 500.0);
        assert_eq!(summary.claude_mean_creation, 200.0);
        assert_eq!(summary.ratio, 2.5);
        assert!(summary.flagged);
    }

    #[test]
    fn the_threshold_is_exclusive() {
        let fontaine = steps(&[0, 400, 0]);
        let claude = steps(&[0, 200, 0]);
        assert!(!summarize(&fontaine, &claude, 2.0).unwrap().flagged);
        assert!(summarize(&fontaine, &claude, 1.99).unwrap().flagged);
    }

    #[test]
    fn base_context_is_the_first_steps_read_plus_creation() {
        let mut fontaine = steps(&[5, 10, 10]);
        fontaine[0].cache_read = 20;
        let summary = summarize(&fontaine, &steps(&[3, 10, 10]), 2.0).unwrap();
        assert_eq!(summary.fontaine_base_context, 25);
        assert_eq!(summary.claude_base_context, 3);
    }

    #[test]
    fn fewer_than_three_steps_on_either_side_is_an_error() {
        let ok = steps(&[1, 2, 3]);
        assert!(summarize(&steps(&[1, 2]), &ok, 2.0).is_err());
        assert!(summarize(&ok, &steps(&[1, 2]), 2.0).is_err());
    }

    #[test]
    fn a_zero_native_write_is_an_error_not_a_division() {
        let err = summarize(&steps(&[1, 5, 1]), &steps(&[1, 0, 1]), 2.0).unwrap_err();
        assert!(err.contains("nothing to compare"), "{err}");
    }

    #[test]
    fn a_scorecard_round_trips() {
        let fontaine = steps(&[1, 400, 400, 2]);
        let claude = steps(&[1, 200, 200, 2]);
        let card = Scorecard {
            date: "2026-01-02".into(),
            model: "acct/sonnet".into(),
            claude_model: "sonnet".into(),
            files: 4,
            threshold: 2.0,
            claude_version: "2.1.319".into(),
            fontaine_served_model: Some("a".into()),
            claude_served_model: None,
            summary: summarize(&fontaine, &claude, 2.0).unwrap(),
            fontaine_totals: totals(&fontaine),
            claude_totals: totals(&claude),
            fontaine_steps: fontaine,
            claude_steps: claude,
        };
        let text = serde_json::to_string_pretty(&card).unwrap();
        assert_eq!(serde_json::from_str::<Scorecard>(&text).unwrap(), card);
        assert!(card.table().contains("ratio=2.00"));
    }

    #[test]
    fn fixture_files_are_equal_sized_and_distinct() {
        let dir = tempfile::tempdir().unwrap();
        write_fixture(dir.path(), 3).unwrap();
        let read =
            |i: usize| std::fs::read_to_string(dir.path().join(format!("f{i}.txt"))).unwrap();
        assert_eq!(read(1).len(), read(2).len());
        assert_eq!(read(2).len(), read(3).len());
        assert_ne!(read(1), read(2));
        assert!(read(1).len() > 8000);
    }
}
