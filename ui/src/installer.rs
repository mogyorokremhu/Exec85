//! The installer: a keyboard-driven wizard. The state machine (`Installer`) knows nothing about
//! the screen or the real disks, so it is fully testable; `draw` paints it.
use crate::account::{valid_hostname, valid_username};
use crate::disks::Disk;
use crate::fs::fmt_disk;
use crate::gfx::{Canvas, Metrics, Mode, Theme};
use crate::install::{Config, Progress, Shared, STEPS as INSTALL_STEPS};
use crate::keys::{char_for, code, Key, Layout};
use crate::layout::Rect;
use crate::paint::{tail, Painter};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Step { Welcome, Theme, Keyboard, Timezone, Hostname, User, Password, Disk, Confirm, Installing, Done }

const ORDER: [Step; 11] = [
    Step::Welcome, Step::Theme, Step::Keyboard, Step::Timezone, Step::Hostname, Step::User,
    Step::Password, Step::Disk, Step::Confirm, Step::Installing, Step::Done,
];

impl Step {
    pub fn name(self) -> &'static str {
        match self {
            Step::Welcome => "welcome", Step::Theme => "theme", Step::Keyboard => "keyboard", Step::Timezone => "timezone",
            Step::Hostname => "hostname", Step::User => "user", Step::Password => "password", Step::Disk => "disk",
            Step::Confirm => "confirm", Step::Installing => "installing", Step::Done => "done",
        }
    }
    fn title(self) -> &'static str {
        match self {
            Step::Welcome => "Welcome", Step::Theme => "Appearance", Step::Keyboard => "Keyboard layout", Step::Timezone => "Time zone",
            Step::Hostname => "Device name", Step::User => "User name", Step::Password => "Password", Step::Disk => "Disk",
            Step::Confirm => "Confirm", Step::Installing => "Installing", Step::Done => "Finished",
        }
    }
    fn index(self) -> usize { ORDER.iter().position(|s| *s == self).unwrap_or(0) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome { Reboot, Live, SetupDone /* TESTING-ONLY */ }

/// A list you move through with the arrow keys; typing narrows it down.
#[derive(Clone, Debug)]
pub struct Picker { pub items: Vec<String>, pub filter: String, pub sel: usize, pub top: usize, pub can_filter: bool }

impl Picker {
    pub fn new(items: Vec<String>, can_filter: bool) -> Picker { Picker { items, filter: String::new(), sel: 0, top: 0, can_filter } }

    /// Indices (into `items`) that match the typed text.
    pub fn visible(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.items.len()).filter(|i| f.is_empty() || self.items[*i].to_lowercase().contains(&f)).collect()
    }
    pub fn visible_items(&self) -> Vec<String> { self.visible().into_iter().map(|i| self.items[i].clone()).collect() }
    pub fn current(&self) -> Option<usize> { self.visible().get(self.sel).copied() }
    pub fn current_text(&self) -> Option<&str> { self.current().map(|i| self.items[i].as_str()) }

    fn fix(&mut self, rows: usize) {
        let n = self.visible().len();
        self.sel = self.sel.min(n.saturating_sub(1));
        if self.sel < self.top { self.top = self.sel; }
        if self.sel >= self.top + rows.max(1) { self.top = self.sel + 1 - rows.max(1); }
    }
    pub fn move_by(&mut self, d: isize, rows: usize) {
        let n = self.visible().len() as isize;
        if n == 0 { return; }
        self.sel = (self.sel as isize + d).clamp(0, n - 1) as usize;
        self.fix(rows);
    }
    pub fn select(&mut self, original: usize, rows: usize) {
        self.filter.clear();
        self.sel = original.min(self.items.len().saturating_sub(1));
        self.top = 0;
        self.fix(rows);
    }
    fn edit_filter(&mut self, c: Option<char>, rows: usize) {
        match c { Some(c) => self.filter.push(c), None => { self.filter.pop(); } }
        self.sel = 0;
        self.top = 0;
        self.fix(rows);
    }
}

pub struct Installer {
    /// Background music: None = there is none, Some(true) = playing, Some(false) = switched off (F8).
    pub music: Option<bool>,
    pub cols: usize,
    pub rows: usize,
    pub step: Step,
    pub mode: Mode,
    pub layout: Layout,
    pub welcome: Picker,
    pub theme: Picker,
    pub kb: Picker,
    pub tz: Picker,
    pub disk_pk: Picker,
    pub disks: Vec<Disk>,
    pub hostname: String,
    pub username: String,
    pub pw1: String,
    pub pw2: String,
    pub pw_field: usize,
    pub empty_warned: bool,
    pub confirm: String,
    pub error: Option<String>,
    pub progress: Shared,
    pub failed: Option<String>,
    /// TESTING-ONLY: offer "Skip installation" (set up in memory) as the first choice on the Disk step.
    pub testing: bool,
    disk_source: Box<dyn Fn() -> Vec<Disk>>,
    runner: Box<dyn Fn(Config, Shared)>,
}

/// TESTING-ONLY: name of the pseudo-disk that means "do not install, set up in memory".
pub const SKIP_NAME: &str = "(memory)";

