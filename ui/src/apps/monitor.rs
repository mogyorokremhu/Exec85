//! System monitor: CPU, memory, load and the running processes. End a process with K.
use super::{Application, Env, Kind, Out};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::time::Duration;

// ---- reading /proc (pure parsers, tested with sample text) -------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct CpuTimes { pub total: u64, pub idle: u64 }

/// Lines "cpu ..." (all CPUs together, first) and "cpu0 ...", "cpu1 ..." of /proc/stat.
pub fn parse_stat(s: &str) -> Vec<CpuTimes> {
    s.lines().filter(|l| l.starts_with("cpu")).filter_map(|l| {
        let n: Vec<u64> = l.split_whitespace().skip(1).filter_map(|v| v.parse().ok()).collect();
        if n.len() < 4 { return None; }
        // user nice system idle iowait irq softirq steal ... : idle time = idle + iowait
        let idle = n[3] + n.get(4).copied().unwrap_or(0);
        Some(CpuTimes { total: n.iter().take(8).sum(), idle })
    }).collect()
}

/// Busy percentage between two readings.
pub fn cpu_percent(prev: CpuTimes, now: CpuTimes) -> f64 {
    let total = now.total.saturating_sub(prev.total);
    if total == 0 { return 0.0; }
    100.0 * (total - now.idle.saturating_sub(prev.idle).min(total)) as f64 / total as f64
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Mem { pub total_kb: u64, pub avail_kb: u64, pub swap_total_kb: u64, pub swap_free_kb: u64 }

pub fn parse_meminfo(s: &str) -> Mem {
    let get = |k: &str| s.lines().find(|l| l.starts_with(k)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0);
    Mem { total_kb: get("MemTotal:"), avail_kb: get("MemAvailable:"), swap_total_kb: get("SwapTotal:"), swap_free_kb: get("SwapFree:") }
}

pub fn parse_loadavg(s: &str) -> (f64, f64, f64) {
    let v: Vec<f64> = s.split_whitespace().take(3).filter_map(|x| x.parse().ok()).collect();
    (v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proc { pub pid: i32, pub name: String, pub state: char, pub ticks: u64, pub rss_kb: u64, pub uid: u32 }

/// One /proc/<pid>/stat line. The name sits in parentheses and may itself contain spaces or ')'.
pub fn parse_proc_stat(stat: &str, page_kb: u64) -> Option<Proc> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let pid: i32 = stat[..open].trim().parse().ok()?;
    let rest: Vec<&str> = stat[close + 1..].split_whitespace().collect();
    // rest[0] = state (field 3); utime = field 14 -> rest[11]; stime = field 15 -> rest[12]; rss = field 24 -> rest[21]
    let num = |i: usize| rest.get(i).and_then(|v| v.parse::<u64>().ok());
    Some(Proc { pid, name: stat[open + 1..close].to_string(), state: rest.first()?.chars().next()?, ticks: num(11)? + num(12)?, rss_kb: num(21)? * page_kb, uid: 0 })
}

pub fn fmt_kb(kb: u64) -> String {
    if kb >= 10 << 20 { format!("{}G", kb >> 20) } else if kb >= 1 << 20 { format!("{:.1}G", kb as f64 / 1048576.0) } else if kb >= 10 << 10 { format!("{}M", kb >> 10) } else if kb >= 1024 { format!("{:.1}M", kb as f64 / 1024.0) } else { format!("{kb}K") }
}

pub fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86400, secs % 86400 / 3600, secs % 3600 / 60);
    if d > 0 { format!("{d}d {h}h") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m {}s", secs % 60) }
}

#[derive(Clone, Default)]
pub struct Snapshot { pub cpu: Vec<CpuTimes>, pub mem: Mem, pub load: (f64, f64, f64), pub uptime: u64, pub procs: Vec<Proc> }

pub fn read_snapshot(proc_root: &Path) -> Snapshot {
    use std::os::unix::fs::MetadataExt;
    let rd = |f: &str| fs::read_to_string(proc_root.join(f)).unwrap_or_default();
    let mut procs = Vec::new();
    if let Ok(dir) = fs::read_dir(proc_root) {
        for e in dir.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if !name.bytes().all(|b| b.is_ascii_digit()) { continue; }
            if let Ok(stat) = fs::read_to_string(e.path().join("stat")) {
                if let Some(mut p) = parse_proc_stat(&stat, 4) { p.uid = e.metadata().map_or(0, |m| m.uid()); procs.push(p); }
            }
        }
    }
    Snapshot {
        cpu: parse_stat(&rd("stat")), mem: parse_meminfo(&rd("meminfo")), load: parse_loadavg(&rd("loadavg")),
        uptime: rd("uptime").split_whitespace().next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u64, procs,
    }
}

