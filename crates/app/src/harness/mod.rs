use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::{
    net::TcpStream,
    time::{Duration, Instant, sleep, timeout},
};
use tokio_postgres::NoTls;

pub mod services;
pub use services::{ServiceProcess, StackError, TestStack};
pub mod pack;
pub use pack::{BuildMode, PackBuildResult, PackInstallResult, PackVerifyResult, VerifyMode};
pub mod config_layers;
pub use config_layers::{ConfigLayers, SecretCheck, apply_secrets, load_toml, merge_json};

const NATS_CONTAINER_PORT: u16 = 4222;
const POSTGRES_CONTAINER_PORT: u16 = 5432;

/// Lightweight E2E environment harness that boots Docker Compose dependencies, exposes service
/// URLs, and captures logs/artifacts (preserved on failure).
pub struct TestEnv {
    name: String,
    root: PathBuf,
    logs_dir: PathBuf,
    artifacts_dir: PathBuf,
    compose_file: PathBuf,
    project_name: String,
    nats_url: String,
    db_url: String,
    shutdown: bool,
}

impl TestEnv {
    /// Bring up the harness: prepare directories, start Compose services, and wait for health.
    pub async fn up() -> Result<Self> {
        let name = resolve_test_name();
        let root = workspace_root().join("target").join("e2e").join(&name);
        let logs_dir = root.join("logs");
        let artifacts_dir = root.join("artifacts");
        fs::create_dir_all(&logs_dir)
            .with_context(|| format!("failed to create logs dir at {}", logs_dir.display()))?;
        fs::create_dir_all(&artifacts_dir).with_context(|| {
            format!(
                "failed to create artifacts dir at {}",
                artifacts_dir.display()
            )
        })?;

        let compose_file = workspace_root()
            .join("tests")
            .join("compose")
            .join("compose.e2e.yml");
        if !compose_file.exists() {
            bail!("compose file not found at {}", compose_file.display());
        }

        let project_name = format!("greentic_e2e_{}", sanitize(&name));
        let mut env = Self {
            name,
            root,
            logs_dir,
            artifacts_dir,
            compose_file,
            project_name,
            nats_url: String::new(),
            db_url: String::new(),
            shutdown: false,
        };

        // Best effort cleanup in case a previous run crashed and left containers behind.
        let _ = env.compose_down();

        env.append_log("starting compose stack")?;
        env.compose_up()?;
        let nats_host_port = env.service_host_port("nats", NATS_CONTAINER_PORT)?;
        let postgres_host_port = env.service_host_port("postgres", POSTGRES_CONTAINER_PORT)?;
        env.nats_url = format!("nats://127.0.0.1:{nats_host_port}");
        env.db_url = format!("postgres://postgres:postgres@127.0.0.1:{postgres_host_port}/postgres");

        let snapshot = EnvSnapshot::capture(&env.name, &env.root, &env.nats_url, &env.db_url)?;
        write_json(&env.root.join("env.json"), &snapshot)?;
        write_text(&env.logs_dir.join("READY"), "ok\n")?;

        env.append_log("compose stack up; waiting for ports")?;
        env.wait_for_ports().await?;
        env.append_log("ports ready; waiting for services")?;
        env.ensure_services_ready().await?;
        env.append_log("compose stack ready")?;

        Ok(env)
    }

