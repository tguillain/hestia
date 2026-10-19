use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use hestia_core::{DeviceId, PointKind, Semantic};
use hestia_hub::{DeviceSnapshot, Hub};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::accessories::{self, BridgeInfo, status};
use crate::http::{self, Request};
use crate::mdns::Advertiser;
use crate::pair_setup::{self, SetupProgress};
use crate::pair_verify::{self, VerifyProgress};
use crate::pairings;
use crate::session::EncryptedSession;
use crate::state::{BridgeState, LightShape, StoredLight, Temperature};

pub(crate) type CharacteristicEvent = (u64, u64, Json);

pub(crate) struct Controller {
    pub id: String,
    pub admin: bool,
}

/// Per-TCP-connection protocol state.
pub(crate) struct Connection {
    pub id: u64,
    pub setup: Option<SetupProgress>,
    pub verify: Option<VerifyProgress>,
    pub controller: Option<Controller>,
    pub subscriptions: HashSet<(u64, u64)>,
}

impl Connection {
    pub(crate) fn new(id: u64) -> Self {
        Self {
            id,
            setup: None,
            verify: None,
            controller: None,
            subscriptions: HashSet::new(),
        }
    }
}

pub(crate) struct Bridge {
    pub hub: Arc<Hub>,
    pub name: String,
    pub failed_setups: AtomicU32,
    pub events: broadcast::Sender<Arc<Vec<CharacteristicEvent>>>,
    pub pairings: broadcast::Sender<()>,
    state: Mutex<BridgeState>,
    setup_owner: Mutex<Option<u64>>,
    advertiser: Advertiser,
    excluded: HashSet<DeviceId>,
}

impl Bridge {
    pub(crate) fn new(
        hub: Arc<Hub>,
        name: String,
        state: BridgeState,
        advertiser: Advertiser,
        excluded: HashSet<DeviceId>,
    ) -> Self {
        Self {
            hub,
            name,
            failed_setups: AtomicU32::new(0),
            events: broadcast::channel(64).0,
            pairings: broadcast::channel(8).0,
            state: Mutex::new(state),
            setup_owner: Mutex::new(None),
            advertiser,
            excluded,
        }
    }

    pub(crate) fn state(&self) -> MutexGuard<'_, BridgeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Only one controller may run pair-setup at a time.
    pub(crate) fn claim_setup(&self, connection: u64) -> bool {
        let mut owner = self
            .setup_owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match *owner {
            Some(current) if current != connection => false,
            _ => {
                *owner = Some(connection);
                true
            }
        }
    }

    pub(crate) fn release_setup(&self, connection: u64) {
        let mut owner = self
            .setup_owner
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if *owner == Some(connection) {
            *owner = None;
        }
    }

    pub(crate) fn pairings_updated(&self) {
        let _ = self.pairings.send(());
        self.advertise();
    }

    pub(crate) fn has_controller(&self, id: &str) -> bool {
        self.state().pairing(id).is_some()
    }

    pub(crate) fn advertise(&self) {
        let txt = {
            let state = self.state();
            HashMap::from([
                ("c#".to_owned(), state.config_number().to_string()),
                ("ff".to_owned(), "0".to_owned()),
                ("id".to_owned(), state.device_id().to_owned()),
                ("md".to_owned(), self.name.clone()),
                ("pv".to_owned(), "1.1".to_owned()),
                ("s#".to_owned(), "1".to_owned()),
                (
                    "sf".to_owned(),
                    if state.is_paired() { "0" } else { "1" }.to_owned(),
                ),
                ("ci".to_owned(), "2".to_owned()),
                ("sh".to_owned(), state.setup_hash()),
            ])
        };
        self.advertiser.publish(txt);
    }