// ---- the app --------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort { Cpu, Mem, Pid, Name }

#[derive(Clone, Debug)]
pub struct Row { pub p: Proc, pub cpu: f64 }

pub struct Monitor {
    root: std::path::PathBuf,
    prev: Snapshot,
    cur: Snapshot,
    cpu_pct: Vec<f64>,
    rows: Vec<Row>,
    users: HashMap<u32, String>,
    sort: Sort,
    sel: usize,
    top: usize,
    view_rows: usize,
    confirm: Option<(i32, bool)>, // (pid, force)
    msg: String,
}

impl Monitor {
    pub fn new() -> Monitor { Monitor::with_root("/proc".into()) }

    pub fn with_root(root: std::path::PathBuf) -> Monitor {
        let first = read_snapshot(&root);
        let users = crate::account::users(Path::new("/")).into_iter().map(|u| (u.uid, u.name)).collect();
        let mut m = Monitor { root, prev: first.clone(), cur: first, cpu_pct: Vec::new(), rows: Vec::new(), users, sort: Sort::Cpu, sel: 0, top: 0, view_rows: 10, confirm: None, msg: String::new() };
        m.rebuild();
        m
    }

    /// Take a new reading and work out the percentages since the previous one.
    pub fn refresh(&mut self) {
        let snap = read_snapshot(&self.root);
        self.prev = std::mem::replace(&mut self.cur, snap);
        self.rebuild();
    }

    fn rebuild(&mut self) {
        self.cpu_pct = self.cur.cpu.iter().zip(self.prev.cpu.iter()).map(|(n, p)| cpu_percent(*p, *n)).collect();
        let ncpu = self.cur.cpu.len().saturating_sub(1).max(1) as f64;
        let total_delta = match (self.cur.cpu.first(), self.prev.cpu.first()) { (Some(n), Some(p)) => n.total.saturating_sub(p.total), _ => 0 };
        let prev_ticks: HashMap<i32, u64> = self.prev.procs.iter().map(|p| (p.pid, p.ticks)).collect();
        let selected_pid = self.rows.get(self.sel).map(|r| r.p.pid);
        self.rows = self.cur.procs.iter().map(|p| {
            let d = p.ticks.saturating_sub(prev_ticks.get(&p.pid).copied().unwrap_or(p.ticks));
            Row { p: p.clone(), cpu: if total_delta == 0 { 0.0 } else { 100.0 * d as f64 * ncpu / total_delta as f64 } }
        }).collect();
        self.sort_rows();
        if let Some(pid) = selected_pid { if let Some(i) = self.rows.iter().position(|r| r.p.pid == pid) { self.sel = i; } }
        self.sel = self.sel.min(self.rows.len().saturating_sub(1));
        self.fix_scroll();
    }

    fn sort_rows(&mut self) {
        match self.sort {
            Sort::Cpu => self.rows.sort_by(|a, b| b.cpu.partial_cmp(&a.cpu).unwrap_or(std::cmp::Ordering::Equal).then(b.p.rss_kb.cmp(&a.p.rss_kb)).then(a.p.pid.cmp(&b.p.pid))),
            Sort::Mem => self.rows.sort_by(|a, b| b.p.rss_kb.cmp(&a.p.rss_kb).then(a.p.pid.cmp(&b.p.pid))),
            Sort::Pid => self.rows.sort_by_key(|r| r.p.pid),
            Sort::Name => self.rows.sort_by(|a, b| a.p.name.to_lowercase().cmp(&b.p.name.to_lowercase()).then(a.p.pid.cmp(&b.p.pid))),
        }
    }

    fn fix_scroll(&mut self) {
        let rows = self.view_rows.max(1);
        if self.sel < self.top { self.top = self.sel; }
        if self.sel >= self.top + rows { self.top = self.sel + 1 - rows; }
    }

    fn selected(&self) -> Option<&Row> { self.rows.get(self.sel) }

    fn end_process(&mut self, pid: i32, force: bool) {
        self.msg = if pid <= 1 { "Refusing to end the system's first process.".into() } else {
            match crate::sys::send_signal(pid, if force { 9 } else { 15 }) {
                Ok(()) => format!("Sent {} to {pid}.", if force { "KILL" } else { "TERM" }),
                Err(e) => format!("Cannot end {pid}: {e}"),
            }
        };
    }
}

