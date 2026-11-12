//! Minecraft server helper. It is only a preset on top of the generic parts of the system: it downloads a file
//! (the server program from Mojang), installs a package (the right Java) and defines a service. Nothing about
//! Minecraft is built into the system itself.
use crate::helper::{Ctx, Runner};
use crate::services::{self, ServiceDef};
use std::path::Path;

// ---- a small JSON reader (the standard library has none) -----------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
pub enum Json { Null, Bool(bool), Num(f64), Str(String), Arr(Vec<Json>), Obj(Vec<(String, Json)>) }

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> { if let Json::Obj(v) = self { v.iter().find(|(k, _)| k == key).map(|(_, v)| v) } else { None } }
    pub fn str(&self) -> Option<&str> { if let Json::Str(s) = self { Some(s) } else { None } }
    pub fn num(&self) -> Option<f64> { if let Json::Num(n) = self { Some(*n) } else { None } }
    pub fn arr(&self) -> Option<&Vec<Json>> { if let Json::Arr(a) = self { Some(a) } else { None } }
}

pub fn parse_json(text: &str) -> Result<Json, String> {
    let mut p = P { b: text.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    if p.i != p.b.len() { return Err("extra text after the JSON value".into()); }
    Ok(v)
}

struct P<'a> { b: &'a [u8], i: usize, depth: usize }

impl P<'_> {
    fn ws(&mut self) { while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\n' | b'\r' | b'\t') { self.i += 1; } }
    fn eat(&mut self, c: u8) -> Result<(), String> { if self.b.get(self.i) == Some(&c) { self.i += 1; Ok(()) } else { Err(format!("expected '{}' at {}", c as char, self.i)) } }
    fn lit(&mut self, s: &str, v: Json) -> Result<Json, String> { if self.b[self.i..].starts_with(s.as_bytes()) { self.i += s.len(); Ok(v) } else { Err(format!("bad value at {}", self.i)) } }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        self.depth += 1;
        if self.depth > 64 { return Err("nested too deeply".into()); }
        let r = match self.b.get(self.i).copied() {
            None => Err("unexpected end".into()),
            Some(b'{') => {
                self.i += 1; let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b'}') { self.i += 1; break; }
                    if !v.is_empty() { self.eat(b',')?; self.ws(); }
                    let k = self.string()?; self.ws(); self.eat(b':')?;
                    v.push((k, self.value()?));
                }
                Ok(Json::Obj(v))
            }
            Some(b'[') => {
                self.i += 1; let mut v = Vec::new();
                loop {
                    self.ws();
                    if self.b.get(self.i) == Some(&b']') { self.i += 1; break; }
                    if !v.is_empty() { self.eat(b',')?; }
                    v.push(self.value()?);
                }
                Ok(Json::Arr(v))
            }
            Some(b'"') => self.string().map(Json::Str),
            Some(b't') => self.lit("true", Json::Bool(true)),
            Some(b'f') => self.lit("false", Json::Bool(false)),
            Some(b'n') => self.lit("null", Json::Null),
            Some(_) => {
                let s = self.i;
                while self.i < self.b.len() && matches!(self.b[self.i], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') { self.i += 1; }
                std::str::from_utf8(&self.b[s..self.i]).ok().and_then(|t| t.parse().ok()).map(Json::Num).ok_or_else(|| format!("bad number at {s}"))
            }
        };
        self.depth -= 1;
        r
    }

    fn string(&mut self) -> Result<String, String> {
        self.eat(b'"')?;
        let mut out: Vec<u8> = Vec::new();
        loop {
            let c = *self.b.get(self.i).ok_or("unterminated string")?;
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = *self.b.get(self.i).ok_or("bad escape")?; self.i += 1;
                    match e {
                        b'"' => out.push(b'"'), b'\\' => out.push(b'\\'), b'/' => out.push(b'/'), b'n' => out.push(b'\n'), b't' => out.push(b'\t'), b'r' => out.push(b'\r'), b'b' => out.push(8), b'f' => out.push(12),
                        b'u' => {
                            let hex = |p: &mut Self| -> Result<u32, String> { let h = p.b.get(p.i..p.i + 4).and_then(|x| std::str::from_utf8(x).ok()).and_then(|x| u32::from_str_radix(x, 16).ok()).ok_or("bad \\u escape")?; p.i += 4; Ok(h) };
                            let mut cp = hex(self)?;
                            if (0xD800..0xDC00).contains(&cp) && self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') { self.i += 2; let lo = hex(self)?; cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF); }
                            let ch = char::from_u32(cp).unwrap_or('\u{fffd}');
                            let mut buf = [0u8; 4]; out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                        }
                        _ => return Err("bad escape".into()),
                    }
                }
                c => out.push(c),
            }
        }
        String::from_utf8(out).map_err(|_| "bad text".to_string())
    }
}

