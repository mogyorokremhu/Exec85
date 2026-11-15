//! Switching web access on and off: the Exec85 screen in a browser (see `remote`).
//!
//! On means two services: `webaccess` (our own web server, listening on this computer only) and `webaccess-tls`
//! (stunnel, which does the encryption and listens on port 8443 on the network). The firewall opens 8443 for the
//! home network only, through the service's `ports`. The certificate is made here, once; its fingerprint is shown so a
//! person can check it in the browser's warning ("not trusted": a certificate made by the computer itself).
use crate::helper::{self, Ctx};
use crate::services::{self, ServiceDef};
use std::path::PathBuf;

pub const WEB: &str = "webaccess";
pub const TLS: &str = "webaccess-tls";
const DIR: &str = "etc/exec85/webaccess";

fn dir(ctx: &Ctx) -> PathBuf { ctx.root.join(DIR) }
fn cert(ctx: &Ctx) -> PathBuf { dir(ctx).join("cert.pem") }
fn key(ctx: &Ctx) -> PathBuf { dir(ctx).join("key.pem") }
fn conf(ctx: &Ctx) -> PathBuf { dir(ctx).join("stunnel.conf") }
fn conf_text() -> String {
    format!("foreground = yes\npid =\noutput = /var/log/exec85/{TLS}.stunnel.log\n\n[exec85-web]\naccept = {}\nconnect = 127.0.0.1:{}\ncert = /{DIR}/cert.pem\nkey = /{DIR}/key.pem\nprotocol = proxy\nsslVersion = all\noptions = NO_SSLv2\noptions = NO_SSLv3\noptions = NO_TLSv1\noptions = NO_TLSv1.1\n", crate::remote::PUBLIC_PORT, crate::remote::LISTEN_PORT)
}

pub fn enabled(ctx: &Ctx) -> bool { ctx.root.join("etc/exec85/services").join(format!("{WEB}.conf")).exists() && ctx.root.join("etc/exec85/services").join(format!("{TLS}.conf")).exists() }

/// The certificate's SHA-256 fingerprint (`AB:CD:...`), if there is one.
pub fn fingerprint(ctx: &Ctx) -> Option<String> {
    if !cert(ctx).exists() { return None; }
    let (code, out) = ctx.runner.run("openssl", &["x509", "-in", &cert(ctx).to_string_lossy(), "-noout", "-fingerprint", "-sha256"]);
    if code != 0 { return None; }
    out.lines().find_map(|l| l.split_once('=').map(|(_, v)| v.trim().to_string())).filter(|f| !f.is_empty())
}

/// `enabled=yes|no`, `port=`, `fingerprint=`, one per line.
pub fn status(ctx: &Ctx) -> String {
    format!("enabled={}\nport={}\nfingerprint={}\n", if enabled(ctx) { "yes" } else { "no" }, crate::remote::PUBLIC_PORT, fingerprint(ctx).unwrap_or_else(|| "-".into()))
}

fn have(ctx: &Ctx, prog: &str) -> bool { ["usr/bin", "usr/sbin", "bin", "sbin"].iter().any(|d| ctx.root.join(d).join(prog).exists()) }

fn make_certificate(ctx: &Ctx) -> Result<(), String> {
    if cert(ctx).exists() && key(ctx).exists() { return Ok(()); }
    std::fs::create_dir_all(dir(ctx)).map_err(|e| e.to_string())?;
    let host = std::fs::read_to_string(ctx.root.join("etc/hostname")).map(|s| s.trim().to_string()).ok().filter(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).unwrap_or_else(|| "exec85".into());
    let (code, out) = ctx.runner.run("openssl", &["req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:prime256v1", "-nodes", "-days", "3650", "-subj", &format!("/CN={host}"),
        "-addext", &format!("subjectAltName=DNS:{host},DNS:{host}.local"), "-keyout", &key(ctx).to_string_lossy(), "-out", &cert(ctx).to_string_lossy()]);
    if code != 0 { return Err(format!("Could not make the certificate: {}", out.lines().last().unwrap_or("").trim())); }
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(key(ctx), std::fs::Permissions::from_mode(0o600));
        let _ = std::fs::set_permissions(cert(ctx), std::fs::Permissions::from_mode(0o644));
    }
    Ok(())
}

fn def(name: &str, description: &str, command: &str, ports: &str) -> ServiceDef {
    ServiceDef { name: name.into(), description: description.into(), command: command.into(), dir: "/".into(), user: "root".into(), env: vec![], autostart: true, ports: ports.into() }
}

pub fn enable(ctx: &Ctx) -> Result<String, String> {
    if !have(ctx, "stunnel") || !have(ctx, "openssl") {
        // an update does not bring new packages: they come from the internet, once
        let (code, out) = ctx.runner.run("apk", &["add", "--no-progress", "stunnel", "openssl"]);
        if code != 0 { return Err(format!("Web access needs two small programs (stunnel and openssl) and they could not be downloaded. Is this computer connected to the internet? {}", out.lines().last().unwrap_or("").trim())); }
    }
    make_certificate(ctx)?;
    std::fs::write(conf(ctx), conf_text()).map_err(|e| e.to_string())?;
    for d in [def(WEB, "Exec85 web access (the screen in a browser)", &format!("/usr/bin/exec85 --remote"), ""),
              def(TLS, "Exec85 web access, encryption", &format!("stunnel /{DIR}/stunnel.conf"), &format!("{}/tcp", crate::remote::PUBLIC_PORT))] {
        helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&d)])?;
    }
    for n in [WEB, TLS] { helper::handle(ctx, &["svc-restart".to_string(), n.to_string()])?; }
    Ok(format!("Web access is on. Open https://<this computer's address>:{} in a browser and log in.", crate::remote::PUBLIC_PORT))
}

