//! What a document must respect before it is saved. Messages are for the household.

use std::collections::{HashMap, HashSet};

use crate::model::{Automation, Document, Rule, Step, parse_time};

pub const MAX_AUTOMATIONS: usize = 200;
pub const MAX_NODES: usize = 60;
pub const MAX_EDGES: usize = 120;
const MAX_DELAY_S: u32 = 24 * 3600;
const MAX_OFFSET_MIN: i32 = 6 * 60;

/// # Errors
/// The first problem found.
pub fn check(document: &Document) -> Result<(), String> {
    if let Some(location) = &document.location
        && !((-90.0..=90.0).contains(&location.latitude)
            && (-180.0..=180.0).contains(&location.longitude))
    {
        return Err("latitude ou longitude hors limites".to_owned());
    }
    if document.automations.len() > MAX_AUTOMATIONS {
        return Err(format!("{MAX_AUTOMATIONS} automatisations au plus"));
    }
    let mut ids = HashSet::new();
    for scene in &document.scenes {
        check_name(&scene.name)?;
        if !ids.insert(scene.id.as_str()) {
            return Err(format!("identifiant « {} » en double", scene.id));
        }
    }
    for automation in &document.automations {
        check_name(&automation.name)?;
        if !ids.insert(automation.id.as_str()) {
            return Err(format!("identifiant « {} » en double", automation.id));
        }
        check_automation(document, automation)
            .map_err(|problem| format!("« {} » : {problem}", automation.name))?;
    }
    Ok(())
}

fn check_automation(document: &Document, automation: &Automation) -> Result<(), String> {
    let graph = &automation.graph;
    if graph.nodes.len() > MAX_NODES {
        return Err(format!("{MAX_NODES} étapes au plus"));
    }
    if graph.edges.len() > MAX_EDGES {
        return Err(format!("{MAX_EDGES} liens au plus"));
    }
    let mut nodes = HashMap::new();
    for node in &graph.nodes {
        if nodes.insert(node.id.as_str(), node).is_some() {
            return Err(format!("étape « {} » en double", node.id));
        }
        check_step(document, &node.step)?;
    }
    if graph.triggers().next().is_none() {
        return Err("il faut un déclencheur".to_owned());
    }
    let mut seen = HashSet::new();
    for edge in &graph.edges {
        let (Some(from), Some(to)) = (nodes.get(edge.from.as_str()), nodes.get(edge.to.as_str()))
        else {
            return Err("un lien relie une étape qui n'existe pas".to_owned());
        };
        if !from.step.ports().contains(&edge.port.as_str()) {
            return Err(format!("sortie « {} » inconnue", edge.port));
        }
        if to.step.is_trigger() {
            return Err("un déclencheur ne peut pas suivre une autre étape".to_owned());
        }
        if !seen.insert((edge.from.as_str(), edge.port.as_str(), edge.to.as_str())) {
            return Err("lien en double".to_owned());
        }
    }
    if has_cycle(automation) {
        return Err("les liens forment une boucle".to_owned());
    }
    Ok(())
}

fn check_step(document: &Document, step: &Step) -> Result<(), String> {
    match step {
        Step::WhenState { for_s, .. } => delay(*for_s),
        Step::WhenThreshold {
            above,
            below,
            for_s,
            ..
        } => {
            if above.is_none() && below.is_none() {
                return Err(
                    "un seuil doit avoir une valeur « au-dessus » ou « en dessous »".to_owned(),
                );
            }
            delay(*for_s)
        }
        Step::AtTime { at, days } => {
            time(at)?;
            check_days(days)
        }
        Step::AtSun {
            offset_min, days, ..
        } => {
            if offset_min.abs() > MAX_OFFSET_MIN {
                return Err("décalage de plus de 6 heures".to_owned());
            }
            check_days(days)
        }
        Step::Every { minutes } => {
            if (1..=1440).contains(minutes) {
                Ok(())
            } else {
                Err("« toutes les » doit valoir de 1 à 1440 minutes".to_owned())
            }
        }
        Step::If { rules, .. } => {
            if rules.is_empty() {
                return Err("une condition sans règle".to_owned());
            }
            rules.iter().try_for_each(check_rule)
        }
        Step::Wait { seconds } => {
            if *seconds == 0 {
                return Err("une attente de 0 seconde".to_owned());
            }
            delay(*seconds)
        }
        Step::WaitFor { rule, timeout_s } => {
            check_rule(rule)?;
            delay(*timeout_s)
        }
        Step::Scene { scene } => {
            if document.scene(scene).is_none() {
                return Err("une étape lance une scène qui n'existe plus".to_owned());
            }
            Ok(())
        }
        Step::OnStart
        | Step::Manual
        | Step::Presence { .. }
        | Step::Set { .. }
        | Step::Toggle { .. }
        | Step::LightsOff => Ok(()),
    }
}

fn check_rule(rule: &Rule) -> Result<(), String> {
    match rule {
        Rule::Time {
            after,
            before,
            days,
        } => {
            if let Some(after) = after {
                time(after)?;
            }
            if let Some(before) = before {
                time(before)?;
            }
            check_days(days)
        }
        Rule::State { .. } | Rule::Sun { .. } | Rule::Presence { .. } => Ok(()),
    }
}

