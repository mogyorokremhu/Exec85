//! "Exec85 is already installed on this computer."
//!
//! When the install disc (or the virtual CD) is still in the drive, the computer starts the installer again and again.
//! So the installer first looks for an Exec85 on the disks: if there is one, it offers to start it, and does so by itself
//! after a few seconds. "Starting" means switching from the disc's system to the installed one without a reboot (see `write_handoff`),
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
pub struct Found { pub disk: String, pub bytes: u64, pub root_uuid: String, pub root_dev: String }

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
            if label == ROOT_LABEL { return Some(Found { disk: d.name.clone(), bytes: d.bytes, root_uuid: uuid, root_dev: root.to_string_lossy().into_owned() }); }
        }
    }
    None
}

/// Note which disk to start. The disc's first process (`/etc/exec85/live-init`, which is PID 1) reads it after this
/// program exits with `EXIT_START_INSTALLED`, mounts that disk and switches to its init (`switch_root`), without a reboot.
pub fn write_handoff(f: &Found) -> Result<(), String> {
    if !f.root_dev.starts_with("/dev/") || f.root_dev.contains("..") || f.root_dev.chars().any(|c| c.is_whitespace()) { return Err("That is not a disk I can start from.".into()); }
    std::fs::write(HANDOFF_FILE, format!("{}\n", f.root_dev)).map_err(|e| format!("Could not prepare the hand-over: {e}"))
}

pub const HANDOFF_FILE: &str = "/handoff-root";
/// Exit code of `exec85 --existing` that means "start the installed system".
pub const EXIT_START_INSTALLED: i32 = 10;

/// What to do while looking for an installed system at boot.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Wait { Keep, GiveUp }

/// What the look at the disks has seen so far.
#[derive(Debug, Clone, Copy, Default)]
pub struct Look {
    pub since_start: std::time::Duration,
    /// Time since the first disk appeared (None: no disk yet).
    pub since_first_disk: Option<std::time::Duration>,
    /// Does any visible disk already have partitions listed? (If not, its partition table may still be being read.)
    pub partitions_seen: bool,
    /// Is there a storage controller (SATA, IDE, NVMe, USB) in this computer, so that a disk can still show up?
    pub controller: bool,
    /// An installed Exec85 was already found: nothing more to wait for.
    pub found: bool,
}

/// Disks appear a while after their driver loads: a few seconds on a normal computer, much longer in a virtual machine without
/// hardware help (a VirtualBox log showed the disk appearing long after the screen). So the wait is generous when there is
/// a storage controller and a disk may still come, and short when there is none; once a disk is visible it is given a little
/// longer for its partitions to be read (more if none are listed yet), and then we stop.
pub fn next_wait(l: &Look) -> Wait {
    use std::time::Duration as D;
    if l.found { return Wait::GiveUp; }
    match l.since_first_disk {
        None => if l.since_start >= if l.controller { D::from_secs(20) } else { D::from_secs(2) } { Wait::GiveUp } else { Wait::Keep },
        Some(d) => if d >= if l.partitions_seen { D::from_secs(3) } else { D::from_secs(6) } { Wait::GiveUp } else { Wait::Keep },
    }
}

/// True when a PCI device of a class that can hold a disk is present: mass storage (0x01..) or a USB controller (0x0c03).
pub fn storage_controller_present(pci_dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(pci_dir) else { return false };
    rd.flatten().any(|e| std::fs::read_to_string(e.path().join("class")).map_or(false, |c| { let c = c.trim().trim_start_matches("0x"); c.starts_with("01") || c.starts_with("0c03") }))
}

/// Does `disk` (a folder in /sys/block) list at least one partition (sda1 ...)?
pub fn has_partitions(sys_block: &Path, disk: &str) -> bool {
    std::fs::read_dir(sys_block.join(disk)).map_or(false, |rd| rd.flatten().any(|e| { let n = e.file_name().to_string_lossy().into_owned(); n.starts_with(disk) && n.len() > disk.len() }))
}

