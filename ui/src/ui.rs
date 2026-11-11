//! Drawing the shell: menu bar, drive row, tiled windows, status bar, menus, dialogs.
use crate::app::{self, Act, App, Dialog, Focus, PromptKind, DRIVE_H, DRIVE_Y, MENUS};
use crate::shell::AppWindow;
use crate::fs::{fmt_size, fmt_time, mode_string};
use crate::gfx::{Canvas, Metrics, Theme};
use crate::paint::{tail, Painter};
use crate::layout::{Rect, Win};

pub fn draw(app: &App, apps: &[AppWindow], c: &mut Canvas, m: &Metrics) {
    let t = Theme::new(app.mode);
    c.fill_rect(0, 0, c.w, c.h, t.bg);
    let mut p = Painter { c, m: *m, t };
    menu_bar(&mut p, app);
    drive_row(&mut p, app);
    for (win, r) in app.tiles() {
        match win {
            Win::Files => files(&mut p, app, r),
            Win::Details => details(&mut p, app, r),
            Win::App(i) => if let Some(w) = apps.get(i) {
                let focused = app.focus == Focus::App(i);
                let inner = p.window(r, &w.app.title(), focused);
                w.app.draw(&mut p, inner, focused);
            },
        }
    }
    status_bar(&mut p, app, apps);
    if let Some(ms) = app.menu { dropdown(&mut p, app, ms); }
    if let Some(d) = app.dialog.clone() { dialog(&mut p, app, d); }
}

fn menu_bar(p: &mut Painter, app: &App) {
    let t = p.t;
    p.cells(0, 0, app.cols, 1, t.bar);
    for (i, m) in MENUS.iter().enumerate() {
        let x = App::menu_x(i);
        let open = app.menu.map_or(false, |ms| ms.menu == i);
        let (fg, bg) = if open { (t.accent_text, t.accent) } else { (t.bar_text, t.bar) };
        p.cells(x - 1, 0, m.title.len() + 2, 1, bg);
        p.text(x, 0, m.title, fg, None);
        p.underline(x, 0, fg);
    }
    let brand = "Exec85";
    if app.cols > brand.len() + 40 { p.text(app.cols - brand.len() - 1, 0, brand, t.dim, None); }
}

fn drive_row(p: &mut Painter, app: &App) {
    let t = p.t;
    let current = app.current_drive();
    let mut col = 1;
    for (i, d) in app.drives.iter().enumerate() {
        let w = 22;
        if col + w > app.cols { break; }
        let cursor = app.focus == Focus::Drives && i == app.drive_cursor;
        let (fg, bg, sub) = if cursor { (t.accent_text, t.accent, t.accent_text) } else { (t.text, t.panel, t.dim) };
        let r = Rect { x: col, y: DRIVE_Y, w, h: DRIVE_H };
        p.cells(r.x, r.y, r.w, r.h, bg);
        p.outline(r, if i == current { t.accent } else { t.border });
        // A little drive icon drawn from rectangles: body, a slot and a light cut out of it.
        let (cw, ch, s) = (p.m.cw, p.m.ch, p.m.scale);
        let (x0, y0, bw, bh) = ((col + 1) * cw + s, (DRIVE_Y + 1) * ch + 4 * s, 3 * cw, 12 * s);
        p.c.fill_rect(x0, y0, bw, bh, fg);
        p.c.fill_rect(x0 + 2 * s, y0 + 4 * s, bw - 4 * s, s, bg);
        p.c.fill_rect(x0 + bw - 5 * s, y0 + bh - 4 * s, 2 * s, 2 * s, bg);
        p.clip(col + 5, DRIVE_Y + 1, w - 6, &d.label, fg, bg);
        let info = if d.total > 0 { format!("{} free", fmt_size(d.free)) } else { d.fstype.clone() };
        p.clip(col + 5, DRIVE_Y + 2, w - 6, &info, sub, bg);
        col += w + 1;
    }
}

