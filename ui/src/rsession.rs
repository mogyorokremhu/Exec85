//! One remote session: its own copy of the Exec85 shell, drawn into memory instead of onto the screen.
//! It runs as the person who logged in (never as root). The web server talks to it through a pipe:
//! lines in (keys), picture messages out (see `rframe`).
//!
//! Lines in:  `k NAME`  a named key, optionally with ctrl-/alt-/shift- in front (`k alt-a`, `k f10`, `k ctrl-c`, `k enter`)
//!            `t TEXT`  typed text (a phone's keyboard, a paste)
//!            `full`    send the whole picture again      `bye`  end the session
use crate::gfx::{Canvas, Metrics};
use crate::keys::{self, Key, Layout};
use crate::rframe;
use crate::shell::Shell;
use crate::app::Effect;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

pub const MIN_W: usize = 480;
pub const MAX_W: usize = 1920;
pub const MIN_H: usize = 320;
pub const MAX_H: usize = 1200;

pub enum Event { Key(Key), Full, Bye, Wake }

/// What one input line means. Unknown or empty lines mean nothing.
pub fn parse_line(line: &str, layout: Layout) -> Vec<Event> {
    let line = line.trim_end_matches(['\r', '\n']);
    if line == "full" { return vec![Event::Full]; }
    if line == "bye" { return vec![Event::Bye]; }
    if let Some(text) = line.strip_prefix("t ") {
        return text.chars().take(2000).filter_map(|c| keys::key_for_char(layout, c)).map(Event::Key).collect();
    }
    let Some(name) = line.strip_prefix("k ") else { return vec![] };
    let (mut ctrl, mut alt, mut shift) = (false, false, false);
    let mut rest = name.trim();
    loop {
        if let Some(r) = rest.strip_prefix("ctrl-") { ctrl = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("alt-") { alt = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("shift-") { shift = true; rest = r; }
        else { break; }
    }
    let mut chars = rest.chars();
    let key = match (chars.next(), chars.next()) {
        (Some(c), None) => keys::key_for_char(layout, c),        // a letter: find it on the person's own layout
        _ => keys::parse_key(rest),
    };
    match key {
        Some(mut k) => { k.ctrl |= ctrl; k.alt |= alt; k.shift |= shift; vec![Event::Key(k)] }
        None => vec![],
    }
}

/// The shell plus what is needed to turn it into pictures.
pub struct Session {
    pub shell: Shell,
    pub canvas: Canvas,
    m: Metrics,
    enc: rframe::Encoder,
    extra: String,
}

impl Session {
    pub fn new(w: usize, h: usize, start: PathBuf, home: PathBuf, user: &str, wake: Arc<dyn Fn() + Send + Sync>) -> Session {
        let (w, h) = (w.clamp(MIN_W, MAX_W), h.clamp(MIN_H, MAX_H));
        let m = Metrics::for_height(h);
        let mut shell = Shell::new(w / m.cw, h / m.ch, crate::read_mode(), start, home, wake);
        shell.app.sleep_minutes = 0;          // a remote screen never goes dark
        let extra = format!(" user={user} uid={} host={} remote=yes", crate::sys::uid(), crate::hostname());
        let mut s = Session { shell, canvas: Canvas::new(w, h), m, enc: rframe::Encoder::new(), extra };
        s.draw();
        s
    }
    fn draw(&mut self) { crate::ui::draw(&self.shell.app, &self.shell.windows, &mut self.canvas, &self.m); }
    pub fn force_full(&mut self) { self.enc.force_full(); }

    /// Run a key and the timers; returns the effects the outside world must carry out.
    pub fn step(&mut self, keys: Vec<Key>) -> Vec<Effect> {
        let mut effects = Vec::new();
        for k in keys { effects.extend(self.shell.handle(k)); }
        self.shell.tick();
        effects.extend(self.shell.take_effects());
        self.draw();
        effects
    }

    /// The messages to send now: a picture update if the screen changed, and the state text if it changed.
    pub fn output(&mut self, from_key: bool) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if let Some(p) = self.enc.diff(&self.canvas) { out.push(rframe::message(rframe::KIND_FRAME, &p)); }
        let lines = if from_key { self.shell.state_lines(&self.extra) } else { self.shell.changed_state_lines(&self.extra) };
        if !lines.is_empty() { out.push(rframe::message(rframe::KIND_STATE, lines.join("\n").as_bytes())); }
        out
    }
}

fn send(out: &mut impl Write, messages: Vec<Vec<u8>>) -> bool {
    for m in messages { if out.write_all(&m).is_err() { return false; } }
    out.flush().is_ok()
}

/// The program `exec85 --remote-session WIDTH HEIGHT`: it speaks on stdin and stdout.
pub fn run(w: usize, h: usize) -> ! {
    crate::load_layout();
    let start = std::env::var("EXEC85_START").map(PathBuf::from).or_else(|_| std::env::var("HOME").map(PathBuf::from)).unwrap_or_else(|_| PathBuf::from("/"));
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| start.clone());
    let user = std::env::var("USER").unwrap_or_else(|_| "?".into());
    let (tx, rx) = channel::<Event>();
    let wake_tx = tx.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || { let _ = wake_tx.send(Event::Wake); });
    let mut s = Session::new(w, h, start, home, &user, wake);
    thread::spawn(move || {
        let layout = keys::layout();
        for line in std::io::stdin().lock().lines().map_while(Result::ok) {
            for e in parse_line(&line, layout) { if tx.send(e).is_err() { return; } }
        }
        let _ = tx.send(Event::Bye);     // the server went away
    });
    let mut stdout = std::io::stdout().lock();
    if !send(&mut stdout, s.output(true)) { std::process::exit(1); }
    loop {
        let timeout = s.shell.next_deadline().unwrap_or(Duration::from_secs(3600));
        let mut keys = Vec::new();
        let mut full = false;
        match rx.recv_timeout(timeout) {
            Ok(Event::Key(k)) => keys.push(k),
            Ok(Event::Full) => full = true,
            Ok(Event::Bye) => std::process::exit(0),
            Ok(Event::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => std::process::exit(0),
        }
        while let Ok(e) = rx.try_recv() { match e { Event::Key(k) => keys.push(k), Event::Full => full = true, Event::Bye => std::process::exit(0), Event::Wake => {} } }
        let from_key = !keys.is_empty();
        let effects = s.step(keys);
        if full { s.force_full(); }
        if !send(&mut stdout, s.output(from_key || full)) { std::process::exit(1); }
        for e in effects {
            let (word, code) = match e {
                Effect::Logout => ("logout", crate::supervisor::EXIT_LOGOUT),
                Effect::PowerOff => ("poweroff", crate::supervisor::EXIT_POWEROFF),
                Effect::Reboot => ("reboot", crate::supervisor::EXIT_REBOOT),
                Effect::SaveTheme(m) => { crate::save_theme(m); continue; }
                Effect::SaveSleep(m) => { crate::save_sleep(m); continue; }
                _ => continue,
            };
            send(&mut stdout, vec![rframe::message(rframe::KIND_BYE, word.as_bytes())]);
            std::process::exit(code);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(e: &Event) -> Key { match e { Event::Key(k) => *k, _ => panic!("not a key") } }

    #[test]
    fn lines_become_keys_on_the_persons_own_layout() {
        let k = key(&parse_line("k alt-a", Layout::Us)[0]);
        assert!(k.alt && k.code == 30);
        let k = key(&parse_line("k f10", Layout::Us)[0]);
        assert_eq!(k.code, keys::code::F10);
        let k = key(&parse_line("k ctrl-c", Layout::Us)[0]);
        assert!(k.ctrl && k.code == 46);
        // on a German keyboard the letter Y sits where a US keyboard has Z
        assert_eq!(key(&parse_line("k y", Layout::De)[0]).code, 44);
        assert_eq!(key(&parse_line("k y", Layout::Us)[0]).code, 21);
        let text = parse_line("t Hi!\n", Layout::Us);
        assert_eq!(text.len(), 3, "the newline is part of the line, not of the text: {}", text.len());
        assert!(key(&text[0]).shift);
        assert!(matches!(parse_line("full", Layout::Us)[0], Event::Full) && matches!(parse_line("bye\r\n", Layout::Us)[0], Event::Bye));
        for junk in ["", "k", "k nonsense-key", "x y", "k alt-", "t \u{1F600}"] { assert!(parse_line(junk, Layout::Us).is_empty(), "{junk:?}"); }
    }

    #[test]
    fn a_session_draws_a_full_picture_then_only_changes_and_reports_state() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let mut s = Session::new(1280, 800, PathBuf::from("/"), PathBuf::from("/"), "tester", wake);
        let first = s.output(true);
        assert!(first.len() == 2 && first[0][0] == rframe::KIND_FRAME && first[1][0] == rframe::KIND_STATE);
        let state = String::from_utf8_lossy(&first[1][5..]).to_string();
        assert!(state.contains("user=tester") && state.contains("remote=yes") && state.contains("menu=none"), "{state}");
        assert!(s.output(false).is_empty(), "nothing changed, nothing sent");
        // Alt+A opens the Apps menu: the picture changes, a few tiles only, and the state says so
        s.step(vec![key(&parse_line("k alt-a", Layout::Us)[0])]);
        let after = s.output(true);
        assert_eq!(after[0][0], rframe::KIND_FRAME);
        assert!(after[0].len() < first[0].len() / 2, "an open menu is a small change: {} of {}", after[0].len(), first[0].len());
        assert!(String::from_utf8_lossy(&after[1][5..]).contains("menu=apps"));
        // the browser reassembles exactly what the session drew
        let mut screen = None;
        rframe::apply(&first[0][5..], &mut screen).unwrap();
        rframe::apply(&after[0][5..], &mut screen).unwrap();
        assert_eq!(screen.unwrap().px.iter().map(|p| p & 0xFFFFFF).collect::<Vec<_>>(), s.canvas.px.iter().map(|p| p & 0xFFFFFF).collect::<Vec<_>>());
        s.force_full();
        assert!(!s.output(false).is_empty(), "a new browser gets everything");
    }

    #[test]
    fn sizes_are_kept_within_sane_limits() {
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
        let s = Session::new(10, 10, PathBuf::from("/"), PathBuf::from("/"), "t", wake.clone());
        assert_eq!((s.canvas.w, s.canvas.h), (MIN_W, MIN_H));
        let s = Session::new(100_000, 100_000, PathBuf::from("/"), PathBuf::from("/"), "t", wake);
        assert_eq!((s.canvas.w, s.canvas.h), (MAX_W, MAX_H));
    }
}
