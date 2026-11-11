//! The shell: the Explorer (`App`) plus up to four app windows tiled next to it.
//! It routes keys, runs the apps' timers, and tells the outside world what it must do
//! (power off, log out, remember the theme).
use crate::app::{App, Effect, Focus};
use crate::apps::{self, Application, Env, Kind, Out};
use crate::gfx::Mode;
use crate::keys::Key;
use crate::layout::Win;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const MAX_APPS: usize = 4;

pub struct AppWindow {
    pub app: Box<dyn Application>,
    last_tick: Instant,
    size: (usize, usize),
}

pub struct Shell {
    pub app: App,
    pub windows: Vec<AppWindow>,
    wake: Arc<dyn Fn() + Send + Sync>,
    home: PathBuf,
    last_state: String,
}

impl Shell {
    pub fn new(cols: usize, rows: usize, mode: Mode, start: PathBuf, home: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Shell {
        Shell { app: App::new(cols, rows, mode, start), windows: Vec::new(), wake, home, last_state: String::new() }
    }

    fn env(&self) -> Env { Env { wake: self.wake.clone(), home: self.home.clone(), cwd: self.app.cwd.clone() } }

    fn push(&mut self, app: Box<dyn Application>) {
        if self.windows.len() >= MAX_APPS { self.app.msg = Some(format!("At most {MAX_APPS} windows: close one first (F4).")); return; }
        self.windows.push(AppWindow { app, last_tick: Instant::now(), size: (0, 0) });
        self.app.set_app_count(self.windows.len());
        self.app.focus = Focus::App(self.windows.len() - 1);
        self.sync_sizes();
    }

    pub fn open(&mut self, kind: Kind) {
        if self.windows.len() >= MAX_APPS { self.push(apps::create(kind, &self.env())); return; }
        let a = apps::create(kind, &self.env());
        self.push(a);
    }

    pub fn open_file(&mut self, path: PathBuf) {
        let a = apps::open_file(path, &self.env());
        self.push(a);
    }

    pub fn close(&mut self, i: usize) {
        if i >= self.windows.len() { return; }
        self.windows.remove(i);
        self.app.set_app_count(self.windows.len());
        self.app.focus = if self.windows.is_empty() { Focus::Files } else { Focus::App(i.min(self.windows.len() - 1)) };
        self.sync_sizes();
    }

    fn request_close(&mut self, i: usize) {
        if i < self.windows.len() && self.windows[i].app.try_close() { self.close(i); }
    }

    fn after_app(&mut self, i: usize, out: Out) {
        match out {
            Out::None => {}
            Out::Close => self.close(i),
            Out::Msg(m) => self.app.msg = Some(m),
            Out::OpenFile(p) => self.open_file(p),
        }
    }

    /// Handle one key. Returns the effects the outside world has to carry out.
    pub fn handle(&mut self, k: Key) -> Vec<Effect> {
        let modal = self.app.dialog.is_some() || self.app.menu.is_some();
        if !modal {
            if let Focus::App(i) = self.app.focus {
                if i < self.windows.len() && !App::is_global(&k) {
                    self.app.msg = None;
                    let env = self.env();
                    let out = self.windows[i].app.handle(k, &env);
                    self.after_app(i, out);
                    return Vec::new();
                }
            }
        }
        let mut outside = Vec::new();
        if let Some(e) = self.app.handle(k) {
            match e {
                Effect::OpenApp(kind) => self.open(kind),
                Effect::OpenFile(p) => self.open_file(p),
                Effect::CloseWindow(i) => self.request_close(i),
                other => outside.push(other),
            }
        }
        outside
    }

    // ---- timers --------------------------------------------------------------------------------

    /// How long the main loop may sleep before an app needs a tick (None = until the next key).
    pub fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        self.windows.iter().filter_map(|w| w.app.interval().map(|iv| (w.last_tick + iv).saturating_duration_since(now))).min()
    }