fn files(p: &mut Painter, app: &App, r: Rect) {
    let t = p.t;
    let active = app.focus == Focus::Files;
    let title = format!("Files  {}", tail(&app.cwd.to_string_lossy(), r.w.saturating_sub(10)));
    let a = p.window(r, &title, active);
    if a.w < 6 || a.h < 2 { return; }
    // Columns: name | size | modified. Narrow windows drop the right-hand columns.
    let (show_date, show_size) = (a.w >= 44, a.w >= 24);
    let date_w = if show_date { 17 } else { 0 };
    let size_w = if show_size { 8 } else { 0 };
    let name_w = a.w - date_w - size_w;
    p.clip(a.x, a.y, a.w, "", t.dim, t.panel);
    p.text(a.x, a.y, "Name", t.dim, None);
    if show_size { p.right(a.x + name_w, a.y, size_w, "Size ", t.dim, t.panel); }
    if show_date { p.text(a.x + name_w + size_w, a.y, "Modified (UTC)", t.dim, None); }
    let rows = a.h - 1;
    for i in 0..rows {
        let idx = app.top + i;
        let Some(e) = app.entries.get(idx) else { break };
        let y = a.y + 1 + i;
        let (fg, bg) = if idx == app.sel {
            if active { (t.accent_text, t.accent) } else { (t.title_off_text, t.title_off) }
        } else { (t.text, t.panel) };
        p.cells(a.x, y, a.w, 1, bg);
        let name = if e.is_dir && !e.is_parent { format!("{}/", e.name) } else { e.name.clone() };
        p.clip(a.x, y, name_w, &name, fg, bg);
        if show_size {
            let size = if e.is_dir { "<dir> ".to_string() } else { format!("{} ", fmt_size(e.size)) };
            p.right(a.x + name_w, y, size_w, &size, fg, bg);
        }
        if show_date && !e.is_parent { p.text(a.x + name_w + size_w, y, &fmt_time(e.mtime), fg, None); }
    }
    if app.entries.is_empty() { p.text(a.x, a.y + 1, "(empty)", t.dim, None); }
}

fn details(p: &mut Painter, app: &App, r: Rect) {
    let t = p.t;
    let a = p.window(r, "Details", app.focus == Focus::Details);
    if a.w < 12 || a.h < 2 { return; }
    let mut rows: Vec<(String, String)> = Vec::new();
    if let Some(e) = app.selected() {
        if e.is_parent {
            rows.push(("Item".into(), "Parent folder".into()));
        } else {
            let kind = match (e.is_link, e.is_dir) { (true, true) => "Link to folder", (true, false) => "Link", (false, true) => "Folder", _ => "File" };
            rows.push(("Name".into(), e.name.clone()));
            rows.push(("Type".into(), kind.into()));
            if !e.is_dir { rows.push(("Size".into(), format!("{} ({} bytes)", fmt_size(e.size), e.size))); }
            rows.push(("Modified".into(), format!("{} UTC", fmt_time(e.mtime))));
            rows.push(("Access".into(), mode_string(e.mode, e.is_dir, e.is_link)));
        }
    }
    rows.push(("Path".into(), app.cwd.to_string_lossy().into_owned()));
    if let Some(d) = app.drives.get(app.current_drive()) {
        rows.push(("Drive".into(), d.label.clone()));
        rows.push(("Format".into(), d.fstype.clone()));
        if d.total > 0 { rows.push(("Space".into(), format!("{} free of {}", fmt_size(d.free), fmt_size(d.total)))); }
    }
    let label_w = 9;
    for (i, (k, v)) in rows.iter().enumerate() {
        if i >= a.h { break; }
        p.text(a.x, a.y + i, k, t.dim, None);
        p.clip(a.x + label_w, a.y + i, a.w.saturating_sub(label_w), v, t.text, t.panel);
    }
}

