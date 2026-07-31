use std::{collections::HashMap, sync::Mutex, time::Duration};

use axum::{
    extract::{Path, Query, State as AxumState},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
    routing::get,
    Router,
};
use serde::{Deserialize, Serialize};
use tauri_plugin_opener::OpenerExt;
use tokio::{net::TcpListener, sync::oneshot};
use url::Url;

/// Default HireShelby control plane (`crates/hireshelby-accounts`).
///
/// Overridable via `HIRESHELBY_ACCOUNTS_URL` so dev, staging, and production
/// builds can point at different control planes without a recompile.
const DEFAULT_ACCOUNTS_BASE_URL: &str = "https://accounts.hireshelby.com";
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
const SESSION_CREDENTIAL_HEADER: &str = "X-HireShelby-Session";

/// Base URL of the control plane for this run.
fn accounts_base_url() -> String {
    std::env::var("HIRESHELBY_ACCOUNTS_URL")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_ACCOUNTS_BASE_URL.to_string())
}

/// Origin sent on identity-bind requests. The control plane checks it, and the
/// challenge body's `origin` field must agree, so both are derived from the
/// same base URL rather than hardcoded separately.
fn accounts_origin() -> String {
    accounts_base_url()
}
const AUTH_COMPLETE_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>HireShelby authentication complete</title>
  <style>
    :root {
      color-scheme: light;
      font-family: ui-sans-serif, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      color: #0d1117;
      background: #0d1117;
    }

    * {
      box-sizing: border-box;
    }

    body {
      min-height: 100vh;
      min-height: 100dvh;
      margin: 0;
      display: grid;
      place-items: center;
      padding: 24px;
      background-color: #0d1117;
      background-image: radial-gradient(circle, rgba(35, 30, 30, 0.16) 1.2px, transparent 1.3px);
      background-size: 37px 37px;
    }

    main {
      width: min(100%, 560px);
      padding: clamp(32px, 8vw, 64px);
      border: 2px solid #0d1117;
      border-radius: 28px;
      background: #f7f9fc;
      box-shadow: 8px 8px 0 #0d1117;
    }

    .mark {
      display: block;
      width: 72px;
      height: auto;
      margin-bottom: 40px;
      color: #0d1117;
    }

    .eyebrow {
      display: inline-flex;
      align-items: center;
      min-height: 32px;
      margin: 0 0 20px;
      padding: 6px 14px;
      border-radius: 999px;
      background: #0d1117;
      font-size: 14px;
      font-weight: 600;
      letter-spacing: 0.01em;
    }

    h1 {
      max-width: 440px;
      margin: 0;
      font-size: clamp(40px, 9vw, 64px);
      font-weight: 600;
      letter-spacing: -0.055em;
      line-height: 0.95;
    }

    p {
      max-width: 390px;
      margin: 24px 0 0;
      font-size: 18px;
      letter-spacing: -0.02em;
      line-height: 1.45;
    }

    @media (max-width: 480px) {
      body {
        padding: 16px;
      }

      main {
        padding: 32px 28px 36px;
        border-radius: 22px;
        box-shadow: 6px 6px 0 #0d1117;
      }

      .mark {
        width: 60px;
        margin-bottom: 32px;
      }
    }
  </style>
</head>
<body>
  <main>
    <svg class="mark" viewBox="0 0 64 64" role="img" aria-label="HireShelby">
      <rect width="64" height="64" rx="14" fill="currentColor"/>
      <text x="32" y="44" text-anchor="middle"
            font-family="ui-sans-serif, -apple-system, sans-serif"
            font-size="34" font-weight="700" fill='#f0f4f8'>HS</text>
    </svg>
    <div class="eyebrow">Authentication complete</div>
    <h1>You&rsquo;re signed in.</h1>
    <p>You can close this window and return to HireShelby.</p>
  </main>
</body>
</html>"#;

#[derive(Default)]
pub(crate) struct AccountsSession(Mutex<Option<StoredSession>>);

#[derive(Default)]
pub(crate) struct AccountsLogin(Mutex<Option<PendingLogin>>);

struct PendingLogin {
    id: uuid::Uuid,
    cancel: oneshot::Sender<()>,
}

