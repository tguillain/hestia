use std::path::Path;

use anyhow::bail;
use hestia_hub::now_millis;
use serde::{Deserialize, Serialize};

use super::secret::valid_name;
use super::watched::Watched;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct User {
    name: String,
    password_hash: String,
    created_at: i64,
}

/// Household accounts in `users.json`.
pub struct UserStore {
    file: Watched<Vec<User>>,
}

impl UserStore {
    /// Opens `users.json` inside `data_dir`.
    ///
    /// # Errors
    /// Fails when the file exists but cannot be read or parsed.
    pub fn open_in(data_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            file: Watched::open(data_dir.join("users.json"))?,
        })
    }

    #[must_use]
    pub fn names(&self) -> Vec<String> {
        self.file
            .current()
            .into_iter()
            .map(|user| user.name)
            .collect()
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.file.current().iter().any(|user| user.name == name)
    }

    pub(crate) fn password_hash(&self, name: &str) -> Option<String> {
        self.file
            .current()
            .into_iter()
            .find(|user| user.name == name)
            .map(|user| user.password_hash)
    }

    /// # Errors
    /// Fails when the name is invalid or taken, or the file cannot be written.
    pub fn add(&self, name: &str, password_hash: String) -> anyhow::Result<()> {
        if !valid_name(name) {
            bail!("user name must be 1-32 characters among a-z, 0-9, '.', '-' and '_'");
        }
        let mut users = self.file.current();
        if users.iter().any(|user| user.name == name) {
            bail!("user {name:?} already exists");
        }
        users.push(User {
            name: name.to_owned(),
            password_hash,
            created_at: now_millis(),
        });
        self.file.save(users)
    }

    /// # Errors
    /// Fails when the user does not exist or the file cannot be written.
    pub fn set_password(&self, name: &str, password_hash: String) -> anyhow::Result<()> {
        let mut users = self.file.current();
        let Some(user) = users.iter_mut().find(|user| user.name == name) else {
            bail!("unknown user {name:?}");
        };
        user.password_hash = password_hash;
        self.file.save(users)
    }

    /// Removing a user also invalidates all of their sessions.
    ///
    /// # Errors
    /// Fails when the user does not exist or the file cannot be written.
    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        let mut users = self.file.current();
        let before = users.len();
        users.retain(|user| user.name != name);
        if users.len() == before {
            bail!("unknown user {name:?}");
        }
        self.file.save(users)
    }
}
