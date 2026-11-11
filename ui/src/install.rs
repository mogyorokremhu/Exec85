//! The installation itself: partition a disk, make file systems, copy the running system onto it,
//! write the settings, create the user, install the bootloader.
//!
//! Disk layout (GPT, works for BIOS and UEFI machines and for disks over 2 TB):
//!   1. 1 MiB   BIOS boot partition  (needed by the BIOS bootloader on GPT disks)
//!   2. 512 MiB EFI System partition (FAT32): bootloader, kernel and boot image. Mounted at /boot.
//!   3. the rest: ext4 root file system
use crate::account;
use crate::disks;
use crate::keys::Layout;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct Config {
    pub disk: String,
    pub hostname: String,
    pub username: String,
    pub password: Option<String>,
    pub layout: Layout,
    pub timezone: String,
    pub dark: bool,
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub dev: PathBuf,        // /dev
    pub target: PathBuf,     // where the new system is assembled
    pub assets: PathBuf,     // kernel, boot image and bootloader files shipped on the install image
    pub zoneinfo: PathBuf,   // /usr/share/zoneinfo
}

impl Paths {
    pub fn live() -> Paths {
        Paths { dev: "/dev".into(), target: "/target".into(), assets: "/usr/share/exec85/install".into(), zoneinfo: "/usr/share/zoneinfo".into() }
    }
}

/// Shared between the installer thread and the screen.
#[derive(Clone, Debug, Default)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub current: String,
    pub log: Vec<String>,
    /// None while running; Some(Ok) or Some(Err(message)) when finished.
    pub result: Option<Result<(), String>>,
}

pub type Shared = Arc<Mutex<Progress>>;

pub const STEPS: [&str; 8] = [
    "Partitioning the disk", "Creating file systems", "Mounting", "Copying system files",
    "Writing settings", "Creating your account", "Installing the bootloader", "Finishing",
];

// ---- text that ends up on the new system (pure functions, unit tested) -----------------------

/// Script for `sfdisk`: a GPT table with the three partitions described above.
pub fn sfdisk_script() -> String {
    "label: gpt\n\
     size=1MiB, type=21686148-6449-6E6F-744E-656564454649, name=\"BIOS boot\"\n\
     size=512MiB, type=C12A7328-F81F-11D2-BA4B-00A0C93EC93B, name=\"EFI System\"\n\
     type=0FC63DAF-8483-4772-8E79-3D69D8477DE4, name=\"Exec85\"\n".to_string()
}

/// Bootloader configuration. Everything it needs sits on the EFI/boot partition.
pub fn limine_conf(root_uuid: &str) -> String {
    format!(
        "timeout: 0\nquiet: yes\n\n/Exec85\n    protocol: linux\n    path: boot():/vmlinuz\n    module_path: boot():/initramfs\n    cmdline: root=UUID={root_uuid} rootfstype=ext4 rw loglevel=3 console=tty0 console=ttyS0,115200\n"
    )
}

pub fn fstab(root_uuid: &str, esp_id: &str) -> String {
    format!("UUID={root_uuid}\t/\text4\trw,relatime\t0 1\nUUID={esp_id}\t/boot\tvfat\trw,umask=0077\t0 2\n")
}

pub fn hosts(hostname: &str) -> String {
    format!("127.0.0.1\tlocalhost\n127.0.1.1\t{hostname}\n::1\t\tlocalhost ip6-localhost ip6-loopback\n")
}

const GROUPS_FOR_FIRST_USER: [&str; 5] = ["wheel", "input", "video", "audio", "users"];