struct StoredSession {
    credential: String,
}

#[derive(Debug, Deserialize)]
struct LoginExchangeResponse {
    session_credential: String,
    expires_at: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AccountsAuthInfo {
    expires_at: String,
    email: Option<String>,
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AuthMeResponse {
    email: Option<String>,
    name: Option<String>,
    expires_at: String,
}

struct CallbackState {
    nonce: String,
    sender: Mutex<Option<oneshot::Sender<Result<String, String>>>>,
}

async fn login_callback(
    Path(nonce): Path<String>,
    Query(query): Query<HashMap<String, String>>,
    AxumState(state): AxumState<std::sync::Arc<CallbackState>>,
) -> Response {
    if nonce != state.nonce {
        return (StatusCode::NOT_FOUND, "Not found").into_response();
    }

    let result = match query.get("code").filter(|code| !code.is_empty()) {
        Some(code) => Ok(code.clone()),
        None => Err(query
            .get("error_description")
            .or_else(|| query.get("error"))
            .cloned()
            .unwrap_or_else(|| "Authentication callback did not include a code".to_owned())),
    };
    if let Some(sender) = state
        .sender
        .lock()
        .expect("callback sender poisoned")
        .take()
    {
        let _ = sender.send(result);
    }

    Html(AUTH_COMPLETE_HTML).into_response()
}

fn api_url(path: &str) -> Result<Url, String> {
    Url::parse(&format!("{}{path}", accounts_base_url()))
        .map_err(|error| format!("invalid Accounts API URL: {error}"))
}

fn login_url(return_to: &str) -> Result<Url, String> {
    let mut login_url = api_url("/v1/auth/login")?;
    login_url
        .query_pairs_mut()
        .append_pair("type", "cli")
        .append_pair("product", "hireshelby")
        .append_pair("returnTo", return_to);
    Ok(login_url)
}

async fn authenticated_user(
    client: &reqwest::Client,
    credential: &str,
) -> Result<AuthMeResponse, String> {
    let response = client
        .get(api_url("/v1/auth/me")?)
        .header(SESSION_CREDENTIAL_HEADER, credential)
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| format!("Accounts session check failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Accounts session check failed with HTTP {}",
            response.status()
        ));
    }
    response
        .json()
        .await
        .map_err(|error| format!("invalid Accounts session response: {error}"))
}

#[tauri::command]
pub(crate) async fn start_accounts_login(
    app: tauri::AppHandle,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
    login: tauri::State<'_, AccountsLogin>,
) -> Result<AccountsAuthInfo, String> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|error| format!("could not start local authentication callback: {error}"))?;
    let port = listener
        .local_addr()
        .map_err(|error| format!("could not read local authentication callback: {error}"))?
        .port();
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let return_to = format!("http://127.0.0.1:{port}/callback/{nonce}");
    let (sender, receiver) = oneshot::channel();
    let callback_state = std::sync::Arc::new(CallbackState {
        nonce: nonce.clone(),
        sender: Mutex::new(Some(sender)),
    });
    let router = Router::new()
        .route("/callback/{nonce}", get(login_callback))
        .with_state(callback_state);
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let login_url = login_url(&return_to)?;
    if let Err(error) = app.opener().open_url(login_url.as_str(), None::<&str>) {
        server.abort();
        return Err(format!("could not open Accounts authentication: {error}"));
    }

    let login_id = uuid::Uuid::new_v4();
    let (cancel_sender, mut cancel_receiver) = oneshot::channel();
    {
        let mut pending = login.0.lock().map_err(|error| error.to_string())?;
        if let Some(previous) = pending.take() {
            let _ = previous.cancel.send(());
        }
        *pending = Some(PendingLogin {
            id: login_id,
            cancel: cancel_sender,
        });
    }

    let exchange_code = tokio::select! {
        result = tokio::time::timeout(LOGIN_TIMEOUT, receiver) => match result {
            Ok(Ok(Ok(code))) => code,
            Ok(Ok(Err(error))) => {
                server.abort();
                return Err(error);
            }
            Ok(Err(_)) => {
                server.abort();
                return Err("local authentication callback stopped unexpectedly".to_owned());
            }
            Err(_) => {
                server.abort();
                return Err("Accounts authentication timed out".to_owned());
            }
        },
        _ = &mut cancel_receiver => {
            server.abort();
            return Err("Accounts authentication canceled".to_owned());
        }
    };
    server.abort();

    let response = app_state
        .http_client
        .post(api_url("/v1/auth/login/exchange")?)
        .json(&serde_json::json!({ "code": exchange_code }))
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|error| format!("Accounts code exchange failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Accounts code exchange failed with HTTP {}",
            response.status()
        ));
    }
    let exchanged: LoginExchangeResponse = response
        .json()
        .await
        .map_err(|error| format!("invalid Accounts code exchange response: {error}"))?;
    if exchanged.session_credential.is_empty() {
        return Err("Accounts code exchange returned an empty credential".to_owned());
    }

    let me = authenticated_user(&app_state.http_client, &exchanged.session_credential).await?;
    if exchanged.expires_at != me.expires_at {
        return Err("Accounts session expiry did not match code exchange".to_owned());
    }
    let info = AccountsAuthInfo {
        expires_at: me.expires_at.clone(),
        email: me.email,
        name: me.name,
    };
    {
        let mut pending = login.0.lock().map_err(|error| error.to_string())?;
        if pending
            .as_ref()
            .is_none_or(|pending| pending.id != login_id)
        {
            return Err("Accounts authentication canceled".to_owned());
        }
        *pending = None;
    }
    *session.0.lock().map_err(|error| error.to_string())? = Some(StoredSession {
        credential: exchanged.session_credential,
    });
    Ok(info)
}

