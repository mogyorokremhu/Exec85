//! Discord bot: pick Python or Node.js, pick where the code comes from (a starter bot, your own folder or a Git
//! address), type the bot's name and token (the token is hidden), and install. The privileged helper does the work;
//! the token is only ever sent to it, never shown, never kept in this app after the install, and never in the state line.
use super::{edit_text, Application, Backend, Env, Job, Kind, Out};
use crate::bot::Lang;
use crate::keys::{code, Key};
use crate::layout::Rect;
use crate::paint::Painter;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Step { Lang, Code, Source, Details, Working, Done }

const CODES: [(&str, &str, &str); 3] = [("starter", "Starter bot", "a small bot that answers !ping, to begin with"), ("folder", "My own bot", "copy the files from a folder on this computer"), ("git", "From Git", "download a bot from a Git address (https://...)")];

pub struct DiscordBot {
    be: Backend,
    job: Job,
    step: Step,
    lang_sel: usize,
    code_sel: usize,
    source: String,
    name: String,
    token: String,
    focus: usize, // on the details screen: 0 name, 1 token, 2 install
    msg: String,
}

impl DiscordBot {
    pub fn new(be: Backend) -> DiscordBot {
        DiscordBot { be, job: Job::new(), step: Step::Lang, lang_sel: 0, code_sel: 0, source: String::new(), name: "mybot".into(), token: String::new(), focus: 0, msg: String::new() }
    }

    fn lang(&self) -> Lang { [Lang::Python, Lang::Node][self.lang_sel] }
    fn kind(&self) -> &'static str { CODES[self.code_sel].0 }

    fn finish(&mut self) {
        let Some((_, r)) = self.job.poll() else { return };
        match r {
            Ok(m) => { self.step = Step::Done; self.msg = m; self.token.clear(); }
            Err(e) => { self.step = Step::Details; self.msg = e; }
        }
    }

    fn install(&mut self, env: &Env) {
        let user = std::env::var("USER").unwrap_or_default();
        let src = if self.kind() == "starter" { "-".to_string() } else { self.source.trim().to_string() };
        let words = ["bot-create", &self.name, self.lang().id(), self.kind(), &src, &self.token, &user].iter().map(|s| s.to_string()).collect();
        if self.job.start(env, &self.be, "create", words) { self.step = Step::Working; self.msg = "Installing the Discord library and setting up the bot. This can take a few minutes...".into(); }
    }
}

impl Application for DiscordBot {
    fn kind(&self) -> Kind { Kind::DiscordBot }
    fn title(&self) -> String { "Discord bot".into() }
    fn interval(&self) -> Option<Duration> { Some(if self.job.busy { Duration::from_millis(400) } else { Duration::from_secs(60) }) }
    fn tick(&mut self, _env: &Env) { self.finish(); }

    fn handle(&mut self, k: Key, env: &Env) -> Out {
        self.finish();
        self.msg = if self.step == Step::Working || self.step == Step::Done { self.msg.clone() } else { String::new() };
        match self.step {
            Step::Lang => match k.code {
                code::UP | code::LEFT => self.lang_sel = self.lang_sel.saturating_sub(1),
                code::DOWN | code::RIGHT => self.lang_sel = (self.lang_sel + 1).min(1),
                code::ENTER => self.step = Step::Code,
                _ => {}
            },
            Step::Code => match k.code {
                code::UP => self.code_sel = self.code_sel.saturating_sub(1),
                code::DOWN => self.code_sel = (self.code_sel + 1).min(CODES.len() - 1),
                code::ESC | code::BACKSPACE => self.step = Step::Lang,
                code::ENTER => { self.step = if self.kind() == "starter" { Step::Details } else { Step::Source }; self.focus = 0; }
                _ => {}
            },
            Step::Source => match k.code {
                code::ESC => self.step = Step::Code,
                code::ENTER => { if self.source.trim().is_empty() { self.msg = if self.kind() == "git" { "Type the Git address, for example https://github.com/you/your-bot.git" } else { "Type the full path of the folder, for example /home/you/my-bot" }.into(); } else { self.step = Step::Details; self.focus = 0; } }
                _ => { edit_text(&mut self.source, &k, 250); }
            },
            Step::Details => match k.code {
                code::ESC => self.step = if self.kind() == "starter" { Step::Code } else { Step::Source },
                code::TAB | code::DOWN => self.focus = (self.focus + 1) % 3,
                code::UP => self.focus = (self.focus + 2) % 3,
                code::ENTER => {
                    if self.focus < 2 { self.focus += 1; }
                    else if crate::services::valid_name(&self.name).is_err() { self.msg = "The name may only have lowercase letters, digits, - and _.".into(); self.focus = 0; }
                    else if !crate::bot::plausible_token(&self.token) { self.msg = "That does not look like a bot token. Copy it from the Discord developer portal (Bot > Reset Token).".into(); self.focus = 1; }
                    else { self.install(env); }
                }
                _ => { if self.focus == 0 { edit_text(&mut self.name, &k, 32); } else if self.focus == 1 { edit_text(&mut self.token, &k, 200); } }
            },
            Step::Working => {}
            Step::Done => if k.code == code::ENTER || k.code == code::ESC { self.step = Step::Lang; self.msg.clear(); },
        }
        Out::None
    }

