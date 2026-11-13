//! Minecraft server: pick a version, pick 2, 4 or 8 GB of memory, tick the EULA box, install. It talks to the
//! privileged helper, which downloads the server from Mojang, installs the right Java and defines a service.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::mc::ram_choices;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step { Versions, Ram, Options, Working, Done }

pub struct Minecraft {
    be: Backend,
    job: Job,
    step: Step,
    latest: String,
    versions: Vec<String>,
    sel: usize,
    mem_kb: u64,
    ram: u32,
    name: String,
    eula: bool,
    focus: usize, // on the options screen: 0 name, 1 EULA box, 2 install
    msg: String,
    first: bool,
    /// This computer's address on the local network, for telling players where to connect.
    addr: Option<String>,
}

pub fn parse_versions(text: &str) -> (String, Vec<String>) {
    let latest = text.lines().find_map(|l| l.strip_prefix("latest ")).unwrap_or("").to_string();
    (latest, text.lines().filter_map(|l| l.strip_prefix("release ")).map(String::from).collect())
}

impl Minecraft {
    pub fn new(be: Backend, mem_kb: u64) -> Minecraft {
        let ram = ram_choices(mem_kb).first().copied().unwrap_or(2);
        Minecraft { be, job: Job::new(), step: Step::Versions, latest: String::new(), versions: vec![], sel: 0, mem_kb, ram, name: "minecraft".into(), eula: false, focus: 0, msg: String::new(), first: true, addr: None }
    }

    pub fn system_memory_kb() -> u64 {
        std::fs::read_to_string("/proc/meminfo").unwrap_or_default().lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn version(&self) -> &str { self.versions.get(self.sel).map_or("", String::as_str) }

    fn finish(&mut self) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("versions", Ok(t)) => { let (l, v) = parse_versions(&t); self.latest = l; self.versions = v; self.sel = 0; self.msg.clear(); }
            ("versions", Err(e)) => self.msg = e,
            ("create", Ok(m)) => { self.step = Step::Done; self.msg = m; self.addr = super::network::local_ip(); }
            ("create", Err(e)) => { self.step = Step::Options; self.msg = e; }
            _ => {}
        }
    }

    fn install(&mut self, env: &Env) {
        let user = std::env::var("USER").unwrap_or_default();
        let words = ["mc-create", &self.name, self.version(), &self.ram.to_string(), if self.eula { "1" } else { "0" }, &user].iter().map(|s| s.to_string()).collect();
        if self.job.start(env, &self.be, "create", words) { self.step = Step::Working; self.msg = "Downloading and installing. This can take a few minutes...".into(); }
    }
}

