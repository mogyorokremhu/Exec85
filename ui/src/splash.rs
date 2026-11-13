//! The boot screen: a glossy "aero" look (sky gradient, drifting glass bubbles, a frosted panel with the name and a
//! shining progress bar). It is drawn from scratch with simple blending, so it needs nothing but the screen.
use crate::gfx::{Canvas, Metrics};

fn lerp(a: f64, b: f64, t: f64) -> f64 { a + (b - a) * t }

fn rgb(c: u32) -> (f64, f64, f64) { (((c >> 16) & 255) as f64, ((c >> 8) & 255) as f64, (c & 255) as f64) }

fn pack(r: f64, g: f64, b: f64) -> u32 { ((r.clamp(0.0, 255.0) as u32) << 16) | ((g.clamp(0.0, 255.0) as u32) << 8) | b.clamp(0.0, 255.0) as u32 }

/// Mix `src` over `dst` with opacity `a` (0..1).
fn blend(dst: u32, src: u32, a: f64) -> u32 {
    let (dr, dg, db) = rgb(dst);
    let (sr, sg, sb) = rgb(src);
    pack(lerp(dr, sr, a), lerp(dg, sg, a), lerp(db, sb, a))
}

fn put(c: &mut Canvas, x: i64, y: i64, color: u32, a: f64) {
    if x < 0 || y < 0 || x >= c.w as i64 || y >= c.h as i64 || a <= 0.0 { return; }
    let i = y as usize * c.w + x as usize;
    c.px[i] = blend(c.px[i], color, a.min(1.0));
}

/// A deterministic pseudo-random number in 0..1 for bubble number `i` and property `k`.
fn rnd(i: usize, k: usize) -> f64 {
    let mut x = (i as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (k as u64 + 7).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    x ^= x >> 29; x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9); x ^= x >> 32;
    (x % 10_000) as f64 / 10_000.0
}

fn sky(c: &mut Canvas) {
    let stops = [(0.0, 0x0A4FA8u32), (0.55, 0x1FA2E8), (1.0, 0x6EE3C2)];
    for y in 0..c.h {
        let f = y as f64 / c.h.max(1) as f64;
        let seg = if f < stops[1].0 { 0 } else { 1 };
        let (f0, c0) = stops[seg];
        let (f1, c1) = stops[seg + 1];
        let t = (f - f0) / (f1 - f0);
        let ((r0, g0, b0), (r1, g1, b1)) = (rgb(c0), rgb(c1));
        let color = pack(lerp(r0, r1, t), lerp(g0, g1, t), lerp(b0, b1, t));
        let row = y * c.w;
        for x in 0..c.w { c.px[row + x] = color; }
    }
    // a soft light in the upper right, like the sun behind glass
    let (cx, cy, r) = (c.w as f64 * 0.8, c.h as f64 * 0.1, c.h as f64 * 0.55);
    for y in (cy - r).max(0.0) as usize..((cy + r) as usize).min(c.h) {
        for x in (cx - r).max(0.0) as usize..((cx + r) as usize).min(c.w) {
            let d = (((x as f64 - cx).powi(2) + (y as f64 - cy).powi(2)).sqrt()) / r;
            if d < 1.0 { put(c, x as i64, y as i64, 0xFFFFFF, 0.28 * (1.0 - d).powi(2)); }
        }
    }
}

fn bubble(c: &mut Canvas, cx: f64, cy: f64, r: f64) {
    for y in ((cy - r).floor() as i64)..=((cy + r).ceil() as i64) {
        for x in ((cx - r).floor() as i64)..=((cx + r).ceil() as i64) {
            let (dx, dy) = (x as f64 - cx, y as f64 - cy);
            let d = (dx * dx + dy * dy).sqrt() / r;
            if d > 1.0 { continue; }
            // glass: a clear body, a brighter rim, and a shine near the upper left
            let mut a = 0.05 + 0.40 * d.powi(8);
            let (sx, sy) = (dx + r * 0.38, dy + r * 0.40);
            let s = (sx * sx + sy * sy).sqrt() / (r * 0.30);
            if s < 1.0 { a += 0.45 * (1.0 - s); }
            put(c, x, y, 0xFFFFFF, a);
        }
    }
}

fn in_rrect(x: f64, y: f64, rx: f64, ry: f64, w: f64, h: f64, r: f64) -> bool {
    if x < rx || y < ry || x >= rx + w || y >= ry + h { return false; }
    let (qx, qy) = ((x - rx).min(rx + w - 1.0 - x), (y - ry).min(ry + h - 1.0 - y));
    if qx >= r || qy >= r { return true; }
    (r - qx).powi(2) + (r - qy).powi(2) <= r * r
}