pub fn disable(ctx: &Ctx) -> Result<String, String> {
    for n in [TLS, WEB] {
        if ctx.root.join("etc/exec85/services").join(format!("{n}.conf")).exists() {
            let _ = helper::handle(ctx, &["svc-stop".to_string(), n.to_string()]);
            helper::handle(ctx, &["svc-remove".to_string(), n.to_string()])?;
        }
    }
    Ok("Web access is off.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, no_stunnel: bool, offline: bool }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match c {
                "openssl" if a[0] == "req" => { let at = |k: &str| a[a.iter().position(|x| *x == k).unwrap() + 1]; std::fs::write(at("-keyout"), "KEY").unwrap(); std::fs::write(at("-out"), "CERT").unwrap(); (0, String::new()) }
                "openssl" => (0, "sha256 Fingerprint=AB:CD:EF\n".into()),
                "apk" => if self.offline { (1, "ERROR: unable to select packages".into()) } else { (0, String::new()) },
                _ => (0, String::new()),
            }
        }
    }
    fn world(tag: &str, with_programs: bool) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-webaccess-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:\n").unwrap();
        std::fs::write(d.join("etc/hostname"), "lab85\n").unwrap();
        if with_programs { std::fs::create_dir_all(d.join("usr/bin")).unwrap(); for p in ["stunnel", "openssl"] { std::fs::write(d.join("usr/bin").join(p), "").unwrap(); } }
        d
    }

    #[test]
    fn it_is_switched_on_with_two_services_and_one_port_and_off_again() {
        let r = world("on", true);
        let f = Fake { calls: RefCell::new(vec![]), no_stunnel: false, offline: false };
        let c = Ctx { root: &r, runner: &f };
        assert!(status(&c).starts_with("enabled=no\n") && status(&c).contains("fingerprint=-"));
        enable(&c).unwrap();
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("-subj /CN=lab85") && calls.contains("DNS:lab85.local") && calls.contains("prime256v1"), "{calls}");
        assert!(!calls.contains("apk add"), "the programs are there already");
        assert_eq!(std::fs::read_to_string(r.join("etc/exec85/webaccess/key.pem")).unwrap(), "KEY");
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(r.join("etc/exec85/webaccess/key.pem")).unwrap().permissions().mode() & 0o777, 0o600, "the key is private");
        }
        let tls = std::fs::read_to_string(r.join("etc/exec85/services/webaccess-tls.conf")).unwrap();
        let web = std::fs::read_to_string(r.join("etc/exec85/services/webaccess.conf")).unwrap();
        assert!(tls.contains("ports=8443/tcp") && tls.contains("command=stunnel /etc/exec85/webaccess/stunnel.conf") && web.contains("command=/usr/bin/exec85 --remote") && web.contains("ports=\n"), "{tls}\n{web}");
        let sc = std::fs::read_to_string(r.join("etc/exec85/webaccess/stunnel.conf")).unwrap();
        assert!(sc.contains("accept = 8443") && sc.contains("connect = 127.0.0.1:8442") && sc.contains("NO_TLSv1.1"), "{sc}");
        assert!(status(&c).contains("enabled=yes") && status(&c).contains("fingerprint=AB:CD:EF"));
        for n in ["webaccess", "webaccess-tls"] { assert!(calls.contains(&format!("rc-service exec85-{n} restart")), "{n}: {calls}"); }
        // again: the certificate is kept (browsers remember it)
        let n = f.calls.borrow().iter().filter(|c| c.starts_with("openssl req")).count();
        enable(&c).unwrap();
        assert_eq!(f.calls.borrow().iter().filter(|c| c.starts_with("openssl req")).count(), n, "no second certificate");
        disable(&c).unwrap();
        assert!(!enabled(&c) && status(&c).starts_with("enabled=no") && r.join("etc/exec85/webaccess/cert.pem").exists(), "off, but the certificate stays");
        disable(&c).unwrap();
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn missing_programs_are_downloaded_once_and_the_reason_is_clear_when_that_fails() {
        let r = world("pkgs", false);
        let f = Fake { calls: RefCell::new(vec![]), no_stunnel: true, offline: false };
        let c = Ctx { root: &r, runner: &f };
        enable(&c).unwrap();
        assert!(f.calls.borrow().iter().any(|c| c == "apk add --no-progress stunnel openssl"));
        let r2 = world("offline", false);
        let f2 = Fake { calls: RefCell::new(vec![]), no_stunnel: true, offline: true };
        let e = enable(&Ctx { root: &r2, runner: &f2 }).unwrap_err();
        assert!(e.contains("connected to the internet") && !enabled(&Ctx { root: &r2, runner: &f2 }), "{e}");
        let _ = (std::fs::remove_dir_all(&r), std::fs::remove_dir_all(&r2));
    }
}
