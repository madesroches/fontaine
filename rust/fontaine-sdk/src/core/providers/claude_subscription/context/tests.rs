use super::*;
use crate::core::providers::claude_subscription::protocol::{SessionSeed, Translated, translate};
use crate::core::providers::claude_subscription::reminder::parse_reminder;
use crate::core::providers::claude_subscription::reminder::tests::{
    EMAIL, NONCE, TEMPLATE_2_1_319, body,
};

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

fn evidence() -> Evidence {
    Evidence {
        read: 10,
        expected: 900,
        gap: Duration::from_secs(5),
        prev_read: 800,
        prev_write_1h: 100,
        thinking: true,
    }
}

fn native_ctx() -> CliContext {
    ctx(Some((2, 1, 319)), Some("user@example.com"))
}

/// The reminder CLI 2.1.319 sent, as the probe learns it.
fn learned() -> Reminder {
    parse_reminder(
        &body(&TEMPLATE_2_1_319.replace("{email}", EMAIL)),
        EMAIL,
        NONCE,
    )
    .expect("the 2.1.319 reminder parses")
}

fn wording() -> String {
    learned().wording
}

fn reminder() -> String {
    learned().with_email(EMAIL)
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
    apply(&mut frames, &native_ctx(), Some(&learned()), layout);
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
        layout(&ctx(Some((2, 1, 318)), Some("a@b.c")), Some(&learned()), &v),
        Layout::Legacy
    );
    assert_eq!(
        layout(&ctx(Some((2, 1, 319)), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );
    assert_eq!(
        layout(&ctx(Some((2, 2, 0)), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );
    assert_eq!(
        layout(&ctx(Some((2, 1, 319)), None), Some(&learned()), &v),
        Layout::Legacy
    );
    assert_eq!(
        layout(&ctx(None, Some("a@b.c")), Some(&learned()), &v),
        Layout::Legacy
    );
    let broken = (2, 9, 100);
    assert_eq!(
        layout(&ctx(Some(broken), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );
    assert_eq!(v.mark(broken, &wording(), &evidence()), Mark::Saved);
    assert_eq!(
        layout(&ctx(Some(broken), Some("a@b.c")), Some(&learned()), &v),
        Layout::Legacy
    );
}

#[test]
fn a_verdict_judges_one_wording_and_a_new_wording_starts_clean() {
    let (_dir, v, _) = store();
    let version = (2, 9, 160);
    assert_eq!(v.mark(version, "wording-a", &evidence()), Mark::Saved);
    assert!(v.contains(version, "wording-a"));
    assert!(!v.contains(version, "wording-b"));
    assert_eq!(v.mark(version, "wording-b", &evidence()), Mark::Saved);
    assert!(!v.strike(version, "wording-c"));
    assert!(!v.strike(version, "wording-d"));
    assert!(v.strike(version, "wording-c"));
    let other = Reminder {
        template: "other {email}".to_string(),
        wording: "wording-b".to_string(),
    };
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&other), &v),
        Layout::Legacy
    );
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );
}

#[test]
fn a_persisted_verdict_round_trips_its_wording_and_an_entry_without_one_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(VERDICTS_FILE);
    let at = unix_now();
    std::fs::write(
        &path,
        json!({"broken": [
            {"cli": "2.9.161", "template": TEMPLATE_REVISION, "at": at},
            {"cli": "2.9.162", "template": 1, "wording": "wording-a", "at": at},
            {"cli": "2.9.163", "template": TEMPLATE_REVISION, "wording": 7, "at": at},
        ]})
        .to_string(),
    )
    .unwrap();
    let (v, _) = reload(dir.path());
    for version in [(2, 9, 161), (2, 9, 162), (2, 9, 163)] {
        assert!(!v.contains(version, "wording-a"), "{version:?}");
    }
    assert_eq!(v.mark((2, 9, 164), "wording-a", &evidence()), Mark::Saved);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(written.contains("\"wording\": \"wording-a\""), "{written}");
    let (restarted, _) = reload(dir.path());
    assert!(restarted.contains((2, 9, 164), "wording-a"));
    assert!(!restarted.contains((2, 9, 164), "wording-b"));
}

#[test]
fn the_layout_needs_a_learned_reminder() {
    let (_dir, v, _) = store();
    let context = ctx(Some((2, 1, 329)), Some("a@b.c"));
    assert_eq!(layout(&context, None, &v), Layout::Legacy);
    assert_eq!(layout(&context, Some(&learned()), &v), Layout::Native);
    let old = ctx(Some((2, 1, 318)), Some("a@b.c"));
    assert_eq!(layout(&old, Some(&learned()), &v), Layout::Legacy);
}

