use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

const FREE_ATTEMPTS: u32 = 5;
const BASE_LOCK: Duration = Duration::from_secs(15 * 60);
const MAX_LOCK: Duration = Duration::from_secs(24 * 3600);
const MAX_TRACKED: usize = 10_000;

struct Failures {
    count: u32,
    locked_until: Option<Instant>,
}

/// Per-address brute-force brake: 5 free failures, then a lock doubling from 15 min to 24 h.
#[derive(Default)]
pub(crate) struct LoginLimiter {
    failures: Mutex<HashMap<IpAddr, Failures>>,
}

impl LoginLimiter {
    pub(crate) fn check(&self, ip: IpAddr) -> Result<(), Duration> {
        let now = Instant::now();
        match self
            .lock()
            .get(&ip)
            .and_then(|failures| failures.locked_until)
        {
            Some(until) if until > now => Err(until - now),
            _ => Ok(()),
        }
    }

    pub(crate) fn failure(&self, ip: IpAddr) {
        let now = Instant::now();
        let mut failures = self.lock();
        if failures.len() >= MAX_TRACKED {
            failures.retain(|_, entry| entry.locked_until.is_some_and(|until| until > now));
        }
        let entry = failures.entry(ip).or_insert(Failures {
            count: 0,
            locked_until: None,
        });
        entry.count += 1;
        if entry.count >= FREE_ATTEMPTS {
            let doublings = (entry.count - FREE_ATTEMPTS).min(16);
            let lock = BASE_LOCK.saturating_mul(1 << doublings).min(MAX_LOCK);
            entry.locked_until = Some(now + lock);
        }
    }

    pub(crate) fn success(&self, ip: IpAddr) {
        self.lock().remove(&ip);
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<IpAddr, Failures>> {
        self.failures.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::{BASE_LOCK, LoginLimiter};
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn locks_after_five_failures_per_address() {
        let limiter = LoginLimiter::default();
        let attacker = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7));
        let neighbour = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
        for _ in 0..4 {
            limiter.failure(attacker);
            assert!(limiter.check(attacker).is_ok());
        }
        limiter.failure(attacker);
        let wait = limiter.check(attacker).unwrap_err();
        assert!(wait + std::time::Duration::from_secs(1) > BASE_LOCK);
        assert!(limiter.check(neighbour).is_ok());
    }
}
