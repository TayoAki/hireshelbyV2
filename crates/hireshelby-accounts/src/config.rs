//! Control-plane configuration.
//!
//! ## The operator key is the crown jewel
//!
//! [`Config::operator_secret_key`] signs NIP-98 requests to the relay's
//! `/operator/communities` surface. That surface sits *above* tenancy: it can
//! create any community and rotate any community's owner. Compromise of this
//! key is compromise of every tenant on the deployment.
//!
//! Consequences enforced here:
//!
//! - It is read from the environment only, never from a config file that could
//!   be committed.
//! - It is never logged. [`Config`]'s `Debug` impl redacts it.
//! - Startup fails closed if it is absent or malformed, rather than falling
//!   back to an unauthenticated mode.

use std::fmt;

use nostr::Keys;

/// Minimal percent-encoding for query values. Avoids a dependency for two call
/// sites; only characters that are unsafe in a query string are escaped.
fn urlencode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config: {0} is required")]
    Missing(&'static str),
    #[error("config: {0} is invalid: {1}")]
    Invalid(&'static str, String),
}

pub struct Config {
    /// `host:port` the control-plane HTTP API binds to.
    pub bind_addr: String,
    /// Postgres connection string for the control-plane database.
    pub database_url: String,
    /// Base URL of the relay whose operator API we provision through,
    /// e.g. `http://localhost:3030`.
    pub relay_api_base_url: String,
    /// Signing key for operator requests. See the module docs — this is the
    /// highest-privilege secret in the system.
    pub operator_secret_key: Keys,
    /// Domain that community hosts are minted under, e.g.
    /// `communities.hireshelby.com`. A community named `acme` becomes
    /// `acme.communities.hireshelby.com`.
    pub community_domain: String,
    /// WorkOS AuthKit client id. When unset, hosted auth is not configured and
    /// login falls back to developer mode (if enabled).
    pub workos_client_id: Option<String>,
    /// Enables the local developer sign-in form, which issues a session for any
    /// email without proving ownership. A complete authentication bypass, so it
    /// defaults to off and must be opted into explicitly.
    pub dev_login_enabled: bool,
}

impl fmt::Debug for Config {
    /// Redacts the operator key. Deriving `Debug` would leak it into any
    /// `tracing` call that formats the config.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("relay_api_base_url", &self.relay_api_base_url)
            .field("operator_pubkey", &self.operator_public_key_hex())
            .field("operator_secret_key", &"<redacted>")
            .field("community_domain", &self.community_domain)
            .field("workos_configured", &self.workos_client_id.is_some())
            // Surfaced on purpose: an operator must be able to see from the
            // startup log that the auth bypass is not on in production.
            .field("dev_login_enabled", &self.dev_login_enabled)
            .finish()
    }
}

fn required(key: &'static str) -> Result<String, ConfigError> {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .ok_or(ConfigError::Missing(key))
}

fn optional_or(key: &str, default: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| default.to_string())
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        let operator_secret = required("HIRESHELBY_OPERATOR_SECRET_KEY")?;
        let operator_secret_key = Keys::parse(&operator_secret).map_err(|e| {
            // Deliberately does not echo the value being parsed.
            ConfigError::Invalid("HIRESHELBY_OPERATOR_SECRET_KEY", e.to_string())
        })?;

        let relay_api_base_url = required("HIRESHELBY_RELAY_API_BASE_URL")?;
        // Fail at startup rather than on the first provisioning attempt.
        url::Url::parse(&relay_api_base_url)
            .map_err(|e| ConfigError::Invalid("HIRESHELBY_RELAY_API_BASE_URL", e.to_string()))?;

        Ok(Self {
            bind_addr: optional_or("HIRESHELBY_BIND_ADDR", "0.0.0.0:4000"),
            database_url: required("HIRESHELBY_DATABASE_URL")?,
            relay_api_base_url: relay_api_base_url.trim_end_matches('/').to_string(),
            operator_secret_key,
            community_domain: optional_or(
                "HIRESHELBY_COMMUNITY_DOMAIN",
                "communities.hireshelby.com",
            ),
            workos_client_id: std::env::var("HIRESHELBY_WORKOS_CLIENT_ID")
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty()),
            dev_login_enabled: std::env::var("HIRESHELBY_DEV_LOGIN")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
        })
    }

    pub fn operator_public_key_hex(&self) -> String {
        self.operator_secret_key.public_key().to_hex()
    }

    /// WorkOS AuthKit authorize URL for this login attempt, or `None` when
    /// hosted auth is not configured.
    pub fn workos_authorize_url(&self, return_to: &str) -> Option<String> {
        let client_id = self.workos_client_id.as_ref()?;
        Some(format!(
            "https://api.workos.com/user_management/authorize?response_type=code\
             &provider=authkit&client_id={}&redirect_uri={}",
            urlencode(client_id),
            urlencode(return_to)
        ))
    }

    /// Full host for a community slug, e.g. `acme` →
    /// `acme.communities.hireshelby.com`.
    pub fn community_host(&self, slug: &str) -> String {
        format!("{slug}.{}", self.community_domain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_keys() -> Keys {
        Keys::parse("0000000000000000000000000000000000000000000000000000000000000003").unwrap()
    }

    fn cfg() -> Config {
        Config {
            bind_addr: "0.0.0.0:4000".into(),
            database_url: "postgres://u:p@localhost/db".into(),
            relay_api_base_url: "http://localhost:3030".into(),
            operator_secret_key: test_keys(),
            community_domain: "communities.hireshelby.com".into(),
            workos_client_id: None,
            dev_login_enabled: false,
        }
    }

    #[test]
    fn workos_url_is_none_until_a_client_id_is_configured() {
        // Without this, login would redirect to a malformed WorkOS URL instead
        // of falling back to developer mode.
        assert!(cfg()
            .workos_authorize_url("http://127.0.0.1:5551/cb")
            .is_none());
    }

    #[test]
    fn workos_url_percent_encodes_the_loopback_redirect() {
        let mut c = cfg();
        c.workos_client_id = Some("client_123".into());
        let url = c
            .workos_authorize_url("http://127.0.0.1:5551/callback/abc")
            .expect("configured");
        assert!(url.starts_with("https://api.workos.com/user_management/authorize?"));
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A5551%2Fcallback%2Fabc"),
            "redirect_uri must be encoded or it truncates the query: {url}"
        );
        assert!(
            !url.contains(' '),
            "no whitespace may leak into the URL: {url}"
        );
    }

    #[test]
    fn dev_login_is_off_unless_explicitly_enabled() {
        // It is a full authentication bypass; the default must be closed.
        assert!(!cfg().dev_login_enabled);
    }

    #[test]
    fn debug_redacts_the_operator_key_and_database_url() {
        let rendered = format!("{:?}", cfg());
        let secret = test_keys().secret_key().to_secret_hex();
        assert!(
            !rendered.contains(&secret),
            "operator secret key must never appear in Debug output"
        );
        assert!(
            !rendered.contains("postgres://u:p@"),
            "database credentials must never appear in Debug output"
        );
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn debug_still_exposes_the_operator_pubkey_for_diagnostics() {
        // The pubkey is not a secret and is needed to confirm the deployment is
        // configured with the key the relay allowlists.
        let rendered = format!("{:?}", cfg());
        assert!(rendered.contains(&test_keys().public_key().to_hex()));
    }

    #[test]
    fn community_host_composes_slug_and_domain() {
        assert_eq!(
            cfg().community_host("acme"),
            "acme.communities.hireshelby.com"
        );
    }
}