/// Time zone names from the system's tz database (zone1970.tab), with UTC first.
pub fn load_timezones(dir: &std::path::Path) -> Vec<String> {
    let text = std::fs::read_to_string(dir.join("zone1970.tab")).unwrap_or_default();
    let mut v: Vec<String> = text.lines().filter(|l| !l.starts_with('#')).filter_map(|l| l.split('\t').nth(2)).map(String::from).collect();
    v.sort();
    v.dedup();
    v.retain(|z| z != "UTC");
    v.insert(0, "UTC".to_string());
    v
}

fn clean(s: &str) -> String { if s.is_empty() { "-".into() } else { s.replace(' ', "_") } }

impl Installer {
    /// `disk_source` lists the disks (called again whenever the Disk step is entered);
    /// `runner` starts the installation in the background and fills in the shared progress.
    pub fn new(cols: usize, rows: usize, mode: Mode, timezones: Vec<String>, testing: bool, disk_source: Box<dyn Fn() -> Vec<Disk>>, runner: Box<dyn Fn(Config, Shared)>) -> Installer {
        let mut tz = Picker::new(timezones, true);
        let utc = tz.items.iter().position(|t| t == "UTC").unwrap_or(0);
        tz.select(utc, 10);
        let mut theme = Picker::new(vec!["Dark".into(), "Light".into()], false);
        theme.sel = if mode == Mode::Light { 1 } else { 0 };
        let kb_items = Layout::ALL.iter().map(|l| format!("{} ({})", l.label(), l.id())).collect();
        Installer {
            music: None, cols, rows, step: Step::Welcome, mode, layout: Layout::Us,
            welcome: Picker::new(vec!["Install Exec85 on this computer".into(), "Try Exec85 without installing".into()], false),
            theme, kb: Picker::new(kb_items, true), tz, disk_pk: Picker::new(vec![], false), disks: vec![],
            hostname: "exec85".into(), username: String::new(), pw1: String::new(), pw2: String::new(), pw_field: 0, empty_warned: false,
            confirm: String::new(), error: None, progress: Arc::new(Mutex::new(Progress::default())), failed: None,
            testing, disk_source, runner,
        }
    }

    // ---- geometry (shared with drawing) -------------------------------------------------------

    fn steps_w(&self) -> usize { (self.cols / 3).min(24) }
    fn content(&self) -> Rect { let w = self.steps_w(); Rect { x: w, y: 1, w: self.cols - w, h: self.rows.saturating_sub(2) } }
    fn inner(&self) -> Rect { let c = self.content(); Rect { x: c.x + 1, y: c.y + 1, w: c.w.saturating_sub(2), h: c.h.saturating_sub(2) } }
    /// Where list items are drawn: below the heading and help text, above the filter line.
    fn list_area(&self) -> Rect { let a = self.inner(); Rect { x: a.x, y: a.y + 4, w: a.w, h: a.h.saturating_sub(7).max(1) } }
    fn list_rows(&self) -> usize { self.list_area().h }

    fn picker(&mut self) -> Option<&mut Picker> {
        match self.step {
            Step::Welcome => Some(&mut self.welcome), Step::Theme => Some(&mut self.theme), Step::Keyboard => Some(&mut self.kb),
            Step::Timezone => Some(&mut self.tz), Step::Disk => Some(&mut self.disk_pk), _ => None,
        }
    }

    fn picker_ref(&self) -> Option<&Picker> {
        match self.step {
            Step::Welcome => Some(&self.welcome), Step::Theme => Some(&self.theme), Step::Keyboard => Some(&self.kb),
            Step::Timezone => Some(&self.tz), Step::Disk => Some(&self.disk_pk), _ => None,
        }
    }

    // ---- results ------------------------------------------------------------------------------

    pub fn selected_disk(&self) -> Option<&Disk> { self.disk_pk.current().and_then(|i| self.disks.get(i)) }
    /// TESTING-ONLY: is the "Skip installation" choice highlighted?
    pub fn skip_selected(&self) -> bool { self.testing && self.selected_disk().map_or(false, |d| d.name == SKIP_NAME) }
    pub fn selected_tz(&self) -> &str { self.tz.current_text().unwrap_or("UTC") }

    pub fn config(&self) -> Config {
        Config {
            disk: if self.skip_selected() { String::new() } else { self.selected_disk().map(|d| d.name.clone()).unwrap_or_default() },
            hostname: self.hostname.clone(), username: self.username.clone(),
            password: if self.pw1.is_empty() { None } else { Some(self.pw1.clone()) },
            layout: self.layout, timezone: self.selected_tz().to_string(), dark: self.mode == Mode::Dark,
        }
    }

    // ---- behaviour ----------------------------------------------------------------------------

    /// Move to Done/failed when the background installation has finished.
    pub fn poll(&mut self) {
        if self.step != Step::Installing { return; }
        let result = self.progress.lock().unwrap().result.clone();
        match result {
            Some(Ok(())) => { self.step = Step::Done; self.failed = None; }
            Some(Err(e)) => { self.step = Step::Done; self.failed = Some(e); }
            None => {}
        }
    }

