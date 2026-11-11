//! Drawing into an in-memory pixel buffer, plus the colour themes.
//! Pixels are 0x00RRGGBB.
use crate::font;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode { Light, Dark }

/// One palette used by every widget, so light/dark applies to the whole interface.
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: u32,             // desktop behind everything
    pub panel: u32,          // window background
    pub text: u32,
    pub dim: u32,            // secondary text
    pub bar: u32,            // menu bar and status bar
    pub bar_text: u32,
    pub accent: u32,         // selection / open menu
    pub accent_text: u32,
    pub border: u32,
    pub title_on: u32,       // title bar of the active window
    pub title_on_text: u32,
    pub title_off: u32,
    pub title_off_text: u32,
}

impl Theme {
    pub fn new(mode: Mode) -> Theme {
        match mode {
            Mode::Dark => Theme {
                bg: 0x0B1D2A, panel: 0x102838, text: 0xE8E1CF, dim: 0x8FA3AE,
                bar: 0x1F3B4D, bar_text: 0xE8E1CF, accent: 0xE0A030, accent_text: 0x0B1D2A,
                border: 0x3F6178, title_on: 0x2F6D8F, title_on_text: 0xF4EEDD,
                title_off: 0x1F3B4D, title_off_text: 0xA9B8C0,
            },
            Mode::Light => Theme {
                bg: 0xF3EBD8, panel: 0xFBF7EA, text: 0x1B1B1B, dim: 0x6B6455,
                bar: 0xC9BFA3, bar_text: 0x1B1B1B, accent: 0x1F5F8B, accent_text: 0xFBF7EA,
                border: 0x8C8268, title_on: 0x1F5F8B, title_on_text: 0xFBF7EA,
                title_off: 0xD9CFB4, title_off_text: 0x4A4536,
            },
        }
    }
}

/// Size of one text cell in pixels (6 x 10 font pixels: glyph plus a 1-pixel margin all round, room for tails). The whole UI is laid out in cells, so
/// windows always line up with the text grid.
#[derive(Clone, Copy, Debug)]
pub struct Metrics { pub scale: usize, pub cw: usize, pub ch: usize }

impl Metrics {
    /// Pick a text size that gives a similar number of rows on any screen height.
    pub fn for_height(h: usize) -> Metrics {
        let scale = ((h + 200) / 400).max(2);
        Metrics { scale, cw: 6 * scale, ch: 10 * scale }
    }
}

pub struct Canvas { pub w: usize, pub h: usize, pub px: Vec<u32> }

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas { Canvas { w, h, px: vec![0; w * h] } }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: u32) {
        for yy in y..(y + h).min(self.h) {
            let row = yy * self.w;
            for xx in x..(x + w).min(self.w) { self.px[row + xx] = c; }
        }
    }

    /// Rectangle outline `t` pixels thick, drawn inside the given box.
    pub fn outline(&mut self, x: usize, y: usize, w: usize, h: usize, t: usize, c: u32) {
        if w == 0 || h == 0 { return; }
        let t = t.min(w).min(h);
        self.fill_rect(x, y, w, t, c);
        self.fill_rect(x, y + h - t, w, t, c);
        self.fill_rect(x, y, t, h, c);
        self.fill_rect(x + w - t, y, t, h, c);
    }

    /// Draw one character with its top-left at pixel (x, y).
    pub fn glyph(&mut self, x: usize, y: usize, ch: char, scale: usize, fg: u32) {
        for (ry, row) in font::glyph(ch).iter().enumerate() {
            for (rx, b) in row.bytes().enumerate() {
                if b == b'#' { self.fill_rect(x + rx * scale, y + ry * scale, scale, scale, fg); }
            }
        }
    }

    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px { out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]); }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn drawing_outside_the_canvas_is_clipped() {
        let mut c = Canvas::new(10, 10);
        c.fill_rect(5, 5, 100, 100, 1);
        c.glyph(8, 8, 'W', 3, 2);
        c.outline(7, 7, 50, 50, 2, 3);
    }
    #[test]
    fn metrics_give_readable_text_on_common_screens() {
        assert_eq!(Metrics::for_height(768).scale, 2);
        assert_eq!(Metrics::for_height(1080).scale, 3);
        assert!(Metrics::for_height(2160).scale >= 5);
        assert_eq!(Metrics::for_height(100).scale, 2);
    }
    #[test]
    fn themes_differ() {
        assert_ne!(Theme::new(Mode::Dark).bg, Theme::new(Mode::Light).bg);
    }
}