#[test]
fn a_native_request_replays_the_learned_text() {
    let drifted = Reminder {
        template: "<system-reminder>\nlearned {email}\n</system-reminder>".to_string(),
        wording: "w".to_string(),
    };
    let mut frames = translated(json!([
        {"role": "user", "content": "start"},
        assistant(),
        {"role": "user", "content": "final"},
    ]))
    .frames;
    apply(&mut frames, &native_ctx(), Some(&drifted), Layout::Native);
    assert_eq!(
        content(&frames, 0)[0]["text"],
        "<system-reminder>\nlearned user@example.com\n</system-reminder>\n"
    );
}

#[test]
fn a_version_is_marked_broken_once() {
    let (_dir, v, _) = store();
    let version = (2, 9, 101);
    assert_eq!(v.mark(version, &wording(), &evidence()), Mark::Saved);
    assert_eq!(v.mark(version, &wording(), &evidence()), Mark::Known);
}

#[test]
fn a_marked_version_survives_a_restart() {
    let (dir, v, warnings) = store();
    let version = (2, 9, 110);
    assert_eq!(v.mark(version, &wording(), &evidence()), Mark::Saved);
    let (restarted, restart_warnings) = reload(dir.path());
    assert!(restarted.contains(version, &wording()));
    assert_eq!(
        layout(
            &ctx(Some(version), Some("a@b.c")),
            Some(&learned()),
            &restarted
        ),
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
    let at = unix_now();
    std::fs::write(
        &path,
        json!({"broken": [
            {"cli": "2.9.111", "template": stale, "wording": wording(), "at": at},
            {"cli": "2.9.112", "template": TEMPLATE_REVISION, "wording": wording(), "at": at},
            {"cli": "not.a.version", "template": TEMPLATE_REVISION, "wording": wording(), "at": at},
        ]})
        .to_string(),
    )
    .unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 111), &wording()));
    assert!(v.contains((2, 9, 112), &wording()));
    assert!(warnings.lock().unwrap().is_empty());
    assert_eq!(v.mark((2, 9, 113), &wording(), &evidence()), Mark::Saved);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("2.9.111"));
    assert!(written.contains("2.9.112") && written.contains("2.9.113"));
}

#[test]
fn an_expired_or_untimed_entry_is_ignored_and_dropped_on_the_next_mark() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(VERDICTS_FILE);
    let now = unix_now();
    let old = now - VERDICT_TTL.as_secs() - 1;
    std::fs::write(
        &path,
        json!({"broken": [
            {"cli": "2.9.114", "template": TEMPLATE_REVISION, "wording": wording(), "at": old},
            {"cli": "2.9.115", "template": TEMPLATE_REVISION, "wording": wording()},
            {"cli": "2.9.116", "template": TEMPLATE_REVISION, "wording": wording(), "at": now - 60},
        ]})
        .to_string(),
    )
    .unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 114), &wording()));
    assert!(!v.contains((2, 9, 115), &wording()));
    assert!(v.contains((2, 9, 116), &wording()));
    assert!(warnings.lock().unwrap().is_empty());
    assert_eq!(
        layout(&ctx(Some((2, 9, 116)), Some("a@b.c")), Some(&learned()), &v),
        Layout::Legacy
    );
    assert_eq!(v.mark((2, 9, 117), &wording(), &evidence()), Mark::Saved);
    let written = std::fs::read_to_string(&path).unwrap();
    assert!(!written.contains("2.9.114") && !written.contains("2.9.115"));
    assert!(written.contains("2.9.116") && written.contains("2.9.117"));
    let entry = parse(&written, unix_now()).unwrap()[&((2, 9, 117), wording())];
    assert_eq!((entry.read, entry.expected), (Some(10), Some(900)));
}

#[test]
fn a_second_strike_for_a_version_is_what_makes_a_verdict() {
    let (_dir, v, _) = store();
    assert!(!v.strike((2, 9, 118), &wording()));
    assert!(!v.strike((2, 9, 119), &wording()));
    assert!(v.strike((2, 9, 118), &wording()));
    v.clear_strike((2, 9, 119), &wording());
    assert!(!v.strike((2, 9, 119), &wording()));
}

