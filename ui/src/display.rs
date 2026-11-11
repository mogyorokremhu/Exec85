//! Getting pixels onto a real screen.
//!
//! First choice: DRM/KMS "dumb buffer" (the modern Linux display API, works
//! with Intel, AMD, nouveau, virtio, bochs and the generic EFI/VESA driver).
//! Fallback: the old framebuffer device /dev/fb0.
use crate::gfx::Canvas;
use crate::sys::{ioctl_ptr, map_shared};
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::AsRawFd;

pub struct Display {
    _file: File,
    map: *mut u32,
    pub width: usize,
    pub height: usize,
    stride_px: usize,
    pub kind: &'static str,
}

impl Display {
    pub fn open() -> io::Result<Display> {
        match drm::open() {
            Ok(d) => Ok(d),
            Err(e1) => fbdev::open().map_err(|e2| io::Error::new(io::ErrorKind::Other, format!("drm: {e1}; fbdev: {e2}"))),
        }
    }

    /// Copy a finished canvas to the screen.
    pub fn present(&self, c: &Canvas) {
        let w = self.width.min(c.w);
        for y in 0..self.height.min(c.h) {
            // SAFETY: `map` covers stride_px*height pixels; we stay inside it.
            unsafe {
                std::ptr::copy_nonoverlapping(c.px.as_ptr().add(y * c.w), self.map.add(y * self.stride_px), w);
            }
        }
    }
}

fn open_rw(path: &str) -> io::Result<File> { OpenOptions::new().read(true).write(true).open(path) }

mod drm {
    use super::*;
    use std::os::raw::c_ulong;

    // ioctl numbers: _IOWR('d', nr, size)
    const fn iowr(nr: u32, size: usize) -> c_ulong { ((3u32 << 30) | ((size as u32) << 16) | (0x64 << 8) | nr) as c_ulong }
    #[derive(Default, Clone, Copy)] #[repr(C)]
    pub struct ModeInfo { clock: u32, hdisplay: u16, hsync_start: u16, hsync_end: u16, htotal: u16, hskew: u16,
        vdisplay: u16, vsync_start: u16, vsync_end: u16, vtotal: u16, vscan: u16, vrefresh: u32, flags: u32, typ: u32, name: [u8; 32] }
    #[derive(Default)] #[repr(C)]
    struct CardRes { fb_ptr: u64, crtc_ptr: u64, conn_ptr: u64, enc_ptr: u64, n_fb: u32, n_crtc: u32, n_conn: u32, n_enc: u32,
        min_w: u32, max_w: u32, min_h: u32, max_h: u32 }
    #[derive(Default)] #[repr(C)]
    struct Connector { enc_ptr: u64, modes_ptr: u64, props_ptr: u64, prop_vals_ptr: u64, n_modes: u32, n_props: u32, n_enc: u32,
        encoder_id: u32, connector_id: u32, connector_type: u32, connector_type_id: u32, connection: u32, mm_w: u32, mm_h: u32,
        subpixel: u32, pad: u32 }
    #[derive(Default)] #[repr(C)]
    struct Encoder { encoder_id: u32, encoder_type: u32, crtc_id: u32, possible_crtcs: u32, possible_clones: u32 }
    #[derive(Default)] #[repr(C)]
    struct Crtc { set_conn_ptr: u64, n_conn: u32, crtc_id: u32, fb_id: u32, x: u32, y: u32, gamma_size: u32, mode_valid: u32, mode: ModeInfo }
    #[derive(Default)] #[repr(C)]
    struct FbCmd { fb_id: u32, width: u32, height: u32, pitch: u32, bpp: u32, depth: u32, handle: u32 }
    #[derive(Default)] #[repr(C)]
    struct CreateDumb { height: u32, width: u32, bpp: u32, flags: u32, handle: u32, pitch: u32, size: u64 }
    #[derive(Default)] #[repr(C)]
    struct MapDumb { handle: u32, pad: u32, offset: u64 }

    pub const GETRESOURCES: c_ulong = iowr(0xA0, std::mem::size_of::<CardRes>());
    #[allow(dead_code)]
    pub const GETCRTC: c_ulong = iowr(0xA1, std::mem::size_of::<Crtc>());
    pub const SETCRTC: c_ulong = iowr(0xA2, std::mem::size_of::<Crtc>());
    pub const GETENCODER: c_ulong = iowr(0xA6, std::mem::size_of::<Encoder>());
    pub const GETCONNECTOR: c_ulong = iowr(0xA7, std::mem::size_of::<Connector>());
    pub const ADDFB: c_ulong = iowr(0xAE, std::mem::size_of::<FbCmd>());
    pub const CREATE_DUMB: c_ulong = iowr(0xB2, std::mem::size_of::<CreateDumb>());
    pub const MAP_DUMB: c_ulong = iowr(0xB3, std::mem::size_of::<MapDumb>());
    const SET_MASTER: c_ulong = 0x641E; // _IO('d', 0x1e)

    fn err(s: &str) -> io::Error { io::Error::new(io::ErrorKind::Other, s.to_string()) }

    pub fn open() -> io::Result<Display> {
        let mut last = err("no /dev/dri/card* device");
        for n in 0..4 {
            match try_card(&format!("/dev/dri/card{n}")) { Ok(d) => return Ok(d), Err(e) => last = io::Error::new(e.kind(), format!("card{n}: {e}")) }
        }
        Err(last)
    }

