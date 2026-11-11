//! Applications that run in the shell's tiled windows.
//!
//! Every app is a small state machine: it receives keys, may ask for a timer tick, and draws itself
//! into the rectangle the tiling layout gives it. Nothing in here touches the screen or the keyboard
//! device directly, so each app is testable on its own.
use crate::keys::Key;
use crate::layout::Rect;
use crate::paint::Painter;
pub mod calculator;
pub mod clock;
pub mod diskusage;
pub mod monitor;
pub mod network;
pub mod notepad;
pub mod paint;
pub mod reversi;
pub mod snake;
pub mod terminal;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind { Notepad, Terminal, Paint, Monitor, DiskUsage, Network, Clock, Calculator, Reversi, Snake }

impl Kind {
    pub fn name(self) -> &'static str {
        match self {
            Kind::Notepad => "notepad", Kind::Terminal => "terminal", Kind::Paint => "paint", Kind::Monitor => "monitor",
            Kind::DiskUsage => "diskusage", Kind::Network => "network", Kind::Clock => "clock",
            Kind::Calculator => "calculator", Kind::Reversi => "reversi", Kind::Snake => "snake",
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
pub enum Out { None, Close, Msg(String), OpenFile(PathBuf) }

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
        Kind::Network => Box::new(network::Network::new()),
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
