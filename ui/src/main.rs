//! Exec85 interface. Milestone 2: the shell (menu bar, drive row, file list, tiled windows).
mod app;
mod display;
mod font;
mod fs;
mod gfx;
mod input;
mod keys;
mod layout;
mod sys;
mod ui;

use app::{App, Effect};
use gfx::{Canvas, Metrics, Mode};
use std::fs as stdfs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::{thread, time::Duration};

/// Write a line to the system console (the serial port in our tests) and stdout.
/// The whole line goes out in ONE write so kernel messages are much less likely to land in the middle of it.
fn log(line: &str) {
    let msg = format!("{line}\n");
    print!("{msg}");
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/console") { let _ = f.write_all(msg.as_bytes()); }
}

fn uptime() -> f64 {
    stdfs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok())).unwrap_or(0.0)
}

fn kb(file: &str, key: &str) -> u64 {
    let s = stdfs::read_to_string(file).unwrap_or_default();
    s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn read_mode() -> Mode {
    match stdfs::read_to_string("/etc/exec85/theme") { Ok(s) if s.trim() == "light" => Mode::Light, _ => Mode::Dark }
}

fn apply(effect: Effect) {
    match effect {
        Effect::SaveTheme(m) => {
            let _ = stdfs::create_dir_all("/etc/exec85");
            let _ = stdfs::write("/etc/exec85/theme", if m == Mode::Light { "light\n" } else { "dark\n" });
        }
        Effect::PowerOff => { let _ = Command::new("/sbin/poweroff").spawn(); }
        Effect::Reboot => { let _ = Command::new("/sbin/reboot").spawn(); }
    }
}

/// Test helper: render the shell to an image file instead of the screen.
///   exec85 --ppm out.ppm [--light] [--size 1024x768] [--dir /etc] [--keys down,down,f10]
fn render_to_file(args: &[String]) {
    let mut mode = Mode::Dark;
    let (mut w, mut h) = (1024usize, 768usize);
    let (mut dir, mut keys, mut out) = (PathBuf::from("/"), String::new(), String::new());
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--ppm" => { i += 1; out = args[i].clone(); }
            "--light" => mode = Mode::Light,
            "--size" => { i += 1; let (a, b) = args[i].split_once('x').expect("--size WxH"); w = a.parse().unwrap(); h = b.parse().unwrap(); }
            "--dir" => { i += 1; dir = PathBuf::from(&args[i]); }
            "--keys" => { i += 1; keys = args[i].clone(); }
            other => eprintln!("ignoring {other}"),
        }
        i += 1;
    }
    let m = Metrics::for_height(h);
    let mut app = App::new(w / m.cw, h / m.ch, mode, dir);
    for name in keys.split(',').filter(|s| !s.is_empty()) {
        match keys::parse_key(name) { Some(k) => { app.handle(k); } None => eprintln!("unknown key {name}") }
    }
    let mut c = Canvas::new(w, h);
    ui::draw(&app, &mut c, &m);
    stdfs::write(&out, c.to_ppm()).expect("write image");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--version") { println!("exec85 {}", env!("CARGO_PKG_VERSION")); return; }
    if args.get(1).map(String::as_str) == Some("--ppm") { render_to_file(&args[1..]); return; }

    // Stop the text console from drawing over us.
    if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty0") {
        use std::os::fd::AsRawFd;
        // KDSETMODE = 0x4B3A, KD_GRAPHICS = 1
        unsafe { sys::ioctl(tty.as_raw_fd(), 0x4B3A, 1 as std::os::raw::c_ulong) };
    }

    let display = match display::Display::open() {
        Ok(d) => d,
        Err(e) => { log(&format!("EXEC85_ERROR no display: {e}")); thread::sleep(Duration::from_secs(5)); std::process::exit(1); }
    };
    let m = Metrics::for_height(display.height);
    let mut app = App::new(display.width / m.cw, display.height / m.ch, read_mode(), PathBuf::from("/"));
    let mut canvas = Canvas::new(display.width, display.height);
    let keys_rx = input::spawn();

    ui::draw(&app, &mut canvas, &m);
    display.present(&canvas);
    log(&format!("EXEC85_READY uptime={:.2}", uptime()));
    log(&format!("EXEC85_DISPLAY {} {}x{}", display.kind, display.width, display.height));
    log(&app.state_line());

    // Idle memory report, 15 seconds after the first screen (MemTotal - MemAvailable).
    thread::spawn(|| {
        thread::sleep(Duration::from_secs(15));
        let (total, avail) = (kb("/proc/meminfo", "MemTotal:"), kb("/proc/meminfo", "MemAvailable:"));
        log(&format!("EXEC85_MEM total_kb={total} used_kb={} exec85_rss_kb={} uptime={:.2}", total.saturating_sub(avail), kb("/proc/self/status", "VmRSS:"), uptime()));
    });

    // Sleep until a key arrives (no polling), handle every queued key, then redraw once.
    loop {
        let first = match keys_rx.recv_timeout(Duration::from_secs(3600)) {
            Ok(k) => k,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => loop { thread::sleep(Duration::from_secs(3600)); },
        };
        let mut effects = Vec::new();
        effects.extend(app.handle(first));
        while let Ok(k) = keys_rx.try_recv() { effects.extend(app.handle(k)); }
        ui::draw(&app, &mut canvas, &m);
        display.present(&canvas);
        log(&app.state_line());
        for e in effects { apply(e); }
    }
}
