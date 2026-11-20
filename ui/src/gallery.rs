//! Photo and video gallery: a web page that shows the pictures and videos of ONE folder you choose, and nothing else.
//!
//! The server (`exec85 --galleryd`) listens on this computer only; stunnel in front of it does the encryption on port 8445.
//! It runs as the owner of the chosen folder (never root) and only hands out
//!   - files inside that folder (a path is refused if it has `..`, starts with a dot, or if its real location after following
//!     links is outside the folder: a link to /etc/passwd inside the folder shows nothing),
//!   - whose names end like a picture or a video (nothing else is listed or served: notes, documents and the like stay private),
//!   - to somebody who knows the gallery's password (its own, so family can look without having an account on this computer).
//! Videos can be skipped through (HTTP range requests). Uploading from the browser is off unless switched on.
use crate::account;
use crate::crypt;
use crate::helper::{self, Ctx};
use crate::remote::{self, Limiter, Request};
use crate::services::{self, ServiceDef};
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const LISTEN_PORT: u16 = 8444;
pub const PUBLIC_PORT: u16 = 8445;
pub const SERVICE: &str = "gallery";
pub const TLS: &str = "gallery-tls";
const DIR: &str = "etc/exec85/gallery";
const PAGE: &str = include_str!("gallery_page.html");
const SESSION_LIFETIME: Duration = Duration::from_secs(24 * 3600);
const MAX_UPLOAD: u64 = 2 << 30;
const MAX_ENTRIES: usize = 5000;
const MAX_CONNECTIONS: usize = 64;

// ---- what counts as a picture or a video -------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Image, Video }

/// (kind, content type) from the file name's ending. SVG is left out on purpose: it can carry scripts.
pub fn media_kind(name: &str) -> Option<(Kind, &'static str)> {
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" => (Kind::Image, "image/jpeg"), "png" => (Kind::Image, "image/png"), "gif" => (Kind::Image, "image/gif"),
        "webp" => (Kind::Image, "image/webp"), "avif" => (Kind::Image, "image/avif"), "bmp" => (Kind::Image, "image/bmp"),
        "mp4" | "m4v" => (Kind::Video, "video/mp4"), "webm" => (Kind::Video, "video/webm"), "mov" => (Kind::Video, "video/quicktime"), "ogv" => (Kind::Video, "video/ogg"),
        _ => return None,
    })
}

/// A path below the gallery folder: the real file, only if it really is inside `root` (already canonical).
pub fn resolve(root: &Path, rel: &str) -> Option<PathBuf> {
    if rel.contains('\0') || rel.starts_with('/') || rel.len() > 1000 { return None; }
    let mut p = root.to_path_buf();
    for c in rel.split('/') {
        if c.is_empty() { continue; }
        if c == "." || c == ".." || c.starts_with('.') { return None; }
        p.push(c);
    }
    let real = std::fs::canonicalize(&p).ok()?;
    if real.starts_with(root) { Some(real) } else { None }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Entry { pub name: String, pub kind: &'static str, pub size: u64, pub mtime: u64 }

/// The folders and the pictures and videos of one folder, folders first, by name.
pub fn list(root: &Path, rel: &str) -> Option<Vec<Entry>> {
    let dir = resolve(root, rel)?;
    if !dir.is_dir() { return None; }
    let mut out = Vec::new();
    for e in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || name.contains(['"', '\\', '\n', '\r']) { continue; }
        let Ok(real) = std::fs::canonicalize(e.path()) else { continue };
        if !real.starts_with(root) { continue; }                 // a link that leads out of the folder shows nothing
        let Ok(md) = std::fs::metadata(&real) else { continue };
        let mtime = md.mtime().max(0) as u64;
        if md.is_dir() { out.push(Entry { name, kind: "dir", size: 0, mtime }); }
        else if md.is_file() { if let Some((k, _)) = media_kind(&name) { out.push(Entry { name, kind: if k == Kind::Image { "image" } else { "video" }, size: md.len(), mtime }); } }
        if out.len() >= MAX_ENTRIES { break; }
    }
    out.sort_by(|a, b| (a.kind != "dir", a.name.to_lowercase()).cmp(&(b.kind != "dir", b.name.to_lowercase())));
    Some(out)
}

/// `Range: bytes=0-99`, `bytes=100-`, `bytes=-500` -> the inclusive byte range, or None when it cannot be satisfied.
pub fn parse_range(h: &str, size: u64) -> Option<(u64, u64)> {
    let spec = h.trim().strip_prefix("bytes=")?;
    if spec.contains(',') || size == 0 { return None; }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() { let n: u64 = b.parse().ok()?; if n == 0 { return None; } (size.saturating_sub(n), size - 1) } else { (a.parse().ok()?, if b.is_empty() { size - 1 } else { b.parse::<u64>().ok()?.min(size - 1) }) };
    if start > end || start >= size { None } else { Some((start, end)) }
}

fn valid_upload_name(n: &str) -> bool { remote::valid_file_name(n) && media_kind(n).is_some() && !n.contains(['"', '\\']) }

// ---- the saved setup -----------------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Config { pub folder: String, pub user: String, pub uploads: bool, pub hash: String }

fn conf_path(ctx: &Ctx) -> PathBuf { ctx.root.join(DIR).join("gallery.conf") }

pub fn load_config(ctx: &Ctx) -> Config {
    let mut c = Config::default();
    for l in std::fs::read_to_string(conf_path(ctx)).unwrap_or_default().lines() {
        match l.split_once('=') {
            Some(("folder", v)) => c.folder = v.into(), Some(("user", v)) => c.user = v.into(),
            Some(("uploads", v)) => c.uploads = v == "yes", Some(("hash", v)) => c.hash = v.into(), _ => {}
        }
    }
    c
}
fn save_config(ctx: &Ctx, c: &Config) -> Result<(), String> {
    std::fs::create_dir_all(ctx.root.join(DIR)).map_err(|e| e.to_string())?;
    std::fs::write(conf_path(ctx), format!("folder={}\nuser={}\nuploads={}\nhash={}\n", c.folder, c.user, if c.uploads { "yes" } else { "no" }, c.hash)).map_err(|e| e.to_string())?;
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(conf_path(ctx), std::fs::Permissions::from_mode(0o600)); }
    Ok(())
}

