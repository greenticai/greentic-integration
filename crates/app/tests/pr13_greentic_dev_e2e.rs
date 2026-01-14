use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use pathdiff::diff_paths;
use serde_yaml_bw::{Mapping, Sequence, Value};
use std::env;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;
use walkdir::WalkDir;
#[path = "support/mod.rs"]
mod support;

/// End-to-end greentic-dev workflow: scaffold component -> build -> pack -> add-step -> build -> run.
#[test]
fn pr13_greentic_dev_component_pack_flow() -> Result<()> {
    let tmp = tempdir().context("tempdir")?;
    let work = tmp.path();
    println!("workspace: {}", work.display());
    let strict = strict_dev_e2e() || std::env::var("CI").is_ok();

    let greentic_dev =
        match support::ensure_tool("greentic-dev", "greentic-dev", strict, "greentic-dev")? {
            Some(p) => p,
            None => return Ok(()),
        };
    let packc = match support::ensure_tool("packc", "packc", strict, "packc")? {
        Some(p) => p,
        None => return Ok(()),
    };
    // Provide a local greentic-dev config so commands requiring a distributor profile succeed.
    let home_dir = work.join("home");
    let xdg_config = work.join(".config");
    let xdg_data = work.join(".local/share");
    let xdg_state = work.join(".local/state");
    let xdg_cache = work.join(".cache");
    for d in [&xdg_config, &xdg_data, &xdg_state, &xdg_cache] {
        fs::create_dir_all(d)?;
    }
    let config_path = xdg_config.join("greentic-dev").join("config.toml");
    fs::create_dir_all(config_path.parent().unwrap())?;
    let fixtures_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into()))
        })
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
    // Also write to HOME/.config in case greentic-dev ignores XDG override.
    let home_config = home_dir.join(".config/greentic-dev/config.toml");
    if let Some(parent) = home_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&home_config, &config_contents)?;
    // Some builds may still look for ~/.config/greentic/config.toml; mirror there as well.
    let legacy_home_config = home_dir.join(".config/greentic/config.toml");
    if let Some(parent) = legacy_home_config.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&legacy_home_config, &config_contents)?;
    // greentic-dev config subcommands operate on ~/.greentic/config.toml; mirror with legacy shape.
    let dot_greentic_config = home_dir.join(".greentic/config.toml");
    if let Some(parent) = dot_greentic_config.parent() {
        fs::create_dir_all(parent)?;
    }
    let legacy_config = r#"[distributor]
default_profile = { name = "default", kind = "pack", store = { kind = "local", path = "__STORE_PATH__" } }

