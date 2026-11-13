//! The boot screen. It looks like the rest of Exec85: the menu-bar strip on top, a framed window in the middle with
//! the name in the big pixel font, and a status bar at the bottom, all in the colours of the chosen light or dark theme.
//! A block runs back and forth in the window's bar to show that the system is busy starting.
use crate::gfx::{Canvas, Metrics, Mode, Theme};
use crate::layout::Rect;
use crate::paint::Painter;

/// One frame. `t` is the time in seconds (it moves the block).
pub fn render(c: &mut Canvas, mode: Mode, t: f64, version: &str) {
    let m = Metrics::for_height(c.h);
    let t_ = Theme::new(mode);
    c.fill_rect(0, 0, c.w, c.h, t_.bg);
    let (cols, rows) = (c.w / m.cw, c.h / m.ch);
    if cols < 4 || rows < 4 { return; }
    let mut p = Painter { c, m, t: t_ };

    // top bar, like the shell and the login screen
    p.cells(0, 0, cols, 1, t_.bar);
    p.text(1, 0, "Exec85", t_.bar_text, None);

    // the window
    let (w, h) = (cols.min(46), 11usize.min(rows.saturating_sub(2)));
    let r = Rect { x: (cols - w) / 2, y: ((rows - h) / 2).max(1), w, h };
    let a = p.window(r, "Starting", true);
    if a.h >= 8 && a.w >= 20 {
        p.big_text(a.x + a.w.saturating_sub(18) / 2, a.y + 1, "Exec85", 3, t_.text);
        let msg = "Starting your server...";
        p.text(a.x + a.w.saturating_sub(msg.len()) / 2, a.y + 5, msg, t_.dim, None);
    }
    // the bar: a block that runs back and forth
    if a.h >= 3 && a.w >= 8 {
        let (bw, by) = (a.w.saturating_sub(4), a.y + a.h.saturating_sub(2));
        p.cells(a.x + 2, by, bw, 1, t_.bg);
        let seg = (bw / 4).max(2);
        let span = bw.saturating_sub(seg);
        let phase = (t * 0.5).fract() * 2.0;                       // 0..2: there and back
        let pos = (if phase < 1.0 { phase } else { 2.0 - phase } * span as f64).round() as usize;
        p.cells(a.x + 2 + pos.min(span), by, seg, 1, t_.accent);
    }

    // bottom bar
    let y = rows - 1;
    p.cells(0, y, cols, 1, t_.bar);
    let ver = format!("Exec85 {version}");
    p.right(1, y, cols.saturating_sub(2), &ver, t_.dim, t_.bar);   // this clears the row, so the left text goes on afterwards
    p.text(1, y, "Please wait", t_.bar_text, None);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, mode: Mode, t: f64) -> Canvas { let mut c = Canvas::new(w, h); render(&mut c, mode, t, "0.6.0"); c }

    #[test]
    fn it_uses_the_same_look_as_the_rest_of_the_system() {
        for mode in [Mode::Dark, Mode::Light] {
            let th = Theme::new(mode);
            let c = frame(1280, 800, mode, 1.0);
            assert_eq!(c.px[2 * c.w + c.w / 2], th.bar, "top bar in the theme's bar colour");
            assert_eq!(c.px[(c.h - 3) * c.w + c.w / 2], th.bar, "bottom bar too");
            assert_eq!(c.px[c.w * 2 + 3], th.bar);
            let bg = c.px.iter().filter(|p| **p == th.bg).count();
            let title = c.px.iter().filter(|p| **p == th.title_on).count();
            assert!(bg > c.px.len() / 3, "the desktop colour fills the screen ({bg})");
            assert!(title > 500, "the window has the active title bar colour ({title})");
            let text = c.px.iter().filter(|p| **p == th.text).count();
            assert!(text > 300, "the name is drawn in the text colour ({text})");
        }
        assert_ne!(frame(640, 400, Mode::Dark, 0.0).px, frame(640, 400, Mode::Light, 0.0).px, "light and dark differ");
    }

    #[test]
    fn only_the_block_moves() {
        let (a, b) = (frame(1024, 768, Mode::Dark, 0.0), frame(1024, 768, Mode::Dark, 0.5));
        let differing: Vec<usize> = a.px.iter().zip(&b.px).enumerate().filter(|(_, (x, y))| x != y).map(|(i, _)| i).collect();
        assert!(!differing.is_empty(), "the block moves between frames");
        assert!(differing.len() < a.px.len() / 20, "everything else stays put ({} pixels changed)", differing.len());
        let accent = Theme::new(Mode::Dark).accent;
        assert!(a.px.iter().any(|p| *p == accent) && b.px.iter().any(|p| *p == accent), "the block is in the accent colour");
        assert_eq!(frame(640, 400, Mode::Dark, 3.0).px, frame(640, 400, Mode::Dark, 3.0).px, "the same time gives the same picture");
    }

    #[test]
    fn the_block_stays_inside_its_bar_at_every_moment() {
        let th = Theme::new(Mode::Dark);
        let rows_with_accent = |c: &Canvas| (0..c.h).filter(|y| (0..c.w).any(|x| c.px[y * c.w + x] == th.accent)).count();
        for i in 0..40 {
            let c = frame(1280, 800, Mode::Dark, i as f64 * 0.137);
            let m = Metrics::for_height(800);
            assert!(rows_with_accent(&c) <= m.ch, "the block fits in one text row (frame {i})");
        }
    }

    #[test]
    fn any_screen_size_works() {
        for (w, h) in [(160, 100), (320, 200), (640, 480), (1024, 768), (1366, 768), (1920, 1080), (3840, 2160)] {
            for mode in [Mode::Dark, Mode::Light] { assert_eq!(frame(w, h, mode, 0.4).px.len(), w * h); }
        }
        let mut tiny = Canvas::new(1, 1); render(&mut tiny, Mode::Dark, 0.0, "x");    // absurd sizes do not crash
        let mut zero = Canvas::new(0, 0); render(&mut zero, Mode::Light, 0.0, "x");
        let mut thin = Canvas::new(2000, 6); render(&mut thin, Mode::Dark, 0.0, "x");
    }
}
