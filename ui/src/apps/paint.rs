//! Paint: draw pictures with the keyboard. Arrows move the cursor, Space lifts or lowers the pen.
//! Tools: pencil, eraser, line, rectangle, filled box, flood fill. Pictures save as PPM (P6) files.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::path::PathBuf;

pub const PALETTE: [u32; 16] = [
    0x000000, 0x1B1B8F, 0x1B8F1B, 0x1B8F8F, 0x8F1B1B, 0x8F1B8F, 0x8F5A1B, 0xB8B8B8,
    0x585858, 0x4B4BFF, 0x4BFF4B, 0x4BFFFF, 0xFF4B4B, 0xFF4BFF, 0xFFFF4B, 0xFFFFFF,
];
const BG: u8 = 15;
const MAX_UNDO: usize = 30;
const MAX_SIDE: usize = 256;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool { Pencil, Eraser, Line, Rect, Box, }

impl Tool {
    fn name(self) -> &'static str { match self { Tool::Pencil => "pencil", Tool::Eraser => "eraser", Tool::Line => "line", Tool::Rect => "rectangle", Tool::Box => "box" } }
}

#[derive(Clone, PartialEq, Eq, Debug)]
enum Prompt { None, Save(String), Open(String) }

// ---- geometry helpers (pure) ----------------------------------------------------------------------

/// Pixels on the straight line between two points (Bresenham), both ends included.
pub fn line_points(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let (mut x, mut y) = (x0, y0);
    let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
    let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
    let mut err = dx + dy;
    let mut out = Vec::new();
    loop {
        out.push((x, y));
        if x == x1 && y == y1 { return out; }
        let e2 = 2 * err;
        if e2 >= dy { err += dy; x += sx; }
        if e2 <= dx { err += dx; y += sy; }
    }
}

pub fn rect_outline(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let (l, r, t, b) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
    let mut v = Vec::new();
    for x in l..=r { v.push((x, t)); if b != t { v.push((x, b)); } }
    for y in (t + 1)..b { v.push((l, y)); if r != l { v.push((r, y)); } }
    v
}

pub fn rect_filled(x0: i32, y0: i32, x1: i32, y1: i32) -> Vec<(i32, i32)> {
    let (l, r, t, b) = (x0.min(x1), x0.max(x1), y0.min(y1), y0.max(y1));
    (t..=b).flat_map(|y| (l..=r).map(move |x| (x, y))).collect()
}

fn nearest(r: u8, g: u8, b: u8) -> u8 {
    (0..16u8).min_by_key(|&i| {
        let c = PALETTE[i as usize];
        let (pr, pg, pb) = ((c >> 16) as i32, ((c >> 8) & 255) as i32, (c & 255) as i32);
        (pr - r as i32).pow(2) + (pg - g as i32).pow(2) + (pb - b as i32).pow(2)
    }).unwrap_or(0)
}

pub struct Paint {
    w: usize,
    h: usize,
    px: Vec<u8>,
    cx: usize,
    cy: usize,
    color: u8,
    tool: Tool,
    pen: bool,
    anchor: Option<(usize, usize)>,
    undo: Vec<Vec<u8>>,
    view: (usize, usize),
    top: (usize, usize), // top-left canvas pixel shown
    sized: bool,
    touched: bool,
    base: PathBuf,
    msg: String,
    prompt: Prompt,
}

impl Paint {
    pub fn new(env: &Env) -> Paint {
        Paint { w: 48, h: 24, px: vec![BG; 48 * 24], cx: 0, cy: 0, color: 0, tool: Tool::Pencil, pen: false, anchor: None, undo: Vec::new(),
            view: (48, 24), top: (0, 0), sized: false, touched: false, base: env.cwd.clone(), msg: String::new(), prompt: Prompt::None }
    }

    fn idx(&self, x: usize, y: usize) -> usize { y * self.w + x }
    pub fn get(&self, x: usize, y: usize) -> u8 { self.px[self.idx(x, y)] }

    fn snapshot(&mut self) { self.undo.push(self.px.clone()); if self.undo.len() > MAX_UNDO { self.undo.remove(0); } }

