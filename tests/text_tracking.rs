use std::ops::Range;
use wexandroidstudio::{
    editor::Document,
    text_tracking::{Projection, TextAnchor},
};

fn token(doc: &Document, text: &str) -> Range<usize> {
    let source = doc.rope().to_string();
    let start = source[..source.find(text).unwrap()].chars().count();
    start..start + text.chars().count()
}
fn tracked(doc: &Document, anchor: &mut TextAnchor) -> String {
    let Projection::Mapped(range) = doc.tracking_snapshot().project(anchor).unwrap() else {
        panic!("expected surviving source span");
    };
    doc.rope().slice(range).to_string()
}

#[test]
fn actual_unicode_crlf_document_insertions_follow_characters_not_line_numbers() {
    let mut d = Document::new("// 😀\r\nfun f() { println(42) }\r\n").unwrap();
    d.defer_syntax();
    let mut a = d
        .tracking_snapshot()
        .anchor(token(&d, "println(42)"))
        .unwrap();
    d.edit(0..0, "// λ\r\n// another\n").unwrap();
    let Projection::Mapped(r) = d.tracking_snapshot().project(&mut a).unwrap() else {
        panic!()
    };
    assert_eq!(d.rope().char_to_line(r.start), 3);
    assert_eq!(d.rope().slice(r).to_string(), "println(42)");
    d.undo().unwrap();
    assert_eq!(tracked(&d, &mut a), "println(42)");
    d.redo().unwrap();
    assert_eq!(tracked(&d, &mut a), "println(42)");
    assert!(d.tree().is_none());
}

#[test]
fn deletion_is_explicit_and_undo_restores_the_exact_source_identity() {
    let mut d = Document::new("first();\ntarget();\nlast();\n").unwrap();
    let r = token(&d, "target();");
    let mut a = d.tracking_snapshot().anchor(r.clone()).unwrap();
    d.edit(r.clone(), "replacement();").unwrap();
    assert_eq!(
        d.tracking_snapshot().project(&mut a).unwrap(),
        Projection::Removed
    );
    d.undo().unwrap();
    assert_eq!(tracked(&d, &mut a), "target();");
    d.redo().unwrap();
    assert_eq!(
        d.tracking_snapshot().project(&mut a).unwrap(),
        Projection::Removed
    );
    d.undo().unwrap();
    d.edit(r.start..r.start + 1, "").unwrap();
    assert_eq!(tracked(&d, &mut a), "arget();");
}

#[test]
fn anchor_created_in_inserted_code_disappears_on_undo_and_returns_on_redo() {
    let mut d = Document::new("base();\n").unwrap();
    d.edit(0..0, "newCode();\n").unwrap();
    let mut a = d
        .tracking_snapshot()
        .anchor(token(&d, "newCode();"))
        .unwrap();
    d.undo().unwrap();
    assert_eq!(
        d.tracking_snapshot().project(&mut a).unwrap(),
        Projection::Removed
    );
    d.redo().unwrap();
    assert_eq!(tracked(&d, &mut a), "newCode();");
}

#[test]
fn real_worker_fork_and_atomic_batch_do_not_mutate_original_anchor_or_document() {
    let mut d = Document::new("fun f(){\nprintln(42)\n}\n").unwrap();
    let original_id = d.tracking_snapshot().id().clone();
    let mut a = d
        .tracking_snapshot()
        .anchor(token(&d, "println(42)"))
        .unwrap();
    let mut fork = d.fork().unwrap();
    fork.edit_batch_parsed(&[
        (7..7, " ".into()),
        (9..9, "    ".into()),
        (20..20, ";".into()),
    ])
    .unwrap();
    assert_eq!(d.tracking_snapshot().id(), &original_id);
    assert_eq!(tracked(&fork, &mut a), "println(42)");
    assert_eq!(tracked(&d, &mut a), "println(42)");
    d = fork;
    assert_ne!(d.tracking_snapshot().id(), &original_id);
    assert_eq!(tracked(&d, &mut a), "println(42)");
    d.undo().unwrap();
    assert_eq!(d.tracking_snapshot().id(), &original_id);
    assert_eq!(tracked(&d, &mut a), "println(42)");
}

