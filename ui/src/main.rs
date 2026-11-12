//! Exec85. One small program with several jobs, chosen by its first argument:
//!   (none)       the supervisor: init starts this; it runs the stages below, one at a time
//!   --installer  the installer wizard (live disc)
//!   --login      the login screen (installed system)
//!   --shell      the file-browser shell
//!   --ppm FILE   draw the shell into an image file (testing)
mod account;
mod app;
mod apps;
mod cmds;
mod crypt;
mod disks;
mod display;
mod fileops;
mod font;
mod fs;
mod gfx;
mod input;
mod install;
mod installer;
mod keys;
mod layout;
mod login;
mod mc;
mod paint;
mod shell;
mod supervisor;
mod helper;
mod services;
mod sys;
mod usb;
mod ui;

use app::{App, Effect};
use gfx::{Canvas, Metrics, Mode};
use keys::Key;
use std::fs as stdfs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::{thread, time::Duration};

fn supervised() -> bool { std::env::var_os("EXEC85_SUPERVISED").is_some() }

/// Log a line. Under the supervisor, stdout is forwarded to the console; otherwise write there directly.
fn log(line: &str) {
    println!("{line}");
    if !supervised() { supervisor::console_line(line); }
}

fn uptime() -> f64 {
    stdfs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|v| v.parse().ok())).unwrap_or(0.0)
}

fn kb(file: &str, key: &str) -> u64 {
    let s = stdfs::read_to_string(file).unwrap_or_default();
    s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
}

fn hostname() -> String { stdfs::read_to_string("/proc/sys/kernel/hostname").map(|s| s.trim().to_string()).unwrap_or_default() }

fn parse_mode(s: &str) -> Option<Mode> {
    match s.trim() { "light" => Some(Mode::Light), "dark" => Some(Mode::Dark), _ => None }
}

/// The user's own choice (~/.config/exec85/theme) wins over the system default (/etc/exec85/theme).
fn read_mode() -> Mode {
    if let Some(home) = std::env::var_os("HOME") {
        if let Some(m) = stdfs::read_to_string(Path::new(&home).join(".config/exec85/theme")).ok().and_then(|s| parse_mode(&s)) { return m; }
    }
    stdfs::read_to_string("/etc/exec85/theme").ok().and_then(|s| parse_mode(&s)).unwrap_or(Mode::Dark)
}

fn system_mode() -> Mode { stdfs::read_to_string("/etc/exec85/theme").ok().and_then(|s| parse_mode(&s)).unwrap_or(Mode::Dark) }

fn load_layout() -> keys::Layout {
    let l = stdfs::read_to_string("/etc/exec85/keyboard").ok().and_then(|s| keys::Layout::from_id(&s)).unwrap_or(keys::Layout::Us);
    keys::set_layout(l);
    l
}

fn save_theme(m: Mode) {
    let text = if m == Mode::Light { "light\n" } else { "dark\n" };
    let path = match std::env::var_os("HOME") {
        Some(h) => Path::new(&h).join(".config/exec85/theme"),
        None => PathBuf::from("/etc/exec85/theme"),
    };
    if let Some(d) = path.parent() { let _ = stdfs::create_dir_all(d); }
    let _ = stdfs::write(path, text);
}

fn sleep_path() -> Option<PathBuf> { std::env::var_os("HOME").map(|h| Path::new(&h).join(".config/exec85/sleep")) }

/// Minutes without a key before the screen turns off by itself (0 = never).
fn read_sleep() -> u32 { sleep_path().and_then(|p| stdfs::read_to_string(p).ok()).and_then(|s| s.trim().parse().ok()).filter(|m| [0, 5, 15, 60].contains(m)).unwrap_or(0) }

fn save_sleep(minutes: u32) {
    let Some(path) = sleep_path() else { return };
    if let Some(d) = path.parent() { let _ = stdfs::create_dir_all(d); }
    let _ = stdfs::write(path, format!("{minutes}\n"));
}

struct Screen { display: display::Display, m: Metrics, canvas: Canvas }

