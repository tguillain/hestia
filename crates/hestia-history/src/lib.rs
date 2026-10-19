//! History of numeric and binary states. The hub only reports changes, so a value holds
//! until the next sample: queries carry the last known value forward.

mod store;

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use hestia_core::{DeviceId, Value};
use hestia_hub::{Event, Hub, now_millis};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio::time::{self, MissedTickBehavior};
use tracing::warn;

pub use store::Buckets;
use store::Store;

const PRUNE_EVERY: Duration = Duration::from_secs(3600);
const MAX_PENDING: usize = 10_000;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HistoryConfig {
    pub enabled: bool,
    pub retention_days: u32,
    /// Samples are written in batches to spare the SD card.
    pub flush_interval_s: u64,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            retention_days: 10,
            flush_interval_s: 30,
        }
    }
}

struct Sample {
    series: String,
    at: i64,
    value: f64,
}

pub struct History {
    store: Store,
    pending: Mutex<Vec<Sample>>,
    config: HistoryConfig,
}

impl History {
    /// Opens (or creates) `history.redb` in `data_dir`.
    ///
    /// # Errors
    /// Fails when the file cannot be opened.
    pub fn open(data_dir: &Path, config: HistoryConfig) -> anyhow::Result<Arc<Self>> {
        std::fs::create_dir_all(data_dir)?;
        Ok(Arc::new(Self {
            store: Store::open(&data_dir.join("history.redb"))?,
            pending: Mutex::new(Vec::new()),
            config,
        }))
    }

    #[must_use]
    pub fn retention_hours(&self) -> u32 {
        self.config.retention_days.saturating_mul(24)
    }

    /// Queues a sample; only numbers and booleans have a history.
    pub fn record(&self, device: &DeviceId, point: &str, at: i64, value: &Value) {
        let value = match value {
            Value::Number(number) if number.is_finite() => *number,
            Value::Bool(flag) => f64::from(u8::from(*flag)),
            _ => return,
        };
        let mut pending = self.pending.lock().unwrap_or_else(PoisonError::into_inner);
        if pending.len() < MAX_PENDING {
            pending.push(Sample {
                series: series_key(device, point),
                at,
                value,
            });
        }
    }

    /// Writes the queued samples in one transaction. Blocking.
    ///
    /// # Errors
    /// Fails when the database cannot be written; the samples are then dropped.
    pub fn flush(&self) -> anyhow::Result<()> {
        let batch =
            std::mem::take(&mut *self.pending.lock().unwrap_or_else(PoisonError::into_inner));
        if batch.is_empty() {
            return Ok(());
        }
        self.store.insert(
            batch
                .iter()
                .map(|sample| (sample.series.as_str(), sample.at, sample.value)),
        )
    }

    /// Averages of `device`/`point` over `buckets` equal slices of the last `hours`. Blocking.
    ///
    /// # Errors
    /// Fails when the database cannot be read.
    pub fn query(
        &self,
        device: &DeviceId,
        point: &str,
        hours: u32,
        buckets: usize,
    ) -> anyhow::Result<Buckets> {
        let to = now_millis();
        let from = to - i64::from(hours.clamp(1, self.retention_hours().max(1))) * 3_600_000;
        self.store.query(
            &series_key(device, point),
            from,
            to,
            buckets.clamp(10, 1000),
        )
    }

    /// Records every state change and keeps the file within the retention period.
    pub fn spawn(self: &Arc<Self>, hub: Arc<Hub>) -> JoinHandle<()> {
        let history = Arc::clone(self);
        tokio::spawn(async move { history.follow(&hub).await })
    }

    async fn follow(self: Arc<Self>, hub: &Hub) {
        let mut events = hub.subscribe();
        // Current values first: after a restart, graphs resume from a known state.
        let now = now_millis();
        for device in hub.devices() {
            for point in &device.points {
                if let Some(value) = &point.value {
                    self.record(&device.id, &point.spec.key, now, value);
                }
            }
        }
        let mut flush = time::interval(Duration::from_secs(self.config.flush_interval_s.max(5)));
        flush.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut prune = time::interval(PRUNE_EVERY);
        prune.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(Event::State { device, point, value, at }) => self.record(&device, &point, at, &value),
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                    Err(RecvError::Closed) => return,
                },
                _ = flush.tick() => self.blocking(Self::flush).await,
                _ = prune.tick() => self.blocking(Self::prune).await,
            }
        }
    }

    fn prune(&self) -> anyhow::Result<()> {
        let cutoff = now_millis() - i64::from(self.retention_hours()) * 3_600_000;
        self.store.remove_before(cutoff)
    }

    async fn blocking(self: &Arc<Self>, job: fn(&Self) -> anyhow::Result<()>) {
        let history = Arc::clone(self);
        match tokio::task::spawn_blocking(move || job(&history)).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => warn!(error = format!("{error:#}"), "history write failed"),
            Err(error) => warn!(%error, "history task failed"),
        }
    }
}

fn series_key(device: &DeviceId, point: &str) -> String {
    format!("{device}\t{point}")
}
