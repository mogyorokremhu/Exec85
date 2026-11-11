//! Finding the disks the system can be installed on.
//! Reads /sys/block (like `lsblk` does) and hides everything that is not a sensible target:
//! RAM disks, loop devices, optical drives, and the medium Exec85 itself was started from.
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Disk { pub name: String, pub bytes: u64, pub model: String, pub removable: bool }

const SKIP_PREFIXES: [&str; 9] = ["loop", "ram", "zram", "sr", "fd", "dm-", "md", "nbd", "mtdblock"];

fn read_trim(p: &Path) -> String { fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default() }

/// True if the device starts with an ISO 9660 file system (the Exec85 install USB stick or disc).
pub fn is_install_media(dev: &Path) -> bool {
    let Ok(mut f) = fs::File::open(dev) else { return false };
    let mut sig = [0u8; 5];
    f.seek(SeekFrom::Start(32769)).is_ok() && f.read_exact(&mut sig).is_ok() && &sig == b"CD001"
}

/// All disks that could receive an installation, sorted by name.
pub fn list(sys_block: &Path, dev_dir: &Path) -> Vec<Disk> {
    let Ok(rd) = fs::read_dir(sys_block) else { return vec![] };
    let mut out: Vec<Disk> = rd.flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if SKIP_PREFIXES.iter().any(|p| name.starts_with(p)) { return None; }
        let sectors: u64 = read_trim(&e.path().join("size")).parse().ok()?;
        if sectors == 0 { return None; }
        if is_install_media(&dev_dir.join(&name)) { return None; }
        let model = [read_trim(&e.path().join("device/vendor")), read_trim(&e.path().join("device/model"))]
            .iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" ");
        Some(Disk {
            removable: read_trim(&e.path().join("removable")) == "1",
            model: if model.is_empty() { "disk".to_string() } else { model },
            bytes: sectors * 512,
            name,
        })
    }).collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn system_disks() -> Vec<Disk> { list(Path::new("/sys/block"), Path::new("/dev")) }

/// Device name of partition `n` (sda -> sda1, nvme0n1 -> nvme0n1p1).
pub fn part_name(disk: &str, n: u32) -> String {
    if disk.ends_with(|c: char| c.is_ascii_digit()) { format!("{disk}p{n}") } else { format!("{disk}{n}") }
}

/// Is any partition of `disk` (or the disk itself) currently mounted?
pub fn is_mounted(mounts: &str, disk: &str) -> bool {
    let base = format!("/dev/{disk}");
    mounts.lines().filter_map(|l| l.split_whitespace().next()).any(|dev| {
        dev == base || (dev.starts_with(&base) && dev[base.len()..].trim_start_matches('p').chars().all(|c| c.is_ascii_digit()) && dev.len() > base.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fake(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("exec85-disks-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (sys, dev) = (base.join("sys"), base.join("dev"));
        fs::create_dir_all(&sys).unwrap();
        fs::create_dir_all(&dev).unwrap();
        (sys, dev)
    }

    fn add(sys: &Path, name: &str, sectors: u64, vendor: &str, model: &str, removable: bool) {
        let d = sys.join(name);
        fs::create_dir_all(d.join("device")).unwrap();
        fs::write(d.join("size"), format!("{sectors}\n")).unwrap();
        fs::write(d.join("removable"), if removable { "1\n" } else { "0\n" }).unwrap();
        fs::write(d.join("device/vendor"), format!("{vendor}\n")).unwrap();
        fs::write(d.join("device/model"), format!("{model}\n")).unwrap();
    }

    #[test]
    fn lists_real_disks_and_hides_the_rest() {
        let (sys, dev) = fake("list");
        add(&sys, "sda", 7_814_037_168, "ATA", "ST4000LM024", false);   // a 4 TB laptop drive
        add(&sys, "nvme0n1", 1_000_215_216, "", "Samsung SSD", false);
        add(&sys, "sr0", 2_097_151, "QEMU", "DVD-ROM", true);
        add(&sys, "loop0", 100, "", "", false);
        add(&sys, "zram0", 100, "", "", false);
        add(&sys, "sdb", 0, "", "", true);                               // empty card reader
        // a USB stick we booted from: starts with an ISO 9660 header
        add(&sys, "sdc", 30_000_000, "SanDisk", "Cruzer", true);
        let mut iso = vec![0u8; 40_000];
        iso[32769..32774].copy_from_slice(b"CD001");
        fs::write(dev.join("sdc"), iso).unwrap();
        fs::write(dev.join("sda"), vec![0u8; 40_000]).unwrap();

        let disks = list(&sys, &dev);
        assert_eq!(disks.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["nvme0n1", "sda"]);
        assert_eq!(disks[1].bytes, 7_814_037_168 * 512);
        assert_eq!(disks[1].model, "ATA ST4000LM024");
        assert_eq!(disks[0].model, "Samsung SSD");
        assert!(!disks[1].removable);
    }

    #[test]
    fn partition_names() {
        assert_eq!(part_name("sda", 2), "sda2");
        assert_eq!(part_name("vda", 3), "vda3");
        assert_eq!(part_name("nvme0n1", 1), "nvme0n1p1");
        assert_eq!(part_name("mmcblk0", 2), "mmcblk0p2");
    }

    #[test]
    fn mounted_disks_are_detected() {
        let mounts = "rootfs / rootfs rw 0 0\n/dev/sda3 /mnt ext4 rw 0 0\n/dev/nvme0n1p2 /boot vfat rw 0 0\n/dev/sdb /data ext4 rw 0 0\n";
        assert!(is_mounted(mounts, "sda"));
        assert!(is_mounted(mounts, "nvme0n1"));
        assert!(is_mounted(mounts, "sdb"));
        assert!(!is_mounted(mounts, "sdc"));
        assert!(!is_mounted(mounts, "sd"), "sd must not match sda3");
    }
}
