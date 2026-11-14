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
/// How often the drive row looks for a pendrive that was plugged in or pulled out.
const DRIVE_CHECK: Duration = Duration::from_secs(2);

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
    last_drive_check: Instant,
    last_activity: Instant,
    sleep_unit: Duration,
    /// Effects that arose from a timer rather than a key (the main loop collects them with `take_effects`).
    pending: Vec<Effect>,
    /// Safely remove the drive mounted at the given path (replaced in tests).
    eject: Box<dyn Fn(&str) -> Result<String, String>>,
}

impl Shell {
    pub fn new(cols: usize, rows: usize, mode: Mode, start: PathBuf, home: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Shell {
        Shell { app: App::new(cols, rows, mode, start), windows: Vec::new(), wake, home, last_state: String::new(), last_drive_check: Instant::now(), last_activity: Instant::now(), sleep_unit: Duration::from_secs(60), pending: Vec::new(), eject: Box::new(|m| crate::helper::call(&["usb-eject", m])) }
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
        // A shortcut does what it says instead of being opened as text.
        if path.extension().map_or(false, |e| e == crate::homedirs::SHORTCUT_EXT) {
            if let Some((name, target)) = std::fs::read_to_string(&path).ok().and_then(|t| crate::homedirs::parse_shortcut(&t)) {
                match target {
                    crate::homedirs::Target::Folder(p) => { if self.app.open_dir(p, None) { self.app.focus = Focus::Files; } else { self.app.msg = Some(format!("{name}: that folder does not exist.")); } }
                    crate::homedirs::Target::File(p) => { if p.is_file() { self.open_file(p); } else { self.app.msg = Some(format!("{name}: that file does not exist.")); } }
                    crate::homedirs::Target::App(a) => match apps::Kind::parse(&a) { Some(k) => self.open(k), None => self.app.msg = Some(format!("{name}: there is no app called {a}.")) },
                }
                return;
            }
        }
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
            Out::Open(kind) => self.open(kind),
        }
    }

