use std::fs;

use serde_json::Value;

#[test]
fn pack_index_shape_matches_runner_contract() {
    let path = std::env::var("PACK_INDEX_PATH")
        .ok()
        .map(std::path::PathBuf::from)
        .filter(|p| p.exists())
        .unwrap_or_else(|| {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .unwrap()
                .join("target")
                .join("index.json")
        });
    if !path.exists() {
        eprintln!(
            "skipping pack_index_shape_matches_runner_contract: no index at {}",
            path.display()
        );
        return;
    }

    let data = fs::read_to_string(&path).expect("read index.json");
    let json: Value = serde_json::from_str(&data).expect("parse index.json");

    let tenants = json
        .get("tenants")
        .and_then(|v| v.as_object())
        .expect("tenants must be an object");
    assert!(
        !tenants.is_empty(),
        "tenants object should contain at least one tenant"
    );
    for (tenant, entry) in tenants {
        assert!(!tenant.is_empty(), "tenant name cannot be empty");
        let obj = entry.as_object().expect("tenant entry must be object");
        let main = obj
            .get("main_pack")
            .and_then(|v| v.as_object())
            .expect("main_pack missing or not object");
        assert_pack_entry(main, "main_pack");
        if let Some(overlays) = obj.get("overlays") {
            let overlays = overlays.as_array().expect("overlays must be array");
            for overlay in overlays {
                let overlay_obj = overlay.as_object().expect("overlay entry must be object");
                assert_pack_entry(overlay_obj, "overlay");
            }
        }
    }
}

fn assert_pack_entry(entry: &serde_json::Map<String, Value>, label: &str) {
    let reference = entry
        .get("reference")
        .and_then(|v| v.as_object())
        .unwrap_or_else(|| panic!("{label} missing reference object"));
    let name = reference
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}.reference.name missing"));
    assert!(!name.is_empty(), "{label}.reference.name cannot be empty");
    let version = reference
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}.reference.version missing"));
    assert!(
        !version.is_empty(),
        "{label}.reference.version cannot be empty"
    );
    let locator = entry
        .get("locator")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}.locator missing"));
    assert!(
        locator.starts_with("oci://"),
        "{label}.locator must be oci://..."
    );
    let digest = entry
        .get("digest")
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("{label}.digest missing"));
    assert!(
        digest.starts_with("sha256:"),
        "{label}.digest must be sha256:..."
    );
    if let Some(path) = entry.get("path").and_then(|v| v.as_str()) {
        assert!(
            !path.is_empty(),
            "{label}.path, when present, cannot be empty"
        );
    }
}