impl Application for Minecraft {
    fn kind(&self) -> Kind { Kind::Minecraft }
    fn title(&self) -> String { "Minecraft server".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(60) }) }
    fn tick(&mut self, env: &Env) {
        self.finish();
        if self.first { self.first = false; self.job.start(env, &self.be, "versions", vec!["mc-versions".into()]); self.msg = "Loading the version list from Mojang...".into(); }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish();
        match self.step {
            Step::Versions => match k.code {
                code::UP => self.sel = self.sel.saturating_sub(1),
                code::DOWN => self.sel = (self.sel + 1).min(self.versions.len().saturating_sub(1)),
                code::PGUP => self.sel = self.sel.saturating_sub(10),
                code::PGDN => self.sel = (self.sel + 10).min(self.versions.len().saturating_sub(1)),
                code::HOME => self.sel = 0,
                code::ENTER => {
                    if self.versions.is_empty() { self.job.start(env, &self.be, "versions", vec!["mc-versions".into()]); self.msg = "Loading the version list...".into(); }
                    else if ram_choices(self.mem_kb).is_empty() { self.msg = "This computer has too little memory to run a Minecraft server (it needs at least about 2.5 GB).".into(); }
                    else { self.step = Step::Ram; self.msg.clear(); }
                }
                _ => {}
            },
            Step::Ram => {
                let ok = ram_choices(self.mem_kb);
                match k.code {
                    code::LEFT | code::UP => if let Some(p) = ok.iter().rev().find(|g| **g < self.ram) { self.ram = *p; },
                    code::RIGHT | code::DOWN => if let Some(n) = ok.iter().find(|g| **g > self.ram) { self.ram = *n; },
                    code::ENTER => { if !ok.contains(&self.ram) { self.ram = ok.first().copied().unwrap_or(2); } self.step = Step::Options; self.focus = 0; }
                    code::ESC | code::BACKSPACE => self.step = Step::Versions,
                    _ => match k.ch() { Some(c @ ('2' | '4' | '8')) => { let g = c.to_digit(10).unwrap(); if ok.contains(&g) { self.ram = g; } else { self.msg = format!("{g} GB does not fit this computer."); } } _ => {} },
                }
            }
            Step::Options => match k.code {
                code::ESC => self.step = Step::Ram,
                code::TAB | code::DOWN => self.focus = (self.focus + 1) % 3,
                code::UP => self.focus = (self.focus + 2) % 3,
                code::ENTER => { if self.focus < 2 { self.focus += 1; } else if crate::services::valid_name(&self.name).is_err() { self.msg = "The name may only have lowercase letters, digits, - and _.".into(); self.focus = 0; } else { self.install(env); } }
                _ => {
                    if self.focus == 1 { if k.ch() == Some(' ') { self.eula = !self.eula; } }
                    else if self.focus == 0 { edit_text(&mut self.name, &k, 32); }
                }
            },
            Step::Working => {}
            Step::Done => if k.code == code::ENTER || k.code == code::ESC { self.step = Step::Versions; self.msg.clear(); },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Minecraft server", t.accent, None);
        let steps = ["1 Version", "2 Memory", "3 Install"];
        let cur = match self.step { Step::Versions => 0, Step::Ram => 1, _ => 2 };
        let mut x = a.x + 18;
        for (i, s) in steps.iter().enumerate() { if x + s.len() < a.x + a.w { p.text(x, a.y, s, if i == cur { t.accent } else { t.dim }, None); } x += s.len() + 2; }
        let body = Rect { x: a.x, y: a.y + 2, w: a.w, h: a.h.saturating_sub(4) };
        match self.step {
            Step::Versions => {
                if self.versions.is_empty() { p.text(a.x, a.y + 2, if self.job.busy { "Loading the version list from Mojang..." } else { "No list yet. Press Enter to try again." }, t.dim, None); }
                else {
                    p.text(a.x, a.y + 2, &format!("Choose a version (the newest is {}):", self.latest), t.dim, None);
                    let items: Vec<String> = self.versions.iter().map(|v| if *v == self.latest { format!("{v}   (latest)") } else { v.clone() }).collect();
                    let r = Rect { x: a.x, y: a.y + 3, w: a.w, h: body.h.saturating_sub(1) };
                    p.list(r, &items, self.sel, self.sel.saturating_sub(r.h.saturating_sub(1)), focused);
                }
            }
            Step::Ram => {
                p.text(a.x, a.y + 2, &format!("Minecraft {} - how much memory should the server get?", self.version()), t.dim, None);
                let ok = ram_choices(self.mem_kb);
                let mut x = a.x + 2;
                for g in [2u32, 4, 8] {
                    let label = format!(" {g} GB ");
                    let (fg, bg) = if !ok.contains(&g) { (t.dim, t.panel) } else if g == self.ram { (t.accent_text, t.accent) } else { (t.text, t.panel) };
                    p.clip(x, a.y + 4, label.len(), &label, fg, bg);
                    if !ok.contains(&g) { p.text(x, a.y + 5, "too big", t.dim, None); }
                    x += label.len() + 3;
                }
                p.text(a.x, a.y + 7, &format!("This computer has {:.1} GB. A bit more than the chosen amount is used in total.", self.mem_kb as f64 / 1_048_576.0), t.dim, None);
            }
            Step::Done => {
                p.text(a.x, a.y + 2, "Your Minecraft server is ready", t.accent, None);
                let mut y = a.y + 4;
                for l in super::wrap_text(&self.msg, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.text, t.panel); y += 1; } }
                y += 1;
                let line = match &self.addr { Some(ip) => format!("Players on your network connect to:  {ip}:25565"), None => "Players connect to this computer's address, port 25565.".to_string() };
                for l in super::wrap_text(&line, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.accent, t.panel); y += 1; } }
                y += 1;
                for l in super::wrap_text("To play from outside your home you must also forward port 25565 on your router. The server starts at every boot and restarts if it crashes.", a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.dim, t.panel); y += 1; } }
            }
            Step::Options | Step::Working => {
                p.text(a.x, a.y + 2, &format!("Minecraft {} with {} GB", self.version(), self.ram), t.text, None);
                p.text(a.x, a.y + 4, "Name of the server", t.dim, None);
                p.field(a.x, a.y + 5, a.w.min(40), &self.name, self.step == Step::Options && self.focus == 0 && focused, false);
                p.clip(a.x, a.y + 8, a.w, &format!("[{}] I have read and agree to the Minecraft EULA: https://aka.ms/MinecraftEULA", if self.eula { "x" } else { " " }), if self.step == Step::Options && self.focus == 1 { t.accent } else { t.text }, t.panel);
                p.clip(a.x, a.y + 10, 14, "  Install  ", if self.step == Step::Options && self.focus == 2 { t.accent_text } else { t.text }, if self.step == Step::Options && self.focus == 2 { t.accent } else { t.panel });
                if self.step == Step::Options && !self.eula { p.paragraph(a.x, a.y + 12, a.w, "Without the tick the server is installed but will not start.", t.dim, t.panel); }
            }
        }
        let hint = match self.step { Step::Versions => "Up/Down Choose   Enter Next", Step::Ram => "Left/Right or 2 4 8   Enter Next   Esc Back", Step::Options => "Tab Next box   Space Tick   Enter Install   Esc Back", Step::Working => "Please wait...", Step::Done => "Enter Back to the start" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if self.step != Step::Done && !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let step = match self.step { Step::Versions => "versions", Step::Ram => "ram", Step::Options => "options", Step::Working => "working", Step::Done => "done" };
        format!("name=minecraft step={step} addr={} versions={} latest={} sel={} ram={} eula={} focus={} busy={} msg={}", n(self.addr.as_deref().unwrap_or("")), self.versions.len(), n(&self.latest), n(self.version()), self.ram, self.eula, self.focus, self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "Up/Down Choose   Enter Next   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app(mem_kb: u64) -> (Minecraft, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![]));
        let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() { "mc-versions" => Ok("latest 1.21.5\nrelease 1.21.5\nrelease 1.21.4\nrelease 1.8.9\n".into()), "mc-create" if r[2] == "1.8.9" => Err("Installing Java failed".into()), "mc-create" => Ok("Minecraft installed and starting".into()), _ => Ok(String::new()) }
        });
        (Minecraft::new(be, mem_kb), calls)
    }
    fn settle(m: &mut Minecraft) { let e = Env::for_tests(); for _ in 0..200 { m.tick(&e); if !m.job.busy { m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Minecraft, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }
    fn text(m: &mut Minecraft, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn versions_are_listed_newest_first_with_the_latest_marked() {
        let (mut m, _c) = app(16_000_000); settle(&mut m);
        assert_eq!((m.latest.as_str(), m.versions.len()), ("1.21.5", 3));
        assert!(m.state().contains("sel=1.21.5") && m.state().contains("step=versions"));
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "down");
        assert_eq!(m.version(), "1.8.9", "stays on the last one");
    }

    #[test]
    fn only_2_4_or_8_gb_and_only_what_fits() {
        let (mut m, _c) = app(6_000_000); settle(&mut m);
        key(&mut m, "enter");
        assert!(m.state().contains("step=ram") && m.state().contains("ram=2"));
        key(&mut m, "right"); assert_eq!(m.ram, 4);
        key(&mut m, "right"); assert_eq!(m.ram, 4, "8 GB does not fit 6 GB of RAM");
        key(&mut m, "8"); assert_eq!(m.ram, 4); assert!(m.msg.contains("does not fit"));
        key(&mut m, "2"); assert_eq!(m.ram, 2);
        key(&mut m, "left"); assert_eq!(m.ram, 2);
        let (mut small, _c) = app(1_000_000); settle(&mut small);
        key(&mut small, "enter");
        assert!(small.state().contains("step=versions") && small.msg.contains("too little memory"), "{}", small.msg);
    }

    #[test]
    fn installing_needs_the_steps_and_passes_the_choices_on() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "enter");            // 1.21.4
        key(&mut m, "4"); key(&mut m, "enter");                // 4 GB
        for _ in 0..9 { key(&mut m, "backspace"); }
        text(&mut m, "survival"); key(&mut m, "enter");        // name, then the EULA box
        key(&mut m, "space"); assert!(m.eula);
        key(&mut m, "enter"); key(&mut m, "enter");            // Install
        assert!(calls.lock().unwrap().iter().any(|c| c.starts_with("mc-create survival 1.21.4 4 1 ")), "{:?}", calls.lock().unwrap());
        assert!(m.state().contains("step=done"), "{}", m.state());
        key(&mut m, "enter"); assert!(m.state().contains("step=versions"));
    }

    #[test]
    fn the_eula_is_off_by_default_and_never_ticked_for_you() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");
        let c = calls.lock().unwrap().iter().find(|c| c.starts_with("mc-create")).cloned().unwrap();
        assert!(c.starts_with("mc-create minecraft 1.21.5 2 0 "), "{c}");
    }

    #[test]
    fn errors_are_shown_and_a_bad_name_is_refused() {
        let (mut m, _c) = app(16_000_000); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter"); key(&mut m, "enter");   // 1.8.9, 2 GB
        for _ in 0..12 { key(&mut m, "backspace"); }
        text(&mut m, "Bad Name"); for _ in 0..3 { key(&mut m, "enter"); }
        assert!(m.state().contains("step=options") && m.msg.contains("lowercase"), "{}", m.msg);
        for _ in 0..12 { key(&mut m, "backspace"); }
        text(&mut m, "old"); key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");
        assert!(m.state().contains("step=options") && m.msg.contains("Java failed"), "{}", m.msg);
    }

    #[test]
    fn the_version_list_can_be_retried_and_everything_draws() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let be: Backend = Arc::new(|_| Err("The download failed.".into()));
        let mut m = Minecraft::new(be, 16_000_000); settle(&mut m);
        assert!(m.msg.contains("failed"));
        let (mut m2, _c) = app(16_000_000); settle(&mut m2);
        for step in 0..4 {
            match step { 1 => key(&mut m2, "enter"), 2 => key(&mut m2, "enter"), 3 => { key(&mut m2, "enter"); key(&mut m2, "enter"); } _ => {} }
            for mm in [&m, &m2] { for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                mm.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            } }
        }
    }
}
