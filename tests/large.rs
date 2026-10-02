//! Editing a file much larger than memory. Ignored by default because it creates a file of
//! HX_LARGE_GIB GiB (default 4):
//!
//!     HX_LARGE_GIB=16 HX_LARGE_DIR=D:\scratch cargo test --release --test large -- --ignored --nocapture
//!
//! The file is created with `set_len`, so its contents are zeros except for three markers.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::time::Instant;

use hexed::doc::{Document, SaveKind};
use hexed::search;

/// Counts heap bytes, so the test can say how much memory the editor itself allocated. Pages of
/// the memory-mapped file are not heap and do not show up here.
mod heap {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    pub struct Counting;
    static CURRENT: AtomicUsize = AtomicUsize::new(0);
    static PEAK: AtomicUsize = AtomicUsize::new(0);

    // SAFETY: forwards every call to the system allocator and only adds bookkeeping.
    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, l: Layout) -> *mut u8 {
            let p = System.alloc(l);
            if !p.is_null() {
                let now = CURRENT.fetch_add(l.size(), Relaxed) + l.size();
                PEAK.fetch_max(now, Relaxed);
            }
            p
        }
        unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
            CURRENT.fetch_sub(l.size(), Relaxed);
            System.dealloc(p, l)
        }
        unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
            let q = System.realloc(p, l, new);
            if !q.is_null() {
                if new >= l.size() {
                    let now = CURRENT.fetch_add(new - l.size(), Relaxed) + (new - l.size());
                    PEAK.fetch_max(now, Relaxed);
                } else {
                    CURRENT.fetch_sub(l.size() - new, Relaxed);
                }
            }
            q
        }
    }

    pub fn reset_peak() {
        PEAK.store(CURRENT.load(Relaxed), Relaxed);
    }

    /// Peak heap bytes since `reset_peak`, in MiB (rounded up).
    pub fn peak_mib() -> usize {
        PEAK.load(Relaxed).div_ceil(1 << 20)
    }
}

#[global_allocator]
static ALLOC: heap::Counting = heap::Counting;

#[test]
#[ignore]
fn edit_a_huge_file_without_loading_it() {
    let gib: u64 = std::env::var("HX_LARGE_GIB").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let dir = match std::env::var("HX_LARGE_DIR") {
        Ok(d) => tempfile::tempdir_in(d).unwrap(),
        Err(_) => tempfile::tempdir().unwrap(),
    };
    let path = dir.path().join("huge.bin");
    let size = gib << 30;
    let needle = b"NEEDLE-AT-THE-END-OF-THE-FILE";
    let t = Instant::now();
    {
        let mut f = File::create(&path).unwrap();
        f.set_len(size).unwrap();
        f.seek(SeekFrom::Start(size - 4096)).unwrap();
        f.write_all(needle).unwrap();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(b"HEADER").unwrap();
    }
    println!("created a {gib} GiB file in {:.2} s", t.elapsed().as_secs_f64());
    heap::reset_peak();

    let t = Instant::now();
    let mut doc = Document::open(&path).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    assert_eq!(doc.len(), size);
    println!("open:            {open_ms:>9.2} ms");
    assert!(open_ms < 1000.0, "opening must not read the file");

    assert_eq!(doc.read_vec(0, 6), b"HEADER");

    // Search the whole file for a string near its end.
    let t = Instant::now();
    let found = search::next(&doc, needle, 0, false).map(|(o, _)| o);
    let secs = t.elapsed().as_secs_f64();
    assert_eq!(found, Some(size - 4096));
    println!("search {gib} GiB:  {:>9.2} s  ({:.2} GB/s)", secs, size as f64 / secs / 1e9);

    // Edits in the middle cost nothing, whatever the size.
    let mid = size / 2;
    let t = Instant::now();
    doc.insert(mid, b"<<inserted>>");
    doc.delete(mid + 100, 50);
    doc.overwrite(0, b"hdr");
    let edit_us = t.elapsed().as_secs_f64() * 1e6;
    println!("3 edits:         {edit_us:>9.1} us");
    assert_eq!(doc.len(), size + 12 - 50);
    assert_eq!(doc.read_vec(mid, 12), b"<<inserted>>");
    assert_eq!(doc.read_vec(0, 6), b"hdrDER");
    // Everything after the insertion moved: the needle is now 12 - 50 bytes earlier.
    let t = Instant::now();
    let moved = search::next(&doc, needle, mid, false).map(|(o, _)| o);
    println!("search after edits: {:.2} s", t.elapsed().as_secs_f64());
    assert_eq!(moved, Some(size - 4096 + 12 - 50));
    let t = Instant::now();
    while doc.undo().is_some() {}
    println!("undo all:        {:>9.1} us", t.elapsed().as_secs_f64() * 1e6);
    assert_eq!(doc.len(), size);
    assert_eq!(doc.read_vec(0, 6), b"HEADER");

    // An overwrite-only edit is patched into the file in place: only the changed bytes are written.
    doc.overwrite(size / 4, b"PATCHED-IN-PLACE");
    doc.overwrite(size - 4096, b"needle");
    let t = Instant::now();
    let kind = doc.save().unwrap();
    println!("save in place:   {:>9.2} ms  ({kind:?})", t.elapsed().as_secs_f64() * 1e3);
    assert_eq!(kind, SaveKind::InPlace { runs: 2, bytes: 22 });
    drop(doc);
    let mut f = File::open(&path).unwrap();
    let mut buf = [0u8; 16];
    f.seek(SeekFrom::Start(size / 4)).unwrap();
    f.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"PATCHED-IN-PLACE");
    assert_eq!(fs::metadata(&path).unwrap().len(), size);

    let peak = heap::peak_mib();
    println!("peak heap allocated by the editor: {peak} MiB for a {gib} GiB file");
    assert!(peak < 64, "heap use should be a few chunk buffers, not the file");
}