    pub async fn down(mut self) -> Result<()> {
        self.append_log("capturing compose logs before teardown")?;
        let _ = self.capture_compose_logs();
        self.append_log("stopping compose stack")?;
        self.compose_down()?;
        self.shutdown = true;
        Ok(())
    }

    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts_dir
    }

    pub fn logs_dir(&self) -> &Path {
        &self.logs_dir
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Boot the Greentic stack (runner/deployer/store) if binaries are available locally.
    pub async fn up_stack(&self) -> Result<TestStack, StackError> {
        services::boot_stack(self).await
    }

    pub fn nats_url(&self) -> String {
        self.nats_url.clone()
    }

    pub fn db_url(&self) -> String {
        self.db_url.clone()
    }

    pub async fn healthcheck(&self) -> Result<()> {
        if !self.logs_dir.exists() {
            bail!("logs dir missing at {}", self.logs_dir.display());
        }
        if !self.artifacts_dir.exists() {
            bail!("artifacts dir missing at {}", self.artifacts_dir.display());
        }
        let ready_marker = self.logs_dir.join("READY");
        if !ready_marker.exists() {
            bail!("missing READY marker at {}", ready_marker.display());
        }

        let heartbeat_path = self.logs_dir.join("healthcheck.txt");
        let heartbeat = format!("healthy at {}\n", now_millis());
        write_text(&heartbeat_path, heartbeat)?;

        self.ensure_services_ready().await?;
        Ok(())
    }

    fn compose_up(&self) -> Result<()> {
        self.run_compose(&["up", "-d", "--remove-orphans"])?;
        Ok(())
    }

    fn compose_down(&self) -> Result<()> {
        self.run_compose(&["down", "-v"])?;
        Ok(())
    }

    fn run_compose(&self, args: &[&str]) -> Result<()> {
        eprintln!(
            "[harness] docker compose -f {} {:?} (project={})",
            self.compose_file.display(),
            args,
            self.project_name
        );
        let output = Command::new("docker")
            .arg("compose")
            .arg("-f")
            .arg(&self.compose_file)
            .args(args)
            .env("COMPOSE_PROJECT_NAME", &self.project_name)
            .current_dir(workspace_root())
            .output()
            .context("failed to execute docker compose")?;

        if output.status.success() {
            eprintln!(
                "[harness] docker compose {:?} completed (code {:?})",
                args,
                output.status.code()
            );
            return Ok(());
        }

        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "docker compose {:?} failed (code {:?}): {}",
            args,
            output.status.code(),
            stderr
        );
    }

    fn service_host_port(&self, service: &str, container_port: u16) -> Result<u16> {
        let output = Command::new("docker")
            .arg("compose")
            .arg("-f")
            .arg(&self.compose_file)
            .arg("port")
            .arg(service)
            .arg(container_port.to_string())
            .env("COMPOSE_PROJECT_NAME", &self.project_name)
            .current_dir(workspace_root())
            .output()
            .context("failed to execute docker compose port")?;

        if !output.status.success() {
            bail!(
                "docker compose port {} {} failed (code {:?}): {}",
                service,
                container_port,
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_compose_port(stdout.trim())
            .with_context(|| format!("failed to parse docker compose port output for {service}"))
    }

    async fn wait_for_ports(&self) -> Result<()> {
        let nats_port = parse_port(&self.nats_url, "nats")?;
        let postgres_port = parse_port(&self.db_url, "postgres")?;
        wait_for_port("nats", nats_port, &self.logs_dir, Duration::from_secs(30)).await?;
        wait_for_port("postgres", postgres_port, &self.logs_dir, Duration::from_secs(40)).await?;
        Ok(())
    }

    async fn ensure_services_ready(&self) -> Result<()> {
        ensure_nats_ready(&self.nats_url, &self.logs_dir).await?;
        ensure_postgres_ready(&self.db_url, &self.logs_dir).await?;
        Ok(())
    }

    fn capture_compose_logs(&self) -> Result<()> {
        let log_path = self.logs_dir.join("compose.log");
        let output = Command::new("docker")
            .arg("compose")
            .arg("-f")
            .arg(&self.compose_file)
            .arg("logs")
            .arg("--no-color")
            .env("COMPOSE_PROJECT_NAME", &self.project_name)
            .current_dir(workspace_root())
            .output()
            .context("failed to run docker compose logs")?;

        if output.status.success() {
            fs::write(&log_path, &output.stdout)
                .with_context(|| format!("failed to write {}", log_path.display()))?;
        } else {
            let note = format!(
                "failed to capture compose logs (code {:?}): {}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            );
            write_text(&log_path, note)?;
        }
        Ok(())
    }

    fn append_log(&self, line: &str) -> Result<()> {
        let journal = self.logs_dir.join("harness.log");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&journal)
            .with_context(|| format!("failed to open {}", journal.display()))?;
        writeln!(file, "[{}] {line}", now_millis())
            .with_context(|| format!("failed to write {}", journal.display()))?;
        Ok(())
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        if self.shutdown {
            return;
        }
        let _ = self.append_log("drop without down(); capturing logs and tearing down");
        let _ = self.capture_compose_logs();
        let _ = self.compose_down();
        let marker = self.logs_dir.join("dropped_without_down");
        let _ = fs::write(
            marker,
            "harness dropped without down(); preserving artifacts\n",
        );
    }
}

/// Quick check to see if the Docker CLI and daemon are reachable.
pub fn docker_available() -> bool {
    Command::new("docker")
        .arg("info")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

impl TestEnv {
    /// Per-tenant artifacts directory under target/e2e/<test>/artifacts/tenants/<tenant>.
    pub fn tenant_artifacts_dir(&self, tenant: &str) -> Result<PathBuf> {
        let dir = self.artifacts_dir.join("tenants").join(tenant);
        fs::create_dir_all(&dir)
            .with_context(|| format!("failed to create tenant artifacts dir {}", dir.display()))?;
        Ok(dir)
    }

    /// Write a tenant-scoped secret to artifacts for use in tests.
    pub fn write_tenant_secret(&self, tenant: &str, key: &str, value: &str) -> Result<PathBuf> {
        let dir = self.tenant_artifacts_dir(tenant)?;
        let path = dir.join("secrets.json");
        let contents = if path.exists() {
            let data = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let mut json: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&data).unwrap_or_default();
            json.insert(
                key.to_string(),
                serde_json::Value::String(value.to_string()),
            );
            serde_json::to_string_pretty(&json)?
        } else {
            serde_json::to_string_pretty(&serde_json::json!({ key: value }))?
        };
        fs::write(&path, contents)
            .with_context(|| format!("failed to write {}", path.display()))?;
        Ok(path)
    }
}

#[derive(Debug, Serialize)]
struct EnvSnapshot {
    name: String,
    root: PathBuf,
    workspace: PathBuf,
    nats_url: String,
    db_url: String,
    timestamp_ms: u128,
    current_dir: Option<PathBuf>,
    env_test_name: Option<String>,
}

impl EnvSnapshot {
    fn capture(name: &str, root: &Path, nats_url: &str, db_url: &str) -> Result<Self> {
        let workspace = workspace_root();
        let current_dir = std::env::current_dir().ok();
        Ok(Self {
            name: name.to_string(),
            root: root.to_path_buf(),
            workspace,
            nats_url: nats_url.to_string(),
            db_url: db_url.to_string(),
            timestamp_ms: now_millis(),
            current_dir,
            env_test_name: std::env::var("E2E_TEST_NAME").ok(),
        })
    }
}

fn resolve_test_name() -> String {
    if let Ok(name) = std::env::var("E2E_TEST_NAME") {
        let cleaned = sanitize(&name);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    if let Some(thread_name) = std::thread::current().name() {
        let cleaned = sanitize(thread_name);
        if !cleaned.is_empty() {
            return cleaned;
        }
    }

    format!("e2e-{}", now_millis())
}

fn sanitize(input: &str) -> String {
    let mut out = String::new();
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    out.trim_matches('_').to_string()
}

fn parse_port(url: &str, label: &str) -> Result<u16> {
    let authority = url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(url)
        .split('/')
        .next()
        .unwrap_or(url);
    let port = authority
        .rsplit_once(':')
        .map(|(_, port)| port)
        .ok_or_else(|| anyhow::anyhow!("missing port in {label} url: {url}"))?;
    let port = port.trim_end_matches("/tcp").trim();
    u16::from_str(port).with_context(|| format!("invalid {label} url port in {url}"))
}

fn parse_compose_port(output: &str) -> Result<u16> {
    let port = output
        .rsplit_once(':')
        .map(|(_, port)| port)
        .ok_or_else(|| anyhow::anyhow!("missing port in compose output: {output}"))?;
    let port = port.trim_end_matches("/tcp").trim();
    u16::from_str(port).with_context(|| format!("invalid compose port: {output}"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or_default()
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let data = serde_json::to_vec_pretty(value).context("failed to serialize JSON")?;
    fs::write(path, data).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn write_text(path: &Path, contents: impl AsRef<[u8]>) -> Result<()> {
    fs::write(path, contents).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

async fn wait_for_port(name: &str, port: u16, logs_dir: &Path, timeout_at: Duration) -> Result<()> {
    let start = Instant::now();
    let addr = format!("127.0.0.1:{port}");
    let mut attempts = 0;
    loop {
        match TcpStream::connect(&addr).await {
            Ok(_) => {
                write_probe(logs_dir, name, "port open")?;
                eprintln!(
                    "[harness] {name} port {} open after {:.1?} ({} attempts)",
                    addr,
                    start.elapsed(),
                    attempts
                );
                return Ok(());
            }
            Err(err) => {
                if start.elapsed() > timeout_at {
                    bail!("{name} did not open port {addr} in time: {err}");
                }
                attempts += 1;
                if attempts % 8 == 0 {
                    eprintln!(
                        "[harness] waiting for {name} port {} (elapsed {:.1?}, last err: {})",
                        addr,
                        start.elapsed(),
                        err
                    );
                }
                sleep(Duration::from_millis(250)).await;
            }
        }
    }
}

#[allow(unused_assignments)]
async fn ensure_nats_ready(url: &str, logs_dir: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut last_err: Option<anyhow::Error> = None;
    let mut attempts = 0;
    loop {
        match async_nats::connect(url).await {
            Ok(client) => {
                if let Err(err) = client.flush().await {
                    last_err = Some(err.into());
                } else {
                    write_probe(logs_dir, "nats", "ready")?;
                    return Ok(());
                }
            }
            Err(err) => {
                last_err = Some(err.into());
            }
        }

        if Instant::now() > deadline {
            if let Some(err) = last_err.take() {
                return Err(err);
            }
            return Err(anyhow::anyhow!("NATS readiness timed out"));
        }
        attempts += 1;
        if attempts % 5 == 0 {
            eprintln!(
                "[harness] waiting for NATS ready at {} (elapsed {:.1?})",
                url,
                attempts as f32 * 0.3
            );
        }
        sleep(Duration::from_millis(300)).await;
    }
}

#[allow(unused_assignments)]
async fn ensure_postgres_ready(url: &str, logs_dir: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut last_err: Option<anyhow::Error> = None;
    let mut attempts = 0;
    loop {
        match tokio_postgres::connect(url, NoTls).await {
            Ok((client, connection)) => {
                let connection_task = tokio::spawn(async move {
                    let _ = connection.await;
                });
                match timeout(Duration::from_secs(5), client.simple_query("SELECT 1")).await {
                    Ok(Ok(_)) => {
                        connection_task.abort();
                        write_probe(logs_dir, "postgres", "ready")?;
                        return Ok(());
                    }
                    Ok(Err(err)) => last_err = Some(err.into()),
                    Err(err) => last_err = Some(err.into()),
                }
            }
            Err(err) => last_err = Some(err.into()),
        }

        if Instant::now() > deadline {
            if let Some(err) = last_err.take() {
                return Err(err);
            }
            return Err(anyhow::anyhow!("postgres readiness timed out"));
        }
        attempts += 1;
        if attempts % 5 == 0 {
            eprintln!(
                "[harness] waiting for postgres ready at {} (elapsed {:.1?})",
                url,
                attempts as f32 * 0.3
            );
        }
        sleep(Duration::from_millis(300)).await;
    }
}

fn write_probe(logs_dir: &Path, service: &str, message: &str) -> Result<()> {
    let probe = logs_dir.join(format!("probe-{service}.log"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&probe)
        .with_context(|| format!("failed to open {}", probe.display()))?;
    writeln!(file, "[{}] {message}", now_millis())
        .with_context(|| format!("failed to write {}", probe.display()))?;
    Ok(())
}
