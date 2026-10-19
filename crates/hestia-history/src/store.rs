use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use redb::{Database, Durability, ReadableDatabase, ReadableTable, TableDefinition};
use serde::Serialize;

/// Series name (`device<TAB>point`) to a compact numeric id.
const SERIES: TableDefinition<&str, u32> = TableDefinition::new("series");
/// `(series id, unix ms)` to value.
const SAMPLES: TableDefinition<(u32, i64), f64> = TableDefinition::new("samples");

/// Evenly spaced averages; `None` until the first known value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Buckets {
    pub from: i64,
    pub step: i64,
    pub values: Vec<Option<f64>>,
}

pub(crate) struct Store {
    db: Database,
    ids: Mutex<HashMap<String, u32>>,
}

impl Store {
    pub(crate) fn open(path: &Path) -> anyhow::Result<Self> {
        let db = Database::create(path)?;
        let write = db.begin_write()?;
        let ids = {
            let table = write.open_table(SERIES)?;
            write.open_table(SAMPLES)?;
            table
                .iter()?
                .map(|entry| entry.map(|(name, id)| (name.value().to_owned(), id.value())))
                .collect::<Result<HashMap<_, _>, _>>()?
        };
        write.commit()?;
        Ok(Self {
            db,
            ids: Mutex::new(ids),
        })
    }

    pub(crate) fn insert<'a>(
        &self,
        samples: impl Iterator<Item = (&'a str, i64, f64)>,
    ) -> anyhow::Result<()> {
        let mut write = self.db.begin_write()?;
        write.set_durability(Durability::Immediate)?;
        {
            let mut ids = self.ids.lock().unwrap_or_else(PoisonError::into_inner);
            let mut series = write.open_table(SERIES)?;
            let mut table = write.open_table(SAMPLES)?;
            for (name, at, value) in samples {
                let id = if let Some(id) = ids.get(name) {
                    *id
                } else {
                    let id = u32::try_from(ids.len())?;
                    series.insert(name, id)?;
                    ids.insert(name.to_owned(), id);
                    id
                };
                table.insert((id, at), value)?;
            }
        }
        write.commit()?;
        Ok(())
    }

    pub(crate) fn remove_before(&self, cutoff: i64) -> anyhow::Result<()> {
        let ids: Vec<u32> = self
            .ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .copied()
            .collect();
        let write = self.db.begin_write()?;
        {
            let mut table = write.open_table(SAMPLES)?;
            for id in ids {
                // Keep the last sample before the cut-off: it still defines the value after it.
                let keep = table
                    .range((id, i64::MIN)..(id, cutoff))?
                    .next_back()
                    .transpose()?
                    .map(|(key, _)| key.value().1);
                if let Some(keep) = keep {
                    table.retain_in((id, i64::MIN)..(id, keep), |_, _| false)?;
                }
            }
        }
        write.commit()?;
        Ok(())
    }

    pub(crate) fn query(
        &self,
        series: &str,
        from: i64,
        to: i64,
        buckets: usize,
    ) -> anyhow::Result<Buckets> {
        let step = ((to - from) / i64::try_from(buckets)?).max(1);
        let mut sums = vec![(0.0, 0_u32); buckets];
        let id = self
            .ids
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(series)
            .copied();
        let mut carried = None;
        if let Some(id) = id {
            let read = self.db.begin_read()?;
            let table = read.open_table(SAMPLES)?;
            carried = table
                .range((id, i64::MIN)..(id, from))?
                .next_back()
                .transpose()?
                .map(|(_, value)| value.value());
            for entry in table.range((id, from)..=(id, to))? {
                let (key, value) = entry?;
                let index = usize::try_from((key.value().1 - from) / step)?.min(buckets - 1);
                sums[index].0 += value.value();
                sums[index].1 += 1;
            }
        }
        let values = sums
            .into_iter()
            .map(|(sum, count)| {
                if count > 0 {
                    carried = Some(sum / f64::from(count));
                }
                carried
            })
            .collect();
        Ok(Buckets { from, step, values })
    }
}

#[cfg(test)]
mod tests {
    use super::Store;

    fn store(name: &str) -> Store {
        let dir =
            std::env::temp_dir().join(format!("hestia-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Store::open(&dir.join("history.redb")).unwrap()
    }

    #[test]
    fn buckets_average_and_carry_forward() {
        let store = store("buckets");
        store
            .insert(
                [
                    ("lamp\tlevel", 5, 10.0),
                    ("lamp\tlevel", 15, 20.0),
                    ("lamp\tlevel", 18, 40.0),
                ]
                .into_iter(),
            )
            .unwrap();
        let buckets = store.query("lamp\tlevel", 10, 50, 4).unwrap();
        assert_eq!(buckets.step, 10);
        assert_eq!(
            buckets.values,
            [Some(30.0), Some(30.0), Some(30.0), Some(30.0)]
        );
        let earlier = store.query("lamp\tlevel", 0, 40, 4).unwrap();
        assert_eq!(
            earlier.values,
            [Some(10.0), Some(30.0), Some(30.0), Some(30.0)]
        );
        let unknown = store.query("lamp\tother", 0, 40, 4).unwrap();
        assert_eq!(unknown.values, [None, None, None, None]);
    }

    #[test]
    fn pruning_keeps_the_value_in_force() {
        let store = store("prune");
        store
            .insert(
                [
                    ("pc\tonline", 100, 1.0),
                    ("pc\tonline", 200, 0.0),
                    ("pc\tonline", 900, 1.0),
                ]
                .into_iter(),
            )
            .unwrap();
        store.remove_before(500).unwrap();
        let buckets = store.query("pc\tonline", 500, 1000, 5).unwrap();
        assert_eq!(
            buckets.values,
            [Some(0.0), Some(0.0), Some(0.0), Some(0.0), Some(1.0)]
        );
        let all = store.query("pc\tonline", 0, 1000, 10).unwrap();
        assert_eq!(all.values[1], None, "the sample at 100 was pruned");
    }
}