    fn typed(&self, k: &Key) -> Option<char> {
        if k.ctrl || k.alt { None } else { char_for(self.layout, k.code, k.shift, k.altgr) }
    }

    fn goto(&mut self, s: Step) {
        self.step = s;
        self.error = None;
        let rows = self.list_rows();
        if s == Step::Disk {
            self.disks = (self.disk_source)();
            // TESTING-ONLY: the skip choice goes first
            if self.testing { self.disks.insert(0, Disk { name: SKIP_NAME.into(), bytes: 0, model: "TESTING ONLY".into(), removable: false }); }
            let items = self.disks.iter().map(|d| if d.name == SKIP_NAME { "Skip installation   (testing only: set up in memory, nothing is erased)".to_string() } else { format!("{:<9}{:>7}  {}", d.name, fmt_disk(d.bytes), d.model) }).collect();
            self.disk_pk = Picker::new(items, false);
            self.disk_pk.move_by(0, rows);
        }
        if s == Step::Confirm { self.confirm.clear(); }
    }

    fn next(&mut self) { let i = self.step.index(); self.goto(ORDER[(i + 1).min(ORDER.len() - 1)]); }
    fn back(&mut self) { let i = self.step.index(); if i > 0 && i <= Step::Confirm.index() { self.goto(ORDER[i - 1]); } }

    fn apply_pick(&mut self) {
        match self.step {
            Step::Theme => self.mode = if self.theme.current() == Some(1) { Mode::Light } else { Mode::Dark },
            Step::Keyboard => if let Some(i) = self.kb.current() { self.layout = Layout::ALL[i]; },
            _ => {}
        }
    }

    pub fn handle(&mut self, k: Key) -> Option<Outcome> {
        self.poll();
        self.error = None;
        match self.step {
            Step::Welcome | Step::Theme | Step::Keyboard | Step::Timezone | Step::Disk => self.handle_picker(k),
            Step::Hostname | Step::User => { self.handle_name(k); None }
            Step::Password => { self.handle_password(k); None }
            Step::Confirm => { self.handle_confirm(k); None }
            Step::Installing => None,
            Step::Done => {
                if k.code == code::ENTER {
                    if self.failed.is_some() { self.failed = None; self.goto(Step::Disk); }
                    else if self.skip_selected() { return Some(Outcome::SetupDone); } // TESTING-ONLY
                    else { return Some(Outcome::Reboot); }
                } else if k.code == code::ESC && self.failed.is_some() { self.failed = None; self.goto(Step::Disk); }
                None
            }
        }
    }

    fn handle_picker(&mut self, k: Key) -> Option<Outcome> {
        let rows = self.list_rows();
        let typed = self.typed(&k);
        let step = self.step;
        let Some(p) = self.picker() else { return None };
        match k.code {
            code::UP => p.move_by(-1, rows),
            code::DOWN => p.move_by(1, rows),
            code::PGUP => p.move_by(-(rows as isize), rows),
            code::PGDN => p.move_by(rows as isize, rows),
            code::HOME => p.move_by(isize::MIN / 2, rows),
            code::END => p.move_by(isize::MAX / 2, rows),
            code::BACKSPACE if p.can_filter => p.edit_filter(None, rows),
            code::ESC => {
                if p.can_filter && !p.filter.is_empty() { p.edit_filter(None, rows); while !p.filter.is_empty() { p.edit_filter(None, rows); } }
                else { self.back(); }
            }
            code::ENTER => {
                if p.current().is_none() {
                    self.error = Some(if step == Step::Disk { "No suitable disk was found.".into() } else { "Nothing matches. Press Backspace to widen the search.".into() });
                } else if step == Step::Welcome && p.current() == Some(1) {
                    return Some(Outcome::Live);
                } else { self.next(); }
            }
            _ => if p.can_filter { if let Some(c) = typed { if c.is_ascii_graphic() { p.edit_filter(Some(c), rows); } } },
        }
        self.apply_pick();
        None
    }

    fn handle_name(&mut self, k: Key) {
        let hostname = self.step == Step::Hostname;
        let typed = self.typed(&k);
        match k.code {
            code::ESC => self.back(),
            code::BACKSPACE => { (if hostname { &mut self.hostname } else { &mut self.username }).pop(); }
            code::ENTER => {
                let v = if hostname { valid_hostname(&self.hostname) } else { valid_username(&self.username) };
                match v { Ok(()) => self.next(), Err(e) => self.error = Some(e.to_string()) }
            }
            _ => if let Some(c) = typed {
                let c = c.to_ascii_lowercase();
                let ok = c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || (!hostname && c == '_');
                let field = if hostname { &mut self.hostname } else { &mut self.username };
                if c.is_ascii_graphic() && !ok { self.error = Some(if hostname { "Only lowercase letters, digits and - can be used here.".into() } else { "Only lowercase letters, digits, - and _ can be used here.".into() }); }
                else if ok && field.len() < if hostname { 63 } else { 32 } { field.push(c); }
            },
        }
    }

