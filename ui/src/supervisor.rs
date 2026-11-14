//! The supervisor: the tiny root process init starts on the first console.
//!
//! It never draws anything. It runs one "stage" at a time as a separate process, so each stage
//! gets the screen and keyboard to itself and the shell never runs as root:
//!   live disc:        installer  ->  (restart)            or   try the shell as root
//!   installed system: login      ->  shell as that user   ->   back to login
//! A stage tells the supervisor what to do next through its exit code.
use crate::account::{self, User};
use crate::sys;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::{thread, time::Duration};

pub const EXIT_LOGOUT: i32 = 0;
pub const EXIT_POWEROFF: i32 = 10;
pub const EXIT_REBOOT: i32 = 11;
pub const EXIT_TRY_LIVE: i32 = 12;
/// TESTING-ONLY: the installer's "Skip installation" finished setting up in memory; go on to the login screen.
pub const EXIT_SETUP_DONE: i32 = 13;
pub const EXIT_LOGIN_OK: i32 = 20;

pub struct Stage { pub code: i32, pub login_user: Option<String> }

/// Write one line to the system console (the serial port in our tests), in a single write.
pub fn console_line(line: &str) {
    if let Ok(mut f) = std::fs::OpenOptions::new().write(true).open("/dev/console") { let _ = f.write_all(format!("{line}\n").as_bytes()); }
}

pub fn cmdline_has(word: &str) -> bool {
    std::fs::read_to_string("/proc/cmdline").map_or(false, |s| s.split_whitespace().any(|w| w == word))
}

/// Run `exe arg` as a child; its output lines are forwarded to the console. With `as_user`, the child
/// drops root privileges (groups, group, then user id) before it starts.
pub fn stage(exe: &Path, arg: &str, as_user: Option<&User>, start_dir: Option<&str>) -> Stage {
    let mut cmd = Command::new(exe);
    cmd.arg(arg).stdout(Stdio::piped()).env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").env("TERM", "linux").env("EXEC85_SUPERVISED", "1");
    match as_user {
        Some(u) => {
            cmd.env("HOME", &u.home).env("USER", &u.name).env("LOGNAME", &u.name).env("SHELL", &u.shell);
            if Path::new(&u.home).is_dir() { let _ = crate::homedirs::ensure(Path::new(&u.home), u.uid, u.gid); cmd.current_dir(&u.home); }
            if start_dir.is_none() { cmd.env("EXEC85_START", &u.home); }
            let (uid, gid) = (u.uid, u.gid);
            let groups = account::group_ids(Path::new("/"), u);
            // SAFETY: runs between fork and exec and calls only system calls.
            unsafe {
                cmd.pre_exec(move || {
                    if sys::setgroups(groups.len(), groups.as_ptr()) != 0 || sys::setgid(gid) != 0 || sys::setuid(uid) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        None => { cmd.env("HOME", "/root").env("USER", "root").env("LOGNAME", "root").env("SHELL", "/bin/ash"); }
    }
    if let Some(d) = start_dir { cmd.env("EXEC85_START", d); }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => { console_line(&format!("EXEC85_SUPERVISOR cannot start {arg}: {e}")); return Stage { code: -1, login_user: None }; }
    };
    let login_user = Arc::new(Mutex::new(None::<String>));
    let reader = child.stdout.take().map(|out| {
        let lu = login_user.clone();
        thread::spawn(move || {
            for line in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(name) = line.strip_prefix("EXEC85_LOGIN_OK ") { *lu.lock().unwrap() = Some(name.trim().to_string()); }
                console_line(&line);
            }
        })
    });
    let code = child.wait().ok().and_then(|s| s.code()).unwrap_or(-1);
    if let Some(r) = reader { let _ = r.join(); }
    let name = login_user.lock().unwrap().take();
    Stage { code, login_user: name }
}

fn power(code: i32) -> ! {
    let cmd = if code == EXIT_REBOOT { "/sbin/reboot" } else { "/sbin/poweroff" };
    console_line(&format!("EXEC85_SUPERVISOR {}", if code == EXIT_REBOOT { "rebooting" } else { "powering off" }));
    let _ = Command::new(cmd).status();
    loop { thread::sleep(Duration::from_secs(3600)); }
}

fn exe_path() -> PathBuf { std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/exec85")) }

/// What to do after a stage finished: power commands never return.
fn after(code: i32) {
    match code { EXIT_POWEROFF | EXIT_REBOOT => power(code), _ => {} }
}

/// The login -> shell loop (installed systems, and the testing build after "Skip installation").
fn login_cycle(exe: &Path) {
    let s = stage(exe, "--login", None, None);
    after(s.code);
    if s.code == EXIT_LOGIN_OK {
        let user = s.login_user.as_deref().and_then(|n| account::users(Path::new("/")).into_iter().find(|u| u.name == n));
        match user {
            Some(u) => { let sh = stage(exe, "--shell", Some(&u), None); after(sh.code); }
            None => { console_line("EXEC85_SUPERVISOR login succeeded but the user vanished"); thread::sleep(Duration::from_secs(2)); }
        }
    } else { thread::sleep(Duration::from_secs(2)); }
}

/// The privileged helper runs for as long as the system does; if it ever dies it is started again.
fn keep_helper_running(exe: PathBuf) {
    thread::spawn(move || loop {
        let _ = Command::new(&exe).arg("--helperd").stdin(Stdio::null()).status();
        thread::sleep(Duration::from_secs(1));
    });
}

/// Stop the boot screen (if it is showing) so the first real screen can have the display to itself.
pub fn stop_splash() {
    let Some(pid) = std::fs::read_to_string("/run/exec85-splash.pid").ok().and_then(|s| s.trim().parse::<i32>().ok()) else { return };
    let _ = sys::send_signal(pid, 15);
    for _ in 0..40 {
        if !Path::new(&format!("/proc/{pid}")).exists() { break; }
        thread::sleep(Duration::from_millis(25));
    }
    if Path::new(&format!("/proc/{pid}")).exists() { let _ = sys::send_signal(pid, 9); }
    let _ = std::fs::remove_file("/run/exec85-splash.pid");
}

pub fn run() -> ! {
    stop_splash();
    let exe = exe_path();
    keep_helper_running(exe.clone());
    let live = cmdline_has("exec85.live");
    console_line(&format!("EXEC85_SUPERVISOR start mode={}", if live { "live" } else { "installed" }));
    if !live { loop { login_cycle(&exe); } }
    loop {
        let s = stage(&exe, "--installer", None, None);
        after(s.code);
        if s.code == EXIT_SETUP_DONE { loop { login_cycle(&exe); } } // TESTING-ONLY
        if s.code == EXIT_TRY_LIVE {
            let sh = stage(&exe, "--shell", None, Some("/"));
            after(sh.code);
        } else if s.code != EXIT_LOGOUT { thread::sleep(Duration::from_secs(2)); }
    }
}
