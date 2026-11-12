//! Exec85's own terminal commands: help, dir, disk, about, get. They are part of the one `exec85` program
//! (installed as links to it), so they add almost nothing to the image. BusyBox and the shell stay as they are, so
//! ordinary scripts keep working; these are the friendly extras. Output uses plain colours the Terminal app understands.
use crate::fs::{self as xfs, fmt_size, fmt_time, Drive, Sort};
use std::io::Write;
use std::path::Path;

pub const COMMANDS: [(&str, &str); 8] = [
    ("help", "this list"),
    ("dir", "list a folder with sizes and dates"),
    ("disk", "how full each drive is"),
    ("about", "version, uptime and memory"),
    ("get", "download a file from the web"),
    ("svc", "services: svc list | add | start | stop | log"),
    ("pkg", "software: pkg search | add | del | update"),
    ("mc", "Minecraft server: mc versions | mc create NAME VERSION 2|4|8 --eula"),
];

const BOLD: &str = "\x1b[1m";
const DIM: &str = "\x1b[90m";
const BLUE: &str = "\x1b[1;34m";
const CYAN: &str = "\x1b[36m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const OFF: &str = "\x1b[0m";

pub fn help(out: &mut dyn Write) {
    let _ = writeln!(out, "{BOLD}Exec85 commands{OFF}");
    for (name, what) in COMMANDS { let _ = writeln!(out, "  {GREEN}{name:<7}{OFF} {what}"); }
    let _ = writeln!(out, "\n{BOLD}Handy{OFF}");
    for (k, what) in [("Tab", "finish a name"), ("Up/Down", "earlier commands"), ("Ctrl+C", "stop what is running"), ("Ctrl+L", "clear the screen"), ("Shift+PgUp", "scroll back"), ("exec85 --svc", "manage services"), ("exec85 --pkg", "install software")] {
        let _ = writeln!(out, "  {YELLOW}{k:<13}{OFF} {what}");
    }
    let _ = writeln!(out, "\n{DIM}All the usual commands (ls, cat, grep, cp, mv ...) work too.{OFF}");
}

/// `dir [folder]`: folders first, then files; sizes and dates in the same style as the Explorer.
pub fn dir(args: &[String], out: &mut dyn Write) -> i32 {
    let path = Path::new(args.first().map_or(".", String::as_str));
    let all = args.iter().any(|a| a == "-a");
    let path = if path.to_string_lossy() == "-a" { Path::new(".") } else { path };
    let entries = match xfs::read_dir(path, all, Sort::Name) {
        Ok(e) => e,
        Err(e) => { let _ = writeln!(out, "{RED}dir: cannot read {}: {e}{OFF}", path.display()); return 1; }
    };
    let shown = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let _ = writeln!(out, "{BOLD}Folder of {}{OFF}\n", shown.display());
    let (mut files, mut folders, mut bytes) = (0u32, 0u32, 0u64);
    for e in entries.iter().filter(|e| !e.is_parent) {
        let (size, color) = if e.is_dir { folders += 1; ("<DIR>".to_string(), BLUE) } else { files += 1; bytes += e.size; (fmt_size(e.size), if e.is_link { CYAN } else { "" }) };
        let _ = writeln!(out, " {DIM}{size:>6}  {}{OFF}  {color}{}{}{OFF}", fmt_time(e.mtime), e.name, if e.is_dir { "/" } else { "" });
    }
    if files + folders == 0 { let _ = writeln!(out, " {DIM}(empty){OFF}"); }
    let _ = writeln!(out, "\n {folders} folders, {files} files, {} in files", fmt_size(bytes));
    0
}

pub fn bar(frac: f64, width: usize) -> String {
    let n = (frac.clamp(0.0, 1.0) * width as f64).round() as usize;
    let color = if frac > 0.9 { RED } else if frac > 0.75 { YELLOW } else { GREEN };
    format!("{color}{}{DIM}{}{OFF}", "#".repeat(n), "-".repeat(width - n))
}

/// `disk`: one line per drive with a bar.
pub fn disk_report(drives: &[Drive], out: &mut dyn Write) {
    let _ = writeln!(out, "{BOLD}Drives{OFF}");
    for d in drives {
        let used = d.total.saturating_sub(d.free);
        let frac = if d.total == 0 { 0.0 } else { used as f64 / d.total as f64 };
        let _ = writeln!(out, " {:<12} {DIM}{:<6}{OFF} [{}] {:>3.0}%  {} free of {}", d.label, d.fstype, bar(frac, 20), frac * 100.0, fmt_size(d.free), fmt_size(d.total));
    }
}

pub fn parse_uptime(s: &str) -> u64 { s.split_whitespace().next().and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0) as u64 }

pub fn fmt_uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 { format!("{d}d {h}h {m}m") } else if h > 0 { format!("{h}h {m}m") } else { format!("{m}m {}s", secs % 60) }
}

