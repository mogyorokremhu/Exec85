//! Network: shows each network interface, its state and addresses, the gateway and DNS servers.
//! (Changing settings needs administrator rights; that arrives with the privileged helper in a later milestone.)
use super::{Application, Env, Kind, Out};
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

pub struct Network { pub ifaces: Vec<Iface>, pub gateway: Option<String>, pub dns: Vec<String>, sel: usize }

impl Network {
    pub fn new() -> Network { let mut n = Network { ifaces: vec![], gateway: None, dns: vec![], sel: 0 }; n.refresh(); n }

    pub fn refresh(&mut self) {
        let addrs = parse_ip_addr(&run("ip", &["-o", "addr"]));
        let mut names: Vec<String> = std::fs::read_dir("/sys/class/net").map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
        names.sort();
        self.ifaces = names.into_iter().map(|n| {
            let b = format!("/sys/class/net/{n}");
            Iface { state: rd(&format!("{b}/operstate")), mac: rd(&format!("{b}/address")), mtu: rd(&format!("{b}/mtu")), addrs: addrs.get(&n).cloned().unwrap_or_default(), name: n }
        }).collect();
        self.gateway = parse_gateway(&run("ip", &["route"]));
        self.dns = parse_dns(&rd("/etc/resolv.conf"));
        self.sel = self.sel.min(self.ifaces.len().saturating_sub(1));
    }
}

impl Application for Network {
    fn kind(&self) -> Kind { Kind::Network }
    fn title(&self) -> String { "Network".into() }
    fn interval(&self) -> Option<Duration> { Some(Duration::from_secs(5)) }
    fn tick(&mut self, _env: &Env) { self.refresh(); }
    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        match k.code {
            code::UP => self.sel = self.sel.saturating_sub(1),
            code::DOWN => self.sel = (self.sel + 1).min(self.ifaces.len().saturating_sub(1)),
            _ => if k.base_char().map(|c| c.to_ascii_lowercase()) == Some('r') { self.refresh(); },
        }
        Out::None
    }
    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        p.text(a.x, a.y, "Interfaces", t.dim, None);
        for (i, f) in self.ifaces.iter().enumerate().take(a.h.saturating_sub(6)) {
            let sel = focused && i == self.sel;
            let (fg, bg) = if sel { (t.accent_text, t.accent) } else { (t.text, t.panel) };
            let addr = f.addrs.first().map_or("no address", |s| s.as_str());
            p.clip(a.x, a.y + 1 + i, a.w, &format!(" {:<8} {:<8} {}", f.name, f.state, addr), fg, bg);
        }
        let y = a.y + a.h.saturating_sub(4);
        if let Some(f) = self.ifaces.get(self.sel) {
            p.clip(a.x, y, a.w, &format!("{}  MAC {}  MTU {}", f.name, f.mac, f.mtu), t.dim, t.panel);
        }
        p.clip(a.x, y + 1, a.w, &format!("Gateway: {}", self.gateway.as_deref().unwrap_or("none")), t.text, t.panel);
        p.clip(a.x, y + 2, a.w, &format!("DNS: {}", if self.dns.is_empty() { "none".into() } else { self.dns.join(", ") }), t.text, t.panel);
        p.clip(a.x, y + 3, a.w, "R Refresh   (settings can be changed from the Terminal for now)", t.dim, t.panel);
    }
    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let list: Vec<String> = self.ifaces.iter().map(|f| format!("{}:{}", f.name, n(&f.state))).collect();
        format!("name=network ifaces={} gateway={} dns={}", n(&list.join(",")), n(self.gateway.as_deref().unwrap_or("")), n(&self.dns.join(",")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn draws_and_reports_state() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let mut n = Network::new();
        n.ifaces = vec![Iface { name: "eth0".into(), state: "up".into(), mac: "aa:bb".into(), mtu: "1500".into(), addrs: vec!["1.2.3.4/24".into()] }];
        n.handle(crate::keys::parse_key("down").unwrap(), &Env::for_tests());
        assert!(n.state().contains("eth0:up"));
        for (w, h) in [(10usize, 5usize), (60, 20)] {
            let mut c = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
            n.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
        }
    }
}
