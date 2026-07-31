//! Login, sessions, and the desktop loopback auth flow.
//!
//! ## The flow
//!
//! 1. Desktop binds a loopback listener and opens
//!    `GET /v1/auth/login?returnTo=http://127.0.0.1:<port>/callback/<nonce>`
//!    in the system browser.
//! 2. The user authenticates. We redirect back to `returnTo?code=<code>`.
//! 3. Desktop `POST /v1/auth/login/exchange` with the code, over HTTPS, and
//!    receives `{session_credential, expires_at}`.
//! 4. Desktop calls `GET /v1/auth/me` with `X-HireShelby-Session`.
//!
//! **The desktop client asserts that `expires_at` from step 3 is byte-identical
//! to `expires_at` from step 4.** Both therefore render the same stored
//! timestamp through [`format_expiry`]; they must not be computed separately.
//!
//! ## Credentials are stored hashed
//!
//! Login codes and session credentials are random 256-bit values. Only their
//! SHA-256 digests are persisted, so a database disclosure does not yield
//! usable credentials. They are compared by looking up the digest, which is
//! also what makes the single-use check on codes atomic.

use std::sync::Arc;

use axum::{
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Json,
};
use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::AppState;

/// Header the desktop client presents its session under.
pub const SESSION_HEADER: &str = "x-hireshelby-session";

/// Authorization codes are exchanged within seconds; a tight window limits the
/// blast radius of a code leaking through browser history or a redirect log.
const CODE_TTL_MINUTES: i64 = 5;
/// Desktop sessions are long-lived so users are not re-prompted constantly.
const SESSION_TTL_DAYS: i64 = 30;

