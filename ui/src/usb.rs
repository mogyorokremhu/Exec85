//! Pendrives (and other USB disks): notice them, mount them under /media, unmount them when they go away
//! or when the user ejects them. This runs inside the privileged helper. It never runs anything from a drive:
//! drives are mounted with `noexec,nosuid,nodev`.
use crate::helper::Runner;
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fs { Fat, ExFat, Ntfs, Ext, Iso, Unknown }

/// Work out the file system from the first 2 KiB of a partition (and, for CD images, `iso` = the "CD001" check).
pub fn detect(buf: &[u8], iso: bool) -> (Fs, String) {
    let text = |r: std::ops::Range<usize>| -> String {
        let raw: Vec<u8> = buf.get(r).unwrap_or(&[]).iter().copied().take_while(|b| *b != 0).collect();
        String::from_utf8_lossy(&raw).trim().to_string()
    };
    if iso { return (Fs::Iso, String::new()); }
    let at = |o: usize, s: &[u8]| buf.get(o..o + s.len()) == Some(s);
    if buf.len() >= 512 && buf[510] == 0x55 && buf[511] == 0xAA {
        if at(3, b"EXFAT   ") { return (Fs::ExFat, String::new()); }
        if at(3, b"NTFS    ") { return (Fs::Ntfs, String::new()); }
        if at(82, b"FAT32   ") { let l = text(71..82); return (Fs::Fat, if l == "NO NAME" { String::new() } else { l }); }
        if at(54, b"FAT16") || at(54, b"FAT12") || at(54, b"FAT   ") { let l = text(43..54); return (Fs::Fat, if l == "NO NAME" { String::new() } else { l }); }
    }
    if buf.len() >= 1082 && buf[1080] == 0x53 && buf[1081] == 0xEF { return (Fs::Ext, text(1144..1160)); }
    (Fs::Unknown, String::new())
}

/// None while the drive cannot be read yet (a USB stick needs a moment after it appears before its data can be read).
pub fn read_signature(dev: &Path) -> Option<(Fs, String)> {
    let mut f = std::fs::File::open(dev).ok()?;
    let mut buf = vec![0u8; 2048];
    let n = f.read(&mut buf).ok()?;
    if n < 512 { return None; }
    buf.truncate(n);
    let mut cd = [0u8; 5];
    let iso = f.read_exact_at(&mut cd, 32769).is_ok() && &cd == b"CD001";
    Some(detect(&buf, iso))
}

/// A folder name for the drive: letters, digits, - _ . only.
pub fn sanitize_label(label: &str, fallback: &str) -> String {
    let s: String = label.trim().chars().map(|c| if c.is_ascii_alphanumeric() || "-_.".contains(c) { c } else if c == ' ' { '_' } else { '-' }).collect();
    let s = s.trim_matches(|c| c == '-' || c == '.').chars().take(24).collect::<String>();
    if s.is_empty() { fallback.to_string() } else { s }
}

/// (file system type for `mount`, options) for a drive that should be mounted for `uid`:`gid`.
pub fn mount_options(fs: Fs, uid: u32, gid: u32) -> Option<(&'static str, String)> {
    let safe = "noexec,nosuid,nodev";
    Some(match fs {
        Fs::Fat => ("vfat", format!("{safe},uid={uid},gid={gid},umask=022,flush")),
        Fs::ExFat => ("exfat", format!("{safe},uid={uid},gid={gid},umask=022")),
        Fs::Ntfs => ("ntfs3", format!("{safe},uid={uid},gid={gid},umask=022")),
        Fs::Ext => ("ext4", safe.to_string()),
        Fs::Iso | Fs::Unknown => return None,
    })
}

fn fs_name(fs: Fs) -> &'static str { match fs { Fs::Fat => "vfat", Fs::ExFat => "exfat", Fs::Ntfs => "ntfs", Fs::Ext => "ext", Fs::Iso => "iso9660", Fs::Unknown => "unknown" } }

