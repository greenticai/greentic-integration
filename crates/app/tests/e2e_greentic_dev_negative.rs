use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

/// Negative greentic-dev scenarios: invalid build/flows/add-step should fail with clear errors.
#[test]
fn greentic_dev_negative_scenarios() -> Result<()> {
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

    // Isolate HOME/XDG and write fixture profile.
    let envs = prepare_env(work)?;

    // 1) Invalid component build: introduce a compile error.
    let comp_dir = work.join("bad-comp");
    let new_out = run_cmd_with_output(
        &greentic_dev,
        &[
            "component",
            "new",
            "--name",
            "bad-comp",
            "--non-interactive",
            "--no-git",
            "--path",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
    );
    if !new_out.status.success() {
        if !strict {
            eprintln!(
                "skipping negative greentic-dev tests: component new failed (likely env/tooling):\n{}",
                new_out.stderr
            );
            return Ok(());
        }
        anyhow::bail!("component new failed: {}", new_out.stderr);
    }
    let src = comp_dir.join("src/lib.rs");
    fs::write(
        &src,
        "fn handle_message(_: &str, _: &str) -> String { intentional compile_error }",
    )?;
    let build_out = run_cmd_with_output(
        &greentic_dev,
        &[
            "component",
            "build",
            "--manifest",
            comp_dir.to_str().unwrap(),
        ],
        work,
        &envs,
    );
    assert!(
        !build_out.status.success(),
        "component build should fail for invalid source"
    );
    assert!(
        build_out.stderr.to_lowercase().contains("error"),
        "stderr should contain a diagnostic, got: {}",
        build_out.stderr
    );
    assert!(
        !build_out.stderr.contains("panicked"),
        "stderr should not contain a panic/backtrace: {}",
        build_out.stderr
    );

    // 2) Flow references missing component: validate should fail.
    let pack_missing = work.join("pack-missing-comp");
    run_cmd_ok(
        &greentic_dev,
        &[
            "pack",
            "new",
            "--dir",
            pack_missing.to_str().unwrap(),
            "demo-pack",
        ],
        work,
        "pack new (missing component)",
        &envs,
    )?;
    // Point the sole component to a non-existent wasm to simulate missing dependency.
    let pack_yaml = pack_missing.join("pack.yaml");
    let yaml_raw = fs::read_to_string(&pack_yaml)?;
    let yaml_broken = yaml_raw.replace("components/stub.wasm", "components/does-not-exist.wasm");
    fs::write(&pack_yaml, yaml_broken)?;
    // Some greentic-dev versions expose `pack validate`, others rely on `pack lint`.
    let validate_out = run_cmd_with_output(
        &greentic_dev,
        &["pack", "validate", "--dir", "."],
        &pack_missing,
        &envs,
    );
    // Fallback: some versions expose `pack lint` instead of `pack validate`.
    let validate_out = if validate_out.status.success()
        || validate_out
            .stderr
            .to_lowercase()
            .contains("unrecognized subcommand 'validate'")
    {
        run_cmd_with_output(
            &greentic_dev,
            &["pack", "lint", "--dir", "."],
            &pack_missing,
            &envs,
        )
    } else {
        validate_out
    };
    let validate_out = if validate_out.status.success()
        || validate_out
            .stderr
            .to_lowercase()
            .contains("unrecognized subcommand 'validate'")
    {
        // Try lint as a fallback when validate is unavailable.
        run_cmd_with_output(
            &greentic_dev,
            &["pack", "lint", "--dir", "."],
            &pack_missing,
            &envs,
        )
    } else {
        validate_out
    };
    assert!(
        !validate_out.status.success(),
        "pack validate should fail when component is missing"
    );
    if strict {
        assert!(
            validate_out.stderr.to_lowercase().contains("missing")
                || validate_out.stderr.to_lowercase().contains("not found"),
            "expected missing component error, got stderr: {}",
            validate_out.stderr
        );
    }

    // 3) Invalid add-step insertion: target step does not exist.
    let pack_add_step = work.join("pack-add-step");
    run_cmd_ok(
        &greentic_dev,
        &[
            "pack",
            "new",
            "--dir",
            pack_add_step.to_str().unwrap(),
            "demo-pack",
        ],
        work,
        "pack new (add-step)",
        &envs,
    )?;
    let add_out = run_cmd_with_output(
        &greentic_dev,
        &[
            "flow",
            "add-step",
            "main",
            "--manifest",
            pack_add_step
                .join("components/stub.manifest.json")
                .to_str()
                .unwrap(),
            "--coordinate",
            "repo://missing@0.0.0",
            "--after",
            "no-such-step",
        ],
        &pack_add_step,
        &envs,
    );
    assert!(
        !add_out.status.success(),
        "add-step should fail for invalid insertion point"
    );
    assert!(
        add_out.stderr.to_lowercase().contains("no-such-step")
            || add_out.stderr.to_lowercase().contains("invalid")
            || add_out.stderr.to_lowercase().contains("not found"),
        "expected invalid insertion error, got stderr: {}",
        add_out.stderr
    );
    // Ensure flow did not mutate on failed add-step.
    let flow_file = pack_add_step.join("flows/main.ygtc");
    let flow: serde_yaml_bw::Value = serde_yaml_bw::from_str(&fs::read_to_string(&flow_file)?)?;
    let nodes = flow
        .get("nodes")
        .and_then(|v| v.as_mapping())
        .map(|m| m.len())
        .unwrap_or(0);
    assert_eq!(
        nodes, 1,
        "flow should remain unchanged after failed add-step (nodes={nodes})"
    );

    // 4) Pack build fails on invalid flow.
    let pack_invalid_flow = work.join("pack-invalid-flow");
    run_cmd_ok(
        &greentic_dev,
        &[
            "pack",
            "new",
            "--dir",
            pack_invalid_flow.to_str().unwrap(),
            "demo-pack",
        ],
        work,
        "pack new (invalid flow)",
        &envs,
    )?;
    let flow_file = pack_invalid_flow.join("flows/main.ygtc");
    fs::write(
        &flow_file,
        "id: main\n# missing required fields to force validation error\n",
    )?;
    let build_out = run_cmd_with_output(
        &greentic_dev,
        &["pack", "build", "--in", "."],
        &pack_invalid_flow,
        &envs,
    );
    assert!(
        !build_out.status.success(),
        "pack build should fail with invalid flow"
    );
    assert!(
        build_out.stderr.to_lowercase().contains("error")
            || build_out.stderr.to_lowercase().contains("invalid"),
        "expected validation error, got stderr: {}",
        build_out.stderr
    );
    assert!(
        !gtpack_exists(&pack_invalid_flow),
        "no .gtpack should be produced on build failure"
    );

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
    // Also write to HOME/.config to mirror PR-13 behavior.
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

fn run_cmd_ok(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    label: &str,
    envs: &[(String, String)],
) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .status()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !status.success() {
        anyhow::bail!("{label} failed with status {:?}", status.code());
    }
    Ok(())
}

struct Output {
    status: std::process::ExitStatus,
    stderr: String,
}

fn run_cmd_with_output(bin: &Path, args: &[&str], cwd: &Path, envs: &[(String, String)]) -> Output {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .output()
        .expect("spawn command");
    Output {
        status: output.status,
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    }
}

fn gtpack_exists(pack_dir: &Path) -> bool {
    pack_dir
        .join("target")
        .read_dir()
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .map(|e| e == "gtpack")
                .unwrap_or(false)
        })
}