// ---- Mojang's lists --------------------------------------------------------------------------------------------------

pub const MANIFEST_URL: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const ALLOWED_HOSTS: [&str; 3] = ["https://piston-meta.mojang.com/", "https://piston-data.mojang.com/", "https://launchermeta.mojang.com/"];

/// Only Mojang's own download hosts are used, whatever a downloaded list says.
pub fn allowed_url(u: &str) -> bool { ALLOWED_HOSTS.iter().any(|h| u.starts_with(h)) && !u.contains(char::is_whitespace) && !u.contains("..") }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version { pub id: String, pub kind: String, pub url: String }

pub fn parse_manifest(text: &str) -> Result<(String, Vec<Version>), String> {
    let j = parse_json(text)?;
    let latest = j.get("latest").and_then(|l| l.get("release")).and_then(Json::str).ok_or("no latest release in the list")?.to_string();
    let versions = j.get("versions").and_then(Json::arr).ok_or("no version list")?
        .iter().filter_map(|v| Some(Version { id: v.get("id")?.str()?.to_string(), kind: v.get("type")?.str()?.to_string(), url: v.get("url")?.str()?.to_string() })).collect();
    Ok((latest, versions))
}

#[derive(Clone, Debug, PartialEq)]
pub struct ServerInfo { pub url: String, pub sha1: String, pub size: u64, pub java: u32 }

pub fn parse_version(text: &str) -> Result<ServerInfo, String> {
    let j = parse_json(text)?;
    let d = j.get("downloads").and_then(|d| d.get("server")).ok_or("This version has no server download.")?;
    let url = d.get("url").and_then(Json::str).ok_or("no server address")?.to_string();
    let sha1 = d.get("sha1").and_then(Json::str).ok_or("no checksum")?.to_string();
    if sha1.len() != 40 || !sha1.chars().all(|c| c.is_ascii_hexdigit()) { return Err("the checksum is not valid".into()); }
    let java = j.get("javaVersion").and_then(|v| v.get("majorVersion")).and_then(Json::num).map_or(8, |n| n as u32);
    Ok(ServerInfo { url, sha1: sha1.to_lowercase(), size: d.get("size").and_then(Json::num).unwrap_or(0.0) as u64, java })
}

/// The Alpine package that provides the Java a Minecraft version asks for.
pub fn java_package(major: u32) -> Result<String, String> {
    match major { 8 => Ok("openjdk8-jre-base".into()), 11 | 17 | 21 | 25 => Ok(format!("openjdk{major}-jre-headless")), n => Err(format!("This version needs Java {n}, which is not available here.")) }
}

/// Where Eclipse Temurin publishes its Java builds (used when Alpine's own packages do not have that Java yet).
pub fn adoptium_api(major: u32) -> String { format!("https://api.adoptium.net/v3/assets/latest/{major}/hotspot?architecture=x64&image_type=jre&os=alpine-linux") }

pub fn allowed_java_url(u: &str) -> bool { (u.starts_with("https://api.adoptium.net/") || u.starts_with("https://github.com/adoptium/")) && !u.contains(char::is_whitespace) && !u.contains("..") }