#[tauri::command]
pub(crate) async fn get_accounts_auth(
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<Option<AccountsAuthInfo>, String> {
    let stored = session
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
        .map(|stored| stored.credential.clone());
    let Some(credential) = stored else {
        return Ok(None);
    };
    match authenticated_user(&app_state.http_client, &credential).await {
        Ok(me) => Ok(Some(AccountsAuthInfo {
            expires_at: me.expires_at,
            email: me.email,
            name: me.name,
        })),
        Err(error) => {
            *session
                .0
                .lock()
                .map_err(|lock_error| lock_error.to_string())? = None;
            Err(error)
        }
    }
}

#[tauri::command]
pub(crate) fn cancel_accounts_login(login: tauri::State<'_, AccountsLogin>) -> Result<(), String> {
    if let Some(pending) = login.0.lock().map_err(|error| error.to_string())?.take() {
        let _ = pending.cancel.send(());
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn clear_accounts_auth(
    session: tauri::State<'_, AccountsSession>,
) -> Result<(), String> {
    *session.0.lock().map_err(|error| error.to_string())? = None;
    Ok(())
}

#[derive(Debug, Deserialize)]
struct NostrIdentityChallenge {
    challenge_id: String,
    nonce: String,
    verification_code: String,
    origin: String,
    expires_at: String,
}

async fn authenticated_json(
    client: &reqwest::Client,
    session: &AccountsSession,
    method: reqwest::Method,
    path: &str,
    body: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let credential = session
        .0
        .lock()
        .map_err(|error| error.to_string())?
        .as_ref()
        .map(|stored| stored.credential.clone())
        .ok_or_else(|| "Sign in to Accounts first".to_owned())?;
    let response = client
        .request(method, api_url(path)?)
        .header(SESSION_CREDENTIAL_HEADER, credential)
        .header(reqwest::header::ORIGIN, &accounts_origin())
        .json(&body)
        .timeout(Duration::from_secs(60))
        .send()
        .await
        .map_err(|error| format!("Accounts request failed: {error}"))?;
    let status = response.status();
    let value: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("invalid Accounts response: {error}"))?;
    if !status.is_success() {
        // Accounts error responses carry a structured `{ error: { code,
        // message, setup_needed, ... } }` body. Pass those through as `Ok` so the
        // frontend's typed handling and friendly per-code messages apply, instead
        // of surfacing a raw JSON blob. Only fall back to a plain string when the
        // body isn't the expected shape.
        if value.get("error").is_some() {
            return Ok(value);
        }
        return Err(format!("Accounts request failed (HTTP {status})."));
    }
    Ok(value)
}

#[tauri::command]
pub(crate) async fn get_accounts_nostr_identity(
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/nostr-identities/current",
        serde_json::json!({}),
    )
    .await
}

#[tauri::command]
pub(crate) async fn bind_accounts_nostr_identity(
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    let challenge_value = authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/nostr-identities/challenge",
        serde_json::json!({ "origin": &accounts_origin() }),
    )
    .await?;
    // A structured error here (e.g. missing_mapping) arrives as an object with an
    // `error` field rather than a challenge — hand it straight back so the
    // frontend maps it to a friendly message instead of hitting a deserialize
    // failure below.
    if challenge_value.get("error").is_some() {
        return Ok(challenge_value);
    }
    let challenge: NostrIdentityChallenge = serde_json::from_value(challenge_value)
        .map_err(|error| format!("invalid Nostr identity challenge: {error}"))?;
    let keys = app_state.signing_keys()?;
    let event = crate::commands::build_nostr_identity_binding_event(
        &keys,
        &challenge.challenge_id,
        &challenge.nonce,
        &challenge.verification_code,
        &challenge.origin,
        &challenge.expires_at,
    )?;
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/nostr-identities/verify",
        serde_json::json!({
            "challenge_id": challenge.challenge_id,
            "nonce": challenge.nonce,
            "signed_payload": nostr::JsonUtil::as_json(&event),
        }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn delete_accounts_nostr_identity(
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/nostr-identities/delete",
        serde_json::json!({}),
    )
    .await
}

#[tauri::command]
pub(crate) async fn list_accounts_communities(
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities/list",
        serde_json::json!({}),
    )
    .await
}

