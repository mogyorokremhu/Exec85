//! Remote access: the SSH server and the firewall in one place. Nothing here is reachable from outside the home
//! network unless you choose that on purpose; passwords for SSH are off unless you turn them on.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Ssh { pub enabled: bool, pub port: u16, pub password: bool, pub keys: Vec<String> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FwRule { pub name: String, pub port: String, pub proto: String, pub scope: String, pub from: String }

pub fn parse_ssh(text: &str) -> Ssh {
    let mut s = Ssh { port: 22, ..Default::default() };
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["enabled", v] => s.enabled = *v == "yes",
            ["port", p] => s.port = p.parse().unwrap_or(22),
            ["password", v] => s.password = *v == "yes",
            ["key", user, typ, comment] => s.keys.push(format!("{user}: {typ} {}", comment.replace('_', " "))),
            _ => {}
        }
    }
    s
}

pub fn parse_fw(text: &str) -> (bool, Vec<FwRule>) {
    let mut on = true; let mut rules = Vec::new();
    for l in text.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        match w.as_slice() {
            ["enabled", v] => on = *v == "yes",
            ["rule", name, port, proto, scope, from] => rules.push(FwRule { name: name.to_string(), port: port.to_string(), proto: proto.to_string(), scope: scope.to_string(), from: from.to_string() }),
            _ => {}
        }
    }
    (on, rules)
}

/// "web 443/tcp anywhere" or "8080" -> the words for `fw-allow` (name, port, scope). The name defaults to `port-<number>`.
pub fn parse_rule_text(t: &str) -> Result<(String, String, String), String> {
    let w: Vec<&str> = t.split_whitespace().collect();
    let (name, port, scope) = match w.as_slice() {
        [port] if port.chars().next().map_or(false, |c| c.is_ascii_digit()) => (format!("port-{}", port.split('/').next().unwrap_or("")), port.to_string(), "local".to_string()),
        [name, port] => (name.to_string(), port.to_string(), "local".to_string()),
        [name, port, scope] => (name.to_string(), port.to_string(), scope.to_string()),
        _ => return Err("Type: NAME PORT/tcp   (for example  web 8080/tcp  or  game 25565/udp anywhere)".into()),
    };
    crate::firewall::parse_port(&port)?;
    if !crate::firewall::valid_rule_name(&name) { return Err("The name may only have lowercase letters, digits, - and _.".into()); }
    if scope != "local" && scope != "anywhere" { return Err("The last word must be local or anywhere.".into()); }
    Ok((name, port, scope))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { List, Port(String), Key(String), Rule(String), Confirm(&'static str) }

pub struct Remote {
    be: Backend,
    job: Job,
    ssh: Ssh,
    fw_on: bool,
    rules: Vec<FwRule>,
    sel: usize,
    mode: Mode,
    msg: String,
    addr: Option<String>,
    first: bool,
}

impl Remote {
    pub fn new(be: Backend) -> Remote { Remote { be, job: Job::new(), ssh: Ssh { port: 22, ..Default::default() }, fw_on: true, rules: vec![], sel: 0, mode: Mode::List, msg: String::new(), addr: None, first: true } }

    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) {
        if !self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { self.msg = "Please wait, the last request is still running.".into(); }
    }
    fn refresh(&mut self, env: &Env) { self.ask(env, "ssh-status", &["ssh-status"]); }
    fn user() -> String { std::env::var("USER").unwrap_or_default() }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("ssh-status", Ok(t)) => { self.ssh = parse_ssh(&t); self.ask(env, "fw-status", &["fw-status"]); }
            ("fw-status", Ok(t)) => { let (on, rules) = parse_fw(&t); self.fw_on = on; self.rules = rules; self.sel = self.sel.min(self.rules.len().saturating_sub(1)); self.addr = super::network::local_ip(); }
            (_, Ok(m)) => { if !m.is_empty() { self.msg = m; } self.refresh(env); }
            (_, Err(e)) => self.msg = e,
        }
    }

    fn set_ssh(&mut self, env: &Env, enabled: bool, port: u16, password: bool) {
        if enabled { let u = Self::user(); self.ask(env, "ssh-enable", &["ssh-enable", &port.to_string(), if password { "1" } else { "0" }, &u]); self.msg = "Setting up the SSH server (this installs it the first time)...".into(); }
        else { self.ask(env, "ssh-disable", &["ssh-disable"]); }
    }
}

