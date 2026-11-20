//! Remote access through a web browser: the whole Exec85 screen, plus file upload and download.
//!
//! `exec85 --remote` listens on 127.0.0.1 only. The encryption (HTTPS) is done by `stunnel` in front of it, which
//! listens on the network. A person logs in with their Exec85 user name and password (never as root), and gets
//! their own session: a copy of the shell running as them (`rsession`), whose pictures are streamed to the page.
//! Files are read and written by a small helper program that runs as the same person, so they can only reach what
//! they could reach at the keyboard.
use crate::account::{self, AuthError, User};
use crate::rframe;
use std::collections::HashMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const LISTEN_PORT: u16 = 8442;
pub const PUBLIC_PORT: u16 = 8443;
const PAGE: &str = include_str!("remote_page.html");
const MAX_SESSIONS: usize = 4;
const MAX_CONNECTIONS: usize = 64;
const IDLE_LIMIT: Duration = Duration::from_secs(30 * 60);
const TOKEN_LIFETIME: Duration = Duration::from_secs(12 * 3600);
const MAX_UPLOAD: u64 = 16 << 30;
const LOGIN_FAILURES: usize = 5;
const LOGIN_WINDOW: Duration = Duration::from_secs(600);
const LOGIN_LOCK: Duration = Duration::from_secs(300);

// ---- the pieces that need no network ------------------------------------------------------------------------

pub struct Request { pub method: String, pub path: String, pub query: HashMap<String, String>, pub headers: HashMap<String, String> }

impl Request {
    pub fn header(&self, name: &str) -> Option<&str> { self.headers.get(name).map(String::as_str) }
    pub fn cookie(&self, name: &str) -> Option<String> {
        self.header("cookie")?.split(';').filter_map(|c| c.trim().split_once('=')).find(|(k, _)| *k == name).map(|(_, v)| v.to_string())
    }
}

pub fn url_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => { out.push(b' '); i += 1; }
            b'%' if i + 2 < b.len() && b[i + 1].is_ascii_hexdigit() && b[i + 2].is_ascii_hexdigit() => { out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap_or(b'?')); i += 3; }
            c => { out.push(c); i += 1; }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn parse_form(s: &str) -> HashMap<String, String> {
    s.split('&').filter(|p| !p.is_empty()).map(|p| match p.split_once('=') { Some((k, v)) => (url_decode(k), url_decode(v)), None => (url_decode(p), String::new()) }).collect()
}

/// Read the request line and headers (a body, if any, stays in the reader).
pub fn read_head(r: &mut impl BufRead) -> Result<Request, &'static str> {
    let mut line = String::new();
    let mut get = |r: &mut dyn BufRead, line: &mut String| -> Result<bool, &'static str> {
        line.clear();
        let n = r.take(8192).read_line(line).map_err(|_| "unreadable")?;
        if n == 0 { return Ok(false); }
        if !line.ends_with('\n') { return Err("line too long"); }
        Ok(true)
    };
    if !get(r, &mut line)? { return Err("empty"); }
    let first = line.trim_end().to_string();
    let mut it = first.split(' ');
    let (method, target, version) = (it.next().unwrap_or(""), it.next().unwrap_or(""), it.next().unwrap_or(""));
    if !version.starts_with("HTTP/1.") || !target.starts_with('/') || !["GET", "POST"].contains(&method) { return Err("not a request we serve"); }
    let (path, query) = match target.split_once('?') { Some((p, q)) => (p.to_string(), parse_form(q)), None => (target.to_string(), HashMap::new()) };
    let mut headers = HashMap::new();
    for _ in 0..64 {
        if !get(r, &mut line)? { return Err("cut short"); }
        let l = line.trim_end();
        if l.is_empty() { return Ok(Request { method: method.into(), path, query, headers }); }
        if let Some((k, v)) = l.split_once(':') { headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string()); }
    }
    Err("too many headers")
}

/// `PROXY TCP4 1.2.3.4 ...` (sent by stunnel so that the real address of the browser is known).
pub fn parse_proxy_line(line: &str) -> Option<IpAddr> {
    let mut it = line.trim_end().split(' ');
    if it.next()? != "PROXY" { return None; }
    it.next()?;
    it.next()?.parse().ok()
}

/// Wrong passwords: five in ten minutes lock that user name, and that address, for five minutes.
#[derive(Default)]
pub struct Limiter { fails: HashMap<String, Vec<Instant>>, locked: HashMap<String, Instant> }

impl Limiter {
    pub fn allowed(&mut self, keys: &[String], now: Instant) -> bool {
        keys.iter().all(|k| self.locked.get(k).map_or(true, |until| now >= *until))
    }
    pub fn failed(&mut self, keys: &[String], now: Instant) {
        for k in keys {
            let list = self.fails.entry(k.clone()).or_default();
            list.retain(|t| now.duration_since(*t) < LOGIN_WINDOW);
            list.push(now);
            if list.len() >= LOGIN_FAILURES { self.locked.insert(k.clone(), now + LOGIN_LOCK); list.clear(); }
        }
        if self.fails.len() > 2000 { self.fails.clear(); }
    }
    pub fn succeeded(&mut self, keys: &[String]) { for k in keys { self.fails.remove(k); } }
}

