//! A `HomeKit` bridge in front of a simulated colour lamp, to test pairing with a real
//! controller without touching real lights.
//!
//! `cargo run -p hestia-homekit --example fake_bridge -- <data dir> <port>`

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use hestia_core::{DeviceInfo, PointKind, PointSpec, Rgb, Semantic, Unit, Value};
use hestia_homekit::{HomeKit, HomeKitConfig};
use hestia_hub::{BoxFuture, Driver, DriverContext, Hub, spawn_driver};

struct SimulatedLamp;

impl Driver for SimulatedLamp {
    fn kind(&self) -> &'static str {
        "simulated"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(async move {
            let id = ctx.device_id("lamp")?;
            let numeric = |min, max, step| PointKind::Numeric { min, max, step };
            ctx.upsert_device(DeviceInfo {
                id: id.clone(),
                name: "Lampe test".to_owned(),
                manufacturer: Some("Simulation".to_owned()),
                model: Some("SIM-1".to_owned()),
                points: vec![
                    PointSpec::new("on", "Allumage", PointKind::Binary, Semantic::OnOff).writable(),
                    PointSpec::new(
                        "brightness",
                        "Luminosité",
                        numeric(0.0, 100.0, 1.0),
                        Semantic::Brightness,
                    )
                    .writable()
                    .with_unit(Unit::Percent),
                    PointSpec::new("color", "Couleur", PointKind::Color, Semantic::Color)
                        .writable(),
                    PointSpec::new(
                        "color_temp",
                        "Blanc",
                        numeric(2000.0, 9000.0, 100.0),
                        Semantic::ColorTemperature,
                    )
                    .writable()
                    .with_unit(Unit::Kelvin),
                    PointSpec::new(
                        "color_mode",
                        "Mode",
                        PointKind::Enum {
                            values: vec!["color".to_owned(), "white".to_owned()],
                        },
                        Semantic::ColorMode,
                    ),
                ],
            });
            ctx.set_state(&id, "on", Value::Bool(false));
            ctx.set_state(&id, "brightness", Value::Number(50.0));
            ctx.set_state(&id, "color_temp", Value::Number(4000.0));
            ctx.set_state(&id, "color_mode", Value::Text("white".to_owned()));
            ctx.set_state(
                &id,
                "color",
                Value::Color(Rgb {
                    r: 255,
                    g: 255,
                    b: 255,
                }),
            );
            ctx.set_available(&id, true);
            loop {
                let pending = ctx.next_command().await;
                let command = pending.command().clone();
                println!("command: {} = {:?}", command.point, command.value);
                let mode = match command.point.as_str() {
                    "color" => Some("color"),
                    "color_temp" => Some("white"),
                    _ => None,
                };
                if let Some(mode) = mode {
                    ctx.set_state(&id, "color_mode", Value::Text(mode.to_owned()));
                }
                ctx.set_state(&id, &command.point, command.value);
                pending.succeed();
            }
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("debug"))
        .with_ansi(false)
        .init();
    let mut args = std::env::args().skip(1);
    let data_dir = PathBuf::from(args.next().unwrap_or_else(|| "homekit-test".to_owned()));
    let port = args.next().map_or(Ok(51899), |port| port.parse())?;
    let hub = Hub::new(HashMap::new());
    spawn_driver(&hub, "simulated", Arc::new(SimulatedLamp));
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    let config = HomeKitConfig {
        name: "Hestia Test".to_owned(),
        port,
        ..HomeKitConfig::default()
    };
    let homekit = HomeKit::start(Arc::clone(&hub), &config, &data_dir).await?;
    let status = homekit.status();
    println!(
        "ready port={port} code={} paired={}",
        status.setup_code, status.paired
    );
    tokio::signal::ctrl_c().await?;
    Ok(())
}
