//! Stripe billing webhooks.
//!
//! Checkout and the customer portal live on hireshelby.com; this service only
//! consumes the webhook stream and mutates `community_plans`. Two properties
//! carry the correctness burden:
//!
//! 1. **Signature verification.** The endpoint is an unauthenticated POST, so
//!    the `Stripe-Signature` header (HMAC-SHA256 over `"{t}.{payload}"`) is
//!    the only thing standing between "Stripe said the customer paid" and
//!    "anyone on the internet said so". Constant-time comparison, bounded
//!    timestamp skew.
//! 2. **Idempotency.** Stripe retries and reorders deliveries. Every processed
//!    event id is recorded in `processed_stripe_events`; a replay is a 200
//!    no-op, because erroring on a replay makes Stripe retry it forever.
//!
//! ## Event mapping
//!
//! | Event | Effect on `community_plans` |
//! |---|---|
//! | `checkout.session.completed` | tier + seats from metadata/quantity, Stripe ids recorded |
//! | `customer.subscription.updated` | seats follow quantity; tier follows metadata |
//! | `customer.subscription.deleted` | back to `trial` with 1 seat (grace path) |
//!
//! The community id rides in Stripe `metadata.community_id`, set when the
//! checkout session is created on the website.

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
};
use hmac::{Hmac, KeyInit as _, Mac};
use sha2::Sha256;
use uuid::Uuid;

use crate::AppState;

/// Maximum accepted age of a webhook signature. Stripe recommends 5 minutes.
const TOLERANCE_SECS: i64 = 300;

#[derive(Debug, PartialEq)]
pub enum SignatureError {
    Malformed,
    Expired,
    Mismatch,
}

/// Parses `Stripe-Signature: t=<unix>,v1=<hex>[,v1=<hex>...]` and verifies one
/// of the `v1` signatures matches `HMAC-SHA256(secret, "{t}.{payload}")`.
pub fn verify_signature(
    header: &str,
    payload: &[u8],
    secret: &str,
    now_unix: i64,
) -> Result<(), SignatureError> {
    let mut timestamp: Option<i64> = None;
    let mut candidates: Vec<String> = Vec::new();
    for part in header.split(',') {
        match part.trim().split_once('=') {
            Some(("t", value)) => timestamp = value.parse().ok(),
            Some(("v1", value)) => candidates.push(value.to_string()),
            _ => {}
        }
    }
    let timestamp = timestamp.ok_or(SignatureError::Malformed)?;
    if candidates.is_empty() {
        return Err(SignatureError::Malformed);
    }
    if (now_unix - timestamp).abs() > TOLERANCE_SECS {
        return Err(SignatureError::Expired);
    }

    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| SignatureError::Malformed)?;
    mac.update(timestamp.to_string().as_bytes());
    mac.update(b".");
    mac.update(payload);
    let expected = hex::encode(mac.finalize().into_bytes());

    // Constant-time compare against every candidate; Stripe sends multiple v1
    // entries during secret rotation.
    use subtle::ConstantTimeEq as _;
    for candidate in &candidates {
        if expected.as_bytes().ct_eq(candidate.as_bytes()).into() {
            return Ok(());
        }
    }
    Err(SignatureError::Mismatch)
}

/// Extracts what we need from a Stripe event, tolerating the fields we ignore.
fn plan_update_from_event(
    event_type: &str,
    object: &serde_json::Value,
) -> Option<(Uuid, Option<String>, Option<i64>)> {
    let community_id = object["metadata"]["community_id"]
        .as_str()
        .and_then(|s| Uuid::parse_str(s).ok())?;
    match event_type {
        "checkout.session.completed" | "customer.subscription.updated" => {
            let tier = object["metadata"]["tier"].as_str().map(str::to_string);
            // Seat count: subscription quantity, or the first line item's.
            let seats = object["quantity"]
                .as_i64()
                .or_else(|| object["items"]["data"][0]["quantity"].as_i64())
                .or_else(|| object["line_items"]["data"][0]["quantity"].as_i64());
            Some((community_id, tier, seats))
        }
        "customer.subscription.deleted" => {
            // Grace path: cancelled customers fall back to a single-seat trial
            // rather than being locked out of their data.
            Some((community_id, Some("trial".into()), Some(1)))
        }
        _ => None,
    }
}

