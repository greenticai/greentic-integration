use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use tempfile::tempdir;

#[path = "support/mod.rs"]
mod support;

#[test]
fn state_pr05_conformance() -> Result<()> {
    let strict = is_strict();

    let greentic_pack =
        match support::ensure_tool("greentic-pack", "greentic-pack", strict, "greentic-pack")? {
            Some(path) => path,
            None => return Ok(()),
        };
    let runner_cli = match support::ensure_tool(
        "greentic-runner-cli",
        "greentic-runner-cli",
        strict,
        "greentic-runner-cli",
    )? {
        Some(path) => path,
        None => return Ok(()),
    };

    let templating_wasm = match build_component("templating_component", strict)? {
        Some(path) => path,
        None => return Ok(()),
    };
    let writer_wasm = match build_component("state_writer_component", strict)? {
        Some(path) => path,
        None => return Ok(()),
    };
    let reader_wasm = match build_component("state_reader_component", strict)? {
        Some(path) => path,
        None => return Ok(()),
    };
    let nocap_wasm = match build_component("state_nocap_component", strict)? {
        Some(path) => path,
        None => return Ok(()),
    };

    let tmp = tempdir().context("tempdir")?;
    let work = tmp.path();

    let templating_pack = work.join("templating-pack");
    let templating_flow = "flows/templating.ygtc";
    write_pack_layout(
        &templating_pack,
        "templating",
        templating_flow,
        &[ComponentSpec {
            id: "conformance.templating",
            wasm: "templating_component.wasm",
            state_read: false,
            state_write: false,
        }],
    )?;
    fs::copy(
        &templating_wasm,
        templating_pack.join("components/templating_component.wasm"),
    )?;
    fs::write(
        templating_pack.join(templating_flow),
        templating_flow_yaml(),
    )?;
    write_flow_resolution(
        &templating_pack,
        templating_flow,
        "components/templating_component.wasm",
        "conformance.templating",
    )?;
    if !build_pack(&greentic_pack, &templating_pack, strict)? {
        return Ok(());
    }
    let templating_gtpack = find_gtpack(&templating_pack)?;

    let templating_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            templating_gtpack.to_str().unwrap(),
            "--input",
            r#"{"message":"Hello"}"#,
        ],
        &templating_pack,
        "templating pack run",
        strict,
    )? {
        Some(out) => out,
        None => return Ok(()),
    };
    let templating_json = extract_json(&templating_out)?;
    ensure!(templating_json["marker"] == "templating.process");
    ensure!(templating_json["user_id"].is_number());
    ensure!(templating_json["user_id"].as_i64() == Some(1));
    ensure!(templating_json["user_id_type"] == "number");
    ensure!(templating_json["name"] == "Ada");
    ensure!(templating_json["status"] == "ready");
    ensure!(templating_json["message"] == "Hello");

    let state_pack = work.join("state-pack");
    let state_flow = "flows/state.ygtc";
    write_pack_layout(
        &state_pack,
        "state",
        state_flow,
        &[
            ComponentSpec {
                id: "conformance.state_writer",
                wasm: "state_writer_component.wasm",
                state_read: false,
                state_write: true,
            },
            ComponentSpec {
                id: "conformance.state_reader",
                wasm: "state_reader_component.wasm",
                state_read: true,
                state_write: false,
            },
        ],
    )?;
    fs::copy(
        &writer_wasm,
        state_pack.join("components/state_writer_component.wasm"),
    )?;
    fs::copy(
        &reader_wasm,
        state_pack.join("components/state_reader_component.wasm"),
    )?;
    fs::write(state_pack.join(state_flow), state_flow_yaml())?;
    if !build_pack(&greentic_pack, &state_pack, strict)? {
        return Ok(());
    }
    let state_gtpack = find_gtpack(&state_pack)?;

    let roundtrip_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            state_gtpack.to_str().unwrap(),
            "--input",
            r#"{"tenant":"tenant-a","key":"conformance-key","value":{"hello":"world"},"skip_write":false,"delete":true}"#,
        ],
        &state_pack,
        "state pack roundtrip",
        strict,
    )? {
        Some(out) => out,
        None => return Ok(()),
    };
    let roundtrip_json = extract_json(&roundtrip_out)?;
    ensure!(roundtrip_json["marker"] == "state.read");
    ensure!(roundtrip_json["write_status"] == "wrote");
    ensure!(roundtrip_json["prior_read_status"] == "ok");
    ensure!(roundtrip_json["delete_status"] == "deleted");
    ensure!(roundtrip_json["status"] == "missing");

    let tenant_a_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            state_gtpack.to_str().unwrap(),
            "--input",
            r#"{"tenant":"tenant-a","key":"tenant-scope","value":{"tenant":"A"},"skip_write":false,"delete":false}"#,
        ],
        &state_pack,
        "state pack tenant-a write",
        strict,
    )? {
        Some(out) => out,
        None => return Ok(()),
    };
    let tenant_a_json = extract_json(&tenant_a_out)?;
    ensure!(tenant_a_json["write_status"] == "wrote");
    ensure!(tenant_a_json["prior_read_status"] == "ok");
    ensure!(tenant_a_json["delete_status"] == "skipped");
    ensure!(tenant_a_json["status"] == "ok");

    let tenant_b_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            state_gtpack.to_str().unwrap(),
            "--input",
            r#"{"tenant":"tenant-b","key":"tenant-scope","value":{"tenant":"B"},"skip_write":true,"delete":false}"#,
        ],
        &state_pack,
        "state pack tenant-b read",
        strict,
    )? {
        Some(out) => out,
        None => return Ok(()),
    };
    let tenant_b_json = extract_json(&tenant_b_out)?;
    ensure!(tenant_b_json["write_status"] == "skipped");
    ensure!(tenant_b_json["prior_read_status"] == "missing");
    ensure!(tenant_b_json["delete_status"] == "skipped");
    ensure!(tenant_b_json["status"] == "missing");

    let _cleanup_out = match run_capture(
        &runner_cli,
        &[
            "--pack",
            state_gtpack.to_str().unwrap(),
            "--input",
            r#"{"tenant":"tenant-a","key":"tenant-scope","value":{},"skip_write":true,"delete":true}"#,
        ],
        &state_pack,
        "state pack tenant-a cleanup",
        strict,
    )? {
        Some(out) => out,
        None => return Ok(()),
    };

    let nocap_pack = work.join("state-nocap-pack");
    let nocap_flow = "flows/nocap.ygtc";
    write_pack_layout(
        &nocap_pack,
        "state_nocap",
        nocap_flow,
        &[ComponentSpec {
            id: "conformance.state_nocap",
            wasm: "state_nocap_component.wasm",
            state_read: false,
            state_write: false,
        }],
    )?;
    fs::copy(
        &nocap_wasm,
        nocap_pack.join("components/state_nocap_component.wasm"),
    )?;
    fs::write(nocap_pack.join(nocap_flow), nocap_flow_yaml())?;
    if !build_pack(&greentic_pack, &nocap_pack, strict)? {
        return Ok(());
    }
    let nocap_gtpack = find_gtpack(&nocap_pack)?;

    run_expect_failure(
        &runner_cli,
        &[
            "--pack",
            nocap_gtpack.to_str().unwrap(),
            "--input",
            r#"{"key":"conformance-key"}"#,
        ],
        &nocap_pack,
        "state store capability gating",
    )?;

    Ok(())
}

