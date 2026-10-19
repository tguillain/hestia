mod limiter;
mod password;
mod secret;
mod sessions;
mod tokens;
mod users;
mod watched;

use std::net::IpAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tracing::{info, warn};

pub use password::{hash_password, hash_password_with};
pub(crate) use secret::valid_name;
pub use tokens::{McpTokens, TokenInfo, TokenScope};
pub use users::UserStore;

use limiter::LoginLimiter;
use password::verify_password;
use sessions::SessionStore;

pub(crate) const SESSION_COOKIE: &str = "hestia_session";

pub(crate) enum LoginError {
    Locked(Duration),
    Invalid,
    Internal,
}

pub struct Auth {
    users: UserStore,
    tokens: McpTokens,
    sessions: SessionStore,
    limiter: LoginLimiter,
    decoy_hash: String,
}

impl Auth {
    /// Opens (or creates) the user and session files under `data_dir`.
    ///
    /// # Errors
    /// Fails when an existing file cannot be read or parsed.
    pub fn open(data_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            users: UserStore::open_in(data_dir)?,
            tokens: McpTokens::open_in(data_dir)?,
            sessions: SessionStore::open(data_dir.join("sessions.json"))?,
            limiter: LoginLimiter::default(),
            decoy_hash: hash_password("decoy password for unknown users")?,
        })
    }

    #[must_use]
    pub fn users(&self) -> &UserStore {
        &self.users
    }

    #[must_use]
    pub fn tokens(&self) -> &McpTokens {
        &self.tokens
    }

    /// Argon2 check off the async runtime.
    pub(crate) async fn check_password(self: &Arc<Self>, name: &str, password: String) -> bool {
        let auth = Arc::clone(self);
        let name = name.to_owned();
        tokio::task::spawn_blocking(move || {
            auth.users
                .password_hash(&name)
                .is_some_and(|hash| verify_password(&password, &hash))
        })
        .await
        .unwrap_or(false)
    }

    pub(crate) async fn login(
        self: &Arc<Self>,
        ip: IpAddr,
        name: String,
        password: String,
    ) -> Result<String, LoginError> {
        self.limiter.check(ip).map_err(LoginError::Locked)?;
        let auth = Arc::clone(self);
        let checked_name = name.clone();
        // Argon2 takes ~100 ms on a Pi: keep it off the async runtime.
        let valid = tokio::task::spawn_blocking(move || {
            let stored = auth.users.password_hash(&checked_name);
            let hash = stored.as_deref().unwrap_or(&auth.decoy_hash);
            verify_password(&password, hash) && stored.is_some()
        })
        .await
        .map_err(|_| LoginError::Internal)?;
        if !valid {
            warn!(%ip, user = name, "login failed");
            self.limiter.failure(ip);
            return Err(LoginError::Invalid);
        }
        self.limiter.success(ip);
        info!(%ip, user = name, "login");
        self.sessions.create(&name).map_err(|error| {
            warn!(%error, "cannot store session");
            LoginError::Internal
        })
    }

    pub(crate) fn authenticate(&self, token: &str) -> Option<String> {
        let user = self.sessions.validate(token)?;
        if self.users.contains(&user) {
            Some(user)
        } else {
            self.sessions.revoke(token);
            None
        }
    }

    /// Checks an MCP bearer token, with the same brute-force brake as passwords.
    pub(crate) fn mcp_token(
        &self,
        ip: IpAddr,
        token: Option<&str>,
    ) -> Result<TokenInfo, LoginError> {
        self.limiter.check(ip).map_err(LoginError::Locked)?;
        if let Some(info) = token.and_then(|token| self.tokens.verify(token)) {
            Ok(info)
        } else {
            warn!(%ip, "MCP request with an invalid token");
            self.limiter.failure(ip);
            Err(LoginError::Invalid)
        }
    }

    pub(crate) fn logout(&self, token: &str) {
        self.sessions.revoke(token);
    }
}
