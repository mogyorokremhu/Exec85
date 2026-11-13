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

pub trait Runner {
    fn run(&self, cmd: &str, args: &[&str]) -> (i32, String);
    /// Like `run`, with text on standard input (for passwords: they never go on a command line). The default ignores the input; real runners pass it on.
    fn run_input(&self, cmd: &str, args: &[&str], input: &str) -> (i32, String) { let _ = input; self.run(cmd, args) }
}

pub struct SysRunner;
impl Runner for SysRunner {
    fn run(&self, cmd: &str, args: &[&str]) -> (i32, String) {
        match std::process::Command::new(cmd).args(args).env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").stdin(std::process::Stdio::null()).output() {
            Ok(o) => { let mut s = String::from_utf8_lossy(&o.stdout).into_owned(); s.push_str(&String::from_utf8_lossy(&o.stderr)); (o.status.code().unwrap_or(-1), s) }
            Err(e) => (-1, format!("cannot run {cmd}: {e}")),
        }
    }
    fn run_input(&self, cmd: &str, args: &[&str], input: &str) -> (i32, String) { SysRunner::spawn_with_input(cmd, args, input) }
}

impl SysRunner {
    fn spawn_with_input(cmd: &str, args: &[&str], input: &str) -> (i32, String) {
        use std::io::Write as _;
        let child = std::process::Command::new(cmd).args(args).env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).spawn();
        let Ok(mut child) = child else { return (-1, format!("cannot run {cmd}")) };
        if let Some(mut si) = child.stdin.take() { let _ = si.write_all(input.as_bytes()); }
        match child.wait_with_output() { Ok(o) => { let mut s = String::from_utf8_lossy(&o.stdout).into_owned(); s.push_str(&String::from_utf8_lossy(&o.stderr)); (o.status.code().unwrap_or(-1), s) } Err(e) => (-1, e.to_string()) }
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

// ---- firewall ----------------------------------------------------------------------------------------------------------------

fn fw_conf_path(root: &Path) -> PathBuf { root.join("etc/exec85/firewall.conf") }
pub fn fw_nft_path(root: &Path) -> PathBuf { root.join("etc/exec85/firewall.nft") }

pub fn fw_config(root: &Path) -> crate::firewall::Config { crate::firewall::parse_config(&std::fs::read_to_string(fw_conf_path(root)).unwrap_or_default()) }

/// The ports the services have declared (each open to the local network).
pub fn fw_service_rules(root: &Path) -> Vec<crate::firewall::Rule> {
    let mut out = Vec::new();
    for n in list_names(root) {
        let Ok(d) = load(root, &n) else { continue };
        for (port, proto) in crate::firewall::parse_ports(&d.ports).unwrap_or_default() { out.push(crate::firewall::Rule { name: d.name.clone(), port, proto, scope: crate::firewall::Scope::Local }); }
    }
    out
}

/// Write the ruleset for the current configuration and load it.
pub fn fw_apply(ctx: &Ctx) -> Result<String, String> {
    let script = crate::firewall::render_ruleset(&fw_config(ctx.root), &fw_service_rules(ctx.root));
    let path = fw_nft_path(ctx.root);
    if let Some(d) = path.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&path, script).map_err(|e| e.to_string())?;
    let (code, out) = ctx.runner.run("nft", &["-f", &path.to_string_lossy()]);
    if code == 0 { Ok(String::new()) } else { Err(format!("The firewall rules could not be loaded: {}", out.trim())) }
}

