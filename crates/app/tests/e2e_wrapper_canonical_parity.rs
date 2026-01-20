use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

#[derive(Debug, Deserialize)]
struct GoldenTranscript {
    transcript: Vec<String>,
}

#[test]
fn e2e_wrapper_canonical_parity() -> Result<()> {
    let strict = std::env::var("GREENTIC_DEV_E2E_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("CI").is_ok();

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

    let workspace_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .context("resolve workspace root")?;
    let fixture_root = workspace_root.join("fixtures").join("packs").join("hello");
    let manifest_path = fixture_root.join("pack.json");
    let golden_path = fixture_root.join("golden").join("hello.json");

    let golden: GoldenTranscript = serde_json::from_str(
        &fs::read_to_string(&golden_path)
            .with_context(|| format!("read golden at {}", golden_path.display()))?,
    )
    .context("parse golden transcript")?;

    let tmp = tempdir().context("tempdir")?;
    let envs = prepare_env(tmp.path())?;

    let canonical_output = run_cmd_output(
        &greentic_pack,
        &["sim", manifest_path.to_str().unwrap()],
        &fixture_root,
        &[],
    )?;
    let canonical = if canonical_output.status.success() {
        canonical_output
    } else {
        if is_unknown_subcommand(&canonical_output.stderr) {
            eprintln!("skipping canonical parity: greentic-pack sim unsupported");
            return Ok(());
        }
        if !strict {
            eprintln!(
                "skipping canonical parity: greentic-pack sim failed: {}",
                canonical_output.stderr.trim()
            );
            return Ok(());
        }
        bail!(
            "greentic-pack sim failed in strict mode (status {:?}): {}",
            canonical_output.status.code(),
            canonical_output.stderr.trim()
        );
    };

    let wrapper = match run_wrapper(&greentic_dev, &manifest_path, &fixture_root, &envs, strict)? {
        Some(out) => out,
        None => return Ok(()),
    };

    let canonical_transcript = extract_transcript(&canonical.stdout)
        .or_else(|| extract_text_transcript(&canonical.stdout));
    let wrapper_transcript =
        extract_transcript(&wrapper.stdout).or_else(|| extract_text_transcript(&wrapper.stdout));

    if let (Some(canon), Some(wrap)) = (canonical_transcript, wrapper_transcript) {
        assert_eq!(
            canon, wrap,
            "wrapper transcript mismatch (canonical={canon:?} wrapper={wrap:?})"
        );
        assert_eq!(
            canon, golden.transcript,
            "canonical transcript mismatch (expected {:?})",
            golden.transcript
        );
    } else {
        assert_contains_transcript(&canonical.stdout, &golden.transcript, "canonical")?;
        assert_contains_transcript(&wrapper.stdout, &golden.transcript, "wrapper")?;
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

fn run_wrapper(
    bin: &Path,
    manifest: &Path,
    cwd: &Path,
    envs: &[(String, String)],
    strict: bool,
) -> Result<Option<CmdOutput>> {
    let output = run_cmd_output(bin, &["pack", "sim", manifest.to_str().unwrap()], cwd, envs)?;
    if output.status.success() {
        return Ok(Some(output));
    }

    let stderr = output.stderr.to_lowercase();
    if is_unknown_subcommand(&stderr) {
        eprintln!("skipping wrapper parity: greentic-dev pack sim unsupported");
        return Ok(None);
    }
    if !strict {
        eprintln!(
            "skipping wrapper parity: greentic-dev pack sim failed: {}",
            output.stderr.trim()
        );
        return Ok(None);
    }

    bail!(
        "greentic-dev pack sim failed in strict mode (status {:?}): {}",
        output.status.code(),
        output.stderr.trim()
    );
}

#[allow(dead_code)]
fn run_cmd(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
    label: &str,
    strict: bool,
) -> Result<CmdOutput> {
    let output = run_cmd_output(bin, args, cwd, envs)?;
    if output.status.success() {
        return Ok(output);
    }

    if !strict {
        eprintln!("{label} failed (non-strict skip): {}", output.stderr.trim());
        return Err(anyhow::anyhow!("non-strict skip"));
    }
    bail!(
        "{label} failed in strict mode (status {:?}): {}",
        output.status.code(),
        output.stderr.trim()
    );
}

fn run_cmd_output(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    envs: &[(String, String)],
) -> Result<CmdOutput> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .output()
        .with_context(|| format!("failed to spawn {}", bin.display()))?;
    Ok(CmdOutput {
        status: output.status,
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn extract_transcript(output: &str) -> Option<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(output).ok()?;
    find_transcript(&value)
}

fn find_transcript(value: &serde_json::Value) -> Option<Vec<String>> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::Array(items)) = map.get("transcript") {
                let mut out = Vec::new();
                for item in items {
                    if let Some(s) = item.as_str() {
                        out.push(s.to_string());
                    } else {
                        return None;
                    }
                }
                return Some(out);
            }
            for v in map.values() {
                if let Some(found) = find_transcript(v) {
                    return Some(found);
                }
            }
            None
        }
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(found) = find_transcript(item) {
                    return Some(found);
                }
            }
            None
        }
        _ => None,
    }
}

fn extract_text_transcript(output: &str) -> Option<Vec<String>> {
    let mut lines = Vec::new();
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("USER:")
            || trimmed.starts_with("BOT:")
            || trimmed.starts_with("SYSTEM:")
        {
            lines.push(trimmed.to_string());
        }
    }
    if lines.is_empty() { None } else { Some(lines) }
}

fn is_unknown_subcommand(stderr: &str) -> bool {
    let stderr = stderr.to_lowercase();
    stderr.contains("unrecognized")
        || stderr.contains("unknown")
        || stderr.contains("invalid subcommand")
}

fn assert_contains_transcript(output: &str, expected: &[String], label: &str) -> Result<()> {
    for line in expected {
        if !output.contains(line) {
            bail!("{label} output missing transcript line: {line}");
        }
    }
    Ok(())
}

struct CmdOutput {
    status: std::process::ExitStatus,
    stdout: String,
    stderr: String,
}
