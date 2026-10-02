//! The editor: a state machine fed with keys and a function that draws the screen. Neither touches
//! a terminal, so both are tested directly and `run_script` can replay a key sequence.

use std::fmt::Write as _;
use std::path::Path;

use crate::doc::{Document, SaveKind};
use crate::search;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Esc,
    Backspace,
    Delete,
    Insert,
    Tab,
    F1,
}

/// Parses a key name as used by `--keys`: a single character, or `esc`, `enter`, `tab`, `space`,
/// `comma`, `up`, `down`, `left`, `right`, `pgup`, `pgdn`, `home`, `end`, `backspace`, `delete`,
/// `insert`, `f1`, or `ctrl-x`.
pub fn parse_key(s: &str) -> Option<Key> {
    let t = s.trim();
    let mut chars = t.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return Some(Key::Char(c));
    }
    let lower = t.to_ascii_lowercase();
    if let Some(c) = lower.strip_prefix("ctrl-") {
        let mut cs = c.chars();
        if let (Some(c), None) = (cs.next(), cs.next()) {
            return Some(Key::Ctrl(c));
        }
    }
    Some(match lower.as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "home" => Key::Home,
        "end" => Key::End,
        "enter" | "return" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "backspace" | "bs" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        "insert" | "ins" => Key::Insert,
        "tab" => Key::Tab,
        "f1" => Key::F1,
        "space" => Key::Char(' '),
        "comma" => Key::Char(','),
        _ => return None,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Plain,
    Dim,
    Offset,
    Modified,
    Cursor,
    Mirror,
    Bar,
    Error,
    Prompt,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    pub text: String,
    pub style: Style,
}

#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub lines: Vec<Vec<Span>>,
}

impl Screen {
    pub fn plain(&self) -> String {
        self.lines.iter().map(|l| l.iter().map(|s| s.text.as_str()).collect::<String>()).collect::<Vec<_>>().join("\n")
    }

    /// The screen with ANSI colours, for replaying a script into a terminal or an image.
    pub fn ansi(&self) -> String {
        let mut out = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                out.push('\n');
            }
            for s in line {
                let code = match s.style {
                    Style::Plain => "",
                    Style::Dim => "90",
                    Style::Offset => "36",
                    Style::Modified => "33",
                    Style::Cursor => "7",
                    Style::Mirror => "100",
                    Style::Bar => "7;1",
                    Style::Error => "31;1",
                    Style::Prompt => "1",
                };
                if code.is_empty() {
                    out.push_str(&s.text);
                } else {
                    let _ = write!(out, "\x1b[{code}m{}\x1b[0m", s.text);
                }
            }
        }
        out
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PromptKind {
    Goto,
    Find(bool),
    Replace,
    SaveAs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Normal,
    Edit,
    Prompt(PromptKind),
    Help,
}

pub struct App {
    pub doc: Document,
    pub cursor: u64,
    pub quit: bool,
    readonly: bool,
    /// Requested bytes per row; 0 picks the widest that fits.
    cols_setting: usize,
    cols: usize,
    top_row: u64,
    view_rows: usize,
    mode: Mode,
    insert: bool,
    ascii_entry: bool,
    nibble: Option<u8>,
    prompt: String,
    last_find: Option<Vec<u8>>,
    last_forward: bool,
    little_endian: bool,
    message: String,
    message_is_error: bool,
}

const HELP: &[&str] = &[
    " hx key reference",
    "",
    " move       h j k l or arrows   PgUp PgDn   Home End   G last byte",
    " goto       g then 0x1f40, 8000, +16, -0x10 or $ (last byte)",
    " find       / forward, ? backward, n next, N previous",
    "            text as typed, or x:de ad be ef for bytes",
    " replace    R then  find => replace   (all matches, one undo step)",
    " edit       e overwrite, i insert, A append at the end; hex digits or, after Tab, text",
    "            Tab switches hex and text entry, Esc leaves, Backspace and Delete remove",
    " delete     x or Delete removes the byte under the cursor",
    " undo       u undo, Ctrl-r redo",
    " save       w or Ctrl-s, W save as",
    " other      E little or big endian values, q quit, Q quit and discard",
    "",
    " press any key to close",
];

