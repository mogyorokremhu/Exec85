//! Internet tunnel (playit.gg): for a home connection behind CGNAT, where ports cannot be forwarded.
//! One screen that says where you are (not installed, waiting for you to claim it, connected) and what to do next.
use super::{wrap_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use crate::tunnel::{self, Info};
use std::time::Duration;

pub struct Tunnel {
    be: Backend,
    job: Job,
    info: Option<Info>,
    msg: String,
    confirm: Option<&'static str>,
    first: bool,
    asked: Option<std::time::Instant>,
}

impl Tunnel {
    pub fn new(be: Backend) -> Tunnel { Tunnel { be, job: Job::new(), info: None, msg: String::new(), confirm: None, first: true, asked: None } }

    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) -> bool { self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) }

    fn refresh(&mut self, env: &Env) { self.asked = Some(std::time::Instant::now()); self.ask(env, "status", &["tunnel-status"]); }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("status", Ok(t)) => self.info = Some(tunnel::parse_status(&t)),
            ("status", Err(e)) => self.msg = e,
            ("claim", Ok(u)) => { self.msg = format!("Open this link: {u}"); self.refresh(env); }
            (_, Ok(m)) => { self.msg = m; self.refresh(env); }
            (_, Err(e)) => { self.msg = e; self.refresh(env); }
        }
    }
}

