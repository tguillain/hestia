use std::path::Path;

use anyhow::bail;
use hestia_hub::now_millis;
use serde::{Deserialize, Serialize};

use super::secret::{digest, random_token, valid_name};
use super::watched::Watched;

const TOKEN_PREFIX: &str = "hestia_";

/// What an AI holding the token may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TokenScope {
    /// Read devices and history.
    Read,
    /// Also send commands.
    Control,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredToken {
    name: String,
    scope: TokenScope,
    digest: String,
    created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenInfo {
    pub name: String,
    pub scope: TokenScope,
}

/// Bearer tokens for the MCP endpoint, in `mcp_tokens.json`. Only digests are stored:
/// a token is shown once, when it is created.
pub struct McpTokens {
    file: Watched<Vec<StoredToken>>,
}

impl McpTokens {
    /// # Errors
    /// Fails when the file exists but cannot be read or parsed.
    pub fn open_in(data_dir: &Path) -> anyhow::Result<Self> {
        Ok(Self {
            file: Watched::open(data_dir.join("mcp_tokens.json"))?,
        })
    }

    /// Creates a token and returns it; it cannot be read back later.
    ///
    /// # Errors
    /// Fails when the name is invalid or taken, or the file cannot be written.
    pub fn create(&self, name: &str, scope: TokenScope) -> anyhow::Result<String> {
        if !valid_name(name) {
            bail!("token name must be 1-32 characters among a-z, 0-9, '.', '-' and '_'");
        }
        let mut tokens = self.file.current();
        if tokens.iter().any(|token| token.name == name) {
            bail!("token {name:?} already exists");
        }
        let token = random_token(TOKEN_PREFIX);
        tokens.push(StoredToken {
            name: name.to_owned(),
            scope,
            digest: digest(&token),
            created_at: now_millis(),
        });
        self.file.save(tokens)?;
        Ok(token)
    }

    /// # Errors
    /// Fails when the token does not exist or the file cannot be written.
    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        let mut tokens = self.file.current();
        let before = tokens.len();
        tokens.retain(|token| token.name != name);
        if tokens.len() == before {
            bail!("unknown token {name:?}");
        }
        self.file.save(tokens)
    }

    #[must_use]
    pub fn list(&self) -> Vec<TokenInfo> {
        self.file
            .current()
            .into_iter()
            .map(|token| TokenInfo {
                name: token.name,
                scope: token.scope,
            })
            .collect()
    }

    pub(crate) fn verify(&self, token: &str) -> Option<TokenInfo> {
        let wanted = digest(token);
        self.file
            .current()
            .into_iter()
            .find(|stored| stored.digest == wanted)
            .map(|stored| TokenInfo {
                name: stored.name,
                scope: stored.scope,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::{McpTokens, TokenScope};

    #[test]
    fn tokens_are_verified_by_digest_and_revocable() {
        let dir = std::env::temp_dir().join(format!("hestia-mcp-tokens-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let tokens = McpTokens::open_in(&dir).unwrap();
        let token = tokens.create("claude", TokenScope::Read).unwrap();
        assert!(token.starts_with("hestia_"));
        assert!(tokens.create("claude", TokenScope::Read).is_err());
        assert_eq!(tokens.verify(&token).unwrap().scope, TokenScope::Read);
        assert!(tokens.verify("hestia_forged").is_none());
        let stored = std::fs::read_to_string(dir.join("mcp_tokens.json")).unwrap();
        assert!(
            !stored.contains(&token),
            "the token itself is never written"
        );
        tokens.remove("claude").unwrap();
        assert!(tokens.verify(&token).is_none());
    }
}
