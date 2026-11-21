use super::*;

type Seen = Arc<Mutex<Vec<String>>>;

fn warn() -> (Warn, Seen) {
    let seen: Seen = Arc::default();
    let sink = seen.clone();
    (
        Warn::to(move |m| sink.lock().unwrap().push(m.to_string())),
        seen,
    )
}

fn env_of(value: Option<&'static str>) -> impl Fn(&str) -> Option<OsString> + Send + Sync {
    move |name| {
        (name == SWITCH)
            .then(|| value.map(OsString::from))
            .flatten()
    }
}

fn frame(text: &str) -> Frame {
    Frame {
        value: json!({"type": "user", "message": {"role": "user", "content": text}}),
        query: false,
    }
}

fn prints(frames: &[Frame]) -> Vec<u64> {
    super::super::context::fingerprints(frames)
}

fn capture(mode: &'static str, dir: &Path) -> (Arc<Capture>, Seen) {
    let (warn, seen) = warn();
    let capture = Capture::from_env(&env_of(Some(mode)), dir, "session", warn).expect("on");
    (capture, seen)
}

fn file_of(dir: &Path) -> PathBuf {
    std::fs::read_dir(dir.join(DIR))
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .expect("a capture file")
}

fn with_marker(frames: &[Frame], at: usize) -> Vec<Frame> {
    let mut out = frames.to_vec();
    out[at].value["message"]["cache_control"] = json!({"type": "ephemeral"});
    out
}

#[test]
fn it_is_off_by_default_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (warn, seen) = warn();
    assert!(Capture::from_env(&env_of(None), dir.path(), "s", warn).is_none());
    assert!(seen.lock().unwrap().is_empty());
    assert!(!dir.path().join(DIR).exists());
}

#[test]
fn an_unrecognised_value_is_off_with_one_warning_naming_the_accepted_ones() {
    let dir = tempfile::tempdir().unwrap();
    let (warn, seen) = warn();
    assert!(Capture::from_env(&env_of(Some("yes")), dir.path(), "s", warn).is_none());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    for accepted in ["`0`", "`1`", "`full`", "`delta`", "empty", "unset"] {
        assert!(seen[0].contains(accepted), "{}", seen[0]);
    }
    assert!(!dir.path().join(DIR).exists());
}

#[test]
fn the_accepted_values_warn_about_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for (value, on) in [
        (None, false),
        (Some(""), false),
        (Some("0"), false),
        (Some("1"), true),
        (Some("full"), true),
        (Some("delta"), true),
    ] {
        let (warn, seen) = warn();
        let capture = Capture::from_env(&env_of(value), dir.path(), "s", warn);
        assert_eq!(capture.is_some(), on, "{value:?}");
        assert!(seen.lock().unwrap().is_empty(), "{value:?}");
    }
    assert!(!dir.path().join(DIR).exists());
}

#[test]
fn nothing_is_created_until_the_first_request_launch() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, seen) = capture("1", dir.path());
    assert!(!dir.path().join(DIR).exists());
    assert!(seen.lock().unwrap().is_empty());

    let bodies = capture.ensure_created().expect("created");
    assert!(bodies.is_dir());
    assert_eq!(bodies.parent().unwrap(), dir.path().join(DIR));
    assert!(file_of(dir.path()).is_file());
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].contains("unredacted"), "{}", seen[0]);
    assert!(seen[0].contains(&*dir.path().join(DIR).to_string_lossy()));
    assert!(seen[0].contains("delete"));
}

#[cfg(unix)]
#[test]
fn the_file_and_directories_are_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture("1", dir.path());
    let bodies = capture.ensure_created().unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&dir.path().join(DIR)), 0o700);
    assert_eq!(mode(&bodies), 0o700);
    assert_eq!(mode(&file_of(dir.path())), 0o600);
}

#[test]
fn an_existing_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("taken.jsonl");
    std::fs::write(&path, "x").unwrap();
    let error = owner_only::create_new(&path).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
}

