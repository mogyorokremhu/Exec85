//! Tailscale: a private network between your own devices (phone, PC, this server), reachable from anywhere.
//!
//! Installing means: the Alpine package, a service for its daemon, and a sign-in (you open an address Tailscale gives on your
//! phone or PC and log in). Traffic that comes through the network is treated like home-network traffic by the firewall.
use crate::helper::{self, Ctx};
use crate::services::{self, ServiceDef};

/// Not "tailscaled": Alpine's own package has an init script of that name.
pub const SVC: &str = "tailscale-daemon";
const OLD_SVC: &str = "tailscaled";
const RUN_SCRIPT: &str = "/etc/exec85/tailscale-run";
/// What the service runs (written by `install`, so that an update never has to carry it): the kernel's tun module, the folders, then the daemon.
const RUN_TEXT: &str = "#!/bin/sh\n# Starts the Tailscale daemon (the service tailscale-daemon runs this).\nmodprobe tun 2>/dev/null\nmkdir -p /var/lib/tailscale /var/run/tailscale\nexec /usr/sbin/tailscaled --state=/var/lib/tailscale/tailscaled.state --socket=/var/run/tailscale/tailscaled.sock\n";

fn have(ctx: &Ctx, prog: &str) -> bool { ["usr/bin", "usr/sbin", "bin", "sbin"].iter().any(|d| ctx.root.join(d).join(prog).exists()) }
fn defined(ctx: &Ctx) -> bool { ctx.root.join("etc/exec85/services").join(format!("{SVC}.conf")).exists() }

/// The text of a JSON string value: `"Key": "value"` (with or without the space).
fn json_string(text: &str, key: &str) -> Option<String> {
    let at = text.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    let end = rest.find('"')?;
    Some(rest[..end].replace("\\/", "/"))
}

/// The first Tailscale address (100.x.y.z) in the `TailscaleIPs` list.
fn first_address(text: &str) -> Option<String> {
    let at = text.find("\"TailscaleIPs\"")?;
    let rest = &text[at..];
    let open = rest.find('[')?;
    let close = rest.find(']')?;
    rest[open + 1..close].split(',').map(|s| s.trim().trim_matches('"')).find(|s| s.starts_with("100.")).map(String::from)
}

/// `installed=yes|no`, `running=yes|no`, `state=` (NeedsLogin, Running, Stopped, ... or -), `address=`, `login=` (an address to open to sign in), `name=`.
pub fn status(ctx: &Ctx) -> String {
    let installed = have(ctx, "tailscaled") && have(ctx, "tailscale");
    let (code, out) = if installed && defined(ctx) { ctx.runner.run("tailscale", &["status", "--json"]) } else { (1, String::new()) };
    let up = code == 0 && out.contains("BackendState");
    let state = if up { json_string(&out, "BackendState").unwrap_or_else(|| "-".into()) } else { "-".into() };
    let login = if up { json_string(&out, "AuthURL").filter(|u| u.starts_with("https://")).unwrap_or_else(|| "-".into()) } else { "-".into() };
    let name = if up { json_string(&out, "HostName").unwrap_or_else(|| "-".into()) } else { "-".into() };
    let address = if up { first_address(&out).unwrap_or_else(|| "-".into()) } else { "-".into() };
    format!("installed={}\nrunning={}\nstate={state}\naddress={address}\nlogin={login}\nname={name}\n", if installed { "yes" } else { "no" }, if up { "yes" } else { "no" })
}

/// The system's start messages without the harmless noise about services of the base system.
fn useful(msg: &str) -> String {
    let keep: Vec<&str> = msg.split(|c| c == '\n' || c == '*').map(str::trim).filter(|l| !l.is_empty() && !l.contains("non existent service") && !l.contains("Caching service dependencies") && !l.starts_with('[')).collect();
    if keep.is_empty() { msg.trim().to_string() } else { keep.join(" / ") }
}

