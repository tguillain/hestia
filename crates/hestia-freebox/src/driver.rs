use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::bail;
use hestia_core::{DeviceId, DeviceInfo, PointKind, PointSpec, Semantic, Unit, Value};
use hestia_hub::{BoxFuture, Driver, DriverContext, PendingCommand};
use serde::Deserialize;
use serde_json::json;
use tokio::time::{self, MissedTickBehavior};
use tracing::{info, warn};

use crate::api::{ApiError, ApiVersion, FreeboxApi};
use crate::model::{Connection, LanHost, Reading, System, WifiConfig};
use crate::presence::{LAN_HOSTS, People, Person, is_home, load_people};
use crate::token::{Token, load_token};

/// The dashboard writes the token: pick it up quickly.
const TOKEN_CHECK: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct FreeboxConfig {
    pub host: String,
    pub port: u16,
    pub poll_interval_s: u64,
    /// A followed phone unseen for this long counts as away.
    pub presence_grace_s: u64,
}

impl Default for FreeboxConfig {
    fn default() -> Self {
        Self {
            host: "mafreebox.freebox.fr".to_owned(),
            port: 80,
            poll_interval_s: 15,
            presence_grace_s: 600,
        }
    }
}

pub struct FreeboxDriver {
    config: FreeboxConfig,
    data_dir: PathBuf,
}

/// What the device currently declares, to re-declare it only when it changes.
#[derive(PartialEq)]
struct Layout {
    readings: Vec<(String, String, bool)>,
    wifi: bool,
}

impl FreeboxDriver {
    /// The app token is read from `data_dir`.
    #[must_use]
    pub fn new(config: FreeboxConfig, data_dir: PathBuf) -> Self {
        Self { config, data_dir }
    }

    async fn serve(&self, ctx: &mut DriverContext) -> anyhow::Result<()> {
        let token = self.wait_for_token(ctx).await;
        let (mut api, version) = FreeboxApi::discover(&self.config.host, self.config.port).await?;
        if token.box_uid != version.uid {
            bail!(
                "the saved token belongs to another Freebox: run `hestia freebox register` again"
            );
        }
        api.login(&token.app_token).await?;
        info!(
            model = version.box_model_name,
            api = version.api_version,
            "Freebox session open"
        );
        let id = ctx.device_id("box")?;
        let mut layout: Option<Layout> = None;
        let mut presence = Presence::default();
        let mut poll = time::interval(Duration::from_secs(self.config.poll_interval_s.max(5)));
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = poll.tick() => {
                    let mut result = poll_once(ctx, &api, &id, &version, &mut layout).await;
                    if result.is_ok() {
                        let people = load_people(&self.data_dir);
                        result = presence
                            .poll(ctx, &api, &people, self.config.presence_grace_s)
                            .await;
                    }
                    match result {
                        Err(ApiError::Session) => api.login(&token.app_token).await?,
                        result => result?,
                    }
                }
                pending = ctx.next_command() => {
                    let mut outcome = write(&api, &pending).await;
                    if matches!(outcome, Err(ApiError::Session)) {
                        api.login(&token.app_token).await?;
                        outcome = write(&api, &pending).await;
                    }
                    finish(ctx, pending, outcome);
                }
            }
        }
    }

    async fn wait_for_token(&self, ctx: &mut DriverContext) -> Token {
        let mut announced = false;
        loop {
            match load_token(&self.data_dir) {
                Ok(Some(token)) => return token,
                Ok(None) if !announced => {
                    info!("Freebox not authorised yet: run `hestia freebox register`");
                    announced = true;
                }
                Ok(None) => {}
                Err(error) => warn!(%error, "cannot read Freebox token"),
            }
            tokio::select! {
                () = time::sleep(TOKEN_CHECK) => {}
                pending = ctx.next_command() => pending.fail("Freebox not authorised yet"),
            }
        }
    }
}

impl Driver for FreeboxDriver {
    fn kind(&self) -> &'static str {
        "freebox"
    }

    fn run<'a>(&'a self, ctx: &'a mut DriverContext) -> BoxFuture<'a, anyhow::Result<()>> {
        Box::pin(self.serve(ctx))
    }
}

async fn poll_once(
    ctx: &DriverContext,
    api: &FreeboxApi,
    id: &DeviceId,
    version: &ApiVersion,
    layout: &mut Option<Layout>,
) -> Result<(), ApiError> {
    let connection: Connection = api.get("connection/").await?;
    let system: System = api.get("system/").await?;
    let wifi = match api.get::<WifiConfig>("wifi/config/").await {
        Ok(wifi) => Some(wifi),
        Err(ApiError::Permission(_)) => None,
        Err(error) => return Err(error),
    };
    let readings = system.readings();
    let current = Layout {
        readings: readings
            .iter()
            .map(|reading| (reading.key.clone(), reading.label.clone(), reading.fan))
            .collect(),
        wifi: wifi.is_some(),
    };
    if layout.as_ref() != Some(&current) {
        ctx.upsert_device(device_info(id.clone(), version, &readings, wifi.is_some()));
        *layout = Some(current);
    }
    #[allow(clippy::cast_precision_loss)]
    let bits = |bytes: u64| Value::Number(bytes.saturating_mul(8) as f64);
    ctx.set_state(id, "internet", Value::Bool(connection.state == "up"));
    ctx.set_state(id, "download", bits(connection.rate_down));
    ctx.set_state(id, "upload", bits(connection.rate_up));
    ctx.set_state(id, "ipv4", Value::Text(connection.ipv4));
    ctx.set_state(
        id,
        "firmware",
        Value::Text(system.firmware_version.chars().take(64).collect()),
    );
    for reading in readings {
        ctx.set_state(id, &reading.key, Value::Number(reading.value));
    }
    if let Some(wifi) = wifi {
        ctx.set_state(id, "wifi", Value::Bool(wifi.enabled));
    }
    ctx.set_available(id, true);
    Ok(())
}

