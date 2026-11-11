//! File operations for the Explorer: create, rename, delete, copy and move, with name checks.
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// A single file or folder name (no slashes, not "." or "..", not empty, not absurdly long).
pub fn valid_name(name: &str) -> Result<(), &'static str> {
    if name.is_empty() { return Err("Enter a name."); }
    if name == "." || name == ".." { return Err("That name is not allowed."); }
    if name.contains('/') || name.contains('\0') { return Err("A name cannot contain '/'."); }
    if name.len() > 255 { return Err("That name is too long."); }
    Ok(())
}

pub fn create_file(dir: &Path, name: &str) -> Result<PathBuf, String> {
    valid_name(name).map_err(String::from)?;
    let p = dir.join(name);
    fs::OpenOptions::new().write(true).create_new(true).open(&p).map_err(|e| format!("Cannot create {name}: {e}"))?;
    Ok(p)
}

pub fn create_dir(dir: &Path, name: &str) -> Result<PathBuf, String> {
    valid_name(name).map_err(String::from)?;
    let p = dir.join(name);
    fs::create_dir(&p).map_err(|e| format!("Cannot create {name}: {e}"))?;
    Ok(p)
}

pub fn rename(dir: &Path, old: &str, new: &str) -> Result<PathBuf, String> {
    valid_name(new).map_err(String::from)?;
    let to = dir.join(new);
    if to.exists() { return Err(format!("{new} already exists.")); }
    fs::rename(dir.join(old), &to).map_err(|e| format!("Cannot rename: {e}"))?;
    Ok(to)
}

/// Delete a file, a link, or a folder with everything in it. Links are removed, never followed.
pub fn delete(path: &Path) -> Result<(), String> {
    let meta = fs::symlink_metadata(path).map_err(|e| format!("Cannot delete: {e}"))?;
    if meta.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) }.map_err(|e| format!("Cannot delete: {e}"))
}

/// A name that does not exist yet in `dir`: "a.txt" -> "a (copy).txt" -> "a (copy 2).txt" ...
pub fn unique_name(dir: &Path, name: &str) -> String {
    if !dir.join(name).exists() { return name.to_string(); }
    let (stem, ext) = match name.rfind('.') { Some(i) if i > 0 => (&name[..i], &name[i..]), _ => (name, "") };
    for n in 1.. {
        let cand = if n == 1 { format!("{stem} (copy){ext}") } else { format!("{stem} (copy {n}){ext}") };
        if !dir.join(&cand).exists() { return cand; }
    }
    unreachable!()
}

/// Copy a file or a whole folder. Symbolic links are copied as links.
pub fn copy_recursive(from: &Path, to: &Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(from)?;
    if meta.file_type().is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if meta.is_dir() {
        // Copying a folder into itself would never end.
        if to.starts_with(from) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "cannot copy a folder into itself")); }
        fs::create_dir(to)?;
        for e in fs::read_dir(from)? { let e = e?; copy_recursive(&e.path(), &to.join(e.file_name()))?; }
        fs::set_permissions(to, meta.permissions())
    } else {
        fs::copy(from, to).map(|_| ())
    }
}

