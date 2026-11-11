//! The shell's state and behaviour: menus, dialogs, focus, drives, the file list.
//! Nothing here touches the screen or the keyboard device, so it is easy to test.
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
pub enum Focus { Drives, Files, Details }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Act {
    Open, Parent, Refresh, Reboot, PowerOff,
    Dark, Light, SortName, SortSize, SortDate, Hidden,
    NextPane, Stacked, Details,
    Keys, About, Sep,
}

/// Things the shell asks the outside world to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Effect { PowerOff, Reboot, SaveTheme(Mode) }

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Dialog { Confirm(Act), Keys, About }

pub struct Item { pub label: &'static str, pub hint: &'static str, pub act: Act }
pub struct MenuDef { pub title: &'static str, pub hot: char, pub items: &'static [Item] }

const fn it(label: &'static str, hint: &'static str, act: Act) -> Item { Item { label, hint, act } }

pub const MENUS: [MenuDef; 4] = [
    MenuDef { title: "File", hot: 'f', items: &[
        it("Open", "Enter", Act::Open), it("Parent folder", "Backspace", Act::Parent), it("Refresh", "F5", Act::Refresh),
        it("", "", Act::Sep), it("Restart", "", Act::Reboot), it("Shut down", "", Act::PowerOff),
    ]},
    MenuDef { title: "View", hot: 'v', items: &[
        it("Dark theme", "", Act::Dark), it("Light theme", "", Act::Light), it("", "", Act::Sep),
        it("Sort by name", "", Act::SortName), it("Sort by size", "", Act::SortSize), it("Sort by date", "", Act::SortDate),
        it("", "", Act::Sep), it("Show hidden files", "Ctrl+H", Act::Hidden),
    ]},
    MenuDef { title: "Window", hot: 'w', items: &[
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
    /// One-line message for the status bar (errors, hints); cleared by the next key.
    pub msg: Option<String>,
}

impl App {
    pub fn new(cols: usize, rows: usize, mode: Mode, start: PathBuf) -> App {
        let mut a = App {
            cols, rows, mode, drives: fs::drives(), drive_cursor: 0, cwd: PathBuf::from("/"), entries: Vec::new(),
            sel: 0, top: 0, sort: Sort::Name, show_hidden: false, focus: Focus::Files, stacked: false, details: true,
            menu: None, dialog: None, msg: None,
        };
        if !a.open_dir(start, None) { a.open_dir(PathBuf::from("/"), None); a.msg = None; }
        a
    }

    // ---- geometry -------------------------------------------------------

    /// Where each window sits. They tile the area between the drive row and the status bar.
    pub fn tiles(&self) -> Vec<(Win, Rect)> {
        let area = Rect { x: 0, y: WIN_Y, w: self.cols, h: self.rows.saturating_sub(WIN_Y + 1) };
        let root = if self.details {
            Node::Split {
                dir: if self.stacked { Dir::Stacked } else { Dir::SideBySide },
                pct: if self.stacked { 62 } else { 64 },
                a: Box::new(Node::Leaf(Win::Files)), b: Box::new(Node::Leaf(Win::Details)),
            }
        } else { Node::Leaf(Win::Files) };
        let mut out = Vec::new();
        root.tiles(area, &mut out);
        out
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

    fn activate(&mut self) {
        let Some(e) = self.selected().cloned() else { return };
        if e.is_parent { self.go_parent(); }
        else if e.is_dir { self.open_dir(self.cwd.join(&e.name), None); }
        else { self.msg = Some("No app can open files yet (Notepad arrives in a later release).".into()); }
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
            Act::Stacked => self.stacked,
            Act::Details => self.details,
            _ => return None,
        })
    }

    fn run(&mut self, act: Act) -> Option<Effect> {
        match act {
            Act::Open => self.activate(),
            Act::Parent => self.go_parent(),
            Act::Refresh => self.refresh(),
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
            Act::Keys => self.dialog = Some(Dialog::Keys),
            Act::About => self.dialog = Some(Dialog::About),
            Act::Sep => {}
        }
        None
    }

    pub fn handle(&mut self, k: Key) -> Option<Effect> {
        self.msg = None;
        if let Some(d) = self.dialog { return self.handle_dialog(d, k); }
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
        if k.ctrl {
            match k.base_char() {
                Some('h') => return self.run(Act::Hidden),
                Some('l') => return self.run(Act::Stacked),
                Some('d') => return self.run(Act::Details),
                _ => return None,
            }
        }
        match k.code {
            code::F10 => { self.menu = Some(MenuState { menu: 0, item: first_item(0) }); return None; }
            code::F1 => return self.run(Act::Keys),
            code::F5 => return self.run(Act::Refresh),
            code::TAB => { self.next_pane(if k.shift { -1 } else { 1 }); return None; }
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
                code::ENTER | code::RIGHT => self.activate(),
                code::BACKSPACE | code::LEFT => self.go_parent(),
                _ => if let Some(c) = k.ch() { if c.is_ascii_graphic() { self.jump_to_letter(c); } },
            },
            Focus::Drives => match k.code {
                code::LEFT => self.drive_cursor = self.drive_cursor.saturating_sub(1),
                code::RIGHT => self.drive_cursor = (self.drive_cursor + 1).min(self.drives.len().saturating_sub(1)),
                code::HOME => self.drive_cursor = 0,
                code::END => self.drive_cursor = self.drives.len().saturating_sub(1),
                code::ENTER | code::DOWN => { self.go_to_drive(); self.focus = Focus::Files; }
                _ => {}
            },
            Focus::Details => {}
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
                    return Some(if act == Act::Reboot { Effect::Reboot } else { Effect::PowerOff });
                }
                if k.code == code::ESC || c == Some('n') { self.dialog = None; }
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
        let dialog = match self.dialog { None => "none".to_string(), Some(d) => format!("{d:?}").to_lowercase() };
        format!(
            "EXEC85_STATE dir={} index={} name={} count={} drive={} focus={} menu={} item={} dialog={} theme={} sort={} hidden={} layout={} details={}",
            clean(&self.cwd.to_string_lossy()), self.sel, clean(self.selected().map_or("-", |e| e.name.as_str())), self.entries.len(),
            self.current_drive(), format!("{:?}", self.focus).to_lowercase(), menu, self.menu.map_or(0, |m| m.item), dialog.replace(['(', ')'], "_"),
            if self.mode == Mode::Dark { "dark" } else { "light" }, format!("{:?}", self.sort).to_lowercase(), self.show_hidden,
            if self.stacked { "stacked" } else { "side" }, self.details,
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
