//! The shell's state and behaviour: menus, dialogs, focus, drives, the file list.
//! Nothing here touches the screen or the keyboard device, so it is easy to test.
use crate::apps::Kind;
use crate::fileops;
use crate::fs::{self, Drive, Entry, Sort};
use crate::gfx::Mode;
use crate::keys::{code, Key};
use crate::layout::{Dir, Node, Rect, Win};
use std::path::PathBuf;

/// Screen rows: 0 = menu bar, 1..=4 = drive row, 5.. = windows, last row = status bar.
pub const DRIVE_Y: usize = 1;
pub const DRIVE_H: usize = 4;
pub const WIN_Y: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Focus { Drives, Files, Details, App(usize) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Open, Parent, Refresh, NewFile, NewFolder, Rename, Delete, Copy, Cut, Paste, Eject, Logout, Reboot, PowerOff,
    OpenApp(Kind), NextWindow, CloseWindow,
    Dark, Light, SortName, SortSize, SortDate, Hidden,
    NextPane, Stacked, Details,
    Keys, About, Sep,
    Sleep, SleepIn(u32),
}

/// Things the shell asks the outside world (or the window manager) to do.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Effect { PowerOff, Reboot, Logout, SaveTheme(Mode), OpenApp(Kind), OpenFile(PathBuf), CloseWindow(usize), Eject(PathBuf), Sleep, SaveSleep(u32), ScreenPower(bool) }

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PromptKind { NewFile, NewFolder, Rename(String) }

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Dialog { Confirm(Act), Prompt(PromptKind, String), Keys, About }

pub struct Item { pub label: &'static str, pub hint: &'static str, pub act: Act }
pub struct MenuDef { pub title: &'static str, pub hot: char, pub items: &'static [Item] }

const fn it(label: &'static str, hint: &'static str, act: Act) -> Item { Item { label, hint, act } }