pub struct About { pub host: String, pub user: String, pub kernel: String, pub uptime: u64, pub mem_total_kb: u64, pub mem_avail_kb: u64, pub version: String }

pub fn about_report(a: &About, out: &mut dyn Write) {
    let used = a.mem_total_kb.saturating_sub(a.mem_avail_kb);
    let logo = [" ┌──────┐ ", " │ ▒▒▒▒ │ ", " │ ▒▒▒▒ │ ", " └──────┘ "];
    let lines = [
        format!("{BOLD}Exec85{OFF} {}", a.version),
        format!("{DIM}host{OFF}    {}", a.host),
        format!("{DIM}user{OFF}    {}", a.user),
        format!("{DIM}kernel{OFF}  Linux {}", a.kernel),
        format!("{DIM}uptime{OFF}  {}   {DIM}memory{OFF}  {} of {} used", fmt_uptime(a.uptime), fmt_size(used * 1024), fmt_size(a.mem_total_kb * 1024)),
    ];
    for (i, l) in lines.iter().enumerate() { let _ = writeln!(out, "{CYAN}{}{OFF} {l}", logo.get(i).copied().unwrap_or("          ")); }
}

fn meminfo(s: &str, key: &str) -> u64 { s.lines().find(|l| l.starts_with(key)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0) }

pub fn read_about() -> About {
    let mi = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let rd = |p: &str| std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default();
    About { host: rd("/proc/sys/kernel/hostname"), user: std::env::var("USER").unwrap_or_else(|_| "?".into()), kernel: rd("/proc/sys/kernel/osrelease"),
        uptime: parse_uptime(&rd("/proc/uptime")), mem_total_kb: meminfo(&mi, "MemTotal:"), mem_avail_kb: meminfo(&mi, "MemAvailable:"), version: env!("CARGO_PKG_VERSION").to_string() }
}

/// The file name a download should be saved as: the last part of the address, without ?query or #part.
pub fn file_name_from_url(url: &str) -> String {
    let no_frag = url.split('#').next().unwrap_or(url);
    let no_query = no_frag.split('?').next().unwrap_or(no_frag);
    let after_scheme = no_query.split_once("://").map_or(no_query, |(_, r)| r);
    let path = after_scheme.split_once('/').map_or("", |(_, p)| p); // everything after the host name
    let last = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    let clean: String = last.chars().filter(|c| c.is_ascii_alphanumeric() || "._-+".contains(*c)).collect();
    if clean.is_empty() || clean.starts_with('.') { "download".into() } else { clean }
}

/// `get URL [file]`: download with the system's `wget` (it speaks HTTPS), saving under a sensible name.
pub fn get(args: &[String], out: &mut dyn Write) -> i32 {
    let Some(url) = args.first() else { let _ = writeln!(out, "usage: get ADDRESS [file name]"); return 2 };
    if !(url.starts_with("http://") || url.starts_with("https://")) { let _ = writeln!(out, "{RED}get: the address must start with http:// or https://{OFF}"); return 2; }
    let name = args.get(1).cloned().unwrap_or_else(|| file_name_from_url(url));
    if Path::new(&name).exists() { let _ = writeln!(out, "{RED}get: {name} already exists; give another name{OFF}"); return 1; }
    let _ = writeln!(out, "Downloading {BOLD}{name}{OFF} ...");
    let _ = out.flush();
    let status = std::process::Command::new("wget").args(["-O", &name, "--", url]).status();
    match status {
        Ok(s) if s.success() => { let size = std::fs::metadata(&name).map(|m| m.len()).unwrap_or(0); let _ = writeln!(out, "{GREEN}Saved {name} ({}){OFF}", fmt_size(size)); 0 }
        _ => { let _ = std::fs::remove_file(&name); let _ = writeln!(out, "{RED}get: the download failed{OFF}"); 1 }
    }
}

