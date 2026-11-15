//! Updates: put a newer Exec85 on this computer without installing again. Only the system's own files change;
//! settings, accounts, services, firewall, game servers and everything in the home folders stay as they are.
use super::{edit_text, wrap_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Default, Debug)]
pub struct Status { pub version: String, pub live: bool, pub pending: bool, pub previous: String, pub available: String, pub token: bool, pub good_after: String }

pub fn parse_status(t: &str) -> Status {
    let mut s = Status::default();
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        match k { "version" => s.version = v.into(), "live" => s.live = v == "yes", "pending" => s.pending = v == "yes", "previous" => s.previous = if v == "-" { String::new() } else { v.into() }, "available" => s.available = if v == "-" { String::new() } else { v.into() }, "token" => s.token = v == "yes", "good_after" => s.good_after = v.into(), _ => {} }
    }
    s
}

/// Update packages on inserted drives (/media/<drive>/exec85-update*.tar.gz), best name last.
pub fn usb_packages() -> Vec<String> {
    let mut v = vec![];
    if let Ok(d) = std::fs::read_dir("/media") {
        for e in d.flatten() {
            if let Ok(f) = std::fs::read_dir(e.path()) {
                for x in f.flatten() { let n = x.file_name().to_string_lossy().to_string(); if n.starts_with("exec85-update") && n.ends_with(".tar.gz") { v.push(x.path().to_string_lossy().to_string()); } }
            }
        }
    }
    v.sort(); v
}

pub struct Updates {
    be: Backend,
    job: Job,
    st: Option<Status>,
    msg: String,
    token_entry: Option<String>,
    first: bool,
    asked: Option<std::time::Instant>,
    packages: fn() -> Vec<String>,
}

impl Updates {
    pub fn new(be: Backend) -> Updates { Updates::with_packages(be, usb_packages) }
    pub fn with_packages(be: Backend, packages: fn() -> Vec<String>) -> Updates { Updates { be, job: Job::new(), st: None, msg: String::new(), token_entry: None, first: true, asked: None, packages } }

    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) }
    fn refresh(&mut self, env: &Env) { self.asked = Some(std::time::Instant::now()); self.ask(env, "status", &["update-status"]); }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("status", Ok(t)) => self.st = Some(parse_status(&t)),
            ("status", Err(e)) => self.msg = e,
            (_, Ok(m)) => { self.msg = m; self.refresh(env); }
            (_, Err(e)) => { self.msg = e; self.refresh(env); }
        }
    }
}