#[test]
fn a_mark_merges_a_verdict_another_store_wrote_in_between() {
    let dir = tempfile::tempdir().unwrap();
    let (first, _) = reload(dir.path());
    let (second, _) = reload(dir.path());
    assert_eq!(
        first.mark((2, 9, 120), &wording(), &evidence()),
        Mark::Saved
    );
    assert_eq!(
        second.mark((2, 9, 121), &wording(), &evidence()),
        Mark::Saved
    );
    let (merged, _) = reload(dir.path());
    assert!(merged.contains((2, 9, 120), &wording()));
    assert!(merged.contains((2, 9, 121), &wording()));
}

#[test]
fn a_missing_file_loads_empty_and_a_corrupt_one_warns_once_and_is_repaired() {
    let dir = tempfile::tempdir().unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 130), &wording()));
    assert!(warnings.lock().unwrap().is_empty());

    let path = dir.path().join(VERDICTS_FILE);
    std::fs::write(&path, "{not json").unwrap();
    let (v, warnings) = reload(dir.path());
    assert!(!v.contains((2, 9, 130), &wording()));
    assert_eq!(warnings.lock().unwrap().len(), 1);
    assert_eq!(v.mark((2, 9, 130), &wording(), &evidence()), Mark::Saved);
    assert_eq!(warnings.lock().unwrap().len(), 1);
    let (repaired, repaired_warnings) = reload(dir.path());
    assert!(repaired.contains((2, 9, 130), &wording()));
    assert!(repaired_warnings.lock().unwrap().is_empty());
}

#[test]
fn a_failed_write_still_marks_in_memory_and_warns_once() {
    let dir = tempfile::tempdir().unwrap();
    let blocker = dir.path().join("not-a-dir");
    std::fs::write(&blocker, "").unwrap();
    let (v, warnings) = store_in(&blocker);
    // Unix warns at load (ENOTDIR) and Windows does not, so count only what the mark adds.
    v.contains((2, 9, 140), &wording());
    let before = warnings.lock().unwrap().len();
    assert_eq!(v.mark((2, 9, 140), &wording(), &evidence()), Mark::Unsaved);
    assert!(v.contains((2, 9, 140), &wording()));
    assert_eq!(v.mark((2, 9, 140), &wording(), &evidence()), Mark::Known);
    assert_eq!(warnings.lock().unwrap().len(), before + 1);
}

#[test]
fn a_data_dir_has_one_shared_store_and_another_dir_has_its_own() {
    let (dir, first, _) = store();
    let (same, _) = store_in(dir.path());
    assert!(Arc::ptr_eq(&first, &same));
    assert_eq!(
        first.mark((2, 9, 150), &wording(), &evidence()),
        Mark::Saved
    );
    assert!(same.contains((2, 9, 150), &wording()));
    assert_eq!(same.mark((2, 9, 150), &wording(), &evidence()), Mark::Known);

    let (_other_dir, other, _) = store();
    assert!(!Arc::ptr_eq(&first, &other));
    assert!(!other.contains((2, 9, 150), &wording()));
}

fn record(frames: usize, blocks: usize, read: u64, write_1h: u64, at: Instant) -> Record {
    Record {
        prints: (0..frames as u64).collect(),
        blocks: vec![blocks; frames],
        merged_tail: false,
        unreplayable_tail: false,
        settings: 0,
        thinking: false,
        usage: CacheUsage {
            read,
            write_1h,
            ..Default::default()
        },
        at,
    }
}

#[test]
fn the_check_flags_only_a_read_short_of_what_was_written() {
    let t0 = Instant::now();
    let prev = record(3, 1, 1000, 500, t0);
    let next = |read| record(5, 1, read, 0, t0 + Duration::from_secs(60));
    assert_eq!(check(&prev, &next(1500)), Outcome::Healthy);
    assert_eq!(check(&prev, &next(1436)), Outcome::Healthy);
    assert_eq!(
        check(&prev, &next(1435)),
        Outcome::Broken(Evidence {
            read: 1435,
            expected: 1500,
            gap: Duration::from_secs(60),
            prev_read: 1000,
            prev_write_1h: 500,
            thinking: false,
        })
    );
    assert!(matches!(check(&prev, &next(1000)), Outcome::Broken(_)));
}

#[test]
fn a_cold_previous_request_gives_no_verdict() {
    let t0 = Instant::now();
    let cold = record(3, 1, 0, 500, t0);
    let soon = t0 + Duration::from_secs(60);
    assert_eq!(
        check(&cold, &record(5, 1, 0, 0, soon)),
        Outcome::Skipped(Skip::ColdPrevious)
    );
}

