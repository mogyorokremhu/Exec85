//! Updates in place: new Exec85 files are put over the old ones on the installed system, without installing again.
//!
//! An update is one package (`exec85-update-<version>.tar.gz`) that holds `manifest` and a `files/` tree. A package may only replace
//! files on a fixed list (the Exec85 program, its start-up scripts, its own boot files and kernel). It can never touch accounts,
//! settings, services, the firewall, Minecraft servers or anything in /home, /srv or /var: those are the owner's, not the system's.
//!
//! Before anything is replaced the old files are saved in `/var/lib/exec85/update/prev`. For a trial period after the update the boot
//! menu shows for 3 seconds with an extra entry "Exec85 (previous version)"; choosing it (or `update rollback`) puts the old files back.
//! After the new version has run well for a while (2 minutes), the menu is hidden again so boot stays fast.
use crate::helper::Ctx;
use std::path::{Path, PathBuf};

pub const STATE_DIR: &str = "var/lib/exec85/update";
pub const CONF: &str = "etc/exec85/update.conf";
pub const VERSION_FILE: &str = "etc/exec85/package-version";
pub const ROLLBACK_SCRIPT: &str = "/sbin/exec85-rollback";
pub const MAX_PACKAGE_BYTES: u64 = 400 << 20;
pub const GOOD_AFTER_DEFAULT: u64 = 120;

// ---- the package -----------------------------------------------------------------------------

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Manifest { pub version: String, pub kernel: Option<String> }

pub fn valid_version(v: &str) -> bool { !v.is_empty() && v.len() <= 20 && v.split('.').all(|p| !p.is_empty() && p.len() <= 6 && p.chars().all(|c| c.is_ascii_digit())) }
fn valid_kernel_name(k: &str) -> bool { !k.is_empty() && k.len() <= 60 && !k.starts_with('.') && k.chars().all(|c| c.is_ascii_alphanumeric() || "._-+".contains(c)) }

pub fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("exec85-update 1") { return Err("This is not an Exec85 update package.".into()); }
    let (mut version, mut kernel) = (None, None);
    for l in lines {
        match l.split_once('=') {
            Some(("version", v)) if valid_version(v.trim()) => version = Some(v.trim().to_string()),
            Some(("version", _)) => return Err("The package has a version number I cannot read.".into()),
            Some(("kernel", k)) if valid_kernel_name(k.trim()) => kernel = Some(k.trim().to_string()),
            Some(("kernel", _)) => return Err("The package names a kernel in a form I do not accept.".into()),
            _ => {}
        }
    }
    Ok(Manifest { version: version.ok_or("The package does not say which version it is.")?, kernel })
}

/// Paths (relative to the root folder) an update may put files at.
pub fn allowed_path(rel: &str) -> bool {
    if rel.is_empty() || rel.starts_with('/') || rel.contains("//") || rel.split('/').any(|p| p == ".." || p == "." || p.is_empty()) || rel.len() > 200 { return false; }
    if !rel.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c)) { return false; }
    const EXACT: [&str; 12] = ["usr/bin/exec85", "sbin/exec85-rollback", "etc/inittab", "etc/mdev.conf", "etc/exec85/early-boot.sh", "etc/exec85/start-splash.sh", "etc/exec85/mc-run", "etc/exec85/shrc", "etc/exec85/live-init", "etc/exec85/package-version", "boot/vmlinuz", "boot/initramfs"];
    EXACT.contains(&rel) || rel.starts_with("usr/share/exec85/") || rel.starts_with("etc/init.d/exec85-") || rel.starts_with("etc/runlevels/default/exec85-") || rel.starts_with("lib/modules/") || (rel.starts_with("usr/bin/") && rel.matches('/').count() == 2)
}

/// A symbolic link may only be one of ours: a command name pointing at `exec85`, or an OpenRC link to one of our init scripts.
pub fn allowed_link(rel: &str, target: &str) -> bool {
    allowed_path(rel) && ((rel.starts_with("usr/bin/") && target == "exec85")
        || (rel.starts_with("etc/runlevels/default/exec85-") && target.starts_with("../../init.d/exec85-") && target.matches("..").count() == 2 && !target.contains(' ')))
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Kind { Dir, File, Link(String) }
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Member { pub rel: String, pub kind: Kind }

/// Read the output of `tar -tzvf` (busybox or GNU) and refuse anything that is not a plain file, folder or one of our links.
pub fn parse_listing(listing: &str) -> Result<Vec<Member>, String> {
    let mut out = Vec::new();
    for l in listing.lines().filter(|l| !l.trim().is_empty()) {
        let t = l.chars().next().unwrap_or(' ');
        let (name, link) = match l.split_once(" -> ") { Some((a, b)) => (a, Some(b.trim())), None => (l, None) };
        let name = name.split_whitespace().last().unwrap_or("");
        let name = name.trim_end_matches('/');
        if name == "manifest" && t == '-' { continue; }
        let Some(rel) = name.strip_prefix("files/") else { if name == "files" && t == 'd' { continue; } return Err(format!("The package contains something outside its files folder: {name}")); };
        let kind = match t {
            'd' => { out.push(Member { rel: rel.to_string(), kind: Kind::Dir }); continue; }
            '-' => Kind::File,
            'l' => Kind::Link(link.ok_or("A link in the package has no target.")?.to_string()),
            _ => return Err(format!("The package contains a kind of entry I do not accept ({name}).")),
        };
        match &kind {
            Kind::File => if !allowed_path(rel) { return Err(format!("The package wants to replace {rel}, which an update may not touch.")); },
            Kind::Link(tg) => if !allowed_link(rel, tg) { return Err(format!("The package contains a link I do not accept: {rel}")); },
            Kind::Dir => {}
        }
        out.push(Member { rel: rel.to_string(), kind });
    }
    if out.is_empty() { return Err("The package is empty.".into()); }
    Ok(out)
}

/// Walk an extracted package and check it again (never trust the first look alone). Returns the files and links to install, folders skipped.
pub fn validate_tree(files_dir: &Path) -> Result<Vec<Member>, String> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<Member>) -> Result<(), String> {
        for e in std::fs::read_dir(dir).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
            let md = std::fs::symlink_metadata(&p).map_err(|e| e.to_string())?;
            if md.file_type().is_dir() { walk(base, &p, out)?; }
            else if md.file_type().is_symlink() {
                let tg = std::fs::read_link(&p).map_err(|e| e.to_string())?.to_string_lossy().into_owned();
                if !allowed_link(&rel, &tg) { return Err(format!("The package contains a link I do not accept: {rel}")); }
                out.push(Member { rel, kind: Kind::Link(tg) });
            } else if md.file_type().is_file() {
                if !allowed_path(&rel) { return Err(format!("The package wants to replace {rel}, which an update may not touch.")); }
                out.push(Member { rel, kind: Kind::File });
            } else { return Err(format!("The package contains an entry I do not accept: {rel}")); }
        }
        Ok(())
    }
    let mut v = Vec::new();
    walk(files_dir, files_dir, &mut v)?;
    v.sort_by(|a, b| a.rel.cmp(&b.rel));
    if v.is_empty() { return Err("The package is empty.".into()); }
    Ok(v)
}

// ---- the boot menu -----------------------------------------------------------------------------

pub struct Boot<'a> { pub root_uuid: &'a str, pub timeout: u32, /// the previous version's kernel and boot image, when the menu offers it
    pub previous: Option<(&'a str, &'a str)>, pub default_previous: bool }