impl App {
    pub fn new(doc: Document) -> App {
        App {
            doc,
            cursor: 0,
            quit: false,
            readonly: false,
            cols_setting: 0,
            cols: 16,
            top_row: 0,
            view_rows: 20,
            mode: Mode::Normal,
            insert: false,
            ascii_entry: false,
            nibble: None,
            prompt: String::new(),
            last_find: None,
            last_forward: true,
            little_endian: true,
            message: String::new(),
            message_is_error: false,
        }
    }

    pub fn readonly(mut self, yes: bool) -> App {
        self.readonly = yes;
        self
    }

    /// Fixes the number of bytes per row (default: as many as fit, up to 16).
    pub fn columns(mut self, n: usize) -> App {
        self.cols_setting = n;
        if n > 0 {
            self.cols = n;
        }
        self
    }

    pub fn set_cursor(&mut self, offset: u64) {
        self.cursor = offset.min(self.max_cursor());
    }

    pub fn is_editing(&self) -> bool {
        self.mode == Mode::Edit
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    fn max_cursor(&self) -> u64 {
        let len = self.doc.len();
        if self.mode == Mode::Edit {
            len
        } else {
            len.saturating_sub(1)
        }
    }

    fn say(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.message_is_error = false;
    }

    fn fail(&mut self, text: impl Into<String>) {
        self.message = text.into();
        self.message_is_error = true;
    }

    fn move_by(&mut self, delta: i64) {
        let target = if delta < 0 { self.cursor.saturating_sub(delta.unsigned_abs()) } else { self.cursor.saturating_add(delta as u64) };
        self.cursor = target.min(self.max_cursor());
        self.nibble = None;
    }

    fn can_edit(&mut self) -> bool {
        if self.readonly {
            self.fail("read-only (started with --readonly)");
            false
        } else {
            true
        }
    }

    pub fn handle_key(&mut self, key: Key) {
        // A message lasts until the next key.
        let had_message = !self.message.is_empty();
        if had_message {
            self.message.clear();
        }
        match self.mode {
            Mode::Help => self.mode = Mode::Normal,
            Mode::Prompt(kind) => self.prompt_key(kind, key),
            Mode::Edit => self.edit_key(key),
            Mode::Normal => self.normal_key(key),
        }
        self.cursor = self.cursor.min(self.max_cursor());
    }

    fn common_motion(&mut self, key: Key) -> bool {
        let page = (self.view_rows.max(1) * self.cols) as i64;
        match key {
            Key::Left => self.move_by(-1),
            Key::Right => self.move_by(1),
            Key::Up => self.move_by(-(self.cols as i64)),
            Key::Down => self.move_by(self.cols as i64),
            Key::PageUp => self.move_by(-page),
            Key::PageDown => self.move_by(page),
            Key::Home => {
                self.cursor -= self.cursor % self.cols as u64;
                self.nibble = None;
            }
            Key::End => {
                self.cursor = (self.cursor - self.cursor % self.cols as u64 + self.cols as u64 - 1).min(self.max_cursor());
                self.nibble = None;
            }
            _ => return false,
        }
        true
    }

    fn normal_key(&mut self, key: Key) {
        if self.common_motion(key) {
            return;
        }
        match key {
            Key::Char('h') => self.move_by(-1),
            Key::Char('l') => self.move_by(1),
            Key::Char('k') => self.move_by(-(self.cols as i64)),
            Key::Char('j') => self.move_by(self.cols as i64),
            Key::Char('G') => self.cursor = self.max_cursor(),
            Key::Char('g') => self.open_prompt(PromptKind::Goto),
            Key::Char('/') => self.open_prompt(PromptKind::Find(true)),
            Key::Char('?') => self.open_prompt(PromptKind::Find(false)),
            Key::Char('n') => self.repeat_find(self.last_forward),
            Key::Char('N') => self.repeat_find(!self.last_forward),
            Key::Char('R') => {
                if self.can_edit() {
                    self.open_prompt(PromptKind::Replace)
                }
            }
            Key::Char('i') | Key::Insert => self.start_edit(true),
            Key::Char('A') => {
                self.start_edit(true);
                if self.mode == Mode::Edit {
                    self.cursor = self.doc.len();
                }
            }
            Key::Char('e') | Key::Enter => self.start_edit(false),
            Key::Char('x') | Key::Delete => self.delete_at_cursor(),
            Key::Char('u') | Key::Ctrl('z') => self.undo(),
            Key::Ctrl('r') | Key::Ctrl('y') => self.redo(),
            Key::Char('w') | Key::Ctrl('s') => self.save(),
            Key::Char('W') => self.open_prompt(PromptKind::SaveAs),
            Key::Char('E') => {
                self.little_endian = !self.little_endian;
                self.say(if self.little_endian { "little endian" } else { "big endian" });
            }
            Key::F1 | Key::Char('H') => self.mode = Mode::Help,
            Key::Char('q') | Key::Ctrl('c') => {
                if self.doc.is_modified() {
                    self.fail("unsaved changes: w saves, Q quits and discards them");
                } else {
                    self.quit = true;
                }
            }
            Key::Char('Q') => self.quit = true,
            _ => {}
        }
    }

    fn start_edit(&mut self, insert: bool) {
        if !self.can_edit() {
            return;
        }
        self.mode = Mode::Edit;
        self.insert = insert;
        self.nibble = None;
        self.say(if insert { "insert: type hex digits, Tab for text, Esc to stop" } else { "overwrite: type hex digits, Tab for text, Esc to stop" });
    }

    fn edit_key(&mut self, key: Key) {
        if self.common_motion(key) {
            return;
        }
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                self.nibble = None;
                self.ascii_entry = false;
            }
            Key::Tab => {
                self.ascii_entry = !self.ascii_entry;
                self.nibble = None;
            }
            Key::Insert => self.insert = !self.insert,
            Key::Backspace => {
                if self.nibble.take().is_some() {
                    return;
                }
                if self.insert {
                    if self.cursor > 0 {
                        self.doc.delete(self.cursor - 1, 1);
                        self.cursor -= 1;
                    }
                } else {
                    self.move_by(-1);
                }
            }
            Key::Delete => self.delete_at_cursor(),
            Key::Ctrl('s') => self.save(),
            Key::Ctrl('z') => self.undo(),
            Key::Ctrl('y') => self.redo(),
            Key::Char(c) if self.ascii_entry => {
                let mut b = [0u8; 4];
                let bytes = c.encode_utf8(&mut b).as_bytes().to_vec();
                self.put(&bytes);
            }
            Key::Char(c) => match c.to_digit(16) {
                Some(d) => match self.nibble.take() {
                    None => self.nibble = Some(d as u8),
                    Some(hi) => self.put(&[hi << 4 | d as u8]),
                },
                None => self.fail(format!("'{c}' is not a hex digit (Tab switches to text entry)")),
            },
            _ => {}
        }
    }

    /// Inserts or overwrites bytes at the cursor and moves past them.
    fn put(&mut self, bytes: &[u8]) {
        if self.insert {
            self.doc.insert(self.cursor, bytes);
        } else {
            self.doc.overwrite(self.cursor, bytes);
        }
        self.cursor += bytes.len() as u64;
    }

    fn delete_at_cursor(&mut self) {
        if !self.can_edit() {
            return;
        }
        if self.cursor < self.doc.len() {
            self.doc.delete(self.cursor, 1);
        }
    }

    fn undo(&mut self) {
        if !self.can_edit() {
            return;
        }
        match self.doc.undo() {
            Some(at) => {
                self.cursor = at.min(self.max_cursor());
                self.say("undone");
            }
            None => self.fail("nothing to undo"),
        }
    }

    fn redo(&mut self) {
        if !self.can_edit() {
            return;
        }
        match self.doc.redo() {
            Some(at) => {
                self.cursor = at.min(self.max_cursor());
                self.say("redone");
            }
            None => self.fail("nothing to redo"),
        }
    }

    fn save(&mut self) {
        if !self.can_edit() {
            return;
        }
        match self.doc.save() {
            Ok(SaveKind::Unchanged) => self.say("no changes to save"),
            Ok(SaveKind::InPlace { runs, bytes }) => self.say(format!("saved in place: {bytes} bytes in {runs} run{}", if runs == 1 { "" } else { "s" })),
            Ok(SaveKind::Rewrite { bytes }) => self.say(format!("saved: rewrote {}", human(bytes))),
            Err(e) => self.fail(format!("save failed: {e}")),
        }
    }

    fn open_prompt(&mut self, kind: PromptKind) {
        self.mode = Mode::Prompt(kind);
        self.prompt.clear();
    }

    fn prompt_key(&mut self, kind: PromptKind, key: Key) {
        match key {
            Key::Esc | Key::Ctrl('c') => self.mode = Mode::Normal,
            Key::Backspace => {
                if self.prompt.pop().is_none() {
                    self.mode = Mode::Normal;
                }
            }
            Key::Ctrl('u') => self.prompt.clear(),
            Key::Char(c) => self.prompt.push(c),
            Key::Enter => {
                let text = std::mem::take(&mut self.prompt);
                self.mode = Mode::Normal;
                self.run_prompt(kind, &text);
            }
            _ => {}
        }
    }

    fn run_prompt(&mut self, kind: PromptKind, text: &str) {
        match kind {
            PromptKind::Goto => match parse_goto(text, self.cursor, self.doc.len()) {
                Ok(off) => {
                    self.cursor = off.min(self.max_cursor());
                    if off > self.max_cursor() {
                        self.say("past the end, moved to the last byte");
                    }
                }
                Err(e) => self.fail(e),
            },
            PromptKind::Find(forward) => match search::parse_pattern(text) {
                Ok(p) => {
                    self.last_find = Some(p);
                    self.last_forward = forward;
                    self.repeat_find(forward);
                }
                Err(e) => self.fail(e),
            },
            PromptKind::Replace => {
                let Some((find, with)) = text.split_once("=>") else {
                    return self.fail("type  find => replacement  (replacement may be empty)");
                };
                let find = match search::parse_pattern(find.trim_end_matches(' ')) {
                    Ok(f) => f,
                    Err(e) => return self.fail(e),
                };
                let with = with.strip_prefix(' ').unwrap_or(with);
                let with = if with.is_empty() {
                    Vec::new()
                } else {
                    match search::parse_pattern(with) {
                        Ok(w) => w,
                        Err(e) => return self.fail(e),
                    }
                };
                let n = search::replace_all(&mut self.doc, &find, &with);
                self.say(if n == 0 { "no matches".to_string() } else { format!("replaced {n} occurrence{} (u undoes them all)", if n == 1 { "" } else { "s" }) });
            }
            PromptKind::SaveAs => {
                if text.is_empty() {
                    return;
                }
                match self.doc.save_as(Path::new(text)) {
                    Ok(SaveKind::Rewrite { bytes }) => self.say(format!("saved {} to {text}", human(bytes))),
                    Ok(_) => self.say(format!("saved to {text}")),
                    Err(e) => self.fail(format!("save failed: {e}")),
                }
            }
        }
    }

    fn repeat_find(&mut self, forward: bool) {
        let Some(pat) = self.last_find.clone() else {
            return self.fail("no search yet: / searches forward, ? backward");
        };
        let found = if forward { search::next(&self.doc, &pat, self.cursor + 1, true) } else { search::prev(&self.doc, &pat, self.cursor, true) };
        match found {
            Some((at, wrapped)) => {
                self.cursor = at;
                self.say(format!("found at 0x{at:x}{}", if wrapped { " (wrapped around)" } else { "" }));
            }
            None => self.fail("not found"),
        }
    }

    // ---- drawing ----

    fn offset_digits(&self) -> usize {
        let mut n = 8;
        let mut v = self.doc.len() >> 32;
        while v > 0 {
            n += 1;
            v >>= 4;
        }
        n.max(8)
    }

    fn pick_columns(&self, width: usize) -> usize {
        if self.cols_setting > 0 {
            return self.cols_setting;
        }
        let ow = self.offset_digits();
        for c in [16usize, 8, 4] {
            // offset, two spaces, hex with a gap every 8, two spaces, |ascii|
            if ow + c * 4 + (c - 1) / 8 + 5 <= width {
                return c;
            }
        }
        4
    }

    fn ensure_visible(&mut self) {
        let row = self.cursor / self.cols as u64;
        let rows = self.view_rows.max(1) as u64;
        if row < self.top_row {
            self.top_row = row;
        } else if row >= self.top_row + rows {
            self.top_row = row + 1 - rows;
        }
    }

    /// Draws a screen of `width` by `height` characters.
    pub fn render(&mut self, width: usize, height: usize) -> Screen {
        let height = height.max(5);
        self.cols = self.pick_columns(width);
        self.view_rows = height - 3;
        self.ensure_visible();
        let mut lines: Vec<Vec<Span>> = Vec::with_capacity(height);

        if self.mode == Mode::Help {
            for i in 0..height {
                let t = HELP.get(i).copied().unwrap_or("");
                lines.push(vec![Span { text: pad(t, width), style: if i == 0 { Style::Bar } else { Style::Plain } }]);
            }
            return Screen { lines };
        }

        // header
        let name = self.doc.path().map_or("(memory)".to_string(), |p| p.file_name().map_or(p.display().to_string(), |n| n.to_string_lossy().into_owned()));
        let mode = match self.mode {
            Mode::Edit if self.insert => "INSERT",
            Mode::Edit => "OVERWRITE",
            _ if self.readonly => "READ-ONLY",
            _ => "NORMAL",
        };
        let entry = if self.mode == Mode::Edit { if self.ascii_entry { " text" } else { " hex" } } else { "" };
        let header = format!(" {name}  {}  {mode}{entry}{}", human(self.doc.len()), if self.doc.is_modified() { "  [+]" } else { "" });
        lines.push(vec![Span { text: pad(&header, width), style: Style::Bar }]);

        // rows
        let ow = self.offset_digits();
        let cols = self.cols;
        let len = self.doc.len();
        let first = self.top_row * cols as u64;
        let data = self.doc.read_vec(first, self.view_rows * cols);
        let flags = self.doc.modified_flags(first, data.len());
        let editing = self.mode == Mode::Edit;
        for r in 0..self.view_rows {
            let row_off = first + (r * cols) as u64;
            let mut spans: Vec<Span> = Vec::new();
            // Rows past the end are blank, except the one holding the append slot while editing, and
            // the first row of an empty file.
            let holds_append_slot = editing && self.cursor == len && row_off <= len && len < row_off + cols as u64;
            if !(row_off < len || holds_append_slot || (len == 0 && row_off == 0)) {
                lines.push(vec![Span { text: pad("", width), style: Style::Plain }]);
                continue;
            }
            spans.push(Span { text: format!("{row_off:0ow$x}  "), style: Style::Offset });
            let mut ascii: Vec<Span> = Vec::new();
            for c in 0..cols {
                let off = row_off + c as u64;
                if c > 0 && c % 8 == 0 {
                    spans.push(Span { text: " ".into(), style: Style::Plain });
                }
                let idx = (off - first) as usize;
                let is_cursor = off == self.cursor;
                let (hex_style, asc_style) = if is_cursor {
                    if self.ascii_entry && editing {
                        (Style::Mirror, Style::Cursor)
                    } else {
                        (Style::Cursor, Style::Mirror)
                    }
                } else {
                    (Style::Plain, Style::Plain)
                };
                if off < len && idx < data.len() {
                    let b = data[idx];
                    let base = if flags[idx] { Style::Modified } else if b == 0 { Style::Dim } else { Style::Plain };
                    let hex = match self.nibble {
                        Some(n) if is_cursor && !self.ascii_entry => format!("{n:x}_"),
                        _ => format!("{b:02x}"),
                    };
                    spans.push(Span { text: hex, style: if is_cursor { hex_style } else { base } });
                    let ch = if (0x20..0x7f).contains(&b) { b as char } else { '.' };
                    ascii.push(Span { text: ch.to_string(), style: if is_cursor { asc_style } else if flags[idx] { Style::Modified } else if b == 0x2e || !(0x20..0x7f).contains(&b) { Style::Dim } else { Style::Plain } });
                } else if off == len && is_cursor && editing {
                    // the slot just past the end, where typing appends
                    spans.push(Span { text: self.nibble.map_or("  ".to_string(), |n| format!("{n:x}_")), style: hex_style });
                    ascii.push(Span { text: " ".into(), style: asc_style });
                } else {
                    spans.push(Span { text: "  ".into(), style: Style::Plain });
                    ascii.push(Span { text: " ".into(), style: Style::Plain });
                }
                if c + 1 < cols {
                    spans.push(Span { text: " ".into(), style: Style::Plain });
                }
            }
            spans.push(Span { text: "  |".into(), style: Style::Plain });
            spans.extend(ascii);
            spans.push(Span { text: "|".into(), style: Style::Plain });
            lines.push(fit(merge(spans), width));
        }

        // value inspector
        lines.push(vec![Span { text: pad(&self.inspector(), width), style: Style::Dim }]);

        // status or prompt
        let last = if let Mode::Prompt(kind) = self.mode {
            let label = match kind {
                PromptKind::Goto => "goto (0x1f40, 8000, +16, -0x10, $)",
                PromptKind::Find(true) => "find (text, or x:de ad)",
                PromptKind::Find(false) => "find backwards (text, or x:de ad)",
                PromptKind::Replace => "replace (find => replacement)",
                PromptKind::SaveAs => "save as",
            };
            Span { text: pad(&format!(" {label}: {}_", self.prompt), width), style: Style::Prompt }
        } else if !self.message.is_empty() {
            Span { text: pad(&format!(" {}", self.message), width), style: if self.message_is_error { Style::Error } else { Style::Plain } }
        } else {
            let pct = if len == 0 { 100 } else { (self.cursor.min(len - 1) as u128 * 100 / len.max(1) as u128) as u64 };
            Span { text: pad(&format!(" 0x{:x} ({})  {pct}%  F1 or H for help", self.cursor, self.cursor), width), style: Style::Dim }
        };
        lines.push(vec![last]);
        Screen { lines }
    }

    fn inspector(&self) -> String {
        let bytes = self.doc.read_vec(self.cursor, 8);
        if bytes.is_empty() {
            return " (end of file)".to_string();
        }
        let le = self.little_endian;
        // For big endian the bytes are used in file order, most significant first.
        let word = |n: usize| -> Option<u64> {
            if bytes.len() < n {
                return None;
            }
            let mut v = 0u64;
            for i in 0..n {
                let b = if le { bytes[n - 1 - i] } else { bytes[i] };
                v = v << 8 | b as u64;
            }
            Some(v)
        };
        let b = bytes[0];
        let mut s = format!(" {} u8 {b} 0x{b:02x}", if le { "LE" } else { "BE" });
        if let Some(v) = word(2) {
            let _ = write!(s, " | u16 {v} i16 {}", v as u16 as i16);
        }
        if let Some(v) = word(4) {
            let _ = write!(s, " | u32 {v} i32 {} f32 {}", v as u32 as i32, f32::from_bits(v as u32));
        }
        if let Some(v) = word(8) {
            let _ = write!(s, " | u64 {v} f64 {}", f64::from_bits(v));
        }
        s
    }
}

