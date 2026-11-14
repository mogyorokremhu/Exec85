//! Applications that run in the shell's tiled windows.
//!
//! Every app is a small state machine: it receives keys, may ask for a timer tick, and draws itself
//! into the rectangle the tiling layout gives it. Nothing in here touches the screen or the keyboard
//! device directly, so each app is testable on its own.
use crate::keys::Key;
use crate::layout::Rect;
use crate::paint::Painter;
pub mod calculator;
pub mod catalog;
pub mod clock;
pub mod discordbot;
pub mod diskusage;
pub mod minecraft;
pub mod monitor;
pub mod network;
pub mod notepad;
pub mod paint;
pub mod remote;
pub mod reversi;
pub mod services;
pub mod software;
pub mod snake;
pub mod terminal;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Notepad, Terminal, Paint, Monitor, DiskUsage, Network, Clock, Calculator, Reversi, Snake, Services, Software, Minecraft, DiscordBot, Remote, Catalog }

impl Kind {
    /// The app a shortcut file names ("app:terminal").
    pub fn parse(name: &str) -> Option<Kind> {
        [Kind::Notepad, Kind::Terminal, Kind::Paint, Kind::Monitor, Kind::DiskUsage, Kind::Network, Kind::Clock, Kind::Calculator, Kind::Reversi, Kind::Snake, Kind::Services, Kind::Software, Kind::Minecraft, Kind::DiscordBot, Kind::Remote, Kind::Catalog]
            .into_iter().find(|k| k.name() == name)
    }

    pub fn name(self) -> &'static str {
        match self {
            Kind::Notepad => "notepad", Kind::Terminal => "terminal", Kind::Paint => "paint", Kind::Monitor => "monitor",
            Kind::DiskUsage => "diskusage", Kind::Network => "network", Kind::Clock => "clock",
            Kind::Calculator => "calculator", Kind::Reversi => "reversi", Kind::Snake => "snake", Kind::Services => "services", Kind::Software => "software", Kind::Minecraft => "minecraft", Kind::DiscordBot => "discordbot", Kind::Remote => "remote", Kind::Catalog => "catalog",
        }
    }
}

/// What an app can see of the system around it.
pub struct Env {
    /// Call this from a background thread to make the shell redraw (e.g. new terminal output).
    pub wake: Arc<dyn Fn() + Send + Sync>,
    pub home: PathBuf,
    /// The folder the Explorer is showing when the app was opened.
    pub cwd: PathBuf,
}

impl Env {
    pub fn for_tests() -> Env { Env { wake: Arc::new(|| {}), home: std::env::temp_dir(), cwd: std::env::temp_dir() } }
}

/// What an app asks the shell to do after handling a key or a tick.
#[derive(Debug, PartialEq, Eq)]
pub enum Out { None, Close, Msg(String), OpenFile(PathBuf), Open(Kind) }

pub trait Application {
    fn kind(&self) -> Kind;
    fn title(&self) -> String;
    fn handle(&mut self, k: Key, env: &Env) -> Out;
    /// Draw into `a`, the window's inner area. `focused` is true for the window that receives keys.
    fn draw(&self, p: &mut Painter, a: Rect, focused: bool);
    /// How often `tick` should run (None = never; the shell then sleeps until the next key).
    fn interval(&self) -> Option<Duration> { None }
    fn tick(&mut self, _env: &Env) {}
    /// The window was given a new size (in cells).
    fn resize(&mut self, _cols: usize, _rows: usize) {}
    /// Asked before the window is closed; return false to keep it open (e.g. unsaved changes).
    fn try_close(&mut self) -> bool { true }
    /// One line of `key=value` pairs describing the app, printed for automated tests.
    fn state(&self) -> String;
    /// Key hints for the status bar while this window has the focus.
    fn hint(&self) -> &'static str { "F4 Close   F6 Next window   F10 Menu" }
}

/// A stand-in used until an app exists.
pub struct Placeholder(pub Kind);

impl Application for Placeholder {
    fn kind(&self) -> Kind { self.0 }
    fn title(&self) -> String { self.0.name().to_string() }
    fn handle(&mut self, _k: Key, _env: &Env) -> Out { Out::None }
    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) { let t = p.t; p.text(a.x, a.y, "Coming soon", t.dim, None); }
    fn state(&self) -> String { format!("name={}", self.0.name()) }
}

