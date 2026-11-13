//! Quick installers ("recipes"): a website, Syncthing, Transmission ... Each recipe says what it needs, which options to ask
//! for, and how to install itself from generic parts: `apk` packages, a service definition, a firewall rule, and files for
//! the secrets (mode 0600) so that a password never lands in a service definition (which everyone can read) or a log.
use crate::helper::Ctx;
use crate::services::{self, ServiceDef};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

pub struct OptionDef { pub key: &'static str, pub label: &'static str, pub default: &'static str, pub secret: bool }
pub struct Recipe { pub id: &'static str, pub title: &'static str, pub blurb: &'static str, pub needs: &'static str, pub options: &'static [OptionDef] }

pub const RECIPES: [Recipe; 3] = [
    Recipe { id: "website", title: "Website", blurb: "serve a folder of web pages (nginx)", needs: "about 10 MB of disk, a few MB of memory",
        options: &[OptionDef { key: "folder", label: "Folder with the web pages", default: "/srv/www/site", secret: false }, OptionDef { key: "port", label: "Port", default: "8090", secret: false }] },
    Recipe { id: "syncthing", title: "Syncthing", blurb: "keep folders in sync between your devices", needs: "about 50 MB of disk, 100 MB of memory",
        options: &[OptionDef { key: "folder", label: "Folder for its settings and files", default: "/srv/syncthing", secret: false }, OptionDef { key: "gui_user", label: "Web page user name", default: "admin", secret: false }, OptionDef { key: "gui_password", label: "Web page password (8 or more characters)", default: "", secret: true }] },
    Recipe { id: "transmission", title: "Transmission", blurb: "download torrents, with a web page", needs: "about 20 MB of disk, 50 MB of memory",
        options: &[OptionDef { key: "folder", label: "Folder for downloads and settings", default: "/srv/transmission", secret: false }, OptionDef { key: "port", label: "Web page port", default: "9091", secret: false }, OptionDef { key: "user", label: "Web page user name", default: "admin", secret: false }, OptionDef { key: "password", label: "Web page password (8 or more characters)", default: "", secret: true }] },
];

pub fn find(id: &str) -> Option<&'static Recipe> { RECIPES.iter().find(|r| r.id == id) }

fn marker(ctx: &Ctx, id: &str) -> std::path::PathBuf { ctx.root.join("etc/exec85/recipes").join(id) }
pub fn installed(ctx: &Ctx, id: &str) -> bool { marker(ctx, id).exists() }
pub fn list(ctx: &Ctx) -> String { RECIPES.iter().map(|r| format!("{} {}\n", r.id, if installed(ctx, r.id) { "installed" } else { "no" })).collect() }

fn value<'a>(values: &'a [(String, String)], r: &'a Recipe, key: &str) -> &'a str {
    values.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()).or_else(|| r.options.iter().find(|o| o.key == key).map(|o| o.default)).unwrap_or("")
}

/// A folder the recipe may use: under /srv or /home, and made only of plain characters (so it is safe in a config file).
pub fn valid_folder(f: &str) -> bool {
    (f.starts_with("/srv/") || f.starts_with("/home/")) && f.len() <= 120 && !f.contains("..") && !f.contains("//") && f.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))
}
pub fn valid_port(p: &str) -> Option<u16> { p.parse::<u16>().ok().filter(|p| *p >= 1) }
pub fn valid_login_name(u: &str) -> bool { !u.is_empty() && u.len() <= 32 && u.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') }
pub fn valid_password(p: &str) -> bool { (8..=64).contains(&p.chars().count()) && p.chars().all(|c| !c.is_control()) }

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() { match c { '"' => o.push_str("\\\""), '\\' => o.push_str("\\\\"), '\n' => o.push_str("\\n"), '\r' => o.push_str("\\r"), '\t' => o.push_str("\\t"), c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)), c => o.push(c) } }
    o.push('"');
    o
}

fn run_ok(ctx: &Ctx, what: &str, cmd: &str, args: &[&str]) -> Result<String, String> {
    let (code, out) = ctx.runner.run(cmd, args);
    if code == 0 { Ok(out) } else { Err(format!("{what} failed: {}", out.lines().last().unwrap_or("").trim())) }
}

fn make_dir(ctx: &Ctx, dir: &Path, mode: u32) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
    let _ = ctx;
    Ok(())
}

