use std::any::Any;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tracing::{error, info, warn};

use crate::driver::{Driver, DriverContext};
use crate::hub::Hub;

const MIN_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(60);
const STABLE_RUN: Duration = Duration::from_secs(60);

/// Runs `driver` under `instance` for the lifetime of the process, restarting it on failure.
pub fn spawn_driver(hub: &Arc<Hub>, instance: &str, driver: Arc<dyn Driver>) -> JoinHandle<()> {
    let ctx = DriverContext::new(Arc::clone(hub), instance.to_owned());
    tokio::spawn(supervise(Arc::clone(hub), driver, ctx))
}

async fn supervise(hub: Arc<Hub>, driver: Arc<dyn Driver>, mut ctx: DriverContext) {
    let mut backoff = MIN_BACKOFF;
    loop {
        let started = Instant::now();
        info!(
            instance = ctx.instance(),
            kind = driver.kind(),
            "driver starting"
        );
        match AssertUnwindSafe(driver.run(&mut ctx)).catch_unwind().await {
            Ok(Ok(())) => warn!(instance = ctx.instance(), "driver returned"),
            Ok(Err(failure)) => {
                warn!(
                    instance = ctx.instance(),
                    error = format!("{failure:#}"),
                    "driver failed"
                );
            }
            Err(panic) => {
                error!(
                    instance = ctx.instance(),
                    panic = panic_message(panic.as_ref()),
                    "driver panicked"
                );
            }
        }
        hub.mark_driver_down(ctx.instance());
        ctx.refuse_queued("driver restarting");
        if started.elapsed() >= STABLE_RUN {
            backoff = MIN_BACKOFF;
        }
        info!(
            instance = ctx.instance(),
            delay_s = backoff.as_secs(),
            "driver restart scheduled"
        );
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown panic")
}