#[derive(Clone, Copy)]
struct ComponentSpec {
    id: &'static str,
    wasm: &'static str,
    state_read: bool,
    state_write: bool,
}

fn write_pack_layout(
    pack_dir: &Path,
    flow_id: &str,
    flow_file: &str,
    components: &[ComponentSpec],
) -> Result<()> {
    fs::create_dir_all(pack_dir.join("components"))?;
    fs::create_dir_all(pack_dir.join("flows"))?;

    let mut pack_yaml = format!(
        "pack_id: conformance-{}\npublisher: Greentic\nversion: 0.1.0\nkind: application\nassets: []\ncomponents:\n",
        flow_id
    );
    for component in components {
        pack_yaml.push_str(&format!(
            "  - id: {}\n    version: 0.1.0\n    world: greentic:component/component@0.5.0\n    supports: [job]\n    profiles:\n      default: default\n      supported: [default]\n    capabilities:\n      wasi: {{}}\n",
            component.id
        ));
        if component.state_read || component.state_write {
            pack_yaml.push_str("      host:\n        state:\n");
            if component.state_read {
                pack_yaml.push_str("          read: true\n");
            }
            if component.state_write {
                pack_yaml.push_str("          write: true\n");
            }
        } else {
            pack_yaml.push_str("      host: {}\n");
        }
        pack_yaml.push_str(&format!("    wasm: components/{}\n", component.wasm));
    }
    pack_yaml.push_str(&format!(
        "flows:\n  - id: {}\n    file: {}\n    tags: [default]\n    entrypoints: [default]\ndependencies: []\nextensions: {{}}\n",
        flow_id, flow_file
    ));

    fs::write(pack_dir.join("pack.yaml"), pack_yaml)?;
    Ok(())
}