impl Application for Tunnel {
    fn kind(&self) -> Kind { Kind::Tunnel }
    fn title(&self) -> String { "Internet tunnel".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(3) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first || (!self.job.busy && self.asked.map_or(true, |t| t.elapsed() >= Duration::from_secs(3))) { self.first = false; if !self.job.busy { self.refresh(env); } }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        if let Some(what) = self.confirm {
            match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => { self.confirm = None; let a = if what == "reset" { "tunnel-reset" } else { "tunnel-remove" }; if self.ask(env, what, &[a]) { self.msg = "Working ...".into(); } }
                _ => { self.confirm = None; self.msg.clear(); }
            }
            return Out::None;
        }
        let Some(info) = self.info.clone() else { return Out::None };
        let c = k.ch().map(|c| c.to_ascii_lowercase());
        if k.code == code::ESC { self.msg.clear(); return Out::None; }
        if !info.installed {
            if c == Some('i') { if self.ask(env, "install", &["tunnel-install"]) { self.msg = "Downloading the tunnel program (about 15 MB) ...".into(); } else { self.msg = "Please wait, still busy.".into(); } }
        } else if !info.secret {
            match c {
                Some('c') => { if self.ask(env, "claim", &["tunnel-claim"]) { self.msg = "Asking playit.gg for a link ...".into(); } else { self.msg = "Please wait, still busy.".into(); } }
                Some('x') => self.confirm = Some("remove"),
                _ => {}
            }
        } else {
            match c {
                Some('s') => { if self.ask(env, "act", &["svc-start", tunnel::SERVICE]) { self.msg = "Starting ...".into(); } }
                Some('t') => { if self.ask(env, "act", &["svc-stop", tunnel::SERVICE]) { self.msg = "Stopping ...".into(); } }
                Some('r') => { if self.ask(env, "act", &["svc-restart", tunnel::SERVICE]) { self.msg = "Restarting ...".into(); } }
                Some('n') => self.confirm = Some("reset"),
                Some('x') => self.confirm = Some("remove"),
                _ => {}
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Internet tunnel (playit.gg)", t.accent, None);
        let mut y = a.y + 2;
        let mut para = |p: &mut Painter, text: &str, color: u32, y: &mut usize| { for l in wrap_text(text, a.w) { if *y < foot.saturating_sub(1) { p.clip(a.x, *y, a.w, &l, color, t.panel); *y += 1; } } *y += 1; };
        let hint;
        match &self.info {
            None => { para(p, "Looking ...", t.dim, &mut y); hint = "".to_string(); }
            Some(i) if !i.installed => {
                para(p, "Your internet provider shares one address between many customers (CGNAT), so players cannot reach a server at home and you cannot forward a port. This program connects OUT to playit.gg, and playit.gg gives you a public address that leads here. No router settings, no call to your provider, nothing to open in the firewall.", t.text, &mut y);
                para(p, "It needs a free playit.gg account (made in your browser, on any device). The program is downloaded from the playit.gg project on GitHub (BSD licence), about 15 MB, and checked before it is installed. Players' game traffic goes through playit.gg's servers.", t.dim, &mut y);
                hint = "I Install".to_string();
            }
            Some(i) if !i.secret => {
                para(p, &format!("Installed. The tunnel program is {}.", i.service), t.text, &mut y);
                match &i.claim {
                    Some(u) => { para(p, "Step 1. On your phone or computer, open this link and log in or sign up (it is free):", t.text, &mut y); para(p, u, t.accent, &mut y); para(p, "Step 2. When you have finished there, this screen changes by itself.", t.dim, &mut y); }
                    None => para(p, "Press C to get your claim link.", t.accent, &mut y),
                }
                hint = "C New claim link   X Remove".to_string();
            }
            Some(i) => {
                para(p, &format!("Connected to your playit.gg account. The tunnel program is {}.", i.service), t.text, &mut y);
                para(p, "Now make the tunnel on playit.gg (on your phone or computer): sign in at playit.gg, Add Tunnel, choose \"Minecraft Java\", local port 25565, and pick a region in Europe if you are offered a choice. Players join with the address playit.gg shows you.", t.dim, &mut y);
                for l in i.agent.iter().take(8) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, l, t.text, t.panel); y += 1; } }
                hint = "S Start  T Stop  R Restart  N Another account  X Remove".to_string();
            }
        }
        let line = match self.confirm { Some("reset") => "Forget this playit.gg account? [Y] Yes  [N] No".to_string(), Some(_) => "Remove the internet tunnel? Its account key is kept.  [Y] Yes  [N] No".to_string(), None => if self.msg.is_empty() { hint } else { self.msg.clone() } };
        p.clip(a.x, foot, a.w, &line, if self.confirm.is_some() || !self.msg.is_empty() { t.accent } else { t.dim }, t.panel);
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let (inst, svc, sec, claim) = match &self.info { Some(i) => (i.installed.to_string(), i.service.clone(), i.secret.to_string(), i.claim.is_some().to_string()), None => ("?".into(), String::new(), "?".into(), "?".into()) };
        format!("name=tunnel installed={inst} service={} secret={sec} claim={claim} confirm={} busy={} msg={}", n(&svc), self.confirm.unwrap_or("-"), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "I Install   C Claim link   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    type St = Arc<Mutex<(String, Vec<String>)>>;
    fn app() -> (Tunnel, St) {
        let st: St = Arc::new(Mutex::new(("installed=no\nservice=none\nsecret=no\nclaim=-\n---\n".to_string(), vec![])));
        let s2 = st.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            let mut g = s2.lock().unwrap(); g.1.push(r.join(" "));
            match r[0].as_str() {
                "tunnel-status" => Ok(g.0.clone()),
                "tunnel-install" => { g.0 = "installed=yes\nservice=running\nsecret=no\nclaim=-\n---\n".into(); Ok("Installed.".into()) }
                "tunnel-claim" => { g.0 = "installed=yes\nservice=running\nsecret=no\nclaim=https://playit.gg/claim/abc123\n---\n".into(); Ok("https://playit.gg/claim/abc123".into()) }
                "tunnel-remove" => { g.0 = "installed=no\nservice=none\nsecret=no\nclaim=-\n---\n".into(); Ok("Removed.".into()) }
                "tunnel-reset" => { g.0 = "installed=yes\nservice=running\nsecret=no\nclaim=-\n---\n".into(); Ok("Forgotten.".into()) }
                _ => Ok(String::new()),
            }
        });
        (Tunnel::new(be), st)
    }
    fn settle(m: &mut Tunnel) { let e = Env::for_tests(); for _ in 0..300 { m.tick(&e); if !m.job.busy { std::thread::sleep(Duration::from_millis(5)); m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Tunnel, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }

    #[test]
    fn it_walks_from_not_installed_to_a_claim_link_and_back() {
        let (mut m, st) = app(); settle(&mut m);
        assert!(m.state().contains("installed=false"), "{}", m.state());
        key(&mut m, "c"); assert!(!st.lock().unwrap().1.iter().any(|c| c == "tunnel-claim"), "nothing to claim before it is installed");
        key(&mut m, "i"); assert!(st.lock().unwrap().1.contains(&"tunnel-install".to_string()));
        assert!(m.state().contains("installed=true") && m.state().contains("claim=false"), "{}", m.state());
        key(&mut m, "c"); assert!(m.state().contains("claim=true") && m.msg.contains("https://playit.gg/claim/abc123"), "{} / {}", m.state(), m.msg);
        key(&mut m, "x"); assert!(m.state().contains("confirm=remove"));
        key(&mut m, "n"); assert!(m.state().contains("confirm=-") && m.state().contains("installed=true"), "No does nothing: {}", m.state());
        key(&mut m, "x"); key(&mut m, "y"); assert!(st.lock().unwrap().1.contains(&"tunnel-remove".to_string()) && m.state().contains("installed=false"), "{}", m.state());
    }

    #[test]
    fn once_connected_it_starts_stops_and_can_forget_the_account() {
        let (mut m, st) = app(); settle(&mut m);
        st.lock().unwrap().0 = "installed=yes\nservice=running\nsecret=yes\nclaim=-\n---\nPhase: running\nTunnels: 1\n".into();
        m.asked = None; settle(&mut m);          // the screen asks again by itself every few seconds
        assert!(m.state().contains("secret=true"), "{}", m.state());
        for (k, want) in [("s", "svc-start playit"), ("t", "svc-stop playit"), ("r", "svc-restart playit")] { key(&mut m, k); assert!(st.lock().unwrap().1.contains(&want.to_string()), "{want}"); }
        assert_eq!(m.info.as_ref().unwrap().agent, vec!["Phase: running".to_string(), "Tunnels: 1".to_string()]);
        key(&mut m, "n"); key(&mut m, "y"); assert!(st.lock().unwrap().1.contains(&"tunnel-reset".to_string()));
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut m, st) = app(); settle(&mut m);
        for step in 0..4 {
            match step { 1 => key(&mut m, "i"), 2 => key(&mut m, "c"), 3 => { st.lock().unwrap().0 = "installed=yes\nservice=running\nsecret=yes\nclaim=-\n---\nPhase: running\n".into(); m.asked = None; settle(&mut m); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
