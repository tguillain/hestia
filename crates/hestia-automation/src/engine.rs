//! Watches the clock and the hub, and fires the triggers that happened.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use hestia_core::{DeviceId, Semantic, Value};
use hestia_hub::{DeviceSnapshot, Event, Hub};
use jiff::Zoned;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;
use tokio::time::{self, Instant, MissedTickBehavior};

use crate::Automations;
use crate::model::{Location, PresenceEvent, Step, SunEvent, parse_time};
use crate::run::show;
use crate::sun::sunrise_sunset;

const CLOCK_CHECK: Duration = Duration::from_secs(1);
/// `on_start` waits for the drivers to report their devices.
pub const START_DELAY: Duration = Duration::from_secs(15);

type PointKey = (DeviceId, String);

pub(crate) async fn run(automations: Arc<Automations>) {
    let hub = Arc::clone(&automations.hub);
    let mut events = hub.subscribe();
    let mut last = snapshot(&hub.devices());
    let mut home = present_people(&hub.devices());
    let mut holds: HashMap<(String, String), JoinHandle<()>> = HashMap::new();
    let mut clock = time::interval(CLOCK_CHECK);
    clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let started = Instant::now();
    let mut started_fired = false;
    // The current minute counts as done: a restart at 07:30 must not run 07:30 twice.
    let mut minute = minute_key(&Zoned::now());
    loop {
        tokio::select! {
            _ = clock.tick() => {
                if !started_fired && started.elapsed() >= START_DELAY {
                    started_fired = true;
                    fire_matching(&automations, |step| matches!(step, Step::OnStart).then(|| "Démarrage de Hestia".to_owned()));
                }
                let now = Zoned::now();
                let key = minute_key(&now);
                if key != minute {
                    minute = key;
                    let location = automations.document().location;
                    fire_matching(&automations, |step| clock_due(step, &now, location));
                }
            }
            event = events.recv() => match event {
                Ok(Event::State { device, point, value, .. }) => {
                    let key = (device.clone(), point.clone());
                    let previous = last.insert(key.clone(), value.clone());
                    on_change(&automations, &mut holds, &key, previous.as_ref(), &value);
                    if is_presence(&hub, &device, &point) {
                        let before = !home.is_empty();
                        if value == Value::Bool(true) {
                            home.insert(device.clone());
                        } else {
                            home.remove(&device);
                        }
                        let event = match (before, !home.is_empty()) {
                            (false, true) => Some(PresenceEvent::FirstArrival),
                            (true, false) => Some(PresenceEvent::LastDeparture),
                            _ => None,
                        };
                        if let Some(event) = event {
                            let why = match event {
                                PresenceEvent::FirstArrival => "Quelqu'un arrive",
                                PresenceEvent::LastDeparture => "Le dernier est parti",
                            };
                            fire_matching(&automations, |step| {
                                (step == &Step::Presence { event }).then(|| why.to_owned())
                            });
                        }
                    }
                }
                Ok(Event::DeviceUpdated { .. } | Event::DeviceRemoved { .. }) | Err(RecvError::Lagged(_)) => {
                    let devices = hub.devices();
                    for (key, value) in snapshot(&devices) {
                        last.entry(key).or_insert(value);
                    }
                    home = present_people(&devices);
                }
                Ok(_) => {}
                Err(RecvError::Closed) => return,
            },
        }
    }
}

/// Fires every trigger node of every enabled automation for which `due` gives a reason.
fn fire_matching(automations: &Arc<Automations>, due: impl Fn(&Step) -> Option<String>) {
    let document = automations.document();
    for automation in document
        .automations
        .iter()
        .filter(|automation| automation.enabled)
    {
        for node in automation.graph.triggers() {
            if let Some(why) = due(&node.step) {
                automations.fire(&automation.id, &node.id, why);
            }
        }
    }
}