pub fn valid_file_name(n: &str) -> bool {
    !n.is_empty() && n.len() <= 200 && !n.starts_with('.') && !n.contains(['/', '\0', '\n', '\r']) && n != ".."
}
pub fn valid_abs_path(p: &str) -> bool { p.starts_with('/') && p.len() <= 4000 && !p.contains(['\0', '\n', '\r']) }

pub(crate) fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() { match c { '"' => o.push_str("\\\""), '\\' => o.push_str("\\\\"), c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)), c => o.push(c) } }
    o.push('"'); o
}

pub(crate) fn random_token() -> String {
    let mut b = [0u8; 32];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") { let _ = f.read_exact(&mut b); }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// ---- what the server needs from the system (replaced in tests) ----------------------------------------------

pub trait System: Send + Sync {
    fn auth(&self, name: &str, password: &str) -> Result<User, AuthError>;
    /// Start `exec85 ARGS...` as that person with the given standard input and output.
    fn spawn(&self, user: &User, args: &[String], stdin: Stdio, stdout: Stdio) -> io::Result<Child>;
    fn power(&self, code: i32);
}

pub struct Real;
impl System for Real {
    fn auth(&self, name: &str, password: &str) -> Result<User, AuthError> {
        let u = account::authenticate(Path::new("/"), name, password)?;
        if u.uid == 0 { return Err(AuthError::Locked); }       // never as root
        Ok(u)
    }
    fn spawn(&self, user: &User, args: &[String], stdin: Stdio, stdout: Stdio) -> io::Result<Child> {
        let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("/usr/bin/exec85"));
        crate::supervisor::user_command(&exe, user).args(args).stdin(stdin).stdout(stdout).stderr(Stdio::null()).spawn()
    }
    fn power(&self, code: i32) { crate::supervisor::power_now(code); }
}

// ---- sessions -----------------------------------------------------------------------------------------------

struct Live {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    sub: Mutex<Option<SyncSender<Vec<u8>>>>,
    need_full: AtomicBool,
    state: Mutex<String>,
    over: AtomicBool,
}

struct Session { user: User, created: Instant, last_key: Mutex<Instant>, live: Mutex<Option<Arc<Live>>> }

pub struct Server { sys: Box<dyn System>, sessions: Mutex<HashMap<String, Arc<Session>>>, limiter: Mutex<Limiter>, connections: AtomicUsize }

impl Server {
    pub fn new(sys: Box<dyn System>) -> Arc<Server> {
        Arc::new(Server { sys, sessions: Mutex::new(HashMap::new()), limiter: Mutex::new(Limiter::default()), connections: AtomicUsize::new(0) })
    }

    fn session(&self, req: &Request) -> Option<Arc<Session>> {
        let token = req.cookie("sid")?;
        let s = self.sessions.lock().unwrap().get(&token).cloned()?;
        if s.created.elapsed() > TOKEN_LIFETIME { self.end(&token); return None; }
        Some(s)
    }

    fn end(&self, token: &str) {
        let s = self.sessions.lock().unwrap().remove(token);
        if let Some(s) = s { if let Some(l) = s.live.lock().unwrap().take() { kill(&l); } }
    }

    /// Sessions nobody has typed into for half an hour are closed.
    pub fn sweep(&self) {
        let old: Vec<String> = self.sessions.lock().unwrap().iter().filter(|(_, s)| s.last_key.lock().unwrap().elapsed() > IDLE_LIMIT || s.created.elapsed() > TOKEN_LIFETIME).map(|(k, _)| k.clone()).collect();
        for t in old { self.end(&t); }
    }

    pub fn session_count(&self) -> usize { self.sessions.lock().unwrap().len() }
}

fn kill(l: &Live) {
    l.over.store(true, Ordering::SeqCst);
    let _ = l.stdin.lock().unwrap().write_all(b"bye\n");
    let _ = l.child.lock().unwrap().kill();
    let _ = l.child.lock().unwrap().wait();
}

fn start_live(srv: &Arc<Server>, sess: &Arc<Session>, w: usize, h: usize) -> io::Result<Arc<Live>> {
    let args = vec!["--remote-session".to_string(), w.to_string(), h.to_string()];
    let mut child = srv.sys.spawn(&sess.user, &args, Stdio::piped(), Stdio::piped())?;
    let (stdin, stdout) = (child.stdin.take().ok_or_else(|| io::Error::other("no stdin"))?, child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?);
    let live = Arc::new(Live { stdin: Mutex::new(stdin), child: Mutex::new(child), sub: Mutex::new(None), need_full: AtomicBool::new(false), state: Mutex::new(String::new()), over: AtomicBool::new(false) });
    let l2 = live.clone();
    let srv2 = srv.clone();
    thread::spawn(move || {
        let mut out = BufReader::new(stdout);
        while let Ok(Some((kind, body))) = rframe::read_message(&mut out) {
            if kind == rframe::KIND_STATE { *l2.state.lock().unwrap() = String::from_utf8_lossy(&body).into_owned(); }
            let msg = rframe::message(kind, &body);
            if let Some(tx) = l2.sub.lock().unwrap().as_ref() { if tx.try_send(msg).is_err() { l2.need_full.store(true, Ordering::SeqCst); } }
        }
        // the session program ended: logout, power off or restart, or it died
        l2.over.store(true, Ordering::SeqCst);
        let code = l2.child.lock().unwrap().wait().ok().and_then(|s| s.code());
        if let Some(tx) = l2.sub.lock().unwrap().as_ref() { let _ = tx.try_send(rframe::message(rframe::KIND_BYE, b"closed")); }
        if let Some(c @ (crate::supervisor::EXIT_POWEROFF | crate::supervisor::EXIT_REBOOT)) = code { srv2.sys.power(c); }
    });
    Ok(live)
}

