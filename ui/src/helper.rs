//! The privileged helper. The shell and its apps run as a normal user and cannot install services or
//! packages. Instead they send short requests to this small root daemon over a local socket. It
//! (1) checks who is asking (root or a member of the `wheel` group), (2) accepts only a fixed list of
//! actions with strictly checked arguments, and (3) never runs a command it was handed as text.
use crate::account;
use crate::services::{self, ServiceDef, Status};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SOCK_DIR: &str = "/run/exec85";
pub const SOCK: &str = "/run/exec85/helper.sock";
const MAX_OUT: usize = 200_000;

// ---- wire format: one request line, one answer line; words are percent-escaped -------------------------------

pub fn esc(s: &str) -> String {
    s.chars().map(|c| match c { '%' => "%25".to_string(), ' ' => "%20".into(), '\n' => "%0A".into(), '\r' => "%0D".into(), '\t' => "%09".into(), c => c.to_string() }).collect()
}

pub fn unesc(s: &str) -> String {
    let b = s.as_bytes();
    let (mut out, mut i) = (Vec::with_capacity(b.len()), 0);
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() && s.is_char_boundary(i + 1) && s.is_char_boundary(i + 3) {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) { out.push(v); i += 3; continue; }
        }
        out.push(b[i]); i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn encode_request(words: &[&str]) -> String { words.iter().map(|w| esc(w)).collect::<Vec<_>>().join(" ") }
pub fn decode_request(line: &str) -> Vec<String> { line.split_whitespace().map(unesc).collect() }

// ---- running commands (replaceable in tests) -------------------------------------------------------------------

pub trait Runner { fn run(&self, cmd: &str, args: &[&str]) -> (i32, String); }

pub struct SysRunner;
impl Runner for SysRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> (i32, String) {
        match std::process::Command::new(cmd).args(args).env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").stdin(std::process::Stdio::null()).output() {
            Ok(o) => { let mut s = String::from_utf8_lossy(&o.stdout).into_owned(); s.push_str(&String::from_utf8_lossy(&o.stderr)); (o.status.code().unwrap_or(-1), s) }
            Err(e) => (-1, format!("cannot run {cmd}: {e}")),
        }
    }
}

pub struct Ctx<'a> { pub root: &'a Path, pub runner: &'a dyn Runner }

// ---- who may ask ---------------------------------------------------------------------------------------------------

pub fn authorized(root: &Path, uid: u32) -> bool {
    if uid == 0 { return true; }
    let Some(u) = account::users(root).into_iter().find(|u| u.uid == uid) else { return false };
    account::parse_group(&std::fs::read_to_string(root.join("etc/group")).unwrap_or_default())
        .iter().any(|g| g.name == "wheel" && (g.gid == u.gid || g.members.iter().any(|m| *m == u.name)))
}

// ---- the actions -------------------------------------------------------------------------------------------------------

pub fn valid_pkg(p: &str) -> bool {
    !p.is_empty() && p.len() <= 64 && p.starts_with(|c: char| c.is_ascii_alphanumeric()) && p.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c))
}

fn conf_dir(root: &Path) -> PathBuf { root.join("etc/exec85/services") }
fn conf_path(root: &Path, name: &str) -> PathBuf { conf_dir(root).join(format!("{name}.conf")) }
fn init_path(root: &Path, name: &str) -> PathBuf { root.join("etc/init.d").join(format!("{}{name}", services::INIT_PREFIX)) }

fn load(root: &Path, name: &str) -> Result<ServiceDef, String> {
    services::valid_name(name).map_err(String::from)?;
    let text = std::fs::read_to_string(conf_path(root, name)).map_err(|_| format!("There is no service called {name}."))?;
    services::parse_conf(&text).ok_or_else(|| "The saved service is damaged.".to_string())
}

pub fn list_names(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(conf_dir(root)).map(|d| d.flatten().filter_map(|e| e.file_name().to_string_lossy().strip_suffix(".conf").map(String::from)).collect()).unwrap_or_default();
    v.sort();
    v
}

