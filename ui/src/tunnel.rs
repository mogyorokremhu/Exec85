//! Internet tunnel for people behind CGNAT (a home connection that cannot forward ports): the playit.gg agent.
//!
//! The agent connects OUT to playit.gg, and playit.gg gives you a public address that leads back to a port on this computer
//! (for Minecraft: 25565). So no router setting and no call to the provider is needed, and nothing has to be opened in the
//! firewall. It is two small programs from the agent's GitHub release (BSD-2-Clause licence): a background program
//! (`playitd`, run as a service under its own user) and a command-line program (`playit`) that makes the claim link.
//! The download is pinned to one release and its SHA-256 sums are checked, so a changed file is never installed.
use crate::helper::Ctx;
use crate::services::{self, ServiceDef};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

pub const RELEASE: &str = "v2.0.2";
pub const BASE_URL: &str = "https://github.com/playit-cloud/playit-agent/releases/download";

pub struct Piece { pub asset: &'static str, pub dest: &'static str, pub sha256: &'static str }
pub const DAEMON: Piece = Piece { asset: "playit-linux-amd64", dest: "/usr/local/bin/playitd", sha256: "9d27a8dbb4d2d8a7107fcabfa33028c789a30a64c97a36447fe4afb94e08dde9" };
pub const CLI: Piece = Piece { asset: "playit-cli-linux-amd64", dest: "/usr/local/bin/playit", sha256: "28dbb93f52acda04af99418d03042659356b33fa4279ef8e431f22cf84378a36" };

pub const USER: &str = "playit";
pub const STATE_DIR: &str = "/var/lib/playit";
pub const SOCKET: &str = "/var/lib/playit/playitd.sock";
pub const SECRET: &str = "/var/lib/playit/playit.toml";
pub const SETUP_LOG: &str = "/var/lib/playit/setup.log";
pub const SERVICE: &str = "playit";

fn at(ctx: &Ctx, abs: &str) -> PathBuf { ctx.root.join(abs.trim_start_matches('/')) }
fn marker(ctx: &Ctx) -> PathBuf { ctx.root.join("etc/exec85/tunnel-installed") }
pub fn installed(ctx: &Ctx) -> bool { marker(ctx).exists() }

fn run_ok(ctx: &Ctx, what: &str, cmd: &str, args: &[&str]) -> Result<String, String> {
    let (code, out) = ctx.runner.run(cmd, args);
    if code == 0 { Ok(out) } else { Err(format!("{what} failed: {}", out.lines().last().unwrap_or("").trim())) }
}

/// The claim link in the output of `playit setup`: https://playit.gg/claim/<letters and digits>
pub fn parse_claim_url(text: &str) -> Option<String> {
    for w in text.split_whitespace() {
        if let Some(code) = w.strip_prefix("https://playit.gg/claim/") {
            if !code.is_empty() && code.len() <= 64 && code.chars().all(|c| c.is_ascii_alphanumeric()) { return Some(w.to_string()); }
        }
    }
    None
}

fn download(ctx: &Ctx, p: &Piece) -> Result<(), String> {
    let tmp = at(ctx, &format!("run/exec85/{}", p.asset));
    std::fs::create_dir_all(tmp.parent().unwrap()).map_err(|e| e.to_string())?;
    let url = format!("{BASE_URL}/{RELEASE}/{}", p.asset);
    // The system's own downloader (like the Minecraft and Java downloads): it uses the system's name lookup, which works on every network
    // we have seen (curl's own lookup failed behind VirtualBox's NAT). It does not check the server's certificate, but the file's SHA-256
    // must be the pinned one, so a changed or fake file is never installed.
    let got = ctx.runner.run("wget", &["-q", "-O", &tmp.to_string_lossy(), &url]);
    if got.0 != 0 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("Downloading {} failed. Is this computer connected to the internet? (Try: wget -O /dev/null https://github.com) {}", p.asset, got.1.lines().last().unwrap_or("").trim()));
    }
    let (code, out) = ctx.runner.run("sha256sum", &[&tmp.to_string_lossy()]);
    let sum = out.split_whitespace().next().unwrap_or("").to_lowercase();
    if code != 0 || sum != p.sha256 { let _ = std::fs::remove_file(&tmp); return Err(format!("The downloaded file {} is not the one expected (its checksum does not match). Nothing was installed.", p.asset)); }
    let dest = at(ctx, p.dest);
    std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
    std::fs::copy(&tmp, &dest).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

