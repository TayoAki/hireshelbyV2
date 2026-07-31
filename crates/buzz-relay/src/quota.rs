//! Seat-quota enforcement against the HireShelby control plane.
//!
//! The control plane owns billing state; the relay owns membership. This
//! module is the bridge: before the relay grows a community's membership, it
//! asks `POST {BUZZ_QUOTA_URL}/v1/quota/seats` whether the community's plan
//! has a seat free.
//!
//! ## Fail-soft, deliberately
//!
//! Every failure mode — unset URL, timeout, 5xx, malformed response — resolves
//! to **allow**. The billing plane being down must degrade to "we may
//! oversell a seat for a while", never to "paying customers cannot invite
//! anyone". The one thing that denies is a definitive `allowed: false` from a
//! healthy control plane.
//!
//! Deployment knobs (read once at first use):
//! - `BUZZ_QUOTA_URL` — control-plane base URL. Unset disables enforcement
//!   entirely, which is the right default for self-contained relays.
//! - `BUZZ_QUOTA_TOKEN` — optional shared secret sent as `X-Quota-Token`,
//!   matching the control plane's `HIRESHELBY_QUOTA_TOKEN`.

use std::sync::OnceLock;
use std::time::Duration;

/// Answering a membership request may not take longer than this; past it we
/// fail soft. Member adds are interactive, so the ceiling is tight.
const QUOTA_TIMEOUT: Duration = Duration::from_secs(3);

/// Result of a seat-quota check against the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SeatQuotaOutcome {
    /// The add may proceed (including every fail-soft path).
    Allowed,
    /// The control plane affirmatively said no; `reason` is user-renderable.
    Denied(String),
}

struct QuotaConfig {
    url: String,
    token: Option<String>,
    http: reqwest::Client,
}

static QUOTA: OnceLock<Option<QuotaConfig>> = OnceLock::new();

fn quota_config() -> &'static Option<QuotaConfig> {
    QUOTA.get_or_init(|| {
        let url = std::env::var("BUZZ_QUOTA_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())?;
        let token = std::env::var("BUZZ_QUOTA_TOKEN")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let http = reqwest::Client::builder()
            .timeout(QUOTA_TIMEOUT)
            .build()
            .ok()?;
        tracing::info!(%url, token_configured = token.is_some(), "seat quota enforcement enabled");
        Some(QuotaConfig { url, token, http })
    })
}

/// Asks the control plane whether `host`'s community may add one more member.
///
/// `seats_in_use` is the relay's current member count for the community — the
/// relay supplies it because membership lives here, not in the billing plane.
pub async fn check_seat_quota(host: &str, seats_in_use: i64) -> SeatQuotaOutcome {
    let Some(config) = quota_config() else {
        return SeatQuotaOutcome::Allowed;
    };

    let mut request = config
        .http
        .post(format!("{}/v1/quota/seats", config.url))
        .json(&serde_json::json!({ "host": host, "seats_in_use": seats_in_use }));
    if let Some(token) = &config.token {
        request = request.header("x-quota-token", token);
    }

    let response = match request.send().await {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, %host, "seat quota check unreachable; allowing (fail-soft)");
            return SeatQuotaOutcome::Allowed;
        }
    };
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), %host, "seat quota check errored; allowing (fail-soft)");
        return SeatQuotaOutcome::Allowed;
    }
    let body: serde_json::Value = match response.json().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, %host, "seat quota response malformed; allowing (fail-soft)");
            return SeatQuotaOutcome::Allowed;
        }
    };

    if body["allowed"].as_bool() == Some(false) {
        let reason = body["reason"]
            .as_str()
            .unwrap_or("Seat limit reached for this community's plan.")
            .to_string();
        return SeatQuotaOutcome::Denied(reason);
    }
    SeatQuotaOutcome::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unset_url_always_allows() {
        // The OnceLock caches per process; this test relies on BUZZ_QUOTA_URL
        // being unset in the unit-test environment, which is the shipped
        // default. Self-contained relays must never block member adds.
        if std::env::var("BUZZ_QUOTA_URL").is_ok() {
            eprintln!("skipping: BUZZ_QUOTA_URL is set in this environment");
            return;
        }
        assert_eq!(
            check_seat_quota("acme.communities.hireshelby.test", 999).await,
            SeatQuotaOutcome::Allowed
        );
    }
}