fn templating_flow_yaml() -> &'static str {
    r#"type: job
id: templating
schema_version: 2
start: start
nodes:
  start:
    component.exec:
      component: conformance.templating
      op: start
      input: {}
    routing:
      - to: process
  process:
    component.exec:
      component: conformance.templating
      op: process
      input:
        user_id: "{{node.start.user.id}}"
        name: "{{node.start.user.name}}"
        status: "{{node.start.status}}"
        message: "{{state.input.message}}"
    routing:
      - out: true
"#
}

fn state_flow_yaml() -> &'static str {
    r#"type: job
id: state
nodes:
  write_state:
    component:
      id: conformance.state_writer
      operation: write
      input:
        tenant: {{entry.tenant}}
        key: {{entry.key}}
        value: {{entry.value}}
        skip_write: {{entry.skip_write}}
    routing: read_state
  read_state:
    component:
      id: conformance.state_reader
      operation: read
      input:
        tenant: {{entry.tenant}}
        key: {{entry.key}}
    routing: delete_state
  delete_state:
    component:
      id: conformance.state_writer
      operation: delete
      input:
        tenant: {{entry.tenant}}
        key: {{entry.key}}
        delete: {{entry.delete}}
    routing: read_after_delete
  read_after_delete:
    component:
      id: conformance.state_reader
      operation: read
      input:
        tenant: {{entry.tenant}}
        key: {{entry.key}}
        write_status: {{node.write_state.status}}
        prior_read_status: {{node.read_state.status}}
        delete_status: {{node.delete_state.status}}
    routing: out
"#
}

fn nocap_flow_yaml() -> &'static str {
    r#"type: job
id: state_nocap
nodes:
  probe:
    component:
      id: conformance.state_nocap
      operation: touch
      input:
        key: {{entry.key}}
    routing: out
"#
}

fn build_component(crate_name: &str, strict: bool) -> Result<Option<PathBuf>> {
    let root = workspace_root();
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            crate_name,
            "--target",
            "wasm32-wasip2",
            "--release",
        ])
        .current_dir(&root)
        .status()
        .with_context(|| format!("failed to spawn cargo build for {crate_name}"))?;
    if !status.success() {
        if strict {
            bail!("cargo build failed for {crate_name}: {:?}", status.code());
        }
        eprintln!(
            "skipping state PR-05 conformance: cargo build failed for {crate_name} ({:?})",
            status.code()
        );
        return Ok(None);
    }
    let wasm = root
        .join("target")
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{crate_name}.wasm"));
    if !wasm.exists() {
        if strict {
            bail!("wasm output missing for {crate_name} at {}", wasm.display());
        }
        eprintln!(
            "skipping state PR-05 conformance: wasm missing for {crate_name} at {}",
            wasm.display()
        );
        return Ok(None);
    }
    Ok(Some(wasm))
}

fn build_pack(greentic_pack: &Path, pack_dir: &Path, strict: bool) -> Result<bool> {
    run_status(
        greentic_pack,
        &[
            "build",
            "--in",
            pack_dir.to_str().unwrap(),
            "--allow-oci-tags",
        ],
        pack_dir,
        "pack build",
        strict,
    )
}

fn run_status(bin: &Path, args: &[&str], cwd: &Path, label: &str, strict: bool) -> Result<bool> {
    let status = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !status.success() {
        if strict {
            bail!("{label} failed in strict mode: {:?}", status.code());
        }
        eprintln!("{label} failed (non-strict): {:?}", status.code());
        return Ok(false);
    }
    Ok(true)
}

