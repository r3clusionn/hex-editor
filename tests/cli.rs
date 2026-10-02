use std::fs;
use std::process::Command;

fn hx(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hx")).args(args).output().unwrap()
}

fn text(o: &std::process::Output) -> String {
    String::from_utf8_lossy(&o.stdout).replace("\r\n", "\n")
}

#[test]
fn keys_replay_prints_the_final_screen() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"0123456789abcdef0123456789ABCDEF").unwrap();
    let o = hx(&[p.to_str().unwrap(), "--keys", "j,l,l", "--screen", "80x8"]);
    assert!(o.status.success());
    let out = text(&o);
    assert!(out.contains("f.bin  32 B  NORMAL"), "{out}");
    assert!(out.contains("00000010  30 31 32 33"), "{out}");
    assert!(out.contains(" 0x12 (18)"), "the cursor moved a row down and two right: {out}");
    // --offset and --columns
    let o = hx(&[p.to_str().unwrap(), "-o", "0x10", "-c", "8", "--keys", "", "--screen", "70x8"]);
    assert!(text(&o).contains(" 0x10 (16)"), "{}", text(&o));
    assert!(text(&o).contains("00000008  38 39 61 62"), "{}", text(&o));
}

#[test]
fn edits_made_with_keys_are_saved_to_the_file() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"hello world").unwrap();
    let o = hx(&[p.to_str().unwrap(), "--keys", "e,4,a,esc,/,text:world,enter,i,tab,text:big ,esc,w", "--screen", "70x8"]);
    assert!(o.status.success());
    assert_eq!(fs::read(&p).unwrap(), b"Jello big world");
}

#[test]
fn colours_are_printed_only_with_ansi() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"abc\0").unwrap();
    assert!(!text(&hx(&[p.to_str().unwrap(), "--keys", "", "--screen", "60x6"])).contains('\x1b'));
    assert!(text(&hx(&[p.to_str().unwrap(), "--keys", "", "--screen", "60x6", "--ansi"])).contains("\x1b[7m"));
}

#[test]
fn errors_exit_with_code_2() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"x").unwrap();
    let bad_key = hx(&[p.to_str().unwrap(), "--keys", "bogus"]);
    assert_eq!(bad_key.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad_key.stderr).contains("unknown key 'bogus'"));
    assert_eq!(hx(&[p.to_str().unwrap(), "--keys", "", "--screen", "wide"]).status.code(), Some(2));
    // Without --keys a terminal is required; under test standard output is a pipe.
    let o = hx(&[p.to_str().unwrap()]);
    assert_eq!(o.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&o.stderr).contains("not a terminal"));
    // a directory is not a file
    assert_eq!(hx(&[d.path().to_str().unwrap(), "--keys", ""]).status.code(), Some(2));
}

#[test]
fn readonly_flag_blocks_edits_from_the_command_line() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("f.bin");
    fs::write(&p, b"keep me").unwrap();
    let o = hx(&["--readonly", p.to_str().unwrap(), "--keys", "x,w", "--screen", "70x8"]);
    assert!(text(&o).contains("read-only"), "{}", text(&o));
    assert_eq!(fs::read(&p).unwrap(), b"keep me");
}
