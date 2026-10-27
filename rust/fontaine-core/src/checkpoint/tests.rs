use std::sync::Mutex;

use super::*;

#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Approver for Warnings {
    fn approve<'a>(
        &'a self,
        _request: crate::permissions::ApprovalRequest,
    ) -> futures::future::BoxFuture<'a, crate::permissions::Answer> {
        Box::pin(async { crate::permissions::Answer::Deny { feedback: None } })
    }

    fn warn(&self, message: &str) {
        self.0.lock().unwrap().push(message.to_string());
    }
}

struct Rig {
    _tmp: tempfile::TempDir,
    session: PathBuf,
    work: PathBuf,
    store: Arc<CheckpointStore>,
    warnings: Arc<Warnings>,
}

fn rig_with(limits: Checkpoints) -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(tmp.path()).unwrap();
    let session = base.join("session");
    let work = base.join("work");
    std::fs::create_dir_all(&session).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let store = Arc::new(CheckpointStore::open(&session).unwrap());
    let warnings = Arc::new(Warnings::default());
    store.configure(limits, None, warnings.clone());
    Rig {
        _tmp: tmp,
        session,
        work,
        store,
        warnings,
    }
}

fn rig() -> Rig {
    rig_with(Checkpoints::default())
}

impl Rig {
    fn scope(&self) -> Scope {
        Scope::new(self.work.clone(), [])
    }

    /// What a `write` of `content` to `name` does: capture, change, finish.
    fn edit(&self, name: &str, content: &str) -> PathBuf {
        let path = self.work.join(name);
        let capture = StoreCapture(self.store.clone()).before("root", "write", &path);
        std::fs::write(&path, content).unwrap();
        if let Some(capture) = capture {
            capture.finish(true);
        }
        path
    }

    fn reopened(&self) -> CheckpointStore {
        CheckpointStore::open(&self.session).unwrap()
    }
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn the_first_pre_image_of_a_turn_wins_and_later_edits_only_add_what_they_left() {
    let r = rig();
    let path = r.work.join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    r.store.begin_turn(0, "first", 0);
    r.edit("a.txt", "v1");
    r.edit("a.txt", "v2");
    let state = lock(&r.store.state, "test");
    let images: Vec<_> = state
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::PreImage { .. }))
        .collect();
    let afters = state
        .entries
        .iter()
        .filter(|e| matches!(e, Entry::After { .. }))
        .count();
    assert_eq!((images.len(), afters), (1, 2));
    let Entry::PreImage { before, .. } = images[0] else {
        unreachable!()
    };
    assert_eq!(
        *before,
        Before::Blob {
            hash: ContentHash::of(b"v0"),
            unix_mode: before_mode(before),
        }
    );
}

fn before_mode(before: &Before) -> Option<u32> {
    match before {
        Before::Blob { unix_mode, .. } => *unix_mode,
        _ => None,
    }
}

#[test]
fn a_file_that_did_not_exist_is_recorded_absent_and_deleted_by_a_restore() {
    let r = rig();
    r.store.begin_turn(0, "make it", 0);
    let path = r.edit("new.txt", "x");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert_eq!(plan.files[0].step, Step::Delete);
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.deleted, std::slice::from_ref(&path));
    assert!(!path.exists());
}

#[test]
fn identical_pre_images_share_one_blob() {
    let r = rig();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(r.work.join(name), "same").unwrap();
    }
    r.store.begin_turn(0, "p", 0);
    r.edit("a.txt", "1");
    r.edit("b.txt", "2");
    let blobs = std::fs::read_dir(r.session.join("checkpoints").join("blobs"))
        .unwrap()
        .count();
    assert_eq!(blobs, 1);
}

#[test]
fn an_oversize_file_is_skipped_warned_once_per_reason_and_the_edit_still_runs() {
    let r = rig_with(Checkpoints {
        max_file_bytes: 4,
        ..Checkpoints::default()
    });
    for name in ["a.txt", "b.txt"] {
        std::fs::write(r.work.join(name), "longer than four").unwrap();
    }
    r.store.begin_turn(0, "p", 0);
    let a = r.edit("a.txt", "changed");
    r.edit("b.txt", "changed");
    assert_eq!(read(&a), "changed");
    assert_eq!(r.warnings.0.lock().unwrap().len(), 1);
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert!(
        plan.files
            .iter()
            .all(|f| f.step == Step::Refused(Refusal::Skipped(SkipReason::TooLarge)))
    );
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.not_restorable.len(), 2);
    assert_eq!(read(&a), "changed");
}