    fn handle_password(&mut self, k: Key) {
        let typed = self.typed(&k);
        match k.code {
            code::ESC => self.back(),
            code::TAB | code::DOWN => self.pw_field = if k.shift { 0 } else { 1 },
            code::UP => self.pw_field = 0,
            code::BACKSPACE => { self.empty_warned = false; (if self.pw_field == 0 { &mut self.pw1 } else { &mut self.pw2 }).pop(); }
            code::ENTER => {
                if self.pw_field == 0 { self.pw_field = 1; return; }
                if self.pw1 != self.pw2 { self.error = Some("The two passwords do not match.".into()); return; }
                if self.pw1.is_empty() && !self.empty_warned {
                    self.empty_warned = true;
                    self.error = Some("No password: anyone who can use this computer can log in. Press Enter again to continue anyway.".into());
                    return;
                }
                self.next();
            }
            _ => if let Some(c) = typed {
                self.empty_warned = false;
                let f = if self.pw_field == 0 { &mut self.pw1 } else { &mut self.pw2 };
                if f.chars().count() < 128 { f.push(c); }
            },
        }
    }

    fn handle_confirm(&mut self, k: Key) {
        let typed = self.typed(&k);
        if self.skip_selected() { // TESTING-ONLY: nothing to erase, so no disk name has to be typed
            match k.code { code::ESC => self.back(), code::ENTER => self.start_install(), _ => {} }
            return;
        }
        match k.code {
            code::ESC => self.back(),
            code::BACKSPACE => { self.confirm.pop(); }
            code::ENTER => {
                let disk = self.selected_disk().map(|d| d.name.clone()).unwrap_or_default();
                if disk.is_empty() { self.error = Some("No disk selected.".into()); }
                else if self.confirm != disk { self.error = Some(format!("Type exactly \"{disk}\" to confirm, or press Esc to go back.")); }
                else { self.start_install(); }
            }
            _ => if let Some(c) = typed { if c.is_ascii_alphanumeric() && self.confirm.len() < 16 { self.confirm.push(c.to_ascii_lowercase()); } },
        }
    }

    fn start_install(&mut self) {
        self.step = Step::Installing;
        *self.progress.lock().unwrap() = Progress { total: INSTALL_STEPS.len(), ..Progress::default() };
        (self.runner)(self.config(), self.progress.clone());
    }

    /// One line describing the state, printed on the console for automated tests and bug reports.
    pub fn state_line(&self) -> String {
        let p = self.progress.lock().unwrap();
        let (sel, filter) = self.picker_ref().map_or(("-".to_string(), "-".to_string()), |pk| (clean(pk.current_text().unwrap_or("")), clean(&pk.filter)));
        let result = match (&p.result, &self.failed) { (None, None) => "none", (Some(Ok(())), _) => "ok", _ => "failed" };
        format!(
            "EXEC85_INSTALLER step={} theme={} layout={} tz={} host={} user={} pw={} disk={} filter={} sel={} field={} error={} progress={}/{} result={} music={}",
            self.step.name(), if self.mode == Mode::Dark { "dark" } else { "light" }, self.layout.id(), clean(self.selected_tz()),
            clean(&self.hostname), clean(&self.username), if self.pw1.is_empty() { "empty" } else { "set" },
            clean(self.selected_disk().map_or("", |d| d.name.as_str())), filter, sel, self.pw_field,
            self.error.as_deref().map_or("none".to_string(), clean), p.done, p.total, result, match self.music { None => "none", Some(true) => "on", Some(false) => "off" },
        )
    }
}

// ---- drawing -----------------------------------------------------------------------------------------