#[tauri::command]
pub(crate) async fn check_accounts_community_name(
    name: String,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities/availability",
        serde_json::json!({ "name": name }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn create_accounts_community(
    name: String,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities",
        serde_json::json!({ "name": name }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn archive_accounts_community(
    community_id: String,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities/archive",
        serde_json::json!({ "community_id": community_id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn unarchive_accounts_community(
    community_id: String,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities/unarchive",
        serde_json::json!({ "community_id": community_id }),
    )
    .await
}

#[tauri::command]
pub(crate) async fn transfer_accounts_community(
    community_id: String,
    transferee_npub: String,
    app_state: tauri::State<'_, crate::app_state::AppState>,
    session: tauri::State<'_, AccountsSession>,
) -> Result<serde_json::Value, String> {
    // The Accounts transfer endpoint expects camelCase keys, unlike the
    // archive/unarchive endpoints which take `community_id`; mirror the web
    // client's payload exactly.
    authenticated_json(
        &app_state.http_client,
        &session,
        reqwest::Method::POST,
        "/v1/communities/transfer",
        serde_json::json!({
            "communityId": community_id,
            "transfereeNpub": transferee_npub,
        }),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_complete_page_uses_buzz_brand() {
        for expected in [
            "<title>HireShelby authentication complete</title>",
            "#0d1117",
            "#0d1117",
            "#f7f9fc",
            "aria-label=\"HireShelby\"",
            "return to HireShelby",
        ] {
            assert!(
                AUTH_COMPLETE_HTML.contains(expected),
                "authentication complete page is missing {expected}"
            );
        }
    }

    #[test]
    fn api_paths_stay_on_accounts_api_origin() {
        let login = api_url("/v1/auth/login").unwrap();
        assert_eq!(
            login.origin().ascii_serialization(),
            "https://app.accounts.xyz"
        );
        assert_eq!(login.path(), "/api/goose/v1/auth/login");
    }

    #[test]
    fn login_defaults_to_auth0_login() {
        let login = login_url("http://127.0.0.1:1234/callback/nonce").unwrap();
        let query: HashMap<_, _> = login.query_pairs().into_owned().collect();

        assert_eq!(query.get("type").map(String::as_str), Some("cli"));
        assert_eq!(query.get("product").map(String::as_str), Some("buzz"));
        assert_eq!(
            query.get("returnTo").map(String::as_str),
            Some("http://127.0.0.1:1234/callback/nonce")
        );
        assert!(!query.contains_key("screen_hint"));
    }
}