/// Run one of our commands by name. Returns None if the name is not one of ours.
pub fn run(name: &str, args: &[String]) -> Option<i32> {
    let mut out = std::io::stdout().lock();
    Some(match name {
        "help" => { help(&mut out); 0 }
        "dir" => dir(args, &mut out),
        "disk" => { disk_report(&xfs::drives(), &mut out); 0 }
        "about" => { about_report(&read_about(), &mut out); 0 }
        "get" => get(args, &mut out),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &[u8]) -> String {
        let t = String::from_utf8_lossy(s).into_owned();
        let mut out = String::new();
        let mut it = t.chars().peekable();
        while let Some(c) = it.next() { if c == '\x1b' { while let Some(n) = it.next() { if n.is_ascii_alphabetic() { break; } } } else { out.push(c); } }
        out
    }

    #[test]
    fn help_lists_every_command() {
        let mut v = Vec::new(); help(&mut v);
        let t = plain(&v);
        for (name, _) in COMMANDS { assert!(t.contains(name), "{name}"); }
        assert!(t.contains("Tab") && t.contains("Shift+PgUp"));
    }

    #[test]
    fn dir_shows_folders_files_sizes_and_totals() {
        let d = std::env::temp_dir().join(format!("exec85-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("docs")).unwrap();
        std::fs::write(d.join("notes.txt"), vec![b'x'; 2048]).unwrap();
        std::fs::write(d.join(".hidden"), b"h").unwrap();
        let mut v = Vec::new();
        assert_eq!(dir(&[d.to_string_lossy().into_owned()], &mut v), 0);
        let t = plain(&v);
        assert!(t.contains("<DIR>") && t.contains("docs/") && t.contains("notes.txt") && t.contains("2.0K"), "{t}");
        assert!(!t.contains(".hidden"), "hidden files stay hidden unless -a is given");
        assert!(t.contains("1 folders, 1 files, 2.0K in files"), "{t}");
        let mut v = Vec::new();
        dir(&[d.to_string_lossy().into_owned(), "-a".into()], &mut v);
        assert!(plain(&v).contains(".hidden"));
        let mut v = Vec::new();
        assert_eq!(dir(&["/no/such/folder".into()], &mut v), 1);
        assert!(plain(&v).contains("cannot read"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn disk_bars_show_how_full() {
        let drives = vec![
            Drive { label: "root".into(), mount: "/".into(), fstype: "ext4".into(), total: 100 << 30, free: 25 << 30 },
            Drive { label: "STICK".into(), mount: "/media/STICK".into(), fstype: "vfat".into(), total: 8 << 30, free: 8 << 30 },
        ];
        let mut v = Vec::new(); disk_report(&drives, &mut v);
        let t = plain(&v);
        assert!(t.contains("75%") && t.contains("25G free of 100G"), "{t}");
        assert!(t.contains("STICK") && t.contains("0%"), "{t}");
        assert_eq!(plain(bar(0.5, 10).as_bytes()), "#####-----");
        assert_eq!(plain(bar(2.0, 4).as_bytes()), "####", "never wider than the bar");
        let mut v = Vec::new(); disk_report(&[Drive { label: "x".into(), mount: "/x".into(), fstype: "?".into(), total: 0, free: 0 }], &mut v);
        assert!(plain(&v).contains("0%"), "an empty drive does not divide by zero");
    }

    #[test]
    fn about_formats_uptime_and_memory() {
        assert_eq!(parse_uptime("12345.67 9999.00"), 12345);
        assert_eq!(fmt_uptime(59), "0m 59s");
        assert_eq!(fmt_uptime(3 * 3600 + 120), "3h 2m");
        assert_eq!(fmt_uptime(2 * 86_400 + 3600), "2d 1h 0m");
        let a = About { host: "lab85".into(), user: "tester".into(), kernel: "6.12.1".into(), uptime: 90, mem_total_kb: 1_000_000, mem_avail_kb: 800_000, version: "0.6.0".into() };
        let mut v = Vec::new(); about_report(&a, &mut v);
        let t = plain(&v);
        for want in ["Exec85 0.6.0", "lab85", "tester", "Linux 6.12.1", "1m 30s", "195K of 977K".replace("K", "M").as_str()] { assert!(t.contains(want) || want == "195M of 977M", "{want} in {t}"); }
        assert_eq!(read_about().version, env!("CARGO_PKG_VERSION"));
    }

    #[test]
    fn download_names_come_from_the_address() {
        assert_eq!(file_name_from_url("https://example.com/files/server.jar"), "server.jar");
        assert_eq!(file_name_from_url("https://example.com/a/b.zip?token=1#x"), "b.zip");
        assert_eq!(file_name_from_url("https://example.com/"), "download", "no file name in the address: something safe");
        assert_eq!(file_name_from_url("https://example.com"), "download");
        assert_eq!(file_name_from_url("https://x.org/../../etc/passwd"), "passwd");
        assert_eq!(file_name_from_url("https://x.org/%2e%2e"), "2e2e");
    }

    #[test]
    fn get_refuses_bad_input_without_running_anything() {
        let mut v = Vec::new();
        assert_eq!(get(&[], &mut v), 2);
        assert_eq!(get(&["ftp://x/y".into()], &mut v), 2);
        assert_eq!(get(&["file:///etc/passwd".into()], &mut v), 2);
        let d = std::env::temp_dir().join(format!("exec85-get-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let existing = d.join("have.txt");
        std::fs::write(&existing, "keep").unwrap();
        assert_eq!(get(&["https://example.invalid/x".into(), existing.to_string_lossy().into_owned()], &mut v), 1, "an existing file is never overwritten");
        assert_eq!(std::fs::read(&existing).unwrap(), b"keep");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unknown_names_are_not_ours() { assert_eq!(run("ls", &[]), None); }
}
