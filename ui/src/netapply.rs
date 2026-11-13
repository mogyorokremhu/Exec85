//! Applying the network settings: bring each interface to the state the configuration asks for.
use crate::helper::Ctx;
use crate::netconf::{self, Config, Mode};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub fn conf_path(root: &Path) -> PathBuf { root.join("etc/exec85/network.conf") }
pub fn load(root: &Path) -> Config { netconf::parse_config(&std::fs::read_to_string(conf_path(root)).unwrap_or_default()) }

pub fn save(root: &Path, c: &Config) -> Result<(), String> {
    let p = conf_path(root);
    if let Some(d) = p.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&p, netconf::render_config(c)).map_err(|e| e.to_string())
}

/// Network interfaces that are real hardware or Wi-Fi (not the loopback, bridges, tunnels or container links).
pub fn interfaces(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(root.join("sys/class/net")).into_iter().flatten().flatten().map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| netconf::valid_iface(n) && root.join("sys/class/net").join(n).join("device").exists()).collect();
    v.sort();
    v
}

fn stop_dhcp(ctx: &Ctx, name: &str) {
    let pidfile = ctx.root.join("run").join(format!("udhcpc.{name}.pid"));
    if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<u32>().ok()) { ctx.runner.run("kill", &[&pid.to_string()]); }
    let _ = std::fs::remove_file(&pidfile);
}

/// Bring one interface to `mode`. Returns what was done, for messages.
pub fn apply_iface(ctx: &Ctx, name: &str, mode: &Mode) -> Result<String, String> {
    if !netconf::valid_iface(name) { return Err("That is not a valid interface name.".into()); }
    let run = |c: &str, a: &[&str]| -> Result<(), String> { let (code, out) = ctx.runner.run(c, a); if code == 0 { Ok(()) } else { Err(format!("{c} {} failed: {}", a.join(" "), out.trim())) } };
    match mode {
        Mode::Off => { stop_dhcp(ctx, name); ctx.runner.run("ip", &["addr", "flush", "dev", name]); run("ip", &["link", "set", name, "down"])?; Ok(format!("{name} is off.")) }
        Mode::Dhcp => {
            stop_dhcp(ctx, name);
            run("ip", &["link", "set", name, "up"])?;
            ctx.runner.run("ip", &["addr", "flush", "dev", name]);
            let pidfile = format!("/run/udhcpc.{name}.pid");
            run("udhcpc", &["-b", "-q", "-S", "-i", name, "-p", &pidfile])?;
            Ok(format!("{name} gets its address automatically."))
        }
        Mode::Static { addr, prefix, gateway, dns } => {
            stop_dhcp(ctx, name);
            run("ip", &["link", "set", name, "up"])?;
            ctx.runner.run("ip", &["addr", "flush", "dev", name]);
            run("ip", &["addr", "add", &format!("{addr}/{prefix}"), "dev", name])?;
            run("ip", &["route", "replace", "default", "via", gateway, "dev", name])?;
            if !dns.is_empty() {
                std::fs::create_dir_all(ctx.root.join("etc")).map_err(|e| e.to_string())?;
                std::fs::write(ctx.root.join("etc/resolv.conf"), netconf::resolv_conf(dns)).map_err(|e| e.to_string())?;
            }
            Ok(format!("{name} uses {addr}/{prefix} through {gateway}."))
        }
    }
}

/// Start (or reload) the Wi-Fi client with the saved networks.
fn apply_wifi(ctx: &Ctx, name: &str, c: &Config) -> Result<(), String> {
    if c.wifi.is_empty() { return Ok(()); }
    let dir = ctx.root.join("etc/wpa_supplicant");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join("wpa_supplicant.conf");
    std::fs::write(&path, netconf::wpa_supplicant_conf(&c.wifi)).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    ctx.runner.run("ip", &["link", "set", name, "up"]);
    let pid = ctx.root.join(format!("run/wpa_supplicant.{name}.pid"));
    let running = std::fs::read_to_string(&pid).ok().and_then(|s| s.trim().parse::<u32>().ok()).map_or(false, |p| ctx.root.join(format!("proc/{p}")).exists());
    if running { ctx.runner.run("wpa_cli", &["-i", name, "reconfigure"]); }
    else { let (code, out) = ctx.runner.run("wpa_supplicant", &["-B", "-i", name, "-c", &path.to_string_lossy(), "-P", &format!("/run/wpa_supplicant.{name}.pid")]); if code != 0 { return Err(format!("The Wi-Fi client did not start: {}", out.trim())); } }
    Ok(())
}

