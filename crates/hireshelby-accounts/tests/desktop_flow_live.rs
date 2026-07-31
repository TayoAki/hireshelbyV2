//! Live end-to-end test of the full desktop client flow.
//!
//! Drives the exact HTTP sequence the desktop performs — dev-login, code
//! exchange, identity challenge, Schnorr-signed binding, community creation,
//! availability, archive/unarchive, list — against a running control plane and
//! relay. This is the test that says "a user could actually do this", which no
//! unit test can.
//!
//! Ignored by default. Run with:
//!
//! ```text
//! HIRESHELBY_TEST_BASE_URL=http://localhost:4000 \
//!   cargo test -p hireshelby-accounts --test desktop_flow_live -- --ignored --nocapture
//! ```
//!
//! The control plane must be started with HIRESHELBY_DEV_LOGIN=true and a
//! relay that allowlists its operator key.

use nostr::{EventBuilder, JsonUtil as _, Keys, Kind, Tag};

fn base_url() -> Option<String> {
    match std::env::var("HIRESHELBY_TEST_BASE_URL") {
        Ok(v) if !v.trim().is_empty() => Some(v.trim_end_matches('/').to_string()),
        _ => {
            eprintln!("skipping: HIRESHELBY_TEST_BASE_URL is not set");
            None
        }
    }
}

