//! Terminal ("Cmd"): a shell in a window. A pseudo-terminal connects the window to the shell, and a small
//! VT100-style emulator turns the program's output (text, cursor moves, colours, scrolling) into a screen.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

const SCROLLBACK: usize = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Color { Default, Idx(u8) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Cell { pub ch: char, pub fg: Color, pub bg: Color, pub bold: bool, pub rev: bool }

const BLANK: Cell = Cell { ch: ' ', fg: Color::Default, bg: Color::Default, bold: false, rev: false };

pub const ANSI: [u32; 16] = [
    0x1B1B1B, 0xCC4444, 0x44AA44, 0xCCAA33, 0x4477CC, 0xAA55AA, 0x44AAAA, 0xCCCCCC,
    0x666666, 0xFF6666, 0x66DD66, 0xFFDD55, 0x6699FF, 0xDD77DD, 0x66DDDD, 0xFFFFFF,
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum St { Ground, Esc, Csi, Osc, OscEsc, Charset }

/// The terminal screen and the parser that fills it.
pub struct Screen {
    pub cols: usize,
    pub rows: usize,
    cells: Vec<Cell>,
    pub scrollback: VecDeque<Vec<Cell>>,
    pub cur: (usize, usize), // (row, col)
    saved: (usize, usize),
    wrap_pending: bool,
    fg: Color,
    bg: Color,
    bold: bool,
    rev: bool,
    top: usize,
    bot: usize,
    st: St,
    params: Vec<u32>,
    param_started: bool,
    private: bool,
    utf: Vec<u8>,
    /// Bytes the terminal must send back to the program (answers to cursor-position queries).
    pub reply: Vec<u8>,
}

impl Screen {
    pub fn new(cols: usize, rows: usize) -> Screen {
        let (cols, rows) = (cols.max(1), rows.max(1));
        Screen { cols, rows, cells: vec![BLANK; cols * rows], scrollback: VecDeque::new(), cur: (0, 0), saved: (0, 0), wrap_pending: false,
            fg: Color::Default, bg: Color::Default, bold: false, rev: false, top: 0, bot: rows - 1, st: St::Ground, params: Vec::new(),
            param_started: false, private: false, utf: Vec::new(), reply: Vec::new() }
    }

    pub fn cell(&self, r: usize, c: usize) -> Cell { self.cells[r * self.cols + c] }
    fn set(&mut self, r: usize, c: usize, cell: Cell) { self.cells[r * self.cols + c] = cell; }
    pub fn row_text(&self, r: usize) -> String { (0..self.cols).map(|c| self.cell(r, c).ch).collect::<String>().trim_end().to_string() }

    fn blank_cell(&self) -> Cell { Cell { bg: self.bg, ..BLANK } }

    pub fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(1), rows.max(1));
        if (cols, rows) == (self.cols, self.rows) { return; }
        let mut cells = vec![BLANK; cols * rows];
        // keep the bottom of the screen if it shrinks (that is where the prompt is)
        let shift = self.cur.0.saturating_sub(rows - 1);
        for r in 0..rows { for c in 0..cols {
            let sr = r + shift;
            if sr < self.rows && c < self.cols { cells[r * cols + c] = self.cells[sr * self.cols + c]; }
        }}
        self.cells = cells;
        self.cols = cols; self.rows = rows;
        self.cur = (self.cur.0.saturating_sub(shift).min(rows - 1), self.cur.1.min(cols - 1));
        self.top = 0; self.bot = rows - 1;
        self.wrap_pending = false;
    }

    fn scroll_up(&mut self, n: usize) {
        let n = n.min(self.bot - self.top + 1);
        for _ in 0..n {
            if self.top == 0 {
                let line: Vec<Cell> = self.cells[..self.cols].to_vec();
                self.scrollback.push_back(line);
                if self.scrollback.len() > SCROLLBACK { self.scrollback.pop_front(); }
            }
            let (t, b, w) = (self.top, self.bot, self.cols);
            self.cells.copy_within((t + 1) * w..(b + 1) * w, t * w);
            let blank = self.blank_cell();
            for c in 0..w { self.cells[b * w + c] = blank; }
        }
    }

    fn scroll_down(&mut self, n: usize) {
        let n = n.min(self.bot - self.top + 1);
        for _ in 0..n {
            let (t, b, w) = (self.top, self.bot, self.cols);
            self.cells.copy_within(t * w..b * w, (t + 1) * w);
            let blank = self.blank_cell();
            for c in 0..w { self.cells[t * w + c] = blank; }
        }
    }

    fn linefeed(&mut self) { if self.cur.0 == self.bot { self.scroll_up(1); } else if self.cur.0 + 1 < self.rows { self.cur.0 += 1; } }

    fn print(&mut self, ch: char) {
        if self.wrap_pending { self.cur.1 = 0; self.linefeed(); self.wrap_pending = false; }
        let cell = Cell { ch, fg: self.fg, bg: self.bg, bold: self.bold, rev: self.rev };
        self.set(self.cur.0, self.cur.1, cell);
        if self.cur.1 + 1 >= self.cols { self.wrap_pending = true; } else { self.cur.1 += 1; }
    }

    fn erase(&mut self, r: usize, from: usize, to: usize) { let b = self.blank_cell(); for c in from..to.min(self.cols) { self.set(r, c, b); } }

    pub fn feed(&mut self, data: &[u8]) { for &b in data { self.byte(b); } }

    fn byte(&mut self, b: u8) {
        match self.st {
            St::Ground => match b {
                0x1b => self.st = St::Esc,
                0x08 => { self.wrap_pending = false; self.cur.1 = self.cur.1.saturating_sub(1); }
                0x09 => { self.wrap_pending = false; self.cur.1 = ((self.cur.1 / 8 + 1) * 8).min(self.cols - 1); }
                0x0a | 0x0b | 0x0c => { self.wrap_pending = false; self.linefeed(); }
                0x0d => { self.wrap_pending = false; self.cur.1 = 0; }
                0x00..=0x1f | 0x7f => {}
                _ => self.utf8(b),
            },
            St::Esc => {
                self.st = St::Ground;
                match b {
                    b'[' => { self.st = St::Csi; self.params.clear(); self.param_started = false; self.private = false; }
                    b']' => self.st = St::Osc,
                    b'(' | b')' | b'*' | b'+' => self.st = St::Charset,
                    b'c' => { let (c, r) = (self.cols, self.rows); *self = Screen { scrollback: std::mem::take(&mut self.scrollback), ..Screen::new(c, r) }; }
                    b'7' => self.saved = self.cur,
                    b'8' => { self.cur = self.saved; self.wrap_pending = false; }
                    b'D' => self.linefeed(),
                    b'E' => { self.cur.1 = 0; self.linefeed(); }
                    b'M' => { if self.cur.0 == self.top { self.scroll_down(1); } else { self.cur.0 = self.cur.0.saturating_sub(1); } }
                    _ => {}
                }
            }
            St::Charset => self.st = St::Ground,
            St::Osc => match b { 0x07 => self.st = St::Ground, 0x1b => self.st = St::OscEsc, _ => {} },
            St::OscEsc => self.st = St::Ground,
            St::Csi => match b {
                b'0'..=b'9' => { let d = (b - b'0') as u32; if !self.param_started { self.params.push(0); self.param_started = true; } let l = self.params.last_mut().unwrap(); *l = l.saturating_mul(10).saturating_add(d).min(99_999); }
                b';' | b':' => { if !self.param_started { self.params.push(0); } self.param_started = false; }
                b'?' | b'>' | b'<' | b'=' => self.private = true,
                0x20..=0x2f => {}
                0x40..=0x7e => { if !self.param_started && self.params.is_empty() { /* no parameters */ } self.st = St::Ground; self.csi(b); }
                0x1b => self.st = St::Esc,
                _ => self.st = St::Ground,
            },
        }
    }

    fn utf8(&mut self, b: u8) {
        if b < 0x80 { self.utf.clear(); self.print(b as char); return; }
        self.utf.push(b);
        let need = match self.utf[0] { 0xC0..=0xDF => 2, 0xE0..=0xEF => 3, 0xF0..=0xF7 => 4, _ => 1 };
        if self.utf.len() >= need {
            let s = String::from_utf8_lossy(&self.utf).into_owned();
            self.utf.clear();
            for ch in s.chars() { self.print(ch); }
        }
    }

    fn p(&self, i: usize, default: usize) -> usize { match self.params.get(i) { Some(&v) if v > 0 => v as usize, _ => default } }

    fn csi(&mut self, fin: u8) {
        let (rows, cols) = (self.rows, self.cols);
        self.wrap_pending = false;
        match fin {
            b'A' => self.cur.0 = self.cur.0.saturating_sub(self.p(0, 1)).max(if self.cur.0 >= self.top { self.top } else { 0 }),
            b'B' => self.cur.0 = (self.cur.0 + self.p(0, 1)).min(if self.cur.0 <= self.bot { self.bot } else { rows - 1 }),
            b'C' => self.cur.1 = (self.cur.1 + self.p(0, 1)).min(cols - 1),
            b'D' => self.cur.1 = self.cur.1.saturating_sub(self.p(0, 1)),
            b'E' => { self.cur.0 = (self.cur.0 + self.p(0, 1)).min(rows - 1); self.cur.1 = 0; }
            b'F' => { self.cur.0 = self.cur.0.saturating_sub(self.p(0, 1)); self.cur.1 = 0; }
            b'G' | b'`' => self.cur.1 = (self.p(0, 1) - 1).min(cols - 1),
            b'd' => self.cur.0 = (self.p(0, 1) - 1).min(rows - 1),
            b'H' | b'f' => { self.cur = ((self.p(0, 1) - 1).min(rows - 1), (self.p(1, 1) - 1).min(cols - 1)); }
            b'J' => {
                let (r, c) = self.cur;
                match self.params.first().copied().unwrap_or(0) {
                    0 => { self.erase(r, c, cols); for rr in r + 1..rows { self.erase(rr, 0, cols); } }
                    1 => { for rr in 0..r { self.erase(rr, 0, cols); } self.erase(r, 0, c + 1); }
                    _ => for rr in 0..rows { self.erase(rr, 0, cols); },
                }
            }
            b'K' => {
                let (r, c) = self.cur;
                match self.params.first().copied().unwrap_or(0) { 0 => self.erase(r, c, cols), 1 => self.erase(r, 0, c + 1), _ => self.erase(r, 0, cols) }
            }
            b'L' => { if self.cur.0 >= self.top && self.cur.0 <= self.bot { let (t, n) = (self.top, self.p(0, 1)); self.top = self.cur.0; self.scroll_down(n); self.top = t; } }
            b'M' => { if self.cur.0 >= self.top && self.cur.0 <= self.bot { let (t, n) = (self.top, self.p(0, 1)); self.top = self.cur.0; self.scroll_up_noscrollback(n); self.top = t; } }
            b'P' => { let (r, c, n) = (self.cur.0, self.cur.1, self.p(0, 1).min(cols - self.cur.1)); for cc in c..cols { let v = if cc + n < cols { self.cell(r, cc + n) } else { self.blank_cell() }; self.set(r, cc, v); } }
            b'@' => { let (r, c, n) = (self.cur.0, self.cur.1, self.p(0, 1).min(cols - self.cur.1)); for cc in (c..cols).rev() { let v = if cc >= c + n { self.cell(r, cc - n) } else { self.blank_cell() }; self.set(r, cc, v); } }
            b'X' => { let (r, c, n) = (self.cur.0, self.cur.1, self.p(0, 1)); self.erase(r, c, c + n); }
            b'S' => { let n = self.p(0, 1); self.scroll_up(n); }
            b'T' => { let n = self.p(0, 1); self.scroll_down(n); }
            b'r' => {
                let (t, b) = (self.p(0, 1) - 1, self.p(1, rows) - 1);
                if t < b && b < rows { self.top = t; self.bot = b; } else { self.top = 0; self.bot = rows - 1; }
                self.cur = (0, 0);
            }
            b's' => self.saved = self.cur,
            b'u' => self.cur = self.saved,
            b'm' => self.sgr(),
            b'n' => if self.params.first() == Some(&6) { self.reply.extend_from_slice(format!("\x1b[{};{}R", self.cur.0 + 1, self.cur.1 + 1).as_bytes()); },
            b'c' => if !self.private { self.reply.extend_from_slice(b"\x1b[?6c"); },
            _ => {} // h, l (modes), t and the rest are accepted and ignored
        }
    }

    /// Delete lines inside the scroll region without feeding the scrollback.
    fn scroll_up_noscrollback(&mut self, n: usize) {
        let keep = std::mem::take(&mut self.scrollback);
        let t = self.top;
        self.top = self.top.max(1); // never a region starting at row 0 => nothing is pushed to scrollback
        if t == 0 { self.top = 0; }
        let (cols, rows_bot) = (self.cols, self.bot);
        let n = n.min(rows_bot - t + 1);
        for _ in 0..n {
            self.cells.copy_within((t + 1) * cols..(rows_bot + 1) * cols, t * cols);
            let blank = self.blank_cell();
            for c in 0..cols { self.cells[rows_bot * cols + c] = blank; }
        }
        self.top = t;
        self.scrollback = keep;
    }

    fn sgr(&mut self) {
        if self.params.is_empty() { self.params.push(0); }
        let mut i = 0;
        while i < self.params.len() {
            let v = self.params[i];
            match v {
                0 => { self.fg = Color::Default; self.bg = Color::Default; self.bold = false; self.rev = false; }
                1 => self.bold = true,
                22 => self.bold = false,
                7 => self.rev = true,
                27 => self.rev = false,
                30..=37 => self.fg = Color::Idx((v - 30) as u8),
                39 => self.fg = Color::Default,
                40..=47 => self.bg = Color::Idx((v - 40) as u8),
                49 => self.bg = Color::Default,
                90..=97 => self.fg = Color::Idx((v - 90 + 8) as u8),
                100..=107 => self.bg = Color::Idx((v - 100 + 8) as u8),
                38 | 48 => {
                    // 38;5;n (256 colours) or 38;2;r;g;b (true colour): snap to the nearest of our 16
                    let color = match self.params.get(i + 1) {
                        Some(5) => { let n = self.params.get(i + 2).copied().unwrap_or(0); i += 2; Some(Color::Idx(idx256_to_16(n))) }
                        Some(2) => { let g = |k: usize| self.params.get(i + k).copied().unwrap_or(0) as i32; let c = nearest16(g(2), g(3), g(4)); i += 4; Some(Color::Idx(c)) }
                        _ => None,
                    };
                    if let Some(c) = color { if v == 38 { self.fg = c; } else { self.bg = c; } }
                }
                _ => {}
            }
            i += 1;
        }
    }

    /// The last up-to-three non-empty lines on the screen (for test output).
    pub fn last_lines(&self) -> Vec<String> {
        let v: Vec<String> = (0..self.rows).map(|r| self.row_text(r)).filter(|l| !l.is_empty()).collect();
        v[v.len().saturating_sub(3)..].to_vec()
    }
}