/// Apply the whole configuration (at boot, and after a change). Interfaces without a setting use DHCP, like before this feature existed.
pub fn apply_all(ctx: &Ctx) -> Vec<String> {
    let c = load(ctx.root);
    let mut notes = Vec::new();
    for name in interfaces(ctx.root) {
        if netconf::is_wifi(&name) {
            // A Wi-Fi adapter with no saved network and no setting is left alone (there is nothing to get an address from).
            if c.wifi.is_empty() && !c.ifaces.iter().any(|i| i.name == name) { continue; }
            if let Err(e) = apply_wifi(ctx, &name, &c) { notes.push(e); }
        }
        match apply_iface(ctx, &name, netconf::mode_of(&c, &name)) { Ok(m) => notes.push(m), Err(e) => notes.push(e) }
    }
    notes
}

/// Save a new mode for one interface and apply it.
pub fn set(ctx: &Ctx, name: &str, mode: Mode) -> Result<String, String> {
    if !interfaces(ctx.root).iter().any(|n| n == name) { return Err(format!("There is no network interface called {name}.")); }
    let mut c = load(ctx.root);
    netconf::set_mode(&mut c, name, mode.clone());
    save(ctx.root, &c)?;
    apply_iface(ctx, name, &mode)
}

pub fn wifi_scan(ctx: &Ctx, name: &str) -> Result<String, String> {
    if !netconf::is_wifi(name) || !interfaces(ctx.root).iter().any(|n| n == name) { return Err("There is no Wi-Fi adapter by that name.".into()); }
    ctx.runner.run("ip", &["link", "set", name, "up"]);
    let (code, out) = ctx.runner.run("iw", &["dev", name, "scan"]);
    if code != 0 { return Err(format!("Scanning failed: {}", out.trim())); }
    Ok(netconf::parse_scan(&out).iter().map(|(s, sig, sec)| format!("{} {} {}\n", netconf::to_hex_pub(s), sig, if *sec { "secured" } else { "open" })).collect())
}

/// Remember a Wi-Fi network (an empty password = an open network) and join it.
pub fn wifi_connect(ctx: &Ctx, name: &str, ssid: &str, passphrase: &str) -> Result<String, String> {
    if !netconf::is_wifi(name) || !interfaces(ctx.root).iter().any(|n| n == name) { return Err("There is no Wi-Fi adapter by that name.".into()); }
    if !netconf::valid_ssid(ssid) { return Err("That network name is not valid.".into()); }
    if !passphrase.is_empty() && !netconf::valid_passphrase(passphrase) { return Err("A Wi-Fi password must be 8 to 63 letters, digits or symbols.".into()); }
    let mut c = load(ctx.root);
    c.wifi.retain(|n| n.ssid != ssid);
    c.wifi.push(netconf::WifiNet { ssid: ssid.to_string(), psk: if passphrase.is_empty() { String::new() } else { netconf::wpa_psk(passphrase, ssid) } });
    save(ctx.root, &c)?;
    apply_wifi(ctx, name, &c)?;
    if netconf::mode_of(&c, name) == &Mode::Dhcp { let _ = apply_iface(ctx, name, &Mode::Dhcp); }
    Ok(format!("Joining {ssid}..."))
}

