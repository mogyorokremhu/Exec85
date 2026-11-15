//! Screen brightness: a real backlight when the laptop has one, otherwise the picture itself is darkened.
//!
//! The setting is a percentage from 10 to 100 (never fully black) kept in /etc/exec85/brightness. The helper writes it to the
//! backlight (`/sys/class/backlight/*/brightness`) and puts it back at every start. When there is no backlight (a desktop screen,
//! a virtual machine) the shell darkens every colour it draws, which works the same on every screen.
use crate::helper::Ctx;
use std::path::PathBuf;

pub const MIN: u32 = 10;
pub const MAX: u32 = 100;
pub const STEP: u32 = 10;
pub const FILE: &str = "etc/exec85/brightness";

/// One step brighter or darker, kept between MIN and MAX and on the steps of ten.
pub fn step(current: u32, up: bool) -> u32 {
    let c = current.clamp(MIN, MAX);
    if up { ((c / STEP) * STEP + STEP).min(MAX) } else if c % STEP == 0 { c.saturating_sub(STEP).max(MIN) } else { ((c / STEP) * STEP).max(MIN) }
}

/// The saved setting (100 when nothing is saved or the file is nonsense).
pub fn saved(ctx: &Ctx) -> u32 {
    std::fs::read_to_string(ctx.root.join(FILE)).ok().and_then(|t| t.trim().parse::<u32>().ok()).filter(|p| (MIN..=MAX).contains(p)).unwrap_or(MAX)
}

/// The backlight to use: the one with the finest steps (a laptop may list several for the same screen).
fn backlight(ctx: &Ctx) -> Option<(PathBuf, u32)> {
    let mut best: Option<(PathBuf, u32)> = None;
    for e in std::fs::read_dir(ctx.root.join("sys/class/backlight")).ok()?.flatten() {
        let max = std::fs::read_to_string(e.path().join("max_brightness")).ok().and_then(|t| t.trim().parse::<u32>().ok()).unwrap_or(0);
        if max >= 2 && e.path().join("brightness").exists() && best.as_ref().map_or(true, |(_, m)| max > *m) { best = Some((e.path(), max)); }
    }
    best
}

fn apply_hardware(ctx: &Ctx, pct: u32) -> bool {
    let Some((dir, max)) = backlight(ctx) else { return false };
    let v = ((max as u64 * pct as u64 + 50) / 100).clamp(1, max as u64);
    std::fs::write(dir.join("brightness"), format!("{v}\n")).is_ok()
}

/// `hw=yes|no` and `percent=N`.
pub fn status(ctx: &Ctx) -> String { format!("hw={}\npercent={}\n", if backlight(ctx).is_some() { "yes" } else { "no" }, saved(ctx)) }

pub fn set(ctx: &Ctx, pct: u32) -> Result<String, String> {
    if !(MIN..=MAX).contains(&pct) { return Err(format!("Brightness is between {MIN} and {MAX} percent.")); }
    if let Some(d) = ctx.root.join(FILE).parent() { std::fs::create_dir_all(d).map_err(|e| e.to_string())?; }
    std::fs::write(ctx.root.join(FILE), format!("{pct}\n")).map_err(|e| e.to_string())?;
    let hw = apply_hardware(ctx, pct);
    Ok(format!("hw={}\npercent={pct}\n", if hw { "yes" } else { "no" }))
}

/// Put the saved brightness back (at start, and when the backlight shows up late).
pub fn restore(ctx: &Ctx) { let p = saved(ctx); if p < MAX || backlight(ctx).is_some() { let _ = apply_hardware(ctx, p); } }

