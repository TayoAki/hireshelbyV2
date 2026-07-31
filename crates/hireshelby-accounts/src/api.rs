//! HTTP surface for the desktop client.
//!
//! The wire contract is defined by
//! `desktop/src/features/communities/hostedCommunityApi.ts` and
//! `desktop/src-tauri/src/accounts.rs`, not by taste:
//!
//! - Errors are a **nested envelope**: `{"error": {"code", "message",
//!   "setup_needed"?}, "correlation_id"?}`. The Tauri layer passes any body
//!   with an `error` field through to the frontend, which maps `code` to a
//!   friendly message. A flat error shape would render as the raw fallback.
//! - Communities are `{id, name, slug, normalized_host, owner_pubkey,
//!   archived_at}`.
//! - Every mutation is a POST; even `list` is called as POST with `{}`.
//! - `transfer` alone takes camelCase keys, mirroring the legacy web client.

use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    auth, db,
    operator::ProvisionRequest,
    plan::{check_seat_available, PlanTier, QuotaDecision},
    AppState,
};

/// Hosted communities per account. Matches `HOSTED_COMMUNITY_LIMIT` in
/// `hostedCommunityApi.ts`; the frontend renders this number in its error
/// copy, so the two must agree.
const MAX_COMMUNITIES_PER_ACCOUNT: i64 = 3;

// ---------------------------------------------------------------------------
// Error envelope
// ---------------------------------------------------------------------------

/// Builder for the `{error: {...}, correlation_id}` envelope.
pub struct ApiFailure {
    status: StatusCode,
    code: &'static str,
    message: String,
    pub setup_needed: bool,
}

impl ApiFailure {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::OK,
            code,
            message: message.into(),
            setup_needed: false,
        }
    }

    pub fn status(mut self, status: StatusCode) -> Self {
        self.status = status;
        self
    }

    pub fn internal() -> Self {
        Self::new("internal", "Something went wrong. Try again.")
            .status(StatusCode::INTERNAL_SERVER_ERROR)
    }

    pub fn into_response_parts(self) -> (StatusCode, Json<serde_json::Value>) {
        // Correlation ids let a support thread find the exact server log line
        // without the user pasting internals.
        let correlation_id = Uuid::new_v4().to_string();
        let mut error = serde_json::json!({
            "code": self.code,
            "message": self.message,
        });
        if self.setup_needed {
            error["setup_needed"] = serde_json::json!(true);
        }
        (
            self.status,
            Json(serde_json::json!({
                "error": error,
                "correlation_id": correlation_id,
            })),
        )
    }
}

/// Convenience: an OK-status failure envelope (the frontend keys off `code`,
/// not HTTP status, for expected states).
pub fn correlated(
    code: &'static str,
    message: impl Into<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    ApiFailure::new(code, message).into_response_parts()
}

type ApiResult = Result<Json<serde_json::Value>, (StatusCode, Json<serde_json::Value>)>;

