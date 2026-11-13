//! Background music for the installer: a looping player (`mpg123`) that is started when a sound card exists,
//! can be paused and resumed with a key, and stops by itself when the installer ends.
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

pub const MUSIC_FILE: &str = "/usr/share/exec85/audio/installer.mp3";
pub const PLAYER: &str = "/usr/bin/mpg123";

extern "C" { fn prctl(option: i32, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> i32; }

/// True when /proc/asound/cards lists at least one sound card.
pub fn card_listed(cards: &str) -> bool { cards.lines().any(|l| l.trim_start().chars().next().map_or(false, |c| c.is_ascii_digit())) }

pub fn sound_card_present() -> bool { std::fs::read_to_string("/proc/asound/cards").map_or(false, |s| card_listed(&s)) }

/// Unmute the usual outputs and set a comfortable volume (best effort: names differ between sound cards).
pub fn unmute() {
    for ctl in ["Master", "Speaker", "Headphone", "PCM", "Front", "Line Out"] {
        let _ = Command::new("amixer").args(["-q", "-c", "0", "sset", ctl, "80%", "unmute"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
}

pub struct Music { child: Child, paused: bool }

impl Music {
    /// Start `player` playing `file` over and over. The player is told to die with this process.
    pub fn start(player: &Path, args: &[&str], file: &Path) -> io::Result<Music> {
        let mut cmd = Command::new(player);
        cmd.args(args).arg(file).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        // SAFETY: between fork and exec only prctl is called (PR_SET_PDEATHSIG = 1, SIGKILL = 9).
        unsafe { cmd.pre_exec(|| { prctl(1, 9, 0, 0, 0); Ok(()) }); }
        Ok(Music { child: cmd.spawn()?, paused: false })
    }

    /// The installer's music, if there is a sound card, a player and a music file.
    pub fn start_default() -> Option<Music> {
        let (player, file) = (PathBuf::from(PLAYER), PathBuf::from(MUSIC_FILE));
        if !player.exists() || !file.exists() || !sound_card_present() { return None; }
        unmute();
        Music::start(&player, &["-q", "--loop", "-1"], &file).ok()
    }

    pub fn paused(&self) -> bool { self.paused }
    pub fn pid(&self) -> u32 { self.child.id() }

    pub fn toggle(&mut self) {
        self.paused = !self.paused;
        let _ = crate::sys::send_signal(self.child.id() as i32, if self.paused { 19 } else { 18 }); // SIGSTOP / SIGCONT
    }

    pub fn state(&self) -> &'static str { if self.paused { "off" } else { "on" } }
}

impl Drop for Music {
    fn drop(&mut self) { let _ = self.child.kill(); let _ = self.child.wait(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sound_cards_are_recognised_in_the_proc_listing() {
        assert!(card_listed(" 0 [Intel          ]: HDA-Intel - HDA Intel\n                      HDA Intel at 0x...\n"));
        assert!(!card_listed("--- no soundcards ---\n"));
        assert!(!card_listed(""));
    }

    fn alive(pid: u32) -> bool { std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(false, |s| !s.contains(") Z ") && !s.contains(") X ")) }
    fn state_of(pid: u32) -> char { std::fs::read_to_string(format!("/proc/{pid}/stat")).ok().and_then(|s| s.rsplit(") ").next().and_then(|r| r.chars().next())).unwrap_or('?') }

    #[test]
    fn music_can_be_paused_resumed_and_stops_when_dropped() {
        // `sleep` stands in for the player: it just has to be a long-running process
        let mut m = Music::start(Path::new("/bin/sleep"), &[], Path::new("30")).unwrap();
        let pid = m.pid();
        assert!(alive(pid) && !m.paused() && m.state() == "on");
        m.toggle();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(m.paused() && m.state() == "off" && state_of(pid) == 'T', "paused = stopped ({})", state_of(pid));
        m.toggle();
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!m.paused() && state_of(pid) != 'T', "resumed ({})", state_of(pid));
        drop(m);
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(!alive(pid), "the player is gone when the music object is dropped");
    }

    #[test]
    fn a_missing_player_is_an_error_not_a_crash() {
        assert!(Music::start(Path::new("/no/such/player"), &[], Path::new("x")).is_err());
        // this test machine has neither player nor card in the default places
        assert!(Music::start_default().is_none() || Path::new(PLAYER).exists());
    }
}
