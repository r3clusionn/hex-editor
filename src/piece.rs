//! A piece table: the file as a list of pieces, each pointing into either the original file or an
//! append-only buffer of added bytes. Editing never moves file data, so inserting into or deleting
//! from a 20 GB file costs the same as doing it in a 20 byte one, and the original can stay
//! memory-mapped and untouched until the edits are saved.
//!
//! The table does not own the original bytes. Reading takes them as an argument, so the same table
//! works over a memory map or a `Vec`.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Src {
    Orig,
    Add,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Piece {
    pub src: Src,
    pub start: u64,
    pub len: u64,
}

/// One undoable change: `old` pieces starting at index `at` were replaced by `new`.
#[derive(Clone, Debug)]
struct Edit {
    at: usize,
    old: Vec<Piece>,
    new: Vec<Piece>,
}

#[derive(Clone, Debug)]
pub struct PieceTable {
    orig_len: u64,
    add: Vec<u8>,
    pieces: Vec<Piece>,
    /// `ends[i]` is the logical offset just past `pieces[i]`.
    ends: Vec<u64>,
    undo: Vec<Edit>,
    redo: Vec<Edit>,
    /// Length of `undo` when the content was last saved, or `None` if that state can no longer be reached.
    saved_at: Option<usize>,
}

/// Most undo steps kept; older ones are dropped.
const MAX_UNDO: usize = 100_000;

impl PieceTable {
    pub fn new(orig_len: u64) -> PieceTable {
        let pieces = if orig_len > 0 { vec![Piece { src: Src::Orig, start: 0, len: orig_len }] } else { Vec::new() };
        let mut t = PieceTable { orig_len, add: Vec::new(), pieces, ends: Vec::new(), undo: Vec::new(), redo: Vec::new(), saved_at: Some(0) };
        t.reindex();
        t
    }

