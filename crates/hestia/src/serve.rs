use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use hestia_api::{Auth, Dashboard, Setup};
use hestia_freebox::FreeboxConfig;
use hestia_history::History;
use hestia_homekit::HomeKit;
use hestia_hub::{Event, Hub, spawn_driver};
use tokio::net::TcpListener;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{Notify, watch};
use tracing::{debug, error, info, warn};

use crate::config::{Config, DriverKind};

/// Open event streams would otherwise keep a graceful shutdown waiting forever.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

pub async fn run(config: Config) -> anyhow::Result<()> {
    let auth = Arc::new(Auth::open(&config.data_dir)?);
    if auth.users().names().is_empty() {
        warn!("no account yet: create one with `hestia user add <name>`");
    }
    let hub = Hub::new(config.labels());
    let history = if config.history.enabled {
        let history = History::open(&config.data_dir, config.history.clone())?;
        history.spawn(Arc::clone(&hub));
        Some(history)
    } else {
        None
    };
    // "Rechercher des appareils" bumps this counter; drivers that can discover react to it.
    let (discovery, discovery_requests) = watch::channel(0_u64);
    for driver in &config.drivers {
        spawn_driver(
            &hub,
            &driver.id,
            driver.build(&config.data_dir, &discovery_requests)?,
        );
    }
    tokio::spawn(log_events(Arc::clone(&hub)));
    // Apple Home is a bonus: a failure there must not take the dashboard down.
    let homekit = if config.homekit.enabled {
        match HomeKit::start(Arc::clone(&hub), &config.homekit, &config.data_dir).await {
            Ok(homekit) => Some(Arc::new(homekit)),
            Err(failure) => {
                error!(error = format!("{failure:#}"), "HomeKit bridge not started");
                None
            }
        }
    } else {
        None
    };

    let listener = TcpListener::bind(config.http.listen)
        .await
        .with_context(|| format!("cannot listen on {}", config.http.listen))?;
    info!(address = %config.http.listen, drivers = config.drivers.len(), "hestia started");
    let (stop, mut stopped) = watch::channel(false);
    let restart = Arc::new(Notify::new());
    let dashboard = Dashboard {
        history: history.clone(),
        charts: config.charts.clone(),
        setup: Setup {
            data_dir: config.data_dir.clone(),
            homekit,
            freebox: config
                .settings_of::<FreeboxConfig>(DriverKind::Freebox)
                .transpose()?,
            version: crate::VERSION.to_owned(),
            restart: Some(Arc::clone(&restart)),
            update_repo: Some(config.update.github.clone()).filter(|repo| !repo.is_empty()),
            discovery: Some(discovery),
        },
    };
    let router = hestia_api::router(Arc::clone(&hub), auth, dashboard);
    let mut server = tokio::spawn(hestia_api::serve(listener, router, async move {
        let _ = stopped.wait_for(|stop| *stop).await;
    }));

    // A clean exit after an update: systemd starts the new program right away.
    tokio::select! {
        () = shutdown_signal() => info!("stopping"),
        () = restart.notified() => info!("stopping to start the updated program"),
        result = &mut server => {
            result??;
            bail!("HTTP server stopped unexpectedly");
        }
    }
    let _ = stop.send(true);
    if let Ok(Ok(Err(error))) = tokio::time::timeout(SHUTDOWN_GRACE, server).await {
        warn!(%error, "HTTP server error during shutdown");
    }
    if let Some(history) = history {
        tokio::task::spawn_blocking(move || history.flush()).await??;
    }
    Ok(())
}

async fn log_events(hub: Arc<Hub>) {
    let mut events = hub.subscribe();
    loop {
        match events.recv().await {
            Ok(Event::State {
                device,
                point,
                value,
                ..
            }) => debug!(%device, point, ?value, "state"),
            Ok(Event::Availability { device, available }) => {
                info!(%device, available, "availability");
            }
            Ok(Event::DeviceUpdated { device }) => info!(%device, "device declared"),
            Ok(Event::Command { .. }) => {}
            Err(RecvError::Lagged(skipped)) => warn!(skipped, "event log lagged"),
            Err(RecvError::Closed) => return,
        }
    }
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        match signal(SignalKind::terminate()) {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