#[test]
fn the_check_gives_no_verdict_when_the_comparison_is_unsound() {
    let t0 = Instant::now();
    let prev = record(3, 1, 1000, 500, t0);
    let soon = t0 + Duration::from_secs(60);

    let skipped = |skip| Outcome::Skipped(skip);
    let mut diverged = record(5, 1, 0, 0, soon);
    diverged.prints[1] = 99;
    assert_eq!(
        check(&prev, &diverged),
        skipped(Skip::NotExtended {
            first_diff: 1,
            prev_len: 3,
            now_len: 5
        })
    );

    assert_eq!(
        check(&prev, &record(3, 1, 0, 0, soon)),
        skipped(Skip::NotExtended {
            first_diff: 3,
            prev_len: 3,
            now_len: 3
        })
    );
    assert_eq!(
        check(&prev, &record(5, 1, 0, 0, t0 + MAX_GAP)),
        skipped(Skip::GapTooLong)
    );
    assert!(matches!(
        check(
            &prev,
            &record(5, 1, 0, 0, t0 + MAX_GAP - Duration::from_secs(1))
        ),
        Outcome::Broken(_)
    ));
    // Two new frames of 9 blocks each are 18, the most the lookback allows.
    assert!(matches!(
        check(&prev, &record(5, 9, 0, 0, soon)),
        Outcome::Broken(_)
    ));
    assert_eq!(
        check(&prev, &record(5, 10, 0, 0, soon)),
        skipped(Skip::TooManyAdded)
    );

    let mut merged = record(3, 1, 1000, 500, t0);
    merged.merged_tail = true;
    assert_eq!(
        check(&merged, &record(5, 1, 0, 0, soon)),
        skipped(Skip::MergedTail)
    );

    let mut media = record(3, 1, 1000, 500, t0);
    media.unreplayable_tail = true;
    assert_eq!(
        check(&media, &record(5, 1, 0, 0, soon)),
        skipped(Skip::UnreplayableTail)
    );

    let mut retuned = record(5, 1, 0, 0, soon);
    retuned.settings = 1;
    assert_eq!(check(&prev, &retuned), skipped(Skip::SettingsChanged));
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
        Some(&wording()),
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
    let short = Some(CacheUsage::default());
    let wrote = Some(CacheUsage {
        read: 100,
        write_1h: 400,
        ..Default::default()
    });
    let usage = |read, write_1h| {
        Some(CacheUsage {
            read,
            write_1h,
            ..Default::default()
        })
    };

    // The first request has nothing to compare with.
    assert_eq!(
        finished(sent("a", version, &conversation(2)), short, at, &v),
        None
    );
    assert_eq!(
        finished(sent("b", version, &conversation(2)), wrote, at, &v),
        None
    );
    // The first shortfall is a strike, not a verdict.
    assert_eq!(
        finished(
            sent("b", version, &conversation(3)),
            usage(200, 400),
            at + Duration::from_secs(5),
            &v,
        ),
        None
    );
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );
    let finding = finished(
        sent("b", version, &conversation(4)),
        usage(300, 400),
        at + Duration::from_secs(10),
        &v,
    )
    .expect("the second consecutive shortfall is a verdict");
    assert_eq!(finding.version, version);
    assert!(finding.saved);
    assert_eq!(
        finding.evidence,
        Evidence {
            read: 300,
            expected: 600,
            gap: Duration::from_secs(5),
            prev_read: 200,
            prev_write_1h: 400,
            thinking: false,
        }
    );
    let warning = finding.warning();
    assert!(
        warning.contains("read 300 of expected 600 after 5 s"),
        "{warning}"
    );
    assert!(warning.contains("2.9.102"), "{warning}");
    assert!(warning.contains(&wording()[..8]), "{warning}");
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&learned()), &v),
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
                ..Default::default()
            }),
            at,
            &v,
        ),
        None
    );
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&learned()), &v),
        Layout::Legacy
    );
}