/// Where a gallery folder may be: a person's own files or a storage disk, never the system.
pub fn allowed_folder(p: &str) -> bool {
    p.len() < 300 && !p.contains("..") && !p.contains(['\0', '\n', '\r', ' ', '"', '\'', '\\', '$', '`', ';', '|', '&']) && p.ends_with(|c: char| c != '/')
        && ["/home/", "/srv/", "/media/", "/mnt/", "/data/"].iter().any(|pre| p.starts_with(pre) && p.len() > pre.len())
}

/// The user the server runs as: the folder's owner, who must be a normal user (not root, not a system account).
pub fn owner_name(uid: u32, users: &[account::User]) -> Result<String, String> {
    if uid == 0 { return Err("That folder belongs to root. Pick a folder of a normal user (for example in /home/yourname).".into()); }
    users.iter().find(|u| u.uid == uid).map(|u| u.name.clone()).ok_or_else(|| "That folder belongs to a user this computer does not know.".to_string())
}

pub fn set_folder(ctx: &Ctx, path: &str) -> Result<String, String> {
    if !allowed_folder(path) { return Err("Pick a folder under /home, /srv, /media, /mnt or /data (no spaces in the name; a spaceless name keeps things simple).".into()); }
    let real = ctx.root.join(path.trim_start_matches('/'));
    let md = std::fs::metadata(&real).map_err(|_| "That folder does not exist.".to_string())?;
    if !md.is_dir() { return Err("That is not a folder.".into()); }
    let user = owner_name(md.uid(), &account::users(ctx.root))?;
    let mut c = load_config(ctx);
    c.folder = path.into(); c.user = user.clone();
    save_config(ctx, &c)?;
    restart_if_on(ctx);
    Ok(format!("The gallery shows {path} (it runs as {user})."))
}

pub fn set_password(ctx: &Ctx, pw: &str) -> Result<String, String> {
    if pw.chars().count() < 8 { return Err("The password needs at least 8 characters.".into()); }
    if pw.len() > 200 || pw.contains(['\n', '\r', '\0']) { return Err("That password cannot be used.".into()); }
    let mut c = load_config(ctx);
    c.hash = crypt::hash_password(pw);
    save_config(ctx, &c)?;
    restart_if_on(ctx);
    Ok("The gallery password is saved.".into())
}

pub fn set_uploads(ctx: &Ctx, on: bool) -> Result<String, String> {
    let mut c = load_config(ctx); c.uploads = on; save_config(ctx, &c)?; restart_if_on(ctx);
    Ok(if on { "People with the password can now add pictures and videos from the browser.".into() } else { "Uploading from the browser is off.".into() })
}

pub fn enabled(ctx: &Ctx) -> bool { ctx.root.join("etc/exec85/services").join(format!("{SERVICE}.conf")).exists() && ctx.root.join("etc/exec85/services").join(format!("{TLS}.conf")).exists() }
fn restart_if_on(ctx: &Ctx) { if enabled(ctx) { let _ = helper::handle(ctx, &["svc-restart".to_string(), SERVICE.to_string()]); } }

/// `enabled=`, `folder=`, `user=`, `uploads=`, `password=set|none`, `port=`, `fingerprint=`.
pub fn status(ctx: &Ctx) -> String {
    let c = load_config(ctx);
    format!("enabled={}\nfolder={}\nuser={}\nuploads={}\npassword={}\nport={PUBLIC_PORT}\nfingerprint={}\n", if enabled(ctx) { "yes" } else { "no" }, if c.folder.is_empty() { "-" } else { &c.folder }, if c.user.is_empty() { "-" } else { &c.user },
        if c.uploads { "yes" } else { "no" }, if c.hash.is_empty() { "none" } else { "set" }, crate::webaccess::fingerprint(ctx).unwrap_or_else(|| "-".into()))
}

