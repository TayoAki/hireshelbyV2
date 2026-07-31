//! Nostr identity binding — proves a signed-in account controls a pubkey.
//!
//! ## Protocol (mirrors `desktop/src-tauri/src/nostr_bind.rs`)
//!
//! 1. `POST /v1/nostr-identities/challenge` mints `{challenge_id, nonce,
//!    verification_code, origin, expires_at}`.
//! 2. The desktop signs a kind-24243 event whose tags echo every challenge
//!    field plus the fixed `audience`/`action`/`protocol`/`version` tags.
//! 3. `POST /v1/nostr-identities/verify` receives `{challenge_id, nonce,
//!    signed_payload}`; we verify the Schnorr signature and that every tag
//!    matches the *stored* challenge — not the caller's copy — then bind the
//!    event's pubkey to the account.
//!
//! ## Why the tags are checked against the stored row
//!
//! The signature only proves the key signed *that event*. Without comparing
//! the event's tags to the challenge we issued, a captured binding event from
//! another context (or another deployment) could be replayed here. The stored
//! challenge is single-use and expiring for the same reason login codes are.
//!
//! ## Binding cardinality
//!
//! One pubkey per account, one account per pubkey. The desktop treats the
//! identity as *the* workspace identity, and communities are owned by pubkey —
//! two accounts sharing one pubkey would both "own" the same communities.

use std::sync::Arc;

use axum::{extract::State, http::HeaderMap, Json};
use base64::Engine as _;
use chrono::{DateTime, Utc};
use nostr::{JsonUtil as _, ToBech32 as _};
use serde::Deserialize;
use uuid::Uuid;

use crate::{api::ApiFailure, auth, AppState};

/// Fixed protocol tags. Kept byte-identical to the desktop's `nostr_bind.rs` —
/// a mismatch on any of these makes every binding fail verification.
const AUDIENCE: &str = "buzz:nostr-identity";
const ACTION: &str = "bind_nostr_identity";
const PROTOCOL: &str = "buzz-nostr-identity";
const VERSION: &str = "1";
const BINDING_KIND: u32 = buzz_core::kind::KIND_NOSTR_IDENTITY_BINDING;

const CHALLENGE_TTL_MINUTES: i64 = 10;

fn expiry_string(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Nonce in the exact shape `nostr_bind::validate_nonce` demands: 43
/// characters of URL-safe base64 (32 random bytes, unpadded). A UUID renders
/// as 32 hex characters and is rejected by the desktop before it ever signs,
/// which is the "invalid nonce" the client surfaces.
fn binding_nonce() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Six-digit code displayed by clients during binding so a user can confirm
/// they are approving the request they initiated.
fn verification_code() -> String {
    let n: u32 = rand::random();
    format!("{:06}", n % 1_000_000)
}

async fn session_or_unauthorized(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<auth::SessionUser, (axum::http::StatusCode, Json<serde_json::Value>)> {
    auth::authenticate(state, headers).await.map_err(|_| {
        ApiFailure::new("unauthorized", "Sign in first.")
            .status(axum::http::StatusCode::UNAUTHORIZED)
            .into_response_parts()
    })
}

// ---------------------------------------------------------------------------
// POST /v1/nostr-identities/challenge
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ChallengeRequest {
    /// Echoed into the challenge so the signing UI can display where the
    /// request came from. The *stored* origin is what verify checks.
    #[serde(default)]
    pub origin: Option<String>,
}

pub async fn challenge(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ChallengeRequest>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;

    let challenge_id = Uuid::new_v4();
    let nonce = binding_nonce();
    let code = verification_code();
    // Server-minted, never the caller's: the origin is signed over, so letting
    // a client choose it would let it bind an identity under someone else's
    // origin. It must also be a bare https origin — `nostr_bind::validate_origin`
    // rejects http, credentials, paths, queries, and fragments.
    let origin = state.config.public_origin.clone();
    let expires_at = Utc::now() + chrono::Duration::minutes(CHALLENGE_TTL_MINUTES);

    // The nonce column is unique; the full challenge payload rides in JSON so
    // verify can compare every field the client signed over.
    let payload = serde_json::json!({
        "challenge_id": challenge_id,
        "verification_code": code,
        "origin": origin,
        "expires_at": expiry_string(expires_at),
    });
    sqlx::query(
        "INSERT INTO identity_challenges (id, account_id, nonce, expires_at) \
         VALUES ($1, $2, $3, $4)",
    )
    .bind(challenge_id)
    .bind(user.account_id)
    .bind(format!("{nonce}|{payload}"))
    .bind(expires_at)
    .execute(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "identity: challenge insert failed");
        ApiFailure::internal().into_response_parts()
    })?;

    Ok(Json(serde_json::json!({
        "challenge_id": challenge_id.to_string(),
        "nonce": nonce,
        "verification_code": code,
        "origin": origin,
        "expires_at": expiry_string(expires_at),
    })))
}

