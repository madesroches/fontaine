//! Keeping an MCP server's oversized result out of the model's context. A successful result over
//! the limit is saved whole in the session's scratch directory and the model is told where; a long
//! error result keeps its two ends. Applied per tool, before the guard's result pipeline, so the
//! scanner and redaction see what the model will.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rig_core::message::ToolResultContent;
use rig_core::tool::ToolOutput;
pub use rig_tools::Spill;
use rig_tools::clip;

use crate::result_pipeline::{ToolOutcome, text_view};

pub const DEFAULT_CHAR_LIMIT: usize = 50_000;
pub const MAX_CHAR_LIMIT: usize = 500_000;
/// Default token limit of a result, as Claude Code's `MAX_MCP_OUTPUT_TOKENS`.
pub const DEFAULT_MAX_TOKENS: usize = 25_000;
/// Characters kept at each end of a long error result.
pub const ERROR_KEEP: usize = 5_000;
pub const ERROR_THRESHOLD: usize = 11_000;

pub struct OutputLimits {
    pub max_tokens: usize,
    /// Where oversized results are saved; `None` falls back to a head/tail cut.
    pub spill: Option<Spill>,
    write_failed: AtomicBool,
}

impl OutputLimits {
    pub fn new(max_tokens: usize, spill: Option<Spill>) -> Self {
        OutputLimits {
            max_tokens,
            spill,
            write_failed: AtomicBool::new(false),
        }
    }
}

impl Default for OutputLimits {
    fn default() -> Self {
        OutputLimits::new(DEFAULT_MAX_TOKENS, None)
    }
}

/// The same estimate the guardian uses.
fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn has_image(output: &ToolOutput) -> bool {
    output
        .as_content()
        .iter()
        .any(|part| matches!(part, ToolResultContent::Image(_)))
}

/// The last `count` characters of `text`.
fn tail(text: &str, count: usize) -> &str {
    if count == 0 {
        return "";
    }
    match text.char_indices().rev().nth(count - 1) {
        Some((start, _)) => text.get(start..).unwrap_or(text),
        None => text,
    }
}

/// `text` with its middle replaced by a marker, keeping `head` and `tail_len` characters.
fn cut_middle(text: &str, head: usize, tail_len: usize) -> String {
    let total = text.chars().count();
    if total <= head + tail_len {
        return text.to_string();
    }
    let removed = total - head - tail_len;
    format!(
        "{}\n[… {removed} chars cut …]\n{}",
        clip(text, head),
        tail(text, tail_len)
    )
}

