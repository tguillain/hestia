//! One run of an automation: from its trigger, step by step along the wires.

use std::sync::Arc;
use std::time::Duration;

use std::collections::BTreeMap;

use hestia_core::{Command, DeviceId, Origin, PointSpec, Semantic, Value};
use jiff::Zoned;
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::Automations;
use crate::model::{Automation, Daylight, Occupancy, Op, Rule, Step, parse_time};
use crate::sun::sunrise_sunset;

/// A run longer than this is a runaway graph, whatever it says.
pub const MAX_STEPS: usize = 100;
const DETAIL_CHARS: usize = 300;
/// How often `wait_for` looks at its rule.
const WAIT_FOR_CHECK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub automation: String,
    pub name: String,
    /// The trigger node that started it.
    pub trigger: String,
    /// What happened, e.g. « Il est 07:30 ».
    pub why: String,
    /// Unix milliseconds.
    pub started: i64,
    #[serde(default)]
    pub ended: Option<i64>,
    pub status: RunStatus,
    /// A test: nothing was commanded, nothing waited.
    #[serde(default)]
    pub dry: bool,
    pub steps: Vec<StepLog>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepLog {
    pub node: String,
    pub at: i64,
    pub status: StepStatus,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Done,
    Failed,
    Simulated,
    Yes,
    No,
    Ok,
    Timeout,
}

/// What the run left: the end status.
pub(crate) async fn execute(
    automations: &Arc<Automations>,
    automation: &Automation,
    start: &str,
    run_id: &str,
    dry: bool,
    mut cancel: watch::Receiver<bool>,
) -> RunStatus {
    let graph = &automation.graph;
    let mut stack: Vec<&str> = vec![start];
    let mut steps = 0;
    let mut failed = false;
    while let Some(id) = stack.pop() {
        steps += 1;
        if steps > MAX_STEPS {
            automations.log_step(
                run_id,
                id,
                StepStatus::Failed,
                format!("plus de {MAX_STEPS} étapes : arrêt"),
            );
            return RunStatus::Failed;
        }
        let Some(node) = graph.node(id) else {
            continue;
        };
        let outcome = tokio::select! {
            outcome = step(automations, &node.step, dry) => outcome,
            _ = cancel.wait_for(|cancelled| *cancelled) => return RunStatus::Cancelled,
        };
        let (status, detail, port) = outcome;
        failed |= status == StepStatus::Failed;
        automations.log_step(run_id, id, status, detail);
        if let Some(port) = port {
            let next: Vec<&str> = graph.next(id, port).map(|node| node.id.as_str()).collect();
            stack.extend(next.into_iter().rev());
        }
    }
    if failed {
        RunStatus::Failed
    } else {
        RunStatus::Done
    }
}

