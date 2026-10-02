use std::fs;

use hexed::doc::{Document, SaveKind};

fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

#[test]
fn an_overwrite_only_edit_is_patched_in_place() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    let orig = bytes(100_000);
    fs::write(&p, &orig).unwrap();
    let mut doc = Document::open(&p).unwrap();
    doc.overwrite(10, b"HELLO");
    doc.overwrite(99_990, b"WORLD");
    assert!(doc.is_modified());
    let kind = doc.save().unwrap();
    assert_eq!(kind, SaveKind::InPlace { runs: 2, bytes: 10 });
    let mut want = orig.clone();
    want[10..15].copy_from_slice(b"HELLO");
    want[99_990..99_995].copy_from_slice(b"WORLD");
    assert_eq!(fs::read(&p).unwrap(), want);
    assert!(!doc.is_modified());
    // The document keeps working on the saved file, and can be saved again.
    assert_eq!(doc.read_vec(8, 9), &want[8..17]);
    doc.overwrite(0, b"Z");
    assert!(matches!(doc.save().unwrap(), SaveKind::InPlace { runs: 1, bytes: 1 }));
    assert_eq!(fs::read(&p).unwrap()[0], b'Z');
}

#[test]
fn a_length_change_rewrites_the_file_through_a_temporary() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    let orig = bytes(50_000);
    fs::write(&p, &orig).unwrap();
    let mut doc = Document::open(&p).unwrap();
    doc.insert(100, b"INSERTED");
    doc.delete(20_000, 500);
    let kind = doc.save().unwrap();
    assert_eq!(kind, SaveKind::Rewrite { bytes: 50_000 + 8 - 500 });
    let mut want = orig.clone();
    want.splice(100..100, b"INSERTED".iter().copied());
    want.drain(20_000..20_500);
    assert_eq!(fs::read(&p).unwrap(), want);
    let leftovers: Vec<_> = fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(leftovers, ["f.bin"], "no temporary file left behind");
    // Still editable after the rewrite, and the history starts fresh.
    assert!(!doc.is_modified());
    assert!(doc.undo().is_none());
    doc.insert(0, b"+");
    doc.save().unwrap();
    assert_eq!(fs::read(&p).unwrap()[0], b'+');
    assert_eq!(fs::read(&p).unwrap().len(), want.len() + 1);
}

#[test]
fn saving_an_unchanged_document_does_nothing() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"abc").unwrap();
    let mut doc = Document::open(&p).unwrap();
    assert_eq!(doc.save().unwrap(), SaveKind::Unchanged);
    doc.insert(0, b"x");
    doc.undo();
    assert_eq!(doc.save().unwrap(), SaveKind::Unchanged, "undone changes are not changes");
}

#[test]
fn a_new_or_empty_file_can_be_created_and_grown() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("new.bin");
    let mut doc = Document::open(&p).unwrap();
    assert_eq!(doc.len(), 0);
    doc.insert(0, b"fresh");
    assert_eq!(doc.save().unwrap(), SaveKind::Rewrite { bytes: 5 });
    assert_eq!(fs::read(&p).unwrap(), b"fresh");
    // An existing zero-length file behaves the same.
    let e = d.path().join("empty.bin");
    fs::write(&e, b"").unwrap();
    let mut doc = Document::open(&e).unwrap();
    doc.insert(0, b"x");
    doc.save().unwrap();
    assert_eq!(fs::read(&e).unwrap(), b"x");
}

#[test]
fn save_as_writes_a_copy_and_switches_to_it() {
    let d = tempfile::tempdir().unwrap();
    let a = d.path().join("a.bin");
    let b = d.path().join("b.bin");
    fs::write(&a, b"original").unwrap();
    let mut doc = Document::open(&a).unwrap();
    doc.overwrite(0, b"O");
    doc.save_as(&b).unwrap();
    assert_eq!(fs::read(&a).unwrap(), b"original", "the first file is untouched");
    assert_eq!(fs::read(&b).unwrap(), b"Original");
    doc.overwrite(1, b"R");
    doc.save().unwrap();
    assert_eq!(fs::read(&b).unwrap(), b"ORiginal");
    assert_eq!(fs::read(&a).unwrap(), b"original");
}

#[test]
fn a_document_without_a_name_cannot_save() {
    let mut doc = Document::from_bytes(b"abc".to_vec());
    doc.insert(0, b"x");
    assert!(doc.save().is_err());
}

#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
#[test]
fn a_failed_save_keeps_the_edits_and_the_original_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("locked.bin");
    let orig = bytes(10_000);
    fs::write(&p, &orig).unwrap();
    let mut doc = Document::open(&p).unwrap();
    doc.insert(5, b"EDIT");
    // Windows refuses to replace a read-only file, so the rename at the end of the rewrite fails.
    let mut perms = fs::metadata(&p).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&p, perms).unwrap();
    assert!(doc.save().is_err());
    assert_eq!(fs::read(&p).unwrap(), orig, "the file on disk is unchanged");
    assert!(doc.is_modified(), "the edit is still pending");
    assert_eq!(doc.read_vec(3, 8), [orig[3], orig[4], b'E', b'D', b'I', b'T', orig[5], orig[6]]);
    assert!(!d.path().join("locked.bin.hx-tmp").exists());
    // Once the obstacle is gone the same document saves fine.
    let mut perms = fs::metadata(&p).unwrap().permissions();
    perms.set_readonly(false);
    fs::set_permissions(&p, perms).unwrap();
    doc.save().unwrap();
    assert_eq!(fs::read(&p).unwrap().len(), 10_004);
}

#[test]
fn write_to_streams_the_edited_content() {
    let mut doc = Document::from_bytes(b"0123456789".to_vec());
    doc.delete(2, 3);
    doc.insert(0, b"ab");
    let mut out = Vec::new();
    assert_eq!(doc.write_to(&mut out).unwrap(), 9);
    assert_eq!(out, b"ab0156789");
}
