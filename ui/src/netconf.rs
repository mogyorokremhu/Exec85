//! Network settings: which interface uses an automatic address (DHCP), a fixed one, or is off, plus Wi-Fi.
//! The settings live in /etc/exec85/network.conf and are applied at every boot and whenever they change.
//! Wi-Fi passwords are never stored: only the derived key (the same one `wpa_passphrase` would make).
use std::fmt::Write as _;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Mode { Dhcp, Off, Static { addr: String, prefix: u8, gateway: String, dns: Vec<String> } }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfaceConf { pub name: String, pub mode: Mode }

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Config { pub ifaces: Vec<IfaceConf>, pub wifi: Vec<WifiNet> }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WifiNet { pub ssid: String, /// 64 hex digits, or empty for an open network
    pub psk: String }

pub fn valid_iface(n: &str) -> bool { !n.is_empty() && n.len() <= 15 && n != "lo" && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') }
pub fn is_wifi(n: &str) -> bool { n.starts_with("wl") }

pub fn parse_ipv4(s: &str) -> Option<[u8; 4]> {
    let p: Vec<&str> = s.split('.').collect();
    if p.len() != 4 { return None; }
    let mut o = [0u8; 4];
    for (i, x) in p.iter().enumerate() { if x.is_empty() || x.len() > 3 || !x.chars().all(|c| c.is_ascii_digit()) || (x.len() > 1 && x.starts_with('0')) { return None; } o[i] = x.parse().ok()?; }
    Some(o)
}

/// "192.168.1.50/24" -> (address, prefix).
pub fn parse_cidr(s: &str) -> Result<(String, u8), String> {
    let (a, p) = s.split_once('/').ok_or("Type the address with its size, like 192.168.1.50/24.")?;
    let o = parse_ipv4(a).ok_or("That is not a valid IPv4 address.")?;
    let prefix: u8 = p.parse().map_err(|_| "The size after the / must be a number from 1 to 30.")?;
    if !(1..=30).contains(&prefix) { return Err("The size after the / must be from 1 to 30 (24 is normal at home).".into()); }
    if o[0] == 0 || o[0] == 127 || o[0] >= 224 { return Err("That address cannot be used for this computer.".into()); }
    Ok((a.to_string(), prefix))
}

/// Is `gw` inside the network `addr/prefix`?
pub fn same_network(addr: &str, prefix: u8, gw: &str) -> bool {
    let (Some(a), Some(g)) = (parse_ipv4(addr), parse_ipv4(gw)) else { return false };
    let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix as u32) };
    (u32::from_be_bytes(a) & mask) == (u32::from_be_bytes(g) & mask)
}

pub fn static_mode(cidr: &str, gateway: &str, dns: &str) -> Result<Mode, String> {
    let (addr, prefix) = parse_cidr(cidr)?;
    if parse_ipv4(gateway).is_none() { return Err("The gateway (your router) must be an address like 192.168.1.1.".into()); }
    if !same_network(&addr, prefix, gateway) { return Err("The gateway must be on the same network as the address.".into()); }
    let dns: Vec<String> = dns.split(|c: char| c == ',' || c.is_whitespace()).filter(|d| !d.is_empty()).map(String::from).collect();
    if dns.len() > 3 { return Err("At most 3 name servers.".into()); }
    if let Some(bad) = dns.iter().find(|d| parse_ipv4(d).is_none()) { return Err(format!("{bad} is not a valid name server address.")); }
    Ok(Mode::Static { addr, prefix, gateway: gateway.to_string(), dns })
}

pub fn parse_config(text: &str) -> Config {
    let mut c = Config::default();
    for line in text.lines() {
        let w: Vec<&str> = line.split_whitespace().collect();
        match w.as_slice() {
            ["iface", name, "dhcp"] if valid_iface(name) => c.ifaces.push(IfaceConf { name: name.to_string(), mode: Mode::Dhcp }),
            ["iface", name, "off"] if valid_iface(name) => c.ifaces.push(IfaceConf { name: name.to_string(), mode: Mode::Off }),
            ["iface", name, "static", cidr, gw, dns] if valid_iface(name) => {
                if let Ok(m) = static_mode(cidr, gw, &if *dns == "-" { String::new() } else { dns.replace(',', " ") }) { c.ifaces.push(IfaceConf { name: name.to_string(), mode: m }); }
            }
            ["wifi", ssid_hex, psk] => { if valid_hex(ssid_hex, 2, 64) && (*psk == "-" || valid_hex(psk, 64, 64)) { c.wifi.push(WifiNet { ssid: from_hex(ssid_hex), psk: if *psk == "-" { String::new() } else { psk.to_string() } }); } }
            _ => {}
        }
    }
    // the last line for an interface wins
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for i in c.ifaces.into_iter().rev() { if !seen.contains(&i.name) { seen.push(i.name.clone()); out.push(i); } }
    out.reverse();
    Config { ifaces: out, wifi: c.wifi }
}

