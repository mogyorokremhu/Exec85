//! Disk usage: how much room each drive has left, and which folders and files take the space.
//! Scans run in the background so the screen never freezes; Esc stops one.
use super::{Application, Env, Kind, Out};
use crate::fs::{self as xfs, fmt_size, Drive};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item { pub name: String, pub size: u64, pub is_dir: bool, pub other_fs: bool }

#[derive(Clone, Default)]
pub struct ScanState { pub path: PathBuf, pub items: Vec<Item>, pub files: u64, pub total: u64, pub done: bool, pub stopped: bool, pub error: Option<String>, pub gen: u64 }

/// Space actually used on disk (whole blocks), like `du`.
fn allocated(m: &std::fs::Metadata) -> u64 { m.blocks() * 512 }

/// Total size of everything under `dir`, staying on the same disk and never following links.
pub fn dir_size(dir: &Path, dev: u64, cancel: &AtomicBool, files: &mut u64) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if cancel.load(Ordering::Relaxed) { return total; }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue }; // does not follow links
            if m.is_dir() { total += allocated(&m); if m.dev() == dev { stack.push(e.path()); } }
            else { total += allocated(&m); *files += 1; }
        }
    }
    total
}

/// Scan the children of `path`. `report` is called after each child with the items so far (sorted by size).
pub fn scan(path: &Path, cancel: &AtomicBool, report: &mut dyn FnMut(&[Item], u64, bool)) -> Result<(), String> {
    let root_meta = std::fs::metadata(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let dev = root_meta.dev();
    let mut children: Vec<_> = std::fs::read_dir(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?.flatten().collect();
    children.sort_by_key(|e| e.file_name());
    let mut items: Vec<Item> = Vec::new();
    let mut files = 0u64;
    for e in children {
        if cancel.load(Ordering::Relaxed) { report(&items, files, false); return Ok(()); }
        let Ok(m) = e.metadata() else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        let item = if m.is_dir() {
            if m.dev() != dev { Item { name, size: 0, is_dir: true, other_fs: true } }
            else { let s = allocated(&m) + dir_size(&e.path(), dev, cancel, &mut files); Item { name, size: s, is_dir: true, other_fs: false } }
        } else { files += 1; Item { name, size: allocated(&m), is_dir: false, other_fs: false } };
        items.push(item);
        items.sort_by(|a, b| b.size.cmp(&a.size).then(a.name.cmp(&b.name)));
        report(&items, files, false);
    }
    report(&items, files, true);
    Ok(())
}

pub struct DiskUsage {
    state: Arc<Mutex<ScanState>>,
    cancel: Arc<AtomicBool>,
    drives: Vec<Drive>,
    sel: usize,
    top: usize,
    rows: usize,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl DiskUsage {
    pub fn new(env: &Env) -> DiskUsage {
        let mut d = DiskUsage { state: Arc::new(Mutex::new(ScanState::default())), cancel: Arc::new(AtomicBool::new(false)), drives: xfs::drives(), sel: 0, top: 0, rows: 8, wake: env.wake.clone() };
        d.start(env.cwd.clone());
        d
    }

    fn start(&mut self, path: PathBuf) {
        self.cancel.store(true, Ordering::SeqCst);          // stop any scan still running
        self.cancel = Arc::new(AtomicBool::new(false));
        let gen = self.state.lock().unwrap().gen + 1;
        *self.state.lock().unwrap() = ScanState { path: path.clone(), gen, ..ScanState::default() };
        self.sel = 0; self.top = 0;
        self.drives = xfs::drives();
        let (state, cancel, wake) = (self.state.clone(), self.cancel.clone(), self.wake.clone());
        std::thread::spawn(move || {
            let mut last = std::time::Instant::now() - std::time::Duration::from_secs(1);
            let r = scan(&path, &cancel, &mut |items, files, done| {
                // update the screen at most ~10 times a second
                if !done && last.elapsed() < std::time::Duration::from_millis(100) { return; }
                last = std::time::Instant::now();
                let mut s = state.lock().unwrap();
                if s.gen != gen { return; }
                s.items = items.to_vec(); s.files = files; s.total = items.iter().map(|i| i.size).sum(); s.done = done;
                s.stopped = !done && cancel.load(Ordering::Relaxed);
                drop(s);
                wake();
            });
            let mut s = state.lock().unwrap();
            if s.gen == gen { if let Err(e) = r { s.error = Some(e); s.done = true; } }
            drop(s);
            wake();
        });
    }

    pub fn snapshot(&self) -> ScanState { self.state.lock().unwrap().clone() }

    fn fix_scroll(&mut self, n: usize) {
        let rows = self.rows.max(1);
        self.sel = self.sel.min(n.saturating_sub(1));
        if self.sel < self.top { self.top = self.sel; }
        if self.sel >= self.top + rows { self.top = self.sel + 1 - rows; }
    }
}

impl Drop for DiskUsage { fn drop(&mut self) { self.cancel.store(true, Ordering::SeqCst); } }

impl Application for DiskUsage {
    fn kind(&self) -> Kind { Kind::DiskUsage }
    fn title(&self) -> String { "Disk usage".into() }
    fn resize(&mut self, _cols: usize, rows: usize) { self.rows = rows.saturating_sub(self.drives.len().min(4) + 4).max(1); }

    fn handle(&mut self, k: Key, _env: &Env) -> Out {
        let s = self.snapshot();
        let n = s.items.len();
        match k.code {
            code::UP => self.sel = self.sel.saturating_sub(1),
            code::DOWN => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            code::PGUP => self.sel = self.sel.saturating_sub(self.rows),
            code::PGDN => self.sel = (self.sel + self.rows).min(n.saturating_sub(1)),
            code::HOME => self.sel = 0,
            code::END => self.sel = n.saturating_sub(1),
            code::ENTER | code::RIGHT => if let Some(it) = s.items.get(self.sel) { if it.is_dir && !it.other_fs { self.start(s.path.join(&it.name)); } },
            code::BACKSPACE | code::LEFT => if let Some(p) = s.path.parent() { self.start(p.to_path_buf()); },
            code::ESC => { self.cancel.store(true, Ordering::SeqCst); }
            _ => if k.ch() == Some('r') { self.start(s.path.clone()); },
        }
        self.fix_scroll(n);
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        if a.h < 6 || a.w < 24 { return; }
        let mut y = a.y;
        let bar_w = a.w.saturating_sub(34).clamp(6, 30);
        for d in self.drives.iter().take(4) {
            if d.total > 0 {
                let used = d.total - d.free;
                p.clip(a.x, y, 10, &d.label, t.text, t.panel);
                p.bar(a.x + 11, y, bar_w, used as f64 / d.total as f64, t.accent, t.bg);
                p.text(a.x + 12 + bar_w, y, &format!("{} of {}", fmt_size(used), fmt_size(d.total)), t.dim, None);
            } else { p.clip(a.x, y, a.w, &format!("{}  ({})", d.label, d.fstype), t.dim, t.panel); }
            y += 1;
        }
        y += 1;
        let s = self.snapshot();
        let status = if let Some(e) = &s.error { e.clone() } else if s.done { format!("{} in {} files", fmt_size(s.total), s.files) } else if s.stopped { format!("Stopped at {} ({} files)", fmt_size(s.total), s.files) } else { format!("Scanning... {} so far ({} files)", fmt_size(s.total), s.files) };
        p.clip(a.x, y, a.w, &s.path.to_string_lossy(), t.accent, t.panel);
        p.clip(a.x, y + 1, a.w, &status, t.dim, t.panel);
        y += 2;
        let rows = a.h.saturating_sub(y - a.y);
        let name_w = a.w.saturating_sub(bar_w + 12).max(8);
        let max = s.items.first().map_or(1, |i| i.size).max(1);
        for i in 0..rows {
            let idx = self.top + i;
            let Some(it) = s.items.get(idx) else { break };
            let (fg, bg) = if idx == self.sel { if focused { (t.accent_text, t.accent) } else { (t.title_off_text, t.title_off) } } else { (t.text, t.panel) };
            p.cells(a.x, y + i, a.w, 1, bg);
            let label = if it.other_fs { format!("{}/ (other disk)", it.name) } else if it.is_dir { format!("{}/", it.name) } else { it.name.clone() };
            p.text(a.x, y + i, &label.chars().take(name_w).collect::<String>(), fg, None);
            p.bar(a.x + name_w + 1, y + i, bar_w, it.size as f64 / max as f64, if idx == self.sel { fg } else { t.accent }, if idx == self.sel { bg } else { t.bg });
            p.right(a.x + name_w + 2 + bar_w, y + i, 8, &fmt_size(it.size), fg, bg);
        }
    }

    fn state(&self) -> String {
        let s = self.snapshot();
        format!("name=diskusage path={} scanning={} entries={} total={} files={} top={} sel={}",
            s.path.to_string_lossy().replace(' ', "_"), !s.done && !s.stopped, s.items.len(), s.total, s.files,
            s.items.first().map_or("-".to_string(), |i| i.name.replace(' ', "_")), s.items.get(self.sel).map_or("-".to_string(), |i| i.name.replace(' ', "_")))
    }
    fn hint(&self) -> &'static str { "Up/Down select   Enter open folder   Backspace up   R rescan   Esc stop   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::fs;

    fn tree(tag: &str) -> PathBuf {
        let r = std::env::temp_dir().join(format!("exec85-du-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&r);
        fs::create_dir_all(r.join("a/deep")).unwrap();
        fs::create_dir_all(r.join("b")).unwrap();
        fs::write(r.join("big.bin"), vec![1u8; 200_000]).unwrap();
        fs::write(r.join("a/f1"), vec![1u8; 40_000]).unwrap();
        fs::write(r.join("a/deep/f2"), vec![1u8; 20_000]).unwrap();
        fs::write(r.join("b/f3"), vec![1u8; 5_000]).unwrap();
        std::os::unix::fs::symlink(r.join("a"), r.join("link")).unwrap();
        r
    }

    fn run_scan(p: &Path) -> (Vec<Item>, u64) {
        let mut out = (Vec::new(), 0);
        scan(p, &AtomicBool::new(false), &mut |items, files, done| { if done { out = (items.to_vec(), files); } }).unwrap();
        out
    }

    #[test]
    fn sizes_are_summed_biggest_first_and_links_are_not_followed() {
        let r = tree("sizes");
        let (items, files) = run_scan(&r);
        let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names[0], "big.bin");
        assert_eq!(names[1], "a");
        assert_eq!(names[2], "b");
        assert!(items[0].size >= 200_000 && items[0].size < 210_000, "{}", items[0].size);
        let a = items.iter().find(|i| i.name == "a").unwrap();
        assert!(a.size >= 60_000 && a.size < 90_000, "a includes its subfolder: {}", a.size);
        let link = items.iter().find(|i| i.name == "link").unwrap();
        assert!(link.size < 8192 && !link.is_dir, "a link counts as itself only: {}", link.size);
        assert_eq!(files, 5, "big.bin, f1, f2, f3, and the link");
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn stopping_returns_quickly_with_what_was_found() {
        let r = tree("cancel");
        let cancel = AtomicBool::new(true);
        let mut last = None;
        scan(&r, &cancel, &mut |items, _, done| last = Some((items.len(), done))).unwrap();
        assert_eq!(last, Some((0, false)), "stopped before the first folder, and says it did not finish");
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn unreadable_paths_are_reported() {
        assert!(scan(Path::new("/definitely/not/here"), &AtomicBool::new(false), &mut |_, _, _| {}).is_err());
    }

    fn wait_done(d: &DiskUsage) -> ScanState {
        for _ in 0..200 { let s = d.snapshot(); if s.done { return s; } std::thread::sleep(std::time::Duration::from_millis(25)); }
        panic!("scan did not finish");
    }

    #[test]
    fn navigating_into_folders_and_back() {
        let r = tree("nav");
        let env = Env { cwd: r.clone(), ..Env::for_tests() };
        let mut d = DiskUsage::new(&env);
        let s = wait_done(&d);
        assert_eq!(s.path, r);
        assert!(d.state().contains("top=big.bin"));
        let press = |d: &mut DiskUsage, k: &str| { d.handle(parse_key(k).unwrap(), &env); };
        press(&mut d, "down");
        assert!(d.state().contains("sel=a"));
        press(&mut d, "enter");
        let s = wait_done(&d);
        assert_eq!(s.path, r.join("a"));
        assert_eq!(s.items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), vec!["f1", "deep"], "biggest first");
        press(&mut d, "backspace");
        let s = wait_done(&d);
        assert_eq!(s.path, r, "Backspace goes to the parent folder");
        press(&mut d, "esc");
        let _ = fs::remove_dir_all(&r);
    }

    #[test]
    fn draws_at_several_sizes_while_scanning_and_after() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let r = tree("draw");
        let env = Env { cwd: r.clone(), ..Env::for_tests() };
        let d = DiskUsage::new(&env);
        for (cols, rows) in [(24usize, 6usize), (50, 14), (110, 40)] {
            let m = Metrics::for_height(800);
            let mut canvas = Canvas::new(1280, 800);
            let mut p = Painter { c: &mut canvas, m, t: Theme::new(Mode::Dark) };
            d.draw(&mut p, Rect { x: 1, y: 1, w: cols, h: rows }, true);
        }
        wait_done(&d);
        let _ = fs::remove_dir_all(&r);
    }
}