    /// Remembers every light the hub currently declares.
    pub(crate) fn sync_lights(&self) {
        let mut layout_changed = false;
        for snapshot in self.hub.devices() {
            if self.excluded.contains(&snapshot.id) {
                continue;
            }
            let Some(shape) = light_shape(&snapshot) else {
                continue;
            };
            match self.state().remember_light(&snapshot.id, shape) {
                Ok(changed) => layout_changed |= changed,
                Err(error) => warn!(%error, "cannot save HomeKit accessories"),
            }
        }
        if layout_changed {
            info!(
                lights = self.state().lights().len(),
                "HomeKit accessory list updated"
            );
            self.advertise();
        }
    }

    /// Pushes new values to subscribed controllers after a hub state change.
    pub(crate) fn notify(&self, device: &DeviceId, point: &str) {
        let (lights, serial) = self.snapshot();
        let info = BridgeInfo {
            name: &self.name,
            serial: &serial,
        };
        let batch: Vec<CharacteristicEvent> = accessories::affected(&lights, device, point)
            .into_iter()
            .filter_map(|(aid, iid)| {
                accessories::read(&info, &lights, &self.hub, aid, iid)
                    .ok()
                    .map(|value| (aid, iid, value))
            })
            .collect();
        if !batch.is_empty() {
            let _ = self.events.send(Arc::new(batch));
        }
    }