/// Paste `src` into `dir` (copy, or move when `cut`). Returns the new path.
pub fn paste(src: &Path, dir: &Path, cut: bool) -> Result<PathBuf, String> {
    let name = src.file_name().map(|n| n.to_string_lossy().into_owned()).ok_or("Nothing to paste.")?;
    if !src.exists() && fs::symlink_metadata(src).is_err() { return Err("The copied item no longer exists.".into()); }
    let mut target = dir.join(unique_name(dir, &name));
    if cut {
        if src.parent() == Some(dir) { return Ok(src.to_path_buf()); } // moving to where it already is
        target = dir.join(unique_name(dir, &name));
        if target.starts_with(src) { return Err("Cannot move a folder into itself.".into()); }
        if fs::rename(src, &target).is_ok() { return Ok(target); }
        // different disk: copy, then remove the original
        copy_recursive(src, &target).map_err(|e| format!("Cannot move: {e}"))?;
        delete(src)?;
        Ok(target)
    } else {
        copy_recursive(src, &target).map_err(|e| format!("Cannot copy: {e}"))?;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-fileops-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names_are_checked() {
        for ok in ["a", "notes.txt", ".hidden", "with space", "ä"] { assert!(valid_name(ok).is_ok(), "{ok}"); }
        for bad in ["", ".", "..", "a/b", "/etc", "a\0b"] { assert!(valid_name(bad).is_err(), "{bad:?}"); }
        assert!(valid_name(&"x".repeat(256)).is_err());
    }

    #[test]
    fn create_rename_delete() {
        let d = tmp("crd");
        let f = create_file(&d, "a.txt").unwrap();
        assert!(f.is_file());
        assert!(create_file(&d, "a.txt").is_err(), "never overwrites an existing file");
        assert!(create_file(&d, "../evil").is_err());
        let sub = create_dir(&d, "sub").unwrap();
        fs::write(sub.join("inner.txt"), b"x").unwrap();
        let r = rename(&d, "a.txt", "b.txt").unwrap();
        assert!(r.is_file() && !f.exists());
        assert!(rename(&d, "b.txt", "sub").is_err(), "cannot rename onto an existing name");
        assert!(rename(&d, "b.txt", "x/y").is_err());
        delete(&sub).unwrap();
        assert!(!sub.exists());
        delete(&r).unwrap();
        assert!(delete(&r).is_err());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn deleting_a_link_leaves_its_target() {
        let d = tmp("link");
        fs::create_dir(d.join("real")).unwrap();
        fs::write(d.join("real/keep.txt"), b"keep").unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
        delete(&d.join("link")).unwrap();
        assert!(d.join("real/keep.txt").exists(), "the folder the link pointed at is untouched");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn unique_names() {
        let d = tmp("uniq");
        assert_eq!(unique_name(&d, "a.txt"), "a.txt");
        fs::write(d.join("a.txt"), b"").unwrap();
        assert_eq!(unique_name(&d, "a.txt"), "a (copy).txt");
        fs::write(d.join("a (copy).txt"), b"").unwrap();
        assert_eq!(unique_name(&d, "a.txt"), "a (copy 2).txt");
        fs::write(d.join("noext"), b"").unwrap();
        assert_eq!(unique_name(&d, "noext"), "noext (copy)");
        fs::write(d.join(".rc"), b"").unwrap();
        assert_eq!(unique_name(&d, ".rc"), ".rc (copy)");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_and_move_files_and_folders() {
        let d = tmp("paste");
        fs::create_dir_all(d.join("src/deep")).unwrap();
        fs::write(d.join("src/a.txt"), b"A").unwrap();
        fs::write(d.join("src/deep/b.txt"), b"B").unwrap();
        fs::create_dir(d.join("dst")).unwrap();
        // copy a folder
        let p = paste(&d.join("src"), &d.join("dst"), false).unwrap();
        assert_eq!(fs::read(p.join("deep/b.txt")).unwrap(), b"B");
        assert!(d.join("src/a.txt").exists(), "a copy leaves the original");
        // paste again: gets a new name
        let p2 = paste(&d.join("src"), &d.join("dst"), false).unwrap();
        assert_eq!(p2.file_name().unwrap(), "src (copy)");
        // move a file
        let m = paste(&d.join("src/a.txt"), &d.join("dst"), true).unwrap();
        assert_eq!(fs::read(&m).unwrap(), b"A");
        assert!(!d.join("src/a.txt").exists(), "a move removes the original");
        // moving something to where it already is does nothing
        assert_eq!(paste(&m, &d.join("dst"), true).unwrap(), m);
        // a folder cannot go into itself
        assert!(paste(&d.join("src"), &d.join("src/deep"), false).is_err());
        assert!(paste(&d.join("src"), &d.join("src/deep"), true).is_err());
        assert!(d.join("src/deep/b.txt").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
