//! The browser window. It reads pages with the small engine in `web.rs`: text, links, lists, tables and downloads,
//! and only sites on the allowed list.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use crate::web::{self, Doc, Style};
use std::time::Duration;

pub const HOME: &str = "about:home";

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mode { View, Address(String), Allow(String), Download { url: String, name: String, size: Option<u64> }, Working(String) }

pub struct Browser {
    web: Backend,
    admin: Backend,
    job: Job,
    mode: Mode,
    url: String,
    history: Vec<String>,
    raw: Raw,
    doc: Doc,
    width: usize,
    scroll: usize,
    sel: Option<usize>,
    msg: String,
    sites: Vec<String>,
    rows: usize,
    first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Raw { Html(String), Text(String) }

/// The answer of the `fetch` request, as the real backend and the tests write it.
pub fn serialize(n: &web::Nav) -> String {
    match n {
        web::Nav::Page { url, html } => format!("PAGE\n{url}\n{html}"),
        web::Nav::Text { url, text } => format!("TEXT\n{url}\n{text}"),
        web::Nav::File { url, name, size } => format!("FILE\n{url}\n{name}\n{}", size.map_or("-".to_string(), |s| s.to_string())),
    }
}

/// The backend that really goes to the web (curl) and reads the allowed list afresh each time.
pub fn web_backend(downloads: std::path::PathBuf) -> Backend {
    std::sync::Arc::new(move |w: &[String]| {
        let list = web::parse_allow(&std::fs::read_to_string(web::ALLOW_FILE).unwrap_or_default());
        match w.first().map(String::as_str) {
            Some("sites") => Ok(list.join("\n")),
            Some("fetch") => web::navigate(&web::Curl, &list, w.get(1).map_or("", String::as_str)).map(|n| serialize(&n)),
            Some("download") => web::download(&web::Curl, &list, w.get(1).map_or("", String::as_str), &downloads),
            _ => Err("unknown request".into()),
        }
    })
}

impl Browser {
    pub fn new(web: Backend, admin: Backend) -> Browser {
        Browser { web, admin, job: Job::new(), mode: Mode::View, url: HOME.into(), history: vec![], raw: Raw::Html(String::new()), doc: Doc::default(), width: 0, scroll: 0, sel: None, msg: String::new(), sites: vec![], rows: 20, first: true }
    }

    fn home_base() -> web::Url { web::Url { host: "home.exec85".into(), path: "/".into() } }

    fn base(&self) -> web::Url { web::parse_url(&self.url).unwrap_or_else(Browser::home_base) }

    fn layout(&mut self, width: usize) {
        if width == self.width && !self.doc.lines.is_empty() { return; }
        self.width = width;
        self.doc = match &self.raw { Raw::Html(h) => web::render(h, width, &self.base()), Raw::Text(t) => web::render_text(t, width) };
    }

    fn show_home(&mut self) {
        self.url = HOME.into();
        self.raw = Raw::Html(web::home_page(&self.sites));
        self.width = 0; self.doc = Doc::default(); self.scroll = 0; self.sel = None; self.mode = Mode::View;
        let w = if self.width == 0 { 80 } else { self.width };
        self.layout(w);
    }

    fn go(&mut self, env: &Env, target: &str) {
        if target == HOME { self.history.push(self.url.clone()); self.show_home(); return; }
        let Some(u) = web::parse_url(target) else { self.msg = "That is not a web address I can open (it needs a name like example.org).".into(); return };
        let target = u.text();
        let target = target.as_str();
        if self.job.start(env, &self.web, "fetch", vec!["fetch".into(), target.to_string()]) { self.mode = Mode::Working(target.to_string()); self.msg = format!("Opening {target} ..."); }
        else { self.msg = "Please wait, still loading.".into(); }
    }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("sites", Ok(t)) => { self.sites = t.lines().map(String::from).filter(|l| !l.is_empty()).collect(); if self.url == HOME { self.show_home(); } }
            ("sites", Err(_)) => {}
            ("fetch", Ok(t)) => {
                let mut it = t.splitn(3, '\n');
                let (kind, url, rest) = (it.next().unwrap_or(""), it.next().unwrap_or("").to_string(), it.next().unwrap_or(""));
                match kind {
                    "PAGE" | "TEXT" => {
                        if self.url != HOME || !self.history.is_empty() || true { self.history.push(self.url.clone()); }
                        self.url = url;
                        self.raw = if kind == "PAGE" { Raw::Html(rest.to_string()) } else { Raw::Text(rest.to_string()) };
                        self.doc = Doc::default(); self.width = 0; self.scroll = 0; self.sel = None; self.mode = Mode::View; self.msg.clear();
                        let w = 80; self.layout(w);
                        if let Raw::Html(_) = self.raw { if !self.doc.title.is_empty() { self.msg.clear(); } }
                    }
                    "FILE" => {
                        let mut f = rest.splitn(2, '\n');
                        let (name, size) = (f.next().unwrap_or("").to_string(), f.next().and_then(|s| s.trim().parse::<u64>().ok()));
                        self.mode = Mode::Download { url, name, size }; self.msg.clear();
                    }
                    _ => { self.mode = Mode::View; self.msg = "The page came back in a form I do not understand.".into(); }
                }
            }
            ("fetch", Err(e)) => { self.mode = Mode::View; self.msg = e; }
            ("download", Ok(m)) => { self.mode = Mode::View; self.msg = m; }
            ("download", Err(e)) => { self.mode = Mode::View; self.msg = e; }
            ("allow", Ok(m)) => { self.mode = Mode::View; self.msg = m; self.refresh_sites(env); }
            ("allow", Err(e)) => { self.mode = Mode::View; self.msg = e; }
            _ => {}
        }
    }

    fn refresh_sites(&mut self, env: &Env) { self.job.start(env, &self.web, "sites", vec!["sites".into()]); }

    fn max_scroll(&self) -> usize { self.doc.lines.len().saturating_sub(self.rows.max(1)) }

    fn scroll_to_sel(&mut self) {
        if let Some(l) = self.sel.and_then(|s| self.doc.link_line(s)) {
            let rows = self.rows.max(1);
            if l < self.scroll { self.scroll = l; } else if l >= self.scroll + rows { self.scroll = l + 1 - rows; }
        }
    }
}

