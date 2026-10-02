//! Finding and replacing byte patterns in the edited content. The content is read in chunks, so
//! the cost is the cost of reading the file once however large it is.

use memchr::memmem;

use crate::doc::Document;

const CHUNK: usize = 4 << 20;

/// Turns what the user typed into bytes. `x:de ad be ef` (spaces optional, `0x` allowed) is a hex
/// pattern; anything else is searched for as its UTF-8 text, spaces included.
pub fn parse_pattern(s: &str) -> Result<Vec<u8>, String> {
    let bytes = if let Some(hex) = s.strip_prefix("x:").or_else(|| s.strip_prefix("X:")) { parse_hex(hex)? } else { s.as_bytes().to_vec() };
    if bytes.is_empty() {
        Err("empty pattern".to_string())
    } else {
        Ok(bytes)
    }
}

/// Hex digits with optional whitespace and `0x` prefixes between bytes.
pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    for token in s.split_whitespace() {
        let t = token.strip_prefix("0x").or_else(|| token.strip_prefix("0X")).unwrap_or(token);
        digits.push_str(t);
    }
    if !digits.len().is_multiple_of(2) {
        return Err("hex needs an even number of digits".to_string());
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|_| format!("'{}' is not hex", &digits[i..i + 2])))
        .collect()
}

/// The first match that starts in `[start, end)`.
pub fn find_forward(doc: &Document, pat: &[u8], start: u64, end: u64) -> Option<u64> {
    let plen = pat.len();
    let end = end.min(doc.len());
    if plen == 0 || start >= end {
        return None;
    }
    let finder = memmem::Finder::new(pat);
    let mut buf = vec![0u8; CHUNK + plen - 1];
    let mut pos = start;
    while pos < end {
        let want = ((end - pos) as usize).min(CHUNK) + plen - 1;
        let n = doc.read(pos, &mut buf[..want]);
        if n < plen {
            break;
        }
        if let Some(i) = finder.find(&buf[..n]) {
            return Some(pos + i as u64);
        }
        pos += CHUNK as u64;
    }
    None
}

/// The last match that starts in `[start, end)`.
pub fn find_backward(doc: &Document, pat: &[u8], start: u64, end: u64) -> Option<u64> {
    let plen = pat.len() as u64;
    if plen == 0 || doc.len() < plen {
        return None;
    }
    let mut hi = end.min(doc.len() - plen + 1);
    if start >= hi {
        return None;
    }
    let finder = memmem::FinderRev::new(pat);
    let mut buf = vec![0u8; CHUNK + pat.len() - 1];
    while hi > start {
        let lo = hi.saturating_sub(CHUNK as u64).max(start);
        let want = (hi - lo) as usize + pat.len() - 1;
        let n = doc.read(lo, &mut buf[..want]);
        if let Some(i) = finder.rfind(&buf[..n]) {
            return Some(lo + i as u64);
        }
        hi = lo;
    }
    None
}

/// The next match at or after `from`. With `wrap`, continues from the start of the file; the flag
/// says whether it had to.
pub fn next(doc: &Document, pat: &[u8], from: u64, wrap: bool) -> Option<(u64, bool)> {
    if let Some(m) = find_forward(doc, pat, from, doc.len()) {
        return Some((m, false));
    }
    if wrap {
        return find_forward(doc, pat, 0, from).map(|m| (m, true));
    }
    None
}

/// The previous match that starts before `before`.
pub fn prev(doc: &Document, pat: &[u8], before: u64, wrap: bool) -> Option<(u64, bool)> {
    if let Some(m) = find_backward(doc, pat, 0, before) {
        return Some((m, false));
    }
    if wrap {
        return find_backward(doc, pat, before, doc.len()).map(|m| (m, true));
    }
    None
}

/// Every non-overlapping match, left to right.
pub fn find_all(doc: &Document, pat: &[u8]) -> Vec<u64> {
    let plen = pat.len();
    let len = doc.len();
    let mut out = Vec::new();
    if plen == 0 || len < plen as u64 {
        return out;
    }
    let finder = memmem::Finder::new(pat);
    let mut buf = vec![0u8; CHUNK + plen - 1];
    let mut pos = 0u64;
    let mut next_allowed = 0u64;
    while pos < len {
        let n = doc.read(pos, &mut buf);
        if n < plen {
            break;
        }
        let mut i = next_allowed.saturating_sub(pos) as usize;
        while i + plen <= n {
            let Some(j) = finder.find(&buf[i..n]) else { break };
            let at = i + j;
            // Matches that start in the overlap belong to the next chunk.
            if at >= CHUNK && pos + (CHUNK as u64) < len {
                break;
            }
            out.push(pos + at as u64);
            next_allowed = pos + (at + plen) as u64;
            i = at + plen;
        }
        pos += CHUNK as u64;
    }
    out
}

