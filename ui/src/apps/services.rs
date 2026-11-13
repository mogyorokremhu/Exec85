//! Services: programs that keep running in the background (a game server, a bot, a web server ...),
//! restart by themselves when they crash and start at boot, with nobody logged in.
//! This app only edits definitions and sends requests to the privileged helper.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use crate::services::{self, ServiceDef};
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row { pub name: String, pub status: String, pub autostart: bool }

pub fn parse_list(s: &str) -> Vec<Row> {
    s.lines().filter_map(|l| { let w: Vec<&str> = l.split_whitespace().collect(); (w.len() == 3).then(|| Row { name: w[0].into(), status: w[1].into(), autostart: w[2] == "true" }) }).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Form { pub fields: [String; 6], pub autostart: bool, pub focus: usize, pub editing: bool }
const LABELS: [&str; 8] = ["Name", "Command", "Folder (optional)", "Run as user", "Variables  NAME=value;NAME2=value", "Open ports to the home network (optional)  e.g. 8080/tcp 25565/udp", "Start at boot", ""];

impl Form {
    fn from_def(d: &ServiceDef, editing: bool) -> Form {
        let vars = d.env.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(";");
        Form { fields: [d.name.clone(), d.command.clone(), d.dir.clone(), d.user.clone(), vars, d.ports.replace(',', " ")], autostart: d.autostart, focus: if editing { 1 } else { 0 }, editing }
    }
    pub fn to_def(&self) -> Result<ServiceDef, String> {
        let mut env = Vec::new();
        for part in self.fields[4].split(';').map(str::trim).filter(|p| !p.is_empty()) {
            let (k, v) = part.split_once('=').ok_or_else(|| format!("'{part}' needs the form NAME=value."))?;
            env.push((k.trim().to_string(), v.to_string()));
        }
        let d = ServiceDef { name: self.fields[0].trim().into(), description: String::new(), command: self.fields[1].trim().into(), dir: self.fields[2].trim().into(), user: self.fields[3].trim().into(), env, autostart: self.autostart, ports: crate::firewall::format_ports(&crate::firewall::parse_ports(&self.fields[5])?) };
        d.validate()?;
        Ok(d)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { List, Form(Form), Log(String, Vec<String>), ConfirmRemove(String) }

pub struct Services {
    be: Backend,
    job: Job,
    rows: Vec<Row>,
    sel: usize,
    mode: Mode,
    msg: String,
    first: bool,
}

impl Services {
    pub fn new(be: Backend) -> Services { Services { be, job: Job::new(), rows: vec![], sel: 0, mode: Mode::List, msg: String::new(), first: true } }

    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) {
        if !self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { self.msg = "Please wait, the last request is still running.".into(); }
    }
    fn current(&self) -> Option<&Row> { self.rows.get(self.sel) }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("list", Ok(t)) => { self.rows = parse_list(&t); self.sel = self.sel.min(self.rows.len().saturating_sub(1)); if self.msg.starts_with("Cannot") { self.msg.clear(); } }
            ("list", Err(e)) => self.msg = format!("Cannot list services: {e}"),
            ("log", Ok(t)) => { if let Mode::Log(_, l) = &mut self.mode { *l = t.lines().map(String::from).collect(); } }
            ("get", Ok(t)) => { if let Some(d) = services::parse_conf(&t) { self.mode = Mode::Form(Form::from_def(&d, true)); } }
            ("save", Ok(_)) => { self.mode = Mode::List; self.msg = "Saved. Start it with S.".into(); self.ask(env, "list", &["svc-list"]); }
            ("remove", Ok(_)) => { self.msg = "Removed.".into(); self.ask(env, "list", &["svc-list"]); }
            (a, Ok(_)) => { self.msg = format!("Done: {a}."); self.ask(env, "list", &["svc-list"]); }
            (_, Err(e)) => self.msg = e,
        }
    }
}

