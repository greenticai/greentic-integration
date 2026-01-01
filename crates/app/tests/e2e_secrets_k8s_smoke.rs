#[path = "support/mod.rs"]
mod support;

use support::{ensure_crane_manifest, find_pack, load_index};

#[test]
fn e2e_secrets_k8s_manifest_reachable() {
    let (tenant, entry) = match load_index() {
        Ok(v) => v,
        Err(err) => {
            eprintln!("skipping: {}", err);
            return;
        }
    };
    assert_eq!(tenant, "integration");
    let pack = match find_pack(&entry, "greentic-packs/secrets-k8s") {
        Some(p) => p,
        None => {
            eprintln!("skipping: secrets-k8s pack missing from index");
            return;
        }
    };
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
