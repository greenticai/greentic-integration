use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

/// Greentic-dev offline/local-store workflow: build component, install to local store, build/validate pack without network.
#[test]
fn greentic_dev_offline_local_store() -> Result<()> {
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

    // Isolate HOME/XDG and write fixture profile; configure local store path.
    let store_path = work.join("local-store");
    fs::create_dir_all(&store_path)?;
    let envs = prepare_env(work, &store_path)?;
    let offline_env = offline_env(&store_path);

    // 1) Scaffold component and make deterministic output.
    let comp_dir = work.join("offline-comp");
    let new_out = run_with_output(
        &greentic_dev,
        &[
            "component",
            "new",
            "--name",
            "offline-comp",
            "--non-interactive",
            "--no-git",
            "--path",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
        &offline_env,
    );
    if !new_out.status.success() {
        if !strict {
            eprintln!(
                "skipping offline test: component new failed (likely env/tooling):\n{}",
                new_out.stderr
            );
            return Ok(());
        }
        anyhow::bail!("component new failed in strict mode: {}", new_out.stderr);
    }
    let src = comp_dir.join("src/lib.rs");
    let code = fs::read_to_string(&src).context("read lib.rs")?;
    let patched = code.replace(
        "format!(\"demo-comp::{operation} => {}\", input.trim())",
        "format!(\"OFFLINE::{}\", input.trim().to_ascii_uppercase())",
    );
    fs::write(&src, patched).context("write lib.rs")?;
    let build_out = run_with_output(
        &greentic_dev,
        &[
            "component",
            "build",
            "--manifest",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
        &offline_env,
    );
    if !build_out.status.success() {
        if !strict {
            eprintln!(
                "skipping offline test: component build failed (likely env/tooling):\n{}",
                build_out.stderr
            );
            return Ok(());
        }
        anyhow::bail!(
            "component build failed in strict mode: {}",
            build_out.stderr
        );
    }
    assert!(
        !build_out.stderr.contains("Could not resolve host")
            && !build_out.stderr.to_lowercase().contains("failed to get"),
        "component build attempted network access while offline: {}",
        build_out.stderr
    );
    // 2) Install into local store (filesystem fetch) and ensure file exists.
    let mut store_wasm = work.join("offline_comp.wasm");
    if store_wasm.exists() {
        if store_wasm.is_dir() {
            fs::remove_dir_all(&store_wasm)?;
        } else {
            fs::remove_file(&store_wasm)?;
        }
    }
    let mut fetch_out = run_with_output(
        &greentic_dev,
        &[
            "component",
            "store",
            "fetch",
            "--fs",
            comp_dir.to_str().unwrap(),
            "--output",
            store_wasm.to_str().unwrap(),
            "--cache-dir",
            store_path.to_str().unwrap(),
        ],
        work,
        &envs,
        &offline_env,
    );
    if !fetch_out.status.success() && output_contains(&fetch_out, "is a directory") {
        let fetch_dir = work.join("store-fetch");
        if fetch_dir.exists() {
            fs::remove_dir_all(&fetch_dir)?;
        }
        fetch_out = run_with_output(
            &greentic_dev,
            &[
                "component",
                "store",
                "fetch",
                "--fs",
                comp_dir.to_str().unwrap(),
                "--output",
                fetch_dir.to_str().unwrap(),
                "--cache-dir",
                store_path.to_str().unwrap(),
            ],
            work,
            &envs,
            &offline_env,
        );
        if fetch_out.status.success() {
            store_wasm = find_wasm(&fetch_dir)?;
        }
    }
    if !fetch_out.status.success() {
        if !strict {
            eprintln!(
                "skipping offline test: component store fetch failed:\n{}",
                fetch_out.stderr
            );
            return Ok(());
        }
        anyhow::bail!(
            "component store fetch failed in strict mode: {}",
            fetch_out.stderr
        );
    }
    assert!(
        store_wasm.exists(),
        "expected wasm in local store at {}",
        store_wasm.display()
    );
    assert!(
        !fetch_out.stderr.contains("Could not resolve host")
            && !fetch_out.stderr.to_lowercase().contains("failed to get"),
        "component store fetch attempted network access while offline: {}",
        fetch_out.stderr
    );

    // 3) Pack build using only local artifacts.
    let pack_dir = work.join("offline-pack");
    run_status(
        &greentic_dev,
        &[
            "pack",
            "new",
            "--dir",
            pack_dir.to_str().unwrap(),
            "offline-pack",
        ],
        work,
        &envs,
        &offline_env,
        strict,
        "pack new",
    )?;

    // Replace pack.yaml to reference our component and wasm.
    let pack_yaml = pack_dir.join("pack.yaml");
    let pack_raw = fs::read_to_string(&pack_yaml)?;
    let pack_custom = pack_raw
        .replace("demo.component", "offline.component")
        .replace("components/stub.wasm", store_wasm.to_str().unwrap());
    fs::write(&pack_yaml, pack_custom)?;

    // Validate pack (offline).
    run_status(
        &greentic_dev,
        &["pack", "validate", "--dir", ".", "--offline"],
        &pack_dir,
        &envs,
        &offline_env,
        strict,
        "pack validate",
    )?;

    // Build pack (offline).
    run_status(
        &greentic_dev,
        &["pack", "build", "--in", ".", "--offline"],
        &pack_dir,
        &envs,
        &offline_env,
        strict,
        "pack build",
    )?;
    let gtpack = find_gtpack(&pack_dir)?;

    // 4) Offline run (best-effort; skip if unavailable).
    match run_with_output(
        &greentic_dev,
        &[
            "pack",
            "run",
            "--pack",
            gtpack.to_str().unwrap(),
            "--input",
            r#"{"text":"world"}"#,
            "--json",
            "--offline",
            "--artifacts",
            pack_dir.join("run-artifacts").to_str().unwrap(),
        ],
        &pack_dir,
        &envs,
        &offline_env,
    ) {
        out if out.status.success() => {
            assert!(
                out.stdout.contains("OFFLINE::WORLD"),
                "expected offline output, got stdout: {}",
                out.stdout
            );
        }
        out => {
            if strict {
                anyhow::bail!("pack run failed in strict mode: {}", out.stderr);
            } else {
                eprintln!("skipping pack run check (non-strict): {}", out.stderr);
            }
        }
    }

    Ok(())
}

fn prepare_env(work: &Path, store_path: &Path) -> Result<Vec<(String, String)>> {
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
    let config_contents = profile_raw.replace("__STORE_PATH__", store_path.to_str().unwrap());
    fs::write(&config_path, &config_contents)?;
    // Also write to HOME/.config to mirror other tests.
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

fn offline_env(store_path: &Path) -> Vec<(String, String)> {
    vec![
        ("CARGO_NET_OFFLINE".into(), "true".into()),
        (
            "GREENTIC_COMPONENT_STORE".into(),
            store_path.to_string_lossy().into_owned(),
        ),
        ("GREENTIC_DEV_OFFLINE".into(), "1".into()),
    ]
}

fn run_status(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    offline_env: &[(String, String)],
    strict: bool,
    label: &str,
) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .envs(offline_env.iter().cloned())
        .status()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !status.success() {
        if strict {
            anyhow::bail!("{label} failed in strict mode: {:?}", status.code());
        } else {
            eprintln!("{label} failed (non-strict, skipping): {:?}", status.code());
            return Err(anyhow::anyhow!("non-strict skip"));
        }
    }
    Ok(())
}

struct CmdOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}

fn run_with_output(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    offline_env: &[(String, String)],
) -> CmdOutput {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .envs(offline_env.iter().cloned())
        .output()
        .expect("spawn command");
    CmdOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

fn output_contains(output: &CmdOutput, needle: &str) -> bool {
    let needle = needle.to_ascii_lowercase();
    output.stdout.to_ascii_lowercase().contains(&needle)
        || output.stderr.to_ascii_lowercase().contains(&needle)
}

fn find_gtpack(pack_dir: &Path) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(pack_dir.join("target"))
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().map(|ext| ext == "gtpack").unwrap_or(false) {
            return Ok(path.to_path_buf());
        }
    }
    anyhow::bail!("gtpack not found under {}", pack_dir.display())
}

fn find_wasm(root: &Path) -> Result<PathBuf> {
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        let path = entry.path();
        if path.extension().map(|ext| ext == "wasm").unwrap_or(false) {
            return Ok(path.to_path_buf());
        }
    }
    anyhow::bail!("wasm not found under {}", root.display())
}