pub fn wifi_forget(ctx: &Ctx, ssid: &str) -> Result<String, String> {
    let mut c = load(ctx.root);
    let before = c.wifi.len();
    c.wifi.retain(|n| n.ssid != ssid);
    if c.wifi.len() == before { return Err("That network is not saved.".into()); }
    save(ctx.root, &c)?;
    for name in interfaces(ctx.root).into_iter().filter(|n| netconf::is_wifi(n)) { let _ = apply_wifi(ctx, &name, &c); }
    Ok(format!("{ssid} is forgotten."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, fail: RefCell<Option<String>>, scan: String }
    fn fake() -> Fake { Fake { calls: RefCell::new(vec![]), fail: RefCell::new(None), scan: String::new() } }
    impl Runner for Fake { fn run(&self, c: &str, a: &[&str]) -> (i32, String) { let line = format!("{c} {}", a.join(" ")); self.calls.borrow_mut().push(line.clone()); if self.fail.borrow().as_ref().map_or(false, |f| line.starts_with(f.as_str())) { return (1, "boom".into()); } if c == "iw" { return (0, self.scan.clone()); } (0, String::new()) } }

    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-net-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for n in ["eth0", "wlan0"] { std::fs::create_dir_all(d.join("sys/class/net").join(n).join("device")).unwrap(); }
        std::fs::create_dir_all(d.join("sys/class/net/lo")).unwrap(); std::fs::create_dir_all(d.join("sys/class/net/docker0")).unwrap();
        std::fs::create_dir_all(d.join("run")).unwrap(); std::fs::create_dir_all(d.join("etc")).unwrap();
        d
    }

    #[test]
    fn only_real_interfaces_are_managed() {
        let r = world("list");
        assert_eq!(interfaces(&r), vec!["eth0", "wlan0"], "not the loopback or a virtual link");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn automatic_fixed_and_off_do_what_they_say() {
        let r = world("modes"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        std::fs::write(r.join("run/udhcpc.eth0.pid"), "4242\n").unwrap();
        let m = netconf::static_mode("192.168.1.50/24", "192.168.1.1", "1.1.1.1 8.8.8.8").unwrap();
        assert!(set(&c, "eth0", m).unwrap().contains("192.168.1.50/24"));
        let calls = f.calls.borrow().join("\n");
        for want in ["kill 4242", "ip link set eth0 up", "ip addr flush dev eth0", "ip addr add 192.168.1.50/24 dev eth0", "ip route replace default via 192.168.1.1 dev eth0"] { assert!(calls.contains(want), "{want} in:\n{calls}"); }
        assert_eq!(std::fs::read_to_string(r.join("etc/resolv.conf")).unwrap(), "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
        assert!(std::fs::read_to_string(conf_path(&r)).unwrap().contains("iface eth0 static 192.168.1.50/24 192.168.1.1 1.1.1.1,8.8.8.8"));
        f.calls.borrow_mut().clear();
        set(&c, "eth0", Mode::Dhcp).unwrap();
        assert!(f.calls.borrow().join("\n").contains("udhcpc -b -q -S -i eth0 -p /run/udhcpc.eth0.pid"));
        f.calls.borrow_mut().clear();
        set(&c, "eth0", Mode::Off).unwrap();
        assert!(f.calls.borrow().join("\n").contains("ip link set eth0 down"));
        assert!(set(&c, "nosuch", Mode::Dhcp).is_err() && set(&c, "lo", Mode::Off).is_err() && set(&c, "docker0", Mode::Off).is_err(), "only real interfaces can be changed");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn at_boot_every_interface_follows_its_setting_and_defaults_to_automatic() {
        let r = world("boot"); let f = fake(); let c = Ctx { root: &r, runner: &f };
        save(&r, &netconf::parse_config("iface eth0 off\n")).unwrap();
        let notes = apply_all(&c);
        assert!(notes.iter().any(|n| n.contains("eth0 is off")) && !notes.iter().any(|n| n.contains("wlan0")), "an unused Wi-Fi adapter is left alone: {notes:?}");
        save(&r, &netconf::parse_config("iface eth0 off\nwifi 43616665 -\n")).unwrap();
        let notes = apply_all(&c);
        assert!(notes.iter().any(|n| n.contains("wlan0 gets its address automatically")), "with a saved network the Wi-Fi adapter is brought up: {notes:?}");
        let _ = std::fs::remove_dir_all(&r);
        let r = world("fail"); let f = fake(); *f.fail.borrow_mut() = Some("ip addr add".into()); let c = Ctx { root: &r, runner: &f };
        let m = netconf::static_mode("192.168.1.50/24", "192.168.1.1", "").unwrap();
        assert!(set(&c, "eth0", m).unwrap_err().contains("failed"), "a command that fails is reported");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn wifi_networks_are_scanned_joined_and_forgotten_without_storing_the_password() {
        let r = world("wifi"); let mut f = fake(); f.scan = "BSS aa:bb(on wlan0)\n\tsignal: -50.00 dBm\n\tSSID: Home\n\tRSN:\n".into(); let c = Ctx { root: &r, runner: &f };
        assert!(wifi_scan(&c, "wlan0").unwrap().contains("signal") == false && wifi_scan(&c, "wlan0").unwrap().ends_with("-50 secured\n"));
        assert!(wifi_scan(&c, "eth0").is_err());
        assert!(wifi_connect(&c, "wlan0", "Home", "correct horse").unwrap().contains("Joining Home"));
        let stored = std::fs::read_to_string(conf_path(&r)).unwrap();
        assert!(!stored.contains("correct horse") && stored.contains(&netconf::wpa_psk("correct horse", "Home")), "only the derived key is kept");
        let w = std::fs::read_to_string(r.join("etc/wpa_supplicant/wpa_supplicant.conf")).unwrap();
        assert!(w.contains("psk=") && !w.contains("correct horse"));
        assert_eq!(std::fs::metadata(r.join("etc/wpa_supplicant/wpa_supplicant.conf")).unwrap().permissions().mode() & 0o777, 0o600);
        assert!(f.calls.borrow().join("\n").contains("wpa_supplicant -B -i wlan0 -c "));
        assert!(wifi_connect(&c, "wlan0", "Home", "short").is_err() && wifi_connect(&c, "wlan0", "", "12345678").is_err() && wifi_connect(&c, "eth0", "Home", "12345678").is_err());
        wifi_connect(&c, "wlan0", "Cafe", "").unwrap();
        assert!(std::fs::read_to_string(conf_path(&r)).unwrap().contains("wifi 43616665 -"), "an open network has no key");
        wifi_forget(&c, "Home").unwrap();
        assert!(!std::fs::read_to_string(conf_path(&r)).unwrap().contains(&netconf::wpa_psk("correct horse", "Home")) && wifi_forget(&c, "Home").is_err());
        let _ = std::fs::remove_dir_all(&r);
    }
}