fn rel(ctx: &Ctx, p: &Path) -> String { crate::mc::absolute_in(ctx.root, p) }

/// Define the service, start it and note that the recipe is installed.
fn finish_service(ctx: &Ctx, id: &str, def: ServiceDef) -> Result<(), String> {
    crate::helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&def)])?;
    crate::helper::handle(ctx, &["svc-start".to_string(), def.name.clone()])?;
    mark(ctx, id)
}

fn mark(ctx: &Ctx, id: &str) -> Result<(), String> {
    std::fs::create_dir_all(ctx.root.join("etc/exec85/recipes")).map_err(|e| e.to_string())?;
    std::fs::write(marker(ctx, id), "").map_err(|e| e.to_string())
}

pub fn install(ctx: &Ctx, id: &str, values: &[(String, String)], user: &str) -> Result<String, String> {
    let r = find(id).ok_or("There is no such installer.")?;
    if !crate::account::users(ctx.root).iter().any(|u| u.name == user) || !crate::ssh::valid_user(user) { return Err("Choose a normal user for this.".into()); }
    if installed(ctx, id) { return Err(format!("{} is already installed.", r.title)); }
    match id {
        "website" => website(ctx, r, values, user),
        "syncthing" => syncthing(ctx, r, values, user),
        "transmission" => transmission(ctx, r, values, user),
        _ => Err("There is no such installer.".into()),
    }
}

fn website(ctx: &Ctx, r: &Recipe, v: &[(String, String)], user: &str) -> Result<String, String> {
    let folder = value(v, r, "folder");
    let port = valid_port(value(v, r, "port")).filter(|p| *p != 22).ok_or("Choose a port from 1 to 65535 (not 22).")?;
    if !valid_folder(folder) { return Err("The folder must be under /srv or /home and may only contain letters, digits and / _ . -".into()); }
    run_ok(ctx, "Installing nginx", "apk", &["add", "--no-progress", "nginx"])?;
    let dir = ctx.root.join(folder.trim_start_matches('/'));
    for parent in dir.ancestors().skip(1).take_while(|p| p.starts_with(ctx.root.join("srv")) || p.starts_with(ctx.root.join("home"))) { if !parent.exists() { make_dir(ctx, parent, 0o755)?; } }
    make_dir(ctx, &dir, 0o755)?;
    let index = dir.join("index.html");
    if !index.exists() { std::fs::write(&index, "<!doctype html>\n<meta charset=\"utf-8\"><title>It works</title>\n<h1>It works</h1>\n<p>This page is served by your Exec85 computer. Put your own pages in this folder.</p>\n").map_err(|e| e.to_string())?; std::fs::set_permissions(&index, std::fs::Permissions::from_mode(0o644)).map_err(|e| e.to_string())?; }
    run_ok(ctx, "Giving the folder to the user", "chown", &["-R", user, &dir.to_string_lossy()])?;
    // nginx runs as its own user: it must be able to walk through every folder on the way (in a private home folder it cannot),
    // so those get "others may enter" (not "others may list or read"), and the pages themselves must be readable.
    let home = ctx.root.join("home");
    if dir.starts_with(&home) {
        for p in dir.ancestors().take_while(|p| p.starts_with(&home) && *p != home) {
            if let Ok(md) = std::fs::metadata(p) { let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(md.permissions().mode() | 0o001)); }
        }
    }
    run_ok(ctx, "Making the pages readable", "chmod", &["-R", "a+rX", &dir.to_string_lossy()])?;
    let conf_dir = ctx.root.join("etc/nginx/http.d");
    std::fs::create_dir_all(&conf_dir).map_err(|e| e.to_string())?;
    let conf = conf_dir.join("default.conf");
    let orig = conf_dir.join("default.conf.orig");
    if conf.exists() && !orig.exists() { let _ = std::fs::copy(&conf, &orig); }
    std::fs::write(&conf, format!("# Generated by Exec85 (the website installer).\nserver {{\n\tlisten {port} default_server;\n\tlisten [::]:{port} default_server;\n\tserver_name _;\n\troot {folder};\n\tindex index.html index.htm;\n\tserver_tokens off;\n\tlocation / {{\n\t\ttry_files $uri $uri/ =404;\n\t}}\n}}\n")).map_err(|e| e.to_string())?;
    run_ok(ctx, "The nginx settings are not valid", "nginx", &["-t"])?;
    crate::helper::handle(ctx, &["fw-allow".into(), "website".into(), format!("{port}/tcp"), "local".into()])?;
    ctx.runner.run("rc-update", &["add", "nginx", "default"]);
    run_ok(ctx, "Starting nginx", "rc-service", &["nginx", "restart"])?;
    mark(ctx, "website")?;
    Ok(format!("Your website is running on port {port}. Put pages in {folder}. On your home network it is at http://{}:{port}/ (also reachable by the device name once that feature is on). To show it to the internet you must forward the port on your router.", crate::apps::network::local_ip().unwrap_or_else(|| "this-computer".into())))
}

