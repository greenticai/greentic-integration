use std::fs;

use serde::Deserialize;

const PROVIDER_EXTENSION_ID: &str = "greentic.provider-extension.v1";

#[derive(Debug, Deserialize)]
struct AuditIndex {
    entries: Vec<AuditEntry>,
}

#[derive(Debug, Deserialize)]
struct AuditEntry {
    oci_ref: String,
    error: Option<String>,
    manifest: Option<ManifestSummary>,
}

#[derive(Debug, Deserialize)]
struct ManifestSummary {
    kind: String,
    supports: Vec<String>,
    categories: Vec<String>,
    extensions: Vec<String>,
    provider_count: usize,
    providers: Vec<ProviderSummary>,
    has_provider_extension: bool,
}

#[derive(Debug, Deserialize)]
struct ProviderSummary {
    provider_type: String,
    config_schema_ref: Option<String>,
    runtime: Option<ProviderRuntimeSummary>,
}

#[derive(Debug, Deserialize)]
struct ProviderRuntimeSummary {
    component_ref: String,
    export: String,
    world: String,
}

#[test]
fn pack_audit_results_are_valid() {
    let has_token = std::env::var("GITHUB_TOKEN")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    let has_owner = std::env::var("GITHUB_ORG")
        .ok()
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
        || std::env::var("GITHUB_USER")
            .ok()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
    if !has_token || !has_owner {
        eprintln!("pack audit env not set; skipping pack audit validation");
        return;
    }

    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("target")
        .join("pack-audit")
        .join("pack_index.json");

    if !path.exists() {
        eprintln!(
            "pack audit index missing at {}; skipping audit validation",
            path.display()
        );
        return;
    }

    let data = fs::read(&path).expect("failed to read pack audit index");
    let index: AuditIndex =
        serde_json::from_slice(&data).expect("failed to parse pack audit index JSON");

    assert!(
        !index.entries.is_empty(),
        "pack audit index contains no entries"
    );

    for entry in index.entries {
        if let Some(err) = entry.error {
            panic!("{} failed audit: {}", entry.oci_ref, err);
        }
        let manifest = entry
            .manifest
            .as_ref()
            .unwrap_or_else(|| panic!("{} missing manifest summary", entry.oci_ref));

        assert!(
            !manifest.categories.is_empty(),
            "{} has empty category set",
            entry.oci_ref
        );

        if manifest.supports.iter().any(|s| s == "messaging") {
            assert!(
                manifest.categories.iter().any(|c| c == "messaging"),
                "{} supports messaging but is not categorized",
                entry.oci_ref
            );
        }

        if manifest.supports.iter().any(|s| s == "event") {
            assert!(
                manifest.categories.iter().any(|c| c == "events"),
                "{} supports events but is not categorized",
                entry.oci_ref
            );
        }

        if manifest.kind == "provider" {
            assert!(
                manifest.has_provider_extension
                    || manifest
                        .extensions
                        .iter()
                        .any(|e| e == PROVIDER_EXTENSION_ID),
                "{} provider pack missing provider extension",
                entry.oci_ref
            );
            assert!(
                manifest.provider_count > 0,
                "{} provider pack has no providers",
                entry.oci_ref
            );
        }

        if manifest.has_provider_extension || manifest.provider_count > 0 {
            assert!(
                manifest
                    .extensions
                    .iter()
                    .any(|e| e == PROVIDER_EXTENSION_ID),
                "{} missing canonical provider extension key",
                entry.oci_ref
            );
            assert_eq!(
                manifest.provider_count,
                manifest.providers.len(),
                "{} provider count mismatch",
                entry.oci_ref
            );
            for provider in &manifest.providers {
                assert!(
                    !provider.provider_type.is_empty(),
                    "{} provider type empty",
                    entry.oci_ref
                );
                let runtime = provider
                    .runtime
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} provider missing runtime", entry.oci_ref));
                assert!(
                    !runtime.component_ref.is_empty()
                        && !runtime.export.is_empty()
                        && !runtime.world.is_empty(),
                    "{} provider runtime fields must be set",
                    entry.oci_ref
                );
                assert!(
                    provider.config_schema_ref.is_some(),
                    "{} provider missing config_schema_ref",
                    entry.oci_ref
                );
            }
        }
    }
}