fn file_name_part(tool: &str) -> String {
    tool.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn save(spill: &Spill, tool: &str, text: &str) -> std::io::Result<std::path::PathBuf> {
    let (path, mut file) = spill.create(&file_name_part(tool))?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(path)
}

fn saved_message(chars: usize, tokens: usize, path: &Path) -> String {
    format!(
        "Output too large ({chars} chars, ~{tokens} tokens), saved to {}. Use read or grep on that file to see it.",
        path.display()
    )
}

/// What `outcome` becomes for the model. `warn` is called at most once per `limits`, when a save
/// fails and the result falls back to a cut.
pub(crate) fn cap(
    outcome: ToolOutcome,
    tool: &str,
    char_limit: usize,
    limits: &OutputLimits,
    warn: &dyn Fn(&str),
) -> ToolOutcome {
    match outcome {
        Ok(output) => {
            if has_image(&output) {
                return Ok(output);
            }
            let text = text_view(&output);
            let chars = text.chars().count();
            let tokens = estimate_tokens(&text);
            if chars <= char_limit && tokens <= limits.max_tokens {
                return Ok(output);
            }
            if let Some(spill) = &limits.spill {
                match save(spill, tool, &text) {
                    Ok(path) => return Ok(ToolOutput::text(saved_message(chars, tokens, &path))),
                    Err(e) => {
                        if !limits.write_failed.swap(true, Ordering::Relaxed) {
                            warn(&format!(
                                "could not save an oversized MCP result under {}: {e}; \
                                 keeping its beginning and end instead",
                                spill.0.display()
                            ));
                        }
                    }
                }
            }
            // The token limit is the tighter bound when it is the one that tripped.
            let keep = char_limit.min(limits.max_tokens.saturating_mul(4)) / 2;
            Ok(ToolOutput::text(cut_middle(&text, keep, keep)))
        }
        // Only a result the server flagged `isError` is capped: it carries no source, unlike a
        // transport failure or a timeout, which says its own short message.
        Err(error) if std::error::Error::source(&error).is_none() => {
            let shown = error.model_output();
            if has_image(shown) {
                return Err(error);
            }
            let text = text_view(shown);
            if text.chars().count() <= ERROR_THRESHOLD {
                return Err(error);
            }
            let cut = ToolOutput::text(cut_middle(&text, ERROR_KEEP, ERROR_KEEP));
            Err(error.with_model_output(cut))
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rig_core::tool::ToolExecutionError;

    use super::*;

    fn limits(spill: Option<&Path>) -> OutputLimits {
        OutputLimits::new(DEFAULT_MAX_TOKENS, spill.map(|p| Spill(p.to_path_buf())))
    }

    fn text_of(outcome: ToolOutcome) -> String {
        match outcome {
            Ok(output) => text_view(&output),
            Err(error) => text_view(error.model_output()),
        }
    }

    fn run(outcome: ToolOutcome, char_limit: usize, limits: &OutputLimits) -> ToolOutcome {
        cap(outcome, "mcp__srv__big", char_limit, limits, &|_| {})
    }

    #[test]
    fn a_result_under_both_limits_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let out = run(
            Ok(ToolOutput::text("x".repeat(1_000))),
            DEFAULT_CHAR_LIMIT,
            &limits(Some(dir.path())),
        );
        assert_eq!(text_of(out), "x".repeat(1_000));
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn the_char_threshold_spills() {
        let dir = tempfile::tempdir().unwrap();
        let text = "a".repeat(DEFAULT_CHAR_LIMIT + 1);
        let message = text_of(run(
            Ok(ToolOutput::text(text.clone())),
            DEFAULT_CHAR_LIMIT,
            &limits(Some(dir.path())),
        ));
        assert!(message.starts_with("Output too large (50001 chars, ~12501 tokens), saved to "));
        assert!(message.ends_with("Use read or grep on that file to see it."));
        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert!(message.contains(&path.display().to_string()));
        assert!(
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("mcp__srv__big-")
        );
        assert_eq!(std::fs::read_to_string(path).unwrap(), text);
    }

    #[test]
    fn the_token_threshold_spills_even_under_the_char_limit() {
        let dir = tempfile::tempdir().unwrap();
        // 400,000 chars is ~100,000 tokens; the declared char limit is high enough to allow it.
        let text = "b".repeat(400_000);
        let message = text_of(run(
            Ok(ToolOutput::text(text.clone())),
            MAX_CHAR_LIMIT,
            &limits(Some(dir.path())),
        ));
        assert!(message.contains("~100000 tokens"), "{message}");
        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        assert_eq!(std::fs::read_to_string(path).unwrap().len(), text.len());
    }

    #[test]
    fn a_raised_char_limit_lets_a_larger_result_through() {
        let dir = tempfile::tempdir().unwrap();
        let text = "c".repeat(80_000);
        let out = run(
            Ok(ToolOutput::text(text.clone())),
            100_000,
            &limits(Some(dir.path())),
        );
        assert_eq!(text_of(out), text);
    }

    #[test]
    fn an_image_result_is_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let output = ToolOutput::content(vec![
            ToolResultContent::text("d".repeat(200_000)),
            ToolResultContent::image_base64(
                "AAAA",
                Some(rig_core::message::ImageMediaType::PNG),
                None,
            ),
        ])
        .unwrap();
        let out = run(
            Ok(output.clone()),
            DEFAULT_CHAR_LIMIT,
            &limits(Some(dir.path())),
        );
        assert_eq!(out.unwrap(), output);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn without_a_spill_directory_the_result_keeps_its_head_and_tail() {
        let text: String = (0..60_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let message = text_of(run(
            Ok(ToolOutput::text(text.clone())),
            DEFAULT_CHAR_LIMIT,
            &limits(None),
        ));
        assert!(message.starts_with(&text[..25_000]));
        assert!(message.ends_with(&text[text.len() - 25_000..]));
        assert!(
            message.contains("[… 10000 chars cut …]"),
            "{}",
            &message[24_990..25_040]
        );
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        let text = "é".repeat(60_000);
        let message = text_of(run(
            Ok(ToolOutput::text(text)),
            DEFAULT_CHAR_LIMIT,
            &limits(None),
        ));
        assert!(message.contains("chars cut"));
        assert!(message.starts_with('é') && message.ends_with('é'));
    }

    #[test]
    fn a_failing_write_cuts_and_warns_once() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the directory should be makes `create_dir_all` fail.
        let blocked = dir.path().join("blocked");
        std::fs::write(&blocked, "").unwrap();
        let limits = limits(Some(&blocked));
        let warnings = Cell::new(0);
        let warn = |_: &str| warnings.set(warnings.get() + 1);
        for _ in 0..2 {
            let out = cap(
                Ok(ToolOutput::text("e".repeat(60_000))),
                "t",
                DEFAULT_CHAR_LIMIT,
                &limits,
                &warn,
            );
            assert!(text_of(out).contains("chars cut"));
        }
        assert_eq!(warnings.get(), 1);
    }

    #[test]
    fn a_long_error_result_keeps_both_ends() {
        let text: String = (0..20_000)
            .map(|i| char::from(b'a' + (i % 26) as u8))
            .collect();
        let error = ToolExecutionError::other("MCP tool 'x' reported an execution error")
            .with_model_output(ToolOutput::text(text.clone()));
        let shown = text_of(run(Err(error), DEFAULT_CHAR_LIMIT, &limits(None)));
        assert!(shown.starts_with(&text[..ERROR_KEEP]));
        assert!(shown.ends_with(&text[text.len() - ERROR_KEEP..]));
        assert!(shown.contains("[… 10000 chars cut …]"));
    }

    #[test]
    fn an_error_result_at_the_threshold_is_untouched() {
        let text = "f".repeat(ERROR_THRESHOLD);
        let error =
            ToolExecutionError::other("failed").with_model_output(ToolOutput::text(text.clone()));
        assert_eq!(
            text_of(run(Err(error), DEFAULT_CHAR_LIMIT, &limits(None))),
            text
        );
    }

    #[test]
    fn a_failure_that_has_a_source_is_left_alone() {
        let text = "g".repeat(ERROR_THRESHOLD + 5_000);
        let error = ToolExecutionError::other("transport failed")
            .with_source(std::io::Error::other("reset"))
            .with_model_output(ToolOutput::text(text.clone()));
        assert_eq!(
            text_of(run(Err(error), DEFAULT_CHAR_LIMIT, &limits(None))),
            text
        );
    }
}
