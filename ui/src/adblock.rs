//! Ad blocker for the whole network: a small DNS server (dnsmasq) that answers "no such address" for the names of known ad and
//! tracker servers. Devices that use this computer as their DNS server (set in the router, or in one phone or PC) never load them.
use crate::helper::{self, Ctx};
use crate::services::{self, ServiceDef};
use std::collections::BTreeSet;

pub const SVC: &str = "adblock";
const DIR: &str = "etc/exec85/adblock";
/// The one list that is ever downloaded: a well-known, community-kept list of ad and tracker servers.
pub const LIST_URL: &str = "https://raw.githubusercontent.com/StevenBlack/hosts/master/hosts";
const MIN_ENTRIES: usize = 1000;

fn dir(ctx: &Ctx) -> std::path::PathBuf { ctx.root.join(DIR) }
fn have(ctx: &Ctx, prog: &str) -> bool { ["usr/bin", "usr/sbin", "bin", "sbin"].iter().any(|d| ctx.root.join(d).join(prog).exists()) }
fn defined(ctx: &Ctx) -> bool { ctx.root.join("etc/exec85/services").join(format!("{SVC}.conf")).exists() }

pub fn valid_domain(d: &str) -> bool {
    !d.is_empty() && d.len() <= 253 && d.contains('.') && !d.starts_with(['.', '-']) && !d.ends_with(['.', '-']) && !d.contains("..")
        && d.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-')
        && d.rsplit('.').next().map_or(false, |tld| tld.chars().any(|c| c.is_ascii_lowercase())) // an address like 0.0.0.0 is not a name
}

pub fn allowed(ctx: &Ctx) -> BTreeSet<String> {
    std::fs::read_to_string(dir(ctx).join("allow.txt")).unwrap_or_default().lines().map(|l| l.trim().to_lowercase()).filter(|l| valid_domain(l)).collect()
}

/// Names the person blocked on purpose (with all their subdomains), one per line in `block.txt`.
pub fn blocked(ctx: &Ctx) -> BTreeSet<String> {
    std::fs::read_to_string(dir(ctx).join("block.txt")).unwrap_or_default().lines().map(|l| l.trim().to_lowercase()).filter(|l| valid_domain(l)).collect()
}

/// One name from a line of any usual list format (hosts file: `0.0.0.0 name`; a plain name; an adblock rule `||name^`), or nothing.
pub fn parse_line(line: &str) -> Option<String> {
    let line = line.trim();
    if line.is_empty() || line.starts_with(['#', '!', '[']) { return None; }
    let name = if let Some(rest) = line.strip_prefix("||") {
        rest.split(['^', '/', '$', ':', ' ']).next().unwrap_or("")
    } else {
        let line = line.split('#').next().unwrap_or("").trim();
        let mut f = line.split_whitespace();
        let first = f.next()?;
        match f.next() { Some(second) => { if matches!(first, "0.0.0.0" | "127.0.0.1" | "::" | "::1") { second } else { return None } } None => first }
    };
    let name = name.trim_start_matches("*.").to_lowercase();
    if valid_domain(&name) && name != "localhost" && !name.ends_with(".localdomain") { Some(name) } else { None }
}

/// Is the name, or a parent of it, on the allowed list?
fn is_allowed(name: &str, allow: &BTreeSet<String>) -> bool {
    if allow.is_empty() { return false; }
    let mut n = name;
    loop {
        if allow.contains(n) { return true; }
        match n.split_once('.') { Some((_, rest)) if rest.contains('.') => n = rest, _ => return false }
    }
}

/// From the text of a list to the lines dnsmasq reads, without the allowed names (and their subdomains). Returns the text and the count.
pub fn build_hosts(raw: &str, allow: &BTreeSet<String>) -> (String, usize) {
    let mut out = String::new(); let mut n = 0;
    for name in raw.lines().filter_map(parse_line) {
        if is_allowed(&name, allow) { continue; }
        out.push_str("0.0.0.0 "); out.push_str(&name); out.push('\n'); n += 1;
    }
    (out, n)
}

const MIN_CAP: usize = 100_000;
/// How many names the DNS server may hold: a quarter of the memory, at about 150 bytes a name.
fn capacity(ctx: &Ctx) -> usize {
    let kb: usize = std::fs::read_to_string(ctx.root.join("proc/meminfo")).unwrap_or_default().lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(1_000_000);
    (kb * 1024 / 4 / 150).max(MIN_CAP)
}