    fn try_card(path: &str) -> io::Result<Display> {
        let file = open_rw(path)?;
        let fd = file.as_raw_fd();
        // Become the display "master" so we may change the video mode (ignore failure: we may already be).
        unsafe { crate::sys::ioctl(fd, SET_MASTER) };

        // 1) how many connectors/crtcs exist
        let mut res = CardRes::default();
        ioctl_ptr(fd, GETRESOURCES, &mut res)?;
        let mut crtcs = vec![0u32; res.n_crtc as usize];
        let mut conns = vec![0u32; res.n_conn as usize];
        let mut encs = vec![0u32; res.n_enc as usize];
        let mut fbs = vec![0u32; res.n_fb as usize];
        res.crtc_ptr = crtcs.as_mut_ptr() as u64; res.conn_ptr = conns.as_mut_ptr() as u64;
        res.enc_ptr = encs.as_mut_ptr() as u64; res.fb_ptr = fbs.as_mut_ptr() as u64;
        ioctl_ptr(fd, GETRESOURCES, &mut res)?;

        // 2) find a connected connector that has at least one video mode
        for &cid in &conns {
            let mut c = Connector { connector_id: cid, ..Default::default() };
            ioctl_ptr(fd, GETCONNECTOR, &mut c)?;
            if c.connection != 1 || c.n_modes == 0 { continue; }
            let mut modes = vec![ModeInfo::default(); c.n_modes as usize + 4];
            let mut cenc = vec![0u32; c.n_enc as usize + 2];
            let mut c2 = Connector { connector_id: cid, n_modes: modes.len() as u32, n_enc: cenc.len() as u32,
                modes_ptr: modes.as_mut_ptr() as u64, enc_ptr: cenc.as_mut_ptr() as u64, ..Default::default() };
            ioctl_ptr(fd, GETCONNECTOR, &mut c2)?;
            if c2.n_modes == 0 { continue; }
            let mode = modes[0]; // the kernel lists the preferred mode first

            // 3) pick a CRTC (the thing that scans out pixels) for this connector
            let mut crtc_id = 0;
            if c2.encoder_id != 0 {
                let mut e = Encoder { encoder_id: c2.encoder_id, ..Default::default() };
                if ioctl_ptr(fd, GETENCODER, &mut e).is_ok() { crtc_id = e.crtc_id; }
            }
            if crtc_id == 0 {
                'outer: for &eid in cenc.iter().take(c2.n_enc as usize) {
                    let mut e = Encoder { encoder_id: eid, ..Default::default() };
                    if ioctl_ptr(fd, GETENCODER, &mut e).is_err() { continue; }
                    for (i, &cr) in crtcs.iter().enumerate() {
                        if e.possible_crtcs & (1 << i) != 0 { crtc_id = cr; break 'outer; }
                    }
                }
            }
            if crtc_id == 0 { continue; }

            // 4) allocate a buffer the size of the screen, wrap it as a framebuffer, show it
            let (w, h) = (mode.hdisplay as u32, mode.vdisplay as u32);
            let mut cd = CreateDumb { width: w, height: h, bpp: 32, ..Default::default() };
            ioctl_ptr(fd, CREATE_DUMB, &mut cd)?;
            let mut fb = FbCmd { width: w, height: h, pitch: cd.pitch, bpp: 32, depth: 24, handle: cd.handle, ..Default::default() };
            ioctl_ptr(fd, ADDFB, &mut fb)?;
            let mut md = MapDumb { handle: cd.handle, ..Default::default() };
            ioctl_ptr(fd, MAP_DUMB, &mut md)?;
            let map = map_shared(fd, md.offset, cd.size as usize)?;
            let mut set = Crtc { set_conn_ptr: &cid as *const u32 as u64, n_conn: 1, crtc_id, fb_id: fb.fb_id, mode_valid: 1, mode, ..Default::default() };
            ioctl_ptr(fd, SETCRTC, &mut set)?;
            return Ok(Display { _file: file, map, width: w as usize, height: h as usize, stride_px: cd.pitch as usize / 4, kind: "drm" });
        }
        Err(err("no connected display found"))
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn ioctl_numbers_match_the_kernel_headers() {
            assert_eq!(GETRESOURCES, 0xC04064A0);
            assert_eq!(GETCRTC, 0xC06864A1);
            assert_eq!(SETCRTC, 0xC06864A2);
            assert_eq!(GETENCODER, 0xC01464A6);
            assert_eq!(GETCONNECTOR, 0xC05064A7);
            assert_eq!(ADDFB, 0xC01C64AE);
            assert_eq!(CREATE_DUMB, 0xC02064B2);
            assert_eq!(MAP_DUMB, 0xC01064B3);
            assert_eq!(std::mem::size_of::<ModeInfo>(), 68);
        }
    }
}

mod fbdev {
    use super::*;
    const GET_VSCREENINFO: std::os::raw::c_ulong = 0x4600;
    const GET_FSCREENINFO: std::os::raw::c_ulong = 0x4602;

    pub fn open() -> io::Result<Display> {
        let file = open_rw("/dev/fb0")?;
        let fd = file.as_raw_fd();
        let mut var = [0u8; 160];
        let mut fix = [0u8; 80];
        ioctl_ptr(fd, GET_VSCREENINFO, &mut var)?;
        ioctl_ptr(fd, GET_FSCREENINFO, &mut fix)?;
        let u32at = |b: &[u8], o: usize| u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let (w, h, bpp) = (u32at(&var, 0) as usize, u32at(&var, 4) as usize, u32at(&var, 24));
        let (len, line) = (u32at(&fix, 24) as usize, u32at(&fix, 48) as usize);
        if bpp != 32 { return Err(io::Error::new(io::ErrorKind::Other, format!("framebuffer is {bpp} bpp, need 32"))); }
        let map = map_shared(fd, 0, len)?;
        Ok(Display { _file: file, map, width: w, height: h, stride_px: line / 4, kind: "fbdev" })
    }
}