pub fn draw(inst: &Installer, c: &mut Canvas, m: &Metrics) {
    let t = Theme::new(inst.mode);
    c.fill_rect(0, 0, c.w, c.h, t.bg);
    let mut p = Painter { c, m: *m, t };
    let cols = inst.cols;
    // top bar
    p.cells(0, 0, cols, 1, t.bar);
    p.text(1, 0, "Exec85 Setup", t.bar_text, None);
    let ver = format!("v{}", env!("CARGO_PKG_VERSION"));
    if cols > 40 { p.text(cols - ver.len() - 1, 0, &ver, t.dim, None); }

    // left: the steps
    let sw = inst.steps_w();
    let left = Rect { x: 0, y: 1, w: sw, h: inst.rows.saturating_sub(2) };
    let a = p.window(left, "Steps", false);
    for (i, s) in ORDER.iter().enumerate() {
        if i >= a.h { break; }
        let cur = inst.step.index();
        let (mark, fg) = if i == cur { (">", t.accent) } else if i < cur { ("*", t.dim) } else { (" ", t.dim) };
        p.text(a.x, a.y + i, mark, fg, None);
        p.clip(a.x + 2, a.y + i, a.w.saturating_sub(2), s.title(), if i == cur { t.text } else { t.dim }, t.panel);
    }

    // right: this step
    let right = inst.content();
    let a = p.window(right, inst.step.title(), true);
    let (x, w) = (a.x, a.w);
    let help = |p: &mut Painter, s: &str| { p.paragraph(x, a.y + 1, w, s, t.dim, t.panel); };
    let heading = |p: &mut Painter, s: &str| p.text(x, a.y, s, t.text, None);
    let la = inst.list_area();

    match inst.step {
        Step::Welcome => {
            heading(&mut p, "Welcome to Exec85");
            help(&mut p, "A small, fast, keyboard-driven operating system for home servers. Use the arrow keys, then press Enter.");
            list(&mut p, inst, la);
        }
        Step::Theme => {
            heading(&mut p, "Light or dark?");
            help(&mut p, "The whole interface uses this. You can change it later in the View menu.");
            list(&mut p, inst, la);
        }
        Step::Keyboard => {
            heading(&mut p, "Choose your keyboard");
            help(&mut p, "Type to search. The layout is used for your name and password, here and at login.");
            list(&mut p, inst, la);
            filter_line(&mut p, inst, &inst.kb, a);
        }
        Step::Timezone => {
            heading(&mut p, "Where are you?");
            help(&mut p, "Type part of a city or region to search, for example: budapest");
            list(&mut p, inst, la);
            filter_line(&mut p, inst, &inst.tz, a);
        }
        Step::Hostname => {
            heading(&mut p, "Name this computer");
            help(&mut p, "How it appears on your network. Lowercase letters, digits and - only.");
            p.field(x, a.y + 4, w.min(40), &inst.hostname, true, false);
        }
        Step::User => {
            heading(&mut p, "Create your user");
            help(&mut p, "You will log in with this name. It becomes an administrator account.");
            p.field(x, a.y + 4, w.min(40), &inst.username, true, false);
        }
        Step::Password => {
            heading(&mut p, "Choose a password");
            help(&mut p, "Optional, but strongly recommended. Tab switches between the two boxes.");
            p.text(x, a.y + 3, "Password", t.dim, None);
            p.field(x, a.y + 4, w.min(40), &inst.pw1, inst.pw_field == 0, true);
            p.text(x, a.y + 6, "Again", t.dim, None);
            p.field(x, a.y + 7, w.min(40), &inst.pw2, inst.pw_field == 1, true);
        }
        Step::Disk => {
            heading(&mut p, "Where should Exec85 go?");
            help(&mut p, "Everything on the disk you choose will be ERASED.");
            if inst.disks.is_empty() { p.text(la.x, la.y, "No suitable disk was found.", t.accent, None); }
            list(&mut p, inst, la);
        }
        Step::Confirm => {
            heading(&mut p, "Ready to install");
            let disk = inst.selected_disk();
            let rows = [
                ("Appearance", if inst.mode == Mode::Dark { "Dark" } else { "Light" }.to_string()),
                ("Keyboard", format!("{} ({})", inst.layout.label(), inst.layout.id())),
                ("Time zone", inst.selected_tz().to_string()),
                ("Device name", inst.hostname.clone()),
                ("User", inst.username.clone()),
                ("Password", if inst.pw1.is_empty() { "none".to_string() } else { "set".to_string() }),
                ("Disk", if inst.skip_selected() { "none (testing: set up in memory)".to_string() } else { disk.map_or("-".to_string(), |d| format!("{} ({}, {})", d.name, fmt_disk(d.bytes), d.model)) }),
            ];
            for (i, (k, v)) in rows.iter().enumerate() {
                p.text(x, a.y + 2 + i, k, t.dim, None);
                p.clip(x + 13, a.y + 2 + i, w.saturating_sub(13), v, t.text, t.panel);
            }
            let name = disk.map_or("", |d| d.name.as_str());
            if inst.skip_selected() { // TESTING-ONLY
                p.paragraph(x, a.y + 10, w, "TESTING BUILD: nothing will be installed or erased. Your settings and account are created in memory only, then you go to the login screen. Press Enter to continue, Esc to go back.", t.accent, t.panel);
            } else {
                let n = p.paragraph(x, a.y + 10, w, &format!("ALL DATA on {name} will be erased. To continue, type the disk name ({name}) and press Enter. Esc goes back."), t.accent, t.panel);
                p.field(x, a.y + 10 + n + 1, w.min(24), &inst.confirm, true, false);
            }
        }
        Step::Installing => {
            let pr = inst.progress.lock().unwrap().clone();
            heading(&mut p, "Installing Exec85...");
            help(&mut p, "Please do not turn the computer off.");
            let bar_w = w.min(50);
            p.cells(x, a.y + 4, bar_w, 1, t.bg);
            let filled = bar_w * pr.done / pr.total.max(1);
            p.cells(x, a.y + 4, filled, 1, t.accent);
            p.outline(Rect { x, y: a.y + 4, w: bar_w, h: 1 }, t.border);
            p.text(x, a.y + 6, &pr.current, t.text, None);
            let start = pr.log.len().saturating_sub(a.h.saturating_sub(9));
            for (i, l) in pr.log[start..].iter().enumerate() { p.clip(x, a.y + 8 + i, w, l, t.dim, t.panel); }
        }
        Step::Done => match &inst.failed {
            None if inst.skip_selected() => { // TESTING-ONLY
                heading(&mut p, "Set up in memory");
                help(&mut p, "Your settings and account exist until you restart. Nothing was written to any disk. Press Enter to go to the login screen.");
            }
            None => {
                heading(&mut p, "Installation complete");
                help(&mut p, "Exec85 is installed. Remove the installation medium (USB stick or disc), then press Enter to restart.");
            }
            Some(e) => {
                heading(&mut p, "The installation failed");
                let n = p.paragraph(x, a.y + 2, w, e, t.accent, t.panel);
                p.paragraph(x, a.y + 3 + n, w, "Nothing more was changed. Press Enter to choose a disk and try again.", t.dim, t.panel);
            }
        },
    }
    if let Some(e) = &inst.error { p.paragraph(x, a.y + a.h.saturating_sub(2), w, e, t.accent, t.panel); }

    // bottom bar
    let y = inst.rows - 1;
    p.cells(0, y, cols, 1, t.bar);
    let hint = match inst.step {
        Step::Welcome | Step::Theme | Step::Disk => "Up/Down Choose   Enter Continue   Esc Back",
        Step::Keyboard | Step::Timezone => "Type to search   Up/Down Choose   Enter Continue   Esc Back",
        Step::Hostname | Step::User => "Type a name   Enter Continue   Esc Back",
        Step::Password => "Tab Next box   Enter Continue   Esc Back",
        Step::Confirm => "Type the disk name   Enter Install   Esc Back",
        Step::Installing => "Installing...",
        Step::Done => "Enter Continue",
    };
    let hint = match inst.music { Some(on) => format!("{hint}   F8 Music {}", if on { "on" } else { "off" }), None => hint.to_string() };
    p.clip(1, y, cols.saturating_sub(2), &hint, t.bar_text, t.bar);
}

