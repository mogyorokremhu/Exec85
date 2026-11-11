//! The few C functions we need from the system's libc. Declared by hand so
//! the project needs no external crates.
use std::io;
use std::os::raw::{c_int, c_ulong, c_void};

extern "C" {
    pub fn ioctl(fd: c_int, request: c_ulong, ...) -> c_int;
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