/// Darken a picture in place: every colour channel times `pct` percent.
pub fn dim(px: &mut [u32], pct: u32) {
    if pct >= MAX { return; }
    let p = pct.max(MIN);
    for v in px.iter_mut() {
        let (r, g, b) = ((*v >> 16) & 0xFF, (*v >> 8) & 0xFF, *v & 0xFF);
        *v = (*v & 0xFF00_0000) | ((r * p / 100) << 16) | ((g * p / 100) << 8) | (b * p / 100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::helper::SysRunner;
    use std::path::Path;

    fn world(tag: &str, with_backlight: &[(&str, u32)]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("exec85-bright-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("etc/exec85")).unwrap();
        for (name, max) in with_backlight {
            let b = d.join("sys/class/backlight").join(name);
            std::fs::create_dir_all(&b).unwrap();
            std::fs::write(b.join("max_brightness"), format!("{max}\n")).unwrap();
            std::fs::write(b.join("brightness"), format!("{max}\n")).unwrap();
        }
        d
    }
    fn ctx(r: &Path) -> Ctx<'_> { Ctx { root: r, runner: &SysRunner } }

    #[test]
    fn steps_stay_between_ten_and_a_hundred_and_land_on_tens() {
        assert_eq!(step(100, true), 100);
        assert_eq!(step(100, false), 90);
        assert_eq!(step(90, true), 100);
        assert_eq!(step(10, false), 10, "never fully dark");
        assert_eq!(step(15, false), 10);
        assert_eq!(step(15, true), 20);
        assert_eq!(step(0, true), 20);
        assert_eq!(step(7, false), 10);
        let mut p = 100; for _ in 0..30 { p = step(p, false); } assert_eq!(p, 10);
        for _ in 0..30 { p = step(p, true); } assert_eq!(p, 100);
    }

    #[test]
    fn the_backlight_gets_the_setting_and_it_is_remembered_and_restored() {
        let r = world("hw", &[("acpi_video0", 15), ("intel_backlight", 960)]);
        let c = ctx(&r);
        assert_eq!(status(&c), "hw=yes\npercent=100\n");
        assert_eq!(set(&c, 50).unwrap(), "hw=yes\npercent=50\n");
        assert_eq!(std::fs::read_to_string(r.join("sys/class/backlight/intel_backlight/brightness")).unwrap().trim(), "480", "the finer backlight is used");
        assert_eq!(std::fs::read_to_string(r.join("sys/class/backlight/acpi_video0/brightness")).unwrap().trim(), "15", "the coarse one is left alone");
        assert_eq!(saved(&c), 50);
        std::fs::write(r.join("sys/class/backlight/intel_backlight/brightness"), "960\n").unwrap();
        restore(&c);
        assert_eq!(std::fs::read_to_string(r.join("sys/class/backlight/intel_backlight/brightness")).unwrap().trim(), "480", "put back at the next start");
        set(&c, 10).unwrap();
        assert_eq!(std::fs::read_to_string(r.join("sys/class/backlight/intel_backlight/brightness")).unwrap().trim(), "96");
        assert!(set(&c, 5).is_err() && set(&c, 101).is_err() && set(&c, 0).is_err());
        assert_eq!(saved(&c), 10, "a refused value changes nothing");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn without_a_backlight_the_setting_is_still_kept_for_the_software_dimming() {
        let r = world("sw", &[]);
        let c = ctx(&r);
        assert_eq!(status(&c), "hw=no\npercent=100\n");
        assert_eq!(set(&c, 70).unwrap(), "hw=no\npercent=70\n");
        assert_eq!(saved(&c), 70);
        restore(&c);                                  // nothing to restore onto: no crash
        std::fs::write(r.join("etc/exec85/brightness"), "banana\n").unwrap();
        assert_eq!(saved(&c), 100, "a damaged file means full brightness");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn a_backlight_that_cannot_be_dimmed_is_ignored() {
        let r = world("odd", &[("broken", 1)]);
        assert_eq!(status(&ctx(&r)), "hw=no\npercent=100\n", "a backlight with one step is no use");
        let _ = std::fs::remove_dir_all(&r);
    }

    #[test]
    fn dimming_scales_every_colour_and_leaves_full_brightness_alone() {
        let mut px = vec![0x00FF_8040u32, 0x0000_0000, 0x00FF_FFFF];
        dim(&mut px, 100);
        assert_eq!(px, vec![0x00FF_8040, 0, 0x00FF_FFFF]);
        dim(&mut px, 50);
        assert_eq!(px, vec![0x007F_4020, 0, 0x007F_7F7F]);
        let mut px = vec![0x00FF_FFFFu32]; dim(&mut px, 1); assert_eq!(px[0], 0x0019_1919, "never below the minimum");
    }
}