/// A point changed from `previous` to `value`: state and threshold triggers, with holds.
fn on_change(
    automations: &Arc<Automations>,
    holds: &mut HashMap<(String, String), JoinHandle<()>>,
    key: &PointKey,
    previous: Option<&Value>,
    value: &Value,
) {
    // The first value seen for a point is not a change.
    let Some(previous) = previous else {
        return;
    };
    if previous == value {
        return;
    }
    let document = automations.document();
    for automation in document
        .automations
        .iter()
        .filter(|automation| automation.enabled)
    {
        for node in automation.graph.triggers() {
            let (matched, still, for_s) = match &node.step {
                Step::WhenState {
                    device,
                    point,
                    to,
                    from,
                    for_s,
                } if device == &key.0 && point == &key.1 => {
                    let matched = to.as_ref().is_none_or(|to| to == value)
                        && from.as_ref().is_none_or(|from| from == previous);
                    let to = to.clone();
                    let still: Box<dyn Fn(&Value) -> bool + Send> =
                        Box::new(move |current| to.as_ref().is_none_or(|to| to == current));
                    (matched, still, *for_s)
                }
                Step::WhenThreshold {
                    device,
                    point,
                    above,
                    below,
                    for_s,
                } if device == &key.0 && point == &key.1 => {
                    let (Some(before), Some(now)) = (previous.as_number(), value.as_number())
                    else {
                        continue;
                    };
                    let crossed = above.is_some_and(|limit| before <= limit && now > limit)
                        || below.is_some_and(|limit| before >= limit && now < limit);
                    let (above, below) = (*above, *below);
                    let still: Box<dyn Fn(&Value) -> bool + Send> = Box::new(move |current| {
                        current.as_number().is_some_and(|current| {
                            above.is_some_and(|limit| current > limit)
                                || below.is_some_and(|limit| current < limit)
                        })
                    });
                    (crossed, still, *for_s)
                }
                _ => continue,
            };
            let hold_key = (automation.id.clone(), node.id.clone());
            if !matched {
                // The value moved on: a hold waiting for it to last is over.
                if holds.get(&hold_key).is_some_and(|hold| !hold.is_finished())
                    && !still(value)
                    && let Some(hold) = holds.remove(&hold_key)
                {
                    hold.abort();
                }
                continue;
            }
            let why = format!(
                "{} : {}",
                point_label(automations, &key.0, &key.1),
                show(value)
            );
            if for_s == 0 {
                automations.fire(&automation.id, &node.id, why);
                continue;
            }
            if let Some(hold) = holds.remove(&hold_key) {
                hold.abort();
            }
            let automations = Arc::clone(automations);
            let (automation_id, node_id) = hold_key.clone();
            let (device, point) = key.clone();
            holds.insert(
                hold_key,
                tokio::spawn(async move {
                    time::sleep(Duration::from_secs(u64::from(for_s))).await;
                    if automations
                        .hub
                        .value(&device, &point)
                        .is_some_and(|current| still(&current))
                    {
                        automations.fire(
                            &automation_id,
                            &node_id,
                            format!("{why} depuis {for_s} s"),
                        );
                    }
                }),
            );
        }
    }
}

/// Why a clock trigger is due this minute, if it is.
pub(crate) fn clock_due(step: &Step, now: &Zoned, location: Option<Location>) -> Option<String> {
    let minute = minute_of_day(now);
    match step {
        Step::Every { minutes } if *minutes > 0 && minute.is_multiple_of(*minutes) => {
            Some(format!("Toutes les {minutes} min"))
        }
        Step::AtTime { .. } | Step::AtSun { .. } => (due_minute(step, now, location)? == minute)
            .then(|| match step {
                Step::AtSun {
                    event: SunEvent::Sunrise,
                    ..
                } => "Lever du soleil".to_owned(),
                Step::AtSun { .. } => "Coucher du soleil".to_owned(),
                _ => format!("Il est {:02}:{:02}", minute / 60, minute % 60),
            }),
        _ => None,
    }
}

/// Minute of the day a time or sun trigger is due today, if it is due today at all.
pub(crate) fn due_minute(step: &Step, now: &Zoned, location: Option<Location>) -> Option<u32> {
    let weekday = u8::try_from(now.weekday().to_monday_one_offset()).ok()?;
    let today = |days: &[u8]| days.is_empty() || days.contains(&weekday);
    match step {
        Step::AtTime { at, days } if today(days) => parse_time(at),
        Step::AtSun {
            event,
            offset_min,
            days,
        } if today(days) => {
            let location = location?;
            let noon = now
                .date()
                .at(12, 0, 0, 0)
                .to_zoned(TimeZone::UTC)
                .ok()?
                .timestamp()
                .as_second();
            let (rise, set) = sunrise_sunset(noon, location.latitude, location.longitude)?;
            let at = match event {
                SunEvent::Sunrise => rise,
                SunEvent::Sunset => set,
            } + i64::from(*offset_min) * 60;
            let local = jiff::Timestamp::from_second(at)
                .ok()?
                .to_zoned(now.time_zone().clone());
            (local.date() == now.date()).then(|| minute_of_day(&local))
        }
        _ => None,
    }
}

