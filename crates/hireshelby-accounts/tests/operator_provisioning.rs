//! End-to-end check that the relay actually accepts our operator signature.
//!
//! The unit tests in `operator.rs` prove the *shape* of the NIP-98 event. Only
//! a live call proves the contract: that the relay's verifier agrees with our
//! signer on URL normalisation, the payload hash, the replay guard, and the
//! operator allowlist. A shape-only test would still pass while every real
//! provisioning attempt returned 401.
//!
//! Ignored by default because it needs a relay. Run it with:
//!
//! ```text
//! # Relay must be started with:
//! #   RELAY_OPERATOR_PUBKEYS=<pubkey of HIRESHELBY_OPERATOR_SECRET_KEY>
//! #   RELAY_OPERATOR_API_ORIGIN=http://localhost:3030
//! HIRESHELBY_TEST_RELAY_URL=http://localhost:3030 \
//! HIRESHELBY_TEST_OPERATOR_SECRET_KEY=<hex> \
//!   cargo test -p hireshelby-accounts --test operator_provisioning -- --ignored --nocapture
//! ```

use std::path::Path;

/// The crate is a binary, so pull the module in directly rather than via a
/// library target that only exists for tests.
#[path = "../src/operator.rs"]
#[allow(dead_code)] // the test uses only the provisioning half of the client
mod operator;

use operator::{OperatorClient, ProvisionRequest};

fn env_or_skip(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(v) if !v.trim().is_empty() => Some(v),
        _ => {
            eprintln!("skipping: {key} is not set");
            None
        }
    }
}

#[tokio::test]
#[ignore = "requires a running relay with this operator pubkey allowlisted"]
async fn relay_accepts_our_signed_provisioning_request() {
    let Some(relay_url) = env_or_skip("HIRESHELBY_TEST_RELAY_URL") else {
        return;
    };
    let Some(secret) = env_or_skip("HIRESHELBY_TEST_OPERATOR_SECRET_KEY") else {
        return;
    };
    let keys = nostr::Keys::parse(&secret).expect("valid operator secret key");

    let client = OperatorClient::new(relay_url, keys);

    // A unique host per run: provisioning is idempotent on host, so a fixed
    // value would pass on the second run even if creation were broken.
    let slug = format!("e2e-{}", &uuid::Uuid::new_v4().to_string()[..8]);
    let host = format!("{slug}.communities.hireshelby.test");

    let response = client
        .provision_community(&ProvisionRequest {
            host: host.clone(),
            initial_owner_pubkey: None,
        })
        .await
        .unwrap_or_else(|e| {
            panic!(
                "relay rejected the operator request: {e}\n\
                 If this is a 401/403, the relay's RELAY_OPERATOR_PUBKEYS does not include \
                 this key, or RELAY_OPERATOR_API_ORIGIN does not match the URL we signed."
            )
        });

    assert_eq!(
        response.host.as_deref(),
        Some(host.as_str()),
        "relay should echo back the host it provisioned"
    );
    println!(
        "provisioned {host} -> community_id={:?}",
        response.community_id
    );
}

#[test]
fn migrations_directory_is_present_for_sqlx_migrate() {
    // `sqlx::migrate!` resolves at compile time relative to the crate root; a
    // rename would fail the build, but an empty directory would not.
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    assert!(dir.is_dir(), "migrations directory missing at {dir:?}");
    let count = std::fs::read_dir(&dir)
        .expect("readable migrations dir")
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "sql"))
        .count();
    assert!(count > 0, "no .sql migrations found in {dir:?}");
}
