//! Scenes and automations, n8n style: graphs of triggers, conditions and actions, edited
//! on the Automatisations page and run inside Hestia. Commands go through the hub like any
//! other, with the `automation` origin.

mod capture;
mod check;
mod engine;
mod legacy;
mod model;
mod run;
mod runs;
mod sun;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::time::{Duration, Instant};

use hestia_core::{Command, DeviceId, Origin, Semantic, Value};
use hestia_hub::storage::{load_json, save_json};
use hestia_hub::{DeviceSnapshot, Hub, now_millis};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tracing::{info, warn};

pub use capture::capture;
pub use check::{MAX_EDGES, MAX_NODES};
pub use model::{
    Automation, Daylight, Document, Edge, Graph, Location, Mode, Node, Occupancy, Op,
    PresenceEvent, Rule, Scene, SetPoint, Step, SunEvent,
};
pub use run::{MAX_STEPS, Run, RunStatus, StepLog, StepStatus, show};

/// More starts than this within a minute switch the automation off.
pub const RUNAWAY: usize = 20;
/// Runs waiting their turn in `queued` mode.
const MAX_QUEUE: usize = 10;
/// Runs kept in memory for the page.
const RECENT_RUNS: usize = 200;

pub struct Automations {
    hub: Arc<Hub>,
    path: PathBuf,
    runs_path: PathBuf,
    document: RwLock<Document>,
    lanes: Mutex<HashMap<String, Lane>>,
    runs: Mutex<VecDeque<Run>>,
}

/// One automation's runs: they take turns.
#[derive(Default)]
struct Lane {
    turn: Arc<tokio::sync::Mutex<()>>,
    /// Stops the run in progress.
    cancel: Option<watch::Sender<bool>>,
    waiting: usize,
    starts: VecDeque<Instant>,
}

