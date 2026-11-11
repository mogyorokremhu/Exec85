//! The login screen, shown on every start of an installed system.
use crate::account::{AuthError, User};
use crate::gfx::{Canvas, Metrics, Mode, Theme};
use crate::keys::{char_for, code, Key, Layout};
use crate::layout::Rect;
use crate::paint::Painter;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Power { Restart, ShutDown }

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome { LoggedIn(User), Restart, ShutDown }

pub struct Login {
    pub cols: usize,
    pub rows: usize,
    pub mode: Mode,
    pub layout: Layout,
    pub hostname: String,
    pub user: String,
    pub pass: String,
    /// 0 = user name, 1 = password
    pub field: usize,
    pub failures: u32,
    pub msg: Option<String>,
    pub confirm: Option<Power>,
    /// After a wrong password the caller should wait this long before accepting more input.
    pub penalty: Option<std::time::Duration>,
}

fn clean(s: &str) -> String { if s.is_empty() { "-".into() } else { s.replace(' ', "_") } }

impl Login {
    /// `default_user` is filled in for convenience (normally the only regular user on the machine).
    pub fn new(cols: usize, rows: usize, mode: Mode, layout: Layout, hostname: String, default_user: Option<String>) -> Login {
        let field = if default_user.is_some() { 1 } else { 0 };
        Login { cols, rows, mode, layout, hostname, user: default_user.unwrap_or_default(), pass: String::new(), field, failures: 0, msg: None, confirm: None, penalty: None }
    }