/// `0x1f40` or `1f40h`-free syntax: hex with `0x`, decimal, `+n` and `-n` relative to the cursor,
/// and `$` for the last byte.
pub fn parse_goto(text: &str, cursor: u64, len: u64) -> Result<u64, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("type an offset".to_string());
    }
    if t == "$" || t.eq_ignore_ascii_case("end") {
        return Ok(len.saturating_sub(1));
    }
    let number = |s: &str| -> Result<u64, String> {
        let s = s.trim();
        if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            u64::from_str_radix(h, 16).map_err(|_| format!("'{s}' is not a hex number"))
        } else {
            s.parse::<u64>().map_err(|_| format!("'{s}' is not a number"))
        }
    };
    if let Some(r) = t.strip_prefix('+') {
        return Ok(cursor.saturating_add(number(r)?));
    }
    if let Some(r) = t.strip_prefix('-') {
        return Ok(cursor.saturating_sub(number(r)?));
    }
    number(t)
}

fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

fn pad(s: &str, width: usize) -> String {
    let mut t: String = s.chars().take(width).collect();
    let n = t.chars().count();
    t.extend(std::iter::repeat_n(' ', width - n));
    t
}

/// Joins neighbouring spans that share a style.
fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for s in spans {
        match out.last_mut() {
            Some(l) if l.style == s.style => l.text.push_str(&s.text),
            _ => out.push(s),
        }
    }
    out
}