fn has_cycle(automation: &Automation) -> bool {
    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Visiting,
        Done,
    }
    fn visit<'a>(
        automation: &'a Automation,
        id: &'a str,
        marks: &mut HashMap<&'a str, Mark>,
    ) -> bool {
        match marks.get(id) {
            Some(Mark::Visiting) => return true,
            Some(Mark::Done) => return false,
            None => {}
        }
        marks.insert(id, Mark::Visiting);
        let looped = automation
            .graph
            .edges
            .iter()
            .filter(|edge| edge.from == id)
            .any(|edge| visit(automation, &edge.to, marks));
        marks.insert(id, Mark::Done);
        looped
    }
    let mut marks = HashMap::new();
    automation
        .graph
        .nodes
        .iter()
        .any(|node| visit(automation, &node.id, &mut marks))
}

fn check_name(name: &str) -> Result<(), String> {
    let length = name.trim().chars().count();
    if (1..=60).contains(&length) {
        Ok(())
    } else {
        Err("nom de 1 à 60 caractères".to_owned())
    }
}

fn time(raw: &str) -> Result<(), String> {
    parse_time(raw)
        .map(|_| ())
        .ok_or_else(|| format!("« {raw} » n'est pas une heure (HH:MM)"))
}

fn delay(seconds: u32) -> Result<(), String> {
    if seconds <= MAX_DELAY_S {
        Ok(())
    } else {
        Err("une durée de plus de 24 heures".to_owned())
    }
}

fn check_days(days: &[u8]) -> Result<(), String> {
    if days.iter().all(|day| (1..=7).contains(day)) {
        Ok(())
    } else {
        Err("jours de 1 (lundi) à 7 (dimanche)".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::check;
    use crate::model::Document;

    fn document(nodes: serde_json::Value, edges: serde_json::Value) -> Document {
        let mut graph = serde_json::Map::new();
        graph.insert("nodes".to_owned(), nodes);
        graph.insert("edges".to_owned(), edges);
        let mut document = serde_json::json!({
            "scenes": [{"id": "soir", "name": "Soirée", "actions": []}],
            "automations": [{"id": "a", "name": "Essai"}]
        });
        document["automations"][0]["graph"] = serde_json::Value::Object(graph);
        serde_json::from_value(document).unwrap()
    }

    #[test]
    fn a_sound_graph_passes() {
        let ok = document(
            serde_json::json!([
                {"id": "t", "type": "at_time", "at": "07:30", "days": [1, 2, 3, 4, 5]},
                {"id": "c", "type": "if", "rules": [{"kind": "presence", "is": "someone"}]},
                {"id": "s", "type": "scene", "scene": "soir"},
                {"id": "w", "type": "wait_for", "rule": {"kind": "sun", "is": "night"}, "timeout_s": 600},
                {"id": "o", "type": "lights_off"}
            ]),
            serde_json::json!([
                {"from": "t", "to": "c"},
                {"from": "c", "port": "yes", "to": "s"},
                {"from": "c", "port": "no", "to": "w"},
                {"from": "w", "port": "timeout", "to": "o"}
            ]),
        );
        assert_eq!(check(&ok), Ok(()));
    }

    #[test]
    fn broken_graphs_are_refused() {
        let cases = [
            (
                serde_json::json!([{"id": "s", "type": "lights_off"}]),
                serde_json::json!([]),
                "déclencheur",
            ),
            (
                serde_json::json!([{"id": "t", "type": "manual"}, {"id": "a", "type": "wait", "seconds": 5}, {"id": "b", "type": "wait", "seconds": 5}]),
                serde_json::json!([{"from": "t", "to": "a"}, {"from": "a", "to": "b"}, {"from": "b", "to": "a"}]),
                "boucle",
            ),
            (
                serde_json::json!([{"id": "t", "type": "manual"}, {"id": "u", "type": "on_start"}]),
                serde_json::json!([{"from": "t", "to": "u"}]),
                "déclencheur ne peut pas",
            ),
            (
                serde_json::json!([{"id": "t", "type": "manual"}, {"id": "a", "type": "lights_off"}]),
                serde_json::json!([{"from": "t", "port": "yes", "to": "a"}]),
                "sortie",
            ),
            (
                serde_json::json!([{"id": "t", "type": "manual"}, {"id": "a", "type": "scene", "scene": "nope"}]),
                serde_json::json!([{"from": "t", "to": "a"}]),
                "scène",
            ),
            (
                serde_json::json!([{"id": "t", "type": "at_time", "at": "25:00"}]),
                serde_json::json!([]),
                "heure",
            ),
            (
                serde_json::json!([{"id": "t", "type": "when_threshold", "device": "x:y", "point": "t"}]),
                serde_json::json!([]),
                "seuil",
            ),
        ];
        for (nodes, edges, expected) in cases {
            let problem = check(&document(nodes, edges)).unwrap_err();
            assert!(
                problem.contains(expected),
                "{problem} should mention {expected}"
            );
        }
    }
}