#[test]
fn the_session_budget_skips_later_files() {
    let r = rig_with(Checkpoints {
        max_session_bytes: 10,
        ..Checkpoints::default()
    });
    std::fs::write(r.work.join("a.txt"), "123456").unwrap();
    std::fs::write(r.work.join("b.txt"), "abcdef").unwrap();
    r.store.begin_turn(0, "p", 0);
    r.edit("a.txt", "x");
    r.edit("b.txt", "y");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    let steps: Vec<_> = plan.files.iter().map(|f| f.step.clone()).collect();
    assert!(matches!(steps[0], Step::Restore { .. }));
    assert_eq!(
        steps[1],
        Step::Refused(Refusal::Skipped(SkipReason::Budget))
    );
}

#[test]
fn a_disabled_store_saves_nothing() {
    let r = rig_with(Checkpoints {
        enabled: false,
        ..Checkpoints::default()
    });
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    r.store.begin_turn(0, "p", 0);
    r.edit("a.txt", "v1");
    assert_eq!(r.store.turns()[0].files, 0);
}

#[test]
fn an_edit_before_any_prompt_is_not_attributed_to_a_turn() {
    let r = rig();
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    let capture = StoreCapture(r.store.clone()).before("root", "write", &r.work.join("a.txt"));
    assert!(capture.is_none());
}

#[test]
fn a_torn_manifest_tail_is_skipped_and_a_reopened_store_reads_what_was_kept() {
    let r = rig();
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    r.store.begin_turn(7, "first", 3);
    r.edit("a.txt", "v1");
    let manifest = r.session.join("checkpoints").join("manifest.jsonl");
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&manifest)
        .unwrap();
    file.write_all(b"{\"kind\":\"turn\",\"turn\":2,\"conv")
        .unwrap();
    drop(file);

    let reopened = r.reopened();
    reopened.configure(Checkpoints::default(), None, r.warnings.clone());
    assert_eq!(reopened.turns().len(), 1);
    let plan = reopened.plan(1, &r.scope()).unwrap();
    assert_eq!((plan.conv_n, plan.user_messages_before), (7, 3));
    assert!(matches!(plan.files[0].step, Step::Restore { .. }));
    // New turns continue the numbering and the torn line did not swallow the next record.
    assert_eq!(reopened.begin_turn(9, "second", 4), 2);
    assert_eq!(r.reopened().turns().len(), 2);
}

#[test]
fn the_earliest_pre_image_across_turns_gives_the_state_at_the_start() {
    let r = rig();
    let path = r.work.join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "v1");
    r.store.begin_turn(0, "two", 1);
    r.edit("a.txt", "v2");
    let plan = r.store.plan(2, &r.scope()).unwrap();
    r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(read(&path), "v1");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert_eq!(plan.files.len(), 1);
    // The first restore left "v1" on record, so this one is not a change the user made.
    assert!(!plan.files[0].changed_since);
    r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(read(&path), "v0");
}

#[test]
fn a_second_code_rewind_of_a_deleted_file_does_not_flag_the_first_ones_result() {
    let r = rig();
    r.store.begin_turn(0, "one", 0);
    let path = r.edit("new.txt", "x");
    r.store.begin_turn(0, "two", 1);
    r.edit("new.txt", "y");
    let plan = r.store.plan(2, &r.scope()).unwrap();
    r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(read(&path), "x");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert!(!plan.files[0].changed_since);
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.deleted, std::slice::from_ref(&path));
}

#[test]
fn the_picker_counts_the_files_of_a_turn_and_the_later_ones() {
    let r = rig();
    for name in ["a.txt", "b.txt"] {
        std::fs::write(r.work.join(name), "v0").unwrap();
    }
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "1");
    r.store.begin_turn(0, "two", 1);
    r.edit("b.txt", "1");
    r.store.begin_turn(0, "three", 2);
    let counts: Vec<_> = r.store.turns().iter().map(|t| t.files).collect();
    assert_eq!(counts, [2, 1, 0]);
}

