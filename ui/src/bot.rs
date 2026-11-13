//! Discord bot quick-installer. Like the Minecraft helper it is only a preset on top of generic parts: install a
//! runtime (`apk`), put files in a folder, keep a secret in a private file, define a service.
//! The bot's token is never put on a command line, in the service definition (which everyone can read) or in a log:
//! it is written to a file in the bot's folder that only the bot's user can read.
use crate::helper::Ctx;
use crate::services::{self, ServiceDef};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang { Python, Node }

impl Lang {
    pub fn parse(s: &str) -> Option<Lang> { match s.to_lowercase().as_str() { "python" | "py" => Some(Lang::Python), "node" | "nodejs" | "js" => Some(Lang::Node), _ => None } }
    pub fn id(self) -> &'static str { match self { Lang::Python => "python", Lang::Node => "node" } }
    pub fn label(self) -> &'static str { match self { Lang::Python => "Python (discord.py)", Lang::Node => "Node.js (discord.js)" } }
    pub fn packages(self) -> &'static [&'static str] { match self { Lang::Python => &["python3", "py3-pip"], Lang::Node => &["nodejs", "npm"] } }
}

/// Where the bot's code comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source { Starter, Folder(String), Git(String) }

pub const STARTER_PY: &str = r#"# A small starter bot. It answers "!ping" with "pong". Change it however you like.
import os
import discord

def read_token():
    for line in open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", ".env")):
        if line.startswith("DISCORD_TOKEN="):
            return line.split("=", 1)[1].strip()
    raise SystemExit("No DISCORD_TOKEN in .env")

intents = discord.Intents.default()
intents.message_content = True
client = discord.Client(intents=intents)

@client.event
async def on_ready():
    print(f"Logged in as {client.user}", flush=True)

@client.event
async def on_message(message):
    if message.author == client.user:
        return
    if message.content.strip() == "!ping":
        await message.channel.send("pong")

client.run(read_token())
"#;

pub const STARTER_JS: &str = r#"// A small starter bot. It answers "!ping" with "pong". Change it however you like.
const fs = require("fs");
const path = require("path");
const { Client, GatewayIntentBits } = require("discord.js");

function readToken() {
  const env = fs.readFileSync(path.join(__dirname, "..", ".env"), "utf8");
  for (const line of env.split("\n")) if (line.startsWith("DISCORD_TOKEN=")) return line.slice(14).trim();
  throw new Error("No DISCORD_TOKEN in .env");
}

const client = new Client({ intents: [GatewayIntentBits.Guilds, GatewayIntentBits.GuildMessages, GatewayIntentBits.MessageContent] });
client.once("ready", () => console.log(`Logged in as ${client.user.tag}`));
client.on("messageCreate", (m) => { if (!m.author.bot && m.content.trim() === "!ping") m.channel.send("pong"); });
client.login(readToken());
"#;

/// A token looks like three parts joined by dots; this only rejects things that cannot be one (spaces, quotes, line breaks ...).
pub fn plausible_token(t: &str) -> bool { t.len() >= 20 && t.len() <= 200 && t.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) }

/// Only addresses a person would clone from: https, no spaces, no option-looking text.
pub fn valid_git_url(u: &str) -> bool { u.starts_with("https://") && u.len() < 300 && !u.contains(char::is_whitespace) && !u.contains("..") && !u.contains('\'') && !u.contains('"') }

pub fn starter(lang: Lang) -> (&'static str, &'static str) { match lang { Lang::Python => ("bot.py", STARTER_PY), Lang::Node => ("bot.js", STARTER_JS) } }

/// The command that runs the bot, from the files it has: `(command line, entry file)`.
pub fn run_command(lang: Lang, dir: &Path) -> Result<String, String> {
    match lang {
        Lang::Python => {
            let entry = ["bot.py", "main.py", "app.py", "__main__.py"].iter().find(|f| dir.join(f).exists()).ok_or("No bot.py, main.py or app.py found in the bot's files.")?;
            Ok(format!("/srv/bots/{}/venv/bin/python {entry}", dir.parent().and_then(|p| p.file_name()).map_or("bot".into(), |n| n.to_string_lossy().into_owned())))
        }
        Lang::Node => {
            let entry = ["bot.js", "index.js", "main.js", "app.js"].iter().find(|f| dir.join(f).exists()).ok_or("No bot.js, index.js or main.js found in the bot's files.")?;
            Ok(format!("node {entry}"))
        }
    }
}