fn nearest16(r: i32, g: i32, b: i32) -> u8 {
    (0..16u8).min_by_key(|&i| { let c = ANSI[i as usize]; (((c >> 16) & 255) as i32 - r).pow(2) + (((c >> 8) & 255) as i32 - g).pow(2) + ((c & 255) as i32 - b).pow(2) }).unwrap_or(7)
}

fn idx256_to_16(n: u32) -> u8 {
    match n {
        0..=15 => n as u8,
        16..=231 => { let n = n - 16; let (r, g, b) = ((n / 36) % 6, (n / 6) % 6, n % 6); let f = |v: u32| if v == 0 { 0 } else { 55 + 40 * v as i32 }; nearest16(f(r), f(g), f(b)) }
        _ => { let v = 8 + 10 * (n as i32 - 232); nearest16(v, v, v) }
    }
}

// ---- keys -> bytes ---------------------------------------------------------------------------------------------

/// The bytes a terminal sends to the program for a key press.
pub fn key_bytes(k: &Key) -> Vec<u8> {
    let esc = |s: &str| s.as_bytes().to_vec();
    match k.code {
        code::ENTER => return vec![b'\r'],
        code::BACKSPACE => return vec![0x7f],
        code::TAB => return if k.shift { esc("\x1b[Z") } else { vec![b'\t'] },
        code::ESC => return vec![0x1b],
        code::UP => return esc("\x1b[A"), code::DOWN => return esc("\x1b[B"), code::RIGHT => return esc("\x1b[C"), code::LEFT => return esc("\x1b[D"),
        code::HOME => return esc("\x1b[H"), code::END => return esc("\x1b[F"),
        code::PGUP => return esc("\x1b[5~"), code::PGDN => return esc("\x1b[6~"), code::DELETE => return esc("\x1b[3~"),
        _ => {}
    }
    let Some(c) = k.base_char() else { return Vec::new() };
    if k.ctrl {
        return match c.to_ascii_lowercase() { 'a'..='z' => vec![c.to_ascii_lowercase() as u8 - b'a' + 1], ' ' | '2' => vec![0], '[' => vec![0x1b], '\\' => vec![0x1c], ']' => vec![0x1d], _ => Vec::new() };
    }
    let mut out = if k.alt { vec![0x1b] } else { Vec::new() };
    out.extend_from_slice(c.to_string().as_bytes());
    out
}

