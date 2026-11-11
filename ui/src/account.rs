//! User accounts: reading /etc/passwd, /etc/shadow and /etc/group, checking logins,
//! and creating the first user during installation. All functions take the root of the
//! filesystem as a parameter, so they work on a freshly installed disk (mounted somewhere)
//! as well as on the running system.
use crate::crypt;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User { pub name: String, pub uid: u32, pub gid: u32, pub home: String, pub shell: String }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group { pub name: String, pub gid: u32, pub members: Vec<String> }

pub fn parse_passwd(s: &str) -> Vec<User> {
    s.lines().filter_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 7 { return None; }
        Some(User { name: f[0].into(), uid: f[2].parse().ok()?, gid: f[3].parse().ok()?, home: f[5].into(), shell: f[6].into() })
    }).collect()
}

/// name -> stored hash field
pub fn parse_shadow(s: &str) -> HashMap<String, String> {
    s.lines().filter_map(|l| { let mut f = l.split(':'); Some((f.next()?.to_string(), f.next()?.to_string())) }).collect()
}

pub fn parse_group(s: &str) -> Vec<Group> {
    s.lines().filter_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.len() < 4 { return None; }
        Some(Group { name: f[0].into(), gid: f[2].parse().ok()?, members: f[3].split(',').filter(|m| !m.is_empty()).map(String::from).collect() })
    }).collect()
}

fn read(root: &Path, rel: &str) -> String { fs::read_to_string(root.join(rel)).unwrap_or_default() }

pub fn users(root: &Path) -> Vec<User> { parse_passwd(&read(root, "etc/passwd")) }

/// People who can log in normally: uid 1000 and up, with a real shell.
pub fn regular_users(root: &Path) -> Vec<User> {
    users(root).into_iter().filter(|u| u.uid >= 1000 && u.uid < 60000 && !u.shell.ends_with("nologin") && !u.shell.ends_with("false")).collect()
}

/// Group ids a user belongs to: the primary group plus every group listing them as a member.
pub fn group_ids(root: &Path, user: &User) -> Vec<u32> {
    let mut v = vec![user.gid];
    for g in parse_group(&read(root, "etc/group")) {
        if g.members.iter().any(|m| m == &user.name) && !v.contains(&g.gid) { v.push(g.gid); }
    }
    v
}

#[derive(Debug, PartialEq, Eq)]
pub enum AuthError { NoSuchUser, Locked, BadPassword }

/// Check a name and password. An account with an empty password field accepts an empty password.
pub fn authenticate(root: &Path, name: &str, password: &str) -> Result<User, AuthError> {
    let user = users(root).into_iter().find(|u| u.name == name).ok_or(AuthError::NoSuchUser)?;
    let hash = parse_shadow(&read(root, "etc/shadow")).get(name).cloned().unwrap_or_else(|| "!".into());
    if hash.is_empty() {
        return if password.is_empty() { Ok(user) } else { Err(AuthError::BadPassword) };
    }
    if hash.starts_with('!') || hash.starts_with('*') { return Err(AuthError::Locked); }
    if crypt::verify(password, &hash) { Ok(user) } else { Err(AuthError::BadPassword) }
}

// ---- validation -------------------------------------------------------------

const RESERVED: [&str; 19] = [
    "root", "daemon", "bin", "sys", "sync", "shutdown", "halt", "mail", "news", "uucp", "nobody", "ftp", "sshd", "ntp",
    "games", "man", "postmaster", "cron", "operator",
];

pub fn valid_username(s: &str) -> Result<(), &'static str> {
    let mut chars = s.chars();
    let Some(first) = chars.next() else { return Err("Enter a user name.") };
    if s.len() > 32 { return Err("User names can be at most 32 characters."); }
    if !(first.is_ascii_lowercase() || first == '_') { return Err("A user name must start with a lowercase letter."); }
    if !chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-') {
        return Err("Use only lowercase letters, digits, - and _.");
    }
    if RESERVED.contains(&s) { return Err("That name is reserved for the system."); }
    Ok(())
}