    fn put(&mut self, x: i32, y: i32, c: u8) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h { let i = self.idx(x as usize, y as usize); self.px[i] = c; }
    }

    fn paint_points(&mut self, pts: Vec<(i32, i32)>, c: u8) { self.snapshot(); self.touched = true; for (x, y) in pts { self.put(x, y, c); } }

    fn flood(&mut self) {
        let (sx, sy) = (self.cx, self.cy);
        let target = self.get(sx, sy);
        if target == self.color { return; }
        self.snapshot();
        self.touched = true;
        let mut stack = vec![(sx, sy)];
        while let Some((x, y)) = stack.pop() {
            if self.get(x, y) != target { continue; }
            let i = self.idx(x, y);
            self.px[i] = self.color;
            if x > 0 { stack.push((x - 1, y)); }
            if y > 0 { stack.push((x, y - 1)); }
            if x + 1 < self.w { stack.push((x + 1, y)); }
            if y + 1 < self.h { stack.push((x, y + 1)); }
        }
    }

    fn stamp(&mut self) {
        let c = if self.tool == Tool::Eraser { BG } else { self.color };
        self.touched = true;
        let (x, y) = (self.cx as i32, self.cy as i32);
        self.put(x, y, c);
    }

    fn move_cursor(&mut self, dx: i32, dy: i32, step: i32) {
        let (ox, oy) = (self.cx as i32, self.cy as i32);
        let nx = (ox + dx * step).clamp(0, self.w as i32 - 1);
        let ny = (oy + dy * step).clamp(0, self.h as i32 - 1);
        if self.pen && matches!(self.tool, Tool::Pencil | Tool::Eraser) {
            if !self.touched || self.undo.is_empty() { self.snapshot(); }
            let c = if self.tool == Tool::Eraser { BG } else { self.color };
            for (x, y) in line_points(ox, oy, nx, ny) { self.put(x, y, c); }
            self.touched = true;
        }
        self.cx = nx as usize; self.cy = ny as usize;
        self.scroll_to_cursor();
    }

    fn scroll_to_cursor(&mut self) {
        let (vw, vh) = (self.view.0.max(1), self.view.1.max(1));
        if self.cx < self.top.0 { self.top.0 = self.cx; }
        if self.cx >= self.top.0 + vw { self.top.0 = self.cx + 1 - vw; }
        if self.cy < self.top.1 { self.top.1 = self.cy; }
        if self.cy >= self.top.1 + vh { self.top.1 = self.cy + 1 - vh; }
    }

    fn space(&mut self) {
        match self.tool {
            Tool::Pencil | Tool::Eraser => {
                self.pen = !self.pen;
                if self.pen { self.snapshot(); self.stamp(); }
            }
            _ => match self.anchor.take() {
                None => self.anchor = Some((self.cx, self.cy)),
                Some((ax, ay)) => {
                    let (ax, ay, bx, by) = (ax as i32, ay as i32, self.cx as i32, self.cy as i32);
                    let pts = match self.tool { Tool::Line => line_points(ax, ay, bx, by), Tool::Rect => rect_outline(ax, ay, bx, by), _ => rect_filled(ax, ay, bx, by) };
                    self.paint_points(pts, self.color);
                }
            },
        }
    }

    pub fn pixels_drawn(&self) -> usize { self.px.iter().filter(|&&p| p != BG).count() }

    fn to_ppm(&self) -> Vec<u8> {
        let mut out = format!("P6\n{} {}\n255\n", self.w, self.h).into_bytes();
        for &p in &self.px { let c = PALETTE[p as usize]; out.extend_from_slice(&[(c >> 16) as u8, (c >> 8) as u8, c as u8]); }
        out
    }

    fn from_ppm(&mut self, data: &[u8]) -> Result<(), String> {
        // header: "P6", width, height, maxval separated by whitespace, '#' comments allowed
        let mut pos = 0;
        let mut token = |data: &[u8]| -> Option<String> {
            while pos < data.len() && (data[pos].is_ascii_whitespace() || data[pos] == b'#') {
                if data[pos] == b'#' { while pos < data.len() && data[pos] != b'\n' { pos += 1; } } else { pos += 1; }
            }
            let start = pos;
            while pos < data.len() && !data[pos].is_ascii_whitespace() { pos += 1; }
            if start == pos { None } else { Some(String::from_utf8_lossy(&data[start..pos]).into_owned()) }
        };
        if token(data).as_deref() != Some("P6") { return Err("Only P6 (binary) PPM pictures can be opened.".into()); }
        let w: usize = token(data).and_then(|t| t.parse().ok()).ok_or("Bad picture width.")?;
        let h: usize = token(data).and_then(|t| t.parse().ok()).ok_or("Bad picture height.")?;
        let max: usize = token(data).and_then(|t| t.parse().ok()).ok_or("Bad picture header.")?;
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE || max != 255 { return Err(format!("Pictures up to {MAX_SIDE}x{MAX_SIDE} with 8-bit colour can be opened.")); }
        pos += 1; // the single whitespace after maxval
        if data.len() < pos + w * h * 3 { return Err("The picture is truncated.".into()); }
        self.snapshot();
        self.w = w; self.h = h;
        self.px = (0..w * h).map(|i| nearest(data[pos + i * 3], data[pos + i * 3 + 1], data[pos + i * 3 + 2])).collect();
        self.cx = 0; self.cy = 0; self.top = (0, 0); self.sized = true; self.touched = true; self.anchor = None;
        Ok(())
    }

    fn prompt_key(&mut self, k: Key) {
        let (Prompt::Save(text) | Prompt::Open(text)) = &mut self.prompt else { return };
        match k.code {
            code::ESC => self.prompt = Prompt::None,
            code::BACKSPACE => { text.pop(); }
            code::ENTER => {
                let t = text.clone();
                let saving = matches!(self.prompt, Prompt::Save(_));
                self.prompt = Prompt::None;
                if t.trim().is_empty() { return; }
                let path = if t.starts_with('/') { PathBuf::from(&t) } else { self.base.join(&t) };
                if saving {
                    self.msg = match std::fs::write(&path, self.to_ppm()) { Ok(()) => format!("Saved {t}."), Err(e) => format!("Cannot save: {e}") };
                } else {
                    self.msg = match std::fs::read(&path).map_err(|e| e.to_string()).and_then(|d| self.from_ppm(&d)) { Ok(()) => format!("Opened {t}."), Err(e) => e };
                }
            }
            _ => if let Some(c) = k.ch() { if text.chars().count() < 200 { text.push(c); } },
        }
    }
}

