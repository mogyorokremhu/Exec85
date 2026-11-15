//! Website monitor: the list of watched sites with their state, and the setup of the phone alerts.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SiteRow { pub id: String, pub seconds: u64, pub state: String, pub ago: u64, pub code: u16, pub url: String }

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info { pub enabled: bool, pub topic: String, pub server: String, pub discord: bool, pub sites: Vec<SiteRow> }

pub fn parse_status(t: &str) -> Info {
    let mut i = Info::default();
    for l in t.lines() {
        if let Some(v) = l.strip_prefix("enabled=") { i.enabled = v.trim() == "yes"; }
        else if let Some(v) = l.strip_prefix("ntfy=") { i.topic = if v.trim() == "-" { String::new() } else { v.trim().to_string() }; }
        else if let Some(v) = l.strip_prefix("server=") { i.server = v.trim().to_string(); }
        else if let Some(v) = l.strip_prefix("discord=") { i.discord = v.trim() == "yes"; }
        else if let Some(rest) = l.strip_prefix("site ") {
            let w: Vec<&str> = rest.splitn(6, ' ').collect();
            if w.len() == 6 { i.sites.push(SiteRow { id: w[0].into(), seconds: w[1].parse().unwrap_or(0), state: w[2].into(), ago: w[3].parse().unwrap_or(0), code: w[4].parse().unwrap_or(0), url: w[5].trim().into() }); }
        }
    }
    i
}

fn how_long(secs: u64) -> String { if secs < 90 { format!("{secs} s") } else if secs < 5400 { format!("{} min", (secs + 30) / 60) } else if secs < 172_800 { format!("{} h", (secs + 1800) / 3600) } else { format!("{} days", secs / 86_400) } }

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { List, Add(String), Topic(String), Discord(String), Remove(String) }

pub struct Sitewatch { be: Backend, job: Job, info: Info, sel: usize, mode: Mode, msg: String, first: bool }

impl Sitewatch {
    pub fn new(be: Backend) -> Sitewatch { Sitewatch { be, job: Job::new(), info: Info::default(), sel: 0, mode: Mode::List, msg: String::new(), first: true } }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { if self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { true } else { self.msg = "Please wait, the last request is still running.".into(); false } }
    fn refresh(&mut self, env: &Env) { self.ask(env, "status", &["sw-status"]); }
    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("status", Ok(t)) => { self.info = parse_status(&t); self.sel = self.sel.min(self.info.sites.len().saturating_sub(1)); }
            ("status", Err(e)) => self.msg = e,
            (_, Ok(m)) => { if !m.is_empty() { self.msg = m; } self.refresh(env); }
            (_, Err(e)) => { self.msg = e; self.refresh(env); }
        }
    }
}

