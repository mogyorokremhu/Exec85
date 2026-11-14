//! "Exec85 is already installed on this computer."
//!
//! When the install disc (or the virtual CD) is still in the drive, the computer starts the installer again and again.
//! So the installer first looks for an Exec85 on the disks: if there is one, it offers to start it, and does so by itself
//! after a few seconds. "Starting" means loading the installed system's own kernel and jumping straight into it (`kexec`),
//! so the boot order of the computer does not matter. Reinstalling (which erases the disk) is a deliberate second choice.
use crate::disks::{self, Disk};
use crate::gfx::{Canvas, Metrics, Mode, Theme};
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

pub const COUNTDOWN_SECS: u32 = 10;
pub const ROOT_LABEL: &str = "exec85";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found { pub disk: String, pub bytes: u64, pub root_uuid: String, pub esp_dev: String }

/// Label and UUID from an ext4 super block (the 1024 bytes that start 1024 bytes into the partition).
pub fn ext4_label_and_uuid(sb: &[u8]) -> Option<(String, String)> {
    if sb.len() < 1024 || u16::from_le_bytes([sb[56], sb[57]]) != 0xEF53 { return None; }
    let label: String = sb[120..136].iter().take_while(|b| **b != 0).map(|b| *b as char).collect();
    let u = &sb[104..120];
    let uuid = format!("{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        u[0], u[1], u[2], u[3], u[4], u[5], u[6], u[7], u[8], u[9], u[10], u[11], u[12], u[13], u[14], u[15]);
    Some((label, uuid))
}

fn read_super(dev: &Path) -> Option<Vec<u8>> {
    let mut f = File::open(dev).ok()?;
    f.seek(SeekFrom::Start(1024)).ok()?;
    let mut sb = vec![0u8; 1024];
    f.read_exact(&mut sb).ok()?;
    Some(sb)
}

/// The first disk whose third partition is an Exec85 root file system (that is the layout the installer makes).
pub fn find(disks_list: &[Disk], dev_dir: &Path) -> Option<Found> {
    for d in disks_list {
        let root = dev_dir.join(disks::part_name(&d.name, 3));
        if let Some((label, uuid)) = read_super(&root).and_then(|sb| ext4_label_and_uuid(&sb)) {
            if label == ROOT_LABEL { return Some(Found { disk: d.name.clone(), bytes: d.bytes, root_uuid: uuid, esp_dev: dev_dir.join(disks::part_name(&d.name, 2)).to_string_lossy().into_owned() }); }
        }
    }
    None
}

/// Load the installed kernel and jump into it. Only returns (with a message) when something went wrong.
pub fn start(f: &Found) -> Result<(), String> {
    let mnt = "/run/exec85-esp";
    let _ = std::fs::create_dir_all(mnt);
    let out = |c: &mut Command| c.output().map_err(|e| e.to_string());
    let m = out(Command::new("mount").args(["-t", "vfat", "-o", "ro", &f.esp_dev, mnt]))?;
    if !m.status.success() { return Err(format!("Could not read the boot partition: {}", String::from_utf8_lossy(&m.stderr).trim())); }
    let (kernel, initrd) = (format!("{mnt}/vmlinuz"), format!("{mnt}/initramfs"));
    let cmd = crate::install::boot_cmdline(&f.root_uuid);
    // -s lets the kernel hand over the screen details itself (so the screen keeps working); fall back to the older way.
    let mut loaded = false;
    let mut why = String::new();
    let policy = std::fs::read_to_string("/proc/sys/kernel/kexec_load_disabled").unwrap_or_else(|_| "?".into());
    for extra in [Some("-s"), None] {
        let mut c = Command::new("kexec");
        if let Some(e) = extra { c.arg(e); }
        let r = c.args(["-l", &kernel, &format!("--initrd={initrd}"), &format!("--command-line={cmd}")]).output().map_err(|e| format!("kexec is not available: {e}"))?;
        if r.status.success() { loaded = true; break; }
        let one = format!("{}{}", String::from_utf8_lossy(&r.stdout), String::from_utf8_lossy(&r.stderr)).split_whitespace().collect::<Vec<_>>().join(" ");
        why.push_str(&format!("[{}: {}] ", extra.unwrap_or("plain"), one.chars().take(70).collect::<String>()));
    }
    let _ = Command::new("umount").arg(mnt).status();
    if !loaded { return Err(format!("The installed system could not be loaded (kexec_load_disabled={}; {}).", policy.trim(), why.chars().take(230).collect::<String>())); }
    let _ = Command::new("sync").status();
    let r = Command::new("kexec").arg("-e").output().map_err(|e| e.to_string())?;
    Err(format!("Could not jump into the installed system: {}", String::from_utf8_lossy(&r.stderr).trim()))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome { Start, Reinstall }

pub struct Existing {
    pub cols: usize, pub rows: usize, pub mode: Mode,
    pub found: Found,
    /// Seconds left until the installed system starts by itself; None once a key was pressed.
    pub remaining: Option<u32>,
    pub msg: Option<String>,
}

impl Existing {
    pub fn new(cols: usize, rows: usize, mode: Mode, found: Found) -> Existing { Existing { cols, rows, mode, found, remaining: Some(COUNTDOWN_SECS), msg: None } }

    /// One second passed.
    pub fn tick(&mut self) -> Option<Outcome> {
        match self.remaining { Some(0) | None => None, Some(1) => { self.remaining = Some(0); Some(Outcome::Start) } Some(n) => { self.remaining = Some(n - 1); None } }
    }

    pub fn handle(&mut self, k: Key) -> Option<Outcome> {
        if self.remaining.is_some() { self.remaining = None; if k.code != code::ENTER { return None; } } // the first key only stops the countdown
        match k.ch().map(|c| c.to_ascii_lowercase()) {
            Some('r') => Some(Outcome::Reinstall),
            Some('s') => Some(Outcome::Start),
            _ => if k.code == code::ENTER { Some(Outcome::Start) } else { None },
        }
    }

    pub fn state_line(&self) -> String {
        format!("EXEC85_STATE page=existing disk={} countdown={} msg={}", self.found.disk, self.remaining.map_or("stopped".to_string(), |n| n.to_string()), self.msg.as_deref().map_or("-".to_string(), |m| m.chars().take(200).collect::<String>().replace(' ', "_")))
    }
}

pub fn draw(e: &Existing, c: &mut Canvas, m: &Metrics) {
    let t = Theme::new(e.mode);
    c.fill_rect(0, 0, c.w, c.h, t.bg);
    let mut p = Painter { c, m: *m, t };
    p.cells(0, 0, e.cols, 1, t.bar);
    p.text(1, 0, "Exec85", t.bar_text, None);
    let (w, h) = (e.cols.min(60), 14usize.min(e.rows.saturating_sub(2)));
    let r = Rect { x: (e.cols.saturating_sub(w)) / 2, y: ((e.rows.saturating_sub(h)) / 2).max(1), w, h };
    let a = p.window(r, "Exec85 is already installed", true);
    let gb = e.found.bytes as f64 / 1e9;
    p.clip(a.x, a.y, a.w, &format!("Found an installed Exec85 on disk {} ({gb:.0} GB).", e.found.disk), t.text, t.panel);
    p.clip(a.x, a.y + 1, a.w, "Nothing is erased unless you choose to reinstall.", t.dim, t.panel);
    match e.remaining {
        Some(n) => p.clip(a.x, a.y + 4, a.w, &format!("Starting it in {n} seconds. Press any key to choose."), t.accent, t.panel),
        None => p.clip(a.x, a.y + 4, a.w, "What do you want to do?", t.accent, t.panel),
    }
    p.clip(a.x, a.y + 6, a.w, "[Enter] Start the installed system", t.text, t.panel);
    p.clip(a.x, a.y + 7, a.w, "[R]     Reinstall (ERASES the whole disk)", t.text, t.panel);
    if let Some(msg) = &e.msg { for (i, l) in crate::apps::wrap_text(msg, a.w).iter().take(3).enumerate() { p.clip(a.x, a.y + 9 + i, a.w, l, t.accent, t.panel); } }
    let y = e.rows - 1;
    p.cells(0, y, e.cols, 1, t.bar);
    p.clip(1, y, e.cols.saturating_sub(2), "Enter Start   R Reinstall", t.bar_text, t.bar);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;

    fn sb(label: &str) -> Vec<u8> {
        let mut v = vec![0u8; 1024];
        v[56] = 0x53; v[57] = 0xEF;
        for (i, b) in label.bytes().enumerate() { v[120 + i] = b; }
        for i in 0..16 { v[104 + i] = (0x10 + i) as u8; }
        v
    }
    fn found() -> Found { Found { disk: "sda".into(), bytes: 20_000_000_000, root_uuid: "u".into(), esp_dev: "/dev/sda2".into() } }

    #[test]
    fn an_ext4_label_and_uuid_are_read_from_the_super_block() {
        let (l, u) = ext4_label_and_uuid(&sb("exec85")).unwrap();
        assert_eq!(l, "exec85");
        assert_eq!(u, "10111213-1415-1617-1819-1a1b1c1d1e1f");
        assert!(ext4_label_and_uuid(&vec![0u8; 1024]).is_none(), "no magic number, no ext4");
        assert!(ext4_label_and_uuid(&[0u8; 10]).is_none());
    }

    #[test]
    fn an_installed_exec85_is_found_only_when_the_label_matches() {
        let dir = std::env::temp_dir().join(format!("exec85-existing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir); std::fs::create_dir_all(&dir).unwrap();
        let mk = |name: &str, label: &str| { let mut data = vec![0u8; 1024]; data.extend(sb(label)); std::fs::write(dir.join(name), data).unwrap(); };
        let disk = |n: &str| Disk { name: n.into(), bytes: 1 << 34, model: "x".into(), removable: false };
        mk("sda3", "somethingelse"); mk("sdb3", "exec85");
        let f = find(&[disk("sda"), disk("sdb")], &dir).unwrap();
        assert_eq!((f.disk.as_str(), f.root_uuid.as_str()), ("sdb", "10111213-1415-1617-1819-1a1b1c1d1e1f"));
        assert!(f.esp_dev.ends_with("sdb2"));
        assert!(find(&[disk("sda")], &dir).is_none());
        assert!(find(&[disk("nvme0n1")], &dir).is_none(), "no partition at all");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn it_starts_by_itself_after_the_countdown_and_a_key_stops_the_countdown() {
        let mut e = Existing::new(100, 40, Mode::Dark, found());
        for _ in 0..COUNTDOWN_SECS - 1 { assert_eq!(e.tick(), None); }
        assert_eq!(e.tick(), Some(Outcome::Start));
        assert_eq!(e.tick(), None, "only once");
        let mut e = Existing::new(100, 40, Mode::Dark, found());
        assert_eq!(e.handle(parse_key("x").unwrap()), None);
        assert_eq!(e.remaining, None);
        for _ in 0..30 { assert_eq!(e.tick(), None, "no more automatic start after a key"); }
        assert_eq!(e.handle(parse_key("enter").unwrap()), Some(Outcome::Start));
    }

    #[test]
    fn reinstalling_needs_a_deliberate_choice() {
        let mut e = Existing::new(100, 40, Mode::Dark, found());
        assert_eq!(e.handle(parse_key("r").unwrap()), None, "the first key only stops the countdown");
        assert_eq!(e.handle(parse_key("r").unwrap()), Some(Outcome::Reinstall));
        let mut e = Existing::new(100, 40, Mode::Dark, found());
        assert_eq!(e.handle(parse_key("enter").unwrap()), Some(Outcome::Start), "Enter during the countdown starts at once");
    }

    #[test]
    fn it_draws_at_several_sizes_and_states() {
        let mut e = Existing::new(100, 40, Mode::Dark, found());
        for stage in 0..2 {
            if stage == 1 { e.handle(parse_key("x").unwrap()); e.msg = Some("The installed system could not be loaded (kexec failed).".into()); }
            for (cols, rows) in [(8usize, 4usize), (40, 12), (100, 40)] {
                e.cols = cols; e.rows = rows;
                let mut c = Canvas::new(1280, 800);
                draw(&e, &mut c, &Metrics::for_height(800));
            }
        }
        assert!(e.state_line().contains("page=existing") && e.state_line().contains("countdown=stopped"));
    }
}