fn list(p: &mut Painter, inst: &Installer, la: Rect) {
    if let Some(pk) = inst.picker_ref() { p.list(la, &pk.visible_items(), pk.sel, pk.top, true); }
}

fn filter_line(p: &mut Painter, _inst: &Installer, pk: &Picker, a: Rect) {
    let t = p.t;
    let y = a.y + a.h.saturating_sub(3);
    p.text(a.x, y, "Search:", t.dim, None);
    p.field(a.x + 8, y, a.w.saturating_sub(8).min(34), &pk.filter, true, false);
    let n = pk.visible().len();
    p.text(a.x + 8 + a.w.saturating_sub(8).min(34) + 2, y, &format!("{n} match{}", if n == 1 { "" } else { "es" }), t.dim, None);
    let _ = tail;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn disks() -> Vec<Disk> {
        vec![
            Disk { name: "nvme0n1".into(), bytes: 512_110_190_592, model: "Samsung SSD".into(), removable: false },
            Disk { name: "sda".into(), bytes: 4_000_787_030_016, model: "ATA ST4000LM024".into(), removable: false },
        ]
    }

    fn inst_with(started: Arc<AtomicUsize>, ds: Vec<Disk>) -> Installer {
        let tzs = ["UTC", "Europe/Berlin", "Europe/Budapest", "America/New_York", "Asia/Tokyo"].iter().map(|s| s.to_string()).collect();
        Installer::new(100, 40, Mode::Dark, tzs, false, Box::new(move || ds.clone()), Box::new(move |_cfg, shared| {
            started.fetch_add(1, Ordering::SeqCst);
            let mut p = shared.lock().unwrap();
            p.result = Some(Ok(())); p.done = p.total;
        }))
    }

    fn press(i: &mut Installer, keys: &str) -> Vec<Outcome> {
        keys.split(',').filter(|s| !s.is_empty()).filter_map(|n| i.handle(parse_key(n).unwrap_or_else(|| panic!("bad key {n}")))).collect()
    }

    fn type_text(i: &mut Installer, s: &str) { for c in s.chars() { i.handle(parse_key(&c.to_string()).unwrap()); } }

    #[test]
    fn timezone_list_comes_from_the_tz_database_with_utc_first() {
        let dir = std::env::temp_dir().join(format!("exec85-tz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("zone1970.tab"), "# comment\nHU\t+4730+01905\tEurope/Budapest\nJP\t+353916+1394441\tAsia/Tokyo\nDE,AT\t+5230+01322\tEurope/Berlin\tmost of Germany\n").unwrap();
        assert_eq!(load_timezones(&dir), vec!["UTC", "Asia/Tokyo", "Europe/Berlin", "Europe/Budapest"]);
        assert_eq!(load_timezones(&dir.join("missing")), vec!["UTC"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn welcome_offers_live_mode() {
        let mut i = inst_with(Arc::default(), disks());
        assert_eq!(i.step, Step::Welcome);
        assert_eq!(press(&mut i, "down,enter"), vec![Outcome::Live]);
        let mut i = inst_with(Arc::default(), disks());
        assert!(press(&mut i, "enter").is_empty());
        assert_eq!(i.step, Step::Theme);
    }

    #[test]
    fn theme_changes_the_screen_immediately_and_is_remembered() {
        let mut i = inst_with(Arc::default(), disks());
        press(&mut i, "enter");
        assert_eq!(i.mode, Mode::Dark);
        press(&mut i, "down");
        assert_eq!(i.mode, Mode::Light);
        press(&mut i, "up");
        assert_eq!(i.mode, Mode::Dark);
        press(&mut i, "down,enter");
        assert!(!i.config().dark);
    }

    #[test]
    fn typing_narrows_the_keyboard_and_timezone_lists() {
        let mut i = inst_with(Arc::default(), disks());
        press(&mut i, "enter,enter");
        assert_eq!(i.step, Step::Keyboard);
        type_text(&mut i, "hung");
        assert_eq!(i.kb.visible().len(), 1);
        assert_eq!(i.layout, Layout::Hu, "the layout applies as soon as it is highlighted");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Timezone);
        assert_eq!(i.selected_tz(), "UTC", "UTC is the starting point");
        type_text(&mut i, "budapest");
        assert_eq!(i.tz.visible_items(), vec!["Europe/Budapest"]);
        press(&mut i, "backspace,backspace,backspace,backspace,backspace,backspace,backspace,backspace");
        assert_eq!(i.tz.visible().len(), 5);
        type_text(&mut i, "europe");
        press(&mut i, "down");
        assert_eq!(i.selected_tz(), "Europe/Budapest");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Hostname);
        assert_eq!(i.config().timezone, "Europe/Budapest");
        assert_eq!(i.config().layout, Layout::Hu);
    }

    #[test]
    fn nothing_matching_is_reported_not_accepted() {
        let mut i = inst_with(Arc::default(), disks());
        press(&mut i, "enter,enter,enter");
        assert_eq!(i.step, Step::Timezone);
        type_text(&mut i, "atlantis");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Timezone);
        assert!(i.error.as_deref().unwrap().starts_with("Nothing matches"));
    }

    #[test]
    fn names_are_validated_and_filtered_as_you_type() {
        let mut i = inst_with(Arc::default(), disks());
        press(&mut i, "enter,enter,enter,enter");
        assert_eq!(i.step, Step::Hostname);
        assert_eq!(i.hostname, "exec85");
        press(&mut i, "backspace,backspace,backspace,backspace,backspace,backspace");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Hostname, "empty name refused");
        type_text(&mut i, "Lab_85!");
        assert_eq!(i.hostname, "lab85", "capitals lowercased, underscore and ! refused");
        assert!(i.error.is_some());
        press(&mut i, "enter");
        assert_eq!(i.step, Step::User);
        press(&mut i, "enter");
        assert_eq!(i.step, Step::User, "user name is required");
        type_text(&mut i, "root");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::User, "reserved names are refused");
        press(&mut i, "backspace,backspace,backspace,backspace");
        type_text(&mut i, "tester");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Password);
        press(&mut i, "esc");
        assert_eq!(i.step, Step::User, "Esc goes back");
        assert_eq!(i.username, "tester", "and keeps what you typed");
    }

    #[test]
    fn password_must_match_and_empty_needs_a_second_enter() {
        let mut i = inst_with(Arc::default(), disks());
        i.step = Step::Password;
        type_text(&mut i, "abc");
        press(&mut i, "enter");
        assert_eq!(i.pw_field, 1);
        type_text(&mut i, "abd");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Password);
        assert_eq!(i.error.as_deref(), Some("The two passwords do not match."));
        press(&mut i, "backspace");
        type_text(&mut i, "c");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Disk);
        assert_eq!(i.config().password.as_deref(), Some("abc"));
        // empty password: warned once, then allowed
        let mut j = inst_with(Arc::default(), disks());
        j.step = Step::Password;
        press(&mut j, "enter,enter");
        assert_eq!(j.step, Step::Password);
        assert!(j.error.as_deref().unwrap().starts_with("No password"));
        press(&mut j, "enter");
        assert_eq!(j.step, Step::Disk);
        assert_eq!(j.config().password, None);
    }

    #[test]
    fn password_is_typed_on_the_chosen_layout() {
        let mut i = inst_with(Arc::default(), disks());
        i.layout = Layout::Hu;
        i.step = Step::Password;
        // physical keys: s e c r e t 7 and the key that is '-' on a Hungarian keyboard (US '/' position, code 53)
        for code in [31u16, 18, 46, 19, 18, 20, 8, 53] { i.handle(Key::new(code)); }
        assert_eq!(i.pw1, "secret7-");
        // y and z swap on this layout
        i.pw1.clear();
        for code in [21u16, 44] { i.handle(Key::new(code)); }
        assert_eq!(i.pw1, "zy");
    }

    #[test]
    fn full_walkthrough_installs_only_after_typing_the_disk_name() {
        let started = Arc::new(AtomicUsize::new(0));
        let mut i = inst_with(started.clone(), disks());
        press(&mut i, "enter,enter,enter,enter");           // welcome, theme, keyboard, timezone
        type_text(&mut i, "");
        press(&mut i, "enter");                              // hostname (default exec85)
        type_text(&mut i, "tester");
        press(&mut i, "enter");
        type_text(&mut i, "pw1");
        press(&mut i, "tab");
        type_text(&mut i, "pw1");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Disk);
        assert_eq!(i.disk_pk.items.len(), 2);
        assert!(i.disk_pk.items[1].contains("sda") && i.disk_pk.items[1].contains("4.0TB"));
        press(&mut i, "down,enter");
        assert_eq!(i.step, Step::Confirm);
        assert_eq!(i.config().disk, "sda");
        // wrong confirmation: nothing happens
        type_text(&mut i, "sdb");
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Confirm);
        assert_eq!(started.load(Ordering::SeqCst), 0, "not started yet");
        assert!(i.error.as_deref().unwrap().contains("\"sda\""));
        press(&mut i, "backspace");
        type_text(&mut i, "a");
        press(&mut i, "enter");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        // the fake runner finished instantly; the screen moves on to Done after polling
        i.poll();
        assert_eq!(i.step, Step::Done);
        assert_eq!(press(&mut i, "enter"), vec![Outcome::Reboot]);
        let c = i.config();
        assert_eq!((c.disk.as_str(), c.hostname.as_str(), c.username.as_str()), ("sda", "exec85", "tester"));
    }

    // ---- TESTING-ONLY: the "Skip installation" choice ----

    fn testing_inst(started: Arc<AtomicUsize>) -> Installer {
        Installer::new(100, 40, Mode::Dark, vec!["UTC".into()], true, Box::new(disks), Box::new(move |cfg, shared| {
            started.fetch_add(1, Ordering::SeqCst);
            assert_eq!(cfg.disk, "", "skipping means no disk is handed to the installer");
            let mut p = shared.lock().unwrap();
            p.result = Some(Ok(())); p.done = p.total;
        }))
    }

    #[test]
    fn testing_build_offers_skip_first_and_it_needs_no_disk_name() {
        let started = Arc::new(AtomicUsize::new(0));
        let mut i = testing_inst(started.clone());
        i.username = "tester".into();
        i.goto(Step::Disk);
        assert!(i.disk_pk.items[0].starts_with("Skip installation"));
        assert_eq!(i.disk_pk.items.len(), 3, "skip + the two real disks");
        assert!(i.skip_selected());
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Confirm);
        assert!(press(&mut i, "x").is_empty(), "typing does nothing here");
        press(&mut i, "enter");
        assert_eq!(started.load(Ordering::SeqCst), 1);
        i.poll();
        assert_eq!(i.step, Step::Done);
        assert_eq!(press(&mut i, "enter"), vec![Outcome::SetupDone]);
        assert!(i.state_line().contains("disk=(memory)"));
    }

    #[test]
    fn testing_build_still_installs_normally_when_a_disk_is_chosen() {
        let mut i = Installer::new(100, 40, Mode::Dark, vec!["UTC".into()], true, Box::new(disks), Box::new(|cfg, s| { assert_eq!(cfg.disk, "sda"); s.lock().unwrap().result = Some(Ok(())); }));
        i.goto(Step::Disk);
        press(&mut i, "down,down,enter"); // third entry = sda
        assert_eq!(i.step, Step::Confirm);
        type_text(&mut i, "sda");
        press(&mut i, "enter");
        i.poll();
        assert_eq!(press(&mut i, "enter"), vec![Outcome::Reboot]);
    }

    #[test]
    fn normal_build_has_no_skip_choice() {
        let mut i = inst_with(Arc::default(), disks());
        i.goto(Step::Disk);
        assert!(!i.disk_pk.items.iter().any(|s| s.contains("Skip")));
        assert!(!i.skip_selected());
    }

    #[test]
    fn a_failed_installation_goes_back_to_choosing_a_disk() {
        let mut i = Installer::new(100, 40, Mode::Dark, vec!["UTC".into()], false, Box::new(disks), Box::new(|_, s| { s.lock().unwrap().result = Some(Err("boom".into())); }));
        i.step = Step::Confirm;
        i.goto(Step::Disk);
        press(&mut i, "enter");
        type_text(&mut i, "nvme0n1");
        press(&mut i, "enter");
        i.poll();
        assert_eq!((i.step, i.failed.as_deref()), (Step::Done, Some("boom")));
        assert!(i.state_line().contains("result=failed"));
        assert!(press(&mut i, "enter").is_empty());
        assert_eq!(i.step, Step::Disk);
    }

    #[test]
    fn no_disks_means_no_way_forward() {
        let mut i = inst_with(Arc::default(), vec![]);
        i.goto(Step::Disk);
        press(&mut i, "enter");
        assert_eq!(i.step, Step::Disk);
        assert!(i.error.as_deref().unwrap().contains("No suitable disk"));
    }

    #[test]
    fn state_line_has_every_field() {
        let i = inst_with(Arc::default(), disks());
        let s = i.state_line();
        for k in ["step=welcome", "theme=dark", "layout=us", "tz=UTC", "host=exec85", "user=-", "pw=empty", "disk=-", "result=none", "error=none"] {
            assert!(s.contains(&format!(" {k}")) || s.contains(k), "{k} missing in {s}");
        }
    }

    #[test]
    fn every_step_draws_at_several_sizes() {
        for (w, h) in [(640, 400), (1024, 768), (1920, 1080)] {
            let m = Metrics::for_height(h);
            let mut i = inst_with(Arc::default(), disks());
            i.cols = w / m.cw; i.rows = h / m.ch;
            for step in ORDER {
                i.goto(step);
                i.username = "tester".into();
                if step == Step::Installing { i.progress.lock().unwrap().total = 8; }
                let mut c = Canvas::new(w, h);
                draw(&i, &mut c, &m);
            }
        }
    }
}
