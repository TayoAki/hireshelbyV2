//! HTTP surface.
//!
//! Endpoint shapes mirror the Builderlab paths the desktop client already
//! calls, so swapping the base URL is the only client-side change required.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    db,
    operator::ProvisionRequest,
    plan::{check_seat_available, PlanTier, QuotaDecision},
    AppState,
};

/// Communities one account may hold before requiring a sales conversation.
/// Anti-abuse on public signup, not a billing tier.
const MAX_COMMUNITIES_PER_ACCOUNT: i64 = 5;

/// A community slug must be safe to use as a DNS label, because it becomes one:
/// `<slug>.communities.hireshelby.com`.
fn validate_slug(slug: &str) -> Result<(), String> {
    if slug.len() < 3 || slug.len() > 63 {
        return Err("slug must be between 3 and 63 characters".into());
    }
    if !slug
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        return Err("slug may contain only lowercase letters, digits, and hyphens".into());
    }
    if slug.starts_with('-') || slug.ends_with('-') {
        return Err("slug may not start or end with a hyphen".into());
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct CreateCommunityRequest {
    pub account_id: Uuid,
    pub slug: String,
    /// Bootstrap owner for the new community. Optional; when omitted the relay
    /// leaves ownership to its own bootstrap path.
    #[serde(default)]
    pub owner_pubkey: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct CreateCommunityResponse {
    pub community_id: Uuid,
    pub host: String,
}

#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: String,
    pub message: String,
}

fn bad_request(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ApiError {
            error: "invalid_request".into(),
            message: message.into(),
        }),
    )
}

fn quota_exceeded(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::PAYMENT_REQUIRED,
        Json(ApiError {
            error: "quota_exceeded".into(),
            message: message.into(),
        }),
    )
}

fn upstream_error(message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        StatusCode::BAD_GATEWAY,
        Json(ApiError {
            error: "relay_unavailable".into(),
            message: message.into(),
        }),
    )
}

/// Provisions a community: validate → check quota → provision on the relay →
/// record locally → seed the trial plan.
///
/// The relay call happens *before* the local insert so a relay rejection never
/// leaves a row claiming tenancy that does not exist. The reverse ordering
/// would strand phantom communities on every relay failure.
pub async fn create_community(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CreateCommunityRequest>,
) -> Result<(StatusCode, Json<CreateCommunityResponse>), (StatusCode, Json<ApiError>)> {
    if let Err(message) = validate_slug(&req.slug) {
        return Err(bad_request(message));
    }

    // Community cap, not a seat check. Plans are per-community (see the
    // community_plans schema), so there is no plan row to consult before the
    // first community exists. Seats are enforced separately, per community, by
    // `check_seats`.
    //
    // The cap is anti-abuse for public signup: without it one account can spin
    // up unlimited tenants on the relay.
    match db::active_community_count(&state.db, req.account_id).await {
        Ok(in_use) if in_use >= MAX_COMMUNITIES_PER_ACCOUNT => {
            return Err(quota_exceeded(format!(
                "community limit reached: {in_use} of {MAX_COMMUNITIES_PER_ACCOUNT}. \
                 Contact sales to raise this limit."
            )));
        }
        Ok(_) => {}
        Err(error) => {
            // Fail soft: a control-plane database hiccup must not block
            // onboarding for a paying customer.
            tracing::error!(%error, "quota: community count failed; allowing (fail-soft)");
        }
    }

    let host = state.config.community_host(&req.slug);

    state
        .operator
        .provision_community(&ProvisionRequest {
            host: host.clone(),
            initial_owner_pubkey: req.owner_pubkey.clone(),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %host, "provisioning: relay rejected the request");
            upstream_error(error.to_string())
        })?;

    let community_id = db::record_community(&state.db, req.account_id, &req.slug, &host)
        .await
        .map_err(|error| {
            // The relay already has the tenant at this point. Surfacing the
            // error is correct — the retry is idempotent on both sides.
            tracing::error!(%error, %host, "provisioning: relay succeeded but local record failed");
            upstream_error(error.to_string())
        })?;

    if let Err(error) = db::insert_default_plan(&state.db, community_id, PlanTier::Trial, 1).await {
        // Non-fatal: the community exists and is usable. A missing plan row is
        // reconciled by the billing sweep rather than failing the request.
        tracing::error!(%error, %community_id, "provisioning: failed to seed trial plan");
    }

    Ok((
        StatusCode::CREATED,
        Json(CreateCommunityResponse { community_id, host }),
    ))
}