impl Application for Sitewatch {
    fn kind(&self) -> Kind { Kind::Sitewatch }
    fn title(&self) -> String { "Website monitor".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if !self.job.busy && (self.first || matches!(self.mode, Mode::List)) { self.first = false; self.refresh(env); }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Add(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => {
                    let w: Vec<&str> = t.split_whitespace().collect();
                    match w.as_slice() {
                        [url] => { self.mode = Mode::List; self.ask(env, "add", &["sw-add", url]); }
                        [url, minutes] if minutes.parse::<u64>().is_ok() => { let secs = (minutes.parse::<u64>().unwrap_or(2) * 60).to_string(); self.mode = Mode::List; self.ask(env, "add", &["sw-add", url, &secs]); }
                        _ => self.msg = "Type the address, and after a space how often to check in minutes (for example: https://example.com 2).".into(),
                    }
                }
                _ => { edit_text(&mut t, &k, 300); self.mode = Mode::Add(t); }
            },
            Mode::Topic(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "notify", &["sw-notify", "ntfy-topic", &v]); }
                _ => { edit_text(&mut t, &k, 64); self.mode = Mode::Topic(t); }
            },
            Mode::Discord(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "notify", &["sw-notify", "discord", &v]); }
                _ => { edit_text(&mut t, &k, 300); self.mode = Mode::Discord(t); }
            },
            Mode::Remove(id) => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => { self.mode = Mode::List; self.ask(env, "remove", &["sw-remove", &id]); }
                _ => self.mode = Mode::List,
            },
            Mode::List => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(self.info.sites.len().saturating_sub(1)),
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('a') => self.mode = Mode::Add(String::new()),
                        Some('d') => if let Some(s) = self.info.sites.get(self.sel) { self.mode = Mode::Remove(s.id.clone()); },
                        Some('n') => self.mode = Mode::Topic(self.info.topic.clone()),
                        Some('w') => self.mode = Mode::Discord(String::new()),
                        Some('t') => { if self.ask(env, "test", &["sw-test"]) { self.msg = "Sending a test message ...".into(); } }
                        Some('s') => { let (tag, w) = if self.info.enabled { ("off", "sw-disable") } else { ("on", "sw-enable") }; if self.ask(env, tag, &[w]) { self.msg = "Working ...".into(); } }
                        Some('r') => self.refresh(env),
                        _ => {}
                    },
                }
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Website monitor", t.accent, None);
        p.text(a.x + 17, a.y, if self.info.enabled { "ON  (checking in the background)" } else { "OFF  (press S to start it)" }, if self.info.enabled { t.text } else { t.accent }, None);
        let alerts = match (self.info.topic.is_empty(), self.info.discord) {
            (true, false) => "Alerts: not set up yet.  Press N for your phone (ntfy.sh topic) or W for Discord.".to_string(),
            (false, d) => format!("Alerts: phone via {} topic \"{}\"{}", self.info.server.trim_start_matches("https://"), self.info.topic, if d { " and Discord" } else { "" }),
            (true, true) => "Alerts: Discord".to_string(),
        };
        p.clip(a.x, a.y + 1, a.w, &alerts, if self.info.topic.is_empty() && !self.info.discord { t.accent } else { t.dim }, t.panel);
        let items: Vec<String> = self.info.sites.iter().map(|s| {
            let st = match s.state.as_str() { "up" => format!("UP   {}", how_long(s.ago)), "down" => format!("DOWN {}", how_long(s.ago)), _ => "checking ...".to_string() };
            format!("{:<16} {:<4} every {:>4}  {}", st, s.id, how_long(s.seconds), s.url)
        }).collect();
        if items.is_empty() { p.text(a.x, a.y + 3, "No websites yet. Press A to add one.", t.dim, None); }
        let h = a.h.saturating_sub(7).max(1);
        p.list(Rect { x: a.x, y: a.y + 3, w: a.w, h }, &items, self.sel, self.sel.saturating_sub(h - 1), focused && matches!(self.mode, Mode::List));
        let prompt_row = foot.saturating_sub(2);
        match &self.mode {
            Mode::Add(v) => { p.text(a.x, prompt_row, "Address and minutes:", t.dim, None); p.field(a.x + 21, prompt_row, a.w.saturating_sub(21), v, true, false); }
            Mode::Topic(v) => { p.text(a.x, prompt_row, "ntfy topic:", t.dim, None); p.field(a.x + 12, prompt_row, a.w.saturating_sub(12).min(66), v, true, false); }
            Mode::Discord(v) => { p.text(a.x, prompt_row, "Discord webhook:", t.dim, None); p.field(a.x + 16, prompt_row, a.w.saturating_sub(16), v, true, false); }
            Mode::Remove(id) => p.clip(a.x, prompt_row, a.w, &format!("Stop watching {id}?  [Y] Yes  [N] No"), t.accent, t.panel),
            Mode::List => {}
        }
        let hint = match self.mode { Mode::List => "A Add  D Remove  N Phone alerts  W Discord  T Test  S On/off  R Refresh", Mode::Topic(_) => "A long secret name (letters, digits, - _). In the ntfy phone app, subscribe to the same name.   Enter Save  Esc Cancel", _ => "Enter Save   Esc Cancel" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::List => "list", Mode::Add(_) => "add", Mode::Topic(_) => "topic", Mode::Discord(_) => "discord", Mode::Remove(_) => "remove" };
        format!("name=sitewatch mode={mode} on={} sites={} up={} down={} topic={} discord={} sel={} busy={} msg={}", self.info.enabled, self.info.sites.len(), self.info.sites.iter().filter(|s| s.state == "up").count(), self.info.sites.iter().filter(|s| s.state == "down").count(), n(&self.info.topic), self.info.discord, self.sel, self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "A Add  D Remove  N Phone alerts  T Test  S On/off   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const STATUS: &str = "enabled=yes\nntfy=garage-4821\nserver=https://ntfy.sh\ndiscord=no\nsite s1 120 up 600 200 https://example.com\nsite s2 60 down 300 0 http://192.168.1.5:8080/\nsite s3 60 unknown 0 0 https://new.example\n";

    fn app() -> (Sitewatch, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); match r[0].as_str() { "sw-status" => Ok(STATUS.into()), "sw-add" if r[1].contains("bad") => Err("That is not a web address I can watch.".into()), _ => Ok("ok".into()) } });
        (Sitewatch::new(be), calls)
    }
    fn settle(m: &mut Sitewatch) { let e = Env::for_tests(); for _ in 0..40 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); } }
    fn press(m: &mut Sitewatch, calls: &Arc<Mutex<Vec<String>>>, k: &str, want: &str) {
        let e = Env::for_tests();
        for _ in 0..200 { m.handle(parse_key(k).unwrap(), &e); for _ in 0..10 { m.tick(&e); std::thread::sleep(Duration::from_millis(2)); } if want.is_empty() || calls.lock().unwrap().iter().any(|c| c == want) { return; } }
        panic!("{want} never asked: {:?}", calls.lock().unwrap().iter().filter(|c| *c != "sw-status").collect::<Vec<_>>());
    }
    fn typed(m: &mut Sitewatch, text: &str) { let e = Env::for_tests(); for c in text.chars() { m.handle(parse_key(&c.to_string()).unwrap_or_else(|| parse_key("space").unwrap()), &e); } }

    #[test]
    fn the_status_text_becomes_rows() {
        let i = parse_status(STATUS);
        assert!(i.enabled && i.topic == "garage-4821" && i.server == "https://ntfy.sh" && !i.discord && i.sites.len() == 3);
        assert_eq!(i.sites[1], SiteRow { id: "s2".into(), seconds: 60, state: "down".into(), ago: 300, code: 0, url: "http://192.168.1.5:8080/".into() });
        assert_eq!(parse_status("ntfy=-\n").topic, "");
        assert_eq!(how_long(30), "30 s"); assert_eq!(how_long(600), "10 min"); assert_eq!(how_long(7200), "2 h"); assert_eq!(how_long(400_000), "4 days");
    }

    #[test]
    fn sites_are_added_removed_and_the_alerts_are_tested_from_the_keyboard() {
        let (mut m, calls) = app(); settle(&mut m);
        assert!(m.state().contains("sites=3") && m.state().contains("up=1") && m.state().contains("down=1") && m.state().contains("topic=garage-4821"), "{}", m.state());
        // add: an address, then minutes
        press(&mut m, &calls, "a", ""); assert!(m.state().contains("mode=add"));
        typed(&mut m, "https://x.example"); typed(&mut m, " 5");
        press(&mut m, &calls, "enter", "sw-add https://x.example 300");
        // remove the selected one, only after yes
        press(&mut m, &calls, "d", ""); assert!(m.state().contains("mode=remove"));
        press(&mut m, &calls, "n", ""); assert!(m.state().contains("mode=list") && !calls.lock().unwrap().iter().any(|c| c.starts_with("sw-remove")));
        press(&mut m, &calls, "down", ""); press(&mut m, &calls, "d", ""); press(&mut m, &calls, "y", "sw-remove s2");
        press(&mut m, &calls, "t", "sw-test");
        press(&mut m, &calls, "s", "sw-disable");
        press(&mut m, &calls, "n", ""); assert!(m.state().contains("mode=topic"));
        press(&mut m, &calls, "esc", ""); assert!(m.state().contains("mode=list"));
        // a bad address shows the helper's explanation
        press(&mut m, &calls, "a", ""); typed(&mut m, "bad"); press(&mut m, &calls, "enter", "sw-add bad");
        for _ in 0..20 { settle(&mut m); if m.msg.contains("not a web address") { break; } }
        assert!(m.msg.contains("not a web address"), "{}", m.msg);
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut m, calls) = app(); settle(&mut m);
        for step in 0..4 {
            match step { 1 => press(&mut m, &calls, "a", ""), 2 => { press(&mut m, &calls, "esc", ""); press(&mut m, &calls, "n", ""); } 3 => { press(&mut m, &calls, "esc", ""); press(&mut m, &calls, "d", ""); } _ => {} }
            for (w, h) in [(8usize, 4usize), (50, 14), (110, 36)] { let mut c = Canvas::new(1280, 800); let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) }; m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true); }
        }
    }
}
