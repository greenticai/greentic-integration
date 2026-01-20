#![allow(dead_code)]
use std::{fs, path::PathBuf, process::Command, sync::Once};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use which::which;

#[derive(Debug, Deserialize)]
pub struct PackReference {
    pub name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub version: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PackEntry {
    pub reference: PackReference,
    pub locator: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub digest: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TenantEntry {
    main_pack: PackEntry,
    #[serde(default)]
    overlays: Vec<PackEntry>,
}

#[derive(Debug, Deserialize)]
struct PackIndex {
    tenants: serde_json::Map<String, Value>,
}

pub fn index_path_from_env() -> PathBuf {
    std::env::var("PACK_INDEX_URL")
        .ok()
        .and_then(|v| v.strip_prefix("file://").map(|s| s.to_string()))
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .parent()
                .and_then(|p| p.parent())
                .unwrap()
                .join("target")
                .join("index.json")
        })
}

pub fn load_index() -> Result<(String, TenantEntry)> {
    let path = index_path_from_env();
    if !path.exists() {
        anyhow::bail!(
            "PACK_INDEX_URL not set and default index missing at {}",
            path.display()
        );
    }
    let data = fs::read_to_string(&path)
        .with_context(|| format!("failed to read index at {}", path.display()))?;
    let index: PackIndex =
        serde_json::from_str(&data).context("failed to parse pack index JSON")?;
    let (tenant, entry) = index
        .tenants
        .into_iter()
        .next()
        .ok_or_else(|| anyhow::anyhow!("no tenants found in index"))?;
    let entry: TenantEntry =
        serde_json::from_value(entry).context("failed to parse tenant entry")?;
    Ok((tenant, entry))
}

pub fn find_pack<'a>(entry: &'a TenantEntry, name: &str) -> Option<&'a PackEntry> {
    if entry.main_pack.reference.name == name {
        return Some(&entry.main_pack);
    }
    entry.overlays.iter().find(|p| p.reference.name == name)
}

pub fn ensure_crane_manifest(locator: &str) -> Result<Value> {
    let crane = std::env::var("CRANE_BIN").unwrap_or_else(|_| "crane".to_string());
    let mut cmd = Command::new(crane);
    cmd.args(["manifest", locator]);
    if let Ok(cfg) = std::env::var("DOCKER_CONFIG") {
        cmd.env("DOCKER_CONFIG", cfg);
    }
    let output = cmd.output().context("failed to exec crane manifest")?;
    if !output.status.success() {
        anyhow::bail!(
            "crane manifest failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let json: Value =
        serde_json::from_slice(&output.stdout).context("failed to parse crane manifest output")?;
    Ok(json)
}

pub fn ensure_tool(
    binary: &str,
    crate_name: &str,
    strict: bool,
    label: &str,
) -> Result<Option<PathBuf>> {
    if let Ok(path) = which(binary) {
        return Ok(Some(path));
    }
    if let Some(path) = cargo_bin_path(binary) {
        return Ok(Some(path));
    }

    let status = Command::new("cargo")
        .args(["binstall", crate_name, "--no-confirm"])
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .context(format!("failed to spawn cargo binstall for {label}"))?;
    if status.success()
        && let Ok(path) = which(binary)
    {
        return Ok(Some(path));
    }

    if strict {
        anyhow::bail!("{label} missing and cargo binstall failed");
    } else {
        eprintln!(
            "skipping {label}: {} not found and cargo binstall failed (status {:?})",
            binary,
            status.code()
        );
        Ok(None)
    }
}

fn cargo_bin_path(binary: &str) -> Option<PathBuf> {
    let cargo_home = std::env::var("CARGO_HOME")
        .ok()
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .map(|home| PathBuf::from(home).join(".cargo"))
        })?;
    let path = cargo_home.join("bin").join(binary);
    if path.exists() { Some(path) } else { None }
}

static LOG_ONCE: Once = Once::new();

/// Initialize test logging for e2e runs.
/// Respects RUST_LOG (or GT_TEST_LOG to force enable); idempotent.
pub fn init_test_logging() {
    LOG_ONCE.call_once(|| {
        let enable = std::env::var("RUST_LOG").is_ok()
            || std::env::var("GT_TEST_LOG")
                .map(|v| v != "0")
                .unwrap_or(false);
        if !enable {
            return;
        }
        let _ = tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| "info".into()),
            )
            .with_writer(std::io::stderr)
            .try_init();
    });
}
