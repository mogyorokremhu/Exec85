//! The few C functions we need from the system's libc. Declared by hand so
//! the project needs no external crates.
use std::ffi::CString;
use std::io;
use std::os::raw::{c_char, c_int, c_ulong, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

extern "C" {
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
    pub fn setgroups(n: usize, list: *const u32) -> c_int;
    pub fn setgid(gid: u32) -> c_int;
    fn localtime_r(t: *const i64, out: *mut Tm) -> *mut Tm;
    fn time(t: *mut i64) -> i64;
    pub fn kill(pid: i32, sig: c_int) -> c_int;
    fn posix_openpt(flags: c_int) -> c_int;
    fn grantpt(fd: c_int) -> c_int;
    fn unlockpt(fd: c_int) -> c_int;
    fn ptsname_r(fd: c_int, buf: *mut c_char, len: usize) -> c_int;
    pub fn setsid() -> i32;
    fn getuid() -> u32;
    pub fn setuid(uid: u32) -> c_int;
    pub fn statvfs(path: *const c_char, buf: *mut c_void) -> c_int;
    pub fn mmap(addr: *mut c_void, len: usize, prot: c_int, flags: c_int, fd: c_int, off: i64) -> *mut c_void;
}

pub const PROT_READ: c_int = 1;
pub const PROT_WRITE: c_int = 2;
pub const MAP_SHARED: c_int = 1;

/// Call an ioctl that takes a pointer to a struct (the kernel reads and/or fills it).
pub fn ioctl_ptr<T>(fd: c_int, request: c_ulong, arg: &mut T) -> io::Result<()> {
    // SAFETY: `arg` is a valid, exclusive reference for the whole call.
    let r = unsafe { ioctl(fd, request, arg as *mut T) };
    if r < 0 { Err(io::Error::last_os_error()) } else { Ok(()) }
}

/// Map `len` bytes of `fd` (starting at `offset`) read/write into our memory.
pub fn map_shared(fd: c_int, offset: u64, len: usize) -> io::Result<*mut u32> {
    // SAFETY: plain mmap; result is checked before use.
    let p = unsafe { mmap(std::ptr::null_mut(), len, PROT_READ | PROT_WRITE, MAP_SHARED, fd, offset as i64) };
    if p as isize == -1 { Err(io::Error::last_os_error()) } else { Ok(p as *mut u32) }
}

/// (total bytes, free bytes) of the filesystem holding `path`.
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // struct statvfs is far smaller than this buffer; on x86_64 the fields we use are
    // f_frsize (index 1), f_blocks (2) and f_bavail (4), all 8 bytes.
    let mut buf = [0u64; 32];
    // SAFETY: valid NUL-terminated path and a buffer larger than struct statvfs.
    let r = unsafe { statvfs(c.as_ptr(), buf.as_mut_ptr() as *mut c_void) };
    if r != 0 { return None; }
    Some((buf[2].saturating_mul(buf[1]), buf[4].saturating_mul(buf[1])))
}

/// The user id this process runs as (0 = root).
pub fn uid() -> u32 {
    // SAFETY: getuid has no failure modes and touches no memory.
    unsafe { getuid() }
}

/// C `struct tm` (x86_64 Linux layout).
#[repr(C)]
struct Tm { sec: i32, min: i32, hour: i32, mday: i32, mon: i32, year: i32, wday: i32, yday: i32, isdst: i32, gmtoff: i64, zone: *const c_char }

/// A moment in the computer's local time zone (/etc/localtime).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local { pub year: i32, pub month: u32, pub day: u32, pub hour: u32, pub min: u32, pub sec: u32 }

/// Seconds since 1970 right now.
pub fn now() -> i64 {
    // SAFETY: time(NULL) just returns the clock.
    unsafe { time(std::ptr::null_mut()) }
}

pub fn local_time(secs: i64) -> Local {
    let mut tm = Tm { sec: 0, min: 0, hour: 0, mday: 1, mon: 0, year: 70, wday: 0, yday: 0, isdst: 0, gmtoff: 0, zone: std::ptr::null() };
    // SAFETY: valid pointers to a properly laid-out struct tm.
    unsafe { localtime_r(&secs, &mut tm) };
    Local { year: tm.year + 1900, month: (tm.mon + 1) as u32, day: tm.mday as u32, hour: tm.hour as u32, min: tm.min as u32, sec: tm.sec as u32 }
}

/// Send a signal to a process (15 = ask it to stop, 9 = force it).
pub fn send_signal(pid: i32, sig: i32) -> std::io::Result<()> {
    // SAFETY: kill only takes numbers.
    if unsafe { kill(pid, sig) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error()) }
}

// ---- pseudo-terminals (what a terminal window talks to its shell through) ------------------------------

pub const TIOCSWINSZ: c_ulong = 0x5414;
pub const TIOCSCTTY: c_ulong = 0x540E;

#[repr(C)]
struct WinSize { rows: u16, cols: u16, xpixel: u16, ypixel: u16 }

/// Open a new pseudo-terminal. Returns (master, slave): the window reads and writes the master; the
/// program running inside uses the slave as its terminal.
pub fn open_pty() -> io::Result<(std::fs::File, std::fs::File)> {
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    const O_RDWR: c_int = 2;
    const O_NOCTTY: c_int = 0o400;
    // SAFETY: plain libc calls; the fd is checked before it is wrapped.
    unsafe {
        let fd = posix_openpt(O_RDWR | O_NOCTTY);
        if fd < 0 { return Err(io::Error::last_os_error()); }
        let master = std::fs::File::from_raw_fd(fd);
        if grantpt(fd) != 0 || unlockpt(fd) != 0 { return Err(io::Error::last_os_error()); }
        let mut buf = [0 as c_char; 128];
        if ptsname_r(fd, buf.as_mut_ptr(), buf.len()) != 0 { return Err(io::Error::last_os_error()); }
        let name = std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned();
        let slave = std::fs::OpenOptions::new().read(true).write(true).custom_flags(O_NOCTTY).open(name)?;
        Ok((master, slave))
    }
}

/// Tell the program inside a terminal how big its window is.
pub fn set_winsize(fd: c_int, cols: usize, rows: usize) {
    let mut ws = WinSize { rows: rows.min(65535) as u16, cols: cols.min(65535) as u16, xpixel: 0, ypixel: 0 };
    let _ = ioctl_ptr(fd, TIOCSWINSZ, &mut ws);
}
