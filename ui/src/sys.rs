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