pub fn limine_conf(b: &Boot) -> String {
    let cmd = crate::install::boot_cmdline(b.root_uuid);
    let mut s = format!("timeout: {}\nquiet: yes\n", b.timeout);
    if b.previous.is_some() && b.default_previous { s.push_str("default_entry: 2\n"); }
    s.push_str(&format!("\n/Exec85\n    protocol: linux\n    path: boot():/vmlinuz\n    module_path: boot():/initramfs\n    cmdline: {cmd}\n"));
    if let Some((k, i)) = b.previous {
        s.push_str(&format!("\n/Exec85 (previous version)\n    protocol: linux\n    path: boot():/{k}\n    module_path: boot():/{i}\n    cmdline: {cmd} init={ROLLBACK_SCRIPT}\n"));
    }
    s
}

/// The root file system's UUID from an existing limine.conf (its `cmdline:` line holds `root=UUID=...`).
pub fn root_uuid_from(conf: &str) -> Option<String> {
    let l = conf.lines().find(|l| l.trim_start().starts_with("cmdline:") && l.contains("root=UUID="))?;
    let v = l.split("root=UUID=").nth(1)?.split_whitespace().next()?;
    if v.is_empty() || !v.chars().all(|c| c.is_ascii_hexdigit() || c == '-') { return None; }
    Some(v.to_string())
}

// ---- state on disk ---------------------------------------------------------------------------------

fn state(ctx: &Ctx) -> PathBuf { ctx.root.join(STATE_DIR) }
fn boot_dir(ctx: &Ctx) -> PathBuf { ctx.root.join("boot") }

pub fn current_version(ctx: &Ctx) -> String {
    std::fs::read_to_string(ctx.root.join(VERSION_FILE)).ok().map(|s| s.trim().to_string()).filter(|s| valid_version(s)).unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string())
}

pub fn read_conf(ctx: &Ctx) -> Vec<(String, String)> {
    std::fs::read_to_string(ctx.root.join(CONF)).unwrap_or_default().lines().filter_map(|l| l.split_once('=').map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))).collect()
}
pub fn conf_value(ctx: &Ctx, key: &str) -> Option<String> { read_conf(ctx).into_iter().find(|(k, _)| k == key).map(|(_, v)| v) }

/// Settings are one line each (`repo=owner/name`, `token=...`, `good_after=120`); the file is private because of the token.
pub fn set_conf(ctx: &Ctx, key: &str, value: &str) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let ok_key = matches!(key, "repo" | "token" | "good_after");
    if !ok_key { return Err("Unknown setting. Use repo, token or good_after.".into()); }
    if value.len() > 200 || value.chars().any(|c| c.is_control() || c.is_whitespace()) { return Err("A setting is one word without spaces.".into()); }
    if key == "repo" && !(value.split('/').count() == 2 && value.chars().all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c))) { return Err("The repository is written owner/name.".into()); }
    if key == "good_after" && value.parse::<u64>().map_or(true, |n| !(5..=3600).contains(&n)) { return Err("good_after is a number of seconds from 5 to 3600.".into()); }
    let mut all: Vec<(String, String)> = read_conf(ctx).into_iter().filter(|(k, _)| k != key).collect();
    if !value.is_empty() { all.push((key.to_string(), value.to_string())); }
    let path = ctx.root.join(CONF);
    if let Some(d) = path.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(&path, all.iter().map(|(k, v)| format!("{k}={v}\n")).collect::<String>()).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())
}

pub fn good_after(ctx: &Ctx) -> u64 { conf_value(ctx, "good_after").and_then(|v| v.parse().ok()).unwrap_or(GOOD_AFTER_DEFAULT) }

pub fn pending(ctx: &Ctx) -> bool { state(ctx).join("pending").exists() }
pub fn previous_version(ctx: &Ctx) -> Option<String> { std::fs::read_to_string(state(ctx).join("prev/version")).ok().map(|s| s.trim().to_string()).filter(|s| valid_version(s)) }
pub fn available_version(ctx: &Ctx) -> Option<String> { std::fs::read_to_string(state(ctx).join("available")).ok().and_then(|s| s.lines().next().map(|l| l.trim().to_string())).filter(|s| valid_version(s)) }

/// `version=… pending=yes|no previous=… available=… token=yes|no repo=… good_after=…`, one per line.
pub fn status(ctx: &Ctx) -> String {
    let live = crate::supervisor::cmdline_has("exec85.live");
    format!("version={}\nlive={}\npending={}\nprevious={}\navailable={}\ntoken={}\nrepo={}\ngood_after={}\n", current_version(ctx), if live { "yes" } else { "no" }, if pending(ctx) { "yes" } else { "no" },
        previous_version(ctx).unwrap_or_else(|| "-".into()), available_version(ctx).unwrap_or_else(|| "-".into()), if conf_value(ctx, "token").is_some() { "yes" } else { "no" }, conf_value(ctx, "repo").unwrap_or_else(|| "-".into()), good_after(ctx))
}

// ---- applying -----------------------------------------------------------------------------------------

fn copy_keep(src: &Path, dst: &Path) -> Result<(), String> {
    if let Some(d) = dst.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let _ = std::fs::remove_file(dst);
    let md = std::fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if md.file_type().is_symlink() { std::os::unix::fs::symlink(std::fs::read_link(src).map_err(|e| e.to_string())?, dst).map_err(|e| e.to_string()) }
    else { std::fs::copy(src, dst).map(|_| ()).map_err(|e| format!("{}: {e}", dst.display())) }
}

