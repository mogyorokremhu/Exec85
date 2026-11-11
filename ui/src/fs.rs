//! Directory listing, drives, and number/date formatting for the file browser.
use crate::sys;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size: u64,
    pub mtime: i64,
    pub mode: u32,
    /// The ".." line that goes up one folder.
    pub is_parent: bool,
}

impl Entry {
    pub fn parent() -> Entry {
        Entry { name: "..".into(), is_dir: true, is_link: false, size: 0, mtime: 0, mode: 0, is_parent: true }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sort { Name, Size, Date }

pub fn read_dir(path: &Path, show_hidden: bool, sort: Sort) -> io::Result<Vec<Entry>> {
    let mut v = Vec::new();
    for e in fs::read_dir(path)? {
        let Ok(e) = e else { continue };
        let name = e.file_name().to_string_lossy().into_owned();
        if !show_hidden && name.starts_with('.') { continue; }
        let lmeta = fs::symlink_metadata(e.path()).ok();
        let is_link = lmeta.as_ref().map_or(false, |m| m.file_type().is_symlink());
        // Follow links so a link to a folder behaves like a folder.
        let meta = fs::metadata(e.path()).ok().or(lmeta);
        let (is_dir, size, mtime, mode) = match &meta {
            Some(m) => (m.is_dir(), m.len(), m.mtime(), m.mode()),
            None => (false, 0, 0, 0),
        };
        v.push(Entry { name, is_dir, is_link, size, mtime, mode, is_parent: false });
    }
    sort_entries(&mut v, sort);
    Ok(v)
}

/// Folders first, then by the chosen key.
pub fn sort_entries(v: &mut [Entry], sort: Sort) {
    let by_name = |a: &Entry, b: &Entry| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then_with(|| a.name.cmp(&b.name));
    v.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then_with(|| match sort {
            Sort::Name => by_name(a, b),
            Sort::Size => b.size.cmp(&a.size).then_with(|| by_name(a, b)),
            Sort::Date => b.mtime.cmp(&a.mtime).then_with(|| by_name(a, b)),
        })
    });
}

pub fn fmt_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "K", "M", "G", "T"];
    if n < 1024 { return format!("{n}B"); }
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1024.0 && i < 4 { v /= 1024.0; i += 1; }
    if v < 10.0 { format!("{:.1}{}", v, U[i]) } else { format!("{:.0}{}", v, U[i]) }
}

/// Disk sizes the way drive makers label them (powers of 1000): a "4 TB" drive shows as 4.0TB.
pub fn fmt_disk(n: u64) -> String {
    const U: [&str; 5] = ["B", "kB", "MB", "GB", "TB"];
    let (mut v, mut i) = (n as f64, 0);
    while v >= 1000.0 && i < 4 { v /= 1000.0; i += 1; }
    if i == 0 { format!("{n}B") } else if v < 10.0 { format!("{:.1}{}", v, U[i]) } else { format!("{:.0}{}", v, U[i]) }
}

/// Days since 1970-01-01 to (year, month, day). Public-domain algorithm by Howard Hinnant.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// "2026-11-11 15:02" in UTC (time zones arrive with the installer).
pub fn fmt_time(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

pub fn mode_string(mode: u32, is_dir: bool, is_link: bool) -> String {
    let mut s = String::new();
    s.push(if is_link { 'l' } else if is_dir { 'd' } else { '-' });
    for (i, c) in "rwxrwxrwx".chars().enumerate() {
        s.push(if mode & (0o400 >> i) != 0 { c } else { '-' });
    }
    s
}

#[derive(Clone, Debug)]
pub struct Drive {
    pub label: String,
    pub mount: PathBuf,
    pub fstype: String,
    pub total: u64,
    pub free: u64,
}

fn unescape_mount(s: &str) -> String {
    // /proc/mounts writes spaces etc. as \040 (octal)
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() + 0 && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else { out.push(b[i]); i += 1; }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lines of /proc/mounts -> (device, mount point, filesystem type).
pub fn parse_mounts(s: &str) -> Vec<(String, String, String)> {
    s.lines().filter_map(|l| {
        let mut p = l.split_whitespace();
        Some((unescape_mount(p.next()?), unescape_mount(p.next()?), p.next()?.to_string()))
    }).collect()
}

/// Which mounts count as "drives": the root, and anything backed by a real device.
pub fn pick_drives(mounts: &[(String, String, String)]) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    for m in mounts {
        if (m.1 == "/" || m.0.starts_with("/dev/")) && !out.iter().any(|o| o.1 == m.1) { out.push(m.clone()); }
    }
    if !out.iter().any(|o| o.1 == "/") { out.insert(0, ("rootfs".into(), "/".into(), "rootfs".into())); }
    out.sort_by(|a, b| (a.1 != "/").cmp(&(b.1 != "/")).then_with(|| a.1.cmp(&b.1)));
    out
}

pub fn drives() -> Vec<Drive> {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    pick_drives(&parse_mounts(&text)).into_iter().map(|(dev, mount, fstype)| {
        let label = if mount == "/" { "root".to_string() } else {
            Path::new(&mount).file_name().map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| dev.rsplit('/').next().unwrap_or("disk").to_string())
        };
        let (total, free) = sys::disk_space(Path::new(&mount)).unwrap_or((0, 0));
        Drive { label, mount: PathBuf::from(mount), fstype, total, free }
    }).collect()
}

