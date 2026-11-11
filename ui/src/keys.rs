//! Keys and keyboard layouts (Linux evdev key codes -> characters).
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Key {
    pub code: u16,
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    /// Right Alt ("AltGr"): reaches the third level of European layouts (@ { } [ ] \ ...).
    pub altgr: bool,
}

pub mod code {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    pub const ENTER: u16 = 28;
    pub const SPACE: u16 = 57;
    pub const F1: u16 = 59;
    pub const F2: u16 = 60;
    pub const F4: u16 = 62;
    pub const F5: u16 = 63;
    pub const F6: u16 = 64;
    pub const F7: u16 = 65;
    pub const F10: u16 = 68;
    pub const HOME: u16 = 102;
    pub const UP: u16 = 103;
    pub const PGUP: u16 = 104;
    pub const LEFT: u16 = 105;
    pub const RIGHT: u16 = 106;
    pub const END: u16 = 107;
    pub const DOWN: u16 = 108;
    pub const PGDN: u16 = 109;
    pub const DELETE: u16 = 111;
}

const NUM_N: &str = "1234567890-=";
const NUM_S: &str = "!@#$%^&*()_+";
const QWE_N: &str = "qwertyuiop[]";
const QWE_S: &str = "QWERTYUIOP{}";
const ASD_N: &str = "asdfghjkl;'`";
const ASD_S: &str = "ASDFGHJKL:\"~";
const ZXC_N: &str = "zxcvbnm,./";
const ZXC_S: &str = "ZXCVBNM<>?";

fn pick(n: &str, s: &str, first: u16, code: u16, shift: bool) -> Option<char> {
    (if shift { s } else { n }).chars().nth((code - first) as usize)
}

/// The character a key produces on a US keyboard, if it is a printing key.
pub fn us_char(code: u16, shift: bool) -> Option<char> {
    match code {
        2..=13 => pick(NUM_N, NUM_S, 2, code, shift),
        16..=27 => pick(QWE_N, QWE_S, 16, code, shift),
        30..=41 => pick(ASD_N, ASD_S, 30, code, shift),
        43 => Some(if shift { '|' } else { '\\' }),
        44..=53 => pick(ZXC_N, ZXC_S, 44, code, shift),
        57 => Some(' '),
        _ => None,
    }
}

// ---- layouts ---------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout { Us, Uk, De, Hu }

impl Layout {
    pub const ALL: [Layout; 4] = [Layout::Us, Layout::Uk, Layout::De, Layout::Hu];
    pub fn id(self) -> &'static str { match self { Layout::Us => "us", Layout::Uk => "uk", Layout::De => "de", Layout::Hu => "hu" } }
    pub fn label(self) -> &'static str {
        match self {
            Layout::Us => "English (US)", Layout::Uk => "English (UK)", Layout::De => "German", Layout::Hu => "Hungarian",
        }
    }
    pub fn from_id(s: &str) -> Option<Layout> { Layout::ALL.into_iter().find(|l| l.id() == s.trim()) }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

/// The layout used for typing, set at start-up (from /etc/exec85/keyboard) or by the installer.
pub fn set_layout(l: Layout) { CURRENT.store(Layout::ALL.iter().position(|x| *x == l).unwrap_or(0) as u8, Ordering::SeqCst); }
pub fn layout() -> Layout { Layout::ALL[(CURRENT.load(Ordering::SeqCst) as usize).min(3)] }

/// `\0` in a table means "no character on this key".
fn pick3(normal: &str, shifted: &str, altgr: &str, first: u16, code: u16, shift: bool, ag: bool) -> Option<char> {
    let i = code.checked_sub(first)? as usize;
    let table = if ag { altgr } else if shift { shifted } else { normal };
    table.chars().nth(i).filter(|c| *c != '\0')
}

