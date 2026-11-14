//! Minecraft server: pick a version, pick 2, 4 or 8 GB of memory, tick the EULA box, install. It talks to the
//! privileged helper, which downloads the server from Mojang, installs the right Java and defines a service.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::mc::{ram_choices, ServerType};
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step { Servers, Log, Type, Versions, Ram, Options, Working, Done }

pub struct Minecraft {
    be: Backend,
    job: Job,
    step: Step,
    kind: ServerType,
    type_sel: usize,
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
    /// The server that was just installed, and what its log says it is doing (asked again every few seconds on the done screen).
    created: String,
    status: String,
    status_asked: Option<std::time::Instant>,
    avail_kb: u64,
    /// The servers that already exist (name, state, folder), the highlighted row (the last row is "create a new server"),
    /// and what the highlighted server's log says.
    servers: Vec<(String, String, String)>,
    srv_sel: usize,
    srv_detail: String,
    srv_asked: Option<std::time::Instant>,
    listed: bool,
    /// Where the new server's folder is made.
    base: String,
    /// The log and console screen: the newest lines of the server's log, the line being typed, and a command waiting to be sent.
    log_for: String,
    log_lines: Vec<String>,
    log_scroll: usize,
    log_follow: bool,
    log_asked: Option<std::time::Instant>,
    input: String,
    pending: Option<String>,
    offer_console: bool,
    rows: usize,
}

pub fn parse_versions(text: &str) -> (String, Vec<String>) {
    let latest = text.lines().find_map(|l| l.strip_prefix("latest ")).unwrap_or("").to_string();
    (latest, text.lines().filter_map(|l| l.strip_prefix("release ")).map(String::from).collect())
}

impl Minecraft {
    pub fn new(be: Backend, mem_kb: u64) -> Minecraft {
        let avail_kb = crate::mc::mem_available_kb();
        let ram = ram_choices(mem_kb).first().copied().unwrap_or(2);   // the smallest choice: the safe default
        Minecraft { log_for: String::new(), log_lines: vec![], log_scroll: 0, log_follow: true, log_asked: None, input: String::new(), pending: None, offer_console: false, rows: 20, servers: vec![], srv_sel: 0, srv_detail: String::new(), srv_asked: None, listed: false, base: crate::mc::DEFAULT_BASE.into(), created: String::new(), status: String::new(), status_asked: None, avail_kb, be, job: Job::new(), step: Step::Type, kind: ServerType::Vanilla, type_sel: 0, latest: String::new(), versions: vec![], sel: 0, mem_kb, ram, name: "minecraft".into(), eula: false, focus: 0, msg: String::new(), first: true, addr: None }
    }

    pub fn system_memory_kb() -> u64 {
        std::fs::read_to_string("/proc/meminfo").unwrap_or_default().lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    fn version(&self) -> &str { self.versions.get(self.sel).map_or("", String::as_str) }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("versions", Ok(t)) => { let (l, v) = parse_versions(&t); self.latest = l; self.versions = v; self.sel = 0; self.msg.clear(); }
            ("versions", Err(e)) => self.msg = e,
            ("create", Ok(m)) => { self.step = Step::Done; self.msg = m; self.addr = super::network::local_ip(); self.created = self.name.clone(); self.status.clear(); self.status_asked = None; }
            ("status", Ok(t)) => self.status = t,
            ("status", Err(_)) => {}
            ("list", Ok(t)) => {
                self.servers = t.lines().filter_map(|l| { let mut w = l.split_whitespace(); Some((w.next()?.to_string(), w.next()?.to_string(), w.next()?.to_string())) }).collect();
                if !self.listed { self.listed = true; if self.step == Step::Type && !self.servers.is_empty() { self.step = Step::Servers; } }
                self.srv_sel = self.srv_sel.min(self.servers.len());
                self.ask_detail(env);
            }
            ("list", Err(e)) => self.msg = e,
            ("detail", Ok(t)) => self.srv_detail = t,
            ("detail", Err(_)) => {}
            ("act", Ok(m)) => { self.msg = if m.is_empty() { "Done.".into() } else { m }; self.job.start(env, &self.be, "list", vec!["mc-list".into()]); }
            ("act", Err(e)) => self.msg = e,
            ("log", Ok(t)) => { self.log_lines = t.lines().map(String::from).collect(); }
            ("log", Err(e)) => self.msg = e,
            ("cmd", Ok(_)) => { self.msg = "Sent. The answer appears in the log.".into(); self.log_asked = None; }
            ("cmd", Err(e)) => { self.offer_console = e.contains("before the console existed"); self.msg = e; }
            ("enable", Ok(m)) => { self.msg = m; self.offer_console = false; }
            ("enable", Err(e)) => self.msg = e,
            ("create", Err(e)) => { self.step = Step::Options; self.msg = e; }
            ("accept", Ok(m)) => { self.eula = true; self.msg = m; }
            ("accept", Err(e)) => self.msg = e,
            _ => {}
        }
    }

    fn ask_detail(&mut self, env: &Env) {
        if let Some((n, _, _)) = self.servers.get(self.srv_sel) { let n = n.clone(); self.job.start(env, &self.be, "detail", vec!["mc-status".into(), n]); self.srv_asked = Some(std::time::Instant::now()); } else { self.srv_detail.clear(); }
    }

    fn act(&mut self, env: &Env, what: &str) {
        let Some((n, _, _)) = self.servers.get(self.srv_sel) else { return };
        let words: Vec<String> = match what { "accept" => vec!["mc-accept".into(), n.clone()], w => vec![format!("svc-{w}"), n.clone()] };
        if self.job.start(env, &self.be, "act", words) { self.msg = match what { "start" => "Starting ...", "stop" => "Stopping ...", "restart" => "Restarting ...", _ => "Accepting the EULA and starting ..." }.into(); } else { self.msg = "Please wait, the last request is still running.".into(); }
    }

    fn install(&mut self, env: &Env) {
        let user = std::env::var("USER").unwrap_or_default();
        let words = ["mc-create", &self.name, self.version(), &self.ram.to_string(), if self.eula { "1" } else { "0" }, &user, self.kind.id(), &self.base].iter().map(|s| s.to_string()).collect();
        if self.job.start(env, &self.be, "create", words) { self.step = Step::Working; self.msg = "Downloading and installing. This can take a few minutes...".into(); }
    }
}