pub const MENUS: [MenuDef; 5] = [
    MenuDef { title: "File", hot: 'f', items: &[
        it("Open", "Enter", Act::Open), it("Parent folder", "Backspace", Act::Parent), it("Refresh", "F5", Act::Refresh),
        it("", "", Act::Sep),
        it("New file", "Ctrl+N", Act::NewFile), it("New folder", "F7", Act::NewFolder), it("Rename", "F2", Act::Rename), it("Delete", "Del", Act::Delete),
        it("", "", Act::Sep),
        it("Copy", "Ctrl+C", Act::Copy), it("Cut", "Ctrl+X", Act::Cut), it("Paste", "Ctrl+V", Act::Paste),
        it("Eject drive", "E in drives", Act::Eject),
        it("", "", Act::Sep), it("Log out", "", Act::Logout), it("Restart", "", Act::Reboot), it("Shut down", "", Act::PowerOff),
    ]},
    MenuDef { title: "View", hot: 'v', items: &[
        it("Dark theme", "", Act::Dark), it("Light theme", "", Act::Light), it("", "", Act::Sep),
        it("Sort by name", "", Act::SortName), it("Sort by size", "", Act::SortSize), it("Sort by date", "", Act::SortDate),
        it("", "", Act::Sep), it("Show hidden files", "Ctrl+H", Act::Hidden),
        it("", "", Act::Sep), it("Sleep now (screen off)", "F8", Act::Sleep),
        it("Sleep after 5 minutes", "", Act::SleepIn(5)), it("Sleep after 15 minutes", "", Act::SleepIn(15)), it("Sleep after 60 minutes", "", Act::SleepIn(60)), it("Never sleep by itself", "", Act::SleepIn(0)),
    ]},
    MenuDef { title: "Apps", hot: 'a', items: &[
        it("Notepad", "", Act::OpenApp(Kind::Notepad)), it("Terminal (Cmd)", "", Act::OpenApp(Kind::Terminal)),
        it("Paint", "", Act::OpenApp(Kind::Paint)), it("System monitor", "", Act::OpenApp(Kind::Monitor)),
        it("Disk usage", "", Act::OpenApp(Kind::DiskUsage)), it("Network", "", Act::OpenApp(Kind::Network)),
        it("Clock and calendar", "", Act::OpenApp(Kind::Clock)), it("Calculator", "", Act::OpenApp(Kind::Calculator)),
        it("Reversi", "", Act::OpenApp(Kind::Reversi)), it("Snake", "", Act::OpenApp(Kind::Snake)),
        it("Services", "", Act::OpenApp(Kind::Services)), it("Software", "", Act::OpenApp(Kind::Software)),
        it("Minecraft server", "", Act::OpenApp(Kind::Minecraft)), it("Discord bot", "", Act::OpenApp(Kind::DiscordBot)), it("Remote access", "", Act::OpenApp(Kind::Remote)), it("App catalog", "", Act::OpenApp(Kind::Catalog)), it("Browser", "", Act::OpenApp(Kind::Browser)),
    ]},
    MenuDef { title: "Window", hot: 'w', items: &[
        it("Next window", "F6", Act::NextWindow), it("Close window", "F4", Act::CloseWindow), it("", "", Act::Sep),
        it("Next pane", "Tab", Act::NextPane), it("Stack panes", "Ctrl+L", Act::Stacked), it("Details pane", "Ctrl+D", Act::Details),
    ]},
    MenuDef { title: "Help", hot: 'h', items: &[
        it("Keyboard shortcuts", "F1", Act::Keys), it("About Exec85", "", Act::About),
    ]},
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MenuState { pub menu: usize, pub item: usize }

fn first_item(menu: usize) -> usize {
    MENUS[menu].items.iter().position(|i| i.act != Act::Sep).unwrap_or(0)
}

pub struct App {
    pub cols: usize,
    pub rows: usize,
    pub mode: Mode,
    pub drives: Vec<Drive>,
    pub drive_cursor: usize,
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub sel: usize,
    pub top: usize,
    pub sort: Sort,
    pub show_hidden: bool,
    pub focus: Focus,
    pub stacked: bool,
    pub details: bool,
    pub menu: Option<MenuState>,
    pub dialog: Option<Dialog>,
    /// How many app windows are open next to the Explorer (the shell keeps this up to date).
    pub app_count: usize,
    /// Copied or cut item: (path, is_cut).
    pub clipboard: Option<(PathBuf, bool)>,
    /// Minutes without a key before the screen turns off by itself (0 = never).
    pub sleep_minutes: u32,
    pub asleep: bool,
    /// One-line message for the status bar (errors, hints); cleared by the next key.
    pub msg: Option<String>,
}

impl App {
    pub fn new(cols: usize, rows: usize, mode: Mode, start: PathBuf) -> App {
        let mut a = App {
            cols, rows, mode, drives: fs::drives(), drive_cursor: 0, cwd: PathBuf::from("/"), entries: Vec::new(),
            sel: 0, top: 0, sort: Sort::Name, show_hidden: false, focus: Focus::Files, stacked: false, details: true,
            menu: None, dialog: None, app_count: 0, clipboard: None, sleep_minutes: 0, asleep: false, msg: None,
        };
        if !a.open_dir(start, None) { a.open_dir(PathBuf::from("/"), None); a.msg = None; }
        a
    }

    // ---- geometry -------------------------------------------------------

    /// Where each window sits. They tile the area between the drive row and the status bar.
    /// Without apps: Files and Details. With apps: the Explorer takes one side (Files over Details)
    /// and the app windows share the other; "stack panes" swaps the two directions.
    pub fn tiles(&self) -> Vec<(Win, Rect)> {
        let area = Rect { x: 0, y: WIN_Y, w: self.cols, h: self.rows.saturating_sub(WIN_Y + 1) };
        let main = if self.stacked { Dir::Stacked } else { Dir::SideBySide };
        let other = if self.stacked { Dir::SideBySide } else { Dir::Stacked };
        let leaf = |w: Win| Box::new(Node::Leaf(w));
        let root = if self.app_count == 0 {
            if self.details { Node::Split { dir: main, pct: if self.stacked { 62 } else { 64 }, a: leaf(Win::Files), b: leaf(Win::Details) } } else { Node::Leaf(Win::Files) }
        } else {
            // The app windows share their half of the screen: 1 = whole area, 2 = split in two,
            // 3 = one tall window beside two stacked ones, 4 = a 2x2 grid.
            let leaf_app = |i: usize| Box::new(Node::Leaf(Win::App(i)));
            let pair = |dir: Dir, a: usize, b: usize| Node::Split { dir, pct: 50, a: leaf_app(a), b: leaf_app(b) };
            let apps = match self.app_count {
                1 => Node::Leaf(Win::App(0)),
                2 => pair(other, 0, 1),
                3 => Node::Split { dir: Dir::SideBySide, pct: 50, a: leaf_app(0), b: Box::new(pair(Dir::Stacked, 1, 2)) },
                _ => Node::Split { dir: Dir::SideBySide, pct: 50, a: Box::new(pair(Dir::Stacked, 0, 1)), b: Box::new(pair(Dir::Stacked, 2, 3)) },
            };
            let explorer = if self.details { Node::Split { dir: other, pct: 70, a: leaf(Win::Files), b: leaf(Win::Details) } } else { Node::Leaf(Win::Files) };
            Node::Split { dir: main, pct: 36, a: Box::new(explorer), b: Box::new(apps) }
        };
        let mut out = Vec::new();
        root.tiles(area, &mut out);
        out
    }

    /// The shell tells us how many app windows exist; focus must not point at a closed one.
    pub fn set_app_count(&mut self, n: usize) {
        self.app_count = n;
        if let Focus::App(i) = self.focus { if i >= n { self.focus = Focus::Files; } }
        self.fix_scroll();
    }

    /// Where menu `i`'s title starts on the menu bar (in cells).
    pub fn menu_x(i: usize) -> usize { 1 + MENUS[..i].iter().map(|m| m.title.len() + 2).sum::<usize>() }

    /// How many file rows fit in the Files window.
    pub fn list_rows(&self) -> usize {
        self.tiles().iter().find(|(w, _)| *w == Win::Files).map_or(1, |(_, r)| r.h.saturating_sub(3)).max(1)
    }

    pub fn selected(&self) -> Option<&Entry> { self.entries.get(self.sel) }
    pub fn current_drive(&self) -> usize { fs::drive_of(&self.drives, &self.cwd) }

    fn fix_scroll(&mut self) {
        let rows = self.list_rows();
        if self.sel < self.top { self.top = self.sel; }
        if self.sel >= self.top + rows { self.top = self.sel + 1 - rows; }
        let max_top = self.entries.len().saturating_sub(rows);
        if self.top > max_top { self.top = max_top; }
    }

    // ---- navigation -----------------------------------------------------

    /// Show folder `path`. Returns false (and leaves everything as it was) if it cannot be read.
    pub fn open_dir(&mut self, path: PathBuf, select: Option<String>) -> bool {
        match fs::read_dir(&path, self.show_hidden, self.sort) {
            Ok(mut list) => {
                if path.parent().is_some() { list.insert(0, Entry::parent()); }
                self.sel = select.and_then(|n| list.iter().position(|e| e.name == n)).unwrap_or(0);
                self.entries = list;
                self.cwd = path;
                self.top = 0;
                self.drives = fs::drives();
                self.drive_cursor = self.current_drive();
                self.fix_scroll();
                true
            }
            Err(e) => { self.msg = Some(format!("Cannot open {}: {}", path.display(), e)); false }
        }
    }

    /// Look at the mounted drives again (a pendrive may have been plugged in or pulled out). Returns true if the row changed.
    pub fn refresh_drives(&mut self) -> bool {
        let now = fs::drives();
        let key = |v: &[Drive]| v.iter().map(|d| format!("{}|{}", d.label, d.mount.display())).collect::<Vec<_>>();
        if key(&now) == key(&self.drives) { return false; }
        let added: Vec<String> = now.iter().filter(|d| !self.drives.iter().any(|o| o.mount == d.mount)).map(|d| d.label.clone()).collect();
        let removed = self.drives.iter().any(|o| !now.iter().any(|d| d.mount == o.mount));
        self.drives = now;
        if !self.cwd.exists() || !self.drives.iter().any(|d| self.cwd.starts_with(&d.mount)) { self.open_dir(PathBuf::from("/"), None); }
        self.drive_cursor = self.current_drive().min(self.drives.len().saturating_sub(1));
        if let Some(l) = added.first() { self.msg = Some(format!("Drive connected: {l}")); } else if removed { self.msg = Some("A drive was removed.".into()); }
        true
    }

    fn go_parent(&mut self) {
        if let Some(p) = self.cwd.parent().map(|p| p.to_path_buf()) {
            let left = self.cwd.file_name().map(|n| n.to_string_lossy().into_owned());
            self.open_dir(p, left);
        }
    }

    fn refresh(&mut self) {
        let keep = self.selected().map(|e| e.name.clone());
        let cwd = self.cwd.clone();
        if !self.open_dir(cwd, keep) { self.go_parent(); }
    }

    fn activate(&mut self) -> Option<Effect> {
        let e = self.selected().cloned()?;
        if e.is_parent { self.go_parent(); }
        else if e.is_dir { self.open_dir(self.cwd.join(&e.name), None); }
        else { return Some(Effect::OpenFile(self.cwd.join(&e.name))); }
        None
    }

    // ---- file operations --------------------------------------------------

    fn selected_item(&self) -> Option<Entry> { self.selected().filter(|e| !e.is_parent).cloned() }

    fn do_prompt(&mut self, kind: PromptKind, text: &str) {
        let cwd = self.cwd.clone();
        let result = match &kind {
            PromptKind::NewFile => fileops::create_file(&cwd, text),
            PromptKind::NewFolder => fileops::create_dir(&cwd, text),
            PromptKind::Rename(old) => fileops::rename(&cwd, old, text),
        };
        match result {
            Ok(p) => { let name = p.file_name().map(|n| n.to_string_lossy().into_owned()); self.open_dir(cwd, name); }
            Err(e) => self.msg = Some(e),
        }
    }

    fn do_delete(&mut self) {
        let Some(e) = self.selected_item() else { return };
        match fileops::delete(&self.cwd.join(&e.name)) {
            Ok(()) => { let cwd = self.cwd.clone(); self.open_dir(cwd, None); self.msg = Some(format!("Deleted {}", e.name)); }
            Err(m) => self.msg = Some(m),
        }
    }

    fn do_copy(&mut self, cut: bool) {
        let Some(e) = self.selected_item() else { return };
        self.clipboard = Some((self.cwd.join(&e.name), cut));
        self.msg = Some(format!("{} {}", if cut { "Cut" } else { "Copied" }, e.name));
    }

    fn do_paste(&mut self) {
        let Some((src, cut)) = self.clipboard.clone() else { self.msg = Some("Nothing to paste. Copy or cut something first.".into()); return };
        match fileops::paste(&src, &self.cwd, cut) {
            Ok(p) => {
                if cut { self.clipboard = None; }
                let name = p.file_name().map(|n| n.to_string_lossy().into_owned());
                let cwd = self.cwd.clone();
                self.open_dir(cwd, name);
            }
            Err(e) => self.msg = Some(e),
        }
    }

    /// Move the keyboard focus between the Explorer and the app windows (F6).
    fn cycle_window(&mut self, step: isize) {
        let n = self.app_count as isize + 1;
        let cur = match self.focus { Focus::App(i) => i as isize + 1, _ => 0 };
        let next = (cur + step).rem_euclid(n);
        self.focus = if next == 0 { Focus::Files } else { Focus::App(next as usize - 1) };
    }

    /// Keys that always belong to the shell, even when an app window has the focus.
    pub fn is_global(k: &Key) -> bool {
        if matches!(k.code, code::F1 | code::F4 | code::F6 | code::F8 | code::F10) { return true; }
        k.alt && !k.ctrl && k.base_char().map_or(false, |c| MENUS.iter().any(|m| m.hot == c))
    }

    fn move_sel(&mut self, delta: isize) {
        if self.entries.is_empty() { return; }
        let last = self.entries.len() as isize - 1;
        self.sel = (self.sel as isize + delta).clamp(0, last) as usize;
        self.fix_scroll();
    }

    /// Jump to the next entry starting with `c` (type-ahead).
    fn jump_to_letter(&mut self, c: char) {
        let n = self.entries.len();
        let c = c.to_ascii_lowercase();
        for i in 1..=n {
            let idx = (self.sel + i) % n;
            let e = &self.entries[idx];
            if !e.is_parent && e.name.chars().next().map(|f| f.to_ascii_lowercase()) == Some(c) {
                self.sel = idx;
                self.fix_scroll();
                return;
            }
        }
    }

    fn visible_panes(&self) -> Vec<Focus> {
        let mut v = vec![Focus::Drives, Focus::Files];
        if self.details { v.push(Focus::Details); }
        v
    }

    fn next_pane(&mut self, step: isize) {
        let v = self.visible_panes();
        let i = v.iter().position(|f| *f == self.focus).unwrap_or(0) as isize;
        self.focus = v[(i + step).rem_euclid(v.len() as isize) as usize];
    }

    // ---- keys -----------------------------------------------------------

    pub fn checked(&self, act: Act) -> Option<bool> {
        Some(match act {
            Act::Dark => self.mode == Mode::Dark,
            Act::Light => self.mode == Mode::Light,
            Act::SortName => self.sort == Sort::Name,
            Act::SortSize => self.sort == Sort::Size,
            Act::SortDate => self.sort == Sort::Date,
            Act::Hidden => self.show_hidden,
            Act::SleepIn(m) => self.sleep_minutes == m,
            Act::Stacked => self.stacked,
            Act::Details => self.details,
            _ => return None,
        })
    }

    fn run(&mut self, act: Act) -> Option<Effect> {
        match act {
            Act::Open => return self.activate(),
            Act::Parent => self.go_parent(),
            Act::Refresh => self.refresh(),
            Act::NewFile => self.dialog = Some(Dialog::Prompt(PromptKind::NewFile, String::new())),
            Act::NewFolder => self.dialog = Some(Dialog::Prompt(PromptKind::NewFolder, String::new())),
            Act::Rename => if let Some(e) = self.selected_item() { self.dialog = Some(Dialog::Prompt(PromptKind::Rename(e.name.clone()), e.name)); },
            Act::Delete => if self.selected_item().is_some() { self.dialog = Some(Dialog::Confirm(Act::Delete)); },
            Act::Copy => self.do_copy(false),
            Act::Cut => self.do_copy(true),
            Act::Paste => self.do_paste(),
            Act::Eject => {
                // The drive under the cursor in the drive row, otherwise the drive of the folder being shown.
                let i = if self.focus == Focus::Drives { self.drive_cursor } else { self.current_drive() };
                match self.drives.get(i) {
                    Some(d) if d.mount.starts_with("/media/") => return Some(Effect::Eject(d.mount.clone())),
                    _ => self.msg = Some("That drive cannot be ejected (only pendrives and other USB drives can).".into()),
                }
            }
            Act::OpenApp(kind) => return Some(Effect::OpenApp(kind)),
            Act::NextWindow => self.cycle_window(1),
            Act::CloseWindow => if let Focus::App(i) = self.focus { return Some(Effect::CloseWindow(i)); } else { self.msg = Some("Only app windows can be closed.".into()); },
            Act::Logout => return Some(Effect::Logout),
            Act::Reboot | Act::PowerOff => self.dialog = Some(Dialog::Confirm(act)),
            Act::Dark => { self.mode = Mode::Dark; return Some(Effect::SaveTheme(Mode::Dark)); }
            Act::Light => { self.mode = Mode::Light; return Some(Effect::SaveTheme(Mode::Light)); }
            Act::SortName => { self.sort = Sort::Name; self.refresh(); }
            Act::SortSize => { self.sort = Sort::Size; self.refresh(); }
            Act::SortDate => { self.sort = Sort::Date; self.refresh(); }
            Act::Hidden => { self.show_hidden = !self.show_hidden; self.refresh(); }
            Act::NextPane => self.next_pane(1),
            Act::Stacked => { self.stacked = !self.stacked; self.fix_scroll(); }
            Act::Details => {
                self.details = !self.details;
                if !self.details && self.focus == Focus::Details { self.focus = Focus::Files; }
                self.fix_scroll();
            }
            Act::Sleep => return Some(Effect::Sleep),
            Act::SleepIn(m) => { self.sleep_minutes = m; return Some(Effect::SaveSleep(m)); }
            Act::Keys => self.dialog = Some(Dialog::Keys),
            Act::About => self.dialog = Some(Dialog::About),
            Act::Sep => {}
        }
        None
    }

    pub fn handle(&mut self, k: Key) -> Option<Effect> {
        self.msg = None;
        if let Some(d) = self.dialog.clone() { return self.handle_dialog(d, k); }
        if self.menu.is_some() { return self.handle_menu(k); }

        // Alt+letter opens a menu; F10 opens the first one.
        if k.alt && !k.ctrl {
            if let Some(c) = k.base_char() {
                if let Some(i) = MENUS.iter().position(|m| m.hot == c) { self.menu = Some(MenuState { menu: i, item: first_item(i) }); return None; }
            }
            if k.code == code::LEFT || k.code == code::RIGHT {
                let n = self.drives.len() as isize;
                let step = if k.code == code::LEFT { -1 } else { 1 };
                self.drive_cursor = (self.drive_cursor as isize + step).rem_euclid(n.max(1)) as usize;
                self.go_to_drive();
                return None;
            }
        }
        match k.code {
            code::F6 => { self.cycle_window(if k.shift { -1 } else { 1 }); return None; }
            code::F4 => return self.run(Act::CloseWindow),
            _ => {}
        }
        if k.ctrl {
            if matches!(self.focus, Focus::App(_)) { return None; }
            match k.base_char() {
                Some('h') => return self.run(Act::Hidden),
                Some('l') => return self.run(Act::Stacked),
                Some('d') => return self.run(Act::Details),
                Some('n') => return self.run(Act::NewFile),
                Some('c') => return self.run(Act::Copy),
                Some('x') => return self.run(Act::Cut),
                Some('v') => return self.run(Act::Paste),
                _ => return None,
            }
        }
        match k.code {
            code::F10 => { self.menu = Some(MenuState { menu: 0, item: first_item(0) }); return None; }
            code::F1 => return self.run(Act::Keys),
            code::F5 => return self.run(Act::Refresh),
            code::F8 => return self.run(Act::Sleep),
            code::TAB if !matches!(self.focus, Focus::App(_)) => { self.next_pane(if k.shift { -1 } else { 1 }); return None; }
            _ => {}
        }
        match self.focus {
            Focus::Files => match k.code {
                code::UP => self.move_sel(-1),
                code::DOWN => self.move_sel(1),
                code::PGUP => self.move_sel(-(self.list_rows() as isize)),
                code::PGDN => self.move_sel(self.list_rows() as isize),
                code::HOME => self.move_sel(isize::MIN / 2),
                code::END => self.move_sel(isize::MAX / 2),
                code::ENTER | code::RIGHT => return self.activate(),
                code::BACKSPACE | code::LEFT => self.go_parent(),
                code::F2 => return self.run(Act::Rename),
                code::F7 => return self.run(Act::NewFolder),
                code::DELETE => return self.run(Act::Delete),
                _ => if let Some(c) = k.ch() { if c.is_ascii_graphic() { self.jump_to_letter(c); } },
            },
            Focus::Drives => match k.code {
                code::LEFT => self.drive_cursor = self.drive_cursor.saturating_sub(1),
                code::RIGHT => self.drive_cursor = (self.drive_cursor + 1).min(self.drives.len().saturating_sub(1)),
                code::HOME => self.drive_cursor = 0,
                code::END => self.drive_cursor = self.drives.len().saturating_sub(1),
                code::ENTER | code::DOWN => { self.go_to_drive(); self.focus = Focus::Files; }
                _ => if k.ch().map(|c| c.to_ascii_lowercase()) == Some('e') { return self.run(Act::Eject); },
            },
            Focus::Details | Focus::App(_) => {}
        }
        None
    }

    fn go_to_drive(&mut self) {
        if let Some(d) = self.drives.get(self.drive_cursor) {
            let m = d.mount.clone();
            let cursor = self.drive_cursor;
            if self.open_dir(m, None) { self.drive_cursor = cursor; }
        }
    }

    fn handle_menu(&mut self, k: Key) -> Option<Effect> {
        let Some(mut ms) = self.menu else { return None };
        let items = MENUS[ms.menu].items;
        let step_item = |from: usize, dir: isize| {
            let n = items.len() as isize;
            let mut i = from as isize;
            for _ in 0..items.len() {
                i = (i + dir).rem_euclid(n);
                if items[i as usize].act != Act::Sep { return i as usize; }
            }
            from
        };
        match k.code {
            code::ESC | code::F10 => { self.menu = None; }
            code::UP => { ms.item = step_item(ms.item, -1); self.menu = Some(ms); }
            code::DOWN => { ms.item = step_item(ms.item, 1); self.menu = Some(ms); }
            code::LEFT | code::RIGHT => {
                let n = MENUS.len() as isize;
                ms.menu = (ms.menu as isize + if k.code == code::LEFT { -1 } else { 1 }).rem_euclid(n) as usize;
                ms.item = first_item(ms.menu);
                self.menu = Some(ms);
            }
            code::ENTER => { self.menu = None; return self.run(items[ms.item].act); }
            _ => {
                if k.alt && !k.ctrl {
                    if let Some(i) = k.base_char().and_then(|c| MENUS.iter().position(|m| m.hot == c)) {
                        self.menu = Some(MenuState { menu: i, item: first_item(i) });
                    }
                }
            }
        }
        None
    }

    fn handle_dialog(&mut self, d: Dialog, k: Key) -> Option<Effect> {
        let c = k.base_char().map(|c| c.to_ascii_lowercase());
        match d {
            Dialog::Confirm(act) => {
                if k.code == code::ENTER || c == Some('y') {
                    self.dialog = None;
                    match act {
                        Act::Reboot => return Some(Effect::Reboot),
                        Act::PowerOff => return Some(Effect::PowerOff),
                        Act::Delete => self.do_delete(),
                        _ => {}
                    }
                    return None;
                }
                if k.code == code::ESC || c == Some('n') { self.dialog = None; }
            }
            Dialog::Prompt(kind, mut text) => {
                match k.code {
                    code::ESC => self.dialog = None,
                    code::ENTER => { self.dialog = None; self.do_prompt(kind, &text); }
                    code::BACKSPACE => { text.pop(); self.dialog = Some(Dialog::Prompt(kind, text)); }
                    _ => {
                        if let Some(ch) = k.ch() { if text.chars().count() < 120 { text.push(ch); } }
                        self.dialog = Some(Dialog::Prompt(kind, text));
                    }
                }
            }
            Dialog::Keys | Dialog::About => {
                if matches!(k.code, code::ESC | code::ENTER | code::SPACE | code::F1) { self.dialog = None; }
            }
        }
        None
    }

    /// One line describing the whole state. Printed on the serial console after every
    /// key so automated tests (and bug reports) can see what the shell is doing.
    pub fn state_line(&self) -> String {
        let clean = |s: &str| s.replace(' ', "_");
        let menu = self.menu.map_or("none".to_string(), |m| MENUS[m.menu].title.to_lowercase());
        let dialog = match &self.dialog {
            None => "none".to_string(),
            Some(Dialog::Prompt(kind, _)) => format!("prompt_{}", match kind { PromptKind::NewFile => "newfile", PromptKind::NewFolder => "newfolder", PromptKind::Rename(_) => "rename" }),
            Some(d) => format!("{d:?}").to_lowercase(),
        };
        format!(
            "EXEC85_STATE dir={} index={} name={} count={} drive={} focus={} menu={} item={} dialog={} theme={} sort={} hidden={} layout={} details={} drives={}",
            clean(&self.cwd.to_string_lossy()), self.sel, clean(self.selected().map_or("-", |e| e.name.as_str())), self.entries.len(),
            self.current_drive(), match self.focus { Focus::App(i) => format!("app{i}"), f => format!("{f:?}").to_lowercase() }, menu, self.menu.map_or(0, |m| m.item), dialog.replace(['(', ')'], "_"),
            if self.mode == Mode::Dark { "dark" } else { "light" }, format!("{:?}", self.sort).to_lowercase(), self.show_hidden,
            if self.stacked { "stacked" } else { "side" }, self.details, self.drives.iter().map(|d| d.label.replace(' ', "_")).collect::<Vec<_>>().join(","),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::fs as stdfs;

    /// Build   root/{alpha/{inner/}, beta/, .hidden, readme.txt, Zed.txt}
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("exec85-app-{}-{}", name, std::process::id()));
        let _ = stdfs::remove_dir_all(&root);
        stdfs::create_dir_all(root.join("alpha/inner")).unwrap();
        stdfs::create_dir_all(root.join("beta")).unwrap();
        stdfs::write(root.join(".hidden"), b"h").unwrap();
        stdfs::write(root.join("readme.txt"), b"hello").unwrap();
        stdfs::write(root.join("Zed.txt"), b"z").unwrap();
        root
    }

    fn press(a: &mut App, keys: &str) -> Vec<Effect> {
        keys.split(',').filter(|s| !s.is_empty()).filter_map(|n| a.handle(parse_key(n).unwrap_or_else(|| panic!("bad key {n}")))).collect()
    }

    fn app(root: &PathBuf) -> App { App::new(100, 40, Mode::Dark, root.clone()) }

    #[test]
    fn arrows_move_through_the_list_and_stop_at_the_ends() {
        let root = tree("arrows");
        let mut a = app(&root);
        // entries: .., alpha, beta, readme.txt, Zed.txt
        assert_eq!(a.entries.len(), 5);
        assert_eq!(a.sel, 0);
        press(&mut a, "down,down");
        assert_eq!(a.selected().unwrap().name, "beta");
        press(&mut a, "up,up,up,up");
        assert_eq!(a.sel, 0);
        press(&mut a, "end");
        assert_eq!(a.selected().unwrap().name, "Zed.txt");
        press(&mut a, "down,down");
        assert_eq!(a.sel, 4);
        press(&mut a, "home");
        assert_eq!(a.sel, 0);
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn enter_opens_folders_and_backspace_returns_to_the_same_item() {
        let root = tree("enter");
        let mut a = app(&root);
        press(&mut a, "down,enter");
        assert_eq!(a.cwd, root.join("alpha"));
        assert_eq!(a.selected().unwrap().name, "..");
        press(&mut a, "down,enter");
        assert_eq!(a.cwd, root.join("alpha/inner"));
        press(&mut a, "backspace");
        assert_eq!(a.cwd, root.join("alpha"));
        assert_eq!(a.selected().unwrap().name, "inner");
        press(&mut a, "left");
        assert_eq!(a.cwd, root);
        assert_eq!(a.selected().unwrap().name, "alpha");
        press(&mut a, "enter,enter"); // ".." is selected inside alpha? no: selection starts at ".."
        assert_eq!(a.cwd, root);
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn typing_a_letter_jumps_to_matching_names() {
        let root = tree("letters");
        let mut a = app(&root);
        press(&mut a, "z");
        assert_eq!(a.selected().unwrap().name, "Zed.txt");
        press(&mut a, "r");
        assert_eq!(a.selected().unwrap().name, "readme.txt");
        press(&mut a, "b");
        assert_eq!(a.selected().unwrap().name, "beta");
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn hidden_files_toggle_and_sorting() {
        let root = tree("hidden");
        let mut a = app(&root);
        assert!(!a.entries.iter().any(|e| e.name == ".hidden"));
        press(&mut a, "ctrl-h");
        assert!(a.entries.iter().any(|e| e.name == ".hidden"));
        press(&mut a, "ctrl-h");
        assert!(!a.entries.iter().any(|e| e.name == ".hidden"));
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn menu_bar_is_fully_keyboard_driven() {
        let root = tree("menu");
        let mut a = app(&root);
        press(&mut a, "f10");
        assert_eq!(a.menu, Some(MenuState { menu: 0, item: 0 }));
        press(&mut a, "right");
        assert_eq!(a.menu.unwrap().menu, 1);
        press(&mut a, "esc");
        assert!(a.menu.is_none());
        // Alt+V opens View; Down selects "Light theme"; Enter applies it.
        press(&mut a, "alt-v,down");
        assert_eq!(a.menu, Some(MenuState { menu: 1, item: 1 }));
        let fx = press(&mut a, "enter");
        assert_eq!(fx, vec![Effect::SaveTheme(Mode::Light)]);
        assert_eq!(a.mode, Mode::Light);
        assert!(a.menu.is_none());
        // separators are skipped: from "Light theme" Down lands on "Sort by name" (index 3)
        press(&mut a, "alt-v,down,down");
        assert_eq!(a.menu.unwrap().item, 3);
        press(&mut a, "esc");
        // wraps around upward
        press(&mut a, "f10,up");
        assert_eq!(a.menu.unwrap().item, MENUS[0].items.len() - 1);
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn shutting_down_needs_confirmation() {
        let root = tree("power");
        let mut a = app(&root);
        let fx = press(&mut a, "f10,up,enter");
        assert!(fx.is_empty());
        assert_eq!(a.dialog, Some(Dialog::Confirm(Act::PowerOff)));
        assert!(press(&mut a, "x").is_empty(), "other keys do nothing");
        assert!(press(&mut a, "esc").is_empty());
        assert_eq!(a.dialog, None);
        press(&mut a, "f10,up,enter");
        assert_eq!(press(&mut a, "y"), vec![Effect::PowerOff]);
        press(&mut a, "f10,up,up,enter");
        assert_eq!(a.dialog, Some(Dialog::Confirm(Act::Reboot)));
        assert_eq!(press(&mut a, "enter"), vec![Effect::Reboot]);
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn tab_cycles_panes_and_layout_toggles() {
        let root = tree("tabs");
        let mut a = app(&root);
        assert_eq!(a.focus, Focus::Files);
        press(&mut a, "tab");
        assert_eq!(a.focus, Focus::Details);
        press(&mut a, "tab");
        assert_eq!(a.focus, Focus::Drives);
        press(&mut a, "tab");
        assert_eq!(a.focus, Focus::Files);
        press(&mut a, "shift-tab");
        assert_eq!(a.focus, Focus::Drives);
        press(&mut a, "tab,tab,ctrl-d,tab");
        assert!(!a.details);
        assert_eq!(a.focus, Focus::Drives, "hidden details pane is skipped");
        press(&mut a, "ctrl-d,ctrl-l");
        assert!(a.details && a.stacked);
        let t = a.tiles();
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].1.w, a.cols, "stacked windows use the full width");
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn drives_row_is_navigable() {
        let root = tree("drives");
        let mut a = app(&root);
        press(&mut a, "tab,tab"); // Details then Drives
        assert_eq!(a.focus, Focus::Drives);
        press(&mut a, "right,left,home,enter");
        assert_eq!(a.focus, Focus::Files);
        assert_eq!(a.cwd, a.drives[0].mount);
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn long_lists_scroll_with_the_selection() {
        let root = std::env::temp_dir().join(format!("exec85-app-long-{}", std::process::id()));
        let _ = stdfs::remove_dir_all(&root);
        stdfs::create_dir_all(&root).unwrap();
        for i in 0..100 { stdfs::write(root.join(format!("f{i:03}")), b"x").unwrap(); }
        let mut a = App::new(100, 20, Mode::Dark, root.clone());
        let rows = a.list_rows();
        assert!(rows < 20);
        press(&mut a, "end");
        assert_eq!(a.sel, a.entries.len() - 1);
        assert_eq!(a.top, a.entries.len() - rows, "last page shown");
        press(&mut a, "pgup");
        assert!(a.sel < a.entries.len() - 1 && a.sel >= a.top && a.sel < a.top + rows);
        press(&mut a, "home");
        assert_eq!((a.sel, a.top), (0, 0));
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_folder_reports_an_error_and_stays_put() {
        let root = tree("err");
        let mut a = app(&root);
        assert!(!a.open_dir(root.join("does-not-exist"), None));
        assert_eq!(a.cwd, root);
        assert!(a.msg.as_deref().unwrap().starts_with("Cannot open"));
        let _ = stdfs::remove_dir_all(&root);
    }


    #[test]
    fn creating_renaming_and_deleting_through_the_keyboard() {
        let root = tree("fileops");
        let mut a = app(&root);
        // new folder: F7, type a name, Enter
        press(&mut a, "f7");
        assert!(matches!(a.dialog, Some(Dialog::Prompt(PromptKind::NewFolder, _))));
        for c in "docs".chars() { a.handle(parse_key(&c.to_string()).unwrap()); }
        press(&mut a, "enter");
        assert!(root.join("docs").is_dir());
        assert_eq!(a.selected().unwrap().name, "docs", "the new item is selected");
        // new file: Ctrl+N
        press(&mut a, "ctrl-n");
        for c in "todo.txt".chars() { a.handle(parse_key(&c.to_string()).unwrap()); }
        press(&mut a, "enter");
        assert!(root.join("todo.txt").is_file());
        // rename it: F2 starts with the old name filled in
        press(&mut a, "f2");
        assert_eq!(a.dialog, Some(Dialog::Prompt(PromptKind::Rename("todo.txt".into()), "todo.txt".into())));
        for _ in 0..8 { press(&mut a, "backspace"); }
        for c in "plan.md".chars() { a.handle(parse_key(&c.to_string()).unwrap()); }
        press(&mut a, "enter");
        assert!(root.join("plan.md").is_file() && !root.join("todo.txt").exists());
        // a bad name is refused and nothing changes
        press(&mut a, "f7,a,/,b,enter");
        assert!(a.msg.is_some());
        assert!(!root.join("a").exists());
        // delete asks first
        press(&mut a, "delete");
        assert_eq!(a.dialog, Some(Dialog::Confirm(Act::Delete)));
        press(&mut a, "n");
        assert!(root.join("plan.md").exists());
        press(&mut a, "delete,y");
        assert!(!root.join("plan.md").exists());
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn copy_cut_and_paste() {
        let root = tree("paste");
        let mut a = app(&root);
        press(&mut a, "z");                         // Zed.txt
        press(&mut a, "ctrl-c");
        press(&mut a, "down,down");                 // wraps? stays at the end: fine
        press(&mut a, "ctrl-v");
        assert!(root.join("Zed (copy).txt").exists(), "paste into the same folder gets a new name");
        // cut a file and paste it into a folder
        press(&mut a, "r");                         // readme.txt
        press(&mut a, "ctrl-x");
        press(&mut a, "a,enter");                   // into alpha/
        assert_eq!(a.cwd, root.join("alpha"));
        press(&mut a, "ctrl-v");
        assert!(root.join("alpha/readme.txt").exists() && !root.join("readme.txt").exists());
        press(&mut a, "ctrl-v");
        assert!(a.msg.as_deref().unwrap().contains("Nothing to paste"), "a cut item is pasted only once");
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn enter_on_a_file_asks_for_it_to_be_opened() {
        let root = tree("openfile");
        let mut a = app(&root);
        press(&mut a, "r");
        assert_eq!(a.handle(parse_key("enter").unwrap()), Some(Effect::OpenFile(root.join("readme.txt"))));
        // the Apps menu asks for apps
        press(&mut a, "alt-a");
        assert_eq!(a.menu.unwrap().menu, 2);
        assert_eq!(a.handle(parse_key("enter").unwrap()), Some(Effect::OpenApp(Kind::Notepad)));
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn explorer_leaves_room_for_app_windows_without_overlap() {
        let root = tree("tiles");
        let mut a = app(&root);
        for n in 0..=4 {
            a.set_app_count(n);
            for stacked in [false, true] {
                a.stacked = stacked;
                for details in [false, true] {
                    a.details = details;
                    let t = a.tiles();
                    let total: usize = t.iter().map(|(_, r)| r.w * r.h).sum();
                    assert_eq!(total, a.cols * (a.rows - WIN_Y - 1), "n={n} stacked={stacked} details={details}");
                    assert_eq!(t.len(), n + if details { 2 } else { 1 });
                }
            }
        }
        a.set_app_count(2);
        a.focus = Focus::App(1);
        a.set_app_count(1);
        assert_eq!(a.focus, Focus::Files, "focus never points at a closed window");
        let _ = stdfs::remove_dir_all(&root);
    }

    #[test]
    fn state_line_is_machine_readable() {
        let root = tree("state");
        let a = app(&root);
        let s = a.state_line();
        assert!(s.starts_with("EXEC85_STATE dir="));
        assert!(s.contains(" index=0 name=.. count=5 "));
        assert!(s.contains(" theme=dark ") && s.contains(" focus=files "));
        let _ = stdfs::remove_dir_all(&root);
    }
}