impl Screen {
    fn open() -> Screen {
        // Stop the text console from drawing over us (needs permission; fine if it is refused).
        if let Ok(tty) = std::fs::OpenOptions::new().write(true).open("/dev/tty0") {
            use std::os::fd::AsRawFd;
            // KDSETMODE = 0x4B3A, KD_GRAPHICS = 1
            unsafe { sys::ioctl(tty.as_raw_fd(), 0x4B3A, 1 as std::os::raw::c_ulong) };
        }
        match display::Display::open() {
            Ok(display) => {
                let m = Metrics::for_height(display.height);
                let canvas = Canvas::new(display.width, display.height);
                Screen { display, m, canvas }
            }
            Err(e) => { log(&format!("EXEC85_ERROR no display: {e}")); thread::sleep(Duration::from_secs(3)); std::process::exit(1); }
        }
    }
    fn cells(&self) -> (usize, usize) { (self.display.width / self.m.cw, self.display.height / self.m.ch) }
    fn show(&self) { self.display.present(&self.canvas); }
}

/// Tell the outside world (and our tests) that a screen is up. The first stage of a boot also
/// reports idle memory 15 seconds later.
fn announce(name: &str, s: &Screen) {
    log(&format!("EXEC85_READY uptime={:.2}", uptime()));
    log(&format!("EXEC85_DISPLAY {} {}x{}", s.display.kind, s.display.width, s.display.height));
    log(&format!("EXEC85_SCREEN {name}"));
    let first = std::fs::OpenOptions::new().write(true).create_new(true).open("/run/exec85-mem-reported").is_ok();
    if first {
        thread::spawn(|| {
            thread::sleep(Duration::from_secs(15));
            let (total, avail) = (kb("/proc/meminfo", "MemTotal:"), kb("/proc/meminfo", "MemAvailable:"));
            log(&format!("EXEC85_MEM total_kb={total} used_kb={} exec85_rss_kb={} uptime={:.2}", total.saturating_sub(avail), kb("/proc/self/status", "VmRSS:"), uptime()));
        });
    }
}

// ---- stages -----------------------------------------------------------------------------------------

fn stage_login() -> ! {
    let layout = load_layout();
    let mut s = Screen::open();
    let (cols, rows) = s.cells();
    let root = Path::new("/");
    let regular = account::regular_users(root);
    let default_user = if regular.len() == 1 { Some(regular[0].name.clone()) } else { None };
    let mut lg = login::Login::new(cols, rows, system_mode(), layout, hostname(), default_user);
    let rx = input::spawn();
    let auth = |n: &str, p: &str| account::authenticate(root, n, p);

    login::draw(&lg, &mut s.canvas, &s.m);
    s.show();
    announce("login", &s);
    log(&lg.state_line());
    loop {
        let first = match rx.recv_timeout(Duration::from_secs(3600)) {
            Ok(k) => k,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => loop { thread::sleep(Duration::from_secs(3600)); },
        };
        let mut keys_now = vec![first];
        while let Ok(k) = rx.try_recv() { keys_now.push(k); }
        for k in keys_now {
            match lg.handle(k, &auth) {
                Some(login::Outcome::LoggedIn(u)) => { log(&format!("EXEC85_LOGIN_OK {}", u.name)); std::process::exit(supervisor::EXIT_LOGIN_OK); }
                Some(login::Outcome::Restart) => std::process::exit(supervisor::EXIT_REBOOT),
                Some(login::Outcome::ShutDown) => std::process::exit(supervisor::EXIT_POWEROFF),
                None => {}
            }
        }
        login::draw(&lg, &mut s.canvas, &s.m);
        s.show();
        log(&lg.state_line());
        if let Some(d) = lg.penalty.take() { thread::sleep(d); while rx.try_recv().is_ok() {} } // slow down password guessing
    }
}

enum Event { Key(Key), Wake }