// ---- HTTP ---------------------------------------------------------------------------------------------------

fn reply(conn: &mut impl Write, status: &str, ctype: &str, body: &[u8], extra: &str) {
    let head = format!("HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nContent-Security-Policy: default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src data: blob:; form-action 'none'; frame-ancestors 'none'\r\n{extra}Connection: close\r\n\r\n", body.len());
    let _ = conn.write_all(head.as_bytes());
    let _ = conn.write_all(body);
}
fn json(conn: &mut impl Write, status: &str, body: &str, extra: &str) { reply(conn, status, "application/json", body.as_bytes(), extra); }
fn fail(conn: &mut impl Write, status: &str, msg: &str) { json(conn, status, &format!("{{\"ok\":false,\"error\":{}}}", json_str(msg)), ""); }

fn read_body(reader: &mut impl Read, req: &Request, max: usize) -> Result<Vec<u8>, &'static str> {
    let len: usize = req.header("content-length").and_then(|v| v.parse().ok()).ok_or("length required")?;
    if len > max { return Err("too large"); }
    let mut b = vec![0u8; len];
    reader.read_exact(&mut b).map_err(|_| "cut short")?;
    Ok(b)
}

pub fn handle_connection(srv: Arc<Server>, conn: TcpStream) {
    if srv.connections.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS { srv.connections.fetch_sub(1, Ordering::SeqCst); return; }
    let closer = conn.try_clone().ok();
    let _ = handle(&srv, conn);
    // Close politely: when a reply goes out before the request's body was read (a refusal), closing with unread bytes makes the
    // system reset the connection and the browser may lose the end of the reply. Finish sending, then read what is left.
    if let Some(c) = closer {
        use std::io::Read;
        let _ = c.shutdown(std::net::Shutdown::Write);
        let _ = c.set_read_timeout(Some(Duration::from_millis(400)));
        let mut sink = [0u8; 4096]; let mut left = 1usize << 20;
        let mut c = c;
        while left > 0 { match c.read(&mut sink) { Ok(0) | Err(_) => break, Ok(n) => left = left.saturating_sub(n) } }
    }
    srv.connections.fetch_sub(1, Ordering::SeqCst);
}

fn handle(srv: &Arc<Server>, conn: TcpStream) -> io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(30)))?;
    conn.set_nodelay(true).ok();
    let peer = conn.peer_addr().map(|a| a.ip()).ok();
    let mut out = conn.try_clone()?;
    let mut reader = BufReader::new(conn);
    // stunnel announces the real address of the browser first (only believed from this computer itself)
    // (connections from this computer without it are not told apart by address: every one would look the same)
    let mut ip = peer.filter(|p| !p.is_loopback());
    if peer.map_or(false, |p| p.is_loopback()) {
        let buf = reader.fill_buf()?;
        if buf.starts_with(b"PROXY ") {
            let mut line = String::new();
            reader.by_ref().take(200).read_line(&mut line)?;
            if let Some(real) = parse_proxy_line(&line) { ip = Some(real); }
        }
    }
    let req = match read_head(&mut reader) { Ok(r) => r, Err(_) => { reply(&mut out, "400 Bad Request", "text/plain", b"bad request", ""); return Ok(()); } };
    let ip_key = ip.map(|i| format!("ip:{i}")).unwrap_or_default();
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") => reply(&mut out, "200 OK", "text/html; charset=utf-8", PAGE.as_bytes(), ""),
        ("GET", "/favicon.ico") => reply(&mut out, "204 No Content", "text/plain", b"", ""),
        ("POST", "/api/login") => login(srv, &req, &mut reader, &mut out, &ip_key),
        (_, path) if path.starts_with("/api/") => {
            // everything else needs a session, and a POST needs the header a web page of another site cannot add
            let Some(sess) = srv.session(&req) else { fail(&mut out, "401 Unauthorized", "Not logged in"); return Ok(()); };
            if req.method == "POST" && req.header("x-exec85") != Some("1") { fail(&mut out, "403 Forbidden", "Missing header"); return Ok(()); }
            match (req.method.as_str(), path) {
                ("GET", "/api/me") => json(&mut out, "200 OK", &format!("{{\"ok\":true,\"user\":{},\"home\":{}}}", json_str(&sess.user.name), json_str(&sess.user.home)), ""),
                ("GET", "/api/state") => { let st = sess.live.lock().unwrap().as_ref().map(|l| l.state.lock().unwrap().clone()).unwrap_or_default(); reply(&mut out, "200 OK", "text/plain; charset=utf-8", st.as_bytes(), ""); }
                ("GET", "/api/stream") => stream(srv, &sess, &req, out)?,
                ("POST", "/api/key") => keys(&sess, &req, &mut reader, &mut out),
                ("POST", "/api/logout") => { if let Some(t) = req.cookie("sid") { srv.end(&t); } json(&mut out, "200 OK", "{\"ok\":true}", "Set-Cookie: sid=; Path=/; Max-Age=0; HttpOnly; Secure; SameSite=Strict\r\n"); }
                ("GET", "/api/download") => download(srv, &sess, &req, &mut out),
                ("POST", "/api/upload") => upload(srv, &sess, &req, &mut reader, &mut out),
                _ => fail(&mut out, "404 Not Found", "No such thing"),
            }
        }
        _ => reply(&mut out, "404 Not Found", "text/plain", b"not found", ""),
    }
    Ok(())
}