fn def(name: &str, description: &str, command: &str, ports: &str) -> ServiceDef {
    ServiceDef { name: name.into(), description: description.into(), command: command.into(), dir: "/".into(), user: "root".into(), env: vec![], autostart: true, ports: ports.into() }
}

pub fn enable(ctx: &Ctx) -> Result<String, String> {
    let c = load_config(ctx);
    if c.folder.is_empty() { return Err("Choose the folder first.".into()); }
    if c.hash.is_empty() { return Err("Set a password first.".into()); }
    crate::webaccess::prepare_tls(ctx)?;
    std::fs::create_dir_all(ctx.root.join(DIR)).map_err(|e| e.to_string())?;
    std::fs::write(ctx.root.join(DIR).join("stunnel.conf"), crate::webaccess::conf_text_in("/etc/exec85/webaccess", PUBLIC_PORT, LISTEN_PORT, &format!("/var/log/exec85/{TLS}.stunnel.log"))).map_err(|e| e.to_string())?;
    for d in [def(SERVICE, "Exec85 photo and video gallery", "/usr/bin/exec85 --galleryd", ""), def(TLS, "Exec85 photo and video gallery, encryption", &format!("stunnel /{DIR}/stunnel.conf"), &format!("{PUBLIC_PORT}/tcp"))] {
        helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&d)])?;
    }
    for n in [SERVICE, TLS] { helper::handle(ctx, &["svc-restart".to_string(), n.to_string()])?; }
    Ok(format!("The gallery is on. Open https://<this computer's address>:{PUBLIC_PORT} in a browser."))
}

pub fn disable(ctx: &Ctx) -> Result<String, String> {
    for n in [TLS, SERVICE] {
        if ctx.root.join("etc/exec85/services").join(format!("{n}.conf")).exists() { let _ = helper::handle(ctx, &["svc-stop".to_string(), n.to_string()]); helper::handle(ctx, &["svc-remove".to_string(), n.to_string()])?; }
    }
    Ok("The gallery is off.".into())
}

// ---- the web server --------------------------------------------------------------------------------------------

struct Server { root: PathBuf, cfg: Config, sessions: Mutex<HashMap<String, Instant>>, limiter: Mutex<Limiter>, connections: AtomicUsize }

fn reply(out: &mut impl Write, status: &str, ctype: &str, body: &[u8], extra: &str) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; media-src 'self'; form-action 'none'; frame-ancestors 'none'\r\n{extra}Connection: close\r\n\r\n", body.len());
    let _ = out.write_all(head.as_bytes()); let _ = out.write_all(body);
}
fn json(out: &mut impl Write, status: &str, body: &str, extra: &str) { reply(out, status, "application/json", body.as_bytes(), extra); }
fn fail(out: &mut impl Write, status: &str, msg: &str) { json(out, status, &format!("{{\"ok\":false,\"error\":{}}}", remote::json_str(msg)), ""); }

impl Server {
    fn session_ok(&self, req: &Request) -> bool {
        let Some(t) = req.cookie("gsid") else { return false };
        let mut s = self.sessions.lock().unwrap();
        s.retain(|_, born| born.elapsed() < SESSION_LIFETIME);
        s.contains_key(&t)
    }
}