/// Look for an installed Exec85, giving the disks time to appear (see `next_wait`). `mdev -s` makes the device files.
pub fn find_waiting() -> Option<Found> {
    let start = std::time::Instant::now();
    let controller = storage_controller_present(Path::new("/sys/bus/pci/devices"));
    let mut first_disk: Option<std::time::Instant> = None;
    loop {
        let list = disks::system_disks();
        if !list.is_empty() && first_disk.is_none() { first_disk = Some(std::time::Instant::now()); }
        let found = find(&list, Path::new("/dev"));
        if found.is_some() { return found; }
        let look = Look { since_start: start.elapsed(), since_first_disk: first_disk.map(|t| t.elapsed()), partitions_seen: list.iter().any(|d| has_partitions(Path::new("/sys/block"), &d.name)), controller, found: false };
        if next_wait(&look) == Wait::GiveUp { return None; }
        let _ = Command::new("mdev").arg("-s").status();
        std::thread::sleep(std::time::Duration::from_millis(300));
    }
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
    fn found() -> Found { Found { disk: "sda".into(), bytes: 20_000_000_000, root_uuid: "u".into(), root_dev: "/dev/sda3".into() } }

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
        assert!(f.root_dev.ends_with("sdb3"));
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
    fn it_waits_for_slow_disks_but_not_forever() {
        use std::time::Duration as D;
        let look = |start: u64, first: Option<u64>, parts: bool, controller: bool, found: bool| Look { since_start: D::from_secs(start), since_first_disk: first.map(D::from_secs), partitions_seen: parts, controller, found };
        assert_eq!(next_wait(&look(10, None, false, true, false)), Wait::Keep, "a controller is there: a disk may still come, even after 10 seconds (slow VM)");
        assert_eq!(next_wait(&look(19, None, false, true, false)), Wait::Keep);
        assert_eq!(next_wait(&look(20, None, false, true, false)), Wait::GiveUp, "but not forever");
        assert_eq!(next_wait(&look(1, None, false, false, false)), Wait::Keep);
        assert_eq!(next_wait(&look(2, None, false, false, false)), Wait::GiveUp, "no controller at all: nothing to wait for");
        assert_eq!(next_wait(&look(9, Some(1), false, true, false)), Wait::Keep, "a disk is there but its partitions are not listed yet");
        assert_eq!(next_wait(&look(9, Some(6), false, true, false)), Wait::GiveUp, "a blank disk does not hold the boot for long");
        assert_eq!(next_wait(&look(9, Some(2), true, true, false)), Wait::Keep, "partitions listed: the device files may still be coming");
        assert_eq!(next_wait(&look(9, Some(3), true, true, false)), Wait::GiveUp, "a disk with other systems on it does not either");
        assert_eq!(next_wait(&look(0, None, false, true, true)), Wait::GiveUp, "found: nothing more to wait for");
    }

    #[test]
    fn storage_controllers_and_partitions_are_recognised_in_sysfs() {
        let d = std::env::temp_dir().join(format!("exec85-pci-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let dev = |n: &str, class: &str| { std::fs::create_dir_all(d.join("pci").join(n)).unwrap(); std::fs::write(d.join("pci").join(n).join("class"), class).unwrap(); };
        assert!(!storage_controller_present(&d.join("pci")));
        dev("0000:00:02.0", "0x030000\n");                  // a display
        dev("0000:00:03.0", "0x020000\n");                  // a network card
        assert!(!storage_controller_present(&d.join("pci")));
        dev("0000:00:1f.2", "0x010601\n");                  // SATA (AHCI)
        assert!(storage_controller_present(&d.join("pci")));
        let _ = std::fs::remove_dir_all(d.join("pci")); dev("0000:00:0d.0", "0x0c0330\n");   // a USB 3 controller
        assert!(storage_controller_present(&d.join("pci")));
        std::fs::create_dir_all(d.join("block/sda/sda1")).unwrap(); std::fs::create_dir_all(d.join("block/sdb/queue")).unwrap();
        assert!(has_partitions(&d.join("block"), "sda") && !has_partitions(&d.join("block"), "sdb") && !has_partitions(&d.join("block"), "nosuch"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn only_a_plain_device_path_can_be_handed_over_to() {
        for bad in ["sda3", "/dev/../etc", "/dev/sda3 ; reboot", "/etc/passwd", ""] {
            let mut f = found(); f.root_dev = bad.into();
            assert!(write_handoff(&f).is_err(), "{bad:?}");
        }
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
            if stage == 1 { e.handle(parse_key("x").unwrap()); e.msg = Some("The installed system could not be loaded (for testing).".into()); }
            for (cols, rows) in [(8usize, 4usize), (40, 12), (100, 40)] {
                e.cols = cols; e.rows = rows;
                let mut c = Canvas::new(1280, 800);
                draw(&e, &mut c, &Metrics::for_height(800));
            }
        }
        assert!(e.state_line().contains("page=existing") && e.state_line().contains("countdown=stopped"));
    }
}
