//! Random edits applied to the piece table and to a plain `Vec<u8>` model. After every step the
//! two must agree on content, length, which bytes count as modified, and what undo and redo give.

use hexed::piece::{PieceTable, Src};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
}

/// The model: every byte remembers the position it had in the original file, if it still is the
/// original byte. A byte is "modified" unless it is at that same position.
#[derive(Clone, PartialEq, Debug)]
struct Model {
    bytes: Vec<(u8, Option<u64>)>,
}

impl Model {
    fn content(&self) -> Vec<u8> {
        self.bytes.iter().map(|b| b.0).collect()
    }
    fn flags(&self) -> Vec<bool> {
        self.bytes.iter().enumerate().map(|(i, b)| b.1 != Some(i as u64)).collect()
    }
}

fn check(t: &PieceTable, orig: &[u8], m: &Model, rng: &mut Rng, ctx: &str) {
    let want = m.content();
    assert_eq!(t.len(), want.len() as u64, "length, {ctx}");
    let mut got = vec![0u8; want.len()];
    assert_eq!(t.read(orig, 0, &mut got), want.len(), "read length, {ctx}");
    assert_eq!(got, want, "content, {ctx}");
    // invariants: no empty pieces, in-bounds sources
    for p in t.pieces() {
        assert!(p.len > 0, "empty piece, {ctx}");
        if p.src == Src::Orig {
            assert!(p.start + p.len <= orig.len() as u64, "orig piece out of bounds, {ctx}");
        }
    }
    assert_eq!(t.pieces().iter().map(|p| p.len).sum::<u64>(), t.len(), "pieces cover the file, {ctx}");
    // a random window, for read and flags
    if !want.is_empty() {
        let off = rng.below(want.len() as u64) as usize;
        let len = rng.below((want.len() - off) as u64 + 1) as usize;
        let mut buf = vec![0u8; len];
        assert_eq!(t.read(orig, off as u64, &mut buf), len, "window read, {ctx}");
        assert_eq!(buf, want[off..off + len], "window content, {ctx}");
        assert_eq!(t.modified_flags(off as u64, len), m.flags()[off..off + len], "flags at {off}+{len}, {ctx}");
    }
}

fn run(seed: u64, orig_len: usize, steps: usize) {
    let mut rng = Rng(seed | 1);
    let orig: Vec<u8> = (0..orig_len).map(|_| rng.next() as u8).collect();
    let mut t = PieceTable::new(orig_len as u64);
    let mut m = Model { bytes: orig.iter().enumerate().map(|(i, &b)| (b, Some(i as u64))).collect() };
    let mut undo: Vec<Model> = Vec::new();
    let mut redo: Vec<Model> = Vec::new();
    // The undo-stack height at the last save, if that state can still be reached. "Modified" means
    // the history is at a different position, not that the bytes differ: insert then delete leaves
    // equal bytes but is still a change.
    let mut saved_pos: Option<usize> = Some(0);

    for step in 0..steps {
        let len = m.bytes.len() as u64;
        let op = rng.below(100);
        let ctx = format!("seed {seed} step {step} op {op}");
        let before = m.clone();
        let changed;
        match op {
            0..=29 => {
                let at = rng.below(len + 1) as usize;
                let n = 1 + rng.below(6) as usize;
                let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                changed = true;
                t.insert(at as u64, &bytes);
                m.bytes.splice(at..at, bytes.iter().map(|&b| (b, None)));
            }
            30..=49 => {
                let at = rng.below(len + 1);
                let n = rng.below(8).min(len - at);
                changed = n > 0;
                t.delete(at, n);
                m.bytes.drain(at as usize..(at + n) as usize);
            }
            50..=64 => {
                let at = rng.below(len + 1) as usize;
                let n = 1 + rng.below(5) as usize;
                let bytes: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                changed = true;
                t.overwrite(at as u64, &bytes);
                let end = (at + n).min(m.bytes.len());
                m.bytes.splice(at..end, bytes.iter().map(|&b| (b, None)));
            }
            65..=72 => {
                // several replacements as one step: sorted and non-overlapping
                let mut ops: Vec<(u64, u64, Vec<u8>)> = Vec::new();
                let mut pos = 0u64;
                while pos <= len && ops.len() < 5 {
                    let at = pos + rng.below(len - pos + 1);
                    let remove = rng.below(4).min(len - at);
                    let insert: Vec<u8> = (0..rng.below(4)).map(|_| rng.next() as u8).collect();
                    ops.push((at, remove, insert));
                    pos = at + remove + 1;
                }
                changed = ops.iter().any(|(_, r, i)| *r > 0 || !i.is_empty());
                t.replace_many(&ops);
                for (at, remove, insert) in ops.iter().rev() {
                    let (a, e) = (*at as usize, (*at + *remove) as usize);
                    m.bytes.splice(a..e, insert.iter().map(|&b| (b, None)));
                }
            }
            73..=86 => {
                if let Some(prev) = undo.pop() {
                    assert!(t.undo().is_some(), "undo available, {ctx}");
                    redo.push(m.clone());
                    m = prev;
                } else {
                    assert!(t.undo().is_none(), "nothing to undo, {ctx}");
                }
                check(&t, &orig, &m, &mut rng, &ctx);
                assert_eq!(t.is_modified(), saved_pos != Some(undo.len()), "modified flag after undo, {ctx}");
                continue;
            }
            87..=94 => {
                if let Some(next) = redo.pop() {
                    assert!(t.redo().is_some(), "redo available, {ctx}");
                    undo.push(m.clone());
                    m = next;
                } else {
                    assert!(t.redo().is_none(), "nothing to redo, {ctx}");
                }
                check(&t, &orig, &m, &mut rng, &ctx);
                assert_eq!(t.is_modified(), saved_pos != Some(undo.len()), "modified flag after redo, {ctx}");
                continue;
            }
            _ => {
                changed = false;
                t.mark_saved();
                saved_pos = Some(undo.len());
            }
        }
        if changed {
            // A new edit discards the redo stack; a saved state that lived there is gone.
            if saved_pos.is_some_and(|p| p > undo.len()) {
                saved_pos = None;
            }
            undo.push(before);
            redo.clear();
        }
        check(&t, &orig, &m, &mut rng, &ctx);
        assert_eq!(t.is_modified(), saved_pos != Some(undo.len()), "modified flag, {ctx}");
    }
}

#[test]
fn random_edits_agree_with_a_vec() {
    for seed in 1..=60u64 {
        run(seed * 0x9E37_79B9, (seed as usize * 7) % 40, 600);
    }
}

#[test]
fn random_edits_on_a_larger_file_and_many_pieces() {
    for seed in 100..104u64 {
        run(seed * 0x2545_F491, 5000, 4000);
    }
}

#[test]
fn an_empty_original_works() {
    for seed in 200..210u64 {
        run(seed, 0, 400);
    }
}