fn status_bar(p: &mut Painter, app: &App, apps: &[AppWindow]) {
    let t = p.t;
    let y = app.rows - 1;
    p.cells(0, y, app.cols, 1, t.bar);
    let left = app.msg.clone().unwrap_or_else(|| match app.focus {
        Focus::Files => "F10 Menu   Tab Next pane   Enter Open   Backspace Up   F1 Keys".to_string(),
        Focus::Drives => "Left/Right Choose drive   Enter Go   Tab Next pane   F1 Keys".to_string(),
        Focus::Details => "Tab Next pane   Ctrl+D Hide details   F1 Keys".to_string(),
        Focus::App(i) => apps.get(i).map_or(String::new(), |w| w.app.hint().to_string()),
    });
    let pos = format!("{}/{}", app.sel + 1, app.entries.len());
    let room = app.cols.saturating_sub(pos.len() + 3);
    p.clip(1, y, room, &left, t.bar_text, t.bar);
    p.text(app.cols - pos.len() - 1, y, &pos, t.bar_text, None);
}

fn dropdown(p: &mut Painter, app: &App, ms: app::MenuState) {
    let t = p.t;
    let menu = &MENUS[ms.menu];
    let label_w = menu.items.iter().map(|i| i.label.len()).max().unwrap_or(0);
    let hint_w = menu.items.iter().map(|i| i.hint.len()).max().unwrap_or(0);
    let w = 3 + label_w + if hint_w > 0 { 2 + hint_w } else { 0 } + 2;
    let x = App::menu_x(ms.menu) - 1;
    let r = Rect { x, y: 1, w, h: menu.items.len() + 2 };
    p.cells(r.x, r.y, r.w, r.h, t.panel);
    p.outline(r, t.border);
    for (i, item) in menu.items.iter().enumerate() {
        let y = r.y + 1 + i;
        if item.act == Act::Sep {
            let m = p.m;
            p.c.fill_rect((r.x + 1) * m.cw, y * m.ch + m.ch / 2, (r.w - 2) * m.cw, m.scale, t.border);
            continue;
        }
        let (fg, bg) = if i == ms.item { (t.accent_text, t.accent) } else { (t.text, t.panel) };
        p.cells(r.x + 1, y, r.w - 2, 1, bg);
        if app.checked(item.act) == Some(true) { p.text(r.x + 1, y, "*", fg, None); }
        p.text(r.x + 3, y, item.label, fg, None);
        if !item.hint.is_empty() { p.right(r.x + 3 + label_w + 2, y, hint_w, item.hint, if i == ms.item { fg } else { t.dim }, bg); }
    }
}

const KEYS_HELP: [&str; 13] = [
    "F10 or Alt+letter   Open the menu bar",
    "Up Down PgUp PgDn   Move through the list",
    "Home End            First / last item",
    "Enter or Right      Open the selected folder",
    "Backspace or Left   Go to the parent folder",
    "A letter            Jump to a name starting with it",
    "Tab / Shift+Tab     Next / previous pane",
    "Alt+Left/Right      Switch drive",
    "F5                  Refresh",
    "Ctrl+H              Show or hide hidden files",
    "Ctrl+L              Stack or side-by-side panes",
    "Ctrl+D              Show or hide the details pane",
    "Esc                 Close menu or dialog",
];