fn syncthing(ctx: &Ctx, r: &Recipe, v: &[(String, String)], user: &str) -> Result<String, String> {
    let folder = value(v, r, "folder");
    let (gu, gp) = (value(v, r, "gui_user"), value(v, r, "gui_password"));
    if !valid_folder(folder) { return Err("The folder must be under /srv or /home and may only contain letters, digits and / _ . -".into()); }
    if !valid_login_name(gu) { return Err("The user name may only have letters, digits and _ - .".into()); }
    if !valid_password(gp) { return Err("The web page password needs 8 or more characters.".into()); }
    run_ok(ctx, "Installing Syncthing", "apk", &["add", "--no-progress", "syncthing"])?;
    let base = ctx.root.join(folder.trim_start_matches('/'));
    let cfg = base.join("config");
    make_dir(ctx, &base, 0o755)?;
    make_dir(ctx, &cfg, 0o700)?;
    make_dir(ctx, &base.join("Sync"), 0o755)?;
    // The settings are made once, with the web page password already in them (stored hashed): the page is never open without a password.
    run_ok(ctx, "Preparing Syncthing", "syncthing", &["generate", &format!("--home={}", cfg.to_string_lossy()), &format!("--gui-user={gu}"), &format!("--gui-password={gp}")])?;
    run_ok(ctx, "Giving the folder to the user", "chown", &["-R", user, &base.to_string_lossy()])?;
    let def = ServiceDef { name: "syncthing".into(), description: "Syncthing".into(), command: format!("syncthing serve --no-browser --no-restart --gui-address=0.0.0.0:8384 --home={}", rel(ctx, &cfg)), dir: rel(ctx, &base), user: user.into(), env: vec![("STNOUPGRADE".into(), "1".into())], autostart: true, ports: "8384/tcp,22000/tcp,22000/udp,21027/udp".into() };
    finish_service(ctx, "syncthing", def)?;
    Ok(format!("Syncthing is starting. Its web page is at http://{}:8384/ (user {gu}, the password you chose). Files you add in the web page go in {}/Sync by default.", crate::apps::network::local_ip().unwrap_or_else(|| "this-computer".into()), folder))
}

fn transmission(ctx: &Ctx, r: &Recipe, v: &[(String, String)], user: &str) -> Result<String, String> {
    let folder = value(v, r, "folder");
    let port = valid_port(value(v, r, "port")).filter(|p| *p != 22).ok_or("Choose a port from 1 to 65535 (not 22).")?;
    let (tu, tp) = (value(v, r, "user"), value(v, r, "password"));
    if !valid_folder(folder) { return Err("The folder must be under /srv or /home and may only contain letters, digits and / _ . -".into()); }
    if !valid_login_name(tu) { return Err("The user name may only have letters, digits and _ - .".into()); }
    if !valid_password(tp) { return Err("The web page password needs 8 or more characters.".into()); }
    run_ok(ctx, "Installing Transmission", "apk", &["add", "--no-progress", "transmission-daemon"])?;
    let base = ctx.root.join(folder.trim_start_matches('/'));
    let (cfg, downloads) = (base.join("config"), base.join("downloads"));
    make_dir(ctx, &base, 0o755)?; make_dir(ctx, &cfg, 0o700)?; make_dir(ctx, &downloads, 0o755)?;
    // The password goes into the settings file (readable only by the user); Transmission turns it into a hash the first time it starts.
    let settings = cfg.join("settings.json");
    std::fs::write(&settings, format!("{{\n\t\"rpc-enabled\": true,\n\t\"rpc-port\": {port},\n\t\"rpc-bind-address\": \"0.0.0.0\",\n\t\"rpc-authentication-required\": true,\n\t\"rpc-username\": {},\n\t\"rpc-password\": {},\n\t\"rpc-whitelist-enabled\": false,\n\t\"rpc-host-whitelist-enabled\": false,\n\t\"download-dir\": {},\n\t\"incomplete-dir-enabled\": false\n}}\n", json_str(tu), json_str(tp), json_str(&rel(ctx, &downloads)))).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    run_ok(ctx, "Giving the folder to the user", "chown", &["-R", user, &base.to_string_lossy()])?;
    let def = ServiceDef { name: "transmission".into(), description: "Transmission".into(), command: format!("transmission-daemon -f --config-dir {}", rel(ctx, &cfg)), dir: rel(ctx, &base), user: user.into(), env: vec![], autostart: true, ports: format!("{port}/tcp") };
    finish_service(ctx, "transmission", def)?;
    Ok(format!("Transmission is starting. Its web page is at http://{}:{port}/ (user {tu}). Downloads go to {folder}/downloads.", crate::apps::network::local_ip().unwrap_or_else(|| "this-computer".into())))
}

