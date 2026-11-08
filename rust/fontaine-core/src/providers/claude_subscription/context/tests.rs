use super::*;
use crate::providers::claude_subscription::protocol::{SessionSeed, Translated, translate};

fn ctx(version: Option<(u32, u32, u32)>, email: Option<&str>) -> CliContext {
    CliContext {
        email: email.map(str::to_string),
        version,
    }
}

fn native_ctx() -> CliContext {
    ctx(Some((2, 1, 319)), Some("user@example.com"))
}

fn reminder() -> String {
    REMINDER_TEMPLATE.replace("{email}", "user@example.com")
}

fn translated(messages: Value) -> Translated {
    let body = json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 64,
        "system": "be brief",
        "messages": messages,
        "tools": [{"name": "read_file", "description": "d", "input_schema": {"type": "object"}}],
    });
    translate(
        body.to_string().as_bytes(),
        None,
        &SessionSeed {
            session: "s1",
            namespace: "root",
            canonical: "claude-sonnet-5-5",
        },
    )
    .unwrap()
}

fn applied(messages: Value, layout: Layout) -> Vec<Frame> {
    let mut frames = translated(messages).frames;
    apply(&mut frames, &native_ctx(), layout);
    frames
}

fn marked(frames: &[Frame]) -> Vec<(usize, usize)> {
    let mut at = Vec::new();
    for (frame, f) in frames.iter().enumerate() {
        let content = &f.value["message"]["content"];
        for (block, b) in content.as_array().into_iter().flatten().enumerate() {
            if b.get("cache_control").is_some() {
                at.push((frame, block));
            }
        }
    }
    at
}

fn assistant() -> Value {
    json!({"role": "assistant", "content": [
        {"type": "tool_use", "id": "t1", "name": "read_file", "input": {}}]})
}

/// A one-run history, the run being `run`, followed by an assistant message and a final one.
fn after(run: Value) -> Vec<Frame> {
    applied(
        json!([
            {"role": "user", "content": run},
            assistant(),
            {"role": "user", "content": "final"},
        ]),
        Layout::Native,
    )
}

fn content(frames: &[Frame], at: usize) -> &Value {
    &frames[at].value["message"]["content"]
}

fn tool_loop(extra_user: Value) -> Vec<Frame> {
    applied(
        json!([
            {"role": "user", "content": "start"},
            assistant(),
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "one"},
                {"type": "text", "text": ""}]},
            {"role": "assistant", "content": [
                {"type": "tool_use", "id": "t2", "name": "read_file", "input": {}}]},
            extra_user,
        ]),
        Layout::Legacy,
    )
}

#[test]
fn the_reminder_template_has_the_measured_shape() {
    let text = reminder();
    assert!(text.starts_with(
        "<system-reminder>\nAs you answer the user's questions, you can use the following context:\n# userEmail\nThe user's email address is user@example.com. Use it only"
    ));
    assert!(text.ends_with(
        "ask the user.\n\nClaude Code attached this context automatically; it isn't part of the user's message. It describes the user's own account and workspace, so they don't need it reported back.\n</system-reminder>"
    ));
}

#[test]
fn the_layout_follows_version_and_email() {
    assert_eq!(
        layout(&ctx(Some((2, 1, 318)), Some("a@b.c"))),
        Layout::Legacy
    );
    assert_eq!(
        layout(&ctx(Some((2, 1, 319)), Some("a@b.c"))),
        Layout::Native
    );
    assert_eq!(layout(&ctx(Some((2, 2, 0)), Some("a@b.c"))), Layout::Native);
    assert_eq!(layout(&ctx(Some((2, 1, 319)), None)), Layout::Legacy);
    assert_eq!(layout(&ctx(None, Some("a@b.c"))), Layout::Legacy);
    let broken = (2, 9, 100);
    assert_eq!(layout(&ctx(Some(broken), Some("a@b.c"))), Layout::Native);
    assert!(mark_broken(broken));
    assert_eq!(layout(&ctx(Some(broken), Some("a@b.c"))), Layout::Legacy);
}

#[test]
fn a_version_is_marked_broken_once() {
    let version = (2, 9, 101);
    assert!(mark_broken(version));
    assert!(!mark_broken(version));
}