pub struct Create<'a> { pub name: &'a str, pub lang: Lang, pub source: Source, pub token: &'a str, pub user: &'a str }

pub fn create(ctx: &Ctx, c: &Create) -> Result<String, String> {
    services::valid_name(c.name).map_err(String::from)?;
    if !plausible_token(c.token) { return Err("That does not look like a bot token (it is one long piece of letters, digits, dots and dashes).".into()); }
    match &c.source { Source::Git(u) if !valid_git_url(u) => return Err("The Git address must start with https:// and have no spaces.".into()), Source::Folder(f) if !f.starts_with('/') || f.contains("..") => return Err("The folder must be a full path starting with /.".into()), _ => {} }
    if ctx.root.join("etc/exec85/services").join(format!("{}.conf", c.name)).exists() { return Err(format!("A service called {} already exists.", c.name)); }
    let base = ctx.root.join("srv/bots").join(c.name);
    if base.exists() { return Err(format!("The folder {} already exists.", base.display())); }
    let app = base.join("app");

    // 1. the runtime (and Git, if the code comes from there)
    let mut pkgs: Vec<&str> = c.lang.packages().to_vec();
    if matches!(c.source, Source::Git(_)) { pkgs.push("git"); }
    let mut args = vec!["add", "--no-progress"]; args.extend(pkgs);
    let (code, out) = ctx.runner.run("apk", &args);
    if code != 0 { return Err(format!("Installing {} failed: {}", c.lang.label(), out.lines().last().unwrap_or("").trim())); }

    let cleanup = || { let _ = std::fs::remove_dir_all(&base); };
    std::fs::create_dir_all(&app).map_err(|e| e.to_string())?;

    // 2. the code
    match &c.source {
        Source::Starter => { let (file, body) = starter(c.lang); std::fs::write(app.join(file), body).map_err(|e| e.to_string())?; }
        Source::Folder(f) => {
            let (code, out) = ctx.runner.run("cp", &["-a", &format!("{}/.", f.trim_end_matches('/')), &app.to_string_lossy()]);
            if code != 0 { cleanup(); return Err(format!("Could not copy {f}: {}", out.trim())); }
        }
        Source::Git(u) => {
            let (code, out) = ctx.runner.run("git", &["clone", "--depth", "1", "--", u, &app.to_string_lossy()]);
            if code != 0 { cleanup(); return Err(format!("Could not download {u}: {}", out.lines().last().unwrap_or("").trim())); }
        }
    }
    let command = match run_command(c.lang, &app) { Ok(x) => x, Err(e) => { cleanup(); return Err(e); } };

    // 3. the library (and the bot's own requirements, if it has a list)
    let dir = app.to_string_lossy().into_owned();
    let steps: Vec<(&str, Vec<String>)> = match c.lang {
        Lang::Python => {
            let venv = base.join("venv").to_string_lossy().into_owned();
            let pip = format!("{venv}/bin/pip");
            let mut v = vec![("python3", vec!["-m".into(), "venv".into(), venv.clone()]), ("pip", vec![pip.clone(), "install".into(), "--no-cache-dir".into(), "discord.py".into()])];
            if app.join("requirements.txt").exists() { v.push(("pip", vec![pip, "install".into(), "--no-cache-dir".into(), "-r".into(), format!("{dir}/requirements.txt")])); }
            v
        }
        Lang::Node => {
            let mut v = vec![("npm", vec!["install".into(), "--prefix".into(), dir.clone(), "--no-audit".into(), "--no-fund".into(), "discord.js".into()])];
            if app.join("package.json").exists() { v.push(("npm", vec!["install".into(), "--prefix".into(), dir.clone(), "--no-audit".into(), "--no-fund".into()])); }
            v
        }
    };
    for (tool, a) in steps {
        let real = if tool == "pip" { a[0].as_str() } else { tool };
        let rest: Vec<&str> = if tool == "pip" { a[1..].iter().map(String::as_str).collect() } else { a.iter().map(String::as_str).collect() };
        let (code, out) = ctx.runner.run(real, &rest);
        if code != 0 { cleanup(); return Err(format!("Installing the Discord library failed: {}", out.lines().last().unwrap_or("").trim())); }
    }

    // 4. the secret, in a file nobody else can read (written before the folder is handed to the bot's user)
    let env = base.join(".env");
    std::fs::write(&env, format!("DISCORD_TOKEN={}\n", c.token)).map_err(|e| e.to_string())?;
    std::fs::set_permissions(&env, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
    let (code, out) = ctx.runner.run("chown", &["-R", c.user, &base.to_string_lossy()]);
    if code != 0 { cleanup(); return Err(format!("Could not give the folder to {}: {}", c.user, out.trim())); }

    // 5. the service
    let def = ServiceDef { name: c.name.into(), description: format!("Discord bot ({})", c.lang.label()), command, dir: crate::mc::absolute_in(ctx.root, &app), user: c.user.into(), env: vec![], autostart: true };
    if let Err(e) = crate::helper::handle(ctx, &["svc-save".to_string(), services::render_conf(&def)]) { cleanup(); return Err(e); }
    crate::helper::handle(ctx, &["svc-start".to_string(), c.name.to_string()])?;
    Ok(format!("Your Discord bot {} is installed and starting. Invite it to your server from the Discord developer portal, then type !ping in a channel it can read.", c.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::Runner;
    use std::cell::RefCell;
    use std::path::PathBuf;

    struct Fake { calls: RefCell<Vec<String>>, fail: RefCell<Option<&'static str>> }
    fn fake() -> Fake { Fake { calls: RefCell::new(vec![]), fail: RefCell::new(None) } }
    impl Runner for Fake {
        fn run(&self, c: &str, a: &[&str]) -> (i32, String) {
            self.calls.borrow_mut().push(format!("{c} {}", a.join(" ")));
            if self.fail.borrow().map_or(false, |f| c == f || a.contains(&f)) { return (1, format!("{c} failed badly")); }
            match c {
                "cp" => { let (from, to) = (a[a.len() - 2].trim_end_matches("/."), a[a.len() - 1]); for e in std::fs::read_dir(from).unwrap().flatten() { std::fs::copy(e.path(), format!("{to}/{}", e.file_name().to_string_lossy())).unwrap(); } (0, String::new()) }
                "git" => { let to = a[a.len() - 1]; std::fs::create_dir_all(to).unwrap(); std::fs::write(format!("{to}/index.js"), "// cloned").unwrap(); std::fs::write(format!("{to}/package.json"), "{}").unwrap(); (0, String::new()) }
                _ => (0, String::new()),
            }
        }
    }

    const TOKEN: &str = "fake-token-for-tests-only-0123456789";

    fn world(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-bot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc")).unwrap();
        std::fs::write(d.join("etc/passwd"), "root:x:0:0::/root:/bin/ash\ntester:x:1000:1000::/home/tester:/bin/ash\n").unwrap();
        std::fs::write(d.join("etc/group"), "wheel:x:10:tester\n").unwrap();
        d
    }

    #[test]
    fn tokens_and_addresses_are_checked() {
        assert!(plausible_token(TOKEN));
        for bad in ["", "short", "has space in it 123456789012345", "quote'inside_1234567890123456", "new\nline_1234567890123456", &"x".repeat(300)] { assert!(!plausible_token(bad), "{bad:?}"); }
        assert!(valid_git_url("https://github.com/me/my-bot.git"));
        for bad in ["http://x/y", "git@github.com:me/x.git", "https://x/y z", "https://x/../y", "--upload-pack=evil", "https://x/'y"] { assert!(!valid_git_url(bad), "{bad}"); }
        assert_eq!(Lang::parse("Python"), Some(Lang::Python)); assert_eq!(Lang::parse("js"), Some(Lang::Node)); assert!(Lang::parse("ruby").is_none());
    }

    #[test]
    fn a_python_starter_bot_is_installed_with_a_private_token() {
        let r = world("py"); let f = fake(); let ctx = Ctx { root: &r, runner: &f };
        let msg = create(&ctx, &Create { name: "helper", lang: Lang::Python, source: Source::Starter, token: TOKEN, user: "tester" }).unwrap();
        assert!(msg.contains("!ping"), "{msg}");
        let bot = std::fs::read_to_string(r.join("srv/bots/helper/app/bot.py")).unwrap();
        assert!(bot.contains("!ping") && bot.contains("pong") && !bot.contains(TOKEN), "the code never contains the token");
        let env = r.join("srv/bots/helper/.env");
        assert_eq!(std::fs::read_to_string(&env).unwrap(), format!("DISCORD_TOKEN={TOKEN}\n"));
        assert_eq!(std::fs::metadata(&env).unwrap().permissions().mode() & 0o777, 0o600, "only its owner can read the token file");
        let conf = std::fs::read_to_string(r.join("etc/exec85/services/helper.conf")).unwrap();
        assert!(!conf.contains(TOKEN), "the token is not in the service definition, which everyone can read");
        assert!(conf.contains("/srv/bots/helper/venv/bin/python bot.py") && conf.contains("dir=/srv/bots/helper/app") && conf.contains("autostart=true"), "{conf}");
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("apk add --no-progress python3 py3-pip") && calls.contains("python3 -m venv ") && calls.contains("pip install --no-cache-dir discord.py") && calls.contains("chown -R tester ") && calls.contains("rc-service exec85-helper start"), "{calls}");
        assert!(!calls.contains(TOKEN), "the token never appears on a command line");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_node_bot_from_git_and_a_python_bot_from_a_folder() {
        let r = world("node"); let f = fake(); let ctx = Ctx { root: &r, runner: &f };
        create(&ctx, &Create { name: "gitbot", lang: Lang::Node, source: Source::Git("https://github.com/me/bot.git".into()), token: TOKEN, user: "tester" }).unwrap();
        let calls = f.calls.borrow().join("\n");
        assert!(calls.contains("apk add --no-progress nodejs npm git") && calls.contains("git clone --depth 1 -- https://github.com/me/bot.git ") && calls.contains("npm install --prefix") && calls.contains("discord.js"), "{calls}");
        assert!(std::fs::read_to_string(r.join("etc/exec85/services/gitbot.conf")).unwrap().contains("node index.js"));
        assert!(calls.matches("npm install").count() == 2, "the bot's own package.json is installed too");
        let _ = std::fs::remove_dir_all(&r);

        let r = world("folder"); let f = fake(); let ctx = Ctx { root: &r, runner: &f };
        let mine = r.join("mybot"); std::fs::create_dir_all(&mine).unwrap(); std::fs::write(mine.join("main.py"), "print('hi')").unwrap(); std::fs::write(mine.join("requirements.txt"), "aiohttp").unwrap();
        create(&ctx, &Create { name: "own", lang: Lang::Python, source: Source::Folder(mine.to_string_lossy().into_owned()), token: TOKEN, user: "tester" }).unwrap();
        assert!(r.join("srv/bots/own/app/main.py").exists());
        assert!(std::fs::read_to_string(r.join("etc/exec85/services/own.conf")).unwrap().contains("python main.py"));
        assert!(f.calls.borrow().join("\n").contains("-r "), "requirements.txt is installed");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn failures_leave_nothing_behind_and_bad_requests_run_nothing() {
        let r = world("fail"); let f = fake(); let ctx = Ctx { root: &r, runner: &f };
        *f.fail.borrow_mut() = Some("discord.py");
        let e = create(&ctx, &Create { name: "b", lang: Lang::Python, source: Source::Starter, token: TOKEN, user: "tester" }).unwrap_err();
        assert!(e.contains("Discord library failed") && !r.join("srv/bots/b").exists() && !r.join("etc/exec85/services/b.conf").exists(), "{e}");
        let _ = std::fs::remove_dir_all(&r);
        let r = world("refuse"); let f = fake(); let ctx = Ctx { root: &r, runner: &f };
        let mk = |n: &'static str, t: &'static str, s: Source| Create { name: n, lang: Lang::Python, source: s, token: t, user: "tester" };
        assert!(create(&ctx, &mk("../x", TOKEN, Source::Starter)).is_err());
        assert!(create(&ctx, &mk("b", "not a token", Source::Starter)).is_err());
        assert!(create(&ctx, &mk("b", TOKEN, Source::Git("--upload-pack=x".into()))).is_err());
        assert!(create(&ctx, &mk("b", TOKEN, Source::Folder("relative".into()))).is_err());
        assert!(create(&ctx, &mk("b", TOKEN, Source::Folder("/a/../etc".into()))).is_err());
        assert!(f.calls.borrow().is_empty(), "nothing was run for refused requests");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn the_starter_code_is_valid_and_reads_its_token_from_the_private_file() {
        assert!(STARTER_PY.contains(".env") && STARTER_PY.contains("on_message") && !STARTER_PY.contains("fake-token"));
        assert!(STARTER_JS.contains(".env") && STARTER_JS.contains("messageCreate"));
        let out = std::process::Command::new("python3").args(["-c", &format!("import ast,sys; ast.parse(sys.argv[1])"), STARTER_PY]).output();
        if let Ok(o) = out { assert!(o.status.success(), "the Python starter parses: {}", String::from_utf8_lossy(&o.stderr)); }
    }
}
