use std::fs;

use hexed::app::{parse_goto, run_script, App};
use hexed::doc::Document;

fn file(content: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("t.bin");
    fs::write(&p, content).unwrap();
    (d, p)
}

fn app(content: &[u8]) -> (tempfile::TempDir, std::path::PathBuf, App) {
    let (d, p) = file(content);
    let a = App::new(Document::open(&p).unwrap());
    (d, p, a)
}

fn play(a: &mut App, keys: &str) -> String {
    run_script(a, keys, 78, 12).unwrap().plain()
}

#[test]
fn the_screen_layout_is_offset_hex_and_ascii() {
    let mut a = App::new(Document::from_bytes(b"Hello, hex editor!\x00\x01\xff".to_vec()));
    let screen = play(&mut a, "");
    let lines: Vec<&str> = screen.lines().collect();
    assert_eq!(lines.len(), 12);
    assert!(lines[0].starts_with(" (memory)  21 B  NORMAL"), "{}", lines[0]);
    assert_eq!(lines[1], "00000000  48 65 6c 6c 6f 2c 20 68  65 78 20 65 64 69 74 6f  |Hello, hex edito|");
    assert_eq!(lines[2].trim_end(), "00000010  72 21 00 01 ff                                    |r!...           |");
    assert!(lines[10].starts_with(" LE u8 72 0x48 | u16 25928"), "{}", lines[10]);
    assert!(lines[11].starts_with(" 0x0 (0)"), "{}", lines[11]);
}

#[test]
fn narrow_terminals_use_fewer_columns() {
    let mut a = App::new(Document::from_bytes((0u8..40).collect()));
    let wide = run_script(&mut a, "", 78, 8).unwrap().plain();
    let narrow = run_script(&mut a, "", 50, 8).unwrap().plain();
    assert!(wide.lines().nth(1).unwrap().contains("00 01 02 03 04 05 06 07  08"));
    let row = narrow.lines().nth(1).unwrap();
    assert!(row.starts_with("00000000  00 01 02 03 04 05 06 07  |"), "{row}");
    let fixed = {
        let mut a = App::new(Document::from_bytes((0u8..40).collect())).columns(4);
        run_script(&mut a, "", 78, 8).unwrap().plain()
    };
    assert!(fixed.lines().nth(2).unwrap().starts_with("00000004  04 05 06 07  |"), "{}", fixed.lines().nth(2).unwrap());
}

#[test]
fn movement_goto_and_scrolling() {
    let (_d, _p, mut a) = app(&(0..=255u8).cycle().take(5000).collect::<Vec<_>>());
    play(&mut a, "l,l,j");
    assert_eq!(a.cursor, 18);
    play(&mut a, "h,k");
    assert_eq!(a.cursor, 1);
    play(&mut a, "end");
    assert_eq!(a.cursor, 15);
    play(&mut a, "home");
    assert_eq!(a.cursor, 0);
    play(&mut a, "G");
    assert_eq!(a.cursor, 4999);
    play(&mut a, "g,text:0x100,enter");
    assert_eq!(a.cursor, 256);
    play(&mut a, "g,text:+16,enter");
    assert_eq!(a.cursor, 272);
    play(&mut a, "g,text:-0x10,enter");
    assert_eq!(a.cursor, 256);
    play(&mut a, "g,text:$,enter");
    assert_eq!(a.cursor, 4999);
    play(&mut a, "g,text:1234,enter");
    assert_eq!(a.cursor, 1234);
    // The row holding the cursor is on screen.
    let screen = play(&mut a, "");
    assert!(screen.contains("000004d0"), "{screen}");
    // A bad goto reports an error and does not move.
    let screen = play(&mut a, "g,text:nope,enter");
    assert_eq!(a.cursor, 1234);
    assert!(screen.contains("'nope' is not a number"), "{screen}");
    // Past the end goes to the last byte.
    play(&mut a, "g,text:999999,enter");
    assert_eq!(a.cursor, 4999);
    // Page keys move by a screen.
    play(&mut a, "g,text:0,enter,pgdn");
    assert_eq!(a.cursor, 9 * 16);
}

