//! Software: search for programs, install them, remove them and update everything (a front end for `apk`).
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pkg { pub name: String, pub version: String, pub desc: String }

/// `nginx-1.28.0-r1 - HTTP and reverse proxy server` or just `nginx-1.28.0-r1`.
pub fn parse_pkg(line: &str) -> Option<Pkg> {
    let (full, desc) = match line.split_once(" - ") { Some((a, b)) => (a.trim(), b.trim()), None => (line.trim(), "") };
    if full.is_empty() || full.contains(' ') { return None; }
    let parts: Vec<&str> = full.rsplitn(3, '-').collect(); // [release, version, name]
    if parts.len() < 3 || !parts[1].starts_with(|c: char| c.is_ascii_digit()) { return None; }
    Some(Pkg { name: parts[2].to_string(), version: format!("{}-{}", parts[1], parts[0]), desc: desc.to_string() })
}

pub fn parse_pkgs(text: &str) -> Vec<Pkg> { text.lines().filter_map(parse_pkg).collect() }

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { Browse, Search(String), Confirm(String, char) }

pub struct Software {
    be: Backend,
    job: Job,
    installed: Vec<Pkg>,
    results: Vec<Pkg>,
    showing_results: bool,
    sel: usize,
    mode: Mode,
    msg: String,
    last_output: Vec<String>,
    first: bool,
}

impl Software {
    pub fn new(be: Backend) -> Software {
        Software { be, job: Job::new(), installed: vec![], results: vec![], showing_results: false, sel: 0, mode: Mode::Browse, msg: String::new(), last_output: vec![], first: true }
    }
    fn items(&self) -> &Vec<Pkg> { if self.showing_results { &self.results } else { &self.installed } }
    fn current(&self) -> Option<Pkg> { self.items().get(self.sel).cloned() }
    fn is_installed(&self, name: &str) -> bool { self.installed.iter().any(|p| p.name == name) }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str], note: &str) {
        if self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { self.msg = note.to_string(); } else { self.msg = "Please wait, the last request is still running.".into(); }
    }
    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        let out_lines = |t: &str| t.lines().rev().filter(|l| !l.trim().is_empty()).take(6).map(String::from).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>();
        match (tag.as_str(), r) {
            ("installed", Ok(t)) => { self.installed = parse_pkgs(&t); self.installed.sort_by(|a, b| a.name.cmp(&b.name)); if !self.showing_results { self.sel = self.sel.min(self.installed.len().saturating_sub(1)); } self.msg.clear(); }
            ("installed", Err(e)) => self.msg = e,
            ("search", Ok(t)) => { self.results = parse_pkgs(&t); self.showing_results = true; self.sel = 0; self.msg = format!("{} found.", self.results.len()); }
            ("search", Err(e)) => self.msg = e,
            (tag, Ok(t)) => { self.last_output = out_lines(&t); self.msg = format!("Done: {tag}."); self.ask(env, "installed", &["apk-installed"], "Done."); self.msg = format!("Done: {tag}."); }
            (tag, Err(e)) => { self.last_output = out_lines(&e); self.msg = format!("{tag} failed: {}", self.last_output.last().cloned().unwrap_or_default()); }
        }
    }
}

