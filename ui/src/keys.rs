//! Keys and the keyboard layout table (Linux evdev key codes).
//! Only the US layout exists so far; the installer adds more in milestone 3.

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Key { pub code: u16, pub shift: bool, pub ctrl: bool, pub alt: bool }

pub mod code {
    pub const ESC: u16 = 1;
    pub const BACKSPACE: u16 = 14;
    pub const TAB: u16 = 15;
    pub const ENTER: u16 = 28;
    pub const SPACE: u16 = 57;
    pub const F1: u16 = 59;
    pub const F5: u16 = 63;
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

impl Key {
    #[allow(dead_code)]
    pub fn new(code: u16) -> Key { Key { code, ..Key::default() } }
    /// The character regardless of Ctrl/Alt (used for Alt+letter menu shortcuts).
    pub fn base_char(&self) -> Option<char> { us_char(self.code, self.shift) }
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
