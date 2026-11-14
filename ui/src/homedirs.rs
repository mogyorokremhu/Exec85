//! The folders every user gets in their home folder (Documents, Downloads, Music, Pictures, Videos, Desktop),
//! and the small "shortcut" files that live on the Desktop.
//!
//! There is no desktop with icons: `Desktop` is an ordinary folder. A shortcut is a text file ending in `.shortcut`:
//! ```text
//! name=Downloads
//! open=folder:/home/anna/Downloads
//! ```
//! `open=` is `folder:PATH`, `file:PATH` or `app:NAME`. Pressing Enter on one in the file manager does what it says.
use std::fs;
use std::io;
use std::os::unix::fs::{chown, PermissionsExt};
use std::path::{Path, PathBuf};

pub const FOLDERS: [&str; 6] = ["Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos"];
pub const SHORTCUT_EXT: &str = "shortcut";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target { Folder(PathBuf), File(PathBuf), App(String) }

pub fn parse_shortcut(text: &str) -> Option<(String, Target)> {
    let (mut name, mut open) = (None, None);
    for l in text.lines() {
        if let Some(v) = l.strip_prefix("name=") { name = Some(v.trim().to_string()); }
        else if let Some(v) = l.strip_prefix("open=") { open = Some(v.trim().to_string()); }
    }
    let open = open?;
    let target = if let Some(p) = open.strip_prefix("folder:") { Target::Folder(PathBuf::from(p)) }
        else if let Some(p) = open.strip_prefix("file:") { Target::File(PathBuf::from(p)) }
        else if let Some(a) = open.strip_prefix("app:") { Target::App(a.to_string()) }
        else { return None };
    if let Target::Folder(p) | Target::File(p) = &target { if !p.is_absolute() { return None; } }
    let name = name.filter(|n| !n.is_empty()).unwrap_or_else(|| open.clone());
    Some((name, target))
}

pub fn render_shortcut(name: &str, target: &Target) -> String {
    let open = match target { Target::Folder(p) => format!("folder:{}", p.display()), Target::File(p) => format!("file:{}", p.display()), Target::App(a) => format!("app:{a}") };
    format!("name={name}\nopen={open}\n")
}

/// The shortcuts a new Desktop starts with.
pub fn default_shortcuts(home: &Path) -> Vec<(String, Target)> {
    let mut v: Vec<(String, Target)> = ["Documents", "Downloads", "Music", "Pictures", "Videos"].iter().map(|f| (f.to_string(), Target::Folder(home.join(f)))).collect();
    v.push(("Terminal".into(), Target::App("terminal".into())));
    v.push(("Browser".into(), Target::App("browser".into())));
    v.push(("App catalog".into(), Target::App("catalog".into())));
    v
}

fn make_dir(p: &Path, uid: u32, gid: u32) -> io::Result<()> {
    if !p.is_dir() { fs::create_dir_all(p)?; }
    fs::set_permissions(p, fs::Permissions::from_mode(0o755))?;
    let _ = chown(p, Some(uid), Some(gid)); // fails harmlessly when not root (tests)
    Ok(())
}

/// Make the standard folders that are missing (never touching what is there) and, once, the first shortcuts on the Desktop.
/// A person who deletes a shortcut does not get it back: a marker remembers that the Desktop was already filled.
pub fn ensure(home: &Path, uid: u32, gid: u32) -> io::Result<()> {
    if !home.is_dir() { return Ok(()); }
    for f in FOLDERS { make_dir(&home.join(f), uid, gid)?; }
    let marker = home.join(".config/exec85/desktop-seeded");
    if !marker.exists() {
        for (name, target) in default_shortcuts(home) {
            let file = home.join("Desktop").join(format!("{name}.{SHORTCUT_EXT}"));
            if !file.exists() { fs::write(&file, render_shortcut(&name, &target))?; let _ = chown(&file, Some(uid), Some(gid)); }
        }
        if let Some(dir) = marker.parent() { make_dir(dir, uid, gid)?; if let Some(cfg) = dir.parent() { let _ = chown(cfg, Some(uid), Some(gid)); } }
        fs::write(&marker, "1\n")?;
        let _ = chown(&marker, Some(uid), Some(gid));
    }
    Ok(())
}

pub fn downloads_dir(home: &Path) -> PathBuf { home.join("Downloads") }

#[cfg(test)]
mod tests {
    use super::*;
    fn tmp(n: &str) -> PathBuf { let p = std::env::temp_dir().join(format!("exec85-home-{n}-{}", std::process::id())); let _ = fs::remove_dir_all(&p); fs::create_dir_all(&p).unwrap(); p }

    #[test]
    fn shortcuts_round_trip_and_bad_ones_are_refused() {
        for t in [Target::Folder("/home/a/Documents".into()), Target::File("/etc/hostname".into()), Target::App("terminal".into())] {
            let text = render_shortcut("Thing", &t);
            assert_eq!(parse_shortcut(&text), Some(("Thing".to_string(), t)));
        }
        assert_eq!(parse_shortcut("name=x\nopen=run:rm -rf /\n"), None);
        assert_eq!(parse_shortcut("name=x\nopen=folder:relative/dir\n"), None, "only absolute paths");
        assert_eq!(parse_shortcut("name=x\n"), None);
        assert_eq!(parse_shortcut("open=app:clock\n").unwrap().0, "app:clock", "no name: the target is shown");
    }

    #[test]
    fn a_home_gets_every_folder_and_the_first_shortcuts_once() {
        let h = tmp("ensure");
        ensure(&h, 1000, 1000).unwrap();
        for f in FOLDERS { assert!(h.join(f).is_dir(), "{f}"); }
        assert!(h.join("Desktop/Downloads.shortcut").is_file() && h.join("Desktop/Terminal.shortcut").is_file());
        let (n, t) = parse_shortcut(&fs::read_to_string(h.join("Desktop/Downloads.shortcut")).unwrap()).unwrap();
        assert_eq!((n.as_str(), t), ("Downloads", Target::Folder(h.join("Downloads"))));
        // the person deletes a shortcut and adds a file: both stay as they are
        fs::remove_file(h.join("Desktop/Music.shortcut")).unwrap();
        fs::write(h.join("Documents/essay.txt"), "hi").unwrap();
        ensure(&h, 1000, 1000).unwrap();
        assert!(!h.join("Desktop/Music.shortcut").exists(), "a deleted shortcut does not come back");
        assert_eq!(fs::read_to_string(h.join("Documents/essay.txt")).unwrap(), "hi");
        // a folder that was deleted is made again
        fs::remove_dir(h.join("Videos")).unwrap();
        ensure(&h, 1000, 1000).unwrap();
        assert!(h.join("Videos").is_dir());
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn a_missing_home_is_left_alone() {
        let p = std::env::temp_dir().join(format!("exec85-nohome-{}", std::process::id()));
        assert!(ensure(&p, 1, 1).is_ok() && !p.exists());
    }
}
