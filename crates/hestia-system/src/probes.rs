//! Readers for Linux `/proc` and `/sys`, split into pure parsers so they can be tested anywhere.

use std::path::Path;

/// `(busy, total)` jiffies from the aggregated `cpu` line of `/proc/stat`.
pub(crate) fn parse_cpu_times(stat: &str) -> Option<(u64, u64)> {
    let line = stat.lines().find(|line| line.starts_with("cpu "))?;
    let fields: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    let [user, nice, system, idle, iowait, irq, softirq, steal] = fields.try_into().ok()?;
    let total = user + nice + system + idle + iowait + irq + softirq + steal;
    Some((total - idle - iowait, total))
}

/// Percentage of memory in use, from `MemTotal` and `MemAvailable`.
pub(crate) fn parse_memory_used(meminfo: &str) -> Option<f64> {
    let field = |name: &str| -> Option<f64> {
        meminfo
            .lines()
            .find_map(|line| line.strip_prefix(name))?
            .trim_start_matches(':')
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    };
    let total = field("MemTotal")?;
    let available = field("MemAvailable")?;
    (total > 0.0).then(|| round1((total - available) / total * 100.0))
}

/// `48686` (millidegrees) becomes 48.7 °C.
pub(crate) fn parse_millidegrees(raw: &str) -> Option<f64> {
    raw.trim()
        .parse::<f64>()
        .ok()
        .map(|milli| round1(milli / 1000.0))
}

pub(crate) fn round1(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

pub(crate) fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// The Raspberry Pi firmware reports under-voltage through the `rpi_volt` hwmon device.
pub(crate) fn under_voltage_alarm() -> Option<bool> {
    let entries = std::fs::read_dir("/sys/class/hwmon").ok()?;
    entries.flatten().find_map(|entry| {
        let directory = entry.path();
        (read(directory.join("name"))?.trim() == "rpi_volt")
            .then(|| read(directory.join("in0_lcrit_alarm")))
            .flatten()
            .map(|alarm| alarm.trim() == "1")
    })
}

/// Share of the file system holding `path` that is used, as `df` computes it.
#[cfg(unix)]
pub(crate) fn disk_used(path: &Path) -> Option<f64> {
    let stats = rustix::fs::statvfs(path).ok()?;
    let used = stats.f_blocks.checked_sub(stats.f_bfree)?;
    let usable = used + stats.f_bavail;
    #[allow(clippy::cast_precision_loss)]
    (usable > 0).then(|| round1(used as f64 / usable as f64 * 100.0))
}

#[cfg(not(unix))]
pub(crate) fn disk_used(_path: &Path) -> Option<f64> {
    None
}

#[cfg(test)]
mod tests {
    use super::{parse_cpu_times, parse_memory_used, parse_millidegrees};

    #[test]
    fn cpu_times() {
        let stat = "cpu  100 5 50 800 20 3 2 0 0 0\ncpu0 1 2 3 4 5 6 7 8 0 0\n";
        assert_eq!(parse_cpu_times(stat), Some((160, 980)));
        assert_eq!(parse_cpu_times("garbage"), None);
    }

    #[test]
    fn memory() {
        let meminfo = "MemTotal:        1891316 kB\nMemFree:          200000 kB\nMemAvailable:    1418487 kB\n";
        assert_eq!(parse_memory_used(meminfo), Some(25.0));
        assert_eq!(
            parse_memory_used("MemTotal: 0 kB\nMemAvailable: 0 kB"),
            None
        );
    }

    #[test]
    fn temperature() {
        assert_eq!(parse_millidegrees("48686\n"), Some(48.7));
        assert_eq!(parse_millidegrees("n/a"), None);
    }
}