type Outcome = (StepStatus, String, Option<&'static str>);

async fn step(automations: &Arc<Automations>, step: &Step, dry: bool) -> Outcome {
    let hub = &automations.hub;
    match step {
        _ if step.is_trigger() => (StepStatus::Done, String::new(), Some("out")),
        Step::If { rules, all } => condition(automations, rules, *all),
        Step::Wait { seconds } => {
            if dry {
                return (
                    StepStatus::Simulated,
                    format!("attendrait {}", duration(*seconds)),
                    Some("out"),
                );
            }
            tokio::time::sleep(Duration::from_secs(u64::from(*seconds))).await;
            (StepStatus::Done, String::new(), Some("out"))
        }
        Step::WaitFor { rule, timeout_s } => wait_for(automations, rule, *timeout_s, dry).await,
        Step::Device {
            device,
            values,
            toggle,
        } => settle(automations, device, values, toggle.as_deref(), dry).await,
        Step::Set {
            device,
            point,
            value,
        } => write(automations, device, point, value.clone(), dry).await,
        Step::Toggle { device, point } => {
            let Some(Value::Bool(current)) = hub.value(device, point) else {
                return (
                    StepStatus::Failed,
                    format!("{} : état inconnu", label(automations, device, point)),
                    None,
                );
            };
            write(automations, device, point, Value::Bool(!current), dry).await
        }
        Step::Scene { scene } => {
            let name = automations
                .document()
                .scene(scene)
                .map_or_else(|| scene.clone(), |scene| scene.name.clone());
            if dry {
                return (
                    StepStatus::Simulated,
                    format!("scène « {name} »"),
                    Some("out"),
                );
            }
            match automations.run_scene(scene, Origin::Automation).await {
                Ok(()) => (StepStatus::Done, format!("scène « {name} »"), Some("out")),
                Err(problem) => (StepStatus::Failed, clip(&problem), None),
            }
        }
        Step::LightsOff => {
            if dry {
                return (
                    StepStatus::Simulated,
                    "toutes les lumières".to_owned(),
                    Some("out"),
                );
            }
            match automations.lights_off().await {
                Ok(count) => (
                    StepStatus::Done,
                    format!("{count} lumière(s) éteinte(s)"),
                    Some("out"),
                ),
                Err(problem) => (StepStatus::Failed, clip(&problem), None),
            }
        }
        _ => (StepStatus::Done, String::new(), Some("out")),
    }
}

/// Leaves by `yes` when all (or any) of the rules hold.
fn condition(automations: &Automations, rules: &[Rule], all: bool) -> Outcome {
    let mut verdicts = Vec::new();
    for rule in rules {
        match evaluate(automations, rule) {
            Ok(verdict) => verdicts.push(verdict),
            Err(problem) => return (StepStatus::Failed, problem, None),
        }
    }
    let holds = if all {
        verdicts.iter().all(|verdict| *verdict)
    } else {
        verdicts.iter().any(|verdict| *verdict)
    };
    if holds {
        (StepStatus::Yes, String::new(), Some("yes"))
    } else {
        (StepStatus::No, String::new(), Some("no"))
    }
}

/// Leaves by `ok` as soon as the rule holds, by `timeout` after `timeout_s`.
async fn wait_for(automations: &Automations, rule: &Rule, timeout_s: u32, dry: bool) -> Outcome {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(u64::from(timeout_s));
    loop {
        match evaluate(automations, rule) {
            Ok(true) => return (StepStatus::Ok, String::new(), Some("ok")),
            Ok(false) if dry => {
                return (
                    StepStatus::Simulated,
                    "attendrait la condition".to_owned(),
                    Some("ok"),
                );
            }
            Ok(false) => {}
            Err(problem) => return (StepStatus::Failed, problem, None),
        }
        if tokio::time::Instant::now() >= deadline {
            return (StepStatus::Timeout, String::new(), Some("timeout"));
        }
        tokio::time::sleep(WAIT_FOR_CHECK).await;
    }
}

/// Brings a device to the state a « Régler un appareil » step describes; a refused command
/// stops the branch.
async fn settle(
    automations: &Automations,
    device: &DeviceId,
    values: &BTreeMap<String, Value>,
    toggle: Option<&str>,
    dry: bool,
) -> Outcome {
    let mut writes: Vec<(String, Value)> = values
        .iter()
        .map(|(point, value)| (point.clone(), value.clone()))
        .collect();
    if let Some(point) = toggle {
        let Some(Value::Bool(current)) = automations.hub.value(device, point) else {
            return (
                StepStatus::Failed,
                format!("{} : état inconnu", label(automations, device, point)),
                None,
            );
        };
        writes.push((point.to_owned(), Value::Bool(!current)));
    }
    let on_off = |point: &str| {
        lookup(automations, device, point)
            .1
            .is_some_and(|spec| spec.semantic == Semantic::OnOff)
    };
    // Switched on before brightness and colour, switched off after them.
    writes.sort_by_key(|(point, value)| match value {
        Value::Bool(true) if on_off(point) => 0,
        Value::Bool(false) if on_off(point) => 2,
        _ => 1,
    });
    let detail = describe_all(automations, device, &writes);
    if dry {
        return (StepStatus::Simulated, detail, Some("out"));
    }
    for (point, value) in writes {
        let command = Command {
            device: device.clone(),
            point,
            value,
        };
        if let Err(error) = automations.hub.command(command, Origin::Automation).await {
            return (
                StepStatus::Failed,
                clip(&format!("{detail} : {error}")),
                None,
            );
        }
    }
    (StepStatus::Done, detail, Some("out"))
}

/// Sets a point; a refused command stops the branch.
async fn write(
    automations: &Automations,
    device: &hestia_core::DeviceId,
    point: &str,
    value: Value,
    dry: bool,
) -> Outcome {
    let detail = describe(automations, device, point, &value);
    if dry {
        return (StepStatus::Simulated, detail, Some("out"));
    }
    let command = Command {
        device: device.clone(),
        point: point.to_owned(),
        value,
    };
    match automations.hub.command(command, Origin::Automation).await {
        Ok(()) => (StepStatus::Done, detail, Some("out")),
        Err(error) => (
            StepStatus::Failed,
            clip(&format!("{detail} : {error}")),
            None,
        ),
    }
}

/// # Errors
/// When the rule cannot be judged, e.g. day or night without the house's location.
pub(crate) fn evaluate(automations: &Automations, rule: &Rule) -> Result<bool, String> {
    match rule {
        Rule::State {
            device,
            point,
            op,
            value,
        } => Ok(automations
            .hub
            .value(device, point)
            .is_some_and(|current| compare(&current, *op, value))),
        Rule::Time {
            after,
            before,
            days,
        } => {
            let now = Zoned::now();
            Ok(within(&now, after.as_deref(), before.as_deref(), days))
        }
        Rule::Sun { is } => {
            let location = automations
                .document()
                .location
                .ok_or("position de la maison inconnue (Automatisations → Position)")?;
            let now = Zoned::now();
            let noon = now
                .date()
                .at(12, 0, 0, 0)
                .to_zoned(jiff::tz::TimeZone::UTC)
                .map_err(|error| error.to_string())?
                .timestamp()
                .as_second();
            let (rise, set) = sunrise_sunset(noon, location.latitude, location.longitude)
                .ok_or("le soleil ne se lève ni ne se couche aujourd'hui ici")?;
            let day = (rise..set).contains(&now.timestamp().as_second());
            Ok(day == (*is == Daylight::Day))
        }
        Rule::Presence { is } => {
            let someone = automations.hub.devices().iter().any(|device| {
                device.points.iter().any(|point| {
                    point.spec.semantic == Semantic::Presence
                        && point.value == Some(Value::Bool(true))
                })
            });
            Ok(someone == (*is == Occupancy::Someone))
        }
    }
}

fn compare(current: &Value, op: Op, expected: &Value) -> bool {
    match op {
        Op::Eq => current == expected,
        Op::Ne => current != expected,
        Op::Gt | Op::Ge | Op::Lt | Op::Le => {
            let (Some(current), Some(expected)) = (current.as_number(), expected.as_number())
            else {
                return false;
            };
            match op {
                Op::Gt => current > expected,
                Op::Ge => current >= expected,
                Op::Lt => current < expected,
                _ => current <= expected,
            }
        }
    }
}

/// Time windows may wrap midnight: 22:00 → 06:00.
pub(crate) fn within(now: &Zoned, after: Option<&str>, before: Option<&str>, days: &[u8]) -> bool {
    let weekday = u8::try_from(now.weekday().to_monday_one_offset()).unwrap_or(0);
    if !days.is_empty() && !days.contains(&weekday) {
        return false;
    }
    let minute = u32::try_from(i32::from(now.hour()) * 60 + i32::from(now.minute())).unwrap_or(0);
    match (after.and_then(parse_time), before.and_then(parse_time)) {
        (Some(after), Some(before)) if after <= before => (after..before).contains(&minute),
        (Some(after), Some(before)) => minute >= after || minute < before,
        (Some(after), None) => minute >= after,
        (None, Some(before)) => minute < before,
        (None, None) => true,
    }
}

fn label(automations: &Automations, device: &DeviceId, point: &str) -> String {
    let (name, spec) = lookup(automations, device, point);
    format!(
        "{name} · {}",
        spec.as_ref().map_or(point, |spec| spec.label.as_str())
    )
}

/// « Chambre : allumé », « Chambre : Luminosité 40 % ».
pub(crate) fn describe(
    automations: &Automations,
    device: &DeviceId,
    point: &str,
    value: &Value,
) -> String {
    describe_all(automations, device, &[(point.to_owned(), value.clone())])
}

/// « Chambre : allumé, Luminosité 40 %, Couleur #ffb450 ».
fn describe_all(
    automations: &Automations,
    device: &DeviceId,
    writes: &[(String, Value)],
) -> String {
    let name = lookup(automations, device, "").0;
    let parts: Vec<String> = writes
        .iter()
        .map(|(point, value)| setting(lookup(automations, device, point).1.as_ref(), point, value))
        .collect();
    format!("{name} : {}", parts.join(", "))
}

/// The device's name and the point's description, when known.
fn lookup(
    automations: &Automations,
    device: &DeviceId,
    point: &str,
) -> (String, Option<PointSpec>) {
    match automations.hub.device(device) {
        Some(snapshot) => {
            let spec = snapshot
                .points
                .iter()
                .find(|candidate| candidate.spec.key == point)
                .map(|candidate| candidate.spec.clone());
            (snapshot.name.clone(), spec)
        }
        None => (device.to_string(), None),
    }
}

fn setting(spec: Option<&PointSpec>, point: &str, value: &Value) -> String {
    if let (Value::Bool(on), Some(spec)) = (value, spec)
        && let Some(word) = state_word(spec.semantic, *on)
    {
        return word.to_owned();
    }
    let label = spec.map_or(point, |spec| spec.label.as_str());
    match spec.and_then(|spec| spec.unit) {
        Some(unit) => format!("{label} {} {}", show(value), unit.symbol()),
        None => format!("{label} {}", show(value)),
    }
}

fn state_word(semantic: Semantic, on: bool) -> Option<&'static str> {
    let (yes, no) = match semantic {
        Semantic::OnOff => ("allumé", "éteint"),
        Semantic::Connectivity => ("en ligne", "hors ligne"),
        Semantic::Presence => ("à la maison", "absent"),
        _ => return None,
    };
    Some(if on { yes } else { no })
}

