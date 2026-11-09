use super::*;
use crate::providers::claude_subscription::protocol::{SessionSeed, Translated, translate};

fn ctx(version: Option<(u32, u32, u32)>, email: Option<&str>) -> CliContext {
    CliContext {
        email: email.map(str::to_string),
        version,
    }
}

/// A store over a fresh temp data dir, and the warnings it emitted.
fn store() -> (tempfile::TempDir, Arc<Verdicts>, Arc<Mutex<Vec<String>>>) {
    let dir = tempfile::tempdir().unwrap();
    let (verdicts, warnings) = store_in(dir.path());
    (dir, verdicts, warnings)
}

fn store_in(dir: &Path) -> (Arc<Verdicts>, Arc<Mutex<Vec<String>>>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = warnings.clone();
    let warn = Warn::to(move |message| sink.lock().unwrap().push(message.to_string()));
    (Verdicts::shared(dir, &warn), warnings)
}

fn reload(dir: &Path) -> (Verdicts, Arc<Mutex<Vec<String>>>) {
    let warnings = Arc::new(Mutex::new(Vec::new()));
    let sink = warnings.clone();
    let warn = Warn::to(move |message| sink.lock().unwrap().push(message.to_string()));
    (Verdicts::load(dir.join(VERDICTS_FILE), warn), warnings)
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
    let (_dir, v, _) = store();
    assert_eq!(
        layout(&ctx(Some((2, 1, 318)), Some("a@b.c")), &v),
        Layout::Legacy
    );
    assert_eq!(
        layout(&ctx(Some((2, 1, 319)), Some("a@b.c")), &v),
        Layout::Native
    );
    assert_eq!(
        layout(&ctx(Some((2, 2, 0)), Some("a@b.c")), &v),
        Layout::Native
    );
    assert_eq!(layout(&ctx(Some((2, 1, 319)), None), &v), Layout::Legacy);
    assert_eq!(layout(&ctx(None, Some("a@b.c")), &v), Layout::Legacy);
    let broken = (2, 9, 100);
    assert_eq!(
        layout(&ctx(Some(broken), Some("a@b.c")), &v),
        Layout::Native
    );
    assert!(v.mark(broken));
    assert_eq!(
        layout(&ctx(Some(broken), Some("a@b.c")), &v),
        Layout::Legacy
    );
}

#[test]
fn a_version_is_marked_broken_once() {
    let (_dir, v, _) = store();
    let version = (2, 9, 101);
    assert!(v.mark(version));
    assert!(!v.mark(version));
}

#[test]
fn a_marked_version_survives_a_restart() {
    let (dir, v, warnings) = store();
    let version = (2, 9, 110);
    assert!(v.mark(version));
    let (restarted, restart_warnings) = reload(dir.path());
    assert!(restarted.contains(version));
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), &restarted),
        Layout::Legacy
    );
    assert!(warnings.lock().unwrap().is_empty());
    assert!(restart_warnings.lock().unwrap().is_empty());
}

#[test]
fn an_entry_of_another_template_revision_is_ignored_and_dropped_on_the_next_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(VERDICTS_FILE);
    let stale = TEMPLATE_REVISION + 1;
    std::fs::write(
        &path,
        json!({"broken": [
            {"cli": "2.9.111", "template": stale},
            {"cli": "2.9.112", "template": TEMPLATE_REVISION},
            {"cli": "not.a.version", "template": TEMPLATE_REVISION},
        ]})
        .to_string(),
    )
    .unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 111)));
    assert!(v.contains((2, 9, 112)));
    assert!(warnings.lock().unwrap().is_empty());
    assert!(v.mark((2, 9, 113)));
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("2.9.111"));
    assert!(written.contains("2.9.112") && written.contains("2.9.113"));
}

#[test]
fn a_mark_merges_a_verdict_another_store_wrote_in_between() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = reload(dir.path());
    let (second, _) = reload(dir.path());
    assert!(first.mark((2, 9, 120)));
    assert!(second.mark((2, 9, 121)));
    let (merged, _) = reload(dir.path());
    assert!(merged.contains((2, 9, 120)));
    assert!(merged.contains((2, 9, 121)));
}