    /// Handle one key. Returns the effects the outside world has to carry out.
    pub fn handle(&mut self, k: Key) -> Vec<Effect> {
        self.last_activity = Instant::now();
        if self.app.asleep {
            // The first key only wakes the screen; it is not passed on to whatever was on it.
            self.app.asleep = false;
            return vec![Effect::ScreenPower(true)];
        }
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
                Effect::Eject(m) => self.eject_drive(m),
                Effect::Sleep => { self.app.asleep = true; outside.push(Effect::ScreenPower(false)); }
                Effect::SaveSleep(m) => { self.last_activity = Instant::now(); outside.push(Effect::SaveSleep(m)); }
                other => outside.push(other),
            }
        }
        outside
    }

    /// How long without a key before the screen turns off by itself (tests can shorten it).
    fn sleep_after(&self) -> Duration { self.sleep_unit * self.app.sleep_minutes }

    pub fn take_effects(&mut self) -> Vec<Effect> { std::mem::take(&mut self.pending) }

    /// Unmount a pendrive. A folder window that is showing it first moves away, so it does not keep the drive busy.
    fn eject_drive(&mut self, mount: PathBuf) {
        if self.app.cwd.starts_with(&mount) { self.app.open_dir(PathBuf::from("/"), None); }
        let m = mount.to_string_lossy().into_owned();
        let result = (self.eject)(&m);
        self.app.refresh_drives();
        self.app.msg = Some(match result { Ok(s) => s, Err(e) => e }); // after the refresh, so "Safe to remove." is what stays on screen
    }

    // ---- timers --------------------------------------------------------------------------------

    /// How long the main loop may sleep before an app needs a tick (None = until the next key).
    pub fn next_deadline(&self) -> Option<Duration> {
        let now = Instant::now();
        let drives = (self.last_drive_check + DRIVE_CHECK).saturating_duration_since(now);
        let idle = if self.app.sleep_minutes > 0 && !self.app.asleep { Some((self.last_activity + self.sleep_after()).saturating_duration_since(now)) } else { None };
        Some(self.windows.iter().filter_map(|w| w.app.interval().map(|iv| (w.last_tick + iv).saturating_duration_since(now))).chain(Some(drives)).chain(idle).min().unwrap_or(drives))
    }

    /// Run the ticks that are due. Returns true if anything ran (so the screen needs a redraw).
    pub fn tick(&mut self) -> bool {
        let now = Instant::now();
        let env = self.env();
        let mut ran = false;
        if self.app.sleep_minutes > 0 && !self.app.asleep && now.duration_since(self.last_activity) >= self.sleep_after() { self.app.asleep = true; self.pending.push(Effect::ScreenPower(false)); ran = true; }
        if now.duration_since(self.last_drive_check) >= DRIVE_CHECK { self.last_drive_check = now; if self.app.refresh_drives() { ran = true; } }
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
        let mut v = vec![format!("{} windows={} asleep={}{}", self.app.state_line(), self.windows.len(), self.app.asleep, extra)];
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
    fn a_shortcut_opens_what_it_points_to() {
        let root = std::env::temp_dir().join(format!("exec85-shortcut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("Documents")).unwrap();
        let (clock, folder, bad) = (root.join("Clock.shortcut"), root.join("Docs.shortcut"), root.join("Bad.shortcut"));
        std::fs::write(&clock, "name=Clock\nopen=app:clock\n").unwrap();
        std::fs::write(&folder, format!("name=Docs\nopen=folder:{}\n", root.join("Documents").display())).unwrap();
        std::fs::write(&bad, "name=Bad\nopen=app:nosuchapp\n").unwrap();
        let mut s = shell();
        s.open_file(clock); assert_eq!(s.windows.len(), 1, "the clock app opened");
        s.open_file(folder); assert_eq!(s.app.cwd, root.join("Documents")); assert_eq!(s.windows.len(), 1, "no window for a folder");
        s.open_file(bad); assert!(s.app.msg.as_deref().unwrap_or("").contains("no app called"), "{:?}", s.app.msg); assert_eq!(s.windows.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    fn fake_drive(mount: &str) -> crate::fs::Drive { crate::fs::Drive { label: "STICK".into(), mount: PathBuf::from(mount), fstype: "vfat".into(), total: 1 << 30, free: 1 << 29 } }

    #[test]
    fn eject_moves_away_from_the_drive_and_reports_the_result() {
        let mut s = shell();
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<String>::new()));
        let seen2 = seen.clone();
        s.eject = Box::new(move |m| { seen2.borrow_mut().push(m.to_string()); Ok("Safe to remove.".into()) });
        s.app.drives.push(fake_drive("/media/STICK"));
        s.app.focus = Focus::Drives;
        s.app.drive_cursor = s.app.drives.len() - 1;
        let fx = s.handle(parse_key("e").unwrap());
        assert!(fx.is_empty(), "ejecting is handled inside the shell");
        assert_eq!(*seen.borrow(), vec!["/media/STICK".to_string()]);
        assert_eq!(s.app.msg.as_deref(), Some("Safe to remove."));
        s.eject = Box::new(|_| Err("Something is still using the drive.".into()));
        s.app.drives.push(fake_drive("/media/STICK"));
        s.app.drive_cursor = s.app.drives.len() - 1;
        s.handle(parse_key("e").unwrap());
        assert_eq!(s.app.msg.as_deref(), Some("Something is still using the drive."));
    }

    #[test]
    fn sleep_now_blanks_the_screen_and_the_next_key_only_wakes_it() {
        let mut s = shell();
        let before = s.app.sel;
        let fx = press(&mut s, "f8");
        assert!(s.app.asleep && fx == vec![Effect::ScreenPower(false)], "{fx:?}");
        let fx = press(&mut s, "down");
        assert!(!s.app.asleep && fx == vec![Effect::ScreenPower(true)], "{fx:?}");
        assert_eq!(s.app.sel, before, "the waking key did not move the selection");
        press(&mut s, "down");
        assert!(s.app.sel >= before, "the next key works as usual");
        assert!(s.state_lines("")[0].contains("asleep=false"));
    }

    #[test]
    fn the_screen_sleeps_by_itself_after_the_chosen_time_and_apps_keep_running() {
        let mut s = shell();
        s.sleep_unit = Duration::from_millis(10);
        press(&mut s, "alt-v");                                  // View menu
        for _ in 0..7 { press(&mut s, "down"); }                // to "Sleep after 5 minutes" (separators are skipped)
        let fx = press(&mut s, "ret");
        assert_eq!(fx, vec![Effect::SaveSleep(5)], "the choice is saved");
        assert_eq!(s.app.sleep_minutes, 5);
        s.open(Kind::Clock);
        assert!(s.next_deadline().unwrap() <= Duration::from_millis(60), "the main loop wakes up in time to blank the screen");
        std::thread::sleep(Duration::from_millis(90));
        assert!(s.tick() && s.app.asleep);
        assert_eq!(s.take_effects(), vec![Effect::ScreenPower(false)]);
        assert_eq!(s.windows.len(), 1, "windows stay open while the screen is off");
        s.tick();
        assert!(s.take_effects().is_empty(), "it only blanks once");
        press(&mut s, "x");
        assert!(!s.app.asleep);
        // "never" turns it off again
        press(&mut s, "alt-v");
        for _ in 0..10 { press(&mut s, "down"); }
        assert_eq!(press(&mut s, "ret"), vec![Effect::SaveSleep(0)]);
        std::thread::sleep(Duration::from_millis(30));
        s.tick();
        assert!(!s.app.asleep, "never means never");
    }

    #[test]
    fn only_usb_drives_can_be_ejected() {
        let mut s = shell();
        s.app.focus = Focus::Drives;
        s.app.drive_cursor = 0; // the root drive
        press(&mut s, "e");
        assert!(s.app.msg.as_deref().unwrap_or("").contains("cannot be ejected"), "{:?}", s.app.msg);
    }

    #[test]
    fn the_drive_row_notices_changes() {
        let mut s = shell();
        assert!(!s.app.refresh_drives() || s.app.drives.len() >= 1);
        let n = s.app.drives.len();
        s.app.drives.push(fake_drive("/media/GONE"));
        assert!(s.app.refresh_drives(), "a drive that vanished is noticed");
        assert_eq!(s.app.drives.len(), n);
        assert!(s.app.msg.as_deref().unwrap_or("").contains("removed"));
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