pub async fn webhook(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> StatusCode {
    // Unconfigured billing = no endpoint. 404 rather than 401 so a deployment
    // without Stripe does not advertise an unauthenticated POST surface.
    let Some(secret) = state.config.stripe_webhook_secret.as_ref() else {
        return StatusCode::NOT_FOUND;
    };

    let Some(signature) = headers
        .get("stripe-signature")
        .and_then(|v| v.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST;
    };
    if let Err(error) = verify_signature(signature, &body, secret, chrono::Utc::now().timestamp()) {
        tracing::warn!(?error, "billing: webhook signature rejected");
        return StatusCode::BAD_REQUEST;
    }

    let Ok(event) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    let event_id = event["id"].as_str().unwrap_or_default().to_string();
    let event_type = event["type"].as_str().unwrap_or_default().to_string();
    if event_id.is_empty() || event_type.is_empty() {
        return StatusCode::BAD_REQUEST;
    }

    // Idempotency gate. The insert is the lock: a second delivery loses the
    // race on the primary key and is acknowledged without re-processing.
    match sqlx::query("INSERT INTO processed_stripe_events (event_id) VALUES ($1)")
        .bind(&event_id)
        .execute(&state.db)
        .await
    {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            tracing::info!(%event_id, "billing: replayed event acknowledged");
            return StatusCode::OK;
        }
        Err(error) => {
            tracing::error!(%error, "billing: idempotency insert failed");
            // 500 → Stripe retries later, when the database is back.
            return StatusCode::INTERNAL_SERVER_ERROR;
        }
    }

    let Some((community_id, tier, seats)) =
        plan_update_from_event(&event_type, &event["data"]["object"])
    else {
        // Unhandled event types are acknowledged: subscribing the endpoint to
        // more events than it consumes must not produce endless retries.
        tracing::info!(%event_type, "billing: event type ignored");
        return StatusCode::OK;
    };

    let stripe_customer = event["data"]["object"]["customer"].as_str();
    let stripe_subscription = event["data"]["object"]["subscription"]
        .as_str()
        .or_else(|| {
            (event_type == "customer.subscription.updated")
                .then(|| event["data"]["object"]["id"].as_str())
                .flatten()
        });

    let result = sqlx::query(
        "UPDATE community_plans SET \
           tier = COALESCE($2, tier), \
           seats_purchased = COALESCE($3, seats_purchased), \
           stripe_customer_id = COALESCE($4, stripe_customer_id), \
           stripe_subscription_id = COALESCE($5, stripe_subscription_id), \
           updated_at = now() \
         WHERE community_id = $1",
    )
    .bind(community_id)
    .bind(tier)
    .bind(seats.map(|s| s as i32))
    .bind(stripe_customer)
    .bind(stripe_subscription)
    .execute(&state.db)
    .await;

    match result {
        Ok(done) if done.rows_affected() > 0 => {
            tracing::info!(%community_id, %event_type, "billing: plan updated");
            StatusCode::OK
        }
        Ok(_) => {
            // Metadata pointed at a community we do not know. Acknowledge —
            // retrying will not create the row — but log loudly.
            tracing::error!(%community_id, "billing: event for unknown community");
            StatusCode::OK
        }
        Err(error) => {
            tracing::error!(%error, "billing: plan update failed");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(payload: &[u8], secret: &str, t: i64) -> String {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(t.to_string().as_bytes());
        mac.update(b".");
        mac.update(payload);
        format!("t={t},v1={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn valid_signature_verifies() {
        let payload = br#"{"id":"evt_1"}"#;
        let header = sign(payload, "whsec_test", 1_000_000);
        assert_eq!(
            verify_signature(&header, payload, "whsec_test", 1_000_000),
            Ok(())
        );
    }

    #[test]
    fn wrong_secret_is_rejected() {
        let payload = br#"{"id":"evt_1"}"#;
        let header = sign(payload, "whsec_other", 1_000_000);
        assert_eq!(
            verify_signature(&header, payload, "whsec_test", 1_000_000),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn tampered_payload_is_rejected() {
        let header = sign(br#"{"id":"evt_1"}"#, "whsec_test", 1_000_000);
        assert_eq!(
            verify_signature(&header, br#"{"id":"evt_2"}"#, "whsec_test", 1_000_000),
            Err(SignatureError::Mismatch)
        );
    }

    #[test]
    fn stale_timestamp_is_rejected_replayed_headers_expire() {
        let payload = br#"{"id":"evt_1"}"#;
        let header = sign(payload, "whsec_test", 1_000_000);
        assert_eq!(
            verify_signature(
                &header,
                payload,
                "whsec_test",
                1_000_000 + TOLERANCE_SECS + 1
            ),
            Err(SignatureError::Expired)
        );
    }

    #[test]
    fn rotation_period_multiple_v1_entries_any_match_passes() {
        let payload = br#"{"id":"evt_1"}"#;
        let good = sign(payload, "whsec_test", 1_000_000);
        let good_sig = good.split("v1=").nth(1).unwrap();
        let header = format!("t=1000000,v1={}, v1={good_sig}", "0".repeat(64));
        assert_eq!(
            verify_signature(&header, payload, "whsec_test", 1_000_000),
            Ok(())
        );
    }

    #[test]
    fn missing_parts_are_malformed() {
        assert_eq!(
            verify_signature("v1=abc", b"x", "s", 0),
            Err(SignatureError::Malformed),
            "no timestamp"
        );
        assert_eq!(
            verify_signature("t=123", b"x", "s", 0),
            Err(SignatureError::Malformed),
            "no v1 signature"
        );
    }

    #[test]
    fn checkout_completed_maps_tier_and_seats_from_metadata() {
        let object = serde_json::json!({
            "metadata": { "community_id": "8fe68279-e9db-41e5-8e45-f6b26bfffda5", "tier": "team" },
            "quantity": 5,
        });
        let (community, tier, seats) =
            plan_update_from_event("checkout.session.completed", &object).unwrap();
        assert_eq!(
            community.to_string(),
            "8fe68279-e9db-41e5-8e45-f6b26bfffda5"
        );
        assert_eq!(tier.as_deref(), Some("team"));
        assert_eq!(seats, Some(5));
    }

    #[test]
    fn subscription_deleted_falls_back_to_trial_not_lockout() {
        let object = serde_json::json!({
            "metadata": { "community_id": "8fe68279-e9db-41e5-8e45-f6b26bfffda5" },
        });
        let (_, tier, seats) =
            plan_update_from_event("customer.subscription.deleted", &object).unwrap();
        assert_eq!(tier.as_deref(), Some("trial"));
        assert_eq!(
            seats,
            Some(1),
            "cancellation keeps one seat so data stays reachable"
        );
    }

    #[test]
    fn events_without_community_metadata_are_ignored() {
        let object = serde_json::json!({ "metadata": {} });
        assert!(plan_update_from_event("checkout.session.completed", &object).is_none());
    }

    #[test]
    fn unhandled_event_types_are_ignored() {
        let object = serde_json::json!({
            "metadata": { "community_id": "8fe68279-e9db-41e5-8e45-f6b26bfffda5" },
        });
        assert!(plan_update_from_event("invoice.paid", &object).is_none());
    }
}