/// Read the names of a list file (any size: one line at a time) into a set; stops with an error when the set gets bigger than `cap`.
fn read_names(path: &std::path::Path, into: &mut std::collections::HashSet<String>, cap: usize) -> Result<(), String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    for l in std::io::BufReader::new(f).split(b'\n') {
        let Ok(l) = l else { break };
        if let Some(n) = parse_line(&String::from_utf8_lossy(&l)) { into.insert(n); if into.len() > cap { return Err(format!("There are too many names: this computer can hold about {cap}. Remove a list (or use a smaller one).")); } }
    }
    Ok(())
}

fn count_valid(path: &std::path::Path) -> usize {
    let mut set = std::collections::HashSet::new();
    let _ = read_names(path, &mut set, usize::MAX);
    set.len()
}

fn lists_dir(ctx: &Ctx) -> std::path::PathBuf { dir(ctx).join("lists") }

/// The lists you added: (name, number of names, where it came from: a web address or `file`).
pub fn lists(ctx: &Ctx) -> Vec<(String, usize, String)> {
    let mut v: Vec<(String, usize, String)> = std::fs::read_dir(lists_dir(ctx)).into_iter().flatten().flatten().filter_map(|e| {
        let f = e.file_name().into_string().ok()?; let name = f.strip_suffix(".list")?.to_string();
        let n = std::fs::read_to_string(lists_dir(ctx).join(format!("{name}.count"))).ok().and_then(|t| t.trim().parse().ok()).unwrap_or(0);
        let src = std::fs::read_to_string(lists_dir(ctx).join(format!("{name}.url"))).map(|t| t.trim().to_string()).unwrap_or_else(|_| "file".into());
        Some((name, n, src))
    }).collect();
    v.sort();
    v
}

/// The DNS servers the computer itself uses now (never itself), or two well-known ones.
fn upstream(ctx: &Ctx) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_to_string(ctx.root.join("etc/resolv.conf")).unwrap_or_default().lines()
        .filter_map(|l| l.strip_prefix("nameserver ")).map(|s| s.trim().to_string()).filter(|s| !s.starts_with("127.") && s != "::1" && s.chars().all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':')).collect();
    v.dedup();
    if v.is_empty() { v = vec!["1.1.1.1".into(), "9.9.9.9".into()]; }
    v
}

pub fn conf_text(upstream: &[String], blocked: &BTreeSet<String>) -> String {
    let mut s = String::from("# Generated by Exec85 (Ad blocker). Change settings in the Ad blocker app, not in this file.\nno-hosts\nno-resolv\nbind-dynamic\ndomain-needed\nbogus-priv\ncache-size=2000\npid-file=/run/exec85-adblock.pid\n");
    s.push_str(&format!("addn-hosts=/{DIR}/blocklist.hosts\n"));
    for u in upstream { s.push_str(&format!("server={u}\n")); }
    for b in blocked { s.push_str(&format!("address=/{b}/0.0.0.0\naddress=/{b}/::\n")); }
    s
}

fn count_lines(path: &std::path::Path) -> usize { std::fs::read_to_string(path).map(|t| t.lines().count()).unwrap_or(0) }

/// `enabled=yes|no`, `installed=`, `entries=`, `updated=`, `allowed=` (comma list), `upstream=`.
pub fn status(ctx: &Ctx) -> String {
    let al: Vec<String> = allowed(ctx).into_iter().collect();
    let updated = std::fs::read_to_string(dir(ctx).join("updated")).map(|t| t.trim().to_string()).ok().filter(|t| !t.is_empty()).unwrap_or_else(|| "-".into());
    let up = upstream(ctx).join(",");
    let bl: Vec<String> = blocked(ctx).into_iter().collect();
    let ls: Vec<String> = lists(ctx).into_iter().map(|(n, c, _)| format!("{n}:{c}")).collect();
    format!("enabled={}\ninstalled={}\nentries={}\nupdated={updated}\nallowed={}\nblocked={}\nlists={}\ncapacity={}\nupstream={up}\n", if defined(ctx) { "yes" } else { "no" }, if have(ctx, "dnsmasq") { "yes" } else { "no" }, count_lines(&dir(ctx).join("blocklist.hosts")), if al.is_empty() { "-".to_string() } else { al.join(",") }, if bl.is_empty() { "-".to_string() } else { bl.join(",") }, if ls.is_empty() { "-".to_string() } else { ls.join(",") }, capacity(ctx))
}

