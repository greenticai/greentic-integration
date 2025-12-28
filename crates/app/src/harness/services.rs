use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use tokio::{
    net::TcpStream,
    time::{Instant, sleep},
};

use super::{now_millis, workspace_root, write_text};

const RUNNER_PORT: u16 = 3333;

#[derive(Debug)]
pub struct ServiceProcess {
    name: String,
    log_path: PathBuf,
    child: Child,
}

impl ServiceProcess {
    pub fn spawn(
        name: &str,
        binary: &Path,
        args: &[&str],
        envs: &[(&str, &str)],
        logs_dir: &Path,
    ) -> Result<Self> {
        let log_path = logs_dir.join(format!("{name}.log"));
        let log_file = File::create(&log_path)
            .with_context(|| format!("failed to create log file {}", log_path.display()))?;
        let log_err = log_file
            .try_clone()
            .with_context(|| format!("failed to clone log file handle {}", log_path.display()))?;

        let mut cmd = Command::new(binary);
        cmd.args(args)
            .envs(envs.iter().map(|(k, v)| (*k, *v)))
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(log_err));

        let child = cmd.spawn().with_context(|| {
            format!("failed to start service {name} using {}", binary.display())
        })?;

        Ok(Self {
            name: name.to_string(),
            log_path,
            child,
        })
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn ensure_running(&mut self) -> Result<()> {
        if let Some(status) = self.child.try_wait()? {
            bail!(
                "service {} exited early with status {:?}",
                self.name,
                status.code()
            );
        }
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if let Some(_status) = self.child.try_wait()? {
            return Ok(());
        }
        self.child
            .kill()
            .with_context(|| format!("failed to kill {}", self.name))?;
        let _ = self.child.wait();
        Ok(())
    }
}

pub struct TestStack {
    runner: ServiceProcess,
}

impl TestStack {
    pub async fn healthcheck(&mut self, logs_dir: &Path) -> Result<()> {
        let start = Instant::now();
        let timeout = Duration::from_secs(20);
        let addr = format!("127.0.0.1:{RUNNER_PORT}");
        loop {
            self.runner.ensure_running()?;
            match TcpStream::connect(&addr).await {
                Ok(_) => {
                    write_probe(logs_dir, "runner", "port open")?;
                    break;
                }
                Err(err) => {
                    if start.elapsed() > timeout {
                        bail!(
                            "runner did not open port {} in time: {} (log: {})",
                            addr,
                            err,
                            self.runner.log_path().display()
                        );
                    }
                    sleep(Duration::from_millis(250)).await;
                }
            }
        }
        Ok(())
    }

    pub async fn down(mut self) -> Result<()> {
        self.runner.stop()?;
        Ok(())
    }
}

pub enum StackError {
    MissingBinary {
        name: &'static str,
        searched: Vec<PathBuf>,
    },
    Startup(anyhow::Error),
}

impl std::fmt::Display for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackError::MissingBinary { name, .. } => write!(f, "missing binary {}", name),
            StackError::Startup(err) => write!(f, "{err}"),
        }
    }
}

impl std::fmt::Debug for StackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StackError::MissingBinary { name, searched } => f
                .debug_struct("MissingBinary")
                .field("name", name)
                .field("searched", searched)
                .finish(),
            StackError::Startup(err) => f.debug_tuple("Startup").field(err).finish(),
        }
    }
}

impl std::error::Error for StackError {}