#[test]
fn a_missing_file_loads_empty_and_a_corrupt_one_warns_once_and_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 130)));
    assert!(warnings.lock().unwrap().is_empty());

    let path = dir.path().join(VERDICTS_FILE);
    std::fs::write(&path, "{not json").unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 130)));
    assert_eq!(warnings.lock().unwrap().len(), 1);
    assert!(v.mark((2, 9, 130)));
    assert_eq!(warnings.lock().unwrap().len(), 1);
    let (repaired, repaired_warnings) = reload(dir.path());
    assert!(repaired.contains((2, 9, 130)));
    assert!(repaired_warnings.lock().unwrap().is_empty());
}

#[test]
fn a_failed_write_still_marks_in_memory_and_warns_once() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let (v, warnings) = store_in(&blocker);
    // Unix warns at load (ENOTDIR) and Windows does not, so count only what the mark adds.
    let before = warnings.lock().unwrap().len();
    assert!(v.mark((2, 9, 140)));
    assert!(v.contains((2, 9, 140)));
    assert!(!v.mark((2, 9, 140)));
    assert_eq!(warnings.lock().unwrap().len(), before + 1);
}

#[test]
fn a_data_dir_has_one_shared_store_and_another_dir_has_its_own() {
    let (dir, first, _) = store();
    let (same, _) = store_in(dir.path());
    assert!(Arc::ptr_eq(&first, &same));
    assert!(first.mark((2, 9, 150)));
    assert!(same.contains((2, 9, 150)));
    assert!(!same.mark((2, 9, 150)));

    let (_other_dir, other, _) = store();
    assert!(!Arc::ptr_eq(&first, &other));
    assert!(!other.contains((2, 9, 150)));
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
    let (_dir, v, _) = store();
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
        finished(sent("a", version, &conversation(2)), short, at, &v),
        None
    );
    // The second reads nothing though the first wrote nothing at 1 h: not short.
    assert_eq!(
        finished(sent("b", version, &conversation(2)), wrote, at, &v),
        None
    );
    assert_eq!(
        finished(
            sent("b", version, &conversation(3)),
            short,
            at + Duration::from_secs(5),
            &v,
        ),
        Some(version)
    );
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), &v),
        Layout::Legacy
    );

    // A later passing check does not clear the verdict, and the warning is not repeated.
    assert_eq!(
        finished(sent("c", version, &conversation(2)), wrote, at, &v),
        None
    );
    assert_eq!(
        finished(
            sent("c", version, &conversation(3)),
            Some(CacheUsage {
                read: 900,
                write_1h: 0
            }),
            at,
            &v,
        ),
        None
    );
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), &v),
        Layout::Legacy
    );
}

#[test]
fn a_legacy_request_or_a_missing_usage_drops_the_conversations_record() {
    let (_dir, v, _) = store();
    let version = (2, 9, 103);
    let at = Instant::now();
    let wrote = Some(CacheUsage {
        read: 100,
        write_1h: 400,
    });
    finished(sent("d", version, &conversation(2)), wrote, at, &v);
    let frames = conversation(3);
    let legacy = Sent::new(
        "d",
        Some(version),
        Layout::Legacy,
        fingerprints(&frames),
        &frames,
    );
    assert_eq!(finished(legacy, wrote, at, &v), None);
    let short = Some(CacheUsage {
        read: 0,
        write_1h: 0,
    });
    assert_eq!(
        finished(sent("d", version, &conversation(4)), short, at, &v),
        None
    );

    finished(sent("e", version, &conversation(2)), wrote, at, &v);
    assert_eq!(
        finished(sent("e", version, &conversation(3)), None, at, &v),
        None
    );
    assert_eq!(
        finished(sent("e", version, &conversation(4)), short, at, &v),
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
