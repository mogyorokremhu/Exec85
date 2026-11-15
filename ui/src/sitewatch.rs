//! Website monitor: checks websites now and then, and sends a notification to a phone (ntfy.sh, or a Discord channel)
//! when one goes down or comes back.
//!
//! A site counts as DOWN after two failed checks in a row (one bad answer is often just a hiccup) and as UP again after the
//! first good one. "Good" means the server answered with anything below 500: a page that exists, or even a "not found" or a login
//! wall, proves the server is running; no answer at all, a timeout, or a 5xx error means down.
//! The checks use the system's own downloader (`wget`, like the Minecraft and tunnel downloads), so name lookup works everywhere.
use crate::helper::Ctx;
use crate::services::{self, ServiceDef};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const SERVICE: &str = "sitewatch";
pub const MAX_SITES: usize = 20;
pub const MIN_SECONDS: u64 = 10;
pub const DEFAULT_SECONDS: u64 = 120;
const FAILS_FOR_DOWN: u32 = 2;
const DIR: &str = "etc/exec85/sitewatch";
const STATE: &str = "var/lib/exec85/sitewatch/state";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Site { pub id: String, pub seconds: u64, pub url: String }

// ---- what is accepted ------------------------------------------------------------------------------------------

pub fn valid_url(u: &str) -> bool {
    (u.starts_with("http://") || u.starts_with("https://")) && u.len() <= 300 && host_of(u).map_or(false, |h| !h.is_empty())
        && u.chars().all(|c| c.is_ascii_alphanumeric() || "-._~:/?&=%+#,;!".contains(c))
}
pub fn host_of(u: &str) -> Option<String> {
    let rest = u.strip_prefix("https://").or_else(|| u.strip_prefix("http://"))?;
    Some(rest.split(['/', '?', '#']).next()?.to_string())
}
/// ntfy topics: the name is the only secret, so letters, digits, - and _ only.
pub fn valid_topic(t: &str) -> bool { !t.is_empty() && t.len() <= 64 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') }
pub fn valid_discord(u: &str) -> bool { (u.starts_with("https://discord.com/api/webhooks/") || u.starts_with("https://discordapp.com/api/webhooks/")) && u.len() <= 300 && u.chars().all(|c| c.is_ascii_alphanumeric() || "-._~:/?&=%".contains(c)) }
pub fn valid_server(u: &str) -> bool { (u.starts_with("https://") || u.starts_with("http://")) && u.len() <= 100 && !u.ends_with('/') && u.chars().all(|c| c.is_ascii_alphanumeric() || "-._:/".contains(c)) }

// ---- the saved lists -----------------------------------------------------------------------------------------

pub fn parse_sites(text: &str) -> Vec<Site> {
    text.lines().filter_map(|l| {
        let mut w = l.splitn(4, ' ');
        if w.next()? != "site" { return None; }
        let (id, secs, url) = (w.next()?, w.next()?.parse::<u64>().ok()?, w.next()?);
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric()) || secs < MIN_SECONDS || !valid_url(url) { return None; }
        Some(Site { id: id.into(), seconds: secs, url: url.into() })
    }).collect()
}
pub fn render_sites(s: &[Site]) -> String { s.iter().map(|x| format!("site {} {} {}\n", x.id, x.seconds, x.url)).collect() }

fn sites_path(ctx: &Ctx) -> PathBuf { ctx.root.join(DIR).join("sites.conf") }
fn notify_path(ctx: &Ctx) -> PathBuf { ctx.root.join(DIR).join("notify.conf") }
fn state_path(ctx: &Ctx) -> PathBuf { ctx.root.join(STATE) }

pub fn load_sites(ctx: &Ctx) -> Vec<Site> { std::fs::read_to_string(sites_path(ctx)).map(|t| parse_sites(&t)).unwrap_or_default() }
fn save_sites(ctx: &Ctx, s: &[Site]) -> Result<(), String> {
    std::fs::create_dir_all(ctx.root.join(DIR)).map_err(|e| e.to_string())?;
    std::fs::write(sites_path(ctx), render_sites(s)).map_err(|e| e.to_string())
}

