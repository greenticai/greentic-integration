use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tempfile::TempDir;
use zip::ZipArchive;

#[test]
fn e2e_ingress_control() -> Result<()> {
    let root = TempDir::new().context("create temp test root")?;
    let layout = TestLayout::create(root.path())?;

    stage_fixture_packs(&layout)?;
    validate_gtpack(&layout.pack_dir.join("control-chain.gtpack"))?;
    validate_gtpack(&layout.pack_dir.join("fast2flow.gtpack"))?;

    let fast2flow_cli = resolve_binary(
        "GREENTIC_FAST2FLOW_CLI_BIN",
        "greentic-fast2flow",
        "greentic-fast2flow",
    )?;
    let fast2flow_host = resolve_binary(
        "GREENTIC_FAST2FLOW_HOST_BIN",
        "greentic-fast2flow",
        "greentic-fast2flow-routing-host",
    )?;
    if let Some(reason) = incompatible_binary_reason(&fast2flow_cli) {
        eprintln!(
            "skipping e2e_ingress_control: incompatible fast2flow cli {} ({reason})",
            fast2flow_cli.display()
        );
        return Ok(());
    }
    if let Some(reason) = incompatible_binary_reason(&fast2flow_host) {
        eprintln!(
            "skipping e2e_ingress_control: incompatible fast2flow host {} ({reason})",
            fast2flow_host.display()
        );
        return Ok(());
    }

    let scope = env_or_default("GREENTIC_E2E_SCOPE", "demo");
    let flows_path = resolve_flows_fixture()?;

    let build_output = run_fast2flow_cli(
        &fast2flow_cli,
        &[
            "index",
            "build",
            "--scope",
            &scope,
            "--flows",
            &flows_path.display().to_string(),
            "--output",
            &layout.indexes_root.display().to_string(),
        ],
    )?;
    let build_json: Value =
        serde_json::from_str(&build_output).context("parse fast2flow index build JSON")?;
    let entry_count = build_json["entries"]
        .as_array()
        .map(|entries| entries.len())
        .unwrap_or_default();
    if entry_count != 3 {
        bail!("expected 3 indexed demo flows, got {entry_count}. build output:\n{build_output}");
    }

    let inspect_output = run_fast2flow_cli(
        &fast2flow_cli,
        &[
            "index",
            "inspect",
            "--scope",
            &scope,
            "--input",
            &layout.indexes_root.display().to_string(),
        ],
    )?;
    if !(inspect_output.contains(&format!("scope={scope}")) && inspect_output.contains("entries=3"))
    {
        bail!("unexpected fast2flow inspect output:\n{inspect_output}");
    }

    assert_dispatch_target(
        &run_routing_host(
            &fast2flow_host,
            hook_request(
                &scope,
                &layout.indexes_root,
                &env_or_default("GREENTIC_E2E_REFUND_TEXT", "refund please"),
            ),
        )?,
        "demo-support/refund_flow",
    )?;

    assert_dispatch_target(
        &run_routing_host(
            &fast2flow_host,
            hook_request(
                &scope,
                &layout.indexes_root,
                &env_or_default("GREENTIC_E2E_SHIPPING_TEXT", "shipping update"),
            ),
        )?,
        "demo-ops/shipping_flow",
    )?;

    assert_dispatch_target(
        &run_routing_host(
            &fast2flow_host,
            hook_request(
                &scope,
                &layout.indexes_root,
                &env_or_default("GREENTIC_E2E_HELLO_TEXT", "hello there"),
            ),
        )?,
        "demo-assistant/welcome_flow",
    )?;

    assert_continue(&run_routing_host(
        &fast2flow_host,
        hook_request(
            &scope,
            &layout.indexes_root,
            &env_or_default("GREENTIC_E2E_UNKNOWN_TEXT", "abracadabra"),
        ),
    )?)?;

    Ok(())
}

struct TestLayout {
    pack_dir: PathBuf,
    indexes_root: PathBuf,
}

impl TestLayout {
    fn create(root: &Path) -> Result<Self> {
        let bundle_root = root.join("bundle");
        let pack_dir = bundle_root.join("packs");
        let indexes_root = bundle_root.join("indexes");
        let registry_root = bundle_root.join("registry");

        for dir in [&bundle_root, &pack_dir, &indexes_root, &registry_root] {
            fs::create_dir_all(dir)
                .with_context(|| format!("create test directory {}", dir.display()))?;
        }

        fs::write(
            bundle_root.join("greentic.demo.yaml"),
            "version: \"1\"\nproject_root: \"./\"\n",
        )
        .context("write greentic.demo.yaml")?;
        fs::write(registry_root.join("latest.json"), "{}\n").context("write registry stub")?;

        Ok(Self {
            pack_dir,
            indexes_root,
        })
    }
}

fn stage_fixture_packs(layout: &TestLayout) -> Result<()> {
    let ws = workspace_root()?;
    let fixtures = ws.join("fixtures").join("packs");
    let fallback = ws
        .join("crates")
        .join("test-packs")
        .join("echo-pack")
        .join("dist")
        .join("echo-pack.gtpack");

    for source_name in ["control-chain.gtpack", "fast2flow.gtpack"] {
        let source = fixtures.join(source_name);
        let resolved = if source.exists() {
            source
        } else if fallback.exists() {
            fallback.clone()
        } else {
            bail!(
                "missing fixture pack {} and fallback fixture {}. Build local fixtures first:\n  ./scripts/build_e2e_fixtures.sh",
                source.display(),
                fallback.display()
            );
        };
        let dest_pack = layout.pack_dir.join(source_name);
        fs::copy(&resolved, &dest_pack).with_context(|| {
            format!(
                "copy fixture pack {} -> {}",
                resolved.display(),
                dest_pack.display()
            )
        })?;
    }

    Ok(())
}