pub fn valid_hostname(s: &str) -> Result<(), &'static str> {
    if s.is_empty() { return Err("Enter a device name."); }
    if s.len() > 63 { return Err("A device name can be at most 63 characters."); }
    if s.starts_with('-') || s.ends_with('-') { return Err("A device name cannot start or end with '-'."); }
    if !s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') { return Err("Use only lowercase letters, digits and '-'."); }
    Ok(())
}

// ---- creating accounts ------------------------------------------------------

fn write_atomic(path: &Path, text: &str, mode: u32) -> io::Result<()> {
    let tmp: PathBuf = path.with_extension("exec85-tmp");
    fs::write(&tmp, text)?;
    fs::set_permissions(&tmp, fs::Permissions::from_mode(mode))?;
    fs::rename(&tmp, path)
}

fn append_line(text: &str, line: &str) -> String {
    let mut t = text.to_string();
    if !t.is_empty() && !t.ends_with('\n') { t.push('\n'); }
    t.push_str(line);
    t.push('\n');
    t
}

/// Lock the root account (no password login); administrators use `doas` instead.
pub fn lock_root(root: &Path) -> io::Result<()> {
    let path = root.join("etc/shadow");
    let text = fs::read_to_string(&path).unwrap_or_default();
    let mut found = false;
    let mut lines: Vec<String> = text.lines().map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        if f.first() == Some(&"root") && f.len() >= 2 { found = true; let mut g: Vec<String> = f.iter().map(|s| s.to_string()).collect(); g[1] = "!".into(); g.join(":") } else { l.to_string() }
    }).collect();
    if !found { lines.insert(0, "root:!::0:::::".into()); }
    write_atomic(&path, &(lines.join("\n") + "\n"), 0o600)
}

