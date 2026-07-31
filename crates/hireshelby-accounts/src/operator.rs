//! NIP-98 signed client for the relay's deployment-root operator API.
//!
//! The relay exposes `POST /operator/communities` (plus archive/unarchive and
//! list) as the only way to create tenancy. It authenticates callers with
//! NIP-98 (`kind:27235`) and gates them against its `RELAY_OPERATOR_PUBKEYS`
//! allowlist — see `buzz-relay/src/handlers/community_provisioning.rs`.
//!
//! This module is the other half of that contract. The relay's verifier checks
//! the `u` (URL), `method`, and `payload` (body SHA-256) tags, and rejects
//! replays by event id, so the signer here must produce all three and a fresh
//! nonce per request.

use base64::Engine as _;
use nostr::{EventBuilder, JsonUtil, Keys, Kind, Tag};
use sha2::{Digest, Sha256};

/// Relay-side NIP-98 kind. Mirrors `buzz_core::kind::KIND_HTTP_AUTH`.
const KIND_HTTP_AUTH: u16 = 27235;

#[derive(Debug, thiserror::Error)]
pub enum OperatorError {
    #[error("operator: failed to sign NIP-98 request: {0}")]
    Signing(String),
    #[error("operator: relay request failed: {0}")]
    Transport(String),
    #[error("operator: relay rejected the request ({status}): {body}")]
    Rejected { status: u16, body: String },
}

/// Builds the `Authorization: Nostr <base64(event)>` header for one request.
///
/// A fresh `nonce` is included on every call. Without it, two identical
/// requests in the same second would serialize to the same event id and the
/// relay's replay guard would reject the second one.
pub fn sign_nip98(
    keys: &Keys,
    method: &str,
    url: &str,
    body: Option<&[u8]>,
) -> Result<String, OperatorError> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let mut tags = vec![
        Tag::parse(["u", url]).map_err(|e| OperatorError::Signing(e.to_string()))?,
        Tag::parse(["method", method]).map_err(|e| OperatorError::Signing(e.to_string()))?,
        Tag::parse(["nonce", &nonce]).map_err(|e| OperatorError::Signing(e.to_string()))?,
    ];
    if let Some(bytes) = body {
        let hash = hex::encode(Sha256::digest(bytes));
        tags.push(
            Tag::parse(["payload", &hash]).map_err(|e| OperatorError::Signing(e.to_string()))?,
        );
    }

    let event = EventBuilder::new(Kind::Custom(KIND_HTTP_AUTH), "")
        .tags(tags)
        .sign_with_keys(keys)
        .map_err(|e| OperatorError::Signing(e.to_string()))?;

    let encoded = base64::engine::general_purpose::STANDARD.encode(event.as_json().as_bytes());
    Ok(format!("Nostr {encoded}"))
}

#[derive(Debug, serde::Serialize)]
pub struct ProvisionRequest {
    /// Fully-qualified community host, e.g. `acme.communities.hireshelby.com`.
    pub host: String,
    /// Optional bootstrap owner. When set on an existing community the relay
    /// rotates ownership, so callers must only pass it deliberately.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_owner_pubkey: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ProvisionResponse {
    #[serde(default)]
    pub community_id: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
}

pub struct OperatorClient {
    http: reqwest::Client,
    base_url: String,
    keys: Keys,
}

impl OperatorClient {
    pub fn new(base_url: impl Into<String>, keys: Keys) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            keys,
        }
    }

    /// Creates (or re-asserts) a community on the relay.
    ///
    /// The relay treats this as idempotent on `host`, so a retry after a
    /// timeout will not duplicate tenancy.
    pub async fn provision_community(
        &self,
        req: &ProvisionRequest,
    ) -> Result<ProvisionResponse, OperatorError> {
        let url = format!("{}/operator/communities", self.base_url);
        let body = serde_json::to_vec(req).map_err(|e| OperatorError::Signing(e.to_string()))?;
        // The signature covers this exact byte string; send it verbatim rather
        // than re-serializing, or the payload hash will not match.
        let auth = sign_nip98(&self.keys, "POST", &url, Some(&body))?;

        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|e| OperatorError::Transport(e.to_string()))?;

        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(OperatorError::Rejected {
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|e| OperatorError::Transport(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys() -> Keys {
        Keys::parse("0000000000000000000000000000000000000000000000000000000000000003").unwrap()
    }

    fn decode(header: &str) -> serde_json::Value {
        let b64 = header.strip_prefix("Nostr ").expect("Nostr scheme prefix");
        let raw = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("valid base64");
        serde_json::from_slice(&raw).expect("valid event JSON")
    }

    fn tag<'a>(event: &'a serde_json::Value, name: &str) -> Option<&'a str> {
        event["tags"]
            .as_array()?
            .iter()
            .find(|t| t[0].as_str() == Some(name))?
            .get(1)?
            .as_str()
    }

    #[test]
    fn signs_kind_27235_with_url_and_method_tags() {
        let header = sign_nip98(
            &keys(),
            "POST",
            "http://localhost:3030/operator/communities",
            None,
        )
        .unwrap();
        let event = decode(&header);
        assert_eq!(event["kind"], 27235);
        assert_eq!(
            tag(&event, "u"),
            Some("http://localhost:3030/operator/communities")
        );
        assert_eq!(tag(&event, "method"), Some("POST"));
    }

    #[test]
    fn payload_tag_is_the_sha256_of_the_exact_body_bytes() {
        let body = br#"{"host":"acme.communities.hireshelby.com"}"#;
        let header =
            sign_nip98(&keys(), "POST", "http://x/operator/communities", Some(body)).unwrap();
        let event = decode(&header);
        assert_eq!(
            tag(&event, "payload"),
            Some(hex::encode(Sha256::digest(body)).as_str()),
            "relay recomputes this hash over the received bytes; a mismatch is a 401"
        );
    }

    #[test]
    fn payload_tag_is_absent_when_there_is_no_body() {
        let header = sign_nip98(&keys(), "GET", "http://x/operator/communities", None).unwrap();
        assert_eq!(tag(&decode(&header), "payload"), None);
    }

    #[test]
    fn each_signature_carries_a_fresh_nonce_so_replay_guard_allows_retries() {
        // Two identical requests in the same second must not collapse to the
        // same event id, or the relay's replay guard rejects the second.
        let a = decode(&sign_nip98(&keys(), "POST", "http://x/y", None).unwrap());
        let b = decode(&sign_nip98(&keys(), "POST", "http://x/y", None).unwrap());
        assert_ne!(tag(&a, "nonce"), tag(&b, "nonce"));
        assert_ne!(a["id"], b["id"]);
    }

    #[test]
    fn signature_is_attributable_to_the_operator_pubkey() {
        let header = sign_nip98(&keys(), "POST", "http://x/y", None).unwrap();
        let event = decode(&header);
        assert_eq!(
            event["pubkey"].as_str(),
            Some(keys().public_key().to_hex().as_str()),
            "relay matches this against RELAY_OPERATOR_PUBKEYS"
        );
    }

    #[test]
    fn provision_request_omits_owner_when_absent() {
        // Sending `initial_owner_pubkey: null` on an existing community would
        // be a different request than omitting it; keep the wire minimal.
        let json = serde_json::to_string(&ProvisionRequest {
            host: "acme.communities.hireshelby.com".into(),
            initial_owner_pubkey: None,
        })
        .unwrap();
        assert!(!json.contains("initial_owner_pubkey"), "got: {json}");
    }
}