impl Application for Minecraft {
    fn kind(&self) -> Kind { Kind::Minecraft }
    fn title(&self) -> String { "Minecraft server".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else if self.step == Step::Log { Duration::from_secs(1) } else if self.step == Step::Done || self.step == Step::Servers { Duration::from_secs(3) } else { Duration::from_secs(60) }) }
    fn resize(&mut self, _cols: usize, rows: usize) { self.rows = rows.saturating_sub(6).max(1); }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first { self.first = false; self.job.start(env, &self.be, "list", vec!["mc-list".into()]); }
        // On the log screen: send a waiting command, then read the newest lines again every couple of seconds
        if self.step == Step::Log && !self.job.busy {
            if let Some(c) = self.pending.take() { self.job.start(env, &self.be, "cmd", vec!["mc-cmd".into(), self.log_for.clone(), c]); }
            else if self.log_asked.map_or(true, |t| t.elapsed() >= Duration::from_secs(2)) { self.log_asked = Some(std::time::Instant::now()); self.job.start(env, &self.be, "log", vec!["svc-log".into(), self.log_for.clone(), "300".into()]); }
        }
        // On the servers screen: look again every few seconds (a server that was starting is running a moment later)
        if self.step == Step::Servers && !self.job.busy && self.srv_asked.map_or(true, |t| t.elapsed() >= Duration::from_secs(3)) { self.srv_asked = Some(std::time::Instant::now()); self.job.start(env, &self.be, "list", vec!["mc-list".into()]); }
        // On the done screen: what does the server's own log say it is doing?
        if self.step == Step::Done && !self.created.is_empty() && !self.job.busy && self.status_asked.map_or(true, |t| t.elapsed() >= Duration::from_secs(3)) {
            self.status_asked = Some(std::time::Instant::now());
            self.job.start(env, &self.be, "status", vec!["mc-status".into(), self.created.clone()]);
        }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.step {
            Step::Servers => {
                let n = self.servers.len();
                match k.code {
                    code::UP => { self.srv_sel = self.srv_sel.saturating_sub(1); self.msg.clear(); self.ask_detail(env); }
                    code::DOWN => { self.srv_sel = (self.srv_sel + 1).min(n); self.msg.clear(); self.ask_detail(env); }
                    code::ENTER => if self.srv_sel >= n { self.step = Step::Type; self.msg.clear(); } else { self.msg = "Press S to start, T to stop or R to restart this server.".into(); },
                    _ => if self.srv_sel < n { match k.ch().map(|c| c.to_ascii_lowercase()) { Some('s') => self.act(env, "start"), Some('t') => self.act(env, "stop"), Some('r') => self.act(env, "restart"), Some('e') => self.act(env, "accept"), Some('l') | Some('c') => { self.log_for = self.servers[self.srv_sel].0.clone(); self.log_lines.clear(); self.log_follow = true; self.input.clear(); self.offer_console = false; self.log_asked = None; self.msg.clear(); self.step = Step::Log; } _ => {} } },
                }
            }
            Step::Log => {
                let page = self.rows.saturating_sub(1).max(1);
                let max = self.log_lines.len().saturating_sub(self.rows.max(1));
                match k.code {
                    code::ESC => { self.step = Step::Servers; self.msg.clear(); self.srv_asked = None; }
                    code::ENTER => { let c = self.input.trim().to_string(); if !c.is_empty() { self.pending = Some(c); self.input.clear(); } }
                    code::PGUP => { if self.log_follow { self.log_scroll = max; self.log_follow = false; } self.log_scroll = self.log_scroll.saturating_sub(page); }
                    code::PGDN => { self.log_scroll = (self.log_scroll + page).min(max); if self.log_scroll >= max { self.log_follow = true; } }
                    code::HOME => { self.log_follow = false; self.log_scroll = 0; }
                    code::END => { self.log_follow = true; }
                    code::F5 => if self.offer_console { self.job.start(env, &self.be, "enable", vec!["mc-console".into(), self.log_for.clone()]); self.msg = "Adding the console ...".into(); },
                    _ => { edit_text(&mut self.input, &k, 200); }
                }
            }
            Step::Type => match k.code {
                code::ESC if !self.servers.is_empty() => { self.step = Step::Servers; self.msg.clear(); }
                code::UP => self.type_sel = self.type_sel.saturating_sub(1),
                code::DOWN => self.type_sel = (self.type_sel + 1).min(ServerType::ALL.len() - 1),
                code::ENTER => {
                    self.kind = ServerType::ALL[self.type_sel];
                    self.versions.clear(); self.latest.clear(); self.sel = 0;
                    self.job.start(env, &self.be, "versions", vec!["mc-versions".into(), self.kind.id().into()]);
                    self.msg = format!("Loading the {} version list...", self.kind.label());
                    self.step = Step::Versions;
                }
                _ => {}
            },
            Step::Versions => match k.code {
                code::ESC | code::BACKSPACE => { self.step = Step::Type; self.msg.clear(); }
                code::UP => self.sel = self.sel.saturating_sub(1),
                code::DOWN => self.sel = (self.sel + 1).min(self.versions.len().saturating_sub(1)),
                code::PGUP => self.sel = self.sel.saturating_sub(10),
                code::PGDN => self.sel = (self.sel + 10).min(self.versions.len().saturating_sub(1)),
                code::HOME => self.sel = 0,
                code::ENTER => {
                    if self.versions.is_empty() { self.job.start(env, &self.be, "versions", vec!["mc-versions".into(), self.kind.id().into()]); self.msg = "Loading the version list...".into(); }
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
                code::TAB | code::DOWN => self.focus = (self.focus + 1) % 4,
                code::UP => self.focus = (self.focus + 3) % 4,
                code::ENTER => {
                    if self.focus < 3 { self.focus += 1; }
                    else if crate::services::valid_name(&self.name).is_err() { self.msg = "The name may only have lowercase letters, digits, - and _.".into(); self.focus = 0; }
                    else if !crate::mc::valid_base(&self.base) { self.msg = "The install folder must be under /srv or /home and may only contain letters, digits and / _ . -".into(); self.focus = 1; }
                    else if self.servers.iter().any(|(n, _, _)| *n == self.name) { self.msg = "You already have a server with that name.".into(); self.focus = 0; }
                    else { self.install(env); }
                }
                _ => {
                    if self.focus == 2 { if k.ch() == Some(' ') { self.eula = !self.eula; } }
                    else if self.focus == 1 { edit_text(&mut self.base, &k, 120); }
                    else if self.focus == 0 { edit_text(&mut self.name, &k, 32); }
                }
            },
            Step::Working => {}
            Step::Done => {
                if k.code == code::ENTER || k.code == code::ESC { self.step = Step::Servers; self.msg.clear(); self.srv_sel = 0; self.job.start(env, &self.be, "list", vec!["mc-list".into()]); }
                else if !self.eula && k.ch().map(|c| c.to_ascii_lowercase()) == Some('e') {
                    // the person at the keyboard asks for the EULA to be accepted for this server
                    self.job.start(env, &self.be, "accept", vec!["mc-accept".into(), self.name.clone()]);
                    self.msg = "Accepting the EULA and starting the server...".into();
                }
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Minecraft server", t.accent, None);
        let steps = ["1 Type", "2 Version", "3 Memory", "4 Install"];
        let cur = match self.step { Step::Servers | Step::Log => 9, Step::Type => 0, Step::Versions => 1, Step::Ram => 2, _ => 3 };
        let mut x = a.x + 18;
        for (i, s) in steps.iter().enumerate() { if x + s.len() < a.x + a.w { p.text(x, a.y, s, if i == cur { t.accent } else { t.dim }, None); } x += s.len() + 2; }
        let body = Rect { x: a.x, y: a.y + 2, w: a.w, h: a.h.saturating_sub(4) };
        match self.step {
            Step::Type => {
                p.text(a.x, a.y + 2, "What kind of Minecraft server?", t.dim, None);
                let items: Vec<String> = ServerType::ALL.iter().map(|k| format!("{:<8} {}", k.label(), k.blurb())).collect();
                p.list(Rect { x: a.x, y: a.y + 4, w: a.w, h: ServerType::ALL.len().min(body.h.saturating_sub(2)) }, &items, self.type_sel, 0, focused);
                p.paragraph(a.x, a.y + 8, a.w, "Vanilla and Paper are for playing with friends (Paper adds plugins); Fabric is for mods, which every player must also install.", t.dim, t.panel);
            }
            Step::Versions => {
                if self.versions.is_empty() { p.text(a.x, a.y + 2, if self.job.busy { "Loading the version list..." } else { "No list yet. Press Enter to try again, Esc to go back." }, t.dim, None); }
                else {
                    p.text(a.x, a.y + 2, &format!("Choose a {} version (the newest is {}):", self.kind.label(), self.latest), t.dim, None);
                    let items: Vec<String> = self.versions.iter().map(|v| if *v == self.latest { format!("{v}   (latest)") } else { v.clone() }).collect();
                    let r = Rect { x: a.x, y: a.y + 3, w: a.w, h: body.h.saturating_sub(1) };
                    p.list(r, &items, self.sel, self.sel.saturating_sub(r.h.saturating_sub(1)), focused);
                }
            }
            Step::Ram => {
                p.text(a.x, a.y + 2, &format!("{} {} - how much memory should the server get?", self.kind.label(), self.version()), t.dim, None);
                let ok = ram_choices(self.mem_kb);
                let mut x = a.x + 2;
                for g in [2u32, 4, 8] {
                    let label = format!(" {g} GB ");
                    let (fg, bg) = if !ok.contains(&g) { (t.dim, t.panel) } else if g == self.ram { (t.accent_text, t.accent) } else { (t.text, t.panel) };
                    p.clip(x, a.y + 4, label.len(), &label, fg, bg);
                    if !ok.contains(&g) { p.text(x, a.y + 5, "too big", t.dim, None); }
                    x += label.len() + 3;
                }
                p.text(a.x, a.y + 7, &format!("This computer has {:.1} GB ({:.1} GB free now). A bit more than the chosen amount is used in total.", self.mem_kb as f64 / 1_048_576.0, self.avail_kb as f64 / 1_048_576.0), t.dim, None);
                if let Some(w) = crate::mc::memory_warning(self.avail_kb, self.ram) { for (i, l) in super::wrap_text(&w, a.w).iter().take(5).enumerate() { p.clip(a.x, a.y + 9 + i, a.w, l, t.accent, t.panel); } }
            }
            Step::Done => {
                p.text(a.x, a.y + 2, "Your Minecraft server is installed", t.accent, None);
                let (state, detail) = self.status.split_once(' ').unwrap_or(("", "Checking what the server is doing..."));
                let shown = if self.eula || state == "failed" || !state.is_empty() { detail.to_string() } else { String::new() };
                let color = if state == "failed" { t.accent } else { t.text };
                if !self.eula && state != "running" && state != "loading" && state != "starting" { /* the EULA note below says it all */ } else {
                    p.clip(a.x, a.y + 3, a.w, &format!("Status: {shown}"), color, t.panel);
                }
                let mut y = a.y + 5;
                for l in super::wrap_text(&self.msg, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.text, t.panel); y += 1; } }
                y += 1;
                let line = match &self.addr { Some(ip) => format!("Players on your network connect to:  {ip}:25565"), None => "Players connect to this computer's address, port 25565.".to_string() };
                for l in super::wrap_text(&line, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.accent, t.panel); y += 1; } }
                y += 1;
                if !self.eula {
                    for l in super::wrap_text("The server will not start yet: the Minecraft EULA has not been accepted. Read it at https://aka.ms/MinecraftEULA, then press E to accept it and start the server.", a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.accent, t.panel); y += 1; } }
                    y += 1;
                }
                for l in super::wrap_text("To play from outside your home you must also forward port 25565 on your router. The server starts at every boot and restarts if it crashes.", a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.dim, t.panel); y += 1; } }
            }
            Step::Options | Step::Working => {
                let ed = self.step == Step::Options;
                p.text(a.x, a.y + 2, &format!("{} {} with {} GB", self.kind.label(), self.version(), self.ram), t.text, None);
                p.text(a.x, a.y + 3, "Name of the server", t.dim, None);
                p.field(a.x, a.y + 4, a.w.min(40), &self.name, ed && self.focus == 0 && focused, false);
                p.text(a.x, a.y + 6, "Install in this folder (the server gets its own folder inside it)", t.dim, None);
                p.field(a.x, a.y + 7, a.w.min(60), &self.base, ed && self.focus == 1 && focused, false);
                p.clip(a.x, a.y + 9, a.w, &format!("[{}] I have read and agree to the Minecraft EULA: https://aka.ms/MinecraftEULA", if self.eula { "x" } else { " " }), if ed && self.focus == 2 { t.accent } else { t.text }, t.panel);
                p.clip(a.x, a.y + 11, 14, "  Install  ", if ed && self.focus == 3 { t.accent_text } else { t.text }, if ed && self.focus == 3 { t.accent } else { t.panel });
                if ed && !self.eula { p.paragraph(a.x, a.y + 13, a.w, "Without the tick the server is installed but will not start.", t.dim, t.panel); }
            }
            Step::Log => {
                let rows = a.h.saturating_sub(6).max(1);
                let max = self.log_lines.len().saturating_sub(rows);
                let top = if self.log_follow { max } else { self.log_scroll.min(max) };
                p.text(a.x, a.y + 2, &format!("Log of {}{}", self.log_for, if self.log_follow { "   (following the newest lines)" } else { "   (scrolled back: End follows again)" }), t.dim, None);
                for i in 0..rows { let Some(l) = self.log_lines.get(top + i) else { break }; p.clip(a.x, a.y + 3 + i, a.w, l, t.text, t.panel); }
                let y = a.y + a.h.saturating_sub(3);
                p.text(a.x, y, "Command:", t.accent, None);
                p.field(a.x + 9, y, a.w.saturating_sub(9).max(1), &self.input, focused, false);
                if self.offer_console { p.clip(a.x, y + 1, a.w, "Press F5 to add the console to this server (then restart it).", t.accent, t.panel); }
            }
            Step::Servers => {
                p.text(a.x, a.y + 2, "Your Minecraft servers", t.dim, None);
                let mut items: Vec<String> = self.servers.iter().map(|(n, st, dir)| format!("{:<14} {:<8} {}", n, st, dir)).collect();
                items.push("+ Create a new server".into());
                let rows = body.h.saturating_sub(5).max(1);
                p.list(Rect { x: a.x, y: a.y + 3, w: a.w, h: rows }, &items, self.srv_sel, self.srv_sel.saturating_sub(rows.saturating_sub(1)), focused);
                if self.srv_sel < self.servers.len() && !self.srv_detail.is_empty() {
                    let d = self.srv_detail.split_once(' ').map_or(self.srv_detail.as_str(), |(_, r)| r);
                    p.clip(a.x, a.y + 3 + rows + 1, a.w, &format!("Log: {d}"), t.text, t.panel);
                }
            }
        }
        let hint = match self.step { Step::Servers => "S Start  T Stop  R Restart  L Log and console  E Accept EULA  Enter New", Step::Log => "Type a Minecraft command, Enter sends   PgUp/PgDn Scroll   End Newest   Esc Back", Step::Type => "Up/Down Choose   Enter Next", Step::Versions => "Up/Down Choose   Enter Next   Esc Back", Step::Ram => "Left/Right or 2 4 8   Enter Next   Esc Back", Step::Options => "Tab Next box   Space Tick   Enter Next / Install   Esc Back", Step::Working => "Please wait...", Step::Done => if self.eula { "Enter Back to the start" } else { "E Accept the EULA and start   Enter Back to the start" } };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if self.step != Step::Done && !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let step = match self.step { Step::Servers => "servers", Step::Log => "log", Step::Type => "type", Step::Versions => "versions", Step::Ram => "ram", Step::Options => "options", Step::Working => "working", Step::Done => "done" };
        format!("name=minecraft log_lines={} servers={} sel_srv={} base={} status={} step={step} kind={} addr={} versions={} latest={} sel={} ram={} eula={} focus={} busy={} msg={}", self.log_lines.len(), self.servers.len(), self.srv_sel, n(&self.base), n(self.status.split(' ').next().unwrap_or("")), self.kind.id(), n(self.addr.as_deref().unwrap_or("")), self.versions.len(), n(&self.latest), n(self.version()), self.ram, self.eula, self.focus, self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
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
            match r[0].as_str() { "mc-accept" => Ok("The EULA is accepted and the server is starting.".into()), "mc-versions" => Ok("latest 1.21.5\nrelease 1.21.5\nrelease 1.21.4\nrelease 1.8.9\n".into()), "mc-create" if r[2] == "1.8.9" => Err("Installing Java failed".into()), "mc-create" => Ok("Minecraft installed and starting".into()), "mc-status" => Ok("loading Preparing the spawn area (43%).".into()), _ => Ok(String::new()) }
        });
        (Minecraft::new(be, mem_kb), calls)
    }
    fn settle(m: &mut Minecraft) { let e = Env::for_tests(); for _ in 0..200 { m.tick(&e); if !m.job.busy { m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Minecraft, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }
    fn text(m: &mut Minecraft, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn the_done_screen_asks_the_server_what_it_is_doing() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");                // type, version, memory
        key(&mut m, "tab"); key(&mut m, "tab"); key(&mut m, " "); key(&mut m, "tab"); key(&mut m, "enter");  // folder, EULA (tick), install
        assert!(m.state().contains("step=done"), "{}", m.state());
        std::thread::sleep(Duration::from_millis(20)); settle(&mut m);
        assert!(calls.lock().unwrap().iter().any(|c| c == "mc-status minecraft"), "{:?}", calls.lock().unwrap());
        assert!(m.state().contains("status=loading"), "{}", m.state());
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
            let mut c = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
            m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
        }
    }

    #[test]
    fn versions_are_listed_newest_first_with_the_latest_marked() {
        let (mut m, _c) = app(16_000_000); settle(&mut m);
        assert!(m.state().contains("step=type"), "the first question is what kind of server");
        key(&mut m, "enter");
        assert_eq!((m.latest.as_str(), m.versions.len()), ("1.21.5", 3));
        assert!(m.state().contains("sel=1.21.5") && m.state().contains("step=versions"));
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "down");
        assert_eq!(m.version(), "1.8.9", "stays on the last one");
    }

    #[test]
    fn only_2_4_or_8_gb_and_only_what_fits() {
        let (mut m, _c) = app(6_000_000); settle(&mut m);
        key(&mut m, "enter"); key(&mut m, "enter");
        assert!(m.state().contains("step=ram") && m.state().contains("ram=2"));
        key(&mut m, "right"); assert_eq!(m.ram, 4);
        key(&mut m, "right"); assert_eq!(m.ram, 4, "8 GB does not fit 6 GB of RAM");
        key(&mut m, "8"); assert_eq!(m.ram, 4); assert!(m.msg.contains("does not fit"));
        key(&mut m, "2"); assert_eq!(m.ram, 2);
        key(&mut m, "left"); assert_eq!(m.ram, 2);
        let (mut small, _c) = app(1_000_000); settle(&mut small);
        key(&mut small, "enter"); key(&mut small, "enter");
        assert!(small.state().contains("step=versions") && small.msg.contains("too little memory"), "{}", small.msg);
    }

    #[test]
    fn installing_needs_the_steps_and_passes_the_choices_on() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "enter");                                   // Vanilla
        key(&mut m, "down"); key(&mut m, "enter");            // 1.21.4
        key(&mut m, "4"); key(&mut m, "enter");                // 4 GB
        for _ in 0..9 { key(&mut m, "backspace"); }
        text(&mut m, "survival"); key(&mut m, "enter"); key(&mut m, "enter");   // name, the folder, then the EULA box
        key(&mut m, "space"); assert!(m.eula);
        key(&mut m, "enter"); key(&mut m, "enter");            // Install
        assert!(calls.lock().unwrap().iter().any(|c| c.starts_with("mc-create survival 1.21.4 4 1 ") && c.ends_with(" vanilla /srv/minecraft")), "{:?}", calls.lock().unwrap());
        assert!(m.state().contains("step=done"), "{}", m.state());
        key(&mut m, "enter"); assert!(m.state().contains("step=servers"), "back to the list of servers: {}", m.state());
    }

    #[test]
    fn the_eula_is_off_by_default_and_never_ticked_for_you() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        for _ in 0..7 { key(&mut m, "enter"); }
        let c = calls.lock().unwrap().iter().find(|c| c.starts_with("mc-create")).cloned().unwrap();
        assert!(c.starts_with("mc-create minecraft 1.21.5 2 0 ") && c.ends_with(" vanilla /srv/minecraft"), "{c}");
    }

    #[test]
    fn errors_are_shown_and_a_bad_name_is_refused() {
        let (mut m, _c) = app(16_000_000); settle(&mut m);
        key(&mut m, "enter");
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter"); key(&mut m, "enter");   // 1.8.9, 2 GB
        for _ in 0..12 { key(&mut m, "backspace"); }
        text(&mut m, "Bad Name"); for _ in 0..4 { key(&mut m, "enter"); }
        assert!(m.state().contains("step=options") && m.msg.contains("lowercase"), "{}", m.msg);
        for _ in 0..12 { key(&mut m, "backspace"); }
        text(&mut m, "old"); for _ in 0..4 { key(&mut m, "enter"); }
        assert!(m.state().contains("step=options") && m.msg.contains("Java failed"), "{}", m.msg);
    }

    #[test]
    fn a_server_installed_without_the_tick_can_be_accepted_from_the_done_screen() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        for _ in 0..7 { key(&mut m, "enter"); }                         // everything on the defaults, EULA box untouched
        assert!(m.state().contains("step=done") && m.state().contains("eula=false") || m.state().contains("step=done"), "{}", m.state());
        key(&mut m, "e");
        assert!(calls.lock().unwrap().contains(&"mc-accept minecraft".to_string()), "{:?}", calls.lock().unwrap());
        assert!(m.eula && m.msg.contains("accepted"), "{}", m.msg);
        let before = calls.lock().unwrap().len();
        key(&mut m, "e"); assert_eq!(calls.lock().unwrap().len(), before, "once accepted the key does nothing");
    }

    #[test]
    fn the_server_type_decides_which_list_and_which_download_is_asked_for() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "enter");                      // Paper
        assert!(m.state().contains("kind=paper") && m.state().contains("step=versions"));
        assert!(calls.lock().unwrap().contains(&"mc-versions paper".to_string()), "{:?}", calls.lock().unwrap());
        key(&mut m, "esc"); assert!(m.state().contains("step=type"), "Esc goes back to the first question");
        key(&mut m, "down"); key(&mut m, "enter");                      // Fabric
        assert!(m.state().contains("kind=fabric") && calls.lock().unwrap().contains(&"mc-versions fabric".to_string()));
        key(&mut m, "enter"); key(&mut m, "enter");                     // version, memory
        key(&mut m, "enter"); key(&mut m, "enter");                     // name -> folder -> the EULA box
        key(&mut m, "space"); key(&mut m, "enter"); key(&mut m, "enter");   // tick it, then Install
        let c = calls.lock().unwrap().iter().find(|c| c.starts_with("mc-create")).cloned().unwrap();
        assert!(c.ends_with(" fabric /srv/minecraft") && c.contains(" 1 "), "{c}");
    }

    fn app_with_servers() -> (Minecraft, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() {
                "mc-list" => Ok("alpha running /srv/minecraft/alpha\nbeta stopped /home/me/games/beta\n".into()),
                "mc-status" if r[1] == "beta" => Ok("failed The EULA has not been accepted, so the server stopped.".into()),
                "mc-status" => Ok("running Running: players can connect.".into()),
                "mc-versions" => Ok("latest 1.21.5\nrelease 1.21.5\n".into()),
                "mc-accept" => Ok("The EULA is accepted and beta is starting.".into()),
                "svc-log" => Ok(format!("{}[Server thread/INFO]: Done (3.2s)! For help, type \"help\"", (1..=40).map(|i| format!("[main/INFO]: line {i} of {}\n", r[1])).collect::<String>())),
                "mc-cmd" if r[1] == "beta" => Err(crate::mc::NO_CONSOLE.into()),
                "mc-cmd" => Ok("Sent.".into()),
                "mc-console" => Ok("The console is added. Restart beta to use it.".into()),
                _ => Ok(String::new()),
            }
        });
        (Minecraft::new(be, 16_000_000), calls)
    }

    #[test]
    fn it_opens_on_the_list_of_servers_when_there_are_some() {
        let (mut m, _c) = app_with_servers(); settle(&mut m);
        assert!(m.state().contains("step=servers") && m.state().contains("servers=2"), "{}", m.state());
        let (mut none, _c) = app(16_000_000); settle(&mut none);
        assert!(none.state().contains("step=type"), "with no servers it goes straight to creating one: {}", none.state());
    }

    #[test]
    fn a_server_can_be_started_stopped_and_restarted_from_the_list() {
        let (mut m, calls) = app_with_servers(); settle(&mut m);
        key(&mut m, "t"); assert!(calls.lock().unwrap().contains(&"svc-stop alpha".to_string()), "{:?}", calls.lock().unwrap());
        key(&mut m, "r"); assert!(calls.lock().unwrap().contains(&"svc-restart alpha".to_string()));
        key(&mut m, "down"); key(&mut m, "s");
        assert!(calls.lock().unwrap().contains(&"svc-start beta".to_string()), "the highlighted server is the one acted on");
        assert!(calls.lock().unwrap().contains(&"mc-status beta".to_string()), "its log state is shown");
        key(&mut m, "e"); assert!(calls.lock().unwrap().contains(&"mc-accept beta".to_string()));
        key(&mut m, "down");                                        // the "create a new server" row
        let before = calls.lock().unwrap().len();
        key(&mut m, "s"); key(&mut m, "t"); assert_eq!(calls.lock().unwrap().len(), before + 0, "the last row is not a server: {:?}", calls.lock().unwrap());
        key(&mut m, "enter"); assert!(m.state().contains("step=type"), "{}", m.state());
        key(&mut m, "esc"); assert!(m.state().contains("step=servers"), "Esc goes back to the list");
    }

    #[test]
    fn the_log_and_console_screen_shows_the_log_and_sends_typed_commands() {
        let (mut m, calls) = app_with_servers(); settle(&mut m); m.resize(80, 16);
        key(&mut m, "l");
        assert!(m.state().contains("step=log"), "{}", m.state());
        std::thread::sleep(Duration::from_millis(10)); settle(&mut m);
        assert!(calls.lock().unwrap().contains(&"svc-log alpha 300".to_string()), "{:?}", calls.lock().unwrap());
        assert!(m.state().contains("log_lines=41"), "{}", m.state());
        text(&mut m, "say hi there"); key(&mut m, "enter");
        std::thread::sleep(Duration::from_millis(10)); settle(&mut m);
        assert!(calls.lock().unwrap().contains(&"mc-cmd alpha say hi there".to_string()), "the whole line is sent as typed: {:?}", calls.lock().unwrap());
        assert!(m.input.is_empty() && m.msg.contains("Sent"), "{} / {}", m.input, m.msg);
        key(&mut m, "enter"); assert!(!calls.lock().unwrap().iter().any(|c| c == "mc-cmd alpha "), "an empty line sends nothing");
        key(&mut m, "pgup"); assert!(!m.log_follow); key(&mut m, "end"); assert!(m.log_follow);
        key(&mut m, "esc"); assert!(m.state().contains("step=servers"));
    }

    #[test]
    fn an_old_server_without_a_console_offers_to_get_one() {
        let (mut m, calls) = app_with_servers(); settle(&mut m); m.resize(80, 16);
        key(&mut m, "down"); key(&mut m, "c");                      // beta
        text(&mut m, "stop"); key(&mut m, "enter");
        std::thread::sleep(Duration::from_millis(10)); settle(&mut m);
        assert!(m.offer_console && m.msg.contains("before the console existed"), "{}", m.msg);
        key(&mut m, "f5");
        assert!(calls.lock().unwrap().contains(&"mc-console beta".to_string()), "{:?}", calls.lock().unwrap());
        assert!(!m.offer_console, "{}", m.msg);
    }

    #[test]
    fn every_servers_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut m, _c) = app_with_servers(); settle(&mut m);
        for step in 0..3 {
            if step == 1 { key(&mut m, "l"); std::thread::sleep(Duration::from_millis(10)); settle(&mut m); } else if step == 2 { key(&mut m, "esc"); key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter"); }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }

    #[test]
    fn the_install_folder_is_asked_checked_and_sent() {
        let (mut m, calls) = app(16_000_000); settle(&mut m);
        key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");        // type, version, memory
        key(&mut m, "tab");                                                      // the folder box
        for _ in 0..20 { key(&mut m, "backspace"); }
        text(&mut m, "/etc/oops"); for _ in 0..3 { key(&mut m, "enter"); }       // folder -> EULA -> Install: refused
        assert!(m.state().contains("step=options") && m.msg.contains("install folder"), "{}", m.msg);
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("mc-create")), "nothing was sent");
        assert!(m.state().contains("focus=1"), "the cursor is put on the folder box: {}", m.state());
        for _ in 0..20 { key(&mut m, "backspace"); }
        text(&mut m, "/home/me/games"); for _ in 0..3 { key(&mut m, "enter"); }
        let c = calls.lock().unwrap().iter().find(|c| c.starts_with("mc-create")).cloned().unwrap();
        assert!(c.ends_with(" vanilla /home/me/games"), "{c}");
    }

    #[test]
    fn a_name_that_is_already_used_is_refused() {
        let (mut m, _c) = app_with_servers(); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter");          // create a new server
        key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");
        for _ in 0..12 { key(&mut m, "backspace"); }
        text(&mut m, "alpha"); for _ in 0..4 { key(&mut m, "enter"); }
        assert!(m.msg.contains("already have a server"), "{}", m.msg);
    }

    #[test]
    fn the_version_list_can_be_retried_and_everything_draws() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let be: Backend = Arc::new(|_| Err("The download failed.".into()));
        let mut m = Minecraft::new(be, 16_000_000); settle(&mut m); key(&mut m, "enter");
        assert!(m.msg.contains("failed"));
        let (mut m2, _c) = app(16_000_000); settle(&mut m2);
        for step in 0..5 {
            match step { 1 => key(&mut m2, "enter"), 2 => key(&mut m2, "enter"), 3 => key(&mut m2, "enter"), 4 => { key(&mut m2, "enter"); key(&mut m2, "enter"); } _ => {} }
            for mm in [&m, &m2] { for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                mm.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            } }
        }
    }
}