/// (download address, sha256) of the newest Temurin JRE of that major version.
pub fn parse_adoptium(text: &str) -> Result<(String, String), String> {
    let j = parse_json(text)?;
    let pkg = j.arr().and_then(|a| a.first()).and_then(|a| a.get("binary")).and_then(|b| b.get("package")).ok_or("No Java of that version is published for this system.")?;
    let link = pkg.get("link").and_then(Json::str).ok_or("no download address")?.to_string();
    let sum = pkg.get("checksum").and_then(Json::str).ok_or("no checksum")?.to_lowercase();
    if sum.len() != 64 || !sum.chars().all(|c| c.is_ascii_hexdigit()) { return Err("the Java checksum is not valid".into()); }
    Ok((link, sum))
}

pub fn valid_version_id(v: &str) -> bool { !v.is_empty() && v.len() <= 40 && v.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) }

/// 2, 4 or 8 GB, and only if the computer has room for it (a bit more than the heap is used in total).
pub fn ram_choices(total_kb: u64) -> Vec<u32> { [2u32, 4, 8].into_iter().filter(|g| total_kb >= *g as u64 * 1024 * 1024 * 5 / 4).collect() }

pub fn eula_text(accepted: bool) -> String {
    format!("#By changing the setting below to TRUE you are indicating your agreement to our EULA (https://aka.ms/MinecraftEULA).\neula={}\n", accepted)
}

pub fn server_command(java: &str, ram_gb: u32) -> String { format!("{java} -Xmx{ram_gb}G -Xms1G -jar server.jar nogui") }

fn tmp_file(ctx: &Ctx, name: &str) -> std::path::PathBuf { ctx.root.join("run/exec85").join(name) }

fn fetch(ctx: &Ctx, url: &str, to: &Path) -> Result<(), String> {
    if !allowed_url(url) { return Err("That address is not on Mojang's download servers.".into()); }
    if let Some(p) = to.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    let (code, out) = ctx.runner.run("wget", &["-q", "-T", "60", "-O", &to.to_string_lossy(), "--", url]);
    if code != 0 { let _ = std::fs::remove_file(to); return Err(format!("The download failed. Is the network working? {}", out.trim())); }
    Ok(())
}

/// `mc-versions`: "latest <id>" then one "release <id>" line per release (newest first, at most 80).
pub fn versions(ctx: &Ctx) -> Result<String, String> {
    let f = tmp_file(ctx, "mc-manifest.json");
    fetch(ctx, MANIFEST_URL, &f)?;
    let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&f);
    let (latest, vs) = parse_manifest(&text)?;
    let mut out = format!("latest {latest}\n");
    for v in vs.iter().filter(|v| v.kind == "release").take(80) { out.push_str(&format!("release {}\n", v.id)); }
    Ok(out)
}

/// The path as the running system sees it: `p` lies under `root`, and the result always starts with `/` (also when `root` is `/` itself).
pub fn absolute_in(root: &Path, p: &Path) -> String { format!("/{}", p.strip_prefix(root).unwrap_or(p).to_string_lossy().trim_start_matches('/')) }

