//! Notifications through ntfy (ntfy.sh, or the household's own server): the « Prévenir »
//! steps of automations and the « Envoyer un essai » button of Réglages. Sent with the
//! system `curl`, as GitHub releases are, so that the program itself carries no TLS.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};
use std::time::Duration;

use hestia_automation::{Notifier, Priority};
use hestia_hub::BoxFuture;
use hestia_hub::storage::{load_json, save_json};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::AsyncWriteExt;
use tracing::info;

const FILE: &str = "ntfy.json";
const SEND_LIMIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NtfySettings {
    /// `https://ntfy.example.fr`, or `http://127.0.0.1:2586` on the same machine.
    pub server: String,
    pub topic: String,
    /// Access token of a ntfy user allowed to publish on the topic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

impl NtfySettings {
    /// Trims what the page sent and refuses what curl or ntfy would not take.
    fn cleaned(self) -> Result<Self, String> {
        let server = self.server.trim().trim_end_matches('/').to_owned();
        let scheme = server
            .strip_prefix("https://")
            .or_else(|| server.strip_prefix("http://"));
        if scheme.is_none_or(str::is_empty)
            || server.len() > 200
            || !server
                .chars()
                .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
        {
            return Err("adresse du serveur invalide (https://… ou http://…)".to_owned());
        }
        let topic = self.topic.trim().to_owned();
        if !(1..=64).contains(&topic.len())
            || !topic
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err("sujet de 1 à 64 lettres, chiffres, - ou _".to_owned());
        }
        let token = self
            .token
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());
        if token.as_ref().is_some_and(|token| {
            token.len() > 200
                || !token
                    .chars()
                    .all(|c| c.is_ascii_graphic() && c != '"' && c != '\\')
        }) {
            return Err("jeton d'accès invalide".to_owned());
        }
        Ok(Self {
            server,
            topic,
            token,
        })
    }
}

pub struct Ntfy {
    path: PathBuf,
    settings: RwLock<Option<NtfySettings>>,
}

impl Ntfy {
    /// Reads `<data>/ntfy.json`; nothing there means notifications are not set up.
    ///
    /// # Errors
    /// Fails when the file exists but cannot be read.
    pub fn open(data_dir: &Path) -> anyhow::Result<Arc<Self>> {
        let path = data_dir.join(FILE);
        let settings: Option<NtfySettings> = load_json(&path)?;
        Ok(Arc::new(Self {
            path,
            settings: RwLock::new(settings),
        }))
    }

    #[must_use]
    pub fn settings(&self) -> Option<NtfySettings> {
        self.settings
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// `None` switches notifications off. The file holds the token: only Hestia reads it.
    ///
    /// # Errors
    /// Why the settings are refused, for the household.
    pub fn save(&self, settings: Option<NtfySettings>) -> Result<(), String> {
        let settings = settings.map(NtfySettings::cleaned).transpose()?;
        match &settings {
            Some(settings) => {
                save_json(&self.path, settings).map_err(|error| format!("{error:#}"))?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
                        .map_err(|error| error.to_string())?;
                }
            }
            None => match std::fs::remove_file(&self.path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.to_string()),
            },
        }
        *self
            .settings
            .write()
            .unwrap_or_else(PoisonError::into_inner) = settings;
        Ok(())
    }

    /// # Errors
    /// Why ntfy did not take the notification, for the household.
    pub async fn send(&self, title: &str, message: &str, priority: Priority) -> Result<(), String> {
        let Some(settings) = self.settings() else {
            return Err("notifications non réglées (Réglages → Notifications)".to_owned());
        };
        let mut body = json!({
            "topic": settings.topic,
            "message": message,
            "priority": level(priority),
        });
        if !title.is_empty() {
            body["title"] = json!(title);
        }
        // The token and the text go through stdin, never on the command line.
        let mut config = format!(
            "url = \"{}/\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"{}\"\n",
            settings.server,
            quoted(&body.to_string()),
        );
        if let Some(token) = &settings.token {
            let _ = writeln!(config, "header = \"Authorization: Bearer {token}\"");
        }
        let output = tokio::time::timeout(SEND_LIMIT, curl(config))
            .await
            .map_err(|_| "ntfy ne répond pas".to_owned())??;
        answer(&output)?;
        info!(
            server = settings.server,
            topic = settings.topic,
            "notification sent"
        );
        Ok(())
    }
}