#[test]
fn a_healthy_pair_between_two_shortfalls_clears_the_strike_and_a_cold_one_does_not() {
    let (_dir, v, _) = store();
    let version = (2, 9, 104);
    let at = Instant::now();
    let usage = |read, write_1h| {
        Some(CacheUsage {
            read,
            write_1h,
            ..Default::default()
        })
    };
    let run = |session: &str, users, usage| {
        finished(sent(session, version, &conversation(users)), usage, at, &v)
    };

    run("f", 2, usage(100, 400));
    assert_eq!(run("f", 3, usage(200, 400)), None);
    // Healthy: read back everything of the previous 600.
    assert_eq!(run("f", 4, usage(700, 0)), None);
    assert_eq!(
        layout(&ctx(Some(version), Some("a@b.c")), Some(&learned()), &v),
        Layout::Native
    );

    // A cold previous request neither strikes nor clears.
    run("g", 2, usage(0, 500));
    assert_eq!(run("g", 3, usage(0, 600)), None);
    run("h", 2, usage(100, 400));
    assert_eq!(run("h", 3, usage(200, 400)), None);
    run("i", 2, usage(0, 500));
    assert_eq!(run("i", 3, usage(0, 600)), None);
    run("j", 2, usage(100, 400));
    assert!(run("j", 3, usage(200, 400)).is_some());
}

