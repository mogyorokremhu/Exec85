//! The log viewer: the kernel's messages and every service's log in one place, with search.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
enum Screen { List, View, Search(String) }

pub struct Logs {
    be: Backend,
    job: Job,
    names: Vec<String>,
    sel: usize,
    screen: Screen,
    lines: Vec<String>,
    scroll: usize,
    rows: usize,
    query: String,
    msg: String,
    first: bool,
    opened: String,
}

/// The index of the next line (after `from`, wrapping round) that contains `query`, ignoring case.
pub fn find_next(lines: &[String], query: &str, from: usize) -> Option<usize> {
    if query.is_empty() || lines.is_empty() { return None; }
    let q = query.to_lowercase();
    (1..=lines.len()).map(|d| (from + d) % lines.len()).find(|i| lines[*i].to_lowercase().contains(&q))
}

impl Logs {
    pub fn new(be: Backend) -> Logs { Logs { be, job: Job::new(), names: vec![], sel: 0, screen: Screen::List, lines: vec![], scroll: 0, rows: 20, query: String::new(), msg: String::new(), first: true, opened: String::new() } }

    fn open(&mut self, env: &Env) {
        let Some(n) = self.names.get(self.sel).cloned() else { return };
        if self.job.start(env, &self.be, "tail", vec!["log-tail".into(), n.clone(), "1000".into()]) { self.opened = n; self.msg = "Reading ...".into(); }
    }

    fn finish(&mut self) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("list", Ok(t)) => { self.names = t.lines().filter(|l| !l.is_empty()).map(String::from).collect(); self.sel = self.sel.min(self.names.len().saturating_sub(1)); }
            ("list", Err(e)) => self.msg = e,
            ("tail", Ok(t)) => { self.lines = t.lines().map(String::from).collect(); self.screen = Screen::View; self.scroll = self.lines.len().saturating_sub(self.rows.max(1)); self.msg.clear(); }
            ("tail", Err(e)) => self.msg = e,
            _ => {}
        }
    }

    fn max_scroll(&self) -> usize { self.lines.len().saturating_sub(self.rows.max(1)) }
}