pub fn add_site(ctx: &Ctx, url: &str, seconds: u64) -> Result<String, String> {
    if !valid_url(url) { return Err("That is not a web address I can watch. It must start with http:// or https:// and have no spaces.".into()); }
    if seconds < MIN_SECONDS { return Err(format!("Check at most every {MIN_SECONDS} seconds.")); }
    let mut sites = load_sites(ctx);
    if sites.len() >= MAX_SITES { return Err(format!("At most {MAX_SITES} websites.")); }
    if sites.iter().any(|s| s.url == url) { return Err("That website is already being watched.".into()); }
    let mut n = 1;
    while sites.iter().any(|s| s.id == format!("s{n}")) { n += 1; }
    let id = format!("s{n}");
    sites.push(Site { id: id.clone(), seconds, url: url.into() });
    save_sites(ctx, &sites)?;
    Ok(format!("Watching {url} (called {id}), every {} seconds.", seconds))
}

pub fn remove_site(ctx: &Ctx, id: &str) -> Result<String, String> {
    let mut sites = load_sites(ctx);
    let before = sites.len();
    sites.retain(|s| s.id != id);
    if sites.len() == before { return Err(format!("There is no website called {id}.")); }
    save_sites(ctx, &sites)?;
    let mut st = load_state(ctx); st.retain(|(i, _)| i != id); let _ = save_state(ctx, &st);
    Ok(format!("{id} is no longer watched."))
}

// ---- where to send the news --------------------------------------------------------------------------------

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notify { pub server: String, pub topic: String, pub discord: String }

pub fn load_notify(ctx: &Ctx) -> Notify {
    let mut n = Notify { server: "https://ntfy.sh".into(), ..Default::default() };
    for l in std::fs::read_to_string(notify_path(ctx)).unwrap_or_default().lines() {
        match l.split_once('=') {
            Some(("server", v)) if valid_server(v) => n.server = v.into(),
            Some(("topic", v)) if valid_topic(v) => n.topic = v.into(),
            Some(("discord", v)) if valid_discord(v) => n.discord = v.into(),
            _ => {}
        }
    }
    n
}

/// key: ntfy-topic | ntfy-server | discord. An empty value removes it.
pub fn set_notify(ctx: &Ctx, key: &str, value: &str) -> Result<String, String> {
    let mut n = load_notify(ctx);
    match key {
        "ntfy-topic" => { if !value.is_empty() && !valid_topic(value) { return Err("The topic may have only letters, digits, - and _ (up to 64). Make it long and hard to guess: anybody who knows it can read your alerts.".into()); } n.topic = value.into(); }
        "ntfy-server" => { let v = if value.is_empty() { "https://ntfy.sh" } else { value }; if !valid_server(v) { return Err("The server address must start with http:// or https:// and have no / at the end.".into()); } n.server = v.into(); }
        "discord" => { if !value.is_empty() && !valid_discord(value) { return Err("That is not a Discord webhook address (it starts with https://discord.com/api/webhooks/).".into()); } n.discord = value.into(); }
        _ => return Err("Use ntfy-topic, ntfy-server or discord.".into()),
    }
    std::fs::create_dir_all(ctx.root.join(DIR)).map_err(|e| e.to_string())?;
    let text = format!("server={}\ntopic={}\ndiscord={}\n", n.server, n.topic, n.discord);
    std::fs::write(notify_path(ctx), text).map_err(|e| e.to_string())?;
    { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(notify_path(ctx), std::fs::Permissions::from_mode(0o600)); }
    Ok("Saved.".into())
}

// ---- checking ------------------------------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check { Up(u16), Down(String) }

/// The answer of `wget -S` (it prints the headers it gets): the last "HTTP/1.x NNN" line is the answer.
pub fn parse_wget(code: i32, out: &str) -> Check {
    let status = out.lines().filter_map(|l| { let t = l.trim_start(); let rest = t.strip_prefix("HTTP/")?; rest.split_whitespace().nth(1)?.parse::<u16>().ok() }).last();
    match status {
        Some(s) if s < 500 => Check::Up(s),
        Some(s) => Check::Down(format!("server error {s}")),
        None => {
            let why = out.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("no answer");
            Check::Down(if code != 0 { why.chars().take(60).collect() } else { "no answer".into() })
        }
    }
}

