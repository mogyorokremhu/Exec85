//! Tailscale: a private network between your own devices. Install it, sign in, and reach this server from anywhere.
use super::{wrap_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::Key;
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Info { pub installed: bool, pub running: bool, pub state: String, pub address: String, pub login: String, pub name: String }

pub fn parse_status(t: &str) -> Info {
    let mut i = Info::default();
    for l in t.lines() {
        let Some((k, v)) = l.split_once('=') else { continue };
        let v = v.trim();
        let dash = |s: &str| if s == "-" { String::new() } else { s.to_string() };
        match k { "installed" => i.installed = v == "yes", "running" => i.running = v == "yes", "state" => i.state = dash(v), "address" => i.address = dash(v), "login" => i.login = dash(v), "name" => i.name = dash(v), _ => {} }
    }
    i
}

pub struct Tailscale { be: Backend, job: Job, info: Info, msg: String, first: bool }

impl Tailscale {
    pub fn new(be: Backend) -> Tailscale { Tailscale { be, job: Job::new(), info: Info::default(), msg: String::new(), first: true } }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { if self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { true } else { self.msg = "Please wait, the last request is still running.".into(); false } }
    fn refresh(&mut self, env: &Env) { self.ask(env, "status", &["ts-status"]); }
    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("status", Ok(t)) => self.info = parse_status(&t),
            ("status", Err(e)) => self.msg = e,
            (_, Ok(m)) => { if !m.is_empty() { self.msg = m; } self.refresh(env); }
            (_, Err(e)) => { self.msg = e; self.refresh(env); }
        }
    }
}

impl Application for Tailscale {
    fn kind(&self) -> Kind { Kind::Tailscale }
    fn title(&self) -> String { "Tailscale".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, env: &Env) { self.finish(env); if !self.job.busy && self.first { self.first = false; self.refresh(env); } else if !self.job.busy && self.info.installed && self.info.state != "Running" { self.refresh(env); } }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        self.msg.clear();
        match k.ch().map(|c| c.to_ascii_lowercase()) {
            Some('i') => { if self.ask(env, "install", &["ts-install"]) { self.msg = "Installing (it downloads the program the first time) ...".into(); } }
            Some('l') => { if self.ask(env, "login", &["ts-login"]) { self.msg = "Starting the sign-in ...".into(); } }
            Some('o') => { self.ask(env, "logout", &["ts-logout"]); }
            Some('x') => { self.ask(env, "remove", &["ts-remove"]); }
            Some('r') => self.refresh(env),
            _ => {}
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Tailscale: a private network for your own devices", t.accent, None);
        let (word, color) = if !self.info.installed { ("not installed (press I)", t.accent) } else if !self.info.running { ("installed, switched off (press I)", t.accent) } else {
            match self.info.state.as_str() { "Running" => ("connected", t.text), "NeedsLogin" => ("waiting for you to sign in (press L)", t.accent), "Stopped" => ("stopped", t.accent), "" => ("starting ...", t.dim), s => (s, t.accent) }
        };
        let mut y = a.y + 2;
        let mut line = |p: &mut Painter, label: &str, text: &str, color: u32, y: &mut usize| { if *y < foot.saturating_sub(2) { p.text(a.x, *y, label, t.dim, None); p.clip(a.x + 12, *y, a.w.saturating_sub(12), text, color, t.panel); *y += 1; } };
        line(p, "State", word, color, &mut y);
        if !self.info.name.is_empty() { line(p, "Name", &self.info.name, t.text, &mut y); }
        if !self.info.address.is_empty() { line(p, "Address", &self.info.address, t.accent, &mut y); }
        y += 1;
        if !self.info.login.is_empty() && self.info.state == "NeedsLogin" {
            for l in wrap_text(&format!("Open this address on your phone or PC and sign in: {}", self.info.login), a.w) { if y < foot.saturating_sub(2) { p.text(a.x, y, &l, t.text, None); y += 1; } }
            y += 1;
        }
        let help = if self.info.state == "Running" { "From any of your devices that have Tailscale (and are signed in to the same account), open https://<address above>:8443 for the screen in a browser, or reach the files and the Minecraft server the same way. Nothing is opened to the internet. Everybody on your Tailscale network gets the same access as your home network." }
            else { "Tailscale links your phone, PC and this server into one private network, wherever you are. Press I to install, L to sign in (you open an address on any device with a browser), then install the free Tailscale app on your phone or PC and sign in with the same account." };
        for l in wrap_text(help, a.w) { if y < foot.saturating_sub(2) { p.text(a.x, y, &l, t.dim, None); y += 1; } }
        p.clip(a.x, foot, a.w, "I Install  L Sign in  O Sign out  X Switch off  R Refresh", t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        format!("name=tailscale installed={} running={} state={} address={} login={} busy={} msg={}", self.info.installed, self.info.running, n(&self.info.state), n(&self.info.address), !self.info.login.is_empty(), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "I Install  L Sign in  O Sign out  X Switch off   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const STATUS: &str = "installed=yes\nrunning=yes\nstate=NeedsLogin\naddress=-\nlogin=https://login.tailscale.com/a/abc\nname=box\n";

    fn app() -> (Tailscale, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); match r[0].as_str() { "ts-status" => Ok(STATUS.into()), _ => Ok("Done.".into()) } });
        (Tailscale::new(be), calls)
    }
    fn press(m: &mut Tailscale, calls: &Arc<Mutex<Vec<String>>>, k: &str, want: &str) {
        let e = Env::for_tests();
        for _ in 0..200 { m.handle(parse_key(k).unwrap(), &e); for _ in 0..10 { m.tick(&e); std::thread::sleep(Duration::from_millis(2)); } if want.is_empty() || calls.lock().unwrap().iter().any(|c| c == want) { return; } }
        panic!("{want} never asked: {:?}", calls.lock().unwrap());
    }

    #[test]
    fn the_status_text_is_read() {
        let i = parse_status(STATUS);
        assert_eq!(i, Info { installed: true, running: true, state: "NeedsLogin".into(), address: String::new(), login: "https://login.tailscale.com/a/abc".into(), name: "box".into() });
    }

    #[test]
    fn the_keys_install_sign_in_and_out_and_switch_off() {
        let (mut m, calls) = app(); let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        assert!(m.state().contains("installed=true") && m.state().contains("state=NeedsLogin"), "{}", m.state());
        press(&mut m, &calls, "i", "ts-install"); press(&mut m, &calls, "l", "ts-login"); press(&mut m, &calls, "o", "ts-logout"); press(&mut m, &calls, "x", "ts-remove");
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut m, _calls) = app(); let e = Env::for_tests();
        for _ in 0..30 { m.tick(&e); std::thread::sleep(Duration::from_millis(3)); }
        for (w, h) in [(8usize, 4usize), (50, 14), (110, 36)] { let mut c = Canvas::new(1280, 800); let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) }; m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true); }
    }
}