fn handle(srv: &Arc<Server>, conn: TcpStream) -> io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(60)))?;
    conn.set_nodelay(true).ok();
    let peer = conn.peer_addr().map(|a| a.ip()).ok();
    let mut out = conn.try_clone()?;
    let mut reader = BufReader::new(conn);
    let mut ip = peer.filter(|p| !p.is_loopback());
    if peer.map_or(false, |p| p.is_loopback()) {
        if reader.fill_buf()?.starts_with(b"PROXY ") {
            let mut line = String::new();
            reader.by_ref().take(200).read_line(&mut line)?;
            if let Some(real) = remote::parse_proxy_line(&line) { ip = Some(real); }
        }
    }
    let req = match remote::read_head(&mut reader) { Ok(r) => r, Err(_) => { reply(&mut out, "400 Bad Request", "text/plain", b"bad request", ""); return Ok(()); } };
    let rel_of = |prefix: &str| remote::url_decode(req.path.strip_prefix(prefix).unwrap_or(""));
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => reply(&mut out, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes(), ""),
        ("POST", "/api/login") => {
            if req.header("x-exec85") != Some("1") { return Ok(fail(&mut out, "403 Forbidden", "Missing header")); }
            let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).unwrap_or(0);
            if len > 1024 { return Ok(fail(&mut out, "400 Bad Request", "Bad request")); }
            let mut b = vec![0u8; len]; reader.read_exact(&mut b)?;
            let pw = remote::parse_form(&String::from_utf8_lossy(&b)).remove("pass").unwrap_or_default();
            let keys: Vec<String> = ip.map(|i| vec![format!("ip:{i}"), "all".to_string()]).unwrap_or_else(|| vec!["all".to_string()]);
            if !srv.limiter.lock().unwrap().allowed(&keys, Instant::now()) { return Ok(fail(&mut out, "429 Too Many Requests", "Too many wrong passwords. Wait five minutes.")); }
            if !srv.cfg.hash.is_empty() && crypt::verify(&pw, &srv.cfg.hash) {
                srv.limiter.lock().unwrap().succeeded(&keys);
                let t = remote::random_token();
                srv.sessions.lock().unwrap().insert(t.clone(), Instant::now());
                json(&mut out, "200 OK", &format!("{{\"ok\":true,\"uploads\":{}}}", srv.cfg.uploads), &format!("Set-Cookie: gsid={t}; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=86400\r\n"));
            } else {
                srv.limiter.lock().unwrap().failed(&keys, Instant::now());
                thread::sleep(Duration::from_millis(1000));
                fail(&mut out, "401 Unauthorized", "Wrong password");
            }
        }
        (_, p) if p.starts_with("/api/") || p.starts_with("/media/") => {
            if !srv.session_ok(&req) { return Ok(fail(&mut out, "401 Unauthorized", "Please log in")); }
            if req.method == "POST" && req.header("x-exec85") != Some("1") { return Ok(fail(&mut out, "403 Forbidden", "Missing header")); }
            match (req.method.as_str(), req.path.split('?').next().unwrap_or("")) {
                ("GET", "/api/list") => {
                    let dir = req.query.get("dir").cloned().unwrap_or_default();
                    match list(&srv.root, &dir) {
                        Some(es) => {
                            let items: Vec<String> = es.iter().map(|e| format!("{{\"n\":{},\"k\":\"{}\",\"s\":{},\"t\":{}}}", remote::json_str(&e.name), e.kind, e.size, e.mtime)).collect();
                            json(&mut out, "200 OK", &format!("{{\"ok\":true,\"dir\":{},\"uploads\":{},\"items\":[{}]}}", remote::json_str(&dir.trim_matches('/')), srv.cfg.uploads, items.join(",")), "");
                        }
                        None => fail(&mut out, "404 Not Found", "No such folder"),
                    }
                }
                ("GET", _) if req.path.starts_with("/media/") => {
                    let rel = rel_of("/media/"); let rel = rel.split('?').next().unwrap_or("");
                    let (Some(path), Some((_, ctype))) = (resolve(&srv.root, rel), media_kind(rel)) else { return Ok(fail(&mut out, "404 Not Found", "Not found")); };
                    if !path.is_file() { return Ok(fail(&mut out, "404 Not Found", "Not found")); }
                    serve_file(&mut out, &req, &path, ctype)?;
                }
                ("POST", "/api/upload") => upload(srv, &req, &mut reader, &mut out),
                ("POST", "/api/logout") => { if let Some(t) = req.cookie("gsid") { srv.sessions.lock().unwrap().remove(&t); } json(&mut out, "200 OK", "{\"ok\":true}", "Set-Cookie: gsid=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict\r\n"); }
                _ => fail(&mut out, "404 Not Found", "No such thing"),
            }
        }
        _ => reply(&mut out, "404 Not Found", "text/plain", b"not found", ""),
    }
    Ok(())
}

fn serve_file(out: &mut TcpStream, req: &Request, path: &Path, ctype: &str) -> io::Result<()> {
    let mut f = std::fs::File::open(path)?;
    let size = f.metadata()?.len();
    let (status, start, end, extra) = match req.header("range") {
        Some(h) => match parse_range(h, size) {
            Some((a, b)) => ("206 Partial Content", a, b, format!("Content-Range: bytes {a}-{b}/{size}\r\n")),
            None => { let head = format!("HTTP/1.1 416 Range Not Satisfiable\r\nContent-Range: bytes */{size}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"); return out.write_all(head.as_bytes()); }
        },
        None => ("200 OK", 0, size.saturating_sub(1), String::new()),
    };
    let len = if size == 0 { 0 } else { end - start + 1 };
    out.set_write_timeout(Some(Duration::from_secs(60)))?;
    write!(out, "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n{extra}Cache-Control: private, max-age=3600\r\nX-Content-Type-Options: nosniff\r\nContent-Disposition: inline\r\nConnection: close\r\n\r\n")?;
    f.seek(SeekFrom::Start(start))?;
    io::copy(&mut f.take(len), out)?;
    Ok(())
}