#[test]
fn concurrent_first_launches_share_one_file_one_directory_and_one_warning() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, seen) = capture("1", dir.path());
    let paths: Vec<_> = (0..8)
        .map(|_| {
            let capture = capture.clone();
            std::thread::spawn(move || capture.ensure_created())
        })
        .collect::<Vec<_>>()
        .into_iter()
        .map(|t| t.join().unwrap().expect("created"))
        .collect();
    assert!(paths.windows(2).all(|w| w[0] == w[1]));
    let entries: Vec<_> = std::fs::read_dir(dir.path().join(DIR)).unwrap().collect();
    assert_eq!(entries.len(), 2, "one file and one bodies directory");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn a_creation_failure_warns_once_and_disables_the_capture() {
    let dir = tempfile::tempdir().unwrap();
    // A file where the directory belongs makes every creation fail.
    std::fs::write(dir.path().join(DIR), "in the way").unwrap();
    let (capture, seen) = capture("1", dir.path());
    assert!(capture.ensure_created().is_none());
    assert!(capture.ensure_created().is_none());
    assert_eq!(seen.lock().unwrap().len(), 1);
    let handle = capture.record_request("s", &[], None, &[frame("a")]);
    handle.record_result(b"{}");
    assert_eq!(seen.lock().unwrap().len(), 1);
}

fn scripted(mode: &'static str) -> (tempfile::TempDir, PathBuf, Vec<Vec<Frame>>, Vec<Vec<Frame>>) {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture(mode, dir.path());
    capture.ensure_created().unwrap();
    let (q1, a1, q2, a2, q3) = (
        frame("q1"),
        frame("a1"),
        frame("q2"),
        frame("a2"),
        frame("q3"),
    );
    let pre = vec![
        vec![q1.clone()],
        vec![q1.clone(), a1.clone(), q2.clone()],
        vec![q1, a1, q2, a2, q3],
    ];
    // The layout moves the marker to the final frame and adds a reminder to history.
    let mut post = Vec::new();
    for frames in &pre {
        let mut sent = with_marker(frames, frames.len() - 1);
        sent[0].value["message"]["reminder"] = json!("r");
        post.push(sent);
    }
    for (i, frames) in pre.iter().enumerate() {
        let handle = capture.record_request("session", &prints(frames), Some(frames), &post[i]);
        handle.record_result(json!({"type": "result", "n": i}).to_string().as_bytes());
    }
    let path = file_of(dir.path());
    (dir, path, pre, post)
}

#[test]
fn full_mode_reassembles_exactly_the_frames_sent() {
    let (_dir, path, pre, post) = scripted("full");
    for (i, sent) in post.iter().enumerate() {
        let got = reassemble(&path, "session", i).unwrap();
        let values: Vec<_> = got.frames.iter().map(|f| f.value.clone()).collect();
        let want: Vec<_> = sent.iter().map(|f| f.value.clone()).collect();
        assert_eq!(values, want);
        assert!(got.frames.last().unwrap().query);
        assert_eq!(got.diverged_at, None, "a moved marker is not a divergence");
    }
    let record: Value = records(&path).unwrap().remove(0);
    let hashes: Vec<String> = prints(&pre[0])
        .iter()
        .map(|p| format!("{p:016x}"))
        .collect();
    assert_eq!(record["hashes"], json!(hashes));
    assert_eq!(record["mode"], "full");
}

#[test]
fn delta_mode_records_only_new_messages_and_reassembles_them() {
    let (_dir, path, pre, _) = scripted("delta");
    let records = records(&path).unwrap();
    let requests: Vec<_> = records.iter().filter(|r| r["kind"] == "request").collect();
    assert_eq!(requests[0]["frames"].as_array().unwrap().len(), 1);
    assert_eq!(requests[1]["frames"].as_array().unwrap().len(), 2);
    assert_eq!(requests[2]["frames"].as_array().unwrap().len(), 2);
    assert_eq!(requests[2]["hashes"].as_array().unwrap().len(), 5);
    for (i, frames) in pre.iter().enumerate() {
        let got = reassemble(&path, "session", i).unwrap();
        let values: Vec<_> = got.frames.iter().map(|f| f.value.clone()).collect();
        let want: Vec<_> = frames.iter().map(|f| f.value.clone()).collect();
        assert_eq!(values, want);
    }
}

