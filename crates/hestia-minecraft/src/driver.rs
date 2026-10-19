use std::time::Duration;

use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Unit, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext};
use serde::Deserialize;
use tokio::time::{self, MissedTickBehavior};
use tracing::debug;

use crate::protocol::query;

const QUERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct MinecraftConfig {
    pub servers: Vec<ServerConfig>,
    pub poll_interval_s: u64,
}

impl Default for MinecraftConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            poll_interval_s: 30,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub name: Option<String>,
}

fn default_port() -> u16 {
    25565
}

pub struct MinecraftDriver {
    config: MinecraftConfig,
}

impl MinecraftDriver {
    #[must_use]
    pub fn new(config: MinecraftConfig) -> Self {
        Self { config }
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        let mut servers = Vec::new();
        for server in &self.config.servers {
            let id = ctx.device_id(&format!("{}-{}", server.host, server.port))?;
            ctx.upsert_device(server_info(id.clone(), server));
            ctx.set_available(&id, true);
            servers.push((id, server));
        }
        let mut poll = time::interval(Duration::from_secs(self.config.poll_interval_s.max(5)));
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    for (id, server) in &servers {
                        refresh(ctx, id, server).await;
                    }
                }
                pending = ctx.next_command() => pending.fail("Minecraft servers are read-only"),
            }
        }
    }
}

impl Driver for MinecraftDriver {
    fn kind(&self) -> &'static str {
        "minecraft"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}

async fn refresh(ctx: &DriverContext, id: &DeviceId, server: &ServerConfig) {
    match query(&server.host, server.port, QUERY_TIMEOUT).await {
        Ok(status) => {
            ctx.set_state(id, "online", Value::Bool(true));
            ctx.set_state(
                id,
                "players",
                Value::Number(f64::from(status.online_players)),
            );
            ctx.set_state(
                id,
                "max_players",
                Value::Number(f64::from(status.max_players)),
            );
            #[allow(clippy::cast_precision_loss)]
            ctx.set_state(
                id,
                "latency",
                Value::Number(status.latency_ms.min(60_000) as f64),
            );
            ctx.set_state(id, "version", Value::Text(truncate(&status.version, 200)));
            ctx.set_state(id, "motd", Value::Text(truncate(&status.motd, 500)));
            ctx.set_state(
                id,
                "player_names",
                Value::Text(truncate(&status.player_names.join(", "), 1000)),
            );
        }
        Err(error) => {
            debug!(server = %id, error = format!("{error:#}"), "Minecraft server unreachable");
            ctx.set_state(id, "online", Value::Bool(false));
            ctx.set_state(id, "players", Value::Number(0.0));
            ctx.set_state(id, "player_names", Value::Text(String::new()));
        }
    }
}

fn truncate(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

fn server_info(id: DeviceId, server: &ServerConfig) -> DeviceInfo {
    let count = |key: &str, label: &str| {
        PointSpec::new(
            key,
            label,
            PointKind::Numeric {
                min: 0.0,
                max: 1_000_000.0,
                step: 1.0,
            },
            Semantic::Count,
        )
    };
    let text = |key: &str, label: &str, max_len| {
        PointSpec::new(key, label, PointKind::Text { max_len }, Semantic::Other)
    };
    DeviceInfo {
        id,
        name: server
            .name
            .clone()
            .unwrap_or_else(|| format!("Minecraft {}", server.host)),
        manufacturer: Some("Mojang".to_owned()),
        model: Some("Serveur Java".to_owned()),
        points: vec![
            PointSpec::new(
                "online",
                "En ligne",
                PointKind::Binary,
                Semantic::Connectivity,
            ),
            count("players", "Joueurs"),
            count("max_players", "Places"),
            text("player_names", "Joueurs connectés", 1000),
            PointSpec::new(
                "latency",
                "Latence",
                PointKind::Numeric {
                    min: 0.0,
                    max: 60_000.0,
                    step: 1.0,
                },
                Semantic::Latency,
            )
            .with_unit(Unit::Millisecond),
            text("version", "Version", 200),
            text("motd", "Message du monde", 500),
        ],
    }
}