fn record(frames: usize, blocks: usize, read: u64, write_1h: u64, at: Instant) -> Record {
    Record {
        prints: (0..frames as u64).collect(),
        blocks: vec![blocks; frames],
        merged_tail: false,
        unreplayable_tail: false,
        settings: 0,
        usage: CacheUsage { read, write_1h },
        at,
    }
}

#[test]
fn the_check_flags_only_a_read_short_of_what_was_written() {
    let t0 = Instant::now();
    let prev = record(3, 1, 1000, 500, t0);
    let next = |read| record(5, 1, read, 0, t0 + Duration::from_secs(60));
    assert!(!check(&prev, &next(1500)));
    assert!(!check(&prev, &next(1436)));
    assert!(check(&prev, &next(1435)));
    assert!(check(&prev, &next(1000)));
}

#[test]
fn the_check_gives_no_verdict_when_the_comparison_is_unsound() {
    let t0 = Instant::now();
    let prev = record(3, 1, 1000, 500, t0);
    let soon = t0 + Duration::from_secs(60);

    let mut diverged = record(5, 1, 0, 0, soon);
    diverged.prints[1] = 99;
    assert!(!check(&prev, &diverged));

    assert!(!check(&prev, &record(3, 1, 0, 0, soon)));
    assert!(!check(&prev, &record(5, 1, 0, 0, t0 + MAX_GAP)));
    assert!(check(
        &prev,
        &record(5, 1, 0, 0, t0 + MAX_GAP - Duration::from_secs(1))
    ));
    // Two new frames of 9 blocks each are 18, the most the lookback allows.
    assert!(check(&prev, &record(5, 9, 0, 0, soon)));
    assert!(!check(&prev, &record(5, 10, 0, 0, soon)));

    let mut merged = record(3, 1, 1000, 500, t0);
    merged.merged_tail = true;
    assert!(!check(&merged, &record(5, 1, 0, 0, soon)));

    let mut media = record(3, 1, 1000, 500, t0);
    media.unreplayable_tail = true;
    assert!(!check(&media, &record(5, 1, 0, 0, soon)));

    let mut retuned = record(5, 1, 0, 0, soon);
    retuned.settings = 1;
    assert!(!check(&prev, &retuned));
    assert_ne!(
        settings_print(Some("high"), &json!({"env": {"X": "{}"}})),
        settings_print(Some("low"), &json!({"env": {"X": "{}"}}))
    );
}

#[test]
fn a_final_frame_place_cannot_replay_is_flagged() {
    let image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}});
    let flagged = |content: Value| {
        let frames = translated(json!([{"role": "user", "content": content}])).frames;
        unreplayable(&frames[0])
    };
    assert!(flagged(
        json!([{"type": "text", "text": "a"}, image.clone()])
    ));
    assert!(flagged(json!([result_block(Some(json!([
        {"type": "text", "text": "a"}, image.clone()])))])));
    assert!(!flagged(json!("plain")));
    assert!(!flagged(json!([result_block(Some(json!("out")))])));
    assert!(!flagged(json!([result_block(Some(json!([
        {"type": "text", "text": "a"}])))])));
}

fn sent(session: &str, version: (u32, u32, u32), frames: &[Frame]) -> Sent {
    Sent::new(
        session,
        Some(version),
        Layout::Native,
        fingerprints(frames),
        frames,
    )
}

fn conversation(users: usize) -> Vec<Frame> {
    let mut messages = vec![json!({"role": "user", "content": "start"})];
    for n in 1..users {
        messages.push(assistant());
        messages.push(json!({"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": "t1", "content": format!("r{n}")}]}));
    }
    translated(Value::Array(messages)).frames
}