[distributor.profiles.default]
kind = "pack"
store = { kind = "local", path = "__STORE_PATH__" }
"#
    .replace("__STORE_PATH__", store_path.to_str().unwrap());
    fs::write(&dot_greentic_config, &legacy_config)?;
    println!(
        "greentic-dev config contents:\n{}",
        config_contents.replace("__STORE_PATH__", "<store>")
    );
    println!(
        "greentic-dev config at {} (and {})",
        config_path.display(),
        home_config.display()
    );
    assert!(
        config_path.exists(),
        "expected greentic-dev config at {}",
        config_path.display()
    );
    let envs = base_env(
        &home_dir,
        &xdg_config,
        &xdg_data,
        &xdg_state,
        &xdg_cache,
        &config_path,
    );

    // 1) Scaffold component
    let comp_dir = work.join("demo-comp");
    match run_cmd_capture_optional(
        &greentic_dev,
        &[
            "component",
            "new",
            "--name",
            "demo-comp",
            "--non-interactive",
            "--no-git",
            "--path",
            comp_dir.to_str().unwrap(),
        ],
        work,
        "component new",
        &envs,
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            if strict {
                anyhow::bail!(
                    "greentic-dev component new skipped (likely missing deps) and GREENTIC_DEV_E2E_STRICT=1"
                );
            }
            return Ok(());
        }
        Err(err) => {
            if strict {
                anyhow::bail!("greentic-dev component new failed in strict mode: {err}");
            } else {
                eprintln!("skipping component new due to error: {err}");
                return Ok(());
            }
        }
    }

    // Replace handle_message with deterministic uppercase transform.
    let src = comp_dir.join("src/lib.rs");
    let code = fs::read_to_string(&src).context("read lib.rs")?;
    let patched = code.replace(
        "format!(\"demo-comp::{operation} => {}\", input.trim())",
        "format!(\"HELLO::{}\", input.trim().to_ascii_uppercase())",
    );
    fs::write(&src, patched).context("write lib.rs")?;

    // Build component.
    match run_cmd_capture_optional(
        &greentic_dev,
        &[
            "component",
            "build",
            "--manifest",
            comp_dir.to_str().unwrap(),
        ],
        work,
        "component build",
        &envs,
    ) {
        Ok(Some(_)) => {}
        Ok(None) => {
            if strict {
                anyhow::bail!(
                    "greentic-dev component build skipped (likely missing deps) and GREENTIC_DEV_E2E_STRICT=1"
                );
            }
            return Ok(());
        }
        Err(err) => {
            if strict {
                anyhow::bail!("greentic-dev component build failed in strict mode: {err}");
            } else {
                eprintln!("skipping component build due to error: {err}");
                return Ok(());
            }
        }
    }
    let built_wasm = comp_dir
        .join("target/wasm32-wasip2/release/demo_comp.wasm")
        .canonicalize()
        .context("locate built wasm")?;

    // Some greentic-dev builds resolve components via a distributor API on localhost:8080; provide
    // a minimal stub so the test stays offline-friendly.
    let _stub = DistributorStub::start("127.0.0.1:8080", built_wasm.to_string_lossy().to_string());

    // 2) Scaffold pack.
    let pack_dir = work.join("demo-pack");
    run_cmd(
        &greentic_dev,
        &[
            "pack",
            "new",
            "--dir",
            pack_dir.to_str().unwrap(),
            "demo-pack",
        ],
        work,
        "pack new",
        &envs,
    )?;

    // Copy component wasm into pack and register in pack.yaml.
    let pack_wasm = pack_dir.join("components/demo_comp.wasm");
    fs::copy(&built_wasm, &pack_wasm).context("copy component wasm into pack")?;
    append_component_to_pack(&pack_dir, &pack_wasm)?;

    // Determine coordinate from manifest (id@version).
    let manifest_path = comp_dir.join("component.manifest.json");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&manifest_path)?).context("parse manifest")?;
    let coord_id = manifest
        .get("id")
        .and_then(|v| v.as_str())
        .context("manifest missing id")?;
    let coord_ver = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .context("manifest missing version")?;
    let coordinate = format!("repo://{coord_id}@{coord_ver}");

    // 3) Insert new component into flow via add-step.
    // Insert new component into flow; must succeed.
    let add_output = Command::new(&greentic_dev)
        .args([
            "flow",
            "add-step",
            "main",
            "--manifest",
            comp_dir.join("component.manifest.json").to_str().unwrap(),
            "--coordinate",
            &coordinate,
            "--profile",
            "default",
            "--after",
            "start",
        ])
        .envs(envs.iter().cloned())
        .current_dir(&pack_dir)
        .output()
        .context("flow add-step failed to spawn")?;
    if !add_output.status.success() {
        let stderr = String::from_utf8_lossy(&add_output.stderr);
        anyhow::bail!(
            "flow add-step failed: {:?}\nstderr:\n{}",
            add_output.status.code(),
            stderr
        );
    }

    // 4) Build + validate pack.
    if let Err(err) = run_cmd(
        &greentic_dev,
        &["pack", "build", "--in", ".", "--offline"],
        &pack_dir,
        "pack build",
        &envs,
    ) {
        if strict {
            anyhow::bail!("pack build failed in strict mode: {err}");
        } else {
            eprintln!("skipping pack verify/run due to build error: {err}");
            return Ok(());
        }
    }
    verify_pack(&packc, &pack_dir, &envs, strict)?;
    // 5) (Optional) Run pack with deterministic input if pack was built.
    if let Ok(gtpack) = find_gtpack(&pack_dir) {
        let run_out = match run_cmd_capture(
            &greentic_dev,
            &[
                "pack",
                "run",
                "--pack",
                gtpack.to_str().unwrap(),
                "--input",
                r#"{"text":"world"}"#,
                "--json",
                "--artifacts",
                pack_dir.join("run-artifacts").to_str().unwrap(),
            ],
            &pack_dir,
            "pack run",
            &envs,
        ) {
            Ok(out) => out,
            Err(err) => {
                if strict {
                    anyhow::bail!("greentic-dev pack run failed in strict mode: {err}");
                } else {
                    eprintln!("skipping output assertion due to run error: {err}");
                    return Ok(());
                }
            }
        };
        println!("pack run output:\n{run_out}");
        assert!(
            run_out.contains("HELLO::WORLD"),
            "expected transformed output in pack run"
        );
    }

    Ok(())
}

fn strict_dev_e2e() -> bool {
    std::env::var("GREENTIC_DEV_E2E_STRICT")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn base_env(
    home: &Path,
    xdg_config: &Path,
    xdg_data: &Path,
    xdg_state: &Path,
    xdg_cache: &Path,
    config: &Path,
) -> Vec<(String, String)> {
    vec![
        ("HOME".into(), home.to_string_lossy().into_owned()),
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
            config.to_string_lossy().into_owned(),
        ),
    ]
}