impl Automations {
    /// Reads `<data>/automations.json`; rules of the first version become automations.
    ///
    /// # Errors
    /// Fails when the file exists but cannot be read.
    pub fn open(hub: Arc<Hub>, data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let path = data_dir.join("automations.json");
        let mut document: Document = load_json(&path)?;
        let legacy = std::mem::take(&mut document.rules);
        if !legacy.is_empty() {
            info!(count = legacy.len(), "rules turned into automations");
            document
                .automations
                .extend(legacy.into_iter().map(legacy::LegacyRule::into_automation));
            save_json(&path, &document)?;
        }
        if let Err(problem) = check::check(&document) {
            warn!(problem, "automations need attention");
        }
        let runs_path = data_dir.join("automation-runs.jsonl");
        let mut runs = runs::load(&runs_path);
        while runs.len() > RECENT_RUNS {
            runs.pop_front();
        }
        Ok(Arc::new(Self {
            hub,
            path,
            runs_path,
            document: RwLock::new(document),
            lanes: Mutex::new(HashMap::new()),
            runs: Mutex::new(runs),
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

    /// Replaces everything at once, as the page edits it. Empty ids get one.
    ///
    /// # Errors
    /// Why the document is refused, for the household.
    pub fn replace(&self, mut document: Document) -> Result<Document, String> {
        for scene in &mut document.scenes {
            if scene.id.is_empty() {
                scene.id = new_id();
            }
        }
        for automation in &mut document.automations {
            if automation.id.is_empty() {
                automation.id = new_id();
            }
        }
        document.rules.clear();
        check::check(&document)?;
        save_json(&self.path, &document).map_err(|error| format!("{error:#}"))?;
        // Runs of automations that were switched off or deleted stop now.
        let live: Vec<&str> = document
            .automations
            .iter()
            .filter(|automation| automation.enabled)
            .map(|automation| automation.id.as_str())
            .collect();
        for (id, lane) in self
            .lanes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
        {
            if !live.contains(&id.as_str())
                && let Some(cancel) = &lane.cancel
            {
                let _ = cancel.send(true);
            }
        }
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

    /// Runs an automation now, from its « manual » trigger or else its first one, even
    /// when it is switched off.
    ///
    /// # Errors
    /// Fails when the automation is unknown.
    pub fn run_now(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let automation = self
            .document()
            .automation(id)
            .cloned()
            .ok_or_else(|| format!("automatisation « {id} » inconnue"))?;
        let start = automation
            .graph
            .triggers()
            .find(|node| node.step == Step::Manual)
            .or_else(|| automation.graph.triggers().next())
            .map(|node| node.id.clone())
            .ok_or("aucun déclencheur")?;
        self.start(&automation, &start, "Lancée à la main".to_owned(), true);
        Ok(())
    }

    /// Follows `automation` as written, without commanding or waiting: conditions are
    /// really read.
    ///
    /// # Errors
    /// Why the automation cannot be tried.
    pub async fn test(self: &Arc<Self>, mut automation: Automation) -> Result<Run, String> {
        let mut document = self.document();
        if automation.id.is_empty() {
            "essai".clone_into(&mut automation.id);
        }
        document
            .automations
            .retain(|other| other.id != automation.id);
        document.automations.push(automation.clone());
        check::check(&document)?;
        let start = automation
            .graph
            .triggers()
            .next()
            .map(|node| node.id.clone())
            .ok_or("aucun déclencheur")?;
        let id = new_id();
        let mut record = Run {
            id: id.clone(),
            automation: automation.id.clone(),
            name: automation.name.clone(),
            trigger: start.clone(),
            why: "Essai".to_owned(),
            started: now_millis(),
            ended: None,
            status: RunStatus::Running,
            dry: true,
            steps: Vec::new(),
        };
        self.push_run(record.clone());
        let (_keep, cancel) = watch::channel(false);
        let status = run::execute(self, &automation, &start, &id, true, cancel).await;
        record = self.finish_run(&id, status).unwrap_or(record);
        Ok(record)
    }

    /// The latest runs, newest first; those of one automation when `automation` is given.
    #[must_use]
    pub fn runs(&self, automation: Option<&str>, limit: usize) -> Vec<Run> {
        self.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .rev()
            .filter(|run| automation.is_none_or(|id| run.automation == id))
            .take(limit)
            .cloned()
            .collect()
    }

    /// A trigger happened.
    pub(crate) fn fire(self: &Arc<Self>, automation_id: &str, trigger: &str, why: String) {
        let Some(automation) = self.document().automation(automation_id).cloned() else {
            return;
        };
        if !automation.enabled {
            return;
        }
        self.start(&automation, trigger, why, false);
    }

    fn start(self: &Arc<Self>, automation: &Automation, trigger: &str, why: String, manual: bool) {
        let mut lanes = self.lanes.lock().unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(automation.id.clone()).or_default();
        if !manual {
            let now = Instant::now();
            lane.starts.push_back(now);
            while lane
                .starts
                .front()
                .is_some_and(|start| now.duration_since(*start) > Duration::from_secs(60))
            {
                lane.starts.pop_front();
            }
            if lane.starts.len() > RUNAWAY {
                lane.starts.clear();
                drop(lanes);
                self.switch_off(
                    &automation.id,
                    format!("Arrêtée par Hestia : plus de {RUNAWAY} démarrages en une minute."),
                );
                return;
            }
        }
        let busy = lane.turn.try_lock().is_err();
        match automation.mode {
            Mode::Single if busy => {
                info!(
                    automation = automation.name,
                    "already running: trigger ignored"
                );
                return;
            }
            Mode::Queued if lane.waiting >= MAX_QUEUE => {
                warn!(automation = automation.name, "queue full: trigger ignored");
                return;
            }
            Mode::Restart => {
                if let Some(cancel) = &lane.cancel {
                    let _ = cancel.send(true);
                }
            }
            Mode::Single | Mode::Queued => {}
        }
        lane.waiting += 1;
        let turn = Arc::clone(&lane.turn);
        drop(lanes);
        let automations = Arc::clone(self);
        let automation = automation.clone();
        let trigger = trigger.to_owned();
        tokio::spawn(async move {
            let _turn = turn.lock().await;
            let (cancel, cancelled) = watch::channel(false);
            {
                let mut lanes = automations
                    .lanes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                let lane = lanes.entry(automation.id.clone()).or_default();
                lane.waiting = lane.waiting.saturating_sub(1);
                lane.cancel = Some(cancel);
            }
            // A queued run may start after the automation changed: it runs the new version,
            // or not at all.
            let current = automations.document().automation(&automation.id).cloned();
            let Some(current) = current.filter(|current| manual || current.enabled) else {
                return;
            };
            info!(automation = current.name, why, "automation started");
            let id = new_id();
            automations.push_run(Run {
                id: id.clone(),
                automation: current.id.clone(),
                name: current.name.clone(),
                trigger: trigger.clone(),
                why,
                started: now_millis(),
                ended: None,
                status: RunStatus::Running,
                dry: false,
                steps: Vec::new(),
            });
            let status =
                run::execute(&automations, &current, &trigger, &id, false, cancelled).await;
            if let Some(record) = automations.finish_run(&id, status) {
                let recent = automations
                    .runs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                runs::append(&automations.runs_path, &record, &recent);
            }
            if status == RunStatus::Failed {
                warn!(automation = current.name, "automation failed");
            }
        });
    }

    fn switch_off(&self, id: &str, note: String) {
        let mut document = self.document();
        let Some(automation) = document
            .automations
            .iter_mut()
            .find(|automation| automation.id == id)
        else {
            return;
        };
        warn!(
            automation = automation.name,
            note, "automation switched off"
        );
        automation.enabled = false;
        automation.note = Some(note);
        if let Err(problem) = self.replace(document) {
            warn!(problem, "cannot switch the automation off");
        }
    }

    fn push_run(&self, run: Run) {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        runs.push_back(run);
        while runs.len() > RECENT_RUNS {
            runs.pop_front();
        }
    }

    pub(crate) fn log_step(&self, run_id: &str, node: &str, status: StepStatus, detail: String) {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(run) = runs.iter_mut().rev().find(|run| run.id == run_id) {
            run.steps.push(StepLog {
                node: node.to_owned(),
                at: now_millis(),
                status,
                detail,
            });
        }
    }

    fn finish_run(&self, run_id: &str, status: RunStatus) -> Option<Run> {
        let mut runs = self.runs.lock().unwrap_or_else(PoisonError::into_inner);
        let run = runs.iter_mut().rev().find(|run| run.id == run_id)?;
        run.status = status;
        run.ended = Some(now_millis());
        Some(run.clone())
    }

    /// Turns off every lamp that is on; returns how many.
    pub(crate) async fn lights_off(&self) -> Result<usize, String> {
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
        self.apply(&lamps, Origin::Automation).await?;
        Ok(lamps.len())
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