pub async fn boot_stack(env: &crate::harness::TestEnv) -> Result<TestStack, StackError> {
    // On non-Linux hosts, fall back to a simple HTTP stub so the test can run locally.
    if std::env::consts::OS != "linux" {
        let port_str = RUNNER_PORT.to_string();
        let stub_args = ["-m", "http.server", &port_str];
        let stub = ServiceProcess::spawn(
            "runner-stub",
            Path::new("python3"),
            &stub_args,
            &[],
            env.logs_dir(),
        )
        .map_err(StackError::Startup)?;
        write_text(
            &env.logs_dir().join("stack-info.log"),
            format!(
                "runner stub (python) listening on 127.0.0.1:{}\nstarted at: {}\n",
                port_str,
                now_millis()
            ),
        )
        .map_err(StackError::Startup)?;
        return Ok(TestStack { runner: stub });
    }

    let runner_bin = locate_binary("greentic-runner");
    if runner_bin.is_none() {
        return Err(StackError::MissingBinary {
            name: "greentic-runner",
            searched: binary_candidates("greentic-runner"),
        });
    }
    let runner_bin = runner_bin.unwrap();
    if !is_binary_compatible(&runner_bin) {
        return Err(StackError::MissingBinary {
            name: "greentic-runner",
            searched: binary_candidates("greentic-runner"),
        });
    }

    let config_dir = env.root().join("config");
    fs::create_dir_all(&config_dir).map_err(|e| StackError::Startup(e.into()))?;

    let bindings_path = workspace_root().join("configs").join("demo_local.yaml");
    if !bindings_path.exists() {
        return Err(StackError::Startup(anyhow::anyhow!(
            "missing runner bindings at {}",
            bindings_path.display()
        )));
    }

    let port_str = RUNNER_PORT.to_string();
    let bindings_str = bindings_path
        .to_str()
        .ok_or_else(|| StackError::Startup(anyhow::anyhow!("invalid bindings path")))?;
    let runner_args = [
        "--bindings",
        bindings_str,
        "--config",
        bindings_str,
        "--allow-dev",
        "--port",
        &port_str,
    ];
    let root_buf = workspace_root();
    let root_str = root_buf
        .to_str()
        .ok_or_else(|| StackError::Startup(anyhow::anyhow!("invalid workspace root")))?;
    let state_dir = env.root().join("runner_state");
    let cache_dir = env.root().join("runner_cache");
    let log_dir = env.logs_dir().join("runner");
    fs::create_dir_all(&state_dir).map_err(|e| StackError::Startup(e.into()))?;
    fs::create_dir_all(&cache_dir).map_err(|e| StackError::Startup(e.into()))?;
    fs::create_dir_all(&log_dir).map_err(|e| StackError::Startup(e.into()))?;
    let runner_env = [
        ("GREENTIC_ROOT".to_string(), root_str.to_string()),
        (
            "GREENTIC_STATE_DIR".to_string(),
            state_dir.to_str().unwrap_or("").to_string(),
        ),
        (
            "GREENTIC_CACHE_DIR".to_string(),
            cache_dir.to_str().unwrap_or("").to_string(),
        ),
        (
            "GREENTIC_LOG_DIR".to_string(),
            log_dir.to_str().unwrap_or("").to_string(),
        ),
        ("RUST_LOG".to_string(), "info".to_string()),
        ("GREENTIC_LOG".to_string(), "info".to_string()),
    ];
    let runner = ServiceProcess::spawn(
        "runner",
        &runner_bin,
        &runner_args,
        &runner_env
            .iter()
            .map(|(k, v)| (k.as_ref(), v.as_ref()))
            .collect::<Vec<_>>(),
        env.logs_dir(),
    )
    .map_err(StackError::Startup)?;

    write_text(
        &env.logs_dir().join("stack-info.log"),
        format!(
            "runner binary: {}\nstarted at: {}\n",
            runner_bin.display(),
            now_millis()
        ),
    )
    .map_err(StackError::Startup)?;

    Ok(TestStack { runner })
}

fn locate_binary(name: &str) -> Option<PathBuf> {
    binary_candidates(name)
        .into_iter()
        .find(|candidate| candidate.exists() && is_binary_compatible(candidate))
}

fn is_binary_compatible(path: &Path) -> bool {
    // Quick compatibility guard: skip obviously wrong OS/arch binaries.
    if let Some(p) = path.to_str() {
        if std::env::consts::OS != "linux" && p.contains("linux") {
            return false;
        }
        if std::env::consts::OS == "linux" && (p.contains("darwin") || p.contains("macos")) {
            return false;
        }
        let arch = std::env::consts::ARCH;
        if arch == "aarch64" && (p.contains("x86_64") || p.contains("amd64")) {
            return false;
        }
        if arch == "x86_64" && (p.contains("aarch64") || p.contains("arm64")) {
            return false;
        }
    }
    // Ensure the binary is executable.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path)
            && meta.permissions().mode() & 0o111 == 0
        {
            return false;
        }
    }
    true
}

fn binary_candidates(name: &str) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let root = workspace_root();
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let platform_dir = format!("{os}-{arch}");
    paths.push(root.join("tests/bin").join(&platform_dir).join(name));

    if os == "macos" {
        // Prefer exact arch first, then common aliases.
        paths.push(
            root.join("tests/bin")
                .join(format!("macos-{arch}"))
                .join(name),
        );
        paths.push(
            root.join("tests/bin")
                .join(format!("darwin-{arch}"))
                .join(name),
        );
        if arch == "aarch64" {
            paths.push(root.join("tests/bin/macos-arm64").join(name));
            paths.push(root.join("tests/bin/darwin-arm64").join(name));
        }
        if arch == "x86_64" {
            paths.push(root.join("tests/bin/macos-x86_64").join(name));
            paths.push(root.join("tests/bin/darwin-x86_64").join(name));
        }
    }
    // Legacy/linux-first fallbacks (CI artifacts today live here).
    paths.push(root.join("tests/bin/linux-x86_64").join(name));
    paths.push(root.join("tests/bin").join(name));
    paths.push(root.join("target/release").join(name));
    paths.push(root.join("target/debug").join(name));
    if let Ok(cargo_home) = std::env::var("CARGO_HOME")
        .or_else(|_| std::env::var("HOME").map(|h| format!("{h}/.cargo")))
    {
        paths.push(PathBuf::from(cargo_home).join("bin").join(name));
    }
    if let Ok(path_var) = std::env::var("PATH") {
        for dir in path_var.split(std::path::MAIN_SEPARATOR) {
            if dir.is_empty() {
                continue;
            }
            paths.push(PathBuf::from(dir).join(name));
        }
    }
    paths
}

fn write_probe(logs_dir: &Path, service: &str, message: &str) -> Result<()> {
    let probe = logs_dir.join(format!("probe-{service}.log"));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&probe)?;
    writeln!(file, "[{}] {message}", now_millis())?;
    Ok(())
}