/// Renders a timestamp for the wire.
///
/// Both `/login/exchange` and `/me` must produce identical strings for the same
/// session — the desktop client compares them and aborts login on a mismatch.
/// Formatting in one place is what guarantees that.
fn format_expiry(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// 256 bits of randomness, URL-safe. Used for both codes and session
/// credentials; a code lands in a redirect URL, so it must not need escaping.
fn generate_secret() -> String {
    // `rand::random()` over a fixed-size array draws from the OS CSPRNG, and is
    // the pattern used elsewhere in this workspace (see buzz-auth/nip42.rs).
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn hash_secret(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("auth: {0}")]
    Db(#[from] sqlx::Error),
    #[error("auth: invalid or expired code")]
    InvalidCode,
    #[error("auth: invalid or expired session")]
    InvalidSession,
    #[error("auth: {0}")]
    BadRequest(String),
    #[error("auth: developer login is disabled")]
    DevLoginDisabled,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let status = match self {
            AuthError::InvalidCode | AuthError::InvalidSession => StatusCode::UNAUTHORIZED,
            AuthError::BadRequest(_) => StatusCode::BAD_REQUEST,
            AuthError::DevLoginDisabled => StatusCode::NOT_FOUND,
            AuthError::Db(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        // Database detail is logged, never returned: it can leak schema and
        // connection information to an unauthenticated caller.
        let message = match self {
            AuthError::Db(ref error) => {
                tracing::error!(%error, "auth: database failure");
                "internal error".to_string()
            }
            ref other => other.to_string(),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

pub struct SessionUser {
    pub account_id: Uuid,
    pub email: String,
    pub name: Option<String>,
    pub expires_at: DateTime<Utc>,
}

/// Resolves the `X-HireShelby-Session` header to a live account.
///
/// Expired and revoked sessions are rejected in SQL rather than in Rust so the
/// check cannot be skipped by a caller that forgets to compare timestamps.
pub async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<SessionUser, AuthError> {
    let credential = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .ok_or(AuthError::InvalidSession)?;

    let row: Option<(Uuid, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT a.id, a.email, a.name, s.expires_at \
         FROM sessions s JOIN accounts a ON a.id = s.account_id \
         WHERE s.credential_hash = $1 AND s.revoked_at IS NULL AND s.expires_at > now()",
    )
    .bind(hash_secret(credential))
    .fetch_optional(&state.db)
    .await?;

    let (account_id, email, name, expires_at) = row.ok_or(AuthError::InvalidSession)?;
    Ok(SessionUser {
        account_id,
        email,
        name,
        expires_at,
    })
}

// ---------------------------------------------------------------------------
// GET /v1/auth/login
// ---------------------------------------------------------------------------

/// The client also sends `type=cli` and `product=hireshelby`. Neither affects
/// behaviour, and `Query` ignores unknown parameters, so they are not declared.
#[derive(Debug, Deserialize)]
pub struct LoginQuery {
    #[serde(rename = "returnTo")]
    pub return_to: String,
}

/// Only loopback callbacks are accepted.
///
/// `returnTo` is attacker-controllable via a crafted link, and we redirect to
/// it carrying a login code. Allowing an arbitrary host would hand that code to
/// whoever asked — a full account takeover. The desktop client always binds
/// 127.0.0.1, so restricting to loopback costs nothing.
fn validate_return_to(return_to: &str) -> Result<url::Url, AuthError> {
    let parsed = url::Url::parse(return_to)
        .map_err(|e| AuthError::BadRequest(format!("returnTo is not a valid URL: {e}")))?;
    if parsed.scheme() != "http" {
        return Err(AuthError::BadRequest(
            "returnTo must use http on loopback".into(),
        ));
    }
    match parsed.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") => Ok(parsed),
        _ => Err(AuthError::BadRequest(
            "returnTo must point at a loopback address".into(),
        )),
    }
}

/// Starts login in the browser.
///
/// With WorkOS configured this redirects to the hosted AuthKit page. In
/// developer mode it renders a local sign-in form instead, so the whole flow is
/// exercisable without external credentials.
pub async fn login(
    State(state): State<Arc<AppState>>,
    Query(query): Query<LoginQuery>,
) -> Result<Response, AuthError> {
    let return_to = validate_return_to(&query.return_to)?;

    if let Some(authorize_url) = state.config.workos_authorize_url(return_to.as_str()) {
        return Ok(Redirect::to(&authorize_url).into_response());
    }

    if !state.config.dev_login_enabled {
        return Err(AuthError::DevLoginDisabled);
    }
    Ok(Html(dev_login_page(return_to.as_str())).into_response())
}

fn dev_login_page(return_to: &str) -> String {
    // Deliberately plain: this exists only for local development and must never
    // be mistaken for the production sign-in experience.
    format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<title>HireShelby developer sign-in</title>
<style>
  body {{ font-family: ui-sans-serif, system-ui, sans-serif; background:#0d1117; color:#f0f4f8;
         display:grid; place-items:center; min-height:100vh; margin:0; }}
  form {{ background:#161c26; padding:32px; border-radius:16px; width:min(100%,420px); }}
  h1 {{ font-size:20px; margin:0 0 4px; }}
  p {{ color:#96a3b4; font-size:14px; margin:0 0 20px; }}
  input {{ width:100%; padding:10px 12px; border-radius:8px; border:1px solid #2d3748;
           background:#0d1117; color:#f0f4f8; font-size:15px; margin-bottom:12px; }}
  button {{ width:100%; padding:10px; border-radius:8px; border:0; background:#63b3ed;
            color:#0d1117; font-weight:600; font-size:15px; cursor:pointer; }}
  .warn {{ margin-top:16px; font-size:12px; color:#f6ad55; }}
</style></head>
<body>
  <form method="post" action="/v1/auth/dev-login">
    <h1>Developer sign-in</h1>
    <p>Local development only.</p>
    <input type="hidden" name="return_to" value="{return_to}">
    <input name="email" type="email" placeholder="you@example.com" required autofocus>
    <input name="name" type="text" placeholder="Display name (optional)">
    <button type="submit">Sign in</button>
    <div class="warn">HIRESHELBY_DEV_LOGIN is enabled. Never enable this in production.</div>
  </form>
</body></html>"#
    )
}

// ---------------------------------------------------------------------------
// POST /v1/auth/dev-login  (developer mode only)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct DevLoginForm {
    pub email: String,
    pub name: Option<String>,
    pub return_to: String,
}

/// Issues a login code for an arbitrary email, without proving ownership.
///
/// This is a complete authentication bypass by design, which is why it is gated
/// on an explicit opt-in and returns 404 otherwise — a disabled endpoint should
/// not even advertise that it exists.
pub async fn dev_login(
    State(state): State<Arc<AppState>>,
    axum::extract::Form(form): axum::extract::Form<DevLoginForm>,
) -> Result<Redirect, AuthError> {
    if !state.config.dev_login_enabled {
        return Err(AuthError::DevLoginDisabled);
    }
    let return_to = validate_return_to(&form.return_to)?;

    let email = form.email.trim().to_lowercase();
    if email.is_empty() {
        return Err(AuthError::BadRequest("email is required".into()));
    }
    let name = form.name.and_then(|n| {
        let n = n.trim().to_string();
        (!n.is_empty()).then_some(n)
    });

    let account_id = upsert_account(
        state.as_ref(),
        &email,
        name.as_deref(),
        &format!("dev:{email}"),
    )
    .await?;
    let code = mint_login_code(state.as_ref(), account_id, return_to.as_str()).await?;

    let mut redirect = return_to;
    redirect.query_pairs_mut().append_pair("code", &code);
    Ok(Redirect::to(redirect.as_str()))
}

/// Finds or creates the account for an authenticated email.
///
/// The conflict target is the case-insensitive email index, not `external_id`.
/// Email is the human's identity; `external_id` only records which provider
/// identity last signed in. Keying on `external_id` breaks the moment the same
/// person arrives through a second provider — or the same provider reissues an
/// id — because the row would collide on the email index instead, which is a
/// 500 rather than a login.
async fn upsert_account(
    state: &AppState,
    email: &str,
    name: Option<&str>,
    external_id: &str,
) -> Result<Uuid, AuthError> {
    let (id,): (Uuid,) = sqlx::query_as(
        "INSERT INTO accounts (email, external_id, name) VALUES ($1, $2, $3) \
         ON CONFLICT (lower(email)) DO UPDATE \
           SET external_id = EXCLUDED.external_id, \
               name = COALESCE(EXCLUDED.name, accounts.name) \
         RETURNING id",
    )
    .bind(email)
    .bind(external_id)
    .bind(name)
    .fetch_one(&state.db)
    .await?;
    Ok(id)
}

async fn mint_login_code(
    state: &AppState,
    account_id: Uuid,
    return_to: &str,
) -> Result<String, AuthError> {
    let code = generate_secret();
    sqlx::query(
        "INSERT INTO login_codes (code_hash, account_id, return_to, expires_at) \
         VALUES ($1, $2, $3, now() + ($4 || ' minutes')::interval)",
    )
    .bind(hash_secret(&code))
    .bind(account_id)
    .bind(return_to)
    .bind(CODE_TTL_MINUTES.to_string())
    .execute(&state.db)
    .await?;
    Ok(code)
}

// ---------------------------------------------------------------------------
// POST /v1/auth/login/exchange
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ExchangeRequest {
    pub code: String,
}

#[derive(Debug, Serialize)]
pub struct ExchangeResponse {
    pub session_credential: String,
    pub expires_at: String,
}

/// Trades a login code for a session.
///
/// The code is consumed with a conditional UPDATE returning the account, so two
/// concurrent exchanges of the same code cannot both succeed — the second sees
/// zero rows. Doing this as SELECT-then-UPDATE would be a race.
pub async fn exchange(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ExchangeRequest>,
) -> Result<Json<ExchangeResponse>, AuthError> {
    let code = req.code.trim();
    if code.is_empty() {
        return Err(AuthError::BadRequest("code is required".into()));
    }

    let row: Option<(Uuid,)> = sqlx::query_as(
        "UPDATE login_codes SET consumed_at = now() \
         WHERE code_hash = $1 AND consumed_at IS NULL AND expires_at > now() \
         RETURNING account_id",
    )
    .bind(hash_secret(code))
    .fetch_optional(&state.db)
    .await?;
    let (account_id,) = row.ok_or(AuthError::InvalidCode)?;

    let credential = generate_secret();
    let expires_at = Utc::now() + Duration::days(SESSION_TTL_DAYS);
    sqlx::query(
        "INSERT INTO sessions (credential_hash, account_id, expires_at) VALUES ($1, $2, $3)",
    )
    .bind(hash_secret(&credential))
    .bind(account_id)
    .bind(expires_at)
    .execute(&state.db)
    .await?;

    // Read the stored value back rather than reusing the local one: Postgres
    // may round the timestamp, and /me will serve whatever was stored. The
    // desktop client compares the two strings exactly.
    let (stored,): (DateTime<Utc>,) =
        sqlx::query_as("SELECT expires_at FROM sessions WHERE credential_hash = $1")
            .bind(hash_secret(&credential))
            .fetch_one(&state.db)
            .await?;

    Ok(Json(ExchangeResponse {
        session_credential: credential,
        expires_at: format_expiry(stored),
    }))
}

// ---------------------------------------------------------------------------
// GET /v1/auth/me
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct MeResponse {
    pub email: Option<String>,
    pub name: Option<String>,
    pub expires_at: String,
}

pub async fn me(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<MeResponse>, AuthError> {
    let user = authenticate(state.as_ref(), &headers).await?;
    Ok(Json(MeResponse {
        email: Some(user.email),
        name: user.name,
        expires_at: format_expiry(user.expires_at),
    }))
}

// ---------------------------------------------------------------------------
// POST /v1/auth/logout
// ---------------------------------------------------------------------------

pub async fn logout(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<StatusCode, AuthError> {
    if let Some(credential) = headers
        .get(SESSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        sqlx::query("UPDATE sessions SET revoked_at = now() WHERE credential_hash = $1 AND revoked_at IS NULL")
            .bind(hash_secret(credential))
            .execute(&state.db)
            .await?;
    }
    // Always 204: revealing whether a credential existed would let a caller
    // probe for valid sessions.
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_are_256_bit_and_url_safe() {
        let secret = generate_secret();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&secret)
            .expect("url-safe base64");
        assert_eq!(decoded.len(), 32, "expected 256 bits of entropy");
        assert!(
            !secret.contains('+') && !secret.contains('/') && !secret.contains('='),
            "code travels in a redirect query string and must not need escaping: {secret}"
        );
    }

    #[test]
    fn secrets_do_not_repeat() {
        let a = generate_secret();
        let b = generate_secret();
        assert_ne!(a, b);
    }

    #[test]
    fn hashing_is_stable_and_hides_the_secret() {
        let secret = generate_secret();
        assert_eq!(hash_secret(&secret), hash_secret(&secret));
        assert!(!hash_secret(&secret).contains(&secret));
        assert_eq!(hash_secret(&secret).len(), 64, "sha256 hex");
    }

    #[test]
    fn loopback_return_to_is_accepted() {
        for url in [
            "http://127.0.0.1:5551/callback/abc",
            "http://localhost:8080/callback/abc",
        ] {
            assert!(validate_return_to(url).is_ok(), "should accept {url}");
        }
    }

    #[test]
    fn non_loopback_return_to_is_rejected() {
        // Each of these would hand a login code to a third party, which is
        // account takeover. This is the highest-severity check in the module.
        for url in [
            "http://evil.example/callback",
            "https://evil.example/callback",
            "http://127.0.0.1.evil.example/callback",
            "http://[email protected]/callback",
            "not-a-url",
        ] {
            assert!(
                validate_return_to(url).is_err(),
                "must reject {url} — it would leak the login code"
            );
        }
    }

    #[test]
    fn expiry_format_is_stable_for_the_clients_equality_check() {
        // The desktop client aborts login when exchange and /me disagree, so
        // the same instant must always render identically.
        let at = DateTime::parse_from_rfc3339("2026-07-31T12:34:56.789Z")
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(format_expiry(at), format_expiry(at));
        assert_eq!(
            format_expiry(at),
            "2026-07-31T12:34:56Z",
            "sub-second precision must be dropped so round-tripping through Postgres cannot alter the string"
        );
    }

    #[test]
    fn disabled_dev_login_reports_not_found_rather_than_forbidden() {
        // 404 avoids advertising that a dev bypass exists on this deployment.
        let response = AuthError::DevLoginDisabled.into_response();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn bad_credentials_are_unauthorized() {
        assert_eq!(
            AuthError::InvalidCode.into_response().status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            AuthError::InvalidSession.into_response().status(),
            StatusCode::UNAUTHORIZED
        );
    }
}