/// Replaces every match of `find` by `replace` as one undoable change. Returns how many.
pub fn replace_all(doc: &mut Document, find: &[u8], replace: &[u8]) -> usize {
    let hits = find_all(doc, find);
    if hits.is_empty() {
        return 0;
    }
    let ops: Vec<(u64, u64, Vec<u8>)> = hits.iter().map(|&h| (h, find.len() as u64, replace.to_vec())).collect();
    doc.replace_many(&ops);
    hits.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(s: &[u8]) -> Document {
        Document::from_bytes(s.to_vec())
    }

    #[test]
    fn pattern_syntax() {
        assert_eq!(parse_pattern("abc").unwrap(), b"abc");
        assert_eq!(parse_pattern("a b").unwrap(), b"a b");
        assert_eq!(parse_pattern("x:de ad BE ef").unwrap(), [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parse_pattern("x:0xDEAD 0xbeef").unwrap(), [0xde, 0xad, 0xbe, 0xef]);
        assert_eq!(parse_pattern("x:deadbeef").unwrap(), [0xde, 0xad, 0xbe, 0xef]);
        assert!(parse_pattern("x:abc").is_err());
        assert!(parse_pattern("x:zz").is_err());
        assert!(parse_pattern("").is_err());
        assert!(parse_pattern("x:").is_err());
    }

    #[test]
    fn forward_backward_and_wrap() {
        let d = doc(b"abcabcabc");
        assert_eq!(next(&d, b"abc", 0, false), Some((0, false)));
        assert_eq!(next(&d, b"abc", 1, false), Some((3, false)));
        assert_eq!(next(&d, b"abc", 7, false), None);
        assert_eq!(next(&d, b"abc", 7, true), Some((0, true)));
        assert_eq!(prev(&d, b"abc", 9, false), Some((6, false)));
        assert_eq!(prev(&d, b"abc", 6, false), Some((3, false)));
        assert_eq!(prev(&d, b"abc", 0, false), None);
        assert_eq!(prev(&d, b"abc", 0, true), Some((6, true)));
        assert_eq!(next(&d, b"zzz", 0, true), None);
        assert_eq!(prev(&d, b"zzz", 5, true), None);
    }

    #[test]
    fn a_match_at_the_very_end_is_found() {
        let d = doc(b"xxabc");
        assert_eq!(find_forward(&d, b"abc", 0, 5), Some(2));
        assert_eq!(find_backward(&d, b"abc", 0, 5), Some(2));
        assert_eq!(find_backward(&d, b"abc", 0, 2), None);
        assert_eq!(find_forward(&d, b"abcd", 0, 5), None);
    }

    #[test]
    fn find_all_does_not_overlap() {
        let d = doc(b"aaaaa");
        assert_eq!(find_all(&d, b"aa"), [0, 2]);
        assert_eq!(find_all(&d, b"b"), Vec::<u64>::new());
    }

    #[test]
    fn replace_all_changes_lengths_and_undoes_in_one_step() {
        let mut d = doc(b"one two one three one");
        assert_eq!(replace_all(&mut d, b"one", b"1"), 3);
        assert_eq!(d.read_vec(0, 100), b"1 two 1 three 1");
        assert_eq!(replace_all(&mut d, b"1", b"seven"), 3);
        assert_eq!(d.read_vec(0, 100), b"seven two seven three seven");
        assert_eq!(replace_all(&mut d, b"seven", b""), 3);
        assert_eq!(d.read_vec(0, 100), b" two  three ");
        d.undo();
        assert_eq!(d.read_vec(0, 100), b"seven two seven three seven");
    }

    #[test]
    fn searches_see_edits_not_the_original() {
        let mut d = doc(b"hello world");
        d.overwrite(0, b"J");
        d.insert(5, b"XYZ");
        assert_eq!(next(&d, b"XYZ", 0, false), Some((5, false)));
        assert_eq!(next(&d, b"hello", 0, false), None);
        assert_eq!(next(&d, b"Jello", 0, false), Some((0, false)));
        // A match that spans original and added bytes.
        assert_eq!(next(&d, b"elloXYZ w", 0, false), Some((1, false)));
    }
}
