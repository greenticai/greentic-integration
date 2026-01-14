use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

#[test]
fn e2e_mcp_pack_smoke() -> Result<()> {
    if std::env::var("GREENTIC_MCP_SMOKE").ok().as_deref() != Some("1") {
        eprintln!("skipping e2e_mcp_pack_smoke: set GREENTIC_MCP_SMOKE=1 to enable");
        return Ok(());
    }

    if std::env::var("MCP_API_KEY").is_err() {
        anyhow::bail!("MCP_API_KEY is required for e2e_mcp_pack_smoke");
    }

    let strict = true;

    let greentic_pack =
        match support::ensure_tool("greentic-pack", "greentic-pack", strict, "greentic-pack")? {
            Some(p) => p,
            None => return Ok(()),
        };
    let greentic_flow =
        match support::ensure_tool("greentic-flow", "greentic-flow", strict, "greentic-flow")? {
            Some(p) => p,
            None => return Ok(()),
        };
    let runner_cli = match support::ensure_tool(
        "greentic-runner-cli",
        "greentic-runner-cli",
        strict,
        "greentic-runner-cli",
    )? {
        Some(p) => p,
        None => return Ok(()),
    };

    let wasm_asset = match resolve_wasm_asset(strict)? {
        Some(path) => path,
        None => return Ok(()),
    };

    let tmp = tempdir().context("tempdir")?;
    let work = tmp.path();
    let pack_dir = work.join("mcp-pack");
    if let Err(err) = run_status(
        &greentic_pack,
        &["new", "--dir", pack_dir.to_str().unwrap(), "mcp-pack"],
        work,
        "pack new",
        strict,
    ) {
        if !strict {
            eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
            return Ok(());
        }
        return Err(err);
    }

    let flow_path = pack_dir.join("flows/main.ygtc");
    if let Err(err) = run_status(
        &greentic_flow,
        &[
            "new",
            "--flow",
            flow_path.to_str().unwrap(),
            "--id",
            "main",
            "--type",
            "messaging",
            "--force",
        ],
        work,
        "flow new",
        strict,
    ) {
        if !strict {
            eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
            return Ok(());
        }
        return Err(err);
    }

    let comp_dir = pack_dir.join("components");
    fs::create_dir_all(&comp_dir)?;
    let local_wasm = comp_dir.join("openweather.component.wasm");
    fs::copy(&wasm_asset, &local_wasm).with_context(|| {
        format!(
            "copy openweather wasm from {}",
            wasm_asset.to_string_lossy()
        )
    })?;
    let weather_payload = r#"{"q":"London,GB"}"#;
    if let Err(err) = run_status(
        &greentic_flow,
        &[
            "add-step",
            "--flow",
            flow_path.to_str().unwrap(),
            "--node-id",
            "weather",
            "--local-wasm",
            local_wasm.to_str().unwrap(),
            "--operation",
            "current_weather_data",
            "--payload",
            weather_payload,
            "--routing-out",
        ],
        work,
        "flow add-step weather",
        strict,
    ) {
        if !strict {
            eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
            return Ok(());
        }
        return Err(err);
    }

    let template_component = std::env::var("GREENTIC_TEMPLATES_COMPONENT")
        .unwrap_or_else(|_| "oci://ghcr.io/greentic-ai/components/templates:latest".to_string());
    let template_operation =
        std::env::var("GREENTIC_TEMPLATES_OPERATION").unwrap_or_else(|_| "render".to_string());
    let template_payload = std::env::var("GREENTIC_TEMPLATES_PAYLOAD").unwrap_or_else(|_| {
        r#"{"template":"Weather in {{name}}: {{weather.0.description}}"}"#.to_string()
    });
    if let Err(err) = run_status(
        &greentic_flow,
        &[
            "add-step",
            "--flow",
            flow_path.to_str().unwrap(),
            "--node-id",
            "template",
            "--component",
            &template_component,
            "--operation",
            &template_operation,
            "--payload",
            &template_payload,
            "--routing-out",
            "--after",
            "weather",
        ],
        work,
        "flow add-step template",
        strict,
    ) {
        if !strict {
            eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
            return Ok(());
        }
        return Err(err);
    }

    if let Err(err) = run_status(
        &greentic_pack,
        &[
            "build",
            "--in",
            pack_dir.to_str().unwrap(),
            "--allow-oci-tags",
        ],
        &pack_dir,
        "pack build",
        strict,
    ) {
        if !strict {
            eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
            return Ok(());
        }
        return Err(err);
    }
    let gtpack = find_gtpack(&pack_dir)?;

    let allow_hosts = std::env::var("GREENTIC_MCP_ALLOW_HOSTS")
        .unwrap_or_else(|_| "api.openweathermap.org".into());
    let run_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            gtpack.to_str().unwrap(),
            "--input",
            "{}",
            "--allow",
            &allow_hosts,
            "--mocks",
            "off",
        ],
        &pack_dir,
        "pack run",
        strict,
    ) {
        Ok(out) => out,
        Err(err) => {
            if !strict {
                eprintln!("skipping e2e_mcp_pack_smoke: {err:?}");
                return Ok(());
            }
            return Err(err);
        }
    };

    let run_out_lower = run_out.to_ascii_lowercase();
    assert!(
        run_out_lower.contains("weather in"),
        "pack run output missing weather template marker: {}",
        run_out
    );

    Ok(())
}

fn resolve_wasm_asset(strict: bool) -> Result<Option<PathBuf>> {
    if let Ok(path) = std::env::var("GREENTIC_MCP_OPENWEATHER_WASM") {
        return Ok(Some(PathBuf::from(path)));
    }
    let default = PathBuf::from(
        "/projects/ai/greentic-ng/greentic-mcp-generator/tests/assets/openweather/openweathermap-org-2.5.component.wasm",
    );
    if default.exists() {
        return Ok(Some(default));
    }
    if strict {
        anyhow::bail!(
            "openweather wasm not found at default path; set GREENTIC_MCP_OPENWEATHER_WASM"
        );
    }
    eprintln!(
        "skipping e2e_mcp_pack_smoke: openweather wasm missing at {}",
        default.display()
    );
    Ok(None)
}

fn run_status(bin: &Path, args: &[&str], cwd: &Path, label: &str, strict: bool) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !status.success() {
        if strict {
            anyhow::bail!("{label} failed in strict mode: {:?}", status.code());
        }
        eprintln!("{label} failed (non-strict skip): {:?}", status.code());
        return Err(anyhow::anyhow!("non-strict skip"));
    }
    Ok(())
}

fn run_capture(bin: &Path, args: &[&str], cwd: &Path, label: &str, strict: bool) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
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