#[tokio::test]
#[ignore = "requires a running control plane (dev login) and relay"]
async fn full_desktop_flow() {
    let Some(base) = base_url() else { return };
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // ── 1. Sign in (dev login → code → session) ─────────────────────────────
    let email = format!(
        "e2e-{}@hireshelby.test",
        &uuid::Uuid::new_v4().to_string()[..8]
    );
    // Workspace reqwest is built without the `form` feature; encode manually.
    let form_body = format!(
        "email={}&name=E2E%20Flow&return_to=http%3A%2F%2F127.0.0.1%3A5551%2Fcallback%2Fabc",
        email.replace('@', "%40")
    );
    let resp = http
        .post(format!("{base}/v1/auth/dev-login"))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form_body)
        .send()
        .await
        .expect("dev-login");
    let location = resp
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .expect("dev-login must redirect");
    let code = location.split("code=").nth(1).expect("code in redirect");

    let exchange: serde_json::Value = http
        .post(format!("{base}/v1/auth/login/exchange"))
        .json(&serde_json::json!({ "code": code }))
        .send()
        .await
        .expect("exchange")
        .json()
        .await
        .unwrap();
    let session = exchange["session_credential"]
        .as_str()
        .expect("session")
        .to_string();
    let with_session = |req: reqwest::RequestBuilder| req.header("X-HireShelby-Session", &session);

    // ── 2. Identity starts unbound: setup_needed, not an error banner ──────
    let current: serde_json::Value =
        with_session(http.post(format!("{base}/v1/nostr-identities/current")))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(current["error"]["code"], "missing_mapping");
    assert_eq!(current["error"]["setup_needed"], true);

    // ── 3. Challenge → sign exactly as the desktop does → verify ───────────
    let challenge: serde_json::Value =
        with_session(http.post(format!("{base}/v1/nostr-identities/challenge")))
            .json(&serde_json::json!({ "origin": base }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    let get = |k: &str| challenge[k].as_str().unwrap_or_default().to_string();
    assert!(!get("challenge_id").is_empty(), "challenge: {challenge}");

    let keys = Keys::generate();
    let tags: Vec<Tag> = [
        ("challenge_id", get("challenge_id")),
        ("nonce", get("nonce")),
        ("verification_code", get("verification_code")),
        ("audience", "buzz:nostr-identity".into()),
        ("action", "bind_nostr_identity".into()),
        ("protocol", "buzz-nostr-identity".into()),
        ("version", "1".into()),
        ("origin", get("origin")),
        ("expires_at", get("expires_at")),
    ]
    .iter()
    .map(|(k, v)| Tag::parse([*k, v.as_str()]).unwrap())
    .collect();
    let event = EventBuilder::new(Kind::Custom(24243), "")
        .tags(tags)
        .sign_with_keys(&keys)
        .unwrap();

    let bound: serde_json::Value =
        with_session(http.post(format!("{base}/v1/nostr-identities/verify")))
            .json(&serde_json::json!({
                "challenge_id": get("challenge_id"),
                "nonce": get("nonce"),
                "signed_payload": event.as_json(),
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(
        bound["identity"]["pubkey_hex"].as_str(),
        Some(keys.public_key().to_hex().as_str()),
        "bind failed: {bound}"
    );
    assert!(
        bound["identity"]["npub"]
            .as_str()
            .unwrap_or_default()
            .starts_with("npub1"),
        "npub encoding missing: {bound}"
    );

    // A replayed challenge must fail: single-use.
    let replay: serde_json::Value =
        with_session(http.post(format!("{base}/v1/nostr-identities/verify")))
            .json(&serde_json::json!({
                "challenge_id": get("challenge_id"),
                "nonce": get("nonce"),
                "signed_payload": event.as_json(),
            }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(
        replay["error"]["code"], "invalid_challenge",
        "replay must fail"
    );

    // ── 4. Availability → create → taken ───────────────────────────────────
    let slug = format!("e2e-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let avail: serde_json::Value =
        with_session(http.post(format!("{base}/v1/communities/availability")))
            .json(&serde_json::json!({ "name": slug }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(
        avail["available"], true,
        "fresh slug should be free: {avail}"
    );

    let created: serde_json::Value = with_session(http.post(format!("{base}/v1/communities")))
        .json(&serde_json::json!({ "name": slug }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let community = &created["community"];
    assert_eq!(
        community["slug"].as_str(),
        Some(slug.as_str()),
        "create: {created}"
    );
    assert_eq!(
        community["owner_pubkey"].as_str(),
        Some(keys.public_key().to_hex().as_str()),
        "owner must be the bound identity"
    );
    let community_id = community["id"].as_str().unwrap().to_string();

    let avail_after: serde_json::Value =
        with_session(http.post(format!("{base}/v1/communities/availability")))
            .json(&serde_json::json!({ "name": slug }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(
        avail_after["available"], false,
        "created slug must now be taken"
    );

    // ── 5. Archive → listed as archived → unarchive ────────────────────────
    let archived: serde_json::Value =
        with_session(http.post(format!("{base}/v1/communities/archive")))
            .json(&serde_json::json!({ "community_id": community_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert!(
        archived["community"]["archived_at"].is_string(),
        "archive: {archived}"
    );

    let listed: serde_json::Value = with_session(http.post(format!("{base}/v1/communities/list")))
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ours = listed["communities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"].as_str() == Some(community_id.as_str()))
        .expect("created community in list");
    assert!(ours["archived_at"].is_string());
    assert!(
        ours["normalized_host"]
            .as_str()
            .unwrap_or_default()
            .starts_with(&slug),
        "normalized_host should start with the slug"
    );

    let unarchived: serde_json::Value =
        with_session(http.post(format!("{base}/v1/communities/unarchive")))
            .json(&serde_json::json!({ "community_id": community_id }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert!(
        unarchived["community"]["archived_at"].is_null(),
        "unarchive: {unarchived}"
    );

    // ── 6. Foreign community id → not_owner, not a 404 probe ───────────────
    let foreign: serde_json::Value =
        with_session(http.post(format!("{base}/v1/communities/archive")))
            .json(&serde_json::json!({ "community_id": uuid::Uuid::new_v4().to_string() }))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(foreign["error"]["code"], "not_owner");

    println!(
        "FULL DESKTOP FLOW PASSED: {email} bound {} and ran the community lifecycle",
        keys.public_key().to_hex()
    );
}
