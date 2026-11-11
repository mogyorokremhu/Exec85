//! Exec85 interface. Milestone 1: show an empty themed screen and report
//! boot time and idle memory on the console so we can measure them.
mod display;
mod gfx;
mod sys;

use gfx::{Canvas, Mode, Theme};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::{thread, time::Duration};

/// Write a line to the system console (the serial port in our tests) and stdout.
fn log(line: &str) {
    println!("{line}");
    if let Ok(mut f) = OpenOptions::new().write(true).open("/dev/console") { let _ = writeln!(f, "{line}"); }
}

fn uptime() -> f64 {
    fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok())).unwrap_or(0.0)
}

fn meminfo(key: &str) -> u64 {
    let s = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn rss_kb() -> u64 {
    let s = fs::read_to_string("/proc/self/status").unwrap_or_default();
    s.lines().find(|l| l.starts_with("VmRSS:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn read_mode() -> Mode {
    match fs::read_to_string("/etc/exec85/theme") { Ok(s) if s.trim() == "light" => Mode::Light, _ => Mode::Dark }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") { println!("exec85 {}", env!("CARGO_PKG_VERSION")); return; }

    // Test helper: render to a PPM image file instead of the screen.  exec85 --ppm out.ppm [--light]
    if args.get(1).map(String::as_str) == Some("--ppm") {
        let mode = if args.iter().any(|a| a == "--light") { Mode::Light } else { Mode::Dark };
        let mut c = Canvas::new(800, 600);
        c.draw_blank(&Theme::new(mode));
        fs::write(args.get(2).expect("usage: exec85 --ppm FILE"), c.to_ppm()).expect("write ppm");
        return;
    }

    // Stop the text console from drawing over us.
    if let Ok(tty) = OpenOptions::new().write(true).open("/dev/tty0") {
        use std::os::fd::AsRawFd;
        // KDSETMODE = 0x4B3A, KD_GRAPHICS = 1
        unsafe { sys::ioctl(tty.as_raw_fd(), 0x4B3A, 1 as std::os::raw::c_ulong) };
    }

    let display = match display::Display::open() {
        Ok(d) => d,
        Err(e) => { log(&format!("EXEC85_ERROR no display: {e}")); thread::sleep(Duration::from_secs(5)); std::process::exit(1); }
    };
    let mut canvas = Canvas::new(display.width, display.height);
    canvas.draw_blank(&Theme::new(read_mode()));
    display.present(&canvas);
    log(&format!("EXEC85_READY uptime={:.2} display={} {}x{}", uptime(), display.kind, display.width, display.height));

    // Let the system settle, then report idle memory use (MemTotal - MemAvailable).
    thread::sleep(Duration::from_secs(15));
    let (total, avail) = (meminfo("MemTotal:"), meminfo("MemAvailable:"));
    log(&format!("EXEC85_MEM total_kb={total} used_kb={} exec85_rss_kb={} uptime={:.2}", total.saturating_sub(avail), rss_kb(), uptime()));

    loop { thread::sleep(Duration::from_secs(3600)); } // later milestones: keyboard loop goes here
}