#[test]
fn a_legacy_request_or_a_missing_usage_drops_the_conversations_record() {
    let (_dir, v, _) = store();
    let version = (2, 9, 103);
    let at = Instant::now();
    let wrote = Some(CacheUsage {
        read: 100,
        write_1h: 400,
        ..Default::default()
    });
    finished(sent("d", version, &conversation(2)), wrote, at, &v);
    let frames = conversation(3);
    let legacy = Sent::new(
        "d",
        Some(version),
        Layout::Legacy,
        None,
        fingerprints(&frames),
        &frames,
    );
    assert_eq!(finished(legacy, wrote, at, &v), None);
    let short = Some(CacheUsage::default());
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
        "input_tokens": 2,
        "cache_creation_input_tokens": 4,
        "cache_creation": {"ephemeral_1h_input_tokens": 3, "ephemeral_5m_input_tokens": 1},
    }));
    assert_eq!(
        usage,
        Some(CacheUsage {
            read: 7,
            write_1h: 3,
            input: 2,
            write_total: 4,
            write_5m: 1,
        })
    );
    assert_eq!(
        CacheUsage::from_result(&json!({"cache_read_input_tokens": 7})),
        Some(CacheUsage {
            read: 7,
            ..Default::default()
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

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Captured {
    type Writer = Captured;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// The events `run` emitted, one line each.
fn logged(run: impl FnOnce()) -> Vec<String> {
    let captured = Captured::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(captured.clone())
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(subscriber, run);
    let text = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    text.lines().map(str::to_string).collect()
}

fn usage_of(read: u64, write_1h: u64) -> Option<CacheUsage> {
    Some(CacheUsage {
        read,
        write_1h,
        input: 3,
        write_total: write_1h + 5,
        write_5m: 5,
    })
}

#[test]
fn a_cache_usage_defaults_what_the_result_leaves_out() {
    let usage = CacheUsage::from_result(&json!({"cache_read_input_tokens": 1})).unwrap();
    assert_eq!((usage.input, usage.write_total, usage.write_5m), (0, 0, 0));
}

#[test]
fn a_thinking_block_in_the_conversation_is_noted() {
    let with = translated(json!([
        {"role": "user", "content": "start"},
        {"role": "assistant", "content": [
            {"type": "thinking", "thinking": "hm", "signature": "sig"},
            {"type": "text", "text": "ok"}]},
        {"role": "user", "content": "next"},
    ]))
    .frames;
    assert!(sent("think", (2, 9, 170), &with).thinking);
    assert!(!sent("think", (2, 9, 170), &conversation(3)).thinking);
}

#[test]
fn every_request_logs_its_outcome_usage_and_no_message_text() {
    let (_dir, v, _) = store();
    let version = (2, 9, 171);
    let at = Instant::now();
    let secret = "do-not-log-this-text";
    let mut first = conversation(2);
    first[0].value["message"]["content"] = json!(secret);
    let rewritten_to = |users| {
        let mut frames = conversation(users);
        frames[0].value["message"]["content"] = json!("other");
        frames
    };
    let lines = logged(|| {
        finished(sent("log1", version, &first), usage_of(100, 400), at, &v);
        // The prefix no longer extends: the first frame was rewritten.
        finished(
            sent("log1", version, &rewritten_to(3)),
            usage_of(200, 400),
            at + Duration::from_secs(5),
            &v,
        );
        finished(
            sent("log1", version, &rewritten_to(5)),
            usage_of(900, 0),
            at + Duration::from_secs(9),
            &v,
        );
        finished(
            sent("log1", version, &rewritten_to(7)),
            usage_of(1, 0),
            at + Duration::from_secs(12),
            &v,
        );
    });
    assert_eq!(lines.len(), 4, "{lines:?}");
    assert!(
        lines[0].contains("outcome=\"skipped:first_request\""),
        "{}",
        lines[0]
    );
    assert!(lines[0].contains("previous_usage=none"), "{}", lines[0]);
    assert!(lines[0].contains("usage=read=100 input=3 write=405 write_1h=400 write_5m=5"));
    assert!(lines[0].contains("wording=") && lines[0].contains("cli=2.9.171"));
    assert!(
        lines[1].contains("outcome=\"skipped:not_extended\""),
        "{}",
        lines[1]
    );
    assert!(lines[1].contains("first_diff=Some(0)"), "{}", lines[1]);
    assert!(lines[1].contains("prev_frames=Some(3)"), "{}", lines[1]);
    assert!(lines[1].contains("now_frames=Some(5)"), "{}", lines[1]);
    assert!(lines[2].contains("outcome=\"healthy\""), "{}", lines[2]);
    assert!(lines[2].contains("gap_secs=Some(4)"), "{}", lines[2]);
    assert!(lines[3].contains("strike=\"strike\""), "{}", lines[3]);
    assert!(lines[3].contains("outcome=\"broken\""), "{}", lines[3]);
    assert!(lines.iter().all(|line| !line.contains(secret)));
}

#[test]
fn a_request_without_a_native_layout_logs_what_is_missing() {
    let (_dir, v, _) = store();
    let frames = conversation(2);
    let legacy = Sent::new(
        "log2",
        Some((2, 9, 172)),
        Layout::Legacy,
        None,
        fingerprints(&frames),
        &frames,
    );
    let lines = logged(|| {
        finished(legacy, usage_of(1, 1), Instant::now(), &v);
        finished(sent("log3", (2, 9, 172), &frames), None, Instant::now(), &v);
    });
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[0].contains("outcome=\"no_record\"") && lines[0].contains("missing=\"layout\""));
    assert!(lines[1].contains("outcome=\"no_record\"") && lines[1].contains("missing=\"usage\""));
}

#[test]
fn a_verdict_is_logged_when_the_second_shortfall_lands() {
    let (_dir, v, _) = store();
    let version = (2, 9, 173);
    let at = Instant::now();
    let lines = logged(|| {
        finished(
            sent("log4", version, &conversation(2)),
            usage_of(100, 400),
            at,
            &v,
        );
        finished(
            sent("log4", version, &conversation(3)),
            usage_of(200, 400),
            at,
            &v,
        );
        finished(
            sent("log4", version, &conversation(4)),
            usage_of(300, 400),
            at,
            &v,
        );
    });
    assert!(lines[1].contains("strike=\"strike\""), "{}", lines[1]);
    assert!(lines[2].contains("strike=\"verdict\""), "{}", lines[2]);
}

#[test]
fn a_persisted_verdict_keeps_its_extra_fields_across_a_merge_and_an_older_file_loads() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(VERDICTS_FILE);
    let at = unix_now();
    std::fs::write(
        &path,
        json!({"broken": [
            {"cli": "2.9.180", "template": TEMPLATE_REVISION, "wording": wording(), "at": at,
             "read": 1, "expected": 2},
        ]})
        .to_string(),
    )
    .unwrap();
    let (first, _) = reload(dir.path());
    assert!(first.contains((2, 9, 180), &wording()));
    assert_eq!(
        first.mark((2, 9, 181), &wording(), &evidence()),
        Mark::Saved
    );
    // Another store persists a second entry: neither entry may lose its fields.
    let (second, _) = reload(dir.path());
    let other = Evidence {
        prev_read: 7,
        prev_write_1h: 8,
        thinking: false,
        ..evidence()
    };
    assert_eq!(second.mark((2, 9, 182), &wording(), &other), Mark::Saved);
    let written = std::fs::read_to_string(&path).unwrap();
    let entries = parse(&written, unix_now()).unwrap();
    let old = entries[&((2, 9, 180), wording())];
    assert_eq!(
        (old.read, old.prev_read, old.thinking),
        (Some(1), None, None)
    );
    let kept = entries[&((2, 9, 181), wording())];
    assert_eq!(
        (kept.prev_read, kept.prev_write_1h, kept.thinking),
        (Some(800), Some(100), Some(true))
    );
    let newest = entries[&((2, 9, 182), wording())];
    assert_eq!(
        (newest.prev_read, newest.prev_write_1h, newest.thinking),
        (Some(7), Some(8), Some(false))
    );
}