pub fn status_of(ctx: &Ctx, d: &ServiceDef) -> Status {
    let (code, out) = ctx.runner.run("rc-service", &[&d.init_name(), "status"]);
    let s = services::parse_status(&out);
    if s == Status::Unknown && code == 0 { Status::Running } else { s }
}

fn cap(mut s: String) -> String { if s.len() > MAX_OUT { s.truncate(MAX_OUT); } s }

fn tail(path: &Path, lines: usize) -> String {
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b[b.len().saturating_sub(64 * 1024)..]).into_owned()).unwrap_or_default();
    let all: Vec<&str> = text.lines().collect();
    all[all.len().saturating_sub(lines)..].join("\n")
}

pub fn handle(ctx: &Ctx, w: &[String]) -> Result<String, String> {
    let arg = |i: usize| w.get(i).map(String::as_str).ok_or_else(|| "Missing argument.".to_string());
    match arg(0)? {
        "ping" => Ok("pong".into()),
        "svc-list" => {
            let mut out = String::new();
            for n in list_names(ctx.root) {
                if let Ok(d) = load(ctx.root, &n) {
                    let st = match status_of(ctx, &d) { Status::Running => "running", Status::Stopped => "stopped", Status::Crashed => "crashed", Status::Unknown => "unknown" };
                    out.push_str(&format!("{} {} {}\n", n, st, d.autostart));
                }
            }
            Ok(out)
        }
        "svc-get" => Ok(services::render_conf(&load(ctx.root, arg(1)?)?)),
        "svc-save" => {
            let d = services::parse_conf(arg(1)?).ok_or("That is not a service definition.")?;
            d.validate()?;
            if !account::users(ctx.root).iter().any(|u| u.name == d.user) { return Err(format!("The user {} does not exist.", d.user)); }
            let script = services::render_script(&d)?;
            std::fs::create_dir_all(conf_dir(ctx.root)).map_err(|e| e.to_string())?;
            std::fs::create_dir_all(ctx.root.join("etc/init.d")).map_err(|e| e.to_string())?;
            std::fs::write(conf_path(ctx.root, &d.name), services::render_conf(&d)).map_err(|e| e.to_string())?;
            let ip = init_path(ctx.root, &d.name);
            std::fs::write(&ip, script).map_err(|e| e.to_string())?;
            std::fs::set_permissions(&ip, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
            let (_c, out) = ctx.runner.run("rc-update", &[if d.autostart { "add" } else { "del" }, &d.init_name(), "default"]);
            let _ = out;
            Ok(String::new())
        }
        "svc-remove" => {
            let d = load(ctx.root, arg(1)?)?;
            ctx.runner.run("rc-service", &[&d.init_name(), "stop"]);
            ctx.runner.run("rc-update", &["del", &d.init_name(), "default"]);
            let _ = std::fs::remove_file(init_path(ctx.root, &d.name));
            std::fs::remove_file(conf_path(ctx.root, &d.name)).map_err(|e| e.to_string())?;
            Ok(String::new())
        }
        a @ ("svc-start" | "svc-stop" | "svc-restart") => {
            let d = load(ctx.root, arg(1)?)?;
            let (code, out) = ctx.runner.run("rc-service", &[&d.init_name(), &a[4..]]);
            if code == 0 { Ok(String::new()) } else { Err(cap(format!("{} failed: {}", &a[4..], out.trim()))) }
        }
        "svc-status" => { let d = load(ctx.root, arg(1)?)?; Ok(format!("{:?}", status_of(ctx, &d)).to_lowercase()) }
        "svc-log" => {
            let d = load(ctx.root, arg(1)?)?;
            let n: usize = arg(2).ok().and_then(|s| s.parse().ok()).unwrap_or(40).clamp(1, 500);
            Ok(tail(&ctx.root.join(d.log_path().trim_start_matches('/')), n))
        }
        "apk-search" => {
            let q = arg(1)?;
            if q.len() < 2 || q.len() > 64 || !q.chars().all(|c| c.is_ascii_alphanumeric() || "._+-".contains(c)) { return Err("Type at least 2 letters or digits to search for.".into()); }
            let (_c, out) = ctx.runner.run("apk", &["search", "-v", "--", q]);
            Ok(out.lines().take(200).collect::<Vec<_>>().join("\n"))
        }
        "apk-installed" => { let (_c, out) = ctx.runner.run("apk", &["info", "-v"]); Ok(cap(out)) }
        "apk-update" => { let (c, out) = ctx.runner.run("apk", &["update"]); if c == 0 { Ok(cap(out)) } else { Err(cap(out)) } }
        "apk-upgrade" => { let (c, out) = ctx.runner.run("apk", &["upgrade", "--no-progress"]); if c == 0 { Ok(cap(out)) } else { Err(cap(out)) } }
        a @ ("apk-add" | "apk-del") => {
            let pkgs: Vec<&str> = w[1..].iter().map(String::as_str).collect();
            if pkgs.is_empty() || pkgs.len() > 20 || !pkgs.iter().all(|p| valid_pkg(p)) { return Err("That is not a valid package name.".into()); }
            let mut args = vec![if a == "apk-add" { "add" } else { "del" }, "--no-progress"];
            args.extend(pkgs);
            let (c, out) = ctx.runner.run("apk", &args);
            if c == 0 { Ok(cap(out)) } else { Err(cap(out)) }
        }
        other => Err(format!("Unknown request: {}", other.chars().take(30).collect::<String>())),
    }
}

/// One request line in, one answer line out.
pub fn answer(ctx: &Ctx, line: &str) -> String {
    match handle(ctx, &decode_request(line)) { Ok(s) => format!("OK {}\n", esc(&s)), Err(e) => format!("ERR {}\n", esc(&e)) }
}

// ---- client (used by the apps and by `exec85 --svc`) ----------------------------------------------------------------------

pub fn call_at(sock: &str, words: &[&str], timeout: Duration) -> Result<String, String> {
    let mut s = UnixStream::connect(sock).map_err(|_| "The system helper is not running.".to_string())?;
    s.set_read_timeout(Some(timeout)).ok();
    s.set_write_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(format!("{}\n", encode_request(words)).as_bytes()).map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| format!("No answer from the helper: {e}"))?;
    let line = line.trim_end();
    if let Some(r) = line.strip_prefix("OK") { Ok(unesc(r.trim_start())) }
    else if let Some(r) = line.strip_prefix("ERR") { Err(unesc(r.trim_start())) }
    else { Err("The helper sent an answer I do not understand.".into()) }
}

