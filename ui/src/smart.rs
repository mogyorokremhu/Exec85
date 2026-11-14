//! Disk health and temperatures: reading what `smartctl` prints, and the processor temperature from /sys.
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Overall { Passed, Failed, Unavailable }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Health { pub overall: Overall, pub temp_c: Option<i32>, pub hours: Option<u64>, pub reallocated: Option<u64> }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level { Ok, Warm, Hot }

fn first_number(s: &str) -> Option<u64> {
    let digits: String = s.trim().chars().take_while(|c| c.is_ascii_digit() || *c == ',').filter(|c| *c != ',').collect();
    digits.parse().ok()
}

/// Read `smartctl -H -A` output (SATA/SAS tables and NVMe summaries).
pub fn parse(out: &str) -> Health {
    let mut h = Health { overall: Overall::Unavailable, temp_c: None, hours: None, reallocated: None };
    for l in out.lines() {
        let t = l.trim();
        if let Some(r) = t.strip_prefix("SMART overall-health self-assessment test result:") { h.overall = if r.trim().starts_with("PASSED") || r.trim().starts_with("OK") { Overall::Passed } else { Overall::Failed }; continue; }
        if t.starts_with("SMART Health Status:") { h.overall = if t.contains("OK") { Overall::Passed } else { Overall::Failed }; continue; }
        // NVMe style: "Temperature:   38 Celsius", "Power On Hours:  1,234"
        if let Some(r) = t.strip_prefix("Temperature:") { if h.temp_c.is_none() { h.temp_c = first_number(r).map(|v| v as i32); } continue; }
        if let Some(r) = t.strip_prefix("Power On Hours:") { h.hours = first_number(r); continue; }
        // SATA attribute table: ID NAME FLAG VALUE WORST THRESH TYPE UPDATED WHEN_FAILED RAW
        let f: Vec<&str> = t.split_whitespace().collect();
        if f.len() >= 10 && f[0].chars().all(|c| c.is_ascii_digit()) {
            let raw = f[9..].join(" ");
            match f[0] {
                "5" => h.reallocated = first_number(&raw),
                "9" => h.hours = first_number(&raw),
                "190" | "194" => { if h.temp_c.is_none() || f[0] == "194" { h.temp_c = first_number(&raw).map(|v| v as i32); } }
                _ => {}
            }
        }
    }
    h
}

pub fn level(temp_c: i32, cpu: bool) -> Level {
    let (warm, hot) = if cpu { (75, 90) } else { (50, 60) };
    if temp_c >= hot { Level::Hot } else if temp_c >= warm { Level::Warm } else { Level::Ok }
}

/// The hottest thermal zone, from /sys/class/thermal/thermal_zone*/temp (millidegrees) or hwmon.
pub fn cpu_temp_c(sys: &Path) -> Option<i32> {
    let mut best: Option<i32> = None;
    let mut look = |dir: &Path, prefix: &str, file_ok: &dyn Fn(&str) -> bool| {
        if let Ok(rd) = std::fs::read_dir(dir) { for e in rd.flatten() {
            if !e.file_name().to_string_lossy().starts_with(prefix) { continue; }
            if let Ok(inner) = std::fs::read_dir(e.path()) { for f in inner.flatten() {
                let n = f.file_name().to_string_lossy().into_owned();
                if !file_ok(&n) { continue; }
                if let Some(v) = std::fs::read_to_string(f.path()).ok().and_then(|t| t.trim().parse::<i64>().ok()) { let c = (v / 1000) as i32; if (1..150).contains(&c) { best = Some(best.map_or(c, |b| b.max(c))); } }
            } }
            if prefix == "thermal_zone" { if let Some(v) = std::fs::read_to_string(e.path().join("temp")).ok().and_then(|t| t.trim().parse::<i64>().ok()) { let c = (v / 1000) as i32; if (1..150).contains(&c) { best = Some(best.map_or(c, |b| b.max(c))); } } }
        } }
    };
    look(&sys.join("class/thermal"), "thermal_zone", &|_| false);
    look(&sys.join("class/hwmon"), "hwmon", &|n| n.starts_with("temp") && n.ends_with("_input"));
    best
}