/// Names (sdb1, sdb ...) of partitions on USB disks: a disk without partitions counts as one.
pub fn usb_partitions(sys_block: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(sys_block) else { return out };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if !(name.starts_with("sd") || name.starts_with("vd") || name.starts_with("mmcblk") || name.starts_with("nvme")) { continue; }
        let real = std::fs::canonicalize(e.path()).unwrap_or_else(|_| e.path());
        if !real.to_string_lossy().contains("/usb") { continue; }
        let parts: Vec<String> = std::fs::read_dir(&real).into_iter().flatten().flatten()
            .map(|p| p.file_name().to_string_lossy().into_owned()).filter(|p| p.starts_with(&name) && real.join(p).join("partition").exists()).collect();
        if parts.is_empty() { out.push(name); } else { out.extend(parts); }
    }
    out.sort();
    out
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event { Added { dev: String, fs: &'static str, label: String, mount: String }, Removed { mount: String }, Error(String) }

pub struct Manager {
    pub sys_block: PathBuf,
    pub dev_dir: PathBuf,
    pub media: PathBuf,
    pub state_dir: PathBuf,
    managed: HashMap<String, String>, // device name -> mount point
    failed: HashSet<String>,
    not_ready: HashMap<String, u32>,
}

impl Manager {
    pub fn new(sys_block: &Path, dev_dir: &Path, media: &Path, state_dir: &Path) -> Manager {
        Manager { sys_block: sys_block.into(), dev_dir: dev_dir.into(), media: media.into(), state_dir: state_dir.into(), managed: HashMap::new(), failed: HashSet::new(), not_ready: HashMap::new() }
    }

    fn ejected_path(&self) -> PathBuf { self.state_dir.join("ejected") }
    pub fn read_ejected(state_dir: &Path) -> HashSet<String> { std::fs::read_to_string(state_dir.join("ejected")).unwrap_or_default().lines().map(String::from).collect() }

    /// Remember that the user ejected `dev`, so it is not mounted again until it has been unplugged.
    pub fn mark_ejected(state_dir: &Path, dev: &str) {
        let mut set = Self::read_ejected(state_dir);
        set.insert(dev.to_string());
        let _ = std::fs::create_dir_all(state_dir);
        let _ = std::fs::write(state_dir.join("ejected"), set.into_iter().collect::<Vec<_>>().join("\n"));
    }

    fn unique_mount(&self, label: &str, dev: &str) -> PathBuf {
        let first = self.media.join(label);
        if !first.exists() && !self.managed.values().any(|m| Path::new(m) == first) { return first; }
        self.media.join(format!("{label}-{dev}"))
    }

    /// One look at the system. `mounted` = device names that are already mounted somewhere; `uid`/`gid` own the files on drives that have no owners.
    pub fn scan(&mut self, runner: &dyn Runner, mounted: &HashSet<String>, uid: u32, gid: u32) -> Vec<Event> {
        let mut events = Vec::new();
        let present: HashSet<String> = usb_partitions(&self.sys_block).into_iter().collect();

        // drives that were taken away (or ejected earlier and now unplugged)
        let gone: Vec<String> = self.managed.keys().filter(|d| !present.contains(*d)).cloned().collect();
        for dev in gone {
            if let Some(mount) = self.managed.remove(&dev) {
                runner.run("umount", &["-l", &mount]);
                let _ = std::fs::remove_dir(&mount);
                events.push(Event::Removed { mount });
            }
        }
        // forget about ejected / failed drives once they are unplugged
        let ejected: HashSet<String> = Self::read_ejected(&self.state_dir);
        let still: Vec<&String> = ejected.iter().filter(|d| present.contains(*d)).collect();
        if still.len() != ejected.len() { let _ = std::fs::write(self.ejected_path(), still.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("\n")); }
        self.failed.retain(|d| present.contains(d));
        self.not_ready.retain(|d, _| present.contains(d));

        for dev in &present {
            if self.managed.contains_key(dev) || self.failed.contains(dev) || mounted.contains(dev) || ejected.contains(dev) { continue; }
            let node = self.dev_dir.join(dev);
            if !node.exists() { continue; } // the device file has not appeared yet; look again next time
            // A drive that was only just plugged in may not be readable for a few seconds: try again for up to 30 s before giving up.
            let Some((fs, label)) = read_signature(&node) else {
                let tries = self.not_ready.entry(dev.clone()).or_insert(0);
                *tries += 1;
                if *tries > 15 { self.failed.insert(dev.clone()); events.push(Event::Error(format!("cannot read {dev}"))); }
                continue;
            };
            let Some((typ, opts)) = mount_options(fs, uid, gid) else { self.failed.insert(dev.clone()); continue };
            let mount = self.unique_mount(&sanitize_label(&label, &format!("usb-{dev}")), dev);
            if std::fs::create_dir_all(&mount).is_err() { self.failed.insert(dev.clone()); events.push(Event::Error(format!("cannot create {}", mount.display()))); continue; }
            let (code, out) = runner.run("mount", &["-t", typ, "-o", &opts, &node.to_string_lossy(), &mount.to_string_lossy()]);
            if code == 0 {
                self.managed.insert(dev.clone(), mount.to_string_lossy().into_owned());
                events.push(Event::Added { dev: dev.clone(), fs: fs_name(fs), label, mount: mount.to_string_lossy().into_owned() });
            } else {
                let _ = std::fs::remove_dir(&mount);
                self.failed.insert(dev.clone());
                events.push(Event::Error(format!("cannot mount {dev} ({}): {}", fs_name(fs), out.trim().replace('\n', " "))));
            }
        }
        events
    }
}

/// Safely remove a drive: write everything out, unmount it, remove its folder.
pub fn eject(runner: &dyn Runner, mounts_text: &str, state_dir: &Path, media: &Path, mount: &str) -> Result<String, String> {
    let m = Path::new(mount);
    if !m.starts_with(media) || m == media || mount.contains("..") { return Err("That drive cannot be ejected.".into()); }
    let dev = mounts_text.lines().find_map(|l| { let mut p = l.split_whitespace(); let d = p.next()?; (p.next()? == mount).then(|| d.to_string()) });
    let Some(dev) = dev else { return Err("That drive is not mounted.".into()) };
    runner.run("sync", &[]);
    let (code, out) = runner.run("umount", &[mount]);
    if code != 0 {
        let busy = out.to_lowercase().contains("busy");
        return Err(if busy { "Something is still using the drive. Close its files and folders, then try again.".into() } else { format!("Cannot eject: {}", out.trim()) });
    }
    let _ = std::fs::remove_dir(m);
    if let Some(name) = dev.strip_prefix("/dev/") { Manager::mark_ejected(state_dir, name); }
    Ok("Safe to remove.".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, umount_code: RefCell<(i32, String)> }
    impl Fake { fn new() -> Fake { Fake { calls: RefCell::new(vec![]), umount_code: RefCell::new((0, String::new())) } } }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) { self.calls.borrow_mut().push(format!("{c} {}", a.join(" "))); if c == "umount" { self.umount_code.borrow().clone() } else { (0, String::new()) } }
    }

    fn fat32(label: &str) -> Vec<u8> {
        let mut b = vec![0u8; 2048];
        b[510] = 0x55; b[511] = 0xAA;
        b[82..90].copy_from_slice(b"FAT32   ");
        let mut l = label.as_bytes().to_vec(); l.resize(11, b' ');
        b[71..82].copy_from_slice(&l);
        b
    }

    #[test]
    fn recognises_file_systems_and_labels() {
        assert_eq!(detect(&fat32("HOLIDAY"), false), (Fs::Fat, "HOLIDAY".into()));
        assert_eq!(detect(&fat32("NO NAME"), false), (Fs::Fat, String::new()));
        let mut ex = vec![0u8; 2048]; ex[3..11].copy_from_slice(b"EXFAT   "); ex[510] = 0x55; ex[511] = 0xAA;
        assert_eq!(detect(&ex, false).0, Fs::ExFat);
        let mut nt = vec![0u8; 2048]; nt[3..11].copy_from_slice(b"NTFS    "); nt[510] = 0x55; nt[511] = 0xAA;
        assert_eq!(detect(&nt, false).0, Fs::Ntfs);
        let mut ext = vec![0u8; 2048]; ext[1080] = 0x53; ext[1081] = 0xEF; ext[1144..1148].copy_from_slice(b"data");
        assert_eq!(detect(&ext, false), (Fs::Ext, "data".into()));
        assert_eq!(detect(&vec![0u8; 2048], false).0, Fs::Unknown);
        assert_eq!(detect(&[], false).0, Fs::Unknown, "an empty read is not a crash");
        assert_eq!(detect(&fat32("X"), true).0, Fs::Iso, "CD images are recognised and left alone");
    }

    #[test]
    fn folder_names_are_safe() {
        assert_eq!(sanitize_label("My Photos", "x"), "My_Photos");
        assert_eq!(sanitize_label("../../etc", "x"), "etc");
        assert_eq!(sanitize_label("a/b\\c", "x"), "a-b-c");
        assert_eq!(sanitize_label("   ", "usb-sdb1"), "usb-sdb1");
        assert_eq!(sanitize_label(&"z".repeat(60), "x").len(), 24);
    }

    #[test]
    fn mount_options_never_allow_running_programs_from_the_drive() {
        for fs in [Fs::Fat, Fs::ExFat, Fs::Ntfs, Fs::Ext] {
            let (_, o) = mount_options(fs, 1000, 1000).unwrap();
            assert!(o.contains("noexec") && o.contains("nosuid") && o.contains("nodev"), "{fs:?}: {o}");
        }
        assert!(mount_options(Fs::Fat, 1000, 100).unwrap().1.contains("uid=1000,gid=100"));
        assert!(mount_options(Fs::Iso, 0, 0).is_none() && mount_options(Fs::Unknown, 0, 0).is_none());
    }

    /// A fake /sys + /dev: `sdb` (USB, one partition sdb1) and `sda` (internal disk).
    fn world(tag: &str, part_data: &[u8]) -> (PathBuf, Manager) {
        let d = std::env::temp_dir().join(format!("exec85-usb-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let usb_real = d.join("devices/pci0/usb1/1-1/block/sdb");
        std::fs::create_dir_all(usb_real.join("sdb1")).unwrap();
        std::fs::write(usb_real.join("sdb1/partition"), "1").unwrap();
        let sata_real = d.join("devices/pci0/ata1/block/sda");
        std::fs::create_dir_all(&sata_real).unwrap();
        std::fs::create_dir_all(d.join("block")).unwrap();
        std::os::unix::fs::symlink(&usb_real, d.join("block/sdb")).unwrap();
        std::os::unix::fs::symlink(&sata_real, d.join("block/sda")).unwrap();
        std::fs::create_dir_all(d.join("dev")).unwrap();
        std::fs::write(d.join("dev/sdb1"), part_data).unwrap();
        std::fs::create_dir_all(d.join("media")).unwrap();
        let m = Manager::new(&d.join("block"), &d.join("dev"), &d.join("media"), &d.join("run"));
        (d, m)
    }

    #[test]
    fn only_usb_disks_are_found() {
        let (d, m) = world("find", &fat32("A"));
        assert_eq!(usb_partitions(&m.sys_block), vec!["sdb1"], "the internal disk sda is not offered");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_plugged_in_drive_is_mounted_once_and_unmounted_when_removed() {
        let (d, mut m) = world("plug", &fat32("HOLIDAY"));
        let f = Fake::new();
        let ev = m.scan(&f, &HashSet::new(), 1000, 1000);
        let mount = d.join("media/HOLIDAY").to_string_lossy().into_owned();
        assert_eq!(ev, vec![Event::Added { dev: "sdb1".into(), fs: "vfat", label: "HOLIDAY".into(), mount: mount.clone() }]);
        assert!(f.calls.borrow()[0].starts_with("mount -t vfat -o noexec,nosuid,nodev,uid=1000,gid=1000,umask=022,flush "), "{:?}", f.calls.borrow());
        assert!(m.scan(&f, &HashSet::new(), 1000, 1000).is_empty(), "no second mount");
        assert_eq!(f.calls.borrow().len(), 1);
        std::fs::remove_dir_all(d.join("devices/pci0/usb1")).unwrap(); // unplug
        let ev = m.scan(&f, &HashSet::new(), 1000, 1000);
        assert_eq!(ev, vec![Event::Removed { mount: mount.clone() }]);
        assert!(f.calls.borrow().iter().any(|c| c == &format!("umount -l {mount}")));
        assert!(!Path::new(&mount).exists(), "the empty folder is removed");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_cd_and_already_mounted_drives_are_left_alone() {
        for (tag, data) in [("unk", vec![0u8; 2048]), ("iso", { let mut v = vec![0u8; 40000]; v[32769..32774].copy_from_slice(b"CD001"); v })] {
            let (d, mut m) = world(tag, &data);
            let f = Fake::new();
            assert!(m.scan(&f, &HashSet::new(), 1, 1).is_empty() && f.calls.borrow().is_empty(), "{tag}");
            let _ = std::fs::remove_dir_all(&d);
        }
        let (d, mut m) = world("mounted", &fat32("A"));
        let f = Fake::new();
        assert!(m.scan(&f, &["sdb1".to_string()].into_iter().collect(), 1, 1).is_empty() && f.calls.borrow().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_failed_mount_is_reported_once_not_every_two_seconds() {
        struct Bad;
        impl Runner for Bad { fn run(&self, _: &str, _: &[&str]) -> (i32, String) { (32, "wrong fs type".into()) } }
        let (d, mut m) = world("bad", &fat32("A"));
        let ev = m.scan(&Bad, &HashSet::new(), 1, 1);
        assert!(matches!(&ev[..], [Event::Error(e)] if e.contains("wrong fs type")), "{ev:?}");
        assert!(m.scan(&Bad, &HashSet::new(), 1, 1).is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_drive_that_is_not_readable_yet_is_retried_not_given_up_on() {
        let (d, mut m) = world("late", &[]); // an empty device file: nothing to read yet
        let f = Fake::new();
        for _ in 0..5 { assert!(m.scan(&f, &HashSet::new(), 1, 1).is_empty()); }
        std::fs::write(d.join("dev/sdb1"), fat32("LATE")).unwrap(); // now the stick is ready
        let ev = m.scan(&f, &HashSet::new(), 1, 1);
        assert!(matches!(&ev[..], [Event::Added { label, .. }] if label == "LATE"), "{ev:?}");
        // a drive that never becomes readable is reported once, after a while
        let (d2, mut m2) = world("never", &[]);
        let mut all = Vec::new();
        for _ in 0..40 { all.extend(m2.scan(&f, &HashSet::new(), 1, 1)); }
        assert_eq!(all.len(), 1, "{all:?}");
        let _ = std::fs::remove_dir_all(&d); let _ = std::fs::remove_dir_all(&d2);
    }

    #[test]
    fn two_drives_with_the_same_label_get_different_folders() {
        let (d, mut m) = world("same", &fat32("STICK"));
        std::fs::create_dir_all(d.join("media/STICK")).unwrap();
        let ev = m.scan(&Fake::new(), &HashSet::new(), 1, 1);
        assert!(matches!(&ev[..], [Event::Added { mount, .. }] if mount.ends_with("STICK-sdb1")), "{ev:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ejecting_unmounts_and_the_drive_is_not_mounted_again_until_unplugged() {
        let (d, mut m) = world("eject", &fat32("HOLIDAY"));
        let f = Fake::new();
        m.scan(&f, &HashSet::new(), 1, 1);
        let mount = d.join("media/HOLIDAY").to_string_lossy().into_owned();
        let mounts = format!("/dev/sdb1 {mount} vfat rw 0 0\n");
        assert_eq!(eject(&f, &mounts, &m.state_dir, &m.media, &mount).unwrap(), "Safe to remove.");
        assert!(f.calls.borrow().iter().any(|c| c == "sync ") && f.calls.borrow().iter().any(|c| c == &format!("umount {mount}")));
        m.managed.clear(); // the watcher sees that it is no longer mounted
        let before = f.calls.borrow().len();
        assert!(m.scan(&f, &HashSet::new(), 1, 1).is_empty() && f.calls.borrow().len() == before, "an ejected drive that is still plugged in stays unmounted");
        std::fs::remove_dir_all(d.join("devices/pci0/usb1")).unwrap();
        m.scan(&f, &HashSet::new(), 1, 1);
        assert!(Manager::read_ejected(&m.state_dir).is_empty(), "unplugging clears the ejected mark");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn eject_refuses_what_it_should_and_explains_a_busy_drive() {
        let (d, m) = world("ejbad", &fat32("A"));
        let f = Fake::new();
        let media = &m.media;
        let inside = media.join("X").to_string_lossy().into_owned();
        for bad in ["/", "/home", "/etc", &format!("{}/../etc", media.display()), &media.to_string_lossy()] {
            assert!(eject(&f, "", &m.state_dir, media, bad).is_err(), "{bad}");
        }
        assert!(eject(&f, "", &m.state_dir, media, &inside).unwrap_err().contains("not mounted"));
        assert!(f.calls.borrow().is_empty(), "nothing was run for refused requests");
        *f.umount_code.borrow_mut() = (1, "umount: target is busy".into());
        let e = eject(&f, &format!("/dev/sdb1 {inside} vfat rw 0 0\n"), &m.state_dir, media, &inside).unwrap_err();
        assert!(e.contains("still using"), "{e}");
        let _ = std::fs::remove_dir_all(&d);
    }
}