/// A rounded box. `fill`: colour and opacity; `gloss`: extra opacity for the upper half; `border`: colour, opacity and thickness.
fn rrect(c: &mut Canvas, x: f64, y: f64, w: f64, h: f64, r: f64, fill: (u32, f64), gloss: f64, border: Option<(u32, f64, f64)>) {
    for py in (y.floor() as i64).max(0)..((y + h).ceil() as i64).min(c.h as i64) {
        for px in (x.floor() as i64).max(0)..((x + w).ceil() as i64).min(c.w as i64) {
            let (fx, fy) = (px as f64, py as f64);
            if !in_rrect(fx, fy, x, y, w, h, r) { continue; }
            let mut a = fill.1 + if fy < y + h * 0.5 { gloss } else { 0.0 };
            let mut color = fill.0;
            if let Some((bc, ba, bt)) = border { if !in_rrect(fx, fy, x + bt, y + bt, w - 2.0 * bt, h - 2.0 * bt, (r - bt).max(0.0)) { color = bc; a = ba; } }
            put(c, px, py, color, a);
        }
    }
}

fn text_width(s: &str, scale: usize) -> usize { s.chars().count() * 6 * scale }

fn centered(c: &mut Canvas, cx: usize, y: usize, s: &str, scale: usize, color: u32) {
    let x = cx.saturating_sub(text_width(s, scale) / 2);
    for (i, ch) in s.chars().enumerate() { if ch != ' ' { c.glyph(x + i * 6 * scale, y, ch, scale, color); } }
}