fn install_one(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(d) = to.parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    let md = std::fs::symlink_metadata(from).map_err(|e| e.to_string())?;
    if md.file_type().is_symlink() {
        let _ = std::fs::remove_file(to);
        return std::os::unix::fs::symlink(std::fs::read_link(from).map_err(|e| e.to_string())?, to).map_err(|e| e.to_string());
    }
    // write beside it, then rename over: a power cut never leaves a half-written file in place
    let tmp = to.with_file_name(format!(".{}.exec85-new", to.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    std::fs::copy(from, &tmp).map_err(|e| format!("{}: {e}", to.display()))?;
    std::fs::rename(&tmp, to).map_err(|e| format!("{}: {e}", to.display()))
}

fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for e in std::fs::read_dir(src).map_err(|e| e.to_string())?.flatten() {
        let (s, d) = (e.path(), dst.join(e.file_name()));
        if std::fs::symlink_metadata(&s).map_err(|e| e.to_string())?.file_type().is_dir() { copy_tree(&s, &d)?; } else { install_one(&s, &d)?; }
    }
    Ok(())
}

/// Install a package that has been extracted to `staging` (`staging/manifest` and `staging/files/...`). `force` allows the same or an older version.
pub fn apply_staged(ctx: &Ctx, staging: &Path, force: bool) -> Result<String, String> {
    let manifest = parse_manifest(&std::fs::read_to_string(staging.join("manifest")).map_err(|_| "The package has no manifest.".to_string())?)?;
    let cur = current_version(ctx);
    if !force && crate::mc::cmp_versions(&manifest.version, &cur) != std::cmp::Ordering::Greater {
        return Err(format!("This is version {} and you already have {cur}: nothing to update.", manifest.version));
    }
    let files = staging.join("files");
    let mut members = validate_tree(&files)?;
    // the kernel and its modules only when the package brings one this system does not have yet
    let new_kernel = manifest.kernel.as_deref().filter(|k| !ctx.root.join("lib/modules").join(k).exists());
    members.retain(|m| (!m.rel.starts_with("boot/") && !m.rel.starts_with("lib/modules/")) || new_kernel.is_some());
    if new_kernel.is_some() && !(files.join("boot/vmlinuz").exists() && files.join("boot/initramfs").exists()) { return Err("The package names a new kernel but does not contain it.".into()); }

    // 1. keep what is about to be replaced
    let st = state(ctx);
    let prev = st.join("prev");
    let _ = std::fs::remove_dir_all(&prev);
    std::fs::create_dir_all(prev.join("files")).map_err(|e| e.to_string())?;
    let mut added = String::new();
    for m in &members {
        if m.kind == Kind::Dir { continue; }
        let dest = ctx.root.join(&m.rel);
        if std::fs::symlink_metadata(&dest).is_ok() { copy_keep(&dest, &prev.join("files").join(&m.rel))?; } else { added.push_str(&m.rel); added.push('\n'); }
    }
    std::fs::write(prev.join("new-files"), &added).map_err(|e| e.to_string())?;
    std::fs::write(prev.join("version"), format!("{cur}\n")).map_err(|e| e.to_string())?;
    let conf_path = boot_dir(ctx).join("limine.conf");
    let old_conf = std::fs::read_to_string(&conf_path).ok();
    if let Some(c) = &old_conf { std::fs::write(prev.join("limine.conf"), c).map_err(|e| e.to_string())?; }
    if new_kernel.is_some() {
        for n in ["vmlinuz", "initramfs"] { let from = boot_dir(ctx).join(n); if from.exists() { let _ = std::fs::remove_file(boot_dir(ctx).join(format!("{n}.prev"))); std::fs::copy(&from, boot_dir(ctx).join(format!("{n}.prev"))).map_err(|e| e.to_string())?; } }
    }

    // 2. replace (the Exec85 program last, so a failure part way leaves the old program running)
    members.sort_by_key(|m| m.rel == "usr/bin/exec85");
    for m in &members {
        match &m.kind { Kind::Dir => {} _ => install_one(&files.join(&m.rel), &ctx.root.join(&m.rel))? }
    }
    std::fs::write(ctx.root.join(VERSION_FILE), format!("{}\n", manifest.version)).map_err(|e| e.to_string())?;

    // 3. the boot menu: for a trial period it shows, with the previous version as the second entry
    if let Some(uuid) = old_conf.as_deref().and_then(root_uuid_from) {
        let prev_files = if new_kernel.is_some() { ("vmlinuz.prev", "initramfs.prev") } else { ("vmlinuz", "initramfs") };
        let conf = limine_conf(&Boot { root_uuid: &uuid, timeout: 3, previous: Some(prev_files), default_previous: false });
        std::fs::write(boot_dir(ctx).join("limine.conf.new"), &conf).map_err(|e| e.to_string())?;
        std::fs::rename(boot_dir(ctx).join("limine.conf.new"), &conf_path).map_err(|e| e.to_string())?;
    }
    std::fs::write(st.join("pending"), format!("{}\n", manifest.version)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(st.join("available"));
    ctx.runner.run("sync", &[]);
    Ok(format!("Updated to {} (it was {cur}). Restart to start using it. For a few boots the menu shows for 3 seconds with \"previous version\" as a way back.", manifest.version))
}

/// Extract a package file and install it.
pub fn apply(ctx: &Ctx, package: &Path, force: bool) -> Result<String, String> {
    if crate::supervisor::cmdline_has("exec85.live") { return Err("Updates are for an installed system. This computer is running from the install disc.".into()); }
    let md = std::fs::metadata(package).map_err(|_| "I cannot read that file.".to_string())?;
    if !md.is_file() || md.len() == 0 || md.len() > MAX_PACKAGE_BYTES { return Err("That is not an update package (wrong kind of file or size).".into()); }
    let pkg = package.to_string_lossy().into_owned();
    let (code, listing) = ctx.runner.run("tar", &["-tzvf", &pkg]);
    if code != 0 { return Err("I cannot open that file as an Exec85 update package (it should be a .tar.gz).".into()); }
    parse_listing(&listing)?;                                     // refuse odd entries before anything is extracted
    let staging = state(ctx).join("staging");
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
    let (code, out) = ctx.runner.run("tar", &["-xzf", &pkg, "-C", &staging.to_string_lossy()]);
    if code != 0 { let _ = std::fs::remove_dir_all(&staging); return Err(format!("Unpacking the package failed: {}", out.lines().last().unwrap_or("").trim())); }
    let r = apply_staged(ctx, &staging, force);
    let _ = std::fs::remove_dir_all(&staging);
    r
}

// ---- trial, good, rollback ---------------------------------------------------------------------------------

/// The new version has run well: hide the boot menu again (the previous version stays saved for `update rollback`).
pub fn mark_good(ctx: &Ctx) -> Result<String, String> {
    if !pending(ctx) { return Ok("Nothing to confirm.".into()); }
    let conf_path = boot_dir(ctx).join("limine.conf");
    if let Some(uuid) = std::fs::read_to_string(&conf_path).ok().as_deref().and_then(root_uuid_from) {
        std::fs::write(boot_dir(ctx).join("limine.conf.new"), limine_conf(&Boot { root_uuid: &uuid, timeout: 0, previous: None, default_previous: false })).map_err(|e| e.to_string())?;
        std::fs::rename(boot_dir(ctx).join("limine.conf.new"), &conf_path).map_err(|e| e.to_string())?;
    }
    let _ = std::fs::remove_file(state(ctx).join("pending"));
    ctx.runner.run("sync", &[]);
    Ok("The new version is confirmed.".into())
}

/// Next boot: start the previous version (as if chosen in the menu), which restores it.
pub fn previous_next(ctx: &Ctx) -> Result<String, String> {
    let Some(prev) = previous_version(ctx) else { return Err("There is no previous version saved.".into()) };
    let conf_path = boot_dir(ctx).join("limine.conf");
    let uuid = std::fs::read_to_string(&conf_path).ok().as_deref().and_then(root_uuid_from).ok_or("I cannot read the boot menu settings.")?;
    let kernel_prev = boot_dir(ctx).join("vmlinuz.prev").exists();
    let files = if kernel_prev { ("vmlinuz.prev", "initramfs.prev") } else { ("vmlinuz", "initramfs") };
    std::fs::write(boot_dir(ctx).join("limine.conf.new"), limine_conf(&Boot { root_uuid: &uuid, timeout: 3, previous: Some(files), default_previous: true })).map_err(|e| e.to_string())?;
    std::fs::rename(boot_dir(ctx).join("limine.conf.new"), &conf_path).map_err(|e| e.to_string())?;
    ctx.runner.run("sync", &[]);
    Ok(format!("Next start will go back to version {prev}. Restart now to do it."))
}

/// Put the previous version back right now (the files; the running programs change at the next restart).
pub fn rollback_now(ctx: &Ctx) -> Result<String, String> {
    let Some(prev) = previous_version(ctx) else { return Err("There is no previous version saved.".into()) };
    let script = ctx.root.join(ROLLBACK_SCRIPT.trim_start_matches('/'));
    let root = ctx.root.to_string_lossy().into_owned();
    let (code, out) = if root == "/" { ctx.runner.run(&script.to_string_lossy(), &["--now"]) } else { ctx.runner.run("env", &[&format!("EXEC85_ROOT={root}"), &script.to_string_lossy(), "--now"]) };
    if code != 0 { return Err(format!("Going back failed: {}", out.lines().last().unwrap_or("").trim())); }
    Ok(format!("Version {prev} is back. Restart to start using it."))
}

// ---- where updates come from: the project's GitHub releases ---------------------------------------------------------

pub const DEFAULT_REPO: &str = "mogyorokremhu/Exec85";

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Release { pub version: String, pub package_url: String, pub sums_url: String }

/// The newest release's data from `https://api.github.com/repos/OWNER/NAME/releases/latest`.
pub fn parse_release(text: &str) -> Result<Release, String> {
    let j = crate::mc::parse_json(text).map_err(|_| "GitHub's answer was not readable.".to_string())?;
    let tag = j.get("tag_name").and_then(|t| t.str()).ok_or("GitHub's answer has no release in it (is the repository name right, and is the token allowed to read it?).")?;
    let version = tag.trim_start_matches('v').to_string();
    if !valid_version(&version) { return Err(format!("The newest release is called {tag}, which is not a version number.")); }
    let assets = j.get("assets").and_then(|a| a.arr()).ok_or("The release has no files.")?;
    let find = |want: &str| assets.iter().find(|a| a.get("name").and_then(|n| n.str()) == Some(want)).and_then(|a| a.get("url")).and_then(|u| u.str()).map(String::from);
    let package_url = find(&format!("exec85-update-v{version}.tar.gz")).ok_or_else(|| format!("Release {version} has no update package (exec85-update-v{version}.tar.gz)."))?;
    let sums_url = find("SHA256SUMS").ok_or("The release has no SHA256SUMS file.")?;
    for u in [&package_url, &sums_url] { if !u.starts_with("https://api.github.com/repos/") || u.contains(char::is_whitespace) || u.contains('"') { return Err("The release points at an address I do not accept.".into()); } }
    Ok(Release { version, package_url, sums_url })
}

/// The expected checksum of `name` from a SHA256SUMS file (`<64 hex digits>  <name>` per line).
pub fn sum_for(sums: &str, name: &str) -> Option<String> {
    sums.lines().filter_map(|l| { let mut w = l.split_whitespace(); let (h, n) = (w.next()?, w.next()?); (n.trim_start_matches('*') == name && h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit())).then(|| h.to_lowercase()) }).next()
}

/// Downloads go through GitHub's own servers only.
pub fn allowed_download_host(host: &str) -> bool { host == "api.github.com" || host == "github.com" || host.ends_with(".githubusercontent.com") }

fn https_host(url: &str) -> Option<&str> { let rest = url.strip_prefix("https://")?; let host = rest.split(|c| c == '/' || c == '?').next()?; (!host.is_empty() && !host.contains('@') && !host.contains(':')).then_some(host) }

/// A curl settings file (given on standard input, so the token never appears in a command line, where other users could see it).
pub fn curl_config(url: &str, accept: &str, token: Option<&str>, resolve: Option<&str>) -> Result<String, String> {
    let bad = |s: &str| s.chars().any(|c| c == '"' || c == '\n' || c == '\r' || c == '\\');
    if bad(url) || bad(accept) || token.map_or(false, bad) || resolve.map_or(false, bad) { return Err("A setting contains a character I do not accept.".into()); }
    let mut c = String::from("silent\nshow-error\nproto = \"=https\"\nuser-agent = \"Exec85-Updater\"\n");
    c.push_str(&format!("header = \"Accept: {accept}\"\n"));
    if let Some(t) = token { c.push_str(&format!("header = \"Authorization: Bearer {t}\"\n")); }
    if let Some(r) = resolve { c.push_str(&format!("resolve = \"{r}\"\n")); }
    c.push_str(&format!("url = \"{url}\"\n"));
    Ok(c)
}

fn resolve_for_host(host: &str) -> Option<String> {
    use std::net::ToSocketAddrs;
    let addrs: Vec<std::net::IpAddr> = (host, 443u16).to_socket_addrs().ok()?.map(|a| a.ip()).collect();
    crate::web::resolve_flag(host, &addrs)
}

fn tmp(ctx: &Ctx, name: &str) -> PathBuf { let d = ctx.root.join("run/exec85"); let _ = std::fs::create_dir_all(&d); d.join(name) }

/// GET one address (a GitHub API address) and return its body. `accept` is the Accept header; redirects are not followed.
fn api_get(ctx: &Ctx, url: &str, accept: &str, token: Option<&str>) -> Result<(u16, Option<String>, Vec<u8>), String> {
    let host = https_host(url).filter(|h| allowed_download_host(h)).ok_or("That address is not one the updater may use.")?;
    let (hdr, body) = (tmp(ctx, "update-h"), tmp(ctx, "update-b"));
    let cfg = curl_config(url, accept, token, resolve_for_host(host).as_deref())?;
    let (code, out) = ctx.runner.run_input("curl", &["-K", "-", "--max-redirs", "0", "--connect-timeout", "15", "--max-time", "60", "-D", &hdr.to_string_lossy(), "-o", &body.to_string_lossy()], &cfg);
    let head = std::fs::read_to_string(&hdr).unwrap_or_default();
    let bytes = std::fs::read(&body).unwrap_or_default();
    let _ = std::fs::remove_file(&hdr); let _ = std::fs::remove_file(&body);
    if code != 0 && head.is_empty() { return Err(if code == 6 { "I could not find github.com (name lookup failed). Is the network connected?".into() } else { format!("I could not reach GitHub: {}", out.lines().last().unwrap_or("").trim()) }); }
    let status = head.lines().next().and_then(|l| l.split_whitespace().nth(1)).and_then(|s| s.parse().ok()).unwrap_or(0);
    let location = head.lines().find_map(|l| l.split_once(':').filter(|(k, _)| k.eq_ignore_ascii_case("location")).map(|(_, v)| v.trim().to_string()));
    Ok((status, location, bytes))
}

fn http_error(status: u16) -> String {
    match status { 401 | 403 | 404 => format!("GitHub answered {status}. For a private repository you need an access token that may read it (Updates > T). Check the repository name too."), s => format!("GitHub answered {s}.") }
}

/// Download a release file (through the asset address, with the token) into `dest`. GitHub answers with a redirect to a signed address;
/// that one is fetched WITHOUT the token.
fn download_asset(ctx: &Ctx, url: &str, token: Option<&str>, dest: &Path, max_bytes: u64) -> Result<(), String> {
    let (status, location, _b) = api_get(ctx, url, "application/octet-stream", token)?;
    let loc = match (status, location) { (301 | 302 | 303 | 307 | 308, Some(l)) => l, (s, _) => return Err(http_error(s)) };
    let host = https_host(&loc).filter(|h| allowed_download_host(h)).ok_or("GitHub sent me to an address I do not accept.")?;
    let cfg = curl_config(&loc, "application/octet-stream", None, resolve_for_host(host).as_deref())?;
    let _ = std::fs::remove_file(dest);
    let (code, out) = ctx.runner.run_input("curl", &["-K", "-", "--max-redirs", "0", "--connect-timeout", "15", "--max-time", "1800", "--max-filesize", &max_bytes.to_string(), "--fail", "-o", &dest.to_string_lossy()], &cfg);
    if code != 0 { let _ = std::fs::remove_file(dest); return Err(format!("The download failed: {}", out.lines().last().unwrap_or("").trim())); }
    Ok(())
}

fn repo(ctx: &Ctx) -> String { conf_value(ctx, "repo").unwrap_or_else(|| DEFAULT_REPO.to_string()) }

/// Ask GitHub for the newest release and note whether it is newer than this system.
pub fn check_online(ctx: &Ctx) -> Result<String, String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo(ctx));
    let token = conf_value(ctx, "token");
    let (status, _l, body) = api_get(ctx, &url, "application/vnd.github+json", token.as_deref())?;
    if status != 200 { return Err(http_error(status)); }
    let rel = parse_release(&String::from_utf8_lossy(&body))?;
    let cur = current_version(ctx);
    let st = state(ctx);
    std::fs::create_dir_all(&st).map_err(|e| e.to_string())?;
    if crate::mc::cmp_versions(&rel.version, &cur) == std::cmp::Ordering::Greater {
        std::fs::write(st.join("available"), format!("{}\n{}\n{}\n", rel.version, rel.package_url, rel.sums_url)).map_err(|e| e.to_string())?;
        Ok(format!("Version {} is available (you have {cur}).", rel.version))
    } else {
        let _ = std::fs::remove_file(st.join("available"));
        Ok(format!("You have the newest version ({cur})."))
    }
}