/// The path of a `java` program of the wanted major version, installing it first if needed: from Alpine's packages when they have it,
/// otherwise Eclipse Temurin's own build, downloaded, checked against its published checksum and unpacked on its own under /opt.
pub fn ensure_java(ctx: &Ctx, major: u32) -> Result<String, String> {
    let rel = |p: &Path| absolute_in(ctx.root, p);
    let find = || [if major == 8 { "java-1.8-openjdk".to_string() } else { format!("java-{major}-openjdk") }].iter().map(|d| ctx.root.join("usr/lib/jvm").join(d).join("bin/java")).chain(Some(ctx.root.join(format!("opt/java-{major}/bin/java")))).find(|p| p.exists());
    if let Ok(pkg) = java_package(major) {
        let (code, _) = ctx.runner.run("apk", &["add", "--no-progress", &pkg]);
        if code == 0 { if let Some(p) = find() { return Ok(rel(&p)); } }
    }
    if let Some(p) = ctx.root.join(format!("opt/java-{major}/bin/java")).exists().then(|| ctx.root.join(format!("opt/java-{major}/bin/java"))) { return Ok(rel(&p)); }
    // Alpine has no such Java: use Temurin's build.
    let api = tmp_file(ctx, "java-api.json");
    let url = adoptium_api(major);
    if !allowed_java_url(&url) { return Err("internal error: bad Java address".into()); }
    if let Some(p) = api.parent() { std::fs::create_dir_all(p).map_err(|e| e.to_string())?; }
    let (code, out) = ctx.runner.run("wget", &["-q", "-T", "60", "-O", &api.to_string_lossy(), "--", &url]);
    if code != 0 { return Err(format!("Java {major} is not in Alpine's packages and the download of its list failed: {}", out.trim())); }
    let (link, sum) = parse_adoptium(&std::fs::read_to_string(&api).map_err(|e| e.to_string())?)?;
    let _ = std::fs::remove_file(&api);
    if !allowed_java_url(&link) { return Err("The Java download address is not on Temurin's servers.".into()); }
    ctx.runner.run("apk", &["add", "--no-progress", "libstdc++", "libgcc", "zlib"]);
    let tgz = ctx.root.join(format!("opt/java-{major}.tar.gz"));
    let dest = ctx.root.join(format!("opt/java-{major}"));
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    let (code, out) = ctx.runner.run("wget", &["-q", "-T", "300", "-O", &tgz.to_string_lossy(), "--", &link]);
    if code != 0 { let _ = std::fs::remove_dir_all(&dest); let _ = std::fs::remove_file(&tgz); return Err(format!("The Java download failed: {}", out.trim())); }
    let (code, out) = ctx.runner.run("sha256sum", &[&tgz.to_string_lossy()]);
    if code != 0 || out.split_whitespace().next().map(str::to_lowercase).as_deref() != Some(sum.as_str()) {
        let _ = std::fs::remove_dir_all(&dest); let _ = std::fs::remove_file(&tgz);
        return Err("The downloaded Java is damaged (its checksum does not match). Nothing was installed; try again.".into());
    }
    let (code, out) = ctx.runner.run("tar", &["-xzf", &tgz.to_string_lossy(), "-C", &dest.to_string_lossy(), "--strip-components=1"]);
    let _ = std::fs::remove_file(&tgz);
    if code != 0 || !dest.join("bin/java").exists() { let _ = std::fs::remove_dir_all(&dest); return Err(format!("Unpacking Java failed: {}", out.trim())); }
    Ok(rel(&dest.join("bin/java")))
}

pub struct Create<'a> { pub name: &'a str, pub version: &'a str, pub ram_gb: u32, pub eula: bool, pub user: &'a str }

