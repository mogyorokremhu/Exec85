//! Drawing helpers shared by the shell, the installer and the login screen.
use crate::gfx::{Canvas, Metrics, Theme};
use crate::layout::Rect;

pub struct Painter<'a> { pub c: &'a mut Canvas, pub m: Metrics, pub t: Theme }

impl Painter<'_> {
    pub fn cells(&mut self, col: usize, row: usize, w: usize, h: usize, color: u32) {
        self.c.fill_rect(col * self.m.cw, row * self.m.ch, w * self.m.cw, h * self.m.ch, color);
    }

    /// Text starting at a cell; `bg` fills each character's cell first.
    pub fn text(&mut self, col: usize, row: usize, s: &str, fg: u32, bg: Option<u32>) {
        for (i, ch) in s.chars().enumerate() {
            let (x, y) = ((col + i) * self.m.cw, row * self.m.ch);
            if let Some(b) = bg { self.c.fill_rect(x, y, self.m.cw, self.m.ch, b); }
            // one font pixel of margin above the glyph
            if ch != ' ' { self.c.glyph(x, y + self.m.scale, ch, self.m.scale, fg); }
        }
    }

    /// Text cut to `w` cells and padded with the background colour.
    pub fn clip(&mut self, col: usize, row: usize, w: usize, s: &str, fg: u32, bg: u32) {
        let shown: String = s.chars().take(w).collect();
        self.cells(col, row, w, 1, bg);
        self.text(col, row, &shown, fg, None);
    }

    pub fn right(&mut self, col: usize, row: usize, w: usize, s: &str, fg: u32, bg: u32) {
        let n = s.chars().count().min(w);
        let tail: String = s.chars().skip(s.chars().count() - n).collect();
        self.cells(col, row, w, 1, bg);
        self.text(col + w - n, row, &tail, fg, None);
    }

    pub fn outline(&mut self, r: Rect, color: u32) {
        let m = self.m;
        self.c.outline(r.x * m.cw, r.y * m.ch, r.w * m.cw, r.h * m.ch, m.scale, color);
    }

    pub fn underline(&mut self, col: usize, row: usize, color: u32) {
        let m = self.m;
        self.c.fill_rect(col * m.cw, row * m.ch + 9 * m.scale, 5 * m.scale, m.scale, color);
    }

    /// A tiled window: panel, title bar, border. Returns the content area (one cell in from the border).
    pub fn window(&mut self, r: Rect, title: &str, active: bool) -> Rect {
        let t = self.t;
        self.cells(r.x, r.y, r.w, r.h, t.panel);
        let (bg, fg) = if active { (t.title_on, t.title_on_text) } else { (t.title_off, t.title_off_text) };
        self.clip(r.x, r.y, r.w, &format!(" {title}"), fg, bg);
        self.outline(r, if active { t.title_on } else { t.border });
        Rect { x: r.x + 1, y: r.y + 1, w: r.w.saturating_sub(2), h: r.h.saturating_sub(2) }
    }

    /// A single-line input box, `w` cells wide, drawn on one row. Passwords show as dots.
    pub fn field(&mut self, col: usize, row: usize, w: usize, text: &str, focused: bool, masked: bool) {
        let t = self.t;
        let shown: String = if masked { "*".repeat(text.chars().count()) } else { text.to_string() };
        let room = w.saturating_sub(3);
        let shown = tail(&shown, room);
        self.cells(col, row, w, 1, t.bg);
        self.outline(Rect { x: col, y: row, w, h: 1 }, if focused { t.accent } else { t.border });
        self.text(col + 1, row, &shown, t.text, None);
        if focused { self.text(col + 1 + shown.chars().count(), row, "_", t.accent, None); }
    }

    /// A scrolling list inside `a`. Returns nothing; the caller owns selection and scroll position.
    pub fn list(&mut self, a: Rect, items: &[String], sel: usize, top: usize, active: bool) {
        let t = self.t;
        for i in 0..a.h {
            let idx = top + i;
            let Some(item) = items.get(idx) else { break };
            let (fg, bg) = if idx == sel { if active { (t.accent_text, t.accent) } else { (t.title_off_text, t.title_off) } } else { (t.text, t.panel) };
            self.clip(a.x, a.y + i, a.w, &format!(" {item}"), fg, bg);
        }
    }

    /// Word-wrap `s` to `w` cells and draw it; returns the number of rows used.
    pub fn paragraph(&mut self, col: usize, row: usize, w: usize, s: &str, fg: u32, bg: u32) -> usize {
        let mut y = row;
        let mut line = String::new();
        for word in s.split_whitespace() {
            if !line.is_empty() && line.chars().count() + 1 + word.chars().count() > w {
                self.clip(col, y, w, &line, fg, bg); y += 1; line.clear();
            }
            if !line.is_empty() { line.push(' '); }
            line.push_str(word);
        }
        if !line.is_empty() { self.clip(col, y, w, &line, fg, bg); y += 1; }
        y - row
    }
}

pub fn tail(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max { s.to_string() } else if max > 3 { format!("...{}", s.chars().skip(n - (max - 3)).collect::<String>()) } else { s.chars().skip(n - max).collect() }
}