/// The people declared as devices, and whether the box refused the network browser.
#[derive(Default)]
struct Presence {
    declared: Vec<Person>,
    refused: bool,
}

impl Presence {
    async fn poll(
        &mut self,
        ctx: &DriverContext,
        api: &FreeboxApi,
        people: &People,
        grace_s: u64,
    ) -> Result<(), ApiError> {
        if self.declared != people.people {
            for gone in self
                .declared
                .iter()
                .filter(|person| !people.people.contains(person))
            {
                if let Ok(id) = ctx.device_id(&person_native(gone)) {
                    ctx.remove_device(&id);
                }
            }
            for person in &people.people {
                if let Ok(id) = ctx.device_id(&person_native(person)) {
                    ctx.upsert_device(person_info(id, person));
                }
            }
            self.declared.clone_from(&people.people);
        }
        if people.people.is_empty() {
            return Ok(());
        }
        let hosts: Vec<LanHost> = match api.get(LAN_HOSTS).await {
            Ok(hosts) => hosts,
            Err(ApiError::Permission(reason)) => {
                if !self.refused {
                    warn!(
                        reason,
                        "Freebox refuses the network browser: presence unavailable"
                    );
                    self.refused = true;
                }
                for person in &people.people {
                    if let Ok(id) = ctx.device_id(&person_native(person)) {
                        ctx.set_available(&id, false);
                    }
                }
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        self.refused = false;
        let now_s = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs());
        for person in &people.people {
            let Ok(id) = ctx.device_id(&person_native(person)) else {
                continue;
            };
            let host = hosts
                .iter()
                .find(|host| host.mac().as_deref() == Some(person.mac.as_str()));
            ctx.set_state(&id, "home", Value::Bool(is_home(host, now_s, grace_s)));
            ctx.set_available(&id, true);
        }
        Ok(())
    }
}

fn person_native(person: &Person) -> String {
    format!("person-{}", person.id)
}

fn person_info(id: DeviceId, person: &Person) -> DeviceInfo {
    DeviceInfo {
        id,
        name: person.name.clone(),
        manufacturer: None,
        model: Some("Présence".to_owned()),
        points: vec![PointSpec::new(
            "home",
            "À la maison",
            PointKind::Binary,
            Semantic::Presence,
        )],
    }
}

/// Sends the Wi-Fi switch command; returns the state the box reports.
async fn write(api: &FreeboxApi, pending: &PendingCommand) -> Result<Option<bool>, ApiError> {
    let command = pending.command();
    let (true, Value::Bool(enabled)) = (command.point == "wifi", &command.value) else {
        return Ok(None);
    };
    let wifi: WifiConfig = api
        .put("wifi/config/", &json!({ "enabled": enabled }))
        .await?;
    Ok(Some(wifi.enabled))
}

fn finish(ctx: &DriverContext, pending: PendingCommand, outcome: Result<Option<bool>, ApiError>) {
    match outcome {
        Ok(Some(enabled)) => {
            ctx.set_state(&pending.command().device, "wifi", Value::Bool(enabled));
            pending.succeed();
        }
        Ok(None) => {
            let reason = format!("point {:?} cannot be written", pending.command().point);
            pending.fail(reason);
        }
        Err(ApiError::Permission(_)) => {
            pending.fail(
                "permission « Modification des réglages de la Freebox » manquante pour Hestia",
            );
        }
        Err(error) => pending.fail(error.to_string()),
    }
}

fn device_info(id: DeviceId, version: &ApiVersion, readings: &[Reading], wifi: bool) -> DeviceInfo {
    let throughput = |key: &str, label: &str| {
        PointSpec::new(
            key,
            label,
            PointKind::Numeric {
                min: 0.0,
                max: 1e12,
                step: 1.0,
            },
            Semantic::Throughput,
        )
        .with_unit(Unit::BitPerSecond)
    };
    let mut points = vec![
        PointSpec::new(
            "internet",
            "Internet",
            PointKind::Binary,
            Semantic::Connectivity,
        ),
        throughput("download", "Débit descendant"),
        throughput("upload", "Débit montant"),
        PointSpec::new(
            "ipv4",
            "IP externe",
            PointKind::Text { max_len: 64 },
            Semantic::Other,
        ),
    ];
    if wifi {
        points.push(PointSpec::new("wifi", "Wi-Fi", PointKind::Binary, Semantic::OnOff).writable());
    }
    points.extend(readings.iter().map(Reading::spec));
    points.push(PointSpec::new(
        "firmware",
        "Firmware",
        PointKind::Text { max_len: 64 },
        Semantic::Other,
    ));
    DeviceInfo {
        id,
        name: "Freebox".to_owned(),
        manufacturer: Some("Free".to_owned()),
        model: Some(version.box_model_name.clone()).filter(|model| !model.is_empty()),
        points,
    }
}