/// Stop what a recipe started and forget it. Files you made (pages, downloads, synced folders) and the installed packages are kept.
pub fn remove(ctx: &Ctx, id: &str) -> Result<String, String> {
    let r = find(id).ok_or("There is no such installer.")?;
    if !installed(ctx, id) { return Err(format!("{} is not installed.", r.title)); }
    match id {
        "website" => { ctx.runner.run("rc-service", &["nginx", "stop"]); ctx.runner.run("rc-update", &["del", "nginx", "default"]); let _ = crate::helper::handle(ctx, &["fw-remove".into(), "website".into()]); }
        _ => { let _ = crate::helper::handle(ctx, &["svc-remove".into(), id.to_string()]); }
    }
    let _ = std::fs::remove_file(marker(ctx, id));
    Ok(format!("{} is stopped and removed. Your files were kept.", r.title))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;
    use std::path::PathBuf;

    struct Fake { calls: RefCell<Vec<String>>, fail: RefCell<Option<&'static str>> }
    fn fake() -> Fake { Fake { calls: RefCell::new(vec![]), fail: RefCell::new(None) } }
    impl Runner for Fake { fn run(&self, c: &str, a: &[&str]) -> (i32, String) { self.calls.borrow_mut().push(format!("{c} {}", a.join(" "))); if self.fail.borrow().map_or(false, |f| c == f) { (1, "boom".into()) } else { (0, String::new()) } } }

    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-recipes-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc/nginx/http.d")).unwrap();
        std::fs::write(d.join("etc/nginx/http.d/default.conf"), "server { listen 80 default_server; return 404; }\n").unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\ntester:x:1000:1000::/home/tester:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:tester\n").unwrap();
        d
    }
    fn kv(p: &[(&str, &str)]) -> Vec<(String, String)> { p.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }

    #[test]
    fn folders_ports_names_and_passwords_are_checked() {
        for ok in ["/srv/www/site", "/home/tester/sites/my-site_1.0"] { assert!(valid_folder(ok), "{ok}"); }
        for bad in ["", "/etc/nginx", "/srv", "relative", "/srv/../etc", "/srv/a b", "/srv/a;b", "/srv/a{b}", "/srv//x", "/srv/a\"b", &format!("/srv/{}", "x".repeat(200))] { assert!(!valid_folder(bad), "{bad}"); }
        assert_eq!(valid_port("8090"), Some(8090)); assert!(valid_port("0").is_none() && valid_port("70000").is_none() && valid_port("x").is_none());
        assert!(valid_login_name("admin") && !valid_login_name("a b") && !valid_login_name(""));
        assert!(valid_password("12345678") && !valid_password("short") && !valid_password("tab\there1"));
        assert_eq!(json_str("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
    }

    #[test]
    fn a_website_is_installed_configured_opened_and_started() {
        let r = world("web"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        let msg = install(&c, "website", &kv(&[("port", "8090")]), "tester").unwrap();
        assert!(msg.contains("port 8090"), "{msg}");
        let conf = std::fs::read_to_string(r.join("etc/nginx/http.d/default.conf")).unwrap();
        assert!(conf.contains("listen 8090 default_server") && conf.contains("root /srv/www/site;") && conf.contains("server_tokens off"), "{conf}");
        assert!(r.join("etc/nginx/http.d/default.conf.orig").exists(), "the original file is kept");
        assert!(std::fs::read_to_string(r.join("srv/www/site/index.html")).unwrap().contains("It works"));
        let calls = f.calls.borrow().join("\n");
        for want in ["apk add --no-progress nginx", "chown -R tester ", "nginx -t", "rc-update add nginx default", "rc-service nginx restart", "nft -f "] { assert!(calls.contains(want), "{want} in:\n{calls}"); }
        assert!(std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap().contains("tcp dport 8090 accept comment \"website\""));
        assert!(installed(&c, "website") && list(&c).contains("website installed") && list(&c).contains("syncthing no"));
        assert!(install(&c, "website", &[], "tester").unwrap_err().contains("already installed"));
        assert!(remove(&c, "website").unwrap().contains("files were kept") && !installed(&c, "website") && r.join("srv/www/site/index.html").exists());
        assert!(!std::fs::read_to_string(r.join("etc/exec85/firewall.nft")).unwrap().contains("8090"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn syncthing_gets_its_password_before_it_ever_runs_and_runs_as_a_service() {
        let r = world("st"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        install(&c, "syncthing", &kv(&[("gui_password", "correct horse")]), "tester").unwrap();
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("syncthing generate --home=") && calls.contains("--gui-user=admin --gui-password=correct horse"), "{calls}");
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/syncthing.conf")).unwrap();
        assert!(conf.contains("--gui-address=0.0.0.0:8384") && !conf.contains("correct horse") && conf.contains("ports=8384/tcp,22000/tcp,22000/udp,21027/udp"), "the password is not in the service definition: {conf}");
        assert!(calls.contains("rc-service exec85-syncthing start"));
        assert_eq!(std::fs::metadata(r.join("srv/syncthing/config")).unwrap().permissions().mode() & 0o777, 0o700, "its settings folder is private");
        assert!(install(&c, "syncthing", &kv(&[("gui_password", "short")]), "tester").is_err() || installed(&c, "syncthing"));
        let _ = std::fs::remove_dir_all(&r);
        let r = world("st2"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        assert!(install(&c, "syncthing", &kv(&[("gui_password", "short")]), "tester").unwrap_err().contains("8 or more"));
        assert!(install(&c, "syncthing", &kv(&[("gui_password", "longenough1"), ("gui_user", "a b")]), "tester").is_err());
        assert!(f.calls.borrow().is_empty(), "nothing was installed for refused requests");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn transmission_keeps_its_password_in_a_private_settings_file() {
        let r = world("tr"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        install(&c, "transmission", &kv(&[("password", "pa\"ss word!"), ("port", "9091")]), "tester").unwrap();
        let s = std::fs::read_to_string(r.join("srv/transmission/config/settings.json")).unwrap();
        assert!(s.contains("\"rpc-password\": \"pa\\\"ss word!\"") && s.contains("\"rpc-authentication-required\": true") && s.contains("\"download-dir\": \"/srv/transmission/downloads\""), "{s}");
        assert_eq!(std::fs::metadata(r.join("srv/transmission/config/settings.json")).unwrap().permissions().mode() & 0o777, 0o600);
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/transmission.conf")).unwrap();
        assert!(!conf.contains("pa") || !conf.contains("word!"), "the password is not in the service definition");
        assert!(conf.contains("transmission-daemon -f --config-dir /srv/transmission/config") && conf.contains("ports=9091/tcp"), "{conf}");
        assert!(!f.calls.borrow().join("\n").contains("word!"), "and not on any command line");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn failures_are_explained_and_bad_requests_run_nothing() {
        let r = world("bad"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        for (id, vals) in [("website", kv(&[("port", "22")])), ("website", kv(&[("folder", "/etc")])), ("website", kv(&[("port", "x")])), ("nosuch", vec![])] { assert!(install(&c, id, &vals, "tester").is_err(), "{id} {vals:?}"); }
        assert!(install(&c, "website", &[], "root").is_err() && install(&c, "website", &[], "ghost").is_err());
        assert!(f.calls.borrow().is_empty());
        *f.fail.borrow_mut() = Some("apk");
        assert!(install(&c, "website", &[], "tester").unwrap_err().contains("Installing nginx failed"));
        *f.fail.borrow_mut() = Some("nginx");
        assert!(install(&c, "website", &[], "tester").unwrap_err().contains("not valid"));
        assert!(!installed(&c, "website"), "a failed install is not recorded as installed");
        assert!(remove(&c, "website").is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}
