//! Keyboard input straight from the kernel's evdev devices (/dev/input/event*).
//! No X11, no libinput. A background thread watches for keyboards (also ones
//! plugged in later) and sends each key press to the UI.
use crate::keys::Key;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::{thread, time::Duration};

const EV_KEY: u16 = 1;
// bit flags for held modifier keys
const LSHIFT: u8 = 1; const RSHIFT: u8 = 2; const LCTRL: u8 = 4; const RCTRL: u8 = 8; const LALT: u8 = 16; const RALT: u8 = 32;

fn modifier_bit(code: u16) -> Option<u8> {
    match code { 42 => Some(LSHIFT), 54 => Some(RSHIFT), 29 => Some(LCTRL), 97 => Some(RCTRL), 56 => Some(LALT), 100 => Some(RALT), _ => None }
}

/// Start watching input devices. Key presses (and auto-repeats) arrive on the returned channel.
pub fn spawn() -> Receiver<Key> {
    let (tx, rx) = channel();
    let mods = Arc::new(AtomicU8::new(0));
    let open: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    thread::spawn(move || loop {
        if let Ok(rd) = fs::read_dir("/dev/input") {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if !name.starts_with("event") { continue; }
                let mut set = open.lock().unwrap();
                if set.contains(&name) { continue; }
                if let Ok(f) = File::open(e.path()) {
                    set.insert(name.clone());
                    drop(set);
                    let (tx, mods, open) = (tx.clone(), mods.clone(), open.clone());
                    thread::spawn(move || { read_device(f, &tx, &mods); open.lock().unwrap().remove(&name); });
                }
            }
        }
        thread::sleep(Duration::from_secs(2));
    });
    rx
}

fn read_device(mut f: File, tx: &Sender<Key>, mods: &AtomicU8) {
    // struct input_event on x86_64: 16 bytes of time, u16 type, u16 code, i32 value = 24 bytes
    let mut buf = [0u8; 24 * 32];
    loop {
        let n = match f.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return, // device unplugged
        };
        for ev in buf[..n].chunks_exact(24) {
            let typ = u16::from_ne_bytes([ev[16], ev[17]]);
            let code = u16::from_ne_bytes([ev[18], ev[19]]);
            let value = i32::from_ne_bytes([ev[20], ev[21], ev[22], ev[23]]);
            if typ != EV_KEY || code >= 0x100 { continue; } // ignore mouse buttons etc.
            if let Some(bit) = modifier_bit(code) {
                if value == 0 { mods.fetch_and(!bit, Ordering::SeqCst); } else { mods.fetch_or(bit, Ordering::SeqCst); }
                continue;
            }
            if value == 1 || value == 2 { // press or auto-repeat
                let m = mods.load(Ordering::SeqCst);
                let key = Key { code, shift: m & (LSHIFT | RSHIFT) != 0, ctrl: m & (LCTRL | RCTRL) != 0, alt: m & (LALT | RALT) != 0 };
                if tx.send(key).is_err() { return; }
            }
        }
    }
}
