use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use hestia_hub::now_millis;
use serde::{Deserialize, Serialize};
use tracing::warn;

use hestia_hub::storage::{load_json, save_json};

use super::secret::{digest, random_token};

const SESSION_TTL_MS: i64 = 30 * 24 * 3600 * 1000;
/// Sliding expiry is only written back once a day, to spare the SD card.
const REFRESH_AFTER_MS: i64 = 24 * 3600 * 1000;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    user: String,
    created_at: i64,
    expires_at: i64,
}

/// Sessions keyed by the SHA-256 of their token: the file never holds a usable token.
pub(crate) struct SessionStore {
    path: PathBuf,
    sessions: Mutex<HashMap<String, Session>>,
}

impl SessionStore {
    pub(crate) fn open(path: PathBuf) -> anyhow::Result<Self> {
        let mut sessions: HashMap<String, Session> = load_json(&path)?;
        let now = now_millis();
        sessions.retain(|_, session| session.expires_at > now);
        Ok(Self {
            path,
            sessions: Mutex::new(sessions),
        })
    }

    pub(crate) fn create(&self, user: &str) -> anyhow::Result<String> {
        let token = random_token("");
        let now = now_millis();
        let mut sessions = self.lock();
        sessions.retain(|_, session| session.expires_at > now);
        sessions.insert(
            digest(&token),
            Session {
                user: user.to_owned(),
                created_at: now,
                expires_at: now + SESSION_TTL_MS,
            },
        );
        save_json(&self.path, &*sessions)?;
        Ok(token)
    }

    pub(crate) fn validate(&self, token: &str) -> Option<String> {
        let key = digest(token);
        let now = now_millis();
        let mut sessions = self.lock();
        let session = sessions.get_mut(&key)?;
        if session.expires_at <= now {
            sessions.remove(&key);
            self.persist(&sessions);
            return None;
        }
        let user = session.user.clone();
        if session.expires_at - now < SESSION_TTL_MS - REFRESH_AFTER_MS {
            session.expires_at = now + SESSION_TTL_MS;
            self.persist(&sessions);
        }
        Some(user)
    }

    pub(crate) fn revoke(&self, token: &str) {
        let mut sessions = self.lock();
        if sessions.remove(&digest(token)).is_some() {
            self.persist(&sessions);
        }
    }

    fn persist(&self, sessions: &HashMap<String, Session>) {
        if let Err(error) = save_json(&self.path, sessions) {
            warn!(%error, "cannot save sessions");
        }
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Session>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