fn ensure_user(ctx: &Ctx) -> Result<(), String> {
    let has = || crate::account::users(ctx.root).iter().any(|u| u.name == USER);
    if has() { return Ok(()); }
    ctx.runner.run("addgroup", &["-S", USER]);
    run_ok(ctx, "Creating the tunnel's own user", "adduser", &["-S", "-D", "-H", "-h", STATE_DIR, "-s", "/sbin/nologin", "-G", USER, USER])?;
    if has() { Ok(()) } else { Err("The tunnel's own user could not be created.".into()) }
}

pub fn install(ctx: &Ctx) -> Result<String, String> {
    if installed(ctx) { return Err("The internet tunnel is already installed.".into()); }
    download(ctx, &DAEMON)?;
    download(ctx, &CLI)?;
    ensure_user(ctx)?;
    let state = at(ctx, STATE_DIR);
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o750)).map_err(|e| e.to_string())?;
    run_ok(ctx, "Giving the tunnel its folder", "chown", &["-R", &format!("{USER}:{USER}"), STATE_DIR])?;
    let def = ServiceDef { name: SERVICE.into(), description: "playit.gg tunnel".into(), command: format!("{} --socket-path {SOCKET} --secret-path {SECRET}", DAEMON.dest),
        dir: STATE_DIR.into(), user: USER.into(), env: vec![], autostart: true, ports: String::new() };
    crate::helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&def)])?;
    crate::helper::handle(ctx, &["svc-start".to_string(), SERVICE.to_string()])?;
    std::fs::create_dir_all(ctx.root.join("etc/exec85")).map_err(|e| e.to_string())?;
    std::fs::write(marker(ctx), RELEASE).map_err(|e| e.to_string())?;
    Ok("The tunnel program is installed and running. Next: get your claim link and open it on a phone or computer.".into())
}

pub fn secret_present(ctx: &Ctx) -> bool { std::fs::metadata(at(ctx, SECRET)).map_or(false, |m| m.len() > 0) }