pub fn call(words: &[&str]) -> Result<String, String> { call_at(SOCK, words, Duration::from_secs(900)) }

// ---- server ---------------------------------------------------------------------------------------------------------------

#[repr(C)] struct UCred { pid: i32, uid: u32, gid: u32 }
extern "C" { fn getsockopt(fd: i32, level: i32, name: i32, val: *mut std::ffi::c_void, len: *mut u32) -> i32; }

fn peer_uid(s: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    let mut c = UCred { pid: 0, uid: u32::MAX, gid: 0 };
    let mut len = std::mem::size_of::<UCred>() as u32;
    // SAFETY: SO_PEERCRED fills a ucred struct of the size we give.
    let r = unsafe { getsockopt(s.as_raw_fd(), 1, 17, &mut c as *mut UCred as *mut std::ffi::c_void, &mut len) };
    if r == 0 { Some(c.uid) } else { None }
}

fn serve_one(mut s: UnixStream) {
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let root = Path::new("/");
    let ok = peer_uid(&s).map_or(false, |u| authorized(root, u));
    let mut line = String::new();
    if BufReader::new(std::io::Read::take(&s, 1 << 20)).read_line(&mut line).is_err() { return; }
    let reply = if ok { answer(&Ctx { root, runner: &SysRunner }, &line) } else { format!("ERR {}\n", esc("Only administrators may do that.")) };
    let _ = s.write_all(reply.as_bytes());
}

/// The process id of a running service: the process owned by the service's user whose command line is exactly the service's command.
pub fn find_pid(proc_dir: &Path, d: &ServiceDef, uid: Option<u32>) -> Option<u32> {
    use std::os::unix::fs::MetadataExt;
    let want = services::split_command(&d.command).ok()?;
    let mut best = None;
    for e in std::fs::read_dir(proc_dir).ok()?.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<u32>().ok() else { continue };
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        if args != want { continue; }
        if let (Some(u), Ok(m)) = (uid, std::fs::metadata(e.path())) { if m.uid() != u { continue; } }
        best = Some(best.map_or(pid, |b: u32| b.min(pid)));
    }
    best
}

