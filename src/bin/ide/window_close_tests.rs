use super::*;
use std::{fs::OpenOptions, os::fd::AsRawFd, time::Instant};

fn fixture() -> (tempfile::TempDir, Workbench, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("A.kt");
    std::fs::write(&path, "val a=1\n").unwrap();
    let ctx = egui::Context::default();
    m::install(&ctx);
    let mut a = Workbench::new(root.path().to_owned(), ctx, None, false);
    let parked = root.path().join("B.kt");
    std::fs::write(&parked, "val b=1\n").unwrap();
    a.parked.insert(
        parked.clone(),
        (Document::open(parked.clone()).unwrap(), Default::default()),
    );
    a.tabs.push(parked.clone());
    (root, a, parked)
}
fn dirty(a: &mut Workbench, parked: &std::path::Path) {
    a.apply_edit(6, 7, "2");
    a.parked.get_mut(parked).unwrap().0.edit(6..7, "3").unwrap();
}
fn hold(a: &Workbench) -> std::fs::File {
    let path = wexandroidstudio::document_saves::lock_path(&a.root, a.doc.path.as_ref().unwrap());
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    file
}
fn waiting(a: &mut Workbench) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        a.document_save_updates();
        if a.save_ui.phase.as_ref().is_some_and(|p| p.3) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "real writer did not reach held fd lock"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}
fn start_clean_probe(a: &mut Workbench) {
    assert!(a.breakpoint_request(Some(Action::Retry)));
    breakpoint_ui::settle(a);
    a.request_window_close();
    a.window_close_step();
    assert!(a.close_ui.phase == Some(Phase::Preparing));
}

#[test]
fn keep_open_preserves_active_parked_selection_text_identity_and_two_undos() {
    let (_root, mut a, parked) = fixture();
    dirty(&mut a, &parked);
    a.edit.anchor = 1;
    a.edit.cursor = 4;
    let guard = a.close_guard();
    let view = (a.edit.anchor, a.edit.cursor);
    a.request_window_close();
    a.window_close_step();
    assert_eq!(a.close_ui.paths.len(), 2);
    assert!(a.close_ui.phase == Some(Phase::Choices));
    a.cancel_window_close();
    a.window_close_step();
    assert!(!a.closing_requested() && a.close_guard() == guard);
    assert_eq!((a.edit.anchor, a.edit.cursor), view);
    a.doc.undo().unwrap();
    a.parked.get_mut(&parked).unwrap().0.undo().unwrap();
    assert_eq!(a.doc.rope(), "val a=1\n");
    assert_eq!(a.parked[&parked].0.rope(), "val b=1\n");
    assert_eq!(
        std::fs::read_to_string(a.doc.path.as_ref().unwrap()).unwrap(),
        "val a=1\n"
    );
    assert_eq!(std::fs::read_to_string(&parked).unwrap(), "val b=1\n");
}

#[test]
fn request_accepts_same_frame_text_then_modal_blocks_text_save_and_launches() {
    let (_root, mut a, _) = fixture();
    a.request_window_close();
    a.apply_edit(6, 7, "2");
    a.window_close_step();
    assert_eq!(a.doc.rope(), "val a=2\n");
    let id = a.doc.text_id().clone();
    let ctx = a.context.clone();
    let _ = ctx.run(
        egui::RawInput {
            events: vec![
                egui::Event::Paste("blocked".into()),
                egui::Event::Ime(egui::ImeEvent::Commit("入力".into())),
            ],
            ..Default::default()
        },
        |ctx| a.dispatch_editor_frame(ctx),
    );
    a.save_document();
    a.build();
    a.run();
    a.start_debug();
    a.start_preview();
    assert_eq!(a.doc.text_id(), &id);
    assert!(a.save_ui.pending.is_none() && !a.close_work_pending());
    a.cancel_window_close();
}

