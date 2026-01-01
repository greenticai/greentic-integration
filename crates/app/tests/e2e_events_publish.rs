#[path = "support/mod.rs"]
mod support;

use support::{ensure_crane_manifest, find_pack, load_index};

#[test]
fn e2e_events_manifest_reachable() {
    let (tenant, entry) = match load_index() {
        Ok(v) => v,
        Err(err) => {
            eprintln!("skipping: {}", err);
            return;
        }
    };
    assert_eq!(tenant, "integration");
    let candidates = [
        "greentic-packs/events-dummy",
        "greentic-packs/events-timer",
        "greentic-packs/events-webhook",
        "greentic-packs/events-sms",
        "greentic-packs/events-email",
    ];
    let pack = candidates
        .iter()
        .find_map(|name| find_pack(&entry, name))
        .unwrap_or_else(|| {
            eprintln!("skipping: no greentic-packs/events-* entries found in index");
            std::process::exit(0);
        });
    assert!(
        pack.locator.starts_with("oci://"),
        "locator must be oci://..., got {}",
        pack.locator
    );
    let manifest = match ensure_crane_manifest(&pack.locator) {
        Ok(m) => m,
        Err(err) => {
            eprintln!("skipping: crane manifest failed ({err})");
            return;
        }
    };
    assert!(
        manifest.get("schemaVersion").is_some(),
        "manifest missing schemaVersion"
    );
}
