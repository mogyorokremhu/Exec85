//! Notepad: a small text editor. Opens from the Explorer (Enter on a file) or the Apps menu.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::path::PathBuf;

const MAX_FILE: u64 = 4 << 20;
const TAB: usize = 4;

#[derive(Clone, PartialEq, Eq, Debug)]
enum Mode { Edit, SaveAs(String), ConfirmClose, Find(String) }

pub struct Notepad {
    lines: Vec<Vec<char>>,
    row: usize,
    col: usize,
    top: usize,
    left: usize,
    path: Option<PathBuf>,
    base: PathBuf,
    dirty: bool,
    readonly: bool,
    final_newline: bool,
    crlf: bool,
    msg: String,
    mode: Mode,
    close_after_save: bool,
    view: (usize, usize), // text area size in cells
}

/// The screen column of character `idx` in `line` (tabs jump to the next multiple of 4).
fn disp_col(line: &[char], idx: usize) -> usize {
    let mut c = 0;
    for ch in line.iter().take(idx) { c = if *ch == '\t' { (c / TAB + 1) * TAB } else { c + 1 }; }
    c
}

impl Notepad {
    pub fn new(env: &Env) -> Notepad {
        Notepad { lines: vec![Vec::new()], row: 0, col: 0, top: 0, left: 0, path: None, base: env.cwd.clone(), dirty: false, readonly: false,
            final_newline: true, crlf: false, msg: String::new(), mode: Mode::Edit, close_after_save: false, view: (60, 20) }
    }

    pub fn open(path: PathBuf, env: &Env) -> Notepad {
        let mut n = Notepad::new(env);
        n.path = Some(path.clone());
        match std::fs::metadata(&path) {
            Ok(m) if m.len() > MAX_FILE => { n.readonly = true; n.msg = format!("{} is too large to edit here ({} MB limit).", path.display(), MAX_FILE >> 20); }
            Ok(_) => match std::fs::read(&path) {
                Ok(bytes) => {
                    let text = match String::from_utf8(bytes) {
                        Ok(t) => t,
                        Err(e) => { n.readonly = true; n.msg = "Not a text file: opened read-only.".into(); String::from_utf8_lossy(e.as_bytes()).into_owned() }
                    };
                    n.crlf = text.contains("\r\n");
                    let text = if n.crlf { text.replace("\r\n", "\n") } else { text };
                    n.final_newline = text.is_empty() || text.ends_with('\n');
                    let body = text.strip_suffix('\n').unwrap_or(&text);
                    n.lines = body.split('\n').map(|l| l.chars().collect()).collect();
                    if n.lines.is_empty() { n.lines.push(Vec::new()); }
                }
                Err(e) => { n.readonly = true; n.msg = format!("Cannot read: {e}"); }
            },
            Err(_) => n.msg = "New file.".into(), // the file does not exist yet: saving creates it
        }
        n
    }

    fn name(&self) -> String { self.path.as_ref().and_then(|p| p.file_name()).map_or("untitled".into(), |n| n.to_string_lossy().into_owned()) }

    fn text(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = self.lines.iter().map(|l| l.iter().collect::<String>()).collect::<Vec<_>>().join(nl);
        if self.final_newline && !(self.lines.len() == 1 && self.lines[0].is_empty()) { s.push_str(nl); }
        s
    }

    fn save_to(&mut self, path: PathBuf) -> bool {
        match std::fs::write(&path, self.text()) {
            Ok(()) => { self.path = Some(path); self.dirty = false; self.msg = "Saved.".into(); true }
            Err(e) => { self.msg = format!("Cannot save: {e}"); false }
        }
    }

    fn save(&mut self) {
        if self.readonly { self.msg = "This file is read-only here.".into(); return; }
        match self.path.clone() {
            Some(p) => { self.save_to(p); }
            None => self.mode = Mode::SaveAs(String::new()),
        }
    }

    // ---- editing --------------------------------------------------------------------------------

    fn edit(&mut self) -> bool { if self.readonly { self.msg = "Read-only: this is not a text file.".into(); false } else { self.dirty = true; true } }

    fn insert(&mut self, c: char) { if self.edit() { self.lines[self.row].insert(self.col, c); self.col += 1; } }

    fn newline(&mut self) {
        if !self.edit() { return; }
        let indent: Vec<char> = self.lines[self.row].iter().take_while(|c| **c == ' ' || **c == '\t').copied().collect();
        let tail = self.lines[self.row].split_off(self.col);
        let mut next = indent.clone();
        next.extend(tail);
        self.row += 1;
        self.lines.insert(self.row, next);
        self.col = indent.len();
    }