fn stage_installer() -> ! {
    load_layout();
    let mut s = Screen::open();
    let (cols, rows) = s.cells();
    let (tx, rx) = channel::<Event>();
    let key_tx = tx.clone();
    input::spawn_with(Arc::new(move |k| key_tx.send(Event::Key(k)).is_ok()));
    let run_tx = tx.clone();
    let runner: Box<dyn Fn(install::Config, install::Shared)> = Box::new(move |cfg, shared| {
        let tx = run_tx.clone();
        thread::spawn(move || install::run_to_shared(cfg, install::Paths::live(), shared, move || { let _ = tx.send(Event::Wake); }));
    });
    // TESTING-ONLY: the testing build is started with "exec85.testing" on the kernel command line.
    let testing = supervisor::cmdline_has("exec85.testing");
    let mut inst = installer::Installer::new(cols, rows, system_mode(), installer::load_timezones(Path::new("/usr/share/zoneinfo")), testing, Box::new(disks::system_disks), runner);

    installer::draw(&inst, &mut s.canvas, &s.m);
    s.show();
    announce("installer", &s);
    log(&inst.state_line());
    loop {
        let ev = match rx.recv_timeout(Duration::from_secs(3600)) { Ok(e) => e, Err(RecvTimeoutError::Timeout) => continue, Err(RecvTimeoutError::Disconnected) => loop { thread::sleep(Duration::from_secs(3600)); } };
        let mut events = vec![ev];
        while let Ok(e) = rx.try_recv() { events.push(e); }
        for e in events {
            match e {
                Event::Key(k) => match inst.handle(k) {
                    Some(installer::Outcome::Reboot) => std::process::exit(supervisor::EXIT_REBOOT),
                    Some(installer::Outcome::Live) => std::process::exit(supervisor::EXIT_TRY_LIVE),
                    Some(installer::Outcome::SetupDone) => std::process::exit(supervisor::EXIT_SETUP_DONE), // TESTING-ONLY
                    None => {}
                },
                Event::Wake => inst.poll(),
            }
        }
        keys::set_layout(inst.layout);
        installer::draw(&inst, &mut s.canvas, &s.m);
        s.show();
        log(&inst.state_line());
    }
}

fn stage_shell() -> ! {
    load_layout();
    let mut s = Screen::open();
    let (cols, rows) = s.cells();
    let start = std::env::var("EXEC85_START").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("/"));
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_else(|_| start.clone());
    let user = std::env::var("USER").unwrap_or_else(|_| "?".into());
    let extra = format!(" user={user} uid={} host={}", sys::uid(), hostname());

    // One channel carries key presses and "something changed" wake-ups from app threads (e.g. terminal output).
    let (tx, rx) = channel::<Event>();
    let key_tx = tx.clone();
    input::spawn_with(Arc::new(move |k| key_tx.send(Event::Key(k)).is_ok()));
    let wake_tx = tx.clone();
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || { let _ = wake_tx.send(Event::Wake); });
    let mut shell = shell::Shell::new(cols, rows, read_mode(), start, home, wake);
    shell.app.sleep_minutes = read_sleep();

    ui::draw(&shell.app, &shell.windows, &mut s.canvas, &s.m);
    s.show();
    announce("shell", &s);
    for l in shell.state_lines(&extra) { log(&l); }
    loop {
        // Sleep until a key arrives or an app needs its next timer tick; with no timers this sleeps for good.
        let timeout = shell.next_deadline().unwrap_or(Duration::from_secs(3600));
        let mut effects = Vec::new();
        let mut from_key = false;
        match rx.recv_timeout(timeout) {
            Ok(Event::Key(k)) => { effects.extend(shell.handle(k)); from_key = true; }
            Ok(Event::Wake) | Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => loop { thread::sleep(Duration::from_secs(3600)); },
        }
        while let Ok(e) = rx.try_recv() { if let Event::Key(k) = e { effects.extend(shell.handle(k)); from_key = true; } }
        shell.tick();
        effects.extend(shell.take_effects());
        // While asleep the screen shows nothing at all (and the display is switched off below).
        if shell.app.asleep { let (w, h) = (s.canvas.w, s.canvas.h); s.canvas.fill_rect(0, 0, w, h, 0); } else { ui::draw(&shell.app, &shell.windows, &mut s.canvas, &s.m); }
        s.show();
        let lines = if from_key { shell.state_lines(&extra) } else { shell.changed_state_lines(&extra) };
        for l in lines { log(&l); }
        for e in effects {
            match e {
                Effect::SaveTheme(m) => save_theme(m),
                Effect::SaveSleep(m) => save_sleep(m),
                Effect::ScreenPower(on) => { let _ = helper::call(&["screen-power", if on { "on" } else { "off" }]); }
                Effect::Logout => std::process::exit(supervisor::EXIT_LOGOUT),
                Effect::PowerOff => std::process::exit(supervisor::EXIT_POWEROFF),
                Effect::Reboot => std::process::exit(supervisor::EXIT_REBOOT),
                _ => {}
            }
        }
    }
}