fn run_cmd(
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
        .with_context(|| {
            format!(
                "{label} failed to spawn (cmd: {} {:?})",
                bin.display(),
                args
            )
        })?;
    if !status.success() {
        anyhow::bail!("{label} failed with status {:?}", status.code());
    }
    Ok(())
}

fn run_cmd_capture_optional(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    label: &str,
    envs: &[(String, String)],
) -> Result<Option<String>> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .output()
        .with_context(|| {
            format!(
                "{label} failed to spawn (cmd: {} {:?})",
                bin.display(),
                args
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let require_online = std::env::var("GREENTIC_DEV_E2E_REQUIRE_ONLINE").is_ok();
        if !require_online
            && (stderr.contains("Could not resolve host") || stderr.contains("failed to get"))
        {
            eprintln!("skipping {label}: probable offline/cargo fetch issue\n{stderr}");
            return Ok(None);
        }
        anyhow::bail!("{label} failed: {}\n{}", output.status, stderr);
    }

    Ok(Some(String::from_utf8_lossy(&output.stdout).to_string()))
}

fn run_cmd_capture(
    bin: &Path,
    args: &[&str],
    cwd: &Path,
    label: &str,
    envs: &[(String, String)],
) -> Result<String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .envs(envs.iter().cloned())
        .output()
        .with_context(|| {
            format!(
                "{label} failed to spawn (cmd: {} {:?})",
                bin.display(),
                args
            )
        })?;
    if !output.status.success() {
        anyhow::bail!(
            "{label} failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn append_component_to_pack(pack_dir: &Path, wasm_path: &Path) -> Result<()> {
    let pack_yaml = pack_dir.join("pack.yaml");
    let mut doc: Value =
        serde_yaml_bw::from_str(&fs::read_to_string(&pack_yaml)?).context("parse pack.yaml")?;
    let mapping = doc
        .as_mapping_mut()
        .context("pack.yaml should be a mapping")?;
    let mut comps = Sequence::new();
    comps.push(serde_yaml_bw::to_value(Mapping::from_iter([
        (Value::from("id"), Value::from("demo-pack.demo-comp")),
        (Value::from("version"), Value::from("0.1.0")),
        (
            Value::from("world"),
            Value::from("greentic:component/component@0.5.0"),
        ),
        (
            Value::from("supports"),
            Value::Sequence({
                let mut seq = Sequence::new();
                seq.push(Value::from("messaging"));
                seq
            }),
        ),
        (
            Value::from("profiles"),
            serde_yaml_bw::to_value(Mapping::from_iter([
                (Value::from("default"), Value::from("stateless")),
                (
                    Value::from("supported"),
                    Value::Sequence({
                        let mut seq = Sequence::new();
                        seq.push(Value::from("stateless"));
                        seq
                    }),
                ),
            ]))?,
        ),
        (
            Value::from("capabilities"),
            serde_yaml_bw::to_value(Mapping::from_iter([
                (
                    Value::from("wasi"),
                    serde_yaml_bw::to_value(Mapping::from_iter([
                        (
                            Value::from("filesystem"),
                            serde_yaml_bw::to_value(Mapping::from_iter([
                                (Value::from("mode"), Value::from("none")),
                                (Value::from("mounts"), Value::Sequence(Sequence::new())),
                            ]))?,
                        ),
                        (Value::from("random"), Value::from(true)),
                        (Value::from("clocks"), Value::from(true)),
                    ]))?,
                ),
                (Value::from("host"), Value::Mapping(Mapping::new())),
            ]))?,
        ),
        (
            Value::from("wasm"),
            Value::from(path_for_pack(pack_dir, wasm_path)?),
        ),
    ]))?);
    mapping.insert(Value::from("components"), Value::Sequence(comps));

    fs::write(&pack_yaml, serde_yaml_bw::to_string(&doc)?).context("write pack.yaml")?;
    Ok(())
}

fn path_for_pack(pack_dir: &Path, wasm_path: &Path) -> Result<String> {
    let rel = diff_paths(wasm_path, pack_dir).context("diff paths")?;
    Ok(rel.to_string_lossy().to_string())
}

fn find_gtpack(pack_dir: &Path) -> Result<PathBuf> {
    for entry in WalkDir::new(pack_dir.join("target"))
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

fn verify_pack(
    packc: &Path,
    pack_dir: &Path,
    envs: &[(String, String)],
    strict: bool,
) -> Result<()> {
    if packc_supports_allow_unsigned(packc, envs)? {
        let status = Command::new(packc)
            .args(["verify", "--allow-unsigned", "--pack", "."])
            .envs(envs.iter().cloned())
            .current_dir(pack_dir)
            .status()
            .context("packc verify (allow-unsigned) failed to spawn")?;
        if !status.success() {
            if strict {
                anyhow::bail!("packc verify failed in strict mode: {:?}", status.code());
            } else {
                eprintln!(
                    "skipping pack verify/build/run due to error: {:?}",
                    status.code()
                );
                return Ok(());
            }
        }
        return Ok(());
    }

    // Fallback: sign then verify with generated Ed25519 keypair.
    let sk = pack_dir.join("tmp-dev-signing").join("sk.pem");
    let pk = sk.with_file_name("pk.pem");
    if let Some(parent) = sk.parent() {
        fs::create_dir_all(parent)?;
    }

    if !Command::new("openssl")
        .args([
            "genpkey",
            "-algorithm",
            "ed25519",
            "-out",
            sk.to_str().unwrap(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        if strict {
            anyhow::bail!("openssl keygen failed and allow-unsigned unsupported");
        } else {
            eprintln!("skipping verify: openssl keygen failed and allow-unsigned unsupported");
            return Ok(());
        }
    }
    if !Command::new("openssl")
        .args([
            "pkey",
            "-in",
            sk.to_str().unwrap(),
            "-pubout",
            "-out",
            pk.to_str().unwrap(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        if strict {
            anyhow::bail!("openssl pubkey export failed");
        } else {
            eprintln!("skipping verify: openssl pubkey export failed");
            return Ok(());
        }
    }

    let sign_status = Command::new(packc)
        .args(["sign", "--pack", ".", "--key", sk.to_str().unwrap()])
        .envs(envs.iter().cloned())
        .current_dir(pack_dir)
        .status()
        .context("packc sign failed to spawn")?;
    if !sign_status.success() {
        if strict {
            anyhow::bail!("packc sign failed in strict mode: {:?}", sign_status.code());
        } else {
            eprintln!(
                "skipping verify: packc sign failed {:?}",
                sign_status.code()
            );
            return Ok(());
        }
    }
    let verify_status = Command::new(packc)
        .args(["verify", "--pack", ".", "--key", pk.to_str().unwrap()])
        .envs(envs.iter().cloned())
        .current_dir(pack_dir)
        .status()
        .context("packc verify failed to spawn")?;
    if !verify_status.success() {
        if strict {
            anyhow::bail!(
                "packc verify failed in strict mode: {:?}",
                verify_status.code()
            );
        } else {
            eprintln!(
                "skipping pack verify/build/run due to error: {:?}",
                verify_status.code()
            );
        }
    }
    Ok(())
}

fn packc_supports_allow_unsigned(packc: &Path, envs: &[(String, String)]) -> Result<bool> {
    let help = Command::new(packc)
        .args(["verify", "--help"])
        .envs(envs.iter().cloned())
        .output()
        .context("packc verify --help failed")?;
    let stdout = String::from_utf8_lossy(&help.stdout);
    let stderr = String::from_utf8_lossy(&help.stderr);
    Ok(stdout.contains("allow-unsigned") || stderr.contains("allow-unsigned"))
}

struct DistributorStub {
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl DistributorStub {
    fn start(addr: &str, artifact_path: String) -> Option<Self> {
        let listener = TcpListener::bind(addr).ok()?;
        listener.set_nonblocking(true).ok()?;

        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            thread::spawn(move || {
                Self::serve(listener, stop, artifact_path);
            })
        };

        Some(Self {
            stop,
            handle: Some(handle),
        })
    }

    fn serve(listener: TcpListener, stop: Arc<AtomicBool>, artifact_path: String) {
        while !stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    Self::handle_conn(&mut stream, &artifact_path);
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(_) => break,
            }
        }
    }

    fn handle_conn(stream: &mut TcpStream, artifact_path: &str) {
        let mut buf = [0u8; 4096];
        // Read whatever fits; ignore the body since we only need to respond.
        let _ = stream.read(&mut buf);

        let body = serde_json::json!({
            "status": "ready",
            "digest": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            "artifact": { "kind": "file_path", "path": artifact_path },
            "signature": { "verified": true, "signer": "stub", "extra": {} },
            "cache": {
                "size_bytes": 0,
                "last_used_utc": "1970-01-01T00:00:00Z",
                "last_refreshed_utc": "1970-01-01T00:00:00Z"
            },
            "secret_requirements": []
        })
        .to_string();

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

impl Drop for DistributorStub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