    fn backspace(&mut self) {
        if !self.edit() { return; }
        if self.col > 0 { self.col -= 1; self.lines[self.row].remove(self.col); }
        else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(cur);
        }
    }

    fn delete(&mut self) {
        if !self.edit() { return; }
        if self.col < self.lines[self.row].len() { self.lines[self.row].remove(self.col); }
        else if self.row + 1 < self.lines.len() { let next = self.lines.remove(self.row + 1); self.lines[self.row].extend(next); }
    }

    fn clamp_col(&mut self) { self.col = self.col.min(self.lines[self.row].len()); }

    fn find(&mut self, needle: &str) {
        if needle.is_empty() { return; }
        let needle = needle.to_lowercase();
        let n = self.lines.len();
        for i in 0..=n {
            let r = (self.row + i) % n;
            let line: String = self.lines[r].iter().collect::<String>().to_lowercase();
            let from = if i == 0 { self.lines[r].iter().take(self.col + 1).map(|c| c.len_utf8()).sum::<usize>().min(line.len()) } else { 0 };
            if let Some(pos) = line.get(from..).and_then(|s| s.find(&needle)) {
                let byte = from + pos;
                self.row = r;
                self.col = line[..byte].chars().count();
                self.msg = String::new();
                return;
            }
        }
        self.msg = format!("\"{needle}\" not found.");
    }

    fn ensure_visible(&mut self) {
        let rows = self.view.1.max(1);
        if self.row < self.top { self.top = self.row; }
        if self.row >= self.top + rows { self.top = self.row + 1 - rows; }
        let dc = disp_col(&self.lines[self.row], self.col);
        let cols = self.view.0.max(1);
        if dc < self.left { self.left = dc; }
        if dc >= self.left + cols { self.left = dc + 1 - cols; }
    }

    fn prompt_key(&mut self, k: Key) {
        let (Mode::SaveAs(text) | Mode::Find(text)) = &mut self.mode else { return };
        match k.code {
            code::ESC => { self.mode = Mode::Edit; self.close_after_save = false; }
            code::BACKSPACE => { text.pop(); }
            code::ENTER => {
                let text = text.clone();
                match &self.mode {
                    Mode::SaveAs(_) => {
                        if text.trim().is_empty() { return; }
                        let p = if text.starts_with('/') { PathBuf::from(&text) } else { self.base.join(&text) };
                        self.mode = Mode::Edit;
                        if self.save_to(p) && self.close_after_save { self.close_after_save = false; self.msg = "Saved. Press F4 to close.".into(); }
                    }
                    _ => { self.mode = Mode::Edit; self.find(&text); }
                }
            }
            _ => if let Some(c) = k.ch() { if text.chars().count() < 200 { text.push(c); } },
        }
    }
}

impl Application for Notepad {
    fn kind(&self) -> Kind { Kind::Notepad }
    fn title(&self) -> String { format!("Notepad - {}{}", self.name(), if self.dirty { " *" } else { "" }) }
    fn resize(&mut self, cols: usize, rows: usize) { self.view = (cols.saturating_sub(6).max(1), rows.saturating_sub(2).max(1)); self.ensure_visible(); }

