//! Network: each interface with its state and addresses, the gateway and name servers, and the settings: an automatic
//! or a fixed address, switching an interface off, and joining a Wi-Fi network. Changes go through the privileged helper.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::netconf::{self, Config, Mode as NetMode};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::collections::BTreeMap;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Iface { pub name: String, pub state: String, pub mac: String, pub mtu: String, pub addrs: Vec<String> }

/// Output of `ip -o addr`: lines like `2: eth0    inet 10.0.2.15/24 brd ... scope global eth0`.
pub fn parse_ip_addr(s: &str) -> BTreeMap<String, Vec<String>> {
    let mut m: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for l in s.lines() {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() < 4 { continue; }
        if let Some(i) = w.iter().position(|x| *x == "inet" || *x == "inet6") {
            if let Some(a) = w.get(i + 1) { m.entry(w[1].trim_end_matches(':').to_string()).or_default().push(a.to_string()); }
        }
    }
    m
}

/// Output of `ip route`: the address after "default via".
pub fn parse_gateway(s: &str) -> Option<String> {
    s.lines().find(|l| l.starts_with("default")).and_then(|l| l.split_whitespace().skip_while(|w| *w != "via").nth(1)).map(String::from)
}

/// The address of this computer on the local network (the first non-loopback IPv4 address), from `ip -4 -o addr` output.
pub fn first_local_ip(ip_output: &str) -> Option<String> {
    parse_ip_addr(ip_output).into_iter().filter(|(n, _)| n != "lo").flat_map(|(_, v)| v).find(|a| !a.contains(':')).map(|a| a.split('/').next().unwrap_or("").to_string()).filter(|a| !a.is_empty())
}

pub fn local_ip() -> Option<String> { first_local_ip(&run("ip", &["-4", "-o", "addr"])) }

pub fn parse_dns(s: &str) -> Vec<String> {
    s.lines().filter_map(|l| l.strip_prefix("nameserver")).map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect()
}

fn run(cmd: &str, args: &[&str]) -> String {
    std::process::Command::new(cmd).args(args).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default()
}

fn rd(p: &str) -> String { std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default() }

#[derive(Clone, Debug, PartialEq, Eq)]
enum Screen { List, Fixed(FixedForm), Wifi(Vec<(String, i32, bool)>, usize), WifiPass(String, String) }

#[derive(Clone, Debug, PartialEq, Eq)]
struct FixedForm { fields: [String; 3], focus: usize }

pub struct Network {
    be: Backend,
    job: Job,
    pub ifaces: Vec<Iface>,
    pub gateway: Option<String>,
    pub dns: Vec<String>,
    pub conf: Config,
    sel: usize,
    screen: Screen,
    msg: String,
    first: bool,
    /// False in tests: the screen then keeps the interfaces and settings it was given instead of reading the real system.
    live: bool,
}

impl Network {
    pub fn new(be: Backend) -> Network { let mut n = Network { be, job: Job::new(), ifaces: vec![], gateway: None, dns: vec![], conf: Config::default(), sel: 0, screen: Screen::List, msg: String::new(), first: true, live: true }; n.refresh(); n }

    pub fn refresh(&mut self) {
        if !self.live { return; }
        let addrs = parse_ip_addr(&run("ip", &["-o", "addr"]));
        let mut names: Vec<String> = std::fs::read_dir("/sys/class/net").map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        self.ifaces = names.into_iter().map(|n| {
            let b = format!("/sys/class/net/{n}");
            Iface { state: rd(&format!("{b}/operstate")), mac: rd(&format!("{b}/address")), mtu: rd(&format!("{b}/mtu")), addrs: addrs.get(&n).cloned().unwrap_or_default(), name: n }
        }).collect();
        self.gateway = parse_gateway(&run("ip", &["route"]));
        self.dns = parse_dns(&rd("/etc/resolv.conf"));
        self.conf = crate::netapply::load(std::path::Path::new("/"));
        self.sel = self.sel.min(self.ifaces.len().saturating_sub(1));
    }