fn fw_save(ctx: &Ctx, c: &crate::firewall::Config) -> Result<String, String> {
    if let Some(d) = fw_conf_path(ctx.root).parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(fw_conf_path(ctx.root), crate::firewall::render_config(c)).map_err(|e| e.to_string())?;
    fw_apply(ctx)
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
            let _ = fw_apply(ctx);   // the service may have declared ports
            Ok(String::new())
        }
        "svc-remove" => {
            let d = load(ctx.root, arg(1)?)?;
            ctx.runner.run("rc-service", &[&d.init_name(), "stop"]);
            ctx.runner.run("rc-update", &["del", &d.init_name(), "default"]);
            let _ = std::fs::remove_file(init_path(ctx.root, &d.name));
            std::fs::remove_file(conf_path(ctx.root, &d.name)).map_err(|e| e.to_string())?;
            let _ = fw_apply(ctx);
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
        "usb-eject" => {
            let mounts = std::fs::read_to_string(ctx.root.join("proc/mounts")).unwrap_or_default();
            crate::usb::eject(ctx.runner, &mounts, &ctx.root.join("run/exec85"), &ctx.root.join("media"), arg(1)?)
        }
        "screen-power" => {
            // Switch the display off or on (the monitor goes to sleep). Programs and services keep running.
            let level = match arg(1)? { "off" => "4", "on" => "0", _ => return Err("Use on or off.".into()) };
            let path = ctx.root.join("sys/class/graphics/fb0/blank");
            std::fs::write(&path, level).map_err(|e| format!("cannot switch the screen: {e}"))?;
            Ok(String::new())
        }
        "mc-versions" => crate::mc::versions(ctx, w.get(1).and_then(|t| crate::mc::ServerType::parse(t)).unwrap_or(crate::mc::ServerType::Vanilla)),
        "mc-create" => {
            let (name, version, ram, eula, user) = (arg(1)?, arg(2)?, arg(3)?, arg(4)?, arg(5)?);
            let ram_gb: u32 = ram.parse().map_err(|_| "RAM must be 2, 4 or 8.".to_string())?;
            let kind = match w.get(6) { Some(t) => crate::mc::ServerType::parse(t).ok_or("Server type must be vanilla, paper or fabric.")?, None => crate::mc::ServerType::Vanilla };
            crate::mc::create(ctx, &crate::mc::Create { kind, name, version, ram_gb, eula: eula == "1", user })
        }
        "fw-status" => {
            let c = fw_config(ctx.root);
            let mut out = format!("enabled {}\n", if c.enabled { "yes" } else { "no" });
            for r in &c.rules { out.push_str(&format!("rule {} {} {} {} manual\n", r.name, r.port, r.proto.id(), r.scope.id())); }
            for r in fw_service_rules(ctx.root) { out.push_str(&format!("rule {} {} {} {} service\n", r.name, r.port, r.proto.id(), r.scope.id())); }
            Ok(out)
        }
        "fw-enable" => { let mut c = fw_config(ctx.root); c.enabled = match arg(1)? { "on" => true, "off" => false, _ => return Err("Use on or off.".into()) }; fw_save(ctx, &c) }
        "fw-allow" => {
            let (name, spec) = (arg(1)?, arg(2)?);
            if !crate::firewall::valid_rule_name(name) { return Err("The rule name may only have lowercase letters, digits, - and _.".into()); }
            let (port, proto) = crate::firewall::parse_port(spec)?;
            let scope = match arg(3).unwrap_or("local") { "local" => crate::firewall::Scope::Local, "anywhere" => crate::firewall::Scope::Anywhere, _ => return Err("The scope must be local or anywhere.".into()) };
            let mut c = fw_config(ctx.root);
            c.rules.retain(|r| r.name != name);
            c.rules.push(crate::firewall::Rule { name: name.to_string(), port, proto, scope });
            fw_save(ctx, &c)
        }
        "fw-remove" => { let mut c = fw_config(ctx.root); let before = c.rules.len(); c.rules.retain(|r| r.name != arg(1).unwrap_or("")); if c.rules.len() == before { return Err("There is no such rule.".into()); } fw_save(ctx, &c) }
        "fw-apply" => fw_apply(ctx),
        "ssh-status" => Ok(crate::ssh::status(ctx)),
        "ssh-enable" => {
            let port: u16 = arg(1)?.parse().map_err(|_| "The port must be a number.".to_string())?;
            crate::ssh::enable(ctx, port, arg(2)? == "1", arg(3)?)
        }
        "ssh-disable" => crate::ssh::disable(ctx),
        "ssh-addkey" => crate::ssh::add_key(ctx, arg(1)?, arg(2)?),
        "mc-accept" => crate::mc::accept_eula(ctx, arg(1)?),
        "net-set" => {
            let name = arg(1)?;
            let mode = match arg(2)? {
                "dhcp" => crate::netconf::Mode::Dhcp,
                "off" => crate::netconf::Mode::Off,
                "static" => crate::netconf::static_mode(arg(3)?, arg(4)?, w.get(5).map_or("", String::as_str))?,
                _ => return Err("The mode must be dhcp, static or off.".into()),
            };
            crate::netapply::set(ctx, name, mode)
        }
        "net-apply" => Ok(crate::netapply::apply_all(ctx).join("\n")),
        "wifi-scan" => crate::netapply::wifi_scan(ctx, arg(1)?),
        "wifi-connect" => crate::netapply::wifi_connect(ctx, arg(1)?, arg(2)?, w.get(3).map_or("", String::as_str)),
        "wifi-forget" => crate::netapply::wifi_forget(ctx, arg(1)?),
        "recipe-list" => Ok(crate::recipes::list(ctx)),
        "recipe-install" => {
            // recipe-install ID USER key=value key=value ...  (values may contain spaces: they travel escaped)
            let (id, user) = (arg(1)?, arg(2)?);
            let values: Vec<(String, String)> = w[3..].iter().filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string()))).collect();
            crate::recipes::install(ctx, id, &values, user)
        }
        "recipe-remove" => crate::recipes::remove(ctx, arg(1)?),
        "bot-create" => {
            let (name, lang, kind, srcarg, token, user) = (arg(1)?, arg(2)?, arg(3)?, arg(4)?, arg(5)?, arg(6)?);
            let lang = crate::bot::Lang::parse(lang).ok_or("The language must be python or node.")?;
            let source = match kind { "starter" => crate::bot::Source::Starter, "folder" => crate::bot::Source::Folder(srcarg.to_string()), "git" => crate::bot::Source::Git(srcarg.to_string()), _ => return Err("The code must come from the starter, a folder or Git.".into()) };
            crate::bot::create(ctx, &crate::bot::Create { name, lang, source, token, user })
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

fn read_cmdline(dir: &Path) -> Vec<String> {
    std::fs::read(dir.join("cmdline")).map(|raw| raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect()).unwrap_or_default()
}

fn read_ppid(dir: &Path) -> Option<u32> {
    let stat = std::fs::read_to_string(dir.join("stat")).ok()?;
    stat[stat.rfind(')')? + 1..].split_whitespace().nth(1)?.parse().ok()
}

/// The program a service runs is a descendant of its `supervise-daemon` (whose command line names the service).
/// Several supervise-daemon processes may briefly exist (the one that daemonizes and the one that stays), so every one is considered.
pub fn find_supervised_pid(proc_dir: &Path, init_name: &str) -> Option<u32> {
    let procs: Vec<(u32, PathBuf)> = std::fs::read_dir(proc_dir).ok()?.flatten().filter_map(|e| Some((e.file_name().to_string_lossy().parse().ok()?, e.path()))).collect();
    let is_sup = |d: &Path| { let a = read_cmdline(d); a.first().map_or(false, |x| x.ends_with("supervise-daemon")) };
    let named = |d: &Path| { let a = read_cmdline(d); a.iter().skip(1).any(|w| w == init_name) };
    let info: Vec<(u32, Option<u32>, bool, bool)> = procs.iter().map(|(p, d)| (*p, read_ppid(d), is_sup(d), named(d))).collect();
    let mut family: Vec<u32> = info.iter().filter(|(_, _, sup, nm)| *sup && *nm).map(|(p, ..)| *p).collect();
    if family.is_empty() { return None; }
    let mut found = Vec::new();
    // walk down the tree a few levels
    for _ in 0..4 {
        let kids: Vec<&(u32, Option<u32>, bool, bool)> = info.iter().filter(|(p, pp, ..)| pp.map_or(false, |pp| family.contains(&pp)) && !family.contains(p)).collect();
        if kids.is_empty() { break; }
        for k in &kids { family.push(k.0); if !k.2 { found.push(k.0); } }
        if !found.is_empty() { break; }
    }
    found.into_iter().min()
}

/// The process id of a running service: the process owned by the service's user whose command line is exactly the service's command.
pub fn find_pid(proc_dir: &Path, d: &ServiceDef, uid: Option<u32>) -> Option<u32> {
    let want = services::split_command(&d.command).ok()?;
    let mut best = None;
    for e in std::fs::read_dir(proc_dir).ok()?.flatten() {
        let Some(pid) = e.file_name().to_string_lossy().parse::<u32>().ok() else { continue };
        let Ok(raw) = std::fs::read(e.path().join("cmdline")) else { continue };
        let args: Vec<String> = raw.split(|b| *b == 0).filter(|a| !a.is_empty()).map(|a| String::from_utf8_lossy(a).into_owned()).collect();
        // The program may be started by its full path (/bin/sleep) while the service says `sleep`: compare the file name.
        let base = |a: &str| a.rsplit('/').next().unwrap_or(a).to_string();
        if args.len() != want.len() || args.is_empty() || base(&args[0]) != base(&want[0]) || args[1..] != want[1..] { continue; }
        // The owner is read from the status file: after a program drops root rights, the /proc/<pid> folder itself stays owned by root.
        if let Some(u) = uid {
            let status = std::fs::read_to_string(e.path().join("status")).unwrap_or_default();
            let owner = status.lines().find_map(|l| l.strip_prefix("Uid:")).and_then(|r| r.split_whitespace().next()).and_then(|v| v.parse::<u32>().ok());
            if owner != Some(u) { continue; }
        }
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
                // OpenRC records the supervised program's process id itself; fall back to looking through /proc.
                let recorded = std::fs::read_to_string(format!("/run/openrc/options/{}/child_pid", d.init_name())).ok().and_then(|v| v.trim().parse::<u32>().ok()).filter(|p| Path::new(&format!("/proc/{p}")).exists());
                let pid = recorded.or_else(|| find_supervised_pid(Path::new("/proc"), &d.init_name())).or_else(|| find_pid(Path::new("/proc"), &d, uid)).map_or("-".to_string(), |p| p.to_string());
                let up = std::fs::read_to_string("/proc/uptime").ok().and_then(|u| u.split_whitespace().next().map(|v| v.split('.').next().unwrap_or("0").to_string())).unwrap_or_default();
                let line = format!("EXEC85_SVC name={n} status={st} pid={pid}");
                // The uptime is only for people reading the console; a new line is printed when status or process change, not every two seconds.
                if last.get(&n) != Some(&line) { crate::supervisor::console_line(&format!("{line} up={up}")); last.insert(n.clone(), line); }
                // A service that is "running" but has no process of its own is probably failing to start: show why.
                if pid == "-" && st == "running" {
                    let log = tail(&Path::new("/").join(d.log_path().trim_start_matches('/')), 3).replace('\n', " / ").replace(' ', "_");
                    let key = format!("{n}:log");
                    let line = format!("EXEC85_SVCLOG name={n} text={}", if log.is_empty() { "(empty)".to_string() } else { log.chars().take(300).collect() });
                    if last.get(&key) != Some(&line) { crate::supervisor::console_line(&line); last.insert(key, line); }
                }
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// Look for USB drives every two seconds: mount new ones under /media, unmount the ones that were pulled out.
fn usb_watch() {
    use crate::usb::{usb_partitions, Event, Manager};
    let mut m = Manager::new(Path::new("/sys/block"), Path::new("/dev"), Path::new("/media"), Path::new(SOCK_DIR));
    let r = SysRunner;
    r.run("modprobe", &["-a", "vfat", "exfat", "ntfs3", "nls_cp437", "nls_utf8", "nls_iso8859-1"]);
    loop {
        let parts = usb_partitions(&m.sys_block);
        if parts.iter().any(|p| !m.dev_dir.join(p).exists()) { r.run("mdev", &["-s"]); }
        let mounts = std::fs::read_to_string("/proc/mounts").unwrap_or_default();
        let mounted: std::collections::HashSet<String> = mounts.lines().filter_map(|l| l.split_whitespace().next()).filter_map(|d| d.strip_prefix("/dev/")).map(String::from).collect();
        // Files on drives without owners (FAT, exFAT, NTFS) belong to the first regular user.
        let (uid, gid) = account::regular_users(Path::new("/")).first().map_or((1000, 1000), |u| (u.uid, u.gid));
        for e in m.scan(&r, &mounted, uid, gid) {
            crate::supervisor::console_line(&match e {
                Event::Added { dev, fs, label, mount } => format!("EXEC85_USB added dev={dev} fs={fs} label={} mount={mount}", if label.is_empty() { "-".to_string() } else { label.replace(' ', "_") }),
                Event::Removed { mount } => format!("EXEC85_USB removed mount={mount}"),
                Event::Error(e) => format!("EXEC85_USB error {}", e.replace(' ', "_")),
            });
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

extern "C" { fn umask(mask: u32) -> u32; }

pub fn serve() -> ! {
    // Folders the helper creates (for Java, for servers) must be enterable by the users that run programs from them.
    // SAFETY: umask only changes this process's default file permissions.
    unsafe { umask(0o022); }
    let _ = std::fs::create_dir_all(SOCK_DIR);
    let _ = std::fs::remove_file(SOCK);
    let l = match UnixListener::bind(SOCK) { Ok(l) => l, Err(e) => { crate::supervisor::console_line(&format!("EXEC85_HELPER cannot listen: {e}")); std::process::exit(1) } };
    let _ = std::fs::set_permissions(SOCK, std::fs::Permissions::from_mode(0o666)); // everyone may connect; the peer check decides
    crate::supervisor::console_line("EXEC85_HELPER ready");
    std::thread::spawn(report_loop);
    std::thread::spawn(usb_watch);
    let _ = fw_apply(&Ctx { root: Path::new("/"), runner: &SysRunner });   // make sure the rules match the services that exist
    for c in l.incoming().flatten() { std::thread::spawn(move || serve_one(c)); }
    std::process::exit(1)
}

/// `exec85 --svc ...` and `exec85 --pkg ...`: the same actions as the apps, from a terminal.
pub fn cli(args: &[String]) -> i32 {
    let sub: Vec<&str> = args.iter().map(String::as_str).collect();
    // Output goes through write!, not println!: when the reader stops early (`... | head -1`) that is not an error worth crashing for.
    let show = |r: Result<String, String>| match r {
        Ok(s) => { if !s.is_empty() { let _ = writeln!(std::io::stdout(), "{}", s.trim_end()); } 0 }
        Err(e) => { let _ = writeln!(std::io::stderr(), "error: {e}"); 1 }
    };
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
                    "--" => { d.command = rest[i + 1..].iter().map(|w| if w.is_empty() || w.contains(|c: char| c.is_whitespace() || c == '\'' || c == '"' || c == '\\') { services::sh_quote(w) } else { w.to_string() }).collect::<Vec<_>>().join(" "); break; }
                    other => { let _ = writeln!(std::io::stderr(), "error: unexpected {other}; put the command after --"); return 2; }
                }
            }
            show(call(&["svc-save", &services::render_conf(&d)]))
        }
        ["--svc", "list"] => show(call(&["svc-list"])),
        ["--svc", a @ ("start" | "stop" | "restart" | "status" | "get"), name] => show(call(&[&format!("svc-{a}"), name])),
        ["--svc", "remove", name] => show(call(&["svc-remove", name])),
        ["--svc", "log", name] => show(call(&["svc-log", name, "40"])),
        ["--mc", "versions"] => show(call(&["mc-versions", "vanilla"])),
        ["--mc", "versions", kind] => show(call(&["mc-versions", kind])),
        ["--mc", "create", name, version, ram, rest @ ..] => {
            let eula = rest.contains(&"--eula");
            let opt = |flag: &str| rest.iter().position(|a| *a == flag).and_then(|i| rest.get(i + 1).copied()).map(String::from);
            let user = opt("--user").unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
            let kind = opt("--type").unwrap_or_else(|| "vanilla".into());
            show(call(&["mc-create", name, version, ram, if eula { "1" } else { "0" }, &user, &kind]))
        }
        ["--bot", "create", name, lang, rest @ ..] => {
            // The token is never given on the command line (others could see it): it comes from $DISCORD_TOKEN or the first line typed in.
            let token = std::env::var("DISCORD_TOKEN").ok().filter(|t| !t.is_empty()).or_else(|| { let mut l = String::new(); std::io::stdin().read_line(&mut l).ok().map(|_| l.trim().to_string()) }).unwrap_or_default();
            let opt = |flag: &str| rest.iter().position(|a| *a == flag).and_then(|i| rest.get(i + 1).copied()).map(String::from);
            let user = opt("--user").unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
            let (kind, arg) = match (opt("--folder"), opt("--git")) { (Some(f), _) => ("folder", f), (None, Some(g)) => ("git", g), _ => ("starter", "-".to_string()) };
            show(call(&["bot-create", name, lang, kind, &arg, &token, &user]))
        }
        ["--app", "list"] => show(call(&["recipe-list"])),
        ["--app", "remove", id] => show(call(&["recipe-remove", id])),
        ["--app", "install", id, rest @ ..] => {
            // app install ID key=value ...  (the catalog screen is the nicer way to type a password)
            let user = std::env::var("USER").unwrap_or_default();
            let mut w: Vec<&str> = vec!["recipe-install", id, &user];
            w.extend(rest.iter().copied());
            show(call(&w))
        }
        ["--ssh", "status"] => show(call(&["ssh-status"])),
        ["--ssh", "disable"] => show(call(&["ssh-disable"])),
        ["--ssh", "enable", rest @ ..] => {
            let opt = |flag: &str| rest.iter().position(|a| *a == flag).and_then(|i| rest.get(i + 1).copied()).map(String::from);
            let user = opt("--user").unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
            show(call(&["ssh-enable", &opt("--port").unwrap_or_else(|| "22".into()), if rest.contains(&"--password-login") { "1" } else { "0" }, &user]))
        }
        ["--ssh", "addkey", key, rest @ ..] => {
            let user = rest.iter().position(|a| *a == "--user").and_then(|i| rest.get(i + 1).copied()).map(String::from).unwrap_or_else(|| std::env::var("USER").unwrap_or_default());
            show(call(&["ssh-addkey", &user, key]))
        }
        ["--mc", "accept", name] => show(call(&["mc-accept", name])),
        ["--net", "set", name, mode, rest @ ..] => { let mut v = vec!["net-set", name, mode]; v.extend(rest.iter().copied()); show(call(&v)) }
        ["--net", "wifi", "scan", iface] => show(call(&["wifi-scan", iface])),
        ["--net", "wifi", "connect", iface, ssid] => { let pw = std::env::var("WIFI_PASSWORD").unwrap_or_default(); show(call(&["wifi-connect", iface, ssid, &pw])) }
        ["--net", "wifi", "forget", ssid] => show(call(&["wifi-forget", ssid])),
        ["--fw", "status"] => show(call(&["fw-status"])),
        ["--fw", on_off @ ("on" | "off")] => show(call(&["fw-enable", on_off])),
        ["--fw", "allow", name, port, rest @ ..] => { let scope = rest.first().copied().unwrap_or("local"); show(call(&["fw-allow", name, port, scope])) }
        ["--fw", "remove", name] => show(call(&["fw-remove", name])),
        ["--usb", "eject", mount] => show(call(&["usb-eject", mount])),
        ["--pkg", a @ ("add" | "del"), pkgs @ ..] if !pkgs.is_empty() => { let mut v = vec![format!("apk-{a}")]; v.extend(pkgs.iter().map(|p| p.to_string())); let r: Vec<&str> = v.iter().map(String::as_str).collect(); show(call(&r)) }
        ["--pkg", "search", q] => show(call(&["apk-search", q])),
        ["--pkg", "update"] => show(call(&["apk-update"])),
        ["--pkg", "list"] => show(call(&["apk-installed"])),
        _ => { let _ = writeln!(std::io::stderr(), "usage: exec85 --svc add NAME [--dir D] [--user U] [--env K=V] [--no-autostart] -- COMMAND...\n       exec85 --svc list|start|stop|restart|status|remove|log NAME\n       exec85 --pkg add|del PACKAGE... | search WORD | update | list\n       exec85 --bot create NAME python|node [--folder PATH | --git URL] [--user U]   (token: set DISCORD_TOKEN first, or type it as the first line)\n       exec85 --ssh status | enable [--port N] [--password-login] [--user U] | disable | addkey \"PUBLIC KEY\" [--user U]\n       exec85 --net set IFACE dhcp|off|static CIDR GATEWAY [DNS] | wifi scan IFACE | wifi connect IFACE SSID (password in WIFI_PASSWORD) | wifi forget SSID\n       exec85 --fw status | on | off | allow NAME PORT[/tcp|udp] [local|anywhere] | remove NAME\n       exec85 --usb eject MOUNTPOINT\n       exec85 --mc versions [vanilla|paper|fabric] | create NAME VERSION 2|4|8 [--eula] [--type vanilla|paper|fabric] [--user U]"); 2 }
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
    fn conf() -> String { services::render_conf(&ServiceDef { name: "bot".into(), description: "A bot".into(), command: "python3 bot.py".into(), dir: "/srv/bot".into(), user: "tester".into(), env: vec![], autostart: true, ports: String::new() }) }

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
        let by_path = ServiceDef { command: "/bin/sleep 1234".into(), ..d.clone() };
        assert_eq!(find_pid(Path::new("/proc"), &by_path, None), Some(child.id()), "the program's file name is what counts");
        let other = ServiceDef { command: "sleep 4321".into(), ..d.clone() };
        assert_eq!(find_pid(Path::new("/proc"), &other, None), None);
        child.kill().unwrap(); child.wait().unwrap();
    }

    #[test]
    fn finds_the_child_of_the_supervisor() {
        let d = std::env::temp_dir().join(format!("exec85-fakeproc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let mk = |pid: u32, ppid: u32, cmd: &[&str]| {
            let p = d.join(pid.to_string());
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("cmdline"), cmd.iter().map(|c| format!("{c}\0")).collect::<String>()).unwrap();
            std::fs::write(p.join("stat"), format!("{pid} (some (odd) name) S {ppid} 0 0\n")).unwrap();
        };
        mk(1, 0, &["/sbin/init"]);
        mk(50, 1, &["supervise-daemon", "exec85-ticker", "--start", "--", "sleep", "100000"]);
        mk(51, 50, &["sleep", "100000"]);
        mk(60, 1, &["/usr/sbin/supervise-daemon", "exec85-other", "--start"]);
        mk(61, 60, &["python3"]);
        assert_eq!(find_supervised_pid(&d, "exec85-ticker"), Some(51));
        assert_eq!(find_supervised_pid(&d, "exec85-other"), Some(61));
        assert_eq!(find_supervised_pid(&d, "exec85-none"), None);
        // a second, childless supervise-daemon with the same name (the launcher) must not hide the real one
        mk(40, 1, &["supervise-daemon", "exec85-ticker", "--start"]);
        assert_eq!(find_supervised_pid(&d, "exec85-ticker"), Some(51));
        // a program that starts a helper of its own: the lowest pid below the supervisor wins
        mk(52, 51, &["sleep-helper"]);
        assert_eq!(find_supervised_pid(&d, "exec85-ticker"), Some(51));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_firewall_follows_the_services_and_the_hand_made_rules() {
        let r = root("fw"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        let with_ports = services::render_conf(&ServiceDef { name: "bot".into(), description: "A bot".into(), command: "python3 bot.py".into(), dir: "/srv/bot".into(), user: "tester".into(), env: vec![], autostart: true, ports: "8080/tcp,25565/udp".into() });
        handle(&c, &["svc-save".into(), with_ports]).unwrap();
        let nft = std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap();
        assert!(nft.contains("tcp dport 8080 accept comment \"bot\"") && nft.contains("udp dport 25565"), "a service's declared ports are opened (to the local network): {nft}");
        assert!(f.calls.borrow().iter().any(|x| x.starts_with("nft -f ")), "and the rules are loaded");
        handle(&c, &w("fw-allow ssh 22 local")).unwrap();
        handle(&c, &w("fw-allow web 443/tcp anywhere")).unwrap();
        let st = handle(&c, &w("fw-status")).unwrap();
        assert!(st.contains("enabled yes") && st.contains("rule ssh 22 tcp local manual") && st.contains("rule web 443 tcp anywhere manual") && st.contains("rule bot 8080 tcp local service"), "{st}");
        assert!(std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap().contains("\ttcp dport 443 accept"), "anywhere = no address condition");
        handle(&c, &w("fw-allow ssh 2222")).unwrap();   // the same name replaces the rule
        assert!(handle(&c, &w("fw-status")).unwrap().contains("rule ssh 2222 tcp local manual") && !handle(&c, &w("fw-status")).unwrap().contains("rule ssh 22 "));
        handle(&c, &w("fw-enable off")).unwrap();
        assert!(std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap().contains("policy accept"));
        handle(&c, &w("fw-enable on")).unwrap();
        handle(&c, &w("fw-remove web")).unwrap();
        assert!(handle(&c, &w("fw-remove web")).is_err(), "removing a rule that is not there is reported");
        handle(&c, &w("svc-remove bot")).unwrap();
        assert!(!std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap().contains("8080"), "a removed service's ports are closed again");
        for bad in ["fw-allow Bad 22", "fw-allow a 0", "fw-allow a 99999", "fw-allow a 22 mars", "fw-allow a x/y", "fw-enable maybe", "fw-allow"] { assert!(handle(&c, &w(bad)).is_err(), "{bad}"); }
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_failing_firewall_load_is_reported_not_ignored() {
        let r = root("fwbad"); let f = Fake::new(); *f.reply.borrow_mut() = (1, "Error: no such table".into());
        let c = Ctx { root: &r, runner: &f };
        assert!(handle(&c, &w("fw-allow a 22")).unwrap_err().contains("could not be loaded"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_ruleset_that_ships_with_the_system_is_the_default_one() {
        let shipped = include_str!("../../image/overlay/etc/exec85/firewall.nft");
        assert_eq!(shipped, crate::firewall::render_ruleset(&crate::firewall::Config::default(), &[]), "regenerate image/overlay/etc/exec85/firewall.nft from render_ruleset if the default changes");
    }

    #[test]
    fn the_screen_can_be_switched_off_and_on() {
        let r = root("screen"); let f = Fake::new(); let c = Ctx { root: &r, runner: &f };
        std::fs::create_dir_all(r.join("sys/class/graphics/fb0")).unwrap();
        handle(&c, &w("screen-power off")).unwrap();
        assert_eq!(std::fs::read_to_string(r.join("sys/class/graphics/fb0/blank")).unwrap(), "4");
        handle(&c, &w("screen-power on")).unwrap();
        assert_eq!(std::fs::read_to_string(r.join("sys/class/graphics/fb0/blank")).unwrap(), "0");
        assert!(handle(&c, &w("screen-power maybe")).is_err() && handle(&c, &w("screen-power")).is_err());
        let _ = std::fs::remove_dir_all(&r);
        let r = root("noscreen"); let c = Ctx { root: &r, runner: &f };
        assert!(handle(&c, &w("screen-power off")).unwrap_err().contains("cannot switch"), "a machine without that switch is reported, not a crash");
        let _ = std::fs::remove_dir_all(&r);
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