// ---- the app --------------------------------------------------------------------------------------------------

struct Shared { screen: Screen, exited: bool }

pub struct Terminal {
    shared: Arc<Mutex<Shared>>,
    master: std::fs::File,
    pid: i32,
    back: usize, // how many lines we are scrolled back
}

impl Terminal {
    /// Start `program` with `args` in a new terminal window.
    pub fn spawn(program: &str, args: &[&str], cwd: &std::path::Path, cols: usize, rows: usize, wake: Arc<dyn Fn() + Send + Sync>) -> std::io::Result<Terminal> {
        let (master, slave) = crate::sys::open_pty()?;
        crate::sys::set_winsize(master.as_raw_fd(), cols, rows);
        let mut cmd = Command::new(program);
        cmd.args(args).env("TERM", "linux").current_dir(cwd)
            .stdin(Stdio::from(slave.try_clone()?)).stdout(Stdio::from(slave.try_clone()?)).stderr(Stdio::from(slave));
        // SAFETY: only system calls run between fork and exec: a new session, then the slave becomes the controlling terminal.
        unsafe {
            cmd.pre_exec(|| {
                crate::sys::setsid();
                crate::sys::ioctl(0, crate::sys::TIOCSCTTY, 0 as std::os::raw::c_ulong);
                Ok(())
            });
        }
        let mut child: Child = cmd.spawn()?;
        drop(cmd); // our copies of the slave end must close, so that we notice when the shell exits
        let pid = child.id() as i32;
        let shared = Arc::new(Mutex::new(Shared { screen: Screen::new(cols, rows), exited: false }));
        let (sh, mut reader, mut writer) = (shared.clone(), master.try_clone()?, master.try_clone()?);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let mut s = sh.lock().unwrap();
                        s.screen.feed(&buf[..n]);
                        let reply = std::mem::take(&mut s.screen.reply);
                        drop(s);
                        if !reply.is_empty() { let _ = writer.write_all(&reply); }
                        wake();
                    }
                }
            }
            let _ = child.wait();
            sh.lock().unwrap().exited = true;
            wake();
        });
        Ok(Terminal { shared, master, pid, back: 0 })
    }

    pub fn open(env: &Env) -> Terminal {
        let shell = std::env::var("SHELL").ok().filter(|s| std::path::Path::new(s).exists()).unwrap_or_else(|| "/bin/sh".into());
        match Terminal::spawn(&shell, &[], &env.cwd, 80, 24, env.wake.clone()) {
            Ok(t) => t,
            Err(e) => {
                // Could not start a shell: show why instead of failing silently.
                let shared = Arc::new(Mutex::new(Shared { screen: Screen::new(80, 24), exited: true }));
                shared.lock().unwrap().screen.feed(format!("Cannot start {shell}: {e}").as_bytes());
                Terminal { shared, master: std::fs::File::open("/dev/null").unwrap(), pid: 0, back: 0 }
            }
        }
    }

    pub fn send(&mut self, bytes: &[u8]) { if !bytes.is_empty() && !self.exited() { let _ = self.master.write_all(bytes); } }
    pub fn exited(&self) -> bool { self.shared.lock().unwrap().exited }
    pub fn screen_text(&self) -> Vec<String> { let s = self.shared.lock().unwrap(); (0..s.screen.rows).map(|r| s.screen.row_text(r)).collect() }
}

