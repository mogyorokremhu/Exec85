//! Ad blocker: switch the network's DNS ad blocker on and off, update its list, allow a name that breaks a site, check a name.
use super::{edit_text, wrap_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info { pub enabled: bool, pub installed: bool, pub entries: String, pub updated: String, pub allowed: String, pub upstream: String }

pub fn parse_status(t: &str) -> Info {
    let mut i = Info::default();
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        let dash = |s: &str| if s == "-" { String::new() } else { s.to_string() };
        match k { "enabled" => i.enabled = v == "yes", "installed" => i.installed = v == "yes", "entries" => i.entries = v.into(), "updated" => i.updated = dash(v), "allowed" => i.allowed = dash(v), "upstream" => i.upstream = v.into(), _ => {} }
    }
    i
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { List, Allow(String), Unallow(String), Check(String) }

pub struct Adblock { be: Backend, job: Job, info: Info, mode: Mode, msg: String, addr: Option<String>, first: bool }

impl Adblock {
    pub fn new(be: Backend) -> Adblock { Adblock { be, job: Job::new(), info: Info::default(), mode: Mode::List, msg: String::new(), addr: None, first: true } }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { if self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { true } else { self.msg = "Please wait, the last request is still running.".into(); false } }
    fn refresh(&mut self, env: &Env) { self.ask(env, "status", &["ab-status"]); }
    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("status", Ok(t)) => { self.info = parse_status(&t); self.addr = super::network::local_ip(); }
            ("status", Err(e)) => self.msg = e,
            (_, Ok(m)) => { if !m.is_empty() { self.msg = m; } self.refresh(env); }
            (_, Err(e)) => { self.msg = e; self.refresh(env); }
        }
    }
}