/// Print one console line whenever a service changes state (so tests, and people watching the console, can see it).
fn report_loop() {
    let mut last = std::collections::HashMap::<String, String>::new();
    loop {
        let ctx = Ctx { root: Path::new("/"), runner: &SysRunner };
        for n in list_names(ctx.root) {
            if let Ok(d) = load(ctx.root, &n) {
                let st = format!("{:?}", status_of(&ctx, &d)).to_lowercase();
                let uid = account::users(ctx.root).into_iter().find(|u| u.name == d.user).map(|u| u.uid);
                let pid = find_pid(Path::new("/proc"), &d, uid).map_or("-".to_string(), |p| p.to_string());
                let line = format!("EXEC85_SVC name={n} status={st} pid={pid}");
                if last.get(&n) != Some(&line) { crate::supervisor::console_line(&line); last.insert(n, line); }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

pub fn serve() -> ! {
    let _ = std::fs::create_dir_all(SOCK_DIR);
    let _ = std::fs::remove_file(SOCK);
    let l = match UnixListener::bind(SOCK) { Ok(l) => l, Err(e) => { crate::supervisor::console_line(&format!("EXEC85_HELPER cannot listen: {e}")); std::process::exit(1) } };
    let _ = std::fs::set_permissions(SOCK, std::fs::Permissions::from_mode(0o666)); // everyone may connect; the peer check decides
    crate::supervisor::console_line("EXEC85_HELPER ready");
    std::thread::spawn(report_loop);
    for c in l.incoming().flatten() { std::thread::spawn(move || serve_one(c)); }
    std::process::exit(1)
}

/// `exec85 --svc ...` and `exec85 --pkg ...`: the same actions as the apps, from a terminal.
pub fn cli(args: &[String]) -> i32 {
    let sub: Vec<&str> = args.iter().map(String::as_str).collect();
    let show = |r: Result<String, String>| match r { Ok(s) => { if !s.is_empty() { println!("{}", s.trim_end()); } 0 } Err(e) => { eprintln!("error: {e}"); 1 } };
    match sub.as_slice() {
        ["--svc", "add", name, rest @ ..] => {
            let mut d = ServiceDef { name: name.to_string(), user: std::env::var("USER").unwrap_or_default(), autostart: true, ..Default::default() };
            let mut i = 0;
            while i < rest.len() {
                match rest[i] {
                    "--dir" if i + 1 < rest.len() => { d.dir = rest[i + 1].into(); i += 2; }
                    "--user" if i + 1 < rest.len() => { d.user = rest[i + 1].into(); i += 2; }
                    "--env" if i + 1 < rest.len() => { if let Some((k, v)) = rest[i + 1].split_once('=') { d.env.push((k.into(), v.into())); } i += 2; }
                    "--no-autostart" => { d.autostart = false; i += 1; }
                    "--" => { d.command = rest[i + 1..].join(" "); break; }
                    other => { eprintln!("error: unexpected {other}; put the command after --"); return 2; }
                }
            }
            show(call(&["svc-save", &services::render_conf(&d)]))
        }
        ["--svc", "list"] => show(call(&["svc-list"])),
        ["--svc", a @ ("start" | "stop" | "restart" | "status" | "get"), name] => show(call(&[&format!("svc-{a}"), name])),
        ["--svc", "remove", name] => show(call(&["svc-remove", name])),
        ["--svc", "log", name] => show(call(&["svc-log", name, "40"])),
        ["--pkg", a @ ("add" | "del"), pkgs @ ..] if !pkgs.is_empty() => { let mut v = vec![format!("apk-{a}")]; v.extend(pkgs.iter().map(|p| p.to_string())); let r: Vec<&str> = v.iter().map(String::as_str).collect(); show(call(&r)) }
        ["--pkg", "search", q] => show(call(&["apk-search", q])),
        ["--pkg", "update"] => show(call(&["apk-update"])),
        ["--pkg", "list"] => show(call(&["apk-installed"])),
        _ => { eprintln!("usage: exec85 --svc add NAME [--dir D] [--user U] [--env K=V] [--no-autostart] -- COMMAND...\n       exec85 --svc list|start|stop|restart|status|remove|log NAME\n       exec85 --pkg add|del PACKAGE... | search WORD | update | list"); 2 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, reply: RefCell<(i32, String)> }
    impl Fake { fn new() -> Fake { Fake { calls: RefCell::new(vec![]), reply: RefCell::new((0, String::new())) } } }
    impl Runner for Fake { fn run(&self, c: &str, a: &[&str]) -> (i32, String) { self.calls.borrow_mut().push(format!("{c} {}", a.join(" "))); self.reply.borrow().clone() } }

    fn root(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-helper-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\ntester:x:1000:1000::/home/tester:/bin/ash\nguest:x:1001:1001::/home/guest:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "root:x:0:\nwheel:x:10:tester\nusers:x:100:guest\n").unwrap();
        d
    }
    fn w(s: &str) -> Vec<String> { decode_request(s) }
    fn conf() -> String { services::render_conf(&ServiceDef { name: "bot".into(), description: "A bot".into(), command: "python3 bot.py".into(), dir: "/srv/bot".into(), user: "tester".into(), env: vec![], autostart: true }) }

    #[test]
    fn wire_format_round_trips_awkward_text() {
        for s in ["plain", "a b", "100%", "line\nbreak", "%41", "tab\t", "é€", ""] { assert_eq!(unesc(&esc(s)), s, "{s:?}"); }
        let line = encode_request(&["svc-save", "a b\nc"]);
        assert!(!line.contains('\n') && line.split(' ').count() == 2);
        assert_eq!(decode_request(&line), vec!["svc-save", "a b\nc"]);
    }

    #[test]
    fn only_root_and_wheel_members_are_let_in() {
        let r = root("auth");
        assert!(authorized(&r, 0) && authorized(&r, 1000));
        assert!(!authorized(&r, 1001), "an ordinary user is refused");
        assert!(!authorized(&r, 4242), "an unknown id is refused");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn saving_a_service_writes_files_and_enables_it() {
        let r = root("save"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        handle(&c, &["svc-save".into(), conf()]).unwrap();
        let script = std::fs::read_to_string(r.join("etc/init.d/exec85-bot")).unwrap();
        assert!(script.contains("supervisor=supervise-daemon") && script.contains("command_user='tester'"));
        assert_eq!(std::fs::metadata(r.join("etc/init.d/exec85-bot")).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(f.calls.borrow().iter().any(|x| x == "rc-update add exec85-bot default"));
        assert_eq!(list_names(&r), vec!["bot"]);
        // switching autostart off removes it from the default runlevel
        let off = conf().replace("autostart=true", "autostart=false");
        handle(&c, &["svc-save".into(), off]).unwrap();
        assert!(f.calls.borrow().iter().any(|x| x == "rc-update del exec85-bot default"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn bad_services_are_refused_and_write_nothing() {
        let r = root("bad"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        assert!(handle(&c, &["svc-save".into(), conf().replace("user=tester", "user=ghost")]).unwrap_err().contains("does not exist"));
        assert!(handle(&c, &["svc-save".into(), conf().replace("name=bot", "name=../../etc/passwd")]).is_err());
        assert!(handle(&c, &["svc-save".into(), "rubbish".into()]).is_err());
        assert!(list_names(&r).is_empty() && !r.join("etc/init.d").exists());
        assert!(f.calls.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn start_stop_status_remove_and_logs() {
        let r = root("life"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        handle(&c, &["svc-save".into(), conf()]).unwrap();
        handle(&c, &w("svc-start bot")).unwrap();
        handle(&c, &w("svc-restart bot")).unwrap();
        assert!(f.calls.borrow().iter().any(|x| x == "rc-service exec85-bot start") && f.calls.borrow().iter().any(|x| x == "rc-service exec85-bot restart"));
        *f.reply.borrow_mut() = (3, " * status: stopped\n".into());
        assert_eq!(handle(&c, &w("svc-status bot")).unwrap(), "stopped");
        assert_eq!(handle(&c, &w("svc-list")).unwrap(), "bot stopped true\n");
        *f.reply.borrow_mut() = (1, "boom".into());
        assert!(handle(&c, &w("svc-start bot")).unwrap_err().contains("boom"));
        *f.reply.borrow_mut() = (0, String::new());
        std::fs::create_dir_all(r.join("var/log/exec85")).unwrap();
        std::fs::write(r.join("var/log/exec85/bot.log"), (1..=100).map(|i| format!("line {i}\n")).collect::<String>()).unwrap();
        assert_eq!(handle(&c, &w("svc-log bot 3")).unwrap(), "line 98\nline 99\nline 100");
        assert!(handle(&c, &w("svc-start nosuch")).is_err());
        handle(&c, &w("svc-remove bot")).unwrap();
        assert!(list_names(&r).is_empty() && !r.join("etc/init.d/exec85-bot").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn package_commands_check_their_arguments() {
        let r = root("apk"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        handle(&c, &w("apk-add nginx python3")).unwrap();
        assert_eq!(f.calls.borrow()[0], "apk add --no-progress nginx python3");
        for bad in ["-X", "a;b", "$(x)", "--allow-untrusted", "A B/c", "../x"] {
            assert!(handle(&c, &["apk-add".into(), bad.into()]).is_err(), "{bad}");
            assert!(handle(&c, &["apk-del".into(), bad.into()]).is_err(), "{bad}");
        }
        assert!(handle(&c, &w("apk-search x")).is_err(), "too short");
        assert!(handle(&c, &["apk-search".into(), "--help".into()]).is_ok(), "an odd search word is passed after `--`, so it can never act as an option");
        handle(&c, &w("apk-search nginx")).unwrap();
        assert!(f.calls.borrow().iter().any(|x| x == "apk search -v -- nginx"));
        assert!(handle(&c, &w("rm -rf /")).is_err(), "unknown actions are refused");
        assert_eq!(f.calls.borrow().len(), 3, "only the add, the odd search and the real search ran");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn finds_the_process_of_a_service() {
        let mut child = std::process::Command::new("sleep").arg("1234").spawn().unwrap();
        let d = ServiceDef { name: "s".into(), command: "sleep 1234".into(), user: "x".into(), ..Default::default() };
        assert_eq!(find_pid(Path::new("/proc"), &d, None), Some(child.id()));
        assert_eq!(find_pid(Path::new("/proc"), &d, Some(crate::sys::uid())), Some(child.id()));
        assert_eq!(find_pid(Path::new("/proc"), &d, Some(crate::sys::uid().wrapping_add(7))), None, "another user's process does not count");
        let other = ServiceDef { command: "sleep 4321".into(), ..d.clone() };
        assert_eq!(find_pid(Path::new("/proc"), &other, None), None);
        child.kill().unwrap(); child.wait().unwrap();
    }

    #[test]
    fn answers_are_one_line_and_the_client_understands_them() {
        let r = root("sock"); let f = Fake::new();
        let c = Ctx { root: &r, runner: &f };
        let ok = answer(&c, "ping\n");
        assert_eq!(ok, "OK pong\n");
        assert!(answer(&c, "nonsense\n").starts_with("ERR "));
        // real socket round trip
        let sock = r.join("h.sock");
        let l = UnixListener::bind(&sock).unwrap();
        let rr = r.clone();
        let t = std::thread::spawn(move || { let (s, _) = l.accept().unwrap(); let mut line = String::new(); BufReader::new(&s).read_line(&mut line).unwrap(); let f = Fake::new(); let c = Ctx { root: &rr, runner: &f }; (&s).write_all(answer(&c, &line).as_bytes()).unwrap(); });
        assert_eq!(call_at(sock.to_str().unwrap(), &["ping"], Duration::from_secs(5)).unwrap(), "pong");
        t.join().unwrap();
        assert!(call_at("/no/such.sock", &["ping"], Duration::from_secs(1)).is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}