#[test]
fn a_file_changed_since_the_last_tool_call_is_flagged() {
    let r = rig();
    let path = r.work.join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "v1");
    assert!(!r.store.plan(1, &r.scope()).unwrap().files[0].changed_since);
    std::fs::write(&path, "the user got there").unwrap();
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert!(plan.files[0].changed_since);
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.kept_changed, std::slice::from_ref(&path));
    assert_eq!(read(&path), "the user got there");
    r.store.restore_code(&plan, &r.scope(), true);
    assert_eq!(read(&path), "v0");
}

#[test]
fn an_edit_made_after_the_plan_is_kept_by_the_restore() {
    let r = rig();
    let path = r.work.join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "v1");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert!(!plan.files[0].changed_since);
    std::fs::write(&path, "the user got there").unwrap();
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.kept_changed, std::slice::from_ref(&path));
    assert_eq!(read(&path), "the user got there");
}

#[test]
fn a_rewind_marker_hides_the_superseded_turns_and_a_new_turn_gets_a_fresh_ordinal() {
    let r = rig();
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    for prompt in ["one", "two", "three"] {
        r.store.begin_turn(0, prompt, 0);
    }
    r.edit("a.txt", "v1");
    r.store.mark_rewound(2, true, None);
    let shown: Vec<_> = r.store.turns().iter().map(|t| t.turn).collect();
    assert_eq!(shown, [1]);
    // The code was put back, so the superseded pre-image is gone from the counts too.
    assert_eq!(r.store.turns()[0].files, 0);
    assert_eq!(r.store.begin_turn(0, "four", 0), 4);
    r.edit("a.txt", "v2");
    let shown: Vec<_> = r.store.turns().iter().map(|t| (t.turn, t.files)).collect();
    assert_eq!(shown, [(1, 1), (4, 1)]);
    // The same holds after a reopen.
    let reopened = r.reopened();
    assert_eq!(
        reopened.turns().iter().map(|t| t.turn).collect::<Vec<_>>(),
        [1, 4]
    );
}

#[test]
fn a_conversation_only_rewind_leaves_the_pre_images_restorable() {
    let r = rig();
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.store.begin_turn(0, "two", 0);
    r.edit("a.txt", "v1");
    r.store.mark_rewound(2, false, None);
    assert_eq!(r.store.turns().len(), 1);
    assert_eq!(r.store.turns()[0].files, 1);
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert_eq!(plan.files.len(), 1);
}

