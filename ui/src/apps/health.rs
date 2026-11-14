//! Disk health and temperatures: each disk's SMART verdict and temperature, and the processor's temperature.
use super::{Application, Backend, Env, Job, Kind, Out};
use crate::disks::{self, Disk};
use crate::keys::Key;
use crate::layout::Rect;
use crate::paint::Painter;
use crate::smart::{self, Health, Level, Overall};
use std::time::Duration;

pub struct HealthApp {
    be: Backend,
    job: Job,
    disks: Vec<Disk>,
    health: Vec<Option<Health>>,
    cpu: Option<i32>,
    asked: usize,
    msg: String,
    first: bool,
}

impl HealthApp {
    pub fn new(be: Backend) -> HealthApp { HealthApp { be, job: Job::new(), disks: vec![], health: vec![], cpu: None, asked: 0, msg: String::new(), first: true } }

    fn refresh(&mut self, env: &Env) {
        self.disks = disks::system_disks();
        self.health = vec![None; self.disks.len()];
        self.cpu = smart::cpu_temp_c(std::path::Path::new("/sys"));
        self.asked = 0;
        self.next(env);
    }

    fn next(&mut self, env: &Env) {
        if let Some(d) = self.disks.get(self.asked) { let name = d.name.clone(); self.job.start(env, &self.be, &format!("disk:{}", self.asked), vec!["disk-health".into(), name]); }
    }

    fn finish(&mut self, env: &Env) {
        let Some((tag, r)) = self.job.poll() else { return };
        if let Some(i) = tag.strip_prefix("disk:").and_then(|n| n.parse::<usize>().ok()) {
            if i < self.health.len() { self.health[i] = Some(match r { Ok(t) => smart::parse(&t), Err(e) => { self.msg = e; smart::parse("") } }); }
            self.asked = i + 1;
            self.next(env);
        }
    }
}

impl Application for HealthApp {
    fn kind(&self) -> Kind { Kind::Health }
    fn title(&self) -> String { "Disk health".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(300) } else { Duration::from_secs(30) }) }
    fn tick(&mut self, env: &Env) {
        self.finish(env);
        if self.first || (!self.job.busy && self.asked >= self.disks.len()) { self.first = false; self.refresh(env); }
    }
    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish(env);
        if k.ch().map(|c| c.to_ascii_lowercase()) == Some('r') && !self.job.busy { self.refresh(env); }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, _focused: bool) {
        let t = p.t;
        let colour = |l: Level| match l { Level::Ok => t.text, Level::Warm => t.accent, Level::Hot => t.accent };
        let cpu = match self.cpu { Some(c) => (format!("Processor temperature: {c} C{}", match smart::level(c, true) { Level::Ok => "", Level::Warm => "  (warm)", Level::Hot => "  (TOO HOT)" }), colour(smart::level(c, true))), None => ("Processor temperature: not reported by this computer".to_string(), t.dim) };
        p.clip(a.x, a.y, a.w, &cpu.0, cpu.1, t.panel);
        let mut y = a.y + 2;
        if self.disks.is_empty() { p.clip(a.x, y, a.w, "No disks found.", t.dim, t.panel); }
        for (i, d) in self.disks.iter().enumerate() {
            if y + 1 >= a.y + a.h { break; }
            p.clip(a.x, y, a.w, &format!("{}  {:.0} GB  {}", d.name, d.bytes as f64 / 1e9, d.model), t.text, t.panel);
            let (line, col) = match self.health.get(i).and_then(|h| h.as_ref()) {
                None => ("checking ...".to_string(), t.dim),
                Some(h) => { let c = match (h.overall, h.temp_c) { (Overall::Failed, _) => t.accent, (_, Some(tc)) => colour(smart::level(tc, false)), _ => t.dim }; (smart::describe(h), c) }
            };
            p.clip(a.x + 2, y + 1, a.w.saturating_sub(2), &line, col, t.panel);
            y += 3;
        }
        p.clip(a.x, a.y + a.h.saturating_sub(1), a.w, if self.msg.is_empty() { "R Check again" } else { &self.msg }, t.dim, t.panel);
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let done = self.health.iter().filter(|h| h.is_some()).count();
        format!("name=health disks={} checked={done} cpu={} busy={} msg={}", self.disks.len(), self.cpu.map_or("-".to_string(), |c| c.to_string()), self.job.busy, n(&self.msg.chars().take(50).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "R Check again   F4 Close" }
}

/// The same report for the terminal (`health`).
pub fn report(be: &Backend, out: &mut dyn std::io::Write) {
    let cpu = smart::cpu_temp_c(std::path::Path::new("/sys"));
    let _ = writeln!(out, "Processor temperature: {}", cpu.map_or("not reported by this computer".to_string(), |c| format!("{c} C")));
    for d in disks::system_disks() {
        let h = be(&["disk-health".to_string(), d.name.clone()]).map(|t| smart::parse(&t)).unwrap_or_else(|_| smart::parse(""));
        let _ = writeln!(out, "disk {} ({:.0} GB, {}) {}", d.name, d.bytes as f64 / 1e9, d.model, smart::describe(&h));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn the_report_lists_every_disk_with_its_verdict() {
        let calls = Arc::new(Mutex::new(vec![])); let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); Ok("SMART overall-health self-assessment test result: PASSED\n194 Temperature_Celsius     0x0022   065   045   000    Old_age   Always       -       35\n".into()) });
        let mut out = Vec::new(); report(&be, &mut out);
        let t = String::from_utf8(out).unwrap();
        assert!(t.starts_with("Processor temperature:"), "{t}");
        for l in calls.lock().unwrap().iter() { assert!(l.starts_with("disk-health ")); }
        for l in t.lines().skip(1) { assert!(l.starts_with("disk ") && l.contains("good") && l.contains("35 C"), "{l}"); }
    }

    #[test]
    fn the_screen_checks_each_disk_in_turn_and_draws_at_every_size() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let be: Backend = Arc::new(|_r: &[String]| Ok("SMART support is: Unavailable\n".into()));
        let mut a = HealthApp::new(be);
        a.disks = vec![Disk { name: "sda".into(), bytes: 20_000_000_000, model: "QEMU HARDDISK".into(), removable: false }, Disk { name: "sdb".into(), bytes: 1_000_000_000_000, model: "X".into(), removable: false }];
        a.health = vec![None; 2]; a.first = false;
        let e = Env::for_tests();
        a.next(&e);
        for _ in 0..300 { a.finish(&e); if !a.job.busy && a.asked >= 2 { break; } std::thread::sleep(Duration::from_millis(5)); }
        assert!(a.state().contains("disks=2") && a.state().contains("checked=2"), "{}", a.state());
        a.cpu = Some(95);
        for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
            let mut c = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
            a.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
        }
    }
}