impl Application for Adblock {
    fn kind(&self) -> Kind { Kind::Adblock }
    fn title(&self) -> String { "Ad blocker".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, env: &Env) { self.finish(env); if !self.job.busy && (self.first || matches!(self.mode, Mode::List)) { self.first = false; self.refresh(env); } }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Allow(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "allow", &["ab-allow", &v]); }
                _ => { edit_text(&mut t, &k, 100); self.mode = Mode::Allow(t); }
            },
            Mode::Unallow(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "unallow", &["ab-unallow", &v]); }
                _ => { edit_text(&mut t, &k, 100); self.mode = Mode::Unallow(t); }
            },
            Mode::Check(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "check", &["ab-check", &v]); }
                _ => { edit_text(&mut t, &k, 100); self.mode = Mode::Check(t); }
            },
            Mode::List => {
                self.msg.clear();
                match k.ch().map(|c| c.to_ascii_lowercase()) {
                    Some('s') => { let w = if self.info.enabled { "ab-disable" } else { "ab-enable" }; if self.ask(env, "toggle", &[w]) && !self.info.enabled { self.msg = "Switching on (the first time it installs the DNS server and downloads the list) ...".into(); } }
                    Some('u') => { if self.ask(env, "update", &["ab-update"]) { self.msg = "Downloading the list ...".into(); } }
                    Some('a') => self.mode = Mode::Allow(String::new()),
                    Some('d') => self.mode = Mode::Unallow(String::new()),
                    Some('c') => self.mode = Mode::Check(String::new()),
                    Some('r') => self.refresh(env),
                    _ => {}
                }
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Ad blocker for the whole network", t.accent, None);
        p.text(a.x + 34, a.y, if self.info.enabled { "ON" } else { "OFF" }, if self.info.enabled { t.text } else { t.accent }, None);
        let mut y = a.y + 2;
        let mut line = |p: &mut Painter, label: &str, text: &str, color: u32, y: &mut usize| { if *y < foot.saturating_sub(2) { p.text(a.x, *y, label, t.dim, None); p.clip(a.x + 12, *y, a.w.saturating_sub(12), text, color, t.panel); *y += 1; } };
        line(p, "Blocks", &if self.info.entries.is_empty() || self.info.entries == "0" { "nothing yet (press S)".to_string() } else { format!("{} names", self.info.entries) }, t.text, &mut y);
        line(p, "List from", if self.info.updated.is_empty() { "-" } else { &self.info.updated }, t.text, &mut y);
        line(p, "Allowed", if self.info.allowed.is_empty() { "nothing (press A to allow a name)" } else { &self.info.allowed }, t.text, &mut y);
        line(p, "Passes on to", &self.info.upstream, t.dim, &mut y);
        if self.info.enabled { line(p, "DNS address", &self.addr.clone().unwrap_or_else(|| "<this computer>".into()), t.accent, &mut y); }
        y += 1;
        let help = "Devices that use this computer as their DNS server do not load the ad and tracker servers on the list. Easiest: in your router's settings, set the DNS server to this computer's address (give the router a second, normal DNS server as a backup, so that the internet keeps working when this computer is off). Or set it on one phone or PC only. Some ads come from the same server as the page and cannot be blocked this way. If a site breaks, press A and type the name that it needs.";
        for l in wrap_text(help, a.w) { if y < foot.saturating_sub(2) { p.text(a.x, y, &l, t.dim, None); y += 1; } }
        let prompt_row = foot.saturating_sub(2);
        match &self.mode {
            Mode::Allow(v) => { p.text(a.x, prompt_row, "Allow:", t.dim, None); p.field(a.x + 8, prompt_row, a.w.saturating_sub(8), v, true, false); }
            Mode::Unallow(v) => { p.text(a.x, prompt_row, "Block again:", t.dim, None); p.field(a.x + 14, prompt_row, a.w.saturating_sub(14), v, true, false); }
            Mode::Check(v) => { p.text(a.x, prompt_row, "Check:", t.dim, None); p.field(a.x + 8, prompt_row, a.w.saturating_sub(8), v, true, false); }
            Mode::List => {}
        }
        let hint = match self.mode { Mode::List => "S On/off  U Update list  A Allow  D Block again  C Check a name  R Refresh", _ => "Type a name like example.com   Enter OK   Esc Cancel" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::List => "list", Mode::Allow(_) => "allow", Mode::Unallow(_) => "unallow", Mode::Check(_) => "check" };
        format!("name=adblock mode={mode} on={} entries={} allowed={} busy={} msg={}", self.info.enabled, n(&self.info.entries), n(&self.info.allowed), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "S On/off  U Update  A Allow  C Check   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const STATUS: &str = "enabled=no\ninstalled=yes\nentries=1502\nupdated=2026-11-20\nallowed=-\nupstream=192.168.1.1\n";

    fn app() -> (Adblock, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); match r[0].as_str() { "ab-status" => Ok(STATUS.into()), _ => Ok("Done.".into()) } });
        (Adblock::new(be), calls)
    }
    fn press(m: &mut Adblock, calls: &Arc<Mutex<Vec<String>>>, k: &str, want: &str) {
        let e = Env::for_tests();
        for _ in 0..200 { m.handle(parse_key(k).unwrap(), &e); for _ in 0..10 { m.tick(&e); std::thread::sleep(Duration::from_millis(2)); } if want.is_empty() || calls.lock().unwrap().iter().any(|c| c == want) { return; } }
        panic!("{want} never asked: {:?}", calls.lock().unwrap());
    }
    fn typed(m: &mut Adblock, text: &str) { let e = Env::for_tests(); for c in text.chars() { m.handle(parse_key(&c.to_string()).unwrap_or_else(|| parse_key(if c == '.' { "period" } else { "x" }).unwrap()), &e); } }

    #[test]
    fn the_status_text_is_read() {
        assert_eq!(parse_status(STATUS), Info { enabled: false, installed: true, entries: "1502".into(), updated: "2026-11-20".into(), allowed: String::new(), upstream: "192.168.1.1".into() });
    }

    #[test]
    fn the_keys_switch_update_allow_block_again_and_check() {
        let (mut m, calls) = app(); let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        assert!(m.state().contains("entries=1502") && m.state().contains("on=false"), "{}", m.state());
        press(&mut m, &calls, "s", "ab-enable");
        press(&mut m, &calls, "u", "ab-update");
        press(&mut m, &calls, "a", ""); assert!(m.state().contains("mode=allow"));
        typed(&mut m, "abc"); press(&mut m, &calls, "enter", "ab-allow abc");
        press(&mut m, &calls, "d", ""); assert!(m.state().contains("mode=unallow")); typed(&mut m, "abc"); press(&mut m, &calls, "enter", "ab-unallow abc");
        press(&mut m, &calls, "c", ""); assert!(m.state().contains("mode=check")); press(&mut m, &calls, "esc", ""); assert!(m.state().contains("mode=list"));
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut m, calls) = app(); let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        for step in 0..3 {
            match step { 1 => press(&mut m, &calls, "a", ""), 2 => { press(&mut m, &calls, "esc", ""); press(&mut m, &calls, "c", ""); } _ => {} }
            for (w, h) in [(8usize, 4usize), (50, 14), (110, 36)] { let mut c = Canvas::new(1280, 800); let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) }; m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true); }
        }
    }
}
