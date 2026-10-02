//! A file being edited: the original (memory-mapped, never loaded) plus a piece table of changes.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use memmap2::Mmap;

use crate::piece::{Piece, PieceTable, Src};

enum Original {
    Map(Mmap),
    Mem(Vec<u8>),
}

impl Original {
    fn bytes(&self) -> &[u8] {
        match self {
            Original::Map(m) => m,
            Original::Mem(v) => v,
        }
    }
}

/// How a save was carried out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveKind {
    Unchanged,
    /// Only same-length edits: the changed byte runs were written into the existing file.
    InPlace { runs: usize, bytes: u64 },
    /// The length changed: the whole file was written to a temporary file that replaced the original.
    Rewrite { bytes: u64 },
}

pub struct Document {
    path: Option<PathBuf>,
    /// The file `orig` was mapped from.
    orig_path: Option<PathBuf>,
    orig: Original,
    table: PieceTable,
}

fn map_file(path: &Path) -> io::Result<Original> {
    let file = File::open(path)?;
    if file.metadata()?.len() == 0 {
        return Ok(Original::Mem(Vec::new()));
    }
    // SAFETY: the map is read-only. If another process truncates the file while it is open the
    // pages past the new end fault when read; the editor has no defence against that.
    Ok(Original::Map(unsafe { Mmap::map(&file)? }))
}

impl Document {
    /// Opens `path`. A path that does not exist gives an empty document that `save` will create.
    pub fn open(path: &Path) -> io::Result<Document> {
        let orig = match map_file(path) {
            Ok(o) => o,
            Err(e) if e.kind() == io::ErrorKind::NotFound => Original::Mem(Vec::new()),
            Err(e) => return Err(e),
        };
        let table = PieceTable::new(orig.bytes().len() as u64);
        let orig_path = matches!(orig, Original::Map(_)).then(|| path.to_path_buf());
        Ok(Document { path: Some(path.to_path_buf()), orig_path, orig, table })
    }

    /// A document that lives only in memory (for tests and piped input).
    pub fn from_bytes(bytes: Vec<u8>) -> Document {
        let table = PieceTable::new(bytes.len() as u64);
        Document { path: None, orig_path: None, orig: Original::Mem(bytes), table }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn len(&self) -> u64 {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn is_modified(&self) -> bool {
        self.table.is_modified()
    }

    pub fn piece_count(&self) -> usize {
        self.table.piece_count()
    }

    pub fn read(&self, offset: u64, buf: &mut [u8]) -> usize {
        self.table.read(self.orig.bytes(), offset, buf)
    }

    pub fn read_vec(&self, offset: u64, len: usize) -> Vec<u8> {
        let len = len.min(self.len().saturating_sub(offset) as usize);
        let mut v = vec![0; len];
        let n = self.read(offset, &mut v);
        v.truncate(n);
        v
    }

    pub fn byte_at(&self, offset: u64) -> Option<u8> {
        let mut b = [0u8];
        (self.read(offset, &mut b) == 1).then_some(b[0])
    }

    pub fn modified_flags(&self, offset: u64, len: usize) -> Vec<bool> {
        self.table.modified_flags(offset, len)
    }

    pub fn insert(&mut self, offset: u64, bytes: &[u8]) {
        self.table.insert(offset, bytes);
    }

    pub fn delete(&mut self, offset: u64, len: u64) {
        self.table.delete(offset, len);
    }

    pub fn overwrite(&mut self, offset: u64, bytes: &[u8]) {
        self.table.overwrite(offset, bytes);
    }

    pub fn replace_many(&mut self, ops: &[(u64, u64, Vec<u8>)]) {
        self.table.replace_many(ops);
    }

    pub fn undo(&mut self) -> Option<u64> {
        self.table.undo()
    }

    pub fn redo(&mut self) -> Option<u64> {
        self.table.redo()
    }

    /// Writes the content to `w`, piece by piece, without loading the file.
    pub fn write_to(&self, w: &mut impl Write) -> io::Result<u64> {
        let mut total = 0;
        for p in self.table.pieces() {
            let bytes = match p.src {
                Src::Orig => &self.orig.bytes()[p.start as usize..(p.start + p.len) as usize],
                Src::Add => self.table.add_bytes(*p),
            };
            w.write_all(bytes)?;
            total += p.len;
        }
        Ok(total)
    }

    /// Saves to the file the document was opened from.
    pub fn save(&mut self) -> io::Result<SaveKind> {
        let path = self.path.clone().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "no file name (use save as)"))?;
        if !self.table.is_modified() {
            return Ok(SaveKind::Unchanged);
        }
        let existing = path.exists();
        if existing && self.table.is_overwrite_only() && matches!(self.orig, Original::Map(_)) {
            let runs: Vec<(u64, Piece)> = self.table.added_runs();
            let mut f = OpenOptions::new().write(true).open(&path)?;
            let mut bytes = 0;
            for (offset, piece) in &runs {
                f.seek(SeekFrom::Start(*offset))?;
                f.write_all(self.table.add_bytes(*piece))?;
                bytes += piece.len;
            }
            f.sync_data()?;
            drop(f);
            self.reopen(&path)?;
            return Ok(SaveKind::InPlace { runs: runs.len(), bytes });
        }
        let bytes = self.rewrite(&path)?;
        Ok(SaveKind::Rewrite { bytes })
    }