impl Application for Browser {
    fn kind(&self) -> Kind { Kind::Browser }
    fn title(&self) -> String { if self.doc.title.is_empty() { "Browser".into() } else { format!("Browser - {}", self.doc.title) } }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(300) } else { Duration::from_secs(3600) }) }
    fn resize(&mut self, _cols: usize, rows: usize) { self.rows = rows.saturating_sub(3).max(1); }
    fn tick(&mut self, env: &Env) {
        if self.first { self.first = false; self.show_home(); self.refresh_sites(env); }
        self.finish(env);
    }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.mode.clone() {
            Mode::Working(_) => { if k.code == code::ESC { self.msg = "Still loading; it will appear when ready.".into(); } }
            Mode::Address(mut t) => match k.code {
                code::ESC => self.mode = Mode::View,
                code::ENTER => { let target = t.trim().to_string(); self.mode = Mode::View; if !target.is_empty() { self.go(env, &target); } }
                _ => { edit_text(&mut t, &k, 300); self.mode = Mode::Address(t); }
            },
            Mode::Allow(mut t) => match k.code {
                code::ESC => self.mode = Mode::View,
                code::ENTER => {
                    let d = t.trim().to_ascii_lowercase();
                    if !web::valid_allow_entry(&d) { self.msg = "Type just the site's name, like example.org.".into(); self.mode = Mode::View; }
                    else if self.job.start(env, &self.admin, "allow", vec!["site-add".into(), d]) { self.mode = Mode::Working("allowing".into()); self.msg = "Allowing the site ...".into(); }
                }
                _ => { edit_text(&mut t, &k, 100); self.mode = Mode::Allow(t); }
            },
            Mode::Download { url, name, size } => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => { if self.job.start(env, &self.web, "download", vec!["download".into(), url.clone()]) { self.mode = Mode::Working(format!("downloading {name}")); self.msg = format!("Downloading {name} ... (this can take a while)"); } }
                Some('n') => { self.mode = Mode::View; self.msg.clear(); }
                _ => if k.code == code::ESC { self.mode = Mode::View; self.msg.clear(); } else { self.mode = Mode::Download { url, name, size }; },
            },
            Mode::View => {
                let page = self.rows.max(1);
                match k.code {
                    code::UP => self.scroll = self.scroll.saturating_sub(1),
                    code::DOWN => self.scroll = (self.scroll + 1).min(self.max_scroll()),
                    code::PGUP => self.scroll = self.scroll.saturating_sub(page.saturating_sub(1).max(1)),
                    code::PGDN | code::SPACE => self.scroll = (self.scroll + page.saturating_sub(1).max(1)).min(self.max_scroll()),
                    code::HOME => self.scroll = 0,
                    code::END => self.scroll = self.max_scroll(),
                    code::TAB => {
                        let n = self.doc.links.len();
                        if n > 0 { self.sel = Some(match (self.sel, k.shift) { (None, false) => 0, (None, true) => n - 1, (Some(i), false) => (i + 1) % n, (Some(i), true) => (i + n - 1) % n }); self.scroll_to_sel(); }
                    }
                    code::ENTER => if let Some(l) = self.sel.and_then(|s| self.doc.links.get(s)).cloned() { self.go(env, &l); } else { self.msg = "Press Tab to choose a link first.".into(); },
                    code::BACKSPACE | code::LEFT => if let Some(prev) = self.history.pop() { if prev == HOME { self.show_home(); } else { let h = self.history.len(); self.go(env, &prev); self.history.truncate(h); } },
                    _ => match k.ch().map(|c| c.to_ascii_lowercase()) {
                        Some('g') => self.mode = Mode::Address(String::new()),
                        Some('h') => self.go(env, HOME),
                        Some('r') => { let u = self.url.clone(); if u != HOME { let h = self.history.len(); self.go(env, &u); self.history.truncate(h); } }
                        Some('+') => self.mode = Mode::Allow(String::new()),
                        Some('q') => return Out::Close,
                        _ => {}
                    },
                }
                if k.ctrl && k.ch().is_none() { /* keep */ }
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        // address line
        match &self.mode {
            Mode::Address(s) => { p.text(a.x, a.y, "Go to:", t.accent, None); p.field(a.x + 7, a.y, a.w.saturating_sub(7).max(1), s, focused, false); }
            Mode::Allow(s) => { p.text(a.x, a.y, "Allow site:", t.accent, None); p.field(a.x + 12, a.y, a.w.saturating_sub(12).max(1), s, focused, false); }
            _ => { p.clip(a.x, a.y, a.w, &format!(" {}", self.url), t.accent_text, t.accent); }
        }
        // the page (laid out for this width if the window was resized since)
        let fresh;
        let doc: &Doc = if a.w == self.width || self.width == 0 { &self.doc } else { fresh = match &self.raw { Raw::Html(h) => web::render(h, a.w, &self.base()), Raw::Text(x) => web::render_text(x, a.w) }; &fresh };
        let body_rows = a.h.saturating_sub(3);
        for i in 0..body_rows {
            let Some(line) = doc.lines.get(self.scroll + i) else { break };
            let mut col = a.x;
            for s in line {
                if col >= a.x + a.w { break; }
                let selected = s.link.is_some() && s.link == self.sel;
                let (fg, bg) = match (s.link, s.style) {
                    (Some(_), _) if selected => (t.accent_text, Some(t.accent)),
                    (Some(_), _) => (t.title_on, None),
                    (None, Style::Heading) => (t.accent, None),
                    (None, Style::Dim) | (None, Style::Quote) => (t.dim, None),
                    _ => (t.text, None),
                };
                p.text(col, a.y + 2 + i, &s.text, fg, bg);
                col += s.text.chars().count();
            }
        }
        if doc.lines.len() > body_rows && body_rows > 0 {
            let pos = self.scroll * body_rows.saturating_sub(1) / self.max_scroll().max(1);
            p.text(a.x + a.w.saturating_sub(1), a.y + 2 + pos.min(body_rows - 1), "#", t.dim, None);
        }
        // the bottom line
        let line = match &self.mode {
            Mode::Download { name, size, .. } => format!("Download {name} ({})?  [Y] Yes  [N] No", size.map_or("size unknown".to_string(), web::human_size)),
            Mode::Working(_) => if self.msg.is_empty() { "Please wait ...".into() } else { self.msg.clone() },
            _ => if !self.msg.is_empty() { self.msg.clone() } else { "Tab Links  Enter Open  G Address  Backspace Back  H Home  + Allow site".into() },
        };
        let colour = if matches!(self.mode, Mode::Download { .. }) || !self.msg.is_empty() { t.accent } else { t.dim };
        p.clip(a.x, foot, a.w, &line, colour, t.panel);
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let mode = match &self.mode { Mode::View => "view", Mode::Address(_) => "address", Mode::Allow(_) => "allow", Mode::Download { .. } => "download", Mode::Working(_) => "working" };
        format!("name=browser mode={mode} url={} title={} lines={} links={} sel={} scroll={} sites={} busy={} msg={}", n(&self.url), n(&self.doc.title), self.doc.lines.len(), self.doc.links.len(),
            self.sel.map_or("-".to_string(), |s| s.to_string()), self.scroll, self.sites.len(), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "Tab Links   Enter Open   G Address   Backspace Back   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app() -> (Browser, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let web: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match (r[0].as_str(), r.get(1).map(String::as_str)) {
                ("sites", _) => Ok("example.org\nfiles.example.net\n".into()),
                ("fetch", Some("https://example.org/")) => Ok(web::Nav::Page { url: "https://example.org/".into(), html: "<title>Example</title><h1>Hi</h1><p>text <a href='/two'>second</a> and <a href='https://files.example.net/tool.iso'>tool</a></p>".into() }).map(|n| serialize(&n)),
                ("fetch", Some("https://example.org/two")) => Ok(serialize(&web::Nav::Text { url: "https://example.org/two".into(), text: "plain page".into() })),
                ("fetch", Some("https://files.example.net/tool.iso")) => Ok(serialize(&web::Nav::File { url: "https://files.example.net/tool.iso".into(), name: "tool.iso".into(), size: Some(5_000_000) })),
                ("fetch", Some(u)) => Err(format!("{u} is not on the list of allowed sites.")),
                ("download", _) => Ok("Saved tool.iso (4.8 MB) in /home/x/Downloads".into()),
                _ => Err("unknown".into()),
            }
        });
        let admin: Backend = { let c3 = calls.clone(); Arc::new(move |r: &[String]| { c3.lock().unwrap().push(r.join(" ")); Ok(format!("{} is now allowed.", r[1])) }) };
        (Browser::new(web, admin), calls)
    }
    fn settle(b: &mut Browser) { let e = Env::for_tests(); for _ in 0..300 { b.tick(&e); if !b.job.busy { std::thread::sleep(Duration::from_millis(5)); b.tick(&e); if !b.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(b: &mut Browser, k: &str) { b.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(b); }
    fn text(b: &mut Browser, t: &str) { for c in t.chars() { key(b, &c.to_string()); } }

    #[test]
    fn it_starts_on_the_home_page_listing_the_allowed_sites() {
        let (mut b, _c) = app(); settle(&mut b);
        let s = b.state();
        assert!(s.contains("url=about:home") && s.contains("sites=2") && s.contains("links=2"), "{s}");
    }

    #[test]
    fn typing_an_address_opens_the_page_and_tab_enter_follows_links() {
        let (mut b, calls) = app(); settle(&mut b);
        key(&mut b, "g"); assert!(b.state().contains("mode=address"));
        text(&mut b, "example.org"); key(&mut b, "enter");
        assert!(b.state().contains("url=https://example.org/") && b.state().contains("title=Example") && b.state().contains("links=2"), "{}", b.state());
        key(&mut b, "tab"); assert!(b.state().contains("sel=0"));
        key(&mut b, "enter"); assert!(b.state().contains("url=https://example.org/two"), "{}", b.state());
        assert!(calls.lock().unwrap().iter().any(|c| c == "fetch https://example.org/two"));
        key(&mut b, "backspace"); assert!(b.state().contains("url=https://example.org/ "), "back goes to the page before: {}", b.state());
        key(&mut b, "h"); assert!(b.state().contains("url=about:home"));
    }

    #[test]
    fn a_forbidden_site_shows_the_reason_and_stays_on_the_current_page() {
        let (mut b, _c) = app(); settle(&mut b);
        key(&mut b, "g"); text(&mut b, "evil.com"); key(&mut b, "enter");
        assert!(b.state().contains("mode=view") && b.state().contains("url=about:home") && b.state().contains("not_on_the_list"), "{}", b.state());
    }

    #[test]
    fn a_file_asks_before_it_downloads() {
        let (mut b, calls) = app(); settle(&mut b);
        key(&mut b, "g"); text(&mut b, "example.org"); key(&mut b, "enter");
        key(&mut b, "tab"); key(&mut b, "tab"); key(&mut b, "enter");
        assert!(b.state().contains("mode=download"), "{}", b.state());
        key(&mut b, "n"); assert!(b.state().contains("mode=view") && !calls.lock().unwrap().iter().any(|c| c.starts_with("download")));
        key(&mut b, "enter"); assert!(b.state().contains("mode=download"));
        key(&mut b, "y"); assert!(calls.lock().unwrap().iter().any(|c| c == "download https://files.example.net/tool.iso"));
        assert!(b.state().contains("Saved_tool.iso"), "{}", b.state());
    }

    #[test]
    fn an_administrator_can_allow_another_site() {
        let (mut b, calls) = app(); settle(&mut b);
        key(&mut b, "+"); text(&mut b, "newsite.org"); key(&mut b, "enter");
        assert!(calls.lock().unwrap().iter().any(|c| c == "site-add newsite.org"), "{:?}", calls.lock().unwrap());
        key(&mut b, "+"); text(&mut b, "not a site"); key(&mut b, "enter");
        assert!(b.state().contains("just_the_site"), "{}", b.state());
    }

    #[test]
    fn it_scrolls_and_draws_at_every_size() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut b, _c) = app(); settle(&mut b);
        key(&mut b, "g"); text(&mut b, "example.org"); key(&mut b, "enter");
        b.resize(40, 8);
        for k in ["down", "down", "pgdn", "end", "home", "up", "tab"] { key(&mut b, k); }
        for step in 0..4 {
            match step { 1 => key(&mut b, "g"), 2 => { key(&mut b, "esc"); key(&mut b, "+"); } 3 => { key(&mut b, "esc"); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                b.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
