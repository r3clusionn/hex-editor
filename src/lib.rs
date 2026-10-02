//! `hx`: a terminal hex editor for files of any size.
//!
//! * [`piece`]: the piece table that makes insert, delete and undo cheap at any file size.
//! * [`doc`]: the memory-mapped file plus its piece table, and saving.
//! * [`search`]: finding and replacing byte patterns across the edited content.
//! * [`app`]: the editor state machine and the screen it draws, free of any terminal.
//! * [`term`]: the crossterm loop around [`app`].

pub mod app;
pub mod doc;
pub mod piece;
pub mod search;
pub mod term;