fn upload(srv: &Arc<Server>, req: &Request, reader: &mut impl Read, out: &mut impl Write) {
    if !srv.cfg.uploads { return fail(out, "403 Forbidden", "Uploading is switched off"); }
    let (Some(dir), Some(name)) = (req.query.get("dir"), req.query.get("name").filter(|n| valid_upload_name(n))) else { return fail(out, "400 Bad Request", "Give a folder and a picture or video name"); };
    let Some(len) = req.header("content-length").and_then(|v| v.parse::<u64>().ok()) else { return fail(out, "411 Length Required", "Length required"); };
    if len > MAX_UPLOAD { return fail(out, "413 Payload Too Large", "That file is too large"); }
    let Some(dirp) = resolve(&srv.root, dir) else { return fail(out, "404 Not Found", "No such folder"); };
    if !dirp.is_dir() { return fail(out, "404 Not Found", "No such folder"); }
    let mut final_name = name.clone(); let mut n = 1;
    while dirp.join(&final_name).exists() { n += 1; final_name = match name.rsplit_once('.') { Some((stem, ext)) => format!("{stem} ({n}).{ext}"), None => format!("{name} ({n})") }; }
    let tmp = dirp.join(format!(".{final_name}.part"));
    let result = (|| -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let got = io::copy(&mut reader.take(len), &mut f)?;
        if got != len { return Err(io::Error::other("cut short")); }
        f.sync_all()?;
        std::fs::rename(&tmp, dirp.join(&final_name))
    })();
    match result {
        Ok(()) => json(out, "200 OK", &format!("{{\"ok\":true,\"name\":{}}}", remote::json_str(&final_name)), ""),
        Err(e) => { let _ = std::fs::remove_file(&tmp); fail(out, "500 Internal Server Error", &e.to_string()) }
    }
}

fn drop_privileges(user: &account::User) -> Result<(), String> {
    let groups = account::group_ids(Path::new("/"), user);
    // SAFETY: plain system calls; the order matters (groups and group first, user last).
    unsafe {
        if crate::sys::setgroups(groups.len(), groups.as_ptr()) != 0 || crate::sys::setgid(user.gid) != 0 || crate::sys::setuid(user.uid) != 0 { return Err(io::Error::last_os_error().to_string()); }
    }
    Ok(())
}

/// `exec85 --galleryd`
pub fn run() -> ! {
    let ctx = Ctx { root: Path::new("/"), runner: &helper::SysRunner };
    let cfg = load_config(&ctx);
    let Ok(root) = std::fs::canonicalize(&cfg.folder) else { eprintln!("the gallery folder {:?} does not exist", cfg.folder); std::process::exit(1) };
    let listener = match TcpListener::bind(("127.0.0.1", LISTEN_PORT)) { Ok(l) => l, Err(e) => { eprintln!("cannot listen on 127.0.0.1:{LISTEN_PORT}: {e}"); std::process::exit(1) } };
    // from here on never root: the folder's owner, who can read exactly what they could read at the keyboard
    if crate::sys::uid() == 0 {
        let Some(user) = account::users(Path::new("/")).into_iter().find(|u| u.name == cfg.user && u.uid != 0) else { eprintln!("the gallery user {:?} is not a normal user", cfg.user); std::process::exit(1) };
        if let Err(e) = drop_privileges(&user) { eprintln!("cannot drop root rights: {e}"); std::process::exit(1); }
    }
    serve(listener, root, cfg)
}