/// Sunrise and sunset today, `HH:MM` local, for the page.
pub(crate) fn sun_today(location: Location, now: &Zoned) -> Option<(String, String)> {
    let time = |event| {
        let step = Step::AtSun {
            event,
            offset_min: 0,
            days: Vec::new(),
        };
        due_minute(&step, now, Some(location))
            .map(|minute| format!("{:02}:{:02}", minute / 60, minute % 60))
    };
    Some((time(SunEvent::Sunrise)?, time(SunEvent::Sunset)?))
}

fn minute_of_day(time: &Zoned) -> u32 {
    u32::try_from(i32::from(time.hour()) * 60 + i32::from(time.minute())).unwrap_or(0)
}

fn minute_key(time: &Zoned) -> (Date, u32) {
    (time.date(), minute_of_day(time))
}

fn snapshot(devices: &[DeviceSnapshot]) -> HashMap<PointKey, Value> {
    devices
        .iter()
        .flat_map(|device| {
            device.points.iter().filter_map(|point| {
                point
                    .value
                    .clone()
                    .map(|value| ((device.id.clone(), point.spec.key.clone()), value))
            })
        })
        .collect()
}

fn point_label(automations: &Automations, device: &DeviceId, point: &str) -> String {
    automations.hub.device(device).map_or_else(
        || format!("{device} · {point}"),
        |snapshot| {
            let label = snapshot
                .points
                .iter()
                .find(|candidate| candidate.spec.key == point)
                .map_or(point, |candidate| candidate.spec.label.as_str());
            format!("{} · {label}", snapshot.name)
        },
    )
}

fn is_presence(hub: &Hub, device: &DeviceId, point: &str) -> bool {
    hub.device(device).is_some_and(|snapshot| {
        snapshot.points.iter().any(|candidate| {
            candidate.spec.key == point && candidate.spec.semantic == Semantic::Presence
        })
    })
}

fn present_people(devices: &[DeviceSnapshot]) -> HashSet<DeviceId> {
    devices
        .iter()
        .filter(|device| {
            device.points.iter().any(|point| {
                point.spec.semantic == Semantic::Presence && point.value == Some(Value::Bool(true))
            })
        })
        .map(|device| device.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{clock_due, due_minute, sun_today};
    use crate::model::{Location, Step, SunEvent};
    use jiff::Zoned;

    const PARIS: Location = Location {
        latitude: 48.8566,
        longitude: 2.3522,
    };

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn clock_triggers_follow_the_week() {
        // 2026-10-20 is a Tuesday.
        let tuesday = at("2026-10-20T07:30[Europe/Paris]");
        let weekdays = Step::AtTime {
            at: "07:30".to_owned(),
            days: vec![1, 2, 3, 4, 5],
        };
        assert_eq!(
            clock_due(&weekdays, &tuesday, None).as_deref(),
            Some("Il est 07:30")
        );
        let weekend = Step::AtTime {
            at: "07:30".to_owned(),
            days: vec![6, 7],
        };
        assert_eq!(clock_due(&weekend, &tuesday, None), None);
        let quarter = Step::Every { minutes: 15 };
        assert!(clock_due(&quarter, &tuesday, None).is_some());
        assert!(clock_due(&quarter, &at("2026-10-20T07:31[Europe/Paris]"), None).is_none());
    }

    #[test]
    fn sunset_in_local_time() {
        let midsummer = at("2026-06-21T08:00[Europe/Paris]");
        let sunset = Step::AtSun {
            event: SunEvent::Sunset,
            offset_min: -30,
            days: Vec::new(),
        };
        // Sunset is about 21:58 in Paris; thirty minutes earlier is about 21:28.
        let minute = due_minute(&sunset, &midsummer, Some(PARIS)).unwrap();
        assert!((i64::from(minute) - (21 * 60 + 28)).abs() <= 3, "{minute}");
        assert_eq!(
            due_minute(&sunset, &midsummer, None),
            None,
            "no location, no sun"
        );
        let (rise, set) = sun_today(PARIS, &midsummer).unwrap();
        assert!(
            rise.starts_with("05:4") && set.starts_with("21:5"),
            "{rise} {set}"
        );
    }
}
