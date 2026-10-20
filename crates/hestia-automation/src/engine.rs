//! Watches the clock and the hub, and runs the rules whose trigger happened.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use hestia_core::{DeviceId, Semantic, Value};
use hestia_hub::{DeviceSnapshot, Event, Hub};
use jiff::Zoned;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::{self, Instant, MissedTickBehavior};
use tracing::{info, warn};

use crate::Automations;
use crate::model::{Location, PresenceEvent, Rule, SunEvent, Trigger, parse_time};
use crate::sun::sunrise_sunset;

/// Several checks a minute, so a rule due at 07:30 runs during 07:30.
const CLOCK_CHECK: Duration = Duration::from_secs(15);
/// A rule fired by a state change waits this long before it may fire again: two rules
/// switching each other must not loop.
const STATE_COOLDOWN: Duration = Duration::from_secs(10);

pub(crate) async fn run(automations: Arc<Automations>) {
    let hub = Arc::clone(&automations.hub);
    let mut events = hub.subscribe();
    let mut home = present_people(&hub.devices());
    let mut clock = time::interval(CLOCK_CHECK);
    clock.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut fired_on: HashMap<String, (Date, u32)> = HashMap::new();
    let mut cooled_until: HashMap<String, Instant> = HashMap::new();
    loop {
        tokio::select! {
            _ = clock.tick() => {
                let now = Zoned::now();
                let document = automations.document();
                for rule in document.rules.iter().filter(|rule| rule.enabled) {
                    let Some(minute) = due_minute(rule, &now, document.location) else {
                        continue;
                    };
                    let today = (now.date(), minute);
                    if minute_of_day(&now) == minute && fired_on.get(&rule.id) != Some(&today) {
                        fired_on.insert(rule.id.clone(), today);
                        fire(&automations, rule);
                    }
                }
            }
            event = events.recv() => match event {
                Ok(Event::State { device, point, value, .. }) => {
                    let document = automations.document();
                    let mut presence = None;
                    if is_presence(&hub, &device, &point) {
                        let before = !home.is_empty();
                        if value == Value::Bool(true) {
                            home.insert(device.clone());
                        } else {
                            home.remove(&device);
                        }
                        presence = match (before, !home.is_empty()) {
                            (false, true) => Some(PresenceEvent::FirstArrival),
                            (true, false) => Some(PresenceEvent::LastDeparture),
                            _ => None,
                        };
                    }
                    for rule in document.rules.iter().filter(|rule| rule.enabled) {
                        let matched = match &rule.trigger {
                            Trigger::State { device: wanted, point: key, value: expected } => {
                                wanted == &device && key == &point && expected == &value
                            }
                            Trigger::Presence { event } => presence == Some(*event),
                            Trigger::Time { .. } | Trigger::Sun { .. } => false,
                        };
                        let cooled = cooled_until
                            .get(&rule.id)
                            .is_none_or(|until| Instant::now() >= *until);
                        if matched && cooled {
                            cooled_until.insert(rule.id.clone(), Instant::now() + STATE_COOLDOWN);
                            fire(&automations, rule);
                        }
                    }
                }
                Ok(Event::DeviceUpdated { .. } | Event::DeviceRemoved { .. }) | Err(RecvError::Lagged(_)) => {
                    home = present_people(&hub.devices());
                }
                Ok(_) => {}
                Err(RecvError::Closed) => return,
            },
        }
    }
}

fn fire(automations: &Arc<Automations>, rule: &Rule) {
    info!(rule = rule.name, "automation triggered");
    let automations = Arc::clone(automations);
    let rule = rule.clone();
    tokio::spawn(async move {
        if let Err(error) = automations.perform(&rule.action).await {
            warn!(rule = rule.name, %error, "automation failed");
        }
    });
}

/// Minute of the day a clock rule is due today, if it is due today at all.
pub(crate) fn due_minute(rule: &Rule, now: &Zoned, location: Option<Location>) -> Option<u32> {
    let weekday = u8::try_from(now.weekday().to_monday_one_offset()).ok()?;
    let today = |days: &[u8]| days.is_empty() || days.contains(&weekday);
    match &rule.trigger {
        Trigger::Time { at, days } if today(days) => parse_time(at),
        Trigger::Sun {
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

/// Sunrise and sunset today, `HH:MM` local, for the Settings page.
pub(crate) fn sun_today(location: Location, now: &Zoned) -> Option<(String, String)> {
    let time = |event| {
        let rule = Rule {
            id: String::new(),
            name: String::new(),
            enabled: true,
            trigger: Trigger::Sun {
                event,
                offset_min: 0,
                days: Vec::new(),
            },
            action: crate::model::Action::Scene {
                scene: String::new(),
            },
        };
        due_minute(&rule, now, Some(location))
            .map(|minute| format!("{:02}:{:02}", minute / 60, minute % 60))
    };
    Some((time(SunEvent::Sunrise)?, time(SunEvent::Sunset)?))
}

fn minute_of_day(time: &Zoned) -> u32 {
    u32::try_from(i32::from(time.hour()) * 60 + i32::from(time.minute())).unwrap_or(0)
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
    use super::{due_minute, sun_today};
    use crate::model::{Action, Location, Rule, SunEvent, Trigger};
    use jiff::Zoned;

    const PARIS: Location = Location {
        latitude: 48.8566,
        longitude: 2.3522,
    };

    fn rule(trigger: Trigger) -> Rule {
        Rule {
            id: "r".to_owned(),
            name: "r".to_owned(),
            enabled: true,
            trigger,
            action: Action::Scene {
                scene: "s".to_owned(),
            },
        }
    }

    fn at(text: &str) -> Zoned {
        text.parse().unwrap()
    }

    #[test]
    fn clock_rules_follow_the_week() {
        // 2026-10-20 is a Tuesday.
        let tuesday = at("2026-10-20T06:00[Europe/Paris]");
        let weekdays = rule(Trigger::Time {
            at: "07:30".to_owned(),
            days: vec![1, 2, 3, 4, 5],
        });
        assert_eq!(due_minute(&weekdays, &tuesday, None), Some(450));
        let weekend = rule(Trigger::Time {
            at: "09:00".to_owned(),
            days: vec![6, 7],
        });
        assert_eq!(due_minute(&weekend, &tuesday, None), None);
    }

    #[test]
    fn sunset_in_local_time() {
        let midsummer = at("2026-06-21T08:00[Europe/Paris]");
        let sunset = rule(Trigger::Sun {
            event: SunEvent::Sunset,
            offset_min: -30,
            days: Vec::new(),
        });
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