/// A value as the household reads it.
#[must_use]
pub fn show(value: &Value) -> String {
    match value {
        Value::Bool(true) => "Oui".to_owned(),
        Value::Bool(false) => "Non".to_owned(),
        Value::Number(number) => {
            if number.fract() == 0.0 {
                format!("{number:.0}")
            } else {
                format!("{number:.1}")
            }
        }
        Value::Color(rgb) => format!("#{:02x}{:02x}{:02x}", rgb.r, rgb.g, rgb.b),
        Value::Text(text) => text.clone(),
    }
}

fn duration(seconds: u32) -> String {
    match seconds {
        s if s % 3600 == 0 => format!("{} h", s / 3600),
        s if s % 60 == 0 => format!("{} min", s / 60),
        s => format!("{s} s"),
    }
}

fn clip(text: &str) -> String {
    text.chars().take(DETAIL_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::{compare, show, within};
    use crate::model::Op;
    use hestia_core::Value;
    use jiff::Zoned;

    #[test]
    fn comparisons() {
        assert!(compare(&Value::Number(21.5), Op::Gt, &Value::Number(20.0)));
        assert!(!compare(&Value::Number(21.5), Op::Lt, &Value::Number(20.0)));
        assert!(compare(&Value::Bool(true), Op::Eq, &Value::Bool(true)));
        assert!(compare(
            &Value::Text("white".into()),
            Op::Ne,
            &Value::Text("color".into())
        ));
        assert!(
            !compare(&Value::Text("a".into()), Op::Gt, &Value::Number(1.0)),
            "text is not a number"
        );
        assert_eq!(show(&Value::Number(40.0)), "40");
        assert_eq!(show(&Value::Bool(false)), "Non");
    }

    #[test]
    fn time_windows_wrap_midnight() {
        let late: Zoned = "2026-10-20T23:30[Europe/Paris]".parse().unwrap();
        let noon: Zoned = "2026-10-20T12:00[Europe/Paris]".parse().unwrap();
        assert!(within(&late, Some("22:00"), Some("06:00"), &[]));
        assert!(!within(&noon, Some("22:00"), Some("06:00"), &[]));
        assert!(
            within(&noon, Some("08:00"), Some("18:00"), &[2]),
            "a Tuesday"
        );
        assert!(!within(&noon, Some("08:00"), Some("18:00"), &[6, 7]));
        assert!(within(&noon, None, Some("13:00"), &[]));
    }
}