async fn session_or_unauthorized(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<auth::SessionUser, (StatusCode, Json<serde_json::Value>)> {
    auth::authenticate(state, headers).await.map_err(|_| {
        ApiFailure::new("unauthorized", "Sign in first.")
            .status(StatusCode::UNAUTHORIZED)
            .into_response_parts()
    })
}

// ---------------------------------------------------------------------------
// Community shapes
// ---------------------------------------------------------------------------

/// Matches `VALID_HOSTED_COMMUNITY_NAME` in `hostedCommunityApi.ts`:
/// `^[a-z0-9]+(?:-[a-z0-9]+)*$`, bounded so it stays a valid DNS label.
fn validate_name(name: &str) -> Result<(), ()> {
    if name.len() < 3 || name.len() > 63 {
        return Err(());
    }
    let mut prev_hyphen = true; // rejects a leading hyphen
    for c in name.chars() {
        match c {
            'a'..='z' | '0'..='9' => prev_hyphen = false,
            '-' if !prev_hyphen => prev_hyphen = true,
            _ => return Err(()),
        }
    }
    if prev_hyphen {
        return Err(()); // trailing hyphen (or all hyphens)
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct CommunityJson {
    id: String,
    name: String,
    slug: String,
    normalized_host: String,
    owner_pubkey: Option<String>,
    archived_at: Option<String>,
}

impl CommunityJson {
    fn from_row(row: db::CommunityRow) -> Self {
        Self {
            id: row.id.to_string(),
            name: row.slug.clone(),
            slug: row.slug,
            normalized_host: row.host,
            owner_pubkey: row.owner_pubkey,
            archived_at: row.archived_at.map(|t| t.to_rfc3339()),
        }
    }
}

// ---------------------------------------------------------------------------
// POST /v1/communities/availability   { name }
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NameRequest {
    pub name: String,
}

pub async fn availability(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NameRequest>,
) -> ApiResult {
    session_or_unauthorized(state.as_ref(), &headers).await?;
    let name = req.name.trim().to_lowercase();
    if validate_name(&name).is_err() {
        return Err(correlated(
            "invalid_name",
            "Use lowercase letters, numbers, and hyphens.",
        ));
    }
    let host = state.config.community_host(&name);
    let taken = db::host_exists(&state.db, &host).await.map_err(|error| {
        tracing::error!(%error, "availability: lookup failed");
        ApiFailure::internal().into_response_parts()
    })?;
    Ok(Json(serde_json::json!({
        "available": !taken,
        "normalized_host": host,
    })))
}

// ---------------------------------------------------------------------------
// POST /v1/communities   { name }
// ---------------------------------------------------------------------------

pub async fn create_community(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<NameRequest>,
) -> ApiResult {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;

    let name = req.name.trim().to_lowercase();
    if validate_name(&name).is_err() {
        return Err(correlated(
            "invalid_name",
            "Use lowercase letters, numbers, and hyphens.",
        ));
    }

    // The community owner is the account's bound Nostr identity — without one
    // there is no pubkey to hand the relay as initial owner.
    let owner_pubkey = db::bound_pubkey(&state.db, user.account_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "create: identity lookup failed");
            ApiFailure::internal().into_response_parts()
        })?
        .ok_or_else(|| {
            let mut failure = ApiFailure::new("missing_mapping", "Connect your identity first.");
            failure.setup_needed = true;
            failure.into_response_parts()
        })?;

    // Community cap. Fail-soft on lookup error: an outage must not block a
    // paying customer, and the cap is anti-abuse rather than billing.
    match db::active_community_count(&state.db, user.account_id).await {
        Ok(in_use) if in_use >= MAX_COMMUNITIES_PER_ACCOUNT => {
            return Err(correlated(
                "limit_reached",
                format!(
                    "You've reached the limit of {MAX_COMMUNITIES_PER_ACCOUNT} hosted communities."
                ),
            ));
        }
        Ok(_) => {}
        Err(error) => {
            tracing::error!(%error, "create: count failed; allowing (fail-soft)");
        }
    }

    let host = state.config.community_host(&name);
    if db::host_exists(&state.db, &host).await.unwrap_or(false) {
        return Err(correlated("taken", "That address is already taken."));
    }

    // Relay first, local record second: a relay rejection must not strand a
    // local row claiming tenancy that does not exist. The reverse order would.
    let relay_response = state
        .operator
        .provision_community(&ProvisionRequest {
            host: host.clone(),
            initial_owner_pubkey: Some(owner_pubkey.clone()),
        })
        .await
        .map_err(|error| {
            tracing::error!(%error, %host, "create: relay rejected provisioning");
            correlated(
                "relay_unavailable",
                "Community provisioning is temporarily unavailable.",
            )
        })?;

    let row = db::record_community_full(
        &state.db,
        user.account_id,
        &name,
        &host,
        &owner_pubkey,
        relay_response.community_id.as_deref(),
    )
    .await
    .map_err(|error| {
        // The relay already has the tenant; a retry is idempotent on both sides.
        tracing::error!(%error, %host, "create: relay ok but local record failed");
        ApiFailure::internal().into_response_parts()
    })?;

    if let Err(error) = db::insert_default_plan(&state.db, row.id, PlanTier::Trial, 1).await {
        // Non-fatal: reconciled by the billing sweep, not by failing the user.
        tracing::error!(%error, community_id = %row.id, "create: trial plan seed failed");
    }

    Ok(Json(serde_json::json!({
        "community": CommunityJson::from_row(row),
    })))
}

// ---------------------------------------------------------------------------
// POST /v1/communities/list   {}
// ---------------------------------------------------------------------------

pub async fn list_communities(State(state): State<Arc<AppState>>, headers: HeaderMap) -> ApiResult {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    let rows = db::list_communities(&state.db, user.account_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "list: query failed");
            ApiFailure::internal().into_response_parts()
        })?;
    Ok(Json(serde_json::json!({
        "communities": rows.into_iter().map(CommunityJson::from_row).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// POST /v1/communities/archive | /unarchive   { community_id }
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CommunityIdRequest {
    pub community_id: String,
}

/// Loads a community and asserts the session account owns it.
///
/// `not_owner` is returned for both wrong-account and nonexistent ids so the
/// endpoint cannot be used to probe which community ids exist.
async fn owned_community(
    state: &AppState,
    account_id: Uuid,
    community_id: &str,
) -> Result<db::CommunityRow, (StatusCode, Json<serde_json::Value>)> {
    let id = Uuid::parse_str(community_id.trim())
        .map_err(|_| correlated("not_owner", "Only the community owner can do that."))?;
    db::community_owned_by(&state.db, id, account_id)
        .await
        .map_err(|error| {
            tracing::error!(%error, "lifecycle: ownership lookup failed");
            ApiFailure::internal().into_response_parts()
        })?
        .ok_or_else(|| correlated("not_owner", "Only the community owner can do that."))
}

pub async fn archive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CommunityIdRequest>,
) -> ApiResult {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    let row = owned_community(&state, user.account_id, &req.community_id).await?;

    let owner = row.owner_pubkey.clone().unwrap_or_default();
    state
        .operator
        .archive_community(&row.host, &owner)
        .await
        .map_err(|error| {
            tracing::error!(%error, host = %row.host, "archive: relay call failed");
            correlated("relay_unavailable", "Archiving is temporarily unavailable.")
        })?;

    let row = db::set_archived(&state.db, row.id, true)
        .await
        .map_err(|error| {
            tracing::error!(%error, "archive: local update failed");
            ApiFailure::internal().into_response_parts()
        })?;
    Ok(Json(
        serde_json::json!({ "community": CommunityJson::from_row(row) }),
    ))
}

pub async fn unarchive(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<CommunityIdRequest>,
) -> ApiResult {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    let row = owned_community(&state, user.account_id, &req.community_id).await?;

    let owner = row.owner_pubkey.clone().unwrap_or_default();
    state
        .operator
        .unarchive_community(&row.host, &owner)
        .await
        .map_err(|error| {
            tracing::error!(%error, host = %row.host, "unarchive: relay call failed");
            correlated(
                "relay_unavailable",
                "Unarchiving is temporarily unavailable.",
            )
        })?;

    let row = db::set_archived(&state.db, row.id, false)
        .await
        .map_err(|error| {
            tracing::error!(%error, "unarchive: local update failed");
            ApiFailure::internal().into_response_parts()
        })?;
    Ok(Json(
        serde_json::json!({ "community": CommunityJson::from_row(row) }),
    ))
}

// ---------------------------------------------------------------------------
// POST /v1/communities/transfer   { communityId, transfereeNpub }  (camelCase)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferRequest {
    pub community_id: String,
    pub transferee_npub: String,
}

pub async fn transfer(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<TransferRequest>,
) -> ApiResult {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    let row = owned_community(&state, user.account_id, &req.community_id).await?;

    use nostr::FromBech32 as _;
    let transferee = nostr::PublicKey::from_bech32(req.transferee_npub.trim())
        .map_err(|_| correlated("transferee_not_registered", "That npub is not valid."))?;
    let transferee_hex = transferee.to_hex();

    // The transferee must already be a registered account with this identity
    // bound — otherwise the transferred community would dangle with an owner
    // that cannot sign in to manage it.
    let transferee_account = db::account_for_pubkey(&state.db, &transferee_hex)
        .await
        .map_err(|error| {
            tracing::error!(%error, "transfer: transferee lookup failed");
            ApiFailure::internal().into_response_parts()
        })?
        .ok_or_else(|| {
            correlated(
                "transferee_not_registered",
                "That person needs a connected identity first.",
            )
        })?;

    // The relay compare-and-swaps on the expected owner; a stale local mirror
    // fails the transfer rather than silently overriding a newer rotation.
    let relay_community_id = row.relay_community_id.clone().ok_or_else(|| {
        tracing::error!(community_id = %row.id, "transfer: relay community id missing");
        ApiFailure::internal().into_response_parts()
    })?;
    let expected_owner = row.owner_pubkey.clone().unwrap_or_default();
    state
        .operator
        .transfer_community(&relay_community_id, &transferee_hex, &expected_owner)
        .await
        .map_err(|error| {
            tracing::error!(%error, "transfer: relay call failed");
            correlated("relay_unavailable", "Transfer is temporarily unavailable.")
        })?;

    let row = db::transfer_owner(&state.db, row.id, transferee_account, &transferee_hex)
        .await
        .map_err(|error| {
            tracing::error!(%error, "transfer: local update failed");
            ApiFailure::internal().into_response_parts()
        })?;
    Ok(Json(
        serde_json::json!({ "community": CommunityJson::from_row(row) }),
    ))
}

// ---------------------------------------------------------------------------
// POST /v1/communities/{community_id}/seats/check   (relay-facing paywall)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct SeatCheckRequest {
    /// Seats currently occupied. Supplied by the caller because membership
    /// lives in the relay's database, not here.
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

/// The per-seat paywall. Returns 200 with `allowed: false` rather than an
/// error status because the caller needs tier + seat count either way to
/// render an upgrade prompt.
pub async fn check_seats(
    State(state): State<Arc<AppState>>,
    Path(community_id): Path<Uuid>,
    Json(req): Json<SeatCheckRequest>,
) -> Result<Json<SeatCheckResponse>, (StatusCode, Json<serde_json::Value>)> {
    let plan = match db::load_plan(&state.db, community_id).await {
        Ok(Some(plan)) => plan,
        Ok(None) => {
            return Err(
                ApiFailure::new("no_plan", "No billing plan for this community.")
                    .status(StatusCode::NOT_FOUND)
                    .into_response_parts(),
            );
        }
        Err(error) => {
            // Fail soft: never lock a paying customer out of inviting people
            // because the billing lookup is having a bad day.
            tracing::error!(%error, %community_id, "seats: plan lookup failed; allowing");
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
    fn accepts_names_the_frontend_accepts() {
        // Mirror of VALID_HOSTED_COMMUNITY_NAME cases.
        for name in ["acme", "acme-corp", "a1b-2c3", "abc"] {
            assert!(validate_name(name).is_ok(), "should accept {name}");
        }
    }

    #[test]
    fn rejects_names_the_frontend_rejects() {
        for name in [
            "ab",            // too short
            &"x".repeat(64), // exceeds a DNS label
            "Acme",          // uppercase
            "acme_corp",     // underscore
            "-acme",         // leading hyphen
            "acme-",         // trailing hyphen
            "acme--corp",    // double hyphen (regex forbids empty segments)
            "acme.corp",     // dot would inject a host label
            "",
        ] {
            assert!(validate_name(name).is_err(), "should reject {name:?}");
        }
    }

    #[test]
    fn error_envelope_is_nested_the_way_the_client_expects() {
        // The Tauri layer forwards any body with an `error` key; the frontend
        // then reads error.code. A flat {error: "...", message: "..."} shape
        // would fail that lookup silently.
        let (_, Json(body)) = correlated("taken", "That address is already taken.");
        assert_eq!(body["error"]["code"], "taken");
        assert!(body["error"]["message"].is_string());
        assert!(body["correlation_id"].is_string());
    }

    #[test]
    fn setup_needed_rides_inside_the_error_object() {
        let mut failure = ApiFailure::new("missing_mapping", "Connect your identity first.");
        failure.setup_needed = true;
        let (_, Json(body)) = failure.into_response_parts();
        assert_eq!(body["error"]["setup_needed"], true);
    }

    #[test]
    fn community_cap_matches_the_frontend_constant() {
        // hostedCommunityApi.ts renders HOSTED_COMMUNITY_LIMIT = 3 in its
        // error copy; disagreeing here shows users the wrong number.
        assert_eq!(MAX_COMMUNITIES_PER_ACCOUNT, 3);
    }

    #[test]
    fn transfer_request_is_camel_case() {
        // The one endpoint whose payload is camelCase, mirroring the legacy
        // web client. A snake_case reading would silently drop both fields.
        let parsed: TransferRequest =
            serde_json::from_str(r#"{"communityId":"abc","transfereeNpub":"npub1xyz"}"#).unwrap();
        assert_eq!(parsed.community_id, "abc");
        assert_eq!(parsed.transferee_npub, "npub1xyz");
    }
}
