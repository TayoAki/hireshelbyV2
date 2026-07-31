//! Contract test: every control-plane path the desktop client calls must have
//! a route registered here.
//!
//! The original gap — eight endpoints the desktop called that nothing served —
//! was found by manually diffing `accounts.rs` against `main.rs`. A missing
//! endpoint fails at *runtime* on a user's machine (the Tauri layer surfaces a
//! raw HTTP error), so the mismatch belongs to CI, not to support tickets.
//!
//! Both files live in this repository, so the test reads them as text. That is
//! deliberate: it needs no network, no running server, and it breaks the build
//! the moment either side drifts.

use std::collections::BTreeSet;
use std::path::Path;

fn extract_paths(source: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = source.as_bytes();
    let needle = b"\"/v1/";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            let start = i + 1; // skip the opening quote
            if let Some(end) = source[start..].find('"') {
                let path = &source[start..start + end];
                // Ignore template-y strings; keep plain paths only.
                if path.chars().all(|c| c.is_ascii_graphic()) && !path.contains("{{") {
                    out.insert(path.to_string());
                }
                i = start + end;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[test]
fn every_desktop_path_has_a_server_route() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let desktop = std::fs::read_to_string(repo_root.join("desktop/src-tauri/src/accounts.rs"))
        .expect("desktop accounts.rs must exist — the contract has two sides");
    let server = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main.rs"))
        .expect("control-plane main.rs");

    let called = extract_paths(&desktop);
    let served = extract_paths(&server);

    assert!(
        !called.is_empty(),
        "found no /v1 paths in the desktop client — the extractor regressed"
    );

    let missing: Vec<&String> = called.difference(&served).collect();
    assert!(
        missing.is_empty(),
        "desktop calls paths the control plane does not serve: {missing:?}\n\
         Add the route in crates/hireshelby-accounts/src/main.rs (or remove the client call)."
    );
}

#[test]
fn extractor_finds_paths_and_skips_noise() {
    let sample = r#"
        .route("/v1/auth/login", get(login))
        let x = api_url("/v1/nostr-identities/current")?;
        // not a path: "v2/other" and "/v1/ has no close
    "#;
    let paths = extract_paths(sample);
    assert!(paths.contains("/v1/auth/login"));
    assert!(paths.contains("/v1/nostr-identities/current"));
    assert_eq!(paths.len(), 2);
}