impl Application for Paint {
    fn kind(&self) -> Kind { Kind::Paint }
    fn title(&self) -> String { "Paint".into() }

    fn resize(&mut self, cols: usize, rows: usize) {
        self.view = (cols.max(1), rows.saturating_sub(3).max(1));
        // The first time the window gets its size, make the picture fill it; afterwards the picture keeps its size.
        if !self.sized && !self.touched {
            self.w = self.view.0.clamp(8, MAX_SIDE); self.h = self.view.1.clamp(4, MAX_SIDE);
            self.px = vec![BG; self.w * self.h];
            self.cx = self.cx.min(self.w - 1); self.cy = self.cy.min(self.h - 1);
        }
        self.scroll_to_cursor();
    }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        if self.prompt != Prompt::None { self.prompt_key(k); return Out::None; }
        self.msg.clear();
        if k.ctrl {
            match k.base_char() {
                Some('s') => self.prompt = Prompt::Save("picture.ppm".into()),
                Some('o') => self.prompt = Prompt::Open(String::new()),
                Some('z') => self.do_undo(),
                _ => {}
            }
            return Out::None;
        }
        let step = if k.shift { 8 } else { 1 };
        match k.code {
            code::UP => self.move_cursor(0, -1, step),
            code::DOWN => self.move_cursor(0, 1, step),
            code::LEFT => self.move_cursor(-1, 0, step),
            code::RIGHT => self.move_cursor(1, 0, step),
            code::HOME => { self.cx = 0; self.scroll_to_cursor(); }
            code::END => { self.cx = self.w - 1; self.scroll_to_cursor(); }
            code::SPACE | code::ENTER => self.space(),
            code::ESC => { self.anchor = None; self.pen = false; }
            _ => if let Some(c) = k.ch() {
                match c.to_ascii_lowercase() {
                    'p' => { self.tool = Tool::Pencil; self.anchor = None; }
                    'e' => { self.tool = Tool::Eraser; self.anchor = None; }
                    'l' => { self.tool = Tool::Line; self.pen = false; self.anchor = None; }
                    'r' => { self.tool = Tool::Rect; self.pen = false; self.anchor = None; }
                    'b' => { self.tool = Tool::Box; self.pen = false; self.anchor = None; }
                    'f' => self.flood(),
                    'c' => { self.snapshot(); self.px.fill(BG); self.touched = true; }
                    'u' => self.do_undo(),
                    '[' => self.color = (self.color + 15) % 16,
                    ']' => self.color = (self.color + 1) % 16,
                    d @ '0'..='9' => self.color = d as u8 - b'0',
                    _ => {}
                }
            },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 5 || a.w < 12 { return; }
        // toolbar: the tool and the 16 colours (two cells each)
        p.clip(a.x, a.y, a.w, &format!("{}{}", self.tool.name(), if self.pen { " (pen down)" } else { "" }), t.text, t.panel);
        for (i, c) in PALETTE.iter().enumerate() {
            let x = a.x + i * 2;
            if x + 2 > a.x + a.w { break; }
            p.cells(x, a.y + 1, 2, 1, *c);
            if i as u8 == self.color { p.text(x, a.y + 1, "*", if *c > 0x808080 { 0x000000 } else { 0xFFFFFF }, None); }
        }
        let rows = a.h - 3;
        let (cw, ch, s) = (p.m.cw, p.m.ch, p.m.scale);
        for vy in 0..rows.min(self.h - self.top.1) {
            for vx in 0..a.w.min(self.w - self.top.0) {
                let c = PALETTE[self.get(self.top.0 + vx, self.top.1 + vy) as usize];
                p.c.fill_rect((a.x + vx) * cw, (a.y + 2 + vy) * ch, cw, ch, c);
            }
        }
        if focused && self.cx >= self.top.0 && self.cy >= self.top.1 && self.cx - self.top.0 < a.w && self.cy - self.top.1 < rows {
            let (x, y) = ((a.x + self.cx - self.top.0) * cw, (a.y + 2 + self.cy - self.top.1) * ch);
            let under = PALETTE[self.get(self.cx, self.cy) as usize];
            let lum = ((under >> 16) & 255) * 3 + ((under >> 8) & 255) * 6 + (under & 255);
            p.c.outline(x, y, cw, ch, s, if lum > 1280 { 0x000000 } else { 0xFFFFFF });
        }
        let y = a.y + a.h - 1;
        match &self.prompt {
            Prompt::Save(text) => { p.text(a.x, y, "Save as:", t.accent, None); p.field(a.x + 9, y, a.w.saturating_sub(9), text, true, false); }
            Prompt::Open(text) => { p.text(a.x, y, "Open:", t.accent, None); p.field(a.x + 6, y, a.w.saturating_sub(6), text, true, false); }
            Prompt::None => p.clip(a.x, y, a.w, &format!("{},{}  {}x{}  {}", self.cx, self.cy, self.w, self.h, self.msg), t.dim, t.bar),
        }
    }