    /// Run the ticks that are due. Returns true if anything ran (so the screen needs a redraw).
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let env = self.env();
        let mut ran = false;
        for w in self.windows.iter_mut() {
            if let Some(iv) = w.app.interval() {
                if now.duration_since(w.last_tick) >= iv { w.app.tick(&env); w.last_tick = now; ran = true; }
            }
        }
        ran
    }

    /// Tell each app how big its window is (in cells) whenever that changes.
    pub fn sync_sizes(&mut self) {
        for (win, r) in self.app.tiles() {
            if let Win::App(i) = win {
                if let Some(w) = self.windows.get_mut(i) {
                    let size = (r.w.saturating_sub(2), r.h.saturating_sub(2));
                    if w.size != size { w.size = size; w.app.resize(size.0, size.1); }
                }
            }
        }
    }

    // ---- state for tests ---------------------------------------------------------------------------

    /// The lines printed on the console after an event. The app line is only printed when it changed.
    pub fn state_lines(&mut self, extra: &str) -> Vec<String> {
        let mut v = vec![format!("{} windows={}{}", self.app.state_line(), self.windows.len(), extra)];
        if let Focus::App(i) = self.app.focus {
            if let Some(w) = self.windows.get(i) { v.push(format!("EXEC85_APP {}", w.app.state())); }
        }
        self.last_state = v.join("\n");
        v
    }

    /// Like `state_lines`, but returns only lines that differ from the last time (for timer redraws).
    pub fn changed_state_lines(&mut self, extra: &str) -> Vec<String> {
        let before = self.last_state.clone();
        let lines = self.state_lines(extra);
        if lines.join("\n") == before { Vec::new() } else { lines }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn shell() -> Shell { Shell::new(100, 40, Mode::Dark, PathBuf::from("/"), std::env::temp_dir(), Arc::new(|| {})) }
    fn press(s: &mut Shell, keys: &str) -> Vec<Effect> {
        keys.split(',').filter(|x| !x.is_empty()).flat_map(|n| s.handle(parse_key(n).unwrap())).collect()
    }

    #[test]
    fn opening_apps_from_the_menu_tiles_them_next_to_the_explorer() {
        let mut s = shell();
        assert_eq!(s.windows.len(), 0);
        press(&mut s, "alt-a,ret");              // Apps menu, first item (Notepad)
        assert_eq!(s.windows.len(), 1);
        assert_eq!(s.app.focus, Focus::App(0));
        press(&mut s, "alt-a,down,down,ret");    // Paint
        assert_eq!(s.windows.len(), 2);
        assert_eq!(s.app.focus, Focus::App(1));
        let tiles = s.app.tiles();
        assert_eq!(tiles.len(), 4, "Files, Details and two apps");
        // no overlap, whole area covered
        let total: usize = tiles.iter().map(|(_, r)| r.w * r.h).sum();
        assert_eq!(total, 100 * (40 - 6));
    }

    #[test]
    fn f6_moves_focus_through_the_windows_and_f4_closes() {
        let mut s = shell();
        press(&mut s, "alt-a,ret,alt-a,ret");
        assert_eq!(s.app.focus, Focus::App(1));
        press(&mut s, "f6");
        assert_eq!(s.app.focus, Focus::Files, "wraps to the Explorer");
        press(&mut s, "f6");
        assert_eq!(s.app.focus, Focus::App(0));
        press(&mut s, "shift-f6");
        assert_eq!(s.app.focus, Focus::Files);
        press(&mut s, "f6,f4");
        assert_eq!(s.windows.len(), 1, "F4 closed the focused app window");
        assert_eq!(s.app.focus, Focus::App(0));
        press(&mut s, "f4");
        assert_eq!(s.windows.len(), 0);
        assert_eq!(s.app.focus, Focus::Files);
    }

    #[test]
    fn at_most_four_windows() {
        let mut s = shell();
        for _ in 0..6 { press(&mut s, "alt-a,ret"); }
        assert_eq!(s.windows.len(), MAX_APPS);
        assert!(s.app.msg.as_deref().unwrap().contains("close one"));
    }

    #[test]
    fn shell_keys_still_work_inside_an_app_but_ordinary_keys_go_to_the_app() {
        let mut s = shell();
        press(&mut s, "alt-a,ret");
        press(&mut s, "f10");
        assert!(s.app.menu.is_some(), "F10 opens the menu even with an app focused");
        press(&mut s, "esc");
        // Tab must reach the app (not move the Explorer's panes)
        let before = s.app.focus;
        press(&mut s, "tab");
        assert_eq!(s.app.focus, before);
        // Ctrl+H must not toggle hidden files while an app has the focus
        press(&mut s, "ctrl-h");
        assert!(!s.app.show_hidden);
    }

    #[test]
    fn closing_the_focused_window_keeps_focus_valid() {
        let mut s = shell();
        press(&mut s, "alt-a,ret,alt-a,ret,alt-a,ret");
        s.close(0);
        assert_eq!(s.windows.len(), 2);
        assert!(matches!(s.app.focus, Focus::App(i) if i < 2));
        s.close(5); // out of range: ignored
        assert_eq!(s.windows.len(), 2);
    }

    #[test]
    fn power_and_theme_effects_are_passed_outwards() {
        let mut s = shell();
        let fx = press(&mut s, "alt-v,down,ret");
        assert_eq!(fx, vec![Effect::SaveTheme(Mode::Light)]);
    }
}
