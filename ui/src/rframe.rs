//! Remote screen pictures: what travels from a remote session to the browser.
//!
//! A message is `[kind u8][length u32 little-endian][payload]`. Kind 1 is a picture update, kind 2 is text
//! (the same state lines the console gets), kind 3 means the session has ended.
//! A picture update lists only the 64x32 tiles that changed since the last one. Each tile is run-length coded
//! (the screen is flat colours and text, so this is small): `x u16, y u16, runs u16, then runs of (count u16, r, g, b)`.
use crate::gfx::Canvas;
use std::io::{self, Read};

pub const KIND_FRAME: u8 = 1;
pub const KIND_STATE: u8 = 2;
pub const KIND_BYE: u8 = 3;
pub const TW: usize = 64;
pub const TH: usize = 32;
/// No message is ever longer than this (a full 1920x1200 screen of noise would be about 11 MB).
pub const MAX_MESSAGE: usize = 24 << 20;

pub fn message(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(payload.len() + 5);
    v.push(kind);
    v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// Read one message; `Ok(None)` at a clean end of the stream.
pub fn read_message(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_MESSAGE { return Err(io::Error::new(io::ErrorKind::InvalidData, "message too long")); }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(Some((head[0], body)))
}

#[derive(Default)]
pub struct Encoder { prev: Vec<u32>, w: usize, h: usize }

impl Encoder {
    pub fn new() -> Encoder { Encoder::default() }
    /// The next picture sent is complete (a browser that has just connected needs everything).
    pub fn force_full(&mut self) { self.prev.clear(); }

    /// A kind-1 payload with the tiles that changed, or None when nothing did.
    pub fn diff(&mut self, c: &Canvas) -> Option<Vec<u8>> {
        let same_size = self.w == c.w && self.h == c.h && self.prev.len() == c.px.len();
        let (tx_n, ty_n) = (c.w.div_ceil(TW), c.h.div_ceil(TH));
        let mut tiles: Vec<u8> = Vec::new();
        let mut count = 0u16;
        for ty in 0..ty_n {
            for tx in 0..tx_n {
                let (x0, y0) = (tx * TW, ty * TH);
                let (x1, y1) = ((x0 + TW).min(c.w), (y0 + TH).min(c.h));
                let changed = !same_size || (y0..y1).any(|y| c.px[y * c.w + x0..y * c.w + x1] != self.prev[y * c.w + x0..y * c.w + x1]);
                if !changed { continue; }
                count += 1;
                tiles.extend_from_slice(&(x0 as u16).to_le_bytes());
                tiles.extend_from_slice(&(y0 as u16).to_le_bytes());
                let at = tiles.len();
                tiles.extend_from_slice(&[0, 0]);
                let mut runs = 0u16;
                let mut cur: Option<(u32, u16)> = None;
                let flush = |cur: &mut Option<(u32, u16)>, tiles: &mut Vec<u8>, runs: &mut u16| {
                    if let Some((px, n)) = cur.take() { tiles.extend_from_slice(&n.to_le_bytes()); tiles.extend_from_slice(&[(px >> 16) as u8, (px >> 8) as u8, px as u8]); *runs += 1; }
                };
                for y in y0..y1 { for x in x0..x1 {
                    let p = c.px[y * c.w + x] & 0x00FF_FFFF;
                    match cur { Some((q, ref mut n)) if q == p && *n < u16::MAX => *n += 1, _ => { flush(&mut cur, &mut tiles, &mut runs); cur = Some((p, 1)); } }
                } }
                flush(&mut cur, &mut tiles, &mut runs);
                tiles[at..at + 2].copy_from_slice(&runs.to_le_bytes());
            }
        }
        if count == 0 && same_size { return None; }
        self.prev.clear(); self.prev.extend_from_slice(&c.px);
        self.w = c.w; self.h = c.h;
        let mut out = Vec::with_capacity(tiles.len() + 6);
        out.extend_from_slice(&(c.w as u16).to_le_bytes());
        out.extend_from_slice(&(c.h as u16).to_le_bytes());
        out.extend_from_slice(&count.to_le_bytes());
        out.extend_from_slice(&tiles);
        Some(out)
    }
}

/// The browser's job done in Rust (used by the tests, and a reference for the page's script).
#[allow(dead_code)]
pub fn apply(payload: &[u8], screen: &mut Option<Canvas>) -> Result<(), String> {
    let u16at = |i: usize| -> Result<usize, String> { payload.get(i..i + 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize).ok_or_else(|| "short".to_string()) };
    let (w, h, n) = (u16at(0)?, u16at(2)?, u16at(4)?);
    if w == 0 || h == 0 || w > 4096 || h > 4096 { return Err("bad size".into()); }
    if screen.as_ref().map_or(true, |s| s.w != w || s.h != h) { *screen = Some(Canvas::new(w, h)); }
    let c = screen.as_mut().unwrap();
    let mut i = 6;
    for _ in 0..n {
        let (x0, y0, runs) = (u16at(i)?, u16at(i + 2)?, u16at(i + 4)?);
        i += 6;
        let (mut x, mut y) = (x0, y0);
        let (x1, _y1) = ((x0 + TW).min(w), (y0 + TH).min(h));
        for _ in 0..runs {
            let cnt = u16at(i)?;
            let rgb = payload.get(i + 2..i + 5).ok_or("short")?;
            let px = ((rgb[0] as u32) << 16) | ((rgb[1] as u32) << 8) | rgb[2] as u32;
            i += 5;
            for _ in 0..cnt {
                if y >= h { return Err("run past the tile".into()); }
                c.px[y * w + x] = px;
                x += 1;
                if x >= x1 { x = x0; y += 1; }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(w: usize, h: usize, f: impl Fn(usize, usize) -> u32) -> Canvas {
        let mut c = Canvas::new(w, h);
        for y in 0..h { for x in 0..w { c.px[y * w + x] = f(x, y); } }
        c
    }

    #[test]
    fn the_first_picture_is_complete_and_decodes_to_the_same_screen() {
        let c = canvas(150, 70, |x, y| ((x * 7 + y * 13) as u32).wrapping_mul(2654435761) & 0xFFFFFF);   // noise: the worst case
        let mut e = Encoder::new();
        let p = e.diff(&c).unwrap();
        let mut screen = None;
        apply(&p, &mut screen).unwrap();
        assert_eq!(screen.unwrap().px, c.px.iter().map(|p| p & 0xFFFFFF).collect::<Vec<_>>());
    }

    #[test]
    fn nothing_changed_means_nothing_is_sent_and_a_small_change_sends_one_tile() {
        let mut c = canvas(256, 128, |_, _| 0x102838);
        let mut e = Encoder::new();
        let first = e.diff(&c).unwrap();
        assert!(first.len() < 256 * 128 / 4, "a flat screen is tiny: {} bytes", first.len());
        assert!(e.diff(&c).is_none());
        c.px[100 * 256 + 200] = 0xFFFFFF;
        let p = e.diff(&c).unwrap();
        assert_eq!(u16::from_le_bytes([p[4], p[5]]), 1, "exactly one tile changed");
        let mut screen = None;
        apply(&first, &mut screen).unwrap(); apply(&p, &mut screen).unwrap();
        assert_eq!(screen.unwrap().px[100 * 256 + 200], 0xFFFFFF);
    }

    #[test]
    fn a_new_browser_gets_everything_again_and_a_resize_does_too() {
        let c = canvas(130, 70, |x, _| (x as u32) << 8);
        let mut e = Encoder::new();
        e.diff(&c).unwrap();
        assert!(e.diff(&c).is_none());
        e.force_full();
        let p = e.diff(&c).unwrap();
        assert_eq!(u16::from_le_bytes([p[4], p[5]]) as usize, 130usize.div_ceil(TW) * 70usize.div_ceil(TH));
        let bigger = canvas(140, 70, |x, _| (x as u32) << 8);
        assert!(e.diff(&bigger).is_some(), "a different size is a different picture");
    }

    #[test]
    fn messages_round_trip_and_reject_nonsense() {
        let m = message(KIND_STATE, b"hello");
        let mut r = &m[..];
        assert_eq!(read_message(&mut r).unwrap(), Some((KIND_STATE, b"hello".to_vec())));
        assert_eq!(read_message(&mut r).unwrap(), None, "clean end");
        let mut huge = vec![1u8]; huge.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_message(&mut &huge[..]).is_err());
        let mut cut = message(KIND_BYE, b"abcdef"); cut.truncate(8);
        assert!(read_message(&mut &cut[..]).is_err(), "a message cut short is an error, not a short read");
    }

    #[test]
    fn broken_pictures_are_refused_not_trusted() {
        let mut s = None;
        assert!(apply(&[1, 0], &mut s).is_err());
        assert!(apply(&[0, 0, 0, 0, 0, 0], &mut s).is_err(), "zero size");
        let p = [64, 0, 32, 0, 1, 0,  0, 0, 0, 0, 1, 0,  255, 255, 1, 2, 3];   // a run of 65535 pixels in a 2048-pixel tile
        assert!(apply(&p, &mut s).is_err());
    }
}