/// What `code` types on `layout` with the given modifiers. (Dead keys type their plain symbol.)
pub fn char_for(layout: Layout, code: u16, shift: bool, altgr: bool) -> Option<char> {
    if code == 57 { return Some(' '); }
    match layout {
        Layout::Us => us_char(code, shift),
        Layout::Uk => match (code, shift) {
            (3, true) => Some('"'), (4, true) => Some('\u{a3}'),
            (40, false) => Some('\''), (40, true) => Some('@'),
            (41, false) => Some('`'), (41, true) => Some('\u{ac}'),
            (43, false) => Some('#'), (43, true) => Some('~'),
            (86, false) => Some('\\'), (86, true) => Some('|'),
            _ => us_char(code, shift),
        },
        Layout::De => match code {
            2..=13 => pick3("1234567890\u{df}\u{b4}", "!\"\u{a7}$%&/()=?`", "\0\0\0\0\0\0{[]}\\\0", 2, code, shift, altgr),
            16..=27 => pick3("qwertzuiop\u{fc}+", "QWERTZUIOP\u{dc}*", "@\0\u{20ac}\0\0\0\0\0\0\0\0~", 16, code, shift, altgr),
            30..=40 => pick3("asdfghjkl\u{f6}\u{e4}", "ASDFGHJKL\u{d6}\u{c4}", "", 30, code, shift, altgr),
            41 => Some(if shift { '\u{b0}' } else { '^' }),
            43 => Some(if shift { '\'' } else { '#' }),
            44..=53 => pick3("yxcvbnm,.-", "YXCVBNM;:_", "", 44, code, shift, altgr),
            86 => Some(if altgr { '|' } else if shift { '>' } else { '<' }),
            _ => None,
        },
        Layout::Hu => match code {
            2..=13 => pick3("123456789\u{f6}\u{fc}\u{f3}", "'\"+!%/=()\u{d6}\u{dc}\u{d3}", "~\0^\0\u{b0}\0`\0\0\0\0\0", 2, code, shift, altgr),
            16..=27 => pick3("qwertzuiop\u{151}\u{fa}", "QWERTZUIOP\u{150}\u{da}", "\\|\0\0\0\0\u{20ac}\0\0\0\u{f7}\u{d7}", 16, code, shift, altgr),
            30..=40 => pick3("asdfghjkl\u{e9}\u{e1}", "ASDFGHJKL\u{c9}\u{c1}", "\0\0\0[]\0\0\0\0$\u{df}", 30, code, shift, altgr),
            41 => Some(if shift { '\u{a7}' } else { '0' }),
            43 => Some(if altgr { '\u{a4}' } else if shift { '\u{170}' } else { '\u{171}' }),
            44..=53 => pick3("yxcvbnm,.-", "YXCVBNM?:_", ">#&@{}<;>*", 44, code, shift, altgr),
            86 => Some(if altgr { '<' } else if shift { '\u{cd}' } else { '\u{ed}' }),
            _ => None,
        },
    }
}

impl Key {
    #[allow(dead_code)]
    pub fn new(code: u16) -> Key { Key { code, ..Key::default() } }
    /// The character regardless of Ctrl/Alt (used for Alt+letter menu shortcuts), on the current layout.
    pub fn base_char(&self) -> Option<char> { char_for(layout(), self.code, self.shift, self.altgr) }
    /// The character typed, or None when Ctrl or Alt is held.
    pub fn ch(&self) -> Option<char> { if self.ctrl || self.alt { None } else { self.base_char() } }
}