/// Create a user with a home folder. `password: None` or `Some("")` means no password.
/// The user is added to each group in `extra_groups` that exists.
pub fn add_user(root: &Path, name: &str, password: Option<&str>, extra_groups: &[&str]) -> io::Result<User> {
    valid_username(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    let existing = users(root);
    if existing.iter().any(|u| u.name == name) { return Err(io::Error::new(io::ErrorKind::AlreadyExists, "user exists")); }
    let mut groups = parse_group(&read(root, "etc/group"));
    let uid = existing.iter().map(|u| u.uid).filter(|u| *u < 60000).max().map_or(1000, |m| (m + 1).max(1000));
    let gid = (uid..).find(|g| !groups.iter().any(|x| x.gid == *g)).unwrap_or(uid);
    let home = format!("/home/{name}");

    fs::create_dir_all(root.join("etc"))?;
    let passwd = append_line(&read(root, "etc/passwd"), &format!("{name}:x:{uid}:{gid}:{name}:{home}:/bin/ash"));
    write_atomic(&root.join("etc/passwd"), &passwd, 0o644)?;

    let hash = match password { Some(p) if !p.is_empty() => crypt::hash_password(p), _ => String::new() };
    let days = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() / 86400);
    let shadow = append_line(&read(root, "etc/shadow"), &format!("{name}:{hash}:{days}:0:99999:7:::"));
    write_atomic(&root.join("etc/shadow"), &shadow, 0o600)?;

    groups.push(Group { name: name.into(), gid, members: vec![] });
    for g in groups.iter_mut() {
        if extra_groups.contains(&g.name.as_str()) && !g.members.iter().any(|m| m == name) { g.members.push(name.into()); }
    }
    let group_text: String = groups.iter().map(|g| format!("{}:x:{}:{}\n", g.name, g.gid, g.members.join(","))).collect();
    write_atomic(&root.join("etc/group"), &group_text, 0o644)?;

    let home_dir = root.join(home.trim_start_matches('/'));
    fs::create_dir_all(&home_dir)?;
    fs::set_permissions(&home_dir, fs::Permissions::from_mode(0o700))?;
    let _ = std::os::unix::fs::chown(&home_dir, Some(uid), Some(gid)); // fails harmlessly when not root (tests)
    Ok(User { name: name.into(), uid, gid, home, shell: "/bin/ash".into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_root(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("exec85-acct-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("etc")).unwrap();
        fs::write(root.join("etc/passwd"), "root:x:0:0:root:/root:/bin/ash\ndaemon:x:2:2:daemon:/sbin:/sbin/nologin\nnobody:x:65534:65534:nobody:/:/sbin/nologin\n").unwrap();
        fs::write(root.join("etc/shadow"), "root:*::0:::::\ndaemon:!::0:::::\n").unwrap();
        fs::write(root.join("etc/group"), "root:x:0:root\nwheel:x:10:root\ninput:x:101:\nvideo:x:27:\n").unwrap();
        root
    }

    #[test]
    fn username_and_hostname_rules() {
        for ok in ["tester", "a", "_svc", "user-1", "x_y"] { assert!(valid_username(ok).is_ok(), "{ok}"); }
        for bad in ["", "Root", "1abc", "has space", "ä", "root", "nobody", "a".repeat(33).as_str(), "-x"] { assert!(valid_username(bad).is_err(), "{bad:?}"); }
        for ok in ["exec85", "lab-1", "a", "x".repeat(63).as_str()] { assert!(valid_hostname(ok).is_ok(), "{ok}"); }
        for bad in ["", "-a", "a-", "A", "a_b", "a.b", "x".repeat(64).as_str()] { assert!(valid_hostname(bad).is_err(), "{bad:?}"); }
    }

    #[test]
    fn creating_a_user_then_logging_in() {
        let root = fake_root("create");
        let u = add_user(&root, "tester", Some("secret7-"), &["wheel", "input", "video", "nonexistent"]).unwrap();
        assert_eq!((u.uid, u.gid, u.home.as_str()), (1000, 1000, "/home/tester"));
        assert!(root.join("home/tester").is_dir());
        assert_eq!(fs::metadata(root.join("home/tester")).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(root.join("etc/shadow")).unwrap().permissions().mode() & 0o777, 0o600, "shadow is private");

        assert_eq!(authenticate(&root, "tester", "secret7-").map(|u| u.name), Ok("tester".to_string()));
        assert_eq!(authenticate(&root, "tester", "secret7"), Err(AuthError::BadPassword));
        assert_eq!(authenticate(&root, "tester", ""), Err(AuthError::BadPassword));
        assert_eq!(authenticate(&root, "nobody-here", "x"), Err(AuthError::NoSuchUser));
        assert_eq!(authenticate(&root, "daemon", "x"), Err(AuthError::Locked));

        let shadow = fs::read_to_string(root.join("etc/shadow")).unwrap();
        assert!(!shadow.contains("secret7"), "the password itself is never stored");
        assert!(shadow.contains("tester:$6$"));

        let groups = parse_group(&fs::read_to_string(root.join("etc/group")).unwrap());
        let members = |g: &str| groups.iter().find(|x| x.name == g).unwrap().members.clone();
        assert_eq!(members("wheel"), vec!["root", "tester"]);
        assert_eq!(members("input"), vec!["tester"]);
        assert_eq!(members("tester"), Vec::<String>::new());
        assert!(!groups.iter().any(|g| g.name == "nonexistent"));
        let mut ids = group_ids(&root, &u);
        ids.sort();
        assert_eq!(ids, vec![10, 27, 101, 1000]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn second_user_gets_the_next_id_and_duplicates_are_refused() {
        let root = fake_root("second");
        add_user(&root, "first", Some("a"), &[]).unwrap();
        let second = add_user(&root, "second", None, &[]).unwrap();
        assert_eq!((second.uid, second.gid), (1001, 1001));
        assert_eq!(add_user(&root, "first", Some("b"), &[]).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(regular_users(&root).iter().map(|u| u.name.as_str()).collect::<Vec<_>>(), ["first", "second"]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_empty_password_means_no_password() {
        let root = fake_root("nopw");
        add_user(&root, "guest1", None, &[]).unwrap();
        assert!(authenticate(&root, "guest1", "").is_ok());
        assert_eq!(authenticate(&root, "guest1", "anything"), Err(AuthError::BadPassword));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn root_can_be_locked() {
        let root = fake_root("lock");
        fs::write(root.join("etc/shadow"), "root:$6$abc$def:19000:0:99999:7:::\n").unwrap();
        lock_root(&root).unwrap();
        assert_eq!(authenticate(&root, "root", "x"), Err(AuthError::Locked));
        let _ = fs::remove_dir_all(&root);
    }
}
