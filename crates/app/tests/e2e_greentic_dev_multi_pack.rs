use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use pathdiff::diff_paths;
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

/// Two packs sharing a component plus isolation check between pack builds.
#[test]
fn greentic_dev_multi_pack_shared_component() -> Result<()> {
    if std::env::var("GREENTIC_DEV_E2E").ok().as_deref() != Some("1") {
        eprintln!(
            "skipping greentic_dev_multi_pack_shared_component: set GREENTIC_DEV_E2E=1 to enable"
        );
        return Ok(());
    }

    let strict = is_strict();
    let greentic_dev =
        match support::ensure_tool("greentic-dev", "greentic-dev", strict, "greentic-dev")? {
            Some(p) => p,
            None => return Ok(()),
        };
    let greentic_pack =
        match support::ensure_tool("greentic-pack", "greentic-pack", strict, "greentic-pack")? {
            Some(p) => p,
            None => return Ok(()),
        };

    let tmp = tempdir().context("tempdir")?;
    let work = tmp.path();
    println!("workspace: {}", work.display());
    let envs = prepare_env(work)?;

    // Shared component.
    let comp_dir = work.join("shared-comp");
    if let Err(err) = run_status(
        &greentic_dev,
        &[
            "component",
            "new",
            "--name",
            "shared-comp",
            "--non-interactive",
            "--no-git",
            "--path",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
        "component new",
        strict,
    ) {
        if !strict {
            eprintln!("skipping multi-pack test: {err:?}");
            return Ok(());
        }
        return Err(err);
    }
    let src = comp_dir.join("src/lib.rs");
    let code = fs::read_to_string(&src)?;
    fs::write(
        &src,
        code.replace(
            "format!(\"demo-comp::{operation} => {}\", input.trim())",
            "format!(\"SHARED::{}\", input.trim().to_ascii_uppercase())",
        ),
    )?;
    run_status(
        &greentic_dev,
        &[
            "component",
            "build",
            "--manifest",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
        "component build",
        strict,
    )?;
    let wasm_path = comp_dir
        .join("target/wasm32-wasip2/release/shared_comp.wasm")
        .canonicalize()
        .context("locate shared wasm")?;
    assert!(
        wasm_path.exists(),
        "expected built shared component at {}",
        wasm_path.display()
    );

    // Pack A and B.
    let pack_a = work.join("pack-a");
    let pack_b = work.join("pack-b");
    run_status(
        &greentic_dev,
        &["pack", "new", "--dir", pack_a.to_str().unwrap(), "pack-a"],
        work,
        &envs,
        "pack new A",
        strict,
    )?;
    run_status(
        &greentic_dev,
        &["pack", "new", "--dir", pack_b.to_str().unwrap(), "pack-b"],
        work,
        &envs,
        "pack new B",
        strict,
    )?;

    write_shared_pack(&pack_a, "pack-a.shared", &wasm_path)?;
    write_shared_pack(&pack_b, "pack-b.shared", &wasm_path)?;
    rewrite_flow_for_shared_component(&pack_a, "pack-a.shared")?;
    rewrite_flow_for_shared_component(&pack_b, "pack-b.shared")?;
    run_status(
        &greentic_pack,
        &["resolve", "--in", pack_a.to_str().unwrap()],
        work,
        &envs,
        "pack resolve A",
        strict,
    )?;
    run_status(
        &greentic_pack,
        &["resolve", "--in", pack_b.to_str().unwrap()],
        work,
        &envs,
        "pack resolve B",
        strict,
    )?;

    // Build both packs.
    run_status(
        &greentic_dev,
        &["pack", "build", "--in", ".", "--allow-oci-tags"],
        &pack_a,
        &envs,
        "pack build A",
        strict,
    )?;
    let pack_b_yaml_before = fs::read_to_string(pack_b.join("pack.yaml"))?;
    run_status(
        &greentic_dev,
        &["pack", "build", "--in", ".", "--allow-oci-tags"],
        &pack_b,
        &envs,
        "pack build B",
        strict,
    )?;
    // Re-run pack B build in non-strict mode to surface build stderr if missing artifact.
    if find_gtpack(&pack_b).is_err() && !strict {
        eprintln!("pack B build produced no gtpack; stdout/stderr follow");
        let _ = run_capture(
            &greentic_dev,
            &["pack", "build", "--in", ".", "--allow-oci-tags"],
            &pack_b,
            &envs,
            "pack build B (diagnostic)",
            strict,
        );
    }
    let pack_b_gtpack = find_gtpack(&pack_b)
        .context("gtpack for pack B not found; ensure pack build produced artifacts")?;
    // Sanity: run pack B to ensure shared component is usable and outputs expected marker.
    let runner_cli = match support::ensure_tool(
        "greentic-runner-cli",
        "greentic-runner-cli",
        strict,
        "greentic-runner-cli",
    )? {
        Some(p) => p,
        None => return Ok(()),
    };
    let run_out_b = run_capture(
        &runner_cli,
        &[
            "--pack",
            pack_b_gtpack.to_str().unwrap(),
            "--input",
            r#""hello""#,
        ],
        &pack_b,
        &envs,
        "pack run B",
        strict,
    )?;
    assert!(
        run_out_b.contains("SHARED::HELLO"),
        "pack B run output missing shared component marker: {}",
        run_out_b
    );

    // Isolation: mutate Pack A flow, rebuild A, ensure Pack B manifest unchanged.
    let flow_path = pack_a.join("flows/main.ygtc");
    let mut flow_yaml: serde_yaml_bw::Value =
        serde_yaml_bw::from_str(&fs::read_to_string(&flow_path)?)?;
    if let Some(mapping) = flow_yaml.as_mapping_mut() {
        mapping.insert(
            serde_yaml_bw::Value::from("title"),
            serde_yaml_bw::Value::from("Changed only in A"),
        );
    }
    fs::write(&flow_path, serde_yaml_bw::to_string(&flow_yaml)?)?;
    run_status(
        &greentic_dev,
        &["pack", "build", "--in", ".", "--allow-oci-tags"],
        &pack_a,
        &envs,
        "pack build A (modified)",
        strict,
    )?;
    let pack_b_yaml_after = fs::read_to_string(pack_b.join("pack.yaml"))?;
    assert_eq!(
        pack_b_yaml_before, pack_b_yaml_after,
        "Pack B manifest should remain unchanged when Pack A changes"
    );

    Ok(())
}

fn write_shared_pack(pack_dir: &Path, comp_id: &str, wasm: &Path) -> Result<()> {
    let pack_yaml = pack_dir.join("pack.yaml");
    let mut doc: serde_yaml_bw::Value = serde_yaml_bw::from_str(&fs::read_to_string(&pack_yaml)?)?;
    let mapping = doc.as_mapping_mut().context("pack yaml mapping")?;
    let mut comps = serde_yaml_bw::Sequence::new();
    let comp_dir = pack_dir.join("components");
    fs::create_dir_all(&comp_dir)?;
    let dest_wasm = comp_dir.join("shared_comp.wasm");
    fs::copy(wasm, &dest_wasm)?;
    let wasm_rel = diff_paths(&dest_wasm, pack_dir).unwrap_or(dest_wasm);
    comps.push(serde_yaml_bw::to_value(serde_yaml_bw::Mapping::from_iter(
        [
            (
                serde_yaml_bw::Value::from("id"),
                serde_yaml_bw::Value::from(comp_id),
            ),
            (
                serde_yaml_bw::Value::from("version"),
                serde_yaml_bw::Value::from("0.1.0"),
            ),
            (
                serde_yaml_bw::Value::from("world"),
                serde_yaml_bw::Value::from("greentic:component/component@0.5.0"),
            ),
            (
                serde_yaml_bw::Value::from("supports"),
                serde_yaml_bw::Value::Sequence({
                    let mut s = serde_yaml_bw::Sequence::new();
                    s.push(serde_yaml_bw::Value::from("messaging"));
                    s
                }),
            ),
            (
                serde_yaml_bw::Value::from("profiles"),
                serde_yaml_bw::to_value(serde_yaml_bw::Mapping::from_iter([
                    (
                        serde_yaml_bw::Value::from("default"),
                        serde_yaml_bw::Value::from("default"),
                    ),
                    (
                        serde_yaml_bw::Value::from("supported"),
                        serde_yaml_bw::Value::Sequence({
                            let mut s = serde_yaml_bw::Sequence::new();
                            s.push(serde_yaml_bw::Value::from("default"));
                            s
                        }),
                    ),
                ]))?,
            ),
            (
                serde_yaml_bw::Value::from("capabilities"),
                serde_yaml_bw::to_value(serde_yaml_bw::Mapping::from_iter([
                    (
                        serde_yaml_bw::Value::from("wasi"),
                        serde_yaml_bw::Value::Mapping(serde_yaml_bw::Mapping::new()),
                    ),
                    (
                        serde_yaml_bw::Value::from("host"),
                        serde_yaml_bw::Value::Mapping(serde_yaml_bw::Mapping::new()),
                    ),
                ]))?,
            ),
            (
                serde_yaml_bw::Value::from("wasm"),
                serde_yaml_bw::Value::from(wasm_rel.to_string_lossy().to_string()),
            ),
        ],
    ))?);
    mapping.insert(
        serde_yaml_bw::Value::from("components"),
        serde_yaml_bw::Value::Sequence(comps),
    );
    mapping.insert(
        serde_yaml_bw::Value::from("extensions"),
        serde_yaml_bw::Value::Mapping(serde_yaml_bw::Mapping::new()),
    );
    fs::write(&pack_yaml, serde_yaml_bw::to_string(&doc)?)?;
    Ok(())
}

fn rewrite_flow_for_shared_component(pack_dir: &Path, comp_id: &str) -> Result<()> {
    let flow_path = pack_dir.join("flows/main.ygtc");
    let mut doc: serde_yaml_bw::Value = serde_yaml_bw::from_str(&fs::read_to_string(&flow_path)?)?;
    let mapping = doc.as_mapping_mut().context("flow yaml mapping")?;
    let nodes_value = mapping
        .get_mut(serde_yaml_bw::Value::from("nodes"))
        .context("flow nodes missing")?;
    let nodes = nodes_value.as_mapping_mut().context("flow nodes mapping")?;
    let start_key = serde_yaml_bw::Value::from("start");
    let node_key = if nodes.contains_key(&start_key) {
        start_key
    } else if let Some(key) = nodes.keys().next().cloned() {
        key
    } else {
        let key = start_key;
        nodes.insert(
            key.clone(),
            serde_yaml_bw::Value::Mapping(serde_yaml_bw::Mapping::new()),
        );
        key
    };
    let start_node = nodes
        .get_mut(&node_key)
        .context("flow start node missing")?;
    let start_map = start_node
        .as_mapping_mut()
        .context("flow start node mapping")?;
    start_map.clear();
    let mut component = serde_yaml_bw::Mapping::new();
    component.insert(
        serde_yaml_bw::Value::from("id"),
        serde_yaml_bw::Value::from(comp_id),
    );
    component.insert(
        serde_yaml_bw::Value::from("operation"),
        serde_yaml_bw::Value::from("handle_message"),
    );
    component.insert(
        serde_yaml_bw::Value::from("input"),
        serde_yaml_bw::Value::from("hello"),
    );
    start_map.insert(
        serde_yaml_bw::Value::from("component"),
        serde_yaml_bw::Value::Mapping(component),
    );
    start_map.insert(
        serde_yaml_bw::Value::from("routing"),
        serde_yaml_bw::Value::from("out"),
    );
    fs::write(&flow_path, serde_yaml_bw::to_string(&doc)?)?;
    let resolve_path = flow_path.with_extension("ygtc.resolve.json");
    let summary_path = flow_path.with_extension("ygtc.resolve.summary.json");
    let _ = fs::remove_file(resolve_path);
    let _ = fs::remove_file(summary_path);
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

fn is_strict() -> bool {
    std::env::var("GREENTIC_DEV_E2E_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("CI").is_ok()
}

fn run_capture(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    label: &str,
    strict: bool,
) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .output()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !output.status.success() {
        if strict {
            anyhow::bail!(
                "{label} failed in strict mode: {:?}\nstderr:\n{}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        eprintln!(
            "{label} failed (non-strict): {:?}\nstderr:\n{}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(anyhow::anyhow!("non-strict skip"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn find_gtpack(pack_dir: &Path) -> Result<PathBuf> {
    for root in ["dist", "target"] {
        let root_path = pack_dir.join(root);
        if !root_path.exists() {
            continue;
        }
        for entry in walkdir::WalkDir::new(&root_path)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.extension().map(|ext| ext == "gtpack").unwrap_or(false) {
                return Ok(path.to_path_buf());
            }
        }
    }
    anyhow::bail!("gtpack not found under {}", pack_dir.display())
}