fn validate_gtpack(path: &Path) -> Result<()> {
    let file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut archive =
        ZipArchive::new(file).with_context(|| format!("read zip archive {}", path.display()))?;

    let mut has_manifest = false;
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .with_context(|| format!("read zip entry {index} from {}", path.display()))?;
        if entry.name() == "manifest.cbor" || entry.name().ends_with("/manifest.cbor") {
            has_manifest = true;
            break;
        }
    }

    if !has_manifest {
        bail!("{} does not contain manifest.cbor", path.display());
    }

    Ok(())
}

fn resolve_flows_fixture() -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("GREENTIC_FAST2FLOW_FLOWS_JSON") {
        let path = PathBuf::from(explicit);
        if path.exists() {
            return Ok(path);
        }
        bail!(
            "GREENTIC_FAST2FLOW_FLOWS_JSON points to missing path {}",
            path.display()
        );
    }

    let path = workspace_root()?
        .join("fixtures")
        .join("fast2flow")
        .join("demo_app_flows.json");
    if path.exists() {
        Ok(path)
    } else {
        bail!(
            "missing local fast2flow flows fixture at {}",
            path.display()
        )
    }
}

fn workspace_root() -> Result<PathBuf> {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .and_then(|p| p.parent())
        .map(Path::to_path_buf)
        .ok_or_else(|| anyhow!("failed to resolve workspace root from CARGO_MANIFEST_DIR"))
}

fn resolve_binary(bin_env: &str, release_name: &str, binary_name: &str) -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var(bin_env) {
        let path = PathBuf::from(explicit);
        if path.exists() {
            return Ok(path);
        }
        bail!("{} points to missing path {}", bin_env, path.display());
    }

    let workspace = workspace_root()?;
    for path in binary_candidates(&workspace, release_name, binary_name) {
        if path.exists() {
            return Ok(path);
        }
    }

    let default_path = workspace
        .join("artifacts")
        .join("fast2flow-release")
        .join("latest")
        .join(binary_name);
    bail!(
        "missing binary {} at {}. Download the latest private release with ./scripts/fetch_fast2flow_release.sh or set {}.",
        binary_name,
        default_path.display(),
        bin_env
    )
}

fn incompatible_binary_reason(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    if bytes.starts_with(b"\x7fELF") && std::env::consts::OS != "linux" {
        return Some(format!(
            "ELF binary on unsupported host {}-{}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ));
    }
    None
}

fn binary_candidates(workspace: &Path, release_name: &str, binary_name: &str) -> Vec<PathBuf> {
    let mut candidates = vec![
        workspace
            .join("artifacts")
            .join("fast2flow-release")
            .join("latest")
            .join(binary_name),
        workspace
            .join("artifacts")
            .join("fast2flow-release")
            .join(release_name)
            .join(binary_name),
    ];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".cache")
                .join("greentic-fast2flow")
                .join("latest")
                .join(binary_name),
        );
    }
    candidates
}

fn run_fast2flow_cli(binary: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new(binary)
        .args(args)
        .output()
        .with_context(|| format!("run {}", binary.display()))?;

    command_output("fast2flow-cli", output)
}

fn run_routing_host(binary: &Path, request: Value) -> Result<Value> {
    let mut child = Command::new(binary)
        .env("FAST2FLOW_LLM_PROVIDER", "disabled")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", binary.display()))?;

    let payload = serde_json::to_vec(&request).context("serialize hook request")?;
    {
        let stdin = child.stdin.as_mut().context("open host stdin")?;
        stdin
            .write_all(&payload)
            .context("write host hook request to stdin")?;
    }

    let output = child
        .wait_with_output()
        .with_context(|| format!("wait for {}", binary.display()))?;
    let stdout = command_output("fast2flow-routing-host", output)?;
    serde_json::from_str(&stdout).with_context(|| format!("parse host output JSON:\n{stdout}"))
}

fn command_output(label: &str, output: std::process::Output) -> Result<String> {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        bail!(
            "{label} failed (status {:?})\nstdout:\n{}\nstderr:\n{}",
            output.status.code(),
            stdout,
            stderr
        );
    }
    Ok(stdout)
}

fn hook_request(scope: &str, indexes_root: &Path, text: &str) -> Value {
    json!({
        "scope": scope,
        "envelope": {
            "text": text,
            "channel": "chat",
            "provider": "demo"
        },
        "session_active": false,
        "input_locale": "en-US",
        "time_budget_ms": 250,
        "registry_path": "/mnt/registry/latest.json",
        "indexes_path": indexes_root.display().to_string(),
        "now_unix_ms": 0
    })
}

fn assert_dispatch_target(output: &Value, expected_target: &str) -> Result<()> {
    let directive = output
        .get("directive")
        .context("host output missing directive")?;
    let kind = directive.get("type").and_then(Value::as_str);
    if kind != Some("dispatch") {
        bail!("expected dispatch directive, got {}", directive);
    }
    let target = directive
        .get("target")
        .and_then(Value::as_str)
        .context("dispatch directive missing target")?;
    if target != expected_target {
        bail!("expected dispatch target {expected_target}, got {target}");
    }
    Ok(())
}

fn assert_continue(output: &Value) -> Result<()> {
    let directive = output
        .get("directive")
        .context("host output missing directive")?;
    let kind = directive.get("type").and_then(Value::as_str);
    if kind != Some("continue") {
        bail!("expected continue directive, got {}", directive);
    }
    Ok(())
}

fn env_or_default(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_string())
}