pub fn render_config(c: &Config) -> String {
    let mut s = String::new();
    for i in &c.ifaces {
        match &i.mode {
            Mode::Dhcp => { let _ = writeln!(s, "iface {} dhcp", i.name); }
            Mode::Off => { let _ = writeln!(s, "iface {} off", i.name); }
            Mode::Static { addr, prefix, gateway, dns } => { let _ = writeln!(s, "iface {} static {addr}/{prefix} {gateway} {}", i.name, if dns.is_empty() { "-".to_string() } else { dns.join(",") }); }
        }
    }
    for w in &c.wifi { let _ = writeln!(s, "wifi {} {}", to_hex(w.ssid.as_bytes()), if w.psk.is_empty() { "-" } else { &w.psk }); }
    s
}

pub fn mode_of<'a>(c: &'a Config, name: &str) -> &'a Mode { c.ifaces.iter().find(|i| i.name == name).map_or(&Mode::Dhcp, |i| &i.mode) }

pub fn set_mode(c: &mut Config, name: &str, mode: Mode) { c.ifaces.retain(|i| i.name != name); c.ifaces.push(IfaceConf { name: name.to_string(), mode }); }

/// The contents of /etc/resolv.conf for these name servers (empty = leave the DHCP-provided one alone).
pub fn resolv_conf(dns: &[String]) -> String { dns.iter().map(|d| format!("nameserver {d}\n")).collect() }

// ---- Wi-Fi ----------------------------------------------------------------------------------------------------------------

pub fn to_hex_pub(s: &str) -> String { to_hex(s.as_bytes()) }
fn to_hex(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
pub fn from_hex(s: &str) -> String { String::from_utf8_lossy(&(0..s.len() / 2).filter_map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()).collect::<Vec<u8>>()).into_owned() }
fn valid_hex(s: &str, min: usize, max: usize) -> bool { s.len() >= min && s.len() <= max && s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit()) }

pub fn valid_ssid(s: &str) -> bool { !s.is_empty() && s.len() <= 32 && !s.chars().any(|c| c.is_control()) }
pub fn valid_passphrase(p: &str) -> bool { (8..=63).contains(&p.len()) && p.chars().all(|c| (' '..='~').contains(&c)) }

// SHA-1, HMAC-SHA1 and PBKDF2: what turns a Wi-Fi password into the 256-bit key (so the password itself never has to be stored).
fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 { msg.push(0); }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 { w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]); }
        for i in 16..80 { w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1); }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i { 0..=19 => ((b & c) | (!b & d), 0x5A827999), 20..=39 => (b ^ c ^ d, 0x6ED9EBA1), 40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC), _ => (b ^ c ^ d, 0xCA62C1D6) };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            e = d; d = c; c = b.rotate_left(30); b = a; a = t;
        }
        h = [h[0].wrapping_add(a), h[1].wrapping_add(b), h[2].wrapping_add(c), h[3].wrapping_add(d), h[4].wrapping_add(e)];
    }
    let mut out = [0u8; 20];
    for i in 0..5 { out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes()); }
    out
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = if key.len() > 64 { sha1(key).to_vec() } else { key.to_vec() };
    k.resize(64, 0);
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect(); inner.extend_from_slice(msg);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect(); outer.extend_from_slice(&sha1(&inner));
    sha1(&outer)
}

/// The WPA2 key for a passphrase and network name: PBKDF2-HMAC-SHA1, 4096 rounds, 32 bytes, as 64 hex digits.
pub fn wpa_psk(passphrase: &str, ssid: &str) -> String {
    let mut out = Vec::new();
    for block in 1u32..=2 {
        let mut salt = ssid.as_bytes().to_vec(); salt.extend_from_slice(&block.to_be_bytes());
        let mut u = hmac_sha1(passphrase.as_bytes(), &salt);
        let mut t = u;
        for _ in 1..4096 { u = hmac_sha1(passphrase.as_bytes(), &u); for (a, b) in t.iter_mut().zip(u.iter()) { *a ^= b; } }
        out.extend_from_slice(&t);
    }
    to_hex(&out[..32])
}