#[test]
fn an_edited_history_reports_where_it_diverged() {
    for mode in ["full", "delta"] {
        let dir = tempfile::tempdir().unwrap();
        let (capture, _) = capture(mode, dir.path());
        capture.ensure_created().unwrap();
        let first = vec![frame("a"), frame("b"), frame("c")];
        let edited = vec![frame("a"), frame("B"), frame("c"), frame("d")];
        let shorter = vec![frame("a"), frame("B")];
        for frames in [&first, &edited, &shorter] {
            capture.record_request("session", &prints(frames), Some(frames), frames);
        }
        let path = file_of(dir.path());
        let second = reassemble(&path, "session", 1).unwrap();
        assert_eq!(second.diverged_at, Some(1), "{mode}");
        assert_eq!(second.frames.len(), 4, "{mode}");
        assert_eq!(second.frames[1].value, edited[1].value, "{mode}");
        let third = reassemble(&path, "session", 2).unwrap();
        assert_eq!(third.diverged_at, Some(2), "a strict prefix, {mode}");
        assert_eq!(third.frames.len(), 2, "{mode}");
        assert_eq!(reassemble(&path, "session", 0).unwrap().diverged_at, None);
    }
}

#[test]
fn conversations_keep_separate_baselines_and_indices() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture("full", dir.path());
    capture.ensure_created().unwrap();
    let one = vec![frame("one")];
    let other = vec![frame("other")];
    let one_more = vec![frame("one"), frame("more")];
    // The first request is recorded and then dropped, as a cut or failed one is.
    drop(capture.record_request("a", &prints(&one), None, &one));
    capture.record_request("b", &prints(&other), None, &other);
    capture.record_request("a", &prints(&one_more), None, &one_more);
    let path = file_of(dir.path());
    assert_eq!(reassemble(&path, "a", 1).unwrap().diverged_at, None);
    assert_eq!(reassemble(&path, "b", 0).unwrap().frames.len(), 1);
    assert_eq!(
        reassemble(&path, "b", 1).unwrap_err().kind(),
        io::ErrorKind::NotFound
    );
}

#[test]
fn an_empty_thinking_text_with_a_signature_survives_byte_for_byte() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture("full", dir.path());
    capture.ensure_created().unwrap();
    let thinking = Frame {
        value: serde_json::from_str(
            r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"","signature":"Zm9v"}],"role":"assistant"}}"#,
        )
        .unwrap(),
        query: false,
    };
    let frames = vec![thinking.clone(), frame("q")];
    capture.record_request("s", &prints(&frames), None, &frames);
    let got = reassemble(&file_of(dir.path()), "s", 0).unwrap();
    assert_eq!(got.frames[0].value.to_string(), thinking.value.to_string());
    assert!(got.frames[0].value.to_string().contains(r#""thinking":"""#));
}

#[test]
fn diff_reports_the_divergence_first_then_the_first_differing_frame() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture("full", dir.path());
    capture.ensure_created().unwrap();
    let first = vec![frame("a"), frame("b"), frame("c")];
    let second = vec![frame("a"), frame("B"), frame("c"), frame("d")];
    capture.record_request("s", &prints(&first), None, &first);
    capture.record_request("s", &prints(&second), None, &second);
    let found = diff(&file_of(dir.path()), "s", 0, 1).unwrap();
    assert_eq!(found.diverged_at, Some(1));
    assert_eq!(found.first_difference, Some(1));
    assert_eq!(found.changes.len(), 1);
    assert!(found.changes[0].starts_with("frames[1].message.content"));
    let text = found.to_string();
    assert!(text.find("diverged").unwrap() < text.find("first differing").unwrap());
}

#[test]
fn the_result_lands_beside_its_request_verbatim() {
    let dir = tempfile::tempdir().unwrap();
    let (capture, _) = capture("full", dir.path());
    capture.ensure_created().unwrap();
    let frames = vec![frame("q")];
    let handle = capture.record_request("s", &prints(&frames), None, &frames);
    handle.record_result(br#"{"type":"result","usage":{"cache_read_input_tokens":7}}"#);
    let records = records(&file_of(dir.path())).unwrap();
    assert_eq!(records[1]["kind"], "result");
    assert_eq!(records[1]["session"], "s");
    assert_eq!(records[1]["index"], 0);
    assert_eq!(records[1]["result"]["usage"]["cache_read_input_tokens"], 7);
}