impl Application for Services {
    fn kind(&self) -> Kind { Kind::Services }
    fn title(&self) -> String { "Services".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(300) } else { Duration::from_secs(3) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first || (!self.job.busy && matches!(self.mode, Mode::List)) { self.first = false; if !self.job.busy { self.ask(env, "list", &["svc-list"]); } }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        let mode = self.mode.clone();
        match mode {
            Mode::Log(name, _) => { if k.code == code::ESC || k.code == code::ENTER { self.mode = Mode::List; } else if k.base_char() == Some('r') { self.ask(env, "log", &["svc-log", &name, "200"]); } }
            Mode::ConfirmRemove(name) => {
                match k.ch().map(|c| c.to_ascii_lowercase()) {
                    Some('y') => { self.mode = Mode::List; self.ask(env, "remove", &["svc-remove", &name]); }
                    Some('n') => self.mode = Mode::List,
                    _ => if k.code == code::ESC { self.mode = Mode::List; } else if k.code == code::ENTER { self.mode = Mode::List; self.ask(env, "remove", &["svc-remove", &name]); },
                }
            }
            Mode::Form(mut f) => {
                self.msg.clear();
                let mut keep = true;
                match k.code {
                    code::ESC => keep = false,
                    code::TAB | code::DOWN => f.focus = (f.focus + 1) % 7,
                    code::UP => f.focus = (f.focus + 6) % 7,
                    code::ENTER => {
                        if f.focus < 6 { f.focus += 1; } else {
                            match f.to_def() {
                                Ok(d) => { keep = false; self.ask(env, "save", &["svc-save", &services::render_conf(&d)]); }
                                Err(e) => self.msg = e,
                            }
                        }
                    }
                    _ => {
                        if f.focus == 6 { if k.ch() == Some(' ') { f.autostart = !f.autostart; } }
                        else if !(f.editing && f.focus == 0) { edit_text(&mut f.fields[f.focus], &k, 200); }
                    }
                }
                if keep { self.mode = Mode::Form(f); } else if matches!(self.mode, Mode::Form(_)) { self.mode = Mode::List; }
            }
            Mode::List => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(self.rows.len().saturating_sub(1)),
                    code::F5 => self.ask(env, "list", &["svc-list"]),
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('n') => {
                            let user = std::env::var("USER").unwrap_or_default();
                            self.mode = Mode::Form(Form::from_def(&ServiceDef { user, autostart: true, ..Default::default() }, false));
                        }
                        Some(c @ ('e' | 's' | 'x' | 'r' | 'd' | 'l')) => if let Some(row) = self.current().cloned() {
                            let n = row.name.as_str();
                            match c {
                                'e' => self.ask(env, "get", &["svc-get", n]),
                                's' => self.ask(env, "start", &["svc-start", n]),
                                'x' => self.ask(env, "stop", &["svc-stop", n]),
                                'r' => self.ask(env, "restart", &["svc-restart", n]),
                                'd' => self.mode = Mode::ConfirmRemove(row.name.clone()),
                                _ => { self.mode = Mode::Log(row.name.clone(), vec![]); self.ask(env, "log", &["svc-log", n, "200"]); }
                            }
                        },
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
        match &self.mode {
            Mode::Form(f) => {
                p.text(a.x, a.y, if f.editing { "Edit service" } else { "New service" }, t.accent, None);
                let mut y = a.y + 2;
                for i in 0..6 {
                    if y + 2 > foot { break; }
                    p.text(a.x, y, LABELS[i], t.dim, None);
                    p.field(a.x, y + 1, a.w.min(70), &f.fields[i], f.focus == i && focused, false);
                    y += 3;
                }
                if y < foot { p.clip(a.x, y, a.w, &format!("{} Start at boot: {}   (Space to change)", if f.focus == 6 { ">" } else { " " }, if f.autostart { "yes" } else { "no" }), if f.focus == 6 { t.accent } else { t.text }, t.panel); }
                p.clip(a.x, foot, a.w, "Tab Next box   Enter Next / Save   Esc Cancel", t.dim, t.panel);
                if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
            }
            Mode::Log(name, lines) => {
                p.text(a.x, a.y, &format!("Log of {name}"), t.accent, None);
                let h = a.h.saturating_sub(3);
                for (i, l) in lines.iter().skip(lines.len().saturating_sub(h)).enumerate() { p.clip(a.x, a.y + 2 + i, a.w, l, t.text, t.panel); }
                if lines.is_empty() { p.text(a.x, a.y + 2, if self.job.busy { "Loading..." } else { "(nothing logged yet)" }, t.dim, None); }
                p.clip(a.x, foot, a.w, "R Reload   Esc Back", t.dim, t.panel);
            }
            _ => {
                p.text(a.x, a.y, "Programs that run in the background", t.dim, None);
                if self.rows.is_empty() { p.text(a.x, a.y + 2, if self.job.busy { "Loading..." } else { "No services yet. Press N to add one." }, t.dim, None); }
                let items: Vec<String> = self.rows.iter().map(|r| format!("{:<20} {:<9} {}", r.name, r.status, if r.autostart { "starts at boot" } else { "manual" })).collect();
                let h = a.h.saturating_sub(4);
                let top = self.sel.saturating_sub(h.saturating_sub(1));
                p.list(Rect { x: a.x, y: a.y + 2, w: a.w, h }, &items, self.sel, top, focused);
                if let Mode::ConfirmRemove(n) = &self.mode { p.clip(a.x, foot - 1, a.w, &format!("Remove {n}? It will be stopped.  [Y] Yes   [N] No"), t.accent, t.panel); }
                let hint = "N New  E Edit  S Start  X Stop  R Restart  D Delete  L Log";
                p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
                if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
            }
        }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let (mode, focus) = match &self.mode { Mode::List => ("list", 0), Mode::Form(f) => ("form", f.focus), Mode::Log(..) => ("log", 0), Mode::ConfirmRemove(_) => ("confirm", 0) };
        format!("name=services mode={mode} focus={focus} count={} sel={} status={} busy={} msg={}", self.rows.len(), n(self.current().map_or("", |r| r.name.as_str())), n(self.current().map_or("", |r| r.status.as_str())), self.job.busy, n(&self.msg))
    }
    fn hint(&self) -> &'static str { "N New  E Edit  S Start  X Stop  R Restart  D Delete  L Log   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::Mutex;

    struct World { calls: Mutex<Vec<String>>, rows: Mutex<String> }
    fn app() -> (Services, Arc<World>) {
        let w = Arc::new(World { calls: Mutex::new(vec![]), rows: Mutex::new("bot running true\nweb stopped false\n".into()) });
        let w2 = w.clone();
        let be: Backend = Arc::new(move |req: &[String]| {
            w2.calls.lock().unwrap().push(req.join(" | "));
            match req[0].as_str() { "svc-list" => Ok(w2.rows.lock().unwrap().clone()), "svc-get" => Ok(services::render_conf(&ServiceDef { name: req[1].clone(), command: "python3 bot.py".into(), user: "tester".into(), dir: "/srv".into(), env: vec![("A".into(), "1".into())], autostart: true, ..Default::default() })), "svc-log" => Ok("l1\nl2".into()), "svc-start" if req[1] == "web" => Err("web failed".into()), _ => Ok(String::new()) }
        });
        (Services::new(be), w)
    }
    use std::sync::Arc;
    fn settle(s: &mut Services) { let e = Env::for_tests(); for _ in 0..200 { s.tick(&e); if !s.job.busy { s.tick(&e); if !s.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(s: &mut Services, k: &str) { s.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(s); }
    fn text(s: &mut Services, t: &str) { for c in t.chars() { key(s, &c.to_string()); } }
    fn calls(w: &World) -> Vec<String> { w.calls.lock().unwrap().clone() }

    #[test]
    fn the_list_is_loaded_and_navigable() {
        let (mut s, _w) = app(); settle(&mut s);
        assert_eq!(s.rows, vec![Row { name: "bot".into(), status: "running".into(), autostart: true }, Row { name: "web".into(), status: "stopped".into(), autostart: false }]);
        key(&mut s, "down");
        assert!(s.state().contains("sel=web") && s.state().contains("status=stopped"));
        key(&mut s, "down"); assert!(s.state().contains("sel=web"), "stays on the last row");
    }

    #[test]
    fn start_stop_restart_and_errors_reach_the_screen() {
        let (mut s, w) = app(); settle(&mut s);
        key(&mut s, "x"); key(&mut s, "r");
        assert!(calls(&w).contains(&"svc-stop | bot".to_string()) && calls(&w).contains(&"svc-restart | bot".to_string()));
        key(&mut s, "down"); key(&mut s, "s");
        assert!(s.msg.contains("web failed"), "{}", s.msg);
    }

    #[test]
    fn adding_a_service_through_the_form() {
        let (mut s, w) = app(); settle(&mut s);
        key(&mut s, "n");
        assert!(s.state().contains("mode=form"));
        text(&mut s, "mc"); key(&mut s, "enter");
        text(&mut s, "java -jar a.jar"); key(&mut s, "enter");
        text(&mut s, "/srv/mc"); key(&mut s, "enter");
        for _ in 0..40 { key(&mut s, "backspace"); } // the user box starts with $USER: empty it
        key(&mut s, "enter");
        text(&mut s, "A=1;B=two"); key(&mut s, "enter");
        text(&mut s, "8080 9090/udp"); key(&mut s, "enter");
        key(&mut s, "space"); key(&mut s, "space");   // toggles twice: stays on
        key(&mut s, "enter");
        assert!(s.msg.contains("valid user") || s.msg.contains("user"), "an empty user is refused: {}", s.msg);
        key(&mut s, "up"); key(&mut s, "up"); key(&mut s, "up");
        text(&mut s, "tester"); key(&mut s, "tab"); key(&mut s, "tab"); key(&mut s, "tab"); key(&mut s, "enter");
        let saved = calls(&w).into_iter().find(|c| c.starts_with("svc-save")).expect("save was sent");
        let d = services::parse_conf(&saved.split(" | ").nth(1).unwrap()).unwrap();
        assert_eq!((d.name.as_str(), d.command.as_str(), d.dir.as_str(), d.user.as_str(), d.autostart), ("mc", "java -jar a.jar", "/srv/mc", "tester", true));
        assert_eq!(d.env, vec![("A".to_string(), "1".to_string()), ("B".to_string(), "two".to_string())]);
        assert_eq!(d.ports, "8080/tcp,9090/udp");
        assert!(s.state().contains("mode=list"));
    }

    #[test]
    fn bad_input_stays_in_the_form_with_a_message() {
        let (mut s, _w) = app(); settle(&mut s);
        key(&mut s, "n"); text(&mut s, "Bad Name");
        for _ in 0..7 { key(&mut s, "enter"); }
        assert!(s.state().contains("mode=form") && !s.msg.is_empty(), "{}", s.msg);
        key(&mut s, "esc"); assert!(s.state().contains("mode=list"));
    }

    #[test]
    fn editing_removing_and_reading_the_log() {
        let (mut s, w) = app(); settle(&mut s);
        key(&mut s, "e");
        match &s.mode { Mode::Form(f) => assert_eq!((f.fields[0].as_str(), f.fields[1].as_str(), f.fields[4].as_str(), f.editing), ("bot", "python3 bot.py", "A=1", true)), m => panic!("{m:?}") }
        key(&mut s, "backspace"); // the name cannot be edited
        match &s.mode { Mode::Form(f) => assert_eq!(f.fields[0], "bot"), _ => unreachable!() }
        key(&mut s, "esc");
        key(&mut s, "l"); assert!(s.state().contains("mode=log"));
        match &s.mode { Mode::Log(_, l) => assert_eq!(l, &vec!["l1".to_string(), "l2".to_string()]), _ => panic!() }
        key(&mut s, "esc");
        key(&mut s, "d"); assert!(s.state().contains("mode=confirm"));
        key(&mut s, "n"); assert!(!calls(&w).iter().any(|c| c.starts_with("svc-remove")));
        key(&mut s, "d"); key(&mut s, "y");
        assert!(calls(&w).contains(&"svc-remove | bot".to_string()));
    }

    #[test]
    fn a_missing_helper_is_explained() {
        let be: Backend = Arc::new(|_| Err("The system helper is not running.".into()));
        let mut s = Services::new(be); settle(&mut s);
        assert!(s.msg.contains("helper"), "{}", s.msg);
    }

    #[test]
    fn draws_every_screen_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut s, _w) = app(); settle(&mut s);
        for step in 0..4 {
            match step { 1 => key(&mut s, "n"), 2 => { key(&mut s, "esc"); key(&mut s, "l"); } 3 => { key(&mut s, "esc"); key(&mut s, "d"); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) };
                s.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