    pub fn len(&self) -> u64 {
        self.ends.last().copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn orig_len(&self) -> u64 {
        self.orig_len
    }

    pub fn piece_count(&self) -> usize {
        self.pieces.len()
    }

    pub fn pieces(&self) -> &[Piece] {
        &self.pieces
    }

    /// True when the content differs from what was last saved.
    pub fn is_modified(&self) -> bool {
        self.saved_at != Some(self.undo.len())
    }

    pub fn mark_saved(&mut self) {
        self.saved_at = Some(self.undo.len());
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    fn reindex(&mut self) {
        self.ends.clear();
        let mut pos = 0;
        for p in &self.pieces {
            pos += p.len;
            self.ends.push(pos);
        }
    }

    /// Index of the piece that contains logical `offset` (`pieces.len()` when `offset == len`).
    fn piece_at(&self, offset: u64) -> usize {
        self.ends.partition_point(|&e| e <= offset)
    }

    fn piece_start(&self, i: usize) -> u64 {
        if i == 0 {
            0
        } else {
            self.ends[i - 1]
        }
    }

    /// Copies up to `buf.len()` bytes starting at logical `offset`; returns how many were copied.
    pub fn read(&self, orig: &[u8], offset: u64, buf: &mut [u8]) -> usize {
        let mut done = 0;
        let mut i = self.piece_at(offset);
        let mut pos = offset;
        while done < buf.len() && i < self.pieces.len() {
            let p = self.pieces[i];
            let skip = pos - self.piece_start(i);
            let take = ((p.len - skip) as usize).min(buf.len() - done);
            let from = (p.start + skip) as usize;
            let src = match p.src {
                Src::Orig => &orig[from..from + take],
                Src::Add => &self.add[from..from + take],
            };
            buf[done..done + take].copy_from_slice(src);
            done += take;
            pos += take as u64;
            i += 1;
        }
        done
    }

    /// For each of `len` bytes from `offset`, whether it differs from the original file at the
    /// same position (added bytes, and original bytes that moved).
    pub fn modified_flags(&self, offset: u64, len: usize) -> Vec<bool> {
        let mut flags = vec![false; len];
        let end = offset + len as u64;
        let mut i = self.piece_at(offset);
        while i < self.pieces.len() {
            let p = self.pieces[i];
            let ps = self.piece_start(i);
            if ps >= end {
                break;
            }
            let moved = p.src == Src::Add || ps != p.start;
            if moved {
                let from = ps.max(offset);
                let to = (ps + p.len).min(end);
                for f in &mut flags[(from - offset) as usize..(to - offset) as usize] {
                    *f = true;
                }
            }
            i += 1;
        }
        flags
    }

    /// The pieces that cover logical `[from, to)`, with the first and last trimmed to fit.
    fn slice(&self, from: u64, to: u64) -> Vec<Piece> {
        let mut out = Vec::new();
        if from >= to {
            return out;
        }
        let mut i = self.piece_at(from);
        while i < self.pieces.len() {
            let ps = self.piece_start(i);
            if ps >= to {
                break;
            }
            let p = self.pieces[i];
            let a = from.max(ps) - ps;
            let b = to.min(ps + p.len) - ps;
            out.push(Piece { src: p.src, start: p.start + a, len: b - a });
            i += 1;
        }
        out
    }

    fn push_add(&mut self, bytes: &[u8]) -> Piece {
        let start = self.add.len() as u64;
        self.add.extend_from_slice(bytes);
        Piece { src: Src::Add, start, len: bytes.len() as u64 }
    }

    /// Replaces `remove` bytes at `offset` by `insert` (either may be empty). Offsets past the end
    /// are clamped.
    pub fn splice(&mut self, offset: u64, remove: u64, insert: &[u8]) {
        let len = self.len();
        let offset = offset.min(len);
        let remove = remove.min(len - offset);
        if remove == 0 && insert.is_empty() {
            return;
        }
        let end = offset + remove;
        let first = self.piece_at(offset);
        // The pieces the change overlaps. A pure insertion only overlaps the piece it splits, and
        // none at all when it lands on a piece boundary.
        let hi = if remove == 0 {
            if first < self.pieces.len() && self.piece_start(first) < offset {
                first + 1
            } else {
                first
            }
        } else {
            let mut hi = first;
            while hi < self.pieces.len() && self.piece_start(hi) < end {
                hi += 1;
            }
            hi
        };
        let mut at = first;
        let mut old: Vec<Piece> = self.pieces[first..hi].to_vec();
        let (range_start, range_end) = if old.is_empty() { (offset, offset) } else { (self.piece_start(first), self.ends[hi - 1]) };

        let mut new = self.slice(range_start, offset);
        let add_start = self.add.len() as u64;
        if !insert.is_empty() {
            let p = self.push_add(insert);
            new.push(p);
        }
        new.extend(self.slice(end, range_end));

        // Typing right after an added run extends that run instead of making a new piece.
        if !insert.is_empty() && range_start == offset && at > 0 {
            let prev = self.pieces[at - 1];
            if prev.src == Src::Add && prev.start + prev.len == add_start {
                old.insert(0, prev);
                new.insert(0, prev);
                at -= 1;
            }
        }
        self.apply(Edit { at, old, new: coalesce(new) }, true);
    }

    pub fn insert(&mut self, offset: u64, bytes: &[u8]) {
        self.splice(offset, 0, bytes);
    }

    pub fn delete(&mut self, offset: u64, len: u64) {
        self.splice(offset, len, &[]);
    }

    /// Overwrites bytes in place; bytes that fit past the end of the file extend it.
    pub fn overwrite(&mut self, offset: u64, bytes: &[u8]) {
        self.splice(offset, bytes.len() as u64, bytes);
    }

    /// Replaces many ranges at once as one undo step. `ops` are `(offset, remove, insert)`, sorted
    /// by offset and not overlapping.
    pub fn replace_many(&mut self, ops: &[(u64, u64, Vec<u8>)]) {
        // Nothing to do (and no undo step to record) unless some operation changes something.
        if !ops.iter().any(|(_, remove, insert)| *remove > 0 || !insert.is_empty()) {
            return;
        }
        let len = self.len();
        let mut new: Vec<Piece> = Vec::new();
        let mut cursor = 0u64;
        for (offset, remove, insert) in ops {
            debug_assert!(*offset >= cursor && offset + remove <= len);
            new.extend(self.slice(cursor, *offset));
            if !insert.is_empty() {
                let p = self.push_add(insert);
                new.push(p);
            }
            cursor = offset + remove;
        }
        new.extend(self.slice(cursor, len));
        let new = coalesce(new);
        let old = self.pieces.clone();
        self.apply(Edit { at: 0, old, new }, true);
    }

    fn apply(&mut self, e: Edit, record: bool) {
        self.pieces.splice(e.at..e.at + e.old.len(), e.new.iter().copied());
        self.reindex();
        if record {
            self.redo.clear();
            if let Some(s) = self.saved_at {
                if s > self.undo.len() {
                    self.saved_at = None;
                }
            }
            self.undo.push(e);
            if self.undo.len() > MAX_UNDO {
                self.undo.remove(0);
                self.saved_at = self.saved_at.and_then(|s| s.checked_sub(1));
            }
        }
    }

    /// Reverts the last change. Returns the logical offset where it happened.
    pub fn undo(&mut self) -> Option<u64> {
        let e = self.undo.pop()?;
        let at_offset = self.piece_start(e.at.min(self.pieces.len()));
        let inverse = Edit { at: e.at, old: e.new.clone(), new: e.old.clone() };
        self.apply(inverse, false);
        self.redo.push(e);
        Some(at_offset)
    }

    pub fn redo(&mut self) -> Option<u64> {
        let e = self.redo.pop()?;
        let at_offset = self.piece_start(e.at.min(self.pieces.len()));
        self.apply(e.clone(), false);
        self.undo.push(e);
        Some(at_offset)
    }

    /// True when every original byte is still at its original position and the length is
    /// unchanged, so the file can be patched in place instead of rewritten.
    pub fn is_overwrite_only(&self) -> bool {
        self.len() == self.orig_len
            && self.pieces.iter().enumerate().all(|(i, p)| p.src == Src::Add || self.piece_start(i) == p.start)
    }

    /// The edited runs of an overwrite-only table: (logical offset, piece) for every added piece.
    pub fn added_runs(&self) -> Vec<(u64, Piece)> {
        self.pieces.iter().enumerate().filter(|(_, p)| p.src == Src::Add).map(|(i, p)| (self.piece_start(i), *p)).collect()
    }

    pub fn add_bytes(&self, p: Piece) -> &[u8] {
        &self.add[p.start as usize..(p.start + p.len) as usize]
    }
}

/// Merges neighbouring pieces that are contiguous in the same source and drops empty ones.
fn coalesce(new: Vec<Piece>) -> Vec<Piece> {
    let mut out: Vec<Piece> = Vec::with_capacity(new.len());
    for p in new {
        if p.len == 0 {
            continue;
        }
        match out.last_mut() {
            Some(q) if q.src == p.src && q.start + q.len == p.start => q.len += p.len,
            _ => out.push(p),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(t: &PieceTable, orig: &[u8]) -> Vec<u8> {
        let mut v = vec![0; t.len() as usize];
        assert_eq!(t.read(orig, 0, &mut v), v.len());
        v
    }

    #[test]
    fn starts_as_one_piece_over_the_original() {
        let t = PieceTable::new(10);
        assert_eq!((t.len(), t.piece_count(), t.is_modified()), (10, 1, false));
        assert_eq!(PieceTable::new(0).piece_count(), 0);
    }

    #[test]
    fn insert_delete_overwrite() {
        let orig = b"hello world".to_vec();
        let mut t = PieceTable::new(orig.len() as u64);
        t.insert(5, b",");
        assert_eq!(text(&t, &orig), b"hello, world");
        t.delete(0, 1);
        assert_eq!(text(&t, &orig), b"ello, world");
        t.overwrite(0, b"J");
        assert_eq!(text(&t, &orig), b"Jllo, world");
        t.overwrite(10, b"DX"); // runs past the end: extends
        assert_eq!(text(&t, &orig), b"Jllo, worlDX");
        t.insert(t.len(), b"!");
        assert_eq!(text(&t, &orig), b"Jllo, worlDX!");
    }

    #[test]
    fn typing_in_a_row_extends_one_piece() {
        let orig = vec![0u8; 100];
        let mut t = PieceTable::new(100);
        for (i, b) in b"abcdef".iter().enumerate() {
            t.insert(50 + i as u64, &[*b]);
        }
        // original before, one added run, original after
        assert_eq!(t.piece_count(), 3);
        assert_eq!(&text(&t, &orig)[50..56], b"abcdef");
    }

    #[test]
    fn undo_and_redo_restore_the_content_and_the_modified_flag() {
        let orig = b"0123456789".to_vec();
        let mut t = PieceTable::new(10);
        t.insert(3, b"XX");
        t.delete(0, 2);
        t.overwrite(4, b"Q");
        let after = text(&t, &orig);
        assert!(t.is_modified());
        while t.undo().is_some() {}
        assert_eq!(text(&t, &orig), orig);
        assert!(!t.is_modified());
        while t.redo().is_some() {}
        assert_eq!(text(&t, &orig), after);
    }

    #[test]
    fn saved_state_tracks_undo_and_redo() {
        let orig = b"abc".to_vec();
        let mut t = PieceTable::new(3);
        t.insert(0, b"x");
        t.mark_saved();
        assert!(!t.is_modified());
        t.insert(0, b"y");
        assert!(t.is_modified());
        t.undo();
        assert!(!t.is_modified());
        t.undo();
        assert!(t.is_modified(), "back before the saved state");
        t.redo();
        assert!(!t.is_modified());
        // A new edit after undoing past the saved state makes it unreachable.
        t.undo();
        t.insert(0, b"z");
        t.undo();
        assert!(t.is_modified());
        assert_eq!(text(&t, &orig), b"abc");
    }

    #[test]
    fn modified_flags_mark_added_and_moved_bytes() {
        let mut t = PieceTable::new(8);
        t.overwrite(2, b"zz");
        assert_eq!(t.modified_flags(0, 8), [false, false, true, true, false, false, false, false]);
        t.insert(6, b"!");
        // the inserted byte and everything after it moved
        assert_eq!(t.modified_flags(5, 4), [false, true, true, true]);
    }

    #[test]
    fn overwrite_only_detection() {
        let mut t = PieceTable::new(10);
        assert!(t.is_overwrite_only());
        t.overwrite(3, b"abc");
        assert!(t.is_overwrite_only());
        assert_eq!(t.added_runs().len(), 1);
        t.insert(0, b"x");
        assert!(!t.is_overwrite_only());
        t.delete(0, 1);
        // same length again and every original byte back in place, but through different pieces
        assert!(t.is_overwrite_only());
    }

    #[test]
    fn replace_many_is_one_undo_step() {
        let orig = b"a-b-c-d".to_vec();
        let mut t = PieceTable::new(7);
        t.replace_many(&[(1, 1, b"--".to_vec()), (3, 1, vec![]), (5, 1, b"+".to_vec())]);
        assert_eq!(text(&t, &orig), b"a--bc+d");
        assert!(t.undo().is_some());
        assert_eq!(text(&t, &orig), orig);
        assert!(t.undo().is_none());
    }

    #[test]
    fn out_of_range_edits_are_clamped() {
        let orig = b"abc".to_vec();
        let mut t = PieceTable::new(3);
        t.delete(2, 99);
        assert_eq!(text(&t, &orig), b"ab");
        t.insert(99, b"Z");
        assert_eq!(text(&t, &orig), b"abZ");
        t.splice(0, 0, b"");
        assert_eq!(t.len(), 3);
    }
}