#[test]
fn a_broken_verdict_needs_a_previous_native_request_and_survives_a_passing_check() {
    let version = (2, 9, 102);
    let at = Instant::now();
    let short = Some(CacheUsage {
        read: 0,
        write_1h: 0,
    });
    let wrote = Some(CacheUsage {
        read: 100,
        write_1h: 400,
    });

    // The first request has nothing to compare with.
    assert_eq!(
        finished(sent("a", version, &conversation(2)), short, at),
        None
    );
    // The second reads nothing though the first wrote nothing at 1 h: not short.
    assert_eq!(
        finished(sent("b", version, &conversation(2)), wrote, at),
        None
    );
    assert_eq!(
        finished(
            sent("b", version, &conversation(3)),
            short,
            at + Duration::from_secs(5)
        ),
        Some(version)
    );
    assert_eq!(layout(&ctx(Some(version), Some("a@b.c"))), Layout::Legacy);

    // A later passing check does not clear the verdict, and the warning is not repeated.
    assert_eq!(
        finished(sent("c", version, &conversation(2)), wrote, at),
        None
    );
    assert_eq!(
        finished(
            sent("c", version, &conversation(3)),
            Some(CacheUsage {
                read: 900,
                write_1h: 0
            }),
            at
        ),
        None
    );
    assert_eq!(layout(&ctx(Some(version), Some("a@b.c"))), Layout::Legacy);
}

#[test]
fn a_legacy_request_or_a_missing_usage_drops_the_conversations_record() {
    let version = (2, 9, 103);
    let at = Instant::now();
    let wrote = Some(CacheUsage {
        read: 100,
        write_1h: 400,
    });
    finished(sent("d", version, &conversation(2)), wrote, at);
    let frames = conversation(3);
    let legacy = Sent::new(
        "d",
        Some(version),
        Layout::Legacy,
        fingerprints(&frames),
        &frames,
    );
    assert_eq!(finished(legacy, wrote, at), None);
    let short = Some(CacheUsage {
        read: 0,
        write_1h: 0,
    });
    assert_eq!(
        finished(sent("d", version, &conversation(4)), short, at),
        None
    );

    finished(sent("e", version, &conversation(2)), wrote, at);
    assert_eq!(
        finished(sent("e", version, &conversation(3)), None, at),
        None
    );
    assert_eq!(
        finished(sent("e", version, &conversation(4)), short, at),
        None
    );
}

#[test]
fn the_records_are_bounded() {
    let mut records = Records::default();
    let at = Instant::now();
    for n in 0..RECORDED + 5 {
        records.put(format!("s{n}"), record(1, 1, 0, 0, at));
    }
    assert_eq!(records.by_session.len(), RECORDED);
    assert!(records.take("s0").is_none());
    assert!(records.take(&format!("s{}", RECORDED + 4)).is_some());
}

#[test]
fn the_cache_usage_reads_the_result() {
    let usage = CacheUsage::from_result(&json!({
        "cache_read_input_tokens": 7,
        "cache_creation": {"ephemeral_1h_input_tokens": 3, "ephemeral_5m_input_tokens": 1},
    }));
    assert_eq!(
        usage,
        Some(CacheUsage {
            read: 7,
            write_1h: 3
        })
    );
    assert_eq!(
        CacheUsage::from_result(&json!({"cache_read_input_tokens": 7})),
        Some(CacheUsage {
            read: 7,
            write_1h: 0
        })
    );
    assert_eq!(CacheUsage::from_result(&json!({"output_tokens": 1})), None);
}

#[test]
fn a_text_run_gets_a_leading_reminder_block() {
    let frames = after(json!([{"type": "text", "text": "hi"}]));
    assert_eq!(
        content(&frames, 0),
        &json!([
            {"type": "text", "text": format!("{}\n", reminder())},
            {"type": "text", "text": "hi"},
        ])
    );
    let frames = after(json!("hi"));
    assert_eq!(
        content(&frames, 0),
        &json!([
            {"type": "text", "text": format!("{}\n", reminder())},
            {"type": "text", "text": "hi"},
        ])
    );
}

fn result_block(content: Option<Value>) -> Value {
    let mut block = json!({"type": "tool_result", "tool_use_id": "t1"});
    if let Some(content) = content {
        block["content"] = content;
    }
    block
}

#[test]
fn a_string_tool_result_gets_the_reminder_appended() {
    let frames = after(json!([result_block(Some(json!("out")))]));
    assert_eq!(
        content(&frames, 0),
        &json!([result_block(Some(json!(format!("out\n\n{}", reminder()))))])
    );
}