impl Application for Remote {
    fn kind(&self) -> Kind { Kind::Remote }
    fn title(&self) -> String { "Remote access".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(10) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first || (!self.job.busy && matches!(self.mode, Mode::List)) { self.first = false; if !self.job.busy { self.refresh(env); } }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Port(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => match t.trim().parse::<u16>() { Ok(p) if p > 0 => { self.mode = Mode::List; self.set_ssh(env, true, p, self.ssh.password); } _ => self.msg = "Type a port number between 1 and 65535.".into() },
                _ => { if k.ch().map_or(false, |c| c.is_ascii_digit()) || k.code == code::BACKSPACE { edit_text(&mut t, &k, 5); self.mode = Mode::Port(t); } }
            },
            Mode::Key(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => { match crate::ssh::valid_public_key(&t) { Ok(key) => { self.mode = Mode::List; let u = Self::user(); self.ask(env, "ssh-addkey", &["ssh-addkey", &u, &key]); } Err(e) => self.msg = e } }
                _ => { edit_text(&mut t, &k, 900); self.mode = Mode::Key(t); }
            },
            Mode::Rule(mut t) => match k.code {
                code::ESC => self.mode = Mode::List,
                code::ENTER => match parse_rule_text(&t) { Ok((n, p, s)) => { self.mode = Mode::List; self.ask(env, "fw-allow", &["fw-allow", &n, &p, &s]); } Err(e) => self.msg = e },
                _ => { edit_text(&mut t, &k, 80); self.mode = Mode::Rule(t); }
            },
            Mode::Confirm(what) => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => {
                    self.mode = Mode::List;
                    match what { "fw-off" => self.ask(env, "fw-enable", &["fw-enable", "off"]), "password" => self.set_ssh(env, true, self.ssh.port, !self.ssh.password), _ => {} }
                }
                Some('n') => self.mode = Mode::List,
                _ => if k.code == code::ESC { self.mode = Mode::List; },
            },
            Mode::List => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(self.rules.len().saturating_sub(1)),
                    code::F5 => self.refresh(env),
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('s') => { let on = !self.ssh.enabled; self.set_ssh(env, on, self.ssh.port, self.ssh.password); }
                        Some('c') => self.mode = Mode::Port(self.ssh.port.to_string()),
                        Some('p') => { if self.ssh.password { self.set_ssh(env, self.ssh.enabled, self.ssh.port, false); } else { self.mode = Mode::Confirm("password"); } }
                        Some('k') => self.mode = Mode::Key(String::new()),
                        Some('f') => { if self.fw_on { self.mode = Mode::Confirm("fw-off"); } else { self.ask(env, "fw-enable", &["fw-enable", "on"]); } }
                        Some('a') => self.mode = Mode::Rule(String::new()),
                        Some('d') => if let Some(r) = self.rules.get(self.sel).cloned() { if r.from == "manual" { self.ask(env, "fw-remove", &["fw-remove", &r.name]); } else { self.msg = format!("{} is opened by the {} service; change its ports in Services.", r.name, r.name); } },
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
        p.text(a.x, a.y, "SSH server", t.accent, None);
        let user = Self::user();
        let line = if self.ssh.enabled { format!("On   port {}   {}   {} key(s)", self.ssh.port, if self.ssh.password { "passwords ALLOWED" } else { "keys only" }, self.ssh.keys.len()) } else { "Off".to_string() };
        p.text(a.x + 12, a.y, &line, if self.ssh.enabled { t.text } else { t.dim }, None);
        if self.ssh.enabled { if let Some(ip) = &self.addr { p.clip(a.x, a.y + 1, a.w, &format!("Connect from another computer:  ssh {user}@{ip}{}", if self.ssh.port == 22 { String::new() } else { format!(" -p {}", self.ssh.port) }), t.dim, t.panel); } }
        p.text(a.x, a.y + 3, &format!("Firewall: {}", if self.fw_on { "ON  (incoming connections are refused except the ports below)" } else { "OFF  (everything is let in!)" }), if self.fw_on { t.text } else { t.accent }, None);
        let items: Vec<String> = self.rules.iter().map(|r| format!("{:<16} {:>5}/{:<4} {:<9} {}", r.name, r.port, r.proto, if r.scope == "local" { "home only" } else { "ANYWHERE" }, if r.from == "service" { "(opened by a service)" } else { "" })).collect();
        if items.is_empty() { p.text(a.x, a.y + 5, "No ports are open.", t.dim, None); }
        let h = a.h.saturating_sub(10).max(1);
        p.list(Rect { x: a.x, y: a.y + 5, w: a.w, h }, &items, self.sel, self.sel.saturating_sub(h - 1), focused && matches!(self.mode, Mode::List));
        let prompt_row = foot.saturating_sub(2);
        match &self.mode {
            Mode::Port(v) => { p.text(a.x, prompt_row, "Port:", t.dim, None); p.field(a.x + 6, prompt_row, 8, v, true, false); }
            Mode::Key(v) => { p.text(a.x, prompt_row, "Public key:", t.dim, None); p.field(a.x + 12, prompt_row, a.w.saturating_sub(12), v, true, false); }
            Mode::Rule(v) => { p.text(a.x, prompt_row, "Open port:", t.dim, None); p.field(a.x + 11, prompt_row, a.w.saturating_sub(11).min(50), v, true, false); }
            Mode::Confirm(w) => p.clip(a.x, prompt_row, a.w, if *w == "fw-off" { "Turn the firewall OFF? Everything will be reachable.  [Y] Yes  [N] No" } else { "Allow password login? Keys are much safer.  [Y] Yes  [N] No" }, t.accent, t.panel),
            Mode::List => {}
        }
        let hint = match self.mode { Mode::List => "S SSH on/off  C Port  P Passwords  K Add key  F Firewall  A Open port  D Close port", Mode::Key(_) => "Paste the public key (the .pub file)   Enter Add   Esc Cancel", _ => "Enter OK   Esc Cancel" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::List => "list", Mode::Port(_) => "port", Mode::Key(_) => "key", Mode::Rule(_) => "rule", Mode::Confirm(_) => "confirm" };
        format!("name=remote mode={mode} ssh={} port={} password={} keys={} fw={} rules={} sel={} busy={} msg={}", if self.ssh.enabled { "on" } else { "off" }, self.ssh.port, self.ssh.password, self.ssh.keys.len(), if self.fw_on { "on" } else { "off" }, self.rules.len(), n(self.rules.get(self.sel).map_or("", |r| r.name.as_str())), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "S SSH  K Add key  F Firewall  A Open port  D Close port   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl laptop";

    fn app() -> (Remote, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() {
                "ssh-status" => Ok("enabled no\nport 22\npassword no\n".into()),
                "fw-status" => Ok("enabled yes\nrule ssh 22 tcp local manual\nrule minecraft 25565 tcp local service\n".into()),
                "fw-allow" if r[1] == "bad" => Err("nope".into()),
                _ => Ok(String::new()),
            }
        });
        (Remote::new(be), calls)
    }
    fn settle(m: &mut Remote) { let e = Env::for_tests(); for _ in 0..300 { m.tick(&e); if !m.job.busy { std::thread::sleep(Duration::from_millis(5)); m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Remote, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }
    fn text(m: &mut Remote, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn status_text_is_read() {
        let s = parse_ssh("enabled yes\nport 2222\npassword no\nkey tester ssh-ed25519 my_laptop\n");
        assert!(s.enabled && s.port == 2222 && !s.password && s.keys == vec!["tester: ssh-ed25519 my laptop"]);
        let (on, rules) = parse_fw("enabled no\nrule ssh 22 tcp local manual\n");
        assert!(!on && rules[0].name == "ssh" && rules[0].from == "manual");
        assert_eq!(parse_rule_text("8080").unwrap(), ("port-8080".into(), "8080".into(), "local".into()));
        assert_eq!(parse_rule_text("game 25565/udp anywhere").unwrap(), ("game".into(), "25565/udp".into(), "anywhere".into()));
        for bad in ["", "a b c d", "Bad 80", "web 0", "web 80 mars", "web x"] { assert!(parse_rule_text(bad).is_err(), "{bad:?}"); }
    }

    #[test]
    fn the_screen_shows_what_the_system_reports() {
        let (mut m, _c) = app(); settle(&mut m);
        assert!(m.state().contains("ssh=off") && m.state().contains("fw=on") && m.state().contains("rules=2") && m.state().contains("sel=ssh"), "{}", m.state());
    }

    #[test]
    fn ssh_is_switched_on_with_keys_only_and_a_key_can_be_added() {
        let (mut m, calls) = app(); settle(&mut m);
        key(&mut m, "s");
        assert!(calls.lock().unwrap().iter().any(|c| c.starts_with("ssh-enable 22 0 ")), "keys only unless passwords are chosen: {:?}", calls.lock().unwrap());
        key(&mut m, "k"); assert!(m.state().contains("mode=key"));
        text(&mut m, "not a key"); key(&mut m, "enter"); assert!(m.state().contains("mode=key") && m.msg.contains("PUBLIC key"), "{}", m.msg);
        for _ in 0..12 { key(&mut m, "backspace"); }
        m.mode = Mode::Key(KEY.to_string()); key(&mut m, "enter");
        assert!(calls.lock().unwrap().iter().any(|c| c.contains("ssh-addkey") && c.contains("AAAAC3NzaC1lZDI1NTE5")), "{:?}", calls.lock().unwrap());
    }

    #[test]
    fn passwords_and_switching_the_firewall_off_ask_first() {
        let (mut m, calls) = app(); settle(&mut m);
        key(&mut m, "p"); assert!(m.state().contains("mode=confirm"));
        key(&mut m, "n"); assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("ssh-enable")));
        key(&mut m, "p"); key(&mut m, "y"); assert!(calls.lock().unwrap().iter().any(|c| c.starts_with("ssh-enable 22 1 ")));
        key(&mut m, "f"); assert!(m.state().contains("mode=confirm"));
        key(&mut m, "y"); assert!(calls.lock().unwrap().contains(&"fw-enable off".to_string()));
    }

    #[test]
    fn ports_are_opened_and_closed_but_a_services_ports_belong_to_the_service() {
        let (mut m, calls) = app(); settle(&mut m);
        key(&mut m, "a"); text(&mut m, "web 8080"); key(&mut m, "enter");
        assert!(calls.lock().unwrap().contains(&"fw-allow web 8080 local".to_string()), "{:?}", calls.lock().unwrap());
        key(&mut m, "a"); text(&mut m, "Bad 1"); key(&mut m, "enter"); assert!(m.msg.contains("lowercase") && m.state().contains("mode=rule"), "{}", m.msg);
        key(&mut m, "esc");
        key(&mut m, "d"); assert!(calls.lock().unwrap().contains(&"fw-remove ssh".to_string()));
        key(&mut m, "down"); key(&mut m, "d"); assert!(m.msg.contains("opened by the minecraft service"), "{}", m.msg);
    }

    #[test]
    fn it_draws_every_mode_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut m, _c) = app(); settle(&mut m);
        for step in 0..5 {
            match step { 1 => key(&mut m, "k"), 2 => { key(&mut m, "esc"); key(&mut m, "a"); } 3 => { key(&mut m, "esc"); key(&mut m, "c"); } 4 => { key(&mut m, "esc"); key(&mut m, "f"); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 14), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