impl Application for Logs {
    fn kind(&self) -> Kind { Kind::Logs }
    fn title(&self) -> String { if self.screen == Screen::List { "Logs".into() } else { format!("Log - {}", self.opened) } }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(300) } else { Duration::from_secs(3600) }) }
    fn resize(&mut self, _cols: usize, rows: usize) { self.rows = rows.saturating_sub(2).max(1); }
    fn tick(&mut self, env: &Env) {
        self.finish();
        if self.first { self.first = false; self.job.start(env, &self.be, "list", vec!["log-list".into()]); }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish();
        match self.screen.clone() {
            Screen::List => match k.code {
                code::UP => self.sel = self.sel.saturating_sub(1),
                code::DOWN => self.sel = (self.sel + 1).min(self.names.len().saturating_sub(1)),
                code::ENTER => self.open(env),
                _ => if k.ch().map(|c| c.to_ascii_lowercase()) == Some('r') { self.job.start(env, &self.be, "list", vec!["log-list".into()]); },
            },
            Screen::View => {
                let page = self.rows.saturating_sub(1).max(1);
                match k.code {
                    code::ESC | code::BACKSPACE => { self.screen = Screen::List; self.msg.clear(); }
                    code::UP => self.scroll = self.scroll.saturating_sub(1),
                    code::DOWN => self.scroll = (self.scroll + 1).min(self.max_scroll()),
                    code::PGUP => self.scroll = self.scroll.saturating_sub(page),
                    code::PGDN | code::SPACE => self.scroll = (self.scroll + page).min(self.max_scroll()),
                    code::HOME => self.scroll = 0,
                    code::END => self.scroll = self.max_scroll(),
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('/') | Some('f') => self.screen = Screen::Search(self.query.clone()),
                        Some('n') => match find_next(&self.lines, &self.query, self.scroll) { Some(i) => { self.scroll = i.min(self.max_scroll()); self.msg = format!("Line {} of {}", i + 1, self.lines.len()); } None => self.msg = format!("No match for \"{}\".", self.query) },
                        Some('r') => self.open(env),
                        _ => {}
                    },
                }
            }
            Screen::Search(mut q) => match k.code {
                code::ESC => self.screen = Screen::View,
                code::ENTER => {
                    self.query = q; self.screen = Screen::View;
                    match find_next(&self.lines, &self.query, self.scroll.saturating_sub(1)) { Some(i) => { self.scroll = i.min(self.max_scroll()); self.msg = format!("Line {} of {}. N finds the next one.", i + 1, self.lines.len()); } None => self.msg = format!("No match for \"{}\".", self.query) }
                }
                _ => { edit_text(&mut q, &k, 60); self.screen = Screen::Search(q); }
            },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        match &self.screen {
            Screen::List => {
                p.text(a.x, a.y, "Choose a log to read", t.dim, None);
                if self.names.is_empty() { p.clip(a.x, a.y + 2, a.w, "Looking for logs ...", t.dim, t.panel); }
                let items: Vec<String> = self.names.iter().map(|n| if n == "kernel" { "kernel   (what the system itself reports)".to_string() } else if n == "messages" { "messages (general system log)".to_string() } else { format!("{n}   (service)") }).collect();
                p.list(Rect { x: a.x, y: a.y + 2, w: a.w, h: a.h.saturating_sub(4) }, &items, self.sel, self.sel.saturating_sub(a.h.saturating_sub(5)), focused);
                p.clip(a.x, foot, a.w, if self.msg.is_empty() { "Up/Down Choose   Enter Read   R Refresh the list" } else { &self.msg }, t.dim, t.panel);
            }
            Screen::View | Screen::Search(_) => {
                let rows = a.h.saturating_sub(2);
                let q = self.query.to_lowercase();
                for i in 0..rows {
                    let Some(l) = self.lines.get(self.scroll + i) else { break };
                    let hit = !q.is_empty() && l.to_lowercase().contains(&q);
                    p.clip(a.x, a.y + i, a.w, l, if hit { t.accent } else { t.text }, t.panel);
                }
                match &self.screen {
                    Screen::Search(s) => { p.text(a.x, foot, "Search:", t.accent, None); p.field(a.x + 8, foot, a.w.saturating_sub(8).max(1), s, focused, false); }
                    _ => p.clip(a.x, foot, a.w, if self.msg.is_empty() { "Up/Down PgUp/PgDn Scroll   End Newest   / Search   N Next match   R Reload   Esc Back" } else { &self.msg }, t.dim, t.panel),
                }
            }
        }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let screen = match self.screen { Screen::List => "list", Screen::View => "view", Screen::Search(_) => "search" };
        format!("name=logs screen={screen} logs={} sel={} open={} lines={} scroll={} query={} busy={} msg={}", self.names.len(), n(self.names.get(self.sel).map_or("", String::as_str)), n(&self.opened), self.lines.len(), self.scroll, n(&self.query), self.job.busy, n(&self.msg.chars().take(50).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "Enter Read   / Search   Esc Back   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app() -> (Logs, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() {
                "log-list" => Ok("kernel\nminecraft\nticker\n".into()),
                "log-tail" if r[1] == "ticker" => Err("That log cannot be read.".into()),
                "log-tail" => Ok((1..=100).map(|i| if i == 40 { "line 40: ERROR something broke".to_string() } else { format!("line {i}: all fine") }).collect::<Vec<_>>().join("\n")),
                _ => Ok(String::new()),
            }
        });
        (Logs::new(be), calls)
    }
    fn settle(l: &mut Logs) { let e = Env::for_tests(); for _ in 0..300 { l.tick(&e); if !l.job.busy { std::thread::sleep(Duration::from_millis(5)); l.tick(&e); if !l.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(l: &mut Logs, k: &str) { l.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(l); }
    fn text(l: &mut Logs, t: &str) { for c in t.chars() { key(l, &c.to_string()); } }

    #[test]
    fn it_lists_the_logs_and_opens_one_at_its_newest_end() {
        let (mut l, calls) = app(); settle(&mut l); l.resize(80, 12);
        assert!(l.state().contains("logs=3") && l.state().contains("sel=kernel"), "{}", l.state());
        key(&mut l, "down"); key(&mut l, "enter");
        assert!(calls.lock().unwrap().iter().any(|c| c == "log-tail minecraft 1000"));
        assert!(l.state().contains("screen=view") && l.state().contains("lines=100"), "{}", l.state());
        assert_eq!(l.scroll, 100 - l.rows, "starts at the newest lines");
        key(&mut l, "home"); assert_eq!(l.scroll, 0);
        key(&mut l, "esc"); assert!(l.state().contains("screen=list"));
        key(&mut l, "down"); key(&mut l, "enter"); assert!(l.state().contains("screen=list") && l.state().contains("cannot_be_read"), "{}", l.state());
    }

    #[test]
    fn search_finds_lines_ignoring_case_and_n_goes_on() {
        let (mut l, _c) = app(); settle(&mut l); l.resize(80, 12);
        key(&mut l, "enter"); key(&mut l, "home");
        key(&mut l, "/"); text(&mut l, "error"); key(&mut l, "enter");
        assert_eq!(l.scroll, 39, "{}", l.state());
        assert!(l.state().contains("query=error"));
        key(&mut l, "n"); assert!(l.msg.contains("Line 40"), "a single match: n stays on it ({})", l.msg);
        key(&mut l, "/"); text(&mut l, "x"); key(&mut l, "backspace"); key(&mut l, "backspace"); key(&mut l, "backspace"); key(&mut l, "backspace"); key(&mut l, "backspace"); key(&mut l, "backspace");
        text(&mut l, "zzz"); key(&mut l, "enter"); assert!(l.msg.contains("No match"), "{}", l.msg);
        assert_eq!(find_next(&["a".into(), "b".into(), "A".into()], "a", 0), Some(2));
        assert_eq!(find_next(&["a".into(), "b".into()], "a", 0), Some(0), "wraps round");
        assert_eq!(find_next(&[], "a", 0), None);
    }

    #[test]
    fn it_draws_every_screen_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut l, _c) = app(); settle(&mut l); l.resize(40, 10);
        for step in 0..3 {
            match step { 1 => key(&mut l, "enter"), 2 => key(&mut l, "/"), _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                l.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