/// Settings files written into the new root.
/// `ids` = (root uuid, boot partition id) for a real installation; `None` for the testing "set up in memory" path.
pub fn write_settings(cfg: &Config, root: &Path, zoneinfo: &Path, ids: Option<(&str, &str)>) -> Result<(), String> {
    let w = |rel: &str, text: String| -> Result<(), String> {
        let p = root.join(rel);
        if let Some(d) = p.parent() { fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?; }
        fs::write(&p, text).map_err(|e| format!("{}: {e}", p.display()))
    };
    w("etc/hostname", format!("{}\n", cfg.hostname))?;
    w("etc/hosts", hosts(&cfg.hostname))?;
    if let Some((root_uuid, esp_id)) = ids {
        w("etc/fstab", fstab(root_uuid, esp_id))?;
        w("etc/exec85/installed", "1\n".to_string())?;
    }
    w("etc/exec85/theme", if cfg.dark { "dark\n" } else { "light\n" }.to_string())?;
    w("etc/exec85/keyboard", format!("{}\n", cfg.layout.id()))?;
    w("etc/timezone", format!("{}\n", cfg.timezone))?;
    let zone = zoneinfo.join(&cfg.timezone);
    if cfg.timezone != "UTC" && zone.is_file() {
        let _ = fs::remove_file(root.join("etc/localtime"));
        fs::copy(&zone, root.join("etc/localtime")).map_err(|e| format!("timezone: {e}"))?;
    }
    // Administrators (group "wheel") can run commands as root with `doas`.
    w("etc/doas.d/exec85.conf", "permit persist :wheel\n".to_string())?;
    Ok(())
}

/// Lock root and create the first user (an administrator).
pub fn create_accounts(cfg: &Config, root: &Path) -> Result<(), String> {
    account::lock_root(root).map_err(|e| format!("locking root: {e}"))?;
    account::add_user(root, &cfg.username, cfg.password.as_deref(), &GROUPS_FOR_FIRST_USER).map_err(|e| format!("creating user: {e}"))?;
    Ok(())
}

// ---- running commands ---------------------------------------------------------------------------

fn tail(s: &str) -> String {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(4)..].join(" | ")
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.stdin(Stdio::null()).output().map_err(|e| format!("{what}: cannot start: {e}"))?;
    if out.status.success() { return Ok(()); }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Err(format!("{what} failed ({}): {}", out.status.code().map_or("signal".into(), |c| c.to_string()), tail(&text)))
}

fn run_with_input(cmd: &mut Command, input: &str, what: &str) -> Result<(), String> {
    let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(|e| format!("{what}: cannot start: {e}"))?;
    child.stdin.take().ok_or("no stdin")?.write_all(input.as_bytes()).map_err(|e| format!("{what}: {e}"))?;
    let out = child.wait_with_output().map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() { return Ok(()); }
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Err(format!("{what} failed: {}", tail(&text)))
}