// ---------------------------------------------------------------------------
// POST /v1/nostr-identities/verify
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct VerifyRequest {
    pub challenge_id: String,
    pub nonce: String,
    /// JSON serialization of the signed kind-24243 event.
    pub signed_payload: String,
}

fn fail(
    code: &'static str,
    message: &'static str,
) -> (axum::http::StatusCode, Json<serde_json::Value>) {
    ApiFailure::new(code, message).into_response_parts()
}

fn event_tag<'a>(event: &'a nostr::Event, name: &str) -> Option<&'a str> {
    event.tags.iter().find_map(|tag| {
        let parts = tag.as_slice();
        (parts.first().map(String::as_str) == Some(name))
            .then(|| parts.get(1).map(String::as_str))
            .flatten()
    })
}

pub async fn verify(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<VerifyRequest>,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;

    let challenge_id = Uuid::parse_str(req.challenge_id.trim())
        .map_err(|_| fail("invalid_challenge", "Malformed challenge id."))?;

    // Consume the challenge atomically. Expired, already-used, or
    // other-account rows all look identical to the caller: one generic error,
    // so verify cannot be used to probe challenge state.
    let row: Option<(String,)> = sqlx::query_as(
        "UPDATE identity_challenges SET consumed_at = now() \
         WHERE id = $1 AND account_id = $2 AND consumed_at IS NULL AND expires_at > now() \
         RETURNING nonce",
    )
    .bind(challenge_id)
    .bind(user.account_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| {
        tracing::error!(%error, "identity: challenge consume failed");
        ApiFailure::internal().into_response_parts()
    })?;
    let (stored,) = row.ok_or_else(|| {
        fail(
            "invalid_challenge",
            "Challenge not found, expired, or already used.",
        )
    })?;

    let (stored_nonce, stored_payload) = stored
        .split_once('|')
        .ok_or_else(|| ApiFailure::internal().into_response_parts())?;
    let payload: serde_json::Value = serde_json::from_str(stored_payload)
        .map_err(|_| ApiFailure::internal().into_response_parts())?;
    let expected = |key: &str| payload[key].as_str().unwrap_or_default().to_string();

    if req.nonce != stored_nonce {
        return Err(fail("invalid_challenge", "Nonce mismatch."));
    }

    // Parse and cryptographically verify the event itself.
    let event = nostr::Event::from_json(&req.signed_payload)
        .map_err(|_| fail("invalid_signature", "Malformed signed payload."))?;
    event
        .verify()
        .map_err(|_| fail("invalid_signature", "Signature verification failed."))?;
    if event.kind.as_u16() as u32 != BINDING_KIND {
        return Err(fail("invalid_signature", "Wrong event kind."));
    }

    // Every tag must match what *we* issued — not what the caller claims.
    let checks: [(&str, String); 8] = [
        ("challenge_id", challenge_id.to_string()),
        ("nonce", stored_nonce.to_string()),
        ("verification_code", expected("verification_code")),
        ("origin", expected("origin")),
        ("expires_at", expected("expires_at")),
        ("audience", AUDIENCE.to_string()),
        ("action", ACTION.to_string()),
        ("protocol", PROTOCOL.to_string()),
    ];
    for (tag, want) in &checks {
        if event_tag(&event, tag) != Some(want.as_str()) {
            return Err(fail(
                "invalid_signature",
                "Signed payload does not match the challenge.",
            ));
        }
    }
    if event_tag(&event, "version") != Some(VERSION) {
        return Err(fail(
            "invalid_signature",
            "Unsupported binding protocol version.",
        ));
    }

    let pubkey_hex = event.pubkey.to_hex();

    // Cardinality: one identity per account…
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT pubkey FROM nostr_identities WHERE account_id = $1")
            .bind(user.account_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| ApiFailure::internal().into_response_parts())?;
    if let Some((bound,)) = existing {
        if bound == pubkey_hex {
            // Re-binding the same key is a no-op success.
            return current_identity_response(&state, user.account_id).await;
        }
        return Err(fail(
            "identity_already_bound",
            "This account is connected to another identity.",
        ));
    }

    // …and one account per pubkey.
    match sqlx::query("INSERT INTO nostr_identities (account_id, pubkey) VALUES ($1, $2)")
        .bind(user.account_id)
        .bind(&pubkey_hex)
        .execute(&state.db)
        .await
    {
        Ok(_) => {}
        Err(sqlx::Error::Database(db)) if db.is_unique_violation() => {
            return Err(fail(
                "pubkey_already_bound",
                "This identity is connected to another account.",
            ));
        }
        Err(error) => {
            tracing::error!(%error, "identity: bind insert failed");
            return Err(ApiFailure::internal().into_response_parts());
        }
    }

    current_identity_response(&state, user.account_id).await
}

