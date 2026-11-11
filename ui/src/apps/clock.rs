//! Clock and calendar: the time in the computer's time zone and a month calendar you can page through.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use crate::sys::{self, Local};
use std::time::Duration;

const MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];

/// Days since 1970-01-01 for a calendar date (Howard Hinnant's algorithm).
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y as i64 - 1 } else { y as i64 };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// 0 = Monday ... 6 = Sunday.
pub fn weekday(y: i32, m: u32, d: u32) -> usize { ((days_from_civil(y, m, d) + 3).rem_euclid(7)) as usize }

pub fn days_in_month(y: i32, m: u32) -> u32 {
    match m { 1 | 3 | 5 | 7 | 8 | 10 | 12 => 31, 4 | 6 | 9 | 11 => 30, _ => if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 { 29 } else { 28 } }
}

pub struct Clock {
    now: fn() -> Local,
    view_year: i32,
    view_month: u32,
    tz: String,
    shown: Local,
}

impl Clock {
    pub fn new() -> Clock { Clock::with_source(|| sys::local_time(sys::now())) }

    pub fn with_source(now: fn() -> Local) -> Clock {
        let n = now();
        let tz = std::fs::read_to_string("/etc/timezone").map(|s| s.trim().to_string()).unwrap_or_else(|_| "UTC".into());
        Clock { now, view_year: n.year, view_month: n.month, tz: if tz.is_empty() { "UTC".into() } else { tz }, shown: n }
    }

    fn shift_month(&mut self, delta: i32) {
        let idx = self.view_year * 12 + (self.view_month as i32 - 1) + delta;
        self.view_year = idx.div_euclid(12);
        self.view_month = (idx.rem_euclid(12) + 1) as u32;
    }
}

impl Application for Clock {
    fn kind(&self) -> Kind { Kind::Clock }
    fn title(&self) -> String { "Clock".into() }
    fn interval(&self) -> Option<Duration> { Some(Duration::from_secs(1)) }
    fn tick(&mut self, _env: &Env) { self.shown = (self.now)(); }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match k.code {
            code::LEFT | code::PGUP => self.shift_month(-1),
            code::RIGHT | code::PGDN => self.shift_month(1),
            code::UP => self.shift_month(-12),
            code::DOWN => self.shift_month(12),
            code::HOME => { let n = (self.now)(); self.view_year = n.year; self.view_month = n.month; }
            _ => {}
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let n = self.shown;
        if a.h < 3 || a.w < 8 { return; }
        let time = format!("{:02}:{:02}:{:02}", n.hour, n.min, n.sec);
        let mut y = a.y;
        if a.w >= 18 && a.h >= 14 { p.big_text(a.x, y, &time, 2, t.text); y += 3; } else { p.text(a.x, y, &time, t.text, None); y += 1; }
        let wd = weekday(n.year, n.month, n.day);
        p.clip(a.x, y, a.w, &format!("{}, {} {} {}", DAYS[wd], n.day, MONTHS[n.month as usize - 1], n.year), t.text, t.panel);
        p.clip(a.x, y + 1, a.w, &format!("Time zone: {}", self.tz), t.dim, t.panel);
        y += 3;
        if a.h < y - a.y + 9 || a.w < 20 { return; }
        p.clip(a.x, y, a.w, &format!("{} {}", MONTHS[self.view_month as usize - 1], self.view_year), t.accent, t.panel);
        y += 1;
        p.text(a.x, y, "Mo Tu We Th Fr Sa Su", t.dim, None);
        let first = weekday(self.view_year, self.view_month, 1);
        for d in 1..=days_in_month(self.view_year, self.view_month) {
            let idx = first + d as usize - 1;
            let (row, col) = (idx / 7, idx % 7);
            let today = self.view_year == n.year && self.view_month == n.month && d == n.day;
            let s = format!("{d:>2}");
            if today { p.text(a.x + col * 3, y + 1 + row, &s, t.accent_text, Some(t.accent)); } else { p.text(a.x + col * 3, y + 1 + row, &s, if col >= 5 { t.dim } else { t.text }, None); }
        }
    }

    fn state(&self) -> String {
        let n = self.shown;
        format!("name=clock date={:04}-{:02}-{:02} time={:02}:{:02}:{:02} view={:04}-{:02} weekday={} tz={}",
            n.year, n.month, n.day, n.hour, n.min, n.sec, self.view_year, self.view_month,
            &DAYS[weekday(n.year, n.month, n.day)][..3].to_lowercase(), self.tz.replace(' ', "_"))
    }
    fn hint(&self) -> &'static str { "Left/Right month   Up/Down year   Home today   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn fixed() -> Local { Local { year: 2026, month: 11, day: 11, hour: 17, min: 49, sec: 3 } }
    fn press(c: &mut Clock, keys: &str) { for k in keys.split(',') { c.handle(parse_key(k).unwrap(), &Env::for_tests()); } }

    #[test]
    fn weekdays_are_right() {
        assert_eq!(weekday(2026, 11, 11), 2, "Wednesday");
        assert_eq!(weekday(1970, 1, 1), 3, "Thursday");
        assert_eq!(weekday(2000, 1, 1), 5, "Saturday");
        assert_eq!(weekday(2024, 2, 29), 3, "leap day was a Thursday");
        assert_eq!(weekday(2026, 12, 25), 4, "Friday");
        assert_eq!(weekday(1999, 12, 31), 4);
    }

    #[test]
    fn month_lengths_and_leap_years() {
        assert_eq!(days_in_month(2026, 2), 28);
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(1900, 2), 28, "1900 was not a leap year");
        assert_eq!(days_in_month(2000, 2), 29);
        assert_eq!(days_in_month(2026, 4), 30);
        assert_eq!(days_in_month(2026, 12), 31);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2023, 11, 14), 19_675);
    }

    #[test]
    fn paging_through_months_and_years() {
        let mut c = Clock::with_source(fixed);
        assert!(c.state().contains("date=2026-11-11 time=17:49:03 view=2026-11 weekday=wed"));
        press(&mut c, "right");
        assert!(c.state().contains("view=2026-12"));
        press(&mut c, "right");
        assert!(c.state().contains("view=2027-01"), "December rolls into the next year");
        press(&mut c, "left,left,left");
        assert!(c.state().contains("view=2026-10"));
        press(&mut c, "down");
        assert!(c.state().contains("view=2027-10"));
        press(&mut c, "up,up");
        assert!(c.state().contains("view=2025-10"));
        press(&mut c, "home");
        assert!(c.state().contains("view=2026-11"), "Home returns to this month");
        press(&mut c, "pgup,pgup,pgup,pgup,pgup,pgup,pgup,pgup,pgup,pgup,pgup,pgup");
        assert!(c.state().contains("view=2025-11"));
    }

    #[test]
    fn ticking_updates_the_displayed_time() {
        fn later() -> Local { Local { sec: 4, ..fixed() } }
        let mut c = Clock::with_source(fixed);
        c.now = later;
        assert!(c.state().contains("time=17:49:03"));
        c.tick(&Env::for_tests());
        assert!(c.state().contains("time=17:49:04"));
        assert_eq!(c.interval(), Some(Duration::from_secs(1)));
    }

    #[test]
    fn draws_at_small_and_large_sizes() {
        use crate::gfx::{Canvas, Metrics, Theme, Mode};
        for (cols, rows) in [(10usize, 4usize), (22, 12), (40, 30)] {
            let m = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut canvas, m, t: Theme::new(Mode::Dark) };
            Clock::with_source(fixed).draw(&mut p, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
    }
}