pub fn create(kind: Kind, env: &Env) -> Box<dyn Application> {
    match kind {
        Kind::Calculator => Box::new(calculator::Calculator::new()),
        Kind::Clock => Box::new(clock::Clock::new()),
        Kind::Notepad => Box::new(notepad::Notepad::new(env)),
        Kind::Paint => Box::new(paint::Paint::new(env)),
        Kind::Monitor => Box::new(monitor::Monitor::new()),
        Kind::Catalog => Box::new(catalog::Catalog::new(helper_backend())),
        Kind::Remote => Box::new(remote::Remote::new(helper_backend())),
        Kind::DiscordBot => Box::new(discordbot::DiscordBot::new(helper_backend())),
        Kind::Minecraft => Box::new(minecraft::Minecraft::new(helper_backend(), minecraft::Minecraft::system_memory_kb())),
        Kind::Services => Box::new(services::Services::new(helper_backend())),
        Kind::Software => Box::new(software::Software::new(helper_backend())),
        Kind::Network => Box::new(network::Network::new(helper_backend())),
        Kind::Terminal => Box::new(terminal::Terminal::open(env)),
        Kind::DiskUsage => Box::new(diskusage::DiskUsage::new(env)),
        Kind::Reversi => Box::new(reversi::Reversi::new()),
        Kind::Snake => Box::new(snake::Snake::new(40, 20)),
        other => Box::new(Placeholder(other)),
    }
}

/// Open a file in the editor.
pub fn open_file(path: PathBuf, env: &Env) -> Box<dyn Application> {
    Box::new(notepad::Notepad::open(path, env))
}

/// A request to the privileged helper (replaceable in tests).
pub type Backend = Arc<dyn Fn(&[String]) -> Result<String, String> + Send + Sync>;

pub fn helper_backend() -> Backend {
    Arc::new(|w: &[String]| { let r: Vec<&str> = w.iter().map(String::as_str).collect(); crate::helper::call(&r) })
}

/// One background request at a time: the screen keeps working while the helper is busy.
pub struct Job { slot: Arc<std::sync::Mutex<Option<(String, Result<String, String>)>>>, pub busy: bool }

impl Job {
    pub fn new() -> Job { Job { slot: Arc::new(std::sync::Mutex::new(None)), busy: false } }
    /// Start `words` on the backend; `tag` says what it was for. Returns false if one is already running.
    pub fn start(&mut self, env: &Env, be: &Backend, tag: &str, words: Vec<String>) -> bool {
        if self.busy { return false; }
        self.busy = true;
        let (slot, be, wake, tag) = (self.slot.clone(), be.clone(), env.wake.clone(), tag.to_string());
        std::thread::spawn(move || { let r = be(&words); *slot.lock().unwrap() = Some((tag, r)); wake(); });
        true
    }
    pub fn poll(&mut self) -> Option<(String, Result<String, String>)> {
        let r = self.slot.lock().unwrap().take();
        if r.is_some() { self.busy = false; }
        r
    }
}

/// Edit a line of text with the keyboard; returns true if the key changed it.
pub fn edit_text(s: &mut String, k: &Key, max: usize) -> bool {
    use crate::keys::code;
    if k.code == code::BACKSPACE { return s.pop().is_some(); }
    if let Some(c) = k.ch() { if !c.is_control() && s.chars().count() < max { s.push(c); return true; } }
    false
}

/// Break `s` into lines at most `w` cells wide, at spaces where possible (a word longer than a line is cut).
pub fn wrap_text(s: &str, w: usize) -> Vec<String> {
    let w = w.max(1);
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        while word.len() > w { if !cur.is_empty() { lines.push(std::mem::take(&mut cur)); } lines.push(word.drain(..w).collect()); }
        let word: String = word.into_iter().collect();
        if cur.is_empty() { cur = word; }
        else if cur.chars().count() + 1 + word.chars().count() <= w { cur.push(' '); cur.push_str(&word); }
        else { lines.push(std::mem::take(&mut cur)); cur = word; }
    }
    if !cur.is_empty() { lines.push(cur); }
    lines
}

/// A message that may be long: wrapped over up to `max` lines, ending at row `bottom`. Returns how many lines were drawn.
pub fn draw_note(p: &mut Painter, x: usize, bottom: usize, w: usize, msg: &str, fg: u32, bg: u32, max: usize) -> usize {
    let mut lines = wrap_text(msg, w);
    if lines.len() > max { lines.truncate(max); if let Some(l) = lines.last_mut() { *l = format!("{}...", l.chars().take(w.saturating_sub(3)).collect::<String>()); } }
    let n = lines.len();
    for (i, l) in lines.iter().enumerate() { p.clip(x, (bottom + 1).saturating_sub(n) + i, w, l, fg, bg); }
    n
}

#[cfg(test)]
mod wrap_tests {
    use super::wrap_text;
    #[test]
    fn long_messages_are_wrapped_not_cut_off() {
        let msg = "Minecraft 26.3 is installed and starting (it takes a minute). Players connect to this computer's address on port 25565.";
        let lines = wrap_text(msg, 30);
        assert!(lines.len() >= 4 && lines.iter().all(|l| l.chars().count() <= 30), "{lines:?}");
        assert_eq!(lines.join(" "), msg, "nothing is lost");
        assert_eq!(wrap_text("short", 30), vec!["short"]);
        assert_eq!(wrap_text("", 30), Vec::<String>::new());
        assert_eq!(wrap_text("abcdefghij", 4), vec!["abcd", "efgh", "ij"], "a word longer than a line is cut");
        assert_eq!(wrap_text("a b", 0), vec!["a", "b"], "a zero width does not loop forever");
    }
}