    fn state(&self) -> String {
        format!("name=paint tool={} color={} cursor={},{} pen={} w={} h={} pixels={} anchor={} undo={}",
            self.tool.name(), self.color, self.cx, self.cy, if self.pen { "down" } else { "up" }, self.w, self.h, self.pixels_drawn(),
            self.anchor.is_some(), self.undo.len())
    }
    fn hint(&self) -> &'static str { "Arrows move (Shift=8)  Space pen/anchor  P E L R B tools  F fill  0-9 [ ] colour  U undo  Ctrl+S/O" }
}

impl Paint {
    fn do_undo(&mut self) {
        if let Some(prev) = self.undo.pop() { if prev.len() == self.px.len() { self.px = prev; } else { self.px = prev; } self.msg = "Undone.".into(); } else { self.msg = "Nothing to undo.".into(); }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::fs;

    fn env() -> Env { Env::for_tests() }
    fn press(p: &mut Paint, keys: &str) { for k in keys.split(',').filter(|s| !s.is_empty()) { p.handle(parse_key(k).unwrap_or_else(|| panic!("bad key {k}")), &env()); } }
    fn small() -> Paint { let mut p = Paint::new(&env()); p.resize(20, 13); p }   // a 20 x 10 picture

    #[test]
    fn lines_rectangles_and_boxes() {
        assert_eq!(line_points(0, 0, 4, 0), vec![(0, 0), (1, 0), (2, 0), (3, 0), (4, 0)]);
        let d = line_points(0, 0, 3, 3);
        assert_eq!(d, vec![(0, 0), (1, 1), (2, 2), (3, 3)]);
        let back = line_points(3, 1, 0, 0);
        assert_eq!((back.first().unwrap(), back.last().unwrap()), (&(3, 1), &(0, 0)));
        assert_eq!(line_points(2, 2, 2, 2), vec![(2, 2)], "a single point");
        let o = rect_outline(1, 1, 4, 3);
        assert_eq!(o.len(), 4 + 4 + 1 + 1, "top and bottom rows plus one pixel on each side of the middle row");
        assert!(!o.contains(&(2, 2)) && o.contains(&(1, 2)) && o.contains(&(4, 2)));
        assert_eq!(rect_filled(4, 3, 1, 1).len(), 12, "corner order does not matter");
        assert_eq!(rect_outline(2, 2, 2, 2), vec![(2, 2)]);
    }

    #[test]
    fn first_window_size_decides_the_picture_size() {
        let p = small();
        assert_eq!((p.w, p.h), (20, 10));
        assert_eq!(p.pixels_drawn(), 0);
    }

    #[test]
    fn pencil_draws_while_the_pen_is_down() {
        let mut p = small();
        press(&mut p, "right,right");
        assert_eq!(p.pixels_drawn(), 0, "pen is up: moving draws nothing");
        press(&mut p, "space,right,right,right");
        assert_eq!(p.pixels_drawn(), 4, "the pixel under the cursor and three more");
        assert_eq!(p.get(2, 0), 0);
        assert_eq!(p.get(5, 0), 0);
        press(&mut p, "space,right");
        assert_eq!(p.pixels_drawn(), 4, "pen up again");
        press(&mut p, "right,right,right");
        assert_eq!(p.pixels_drawn(), 4);
    }

    #[test]
    fn shift_moves_eight_and_the_pen_covers_the_gap() {
        let mut p = small();
        press(&mut p, "space,shift-right");
        assert_eq!(p.cx, 8);
        assert_eq!(p.pixels_drawn(), 9, "no gaps when jumping");
        press(&mut p, "shift-right,shift-right");
        assert_eq!(p.cx, 19, "stops at the edge");
        press(&mut p, "shift-down,shift-down");
        assert_eq!(p.cy, 9);
    }

    #[test]
    fn colours_tools_and_eraser() {
        let mut p = small();
        press(&mut p, "4,space,right");
        assert_eq!((p.get(0, 0), p.get(1, 0)), (4, 4));
        press(&mut p, "]");
        assert_eq!(p.color, 5);
        press(&mut p, "0,[");
        assert_eq!(p.color, 15, "[ wraps around to the last colour");
        press(&mut p, "e,left");                     // eraser with the pen still down paints the background
        assert_eq!(p.get(0, 0), BG);
        press(&mut p, "space");
        assert_eq!(p.state().contains("pen=up"), true);
    }

    #[test]
    fn line_rectangle_and_box_tools_use_two_corners() {
        let mut p = small();
        press(&mut p, "2,l,space,right,right,right,down,down,space");
        assert_eq!(p.pixels_drawn(), 4, "a line from (0,0) to (3,2)");
        assert_eq!(p.get(3, 2), 2);
        let mut p = small();
        press(&mut p, "r,space,right,right,right,down,down,space");
        assert_eq!(p.pixels_drawn(), 10, "outline of a 4 x 3 rectangle");
        assert_eq!(p.get(1, 1), BG, "hollow");
        let mut p = small();
        press(&mut p, "b,space,right,right,right,down,down,space");
        assert_eq!(p.pixels_drawn(), 12);
        // Esc cancels a half-finished shape
        let mut p = small();
        press(&mut p, "l,space,right,esc,right,space");
        assert_eq!(p.state().contains("anchor=true"), true, "after Esc the next Space starts a new line");
        assert_eq!(p.pixels_drawn(), 0);
    }

    #[test]
    fn flood_fill_stops_at_other_colours() {
        let mut p = small();
        press(&mut p, "1,r,space,shift-right,shift-down,space,p");   // a blue frame 9 x 9
        let frame = p.pixels_drawn();
        assert_eq!(frame, 32);
        press(&mut p, "left,left,left,left,up,up,up,up,3,f");           // the cursor was on the frame's corner (8,8); now (4,4)
        assert_eq!(p.get(4, 4), 3, "inside the frame is filled");
        assert_eq!(p.get(10, 0), BG, "outside the frame is not");
        assert_eq!(p.pixels_drawn(), frame + 49, "7 x 7 inside");
        press(&mut p, "f");
        assert_eq!(p.pixels_drawn(), frame + 49, "filling with the colour already there does nothing");
    }

    #[test]
    fn undo_steps_back_through_changes() {
        let mut p = small();
        press(&mut p, "b,space,right,right,space");
        assert_eq!(p.pixels_drawn(), 3);
        press(&mut p, "u");
        assert_eq!(p.pixels_drawn(), 0);
        press(&mut p, "u");
        assert!(p.msg == "Nothing to undo.");
        press(&mut p, "ctrl-z");
        assert_eq!(p.pixels_drawn(), 0);
        press(&mut p, "p,space,right,c");
        assert_eq!(p.pixels_drawn(), 0, "clear");
        press(&mut p, "u");
        assert_eq!(p.pixels_drawn(), 2, "and clear can be undone");
    }

    #[test]
    fn pictures_round_trip_through_ppm_files() {
        let d = std::env::temp_dir().join(format!("exec85-paint-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        let mut p = Paint::new(&Env { cwd: d.clone(), ..env() });
        p.resize(20, 13);
        press(&mut p, "9,b,space,right,right,down,space,ctrl-s");
        for _ in 0.."picture.ppm".len() { press(&mut p, "backspace"); }
        for c in "art.ppm".chars() { p.handle(parse_key(&c.to_string()).unwrap(), &env()); }
        press(&mut p, "enter");
        let bytes = fs::read(d.join("art.ppm")).unwrap();
        assert!(bytes.starts_with(b"P6\n20 10\n255\n"));
        assert_eq!(bytes.len(), 13 + 20 * 10 * 3);
        let mut q = Paint::new(&Env { cwd: d.clone(), ..env() });
        q.resize(20, 13);
        q.from_ppm(&bytes).unwrap();
        assert_eq!(q.px, p.px, "same picture after loading");
        // other sizes and bad files
        assert!(q.from_ppm(b"P3\n1 1\n255\n").is_err());
        assert!(q.from_ppm(b"P6\n2 2\n255\n\x00").is_err(), "truncated");
        assert!(q.from_ppm(b"P6\n9999 2\n255\n").is_err());
        q.from_ppm(b"P6\n# a comment\n2 1\n255\n\xff\x00\x00\x00\x00\xff").unwrap();
        assert_eq!((q.w, q.h, q.px.clone()), (2, 1, vec![12, 9]), "arbitrary colours snap to the nearest palette colour");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let mut p = small();
        press(&mut p, "7,space,shift-right,shift-down");
        for (cols, rows) in [(12usize, 5usize), (30, 12), (80, 40)] {
            let m = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut pt = Painter { c: &mut canvas, m, t: Theme::new(Mode::Dark) };
            p.draw(&mut pt, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
    }
}