    /// Saves to a new path, which becomes the document's file.
    pub fn save_as(&mut self, path: &Path) -> io::Result<SaveKind> {
        self.path = Some(path.to_path_buf());
        let bytes = self.rewrite(path)?;
        Ok(SaveKind::Rewrite { bytes })
    }

    /// Writes everything to a temporary file next to `path`, then moves it over `path`.
    fn rewrite(&mut self, path: &Path) -> io::Result<u64> {
        let tmp = tmp_name(path);
        let written = (|| {
            let mut w = BufWriter::with_capacity(1 << 20, File::create(&tmp)?);
            let n = self.write_to(&mut w)?;
            w.flush()?;
            w.into_inner().map_err(|e| e.into_error())?.sync_data()?;
            if let Ok(meta) = fs::metadata(path) {
                let _ = fs::set_permissions(&tmp, meta.permissions());
            }
            Ok::<u64, io::Error>(n)
        })();
        let n = match written {
            Ok(n) => n,
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                return Err(e);
            }
        };
        // A mapped file cannot be replaced on Windows, so release the map first. The table keeps
        // pointing into the original, which is untouched until the rename succeeds, so if the
        // rename fails the map is restored and nothing is lost.
        let target_is_mapped = matches!(self.orig, Original::Map(_)) && self.orig_path.as_deref() == Some(path);
        if target_is_mapped {
            self.orig = Original::Mem(Vec::new());
        }
        match fs::rename(&tmp, path) {
            Ok(()) => {
                self.reopen(path)?;
                Ok(n)
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                if target_is_mapped {
                    match map_file(path) {
                        Ok(o) => self.orig = o,
                        Err(_) => {
                            // The file cannot be read back, so the table has nothing to point at.
                            self.table = PieceTable::new(0);
                            return Err(io::Error::new(e.kind(), format!("{e}; the file could not be reopened, unsaved edits were lost")));
                        }
                    }
                }
                Err(e)
            }
        }
    }

    fn reopen(&mut self, path: &Path) -> io::Result<()> {
        self.orig = Original::Mem(Vec::new());
        self.orig = map_file(path)?;
        self.orig_path = matches!(self.orig, Original::Map(_)).then(|| path.to_path_buf());
        self.table = PieceTable::new(self.orig.bytes().len() as u64);
        Ok(())
    }
}

fn tmp_name(path: &Path) -> PathBuf {
    let mut name = path.file_name().map(|n| n.to_os_string()).unwrap_or_default();
    name.push(".hx-tmp");
    path.with_file_name(name)
}