// ---------------------------------------------------------------------------
// POST /v1/nostr-identities/current
// ---------------------------------------------------------------------------

async fn current_identity_response(
    state: &AppState,
    account_id: Uuid,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let row: Option<(String,)> =
        sqlx::query_as("SELECT pubkey FROM nostr_identities WHERE account_id = $1")
            .bind(account_id)
            .fetch_optional(&state.db)
            .await
            .map_err(|_| ApiFailure::internal().into_response_parts())?;

    let identity = row.map(|(hex,)| {
        let npub = nostr::PublicKey::from_hex(&hex)
            .ok()
            .and_then(|pk| pk.to_bech32().ok());
        serde_json::json!({ "pubkey_hex": hex, "npub": npub })
    });

    Ok(Json(match identity {
        Some(identity) => serde_json::json!({ "identity": identity }),
        // No binding yet is a state, not an error; `setup_needed` tells the
        // desktop to render the connect flow rather than an error banner.
        None => serde_json::json!({
            "identity": null,
            "error": { "code": "missing_mapping", "setup_needed": true }
        }),
    }))
}

pub async fn current(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    current_identity_response(&state, user.account_id).await
}

// ---------------------------------------------------------------------------
// POST /v1/nostr-identities/delete
// ---------------------------------------------------------------------------