fn login(srv: &Arc<Server>, req: &Request, reader: &mut impl Read, out: &mut impl Write, ip_key: &str) {
    if req.header("x-exec85") != Some("1") { return fail(out, "403 Forbidden", "Missing header"); }
    let Ok(body) = read_body(reader, req, 4096) else { return fail(out, "400 Bad Request", "Bad request"); };
    let form = parse_form(&String::from_utf8_lossy(&body));
    let (name, pw) = (form.get("user").cloned().unwrap_or_default(), form.get("pass").cloned().unwrap_or_default());
    let mut keys = vec![format!("user:{name}")];
    if !ip_key.is_empty() { keys.push(ip_key.to_string()); }
    if !srv.limiter.lock().unwrap().allowed(&keys, Instant::now()) { return fail(out, "429 Too Many Requests", "Too many wrong passwords. Wait five minutes."); }
    match srv.sys.auth(&name, &pw) {
        Ok(user) if user.uid != 0 => {
            if srv.session_count() >= MAX_SESSIONS { srv.sweep(); }
            if srv.session_count() >= MAX_SESSIONS { return fail(out, "503 Service Unavailable", "Too many people are logged in."); }
            srv.limiter.lock().unwrap().succeeded(&keys);
            let token = random_token();
            let body = format!("{{\"ok\":true,\"user\":{},\"home\":{}}}", json_str(&user.name), json_str(&user.home));
            srv.sessions.lock().unwrap().insert(token.clone(), Arc::new(Session { user, created: Instant::now(), last_key: Mutex::new(Instant::now()), live: Mutex::new(None) }));
            json(out, "200 OK", &body, &format!("Set-Cookie: sid={token}; Path=/; HttpOnly; Secure; SameSite=Strict\r\n"));
        }
        _ => {
            srv.limiter.lock().unwrap().failed(&keys, Instant::now());
            thread::sleep(Duration::from_millis(1000));      // slows down guessing; every kind of failure looks the same
            fail(out, "401 Unauthorized", "Login incorrect");
        }
    }
}

fn keys(sess: &Arc<Session>, req: &Request, reader: &mut impl Read, out: &mut impl Write) {
    let Ok(body) = read_body(reader, req, 16384) else { return fail(out, "400 Bad Request", "Bad request"); };
    let text = String::from_utf8_lossy(&body);
    let Some(live) = sess.live.lock().unwrap().clone() else { return fail(out, "409 Conflict", "No screen yet"); };
    let mut data = String::new();
    for l in text.lines() { if l.starts_with("k ") || l.starts_with("t ") || l == "full" { data.push_str(l); data.push('\n'); } }
    *sess.last_key.lock().unwrap() = Instant::now();
    if live.stdin.lock().unwrap().write_all(data.as_bytes()).is_err() { return fail(out, "410 Gone", "The session has ended"); }
    json(out, "200 OK", "{\"ok\":true}", "");
}

fn stream(srv: &Arc<Server>, sess: &Arc<Session>, req: &Request, mut out: TcpStream) -> io::Result<()> {
    let num = |k: &str, d: usize| req.query.get(k).and_then(|v| v.parse().ok()).unwrap_or(d);
    let live = {
        let mut slot = sess.live.lock().unwrap();
        if slot.as_ref().map_or(true, |l| l.over.load(Ordering::SeqCst)) {
            match start_live(srv, sess, num("w", 1280), num("h", 800)) { Ok(l) => *slot = Some(l), Err(_) => { drop(slot); fail(&mut out, "500 Internal Server Error", "Could not start the session"); return Ok(()); } }
        }
        slot.clone().unwrap()
    };
    let (tx, rx): (SyncSender<Vec<u8>>, Receiver<Vec<u8>>) = sync_channel(256);
    *live.sub.lock().unwrap() = Some(tx);      // a newer browser takes over; the older one's channel closes
    let _ = live.stdin.lock().unwrap().write_all(b"full\n");
    out.set_write_timeout(Some(Duration::from_secs(30)))?;
    out.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n")?;
    loop {
        if live.need_full.swap(false, Ordering::SeqCst) { while rx.try_recv().is_ok() {} let _ = live.stdin.lock().unwrap().write_all(b"full\n"); }
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(m) => { out.write_all(&m)?; let mut n = 0; loop { match rx.try_recv() { Ok(m) => { out.write_all(&m)?; n += 1; if n > 64 { break; } } Err(TryRecvError::Empty) => break, Err(TryRecvError::Disconnected) => return Ok(()) } } }
            Err(RecvTimeoutError::Timeout) => { out.write_all(&rframe::message(0, b""))?; if live.over.load(Ordering::SeqCst) { return Ok(()); } }   // a keep-alive that also finds dead connections
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
        if live.over.load(Ordering::SeqCst) && rx.try_recv().is_err() { return Ok(()); }
    }
}