/// Download the server, install its Java and define (and, with the EULA accepted, start) the service.
pub fn create(ctx: &Ctx, c: &Create) -> Result<String, String> {
    services::valid_name(c.name).map_err(String::from)?;
    if !valid_version_id(c.version) { return Err("That is not a valid version.".into()); }
    let mem_kb = std::fs::read_to_string(ctx.root.join("proc/meminfo")).unwrap_or_default().lines().find(|l| l.starts_with("MemTotal:")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
    if !ram_choices(mem_kb).contains(&c.ram_gb) { return Err(format!("This computer does not have enough memory for {} GB. Choose from: {:?}.", c.ram_gb, ram_choices(mem_kb))); }
    if ctx.root.join("etc/exec85/services").join(format!("{}.conf", c.name)).exists() { return Err(format!("A service called {} already exists.", c.name)); }
    let dir = ctx.root.join("srv/minecraft").join(c.name);
    if dir.exists() { return Err(format!("The folder {} already exists.", dir.display())); }

    let mf = tmp_file(ctx, "mc-manifest.json");
    fetch(ctx, MANIFEST_URL, &mf)?;
    let (_, vs) = parse_manifest(&std::fs::read_to_string(&mf).map_err(|e| e.to_string())?)?;
    let _ = std::fs::remove_file(&mf);
    let v = vs.iter().find(|v| v.id == c.version).ok_or_else(|| format!("Version {} is not in Mojang's list.", c.version))?;
    let vf = tmp_file(ctx, "mc-version.json");
    fetch(ctx, &v.url, &vf)?;
    let info = parse_version(&std::fs::read_to_string(&vf).map_err(|e| e.to_string())?)?;
    let _ = std::fs::remove_file(&vf);
    let java = ensure_java(ctx, info.java)?;

    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let jar = dir.join("server.jar");
    if let Err(e) = fetch(ctx, &info.url, &jar) { let _ = std::fs::remove_dir_all(&dir); return Err(e); }
    let (code, out) = ctx.runner.run("sha1sum", &[&jar.to_string_lossy()]);
    if code != 0 || out.split_whitespace().next().map(str::to_lowercase).as_deref() != Some(info.sha1.as_str()) {
        let _ = std::fs::remove_dir_all(&dir);
        return Err("The downloaded file is damaged (its checksum does not match). Nothing was installed; try again.".into());
    }
    std::fs::write(dir.join("eula.txt"), eula_text(c.eula)).map_err(|e| e.to_string())?;
    let (code, out) = ctx.runner.run("chown", &["-R", c.user, &dir.to_string_lossy()]);
    if code != 0 { return Err(format!("Could not give the folder to {}: {}", c.user, out.trim())); }

    let def = ServiceDef { name: c.name.into(), description: format!("Minecraft server {}", c.version), command: server_command(&java, c.ram_gb),
        dir: format!("/srv/minecraft/{}", c.name), user: c.user.into(), env: vec![], autostart: c.eula };
    crate::helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&def)])?;
    if c.eula {
        crate::helper::handle(ctx, &["svc-start".to_string(), c.name.to_string()])?;
        Ok(format!("Minecraft {} is installed and starting (it takes a minute). Players connect to this computer's address on port 25565.", c.version))
    } else {
        Ok(format!("Minecraft {} is installed but will not start until the EULA is accepted: run it again with the EULA accepted, or set eula=true in /srv/minecraft/{}/eula.txt.", c.version, c.name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    #[test]
    fn json_reader_handles_the_shapes_mojang_uses() {
        let j = parse_json(r#" {"a": [1, 2.5, -3e2, true, false, null], "s": "x\"y\\zé😀\n", "o": {"k": "v"}, "e": []} "#).unwrap();
        assert_eq!(j.get("a").unwrap().arr().unwrap().len(), 6);
        assert_eq!(j.get("a").unwrap().arr().unwrap()[2], Json::Num(-300.0));
        assert_eq!(j.get("s").unwrap().str().unwrap(), "x\"y\\zé😀\n");
        assert_eq!(j.get("o").unwrap().get("k").unwrap().str(), Some("v"));
        for bad in ["", "{", "[1,", r#"{"a" 1}"#, r#""open"#, "tru", "{} extra", r#""\x""#] { assert!(parse_json(bad).is_err(), "{bad:?}"); }
        let deep = "[".repeat(200) + &"]".repeat(200);
        assert!(parse_json(&deep).is_err(), "absurd nesting is refused, not a crash");
    }

    const MANIFEST: &str = r#"{"latest":{"release":"1.21.5","snapshot":"25w01a"},"versions":[
        {"id":"25w01a","type":"snapshot","url":"https://piston-meta.mojang.com/v1/packages/aa/25w01a.json"},
        {"id":"1.21.5","type":"release","url":"https://piston-meta.mojang.com/v1/packages/bb/1.21.5.json"},
        {"id":"1.8.9","type":"release","url":"https://piston-meta.mojang.com/v1/packages/cc/1.8.9.json"}]}"#;
    const VERSION: &str = r#"{"downloads":{"client":{"url":"https://piston-data.mojang.com/c.jar"},"server":{"sha1":"0123456789abcdef0123456789abcdef01234567","size":1000,"url":"https://piston-data.mojang.com/v1/objects/0123/server.jar"}},"javaVersion":{"component":"x","majorVersion":21}}"#;

    #[test]
    fn mojang_lists_are_understood() {
        let (latest, vs) = parse_manifest(MANIFEST).unwrap();
        assert_eq!(latest, "1.21.5");
        assert_eq!(vs.len(), 3);
        assert_eq!(vs.iter().filter(|v| v.kind == "release").map(|v| v.id.as_str()).collect::<Vec<_>>(), ["1.21.5", "1.8.9"]);
        let s = parse_version(VERSION).unwrap();
        assert_eq!((s.java, s.size), (21, 1000));
        assert!(s.url.ends_with("server.jar"));
        assert_eq!(parse_version(r#"{"downloads":{"client":{}}}"#).unwrap_err(), "This version has no server download.");
        assert!(parse_version(&VERSION.replace("0123456789abcdef0123456789abcdef01234567", "zz")).is_err(), "a bad checksum is refused");
        assert_eq!(parse_version(r#"{"downloads":{"server":{"sha1":"0123456789abcdef0123456789abcdef01234567","url":"u"}}}"#).unwrap().java, 8, "old versions without a Java entry use Java 8");
    }

    #[test]
    fn only_mojang_addresses_are_downloaded() {
        assert!(allowed_url("https://piston-data.mojang.com/v1/objects/x/server.jar"));
        for bad in ["http://piston-data.mojang.com/x", "https://evil.example/x", "https://piston-data.mojang.com.evil.example/x", "file:///etc/passwd", "https://piston-meta.mojang.com/a b", "https://piston-meta.mojang.com/../x", ""] { assert!(!allowed_url(bad), "{bad}"); }
    }

    #[test]
    fn java_ram_and_eula_choices() {
        assert_eq!(java_package(21).unwrap(), "openjdk21-jre-headless");
        assert_eq!(java_package(8).unwrap(), "openjdk8-jre-base");
        assert!(java_package(13).is_err());
        assert_eq!(ram_choices(1_000_000), Vec::<u32>::new(), "a 1 GB computer is offered nothing");
        assert_eq!(ram_choices(3_000_000), vec![2]);
        assert_eq!(ram_choices(6_000_000), vec![2, 4]);
        assert_eq!(ram_choices(16_000_000), vec![2, 4, 8]);
        assert!(eula_text(true).ends_with("eula=true\n") && eula_text(false).ends_with("eula=false\n") && eula_text(true).contains("aka.ms/MinecraftEULA"));
        assert_eq!(server_command("/usr/lib/jvm/java-21-openjdk/bin/java", 4), "/usr/lib/jvm/java-21-openjdk/bin/java -Xmx4G -Xms1G -jar server.jar nogui");
        assert!(valid_version_id("1.21.5") && valid_version_id("25w01a") && !valid_version_id("../x") && !valid_version_id("a b") && !valid_version_id(""));
    }

    /// A fake world: wget serves canned files, sha1sum answers, everything else succeeds.
    struct Fake { calls: RefCell<Vec<String>>, sha: String, java: u32, alpine_has_java: bool, java_sha: String }
    fn fake(sha: &str) -> Fake { Fake { calls: RefCell::new(vec![]), sha: sha.into(), java: 21, alpine_has_java: true, java_sha: "a".repeat(64) } }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            match c {
                "wget" => {
                    let (to, url) = (a[a.iter().position(|x| *x == "-O").unwrap() + 1], *a.last().unwrap());
                    let body = if url == MANIFEST_URL { MANIFEST.to_string() }
                        else if url.contains("1.21.5.json") { VERSION.replace("\"majorVersion\":21", &format!("\"majorVersion\":{}", self.java)) }
                        else if url.contains("api.adoptium.net") { format!(r#"[{{"binary":{{"package":{{"link":"https://github.com/adoptium/temurin25-binaries/releases/download/x/jre.tar.gz","checksum":"{}"}}}}}}]"#, "a".repeat(64)) }
                        else { "JAR".to_string() };
                    std::fs::write(to, body).unwrap(); (0, String::new())
                }
                "sha1sum" => (0, format!("{}  {}\n", self.sha, a[0])),
                "sha256sum" => (0, format!("{}  {}\n", self.java_sha, a[0])),
                "tar" => { let dest = a[a.iter().position(|x| *x == "-C").unwrap() + 1]; std::fs::create_dir_all(format!("{dest}/bin")).unwrap(); std::fs::write(format!("{dest}/bin/java"), "").unwrap(); (0, String::new()) }
                "apk" => if !self.alpine_has_java && a.iter().any(|x| x.starts_with("openjdk")) { (1, "ERROR: unable to select packages".into()) } else { (0, "OK".into()) },
                _ => (0, String::new()),
            }
        }
    }

    fn world(tag: &str, mem_kb: u64) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-mc-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap(); std::fs::create_dir_all(d.join("proc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\ntester:x:1000:1000::/home/tester:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:tester\n").unwrap();
        std::fs::write(d.join("proc/meminfo"), format!("MemTotal:       {mem_kb} kB\n")).unwrap();
        std::fs::create_dir_all(d.join("usr/lib/jvm/java-21-openjdk/bin")).unwrap();
        std::fs::write(d.join("usr/lib/jvm/java-21-openjdk/bin/java"), "").unwrap();
        d
    }

    const GOOD_SHA: &str = "0123456789abcdef0123456789abcdef01234567";

    #[test]
    fn creating_a_server_downloads_checks_and_defines_a_service() {
        let r = world("ok", 16_000_000); let f = fake(GOOD_SHA); let ctx = Ctx { root: &r, runner: &f };
        let msg = create(&ctx, &Create { name: "mc", version: "1.21.5", ram_gb: 4, eula: true, user: "tester" }).unwrap();
        assert!(msg.contains("starting"), "{msg}");
        assert_eq!(std::fs::read_to_string(r.join("srv/minecraft/mc/eula.txt")).unwrap().lines().last(), Some("eula=true"));
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/mc.conf")).unwrap();
        assert!(conf.contains("-Xmx4G") && conf.contains("-jar server.jar nogui") && conf.contains("/usr/lib/jvm/java-21-openjdk/bin/java") && conf.contains("dir=/srv/minecraft/mc") && conf.contains("autostart=true"), "{conf}");
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("apk add --no-progress openjdk21-jre-headless") && calls.contains("chown -R tester ") && calls.contains("rc-service exec85-mc start"), "{calls}");
        assert!(r.join("etc/init.d/exec85-mc").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_java_alpine_does_not_have_is_downloaded_from_temurin_and_checked() {
        let r = world("temurin", 16_000_000); let mut f = fake(GOOD_SHA); f.java = 25; f.alpine_has_java = false;
        let ctx = Ctx { root: &r, runner: &f };
        create(&ctx, &Create { name: "mc", version: "1.21.5", ram_gb: 4, eula: true, user: "tester" }).unwrap();
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/mc.conf")).unwrap();
        assert!(conf.contains("/opt/java-25/bin/java -Xmx4G"), "{conf}");
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("api.adoptium.net/v3/assets/latest/25/") && calls.contains("sha256sum") && calls.contains("tar -xzf"), "{calls}");
        assert!(!r.join("opt/java-25.tar.gz").exists(), "the download is removed after unpacking");
        let _ = std::fs::remove_dir_all(&r);
        // a damaged Java download installs nothing
        let r = world("temurin-bad", 16_000_000); let mut f = fake(GOOD_SHA); f.java = 25; f.alpine_has_java = false; f.java_sha = "b".repeat(64);
        let e = create(&Ctx { root: &r, runner: &f }, &Create { name: "mc", version: "1.21.5", ram_gb: 4, eula: true, user: "tester" }).unwrap_err();
        assert!(e.contains("damaged") && !r.join("opt/java-25").exists() && !r.join("srv/minecraft/mc").exists(), "{e}");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn paths_keep_their_leading_slash_even_when_the_root_is_slash() {
        assert_eq!(absolute_in(Path::new("/"), Path::new("/opt/java-25/bin/java")), "/opt/java-25/bin/java");
        assert_eq!(absolute_in(Path::new("/tmp/w"), Path::new("/tmp/w/opt/java-25/bin/java")), "/opt/java-25/bin/java");
        assert_eq!(absolute_in(Path::new("/"), Path::new("/usr/lib/jvm/java-21-openjdk/bin/java")), "/usr/lib/jvm/java-21-openjdk/bin/java");
    }

    #[test]
    fn temurin_answers_and_addresses_are_checked() {
        let good = format!(r#"[{{"binary":{{"package":{{"link":"https://github.com/adoptium/x/a.tar.gz","checksum":"{}"}}}}}}]"#, "c".repeat(64));
        assert_eq!(parse_adoptium(&good).unwrap().1, "c".repeat(64));
        assert!(parse_adoptium("[]").is_err() && parse_adoptium("{}").is_err());
        assert!(parse_adoptium(&good.replace(&"c".repeat(64), "short")).is_err());
        assert!(allowed_java_url("https://github.com/adoptium/temurin25-binaries/x") && allowed_java_url(&adoptium_api(25)));
        for bad in ["https://github.com/evil/x", "http://api.adoptium.net/x", "https://api.adoptium.net.evil.example/x", "https://github.com/adoptium/../x"] { assert!(!allowed_java_url(bad), "{bad}"); }
    }

    #[test]
    fn without_the_eula_nothing_is_started() {
        let r = world("noeula", 16_000_000); let f = fake(GOOD_SHA); let ctx = Ctx { root: &r, runner: &f };
        let msg = create(&ctx, &Create { name: "mc", version: "1.21.5", ram_gb: 2, eula: false, user: "tester" }).unwrap();
        assert!(msg.contains("EULA"), "{msg}");
        assert!(std::fs::read_to_string(r.join("srv/minecraft/mc/eula.txt")).unwrap().ends_with("eula=false\n"));
        assert!(std::fs::read_to_string(r.join("etc/exec85/services/mc.conf")).unwrap().contains("autostart=false"));
        assert!(!f.calls.borrow().iter().any(|c| c.contains("rc-service exec85-mc start")), "not started");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_damaged_download_installs_nothing() {
        let r = world("bad", 16_000_000); let f = fake(&"f".repeat(40)); let ctx = Ctx { root: &r, runner: &f };
        let e = create(&ctx, &Create { name: "mc", version: "1.21.5", ram_gb: 2, eula: true, user: "tester" }).unwrap_err();
        assert!(e.contains("damaged"), "{e}");
        assert!(!r.join("srv/minecraft/mc").exists() && !r.join("etc/exec85/services/mc.conf").exists());
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn bad_requests_are_refused_before_anything_is_downloaded() {
        let r = world("refuse", 3_000_000); let f = fake(GOOD_SHA); let ctx = Ctx { root: &r, runner: &f };
        let mk = |n: &'static str, v: &'static str, ram, u: &'static str| Create { name: n, version: v, ram_gb: ram, eula: true, user: u };
        assert!(create(&ctx, &mk("mc", "1.21.5", 8, "tester")).unwrap_err().contains("enough memory"), "8 GB does not fit a 3 GB computer");
        assert!(create(&ctx, &mk("mc", "1.21.5", 3, "tester")).is_err(), "only 2, 4 or 8");
        assert!(create(&ctx, &mk("../x", "1.21.5", 2, "tester")).is_err());
        assert!(create(&ctx, &mk("mc", "../../etc", 2, "tester")).is_err());
        assert!(create(&ctx, &mk("mc", "9.9.9", 2, "tester")).unwrap_err().contains("not in Mojang's list"));
        assert!(!f.calls.borrow().iter().any(|c| c.starts_with("apk") || c.starts_with("chown")), "no software was installed");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_version_list_shows_releases_newest_first() {
        let r = world("list", 8_000_000); let f = fake(GOOD_SHA); let ctx = Ctx { root: &r, runner: &f };
        assert_eq!(versions(&ctx).unwrap(), "latest 1.21.5\nrelease 1.21.5\nrelease 1.8.9\n");
        let _ = std::fs::remove_dir_all(&r);
    }
}