#[test]
fn goto_syntax() {
    assert_eq!(parse_goto("0x10", 0, 100), Ok(16));
    assert_eq!(parse_goto(" 42 ", 0, 100), Ok(42));
    assert_eq!(parse_goto("+5", 10, 100), Ok(15));
    assert_eq!(parse_goto("-5", 3, 100), Ok(0));
    assert_eq!(parse_goto("$", 0, 100), Ok(99));
    assert!(parse_goto("", 0, 100).is_err());
    assert!(parse_goto("0xzz", 0, 100).is_err());
}

#[test]
fn overwriting_with_hex_digits_and_saving_in_place() {
    let (_d, p, mut a) = app(b"ABCDEFGH");
    let screen = play(&mut a, "l,e,5");
    assert!(screen.contains("OVERWRITE hex"), "{screen}");
    // one nibble typed so far: shown pending, nothing written yet
    let row = play(&mut a, "").lines().nth(1).unwrap().to_string();
    assert!(row.contains("41 5_ 43"), "{row}");
    assert!(!a.doc.is_modified());
    let screen = play(&mut a, "8");
    assert!(screen.contains("41 58 43"), "5 then 8 wrote 0x58 'X' over 'B': {screen}");
    play(&mut a, "esc");
    assert!(a.doc.is_modified());
    let screen = play(&mut a, "w");
    assert!(screen.contains("saved in place: 1 bytes in 1 run"), "{screen}");
    assert_eq!(fs::read(&p).unwrap(), b"AXCDEFGH");
    assert!(!a.doc.is_modified());
}

#[test]
fn insert_mode_text_entry_backspace_and_delete() {
    let (_d, p, mut a) = app(b"AB");
    play(&mut a, "i,tab,text:xyz");
    assert_eq!(a.doc.read_vec(0, 10), b"xyzAB");
    play(&mut a, "backspace");
    assert_eq!(a.doc.read_vec(0, 10), b"xyAB");
    play(&mut a, "left,delete");
    assert_eq!(a.doc.read_vec(0, 10), b"xAB");
    play(&mut a, "esc,w");
    assert_eq!(fs::read(&p).unwrap(), b"xAB");
    // typing hex in insert mode at the end appends
    play(&mut a, "A,4,3,esc");
    assert_eq!(a.doc.read_vec(0, 10), b"xABC");
    // `i` on the last byte inserts before it
    play(&mut a, "G,i,5,a,esc");
    assert_eq!(a.doc.read_vec(0, 10), b"xABZC");
}

#[test]
fn a_non_hex_key_in_hex_entry_is_refused() {
    let (_d, _p, mut a) = app(b"AB");
    let screen = play(&mut a, "e,z");
    assert!(screen.contains("'z' is not a hex digit"), "{screen}");
    assert_eq!(a.doc.read_vec(0, 2), b"AB");
}

#[test]
fn undo_redo_and_delete() {
    let (_d, _p, mut a) = app(b"ABCDEF");
    play(&mut a, "x");
    assert_eq!(a.doc.read_vec(0, 10), b"BCDEF");
    play(&mut a, "l,x");
    assert_eq!(a.doc.read_vec(0, 10), b"BDEF");
    play(&mut a, "u");
    assert_eq!(a.doc.read_vec(0, 10), b"BCDEF");
    play(&mut a, "u,u");
    assert_eq!(a.doc.read_vec(0, 10), b"ABCDEF");
    let screen = play(&mut a, "u");
    assert!(screen.contains("nothing to undo"), "{screen}");
    play(&mut a, "ctrl-r,ctrl-r");
    assert_eq!(a.doc.read_vec(0, 10), b"BDEF");
}

#[test]
fn search_forward_backward_and_wrap() {
    let (_d, _p, mut a) = app(b"..needle..needle..NEEDLE..\xde\xad\xbe\xef..");
    let screen = play(&mut a, "/,text:needle,enter");
    assert_eq!(a.cursor, 2);
    assert!(screen.contains("found at 0x2"), "{screen}");
    play(&mut a, "n");
    assert_eq!(a.cursor, 10);
    let screen = play(&mut a, "n");
    assert_eq!(a.cursor, 2, "wrapped to the first match: {screen}");
    assert!(screen.contains("wrapped"), "{screen}");
    play(&mut a, "N");
    assert_eq!(a.cursor, 10, "N goes the other way (previous, wrapping from the start)");
    // hex search
    play(&mut a, "/,text:x:de ad be ef,enter");
    assert_eq!(a.cursor, 26);
    // backward search
    play(&mut a, "G,?,text:needle,enter");
    assert_eq!(a.cursor, 10);
    let screen = play(&mut a, "/,text:absent,enter");
    assert!(screen.contains("not found"), "{screen}");
    let screen = play(&mut a, "/,text:x:abc,enter");
    assert!(screen.contains("even number of digits"), "{screen}");
}