#[test]
fn equal_bytes_and_equal_wire_clocks_are_not_document_identity() {
    let d = Document::new("val a = 1\n").unwrap();
    let other = Document::new("val a = 1\n").unwrap();
    let mut a = d.tracking_snapshot().anchor(0..3).unwrap();
    assert_eq!(d.content_hash(), other.content_hash());
    assert_eq!(d.version, other.version);
    assert_eq!(
        other.tracking_snapshot().project(&mut a).unwrap(),
        Projection::Unrelated
    );
    let mut f = d.fork().unwrap();
    let mut g = d.fork().unwrap();
    f.edit(0..0, "f ").unwrap();
    g.edit(0..0, "g ").unwrap();
    assert_eq!(f.version, g.version);
    assert_ne!(f.tracking_snapshot().id(), g.tracking_snapshot().id());
    assert_eq!(tracked(&f, &mut a), "val");
    assert_eq!(tracked(&g, &mut a), "val");
}

#[test]
fn noop_rejected_and_exhausted_edits_preserve_tracking_state_and_history() {
    let mut d = Document::new("val a = 1\n").unwrap();
    let id = d.tracking_snapshot().id().clone();
    d.edit(0..3, "val").unwrap();
    d.edit_batch(&[(0..3, "val".into())]).unwrap();
    assert!(
        d.edit_batch(&[(0..5, "x".into()), (4..6, "y".into())])
            .is_err()
    );
    assert_eq!(d.tracking_snapshot().id(), &id);
    assert!(!d.undo().unwrap());
    d.version = i32::MAX;
    assert!(d.edit(0..0, "x").is_err());
    assert!(d.edit_batch(&[(0..0, "x".into())]).is_err());
    assert_eq!(d.tracking_snapshot().id(), &id);
}

#[test]
fn save_receipt_of_an_older_revision_keeps_newer_anchor_and_undo_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("A.kt");
    std::fs::write(&p, "target();\n").unwrap();
    let mut d = Document::open(&p).unwrap();
    let mut a = d
        .tracking_snapshot()
        .anchor(token(&d, "target();"))
        .unwrap();
    d.edit(0..0, "// captured\n").unwrap();
    let snapshot = d.save_snapshot().unwrap();
    d.edit(0..0, "// newer\n").unwrap();
    let id = d.tracking_snapshot().id().clone();
    let receipt = snapshot.write(|| false).unwrap();
    d.accept_saved(&receipt).unwrap();
    assert_eq!(d.tracking_snapshot().id(), &id);
    assert!(d.dirty());
    assert_eq!(tracked(&d, &mut a), "target();");
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "// captured\ntarget();\n"
    );
    d.undo().unwrap();
    assert!(!d.dirty());
    assert_eq!(tracked(&d, &mut a), "target();");
}

#[test]
fn joined_lines_keep_two_anchor_identities_and_undo_can_split_them_again() {
    let mut d = Document::new("first();\nsecond();\n").unwrap();
    let mut a = d.tracking_snapshot().anchor(token(&d, "first();")).unwrap();
    let mut b = d
        .tracking_snapshot()
        .anchor(token(&d, "second();"))
        .unwrap();
    d.edit(8..9, " ").unwrap();
    for marker in [&mut a, &mut b] {
        let Projection::Mapped(r) = d.tracking_snapshot().project(marker).unwrap() else {
            panic!()
        };
        assert_eq!(d.rope().char_to_line(r.start), 0);
    }
    d.undo().unwrap();
    assert_eq!(tracked(&d, &mut a), "first();");
    let Projection::Mapped(r) = d.tracking_snapshot().project(&mut b).unwrap() else {
        panic!()
    };
    assert_eq!(d.rope().char_to_line(r.start), 1);
}

#[test]
fn observed_anchors_continue_after_history_pruning_unobserved_ones_are_explicit() {
    let mut d = Document::new("target();\n").unwrap();
    d.defer_syntax();
    let mut live = d.tracking_snapshot().anchor(0..9).unwrap();
    let mut missed = d.tracking_snapshot().anchor(0..9).unwrap();
    for _ in 0..300 {
        d.edit(0..0, " ").unwrap();
        assert_eq!(tracked(&d, &mut live), "target();");
    }
    assert_eq!(
        d.tracking_snapshot().project(&mut missed).unwrap(),
        Projection::Unrelated
    );
    assert_eq!(tracked(&d, &mut live), "target();");
}