fn today(ctx: &Ctx) -> String {
    let (code, out) = ctx.runner.run("date", &["+%Y-%m-%d"]);
    if code == 0 && out.trim().len() == 10 { out.trim().to_string() } else { "today".into() }
}

/// Download the list again (and keep it only if it looks right).
fn download(ctx: &Ctx) -> Result<(), String> {
    std::fs::create_dir_all(dir(ctx)).map_err(|e| e.to_string())?;
    let tmp = dir(ctx).join("blocklist.download");
    let (code, out) = ctx.runner.run("wget", &["-q", "-T", "120", "-U", "Exec85", "-O", &tmp.to_string_lossy(), "--", LIST_URL]);
    if code != 0 { let _ = std::fs::remove_file(&tmp); return Err(format!("Could not download the block list (is the computer online?): {}", out.lines().last().unwrap_or("").trim())); }
    let n = count_valid(&tmp);
    if n < MIN_ENTRIES { let _ = std::fs::remove_file(&tmp); return Err("The downloaded list does not look right (too short). Nothing was changed.".into()); }
    std::fs::rename(&tmp, dir(ctx).join("blocklist.raw")).map_err(|e| e.to_string())?;
    std::fs::write(dir(ctx).join("updated"), format!("{}\n", today(ctx))).map_err(|e| e.to_string())?;
    Ok(())
}

/// Write the files dnsmasq reads: the downloaded list and your own lists (without the allowed names), and the settings.
fn rebuild(ctx: &Ctx) -> Result<usize, String> {
    use std::io::Write;
    let main = dir(ctx).join("blocklist.raw");
    if !main.exists() { return Err("There is no block list yet; switch the ad blocker on first.".into()); }
    let cap = capacity(ctx);
    let mut names = std::collections::HashSet::new();
    read_names(&main, &mut names, cap)?;
    for e in std::fs::read_dir(lists_dir(ctx)).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().ends_with(".list") { read_names(&e.path(), &mut names, cap)?; }
    }
    let allow = allowed(ctx);
    let mut v: Vec<&String> = names.iter().filter(|n| !is_allowed(n, &allow)).collect();
    v.sort();
    let tmp = dir(ctx).join("blocklist.hosts.new");
    { let mut w = std::io::BufWriter::new(std::fs::File::create(&tmp).map_err(|e| e.to_string())?); for n in &v { writeln!(w, "0.0.0.0 {n}").map_err(|e| e.to_string())?; } w.flush().map_err(|e| e.to_string())?; }
    std::fs::rename(&tmp, dir(ctx).join("blocklist.hosts")).map_err(|e| e.to_string())?;
    std::fs::write(dir(ctx).join("dnsmasq.conf"), conf_text(&upstream(ctx), &blocked(ctx))).map_err(|e| e.to_string())?;
    Ok(v.len())
}

pub fn enable(ctx: &Ctx) -> Result<String, String> {
    if !have(ctx, "dnsmasq") {
        let (code, out) = ctx.runner.run("apk", &["add", "--no-progress", "dnsmasq"]);
        if code != 0 { return Err(format!("Could not install the DNS server (is the computer online?): {}", out.lines().last().unwrap_or("").trim())); }
    }
    if !dir(ctx).join("blocklist.raw").exists() { download(ctx)?; }
    let n = rebuild(ctx)?;
    let d = ServiceDef { name: SVC.into(), description: "Ad blocker (DNS server for the network)".into(), command: format!("/usr/sbin/dnsmasq --keep-in-foreground --conf-file=/{DIR}/dnsmasq.conf"), dir: "/".into(), user: "root".into(), env: vec![], autostart: true, ports: "53/udp,53/tcp".into() };
    helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&d)])?;
    helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()]).map_err(|e| format!("The ad blocker is set up but did not start: {}", e.lines().filter(|l| !l.contains("non existent service") && !l.contains("Caching service")).collect::<Vec<_>>().join(" ").trim()))?;
    Ok(format!("The ad blocker is on ({n} names blocked). Now set this computer's address as the DNS server in your router (or in one phone or PC) so that devices use it."))
}

