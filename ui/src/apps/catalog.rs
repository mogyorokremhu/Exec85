//! App catalog: one list of everything you can install, what it needs, and whether it is installed. Quick installers
//! (website, Syncthing, Transmission ...) are filled in and installed right here; the bigger ones (Minecraft, Discord bot)
//! have their own screens, and Enter opens them.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use crate::recipes::{Recipe, RECIPES};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Entry { Recipe(usize), App(Kind, &'static str, &'static str) }

const ENTRIES: [Entry; 5] = [
    Entry::App(Kind::Minecraft, "Minecraft server", "Vanilla, Paper or Fabric, with the version and memory you choose"),
    Entry::App(Kind::DiscordBot, "Discord bot", "Python or Node.js, with a starter bot or your own"),
    Entry::Recipe(0), Entry::Recipe(1), Entry::Recipe(2),
];

#[derive(Clone, Debug, PartialEq, Eq)]
enum Screen { List, Form(Vec<String>, usize), Working, Done, Confirm }

pub struct Catalog {
    be: Backend,
    job: Job,
    installed: Vec<String>,
    sel: usize,
    screen: Screen,
    msg: String,
    first: bool,
}

impl Catalog {
    pub fn new(be: Backend) -> Catalog { Catalog { be, job: Job::new(), installed: vec![], sel: 0, screen: Screen::List, msg: String::new(), first: true } }

    fn recipe(&self) -> Option<&'static Recipe> { match ENTRIES.get(self.sel) { Some(Entry::Recipe(i)) => RECIPES.get(*i), _ => None } }
    fn is_installed(&self, id: &str) -> bool { self.installed.iter().any(|i| i == id) }
    fn ask(&mut self, env: &Env, tag: &str, words: Vec<String>) { if !self.job.start(env, &self.be, tag, words) { self.msg = "Please wait, the last request is still running.".into(); } }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("list", Ok(t)) => self.installed = t.lines().filter_map(|l| { let w: Vec<&str> = l.split_whitespace().collect(); (w.len() == 2 && w[1] == "installed").then(|| w[0].to_string()) }).collect(),
            ("list", Err(e)) => self.msg = e,
            ("install", Ok(m)) => { self.screen = Screen::Done; self.msg = m; self.ask(env, "list", vec!["recipe-list".into()]); }
            ("install", Err(e)) => { self.msg = e; if let (Some(r), true) = (self.recipe(), true) { let vals = r.options.iter().map(|o| o.default.to_string()).collect(); self.screen = Screen::Form(vals, 0); } }
            ("remove", Ok(m)) => { self.msg = m; self.ask(env, "list", vec!["recipe-list".into()]); }
            ("remove", Err(e)) => self.msg = e,
            _ => {}
        }
    }
}