fn dialog(p: &mut Painter, app: &App, d: Dialog) {
    let t = p.t;
    let name = app.selected().map_or(String::new(), |e| e.name.clone());
    let (title, lines): (&str, Vec<String>) = match &d {
        Dialog::Confirm(Act::Delete) => ("Delete", vec![format!("Delete \"{name}\"?"), "A folder is deleted with everything inside it.".to_string(), String::new(), "[Y] Yes     [N] No".to_string()]),
        Dialog::Confirm(a) => (
            if *a == Act::Reboot { "Restart" } else { "Shut down" },
            vec![if *a == Act::Reboot { "Restart this computer now?" } else { "Shut down this computer now?" }.to_string(), String::new(), "[Y] Yes     [N] No".to_string()],
        ),
        Dialog::Prompt(kind, _) => (
            match kind { PromptKind::NewFile => "New file", PromptKind::NewFolder => "New folder", PromptKind::Rename(_) => "Rename" },
            vec![match kind { PromptKind::NewFile => "Name of the new file:", PromptKind::NewFolder => "Name of the new folder:", PromptKind::Rename(_) => "New name:" }.to_string(), String::new(), String::new(), "Enter OK     Esc Cancel".to_string(), "                                          ".to_string()],
        ),
        Dialog::Keys => ("Keyboard shortcuts", KEYS_HELP.iter().map(|s| s.to_string()).collect()),
        Dialog::About => ("About Exec85", vec![
            format!("Exec85 {}", env!("CARGO_PKG_VERSION")),
            "A small, fast, keyboard-driven Linux server OS.".into(), String::new(),
            "Early alpha: the shell works; installer,".into(), "login and apps are still to come.".into(),
        ]),
    };
    let inner = lines.iter().map(|l| l.chars().count()).max().unwrap_or(10).max(title.len() + 2);
    let w = (inner + 4).min(app.cols);
    let h = (lines.len() + 4).min(app.rows);
    let r = Rect { x: (app.cols - w) / 2, y: (app.rows - h) / 2, w, h };
    p.cells(r.x, r.y, r.w, r.h, t.panel);
    p.clip(r.x, r.y, r.w, &format!(" {title}"), t.title_on_text, t.title_on);
    p.outline(r, t.title_on);
    for (i, l) in lines.iter().enumerate() { p.clip(r.x + 2, r.y + 2 + i, r.w.saturating_sub(4), l, t.text, t.panel); }
    if let Dialog::Prompt(_, text) = &d { p.field(r.x + 2, r.y + 3, r.w.saturating_sub(4), text, true, false); }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gfx::Mode;
    use crate::keys::parse_key;
    use std::path::PathBuf;

    fn render(mode: Mode, keys: &str) -> (Canvas, Metrics, App) {
        let (w, h) = (1024, 768);
        let m = Metrics::for_height(h);
        let mut app = App::new(w / m.cw, h / m.ch, mode, PathBuf::from("/"));
        for k in keys.split(',').filter(|s| !s.is_empty()) { app.handle(parse_key(k).unwrap()); }
        let mut c = Canvas::new(w, h);
        draw(&app, &[], &mut c, &m);
        (c, m, app)
    }

    #[test]
    fn menu_bar_and_background_use_the_theme() {
        for mode in [Mode::Dark, Mode::Light] {
            let t = Theme::new(mode);
            let (c, m, _) = render(mode, "");
            assert_eq!(c.px[2 * c.w + c.w / 2], t.bar, "menu bar colour at the top centre");
            assert_eq!(c.px[(c.h - 1) * c.w + 3], t.bg, "leftover pixels below the last row are desktop colour");
            let _ = m;
        }
    }

    #[test]
    fn open_menu_and_dialog_draw_without_panicking_at_many_sizes() {
        for (w, h) in [(640, 400), (800, 600), (1024, 768), (1920, 1080), (3840, 2160), (200, 150)] {
            let m = Metrics::for_height(h);
            let (cols, rows) = (w / m.cw, h / m.ch);
            if cols < 20 || rows < 8 { continue; }
            let mut app = App::new(cols, rows, Mode::Dark, PathBuf::from("/"));
            for keys in ["", "f10", "alt-v,down", "f1", "ctrl-l", "tab,tab"] {
                for k in keys.split(',').filter(|s| !s.is_empty()) { app.handle(parse_key(k).unwrap()); }
                let mut c = Canvas::new(w, h);
                draw(&app, &[], &mut c, &m);
            }
        }
    }

    #[test]
    fn switching_theme_changes_every_surface() {
        let (dark, _, _) = render(Mode::Dark, "");
        let (light, _, a) = render(Mode::Dark, "alt-v,down,enter");
        assert_eq!(a.mode, Mode::Light);
        let differing = dark.px.iter().zip(&light.px).filter(|(a, b)| a != b).count();
        assert!(differing > dark.px.len() * 9 / 10, "almost every pixel changes with the theme");
    }

    #[test]
    fn tail_keeps_the_end_of_long_paths() {
        assert_eq!(tail("/usr/local/share", 20), "/usr/local/share");
        assert_eq!(tail("/usr/local/share/doc", 10), "...are/doc");
    }
}