#[test]
fn save_all_waits_for_real_receipts_and_cancel_prevents_late_exit_without_rolling_back() {
    let (_root, mut a, parked) = fixture();
    dirty(&mut a, &parked);
    let lock = hold(&a);
    a.request_window_close();
    a.window_close_step();
    a.close_choose(true);
    waiting(&mut a);
    assert!(a.close_ui.phase == Some(Phase::Saving) && !a.close_ui.allow);
    let id = a.doc.text_id().clone();
    a.cancel_window_close();
    drop(lock);
    assert!(a.wait_for_save());
    a.window_close_step();
    assert!(!a.closing_requested());
    assert_eq!(a.doc.text_id(), &id);
    assert!(!a.doc.dirty() && !a.parked[&parked].0.dirty());
    assert_eq!(
        std::fs::read_to_string(a.doc.path.as_ref().unwrap()).unwrap(),
        "val a=2\n"
    );
    assert_eq!(std::fs::read_to_string(&parked).unwrap(), "val b=3\n");
    a.doc.undo().unwrap();
    assert_eq!(a.doc.rope(), "val a=1\n");
    assert!(a.doc.dirty());
}

#[test]
fn partial_source_write_retains_window_and_unsaved_parked_buffer() {
    let (_root, mut a, parked) = fixture();
    dirty(&mut a, &parked);
    let out = std::process::Command::new("python3")
        .arg("-c")
        .arg("import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('val external=9\\n')")
        .arg(&parked)
        .status()
        .unwrap();
    assert!(out.success());
    a.request_window_close();
    a.window_close_step();
    a.close_choose(true);
    assert!(!a.wait_for_save());
    a.window_close_step();
    assert!(a.close_ui.open && !a.close_ui.allow && a.close_ui.phase == Some(Phase::Choices));
    assert!(a.close_ui.error.is_some());
    assert!(!a.doc.dirty() && a.parked[&parked].0.dirty());
    assert_eq!(a.parked[&parked].0.rope(), "val b=3\n");
    assert_eq!(
        std::fs::read_to_string(&parked).unwrap(),
        "val external=9\n"
    );
    a.cancel_window_close();
}

#[test]
fn cancel_after_actual_commit_reply_wins_before_frame_end() {
    let (_root, mut a, _) = fixture();
    start_clean_probe(&mut a);
    breakpoint_ui::settle(&mut a);
    a.window_close_step();
    assert!(a.close_ui.phase == Some(Phase::Committing));
    breakpoint_ui::settle(&mut a);
    assert!(matches!(
        a.close_ui.outcome.as_ref().unwrap().status,
        CloseStatus::Committed { .. }
    ));
    a.cancel_window_close();
    a.window_close_step();
    assert!(!a.close_ui.allow && !a.close_ui.open);
    breakpoint_ui::settle(&mut a);
    assert!(a.debug.bp_report.as_ref().unwrap().close_outcome.is_none());
}

#[test]
fn source_change_invalidates_pending_close_and_retains_the_new_text() {
    let (_root, mut a, _) = fixture();
    start_clean_probe(&mut a);
    a.doc.edit(0..0, "// newer\n").unwrap();
    a.window_close_step();
    assert!(a.close_ui.phase == Some(Phase::Choices) && !a.close_ui.allow);
    assert!(
        a.close_ui
            .error
            .as_ref()
            .unwrap()
            .contains("Sources changed")
    );
    breakpoint_ui::settle(&mut a);
    a.cancel_window_close();
    a.doc.undo().unwrap();
    assert_eq!(a.doc.rope(), "val a=1\n");
}

#[test]
fn newly_pending_preview_after_actual_commit_retains_window_and_releases_seal() {
    let (_root, mut a, _) = fixture();
    start_clean_probe(&mut a);
    breakpoint_ui::settle(&mut a);
    a.window_close_step();
    breakpoint_ui::settle(&mut a);
    a.preview.busy = true;
    a.window_close_step();
    assert!(a.close_ui.phase == Some(Phase::Failed) && !a.close_ui.allow);
    breakpoint_ui::settle(&mut a);
    assert!(a.debug.bp_report.as_ref().unwrap().close_outcome.is_none());
    a.preview.busy = false;
    a.cancel_window_close();
}

#[test]
fn prior_modal_defers_close_and_counter_exhaustion_never_mutates_the_document() {
    let (_root, mut a, _) = fixture();
    a.apply_edit(6, 7, "2");
    a.projects.open = true;
    a.request_window_close();
    a.window_close_step();
    assert!(a.close_ui.requested && !a.close_ui.open && a.projects.open);
    a.cancel_window_close();
    assert!(a.projects.open);
    a.projects.open = false;
    a.close_ui.clock = u64::MAX;
    let id = a.doc.text_id().clone();
    a.request_window_close();
    a.window_close_step();
    assert!(!a.closing_requested() && a.doc.text_id() == &id);
    assert!(a.status.contains("exhausted"));
}