pub fn disable(ctx: &Ctx) -> Result<String, String> {
    if defined(ctx) { let _ = helper::handle(ctx, &["svc-stop".to_string(), SVC.to_string()]); helper::handle(ctx, &["svc-remove".to_string(), SVC.to_string()])?; }
    Ok("The ad blocker is off. Devices that still use this computer as their DNS server will fail to look up names until you change that setting back.".into())
}

fn fetch_list(ctx: &Ctx, url: &str, to: &std::path::Path) -> Result<(), String> {
    let tmp = to.with_extension("download");
    let (code, out) = ctx.runner.run("wget", &["-q", "-T", "600", "-U", "Exec85", "-O", &tmp.to_string_lossy(), "--", url]);
    if code != 0 { let _ = std::fs::remove_file(&tmp); return Err(format!("Could not download {url}: {}", out.lines().last().unwrap_or("").trim())); }
    std::fs::rename(&tmp, to).map_err(|e| e.to_string())
}

fn list_name_from(source: &str) -> String {
    let base = source.trim_end_matches('/').rsplit('/').next().unwrap_or("list");
    let stem = base.rsplit_once('.').map_or(base, |(a, _)| a);
    let n: String = stem.to_lowercase().chars().map(|c| if c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' { c } else { '-' }).collect();
    let n = n.trim_matches(['-', '_']).chars().take(32).collect::<String>();
    if n.is_empty() { "list".into() } else { n }
}

fn valid_list_name(n: &str) -> bool { !n.is_empty() && n.len() <= 32 && n.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') }

/// Add a list of names to block: a file (hosts file, plain names one per line, or an adblock list) from /home, /srv, /media, /mnt or /data, or a web address (https).
pub fn import(ctx: &Ctx, source: &str, name: Option<&str>) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty() || source.chars().any(|c| c.is_control() || c == ' ') || source.len() > 400 { return Err("Give the full path of a list file (under /home, /srv, /media, /mnt or /data) or a web address starting with https://".into()); }
    let is_url = source.starts_with("https://");
    if source.starts_with("http://") { return Err("Use an https:// address.".into()); }
    if !is_url && (!["/home/", "/srv/", "/media/", "/mnt/", "/data/"].iter().any(|p| source.starts_with(p)) || source.contains("..")) { return Err("Give the full path of a list file under /home, /srv, /media, /mnt or /data, or a web address starting with https://".into()); }
    if is_url && !source[8..].split('/').next().map_or(false, |h| h.contains('.') && h.chars().all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == ':')) { return Err("That web address does not look right.".into()); }
    let name = match name { Some(n) if !n.is_empty() => n.to_string(), _ => list_name_from(source) };
    if !valid_list_name(&name) { return Err("The list name may only have lowercase letters, digits, - and _.".into()); }
    if !dir(ctx).join("blocklist.raw").exists() { return Err("Switch the ad blocker on first (press S).".into()); }
    std::fs::create_dir_all(lists_dir(ctx)).map_err(|e| e.to_string())?;
    let dest = lists_dir(ctx).join(format!("{name}.list"));
    let backup = lists_dir(ctx).join(format!("{name}.previous"));
    if dest.exists() { let _ = std::fs::rename(&dest, &backup); }
    let restore = |ctx: &Ctx| { let _ = std::fs::remove_file(lists_dir(ctx).join(format!("{name}.list"))); if backup.exists() { let _ = std::fs::rename(&backup, lists_dir(ctx).join(format!("{name}.list"))); } };
    let got = if is_url { fetch_list(ctx, source, &dest) } else {
        let real = ctx.root.join(source.trim_start_matches('/'));
        match std::fs::metadata(&real) { Ok(m) if m.is_file() => std::fs::copy(&real, &dest).map(|_| ()).map_err(|e| e.to_string()), Ok(_) => Err("That is not a file.".into()), Err(_) => Err("That file does not exist.".into()) }
    };
    if let Err(e) = got { restore(ctx); return Err(e); }
    let n = count_valid(&dest);
    if n == 0 { restore(ctx); return Err("I found no site names in it. A list is a hosts file (0.0.0.0 name), names one per line, or an adblock list (||name^).".into()); }
    let _ = std::fs::write(lists_dir(ctx).join(format!("{name}.count")), format!("{n}\n"));
    if is_url { let _ = std::fs::write(lists_dir(ctx).join(format!("{name}.url")), format!("{source}\n")); } else { let _ = std::fs::remove_file(lists_dir(ctx).join(format!("{name}.url"))); }
    let total = match rebuild(ctx) { Ok(t) => t, Err(e) => { restore(ctx); let _ = rebuild(ctx); return Err(e); } };
    let _ = std::fs::remove_file(&backup);
    if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; }
    Ok(format!("List {name} added: {n} names. The ad blocker now blocks {total} names in total."))
}

