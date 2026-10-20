//! Scenes and "when … then …" rules, set up from the Settings page and run inside Hestia:
//! commands go through the hub like any other, with the `automation` origin.

mod capture;
mod engine;
mod model;
mod sun;

use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use hestia_core::{Command, DeviceId, Origin, Semantic, Value};
use hestia_hub::storage::{load_json, save_json};
use hestia_hub::{DeviceSnapshot, Hub};
use tokio::task::JoinHandle;
use tracing::warn;

pub use capture::capture;
pub use model::{
    Action, Document, Location, PresenceEvent, Rule, Scene, SetPoint, SunEvent, Trigger,
};

pub struct Automations {
    hub: Arc<Hub>,
    path: PathBuf,
    document: RwLock<Document>,
}

impl Automations {
    /// Reads `<data>/automations.json`. A file that no longer validates is kept as is:
    /// rules pointing at a missing scene just fail when they fire.
    ///
    /// # Errors
    /// Fails when the file exists but cannot be read.
    pub fn open(hub: Arc<Hub>, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let path = data_dir.join("automations.json");
        let document: Document = load_json(&path)?;
        if let Err(problem) = document.validate() {
            warn!(problem, "automations need attention");
        }
        Ok(Arc::new(Self {
            hub,
            path,
            document: RwLock::new(document),
        }))
    }

    /// Starts watching the clock and the hub.
    pub fn spawn(self: &Arc<Self>) -> JoinHandle<()> {
        tokio::spawn(engine::run(Arc::clone(self)))
    }

    #[must_use]
    pub fn document(&self) -> Document {
        self.document
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Replaces everything at once, as the Settings page edits it. Empty ids get one.
    ///
    /// # Errors
    /// Why the document is refused, for the household.
    pub fn replace(&self, mut document: Document) -> Result<Document, String> {
        for scene in &mut document.scenes {
            if scene.id.is_empty() {
                scene.id = new_id();
            }
        }
        for rule in &mut document.rules {
            if rule.id.is_empty() {
                rule.id = new_id();
            }
        }
        document.validate()?;
        save_json(&self.path, &document).map_err(|error| format!("{error:#}"))?;
        *self
            .document
            .write()
            .unwrap_or_else(PoisonError::into_inner) = document.clone();
        Ok(document)
    }

    /// Records a new scene from the current state of `devices`.
    ///
    /// # Errors
    /// Fails when no device has anything to record, or the file cannot be written.
    pub fn capture_scene(&self, name: &str, devices: &[DeviceId]) -> Result<Scene, String> {
        let actions: Vec<SetPoint> = devices
            .iter()
            .filter_map(|id| self.hub.device(id))
            .flat_map(|device| capture(&device))
            .collect();
        if actions.is_empty() {
            return Err(
                "aucune lampe à enregistrer : choisis au moins une lampe allumable".to_owned(),
            );
        }
        let scene = Scene {
            id: new_id(),
            name: name.trim().to_owned(),
            actions,
        };
        let mut document = self.document();
        document.scenes.push(scene.clone());
        self.replace(document)?;
        Ok(scene)
    }

    /// # Errors
    /// Fails when the scene is unknown or a device refused a command.
    pub async fn run_scene(&self, id: &str, origin: Origin) -> Result<(), String> {
        let scene = self
            .document()
            .scene(id)
            .cloned()
            .ok_or_else(|| format!("scène « {id} » inconnue"))?;
        self.apply(&scene.actions, origin).await
    }

    /// Sunrise and sunset today, `HH:MM`, once the location is known.
    #[must_use]
    pub fn sun_today(&self) -> Option<(String, String)> {
        engine::sun_today(self.document().location?, &jiff::Zoned::now())
    }

    pub(crate) async fn perform(&self, action: &Action) -> Result<(), String> {
        match action {
            Action::Scene { scene } => self.run_scene(scene, Origin::Automation).await,
            Action::Set(set) => {
                self.apply(std::slice::from_ref(set), Origin::Automation)
                    .await
            }
            Action::LightsOff => {
                let lamps: Vec<SetPoint> = self
                    .hub
                    .devices()
                    .iter()
                    .filter(|device| is_lamp(device))
                    .filter_map(|device| {
                        let on = device.points.iter().find(|point| {
                            point.spec.semantic == Semantic::OnOff && point.spec.is_writable()
                        })?;
                        (on.value != Some(Value::Bool(false))).then(|| SetPoint {
                            device: device.id.clone(),
                            point: on.spec.key.clone(),
                            value: Value::Bool(false),
                        })
                    })
                    .collect();
                self.apply(&lamps, Origin::Automation).await
            }
        }
    }

    /// Every write is tried; the first failure is reported.
    async fn apply(&self, actions: &[SetPoint], origin: Origin) -> Result<(), String> {
        let mut failure = None;
        for action in actions {
            let command = Command {
                device: action.device.clone(),
                point: action.point.clone(),
                value: action.value.clone(),
            };
            if let Err(error) = self.hub.command(command, origin).await {
                failure.get_or_insert_with(|| format!("{} : {error}", action.device));
            }
        }
        failure.map_or(Ok(()), Err)
    }
}

/// A switch that also dims or colours: a lamp.
#[must_use]
pub fn is_lamp(device: &DeviceSnapshot) -> bool {
    let has = |semantic: Semantic| {
        device
            .points
            .iter()
            .any(|point| point.spec.semantic == semantic && point.spec.is_writable())
    };
    has(Semantic::OnOff) && (has(Semantic::Brightness) || has(Semantic::Color))
}

fn new_id() -> String {
    format!("{:08x}", rand::random::<u32>())
}