impl Drop for Terminal {
    fn drop(&mut self) { if self.pid > 0 && !self.exited() { let _ = crate::sys::send_signal(self.pid, 1); } } // SIGHUP, like closing a terminal
}

impl Application for Terminal {
    fn kind(&self) -> Kind { Kind::Terminal }
    fn title(&self) -> String { if self.exited() { "Terminal (finished)".into() } else { "Terminal".into() } }

    fn resize(&mut self, cols: usize, rows: usize) {
        if cols == 0 || rows == 0 { return; }
        crate::sys::set_winsize(self.master.as_raw_fd(), cols, rows);
        self.shared.lock().unwrap().screen.resize(cols, rows);
    }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        if self.exited() {
            if k.code == code::ENTER { return Out::Close; }
            return Out::None;
        }
        // Shift+PageUp/PageDown scroll through what has already gone off the top
        if k.shift && (k.code == code::PGUP || k.code == code::PGDN) {
            let s = self.shared.lock().unwrap();
            let page = s.screen.rows.saturating_sub(1).max(1);
            let max = s.screen.scrollback.len();
            drop(s);
            self.back = if k.code == code::PGUP { (self.back + page).min(max) } else { self.back.saturating_sub(page) };
            return Out::None;
        }
        self.back = 0;
        self.send(&key_bytes(&k));
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let s = self.shared.lock().unwrap();
        let sc = &s.screen;
        let color = |c: Color, default: u32| match c { Color::Default => default, Color::Idx(i) => ANSI[i as usize & 15] };
        let back = self.back.min(sc.scrollback.len());
        for vr in 0..a.h.min(sc.rows) {
            // line index counted from the top of (scrollback + screen)
            let line = sc.scrollback.len() - back + vr;
            for vc in 0..a.w.min(sc.cols) {
                let cell = if line < sc.scrollback.len() { sc.scrollback[line].get(vc).copied().unwrap_or(BLANK) } else { let r = line - sc.scrollback.len(); if r < sc.rows { sc.cell(r, vc) } else { BLANK } };
                let (mut fg, mut bg) = (color(cell.fg, t.text), color(cell.bg, t.panel));
                if cell.bold && cell.fg != Color::Default { if let Color::Idx(i) = cell.fg { fg = ANSI[(i as usize % 8) + 8]; } }
                if cell.rev { std::mem::swap(&mut fg, &mut bg); }
                p.text(a.x + vc, a.y + vr, &cell.ch.to_string(), fg, Some(bg));
            }
        }
        if focused && back == 0 && sc.cur.0 < a.h && sc.cur.1 < a.w && !s.exited {
            let c = sc.cell(sc.cur.0, sc.cur.1);
            p.text(a.x + sc.cur.1, a.y + sc.cur.0, &c.ch.to_string(), t.accent_text, Some(t.accent));
        }
        if back > 0 { p.text(a.x + a.w.saturating_sub(18), a.y, &format!(" scrolled back {back} "), t.accent_text, Some(t.accent)); }
        if s.exited { p.clip(a.x, a.y + a.h - 1, a.w, "[the program has finished: press Enter to close]", t.accent_text, t.accent); }
    }

    fn state(&self) -> String {
        let s = self.shared.lock().unwrap();
        let l = s.screen.last_lines();
        let f = |i: usize| l.get(i).map_or("-".to_string(), |x| x.chars().take(70).collect::<String>().replace(' ', "_"));
        let n = l.len();
        format!("name=terminal pid={} cols={} rows={} cur={},{} exited={} back={} l1={} l2={} l3={}", self.pid, s.screen.cols, s.screen.rows, s.screen.cur.0, s.screen.cur.1, s.exited, self.back,
            if n >= 3 { f(n - 3) } else { "-".into() }, if n >= 2 { f(n - 2) } else { "-".into() }, if n >= 1 { f(n - 1) } else { "-".into() })
    }
    fn hint(&self) -> &'static str { "Shift+PgUp/PgDn scroll back   F4 Close window   F6 Next window   F10 Menu" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::time::{Duration, Instant};

    fn screen_with(cols: usize, rows: usize, input: &str) -> Screen { let mut s = Screen::new(cols, rows); s.feed(input.as_bytes()); s }

    #[test]
    fn plain_text_lines_and_wrapping() {
        let s = screen_with(10, 4, "hello\r\nworld");
        assert_eq!((s.row_text(0), s.row_text(1)), ("hello".into(), "world".into()));
        assert_eq!(s.cur, (1, 5));
        let s = screen_with(5, 3, "abcdefgh");
        assert_eq!((s.row_text(0), s.row_text(1)), ("abcde".into(), "fgh".into()), "long lines wrap");
        let s = screen_with(5, 3, "abcde");
        assert_eq!(s.cur, (0, 4), "the cursor waits at the edge until the next character");
        let s = screen_with(10, 2, "a\tb");
        assert_eq!(s.cell(0, 8).ch, 'b', "tab goes to the next multiple of 8");
        let s = screen_with(10, 2, "ab\x08c");
        assert_eq!(s.row_text(0), "ac", "backspace moves left without erasing");
    }

    #[test]
    fn scrolling_feeds_the_scrollback() {
        let mut s = Screen::new(6, 3);
        for i in 0..6 { s.feed(format!("l{i}\r\n").as_bytes()); }
        assert_eq!(s.row_text(0), "l4");
        assert_eq!(s.scrollback.len(), 4, "l0..l3 went off the top");
        let first: String = s.scrollback[0].iter().map(|c| c.ch).collect::<String>().trim_end().to_string();
        assert_eq!(first, "l0");
    }

    #[test]
    fn cursor_movement_and_erasing() {
        let mut s = screen_with(10, 5, "\x1b[3;4Hx");
        assert_eq!((s.cell(2, 3).ch, s.cur), ('x', (2, 4)));
        s.feed(b"\x1b[2A\x1b[3Cy");
        assert_eq!(s.cell(0, 7).ch, 'y');
        s.feed(b"\x1b[1;1Hzzzzzz\x1b[1;3H\x1b[K");
        assert_eq!(s.row_text(0), "zz", "K erases to the end of the line");
        s.feed(b"\x1b[2J");
        assert!((0..5).all(|r| s.row_text(r).is_empty()), "2J clears the screen");
        s.feed(b"\x1b[HABC\x1b[1K");
        assert_eq!(s.row_text(0), "", "1K erases from the start up to and including the cursor");
        let mut s = screen_with(10, 3, "aaaa\r\nbbbb\r\ncccc\x1b[2;3H\x1b[0J");
        assert_eq!((s.row_text(0), s.row_text(1), s.row_text(2)), ("aaaa".into(), "bb".into(), "".into()));
        s.feed(b"\x1b[1;1H\x1b[P");
        assert_eq!(s.row_text(0), "aaa", "P deletes a character");
        s.feed(b"\x1b[2@");
        assert_eq!(s.row_text(0), "  aaa", "@ inserts blanks");
    }

    #[test]
    fn colours_and_attributes() {
        let s = screen_with(10, 2, "\x1b[31mred\x1b[0m\x1b[1;32mG\x1b[44mB\x1b[7mR\x1b[0mn");
        assert_eq!(s.cell(0, 0).fg, Color::Idx(1));
        assert_eq!(s.cell(0, 3).fg, Color::Idx(2));
        assert!(s.cell(0, 3).bold);
        assert_eq!(s.cell(0, 4).bg, Color::Idx(4));
        assert!(s.cell(0, 5).rev);
        assert_eq!((s.cell(0, 6).fg, s.cell(0, 6).bg, s.cell(0, 6).rev), (Color::Default, Color::Default, false), "reset");
        let s = screen_with(10, 1, "\x1b[93ma\x1b[38;5;196mb\x1b[38;2;0;255;0mc\x1b[48;5;21md");
        assert_eq!(s.cell(0, 0).fg, Color::Idx(11), "bright yellow");
        assert_eq!(s.cell(0, 1).fg, Color::Idx(1), "256-colour red snaps to the nearest of 16");
        assert_eq!(s.cell(0, 2).fg, Color::Idx(2), "true-colour green");
        assert_eq!(s.cell(0, 3).bg, Color::Idx(4), "256-colour blue");
    }

    #[test]
    fn scroll_regions_and_reverse_index() {
        let mut s = screen_with(6, 5, "1\r\n2\r\n3\r\n4\r\n5");
        s.feed(b"\x1b[2;4r");                    // lines 2-4 scroll, 1 and 5 stay
        s.feed(b"\x1b[4;1H\n");
        assert_eq!((s.row_text(0), s.row_text(1), s.row_text(2), s.row_text(4)), ("1".into(), "3".into(), "4".into(), "5".into()));
        s.feed(b"\x1b[2;1H\x1bM");
        assert_eq!(s.row_text(1), "", "reverse index at the top of the region scrolls down");
        let mut t = screen_with(6, 4, "a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
        assert_eq!((t.row_text(1), t.row_text(2)), ("".into(), "b".into()), "L inserts a line");
        t.feed(b"\x1b[M");
        assert_eq!((t.row_text(1), t.row_text(2)), ("b".into(), "c".into()), "M deletes a line");
        assert_eq!(t.scrollback.len(), 0, "insert/delete line never pollutes the scrollback");
    }

    #[test]
    fn answers_cursor_position_and_device_queries() {
        let mut s = screen_with(10, 5, "\x1b[3;7H\x1b[6n");
        assert_eq!(s.reply, b"\x1b[3;7R");
        s.reply.clear();
        s.feed(b"\x1b[c");
        assert_eq!(s.reply, b"\x1b[?6c");
    }

    #[test]
    fn unknown_escape_sequences_are_swallowed_and_utf8_is_decoded() {
        let s = screen_with(20, 2, "\x1b]0;window title\x07ok\x1b[?25l\x1b(Bz\x1b[999;999Hq");
        assert_eq!(s.cell(0, 0).ch, 'o');
        assert_eq!(s.cell(0, 2).ch, 'z');
        assert_eq!((s.cell(1, 19).ch, s.cur), ('q', (1, 19)), "positions beyond the screen are clamped");
        let s = screen_with(10, 1, "é€x");
        assert_eq!(s.row_text(0), "é€x");
        let s = screen_with(10, 1, "a\x1b[");
        assert_eq!(s.row_text(0), "a");
    }

    #[test]
    fn resizing_keeps_the_bottom_of_the_screen() {
        let mut s = screen_with(10, 6, "one\r\ntwo\r\nthree\r\nfour");
        s.resize(10, 3);
        assert_eq!((s.row_text(0), s.row_text(2)), ("two".into(), "four".into()), "the prompt end stays visible when shrinking");
        assert!(s.cur.0 < 3);
        s.resize(20, 8);
        assert_eq!((s.cols, s.rows), (20, 8));
        assert_eq!(s.row_text(0), "two");
    }

    #[test]
    fn key_presses_become_the_bytes_a_terminal_sends() {
        let k = |name: &str| parse_key(name).unwrap();
        assert_eq!(key_bytes(&k("a")), b"a");
        assert_eq!(key_bytes(&k("A")), b"A");
        assert_eq!(key_bytes(&k("enter")), b"\r");
        assert_eq!(key_bytes(&k("backspace")), vec![0x7f]);
        assert_eq!(key_bytes(&k("ctrl-c")), vec![3]);
        assert_eq!(key_bytes(&k("ctrl-d")), vec![4]);
        assert_eq!(key_bytes(&k("ctrl-l")), vec![12]);
        assert_eq!(key_bytes(&k("alt-f")), b"\x1bf");
        assert_eq!(key_bytes(&k("up")), b"\x1b[A");
        assert_eq!(key_bytes(&k("delete")), b"\x1b[3~");
        assert_eq!(key_bytes(&k("shift-tab")), b"\x1b[Z");
        assert_eq!(key_bytes(&k("/")), b"/");
        assert_eq!(key_bytes(&Key::new(0)), Vec::<u8>::new());
    }

    fn wait_for(t: &Terminal, what: &str) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end { if t.screen_text().iter().any(|l| l.contains(what)) { return true; } std::thread::sleep(Duration::from_millis(20)); }
        false
    }

    #[test]
    fn a_real_program_runs_in_the_terminal_and_sees_the_window_size() {
        let t = Terminal::spawn("/bin/sh", &["-c", "printf 'size:'; stty size; printf 'done\\n'"], &std::env::temp_dir(), 50, 10, Arc::new(|| {})).unwrap();
        assert!(wait_for(&t, "10 50"), "the program was told the window is 10 rows by 50 columns: {:?}", t.screen_text());
        assert!(wait_for(&t, "done"));
        for _ in 0..100 { if t.exited() { break; } std::thread::sleep(Duration::from_millis(20)); }
        assert!(t.exited(), "we notice when the program has finished");
    }

    #[test]
    fn typing_reaches_the_program_and_is_echoed() {
        let mut t = Terminal::spawn("/bin/cat", &[], &std::env::temp_dir(), 40, 8, Arc::new(|| {})).unwrap();
        for c in "abc".chars() { t.handle(parse_key(&c.to_string()).unwrap(), &Env::for_tests()); }
        t.handle(parse_key("enter").unwrap(), &Env::for_tests());
        assert!(wait_for(&t, "abc"), "{:?}", t.screen_text());
        // Ctrl+D ends cat; the window then offers to close
        t.handle(parse_key("ctrl-d").unwrap(), &Env::for_tests());
        for _ in 0..100 { if t.exited() { break; } std::thread::sleep(Duration::from_millis(20)); }
        assert!(t.exited());
        assert_eq!(t.handle(parse_key("enter").unwrap(), &Env::for_tests()), Out::Close);
        assert!(t.title().contains("finished"));
    }

    #[test]
    fn resizing_the_window_resizes_the_terminal_the_program_sees() {
        let mut t = Terminal::spawn("/bin/sh", &["-c", "sleep 0.3; stty size"], &std::env::temp_dir(), 30, 6, Arc::new(|| {})).unwrap();
        t.resize(44, 12);
        assert!(wait_for(&t, "12 44"), "{:?}", t.screen_text());
    }

    #[test]
    fn a_missing_program_is_reported_not_hidden() {
        assert!(Terminal::spawn("/no/such/shell", &[], &std::env::temp_dir(), 40, 8, Arc::new(|| {})).is_err());
    }

    #[test]
    fn draws_with_colours_scrollback_and_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let t = Terminal::spawn("/bin/sh", &["-c", "printf '\\033[31mred\\033[0m plain\\n'"], &std::env::temp_dir(), 40, 8, Arc::new(|| {})).unwrap();
        assert!(wait_for(&t, "plain"));
        for (cols, rows) in [(10usize, 3usize), (40, 8), (100, 40)] {
            let m = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut canvas, m, t: Theme::new(Mode::Dark) };
            t.draw(&mut p, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
    }
}