pub async fn delete(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, (axum::http::StatusCode, Json<serde_json::Value>)> {
    let user = session_or_unauthorized(state.as_ref(), &headers).await?;
    sqlx::query("DELETE FROM nostr_identities WHERE account_id = $1")
        .bind(user.account_id)
        .execute(&state.db)
        .await
        .map_err(|error| {
            tracing::error!(%error, "identity: delete failed");
            ApiFailure::internal().into_response_parts()
        })?;
    // Idempotent: deleting an absent binding is success, and the response is
    // the same "not bound" shape `current` returns.
    Ok(Json(serde_json::json!({
        "identity": null,
        "error": { "code": "missing_mapping", "setup_needed": true }
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr::{EventBuilder, FromBech32 as _, JsonUtil, Keys, Kind, Tag};

    fn signed_binding_event(
        keys: &Keys,
        challenge_id: &str,
        nonce: &str,
        code: &str,
        origin: &str,
        expires_at: &str,
    ) -> nostr::Event {
        let tags: Vec<Tag> = [
            ("challenge_id", challenge_id),
            ("nonce", nonce),
            ("verification_code", code),
            ("audience", AUDIENCE),
            ("action", ACTION),
            ("protocol", PROTOCOL),
            ("version", VERSION),
            ("origin", origin),
            ("expires_at", expires_at),
        ]
        .iter()
        .map(|(k, v)| Tag::parse([*k, *v]).unwrap())
        .collect();
        EventBuilder::new(Kind::Custom(BINDING_KIND as u16), "")
            .tags(tags)
            .sign_with_keys(keys)
            .unwrap()
    }

    #[test]
    fn binding_event_round_trips_and_verifies() {
        let keys = Keys::generate();
        let event = signed_binding_event(
            &keys,
            "550e8400-e29b-41d4-a716-446655440000",
            "abc",
            "123456",
            "http://localhost:4000",
            "2999-01-01T00:00:00Z",
        );
        let parsed = nostr::Event::from_json(event.as_json()).unwrap();
        parsed.verify().expect("schnorr verification");
        assert_eq!(parsed.kind.as_u16() as u32, BINDING_KIND);
        assert_eq!(event_tag(&parsed, "audience"), Some(AUDIENCE));
        assert_eq!(event_tag(&parsed, "verification_code"), Some("123456"));
    }

    #[test]
    fn tampered_signature_fails_verification() {
        let keys = Keys::generate();
        let event = signed_binding_event(
            &keys,
            "id",
            "n",
            "000000",
            "http://x",
            "2999-01-01T00:00:00Z",
        );
        // Flip the content after signing: the id/signature no longer match.
        let mut json: serde_json::Value = serde_json::from_str(&event.as_json()).unwrap();
        json["content"] = serde_json::json!("tampered");
        let parsed = nostr::Event::from_json(json.to_string());
        assert!(
            parsed.map(|e| e.verify().is_err()).unwrap_or(true),
            "a tampered event must not verify"
        );
    }

    #[test]
    fn kind_matches_the_desktop_constant() {
        // desktop/src-tauri/src/nostr_bind.rs pins KIND to buzz-core's
        // KIND_NOSTR_IDENTITY_BINDING; drift here breaks every binding.
        assert_eq!(BINDING_KIND, 24243);
    }

    // ---- Client-compatibility guards -------------------------------------
    //
    // These mirror desktop/src-tauri/src/nostr_bind.rs exactly. The desktop
    // validates a challenge BEFORE signing it, so a server-side value it
    // rejects never reaches the signature — the user just sees an error. A
    // round-trip test against our own verifier cannot catch that, which is how
    // the original "invalid nonce" shipped.

    /// Copy of `nostr_bind::validate_nonce`.
    fn desktop_accepts_nonce(nonce: &str) -> bool {
        const NONCE_CHARS: &str =
            "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789_-";
        !nonce.is_empty() && nonce.len() == 43 && nonce.chars().all(|c| NONCE_CHARS.contains(c))
    }

    /// Copy of `nostr_bind::validate_origin`.
    fn desktop_accepts_origin(origin: &str) -> bool {
        match url::Url::parse(origin) {
            Ok(u) => {
                u.scheme() == "https"
                    && u.host_str().is_some()
                    && u.username().is_empty()
                    && u.password().is_none()
                    && u.path() == "/"
                    && u.query().is_none()
                    && u.fragment().is_none()
            }
            Err(_) => false,
        }
    }

    #[test]
    fn generated_nonce_passes_the_desktop_validator() {
        for _ in 0..32 {
            let nonce = binding_nonce();
            assert!(
                desktop_accepts_nonce(&nonce),
                "desktop would reject nonce {nonce:?} (len {})",
                nonce.len()
            );
        }
    }

    #[test]
    fn a_uuid_nonce_would_be_rejected_by_the_desktop() {
        // Documents the original bug: 32 hex chars, not 43 base64url.
        let uuid_nonce = Uuid::new_v4().simple().to_string();
        assert!(!desktop_accepts_nonce(&uuid_nonce));
    }

    #[test]
    fn default_origin_passes_the_desktop_validator() {
        assert!(desktop_accepts_origin("https://accounts.hireshelby.com"));
    }

    #[test]
    fn http_and_pathful_origins_would_be_rejected() {
        // The second case is what the desktop sent us in local dev.
        for origin in [
            "http://localhost:4000",
            "http://accounts.hireshelby.com",
            "https://accounts.hireshelby.com/api",
            "https://user:pw@accounts.hireshelby.com",
            "https://accounts.hireshelby.com/?x=1",
        ] {
            assert!(
                !desktop_accepts_origin(origin),
                "{origin} should be rejected client-side"
            );
        }
    }

    #[test]
    fn verification_codes_are_six_digits() {
        for _ in 0..64 {
            let code = verification_code();
            assert_eq!(code.len(), 6);
            assert!(code.chars().all(|c| c.is_ascii_digit()));
        }
    }

    #[test]
    fn npub_encoding_matches_the_pubkey() {
        let keys = Keys::generate();
        let npub = keys.public_key().to_bech32().unwrap();
        assert!(npub.starts_with("npub1"));
        let round = nostr::PublicKey::from_bech32(&npub).unwrap();
        assert_eq!(round.to_hex(), keys.public_key().to_hex());
    }
}