pub fn check_site(ctx: &Ctx, url: &str) -> Check {
    let (code, out) = ctx.runner.run("wget", &["-S", "-O", "/dev/null", "-T", "10", "-t", "1", url]);
    parse_wget(code, &out)
}

// ---- the state of each site and the alerts -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct State { pub up: bool, pub since: u64, pub last: u64, pub fails: u32, pub code: u16 }

pub fn parse_state(text: &str) -> Vec<(String, State)> {
    text.lines().filter_map(|l| {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.len() != 6 { return None; }
        Some((w[0].to_string(), State { up: w[1] == "up", since: w[2].parse().ok()?, last: w[3].parse().ok()?, fails: w[4].parse().ok()?, code: w[5].parse().ok()? }))
    }).collect()
}
pub fn render_state(s: &[(String, State)]) -> String { s.iter().map(|(i, t)| format!("{i} {} {} {} {} {}\n", if t.up { "up" } else { "down" }, t.since, t.last, t.fails, t.code)).collect() }
pub fn load_state(ctx: &Ctx) -> Vec<(String, State)> { std::fs::read_to_string(state_path(ctx)).map(|t| parse_state(&t)).unwrap_or_default() }
fn save_state(ctx: &Ctx, s: &[(String, State)]) -> Result<(), String> {
    if let Some(d) = state_path(ctx).parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(state_path(ctx), render_state(s)).map_err(|e| e.to_string())
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Alert { pub id: String, pub down: bool, pub title: String, pub text: String }

fn duration_words(secs: u64) -> String {
    if secs < 90 { format!("{secs} seconds") } else if secs < 5400 { format!("{} minutes", (secs + 30) / 60) } else { format!("{} hours", (secs + 1800) / 3600) }
}

/// Run the checks that are due and say what changed. `check` does the real work (replaced in tests).
pub fn tick(sites: &[Site], states: &mut Vec<(String, State)>, now: u64, check: &mut dyn FnMut(&Site) -> Check) -> Vec<Alert> {
    let mut alerts = Vec::new();
    states.retain(|(i, _)| sites.iter().any(|s| &s.id == i));
    for site in sites {
        let idx = states.iter().position(|(i, _)| *i == site.id);
        if let Some(i) = idx { if now.saturating_sub(states[i].1.last) < site.seconds { continue; } }
        let host = host_of(&site.url).unwrap_or_else(|| site.url.clone());
        let result = check(site);
        match (idx, result) {
            (None, Check::Up(c)) => states.push((site.id.clone(), State { up: true, since: now, last: now, fails: 0, code: c })),
            (None, Check::Down(_)) => states.push((site.id.clone(), State { up: true, since: now, last: now, fails: 1, code: 0 })),   // not told yet: it may be a hiccup
            (Some(i), Check::Up(c)) => {
                let st = &mut states[i].1;
                let was_down = !st.up && st.fails >= FAILS_FOR_DOWN;
                if was_down { alerts.push(Alert { id: site.id.clone(), down: false, title: format!("Exec85: {host} is back up"), text: format!("{host} is back UP after {}.", duration_words(now.saturating_sub(st.since))) }); }
                if !st.up || was_down { st.since = now; }
                *st = State { up: true, since: if was_down { now } else { st.since }, last: now, fails: 0, code: c };
            }
            (Some(i), Check::Down(why)) => {
                let st = &mut states[i].1;
                st.fails += 1; st.last = now; st.code = 0;
                if st.fails == FAILS_FOR_DOWN { st.up = false; st.since = now; alerts.push(Alert { id: site.id.clone(), down: true, title: format!("Exec85: {host} is down"), text: format!("{host} is DOWN ({why}).") }); }
            }
        }
    }
    alerts
}

fn plain(s: &str) -> String { s.chars().map(|c| if c.is_ascii_alphanumeric() || " .:()/_-,".contains(c) { c } else { ' ' }).collect() }

/// Tell the phone (and the Discord channel) about a change. Returns what went wrong, if anything.
pub fn notify(ctx: &Ctx, n: &Notify, a: &Alert) -> Result<(), String> {
    let mut errors = Vec::new();
    let (title, text) = (plain(&a.title), plain(&a.text));
    if !n.topic.is_empty() {
        let url = format!("{}/{}", n.server, n.topic);
        let tags = if a.down { "Tags: rotating_light" } else { "Tags: white_check_mark" };
        let (code, out) = ctx.runner.run("wget", &["-q", "-O", "/dev/null", "-T", "15", "-t", "1", "--header", &format!("Title: {title}"), "--header", tags, "--post-data", &text, &url]);
        if code != 0 { errors.push(format!("ntfy: {}", out.lines().last().unwrap_or("failed").trim())); }
    }
    if !n.discord.is_empty() {
        let body = format!("{{\"content\":\"{}\"}}", text);
        let (code, out) = ctx.runner.run("wget", &["-q", "-O", "/dev/null", "-T", "15", "-t", "1", "--header", "Content-Type: application/json", "--post-data", &body, &n.discord]);
        if code != 0 { errors.push(format!("Discord: {}", out.lines().last().unwrap_or("failed").trim())); }
    }
    if n.topic.is_empty() && n.discord.is_empty() { return Err("No notification is set up yet (an ntfy topic or a Discord webhook).".into()); }
    if errors.is_empty() { Ok(()) } else { Err(errors.join("; ")) }
}

pub fn test_notification(ctx: &Ctx) -> Result<String, String> {
    let a = Alert { id: "test".into(), down: false, title: "Exec85: test".into(), text: "This is a test message from your Exec85 website monitor.".into() };
    notify(ctx, &load_notify(ctx), &a).map(|_| "Test message sent. Look at your phone.".to_string())
}

// ---- status for the app and the command -------------------------------------------------------------------------

pub fn enabled(ctx: &Ctx) -> bool { ctx.root.join("etc/exec85/services").join(format!("{SERVICE}.conf")).exists() }

/// `enabled=`, `ntfy=` (topic or -), `server=`, `discord=` (yes/no), then `site ID SECONDS up|down|unknown SINCE_SECONDS_AGO CODE URL`.
pub fn status(ctx: &Ctx) -> String {
    let (n, st, now) = (load_notify(ctx), load_state(ctx), unix_now());
    let mut s = format!("enabled={}\nntfy={}\nserver={}\ndiscord={}\n", if enabled(ctx) { "yes" } else { "no" }, if n.topic.is_empty() { "-" } else { &n.topic }, n.server, if n.discord.is_empty() { "no" } else { "yes" });
    for site in load_sites(ctx) {
        let (word, ago, code) = match st.iter().find(|(i, _)| *i == site.id) {
            Some((_, t)) if t.up && t.fails < FAILS_FOR_DOWN && t.code == 0 && t.fails > 0 => ("unknown", 0, 0),
            Some((_, t)) => (if t.up { "up" } else { "down" }, now.saturating_sub(t.since), t.code),
            None => ("unknown", 0, 0),
        };
        s.push_str(&format!("site {} {} {word} {ago} {code} {}\n", site.id, site.seconds, site.url));
    }
    s
}

pub fn unix_now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) }