    pub fn handle(&mut self, k: Key, auth: &dyn Fn(&str, &str) -> Result<User, AuthError>) -> Option<Outcome> {
        self.penalty = None;
        let typed = if k.ctrl || k.alt { None } else { char_for(self.layout, k.code, k.shift, k.altgr) };

        if let Some(p) = self.confirm {
            let c = typed.map(|c| c.to_ascii_lowercase());
            if k.code == code::ENTER || c == Some('y') {
                self.confirm = None;
                return Some(if p == Power::Restart { Outcome::Restart } else { Outcome::ShutDown });
            }
            if k.code == code::ESC || c == Some('n') { self.confirm = None; }
            return None;
        }

        self.msg = None;
        match k.code {
            0x3c => { self.confirm = Some(Power::Restart); }   // F2
            0x3d => { self.confirm = Some(Power::ShutDown); }  // F3
            code::TAB | code::DOWN => self.field = if k.shift { 0 } else { 1 },
            code::UP => self.field = 0,
            code::ESC => { self.pass.clear(); }
            code::BACKSPACE => { (if self.field == 0 { &mut self.user } else { &mut self.pass }).pop(); }
            code::ENTER => {
                if self.field == 0 { self.field = 1; return None; }
                match auth(&self.user, &self.pass) {
                    Ok(u) => { self.pass.clear(); return Some(Outcome::LoggedIn(u)); }
                    Err(_) => {
                        // Same message for "no such user", "locked" and "wrong password": do not reveal which.
                        self.failures += 1;
                        self.msg = Some("Login incorrect".into());
                        self.pass.clear();
                        self.field = 1;
                        self.penalty = Some(std::time::Duration::from_millis(1000));
                    }
                }
            }
            _ => if let Some(c) = typed {
                if self.field == 0 {
                    let c = c.to_ascii_lowercase();
                    if (c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') && self.user.len() < 32 { self.user.push(c); }
                } else if self.pass.chars().count() < 128 { self.pass.push(c); }
            },
        }
        None
    }

    pub fn state_line(&self) -> String {
        format!(
            "EXEC85_LOGIN host={} user={} field={} pw_len={} failures={} msg={} layout={} power={} theme={}",
            clean(&self.hostname), clean(&self.user), if self.field == 0 { "user" } else { "password" }, self.pass.chars().count(),
            self.failures, self.msg.as_deref().map_or("none".to_string(), clean), self.layout.id(),
            match self.confirm { None => "none", Some(Power::Restart) => "restart", Some(Power::ShutDown) => "shutdown" },
            if self.mode == Mode::Dark { "dark" } else { "light" },
        )
    }
}

pub fn draw(l: &Login, c: &mut Canvas, m: &Metrics) {
    let t = Theme::new(l.mode);
    c.fill_rect(0, 0, c.w, c.h, t.bg);
    let mut p = Painter { c, m: *m, t };
    p.cells(0, 0, l.cols, 1, t.bar);
    p.right(1, 0, l.cols.saturating_sub(2), &l.hostname, t.dim, t.bar);
    p.text(1, 0, "Exec85", t.bar_text, None);

    let (w, h) = (l.cols.min(46), 13usize.min(l.rows.saturating_sub(2)));
    let r = Rect { x: (l.cols - w) / 2, y: ((l.rows - h) / 2).max(1), w, h };
    let a = p.window(r, "Log in", true);
    p.text(a.x, a.y, "User name", t.dim, None);
    p.field(a.x, a.y + 1, a.w, &l.user, l.field == 0, false);
    p.text(a.x, a.y + 4, "Password", t.dim, None);
    p.field(a.x, a.y + 5, a.w, &l.pass, l.field == 1, true);
    if let Some(msg) = &l.msg { p.text(a.x, a.y + 8, msg, t.accent, None); }

    let y = l.rows - 1;
    p.cells(0, y, l.cols, 1, t.bar);
    p.clip(1, y, l.cols.saturating_sub(2), "Enter Log in   Tab Next box   F2 Restart   F3 Shut down", t.bar_text, t.bar);

    if let Some(pw) = l.confirm {
        let text = if pw == Power::Restart { "Restart this computer now?" } else { "Shut down this computer now?" };
        let w = 34.min(l.cols);
        let r = Rect { x: (l.cols - w) / 2, y: l.rows / 2 - 2, w, h: 5 };
        p.cells(r.x, r.y, r.w, r.h, t.panel);
        p.clip(r.x, r.y, r.w, if pw == Power::Restart { " Restart" } else { " Shut down" }, t.title_on_text, t.title_on);
        p.outline(r, t.title_on);
        p.clip(r.x + 2, r.y + 2, r.w - 4, text, t.text, t.panel);
        p.clip(r.x + 2, r.y + 3, r.w - 4, "[Y] Yes     [N] No", t.dim, t.panel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn user() -> User { User { name: "tester".into(), uid: 1000, gid: 1000, home: "/home/tester".into(), shell: "/bin/ash".into() } }

    fn auth(name: &str, pw: &str) -> Result<User, AuthError> {
        match (name, pw) { ("tester", "secret7-") => Ok(user()), ("tester", _) => Err(AuthError::BadPassword), ("locked", _) => Err(AuthError::Locked), _ => Err(AuthError::NoSuchUser) }
    }

    fn press(l: &mut Login, keys: &str) -> Vec<Outcome> {
        keys.split(',').filter(|s| !s.is_empty()).filter_map(|n| l.handle(parse_key(n).unwrap_or_else(|| panic!("bad key {n}")), &auth)).collect()
    }

    fn type_text(l: &mut Login, s: &str) -> Vec<Outcome> {
        s.chars().filter_map(|c| {
            let k = parse_key(&c.to_string()).unwrap();
            l.handle(k, &auth)
        }).collect()
    }

    fn login(default: Option<&str>) -> Login { Login::new(100, 40, Mode::Dark, Layout::Us, "lab85".into(), default.map(String::from)) }

    #[test]
    fn single_user_is_prefilled_and_password_focused() {
        let l = login(Some("tester"));
        assert_eq!((l.user.as_str(), l.field), ("tester", 1));
        let l = login(None);
        assert_eq!((l.user.as_str(), l.field), ("", 0));
    }

    #[test]
    fn wrong_password_is_refused_with_a_delay_and_the_right_one_logs_in() {
        let mut l = login(Some("tester"));
        type_text(&mut l, "wrong");
        assert_eq!(l.pass, "wrong");
        assert!(press(&mut l, "enter").is_empty());
        assert_eq!((l.failures, l.msg.as_deref(), l.pass.as_str()), (1, Some("Login incorrect"), ""));
        assert!(l.penalty.is_some(), "a pause slows down password guessing");
        type_text(&mut l, "secret7-");
        assert_eq!(press(&mut l, "enter"), vec![Outcome::LoggedIn(user())]);
        assert_eq!(l.pass, "", "the password is not kept in memory after login");
    }

    #[test]
    fn typing_a_user_name_then_enter_moves_to_the_password() {
        let mut l = login(None);
        type_text(&mut l, "Tester");
        assert_eq!(l.user, "tester");
        press(&mut l, "enter");
        assert_eq!(l.field, 1);
        type_text(&mut l, "secret7-");
        assert_eq!(press(&mut l, "enter").len(), 1);
    }

    #[test]
    fn unknown_and_locked_accounts_look_the_same_as_a_wrong_password() {
        for name in ["nobody-here", "locked", "tester"] {
            let mut l = login(None);
            type_text(&mut l, name);
            press(&mut l, "enter");
            type_text(&mut l, "x");
            assert!(press(&mut l, "enter").is_empty());
            assert_eq!(l.msg.as_deref(), Some("Login incorrect"), "{name}");
        }
    }

    #[test]
    fn power_keys_ask_first() {
        let mut l = login(Some("tester"));
        assert!(press(&mut l, "f2").is_empty());
        assert_eq!(l.confirm, Some(Power::Restart));
        assert!(press(&mut l, "x").is_empty());
        assert!(press(&mut l, "n").is_empty());
        assert_eq!(l.confirm, None);
        press(&mut l, "f3");
        assert_eq!(press(&mut l, "y"), vec![Outcome::ShutDown]);
        press(&mut l, "f2");
        assert_eq!(press(&mut l, "enter"), vec![Outcome::Restart]);
    }

    #[test]
    fn password_uses_the_installed_layout() {
        let mut l = Login::new(100, 40, Mode::Dark, Layout::Hu, "x".into(), Some("tester".into()));
        for code in [31u16, 18, 46, 19, 18, 20, 8, 53] { l.handle(Key::new(code), &auth); }
        assert_eq!(l.pass, "secret7-");
        assert_eq!(l.handle(Key::new(28), &auth), Some(Outcome::LoggedIn(user())));
    }

    #[test]
    fn draws_at_several_sizes() {
        for (w, h) in [(640, 400), (1024, 768), (1920, 1080)] {
            let m = Metrics::for_height(h);
            let mut l = login(Some("tester"));
            l.cols = w / m.cw; l.rows = h / m.ch;
            for keys in ["", "x", "f2"] { press(&mut l, keys); let mut c = Canvas::new(w, h); draw(&l, &mut c, &m); }
        }
    }

    #[test]
    fn state_line_never_contains_the_password() {
        let mut l = login(Some("tester"));
        type_text(&mut l, "hunter2");
        let s = l.state_line();
        assert!(s.contains("pw_len=7") && !s.contains("hunter2"));
    }
}