impl Application for Software {
    fn kind(&self) -> Kind { Kind::Software }
    fn title(&self) -> String { "Software".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(300) } else { Duration::from_secs(30) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first { self.first = false; self.ask(env, "installed", &["apk-installed"], "Loading..."); }
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Search(mut q) => {
                match k.code {
                    code::ESC => self.mode = Mode::Browse,
                    code::ENTER => { self.mode = Mode::Browse; if q.trim().len() < 2 { self.msg = "Type at least 2 letters or digits to search for.".into(); } else { self.ask(env, "search", &["apk-search", q.trim()], "Searching..."); } }
                    _ => { edit_text(&mut q, &k, 64); self.mode = Mode::Search(q); }
                }
            }
            Mode::Confirm(name, what) => {
                match k.ch().map(|c| c.to_ascii_lowercase()) {
                    Some('y') => { self.mode = Mode::Browse; match what { 'i' => self.ask(env, "install", &["apk-add", &name], "Installing, please wait..."), 'r' => self.ask(env, "remove", &["apk-del", &name], "Removing..."), _ => self.ask(env, "upgrade", &["apk-upgrade"], "Updating everything, please wait...") } }
                    Some('n') => self.mode = Mode::Browse,
                    _ => if k.code == code::ESC { self.mode = Mode::Browse; },
                }
            }
            Mode::Browse => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(self.items().len().saturating_sub(1)),
                    code::PGUP => self.sel = self.sel.saturating_sub(10),
                    code::PGDN => self.sel = (self.sel + 10).min(self.items().len().saturating_sub(1)),
                    code::TAB => { self.showing_results = !self.showing_results; self.sel = 0; }
                    code::ESC => { if self.showing_results { self.showing_results = false; self.sel = 0; } }
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('s') | Some('/') => { self.mode = Mode::Search(String::new()); }
                        Some('i') => if let Some(p) = self.current() {
                            if self.is_installed(&p.name) { self.msg = format!("{} is already installed.", p.name); } else { self.mode = Mode::Confirm(p.name, 'i'); }
                        },
                        Some('x') => if let Some(p) = self.current() { if self.is_installed(&p.name) { self.mode = Mode::Confirm(p.name, 'r'); } else { self.msg = format!("{} is not installed.", p.name); } },
                        Some('u') => self.ask(env, "update", &["apk-update"], "Refreshing the package list..."),
                        Some('g') => self.mode = Mode::Confirm("everything".into(), 'g'),
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
        p.text(a.x, a.y, if self.showing_results { "Search results" } else { "Installed programs" }, t.accent, None);
        p.right(a.x, a.y, a.w, &format!("{} items", self.items().len()), t.dim, t.panel);
        let rows = a.h.saturating_sub(if self.last_output.is_empty() { 5 } else { 5 + self.last_output.len().min(3) });
        let items: Vec<String> = self.items().iter().map(|k| format!("{:<24} {:<14} {}{}", k.name, k.version, if self.showing_results && self.is_installed(&k.name) { "[installed] " } else { "" }, k.desc)).collect();
        let top = self.sel.saturating_sub(rows.saturating_sub(1));
        p.list(Rect { x: a.x, y: a.y + 2, w: a.w, h: rows }, &items, self.sel, top, focused && matches!(self.mode, Mode::Browse));
        let mut y = a.y + 2 + rows;
        for l in self.last_output.iter().rev().take(3).rev() { if y < foot { p.clip(a.x, y, a.w, l, t.dim, t.panel); y += 1; } }
        match &self.mode {
            Mode::Search(q) => { p.text(a.x, foot - 1, "Search for:", t.dim, None); p.field(a.x + 12, foot - 1, a.w.saturating_sub(12).min(40), q, true, false); }
            Mode::Confirm(n, w) => p.clip(a.x, foot - 1, a.w, &match w { 'i' => format!("Install {n}?  [Y] Yes  [N] No"), 'r' => format!("Remove {n}?  [Y] Yes  [N] No"), _ => "Update all installed programs?  [Y] Yes  [N] No".to_string() }, t.accent, t.panel),
            Mode::Browse => {}
        }
        let hint = "S Search  I Install  X Remove  U Refresh list  G Update all  Tab Switch";
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::Browse => "browse", Mode::Search(_) => "search", Mode::Confirm(..) => "confirm" };
        format!("name=software mode={mode} view={} installed={} results={} sel={} busy={} msg={}", if self.showing_results { "results" } else { "installed" }, self.installed.len(), self.results.len(), n(&self.current().map_or(String::new(), |p| p.name)), self.job.busy, n(&self.msg))
    }
    fn hint(&self) -> &'static str { "S Search  I Install  X Remove  U Refresh  G Update all   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app() -> (Software, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![]));
        let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() {
                "apk-installed" => Ok("busybox-1.37.0-r18\nnginx-1.28.0-r1\n".into()),
                "apk-search" => Ok("nginx-1.28.0-r1 - HTTP server\npy3-pip-24.0-r2 - Python package installer\n".into()),
                "apk-add" if r[1] == "py3-pip" => Err("ERROR: unable to select packages".into()),
                _ => Ok("OK: 50 MiB in 80 packages".into()),
            }
        });
        (Software::new(be), calls)
    }
    fn settle(s: &mut Software) { let e = Env::for_tests(); for _ in 0..200 { s.tick(&e); if !s.job.busy { s.tick(&e); if !s.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(s: &mut Software, k: &str) { s.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(s); }
    fn text(s: &mut Software, t: &str) { for c in t.chars() { key(s, &c.to_string()); } }

    #[test]
    fn package_lines_are_split_into_name_version_description() {
        assert_eq!(parse_pkg("nginx-1.28.0-r1 - HTTP server").unwrap(), Pkg { name: "nginx".into(), version: "1.28.0-r1".into(), desc: "HTTP server".into() });
        assert_eq!(parse_pkg("py3-pip-24.0-r2").unwrap().name, "py3-pip");
        assert_eq!(parse_pkg("openjdk21-jre-headless-21.0.5_p11-r0 - JRE").unwrap().name, "openjdk21-jre-headless");
        assert!(parse_pkg("").is_none() && parse_pkg("WARNING: something odd").is_none() && parse_pkg("nodash").is_none());
    }

    #[test]
    fn lists_what_is_installed_and_searches() {
        let (mut s, calls) = app(); settle(&mut s);
        assert_eq!(s.installed.len(), 2);
        key(&mut s, "s"); text(&mut s, "web"); key(&mut s, "enter");
        assert!(calls.lock().unwrap().contains(&"apk-search web".to_string()));
        assert!(s.state().contains("view=results") && s.state().contains("results=2"));
        key(&mut s, "tab"); assert!(s.state().contains("view=installed"));
    }

    #[test]
    fn short_searches_are_refused_without_asking_the_helper() {
        let (mut s, calls) = app(); settle(&mut s);
        key(&mut s, "s"); text(&mut s, "a"); key(&mut s, "enter");
        assert!(s.msg.contains("at least 2"));
        assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("apk-search")));
    }

    #[test]
    fn install_asks_first_and_reports_failures() {
        let (mut s, calls) = app(); settle(&mut s);
        key(&mut s, "s"); text(&mut s, "py"); key(&mut s, "enter");
        key(&mut s, "down"); key(&mut s, "i");
        assert!(s.state().contains("mode=confirm"));
        key(&mut s, "n"); assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("apk-add")));
        key(&mut s, "i"); key(&mut s, "y");
        assert!(calls.lock().unwrap().contains(&"apk-add py3-pip".to_string()));
        assert!(s.msg.contains("failed") && s.msg.contains("unable"), "{}", s.msg);
        key(&mut s, "up"); key(&mut s, "i"); assert!(s.msg.contains("already installed"));
    }

    #[test]
    fn remove_and_update_all_ask_first() {
        let (mut s, calls) = app(); settle(&mut s);
        key(&mut s, "x"); key(&mut s, "y");
        assert!(calls.lock().unwrap().contains(&"apk-del busybox".to_string()));
        key(&mut s, "g"); key(&mut s, "n"); assert!(!calls.lock().unwrap().contains(&"apk-upgrade".to_string()));
        key(&mut s, "g"); key(&mut s, "y"); assert!(calls.lock().unwrap().contains(&"apk-upgrade".to_string()));
        key(&mut s, "u"); assert!(calls.lock().unwrap().contains(&"apk-update".to_string()));
    }

    #[test]
    fn draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode as M, Theme};
        let (mut s, _c) = app(); settle(&mut s);
        for step in 0..3 {
            match step { 1 => { key(&mut s, "s"); text(&mut s, "ng"); key(&mut s, "enter"); } 2 => key(&mut s, "i"), _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(M::Dark) };
                s.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