impl Application for Monitor {
    fn kind(&self) -> Kind { Kind::Monitor }
    fn title(&self) -> String { "System monitor".into() }
    fn interval(&self) -> Option<Duration> { Some(Duration::from_secs(1)) }
    fn tick(&mut self, _env: &Env) { self.refresh(); }
    fn resize(&mut self, _cols: usize, rows: usize) { self.view_rows = rows.saturating_sub(7).max(1); self.fix_scroll(); }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        if let Some((pid, force)) = self.confirm {
            match k.ch().map(|c| c.to_ascii_lowercase()) {
                Some('y') => { self.confirm = None; self.end_process(pid, force); self.refresh(); }
                _ => if k.code == code::ESC || k.ch().map(|c| c.to_ascii_lowercase()) == Some('n') { self.confirm = None; self.msg.clear(); },
            }
            return Out::None;
        }
        self.msg.clear();
        match k.code {
            code::UP => self.sel = self.sel.saturating_sub(1),
            code::DOWN => self.sel = (self.sel + 1).min(self.rows.len().saturating_sub(1)),
            code::PGUP => self.sel = self.sel.saturating_sub(self.view_rows),
            code::PGDN => self.sel = (self.sel + self.view_rows).min(self.rows.len().saturating_sub(1)),
            code::HOME => self.sel = 0,
            code::END => self.sel = self.rows.len().saturating_sub(1),
            code::DELETE => if let Some(r) = self.selected() { self.confirm = Some((r.p.pid, false)); },
            _ => if let Some(c) = k.ch() {
                match c {
                    'c' => { self.sort = Sort::Cpu; self.sort_rows(); }
                    'm' => { self.sort = Sort::Mem; self.sort_rows(); }
                    'p' => { self.sort = Sort::Pid; self.sort_rows(); }
                    'n' => { self.sort = Sort::Name; self.sort_rows(); }
                    'k' => if let Some(r) = self.selected() { self.confirm = Some((r.p.pid, false)); },
                    'K' => if let Some(r) = self.selected() { self.confirm = Some((r.p.pid, true)); },
                    'r' => self.refresh(),
                    _ => {}
                }
            },
        }
        self.fix_scroll();
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 8 || a.w < 30 { return; }
        let bar_w = a.w.saturating_sub(24).clamp(6, 40);
        let agg = self.cpu_pct.first().copied().unwrap_or(0.0);
        let ncpu = self.cur.cpu.len().saturating_sub(1);
        p.text(a.x, a.y, "CPU ", t.dim, None);
        p.bar(a.x + 4, a.y, bar_w, agg / 100.0, t.accent, t.bg);
        p.text(a.x + 5 + bar_w, a.y, &format!("{agg:>3.0}%  {ncpu} core{}", if ncpu == 1 { "" } else { "s" }), t.text, None);
        let m = self.cur.mem;
        let used = m.total_kb.saturating_sub(m.avail_kb);
        p.text(a.x, a.y + 1, "Mem ", t.dim, None);
        p.bar(a.x + 4, a.y + 1, bar_w, if m.total_kb == 0 { 0.0 } else { used as f64 / m.total_kb as f64 }, t.accent, t.bg);
        p.text(a.x + 5 + bar_w, a.y + 1, &format!("{} / {}", fmt_kb(used), fmt_kb(m.total_kb)), t.text, None);
        if m.swap_total_kb > 0 {
            p.text(a.x, a.y + 2, "Swap", t.dim, None);
            let su = m.swap_total_kb - m.swap_free_kb;
            p.bar(a.x + 4, a.y + 2, bar_w, su as f64 / m.swap_total_kb as f64, t.accent, t.bg);
            p.text(a.x + 5 + bar_w, a.y + 2, &format!("{} / {}", fmt_kb(su), fmt_kb(m.swap_total_kb)), t.text, None);
        }
        p.clip(a.x, a.y + 3, a.w, &format!("Load {:.2} {:.2} {:.2}   Up {}   {} processes", self.cur.load.0, self.cur.load.1, self.cur.load.2, fmt_uptime(self.cur.uptime), self.rows.len()), t.dim, t.panel);
        // process table
        let hy = a.y + 5;
        p.clip(a.x, hy, a.w, "  PID  USER       CPU%    MEM  NAME", t.dim, t.bar);
        let rows = a.h.saturating_sub(7);
        for i in 0..rows {
            let idx = self.top + i;
            let Some(r) = self.rows.get(idx) else { break };
            let (fg, bg) = if idx == self.sel { if focused { (t.accent_text, t.accent) } else { (t.title_off_text, t.title_off) } } else { (t.text, t.panel) };
            let user = self.users.get(&r.p.uid).cloned().unwrap_or_else(|| r.p.uid.to_string());
            p.clip(a.x, hy + 1 + i, a.w, &format!("{:>5}  {:<9} {:>5.1} {:>6}  {}", r.p.pid, user.chars().take(9).collect::<String>(), r.cpu, fmt_kb(r.p.rss_kb), r.p.name), fg, bg);
        }
        let y = a.y + a.h - 1;
        match self.confirm {
            Some((pid, force)) => p.clip(a.x, y, a.w, &format!("End process {pid}{}? [Y] Yes  [N] No", if force { " (force)" } else { "" }), t.accent, t.bar),
            None => p.clip(a.x, y, a.w, &self.msg, t.dim, t.bar),
        }
    }

    fn state(&self) -> String {
        let m = self.cur.mem;
        format!("name=monitor cpus={} cpu_pct={:.0} mem_used_kb={} mem_total_kb={} procs={} sel_pid={} sort={} uptime={}",
            self.cur.cpu.len().saturating_sub(1), self.cpu_pct.first().copied().unwrap_or(0.0), m.total_kb.saturating_sub(m.avail_kb), m.total_kb,
            self.rows.len(), self.selected().map_or(0, |r| r.p.pid),
            match self.sort { Sort::Cpu => "cpu", Sort::Mem => "mem", Sort::Pid => "pid", Sort::Name => "name" }, self.cur.uptime)
    }
    fn hint(&self) -> &'static str { "Up/Down select   C M P N sort   K end process (Shift+K force)   R refresh   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    const STAT: &str = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 60 0 30 400 10 0 0 0 0 0\ncpu1 40 0 20 400 40 0 0 0 0 0\nintr 1 2 3\nctxt 99\n";

    #[test]
    fn cpu_lines_and_percentages() {
        let v = parse_stat(STAT);
        assert_eq!(v.len(), 3, "total plus two cores");
        assert_eq!(v[0], CpuTimes { total: 1000, idle: 850 });
        let later = CpuTimes { total: 1200, idle: 950 };
        assert!((cpu_percent(v[0], later) - 50.0).abs() < 1e-9, "200 ticks passed, 100 of them idle");
        assert_eq!(cpu_percent(v[0], v[0]), 0.0, "no time passed");
        assert_eq!(cpu_percent(CpuTimes::default(), CpuTimes { total: 100, idle: 0 }), 100.0);
    }

    #[test]
    fn memory_and_load() {
        let m = parse_meminfo("MemTotal:  8000000 kB\nMemFree: 100 kB\nMemAvailable: 6000000 kB\nSwapTotal: 2000 kB\nSwapFree: 500 kB\n");
        assert_eq!(m, Mem { total_kb: 8_000_000, avail_kb: 6_000_000, swap_total_kb: 2000, swap_free_kb: 500 });
        assert_eq!(parse_loadavg("0.52 0.58 0.59 1/345 6789\n"), (0.52, 0.58, 0.59));
        assert_eq!(parse_loadavg(""), (0.0, 0.0, 0.0));
    }

    #[test]
    fn process_lines_with_awkward_names() {
        let line = "1234 (my (odd) name) S 1 1234 1234 0 -1 4194560 100 0 0 0 70 30 0 0 20 0 1 0 5000 12345678 2500 18446744073709551615 0 0";
        let p = parse_proc_stat(line, 4).unwrap();
        assert_eq!((p.pid, p.name.as_str(), p.state, p.ticks, p.rss_kb), (1234, "my (odd) name", 'S', 100, 10_000));
        assert!(parse_proc_stat("garbage", 4).is_none());
        assert!(parse_proc_stat("1 (x) S 1", 4).is_none(), "too short");
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_kb(512), "512K");
        assert_eq!(fmt_kb(1536), "1.5M");
        assert_eq!(fmt_kb(50 << 10), "50M");
        assert_eq!(fmt_kb(3 << 20), "3.0G");
        assert_eq!(fmt_kb(16 << 20), "16G");
        assert_eq!(fmt_uptime(59), "0m 59s");
        assert_eq!(fmt_uptime(3700), "1h 1m");
        assert_eq!(fmt_uptime(90_000), "1d 1h");
    }

    /// A pretend /proc with three processes.
    fn fake_proc(tag: &str, busy_ticks: [u64; 3]) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("exec85-mon-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("stat"), STAT).unwrap();
        fs::write(root.join("meminfo"), "MemTotal: 1000 kB\nMemAvailable: 400 kB\n").unwrap();
        fs::write(root.join("loadavg"), "0.10 0.20 0.30 1/2 3\n").unwrap();
        fs::write(root.join("uptime"), "125.50 100.00\n").unwrap();
        for (i, (name, rss)) in [("init", 100), ("worker", 900), ("idle", 10)].iter().enumerate() {
            let dir = root.join((i + 1).to_string());
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join("stat"), format!("{} ({name}) S 0 0 0 0 0 0 0 0 0 0 {} 0 0 0 0 0 0 0 0 0 {rss} 0", i + 1, busy_ticks[i])).unwrap();
        }
        root
    }

    #[test]
    fn snapshot_percentages_and_sorting() {
        let root = fake_proc("snap", [10, 20, 0]);
        let mut m = Monitor::with_root(root.clone());
        assert_eq!(m.rows.len(), 3);
        // the "worker" uses 100 more ticks while the CPUs (two of them) advance by 200 ticks in total
        fs::write(root.join("stat"), "cpu  200 0 100 1500 100 0 0 0 0 0\ncpu0 120 0 60 750 20 0 0 0 0 0\ncpu1 80 0 40 750 80 0 0 0 0 0\n").unwrap();
        fs::write(root.join("2/stat"), "2 (worker) S 0 0 0 0 0 0 0 0 0 0 120 0 0 0 0 0 0 0 0 0 900 0").unwrap();
        m.refresh();
        // total went from 1000 to 1900: +900; busy went from 150 to 300
        assert!((m.cpu_pct[0] - 100.0 * 150.0 / 900.0).abs() < 1e-6, "{:?}", m.cpu_pct);
        assert_eq!(m.rows[0].p.name, "worker", "sorted by CPU: the busy one first");
        assert!((m.rows[0].cpu - 100.0 * 100.0 * 2.0 / 900.0).abs() < 1e-6, "{}", m.rows[0].cpu);
        assert_eq!(m.cur.uptime, 125);
        let press = |m: &mut Monitor, k: &str| { m.handle(parse_key(k).unwrap(), &Env::for_tests()); };
        press(&mut m, "p");
        assert_eq!(m.rows.iter().map(|r| r.p.pid).collect::<Vec<_>>(), vec![1, 2, 3]);
        press(&mut m, "m");
        assert_eq!(m.rows[0].p.name, "worker", "memory: 900 pages first");
        press(&mut m, "n");
        assert_eq!(m.rows.iter().map(|r| r.p.name.as_str()).collect::<Vec<_>>(), vec!["idle", "init", "worker"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn selection_survives_refreshes_and_ending_a_process_asks_first() {
        let root = fake_proc("sel", [0, 0, 0]);
        let mut m = Monitor::with_root(root.clone());
        let press = |m: &mut Monitor, k: &str| { m.handle(parse_key(k).unwrap(), &Env::for_tests()); };
        press(&mut m, "p");
        press(&mut m, "down");
        assert_eq!(m.selected().unwrap().p.pid, 2);
        m.refresh();
        assert_eq!(m.selected().unwrap().p.pid, 2, "same process still selected after the list is rebuilt");
        press(&mut m, "up");
        press(&mut m, "k");
        assert!(m.confirm.is_some());
        press(&mut m, "n");
        assert!(m.confirm.is_none());
        press(&mut m, "k");
        press(&mut m, "y");
        assert!(m.msg.contains("Refusing"), "pid 1 is never ended: {}", m.msg);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_real_proc_can_be_read() {
        let s = read_snapshot(Path::new("/proc"));
        assert!(!s.procs.is_empty() && s.mem.total_kb > 0 && !s.cpu.is_empty());
        assert!(s.procs.iter().any(|p| p.pid == std::process::id() as i32), "this test process is in the list");
    }

    #[test]
    fn draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let root = fake_proc("draw", [1, 2, 3]);
        let m = Monitor::with_root(root.clone());
        for (cols, rows) in [(20usize, 5usize), (40, 12), (100, 40)] {
            let met = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut canvas, m: met, t: Theme::new(Mode::Dark) };
            m.draw(&mut p, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
        let _ = fs::remove_dir_all(&root);
    }
}