impl Notifier for Ntfy {
    fn notify<'a>(
        &'a self,
        title: &'a str,
        message: &'a str,
        priority: Priority,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(self.send(title, message, priority))
    }
}

/// ntfy's levels: 1 (min) to 5 (max).
fn level(priority: Priority) -> u8 {
    match priority {
        Priority::Low => 2,
        Priority::Normal => 3,
        Priority::High => 4,
        Priority::Urgent => 5,
    }
}

/// A string as a curl config file wants it between double quotes.
fn quoted(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out
}

/// What curl printed: the body, then the status code on the last line.
async fn curl(config: String) -> Result<String, String> {
    let mut child = tokio::process::Command::new("curl")
        .args(["-sS", "--max-time", "10", "-w", "\n%{http_code}", "-K", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => "curl est introuvable sur cette machine".to_owned(),
            _ => error.to_string(),
        })?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(config.as_bytes())
            .await
            .map_err(|error| error.to_string())?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| error.to_string())?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        let reason = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if reason.is_empty() {
            "serveur ntfy injoignable".to_owned()
        } else {
            format!("serveur ntfy injoignable : {reason}")
        })
    }
}

fn answer(output: &str) -> Result<(), String> {
    let (body, code) = output.rsplit_once('\n').unwrap_or(("", output));
    match code.trim() {
        "200" => Ok(()),
        "401" | "403" => Err("ntfy refuse l'envoi : jeton ou droits du sujet".to_owned()),
        "429" => Err("ntfy limite les envois, réessaie plus tard".to_owned()),
        code => {
            let reason = serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|error| error["error"].as_str().map(str::to_owned));
            Err(match reason {
                Some(reason) => format!("ntfy a répondu {code} : {reason}"),
                None => format!("ntfy a répondu {code}"),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NtfySettings, answer, quoted};

    fn settings(server: &str, topic: &str, token: Option<&str>) -> Result<NtfySettings, String> {
        NtfySettings {
            server: server.to_owned(),
            topic: topic.to_owned(),
            token: token.map(str::to_owned),
        }
        .cleaned()
    }

    #[test]
    fn settings_are_cleaned_or_refused() {
        let ok = settings(" https://ntfy.example.fr/ ", "maison", Some("  ")).unwrap();
        assert_eq!(ok.server, "https://ntfy.example.fr");
        assert_eq!(ok.token, None, "a blank token is no token");
        assert!(settings("http://127.0.0.1:2586", "alertes_pi", Some("tk_abc")).is_ok());
        assert!(settings("ntfy.example.fr", "maison", None).is_err());
        assert!(settings("https://", "maison", None).is_err());
        assert!(settings("https://a.fr\"\nurl = x", "maison", None).is_err());
        assert!(settings("https://a.fr", "ma maison", None).is_err());
        assert!(settings("https://a.fr", "maison", Some("tk\"x")).is_err());
    }

    #[test]
    fn config_strings_are_escaped() {
        assert_eq!(quoted(r#"{"a":"b\"c\n"}"#), r#"{\"a\":\"b\\\"c\\n\"}"#);
        assert_eq!(quoted("deux\nlignes"), "deux\\nlignes");
    }

    #[test]
    fn answers_are_explained() {
        assert_eq!(answer("{\"id\":\"x\"}\n200"), Ok(()));
        assert!(answer("{}\n403").unwrap_err().contains("jeton"));
        assert_eq!(
            answer("{\"code\":40001,\"error\":\"invalid request\"}\n400").unwrap_err(),
            "ntfy a répondu 400 : invalid request"
        );
    }
}