pub fn enable(ctx: &Ctx) -> Result<String, String> {
    let d = ServiceDef { name: SERVICE.into(), description: "Exec85 website monitor".into(), command: "/usr/bin/exec85 --sitewatchd".into(), dir: "/".into(), user: "root".into(), env: vec![], autostart: true, ports: String::new() };
    crate::helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&d)])?;
    crate::helper::handle(ctx, &["svc-restart".to_string(), SERVICE.to_string()])?;
    Ok("The website monitor is on.".into())
}
pub fn disable(ctx: &Ctx) -> Result<String, String> {
    if enabled(ctx) { let _ = crate::helper::handle(ctx, &["svc-stop".to_string(), SERVICE.to_string()]); crate::helper::handle(ctx, &["svc-remove".to_string(), SERVICE.to_string()])?; }
    Ok("The website monitor is off.".into())
}

/// `exec85 --sitewatchd`: the background program. It reads its lists again every round, so changes need no restart.
pub fn run() -> ! {
    let ctx = Ctx { root: std::path::Path::new("/"), runner: &crate::helper::SysRunner };
    loop {
        let sites = load_sites(&ctx);
        let mut states = load_state(&ctx);
        let alerts = tick(&sites, &mut states, unix_now(), &mut |s| check_site(&ctx, &s.url));
        let _ = save_state(&ctx, &states);
        for a in &alerts {
            let r = notify(&ctx, &load_notify(&ctx), a);
            crate::supervisor::console_line(&format!("EXEC85_SITEWATCH {} {} {}", a.id, if a.down { "down" } else { "up" }, match r { Ok(()) => "sent".to_string(), Err(e) => format!("not-sent {}", e.replace(' ', "_")) }));
        }
        std::thread::sleep(Duration::from_secs(5));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;

    struct Fake { calls: RefCell<Vec<String>>, fail_post: bool }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            if c == "wget" && a.contains(&"--post-data") && self.fail_post { return (1, "wget: server returned error: HTTP/1.1 403 Forbidden".into()); }
            (0, String::new())
        }
    }
    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-sitewatch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:\n").unwrap();
        d
    }

    #[test]
    fn only_sane_addresses_and_topics_are_accepted() {
        for ok in ["http://example.com", "https://example.com/a/b?x=1&y=2", "http://192.168.1.5:8080/", "https://my-site.example.org:8443/status"] { assert!(valid_url(ok), "{ok}"); }
        for bad in ["", "ftp://x.org", "example.com", "http://", "http:///x", "https://a b.com", "https://x.com/\"; rm", "https://u:p@x.com/", "https://x.com/'x", &format!("https://x.com/{}", "a".repeat(300))] { assert!(!valid_url(bad), "{bad:?}"); }
        assert!(valid_topic("my-secret_topic-4821") && !valid_topic("") && !valid_topic("a b") && !valid_topic("a/b") && !valid_topic(&"a".repeat(65)));
        assert!(valid_discord("https://discord.com/api/webhooks/123/abc-DEF_1") && !valid_discord("https://evil.example/api/webhooks/1/2") && !valid_discord("https://discord.com/api/webhooks/1/2 x"));
        assert!(valid_server("https://ntfy.sh") && valid_server("http://10.0.2.2:18090") && !valid_server("https://ntfy.sh/") && !valid_server("ntfy.sh"));
        assert_eq!(host_of("https://example.com:8443/x?y"), Some("example.com:8443".into()));
    }

    #[test]
    fn the_answer_of_the_downloader_is_understood() {
        assert_eq!(parse_wget(0, "  HTTP/1.1 200 OK\n  Content-Type: text/html\n"), Check::Up(200));
        assert_eq!(parse_wget(8, "  HTTP/1.1 404 Not Found\nwget: server returned error: HTTP/1.1 404 Not Found"), Check::Up(404));
        assert_eq!(parse_wget(8, "  HTTP/1.1 503 Service Unavailable\nwget: server returned error: HTTP/1.1 503 Service Unavailable"), Check::Down("server error 503".into()));
        assert_eq!(parse_wget(1, "wget: bad address 'nope.example'"), Check::Down("wget: bad address 'nope.example'".into()));
        assert_eq!(parse_wget(1, "  HTTP/1.1 301 Moved\n  Location: https://x\n  HTTP/1.1 200 OK\n"), Check::Up(200), "the last answer counts");
        assert!(matches!(parse_wget(1, ""), Check::Down(_)));
    }

    #[test]
    fn a_site_is_down_after_two_bad_checks_and_up_after_one_good_one_and_each_change_alerts_once() {
        let sites = vec![Site { id: "s1".into(), seconds: 60, url: "https://example.com/".into() }];
        let mut st = Vec::new();
        let mut answers = vec![Check::Up(200), Check::Down("timed out".into()), Check::Down("timed out".into()), Check::Down("timed out".into()), Check::Up(200), Check::Up(200)].into_iter();
        let mut run = |st: &mut Vec<(String, State)>, now: u64| tick(&sites, st, now, &mut |_| answers.next().unwrap());
        assert!(run(&mut st, 1000).is_empty(), "first check, up: nothing to say");
        assert!(run(&mut st, 1030).is_empty(), "not due yet: the checker is not even asked (the next answer is kept for later)");
    }

    #[test]
    fn the_timeline_of_an_outage() {
        let sites = vec![Site { id: "s1".into(), seconds: 60, url: "https://example.com/".into() }];
        let mut st = Vec::new();
        let script = [Check::Up(200), Check::Down("timed out".into()), Check::Down("timed out".into()), Check::Down("timed out".into()), Check::Up(200), Check::Up(200)];
        let mut i = 0;
        let mut now = 1000u64;
        let mut all = Vec::new();
        for _ in 0..script.len() {
            let a = tick(&sites, &mut st, now, &mut |_| { let r = script[i].clone(); i += 1; r });
            all.push((now, a));
            now += 60;
        }
        assert!(all[0].1.is_empty() && all[1].1.is_empty(), "one bad check is a hiccup");
        assert_eq!(all[2].1.len(), 1, "the second bad check in a row tells you");
        assert!(all[2].1[0].down && all[2].1[0].text.contains("example.com is DOWN") && all[2].1[0].title.contains("is down"));
        assert!(all[3].1.is_empty(), "still down: no second message");
        assert_eq!(all[4].1.len(), 1);
        assert!(!all[4].1[0].down && all[4].1[0].text.contains("back UP after 2 minutes"), "{}", all[4].1[0].text);
        assert!(all[5].1.is_empty());
        assert!(st[0].1.up && st[0].1.fails == 0);
        // a hiccup that goes away says nothing at all
        let mut st = Vec::new(); let script = [Check::Up(200), Check::Down("x".into()), Check::Up(200)]; let mut i = 0; let mut now = 5000;
        for _ in 0..3 { assert!(tick(&sites, &mut st, now, &mut |_| { let r = script[i].clone(); i += 1; r }).is_empty()); now += 60; }
        // a site that is down from the very first check is told after the second
        let mut st = Vec::new(); let mut n = 0;
        assert!(tick(&sites, &mut st, 9000, &mut |_| { n += 1; Check::Down("no answer".into()) }).is_empty());
        let a = tick(&sites, &mut st, 9060, &mut |_| Check::Down("no answer".into()));
        assert_eq!(a.len(), 1);
        // removed sites lose their state
        tick(&[], &mut st, 9100, &mut |_| unreachable!());
        assert!(st.is_empty());
    }

    #[test]
    fn the_lists_state_and_notification_settings_are_saved_and_checked() {
        let r = world("lists");
        let f = Fake { calls: RefCell::new(vec![]), fail_post: false };
        let c = Ctx { root: &r, runner: &f };
        assert!(add_site(&c, "https://example.com", 5).is_err() && add_site(&c, "nope", 60).is_err());
        add_site(&c, "https://example.com", 60).unwrap();
        assert!(add_site(&c, "https://example.com", 60).is_err(), "twice is refused");
        assert_eq!(add_site(&c, "http://192.168.1.5:8080/", 120).unwrap(), "Watching http://192.168.1.5:8080/ (called s2), every 120 seconds.");
        assert_eq!(load_sites(&c).len(), 2);
        assert!(remove_site(&c, "s9").is_err());
        remove_site(&c, "s1").unwrap();
        assert_eq!(add_site(&c, "https://again.example", 60).unwrap(), "Watching https://again.example (called s1), every 60 seconds.", "a freed name is reused");
        for i in 0..MAX_SITES { let _ = add_site(&c, &format!("https://site{i}.example"), 60); }
        assert_eq!(load_sites(&c).len(), MAX_SITES);
        assert!(add_site(&c, "https://onetoomany.example", 60).is_err());
        // settings
        assert!(set_notify(&c, "ntfy-topic", "bad topic").is_err() && set_notify(&c, "discord", "https://evil.example/").is_err() && set_notify(&c, "pigeon", "x").is_err());
        set_notify(&c, "ntfy-topic", "garage-alerts-4821").unwrap(); set_notify(&c, "ntfy-server", "http://10.0.2.2:18090").unwrap();
        assert_eq!(load_notify(&c), Notify { server: "http://10.0.2.2:18090".into(), topic: "garage-alerts-4821".into(), discord: String::new() });
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(r.join("etc/exec85/sitewatch/notify.conf")).unwrap().permissions().mode() & 0o777, 0o600, "the topic is a secret");
        }
        set_notify(&c, "ntfy-server", "").unwrap();
        assert_eq!(load_notify(&c).server, "https://ntfy.sh");
        // state round-trips and a damaged file is ignored line by line
        let s = vec![("s1".to_string(), State { up: false, since: 10, last: 20, fails: 2, code: 0 })];
        assert_eq!(parse_state(&render_state(&s)), s);
        assert_eq!(parse_state("garbage\ns1 up 1 2 3\ns2 up 1 2 0 200\n").len(), 1);
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_message_goes_to_the_phone_and_to_discord_and_failures_are_reported() {
        let r = world("send");
        let f = Fake { calls: RefCell::new(vec![]), fail_post: false };
        let c = Ctx { root: &r, runner: &f };
        let a = Alert { id: "s1".into(), down: true, title: "Exec85: example.com is down".into(), text: "example.com is DOWN (timed out).".into() };
        assert!(notify(&c, &load_notify(&c), &a).is_err(), "nothing set up yet");
        set_notify(&c, "ntfy-topic", "garage-4821").unwrap();
        set_notify(&c, "discord", "https://discord.com/api/webhooks/1/abc").unwrap();
        notify(&c, &load_notify(&c), &a).unwrap();
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("--header Title: Exec85: example.com is down --header Tags: rotating_light --post-data example.com is DOWN (timed out). https://ntfy.sh/garage-4821"), "{calls}");
        assert!(calls.contains("--header Content-Type: application/json --post-data {\"content\":\"example.com is DOWN (timed out).\"} https://discord.com/api/webhooks/1/abc"), "{calls}");
        // nothing a stranger types can break out of a message
        let evil = Alert { id: "s1".into(), down: true, title: "t".into(), text: "x\"}; rm -rf / `id` $(id)".into() };
        notify(&c, &load_notify(&c), &evil).unwrap();
        assert!(!f.calls.borrow().iter().any(|c| c.contains('`') || c.contains("$(") || c.contains("\"}; rm")), "{:?}", f.calls.borrow().last());
        let f2 = Fake { calls: RefCell::new(vec![]), fail_post: true };
        let e = notify(&Ctx { root: &r, runner: &f2 }, &load_notify(&c), &a).unwrap_err();
        assert!(e.contains("ntfy:") && e.contains("Discord:") && e.contains("403"), "{e}");
        assert!(test_notification(&c).unwrap().contains("Test message sent"));
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_status_lists_every_site_with_what_is_known() {
        let r = world("status");
        let f = Fake { calls: RefCell::new(vec![]), fail_post: false };
        let c = Ctx { root: &r, runner: &f };
        assert!(status(&c).starts_with("enabled=no\nntfy=-\nserver=https://ntfy.sh\ndiscord=no\n"));
        add_site(&c, "https://example.com", 60).unwrap(); add_site(&c, "https://down.example", 60).unwrap();
        save_state(&c, &[("s2".to_string(), State { up: false, since: unix_now() - 300, last: unix_now(), fails: 2, code: 0 })]).unwrap();
        let t = status(&c);
        assert!(t.contains("site s1 60 unknown 0 0 https://example.com\n"), "{t}");
        assert!(t.contains("site s2 60 down 300 0 https://down.example\n") || t.contains("site s2 60 down 301 0 https://down.example\n"), "{t}");
        enable(&c).unwrap();
        assert!(status(&c).starts_with("enabled=yes\n"));
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/sitewatch.conf")).unwrap();
        assert!(conf.contains("command=/usr/bin/exec85 --sitewatchd") && conf.contains("user=root"), "{conf}");
        disable(&c).unwrap();
        assert!(!enabled(&c));
        let _ = std::fs::remove_dir_all(&r);
    }
}
