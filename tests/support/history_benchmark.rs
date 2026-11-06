use std::{hint::black_box, time::Instant};
use wexandroidstudio::editor::Document;
fn main() {
    let mut text = String::from("// Real Kotlin document; no synthetic parser delay\n");
    for n in 0..100000 {
        text.push_str(&format!("private val property{n} = {n}\n"));
    }
    let start = Instant::now();
    let mut d = Document::new(&text).unwrap();
    let open = start.elapsed().as_micros();
    let original = d.content_hash();
    let mut edit = vec![];
    let mut undo = vec![];
    let mut redo = vec![];
    for n in 0..8 {
        let p = d.rope().len_chars();
        d.set_view(p, p);
        let start = Instant::now();
        d.edit(p..p, &format!("// appended {n} 😀\n")).unwrap();
        edit.push(start.elapsed().as_micros());
    }
    let hstart = Instant::now();
    let changed = d.content_hash();
    let first_hash = hstart.elapsed().as_micros();
    let start = Instant::now();
    for _ in 0..64 {
        assert_eq!(black_box(d.content_hash()), changed);
    }
    let repeated_hash = start.elapsed().as_micros();
    for _ in 0..8 {
        let start = Instant::now();
        assert!(d.undo().unwrap());
        undo.push(start.elapsed().as_micros());
    }
    assert_eq!(d.rope().to_string(), text);
    assert!(!d.dirty());
    assert_eq!(d.content_hash(), original);
    for _ in 0..8 {
        let start = Instant::now();
        assert!(d.redo().unwrap());
        redo.push(start.elapsed().as_micros());
    }
    assert_eq!(d.content_hash(), changed);
    assert!(d.dirty());
    assert!(!d.tree().unwrap().root_node().has_error());
    assert_eq!(
        d.tree().unwrap().root_node().end_byte(),
        d.rope().len_bytes()
    );
    println!(
        "{{\"result\":\"REAL_HISTORY_BENCH_OK\",\"lines\":100000,\"bytes\":{},\"open_us\":{open},\"edit_us\":{edit:?},\"undo_us\":{undo:?},\"redo_us\":{redo:?},\"first_hash_us\":{first_hash},\"repeated_hash_64_us\":{repeated_hash},\"profile\":\"release\",\"synthetic_delay\":false}}",
        text.len()
    );
}