impl Application for Catalog {
    fn kind(&self) -> Kind { Kind::Catalog }
    fn title(&self) -> String { "App catalog".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(30) }) }
    fn tick(&mut self, env: &Env) { self.finish(env); if self.first { self.first = false; self.ask(env, "list", vec!["recipe-list".into()]); } }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        match self.screen.clone() {
            Screen::List => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(ENTRIES.len() - 1),
                    code::ENTER => match ENTRIES[self.sel] {
                        Entry::App(kind, ..) => return Out::Open(kind),
                        Entry::Recipe(i) => {
                            let r = &RECIPES[i];
                            if self.is_installed(r.id) { self.msg = format!("{} is installed. Press R to remove it.", r.title); }
                            else { self.screen = Screen::Form(r.options.iter().map(|o| o.default.to_string()).collect(), 0); }
                        }
                    },
                    _ => if k.ch().map(|c| c.to_ascii_lowercase()) == Some('r') { if let Some(r) = self.recipe() { if self.is_installed(r.id) { self.screen = Screen::Confirm; } else { self.msg = format!("{} is not installed.", r.title); } } },
                }
            }
            Screen::Confirm => match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => { self.screen = Screen::List; if let Some(r) = self.recipe() { self.ask(env, "remove", vec!["recipe-remove".into(), r.id.to_string()]); } }
                Some('n') => self.screen = Screen::List,
                _ => if k.code == code::ESC { self.screen = Screen::List; },
            },
            Screen::Form(mut vals, mut focus) => {
                let Some(r) = self.recipe() else { self.screen = Screen::List; return Out::None };
                let n = r.options.len();
                match k.code {
                    code::ESC => self.screen = Screen::List,
                    code::TAB | code::DOWN => { focus = (focus + 1) % (n + 1); self.screen = Screen::Form(vals, focus); }
                    code::UP => { focus = (focus + n) % (n + 1); self.screen = Screen::Form(vals, focus); }
                    code::ENTER => {
                        if focus < n { focus += 1; self.screen = Screen::Form(vals, focus); }
                        else {
                            let user = std::env::var("USER").unwrap_or_default();
                            let mut words = vec!["recipe-install".to_string(), r.id.to_string(), user];
                            for (o, v) in r.options.iter().zip(vals.iter()) { words.push(format!("{}={}", o.key, v.trim())); }
                            self.screen = Screen::Working; self.msg = "Installing. This can take a few minutes...".into();
                            self.ask(env, "install", words);
                        }
                    }
                    _ => { if focus < n { edit_text(&mut vals[focus], &k, 120); } self.screen = Screen::Form(vals, focus); }
                }
            }
            Screen::Working => {}
            Screen::Done => if k.code == code::ENTER || k.code == code::ESC { self.screen = Screen::List; self.msg.clear(); },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        match &self.screen {
            Screen::List | Screen::Confirm => {
                p.text(a.x, a.y, "Everything you can install", t.dim, None);
                let items: Vec<String> = ENTRIES.iter().map(|e| match e {
                    Entry::App(_, title, _) => format!("{:<18} opens its own screen", title),
                    Entry::Recipe(i) => format!("{:<18} {}", RECIPES[*i].title, if self.is_installed(RECIPES[*i].id) { "installed" } else { "not installed" }),
                }).collect();
                p.list(Rect { x: a.x, y: a.y + 2, w: a.w, h: ENTRIES.len() }, &items, self.sel, 0, focused && self.screen == Screen::List);
                let (blurb, needs) = match ENTRIES[self.sel] { Entry::App(_, _, b) => (b.to_string(), String::new()), Entry::Recipe(i) => (RECIPES[i].blurb.to_string(), format!("Needs: {}", RECIPES[i].needs)) };
                let mut y = a.y + 3 + ENTRIES.len();
                for l in super::wrap_text(&blurb, a.w) { if y < foot.saturating_sub(2) { p.clip(a.x, y, a.w, &l, t.text, t.panel); y += 1; } }
                if !needs.is_empty() { for l in super::wrap_text(&needs, a.w) { if y < foot.saturating_sub(2) { p.clip(a.x, y, a.w, &l, t.dim, t.panel); y += 1; } } }
                if self.screen == Screen::Confirm { p.clip(a.x, foot.saturating_sub(1), a.w, "Remove it? It stops; your files are kept.  [Y] Yes  [N] No", t.accent, t.panel); }
                p.clip(a.x, foot, a.w, "Up/Down Choose   Enter Install / Open   R Remove", t.dim, t.panel);
            }
            Screen::Form(vals, focus) => {
                let Some(r) = self.recipe() else { return };
                p.text(a.x, a.y, &format!("Install {}", r.title), t.accent, None);
                for (i, o) in r.options.iter().enumerate() {
                    let y = a.y + 2 + i * 3;
                    if y + 2 > foot { break; }
                    p.text(a.x, y, o.label, t.dim, None);
                    p.field(a.x, y + 1, a.w.min(60), &vals[i], *focus == i && focused, o.secret);
                }
                let y = a.y + 2 + r.options.len() * 3;
                if y < foot { p.clip(a.x, y, 14, "  Install  ", if *focus == r.options.len() { t.accent_text } else { t.text }, if *focus == r.options.len() { t.accent } else { t.panel }); }
                p.clip(a.x, foot, a.w, "Tab Next box   Enter Next / Install   Esc Cancel", t.dim, t.panel);
            }
            Screen::Working => { p.text(a.x, a.y, "Installing...", t.accent, None); p.clip(a.x, foot, a.w, "Please wait...", t.dim, t.panel); }
            Screen::Done => {
                p.text(a.x, a.y, "Installed", t.accent, None);
                let mut y = a.y + 2;
                for l in super::wrap_text(&self.msg, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.text, t.panel); y += 1; } }
                p.clip(a.x, foot, a.w, "Enter Back to the list", t.dim, t.panel);
            }
        }
        if !matches!(self.screen, Screen::Done) && !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let screen = match &self.screen { Screen::List => "list", Screen::Form(..) => "form", Screen::Working => "working", Screen::Done => "done", Screen::Confirm => "confirm" };
        let id = match ENTRIES[self.sel] { Entry::Recipe(i) => RECIPES[i].id, Entry::App(_, t, _) => if t.starts_with("Minecraft") { "minecraft" } else { "discordbot" } };
        format!("name=catalog screen={screen} entries={} sel={id} installed={} busy={} msg={}", ENTRIES.len(), n(&self.installed.join(",")), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "Up/Down Choose   Enter Install / Open   R Remove   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app() -> (Catalog, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() { "recipe-list" => Ok("website installed\nsyncthing no\ntransmission no\n".into()), "recipe-install" if r[1] == "transmission" => Err("The web page password needs 8 or more characters.".into()), "recipe-install" => Ok("Syncthing is starting.".into()), _ => Ok("Removed.".into()) }
        });
        (Catalog::new(be), calls)
    }
    fn settle(m: &mut Catalog) { let e = Env::for_tests(); for _ in 0..300 { m.tick(&e); if !m.job.busy { std::thread::sleep(Duration::from_millis(5)); m.tick(&e); if !m.job.busy { return; } } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut Catalog, k: &str) -> Out { let o = m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); o }
    fn text(m: &mut Catalog, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn it_lists_everything_with_what_is_installed() {
        let (mut m, _c) = app(); settle(&mut m);
        assert!(m.state().contains("entries=5") && m.state().contains("installed=website"), "{}", m.state());
    }

    #[test]
    fn the_bigger_installers_open_their_own_screens() {
        let (mut m, _c) = app(); settle(&mut m);
        assert_eq!(key(&mut m, "enter"), Out::Open(Kind::Minecraft));
        key(&mut m, "down"); assert_eq!(key(&mut m, "enter"), Out::Open(Kind::DiscordBot));
    }

    #[test]
    fn a_quick_installer_is_filled_in_and_sent_with_a_hidden_password() {
        let (mut m, calls) = app(); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "down");          // Syncthing (the website, row 3, is installed)
        assert!(m.state().contains("sel=syncthing"));
        key(&mut m, "enter"); assert!(m.state().contains("screen=form"));
        key(&mut m, "enter"); key(&mut m, "enter"); text(&mut m, "longenough1"); key(&mut m, "enter"); key(&mut m, "enter");
        let c = calls.lock().unwrap().iter().find(|c| c.starts_with("recipe-install")).cloned().unwrap();
        assert!(c.starts_with("recipe-install syncthing ") && c.contains("folder=/srv/syncthing") && c.contains("gui_user=admin") && c.contains("gui_password=longenough1"), "{c}");
        assert!(!m.state().contains("longenough1"), "the password is never in the state line");
        assert!(m.state().contains("screen=done"), "{}", m.state());
        key(&mut m, "enter"); assert!(m.state().contains("screen=list"));
    }

    #[test]
    fn an_installed_one_cannot_be_installed_twice_but_can_be_removed_after_asking() {
        let (mut m, calls) = app(); settle(&mut m);
        key(&mut m, "down"); key(&mut m, "down");                               // Website: installed
        key(&mut m, "enter"); assert!(m.msg.contains("is installed") && m.state().contains("screen=list"), "{}", m.msg);
        key(&mut m, "r"); assert!(m.state().contains("screen=confirm"));
        key(&mut m, "n"); assert!(!calls.lock().unwrap().iter().any(|c| c.starts_with("recipe-remove")));
        key(&mut m, "r"); key(&mut m, "y"); assert!(calls.lock().unwrap().contains(&"recipe-remove website".to_string()));
        key(&mut m, "down"); key(&mut m, "r"); assert!(m.msg.contains("not installed"), "{}", m.msg);
    }

    #[test]
    fn a_refused_install_comes_back_to_the_form_with_the_reason() {
        let (mut m, _c) = app(); settle(&mut m);
        for _ in 0..4 { key(&mut m, "down"); }                                  // Transmission
        key(&mut m, "enter"); for _ in 0..5 { key(&mut m, "enter"); }
        assert!(m.state().contains("screen=form") && m.msg.contains("8 or more"), "{} {}", m.state(), m.msg);
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut m, _c) = app(); settle(&mut m);
        for step in 0..4 {
            match step { 1 => { key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter"); } 2 => { key(&mut m, "esc"); key(&mut m, "up"); key(&mut m, "up"); key(&mut m, "r"); } 3 => { key(&mut m, "n"); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
