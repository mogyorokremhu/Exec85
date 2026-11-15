//! Photo and video gallery: choose the folder, set the password, switch the web page on.
use super::{edit_text, wrap_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info { pub enabled: bool, pub folder: String, pub user: String, pub uploads: bool, pub password: bool, pub port: String, pub fingerprint: String }

pub fn parse_status(t: &str) -> Info {
    let mut i = Info::default();
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        match k { "enabled" => i.enabled = v == "yes", "folder" => i.folder = if v == "-" { String::new() } else { v.into() }, "user" => i.user = if v == "-" { String::new() } else { v.into() },
            "uploads" => i.uploads = v == "yes", "password" => i.password = v == "set", "port" => i.port = v.into(), "fingerprint" => i.fingerprint = if v == "-" { String::new() } else { v.into() }, _ => {} }
    }
    i
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { List, Folder(String), Password(String) }

pub struct Gallery { be: Backend, job: Job, info: Info, mode: Mode, msg: String, addr: Option<String>, first: bool }

impl Gallery {
    pub fn new(be: Backend) -> Gallery { Gallery { be, job: Job::new(), info: Info::default(), mode: Mode::List, msg: String::new(), addr: None, first: true } }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { if self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { true } else { self.msg = "Please wait, the last request is still running.".into(); false } }
    fn refresh(&mut self, env: &Env) { self.ask(env, "status", &["gal-status"]); }
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

impl Application for Gallery {
    fn kind(&self) -> Kind { Kind::Gallery }
    fn title(&self) -> String { "Photo gallery".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, env: &Env) { self.finish(env); if !self.job.busy && (self.first || matches!(self.mode, Mode::List)) { self.first = false; self.refresh(env); } }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Folder(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.trim().to_string(); self.ask(env, "folder", &["gal-folder", &v]); }
                _ => { edit_text(&mut t, &k, 250); self.mode = Mode::Folder(t); }
            },
            Mode::Password(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { self.mode = Mode::List; let v = t.clone(); self.ask(env, "password", &["gal-password", &v]); }
                _ => { edit_text(&mut t, &k, 100); self.mode = Mode::Password(t); }
            },
            Mode::List => {
                self.msg.clear();
                match k.ch().map(|c| c.to_ascii_lowercase()) {
                    Some('f') => self.mode = Mode::Folder(self.info.folder.clone()),
                    Some('p') => self.mode = Mode::Password(String::new()),
                    Some('u') => { let w = if self.info.uploads { "off" } else { "on" }; self.ask(env, "uploads", &["gal-uploads", w]); }
                    Some('s') => { let w = if self.info.enabled { "gal-disable" } else { "gal-enable" }; if self.ask(env, "toggle", &[w]) { self.msg = "Working (the first time it makes a certificate) ...".into(); } }
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
        p.text(a.x, a.y, "Photo and video gallery", t.accent, None);
        p.text(a.x + 25, a.y, if self.info.enabled { "ON" } else { "OFF" }, if self.info.enabled { t.text } else { t.accent }, None);
        let mut y = a.y + 2;
        let mut line = |p: &mut Painter, label: &str, text: &str, color: u32, y: &mut usize| { if *y < foot.saturating_sub(2) { p.text(a.x, *y, label, t.dim, None); p.clip(a.x + 12, *y, a.w.saturating_sub(12), text, color, t.panel); *y += 1; } };
        line(p, "Folder", if self.info.folder.is_empty() { "not chosen yet (press F)" } else { &self.info.folder }, if self.info.folder.is_empty() { t.accent } else { t.text }, &mut y);
        line(p, "Runs as", if self.info.user.is_empty() { "-" } else { &self.info.user }, t.text, &mut y);
        line(p, "Password", if self.info.password { "set (press P to change)" } else { "not set yet (press P)" }, if self.info.password { t.text } else { t.accent }, &mut y);
        line(p, "Adding", if self.info.uploads { "people with the password can add pictures and videos (U to switch off)" } else { "off: people can only look (U to switch on)" }, t.text, &mut y);
        if self.info.enabled { let addr = format!("https://{}:{}", self.addr.clone().unwrap_or_else(|| "<this computer>".into()), self.info.port); line(p, "Address", &addr, t.accent, &mut y); }
        y += 1;
        for l in wrap_text("The page shows only pictures and videos of the chosen folder (and its subfolders), nothing else, and only to people who know the password. Your browser will warn once that the certificate is not trusted; it was made by this computer itself, so continue.", a.w) { if y < foot.saturating_sub(2) { p.clip(a.x, y, a.w, &l, t.dim, t.panel); y += 1; } }
        let prompt_row = foot.saturating_sub(2);
        match &self.mode {
            Mode::Folder(v) => { p.text(a.x, prompt_row, "Folder:", t.dim, None); p.field(a.x + 8, prompt_row, a.w.saturating_sub(8), v, true, false); }
            Mode::Password(v) => { p.text(a.x, prompt_row, "Password:", t.dim, None); p.field(a.x + 10, prompt_row, a.w.saturating_sub(10).min(50), v, true, true); }
            Mode::List => {}
        }
        let hint = match self.mode { Mode::List => "F Folder  P Password  U Adding  S On/off  R Refresh", Mode::Folder(_) => "A folder under /home, /srv, /media, /mnt or /data   Enter Save  Esc Cancel", Mode::Password(_) => "At least 8 characters   Enter Save  Esc Cancel" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::List => "list", Mode::Folder(_) => "folder", Mode::Password(_) => "password" };
        format!("name=gallery mode={mode} on={} folder={} password={} uploads={} busy={} msg={}", self.info.enabled, n(&self.info.folder), self.info.password, self.info.uploads, self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "F Folder  P Password  U Adding  S On/off   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const STATUS: &str = "enabled=no\nfolder=/home/tester/pics\nuser=tester\nuploads=no\npassword=set\nport=8445\nfingerprint=AB:CD\n";

    fn app() -> (Gallery, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); match r[0].as_str() { "gal-status" => Ok(STATUS.into()), "gal-folder" if r[1].contains("bad") => Err("That folder does not exist.".into()), _ => Ok("ok".into()) } });
        (Gallery::new(be), calls)
    }
    fn press(m: &mut Gallery, calls: &Arc<Mutex<Vec<String>>>, k: &str, want: &str) {
        let e = Env::for_tests();
        for _ in 0..200 { m.handle(parse_key(k).unwrap(), &e); for _ in 0..10 { m.tick(&e); std::thread::sleep(Duration::from_millis(2)); } if want.is_empty() || calls.lock().unwrap().iter().any(|c| c == want) { return; } }
        panic!("{want} never asked: {:?}", calls.lock().unwrap().iter().filter(|c| *c != "gal-status").collect::<Vec<_>>());
    }
    fn typed(m: &mut Gallery, text: &str) { let e = Env::for_tests(); for c in text.chars() { m.handle(parse_key(&c.to_string()).unwrap_or_else(|| parse_key(if c == ' ' { "space" } else { "x" }).unwrap()), &e); } }

    #[test]
    fn the_status_text_is_read() {
        let i = parse_status(STATUS);
        assert_eq!(i, Info { enabled: false, folder: "/home/tester/pics".into(), user: "tester".into(), uploads: false, password: true, port: "8445".into(), fingerprint: "AB:CD".into() });
        assert_eq!(parse_status("folder=-\nuser=-\npassword=none\n").folder, "");
    }

    #[test]
    fn the_folder_password_adding_and_switch_are_set_from_the_keyboard() {
        let (mut m, calls) = app();
        let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        assert!(m.state().contains("folder=/home/tester/pics") && m.state().contains("password=true") && m.state().contains("on=false"), "{}", m.state());
        press(&mut m, &calls, "u", "gal-uploads on");
        press(&mut m, &calls, "s", "gal-enable");
        press(&mut m, &calls, "p", ""); assert!(m.state().contains("mode=password"));
        typed(&mut m, "abc"); press(&mut m, &calls, "enter", "gal-password abc");
        press(&mut m, &calls, "f", ""); assert!(m.state().contains("mode=folder"));
        press(&mut m, &calls, "esc", ""); assert!(m.state().contains("mode=list"));
        press(&mut m, &calls, "f", ""); for _ in 0..30 { press(&mut m, &calls, "backspace", ""); } typed(&mut m, "/srv/bad"); press(&mut m, &calls, "enter", "gal-folder /srv/bad");
        for _ in 0..40 { for _ in 0..10 { m.tick(&e); std::thread::sleep(Duration::from_millis(2)); } if m.msg.contains("does not exist") { break; } }
        assert!(m.msg.contains("does not exist"), "{}", m.msg);
        assert!(!m.state().contains("abc"), "the password is never in the state line");
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut m, calls) = app(); let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        for step in 0..3 {
            match step { 1 => press(&mut m, &calls, "f", ""), 2 => { press(&mut m, &calls, "esc", ""); press(&mut m, &calls, "p", ""); } _ => {} }
            for (w, h) in [(8usize, 4usize), (50, 14), (110, 36)] { let mut c = Canvas::new(1280, 800); let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) }; m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true); }
        }
    }
}