/// Parse names like "down", "ctrl-h", "alt-v", "f10", "e" (used by test tools).
pub fn parse_key(s: &str) -> Option<Key> {
    let mut k = Key::default();
    let mut rest = s;
    loop {
        if let Some(r) = rest.strip_prefix("ctrl-") { k.ctrl = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("alt-") { k.alt = true; rest = r; }
        else if let Some(r) = rest.strip_prefix("shift-") { k.shift = true; rest = r; }
        else { break; }
    }
    let named = match rest {
        "esc" => Some(code::ESC), "backspace" => Some(code::BACKSPACE), "tab" => Some(code::TAB),
        "enter" | "ret" => Some(code::ENTER), "space" => Some(code::SPACE),
        "up" => Some(code::UP), "down" => Some(code::DOWN), "left" => Some(code::LEFT), "right" => Some(code::RIGHT),
        "home" => Some(code::HOME), "end" => Some(code::END), "pgup" => Some(code::PGUP), "pgdn" => Some(code::PGDN),
        "delete" => Some(code::DELETE),
        f if f.starts_with('f') && f.len() > 1 => match f[1..].parse::<u16>() {
            Ok(n @ 1..=10) => Some(58 + n),
            Ok(11) => Some(87),
            Ok(12) => Some(88),
            _ => None,
        },
        _ => None,
    };
    if let Some(c) = named { k.code = c; return Some(k); }
    let mut it = rest.chars();
    let c = it.next()?;
    if it.next().is_some() { return None; }
    for code in 2u16..=57 {
        for shift in [false, true] {
            if us_char(code, shift) == Some(c) { k.code = code; k.shift |= shift; return Some(k); }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn us_layout_basics() {
        assert_eq!(us_char(30, false), Some('a'));
        assert_eq!(us_char(30, true), Some('A'));
        assert_eq!(us_char(2, true), Some('!'));
        assert_eq!(us_char(53, false), Some('/'));
        assert_eq!(us_char(57, false), Some(' '));
        assert_eq!(us_char(code::ENTER, false), None);
        assert_eq!(us_char(18, false), Some('e'));
        assert_eq!(us_char(35, false), Some('h'));
    }
    #[test]
    fn european_layouts() {
        use super::Layout::*;
        // Germany and Hungary swap Y and Z
        assert_eq!(char_for(De, 21, false, false), Some('z'));
        assert_eq!(char_for(De, 44, true, false), Some('Y'));
        assert_eq!(char_for(Hu, 21, false, false), Some('z'));
        assert_eq!(char_for(Hu, 44, false, false), Some('y'));
        assert_eq!(char_for(Us, 21, false, false), Some('y'));
        // letters that do not move are the same everywhere
        for l in Layout::ALL { assert_eq!(char_for(l, 18, false, false), Some('e')); assert_eq!(char_for(l, 31, true, false), Some('S')); }
        // Hungarian: digits, accented keys and the minus key
        assert_eq!(char_for(Hu, 41, false, false), Some('0'));
        assert_eq!(char_for(Hu, 11, false, false), Some('\u{f6}'));
        assert_eq!(char_for(Hu, 53, false, false), Some('-'));
        assert_eq!(char_for(Hu, 5, true, false), Some('!'));
        assert_eq!(char_for(Hu, 47, false, true), Some('@'));
        assert_eq!(char_for(Hu, 16, false, true), Some('\\'));
        assert_eq!(char_for(Hu, 48, false, true), Some('{'));
        // German
        assert_eq!(char_for(De, 16, false, true), Some('@'));
        assert_eq!(char_for(De, 8, false, true), Some('{'));
        assert_eq!(char_for(De, 12, false, true), Some('\\'));
        assert_eq!(char_for(De, 3, true, false), Some('"'));
        // UK
        assert_eq!(char_for(Uk, 4, true, false), Some('\u{a3}'));
        assert_eq!(char_for(Uk, 40, true, false), Some('@'));
        assert_eq!(char_for(Uk, 30, false, false), Some('a'));
    }

    #[test]
    fn every_printable_ascii_character_can_be_typed_on_every_layout() {
        // so no password made of ASCII characters is impossible to enter on any supported layout
        for l in Layout::ALL {
            let mut reachable = std::collections::HashSet::new();
            for code in 1u16..=90 {
                for shift in [false, true] { for altgr in [false, true] {
                    if let Some(c) = char_for(l, code, shift, altgr) { reachable.insert(c); }
                }}
            }
            for c in (32u8..=126).map(|b| b as char) {
                assert!(reachable.contains(&c), "{:?} cannot type {c:?}", l);
            }
        }
    }

    #[test]
    fn layout_ids_round_trip() {
        for l in Layout::ALL { assert_eq!(Layout::from_id(l.id()), Some(l)); }
        assert_eq!(Layout::from_id("hu\n"), Some(Layout::Hu));
        assert_eq!(Layout::from_id("xx"), None);
    }

    #[test]
    fn ctrl_and_alt_do_not_type() {
        let k = Key { code: 30, ctrl: true, ..Key::default() };
        assert_eq!(k.ch(), None);
        assert_eq!(k.base_char(), Some('a'));
    }
    #[test]
    fn parse_names() {
        assert_eq!(parse_key("down"), Some(Key::new(code::DOWN)));
        assert_eq!(parse_key("f10"), Some(Key::new(code::F10)));
        assert_eq!(parse_key("f1"), Some(Key::new(code::F1)));
        assert_eq!(parse_key("f12"), Some(Key::new(88)));
        assert_eq!(parse_key("ctrl-h").map(|k| (k.ctrl, k.code)), Some((true, 35)));
        assert_eq!(parse_key("alt-v").map(|k| (k.alt, k.code)), Some((true, 47)));
        assert_eq!(parse_key("E").map(|k| (k.shift, k.code)), Some((true, 18)));
        assert_eq!(parse_key("nonsense"), None);
    }
}