fn download(srv: &Arc<Server>, sess: &Arc<Session>, req: &Request, out: &mut impl Write) {
    let Some(path) = req.query.get("path").filter(|p| valid_abs_path(p)) else { return fail(out, "400 Bad Request", "Give the full path of a file"); };
    let args = vec!["--remote-file".to_string(), "get".to_string(), path.clone()];
    let Ok(mut child) = srv.sys.spawn(&sess.user, &args, Stdio::null(), Stdio::piped()) else { return fail(out, "500 Internal Server Error", "Could not read the file"); };
    let mut rd = BufReader::new(child.stdout.take().unwrap());
    let mut head = String::new();
    let _ = rd.by_ref().take(4096).read_line(&mut head);
    let mut it = head.trim_end().splitn(3, ' ');
    if it.next() != Some("OK") { let _ = child.wait(); return fail(out, "404 Not Found", head.trim_end().strip_prefix("ERR ").unwrap_or("Could not read the file")); }
    let (size, name) = (it.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(0), it.next().unwrap_or("file").to_string());
    let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || "._- ()".contains(c) { c } else { '_' }).collect();
    let _ = write!(out, "HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: {size}\r\nContent-Disposition: attachment; filename=\"{safe}\"\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n");
    let _ = io::copy(&mut rd.take(size), out);
    let _ = child.wait();
}

fn upload(srv: &Arc<Server>, sess: &Arc<Session>, req: &Request, reader: &mut impl Read, out: &mut impl Write) {
    let (Some(dir), Some(name)) = (req.query.get("dir").filter(|d| valid_abs_path(d)), req.query.get("name").filter(|n| valid_file_name(n))) else { return fail(out, "400 Bad Request", "Give a folder and a file name"); };
    let Some(len) = req.header("content-length").and_then(|v| v.parse::<u64>().ok()) else { return fail(out, "411 Length Required", "Length required"); };
    if len > MAX_UPLOAD { return fail(out, "413 Payload Too Large", "That file is too large"); }
    let args = vec!["--remote-file".to_string(), "put".to_string(), dir.clone(), name.clone(), len.to_string()];
    let Ok(mut child) = srv.sys.spawn(&sess.user, &args, Stdio::piped(), Stdio::piped()) else { return fail(out, "500 Internal Server Error", "Could not write the file"); };
    {
        let mut stdin = child.stdin.take().unwrap();
        let mut body = reader.take(len);
        let copied = io::copy(&mut body, &mut stdin).unwrap_or(0);
        drop(stdin);
        if copied != len { let _ = child.kill(); let _ = child.wait(); return fail(out, "400 Bad Request", "The upload was cut short"); }
    }
    let mut rd = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    let _ = rd.by_ref().take(4096).read_line(&mut line);
    let _ = child.wait();
    if line.starts_with("OK") { *sess.last_key.lock().unwrap() = Instant::now(); json(out, "200 OK", &format!("{{\"ok\":true,\"name\":{}}}", json_str(line.trim_end().strip_prefix("OK ").unwrap_or(name))), ""); }
    else { fail(out, "409 Conflict", line.trim_end().strip_prefix("ERR ").unwrap_or("Could not write the file")); }
}

/// Send a file: a first line `OK SIZE NAME` (or `ERR why`), then the bytes.
pub fn file_get(path: &str, out: &mut impl Write) -> i32 {
    match std::fs::File::open(path).and_then(|f| f.metadata().map(|m| (f, m))) {
        Ok((f, m)) if m.is_file() => {
            let name = Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            let _ = writeln!(out, "OK {} {}", m.len(), name.replace('\n', " "));
            let _ = io::copy(&mut BufReader::new(f), out);
            0
        }
        Ok(_) => { let _ = writeln!(out, "ERR That is a folder, not a file."); 1 }
        Err(e) => { let _ = writeln!(out, "ERR {}", match e.kind() { io::ErrorKind::NotFound => "There is no such file.".to_string(), io::ErrorKind::PermissionDenied => "You are not allowed to read that file.".to_string(), _ => e.to_string() }); 1 }
    }
}