/// wpa_supplicant's configuration: the networks this computer may join. The names go in as hex, so nothing in a name can break the file.
pub fn wpa_supplicant_conf(nets: &[WifiNet]) -> String {
    let mut s = String::from("# Generated by Exec85. Change Wi-Fi networks in the Network app.\nctrl_interface=/run/wpa_supplicant\nctrl_interface_group=0\nupdate_config=0\n");
    for n in nets {
        let _ = write!(s, "\nnetwork={{\n\tssid={}\n", to_hex(n.ssid.as_bytes()));
        if n.psk.is_empty() { s.push_str("\tkey_mgmt=NONE\n"); } else { let _ = writeln!(s, "\tpsk={}", n.psk); }
        s.push_str("}\n");
    }
    s
}

/// `iw dev wlan0 scan` output -> (network name, signal in dBm, secured), strongest first, without duplicates or hidden networks.
pub fn parse_scan(text: &str) -> Vec<(String, i32, bool)> {
    let mut nets: Vec<(String, i32, bool)> = Vec::new();
    let (mut ssid, mut sig, mut secure, mut any) = (String::new(), -100, false, false);
    let mut flush = |ssid: &mut String, sig: &mut i32, secure: &mut bool, any: &mut bool, nets: &mut Vec<(String, i32, bool)>| {
        if *any && !ssid.is_empty() { if let Some(n) = nets.iter_mut().find(|n| n.0 == *ssid) { if *sig > n.1 { n.1 = *sig; n.2 = *secure; } } else { nets.push((ssid.clone(), *sig, *secure)); } }
        ssid.clear(); *sig = -100; *secure = false; *any = false;
    };
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("BSS ") { flush(&mut ssid, &mut sig, &mut secure, &mut any, &mut nets); any = true; }
        else if let Some(v) = t.strip_prefix("SSID:") { ssid = v.trim().to_string(); }
        else if let Some(v) = t.strip_prefix("signal:") { sig = v.trim().trim_end_matches("dBm").trim().parse::<f64>().map(|x| x as i32).unwrap_or(-100); }
        else if t.starts_with("RSN:") || t.starts_with("WPA:") || t.contains("Privacy") { secure = true; }
    }
    flush(&mut ssid, &mut sig, &mut secure, &mut any, &mut nets);
    nets.sort_by(|a, b| b.1.cmp(&a.1));
    nets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_checked_strictly() {
        assert_eq!(parse_ipv4("192.168.1.50"), Some([192, 168, 1, 50]));
        for bad in ["", "1.2.3", "1.2.3.4.5", "256.1.1.1", "01.2.3.4", "a.b.c.d", "1.2.3.-4", "1..3.4", " 1.2.3.4"] { assert!(parse_ipv4(bad).is_none(), "{bad:?}"); }
        assert_eq!(parse_cidr("192.168.1.50/24").unwrap(), ("192.168.1.50".into(), 24));
        for bad in ["192.168.1.50", "192.168.1.50/0", "192.168.1.50/31", "192.168.1.50/x", "0.1.2.3/8", "127.0.0.1/8", "224.0.0.1/8"] { assert!(parse_cidr(bad).is_err(), "{bad}"); }
        assert!(same_network("192.168.1.50", 24, "192.168.1.1") && !same_network("192.168.1.50", 24, "192.168.2.1") && same_network("10.1.2.3", 8, "10.200.0.1"));
    }

    #[test]
    fn fixed_settings_need_a_gateway_on_the_same_network_and_valid_name_servers() {
        let m = static_mode("192.168.1.50/24", "192.168.1.1", "1.1.1.1, 8.8.8.8").unwrap();
        assert_eq!(m, Mode::Static { addr: "192.168.1.50".into(), prefix: 24, gateway: "192.168.1.1".into(), dns: vec!["1.1.1.1".into(), "8.8.8.8".into()] });
        assert!(static_mode("192.168.1.50/24", "10.0.0.1", "").unwrap_err().contains("same network"));
        assert!(static_mode("192.168.1.50/24", "router", "").is_err() && static_mode("192.168.1.50/24", "192.168.1.1", "dns.example").is_err() && static_mode("192.168.1.50/24", "192.168.1.1", "1.1.1.1 1.0.0.1 8.8.8.8 8.8.4.4").is_err());
        assert!(matches!(static_mode("192.168.1.50/24", "192.168.1.1", "").unwrap(), Mode::Static { dns, .. } if dns.is_empty()), "name servers are optional");
    }

    #[test]
    fn the_configuration_round_trips_and_the_last_line_for_an_interface_wins() {
        let mut c = Config::default();
        set_mode(&mut c, "eth0", Mode::Dhcp);
        set_mode(&mut c, "eth1", static_mode("10.0.0.5/8", "10.0.0.1", "").unwrap());
        set_mode(&mut c, "eth2", Mode::Off);
        c.wifi.push(WifiNet { ssid: "Home Wi-Fi \u{e9}".into(), psk: wpa_psk("password", "IEEE") });
        c.wifi.push(WifiNet { ssid: "Cafe".into(), psk: String::new() });
        assert_eq!(parse_config(&render_config(&c)), c);
        set_mode(&mut c, "eth0", Mode::Off);
        assert_eq!(c.ifaces.iter().filter(|i| i.name == "eth0").count(), 1);
        assert_eq!(parse_config("iface eth0 dhcp\niface eth0 off\n").ifaces, vec![IfaceConf { name: "eth0".into(), mode: Mode::Off }]);
        assert!(parse_config("iface lo dhcp\niface bad;name dhcp\niface eth0 static 1.2.3.4/24 9.9.9.9 -\nwifi zz -\nrubbish").ifaces.is_empty(), "bad lines are ignored");
        assert_eq!(mode_of(&Config::default(), "eth9"), &Mode::Dhcp, "the default for an unknown interface is automatic");
    }

    #[test]
    fn the_wifi_key_matches_the_published_test_vectors() {
        assert_eq!(wpa_psk("password", "IEEE"), "f42c6fc52df0ebef9ebb4b90b38a5f902e83fe1b135a70e23aed762e9710a12e");
        assert_eq!(wpa_psk("ThisIsAPassword", "ThisIsASSID"), "0dc0d6eb90555ed6419756b9a15ec3e3209b63df707dd508d14581f8982721af");
        assert_eq!(to_hex(&sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(to_hex(&hmac_sha1(b"key", b"The quick brown fox jumps over the lazy dog")), "de7c9b85b8b78aa6bc8a7a36f70a90701c9db4d9");
    }

    #[test]
    fn wifi_names_and_passwords_are_checked_and_the_file_cannot_be_broken_by_a_name() {
        assert!(valid_ssid("My Net") && !valid_ssid("") && !valid_ssid(&"x".repeat(33)) && !valid_ssid("a\nb"));
        assert!(valid_passphrase("12345678") && !valid_passphrase("short") && !valid_passphrase(&"x".repeat(64)) && !valid_passphrase("pass\u{e9}word12"));
        let conf = wpa_supplicant_conf(&[WifiNet { ssid: "evil\"}\nnetwork={".into(), psk: "a".repeat(64) }, WifiNet { ssid: "Open".into(), psk: String::new() }]);
        assert_eq!(conf.matches("network={").count(), 2, "{conf}");
        assert!(conf.contains(&format!("ssid={}", to_hex(b"Open"))) && conf.contains("key_mgmt=NONE") && conf.contains(&format!("psk={}", "a".repeat(64))));
        assert!(!conf.contains("evil"), "the name only appears as hex");
    }

    #[test]
    fn scan_results_are_listed_strongest_first() {
        let scan = "BSS aa:bb:cc:dd:ee:01(on wlan0)\n\tsignal: -45.00 dBm\n\tSSID: Home\n\tRSN:\t * Version: 1\nBSS aa:bb:cc:dd:ee:02(on wlan0)\n\tsignal: -70.00 dBm\n\tSSID: Cafe\nBSS aa:bb:cc:dd:ee:03(on wlan0)\n\tsignal: -80.00 dBm\n\tSSID: Home\nBSS aa:bb:cc:dd:ee:04(on wlan0)\n\tsignal: -60.00 dBm\n\tSSID: \n";
        assert_eq!(parse_scan(scan), vec![("Home".to_string(), -45, true), ("Cafe".to_string(), -70, false)], "hidden networks and weaker duplicates are left out");
        assert!(parse_scan("").is_empty());
    }

    #[test]
    fn name_servers_become_a_resolv_conf() {
        assert_eq!(resolv_conf(&["1.1.1.1".into(), "8.8.8.8".into()]), "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
        assert_eq!(resolv_conf(&[]), "");
    }
}