#[test]
fn a_string_tool_result_is_trimmed_of_trailing_whitespace_before_the_reminder() {
    for ending in ["", "\n", "\n\n", " \n", "  ", "\t\n "] {
        let frames = after(json!([result_block(Some(json!(format!(
            "alpha contents{ending}"
        ))))]));
        assert_eq!(
            content(&frames, 0),
            &json!([result_block(Some(json!(format!(
                "alpha contents\n\n{}",
                reminder()
            ))))])
        );
    }
}

#[test]
fn a_last_text_block_gets_one_newline_untrimmed_before_the_reminder_block() {
    for (ending, expected) in [
        ("\n", "\n\n"),
        ("  ", "  \n"),
        ("\t\n ", "\t\n \n"),
        ("", "\n"),
    ] {
        let frames = after(json!([result_block(Some(json!([
            {"type": "text", "text": format!("x{ending}")}])))]));
        assert_eq!(
            content(&frames, 0),
            &json!([result_block(Some(json!([
                {"type": "text", "text": format!("x{expected}")},
                {"type": "text", "text": reminder()}])))])
        );
    }
}

#[test]
fn an_array_tool_result_gets_the_reminder_as_a_new_last_text_block() {
    let frames = after(json!([result_block(Some(json!([
        {"type": "text", "text": "a"}, {"type": "text", "text": "b"}])))]));
    assert_eq!(
        content(&frames, 0),
        &json!([result_block(Some(json!([
            {"type": "text", "text": "a"},
            {"type": "text", "text": "b\n"},
            {"type": "text", "text": reminder()}])))])
    );
}

#[test]
fn only_the_last_of_parallel_results_changes() {
    let first = result_block(Some(json!("one")));
    let mut second = result_block(Some(json!("two")));
    second["tool_use_id"] = json!("t2");
    let frames = after(json!([first.clone(), second]));
    let blocks = content(&frames, 0).as_array().unwrap();
    assert_eq!(blocks[0], first);
    assert_eq!(
        blocks[1]["content"],
        json!(format!("two\n\n{}", reminder()))
    );
}

#[test]
fn a_result_then_text_gets_a_trailing_reminder_block() {
    let frames = after(json!([
        result_block(Some(json!("out"))), {"type": "text", "text": "and"}]));
    assert_eq!(
        content(&frames, 0),
        &json!([
            result_block(Some(json!("out"))),
            {"type": "text", "text": "and"},
            {"type": "text", "text": reminder()},
        ])
    );
}

#[test]
fn a_result_then_an_empty_text_block_swaps_it_for_the_bare_reminder() {
    let frames = after(json!([
        result_block(Some(json!("out"))), {"type": "text", "text": ""}]));
    assert_eq!(
        content(&frames, 0),
        &json!([
            result_block(Some(json!("out"))),
            {"type": "text", "text": reminder()},
        ])
    );
}

#[test]
fn empty_absent_and_array_tool_result_content_each_take_the_reminder_bare() {
    let frames = after(json!([result_block(Some(json!("")))]));
    assert_eq!(
        content(&frames, 0),
        &json!([result_block(Some(json!(reminder())))])
    );

    let frames = after(json!([result_block(None)]));
    assert_eq!(
        content(&frames, 0),
        &json!([result_block(Some(json!(reminder())))])
    );

    let frames = after(json!([result_block(Some(json!([])))]));
    assert_eq!(
        content(&frames, 0),
        &json!([result_block(Some(
            json!([{"type": "text", "text": reminder()}])
        ))])
    );
}

#[test]
fn an_unknown_shape_is_left_unchanged_and_later_runs_still_get_the_reminder() {
    let image = json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}});
    let ends_in_image = result_block(Some(json!([{"type": "text", "text": "a"}, image.clone()])));
    let frames = applied(
        json!([
            {"role": "user", "content": [ends_in_image.clone()]},
            assistant(),
            {"role": "user", "content": [image.clone()]},
            assistant(),
            {"role": "user", "content": "later"},
            assistant(),
            {"role": "user", "content": "final"},
        ]),
        Layout::Native,
    );
    assert_eq!(content(&frames, 0), &json!([ends_in_image]));
    assert_eq!(content(&frames, 2), &json!([image]));
    assert_eq!(
        content(&frames, 4)[0]["text"],
        json!(format!("{}\n", reminder()))
    );
}

