//! Past runs, one JSON line each in `<data>/automation-runs.jsonl`, the latest kept.

use std::collections::VecDeque;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use tracing::warn;

use crate::run::Run;

pub const KEEP_RUNS: usize = 500;
/// The file is rewritten with the latest runs only once it grows past this.
const TRIM_AT_BYTES: u64 = 2 * 1024 * 1024;

/// The latest runs, oldest first. Unreadable lines are skipped.
#[must_use]
pub fn load(path: &Path) -> VecDeque<Run> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return VecDeque::new();
    };
    let mut runs: VecDeque<Run> = text
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect();
    while runs.len() > KEEP_RUNS {
        runs.pop_front();
    }
    runs
}

/// Adds a finished run; trims the file when it grew too big.
pub fn append(path: &Path, run: &Run, recent: &VecDeque<Run>) {
    let result = (|| {
        let line = serde_json::to_string(run)?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "{line}")?;
        if file.metadata()?.len() > TRIM_AT_BYTES {
            let mut kept = String::new();
            for past in recent.iter().filter(|past| past.ended.is_some()) {
                kept.push_str(&serde_json::to_string(past)?);
                kept.push('\n');
            }
            let temporary = path.with_extension("jsonl.tmp");
            std::fs::write(&temporary, kept)?;
            std::fs::rename(&temporary, path)?;
        }
        Ok::<_, anyhow::Error>(())
    })();
    if let Err(error) = result {
        warn!(%error, "cannot record the automation run");
    }
}