/// Test helper: render the shell to an image file instead of the screen.
///   exec85 --ppm out.ppm [--light] [--size 1024x768] [--dir /etc] [--keys down,down,f10]
///          [--screen installer|login]   (installer and login use made-up disks and users)
fn render_to_file(args: &[String]) {
    let mut mode = Mode::Dark;
    let mut screen = String::from("shell");
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
            "--screen" => { i += 1; screen = args[i].clone(); }
            other => eprintln!("ignoring {other}"),
        }
        i += 1;
    }
    let m = Metrics::for_height(h);
    let (cols, rows) = (w / m.cw, h / m.ch);
    let mut c = Canvas::new(w, h);
    let parsed: Vec<Key> = keys.split(',').filter(|s| !s.is_empty()).filter_map(|n| { let k = keys::parse_key(n); if k.is_none() { eprintln!("unknown key {n}"); } k }).collect();
    match screen.as_str() {
        "installer" => {
            let disks = || vec![
                disks::Disk { name: "nvme0n1".into(), bytes: 512_110_190_592, model: "Samsung SSD 970".into(), removable: false },
                disks::Disk { name: "sda".into(), bytes: 4_000_787_030_016, model: "ATA ST4000LM024".into(), removable: false },
            ];
            let tzs = ["UTC", "Africa/Cairo", "America/New_York", "Asia/Tokyo", "Europe/Berlin", "Europe/Budapest", "Europe/London"].iter().map(|s| s.to_string()).collect();
            let testing = args.iter().any(|a| a == "--testing");
            let mut inst = installer::Installer::new(cols, rows, mode, tzs, testing, Box::new(disks), Box::new(|_, _| {}));
            for k in parsed { inst.handle(k); }
            installer::draw(&inst, &mut c, &m);
        }
        "login" => {
            let mut lg = login::Login::new(cols, rows, mode, keys::Layout::Us, "exec85".into(), Some("tester".into()));
            let deny = |_: &str, _: &str| Err(account::AuthError::BadPassword);
            for k in parsed { lg.handle(k, &deny); }
            login::draw(&lg, &mut c, &m);
        }
        _ => {
            let mut shell = shell::Shell::new(cols, rows, mode, dir, std::env::temp_dir(), Arc::new(|| {}));
            for k in parsed { shell.handle(k); }
            shell.sync_sizes();
            ui::draw(&shell.app, &shell.windows, &mut c, &m);
        }
    }
    stdfs::write(&out, c.to_ppm()).expect("write image");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    // Installed under another name (dir, disk, about, get ...), the program acts as that command.
    if let Some(name) = args.first().and_then(|a| Path::new(a).file_name()).map(|n| n.to_string_lossy().into_owned()) {
        if let Some(code) = cmds::run(&name, &args[1..]) { std::process::exit(code); }
        // `svc` and `pkg` are short forms of `exec85 --svc` and `exec85 --pkg`.
        if name == "svc" || name == "pkg" || name == "mc" {
            let mut a = vec![format!("--{name}")];
            a.extend(args[1..].iter().cloned());
            std::process::exit(helper::cli(&a));
        }
    }
    if args.get(1).map(String::as_str) == Some("--cmd") {
        if let Some(code) = args.get(2).and_then(|n| cmds::run(n, &args[3..])) { std::process::exit(code); }
        eprintln!("unknown command; try: exec85 --cmd help"); std::process::exit(2);
    }
    match args.get(1).map(String::as_str) {
        Some("--version") => println!("exec85 {}", env!("CARGO_PKG_VERSION")),
        Some("--ppm") => render_to_file(&args[1..]),
        Some("--installer") => stage_installer(),
        Some("--login") => stage_login(),
        Some("--shell") => stage_shell(),
        Some("--helperd") => helper::serve(),
        Some("--svc") | Some("--pkg") | Some("--usb") | Some("--mc") => std::process::exit(helper::cli(&args[1..])),
        None => supervisor::run(),
        Some(other) => { eprintln!("unknown option {other}; try --version"); std::process::exit(2); }
    }
}
