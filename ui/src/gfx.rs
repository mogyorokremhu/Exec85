//! Drawing into an in-memory pixel buffer, plus the colour themes.
//! Pixels are 0x00RRGGBB.

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode { Light, Dark }

/// One palette used by every widget, so light/dark applies to the whole UI.
#[derive(Clone, Copy)]
pub struct Theme { pub bg: u32, pub bar: u32, pub text: u32 }

impl Theme {
    pub fn new(mode: Mode) -> Theme {
        match mode {
            Mode::Dark => Theme { bg: 0x0B1D2A, bar: 0x1F3B4D, text: 0xE8E1CF },
            Mode::Light => Theme { bg: 0xF3EBD8, bar: 0xC9BFA3, text: 0x1B1B1B },
        }
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

    /// Draw text with the tiny built-in 5x7 font, each font pixel `scale` px square.
    pub fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: u32) {
        let mut cx = x;
        for ch in s.chars() {
            if let Some(g) = glyph(ch) {
                for (ry, bits) in g.iter().enumerate() {
                    for rx in 0..5 {
                        if bits & (0x10 >> rx) != 0 {
                            self.fill_rect(cx + rx * scale, y + ry * scale, scale, scale, c);
                        }
                    }
                }
            }
            cx += 6 * scale;
        }
    }

    /// Milestone 1 screen: an empty themed desktop with a title bar. The menu
    /// bar, drive row and file list arrive in milestone 2.
    pub fn draw_blank(&mut self, t: &Theme) {
        self.fill_rect(0, 0, self.w, self.h, t.bg);
        self.fill_rect(0, 0, self.w, 28, t.bar);
        self.text(10, 6, "Exec85", 3, t.text);
    }

    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for p in &self.px { out.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]); }
        out
    }
}

/// 5x7 glyphs; only the characters we need so far.
fn glyph(c: char) -> Option<[u8; 7]> {
    Some(match c {
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'x' => [0, 0, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001],
        'e' => [0, 0, 0b01110, 0b10001, 0b11111, 0b10000, 0b01110],
        'c' => [0, 0, 0b01110, 0b10000, 0b10000, 0b10001, 0b01110],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blank_screen_uses_theme_colours() {
        for m in [Mode::Dark, Mode::Light] {
            let t = Theme::new(m);
            let mut c = Canvas::new(320, 200);
            c.draw_blank(&t);
            assert_eq!(c.px[100 * 320 + 160], t.bg, "centre is background");
            assert_eq!(c.px[1 * 320 + 300], t.bar, "top strip is the bar");
            assert!(c.px.iter().any(|&p| p == t.text), "title text drawn");
        }
    }
    #[test]
    fn drawing_outside_the_canvas_is_clipped() {
        let mut c = Canvas::new(10, 10);
        c.fill_rect(5, 5, 100, 100, 1);
        c.text(8, 8, "Exec85", 3, 2);
    }
}