#[test]
fn a_run_of_user_frames_takes_rule_one_on_its_first_and_rules_two_and_three_on_its_last() {
    let frames = applied(
        json!([
            {"role": "user", "content": "first"},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "out"}]},
            assistant(),
            {"role": "user", "content": "final"},
        ]),
        Layout::Native,
    );
    // The run holds a tool result, so the last block decides: rule 2 on the last frame.
    assert_eq!(content(&frames, 0), &json!("first"));
    assert_eq!(
        content(&frames, 1),
        &json!([result_block(Some(json!(format!("out\n\n{}", reminder()))))])
    );

    let frames = applied(
        json!([
            {"role": "user", "content": "first"},
            {"role": "user", "content": "second"},
            assistant(),
            {"role": "user", "content": "final"},
        ]),
        Layout::Native,
    );
    assert_eq!(
        content(&frames, 0)[0]["text"],
        json!(format!("{}\n", reminder()))
    );
    assert_eq!(content(&frames, 1), &json!("second"));
}

#[test]
fn the_final_frame_never_gets_the_reminder() {
    let frames = applied(
        json!([
            {"role": "user", "content": "start"},
            assistant(),
            {"role": "user", "content": "final"},
        ]),
        Layout::Native,
    );
    assert!(frames[0].value.to_string().contains("system-reminder"));
    assert!(!frames[2].value.to_string().contains("system-reminder"));
}

#[test]
fn native_puts_one_marker_on_the_final_frames_last_cacheable_block() {
    let frames = applied(
        json!([
            {"role": "user", "content": "start"},
            assistant(),
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": "two"},
                {"type": "text", "text": ""}]},
        ]),
        Layout::Native,
    );
    assert_eq!(marked(&frames), [(2, 0)]);
    assert_eq!(
        content(&frames, 2)[0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert!(!frames[2].value.to_string().contains("system-reminder"));
}

#[test]
fn native_string_content_in_the_final_frame_becomes_one_marked_text_block() {
    let frames = applied(
        json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": [{"type": "text", "text": "ok"}]},
            {"role": "user", "content": "second"},
        ]),
        Layout::Native,
    );
    assert_eq!(marked(&frames), [(2, 0)]);
    assert_eq!(content(&frames, 2)[0]["text"], "second");
}

#[test]
fn a_single_user_message_gets_no_marker_in_either_layout() {
    for layout in [Layout::Native, Layout::Legacy] {
        let frames = applied(json!([{"role": "user", "content": "hi"}]), layout);
        assert!(marked(&frames).is_empty());
    }
}

#[test]
fn legacy_marks_the_last_history_user_message_and_adds_no_reminder() {
    let frames = tool_loop(json!({"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "t2", "content": "two"}]}));
    // The trailing empty text block is skipped for the tool result before it.
    assert_eq!(marked(&frames), [(2, 0)]);
    assert_eq!(
        content(&frames, 2)[0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert!(
        frames
            .iter()
            .all(|f| !f.value.to_string().contains("system-reminder"))
    );
}

#[test]
fn legacy_string_content_becomes_one_marked_text_block_and_the_final_frame_stays_a_string() {
    let frames = applied(
        json!([
            {"role": "user", "content": "first"},
            {"role": "assistant", "content": [{"type": "text", "text": "ok"}]},
            {"role": "user", "content": "second"},
        ]),
        Layout::Legacy,
    );
    assert_eq!(marked(&frames), [(0, 0)]);
    assert_eq!(content(&frames, 0)[0]["text"], "first");
    assert_eq!(content(&frames, 2), "second");
}

#[test]
fn legacy_marks_the_last_cacheable_block_of_the_history_frame() {
    let frames = applied(
        json!([
            {"role": "user", "content": [
                {"type": "text", "text": "a", "cache_control": {"type": "ephemeral", "ttl": "5m"}},
                {"type": "text", "text": "b"}]},
            {"role": "assistant", "content": [
                {"type": "text", "text": "ok", "cache_control": {"type": "ephemeral"}}]},
            {"role": "user", "content": [
                {"type": "text", "text": "c", "cache_control": {"type": "ephemeral"}}]},
        ]),
        Layout::Legacy,
    );
    assert_eq!(marked(&frames), [(0, 1)]);
}