/// One frame. `t` is the time in seconds (it moves the bubbles and the shine); `progress` is 0..1, or None for an endless wait.
pub fn render(c: &mut Canvas, t: f64, progress: Option<f64>, version: &str) {
    let (w, h) = (c.w as f64, c.h as f64);
    sky(c);
    for i in 0..18 {
        let r = lerp(0.018, 0.085, rnd(i, 0)) * h;
        let speed = lerp(0.015, 0.05, rnd(i, 1));
        let phase = (t * speed + rnd(i, 2)).fract();
        let cy = (1.0 - phase) * (h + 2.0 * r) - r;
        let cx = rnd(i, 3) * w + (t * 0.6 + rnd(i, 4) * 6.28).sin() * 0.02 * w;
        bubble(c, cx, cy, r);
    }
    // the frosted panel
    let m = Metrics::for_height(c.h);
    let (pw, ph) = ((w * 0.52).max(280.0).min(w - 20.0), (h * 0.36).max(150.0).min(h - 20.0));
    let (px, py) = ((w - pw) / 2.0, (h - ph) / 2.0);
    rrect(c, px, py, pw, ph, ph * 0.12, (0xFFFFFF, 0.17), 0.10, Some((0xFFFFFF, 0.60, 2.0)));
    // the name, with a soft shadow
    let logo_scale = ((pw * 0.66) / text_width("Exec85", 1) as f64).max(2.0) as usize;
    let cx = (px + pw / 2.0) as usize;
    let ly = (py + ph * 0.16) as usize;
    centered(c, cx + logo_scale / 2 + 1, ly + logo_scale / 2 + 1, "Exec85", logo_scale, 0x0A3070);
    centered(c, cx, ly, "Exec85", logo_scale, 0xFFFFFF);
    centered(c, cx, ly + 9 * logo_scale + m.scale * 4, "Starting your server...", m.scale.max(1) * 2, 0xEAF8FF);
    // the progress bar: a glass pill with a green shining fill
    let (bw, bh) = (pw * 0.78, (ph * 0.085).max(10.0));
    let (bx, by) = ((w - bw) / 2.0, py + ph * 0.80);
    rrect(c, bx, by, bw, bh, bh / 2.0, (0x0A3070, 0.30), 0.0, Some((0xFFFFFF, 0.70, 1.5)));
    let (fx, fw) = match progress {
        Some(p) => (bx, bw * p.clamp(0.0, 1.0)),
        None => { let seg = bw * 0.32; let pos = (t * 0.55).fract(); (bx + (bw + seg) * pos - seg, seg) }
    };
    let (lo, hi) = (fx.max(bx), (fx + fw).min(bx + bw));
    if hi - lo > 2.0 {
        for py2 in (by + 2.0) as i64..(by + bh - 2.0) as i64 {
            let f = (py2 as f64 - by) / bh;
            let base = blend(0x2FB344, 0x7CE05A, 1.0 - f);
            for px2 in lo as i64..hi as i64 {
                if !in_rrect(px2 as f64, py2 as f64, bx + 2.0, by + 2.0, bw - 4.0, bh - 4.0, (bh - 4.0) / 2.0) { continue; }
                let edge = ((px2 as f64 - lo) / (hi - lo)).min(1.0);
                put(c, px2, py2, base, 0.95);
                if f < 0.45 { put(c, px2, py2, 0xFFFFFF, 0.32 * (1.0 - edge * 0.4)); }
            }
        }
    }
    let ver = format!("Exec85 {version}");
    let vs = m.scale.max(1);
    let vx = c.w.saturating_sub(text_width(&ver, vs) + 16);
    for (i, ch) in ver.chars().enumerate() { if ch != ' ' { c.glyph(vx + i * 6 * vs, c.h.saturating_sub(10 * vs + 12), ch, vs, 0xDDF3FF); } }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, t: f64, p: Option<f64>) -> Canvas { let mut c = Canvas::new(w, h); render(&mut c, t, p, "0.6.0"); c }

    #[test]
    fn it_draws_a_sky_a_panel_and_the_name() {
        let c = frame(1280, 800, 1.0, None);
        let top = c.px[10 * c.w + 5];
        let bottom = c.px[(c.h - 5) * c.w + 5];
        assert_ne!(top, bottom, "a gradient, not one colour");
        let (tr, tb) = ((top >> 16) & 255, top & 255);
        assert!(tb > tr, "the top of the sky is blue");
        assert!(bottom & 0xFF00 > top & 0xFF00, "and it turns greener towards the bottom");
        let white = c.px.iter().filter(|p| **p == 0xFFFFFF).count();
        assert!(white > 800, "the name is drawn in white ({white} pixels)");
        let shadow = c.px.iter().filter(|p| **p == 0x0A3070).count();
        assert!(shadow > 400, "with a shadow behind it ({shadow})");
    }

    #[test]
    fn it_moves() {
        let (a, b) = (frame(640, 400, 0.0, None), frame(640, 400, 1.7, None));
        let differing = a.px.iter().zip(&b.px).filter(|(x, y)| x != y).count();
        assert!(differing > 640 * 400 / 50, "bubbles and the shine move between frames ({differing})");
        assert_eq!(frame(640, 400, 3.0, None).px, frame(640, 400, 3.0, None).px, "the same time gives the same picture");
    }

    #[test]
    fn the_bar_fills_as_progress_grows() {
        let (empty, half, full) = (frame(1280, 800, 0.0, Some(0.0)), frame(1280, 800, 0.0, Some(0.5)), frame(1280, 800, 0.0, Some(1.0)));
        let green = |c: &Canvas| c.px.iter().filter(|p| { let (r, g, b) = ((**p >> 16) & 255, (**p >> 8) & 255, **p & 255); g > 150 && r > 20 && r < 190 && b < 110 && g > r + 30 && g > b + 60 }).count();
        let (e, h, f) = (green(&empty), green(&half), green(&full));
        assert!(e < h && h < f, "empty {e} < half {h} < full {f}");
        assert!(f as f64 / h as f64 > 1.6 && (f as f64 / h as f64) < 2.4, "about twice as much green at 100% as at 50%: {h} -> {f}");
    }

    #[test]
    fn any_screen_size_works() {
        for (w, h) in [(160, 100), (320, 200), (640, 480), (1024, 768), (1366, 768), (1920, 1080), (3840, 2160)] {
            let c = frame(w, h, 0.4, Some(0.3));
            assert_eq!(c.px.len(), w * h);
        }
        let mut tiny = Canvas::new(1, 1); render(&mut tiny, 0.0, None, "x");   // does not crash on absurd sizes
        let mut zero = Canvas::new(0, 0); render(&mut zero, 0.0, None, "x");
    }

    #[test]
    fn blending_helpers_behave() {
        assert_eq!(blend(0x000000, 0xFFFFFF, 0.0), 0x000000);
        assert_eq!(blend(0x000000, 0xFFFFFF, 1.0), 0xFFFFFF);
        assert_eq!(blend(0x000000, 0xFFFFFF, 0.5) & 0xFF, 127);
        assert!(in_rrect(50.0, 50.0, 0.0, 0.0, 100.0, 100.0, 20.0) && !in_rrect(0.0, 0.0, 0.0, 0.0, 100.0, 100.0, 20.0), "the corners are rounded");
    }
}