/// Receive `size` bytes into DIR/NAME. An existing file is never overwritten: the next free name is used.
pub fn file_put(dir: &str, name: &str, size: u64, input: &mut impl Read, out: &mut impl Write) -> i32 {
    let dir = Path::new(dir);
    if !valid_file_name(name) || !dir.is_dir() { let _ = writeln!(out, "ERR That folder does not exist."); return 1; }
    let mut final_name = name.to_string();
    let mut n = 1;
    while dir.join(&final_name).exists() { n += 1; final_name = match name.rsplit_once('.') { Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"), _ => format!("{name} ({n})") }; }
    let tmp = dir.join(format!(".{final_name}.part"));
    let result = (|| -> io::Result<()> {
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp)?;
        let got = io::copy(&mut input.take(size), &mut f)?;
        if got != size { return Err(io::Error::other("The upload was cut short.")); }
        f.sync_all()?;
        std::fs::rename(&tmp, dir.join(&final_name))
    })();
    match result {
        Ok(()) => { let _ = writeln!(out, "OK {final_name}"); 0 }
        Err(e) => { let _ = std::fs::remove_file(&tmp); let _ = writeln!(out, "ERR {}", if e.kind() == io::ErrorKind::PermissionDenied { "You are not allowed to write in that folder.".to_string() } else { e.to_string() }); 1 }
    }
}

/// `exec85 --remote-file get PATH` and `put DIR NAME SIZE`, run as the person who is logged in.
pub fn file_command(args: &[String]) -> i32 {
    let mut out = io::stdout().lock();
    match args.first().map(String::as_str) {
        Some("get") if args.len() == 2 => file_get(&args[1], &mut out),
        Some("put") if args.len() == 4 => file_put(&args[1], &args[2], args[3].parse().unwrap_or(0), &mut io::stdin().lock(), &mut out),
        _ => { let _ = writeln!(out, "ERR bad request"); 2 }
    }
}

/// `exec85 --remote`: serve until stopped.
pub fn run() -> ! {
    let srv = Server::new(Box::new(Real));
    let listener = match TcpListener::bind(("127.0.0.1", LISTEN_PORT)) { Ok(l) => l, Err(e) => { eprintln!("cannot listen on 127.0.0.1:{LISTEN_PORT}: {e}"); std::process::exit(1); } };
    let s2 = srv.clone();
    thread::spawn(move || loop { thread::sleep(Duration::from_secs(30)); s2.sweep(); });
    for conn in listener.incoming().flatten() { let s = srv.clone(); thread::spawn(move || handle_connection(s, conn)); }
    std::process::exit(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[test]
    fn forms_urls_and_cookies_are_read_strictly() {
        assert_eq!(url_decode("a%20b+c%2Fd%zz%4"), "a b c/d%zz%4");
        let f = parse_form("user=tester&pass=se%26cret%3D+x&empty=&flag");
        assert_eq!((f["user"].as_str(), f["pass"].as_str(), f["empty"].as_str(), f["flag"].as_str()), ("tester", "se&cret= x", "", ""));
        let r = read_head(&mut &b"GET /api/stream?w=800&h=600 HTTP/1.1\r\nHost: x\r\nCookie: a=1; sid=abc; b=2\r\n\r\n"[..]).unwrap();
        assert_eq!((r.method.as_str(), r.path.as_str(), r.query["w"].as_str(), r.cookie("sid").as_deref(), r.cookie("nope")), ("GET", "/api/stream", "800", Some("abc"), None));
    }

    #[test]
    fn only_sane_requests_are_accepted() {
        for bad in ["", "GET\r\n\r\n", "DELETE / HTTP/1.1\r\n\r\n", "GET http://x/ HTTP/1.1\r\n\r\n", "GET / SPDY/3\r\n\r\n", "GET / HTTP/1.1\r\nHost: x\r\n"] {
            assert!(read_head(&mut bad.as_bytes()).is_err(), "{bad:?}");
        }
        let long = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(9000));
        assert!(read_head(&mut long.as_bytes()).is_err(), "a line of 9000 bytes");
        let many = format!("GET / HTTP/1.1\r\n{}\r\n", "X: y\r\n".repeat(100));
        assert!(read_head(&mut many.as_bytes()).is_err(), "a hundred headers");
    }

    #[test]
    fn the_real_address_comes_from_the_proxy_line() {
        assert_eq!(parse_proxy_line("PROXY TCP4 192.168.1.7 192.168.1.5 5000 8443\r\n"), Some("192.168.1.7".parse().unwrap()));
        assert_eq!(parse_proxy_line("PROXY TCP6 fe80::1 fe80::2 1 2\r\n"), Some("fe80::1".parse().unwrap()));
        assert_eq!(parse_proxy_line("PROXY UNKNOWN\r\n"), None);
        assert_eq!(parse_proxy_line("GET / HTTP/1.1"), None);
    }

    #[test]
    fn five_wrong_passwords_lock_the_name_and_the_address_for_a_while() {
        let mut l = Limiter::default();
        let keys = vec!["user:tester".to_string(), "ip:1.2.3.4".to_string()];
        let other = vec!["user:other".to_string(), "ip:5.6.7.8".to_string()];
        let t0 = Instant::now();
        for i in 0..4 { assert!(l.allowed(&keys, t0 + Duration::from_secs(i))); l.failed(&keys, t0 + Duration::from_secs(i)); }
        assert!(l.allowed(&keys, t0 + Duration::from_secs(5)));
        l.failed(&keys, t0 + Duration::from_secs(5));
        assert!(!l.allowed(&keys, t0 + Duration::from_secs(6)), "locked");
        assert!(!l.allowed(&["user:tester".into(), "ip:9.9.9.9".into()], t0 + Duration::from_secs(6)), "the name is locked from any address");
        assert!(!l.allowed(&["user:x".into(), "ip:1.2.3.4".into()], t0 + Duration::from_secs(6)), "the address is locked for any name");
        assert!(l.allowed(&other, t0 + Duration::from_secs(6)), "other people are not affected");
        assert!(l.allowed(&keys, t0 + LOGIN_LOCK + Duration::from_secs(10)), "the lock ends");
        // old failures are forgotten, and a good login clears the count
        let mut l = Limiter::default();
        for i in 0..4 { l.failed(&keys, t0 + Duration::from_secs(i)); }
        l.failed(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(60));
        assert!(l.allowed(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(61)));
        for _ in 0..3 { l.failed(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(62)); }
        l.succeeded(&keys);
        for _ in 0..4 { l.failed(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(63)); }
        assert!(l.allowed(&keys, t0 + LOGIN_WINDOW + Duration::from_secs(64)));
    }

    #[test]
    fn names_and_paths_are_checked() {
        for ok in ["a.txt", "My file (2).pdf", "x"] { assert!(valid_file_name(ok), "{ok}"); }
        for bad in ["", ".hidden", "..", "a/b", "a\\b\0", "line\nbreak", &"x".repeat(201)] { assert!(!valid_file_name(bad), "{bad:?}"); }
        assert!(valid_abs_path("/home/tester/a b.txt") && !valid_abs_path("relative") && !valid_abs_path("/a\0b") && !valid_abs_path("/a\nb"));
        assert_eq!(json_str("a\"b\\c\nd"), "\"a\\\"b\\\\c\\u000ad\"");
    }

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-remote-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
    }

    #[test]
    fn files_go_both_ways_never_overwrite_and_never_leave_half_files() {
        let d = tmpdir("files");
        let ds = d.to_str().unwrap();
        let mut out = Vec::new();
        assert_eq!(file_put(ds, "note.txt", 5, &mut &b"hello"[..], &mut out), 0);
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "OK note.txt\n");
        out.clear();
        assert_eq!(file_put(ds, "note.txt", 5, &mut &b"HELLO"[..], &mut out), 0);
        assert_eq!(String::from_utf8(out.clone()).unwrap(), "OK note (2).txt\n", "the first file is safe");
        assert_eq!(std::fs::read(d.join("note.txt")).unwrap(), b"hello");
        out.clear();
        assert_eq!(file_put(ds, "cut.bin", 100, &mut &b"short"[..], &mut out), 1);
        assert!(String::from_utf8(out.clone()).unwrap().starts_with("ERR") && !d.join("cut.bin").exists() && !d.join(".cut.bin.part").exists(), "{:?}", std::fs::read_dir(&d).unwrap().count());
        out.clear();
        assert_eq!(file_put("/nonexistent-folder", "a", 1, &mut &b"x"[..], &mut out), 1);
        out.clear();
        assert_eq!(file_get(&format!("{ds}/note.txt"), &mut out), 0);
        assert_eq!(out, b"OK 5 note.txt\nhello");
        for bad in [ds.to_string(), format!("{ds}/missing")] { out.clear(); assert_eq!(file_get(&bad, &mut out), 1); assert!(out.starts_with(b"ERR "), "{bad}"); }
        let _ = std::fs::remove_dir_all(&d);
    }

    // ---- the whole server, with a made-up login and a made-up session program ----

    struct Fake { log: PathBuf, powered: Arc<Mutex<Vec<i32>>> }
    impl System for Fake {
        fn auth(&self, name: &str, pw: &str) -> Result<User, AuthError> {
            match (name, pw) {
                ("tester", "secret7-") => Ok(User { name: "tester".into(), uid: 1000, gid: 1000, home: "/home/tester".into(), shell: "/bin/ash".into() }),
                ("root", "toor") => Ok(User { name: "root".into(), uid: 0, gid: 0, home: "/root".into(), shell: "/bin/ash".into() }),
                _ => Err(AuthError::BadPassword),
            }
        }
        fn spawn(&self, _u: &User, args: &[String], stdin: Stdio, stdout: Stdio) -> io::Result<Child> {
            assert_eq!(&args[0], "--remote-session");
            // says "hello" as a state message, then keeps what it is sent
            Command::new("sh").arg("-c").arg(format!("printf '\\002\\005\\000\\000\\000hello'; cat > {}", self.log.display())).stdin(stdin).stdout(stdout).spawn()
        }
        fn power(&self, code: i32) { self.powered.lock().unwrap().push(code); }
    }

    fn http(addr: std::net::SocketAddr, head: &str, body: &[u8]) -> (String, Vec<u8>) {
        let mut c = TcpStream::connect(addr).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        c.write_all(head.as_bytes()).unwrap(); c.write_all(body).unwrap();
        let mut all = Vec::new(); let _ = c.read_to_end(&mut all);
        let at = all.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(all.len());
        (String::from_utf8_lossy(&all[..at]).into_owned(), all[(at + 4).min(all.len())..].to_vec())
    }
    fn post(addr: std::net::SocketAddr, path: &str, cookie: &str, extra: &str, body: &str) -> (String, Vec<u8>) {
        http(addr, &format!("POST {path} HTTP/1.1\r\nHost: x\r\n{cookie}{extra}Content-Length: {}\r\n\r\n", body.len()), body.as_bytes())
    }

    #[test]
    fn the_server_logs_people_in_keeps_them_apart_and_passes_keys_on() {
        let d = tmpdir("server");
        let log = d.join("keys.txt");
        let srv = Server::new(Box::new(Fake { log: log.clone(), powered: Arc::new(Mutex::new(vec![])) }));
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let s2 = srv.clone();
        thread::spawn(move || for c in listener.incoming().flatten() { let s = s2.clone(); thread::spawn(move || handle_connection(s, c)); });
        let h = "X-Exec85: 1\r\n";

        let (head, body) = http(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n", b"");
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("Content-Security-Policy") && head.contains("X-Frame-Options: DENY") && String::from_utf8_lossy(&body).contains("<title>"));
        assert!(http(addr, "GET /api/me HTTP/1.1\r\nHost: x\r\n\r\n", b"").0.starts_with("HTTP/1.1 401"), "nothing without a login");
        assert!(post(addr, "/api/login", "", "", "user=tester&pass=secret7-").0.starts_with("HTTP/1.1 403"), "a login without the header is refused (another web site could send it)");
        let (head, _) = post(addr, "/api/login", "", h, "user=tester&pass=wrong");
        assert!(head.starts_with("HTTP/1.1 401"), "{head}");
        let (head, _) = post(addr, "/api/login", "", h, "user=root&pass=toor");
        assert!(head.starts_with("HTTP/1.1 401"), "root never logs in here: {head}");
        for _ in 0..4 { srv.limiter.lock().unwrap().failed(&["user:tester".to_string(), "ip:127.0.0.1".to_string()], Instant::now()); }
        let (head, _) = post(addr, "/api/login", "", h, "user=tester&pass=secret7-");
        assert!(head.starts_with("HTTP/1.1 429"), "locked after too many wrong passwords, even with the right one: {head}");
        *srv.limiter.lock().unwrap() = Limiter::default();

        let (head, body) = post(addr, "/api/login", "", h, "user=tester&pass=secret7-");
        assert!(head.starts_with("HTTP/1.1 200") && String::from_utf8_lossy(&body).contains("\"user\":\"tester\""), "{head}");
        let cookie_line = head.lines().find(|l| l.starts_with("Set-Cookie: sid=")).unwrap().to_string();
        assert!(cookie_line.contains("HttpOnly") && cookie_line.contains("Secure") && cookie_line.contains("SameSite=Strict"));
        let token = cookie_line["Set-Cookie: sid=".len()..].split(';').next().unwrap().to_string();
        assert_eq!(token.len(), 64);
        let ck = format!("Cookie: sid={token}\r\n");
        assert!(http(addr, &format!("GET /api/me HTTP/1.1\r\nHost: x\r\n{ck}\r\n"), b"").1.starts_with(b"{\"ok\":true,\"user\":\"tester\""));
        assert!(post(addr, "/api/key", &ck, "", "k alt-a\n").0.starts_with("HTTP/1.1 403"), "a key press without the header is refused");
        // a refusal sent before the body was read still arrives whole (the connection is not reset under the browser)
        for _ in 0..30 {
            let mut c = TcpStream::connect(addr).unwrap(); c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
            let body = "k alt-a\n".repeat(300);
            c.write_all(format!("POST /api/key HTTP/1.1\r\nHost: x\r\n{ck}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            let mut all = Vec::new(); c.read_to_end(&mut all).expect("the reply must arrive whole");
            let text = String::from_utf8_lossy(&all).into_owned();
            assert!(text.starts_with("HTTP/1.1 403") && text.contains("Missing header"), "{text:?}");
        }
        assert!(post(addr, "/api/key", &ck, h, "k alt-a\n").0.starts_with("HTTP/1.1 409"), "no screen has been asked for yet");

        // the screen stream: the first thing it says is what the session program says
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        s.write_all(format!("GET /api/stream?w=800&h=600 HTTP/1.1\r\nHost: x\r\n{ck}\r\n").as_bytes()).unwrap();
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut line = String::new();
        loop { line.clear(); r.read_line(&mut line).unwrap(); if line == "\r\n" { break; } }
        assert_eq!(rframe::read_message(&mut r).unwrap(), Some((rframe::KIND_STATE, b"hello".to_vec())));
        // keys reach the session program, and nothing else does
        let (head, _) = post(addr, "/api/key", &ck, h, "k alt-a\nt hi there\nrm -rf /\nfull\n");
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let (head, body) = http(addr, &format!("GET /api/state HTTP/1.1\r\nHost: x\r\n{ck}\r\n"), b"");
        assert!(head.starts_with("HTTP/1.1 200") && body == b"hello", "{head}");
        // wait until the session program has really taken in the keys (a slow machine may not have started it yet)
        for _ in 0..100 { if std::fs::read_to_string(&log).unwrap_or_default().contains("full") { break; } thread::sleep(Duration::from_millis(100)); }
        // the program is ended on logout, and the old token stops working
        let (head, _) = post(addr, "/api/logout", &ck, h, "");
        assert!(head.starts_with("HTTP/1.1 200") && head.contains("Max-Age=0"));
        assert!(http(addr, &format!("GET /api/me HTTP/1.1\r\nHost: x\r\n{ck}\r\n"), b"").0.starts_with("HTTP/1.1 401"));
        let mut kept = String::new();
        for _ in 0..50 { kept = std::fs::read_to_string(&log).unwrap_or_default(); if kept.contains("full") { break; } thread::sleep(Duration::from_millis(100)); }
        assert!(kept.starts_with("full\n"), "a new browser asks for the whole picture first: {kept:?}");
        assert!(kept.contains("k alt-a\nt hi there\nfull\n") && !kept.contains("rm -rf"), "{kept:?}");
        assert_eq!(srv.session_count(), 0);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn idle_sessions_are_closed() {
        let srv = Server::new(Box::new(Fake { log: PathBuf::from("/dev/null"), powered: Arc::new(Mutex::new(vec![])) }));
        let user = User { name: "t".into(), uid: 1000, gid: 1000, home: "/".into(), shell: "/bin/ash".into() };
        let old = Instant::now().checked_sub(IDLE_LIMIT + Duration::from_secs(5)).unwrap();
        srv.sessions.lock().unwrap().insert("a".into(), Arc::new(Session { user: user.clone(), created: old, last_key: Mutex::new(old), live: Mutex::new(None) }));
        srv.sessions.lock().unwrap().insert("b".into(), Arc::new(Session { user, created: Instant::now(), last_key: Mutex::new(Instant::now()), live: Mutex::new(None) }));
        srv.sweep();
        assert_eq!(srv.session_count(), 1);
        assert!(srv.sessions.lock().unwrap().contains_key("b"));
    }
}