    fn draw(&self, p: &mut Painter, a: Rect, focused: bool) {
        let t = p.t;
        let foot = a.y + a.h.saturating_sub(1);
        p.text(a.x, a.y, "Discord bot", t.accent, None);
        let steps = ["1 Language", "2 Code", "3 Install"];
        let cur = match self.step { Step::Lang => 0, Step::Code | Step::Source => 1, _ => 2 };
        let mut x = a.x + 14;
        for (i, s) in steps.iter().enumerate() { if x + s.len() < a.x + a.w { p.text(x, a.y, s, if i == cur { t.accent } else { t.dim }, None); } x += s.len() + 2; }
        match self.step {
            Step::Lang => {
                p.text(a.x, a.y + 2, "Which language is the bot written in?", t.dim, None);
                let items: Vec<String> = [Lang::Python, Lang::Node].iter().map(|l| l.label().to_string()).collect();
                p.list(Rect { x: a.x, y: a.y + 4, w: a.w, h: 2 }, &items, self.lang_sel, 0, focused);
                p.paragraph(a.x, a.y + 8, a.w, "The right runtime and Discord library are installed for you.", t.dim, t.panel);
            }
            Step::Code => {
                p.text(a.x, a.y + 2, "Where does the bot's code come from?", t.dim, None);
                let items: Vec<String> = CODES.iter().map(|c| format!("{:<12} {}", c.1, c.2)).collect();
                p.list(Rect { x: a.x, y: a.y + 4, w: a.w, h: 3 }, &items, self.code_sel, 0, focused);
            }
            Step::Source => {
                p.text(a.x, a.y + 2, if self.kind() == "git" { "Git address of the bot" } else { "Folder with the bot's files (full path)" }, t.dim, None);
                p.field(a.x, a.y + 3, a.w.min(70), &self.source, focused, false);
                p.paragraph(a.x, a.y + 6, a.w, if self.lang() == Lang::Python { "Needs bot.py or main.py. A requirements.txt is installed too." } else { "Needs index.js or bot.js. A package.json is installed too." }, t.dim, t.panel);
            }
            Step::Details | Step::Working | Step::Done if self.step != Step::Done => {
                p.text(a.x, a.y + 2, &format!("{}, {}", self.lang().label(), CODES[self.code_sel].1), t.text, None);
                p.text(a.x, a.y + 4, "Name of the bot (its service)", t.dim, None);
                p.field(a.x, a.y + 5, a.w.min(40), &self.name, self.step == Step::Details && self.focus == 0 && focused, false);
                p.text(a.x, a.y + 8, "Bot token (hidden; from the Discord developer portal)", t.dim, None);
                p.field(a.x, a.y + 9, a.w.min(70), &self.token, self.step == Step::Details && self.focus == 1 && focused, true);
                let on = self.step == Step::Details && self.focus == 2;
                p.clip(a.x, a.y + 12, 14, "  Install  ", if on { t.accent_text } else { t.text }, if on { t.accent } else { t.panel });
            }
            _ => {
                p.text(a.x, a.y + 2, "Your bot is installed", t.accent, None);
                let mut y = a.y + 4;
                for l in super::wrap_text(&self.msg, a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.text, t.panel); y += 1; } }
                y += 1;
                for l in super::wrap_text("It runs as a service: it restarts if it crashes and starts at every boot. See it in Apps > Services. In the Discord developer portal, turn on the Message Content intent for the bot, then invite it to your server.", a.w) { if y < foot.saturating_sub(1) { p.clip(a.x, y, a.w, &l, t.dim, t.panel); y += 1; } }
            }
        }
        let hint = match self.step { Step::Lang => "Up/Down Choose   Enter Next", Step::Code => "Up/Down Choose   Enter Next   Esc Back", Step::Source => "Type   Enter Next   Esc Back", Step::Details => "Tab Next box   Enter Next / Install   Esc Back", Step::Working => "Please wait...", Step::Done => "Enter Back to the start" };
        p.clip(a.x, foot, a.w, hint, t.dim, t.panel);
        if self.step != Step::Done && !self.msg.is_empty() { super::draw_note(p, a.x, foot.saturating_sub(1), a.w, &self.msg, t.accent, t.panel, 3); }
    }

    fn state(&self) -> String {
        let n = |s: &str| if s.is_empty() { "-".to_string() } else { s.replace(' ', "_") };
        let step = match self.step { Step::Lang => "lang", Step::Code => "code", Step::Source => "source", Step::Details => "details", Step::Working => "working", Step::Done => "done" };
        // The token itself is never printed, only how long it is.
        format!("name=discordbot step={step} lang={} code={} source={} botname={} token_len={} focus={} busy={} msg={}", self.lang().id(), self.kind(), n(&self.source), n(&self.name), self.token.chars().count(), self.focus, self.job.busy, n(&self.msg.chars().take(60).collect::<String>()))
    }
    fn hint(&self) -> &'static str { "Up/Down Choose   Enter Next   F4 Close" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_key;
    use std::sync::{Arc, Mutex};

    const TOKEN: &str = "fake-token-for-tests-only-0123456789";

    fn app() -> (DiscordBot, Arc<Mutex<Vec<String>>>) {
        let calls = Arc::new(Mutex::new(vec![]));
        let c2 = calls.clone();
        let be: Backend = Arc::new(move |r: &[String]| { c2.lock().unwrap().push(r.join(" ")); if r[1] == "broken" { Err("Installing the Discord library failed: boom".into()) } else { Ok("Your Discord bot is installed and starting.".into()) } });
        (DiscordBot::new(be), calls)
    }
    fn settle(m: &mut DiscordBot) { let e = Env::for_tests(); for _ in 0..200 { m.tick(&e); if !m.job.busy { return; } std::thread::sleep(Duration::from_millis(5)); } }
    fn key(m: &mut DiscordBot, k: &str) { m.handle(parse_key(k).unwrap(), &Env::for_tests()); settle(m); }
    fn text(m: &mut DiscordBot, t: &str) { for c in t.chars() { key(m, &c.to_string()); } }

    #[test]
    fn a_starter_python_bot_goes_through_the_steps() {
        let (mut m, calls) = app();
        assert!(m.state().contains("step=lang") && m.state().contains("lang=python"));
        key(&mut m, "enter"); assert!(m.state().contains("step=code"));
        key(&mut m, "enter"); assert!(m.state().contains("step=details"), "the starter needs no folder or address");
        for _ in 0..6 { key(&mut m, "backspace"); }
        text(&mut m, "pingbot"); key(&mut m, "enter");
        text(&mut m, TOKEN); assert!(!m.state().contains(TOKEN) && m.state().contains(&format!("token_len={}", TOKEN.len())), "the token is never in the state line");
        key(&mut m, "enter"); key(&mut m, "enter");
        let c = calls.lock().unwrap().clone();
        assert_eq!(c, vec![format!("bot-create pingbot python starter - {TOKEN} {}", std::env::var("USER").unwrap_or_default())]);
        assert!(m.state().contains("step=done") && m.token.is_empty(), "the app forgets the token once it has been sent");
        key(&mut m, "enter"); assert!(m.state().contains("step=lang"));
    }

    #[test]
    fn node_with_your_own_folder_or_a_git_address() {
        let (mut m, calls) = app();
        key(&mut m, "down"); key(&mut m, "enter");                         // Node
        key(&mut m, "down"); key(&mut m, "enter");                         // my own bot
        assert!(m.state().contains("step=source") && m.state().contains("lang=node") && m.state().contains("code=folder"));
        key(&mut m, "enter"); assert!(m.msg.contains("folder"), "an empty folder is refused: {}", m.msg);
        text(&mut m, "/home/tester/bot"); key(&mut m, "enter");
        key(&mut m, "enter"); text(&mut m, TOKEN); key(&mut m, "enter"); key(&mut m, "enter");     // keep the name "mybot"
        assert!(calls.lock().unwrap()[0].starts_with("bot-create mybot node folder /home/tester/bot "), "{:?}", calls.lock().unwrap());
        key(&mut m, "enter");
        key(&mut m, "enter"); key(&mut m, "down"); key(&mut m, "down"); key(&mut m, "enter");   // python, from Git
        assert!(m.state().contains("code=git"));
        m.source = "https://github.com/me/bot.git".into(); key(&mut m, "enter");    // (a colon cannot be typed with the test keys)
        key(&mut m, "enter"); text(&mut m, TOKEN); key(&mut m, "enter"); key(&mut m, "enter");
        assert!(calls.lock().unwrap()[1].contains(" git https://github.com/me/bot.git "), "{:?}", calls.lock().unwrap());
    }

    #[test]
    fn bad_names_and_tokens_are_refused_before_anything_is_sent() {
        let (mut m, calls) = app();
        key(&mut m, "enter"); key(&mut m, "enter");
        for _ in 0..6 { key(&mut m, "backspace"); } text(&mut m, "Bad Name");
        key(&mut m, "enter"); key(&mut m, "enter"); key(&mut m, "enter");
        assert!(m.msg.contains("lowercase") && m.state().contains("focus=0"), "{}", m.msg);
        key(&mut m, "enter"); text(&mut m, "short"); key(&mut m, "enter");
        // the name still has a space: fix it first
        for _ in 0..8 { key(&mut m, "backspace"); }
        assert!(calls.lock().unwrap().is_empty(), "nothing was sent");
    }

    #[test]
    fn a_failed_install_stays_on_the_details_screen_with_the_reason() {
        let (mut m, _c) = app();
        key(&mut m, "enter"); key(&mut m, "enter");
        for _ in 0..6 { key(&mut m, "backspace"); } text(&mut m, "broken");
        key(&mut m, "enter"); text(&mut m, TOKEN); key(&mut m, "enter"); key(&mut m, "enter");
        assert!(m.state().contains("step=details") && m.msg.contains("boom"), "{}", m.msg);
        assert!(!m.token.is_empty(), "after a failure the token is still there to try again");
    }

    #[test]
    fn every_screen_draws_at_several_sizes() {
        use crate::gfx::{Canvas, Metrics, Mode, Theme};
        let (mut m, _c) = app();
        for step in 0..5 {
            match step { 1 => key(&mut m, "enter"), 2 => { key(&mut m, "down"); key(&mut m, "enter"); } 3 => { key(&mut m, "enter"); text(&mut m, "/x"); key(&mut m, "enter"); } 4 => { text(&mut m, TOKEN); } _ => {} }
            for (w, h) in [(8usize, 4usize), (40, 12), (100, 36)] {
                let mut c = Canvas::new(1280, 800);
                let mut p = Painter { c: &mut c, m: Metrics::for_height(800), t: Theme::new(Mode::Dark) };
                m.draw(&mut p, Rect { x: 1, y: 1, w, h }, true);
            }
        }
    }
}
