use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

/// Snapshot stability for greentic-dev generated flows and packs.
#[test]
fn greentic_dev_snapshots_are_stable() -> Result<()> {
    let strict = std::env::var("GREENTIC_DEV_E2E_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("CI").is_ok();

    let greentic_dev =
        match support::ensure_tool("greentic-dev", "greentic-dev", strict, "greentic-dev")? {
            Some(p) => p,
            None => return Ok(()),
        };

    let tmp = tempdir().context("tempdir")?;
    let work = tmp.path();
    println!("workspace: {}", work.display());

    let envs = prepare_env(work)?;

    // Generate pack.
    if let Err(err) = run_status(
        &greentic_dev,
        &["pack", "new", "--dir", "snap-pack", "snap-pack"],
        work,
        &envs,
        "pack new",
        strict,
    ) {
        if !strict {
            eprintln!("skipping snapshot test (non-strict): {err:?}");
            return Ok(());
        }
        return Err(err);
    }
    let pack_dir = work.join("snap-pack");

    // Try to insert an extra step via add-step; tolerate failure in non-strict.
    let add_out = Command::new(&greentic_dev)
        .args([
            "flow",
            "add-step",
            "main",
            "--manifest",
            pack_dir
                .join("components/stub.manifest.json")
                .to_str()
                .unwrap(),
            "--coordinate",
            "repo://snap.component@0.1.0",
            "--after",
            "start",
        ])
        .current_dir(&pack_dir)
        .envs(envs.iter().cloned())
        .output()
        .context("flow add-step failed to spawn")?;
    if !add_out.status.success() && strict {
        anyhow::bail!(
            "flow add-step failed in strict mode: {}",
            String::from_utf8_lossy(&add_out.stderr)
        );
    }
    // Require flow to be mutated: expect an extra node beyond 'start'.
    let flow_yaml = fs::read_to_string(pack_dir.join("flows/main.ygtc"))?;
    let flow_json: serde_json::Value = serde_yaml_bw::from_str(&flow_yaml)?;
    let nodes = flow_json
        .get("nodes")
        .and_then(|v| v.as_object())
        .cloned()
        .unwrap_or_default();
    let node_ids: Vec<_> = nodes.keys().cloned().collect();
    assert!(
        nodes.len() > 1,
        "add-step did not add a node; nodes present: {:?}",
        node_ids
    );

    // Snapshot pack.yaml
    let pack_yaml = fs::read_to_string(pack_dir.join("pack.yaml"))?;
    let normalized_pack = normalize_yaml(&pack_yaml)?;
    insta::assert_snapshot!("snap_pack_yaml", normalized_pack);

    // Snapshot flow definition
    let flow_yaml = fs::read_to_string(pack_dir.join("flows/main.ygtc"))?;
    let normalized_flow = normalize_yaml(&flow_yaml)?;
    insta::assert_snapshot!("snap_flow_main", normalized_flow);

    // Regenerate pack again to check deterministic ordering.
    if let Err(err) = run_status(
        &greentic_dev,
        &["pack", "new", "--dir", "snap-pack-2", "snap-pack"],
        work,
        &envs,
        "pack new (repeat)",
        strict,
    ) {
        if strict {
            return Err(err);
        }
    } else {
        let pack2_yaml = fs::read_to_string(work.join("snap-pack-2/pack.yaml"))?;
        let normalized_pack2 = normalize_yaml(&pack2_yaml)?;
        assert_eq!(
            normalized_pack, normalized_pack2,
            "pack.yaml ordering drifted"
        );
    }

    Ok(())
}

fn prepare_env(work: &Path) -> Result<Vec<(String, String)>> {
    let home_dir = work.join("home");
    let xdg_config = work.join(".config");
    let xdg_data = work.join(".local/share");
    let xdg_state = work.join(".local/state");
    let xdg_cache = work.join(".cache");
    for d in [&xdg_config, &xdg_data, &xdg_state, &xdg_cache] {
        fs::create_dir_all(d)?;
    }
    let config_path = xdg_config.join("greentic-dev").join("config.toml");
    if let Some(parent) = config_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let fixtures_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("tests")
        .join("fixtures");
    let profile_tpl = fixtures_root
        .join("greentic-dev")
        .join("profiles")
        .join("default.toml");
    let profile_raw = fs::read_to_string(&profile_tpl).context("read profile template")?;
    let store_path = work.join("store");
    fs::create_dir_all(&store_path)?;
    let config_contents = profile_raw.replace("__STORE_PATH__", store_path.to_str().unwrap());
    fs::write(&config_path, &config_contents)?;
    // Also write to HOME/.config to mirror other greentic-dev tests.
    let home_config = home_dir.join(".config/greentic-dev/config.toml");
    if let Some(parent) = home_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&home_config, &config_contents)?;

    Ok(vec![
        ("HOME".into(), home_dir.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME".into(),
            xdg_config.to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_HOME".into(),
            xdg_data.to_string_lossy().into_owned(),
        ),
        (
            "XDG_STATE_HOME".into(),
            xdg_state.to_string_lossy().into_owned(),
        ),
        (
            "XDG_CACHE_HOME".into(),
            xdg_cache.to_string_lossy().into_owned(),
        ),
        ("GREENTIC_DISTRIBUTOR_PROFILE".into(), "default".into()),
        (
            "GREENTIC_CONFIG_FILE".into(),
            config_path.to_string_lossy().into_owned(),
        ),
    ])
}

fn run_status(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    label: &str,
    strict: bool,
) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .status()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !status.success() {
        if strict {
            anyhow::bail!("{label} failed in strict mode: {:?}", status.code());
        } else {
            eprintln!("{label} failed (non-strict skip): {:?}", status.code());
            return Err(anyhow::anyhow!("non-strict skip"));
        }
    }
    Ok(())
}

fn normalize_yaml(input: &str) -> Result<String> {
    let mut value: serde_json::Value = serde_yaml_bw::from_str(input)?;
    canonicalize_json(&mut value);
    Ok(serde_json::to_string_pretty(&value)?)
}

fn canonicalize_json(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            let mut sorted = BTreeMap::new();
            for (k, mut v) in std::mem::take(map) {
                canonicalize_json(&mut v);
                sorted.insert(k, v);
            }
            *map = sorted.into_iter().collect();
        }
        serde_json::Value::Array(arr) => {
            for v in arr.iter_mut() {
                canonicalize_json(v);
            }
        }
        _ => {}
    }
}