#[test]
fn a_new_epoch_hides_everything_before_it_and_restarts_the_numbers() {
    let r = rig();
    std::fs::write(r.work.join("a.txt"), "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "v1");
    r.store.begin_epoch();
    assert!(r.store.turns().is_empty());
    assert!(r.store.plan(1, &r.scope()).is_none());
    assert_eq!(r.store.begin_turn(5, "fresh", 0), 1);
    assert_eq!(r.store.turns().len(), 1);
    assert_eq!(r.store.plan(1, &r.scope()).unwrap().files.len(), 0);
    assert_eq!(r.reopened().turns().len(), 1);
}

#[test]
fn a_path_outside_the_workspace_is_refused() {
    let r = rig();
    let outside = r.work.parent().unwrap().join("outside.txt");
    std::fs::write(&outside, "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    let capture = StoreCapture(r.store.clone()).before("root", "write", &outside);
    std::fs::write(&outside, "v1").unwrap();
    capture.unwrap().finish(true);
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert_eq!(plan.files[0].step, Step::Refused(Refusal::OutOfScope));
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.not_restorable.len(), 1);
    assert_eq!(read(&outside), "v1");
}

#[cfg(unix)]
#[test]
fn a_symlinked_target_or_parent_is_refused_rather_than_followed() {
    use std::os::unix::fs::symlink;

    let r = rig();
    let elsewhere = r.work.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::fs::write(elsewhere.join("secret.txt"), "precious").unwrap();
    std::fs::create_dir_all(r.work.join("sub")).unwrap();
    std::fs::write(r.work.join("sub").join("f.txt"), "v0").unwrap();
    std::fs::write(r.work.join("t.txt"), "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("sub/f.txt", "v1");
    r.edit("t.txt", "v1");

    // After the edit, the target and the parent directory are swapped for links that stay
    // inside the workspace, so only the link check can catch them.
    std::fs::remove_file(r.work.join("t.txt")).unwrap();
    symlink(elsewhere.join("secret.txt"), r.work.join("t.txt")).unwrap();
    std::fs::remove_dir_all(r.work.join("sub")).unwrap();
    symlink(&elsewhere, r.work.join("sub")).unwrap();

    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert!(
        plan.files
            .iter()
            .all(|f| f.step == Step::Refused(Refusal::Symlink)),
        "{plan:?}"
    );
    let report = r.store.restore_code(&plan, &r.scope(), true);
    assert_eq!(report.not_restorable.len(), 2);
    assert_eq!(read(&elsewhere.join("secret.txt")), "precious");
}

#[cfg(unix)]
#[test]
fn the_file_mode_comes_back_with_the_content() {
    use std::os::unix::fs::PermissionsExt;

    let r = rig();
    let path = r.work.join("run.sh");
    std::fs::write(&path, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("run.sh", "changed");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    let plan = r.store.plan(1, &r.scope()).unwrap();
    r.store.restore_code(&plan, &r.scope(), true);
    assert_eq!(read(&path), "#!/bin/sh\n");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o755
    );
}

#[test]
fn a_damaged_blob_is_not_written_back() {
    let r = rig();
    let path = r.work.join("a.txt");
    std::fs::write(&path, "v0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.edit("a.txt", "v1");
    let blob = r
        .session
        .join("checkpoints")
        .join("blobs")
        .join(&ContentHash::of(b"v0").0);
    std::fs::write(blob, "tampered").unwrap();
    let plan = r.store.plan(1, &r.scope()).unwrap();
    let report = r.store.restore_code(&plan, &r.scope(), true);
    assert_eq!(report.not_restorable, [(path.clone(), Refusal::BlobLost)]);
    assert_eq!(read(&path), "v1");
}

#[test]
fn a_manifest_path_in_the_memory_directory_is_not_restored() {
    let r = rig();
    let memory = r.work.join("memory");
    std::fs::create_dir_all(&memory).unwrap();
    let note = memory.join("note.md");
    std::fs::write(&note, "v0").unwrap();
    // Recorded before the memory directory was known to the store.
    r.store.begin_turn(0, "one", 0);
    r.edit("memory/note.md", "v1");
    r.store.configure(
        Checkpoints::default(),
        Some(memory.clone()),
        r.warnings.clone(),
    );
    let plan = r.store.plan(1, &r.scope()).unwrap();
    assert_eq!(plan.files[0].step, Step::Refused(Refusal::Memory));
    assert_eq!(r.store.turns()[0].files, 0);
    r.store.restore_code(&plan, &r.scope(), true);
    assert_eq!(read(&note), "v1");
}

#[test]
fn a_rewind_hides_only_the_pre_images_of_the_paths_it_restored() {
    let r = rig();
    std::fs::write(r.work.join("kept.txt"), "k0").unwrap();
    std::fs::write(r.work.join("done.txt"), "d0").unwrap();
    r.store.begin_turn(0, "one", 0);
    r.store.begin_turn(0, "two", 0);
    let kept = r.edit("kept.txt", "k1");
    let done = r.edit("done.txt", "d1");
    std::fs::write(&kept, "the user got there").unwrap();
    let plan = r.store.plan(2, &r.scope()).unwrap();
    let report = r.store.restore_code(&plan, &r.scope(), false);
    assert_eq!(report.kept_changed, std::slice::from_ref(&kept));
    let restored = report
        .restored
        .iter()
        .chain(&report.deleted)
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    r.store.mark_rewound(2, true, Some(restored));
    assert_eq!(read(&done), "d0");
    let plan = r.store.plan(1, &r.scope()).unwrap();
    let paths: Vec<_> = plan.files.iter().map(|f| f.path.clone()).collect();
    assert_eq!(paths, [kept]);
    assert_eq!(r.reopened().plan(1, &r.scope()).unwrap().files.len(), 1);
}