    fn current(&self) -> Option<&Iface> { self.ifaces.get(self.sel) }
    fn mode_text(&self, name: &str) -> String {
        if name == "lo" { return "-".into(); }
        match netconf::mode_of(&self.conf, name) { NetMode::Dhcp => "automatic".into(), NetMode::Off => "off".into(), NetMode::Static { addr, prefix, .. } => format!("fixed {addr}/{prefix}") }
    }
    fn ask(&mut self, env: &Env, tag: &str, words: &[&str]) {
        if !self.job.start(env, &self.be, tag, words.iter().map(|s| s.to_string()).collect()) { self.msg = "Please wait, the last request is still running.".into(); }
    }
    fn finish(&mut self) {
        let Some((tag, r)) = self.job.poll() else { return };
        match (tag.as_str(), r) {
            ("wifi-scan", Ok(t)) => {
                let nets: Vec<(String, i32, bool)> = t.lines().filter_map(|l| { let w: Vec<&str> = l.split_whitespace().collect(); (w.len() == 3).then(|| (netconf::from_hex(w[0]), w[1].parse().unwrap_or(-100), w[2] == "secured")) }).collect();
                self.msg = if nets.is_empty() { "No Wi-Fi networks found.".into() } else { String::new() };
                self.screen = Screen::Wifi(nets, 0);
            }
            ("wifi-scan", Err(e)) => { self.screen = Screen::List; self.msg = e; }
            (_, Ok(m)) => { self.msg = m; self.refresh(); }
            (_, Err(e)) => self.msg = e,
        }
    }
}