pub fn install(ctx: &Ctx) -> Result<String, String> {
    if !have(ctx, "tailscaled") {
        let (code, out) = ctx.runner.run("apk", &["add", "--no-progress", "tailscale"]);
        if code != 0 { return Err(format!("Could not install Tailscale (is the computer online?): {}", out.lines().last().unwrap_or("").trim())); }
    }
    // an earlier version used the name of Alpine's own init script: take that definition away
    if ctx.root.join("etc/exec85/services").join(format!("{OLD_SVC}.conf")).exists() { let _ = helper::handle(ctx, &["svc-stop".to_string(), OLD_SVC.to_string()]); let _ = helper::handle(ctx, &["svc-remove".to_string(), OLD_SVC.to_string()]); }
    {
        use std::os::unix::fs::PermissionsExt;
        let script = ctx.root.join(RUN_SCRIPT.trim_start_matches('/'));
        if let Some(dir) = script.parent() { std::fs::create_dir_all(dir).map_err(|e| e.to_string())?; }
        std::fs::write(&script, RUN_TEXT).map_err(|e| e.to_string())?;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    let d = ServiceDef { name: SVC.into(), description: "Tailscale (private network between your devices)".into(), command: RUN_SCRIPT.into(), dir: "/".into(), user: "root".into(), env: vec![], autostart: true, ports: String::new() };
    helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&d)])?;
    helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()]).map_err(|e| format!("Tailscale is installed but did not start: {}", useful(&e)))?;
    Ok("Tailscale is installed and running. Press L to sign in.".into())
}

/// Start the sign-in: Tailscale then offers an address to open on any device with a browser.
pub fn login(ctx: &Ctx) -> Result<String, String> {
    if !have(ctx, "tailscale") || !defined(ctx) { return Err("Install Tailscale first (press I).".into()); }
    // `up` waits for the sign-in; a short timeout starts it and returns, the daemon keeps waiting for you
    let (_, out) = ctx.runner.run("tailscale", &["up", "--timeout=6s"]);
    let url = out.split_whitespace().find(|w| w.starts_with("https://")).map(String::from)
        .or_else(|| json_string(&ctx.runner.run("tailscale", &["status", "--json"]).1, "AuthURL").filter(|u| u.starts_with("https://")));
    match url {
        Some(u) => Ok(format!("Open this address on your phone or PC and sign in: {u}")),
        None => Ok("Started. If it does not show an address yet, press R in a few seconds.".into()),
    }
}

pub fn logout(ctx: &Ctx) -> Result<String, String> {
    if !have(ctx, "tailscale") || !defined(ctx) { return Err("Tailscale is not installed.".into()); }
    let (code, out) = ctx.runner.run("tailscale", &["logout"]);
    if code != 0 { return Err(format!("Could not sign out: {}", out.lines().last().unwrap_or("").trim())); }
    Ok("Signed out. This computer is no longer on your Tailscale network.".into())
}