/// Download the available update, check its checksum, and install it.
pub fn install_online(ctx: &Ctx) -> Result<String, String> {
    if crate::supervisor::cmdline_has("exec85.live") { return Err("Updates are for an installed system. This computer is running from the install disc.".into()); }
    if available_version(ctx).is_none() { check_online(ctx)?; }
    let text = std::fs::read_to_string(state(ctx).join("available")).map_err(|_| "There is no newer version.".to_string())?;
    let mut l = text.lines();
    let (version, package_url, sums_url) = (l.next().unwrap_or(""), l.next().unwrap_or(""), l.next().unwrap_or(""));
    if !valid_version(version) { return Err("The saved update note is damaged. Check again.".into()); }
    let token = conf_value(ctx, "token");
    let dir = state(ctx).join("download");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let name = format!("exec85-update-v{version}.tar.gz");
    let (pkg, sums) = (dir.join(&name), dir.join("SHA256SUMS"));
    download_asset(ctx, sums_url, token.as_deref(), &sums, 1 << 20)?;
    let want = sum_for(&std::fs::read_to_string(&sums).unwrap_or_default(), &name).ok_or("The release's SHA256SUMS does not list the update package.")?;
    download_asset(ctx, package_url, token.as_deref(), &pkg, MAX_PACKAGE_BYTES)?;
    let (code, out) = ctx.runner.run("sha256sum", &[&pkg.to_string_lossy()]);
    let got = out.split_whitespace().next().unwrap_or("").to_lowercase();
    if code != 0 || got != want { let _ = std::fs::remove_dir_all(&dir); return Err("The downloaded update is damaged or has been changed (its checksum does not match). Nothing was installed.".into()); }
    let r = apply(ctx, &pkg, false);
    let _ = std::fs::remove_dir_all(&dir);
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    #[test]
    fn manifests_are_read_strictly() {
        assert_eq!(parse_manifest("exec85-update 1\nversion=0.9.4\nkernel=6.12.58-0-lts\n").unwrap(), Manifest { version: "0.9.4".into(), kernel: Some("6.12.58-0-lts".into()) });
        assert_eq!(parse_manifest("exec85-update 1\nversion=1.0\n").unwrap().kernel, None);
        for bad in ["", "version=1.0\n", "exec85-update 2\nversion=1.0\n", "exec85-update 1\n", "exec85-update 1\nversion=1.0;rm\n", "exec85-update 1\nversion=1.0\nkernel=../../x\n", "exec85-update 1\nversion=1..0\n"] { assert!(parse_manifest(bad).is_err(), "{bad:?}"); }
    }

    #[test]
    fn only_the_systems_own_files_may_be_replaced() {
        for ok in ["usr/bin/exec85", "sbin/exec85-rollback", "etc/inittab", "etc/exec85/early-boot.sh", "etc/init.d/exec85-net", "etc/runlevels/default/exec85-net", "usr/share/exec85/audio/installer.mp3", "boot/vmlinuz", "lib/modules/6.12.58-0-lts/kernel/fs/ext4/ext4.ko.gz", "usr/bin/dir", "etc/exec85/package-version"] { assert!(allowed_path(ok), "{ok}"); }
        for bad in ["etc/passwd", "etc/shadow", "etc/exec85/firewall.conf", "etc/exec85/services/x.conf", "etc/exec85/update.conf", "etc/exec85/network.conf", "etc/exec85/ssh.conf", "etc/exec85/sites.allow", "etc/doas.conf", "etc/ssh/sshd_config", "home/tester/x", "srv/minecraft/x", "var/lib/exec85/update/pending", "etc/exec85/recipes/website", "bin/busybox", "usr/bin/sub/deeper", "../etc/passwd", "usr/bin/../../etc/passwd", "/etc/passwd", "usr//bin/exec85", "etc/exec85/early-boot.sh ", "", "usr/bin/ex ec", "boot/limine.conf", "boot/EFI/BOOT/BOOTX64.EFI"] { assert!(!allowed_path(bad), "{bad:?}"); }
        assert!(allowed_link("usr/bin/dir", "exec85") && !allowed_link("usr/bin/dir", "/bin/sh") && !allowed_link("usr/bin/dir", "../../etc/passwd"));
        assert!(allowed_link("etc/runlevels/default/exec85-net", "../../init.d/exec85-net") && !allowed_link("etc/runlevels/default/exec85-net", "/etc/shadow") && !allowed_link("etc/runlevels/default/exec85-net", "../../../etc/shadow"));
        assert!(!allowed_link("etc/passwd", "exec85"));
    }

    #[test]
    fn the_package_listing_is_checked_before_anything_is_unpacked() {
        let good = "-rw-r--r-- 0/0 40 2026-11-15 10:00:00 manifest\ndrwxr-xr-x 0/0 0 2026-11-15 10:00:00 files/\ndrwxr-xr-x 0/0 0 2026-11-15 10:00:00 files/usr/bin/\n-rwxr-xr-x 0/0 6422472 2026-11-15 10:00:00 files/usr/bin/exec85\nlrwxrwxrwx 0/0 0 2026-11-15 10:00:00 files/usr/bin/dir -> exec85\n";
        let m = parse_listing(good).unwrap();
        assert!(m.contains(&Member { rel: "usr/bin/exec85".into(), kind: Kind::File }) && m.contains(&Member { rel: "usr/bin/dir".into(), kind: Kind::Link("exec85".into()) }));
        for bad in ["-rw-r--r-- 0/0 5 d files/etc/passwd\n", "lrwxrwxrwx 0/0 0 d files/usr/bin/x -> /etc\n", "-rw-r--r-- 0/0 5 d /etc/passwd\n", "-rw-r--r-- 0/0 5 d other/file\n", "crw-r--r-- 0/0 0 d files/usr/bin/exec85\n", "hrw-r--r-- 0/0 0 d files/usr/bin/exec85\n", "-rw-r--r-- 0/0 5 d files/../etc/passwd\n", "", "-rw-r--r-- 0/0 40 d manifest\n"] { assert!(parse_listing(bad).is_err(), "{bad:?}"); }
    }

    #[test]
    fn the_boot_menu_for_the_trial_and_afterwards() {
        let uuid = "1111-2222-aaaa";
        let trial = limine_conf(&Boot { root_uuid: uuid, timeout: 3, previous: Some(("vmlinuz.prev", "initramfs.prev")), default_previous: false });
        assert!(trial.starts_with("timeout: 3\n") && trial.contains("/Exec85\n") && trial.contains("/Exec85 (previous version)") && trial.contains("path: boot():/vmlinuz.prev") && trial.contains("init=/sbin/exec85-rollback") && !trial.contains("default_entry"), "{trial}");
        assert_eq!(trial.matches("root=UUID=1111-2222-aaaa").count(), 2);
        let back = limine_conf(&Boot { root_uuid: uuid, timeout: 3, previous: Some(("vmlinuz", "initramfs")), default_previous: true });
        assert!(back.contains("default_entry: 2"), "{back}");
        let good = limine_conf(&Boot { root_uuid: uuid, timeout: 0, previous: None, default_previous: false });
        assert!(good.starts_with("timeout: 0\n") && !good.contains("previous") && !good.contains("exec85-rollback"), "{good}");
        assert_eq!(root_uuid_from(&good).as_deref(), Some(uuid), "the installer's own file and ours read the same way");
        assert_eq!(root_uuid_from(&crate::install::limine_conf("abcd-1234")).as_deref(), Some("abcd-1234"));
        assert_eq!(root_uuid_from("cmdline: quiet\n"), None);
    }

    struct Fake { calls: RefCell<Vec<String>> }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            if c == "env" || a.first() == Some(&"--now") { let mut cmd = std::process::Command::new(if c == "env" { a[1] } else { c }); if c == "env" { cmd.env("EXEC85_ROOT", a[0].trim_start_matches("EXEC85_ROOT=")).arg(a[2]); } else { cmd.arg("--now"); } let o = cmd.output().unwrap(); return (o.status.code().unwrap_or(1), String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr)); }
            (0, String::new())
        }
    }
    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-update-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for p in ["usr/bin", "sbin", "etc/exec85/services", "etc/init.d", "etc/runlevels/default", "boot", "lib/modules/6.12.1-lts", "home/tester", "srv/minecraft/mc"] { std::fs::create_dir_all(d.join(p)).unwrap(); }
        let w = |p: &str, t: &str| std::fs::write(d.join(p), t).unwrap();
        w("usr/bin/exec85", "OLD-BINARY"); w("etc/exec85/early-boot.sh", "old early boot"); w("etc/inittab", "old inittab");
        w("etc/exec85/firewall.conf", "enabled yes\nrule mine 9000 tcp local\n"); w("etc/exec85/services/mc.conf", "name=mc\n"); w("etc/exec85/network.conf", "eth0 dhcp\n");
        w("home/tester/essay.txt", "my essay"); w("srv/minecraft/mc/world", "WORLD");
        w("boot/vmlinuz", "OLD-KERNEL"); w("boot/initramfs", "OLD-INITRAMFS"); w("etc/exec85/package-version", "0.9.3\n");
        std::os::unix::fs::symlink("exec85", d.join("usr/bin/dir")).unwrap();
        std::fs::write(d.join("boot/limine.conf"), crate::install::limine_conf("abcd-1234")).unwrap();
        std::fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../image/overlay/sbin/exec85-rollback"), d.join("sbin/exec85-rollback")).unwrap();
        { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(d.join("sbin/exec85-rollback"), std::fs::Permissions::from_mode(0o755)).unwrap(); }
        d
    }
    /// A package as `tar` would unpack it.
    fn package(d: &Path, version: &str, kernel: Option<&str>) -> PathBuf {
        let s = d.join("pkgsrc");
        let _ = std::fs::remove_dir_all(&s);
        std::fs::create_dir_all(s.join("files/usr/bin")).unwrap(); std::fs::create_dir_all(s.join("files/etc/exec85")).unwrap(); std::fs::create_dir_all(s.join("files/etc/init.d")).unwrap();
        std::fs::write(s.join("manifest"), format!("exec85-update 1\nversion={version}\n{}", kernel.map_or(String::new(), |k| format!("kernel={k}\n")))).unwrap();
        std::fs::write(s.join("files/usr/bin/exec85"), "NEW-BINARY").unwrap();
        std::fs::write(s.join("files/etc/exec85/early-boot.sh"), "new early boot").unwrap();
        std::fs::write(s.join("files/etc/init.d/exec85-new"), "a new init script").unwrap();
        std::os::unix::fs::symlink("exec85", s.join("files/usr/bin/browse")).unwrap();
        if let Some(k) = kernel { std::fs::create_dir_all(s.join("files/boot")).unwrap(); std::fs::create_dir_all(s.join(format!("files/lib/modules/{k}"))).unwrap(); std::fs::write(s.join("files/boot/vmlinuz"), "NEW-KERNEL").unwrap(); std::fs::write(s.join("files/boot/initramfs"), "NEW-INITRAMFS").unwrap(); std::fs::write(s.join(format!("files/lib/modules/{k}/modules.dep")), "dep").unwrap(); }
        s
    }
    fn read(d: &Path, p: &str) -> String { std::fs::read_to_string(d.join(p)).unwrap_or_default() }

    #[test]
    fn an_update_replaces_only_the_systems_files_and_keeps_everything_else() {
        let r = world("apply"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        let pkg = package(&r, "0.9.4", None);
        let msg = apply_staged(&c, &pkg, false).unwrap();
        assert!(msg.contains("Updated to 0.9.4") && msg.contains("it was 0.9.3"), "{msg}");
        assert_eq!((read(&r, "usr/bin/exec85").as_str(), read(&r, "etc/exec85/early-boot.sh").as_str(), read(&r, "etc/init.d/exec85-new").as_str()), ("NEW-BINARY", "new early boot", "a new init script"));
        assert_eq!(std::fs::read_link(r.join("usr/bin/browse")).unwrap().to_string_lossy(), "exec85");
        // the owner's things are untouched
        assert_eq!(read(&r, "etc/exec85/firewall.conf"), "enabled yes\nrule mine 9000 tcp local\n");
        assert_eq!((read(&r, "etc/exec85/services/mc.conf").as_str(), read(&r, "etc/exec85/network.conf").as_str(), read(&r, "home/tester/essay.txt").as_str(), read(&r, "srv/minecraft/mc/world").as_str()), ("name=mc\n", "eth0 dhcp\n", "my essay", "WORLD"));
        assert_eq!(read(&r, "etc/inittab"), "old inittab", "a file the package does not contain stays as it is");
        assert_eq!(current_version(&c), "0.9.4");
        let st = status(&c);
        assert!(st.contains("version=0.9.4") && st.contains("pending=yes") && st.contains("previous=0.9.3"), "{st}");
        // the trial menu
        let conf = read(&r, "boot/limine.conf");
        assert!(conf.contains("timeout: 3") && conf.contains("/Exec85 (previous version)") && conf.contains("path: boot():/vmlinuz\n"), "kernel unchanged: the previous entry uses the same files, only the init differs\n{conf}");
        assert!(!r.join("boot/vmlinuz.prev").exists());
        // the same version again and older ones are refused unless forced
        assert!(apply_staged(&c, &package(&r, "0.9.4", None), false).unwrap_err().contains("nothing to update"));
        assert!(apply_staged(&c, &package(&r, "0.9.0", None), false).is_err());
        assert!(apply_staged(&c, &package(&r, "0.9.4", None), true).is_ok());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn nothing_is_installed_from_a_package_with_a_forbidden_file() {
        let r = world("evil"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        let pkg = package(&r, "0.9.4", None);
        std::fs::create_dir_all(pkg.join("files/etc")).unwrap(); std::fs::write(pkg.join("files/etc/passwd"), "root::0:0::/:/bin/sh\n").unwrap();
        let e = apply_staged(&c, &pkg, false).unwrap_err();
        assert!(e.contains("etc/passwd") && e.contains("may not touch"), "{e}");
        assert_eq!((read(&r, "usr/bin/exec85").as_str(), previous_version(&c)), ("OLD-BINARY", None), "nothing was replaced and nothing was saved");
        let pkg = package(&r, "0.9.4", None);
        std::os::unix::fs::symlink("/etc/shadow", pkg.join("files/usr/bin/evil")).unwrap();
        assert!(apply_staged(&c, &pkg, false).unwrap_err().contains("link"));
        assert_eq!(read(&r, "usr/bin/exec85"), "OLD-BINARY");
        let pkg = package(&r, "0.9.4", None); std::fs::remove_file(pkg.join("manifest")).unwrap();
        assert!(apply_staged(&c, &pkg, false).unwrap_err().contains("manifest"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn rollback_puts_the_old_files_back_and_removes_the_new_ones() {
        let r = world("rollback"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        apply_staged(&c, &package(&r, "0.9.4", None), false).unwrap();
        std::fs::write(r.join("home/tester/new-essay.txt"), "written after the update").unwrap();
        let msg = rollback_now(&c).unwrap();
        assert!(msg.contains("0.9.3 is back"), "{msg}");
        assert_eq!((read(&r, "usr/bin/exec85").as_str(), read(&r, "etc/exec85/early-boot.sh").as_str()), ("OLD-BINARY", "old early boot"));
        assert!(!r.join("etc/init.d/exec85-new").exists() && !r.join("usr/bin/browse").exists(), "files the update added are gone");
        assert_eq!(std::fs::read_link(r.join("usr/bin/dir")).unwrap().to_string_lossy(), "exec85");
        assert_eq!(read(&r, "boot/limine.conf"), crate::install::limine_conf("abcd-1234"), "the boot menu is as it was");
        assert!(!pending(&c) && previous_version(&c).is_none(), "{}", status(&c));
        assert_eq!(read(&r, "home/tester/new-essay.txt"), "written after the update", "nothing of the owner's is touched by going back");
        assert!(rollback_now(&c).unwrap_err().contains("no previous"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_new_kernel_is_installed_with_the_old_one_kept_for_the_previous_entry() {
        let r = world("kernel"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        apply_staged(&c, &package(&r, "0.9.4", Some("6.12.9-lts")), false).unwrap();
        assert_eq!((read(&r, "boot/vmlinuz").as_str(), read(&r, "boot/initramfs").as_str(), read(&r, "boot/vmlinuz.prev").as_str(), read(&r, "boot/initramfs.prev").as_str()), ("NEW-KERNEL", "NEW-INITRAMFS", "OLD-KERNEL", "OLD-INITRAMFS"));
        assert!(r.join("lib/modules/6.12.9-lts/modules.dep").exists() && r.join("lib/modules/6.12.1-lts").exists(), "the old modules stay for the old kernel");
        let conf = read(&r, "boot/limine.conf");
        assert!(conf.contains("path: boot():/vmlinuz.prev") && conf.contains("module_path: boot():/initramfs.prev"), "{conf}");
        // a kernel the system already has is not installed again
        let r2 = world("nokernel"); let c2 = Ctx { root: &r2, runner: &f };
        apply_staged(&c2, &package(&r2, "0.9.4", Some("6.12.1-lts")), false).unwrap();
        assert_eq!(read(&r2, "boot/vmlinuz"), "OLD-KERNEL");
        // going back brings the old kernel back
        rollback_now(&c).unwrap();
        assert_eq!((read(&r, "boot/vmlinuz").as_str(), read(&r, "boot/initramfs").as_str()), ("OLD-KERNEL", "OLD-INITRAMFS"));
        assert!(!r.join("boot/vmlinuz.prev").exists());
        let _ = std::fs::remove_dir_all(&r); let _ = std::fs::remove_dir_all(&r2);
    }

    #[test]
    fn a_good_trial_hides_the_menu_and_the_previous_version_can_be_chosen_for_the_next_start() {
        let r = world("good"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        assert_eq!(mark_good(&c).unwrap(), "Nothing to confirm.");
        apply_staged(&c, &package(&r, "0.9.4", None), false).unwrap();
        mark_good(&c).unwrap();
        let conf = read(&r, "boot/limine.conf");
        assert!(conf.starts_with("timeout: 0") && !conf.contains("previous"), "{conf}");
        assert!(!pending(&c) && previous_version(&c).as_deref() == Some("0.9.3"), "the previous version stays saved");
        let msg = previous_next(&c).unwrap();
        assert!(msg.contains("0.9.3"), "{msg}");
        let conf = read(&r, "boot/limine.conf");
        assert!(conf.contains("default_entry: 2") && conf.contains("timeout: 3") && conf.contains("init=/sbin/exec85-rollback"), "{conf}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn settings_are_private_one_word_values() {
        let r = world("conf"); let f = Fake { calls: RefCell::new(vec![]) }; let c = Ctx { root: &r, runner: &f };
        set_conf(&c, "repo", "me/Project").unwrap(); set_conf(&c, "token", "ghp_abc123").unwrap(); set_conf(&c, "good_after", "30").unwrap();
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(r.join(CONF)).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!((conf_value(&c, "repo").as_deref(), good_after(&c)), (Some("me/Project"), 30));
        assert!(status(&c).contains("token=yes") && !status(&c).contains("ghp_abc123"), "the token is never shown");
        set_conf(&c, "token", "").unwrap(); assert!(conf_value(&c, "token").is_none());
        for (k, v) in [("nosuch", "x"), ("repo", "no-slash"), ("repo", "a/b/c"), ("good_after", "1"), ("good_after", "x"), ("token", "has space"), ("token", "new\nline")] { assert!(set_conf(&c, k, v).is_err(), "{k}={v:?}"); }
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_newest_release_is_read_from_githubs_answer() {
        let j = r#"{"tag_name":"v0.9.5","assets":[{"name":"exec85-v0.9.5.iso","url":"https://api.github.com/repos/o/r/releases/assets/1"},{"name":"exec85-update-v0.9.5.tar.gz","url":"https://api.github.com/repos/o/r/releases/assets/2"},{"name":"SHA256SUMS","url":"https://api.github.com/repos/o/r/releases/assets/3"}]}"#;
        assert_eq!(parse_release(j).unwrap(), Release { version: "0.9.5".into(), package_url: "https://api.github.com/repos/o/r/releases/assets/2".into(), sums_url: "https://api.github.com/repos/o/r/releases/assets/3".into() });
        assert!(parse_release(&j.replace("exec85-update-v0.9.5.tar.gz", "other.tar.gz")).unwrap_err().contains("no update package"));
        assert!(parse_release(&j.replace(",{\"name\":\"SHA256SUMS\",\"url\":\"https://api.github.com/repos/o/r/releases/assets/3\"}", "")).unwrap_err().contains("SHA256SUMS"));
        assert!(parse_release(&j.replace("https://api.github.com/repos/o/r/releases/assets/2", "https://evil.example/x")).unwrap_err().contains("do not accept"));
        assert!(parse_release(&j.replace("v0.9.5\"", "latest\"")).is_err());
        assert!(parse_release(r#"{"message":"Not Found"}"#).unwrap_err().contains("token"));
        assert!(parse_release("not json").is_err());
    }

    #[test]
    fn checksums_and_hosts_are_checked() {
        let h = "a".repeat(64);
        assert_eq!(sum_for(&format!("{h}  exec85-update-v1.tar.gz\n{}  other\n", "b".repeat(64)), "exec85-update-v1.tar.gz").as_deref(), Some(h.as_str()));
        assert_eq!(sum_for(&format!("{h} *exec85-update-v1.tar.gz\n"), "exec85-update-v1.tar.gz").as_deref(), Some(h.as_str()), "the binary-mode star");
        assert_eq!(sum_for("short  exec85-update-v1.tar.gz\n", "exec85-update-v1.tar.gz"), None);
        assert_eq!(sum_for(&format!("{h}  other\n"), "exec85-update-v1.tar.gz"), None);
        for ok in ["api.github.com", "github.com", "objects.githubusercontent.com", "release-assets.githubusercontent.com"] { assert!(allowed_download_host(ok), "{ok}"); }
        for bad in ["evil.com", "githubusercontent.com.evil.com", "notgithub.com", "api.github.com.evil.com", "xgithub.com"] { assert!(!allowed_download_host(bad), "{bad}"); }
        assert_eq!(https_host("https://api.github.com/repos/x"), Some("api.github.com"));
        for bad in ["http://api.github.com/x", "https://user@api.github.com/x", "https://api.github.com:444/x", "https:///x"] { assert_eq!(https_host(bad), None, "{bad}"); }
    }

    #[test]
    fn the_token_goes_into_curls_settings_on_standard_input_never_into_arguments() {
        let c = curl_config("https://api.github.com/repos/o/r/releases/latest", "application/vnd.github+json", Some("ghp_secret"), Some("api.github.com:443:140.82.121.6")).unwrap();
        assert!(c.contains("header = \"Authorization: Bearer ghp_secret\"") && c.contains("resolve = \"api.github.com:443:140.82.121.6\"") && c.contains("proto = \"=https\"") && c.contains("url = \"https://api.github.com/repos/o/r/releases/latest\""), "{c}");
        assert!(!curl_config("https://x/y", "a", None, None).unwrap().contains("Authorization"));
        for bad in ["a\"b", "a\nb", "a\\b"] { assert!(curl_config(bad, "a", None, None).is_err() && curl_config("https://x", "a", Some(bad), None).is_err(), "{bad:?}"); }
    }

    struct Net { args: RefCell<Vec<String>>, configs: RefCell<Vec<String>>, tamper: bool, status: u16 }
    impl Runner for Net {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.args.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match c {
                "sha256sum" => (0, format!("{}  {}\n", if self.tamper { "0".repeat(64) } else { PKG_SUM.with(|s| s.borrow().clone()) }, a[0])),
                "tar" => { if a[0] == "-tzvf" { (0, "-rw-r--r-- 0/0 40 d manifest\ndrwxr-xr-x 0/0 0 d files/\n-rwxr-xr-x 0/0 5 d files/usr/bin/exec85\n".into()) } else { let to = a[a.iter().position(|x| *x == "-C").unwrap() + 1]; std::fs::create_dir_all(format!("{to}/files/usr/bin")).unwrap(); std::fs::write(format!("{to}/manifest"), "exec85-update 1\nversion=9.9.9\n").unwrap(); std::fs::write(format!("{to}/files/usr/bin/exec85"), "NEW-BINARY").unwrap(); (0, String::new()) } }
                _ => (0, String::new()),
            }
        }
        fn run_input(&self, c: &str, a: &[&str], input: &str) -> (i32, String) {
            self.args.borrow_mut().push(format!("{c} {}", a.join(" "))); self.configs.borrow_mut().push(input.to_string());
            let out = |flag: &str| a.iter().position(|x| *x == flag).map(|i| a[i + 1].to_string());
            let url = input.lines().find_map(|l| l.strip_prefix("url = \"").map(|u| u.trim_end_matches('"').to_string())).unwrap_or_default();
            if let Some(hdr) = out("-D") {   // an API address: a redirect (or an error status)
                let body = out("-o").unwrap();
                if url.ends_with("/releases/latest") { std::fs::write(&hdr, "HTTP/2 200 \r\n\r\n").unwrap(); std::fs::write(&body, r#"{"tag_name":"v9.9.9","assets":[{"name":"exec85-update-v9.9.9.tar.gz","url":"https://api.github.com/repos/o/r/releases/assets/2"},{"name":"SHA256SUMS","url":"https://api.github.com/repos/o/r/releases/assets/3"}]}"#).unwrap(); }
                else if self.status != 302 { std::fs::write(&hdr, format!("HTTP/2 {} \r\n\r\n", self.status)).unwrap(); std::fs::write(&body, "").unwrap(); }
                else { std::fs::write(&hdr, format!("HTTP/2 302 \r\nlocation: https://objects.githubusercontent.com/signed/{}\r\n\r\n", url.rsplit('/').next().unwrap())).unwrap(); std::fs::write(&body, "").unwrap(); }
                return (0, String::new());
            }
            let dest = out("-o").unwrap();
            if url.ends_with("/3") { std::fs::write(dest, format!("{}  exec85-update-v9.9.9.tar.gz\n", PKG_SUM.with(|s| s.borrow().clone()))).unwrap(); } else { std::fs::write(dest, "TARBALL").unwrap(); }
            (0, String::new())
        }
    }
    fn cx<'a>(r: &'a Path, f: &'a Net) -> Ctx<'a> { Ctx { root: r, runner: f } }
    thread_local! { static PKG_SUM: RefCell<String> = RefCell::new("c".repeat(64)); }

    #[test]
    fn an_update_is_found_downloaded_checked_and_installed_from_github() {
        let r = world("net");
        let f = Net { args: RefCell::new(vec![]), configs: RefCell::new(vec![]), tamper: false, status: 302 };
        set_conf(&cx(&r, &f), "repo", "o/r").unwrap(); set_conf(&cx(&r, &f), "token", "ghp_tok").unwrap();
        let msg = check_online(&cx(&r, &f)).unwrap();
        assert!(msg.contains("9.9.9 is available"), "{msg}");
        assert_eq!(available_version(&cx(&r, &f)).as_deref(), Some("9.9.9"));
        assert!(f.configs.borrow()[0].contains("Bearer ghp_tok") && f.configs.borrow()[0].contains("releases/latest"));
        assert!(!f.args.borrow().iter().any(|a| a.contains("ghp_tok")), "the token is in no command line: {:?}", f.args.borrow());
        let msg = install_online(&cx(&r, &f)).unwrap();
        assert!(msg.contains("Updated to 9.9.9"), "{msg}");
        assert_eq!(read(&r, "usr/bin/exec85"), "NEW-BINARY");
        // the token went to GitHub's API, not to the redirect target (which is a signed address)
        let cfgs = f.configs.borrow().join("\n---\n");
        for block in cfgs.split("\n---\n") { if block.contains("objects.githubusercontent.com") { assert!(!block.contains("Authorization"), "the signed download gets no token:\n{block}"); } }
        assert!(cfgs.contains("objects.githubusercontent.com"), "{cfgs}");
        assert!(!state(&cx(&r, &f)).join("download").exists(), "the download is cleaned up");
        let _ = std::fs::remove_dir_all(&r);
        // a package whose checksum does not match is never installed
        let r = world("nettamper");
        let f = Net { args: RefCell::new(vec![]), configs: RefCell::new(vec![]), tamper: true, status: 302 };
        check_online(&cx(&r, &f)).unwrap();
        let e = install_online(&cx(&r, &f)).unwrap_err();
        assert!(e.contains("damaged or has been changed"), "{e}");
        assert_eq!(read(&r, "usr/bin/exec85"), "OLD-BINARY");
        // a wrong token or repository gives a useful answer
        let f = Net { args: RefCell::new(vec![]), configs: RefCell::new(vec![]), tamper: false, status: 404 };
        let _ = std::fs::remove_dir_all(state(&cx(&r, &f)));
        std::fs::create_dir_all(state(&cx(&r, &f))).unwrap();
        std::fs::write(state(&cx(&r, &f)).join("available"), "9.9.9\nhttps://api.github.com/repos/o/r/releases/assets/2\nhttps://api.github.com/repos/o/r/releases/assets/3\n").unwrap();
        let e = install_online(&cx(&r, &f)).unwrap_err();
        assert!(e.contains("404") && e.contains("token"), "{e}");
        let _ = std::fs::remove_dir_all(&r);
    }
}