    pub(crate) async fn dispatch(
        &self,
        connection: &mut Connection,
        request: Request,
    ) -> (Vec<u8>, Option<EncryptedSession>) {
        let reply = match (request.method.as_str(), request.path.as_str()) {
            ("POST", "/pair-setup") => {
                let body = pair_setup::handle(self, connection, &request.body).await;
                http::response(200, http::TLV, &body)
            }
            ("POST", "/pair-verify") => {
                let (body, session) = pair_verify::handle(self, connection, &request.body);
                return (http::response(200, http::TLV, &body), session);
            }
            ("POST", "/identify") if !self.state().is_paired() => http::response(204, "", &[]),
            _ if connection.controller.is_none() => {
                http::response(470, http::JSON, br#"{"status":-70401}"#)
            }
            ("GET", "/accessories") => {
                let (lights, serial) = self.snapshot();
                let info = BridgeInfo {
                    name: &self.name,
                    serial: &serial,
                };
                let body = accessories::database(&info, &lights, &self.hub).to_string();
                http::response(200, http::JSON, body.as_bytes())
            }
            ("GET", "/characteristics") => self.read_characteristics(&request.query),
            ("PUT", "/characteristics") => {
                self.write_characteristics(connection, &request.body).await
            }
            ("PUT", "/prepare") => http::response(200, http::JSON, br#"{"status":0}"#),
            ("POST", "/pairings") => http::response(
                200,
                http::TLV,
                &pairings::handle(self, connection, &request.body),
            ),
            _ => http::response(404, http::JSON, br#"{"status":-70409}"#),
        };
        (reply, None)
    }

    fn read_characteristics(&self, query: &str) -> Vec<u8> {
        let ids = query
            .split('&')
            .find_map(|pair| pair.strip_prefix("id="))
            .unwrap_or_default();
        let (lights, serial) = self.snapshot();
        let info = BridgeInfo {
            name: &self.name,
            serial: &serial,
        };
        let mut all_ok = true;
        let items: Vec<Json> = ids
            .split(',')
            .filter_map(|id| id.split_once('.'))
            .filter_map(|(aid, iid)| Some((aid.parse::<u64>().ok()?, iid.parse::<u64>().ok()?)))
            .map(
                |(aid, iid)| match accessories::read(&info, &lights, &self.hub, aid, iid) {
                    Ok(value) => json!({"aid": aid, "iid": iid, "value": value}),
                    Err(code) => {
                        all_ok = false;
                        json!({"aid": aid, "iid": iid, "status": code})
                    }
                },
            )
            .collect();
        let items = if all_ok {
            items
        } else {
            items
                .into_iter()
                .map(|mut item| {
                    if item.get("status").is_none() {
                        item["status"] = json!(status::OK);
                    }
                    item
                })
                .collect()
        };
        let body = json!({ "characteristics": items }).to_string();
        http::response(if all_ok { 200 } else { 207 }, http::JSON, body.as_bytes())
    }

    async fn write_characteristics(&self, connection: &mut Connection, body: &[u8]) -> Vec<u8> {
        #[derive(Deserialize)]
        struct Item {
            aid: u64,
            iid: u64,
            #[serde(default)]
            value: Option<Json>,
            #[serde(default)]
            ev: Option<bool>,
        }
        #[derive(Deserialize)]
        struct Body {
            characteristics: Vec<Item>,
        }
        let Ok(request) = serde_json::from_slice::<Body>(body) else {
            return http::response(400, http::JSON, br#"{"status":-70410}"#);
        };
        let (lights, _) = self.snapshot();
        let mut statuses: Vec<(u64, u64, i32)> = Vec::new();
        let mut writes: BTreeMap<u64, Vec<(u64, Json)>> = BTreeMap::new();
        for item in request.characteristics {
            if let Some(subscribe) = item.ev {
                if subscribe {
                    connection.subscriptions.insert((item.aid, item.iid));
                } else {
                    connection.subscriptions.remove(&(item.aid, item.iid));
                }
                if item.value.is_none() {
                    statuses.push((item.aid, item.iid, status::OK));
                }
            }
            if let Some(value) = item.value {
                writes.entry(item.aid).or_default().push((item.iid, value));
            }
        }
        for (aid, changes) in writes {
            if aid == accessories::BRIDGE_AID {
                statuses.extend(changes.iter().map(|(iid, _)| {
                    let code = if *iid == accessories::iid::IDENTIFY {
                        status::OK
                    } else {
                        status::READ_ONLY
                    };
                    (aid, *iid, code)
                }));
                continue;
            }
            let Some((device, light)) = accessories::find(&lights, aid) else {
                statuses.extend(
                    changes
                        .iter()
                        .map(|(iid, _)| (aid, *iid, status::NOT_FOUND)),
                );
                continue;
            };
            for (iid, code) in accessories::write(device, light, &self.hub, &changes).await {
                statuses.push((aid, iid, code));
            }
        }
        if statuses.iter().all(|(_, _, code)| *code == status::OK) {
            return http::response(204, "", &[]);
        }
        let items: Vec<Json> = statuses
            .into_iter()
            .map(|(aid, iid, code)| json!({"aid": aid, "iid": iid, "status": code}))
            .collect();
        let body = json!({ "characteristics": items }).to_string();
        http::response(207, http::JSON, body.as_bytes())
    }

    fn snapshot(&self) -> (BTreeMap<DeviceId, StoredLight>, String) {
        let state = self.state();
        (state.lights().clone(), state.device_id().to_owned())
    }
}

/// A light is anything with writable on/off and brightness points.
fn light_shape(snapshot: &DeviceSnapshot) -> Option<LightShape> {
    let writable = |semantic: Semantic| {
        snapshot
            .points
            .iter()
            .find(|point| point.spec.semantic == semantic && point.spec.is_writable())
    };
    let temperature =
        writable(Semantic::ColorTemperature).and_then(|point| match point.spec.kind {
            PointKind::Numeric { min, max, .. } => Some(Temperature {
                key: point.spec.key.clone(),
                min_kelvin: min,
                max_kelvin: max,
            }),
            _ => None,
        });
    Some(LightShape {
        name: snapshot.name.clone(),
        manufacturer: snapshot.manufacturer.clone(),
        model: snapshot.model.clone(),
        on: writable(Semantic::OnOff)?.spec.key.clone(),
        brightness: writable(Semantic::Brightness)?.spec.key.clone(),
        color: writable(Semantic::Color).map(|point| point.spec.key.clone()),
        temperature,
    })
}