fn serve(listener: TcpListener, root: PathBuf, cfg: Config) -> ! {
    let srv = Arc::new(Server { root, cfg, sessions: Mutex::new(HashMap::new()), limiter: Mutex::new(Limiter::default()), connections: AtomicUsize::new(0) });
    for conn in listener.incoming().flatten() {
        let s = srv.clone();
        thread::spawn(move || {
            if s.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS { s.connections.fetch_sub(1, Ordering::SeqCst); return; }
            let closer = conn.try_clone().ok();
            let _ = handle(&s, conn);
            if let Some(c) = closer { remote::close_politely(c); }
            s.connections.fetch_sub(1, Ordering::SeqCst);
        });
    }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::SysRunner;

    fn tmp(tag: &str) -> PathBuf { let d = std::env::temp_dir().join(format!("exec85-gallery-{tag}-{}", std::process::id())); let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d }

    /// root/gal/{a.png, B.JPG, movie.mp4, notes.txt, .hidden.png, trip/{c.webp, deep/d.gif}, leak.png -> ../outside.txt, out -> ../outsidedir}, root/outside.txt, root/outsidedir/x.png
    fn world(tag: &str) -> (PathBuf, PathBuf) {
        let base = tmp(tag);
        let g = base.join("gal"); std::fs::create_dir_all(g.join("trip/deep")).unwrap(); std::fs::create_dir_all(base.join("outsidedir")).unwrap();
        for (f, body) in [("a.png", "PNGDATA"), ("B.JPG", "JPGDATA"), ("movie.mp4", "0123456789MOVIE"), ("notes.txt", "secret notes"), (".hidden.png", "h"), ("trip/c.webp", "WEBP"), ("trip/deep/d.gif", "GIF")] { std::fs::write(g.join(f), body).unwrap(); }
        std::fs::write(base.join("outside.txt"), "outside").unwrap(); std::fs::write(base.join("outsidedir/x.png"), "X").unwrap();
        std::os::unix::fs::symlink("../outside.txt", g.join("leak.png")).unwrap();
        std::os::unix::fs::symlink("../outsidedir", g.join("out")).unwrap();
        (base.clone(), std::fs::canonicalize(&g).unwrap())
    }

    #[test]
    fn only_pictures_and_videos_count_and_svg_does_not() {
        assert_eq!(media_kind("a.PNG"), Some((Kind::Image, "image/png")));
        assert_eq!(media_kind("x.mov").map(|m| m.0), Some(Kind::Video));
        assert_eq!(media_kind("clip.webm").map(|m| m.1), Some("video/webm"));
        for no in ["notes.txt", "x.svg", "x.html", "x.php", "x", "x.png.exe", "x.pdf", "x.js"] { assert!(media_kind(no).is_none(), "{no}"); }
        assert!(media_kind("x.svg").is_none() && media_kind("a.html").is_none() && media_kind("a").is_none());
    }

    #[test]
    fn a_path_is_only_ever_inside_the_gallery_folder() {
        let (base, root) = world("paths");
        assert!(resolve(&root, "").is_some() && resolve(&root, "trip").is_some() && resolve(&root, "trip/deep/d.gif").is_some() && resolve(&root, "/trip").is_none());
        for bad in ["..", "../outside.txt", "trip/../a.png", "trip/../../outside.txt", ".hidden.png", "trip/./c.webp", "leak.png", "out", "out/x.png", "a\0.png", "/etc/passwd", &"a/".repeat(600)] { assert!(resolve(&root, bad).is_none(), "{bad:?}"); }
        assert!(resolve(&root, "missing.png").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_list_shows_folders_pictures_and_videos_and_nothing_else() {
        let (base, root) = world("list");
        let names = |rel: &str| list(&root, rel).unwrap().iter().map(|e| format!("{}:{}", e.kind, e.name)).collect::<Vec<_>>();
        assert_eq!(names(""), vec!["dir:trip", "image:a.png", "image:B.JPG", "video:movie.mp4"], "folders first; no notes, no hidden file, no links that lead out");
        assert_eq!(names("trip"), vec!["dir:deep", "image:c.webp"]);
        assert_eq!(list(&root, "").unwrap()[3].size, 15);
        assert!(list(&root, "nope").is_none() && list(&root, "a.png").is_none() && list(&root, "..").is_none() && list(&root, "out").is_none());
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn ranges_for_skipping_through_a_video() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=900-", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        assert_eq!(parse_range("bytes=990-5000", 1000), Some((990, 999)), "an end past the file is cut");
        for bad in ["bytes=1000-", "bytes=5-2", "bytes=0-1,5-6", "items=0-1", "bytes=-0", "bytes=a-b", "bytes=", ""] { assert_eq!(parse_range(bad, 1000), None, "{bad:?}"); }
        assert_eq!(parse_range("bytes=0-1", 0), None);
    }

    #[test]
    fn the_folder_the_user_and_the_password_are_checked_when_saved() {
        for ok in ["/home/tester/Pictures", "/srv/photos", "/media/disk1/pics", "/mnt/nas/family"] { assert!(allowed_folder(ok), "{ok}"); }
        for bad in ["/", "/etc", "/home/", "/root/x", "/home/a b", "/home/../etc", "relative", "/home/x/", "/proc/1", "/home/x;rm", "/home/x$y", "/home/x'y", &format!("/home/{}", "a".repeat(300))] { assert!(!allowed_folder(bad), "{bad:?}"); }
        let users = vec![account::User { name: "tester".into(), uid: 1000, gid: 1000, home: "/home/tester".into(), shell: "/bin/ash".into() }];
        assert_eq!(owner_name(1000, &users).unwrap(), "tester");
        assert!(owner_name(0, &users).unwrap_err().contains("root") && owner_name(4321, &users).is_err());
        let base = tmp("setup"); std::fs::create_dir_all(base.join("etc")).unwrap(); std::fs::create_dir_all(base.join("home/tester/pics")).unwrap();
        let me = crate::sys::uid();
        let uid = if me == 0 { std::os::unix::fs::chown(base.join("home/tester/pics"), Some(1000), Some(1000)).unwrap(); 1000 } else { me };
        std::fs::write(base.join("etc/passwd"), format!("root:x:0:0::/root:/bin/ash\ntester:x:{uid}:{uid}::/home/tester:/bin/ash\n")).unwrap();
        std::fs::write(base.join("etc/group"), "wheel:x:10:\n").unwrap();
        let c = Ctx { root: &base, runner: &SysRunner };
        assert!(set_folder(&c, "/home/tester/missing").is_err() && set_folder(&c, "/etc").is_err());
        assert_eq!(set_folder(&c, "/home/tester/pics").unwrap(), "The gallery shows /home/tester/pics (it runs as tester).");
        assert!(set_password(&c, "short").is_err() && set_password(&c, "has\nnewline123").is_err());
        assert!(enable(&c).unwrap_err().contains("password"), "no password, no gallery");
        set_password(&c, "correct horse 1").unwrap();
        let cfg = load_config(&c);
        assert!(crypt::verify("correct horse 1", &cfg.hash) && !crypt::verify("wrong", &cfg.hash) && cfg.hash.starts_with("$6$") && cfg.folder == "/home/tester/pics" && cfg.user == "tester" && !cfg.uploads);
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(conf_path(&c)).unwrap().permissions().mode() & 0o777, 0o600, "the hash is private");
        }
        set_uploads(&c, true).unwrap(); assert!(load_config(&c).uploads);
        let t = status(&c);
        assert!(t.contains("enabled=no\n") && t.contains("folder=/home/tester/pics\n") && t.contains("password=set\n") && t.contains("uploads=yes\n") && !t.contains("$6$"), "{t}");
        let _ = std::fs::remove_dir_all(&base);
    }

    // ---- the whole server, on a real port ----

    fn start(root: PathBuf, uploads: bool) -> std::net::SocketAddr {
        let cfg = Config { folder: root.to_string_lossy().into(), user: String::new(), uploads, hash: crypt::hash_password("open sesame 1") };
        let l = TcpListener::bind("127.0.0.1:0").unwrap(); let addr = l.local_addr().unwrap();
        thread::spawn(move || serve(l, root, cfg));
        addr
    }
    fn http(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> (String, Vec<u8>) { http_at(addr, head, body, false) }
    /// `hang_up`: close the sending side after the body (a connection that drops in the middle of an upload).
    fn http_at(addr: std::net::SocketAddr, head: &str, body: &[u8], hang_up: bool) -> (String, Vec<u8>) {
        let mut c = TcpStream::connect(addr).unwrap(); c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(head.as_bytes()).unwrap(); c.write_all(body).unwrap();
        if hang_up { c.shutdown(std::net::Shutdown::Write).unwrap(); }
        let mut all = Vec::new(); let _ = c.read_to_end(&mut all);
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(all.len());
        (String::from_utf8_lossy(&all[..at]).into_owned(), all[(at + 4).min(all.len())..].to_vec())
    }
    fn login(addr: std::net::SocketAddr, pw: &str) -> (String, String) {
        let body = format!("pass={pw}");
        let (head, _) = http(addr, &format!("POST /api/login HTTP/1.1\r\nHost: x\r\nX-Exec85: 1\r\nContent-Length: {}\r\n\r\n", body.len()), body.as_bytes());
        let cookie = head.lines().find(|l| l.starts_with("Set-Cookie: gsid=")).map(|l| l["Set-Cookie: ".len()..].split(';').next().unwrap().to_string()).unwrap_or_default();
        (head, cookie)
    }
    fn get(addr: std::net::SocketAddr, path: &str, ck: &str, extra: &str) -> (String, Vec<u8>) { http(addr, &format!("GET {path} HTTP/1.1\r\nHost: x\r\nCookie: {ck}\r\n{extra}\r\n"), b"") }

    #[test]
    fn the_server_shows_only_what_was_chosen_to_people_with_the_password() {
        let (base, root) = world("server");
        let addr = start(root.clone(), false);
        assert!(http(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n", b"").0.contains("200 OK"));
        for p in ["/api/list", "/media/a.png"] { assert!(get(addr, p, "gsid=nothing", "").0.starts_with("HTTP/1.1 401"), "{p}: nothing without a login"); }
        assert!(login(addr, "wrong").0.starts_with("HTTP/1.1 401"));
        let (h, _) = http(addr, "POST /api/login HTTP/1.1\r\nHost: x\r\nContent-Length: 4\r\n\r\npass", b""); assert!(h.starts_with("HTTP/1.1 403"), "no login without the protective header");
        let (head, ck) = login(addr, "open%20sesame%201");
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("HttpOnly") && head.contains("Secure") && head.contains("SameSite=Strict"), "{head}");
        let (h, b) = get(addr, "/api/list?dir=", &ck, "");
        let list_json = String::from_utf8_lossy(&b).to_string();
        assert!(h.starts_with("HTTP/1.1 200") && list_json.contains("\"a.png\"") && list_json.contains("\"movie.mp4\"") && list_json.contains("\"trip\"") && list_json.contains("\"uploads\":false"), "{list_json}");
        for hidden in ["notes.txt", "leak.png", "\"out\"", ".hidden", "outside"] { assert!(!list_json.contains(hidden), "{hidden} must not be listed: {list_json}"); }
        let (h, b) = get(addr, "/media/a.png", &ck, ""); assert!(h.starts_with("HTTP/1.1 200") && h.contains("Content-Type: image/png") && h.contains("nosniff") && h.contains("Accept-Ranges: bytes") && b == b"PNGDATA");
        assert_eq!(get(addr, "/media/trip/deep/d.gif", &ck, "").1, b"GIF");
        let (h, b) = get(addr, "/media/movie.mp4", &ck, "Range: bytes=2-5\r\n"); assert!(h.starts_with("HTTP/1.1 206") && h.contains("Content-Range: bytes 2-5/15") && b == b"2345", "{h}");
        assert_eq!(get(addr, "/media/movie.mp4", &ck, "Range: bytes=-5\r\n").1, b"MOVIE");
        assert!(get(addr, "/media/movie.mp4", &ck, "Range: bytes=500-\r\n").0.starts_with("HTTP/1.1 416"));
        for bad in ["/media/notes.txt", "/media/leak.png", "/media/out/x.png", "/media/..%2foutside.txt", "/media/%2e%2e/outside.txt", "/media/trip/..%2fa.png", "/media/.hidden.png", "/media//etc/passwd", "/media/missing.png", "/media/trip", "/api/list?dir=..", "/api/list?dir=out"] {
            let (h, b) = get(addr, bad, &ck, ""); assert!(h.starts_with("HTTP/1.1 404"), "{bad}: {h}"); assert!(!String::from_utf8_lossy(&b).contains("outside") || b.starts_with(b"{\"ok\":false"), "{bad} leaked");
        }
        // uploading is off
        let (h, _) = http(addr, &format!("POST /api/upload?dir=&name=n.png HTTP/1.1\r\nHost: x\r\nX-Exec85: 1\r\nCookie: {ck}\r\nContent-Length: 1\r\n\r\n"), b"x"); assert!(h.starts_with("HTTP/1.1 403"), "{h}");
        let (h, _) = http(addr, &format!("POST /api/logout HTTP/1.1\r\nHost: x\r\nX-Exec85: 1\r\nCookie: {ck}\r\nContent-Length: 0\r\n\r\n"), b""); assert!(h.starts_with("HTTP/1.1 200"));
        assert!(get(addr, "/api/list", &ck, "").0.starts_with("HTTP/1.1 401"), "logged out");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn uploads_when_switched_on_only_take_pictures_and_videos_and_never_overwrite() {
        let (base, root) = world("upload");
        let addr = start(root.clone(), true);
        let (_, ck) = login(addr, "open%20sesame%201");
        let up = |dir: &str, name: &str, body: &[u8]| http(addr, &format!("POST /api/upload?dir={dir}&name={name} HTTP/1.1\r\nHost: x\r\nX-Exec85: 1\r\nCookie: {ck}\r\nContent-Length: {}\r\n\r\n", body.len()), body);
        let (h, b) = up("", "new.png", b"NEWPNG"); assert!(h.starts_with("HTTP/1.1 200") && String::from_utf8_lossy(&b).contains("\"new.png\""), "{h}");
        assert_eq!(std::fs::read(root.join("new.png")).unwrap(), b"NEWPNG");
        let (_, b) = up("", "a.png", b"OVERWRITE"); assert!(String::from_utf8_lossy(&b).contains("a (2).png"));
        assert_eq!(std::fs::read(root.join("a.png")).unwrap(), b"PNGDATA", "an existing picture is never replaced");
        assert!(up("trip", "t.webp", b"T").0.starts_with("HTTP/1.1 200") && root.join("trip/t.webp").exists());
        for (dir, name) in [("", "evil.php"), ("", "page.html"), ("", "x.svg"), ("", "..%2fesc.png"), ("", ".dot.png"), ("..", "esc.png"), ("out", "esc.png"), ("nope", "a.png"), ("", "a%22b.png")] {
            assert!(!up(dir, name, b"x").0.starts_with("HTTP/1.1 200"), "{dir:?}/{name}");
        }
        assert!(!base.join("esc.png").exists() && !base.join("outsidedir/esc.png").exists() && !root.join("evil.php").exists());
        let (h, _) = http_at(addr, &format!("POST /api/upload?dir=&name=cut.png HTTP/1.1\r\nHost: x\r\nX-Exec85: 1\r\nCookie: {ck}\r\nContent-Length: 100\r\n\r\n"), b"short", true);
        assert!(!h.starts_with("HTTP/1.1 200") && !root.join("cut.png").exists() && !root.join(".cut.png.part").exists(), "{h}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn five_wrong_passwords_lock_the_login() {
        let (base, root) = world("lock");
        let addr = start(root, false);
        for _ in 0..5 { assert!(login(addr, "bad").0.starts_with("HTTP/1.1 401")); }
        assert!(login(addr, "open%20sesame%201").0.starts_with("HTTP/1.1 429"), "even the right password waits");
        let _ = std::fs::remove_dir_all(&base);
    }
}