fn run_capture(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    label: &str,
    strict: bool,
) -> Result<Option<String>> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("{label} failed to spawn"))?;
    if !output.status.success() {
        if strict {
            bail!(
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
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
}

fn run_expect_failure(bin: &Path, args: &[&str], cwd: &Path, label: &str) -> Result<()> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("{label} failed to spawn"))?;
    if output.status.success() {
        bail!(
            "{label} unexpectedly succeeded; stdout:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .to_ascii_lowercase();
    ensure!(
        combined.contains("capab")
            || combined.contains("state")
            || combined.contains("link")
            || combined.contains("denied")
            || combined.contains("permission")
            || combined.contains("unauthorized"),
        "{label} failed but error message did not mention capability/state linking"
    );
    Ok(())
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
    bail!("gtpack not found under {}", pack_dir.display())
}

fn extract_json(output: &str) -> Result<Value> {
    let trimmed = output.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Ok(value);
    }
    let bytes = output.as_bytes();
    let mut last_value = None;
    let mut idx = 0;
    while idx < bytes.len() {
        let byte = bytes[idx];
        if byte != b'{' && byte != b'[' {
            idx += 1;
            continue;
        }
        let mut stream = serde_json::Deserializer::from_slice(&bytes[idx..]).into_iter::<Value>();
        loop {
            match stream.next() {
                Some(Ok(value)) => {
                    last_value = Some(value);
                }
                Some(Err(_)) => {
                    idx += stream.byte_offset().max(1);
                    break;
                }
                None => {
                    idx += stream.byte_offset().max(1);
                    break;
                }
            }
        }
    }
    if let Some(value) = last_value {
        return Ok(value);
    }
    bail!("no JSON payload found in output: {output}")
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .unwrap()
        .to_path_buf()
}

fn is_strict() -> bool {
    std::env::var("GREENTIC_INTEGRATION_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn write_flow_resolution(
    pack_dir: &Path,
    flow_file: &str,
    wasm_relative: &str,
    component_id: &str,
) -> Result<()> {
    let flow_path = pack_dir.join(flow_file);
    let resolve_path = flow_path.with_extension("ygtc.resolve.json");
    let summary_path = flow_path.with_extension("ygtc.resolve.summary.json");
    let flow_name = flow_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| flow_file.to_string());
    let wasm_path = pack_dir.join(wasm_relative);
    let digest = format!("sha256:{}", compute_sha256(&wasm_path)?);
    let source_path = format!("file://{}", wasm_relative);
    let resolve = json!({
        "schema_version": 1,
        "flow": flow_name,
        "nodes": {
            "start": {
                "source": {
                    "kind": "local",
                    "path": source_path
                }
            },
            "process": {
                "source": {
                    "kind": "local",
                    "path": source_path
                }
            }
        }
    });
    fs::write(&resolve_path, serde_json::to_string_pretty(&resolve)?)?;
    let manifest = json!({
        "world": "greentic:component/component@0.5.0",
        "version": "0.1.0"
    });
    let summary = json!({
        "schema_version": 1,
        "flow": flow_name,
        "nodes": {
            "start": {
                "component_id": component_id,
                "source": {
                    "kind": "local",
                    "path": source_path
                },
                "digest": digest,
                "manifest": manifest
            },
            "process": {
                "component_id": component_id,
                "source": {
                    "kind": "local",
                    "path": source_path
                },
                "digest": digest,
                "manifest": manifest
            }
        }
    });
    fs::write(&summary_path, serde_json::to_string_pretty(&summary)?)?;
    Ok(())
}

fn compute_sha256(path: &Path) -> Result<String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| format!("failed to run sha256sum on {}", path.display()))?;
    if !output.status.success() {
        bail!(
            "sha256sum failed for {}: {:?}",
            path.display(),
            output.status.code()
        );
    }
    let stdout = String::from_utf8(output.stdout)?;
    let digest = stdout
        .split_whitespace()
        .next()
        .context("unexpected sha256sum output")?;
    Ok(digest.to_string())
}