fn random_hex(bytes: usize) -> String {
    use std::io::Read;
    let mut b = vec![0u8; bytes];
    if let Ok(mut f) = fs::File::open("/dev/urandom") { let _ = f.read_exact(&mut b); }
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn random_uuid() -> String {
    fs::read_to_string("/proc/sys/kernel/random/uuid").map(|s| s.trim().to_string()).unwrap_or_else(|_| {
        let h = random_hex(16).to_lowercase();
        format!("{}-{}-4{}-a{}-{}", &h[0..8], &h[8..12], &h[13..16], &h[17..20], &h[20..32])
    })
}

fn wait_for(path: &Path, secs: u64) -> Result<(), String> {
    for _ in 0..secs * 10 { if path.exists() { return Ok(()); } std::thread::sleep(std::time::Duration::from_millis(100)); }
    Err(format!("{} did not appear", path.display()))
}

fn zero_start(path: &Path, mib: usize) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().write(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(&vec![0u8; mib << 20]).map_err(|e| format!("{}: {e}", path.display()))
}

/// Run the whole installation. `report` is told about progress and log lines.
pub fn run_install(cfg: &Config, paths: &Paths, report: &mut dyn FnMut(usize, &str)) -> Result<(), String> {
    let step = |i: usize, report: &mut dyn FnMut(usize, &str)| report(i, STEPS[i]);

    // Safety checks before touching anything.
    if cfg.disk.is_empty() || !cfg.disk.chars().all(|c| c.is_ascii_alphanumeric()) { return Err(format!("Not a valid disk name: {:?}", cfg.disk)); }
    let disk_dev = paths.dev.join(&cfg.disk);
    if !disk_dev.exists() { return Err(format!("{} does not exist", disk_dev.display())); }
    if disks::is_install_media(&disk_dev) { return Err("Refusing to erase the installation medium.".into()); }
    if disks::is_mounted(&fs::read_to_string("/proc/mounts").unwrap_or_default(), &cfg.disk) {
        return Err(format!("{} is in use (mounted). Not erasing it.", cfg.disk));
    }
    let part = |n: u32| paths.dev.join(disks::part_name(&cfg.disk, n));
    let (esp, root_part) = (part(2), part(3));
    let (root_uuid, esp_serial) = (random_uuid(), random_hex(4));
    let esp_id = format!("{}-{}", &esp_serial[..4], &esp_serial[4..]);
    let target = &paths.target;

    step(0, report);
    run_with_input(Command::new("sfdisk").args(["--wipe", "always", "--wipe-partitions", "always"]).arg(&disk_dev), &sfdisk_script(), "sfdisk")?;
    for n in 1..=3 { wait_for(&part(n), 15)?; }

    step(1, report);
    zero_start(&esp, 1)?;
    zero_start(&root_part, 1)?;
    run(Command::new("mkfs.fat").args(["-F", "32", "-n", "EXEC85", "-i", &esp_serial]).arg(&esp), "mkfs.fat")?;
    run(Command::new("mkfs.ext4").args(["-F", "-q", "-L", "exec85", "-U", &root_uuid]).arg(&root_part), "mkfs.ext4")?;

    step(2, report);
    fs::create_dir_all(target).map_err(|e| format!("{}: {e}", target.display()))?;
    let result = (|| -> Result<(), String> {
        run(Command::new("mount").args(["-t", "ext4"]).arg(&root_part).arg(target), "mount root")?;
        fs::create_dir_all(target.join("boot")).map_err(|e| e.to_string())?;
        run(Command::new("mount").args(["-t", "vfat"]).arg(&esp).arg(target.join("boot")), "mount boot")?;

        step(3, report);
        for d in ["bin", "etc", "home", "lib", "media", "mnt", "opt", "root", "sbin", "srv", "usr", "var"] {
            if Path::new("/").join(d).exists() { run(Command::new("cp").arg("-a").arg(Path::new("/").join(d)).arg(format!("{}/", target.display())), &format!("copying /{d}"))?; }
        }
        for (d, mode) in [("proc", 0o755), ("sys", 0o755), ("dev", 0o755), ("run", 0o755), ("tmp", 0o1777)] {
            use std::os::unix::fs::PermissionsExt;
            let p = target.join(d);
            fs::create_dir_all(&p).map_err(|e| e.to_string())?;
            fs::set_permissions(&p, fs::Permissions::from_mode(mode)).map_err(|e| e.to_string())?;
        }
        let _ = fs::remove_dir_all(target.join("usr/share/exec85/install")); // the install files are not part of the installed system

        step(4, report);
        write_settings(cfg, target, &paths.zoneinfo, Some((&root_uuid, &esp_id)))?;
        step(5, report);
        create_accounts(cfg, target)?;

        step(6, report);
        let boot = target.join("boot");
        for (src, dst) in [("vmlinuz", "vmlinuz"), ("initramfs", "initramfs"), ("limine-bios.sys", "limine-bios.sys")] {
            fs::copy(paths.assets.join(src), boot.join(dst)).map_err(|e| format!("copying {src}: {e}"))?;
        }
        fs::create_dir_all(boot.join("EFI/BOOT")).map_err(|e| e.to_string())?;
        fs::copy(paths.assets.join("BOOTX64.EFI"), boot.join("EFI/BOOT/BOOTX64.EFI")).map_err(|e| format!("copying BOOTX64.EFI: {e}"))?;
        fs::write(boot.join("limine.conf"), limine_conf(&root_uuid)).map_err(|e| e.to_string())?;
        run(Command::new("limine").arg("bios-install").arg(&disk_dev), "limine bios-install")?;
        Ok(())
    })();

    step(7, report);
    run(&mut Command::new("sync"), "sync").ok();
    let _ = Command::new("umount").arg(target.join("boot")).status();
    let _ = Command::new("umount").arg(target).status();
    run(&mut Command::new("sync"), "sync").ok();
    result
}

// ---- TESTING-ONLY: "set up in memory" (no disk is touched) ------------------------------------------
// Used by the testing build's "Skip installation" choice. To remove the feature, delete everything marked
// TESTING-ONLY (this block, the pseudo-disk in installer.rs, the supervisor branch and the build flag).

pub const MEMORY_STEPS: [&str; 2] = ["Applying settings", "Creating your account"];

/// Apply the wizard's choices to the RUNNING (RAM-only) system: settings, user account, host name.
pub fn setup_in_memory(cfg: &Config, root: &Path, zoneinfo: &Path, report: &mut dyn FnMut(usize, &str)) -> Result<(), String> {
    report(0, MEMORY_STEPS[0]);
    write_settings(cfg, root, zoneinfo, None)?;
    report(1, MEMORY_STEPS[1]);
    create_accounts(cfg, root)?;
    if root == Path::new("/") { let _ = fs::write("/proc/sys/kernel/hostname", &cfg.hostname); }
    Ok(())
}

/// Drive `run_install`, publishing progress into `shared`.
pub fn run_to_shared(cfg: Config, paths: Paths, shared: Shared, wake: impl Fn()) {
    let set = |f: &mut dyn FnMut(&mut Progress)| { f(&mut shared.lock().unwrap()); wake(); };
    let memory_only = cfg.disk.is_empty(); // TESTING-ONLY: no disk chosen = set up in memory
    set(&mut |p| { p.total = if memory_only { MEMORY_STEPS.len() } else { STEPS.len() }; p.done = 0; });
    let mut report = |i: usize, label: &str| {
        set(&mut |p| { p.done = i; p.current = label.to_string(); p.log.push(format!("[{}/{}] {}", i + 1, if memory_only { MEMORY_STEPS.len() } else { STEPS.len() }, label)); });
        println!("EXEC85_INSTALL step={}/{} {}", i + 1, if memory_only { MEMORY_STEPS.len() } else { STEPS.len() }, label.replace(' ', "_"));
    };
    let r = if memory_only { setup_in_memory(&cfg, Path::new("/"), &paths.zoneinfo, &mut report) } else { run_install(&cfg, &paths, &mut report) };
    match &r {
        Ok(()) => println!("EXEC85_INSTALL result=ok"),
        Err(e) => println!("EXEC85_INSTALL result=failed {}", e.replace(' ', "_")),
    }
    set(&mut |p| { p.result = Some(r.clone()); if r.is_ok() { p.done = p.total; } });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config { disk: "sda".into(), hostname: "lab85".into(), username: "tester".into(), password: Some("pw-1".into()), layout: Layout::Hu, timezone: "Europe/Budapest".into(), dark: false }
    }

    #[test]
    fn partition_script_is_gpt_with_bios_boot_efi_and_root() {
        let s = sfdisk_script();
        assert!(s.starts_with("label: gpt\n"));
        let lines: Vec<&str> = s.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[1].contains("21686148-6449-6E6F-744E-656564454649"), "BIOS boot partition type");
        assert!(lines[2].contains("C12A7328-F81F-11D2-BA4B-00A0C93EC93B"), "EFI system partition type");
        assert!(lines[3].contains("0FC63DAF-8483-4772-8E79-3D69D8477DE4") && !lines[3].contains("size="), "root takes the rest");
    }

    #[test]
    fn boot_files_point_at_the_right_partition() {
        let c = limine_conf("1111-2222");
        assert!(c.contains("root=UUID=1111-2222"));
        assert!(c.contains("path: boot():/vmlinuz") && c.contains("module_path: boot():/initramfs"));
        assert!(!c.contains("exec85.live"), "an installed system must not start the installer");
        let f = fstab("1111-2222", "AB12-CD34");
        assert!(f.contains("UUID=1111-2222\t/\text4") && f.contains("UUID=AB12-CD34\t/boot\tvfat"));
    }

    #[test]
    fn settings_land_in_the_new_root() {
        let base = std::env::temp_dir().join(format!("exec85-inst-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (root, zi) = (base.join("root"), base.join("zoneinfo"));
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::create_dir_all(zi.join("Europe")).unwrap();
        fs::write(zi.join("Europe/Budapest"), b"TZif2-fake").unwrap();
        fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/ash\n").unwrap();
        fs::write(root.join("etc/shadow"), "root:*::0:::::\n").unwrap();
        fs::write(root.join("etc/group"), "root:x:0:root\nwheel:x:10:root\ninput:x:101:\n").unwrap();

        write_settings(&cfg(), &root, &zi, Some(("uuid-root", "AB12-CD34"))).unwrap();
        create_accounts(&cfg(), &root).unwrap();
        let rd = |p: &str| fs::read_to_string(root.join(p)).unwrap();
        assert_eq!(rd("etc/hostname"), "lab85\n");
        assert!(rd("etc/hosts").contains("127.0.1.1\tlab85"));
        assert_eq!(rd("etc/exec85/theme"), "light\n");
        assert_eq!(rd("etc/exec85/keyboard"), "hu\n");
        assert_eq!(rd("etc/timezone"), "Europe/Budapest\n");
        assert_eq!(fs::read(root.join("etc/localtime")).unwrap(), b"TZif2-fake");
        assert_eq!(rd("etc/doas.d/exec85.conf"), "permit persist :wheel\n");
        assert!(rd("etc/fstab").contains("uuid-root"));
        // the account works for login, root does not
        assert!(account::authenticate(&root, "tester", "pw-1").is_ok());
        assert_eq!(account::authenticate(&root, "root", "pw-1"), Err(account::AuthError::Locked));
        assert!(rd("etc/group").contains("wheel:x:10:root,tester"));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn setting_up_in_memory_writes_the_account_but_no_disk_files() {
        let base = std::env::temp_dir().join(format!("exec85-mem-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (root, zi) = (base.join("root"), base.join("zoneinfo"));
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::create_dir_all(&zi).unwrap();
        fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/ash\n").unwrap();
        fs::write(root.join("etc/shadow"), "root:*::0:::::\n").unwrap();
        fs::write(root.join("etc/group"), "root:x:0:root\nwheel:x:10:root\n").unwrap();
        let mut c = cfg();
        c.disk = String::new();
        let mut steps = Vec::new();
        setup_in_memory(&c, &root, &zi, &mut |i, l| steps.push((i, l.to_string()))).unwrap();
        assert_eq!(steps.len(), 2);
        assert!(account::authenticate(&root, "tester", "pw-1").is_ok());
        assert_eq!(fs::read_to_string(root.join("etc/exec85/theme")).unwrap(), "light\n");
        assert_eq!(fs::read_to_string(root.join("etc/exec85/keyboard")).unwrap(), "hu\n");
        assert!(!root.join("etc/fstab").exists(), "nothing that belongs to a disk install");
        assert!(!root.join("etc/exec85/installed").exists());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn refuses_dangerous_disk_names_before_doing_anything() {
        let mut c = cfg();
        for bad in ["", "../etc", "sda/../sdb", "sda; reboot", "/dev/sda"] {
            c.disk = bad.into();
            let mut reported = 0;
            let r = run_install(&c, &Paths { dev: "/nonexistent".into(), ..Paths::live() }, &mut |_, _| reported += 1);
            assert!(r.is_err(), "{bad:?}");
            assert_eq!(reported, 0, "nothing was started for {bad:?}");
        }
        c.disk = "sdz".into(); // valid name, but no such device
        assert!(run_install(&c, &Paths { dev: "/nonexistent".into(), ..Paths::live() }, &mut |_, _| panic!("must not start")).unwrap_err().contains("does not exist"));
    }
}