impl Application for Updates {
    fn kind(&self) -> Kind { Kind::Updates }
    fn title(&self) -> String { "Updates".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if !self.job.busy && (self.first || self.asked.map_or(true, |t| t.elapsed() >= Duration::from_secs(5))) { self.first = false; self.refresh(env); }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        if let Some(buf) = self.token_entry.as_mut() {
            if k.code == code::ESC { self.token_entry = None; }
            else if k.code == code::ENTER {
                let t = self.token_entry.take().unwrap_or_default();
                if self.ask(env, "token", &["update-config", "token", if t.is_empty() { "-" } else { &t }]) { self.msg = "Saving ...".into(); }
            } else { edit_text(buf, &k, 120); }
            return Out::None;
        }
        let Some(st) = self.st.clone() else { return Out::None };
        if st.live { return Out::None; }
        if k.code == code::ESC { self.msg.clear(); return Out::None; }
        let c = k.ch().map(|c| c.to_ascii_lowercase());
        let busy = |s: &mut Updates| s.msg = "Please wait, still busy.".into();
        match c {
            Some('c') => { if self.ask(env, "check", &["update-check"]) { self.msg = "Looking for a newer version ...".into(); } else { busy(self); } }
            Some('u') => { if self.ask(env, "install", &["update-install"]) { self.msg = "Downloading and installing the update ...".into(); } else { busy(self); } }
            Some('p') => match (self.packages)().last().cloned() {
                Some(p) => { if self.ask(env, "apply", &["update-apply", &p]) { self.msg = "Installing the update from the drive ...".into(); } else { busy(self); } }
                None => self.msg = "No update file found. Put exec85-update-....tar.gz on a USB drive and plug it in.".into(),
            },
            Some('t') => self.token_entry = Some(String::new()),
            Some('r') => { if st.previous.is_empty() { self.msg = "There is no previous version saved.".into(); } else if self.ask(env, "rollback", &["update-rollback"]) { self.msg = "Going back ...".into(); } else { busy(self); } }
            Some('k') => { if st.pending { if self.ask(env, "good", &["update-good"]) { self.msg = "Marked as good.".into(); } } }
            _ => {}
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Updates", t.accent, None);
        let mut y = a.y + 2;
        let mut para = |p: &mut Painter, text: &str, color: u32, y: &mut usize| { for l in wrap_text(text, a.w) { if *y < foot { p.clip(a.x, *y, a.w, &l, color, t.panel); *y += 1; } } };
        let mut hint = String::new();
        match &self.st {
            None => para(p, "Looking ...", t.dim, &mut y),
            Some(s) if s.live => para(p, "This is the install disc, not an installed system. Install first; updates are for installed systems.", t.text, &mut y),
            Some(s) => {
                para(p, &format!("Installed version: {}", s.version), t.text, &mut y);
                if !s.available.is_empty() { para(p, &format!("A newer version is ready to install: {}", s.available), t.accent, &mut y); }
                if !s.previous.is_empty() { para(p, &format!("Previous version kept: {}", s.previous), t.dim, &mut y); }
                if s.pending { para(p, &format!("Trial: the new version becomes final after {} s of running well. Until then the boot menu offers the previous version.", s.good_after), t.accent, &mut y); }
                y += 1;
                para(p, "An update replaces only Exec85's own files. Your settings, accounts, services, firewall, game servers and files are not touched, and nothing is reinstalled. Restart afterwards (File, Restart) to run the new version.", t.dim, &mut y);
                y += 1;
                para(p, if s.token { "GitHub access key: set." } else { "GitHub access key: not set (needed to download updates, because the project is private)." }, t.dim, &mut y);
                hint = "C Check  U Install online  P From USB  T Key  R Previous  K Keep".into();
            }
        }
        let line = if let Some(b) = &self.token_entry { format!("Paste the GitHub key, Enter to save, Esc to cancel: {}", "*".repeat(b.chars().count().min(40))) } else if self.msg.is_empty() { hint } else { self.msg.clone() };
        p.clip(a.x, foot, a.w, &line, if self.token_entry.is_some() || !self.msg.is_empty() { t.accent } else { t.dim }, t.panel);
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let (v, pend, prev, av, tok, live) = match &self.st { Some(s) => (s.version.clone(), s.pending.to_string(), s.previous.clone(), s.available.clone(), s.token.to_string(), s.live.to_string()), None => (String::new(), "?".into(), String::new(), String::new(), "?".into(), "?".into()) };
        format!("name=updates version={} live={live} pending={pend} previous={} available={} token={tok} entry={} busy={} msg={}", n(&v), n(&prev), n(&av), self.token_entry.is_some(), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "C Check   U Install   P USB   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    type St = Arc<Mutex<(String, Vec<String>)>>;
    fn none() -> Vec<String> { vec![] }
    fn one() -> Vec<String> { vec!["/media/X/exec85-update-v9.tar.gz".into()] }
    fn app(pk: fn() -> Vec<String>) -> (Updates, St) {
        let st: St = Arc::new(Mutex::new(("version=0.9.3\nlive=no\npending=no\nprevious=-\navailable=-\ntoken=no\nrepo=x\ngood_after=120\n".to_string(), vec![])));
        let s2 = st.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            let mut g = s2.lock().unwrap(); g.1.push(r.join(" "));
            match r[0].as_str() {
                "update-status" => Ok(g.0.clone()),
                "update-apply" => { g.0 = "version=0.9.4\nlive=no\npending=yes\nprevious=0.9.3\navailable=-\ntoken=no\nrepo=x\ngood_after=120\n".into(); Ok("Updated to 0.9.4. Restart to use it.".into()) }
                _ => Ok("ok".into()),
            }
        });
        (Updates::with_packages(be, pk), st)
    }
    fn settle(m: &mut Updates) { let e = Env::for_tests(); for _ in 0..300 { m.tick(&e); if !m.job.busy { std::thread::sleep(Duration::from_millis(5)); m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Updates, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }

    #[test]
    fn it_applies_a_package_from_a_drive_and_shows_the_trial() {
        let (mut m, st) = app(one); settle(&mut m);
        assert!(m.state().contains("version=0.9.3") && m.state().contains("pending=false"), "{}", m.state());
        key(&mut m, "r"); assert!(!st.lock().unwrap().1.iter().any(|c| c == "update-rollback"), "nothing to go back to");
        key(&mut m, "p"); assert!(st.lock().unwrap().1.contains(&"update-apply /media/X/exec85-update-v9.tar.gz".to_string()));
        m.asked = None; settle(&mut m);
        assert!(m.state().contains("version=0.9.4") && m.state().contains("pending=true") && m.state().contains("previous=0.9.3"), "{}", m.state());
        key(&mut m, "r"); assert!(st.lock().unwrap().1.contains(&"update-rollback".to_string()));
    }

    #[test]
    fn it_says_so_when_no_drive_has_a_package_and_keeps_the_token_secret() {
        let (mut m, st) = app(none); settle(&mut m);
        key(&mut m, "p"); assert!(m.msg.contains("No update file"), "{}", m.msg);
        key(&mut m, "t"); assert!(m.state().contains("entry=true"));
        for ch in ["a", "b", "c"] { key(&mut m, ch); }
        key(&mut m, "enter");
        assert!(st.lock().unwrap().1.contains(&"update-config token abc".to_string()), "{:?}", st.lock().unwrap().1);
        assert!(!m.state().contains("abc"), "the key never shows in the state line");
    }

    #[test]
    fn on_the_install_disc_nothing_happens_and_every_screen_draws() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut m, st) = app(one); settle(&mut m);
        for step in 0..3 {
            match step { 1 => { st.lock().unwrap().0 = "version=0.9.4\nlive=no\npending=yes\nprevious=0.9.3\navailable=0.9.5\ntoken=yes\nrepo=x\ngood_after=120\n".into(); m.asked = None; settle(&mut m); } 2 => { st.lock().unwrap().0 = "version=0.9.3\nlive=yes\npending=no\nprevious=-\navailable=-\ntoken=no\nrepo=x\ngood_after=120\n".into(); m.asked = None; settle(&mut m); key(&mut m, "u"); assert!(!st.lock().unwrap().1.contains(&"update-install".to_string())); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