/// Index of the drive that contains `path` (the one with the longest matching mount point).
pub fn drive_of(drives: &[Drive], path: &Path) -> usize {
    drives.iter().enumerate()
        .filter(|(_, d)| path.starts_with(&d.mount))
        .max_by_key(|(_, d)| d.mount.components().count())
        .map_or(0, |(i, _)| i)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(fmt_size(0), "0B");
        assert_eq!(fmt_size(1023), "1023B");
        assert_eq!(fmt_size(1024), "1.0K");
        assert_eq!(fmt_size(1536), "1.5K");
        assert_eq!(fmt_size(10 * 1024 * 1024), "10M");
        assert_eq!(fmt_size(4 * 1024u64.pow(4)), "4.0T");
    }

    #[test]
    fn disk_sizes_use_drive_maker_units() {
        assert_eq!(fmt_disk(4_000_787_030_016), "4.0TB");
        assert_eq!(fmt_disk(512_110_190_592), "512GB");
        assert_eq!(fmt_disk(31_914_983_424), "32GB");
        assert_eq!(fmt_disk(999), "999B");
        assert_eq!(fmt_disk(1_000_204_886_016), "1.0TB");
    }

    #[test]
    fn dates() {
        assert_eq!(fmt_time(0), "1970-01-01 00:00");
        assert_eq!(fmt_time(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(fmt_time(951_782_400), "2000-02-29 00:00"); // leap day
        assert_eq!(fmt_time(-86_400), "1969-12-31 00:00");
    }

    #[test]
    fn permissions() {
        assert_eq!(mode_string(0o755, true, false), "drwxr-xr-x");
        assert_eq!(mode_string(0o644, false, false), "-rw-r--r--");
        assert_eq!(mode_string(0o777, false, true), "lrwxrwxrwx");
    }

    #[test]
    fn mounts_become_drives() {
        let text = "rootfs / rootfs rw 0 0\nproc /proc proc rw 0 0\n/dev/sda2 /mnt/big\\040disk ext4 rw 0 0\ntmpfs /run tmpfs rw 0 0\n/dev/sda1 /boot vfat rw 0 0\n";
        let d = pick_drives(&parse_mounts(text));
        let mounts: Vec<&str> = d.iter().map(|m| m.1.as_str()).collect();
        assert_eq!(mounts, vec!["/", "/boot", "/mnt/big disk"]);
        assert_eq!(pick_drives(&[])[0].1, "/");
    }

    #[test]
    fn path_belongs_to_longest_mount() {
        let mk = |m: &str| Drive { label: m.into(), mount: PathBuf::from(m), fstype: "x".into(), total: 0, free: 0 };
        let ds = vec![mk("/"), mk("/mnt/a"), mk("/mnt")];
        assert_eq!(drive_of(&ds, Path::new("/etc")), 0);
        assert_eq!(drive_of(&ds, Path::new("/mnt/a/x")), 1);
        assert_eq!(drive_of(&ds, Path::new("/mnt/b")), 2);
        assert_eq!(drive_of(&ds, Path::new("/mntx")), 0);
    }

    #[test]
    fn listing_puts_folders_first_and_respects_hidden_and_sort() {
        let dir = std::env::temp_dir().join(format!("exec85-fs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("Beta")).unwrap();
        fs::create_dir_all(dir.join("alpha")).unwrap();
        fs::write(dir.join("zebra.txt"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("apple.txt"), vec![0u8; 5000]).unwrap();
        fs::write(dir.join(".hidden"), b"x").unwrap();
        let names = |v: Vec<Entry>| v.into_iter().map(|e| e.name).collect::<Vec<_>>();
        assert_eq!(names(read_dir(&dir, false, Sort::Name).unwrap()), ["alpha", "Beta", "apple.txt", "zebra.txt"]);
        assert_eq!(names(read_dir(&dir, true, Sort::Name).unwrap()).len(), 5);
        assert_eq!(names(read_dir(&dir, false, Sort::Size).unwrap()), ["alpha", "Beta", "apple.txt", "zebra.txt"]);
        let _ = fs::remove_dir_all(&dir);
    }
}