pub fn unimport(ctx: &Ctx, name: &str) -> Result<String, String> {
    if !valid_list_name(name) || !lists_dir(ctx).join(format!("{name}.list")).exists() { return Err("There is no list with that name (see the Lists line).".into()); }
    for ext in ["list", "count", "url"] { let _ = std::fs::remove_file(lists_dir(ctx).join(format!("{name}.{ext}"))); }
    let total = rebuild(ctx)?;
    if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; }
    Ok(format!("List {name} removed. The ad blocker now blocks {total} names."))
}

pub fn update(ctx: &Ctx) -> Result<String, String> {
    download(ctx)?;
    let mut failed = vec![];
    for (name, _, src) in lists(ctx) {
        if !src.starts_with("https://") { continue; }
        let dest = lists_dir(ctx).join(format!("{name}.list"));
        let keep = lists_dir(ctx).join(format!("{name}.previous"));
        let _ = std::fs::copy(&dest, &keep);
        match fetch_list(ctx, &src, &dest) {
            Ok(()) if count_valid(&dest) > 0 => { let _ = std::fs::write(lists_dir(ctx).join(format!("{name}.count")), format!("{}\n", count_valid(&dest))); }
            _ => { let _ = std::fs::rename(&keep, &dest); failed.push(name); }
        }
        let _ = std::fs::remove_file(&keep);
    }
    let n = rebuild(ctx)?;
    if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; }
    Ok(if failed.is_empty() { format!("The block lists are up to date ({n} names).") } else { format!("The block lists are up to date ({n} names), but these lists could not be downloaded and were kept as they were: {}.", failed.join(", ")) })
}

pub fn allow(ctx: &Ctx, domain: &str) -> Result<String, String> {
    let d = domain.trim().to_lowercase();
    if !valid_domain(&d) { return Err("Type a name like example.com (lowercase letters, digits, - and dots).".into()); }
    let mut set = allowed(ctx); set.insert(d.clone());
    save_allowed(ctx, &set)?;
    if dir(ctx).join("blocklist.raw").exists() { rebuild(ctx)?; if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; } }
    Ok(format!("{d} (and its subdomains) will no longer be blocked."))
}

pub fn unallow(ctx: &Ctx, domain: &str) -> Result<String, String> {
    let d = domain.trim().to_lowercase();
    let mut set = allowed(ctx);
    if !set.remove(&d) { return Err("That name is not on the allowed list.".into()); }
    save_allowed(ctx, &set)?;
    if dir(ctx).join("blocklist.raw").exists() { rebuild(ctx)?; if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; } }
    Ok(format!("{d} is blocked again if it is on the list."))
}

fn save_names(ctx: &Ctx, file: &str, set: &BTreeSet<String>) -> Result<(), String> {
    std::fs::create_dir_all(dir(ctx)).map_err(|e| e.to_string())?;
    std::fs::write(dir(ctx).join(file), set.iter().map(|d| format!("{d}\n")).collect::<String>()).map_err(|e| e.to_string())
}
fn save_allowed(ctx: &Ctx, set: &BTreeSet<String>) -> Result<(), String> { save_names(ctx, "allow.txt", set) }

pub fn block(ctx: &Ctx, domain: &str) -> Result<String, String> {
    let d = domain.trim().to_lowercase();
    if !valid_domain(&d) { return Err("Type a name like example.com (lowercase letters, digits, - and dots).".into()); }
    let mut set = blocked(ctx); set.insert(d.clone());
    save_names(ctx, "block.txt", &set)?;
    // a name you block on purpose is no longer on the allowed list
    let mut al = allowed(ctx); if al.remove(&d) { save_names(ctx, "allow.txt", &al)?; }
    if dir(ctx).join("blocklist.raw").exists() { rebuild(ctx)?; if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; } }
    Ok(format!("{d} and all its subdomains are blocked."))
}