/// Ask the agent for a claim link (`playit setup` prints it and then waits until the link has been opened and finished).
pub fn claim(ctx: &Ctx) -> Result<String, String> {
    if !installed(ctx) { return Err("Install the internet tunnel first.".into()); }
    if secret_present(ctx) { return Err("This computer is already connected to a playit.gg account. Use Reset to start over with another one.".into()); }
    ctx.runner.run("pkill", &["-f", &format!("{} --socket-path {SOCKET} setup", CLI.dest)]);
    let log = at(ctx, SETUP_LOG);
    let _ = std::fs::remove_file(&log);
    let script = format!("setsid {} --socket-path {SOCKET} setup > {SETUP_LOG} 2>&1 < /dev/null &", CLI.dest);
    ctx.runner.run("sh", &["-c", &script]);
    for _ in 0..50 {
        if let Some(u) = std::fs::read_to_string(&log).ok().and_then(|t| parse_claim_url(&t)) { return Ok(u); }
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
    Err("The claim link did not appear. Is this computer connected to the internet? Check the log (logs playit) and try again.".into())
}

#[derive(Debug, PartialEq, Eq, Default, Clone)]
pub struct Info { pub installed: bool, pub service: String, pub secret: bool, pub claim: Option<String>, pub agent: Vec<String> }

/// `installed=yes service=running secret=no claim=URL`, then `---` and what the agent itself reports.
pub fn status(ctx: &Ctx) -> String {
    let inst = installed(ctx);
    let service = std::fs::read_to_string(ctx.root.join("etc/exec85/services").join(format!("{SERVICE}.conf"))).ok().and_then(|t| services::parse_conf(&t))
        .map_or("none".to_string(), |d| match crate::helper::status_of(ctx, &d) { services::Status::Running => "running", services::Status::Stopped => "stopped", services::Status::Crashed => "crashed", services::Status::Unknown => "unknown" }.to_string());
    let secret = secret_present(ctx);
    let claim = if secret { None } else { std::fs::read_to_string(at(ctx, SETUP_LOG)).ok().and_then(|t| parse_claim_url(&t)) };
    let mut out = format!("installed={}\nservice={service}\nsecret={}\nclaim={}\n---\n", if inst { "yes" } else { "no" }, if secret { "yes" } else { "no" }, claim.as_deref().unwrap_or("-"));
    if inst && service == "running" {
        let (code, text) = ctx.runner.run(CLI.dest, &["--socket-path", SOCKET, "status"]);
        if code == 0 { for l in text.lines().take(30) { out.push_str(l); out.push('\n'); } }
    }
    out
}

pub fn parse_status(text: &str) -> Info {
    let mut i = Info::default();
    let (head, tail) = text.split_once("---\n").unwrap_or((text, ""));
    for l in head.lines() {
        match l.split_once('=') {
            Some(("installed", v)) => i.installed = v == "yes",
            Some(("service", v)) => i.service = v.to_string(),
            Some(("secret", v)) => i.secret = v == "yes",
            Some(("claim", v)) if v != "-" => i.claim = Some(v.to_string()),
            _ => {}
        }
    }
    i.agent = tail.lines().map(String::from).collect();
    i
}

/// Forget the playit.gg account on this computer (to connect another one).
pub fn reset(ctx: &Ctx) -> Result<String, String> {
    if !installed(ctx) { return Err("The internet tunnel is not installed.".into()); }
    ctx.runner.run("pkill", &["-f", &format!("{} --socket-path {SOCKET} setup", CLI.dest)]);
    let _ = crate::helper::handle(ctx, &["svc-stop".to_string(), SERVICE.to_string()]);
    let _ = std::fs::remove_file(at(ctx, SECRET));
    let _ = std::fs::remove_file(at(ctx, SETUP_LOG));
    crate::helper::handle(ctx, &["svc-start".to_string(), SERVICE.to_string()])?;
    Ok("The account was forgotten. Get a new claim link.".into())
}

/// Stop the tunnel and take the programs away. The account's key stays in its folder, so installing again does not need a new claim.
pub fn remove(ctx: &Ctx) -> Result<String, String> {
    if !installed(ctx) { return Err("The internet tunnel is not installed.".into()); }
    ctx.runner.run("pkill", &["-f", &format!("{} --socket-path {SOCKET} setup", CLI.dest)]);
    let _ = crate::helper::handle(ctx, &["svc-remove".to_string(), SERVICE.to_string()]);
    for p in [&DAEMON, &CLI] { let _ = std::fs::remove_file(at(ctx, p.dest)); }
    let _ = std::fs::remove_file(marker(ctx));
    Ok(format!("The internet tunnel is stopped and removed. Its account key was kept in {STATE_DIR}."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, tamper: bool, offline: bool }
    fn fake() -> Fake { Fake { calls: RefCell::new(vec![]), tamper: false, offline: false } }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match c {
                "wget" => { if self.offline { return (1, "wget: bad address 'github.com'".into()); } let to = a[a.iter().position(|x| *x == "-O").unwrap() + 1]; std::fs::write(to, "BINARY").unwrap(); (0, String::new()) }
                "sha256sum" => { let want = if a[0].ends_with(CLI.asset) { CLI.sha256 } else { DAEMON.sha256 }; (0, format!("{}  {}\n", if self.tamper { "0".repeat(64) } else { want.to_string() }, a[0])) }
                _ => (0, String::new()),
            }
        }
    }
    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-tunnel-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\nplayit:x:100:101::/var/lib/playit:/sbin/nologin\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:\n").unwrap();
        d
    }

    #[test]
    fn the_claim_link_is_found_in_the_agents_output() {
        let out = "Open this link to finish setting up playit:\nhttps://playit.gg/claim/e429984529\nOpen this link to finish setting up playit:\nhttps://playit.gg/claim/e429984529\n";
        assert_eq!(parse_claim_url(out).as_deref(), Some("https://playit.gg/claim/e429984529"));
        for bad in ["", "https://playit.gg/claim/", "https://evil.example/claim/abc", "http://playit.gg/claim/abc", "https://playit.gg.evil.com/claim/abc", &format!("https://playit.gg/claim/{}", "a".repeat(100))] { assert_eq!(parse_claim_url(bad), None, "{bad:?}"); }
        assert_eq!(parse_claim_url("https://playit.gg/claim/a;rm -rf"), None, "only letters and digits");
    }

    #[test]
    fn it_is_installed_from_the_pinned_release_and_only_if_the_checksums_match() {
        let r = world("install"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        let msg = install(&c).unwrap();
        assert!(msg.contains("claim link"), "{msg}");
        let calls = f.calls.borrow().join("\n");
        for want in ["https://github.com/playit-cloud/playit-agent/releases/download/v2.0.2/playit-linux-amd64", "https://github.com/playit-cloud/playit-agent/releases/download/v2.0.2/playit-cli-linux-amd64", "chown -R playit:playit /var/lib/playit", "rc-service exec85-playit start"] { assert!(calls.contains(want), "{want} in\n{calls}"); }
        assert_eq!(std::fs::metadata(r.join("usr/local/bin/playitd")).unwrap().permissions().mode() & 0o777, 0o755);
        assert!(r.join("usr/local/bin/playit").exists() && installed(&c));
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/playit.conf")).unwrap();
        assert!(conf.contains("command=/usr/local/bin/playitd --socket-path /var/lib/playit/playitd.sock --secret-path /var/lib/playit/playit.toml") && conf.contains("user=playit") && conf.contains("autostart=true"), "{conf}");
        assert_eq!(std::fs::metadata(r.join("var/lib/playit")).unwrap().permissions().mode() & 0o777, 0o750, "its folder is private");
        assert!(install(&c).unwrap_err().contains("already installed"));
        let _ = std::fs::remove_dir_all(&r);
        // a file that does not match is refused, nothing is installed, and the temporary file is gone
        let r = world("tamper"); let mut f = fake(); f.tamper = true; let c = Ctx { root: &r, runner: &f };
        let e = install(&c).unwrap_err();
        assert!(e.contains("not the one expected"), "{e}");
        assert!(!r.join("usr/local/bin/playitd").exists() && !installed(&c) && !r.join("run/exec85/playit-linux-amd64").exists());
        let _ = std::fs::remove_dir_all(&r);
        // no internet: a clear failure, nothing installed
        let r = world("offline"); let mut f = fake(); f.offline = true; let c = Ctx { root: &r, runner: &f };
        let e = install(&c).unwrap_err(); assert!(e.contains("failed") && e.contains("connected to the internet") && e.contains("bad address") && !installed(&c), "{e}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_tunnels_own_user_is_made_when_missing() {
        let r = world("user"); std::fs::write(r.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\n").unwrap();
        let f = fake(); let c = Ctx { root: &r, runner: &f };
        let e = install(&c).unwrap_err();     // the fake adduser does not really add one
        assert!(e.contains("own user"), "{e}");
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("addgroup -S playit") && calls.contains("adduser -S -D -H -h /var/lib/playit -s /sbin/nologin -G playit playit"), "{calls}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_claim_link_is_asked_for_and_read_from_the_agents_log() {
        let r = world("claim"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        assert!(claim(&c).unwrap_err().contains("Install"));
        install(&c).unwrap();
        // what `playit setup` writes while it waits (the fake does not start it, so the log is written here)
        std::fs::write(r.join("var/lib/playit/setup.log"), "Open this link to finish setting up playit:\nhttps://playit.gg/claim/abc123\n").unwrap();
        let before = f.calls.borrow().len();
        // the old log is deleted first, so write it again from a thread shortly after (as the real program does)
        let log = r.join("var/lib/playit/setup.log");
        let t = std::thread::spawn(move || { std::thread::sleep(std::time::Duration::from_millis(100)); std::fs::write(log, "Open this link to finish setting up playit:\nhttps://playit.gg/claim/abc123\n").unwrap(); });
        assert_eq!(claim(&c).unwrap(), "https://playit.gg/claim/abc123");
        t.join().unwrap();
        let calls = f.calls.borrow()[before..].join("\n");
        assert!(calls.contains("pkill -f /usr/local/bin/playit --socket-path /var/lib/playit/playitd.sock setup") && calls.contains("setsid /usr/local/bin/playit --socket-path /var/lib/playit/playitd.sock setup"), "{calls}");
        let s = status(&c);
        let i = parse_status(&s);
        assert!(i.installed && !i.secret && i.claim.as_deref() == Some("https://playit.gg/claim/abc123"), "{s}");
        // once the account is connected a new link is refused, and the old link is no longer shown
        std::fs::write(r.join("var/lib/playit/playit.toml"), "secret_key = \"x\"\n").unwrap();
        assert!(claim(&c).unwrap_err().contains("already connected"));
        let i = parse_status(&status(&c)); assert!(i.secret && i.claim.is_none(), "{i:?}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn status_reads_back_and_reset_and_remove_clean_up() {
        let r = world("status"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        let s = status(&c); let i = parse_status(&s);
        assert!(!i.installed && i.service == "none" && !i.secret && i.agent.is_empty(), "{s}");
        install(&c).unwrap();
        std::fs::write(r.join("var/lib/playit/playit.toml"), "secret_key = \"x\"\n").unwrap();
        assert!(reset(&c).unwrap().contains("forgotten") && !secret_present(&c));
        assert!(remove(&c).unwrap().contains("removed") && !installed(&c) && !r.join("usr/local/bin/playitd").exists() && !r.join("etc/exec85/services/playit.conf").exists());
        assert!(remove(&c).is_err() && reset(&c).is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}