pub fn describe(h: &Health) -> String {
    match h.overall {
        Overall::Unavailable => "health: not available (virtual disks and some USB drives do not report it)".into(),
        o => {
            let mut s = format!("health: {}", if o == Overall::Passed { "good" } else { "FAILING: back up your files now" });
            if let Some(t) = h.temp_c { s.push_str(&format!(", {t} C")); }
            if let Some(h) = h.hours { s.push_str(&format!(", on for {h} hours")); }
            if let Some(r) = h.reallocated { if r > 0 { s.push_str(&format!(", {r} bad sectors replaced")); } }
            s
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SATA: &str = "smartctl 7.4 2023-08-01 r5530 [x86_64-linux-6.12.0-lts] (local build)\n=== START OF READ SMART DATA SECTION ===\nSMART overall-health self-assessment test result: PASSED\n\nSMART Attributes Data Structure revision number: 16\nVendor Specific SMART Attributes with Thresholds:\nID# ATTRIBUTE_NAME          FLAG     VALUE WORST THRESH TYPE      UPDATED  WHEN_FAILED RAW_VALUE\n  5 Reallocated_Sector_Ct   0x0033   100   100   010    Pre-fail  Always       -       8\n  9 Power_On_Hours          0x0032   097   097   000    Old_age   Always       -       12345\n194 Temperature_Celsius     0x0022   065   045   000    Old_age   Always       -       35 (Min/Max 20/55)\n";
    const NVME: &str = "=== START OF SMART DATA SECTION ===\nSMART overall-health self-assessment test result: PASSED\n\nSMART/Health Information (NVMe Log 0x02)\nCritical Warning:                   0x00\nTemperature:                        38 Celsius\nAvailable Spare:                    100%\nPower On Hours:                     1,234\n";

    #[test]
    fn a_sata_disk_report_is_read() {
        assert_eq!(parse(SATA), Health { overall: Overall::Passed, temp_c: Some(35), hours: Some(12345), reallocated: Some(8) });
        let d = describe(&parse(SATA));
        assert!(d.contains("good") && d.contains("35 C") && d.contains("12345 hours") && d.contains("8 bad sectors"), "{d}");
    }

    #[test]
    fn an_nvme_report_is_read() {
        assert_eq!(parse(NVME), Health { overall: Overall::Passed, temp_c: Some(38), hours: Some(1234), reallocated: None });
    }

    #[test]
    fn a_failing_disk_is_called_failing_and_a_disk_without_smart_is_not_called_healthy() {
        let bad = parse("SMART overall-health self-assessment test result: FAILED!\n");
        assert_eq!(bad.overall, Overall::Failed); assert!(describe(&bad).contains("FAILING"));
        for none in ["", "SMART support is: Unavailable - device lacks SMART capability.\n", "Smartctl open device: /dev/sda failed: Permission denied\n"] {
            let h = parse(none); assert_eq!(h.overall, Overall::Unavailable, "{none:?}"); assert!(describe(&h).contains("not available"));
        }
        assert_eq!(parse("SMART Health Status: OK\n").overall, Overall::Passed);
    }

    #[test]
    fn temperatures_are_graded() {
        assert_eq!((level(40, false), level(52, false), level(65, false)), (Level::Ok, Level::Warm, Level::Hot));
        assert_eq!((level(60, true), level(80, true), level(95, true)), (Level::Ok, Level::Warm, Level::Hot));
    }

    #[test]
    fn the_cpu_temperature_is_the_hottest_zone() {
        let d = std::env::temp_dir().join(format!("exec85-thermal-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        assert_eq!(cpu_temp_c(&d), None);
        for (z, t) in [("thermal_zone0", "42000"), ("thermal_zone1", "67000"), ("thermal_zone2", "-274000")] {
            std::fs::create_dir_all(d.join("class/thermal").join(z)).unwrap(); std::fs::write(d.join("class/thermal").join(z).join("temp"), t).unwrap();
        }
        assert_eq!(cpu_temp_c(&d), Some(67));
        std::fs::create_dir_all(d.join("class/hwmon/hwmon3")).unwrap(); std::fs::write(d.join("class/hwmon/hwmon3/temp1_input"), "71500").unwrap();
        assert_eq!(cpu_temp_c(&d), Some(71));
        let _ = std::fs::remove_dir_all(&d);
    }
}