pub fn remove(ctx: &Ctx) -> Result<String, String> {
    if defined(ctx) { let _ = helper::handle(ctx, &["svc-stop".to_string(), SVC.to_string()]); helper::handle(ctx, &["svc-remove".to_string(), SVC.to_string()])?; }
    Ok("Tailscale is switched off. (The program stays installed; press I to switch it on again.)".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, json: String, up_out: String }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match (c, a.first().copied()) {
                ("tailscale", Some("status")) => (0, self.json.clone()),
                ("tailscale", Some("up")) => (1, self.up_out.clone()),
                _ => (0, String::new()),
            }
        }
    }
    fn world(tag: &str, installed: bool) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-ts-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc/exec85/services")).unwrap(); std::fs::create_dir_all(d.join("usr/sbin")).unwrap(); std::fs::create_dir_all(d.join("usr/bin")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\n").unwrap();
        if installed { std::fs::write(d.join("usr/sbin/tailscaled"), "").unwrap(); std::fs::write(d.join("usr/bin/tailscale"), "").unwrap(); }
        d
    }
    const NEEDS: &str = r#"{"Version":"1.86.2","BackendState": "NeedsLogin","AuthURL": "https://login.tailscale.com/a/abc123","Self":{"HostName":"mogyorokrem"},"TailscaleIPs":null}"#;
    const RUNNING: &str = r#"{"BackendState":"Running","AuthURL":"","TailscaleIPs":["100.101.102.103","fd7a:115c::1"],"Self":{"HostName":"mogyorokrem"}}"#;

    #[test]
    fn start_messages_lose_the_noise() {
        let m = " * Caching service dependencies ... [ ok ]\n * Starting tailscaled ...Service 'acpid' needs non existent service 'dev'\n * ERROR: tailscaled failed to start";
        assert_eq!(useful(m), "ERROR: tailscaled failed to start");
    }

    #[test]
    fn the_status_is_read_from_tailscales_answer() {
        let r = world("status", true);
        std::fs::write(r.join("etc/exec85/services/tailscale-daemon.conf"), "name=tailscale-daemon\n").unwrap();
        let f = Fake { calls: RefCell::new(vec![]), json: NEEDS.into(), up_out: String::new() };
        let t = status(&Ctx { root: &r, runner: &f });
        assert!(t.contains("installed=yes") && t.contains("running=yes") && t.contains("state=NeedsLogin") && t.contains("login=https://login.tailscale.com/a/abc123") && t.contains("address=-"), "{t}");
        let f = Fake { calls: RefCell::new(vec![]), json: RUNNING.into(), up_out: String::new() };
        let t = status(&Ctx { root: &r, runner: &f });
        assert!(t.contains("state=Running") && t.contains("address=100.101.102.103") && t.contains("login=-") && t.contains("name=mogyorokrem"), "{t}");
        let none = world("none", false);
        let t = status(&Ctx { root: &none, runner: &f });
        assert!(t.contains("installed=no") && t.contains("running=no") && t.contains("state=-"), "{t}");
    }

    #[test]
    fn installing_adds_the_package_and_a_service_and_login_shows_the_address() {
        let r = world("install", false);
        let f = Fake { calls: RefCell::new(vec![]), json: NEEDS.into(), up_out: "To authenticate, visit:\n\n\thttps://login.tailscale.com/a/xyz789\n".into() };
        let ctx = Ctx { root: &r, runner: &f };
        assert!(login(&ctx).unwrap_err().contains("Install Tailscale first"));
        let msg = install(&ctx).unwrap();
        assert!(msg.contains("installed"), "{msg}");
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/tailscale-daemon.conf")).unwrap();
        assert!(conf.contains("command=/etc/exec85/tailscale-run") && conf.contains("user=root") && conf.contains("name=tailscale-daemon"), "{conf}");
        let run = std::fs::read_to_string(r.join("etc/exec85/tailscale-run")).unwrap();
        assert!(run.starts_with("#!/bin/sh") && run.contains("modprobe tun") && run.contains("exec /usr/sbin/tailscaled"), "{run}");
        assert!(f.calls.borrow().iter().any(|c| c == "apk add --no-progress tailscale"), "{:?}", f.calls.borrow());
        std::fs::write(r.join("usr/sbin/tailscaled"), "").unwrap(); std::fs::write(r.join("usr/bin/tailscale"), "").unwrap();
        let l = login(&ctx).unwrap();
        assert!(l.contains("https://login.tailscale.com/a/xyz789"), "{l}");
        assert!(f.calls.borrow().iter().any(|c| c == "tailscale up --timeout=6s"));
        let before = f.calls.borrow().iter().filter(|c| c.starts_with("apk")).count();
        install(&ctx).unwrap();
        assert_eq!(f.calls.borrow().iter().filter(|c| c.starts_with("apk")).count(), before, "no second download when it is already there");
    }
}