pub fn unblock(ctx: &Ctx, domain: &str) -> Result<String, String> {
    let d = domain.trim().to_lowercase();
    let mut set = blocked(ctx);
    if !set.remove(&d) { return Err("That name is not on your own block list.".into()); }
    save_names(ctx, "block.txt", &set)?;
    if dir(ctx).join("blocklist.raw").exists() { rebuild(ctx)?; if defined(ctx) { helper::handle(ctx, &["svc-restart".to_string(), SVC.to_string()])?; } }
    Ok(format!("{d} is no longer blocked by you (it stays blocked if it is on the downloaded list)."))
}

/// Is this name on the list that is in use (the allowed names are already taken out of it)?
pub fn check(ctx: &Ctx, domain: &str) -> Result<String, String> {
    let d = domain.trim().to_lowercase();
    if !valid_domain(&d) { return Err("Type a name like ads.example.com.".into()); }
    if blocked(ctx).iter().any(|b| d == *b || d.ends_with(&format!(".{b}"))) { return Ok(format!("{d} is BLOCKED (on your own block list).")); }
    let hosts = std::fs::read_to_string(dir(ctx).join("blocklist.hosts")).map_err(|_| "There is no block list yet; switch the ad blocker on first.".to_string())?;
    let needle = format!("0.0.0.0 {d}");
    if hosts.lines().any(|l| l == needle) { Ok(format!("{d} is BLOCKED.")) }
    else if allowed(ctx).iter().any(|a| d == *a || d.ends_with(&format!(".{a}"))) { Ok(format!("{d} is on your allowed list, so it is not blocked.")) }
    else { Ok(format!("{d} is not on the list, so it is not blocked.")) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, list: String }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match c {
                "wget" => { let to = a[a.iter().position(|x| *x == "-O").unwrap() + 1]; std::fs::write(to, &self.list).unwrap(); (0, String::new()) }
                "date" => (0, String::from("2026-11-20")),
                _ => (0, String::new()),
            }
        }
    }
    fn world(tag: &str, with_pkg: bool) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-ab-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc/exec85/services")).unwrap(); std::fs::create_dir_all(d.join("usr/sbin")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/resolv.conf"), "nameserver 192.168.1.1\nnameserver 127.0.0.1\n").unwrap();
        if with_pkg { std::fs::write(d.join("usr/sbin/dnsmasq"), "").unwrap(); }
        d
    }
    fn list(n: usize) -> String {
        let mut s = String::from("# a list\n127.0.0.1 localhost\n::1 localhost\n0.0.0.0 0.0.0.0\n");
        for i in 0..n { s.push_str(&format!("0.0.0.0 ads{i}.example.net # tracker\n")); }
        s.push_str("0.0.0.0 ad.doubleclick.net\n0.0.0.0 Metrics.Example.ORG\nnot a host line\n");
        s
    }

    #[test]
    fn the_list_is_turned_into_dnsmasq_lines_without_allowed_names() {
        let raw = list(5);
        let (h, n) = build_hosts(&raw, &BTreeSet::new());
        assert_eq!(n, 7, "{h}");
        assert!(h.contains("0.0.0.0 ad.doubleclick.net\n") && h.contains("0.0.0.0 metrics.example.org\n") && !h.contains("localhost") && !h.contains("0.0.0.0 0.0.0.0"), "{h}");
        let allow: BTreeSet<String> = ["example.net".to_string(), "doubleclick.net".to_string()].into();
        let (h2, n2) = build_hosts(&raw, &allow);
        assert_eq!(n2, 1, "{h2}");
        assert!(h2.contains("metrics.example.org") && !h2.contains("ads0.example.net") && !h2.contains("ad.doubleclick.net"), "allowed names and their subdomains are left out: {h2}");
        assert!(valid_domain("a.b-c.org") && !valid_domain("nodot") && !valid_domain("-a.org") && !valid_domain("a..org") && !valid_domain("a b.org") && !valid_domain("A.org"));
    }

    #[test]
    fn switching_on_installs_downloads_writes_the_files_and_starts_the_service() {
        let r = world("on", false);
        let f = Fake { calls: RefCell::new(vec![]), list: list(1500) };
        let ctx = Ctx { root: &r, runner: &f };
        assert!(check(&ctx, "ad.doubleclick.net").unwrap_err().contains("switch the ad blocker on"));
        let m = enable(&ctx).unwrap();
        assert!(m.contains("1502 names") && m.contains("DNS server"), "{m}");
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("apk add --no-progress dnsmasq") && calls.contains(LIST_URL), "{calls}");
        let conf = std::fs::read_to_string(r.join("etc/exec85/adblock/dnsmasq.conf")).unwrap();
        assert!(conf.contains("addn-hosts=/etc/exec85/adblock/blocklist.hosts") && conf.contains("server=192.168.1.1") && !conf.contains("server=127.") && conf.contains("no-resolv"), "{conf}");
        let svc = std::fs::read_to_string(r.join("etc/exec85/services/adblock.conf")).unwrap();
        assert!(svc.contains("/usr/sbin/dnsmasq --keep-in-foreground --conf-file=/etc/exec85/adblock/dnsmasq.conf") && svc.contains("ports=53/udp,53/tcp") && svc.contains("user=root"), "{svc}");
        let st = status(&ctx);
        assert!(st.contains("enabled=yes") && st.contains("entries=1502") && st.contains("updated=2026-11-20") && st.contains("upstream=192.168.1.1"), "{st}");
        assert!(check(&ctx, "ad.doubleclick.net").unwrap().contains("BLOCKED"));
        assert!(check(&ctx, "www.example.com").unwrap().contains("not blocked"));
        assert!(check(&ctx, "bad name").is_err());
    }

    #[test]
    fn names_are_read_from_every_usual_list_format() {
        for (line, want) in [("0.0.0.0 ads.example.net", Some("ads.example.net")), ("127.0.0.1 Track.Example.org # note", Some("track.example.org")), ("plain.example.com", Some("plain.example.com")), ("||adblock.example.com^", Some("adblock.example.com")),
            ("||rule.example.com^$third-party", Some("rule.example.com")), ("*.wild.example.com", Some("wild.example.com")), ("# comment", None), ("! comment", None), ("[Adblock Plus 2.0]", None), ("", None),
            ("127.0.0.1 localhost", None), ("0.0.0.0 0.0.0.0", None), ("not a host line", None), ("@@||allowed.example.com^", None), ("1.2.3.4", None)] {
            assert_eq!(parse_line(line).as_deref(), want, "{line:?}");
        }
    }

    #[test]
    fn your_own_lists_are_imported_from_files_and_addresses_merged_limited_and_removed() {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::Permissions::from_mode(0o644);
        let r = world("import", true);
        let f = Fake { calls: RefCell::new(vec![]), list: list(1500) };
        let ctx = Ctx { root: &r, runner: &f };
        assert!(import(&ctx, "/home/guy/mine.txt", None).unwrap_err().contains("Switch the ad blocker on"));
        enable(&ctx).unwrap();
        std::fs::create_dir_all(r.join("home/guy")).unwrap();
        std::fs::write(r.join("home/guy/My List.txt"), "# mine\nfirst.example.io\n0.0.0.0 second.example.io\n||third.example.io^\nads0.example.net\n").unwrap();
        std::fs::write(r.join("home/guy/mylist.txt"), "# mine\nfirst.example.io\n0.0.0.0 second.example.io\n||third.example.io^\nads0.example.net\n").unwrap();
        for bad in ["http://x.example.org/l", "/etc/passwd", "/home/guy/../../etc/passwd", "/home/guy/mylist.txt extra", "relative.txt", "https://nodot/list", ""] { assert!(import(&ctx, bad, None).is_err(), "{bad:?}"); }
        assert!(import(&ctx, "/home/guy/missing.txt", None).unwrap_err().contains("does not exist"));
        std::fs::write(r.join("home/guy/empty.txt"), "# nothing here\nnot a list at all\n").unwrap();
        assert!(import(&ctx, "/home/guy/empty.txt", None).unwrap_err().contains("no site names"));
        assert!(!r.join("etc/exec85/adblock/lists/empty.list").exists(), "a bad list leaves nothing behind");
        let m = import(&ctx, "/home/guy/mylist.txt", None).unwrap();
        assert!(m.contains("List mylist added: 4 names") && m.contains("1505 names in total"), "{m}");
        assert!(check(&ctx, "second.example.io").unwrap().contains("BLOCKED") && status(&ctx).contains("lists=mylist:4"), "{}", status(&ctx));
        // from a web address; the same list again under another name is merged without doubles
        let m2 = import(&ctx, "https://lists.example.org/path/big.hosts", Some("big")).unwrap();
        assert!(m2.contains("List big added: 1502 names") && m2.contains("names in total"), "{m2}");
        assert!(f.calls.borrow().iter().any(|c| c.contains("https://lists.example.org/path/big.hosts")));
        assert!(status(&ctx).contains("lists=big:1502,mylist:4"), "{}", status(&ctx));
        assert!(std::fs::read_to_string(r.join("etc/exec85/adblock/lists/big.url")).unwrap().contains("https://lists.example.org"));
        assert!(update(&ctx).unwrap().contains("up to date") && f.calls.borrow().iter().filter(|c| c.contains("lists.example.org")).count() == 2, "web lists are fetched again on update, files are not");
        // allowed names also leave imported lists
        allow(&ctx, "example.io").unwrap();
        assert!(check(&ctx, "second.example.io").unwrap().contains("allowed list"));
        unallow(&ctx, "example.io").unwrap();
        assert!(unimport(&ctx, "nothing").is_err() && unimport(&ctx, "../x").is_err());
        let u = unimport(&ctx, "mylist").unwrap();
        assert!(u.contains("removed") && check(&ctx, "second.example.io").unwrap().contains("not blocked"), "{u}");
        // the limit: this computer holds only so many names
        std::fs::create_dir_all(r.join("proc")).unwrap(); std::fs::write(r.join("proc/meminfo"), "MemTotal: 1000 kB\n").unwrap();
        let mut huge = String::new(); for i in 0..(MIN_CAP + 50) { huge.push_str(&format!("h{i}.example.biz\n")); }
        std::fs::write(r.join("home/guy/huge.txt"), huge).unwrap();
        let e = import(&ctx, "/home/guy/huge.txt", None).unwrap_err();
        assert!(e.contains("too many names"), "{e}");
        assert!(!r.join("etc/exec85/adblock/lists/huge.list").exists() && status(&ctx).contains("lists=big:1502"), "the old state is kept");
    }

    #[test]
    fn names_can_be_allowed_and_blocked_again_and_the_list_updated_and_the_blocker_switched_off() {
        let r = world("allow", true);
        let f = Fake { calls: RefCell::new(vec![]), list: list(1500) };
        let ctx = Ctx { root: &r, runner: &f };
        enable(&ctx).unwrap();
        assert!(!f.calls.borrow().iter().any(|c| c.starts_with("apk")), "no second install");
        assert!(allow(&ctx, "NOT A NAME").is_err());
        allow(&ctx, "Ad.Doubleclick.net").unwrap();
        assert!(check(&ctx, "ad.doubleclick.net").unwrap().contains("allowed list"));
        assert!(status(&ctx).contains("allowed=ad.doubleclick.net") && status(&ctx).contains("entries=1501"));
        assert!(unallow(&ctx, "nothing.example.org").is_err());
        unallow(&ctx, "ad.doubleclick.net").unwrap();
        assert!(check(&ctx, "ad.doubleclick.net").unwrap().contains("BLOCKED"));
        // a bad download changes nothing
        let bad = Fake { calls: RefCell::new(vec![]), list: "<html>not a list</html>".into() };
        assert!(update(&Ctx { root: &r, runner: &bad }).unwrap_err().contains("does not look right"));
        assert!(status(&ctx).contains("entries=1502"));
        assert!(update(&ctx).unwrap().contains("up to date"));
        // your own names: blocked with their subdomains, in the DNS server's settings; and taken away again
        assert!(block(&ctx, "bad name").is_err() && unblock(&ctx, "nothing.example.org").is_err());
        block(&ctx, "Games.Example.COM").unwrap();
        let conf = std::fs::read_to_string(r.join("etc/exec85/adblock/dnsmasq.conf")).unwrap();
        assert!(conf.contains("address=/games.example.com/0.0.0.0") && conf.contains("address=/games.example.com/::"), "{conf}");
        assert!(check(&ctx, "www.games.example.com").unwrap().contains("BLOCKED") && status(&ctx).contains("blocked=games.example.com"));
        allow(&ctx, "games.example.com").unwrap(); block(&ctx, "games.example.com").unwrap();
        assert!(!status(&ctx).contains("allowed=games"), "blocking on purpose takes the name off the allowed list: {}", status(&ctx));
        unblock(&ctx, "games.example.com").unwrap();
        assert!(!std::fs::read_to_string(r.join("etc/exec85/adblock/dnsmasq.conf")).unwrap().contains("address=/") && check(&ctx, "www.games.example.com").unwrap().contains("not blocked"));
        disable(&ctx).unwrap();
        assert!(!r.join("etc/exec85/services/adblock.conf").exists() && status(&ctx).contains("enabled=no"));
    }
}