#[derive(Debug, Deserialize)]
pub struct SeatCheckRequest {
    /// Seats currently occupied in this community. Supplied by the caller
    /// because membership lives in the relay's database, not here.
    pub seats_in_use: i64,
}

#[derive(Debug, Serialize)]
pub struct SeatCheckResponse {
    pub allowed: bool,
    pub seats_purchased: i64,
    pub tier: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Answers "may this community add one more member?"
///
/// This is where per-seat pricing is actually enforced. It returns 200 with
/// `allowed: false` rather than an error status, because the caller needs the
/// tier and seat count to render an upgrade prompt either way.
pub async fn check_seats(
    State(state): State<Arc<AppState>>,
    Path(community_id): Path<Uuid>,
    Json(req): Json<SeatCheckRequest>,
) -> Result<Json<SeatCheckResponse>, (StatusCode, Json<ApiError>)> {
    let plan = match db::load_plan(&state.db, community_id).await {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(ApiError {
                    error: "no_plan".into(),
                    message: format!("no billing plan for community {community_id}"),
                }),
            ));
        }
        Err(error) => {
            // Fail soft: never lock a paying customer out of inviting people
            // because our billing lookup is having a bad day.
            tracing::error!(%error, %community_id, "quota: plan lookup failed; allowing (fail-soft)");
            return Ok(Json(SeatCheckResponse {
                allowed: true,
                seats_purchased: 0,
                tier: "unknown".into(),
                reason: Some("billing lookup unavailable; allowed".into()),
            }));
        }
    };

    let decision = check_seat_available(&plan, req.seats_in_use);
    Ok(Json(SeatCheckResponse {
        allowed: decision.permits(),
        seats_purchased: plan.seats_purchased,
        tier: plan.tier.as_str().to_string(),
        reason: match decision {
            QuotaDecision::Deny { reason } | QuotaDecision::Undetermined { reason } => Some(reason),
            QuotaDecision::Allow => None,
        },
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_dns_safe_slugs() {
        for slug in ["acme", "acme-corp", "a1b2c3", "x".repeat(63).as_str()] {
            assert!(validate_slug(slug).is_ok(), "should accept {slug}");
        }
    }

    #[test]
    fn rejects_slugs_that_would_break_the_hostname() {
        // Each of these would produce an invalid or ambiguous DNS label once
        // composed into <slug>.communities.hireshelby.com.
        for slug in [
            "ab",            // too short
            &"x".repeat(64), // too long for a DNS label
            "Acme",          // uppercase
            "acme_corp",     // underscore is not valid in a hostname
            "-acme",         // leading hyphen
            "acme-",         // trailing hyphen
            "acme.corp",     // would inject an extra label
            "acme corp",     // space
        ] {
            assert!(validate_slug(slug).is_err(), "should reject {slug:?}");
        }
    }

    #[test]
    fn quota_denial_maps_to_402_not_403() {
        // 402 Payment Required tells the client this is a billing state the
        // user can resolve by upgrading, not an authorization failure.
        let (status, _) = quota_exceeded("seat limit reached");
        assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    }

    #[test]
    fn relay_failure_maps_to_502_not_500() {
        // The failure is upstream; 502 keeps our own health signal honest.
        let (status, _) = upstream_error("relay refused");
        assert_eq!(status, StatusCode::BAD_GATEWAY);
    }
}