/// Cuts or pads a line of spans to exactly `width` characters.
fn fit(spans: Vec<Span>, width: usize) -> Vec<Span> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        if used >= width {
            break;
        }
        let n = s.text.chars().count();
        if used + n <= width {
            used += n;
            out.push(s);
        } else {
            out.push(Span { text: s.text.chars().take(width - used).collect(), style: s.style });
            used = width;
        }
    }
    if used < width {
        out.push(Span { text: " ".repeat(width - used), style: Style::Plain });
    }
    out
}

/// Replays comma-separated key names (see [`parse_key`]; `text:abc` types the characters of
/// `abc`) and returns the screen that results.
pub fn run_script(app: &mut App, keys: &str, width: usize, height: usize) -> Result<Screen, String> {
    // Draw once so the viewport size is known before the first key, as in the real loop.
    app.render(width, height);
    for token in keys.split(',').filter(|t| !t.trim().is_empty()) {
        if let Some(text) = token.trim_start().strip_prefix("text:") {
            for c in text.chars() {
                app.handle_key(Key::Char(c));
            }
        } else {
            let key = parse_key(token).ok_or_else(|| format!("unknown key '{}'", token.trim()))?;
            app.handle_key(key);
        }
        app.render(width, height);
        if app.quit {
            break;
        }
    }
    Ok(app.render(width, height))
}