#[test]
fn replace_all_is_one_undo_step() {
    let (_d, p, mut a) = app(b"cat dog cat bird cat");
    let screen = play(&mut a, "R,text:cat => lion,enter");
    assert!(screen.contains("replaced 3 occurrences"), "{screen}");
    assert_eq!(a.doc.read_vec(0, 100), b"lion dog lion bird lion");
    play(&mut a, "w");
    assert_eq!(fs::read(&p).unwrap(), b"lion dog lion bird lion");
    play(&mut a, "R,text:lion =>,enter");
    assert_eq!(a.doc.read_vec(0, 100), b" dog  bird ");
    play(&mut a, "u");
    assert_eq!(a.doc.read_vec(0, 100), b"lion dog lion bird lion");
    let screen = play(&mut a, "R,text:nothing here,enter");
    assert!(screen.contains("find => replacement"), "{screen}");
    let screen = play(&mut a, "R,text:zzz => y,enter");
    assert!(screen.contains("no matches"), "{screen}");
}

#[test]
fn quitting_protects_unsaved_changes() {
    let (_d, _p, mut a) = app(b"abc");
    play(&mut a, "q");
    assert!(a.quit, "an unmodified document quits");
    let (_d2, _p2, mut b) = app(b"abc");
    let screen = play(&mut b, "x,q");
    assert!(!b.quit);
    assert!(screen.contains("unsaved changes"), "{screen}");
    play(&mut b, "Q");
    assert!(b.quit);
    let (_d3, _p3, mut c) = app(b"abc");
    play(&mut c, "x,w,q");
    assert!(c.quit, "saved changes do not block quitting");
}

#[test]
fn read_only_mode_refuses_every_change() {
    let (_d, p) = file(b"abc");
    let mut a = App::new(Document::open(&p).unwrap()).readonly(true);
    for keys in ["e", "i", "x", "R", "w"] {
        let screen = play(&mut a, keys);
        assert!(screen.contains("read-only"), "{keys}: {screen}");
    }
    assert!(!a.doc.is_modified());
    assert!(play(&mut a, "").contains("READ-ONLY"));
}

#[test]
fn modified_bytes_are_flagged_in_the_ansi_output() {
    let mut a = App::new(Document::from_bytes(b"abcdef".to_vec()));
    run_script(&mut a, "e,7,a,esc", 78, 8).unwrap();
    let ansi = run_script(&mut a, "", 78, 8).unwrap().ansi();
    assert!(ansi.contains("\x1b[33m7a"), "the edited byte is yellow: {ansi:?}");
    assert!(ansi.contains("\x1b[7m"), "the cursor is reverse video");
}

#[test]
fn help_opens_and_any_key_closes_it() {
    let mut a = App::new(Document::from_bytes(b"abc".to_vec()));
    let screen = play(&mut a, "f1");
    assert!(screen.contains("hx key reference") && screen.contains("find"), "{screen}");
    let screen = play(&mut a, "x");
    assert!(screen.contains("00000000"), "back to the editor");
    assert!(!a.doc.is_modified(), "the key that closed help was not executed");
}

#[test]
fn an_empty_file_shows_a_row_and_can_be_typed_into() {
    let mut a = App::new(Document::from_bytes(Vec::new()));
    let screen = play(&mut a, "");
    assert!(screen.lines().nth(1).unwrap().starts_with("00000000  "), "{screen}");
    play(&mut a, "i,4,1,4,2,esc");
    assert_eq!(a.doc.read_vec(0, 10), b"AB");
}

#[test]
fn big_endian_inspector_and_unknown_keys() {
    let mut a = App::new(Document::from_bytes(vec![0x12, 0x34, 0x56, 0x78, 0, 0, 0, 0]));
    assert!(play(&mut a, "").contains("u16 13330"));
    let screen = play(&mut a, "E");
    assert!(screen.contains("big endian") && screen.contains("u16 4660"), "{screen}");
    assert!(run_script(&mut a, "nonsense", 78, 8).is_err());
}