#[test]
fn closing_cancels_deferred_run_but_not_accepted_source_write() {
    let (_root, mut a, _) = fixture();
    let lock = hold(&a);
    a.apply_edit(6, 7, "2");
    a.selected = Some("emulator-5580".into());
    assert!(!a.save_before(save_ui::After::Run));
    waiting(&mut a);
    a.request_window_close();
    a.window_close_step();
    assert!(a.close_ui.requested && !a.close_ui.open);
    a.cancel_window_close();
    drop(lock);
    assert!(a.wait_for_save());
    assert!(!a.running && !a.building && a.last_build.is_none());
    assert_eq!(
        std::fs::read_to_string(a.doc.path.as_ref().unwrap()).unwrap(),
        "val a=2\n"
    );
}

#[test]
fn close_save_authorization_never_bypasses_an_unrelated_review() {
    let (_root, mut a, _) = fixture();
    a.apply_edit(6, 7, "2");
    a.request_window_close();
    a.window_close_step();
    a.file_ui.open = true;
    a.close_choose(true);
    assert!(a.close_ui.phase == Some(Phase::Choices));
    assert!(a.save_ui.pending.is_none() && !a.save_ui.saver.worker_started());
    a.file_ui.open = false;
    a.cancel_window_close();
}

#[test]
fn clean_sources_with_inflight_debug_require_confirmation_and_keep_open_does_not_stop_it() {
    let (_root, mut a, _) = fixture();
    a.debug.busy = true;
    a.request_window_close();
    a.window_close_step();
    assert!(a.close_ui.open && a.close_ui.phase == Some(Phase::Choices));
    assert!(a.close_ui.plan.is_none() && a.close_ui.nonce.is_none());
    assert_eq!(a.close_ui.source_count(), 0);
    a.cancel_window_close();
    assert!(a.debug.busy && !a.closing_requested());
}

#[test]
fn projectless_close_stays_inert_and_untitled_save_is_explicitly_disabled() {
    let root = tempfile::tempdir().unwrap();
    let ctx = egui::Context::default();
    let mut a = Workbench::for_launch(root.path().to_owned(), ctx.clone(), None, false, true);
    a.request_window_close();
    assert!(a.close_fast_exit(&ctx));
    assert!(
        a.language.is_none() && !a.debug.bp_service.active() && !a.save_ui.saver.worker_started()
    );
    let (_root, mut a, _) = fixture();
    a.doc = Document::new("draft").unwrap();
    a.request_window_close();
    a.window_close_step();
    a.close_choose(true);
    assert!(!a.close_ui.allow && a.close_ui.untitled && a.close_ui.error.is_some());
    assert!(a.save_ui.pending.is_none());
    a.cancel_window_close();
}

#[test]
fn actual_external_clean_reload_invalidates_close_with_generic_not_debug_labels() {
    let (_root, mut a, _) = fixture();
    start_clean_probe(&mut a);
    let path = a.doc.path.clone().unwrap();
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg("import pathlib,sys;pathlib.Path(sys.argv[1]).write_text('val external=8\\n')")
        .arg(&path)
        .status()
        .unwrap();
    assert!(status.success());
    a.doc = Document::open(path.clone()).unwrap();
    a.window_close_step();
    assert!(a.close_ui.phase == Some(Phase::Choices) && !a.close_ui.allow);
    assert_eq!(a.close_ui.source_count(), 0);
    assert!(!a.doc.dirty());
    assert_eq!(
        a.close_choices_heading(),
        "Sources changed · confirm closing again"
    );
    assert_eq!(a.close_primary_label(), "Close window");
    assert!(!a.close_ui.choices_painted());
    let ctx = a.context.clone();
    let _ = ctx.run(egui::RawInput::default(), |ctx| a.window_close_dialog(ctx));
    assert!(a.close_ui.choices_painted());
    a.cancel_window_close();
    breakpoint_ui::settle(&mut a);
    assert_eq!(a.doc.rope(), "val external=8\n");
    assert_eq!(std::fs::read_to_string(path).unwrap(), "val external=8\n");
}