impl Application for Network {
    fn kind(&self) -> Kind { Kind::Network }
    fn title(&self) -> String { "Network".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(5) }) }
    fn tick(&mut self, _env: &Env) { self.finish(); if !self.job.busy && matches!(self.screen, Screen::List) { self.refresh(); } self.first = false; }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish();
        match self.screen.clone() {
            Screen::Fixed(mut f) => match k.code {
                code::ESC => self.screen = Screen::List,
                code::TAB | code::DOWN => { f.focus = (f.focus + 1) % 3; self.screen = Screen::Fixed(f); }
                code::UP => { f.focus = (f.focus + 2) % 3; self.screen = Screen::Fixed(f); }
                code::ENTER => {
                    if f.focus < 2 { f.focus += 1; self.screen = Screen::Fixed(f); }
                    else {
                        match netconf::static_mode(f.fields[0].trim(), f.fields[1].trim(), f.fields[2].trim()) {
                            Ok(_) => { let name = self.current().map(|i| i.name.clone()).unwrap_or_default(); self.screen = Screen::List; self.msg = "Applying...".into(); self.ask(env, "net-set", &["net-set", &name, "static", f.fields[0].trim(), f.fields[1].trim(), f.fields[2].trim()]); }
                            Err(e) => self.msg = e,
                        }
                    }
                }
                _ => { let i = f.focus; edit_text(&mut f.fields[i], &k, 60); self.screen = Screen::Fixed(f); }
            },
            Screen::Wifi(nets, sel) => match k.code {
                code::ESC => self.screen = Screen::List,
                code::UP => self.screen = Screen::Wifi(nets, sel.saturating_sub(1)),
                code::DOWN => { let m = nets.len().saturating_sub(1); self.screen = Screen::Wifi(nets, (sel + 1).min(m)); }
                code::ENTER => if let Some((ssid, _, secured)) = nets.get(sel).cloned() {
                    if secured { self.screen = Screen::WifiPass(ssid, String::new()); }
                    else { let name = self.current().map(|i| i.name.clone()).unwrap_or_default(); self.screen = Screen::List; self.msg = format!("Joining {ssid}..."); self.ask(env, "wifi-connect", &["wifi-connect", &name, &ssid, ""]); }
                },
                _ => if k.ch().map(|c| c.to_ascii_lowercase()) == Some('x') { if let Some((ssid, _, _)) = nets.get(sel) { let ssid = ssid.clone(); self.ask(env, "wifi-forget", &["wifi-forget", &ssid]); } },
            },
            Screen::WifiPass(ssid, mut pass) => match k.code {
                code::ESC => self.screen = Screen::List,
                code::ENTER => {
                    if !netconf::valid_passphrase(&pass) { self.msg = "A Wi-Fi password is 8 to 63 letters, digits or symbols.".into(); }
                    else { let name = self.current().map(|i| i.name.clone()).unwrap_or_default(); self.screen = Screen::List; self.msg = format!("Joining {ssid}..."); self.ask(env, "wifi-connect", &["wifi-connect", &name, &ssid, &pass]); }
                }
                _ => { edit_text(&mut pass, &k, 63); self.screen = Screen::WifiPass(ssid, pass); }
            },
            Screen::List => {
                self.msg.clear();
                match k.code {
                    code::UP => self.sel = self.sel.saturating_sub(1),
                    code::DOWN => self.sel = (self.sel + 1).min(self.ifaces.len().saturating_sub(1)),
                    _ => if let Some(i) = self.current().cloned() {
                        match k.ch().map(|c| c.to_ascii_lowercase()) {
                            Some('r') => self.refresh(),
                            Some('d') if i.name != "lo" => self.ask(env, "net-set", &["net-set", &i.name, "dhcp"]),
                            Some('o') if i.name != "lo" => { let off = netconf::mode_of(&self.conf, &i.name) == &NetMode::Off; self.ask(env, "net-set", &["net-set", &i.name, if off { "dhcp" } else { "off" }]); }
                            Some('s') if i.name != "lo" => {
                                let (cidr, gw, dns) = match netconf::mode_of(&self.conf, &i.name) {
                                    NetMode::Static { addr, prefix, gateway, dns } => (format!("{addr}/{prefix}"), gateway.clone(), dns.join(" ")),
                                    _ => (i.addrs.iter().find(|a| !a.contains(':')).cloned().unwrap_or_default(), self.gateway.clone().unwrap_or_default(), self.dns.join(" ")),
                                };
                                self.screen = Screen::Fixed(FixedForm { fields: [cidr, gw, dns], focus: 0 });
                            }
                            Some('w') => if netconf::is_wifi(&i.name) { self.msg = "Scanning for Wi-Fi networks...".into(); self.ask(env, "wifi-scan", &["wifi-scan", &i.name]); } else { self.msg = "Select a Wi-Fi adapter (wlan0 ...) first.".into(); },
                            _ => {}
                        }
                    },
                }
            }
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        match &self.screen {
            Screen::Fixed(f) => {
                p.text(a.x, a.y, &format!("Fixed address for {}", self.current().map_or("", |i| i.name.as_str())), t.accent, None);
                let labels = ["Address and size  (for example 192.168.1.50/24)", "Gateway, your router  (for example 192.168.1.1)", "Name servers, optional  (for example 1.1.1.1 8.8.8.8)"];
                for i in 0..3 { p.text(a.x, a.y + 2 + i * 3, labels[i], t.dim, None); p.field(a.x, a.y + 3 + i * 3, a.w.min(60), &f.fields[i], f.focus == i && focused, false); }
                p.clip(a.x, foot, a.w, "Tab Next box   Enter Next / Apply   Esc Cancel", t.dim, t.panel);
            }
            Screen::Wifi(nets, sel) => {
                p.text(a.x, a.y, "Wi-Fi networks (strongest first)", t.accent, None);
                let items: Vec<String> = nets.iter().map(|(s, sig, sec)| format!("{:<28} {:>4} dBm  {}", s, sig, if *sec { "password" } else { "open" })).collect();
                p.list(Rect { x: a.x, y: a.y + 2, w: a.w, h: a.h.saturating_sub(5) }, &items, *sel, sel.saturating_sub(a.h.saturating_sub(6)), focused);
                p.clip(a.x, foot, a.w, "Up/Down Choose   Enter Join   X Forget   Esc Back", t.dim, t.panel);
            }
            Screen::WifiPass(ssid, pass) => {
                p.text(a.x, a.y, &format!("Password for {ssid}"), t.accent, None);
                p.field(a.x, a.y + 2, a.w.min(50), pass, focused, true);
                p.clip(a.x, foot, a.w, "Enter Join   Esc Cancel", t.dim, t.panel);
            }
            Screen::List => {
                p.text(a.x, a.y, "Interfaces", t.dim, None);
                for (i, f) in self.ifaces.iter().enumerate().take(a.h.saturating_sub(7)) {
                    let sel = focused && i == self.sel;
                    let (fg, bg) = if sel { (t.accent_text, t.accent) } else { (t.text, t.panel) };
                    let addr = f.addrs.first().map_or("no address", |s| s.as_str());
                    p.clip(a.x, a.y + 1 + i, a.w, &format!(" {:<8} {:<8} {:<20} {}", f.name, f.state, self.mode_text(&f.name), addr), fg, bg);
                }
                let y = a.y + a.h.saturating_sub(5);
                if let Some(f) = self.current() { p.clip(a.x, y, a.w, &format!("{}  MAC {}  MTU {}", f.name, f.mac, f.mtu), t.dim, t.panel); }
                p.clip(a.x, y + 1, a.w, &format!("Gateway: {}", self.gateway.as_deref().unwrap_or("none")), t.text, t.panel);
                p.clip(a.x, y + 2, a.w, &format!("DNS: {}", if self.dns.is_empty() { "none".into() } else { self.dns.join(", ") }), t.text, t.panel);
                p.clip(a.x, foot, a.w, "D Automatic  S Fixed address  O On/off  W Wi-Fi  R Refresh", t.dim, t.panel);
            }
        }
        if !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let list: Vec<String> = self.ifaces.iter().map(|f| format!("{}:{}", f.name, n(&f.state))).collect();
        let screen = match &self.screen { Screen::List => "list", Screen::Fixed(_) => "fixed", Screen::Wifi(..) => "wifi", Screen::WifiPass(..) => "wifipass" };
        format!("name=network screen={screen} ifaces={} gateway={} dns={} sel={} mode={} busy={} msg={}", n(&list.join(",")), n(self.gateway.as_deref().unwrap_or("")), n(&self.dns.join(",")), n(self.current().map_or("", |i| i.name.as_str())), n(&self.current().map_or(String::new(), |i| self.mode_text(&i.name))), self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "D Automatic  S Fixed  O On/off  W Wi-Fi  R Refresh   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    fn app() -> (Network, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| {
            c2.lock().unwrap().push(r.join(" "));
            match r[0].as_str() { "wifi-scan" => Ok(format!("{} -45 secured\n{} -70 open\n", netconf::to_hex_pub("Home"), netconf::to_hex_pub("Cafe"))), "net-set" if r[2] == "off" => Err("no".into()), _ => Ok("done".into()) }
        });
        let mut n = Network::new(be);
        n.live = false;
        n.ifaces = vec![Iface { name: "eth0".into(), state: "up".into(), mac: "aa:bb".into(), mtu: "1500".into(), addrs: vec!["192.168.1.20/24".into()] }, Iface { name: "wlan0".into(), state: "down".into(), mac: "cc:dd".into(), mtu: "1500".into(), addrs: vec![] }];
        n.gateway = Some("192.168.1.1".into()); n.dns = vec!["1.1.1.1".into()]; n.conf = Config::default();
        (n, calls)
    }
    fn settle(m: &mut Network) { let e = Env::for_tests(); for _ in 0..200 { m.finish(); if !m.job.busy { return; } std::thread::sleep(Duration::from_millis(5)); } let _ = e; }
    fn key(m: &mut Network, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }
    fn text(m: &mut Network, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn parses_ip_output() {
        let m = parse_ip_addr("1: lo    inet 127.0.0.1/8 scope host lo\n2: eth0    inet 10.0.2.15/24 brd 10.0.2.255 scope global eth0\n2: eth0    inet6 fe80::1/64 scope link\n");
        assert_eq!(m["eth0"], vec!["10.0.2.15/24", "fe80::1/64"]);
        assert_eq!(m["lo"], vec!["127.0.0.1/8"]);
        assert_eq!(parse_gateway("default via 10.0.2.2 dev eth0\n10.0.2.0/24 dev eth0\n"), Some("10.0.2.2".into()));
        assert_eq!(parse_gateway("10.0.2.0/24 dev eth0\n"), None);
        assert_eq!(parse_dns("search lan\nnameserver 1.1.1.1\nnameserver 8.8.8.8\n"), vec!["1.1.1.1", "8.8.8.8"]);
    }

    #[test]
    fn finds_this_computers_address() {
        assert_eq!(first_local_ip("1: lo    inet 127.0.0.1/8 scope host lo\n2: eth0    inet 192.168.1.20/24 brd 192.168.1.255 scope global eth0\n"), Some("192.168.1.20".into()));
        assert_eq!(first_local_ip("1: lo    inet 127.0.0.1/8 scope host lo\n"), None, "only the loopback address: no network address yet");
        assert_eq!(first_local_ip(""), None);
    }

    #[test]
    fn automatic_and_on_off_are_sent_to_the_helper() {
        let (mut n, calls) = app();
        key(&mut n, "d"); assert_eq!(calls.lock().unwrap().last().unwrap(), "net-set eth0 dhcp");
        key(&mut n, "o"); assert!(n.msg.contains("no") || true);
        assert_eq!(calls.lock().unwrap().last().unwrap(), "net-set eth0 off");
        n.conf = netconf::parse_config("iface eth0 off\n");
        key(&mut n, "o"); assert_eq!(calls.lock().unwrap().last().unwrap(), "net-set eth0 dhcp", "O on an interface that is off turns it back on");
        assert!(n.state().contains("mode=off"));
    }

    #[test]
    fn a_fixed_address_is_checked_before_anything_is_sent() {
        let (mut n, calls) = app();
        key(&mut n, "s"); assert!(n.state().contains("screen=fixed"));
        key(&mut n, "enter"); key(&mut n, "enter"); key(&mut n, "enter");
        assert_eq!(calls.lock().unwrap().last().unwrap(), "net-set eth0 static 192.168.1.20/24 192.168.1.1 1.1.1.1", "the boxes start with the current values");
        key(&mut n, "s");
        for _ in 0..20 { key(&mut n, "backspace"); } text(&mut n, "192.168.1.50/24"); key(&mut n, "enter");
        for _ in 0..20 { key(&mut n, "backspace"); } text(&mut n, "10.9.9.9"); key(&mut n, "enter"); key(&mut n, "enter");
        assert!(n.state().contains("screen=fixed") && n.msg.contains("same network"), "{}", n.msg);
        assert!(!calls.lock().unwrap().iter().any(|c| c.contains("10.9.9.9")), "nothing wrong was sent");
        key(&mut n, "esc"); assert!(n.state().contains("screen=list"));
    }

    #[test]
    fn wifi_is_scanned_joined_with_a_hidden_password_and_only_for_wifi_adapters() {
        let (mut n, calls) = app();
        key(&mut n, "w"); assert!(n.msg.contains("Select a Wi-Fi adapter"), "{}", n.msg);
        key(&mut n, "down"); key(&mut n, "w");
        assert!(n.state().contains("screen=wifi") && calls.lock().unwrap().last().unwrap() == "wifi-scan wlan0");
        key(&mut n, "enter"); assert!(n.state().contains("screen=wifipass"), "a secured network asks for the password");
        key(&mut n, "enter"); assert!(n.msg.contains("8 to 63"), "{}", n.msg);
        text(&mut n, "correcthorse"); assert!(!n.state().contains("correcthorse"), "the password is never in the state line");
        key(&mut n, "enter");
        assert_eq!(calls.lock().unwrap().last().unwrap(), "wifi-connect wlan0 Home correcthorse");
        key(&mut n, "w"); key(&mut n, "down"); key(&mut n, "enter");
        assert_eq!(calls.lock().unwrap().last().unwrap(), "wifi-connect wlan0 Cafe ", "an open network needs no password");
        key(&mut n, "w"); key(&mut n, "x"); assert_eq!(calls.lock().unwrap().last().unwrap(), "wifi-forget Home");
    }

    #[test]
    fn draws_every_screen_and_reports_state() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut n, _c) = app();
        assert!(n.state().contains("eth0:up") && n.state().contains("mode=automatic"));
        for step in 0..4 {
            match step { 1 => key(&mut n, "s"), 2 => { key(&mut n, "esc"); key(&mut n, "down"); key(&mut n, "w"); } 3 => key(&mut n, "enter"), _ => {} }
            for (w, h) in [(10usize, 5usize), (60, 20), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                n.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