    fn try_close(&mut self) -> bool {
        if self.dirty { self.mode = Mode::ConfirmClose; self.msg = "Save changes before closing?".into(); false } else { true }
    }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match self.mode.clone() {
            Mode::SaveAs(_) | Mode::Find(_) => { self.prompt_key(k); return Out::None; }
            Mode::ConfirmClose => {
                match k.ch().map(|c| c.to_ascii_lowercase()) {
                    Some('y') => { if self.path.is_some() { let p = self.path.clone().unwrap(); if self.save_to(p) { return Out::Close; } self.mode = Mode::Edit; } else { self.mode = Mode::SaveAs(String::new()); self.close_after_save = true; } }
                    Some('n') => return Out::Close,
                    _ => if k.code == code::ESC { self.mode = Mode::Edit; self.msg.clear(); },
                }
                return Out::None;
            }
            Mode::Edit => {}
        }
        self.msg.clear();
        if k.ctrl {
            match k.code {
                code::HOME => { self.row = 0; self.col = 0; }
                code::END => { self.row = self.lines.len() - 1; self.col = self.lines[self.row].len(); }
                _ => match k.base_char() {
                    Some('s') => self.save(),
                    Some('f') => self.mode = Mode::Find(String::new()),
                    _ => {}
                },
            }
            self.ensure_visible();
            return Out::None;
        }
        match k.code {
            code::UP => { if self.row > 0 { self.row -= 1; self.clamp_col(); } }
            code::DOWN => { if self.row + 1 < self.lines.len() { self.row += 1; self.clamp_col(); } }
            code::LEFT => { if self.col > 0 { self.col -= 1; } else if self.row > 0 { self.row -= 1; self.col = self.lines[self.row].len(); } }
            code::RIGHT => { if self.col < self.lines[self.row].len() { self.col += 1; } else if self.row + 1 < self.lines.len() { self.row += 1; self.col = 0; } }
            code::HOME => self.col = 0,
            code::END => self.col = self.lines[self.row].len(),
            code::PGUP => { self.row = self.row.saturating_sub(self.view.1); self.clamp_col(); }
            code::PGDN => { self.row = (self.row + self.view.1).min(self.lines.len() - 1); self.clamp_col(); }
            code::ENTER => self.newline(),
            code::BACKSPACE => self.backspace(),
            code::DELETE => self.delete(),
            code::TAB => self.insert('\t'),
            _ => if let Some(c) = k.ch() { if !c.is_control() { self.insert(c); } },
        }
        self.ensure_visible();
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 3 || a.w < 12 { return; }
        let gutter = 5;
        let text_w = a.w - gutter;
        let rows = a.h - 1;
        for i in 0..rows {
            let r = self.top + i;
            let y = a.y + i;
            p.clip(a.x, y, a.w, "", t.text, t.panel);
            let Some(line) = self.lines.get(r) else { continue };
            p.text(a.x, y, &format!("{:>4}", r + 1), t.dim, None);
            let mut c = 0;
            for ch in line {
                let w = if *ch == '\t' { (c / TAB + 1) * TAB - c } else { 1 };
                if c + w > self.left && c >= self.left && c - self.left < text_w && *ch != '\t' { p.text(a.x + gutter + c - self.left, y, &ch.to_string(), t.text, None); }
                c += w;
            }
        }
        // cursor
        if focused && self.mode == Mode::Edit && self.row >= self.top && self.row < self.top + rows {
            let dc = disp_col(&self.lines[self.row], self.col);
            if dc >= self.left && dc - self.left < text_w {
                let ch = self.lines[self.row].get(self.col).copied().filter(|c| *c != '\t').unwrap_or(' ');
                p.text(a.x + gutter + dc - self.left, a.y + self.row - self.top, &ch.to_string(), t.accent_text, Some(t.accent));
            }
        }
        let y = a.y + a.h - 1;
        match &self.mode {
            Mode::SaveAs(text) => { p.text(a.x, y, "Save as:", t.accent, None); p.field(a.x + 9, y, a.w.saturating_sub(9), text, true, false); }
            Mode::Find(text) => { p.text(a.x, y, "Find:", t.accent, None); p.field(a.x + 6, y, a.w.saturating_sub(6), text, true, false); }
            Mode::ConfirmClose => p.clip(a.x, y, a.w, "Save changes? [Y] Yes   [N] No   [Esc] Cancel", t.accent, t.bar),
            Mode::Edit => {
                p.clip(a.x, y, a.w, &format!("Ln {}, Col {}   {}{}", self.row + 1, self.col + 1, if self.dirty { "modified  " } else { "" }, self.msg), t.dim, t.bar);
            }
        }
    }

    fn state(&self) -> String {
        let first: String = self.lines[0].iter().take(40).collect();
        format!("name=notepad file={} row={} col={} lines={} dirty={} readonly={} mode={} text={}",
            self.name().replace(' ', "_"), self.row + 1, self.col + 1, self.lines.len(), self.dirty, self.readonly,
            match self.mode { Mode::Edit => "edit", Mode::SaveAs(_) => "saveas", Mode::ConfirmClose => "confirmclose", Mode::Find(_) => "find" },
            if first.is_empty() { "-".to_string() } else { first.replace(' ', "_") })
    }
    fn hint(&self) -> &'static str { "Ctrl+S Save   Ctrl+F Find   Ctrl+Home/End   F4 Close   F6 Next window" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::fs;

    fn env() -> Env { Env::for_tests() }
    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("exec85-notepad-{tag}-{}", std::process::id())); let _ = fs::remove_dir_all(&d); fs::create_dir_all(&d).unwrap(); d }
    fn press(n: &mut Notepad, keys: &str) { for k in keys.split(',').filter(|s| !s.is_empty()) { n.handle(parse_key(k).unwrap_or_else(|| panic!("bad key {k}")), &env()); } }
    fn type_text(n: &mut Notepad, s: &str) {
        for c in s.chars() {
            if c == '\n' { press(n, "enter"); }
            else if let Some(k) = parse_key(&c.to_string()) { n.handle(k, &env()); }
            else { n.insert(c); } // characters a US keyboard has no key for (tab, accents)
        }
    }
    fn lines(n: &Notepad) -> Vec<String> { n.lines.iter().map(|l| l.iter().collect()).collect() }

    #[test]
    fn typing_and_editing() {
        let mut n = Notepad::new(&env());
        type_text(&mut n, "hello\nworld");
        assert_eq!(lines(&n), vec!["hello", "world"]);
        assert!(n.dirty);
        press(&mut n, "left,left,left,left,left,backspace");   // join the lines
        assert_eq!(lines(&n), vec!["helloworld"]);
        press(&mut n, "delete");
        assert_eq!(lines(&n), vec!["helloorld"]);
        press(&mut n, "home,end,backspace");
        assert_eq!(lines(&n), vec!["helloorl"]);
        press(&mut n, "ctrl-home");
        assert_eq!((n.row, n.col), (0, 0));
        type_text(&mut n, "ä€ ");                                // non-ASCII characters count as one
        assert_eq!(lines(&n)[0], "ä€ helloorl");
        assert_eq!(n.col, 3);
    }

    #[test]
    fn enter_keeps_the_indentation_and_cursor_moves_between_lines() {
        let mut n = Notepad::new(&env());
        type_text(&mut n, "    indented\nshort");           // the second line inherits the 4-space indent
        assert_eq!(lines(&n), vec!["    indented", "    short"]);
        press(&mut n, "up,end,enter");
        assert_eq!(lines(&n), vec!["    indented", "    ", "    short"]);
        assert_eq!((n.row, n.col), (1, 4));
        press(&mut n, "down,end,right");
        assert_eq!((n.row, n.col), (2, 9), "right at the very end of the text stays put");
        press(&mut n, "up");
        assert_eq!((n.row, n.col), (1, 4), "going up onto a shorter line clamps the column");
        press(&mut n, "up,up,up");
        assert_eq!(n.row, 0, "cannot go above the first line");
        press(&mut n, "home,left");
        assert_eq!((n.row, n.col), (0, 0));
        press(&mut n, "down,left");
        assert_eq!((n.row, n.col), (0, 12), "Left at the start of a line wraps to the end of the previous line");
    }

    #[test]
    fn save_and_open_round_trip() {
        let d = tmp("save");
        let mut n = Notepad::new(&Env { cwd: d.clone(), ..env() });
        type_text(&mut n, "line one\nline two");
        press(&mut n, "ctrl-s");
        assert_eq!(n.state().contains("mode=saveas"), true);
        type_text(&mut n, "notes.txt");
        press(&mut n, "enter");
        assert!(!n.dirty);
        assert_eq!(fs::read_to_string(d.join("notes.txt")).unwrap(), "line one\nline two\n");
        let m = Notepad::open(d.join("notes.txt"), &env());
        assert_eq!(lines(&m), vec!["line one", "line two"]);
        assert!(!m.dirty && m.title() == "Notepad - notes.txt");
        // saving again goes straight to the same file
        let mut m = m;
        type_text(&mut m, "!");
        assert!(m.dirty && m.title().ends_with("*"));
        press(&mut m, "ctrl-s");
        assert_eq!(fs::read_to_string(d.join("notes.txt")).unwrap(), "!line one\nline two\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn windows_line_endings_and_missing_final_newline_survive() {
        let d = tmp("crlf");
        fs::write(d.join("w.txt"), "a\r\nb\r\n").unwrap();
        let mut n = Notepad::open(d.join("w.txt"), &env());
        assert_eq!(lines(&n), vec!["a", "b"]);
        type_text(&mut n, "x");
        press(&mut n, "ctrl-s");
        assert_eq!(fs::read(d.join("w.txt")).unwrap(), b"xa\r\nb\r\n");
        fs::write(d.join("nonl.txt"), "no newline").unwrap();
        let mut n = Notepad::open(d.join("nonl.txt"), &env());
        press(&mut n, "ctrl-end");
        type_text(&mut n, "!");
        press(&mut n, "ctrl-s");
        assert_eq!(fs::read(d.join("nonl.txt")).unwrap(), b"no newline!");
        fs::write(d.join("empty.txt"), "").unwrap();
        assert_eq!(lines(&Notepad::open(d.join("empty.txt"), &env())), vec![""]);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn binary_files_open_read_only_and_cannot_be_damaged() {
        let d = tmp("bin");
        fs::write(d.join("b.bin"), [0x66u8, 0xff, 0xfe, 0x00, 0x67]).unwrap();
        let mut n = Notepad::open(d.join("b.bin"), &env());
        assert!(n.readonly);
        type_text(&mut n, "zzz");
        press(&mut n, "ctrl-s,backspace,delete,enter");
        assert!(!n.dirty);
        assert_eq!(fs::read(d.join("b.bin")).unwrap(), [0x66u8, 0xff, 0xfe, 0x00, 0x67], "the file is untouched");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn closing_with_unsaved_changes_asks_first() {
        let d = tmp("close");
        let mut n = Notepad::open(d.join("c.txt"), &env());
        assert!(n.try_close(), "nothing changed, closes at once");
        type_text(&mut n, "draft");
        assert!(!n.try_close(), "unsaved: stays open and asks");
        assert!(n.state().contains("mode=confirmclose"));
        press(&mut n, "esc");
        assert!(n.state().contains("mode=edit") && n.dirty);
        n.try_close();
        assert_eq!(n.handle(parse_key("n").unwrap(), &env()), Out::Close, "N discards");
        assert!(!d.join("c.txt").exists());
        let mut n = Notepad::open(d.join("c.txt"), &env());
        type_text(&mut n, "keep me");
        n.try_close();
        assert_eq!(n.handle(parse_key("y").unwrap(), &env()), Out::Close, "Y saves and closes");
        assert_eq!(fs::read_to_string(d.join("c.txt")).unwrap(), "keep me\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn find_jumps_to_the_next_match_and_wraps() {
        let mut n = Notepad::new(&env());
        type_text(&mut n, "alpha beta\ngamma\nBeta again");
        press(&mut n, "ctrl-home,ctrl-f");
        type_text(&mut n, "BETA");
        press(&mut n, "enter");
        assert_eq!((n.row, n.col), (0, 6), "case-insensitive");
        press(&mut n, "ctrl-f");
        type_text(&mut n, "beta");
        press(&mut n, "enter");
        assert_eq!((n.row, n.col), (2, 0), "next match");
        press(&mut n, "ctrl-f");
        type_text(&mut n, "beta");
        press(&mut n, "enter");
        assert_eq!((n.row, n.col), (0, 6), "wraps around");
        press(&mut n, "ctrl-f");
        type_text(&mut n, "zebra");
        press(&mut n, "enter");
        assert!(n.msg.contains("not found"));
    }

    #[test]
    fn tabs_count_as_four_columns() {
        let line: Vec<char> = "\tab\tc".chars().collect();
        assert_eq!(disp_col(&line, 0), 0);
        assert_eq!(disp_col(&line, 1), 4);
        assert_eq!(disp_col(&line, 3), 6);
        assert_eq!(disp_col(&line, 4), 8);
        assert_eq!(disp_col(&line, 5), 9);
    }

    #[test]
    fn scrolling_keeps_the_cursor_visible() {
        let mut n = Notepad::new(&env());
        n.resize(40, 12);
        for i in 0..60 { type_text(&mut n, &format!("line {i}\n")); }
        assert!(n.top > 40 && n.row >= n.top && n.row < n.top + n.view.1, "top={} row={}", n.top, n.row);
        press(&mut n, "ctrl-home");
        assert_eq!(n.top, 0);
        press(&mut n, "pgdn");
        assert_eq!(n.row, n.view.1);
    }

    #[test]
    fn draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as ThemeMode, Theme};
        let mut n = Notepad::new(&env());
        type_text(&mut n, "a line with\ttabs and ünïcode\nsecond");
        for (cols, rows) in [(10usize, 3usize), (30, 8), (80, 30)] {
            let m = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut canvas, m, t: Theme::new(ThemeMode::Dark) };
            n.draw(&mut p, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
    }
}
