mod deployment;
mod path_safety;
mod session;

use std::{fs, net::SocketAddr, path::PathBuf, process::Command as ProcessCommand, sync::Arc};

use anyhow::{Context, Result, anyhow, bail};
use axum::{
    Extension, Json, Router,
    extract::Query,
    http::StatusCode,
    routing::{get, post},
};
use camino::{Utf8Path, Utf8PathBuf};
use clap::{Args, Parser, Subcommand, ValueEnum};
use directories::ProjectDirs;
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use notify::{Event, RecommendedWatcher, RecursiveMode, Watcher, recommended_watcher};
use once_cell::sync::Lazy;
use parking_lot::RwLock;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use tokio::{net::TcpListener, signal, sync::mpsc, task::JoinSet};
use tracing::{error, info, trace, warn};
use uuid::Uuid;
use which::which;

use crate::deployment::{
    ChannelPlan, DeploymentPlan, MessagingPlan, MessagingSubjectPlan, RunnerPlan, TelemetryPlan,
};
use crate::path_safety::normalize_under_root;
use crate::session::{
    FileSessionStore, InMemorySessionStore, SessionFilter, SessionRecord, SessionStore,
    SessionUpsert,
};

static APP_NAME: &str = "greentic-integration";
static DEFAULT_CONFIG: Lazy<AppConfig> = Lazy::new(AppConfig::default);

#[derive(Parser, Debug)]
#[command(
    name = "greentic-integration",
    version,
    about = "Greentic integration harness CLI"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Run the integration HTTP/WebSocket bridge
    Serve(ServeArgs),
    /// Helper utilities for pack management
    Packs {
        #[command(subcommand)]
        command: PacksCommand,
    },
    /// Session-store maintenance commands
    Sessions {
        #[command(subcommand)]
        command: SessionCommand,
    },
    /// Runner proxy utilities
    Runner {
        #[command(subcommand)]
        command: RunnerCommandCli,
    },
    /// Run .gtest scripts via greentic-integration-tester
    Gtest(GtestArgs),
}

#[derive(Args, Debug)]
struct ServeArgs {
    /// Path to the configuration file (defaults to config/dev.toml)
    #[arg(long, value_name = "PATH")]
    config: Option<Utf8PathBuf>,
    /// Enable pack hot-reload (dev only)
    #[arg(long)]
    watch: bool,
}

#[derive(Subcommand, Debug)]
enum PacksCommand {
    /// Validate pack manifests under the configured packs root
    Validate,
    /// List available packs discovered under the configured root
    List(PackListArgs),
    /// Rebuild the pack index locally or via HTTP
    Reload(ReloadArgs),
    /// Infer a base deployment plan for a pack and print it
    Plan(PlanArgs),
}

#[derive(Args, Debug, Default)]
struct PackListArgs {
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    team: Option<String>,
    #[arg(long)]
    user: Option<String>,
}

#[derive(Args, Debug)]
struct GtestArgs {
    /// Path to the .gtest script or directory
    #[arg(long, value_name = "PATH")]
    test: PathBuf,
    /// Working directory for command execution
    #[arg(long, value_name = "PATH")]
    workdir: Option<PathBuf>,
    /// Keep the working directory on success
    #[arg(long)]
    keep_workdir: bool,
    /// Repository root for built-in substitutions
    #[arg(long, value_name = "PATH")]
    repo_root: Option<PathBuf>,
    /// Prepend PATH entries for spawned commands
    #[arg(long, value_name = "PATHS")]
    prepend_path: Option<String>,
    /// Stop scheduling new tests after the first failure
    #[arg(long)]
    fail_fast: bool,
    /// Number of tests to run in parallel
    #[arg(long, value_name = "N", default_value_t = 1)]
    concurrency: usize,
    /// Report format
    #[arg(long, value_enum, default_value_t = GtestReportFormat::Text)]
    report: GtestReportFormat,
    /// Write report output to a file
    #[arg(long, value_name = "PATH")]
    report_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum GtestReportFormat {
    Text,
    Json,
}

#[derive(Subcommand, Debug)]
enum SessionCommand {
    /// Remove matching sessions from the backing store
    Purge(SessionPurgeArgs),
    /// Resume a stored session (sends payload and clears session)
    Resume(SessionResumeArgs),
    /// List resumable sessions
    List(SessionListArgs),
}

#[derive(Args, Debug)]
struct SessionPurgeArgs {
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    team: Option<String>,
    #[arg(long)]
    user: Option<String>,
}

#[derive(Args, Debug)]
struct PlanArgs {
    /// Environment name to stamp on the plan
    #[arg(long, default_value = "dev")]
    environment: String,
    /// Tenant to use (defaults to config defaults)
    #[arg(long)]
    tenant: Option<String>,
    /// Pack id to resolve from the pack index
    #[arg(long)]
    pack_id: Option<String>,
    /// Pretty-print JSON output
    #[arg(long, default_value_t = false)]
    pretty: bool,
}

#[derive(Args, Debug)]
struct SessionResumeArgs {
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    team: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long, value_name = "JSON")]
    payload: Option<String>,
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AppConfig {
    #[serde(default)]
    server: ServerConfig,
    #[serde(default)]
    packs: PackConfig,
    #[serde(default)]
    runner: RunnerConfig,
    #[serde(default)]
    stores: StoresConfig,
    #[serde(default)]
    defaults: SeedDefaults,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            server: ServerConfig {
                listen_addr: "0.0.0.0:8080".into(),
            },
            packs: PackConfig {
                root: Utf8PathBuf::from("packs"),
                default_tenant: "dev".into(),
            },
            runner: RunnerConfig {
                wasm_cache: Utf8PathBuf::from(".cache/wasm"),
            },
            stores: StoresConfig {
                session: StoreConfig::file(default_session_store_path()),
                state: StoreConfig::memory(),
            },
            defaults: SeedDefaults::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ServerConfig {
    #[serde(default = "default_listen_addr")]
    listen_addr: String,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen_addr: default_listen_addr(),
        }
    }
}

fn default_listen_addr() -> String {
    "0.0.0.0:8080".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PackConfig {
    #[serde(default = "default_packs_root")]
    root: Utf8PathBuf,
    #[serde(default = "default_tenant")]
    default_tenant: String,
}

impl Default for PackConfig {
    fn default() -> Self {
        Self {
            root: default_packs_root(),
            default_tenant: default_tenant(),
        }
    }
}

fn default_packs_root() -> Utf8PathBuf {
    Utf8PathBuf::from("packs")
}

fn default_tenant() -> String {
    "dev".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerConfig {
    #[serde(default = "default_wasm_cache")]
    wasm_cache: Utf8PathBuf,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            wasm_cache: default_wasm_cache(),
        }
    }
}

fn default_wasm_cache() -> Utf8PathBuf {
    Utf8PathBuf::from(".cache/wasm")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoresConfig {
    #[serde(default = "StoreConfig::memory")]
    session: StoreConfig,
    #[serde(default = "StoreConfig::memory")]
    state: StoreConfig,
}

impl Default for StoresConfig {
    fn default() -> Self {
        Self {
            session: StoreConfig::file(default_session_store_path()),
            state: StoreConfig::memory(),
        }
    }
}

fn default_session_store_path() -> Utf8PathBuf {
    Utf8PathBuf::from(".data/sessions.json")
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SeedDefaults {
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct PackIndex {
    entries: Vec<PackEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct PackEntry {
    id: String,
    name: Option<String>,
    kind: Option<String>,
    path: Utf8PathBuf,
}

#[derive(Debug, Deserialize)]
struct PackManifestStub {
    id: String,
    version: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
    #[serde(default)]
    r#type: Option<String>,
    #[serde(default)]
    scenarios: Vec<ScenarioStub>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ScenarioStub {
    id: String,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default)]
    golden: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreConfig {
    #[serde(default)]
    backend: StoreBackend,
    redis_url: Option<String>,
    #[serde(default)]
    redis_prefix: Option<String>,
    #[serde(default)]
    file_path: Option<Utf8PathBuf>,
}

impl StoreConfig {
    fn memory() -> Self {
        Self {
            backend: StoreBackend::Memory,
            redis_url: None,
            redis_prefix: None,
            file_path: None,
        }
    }

    fn file(path: Utf8PathBuf) -> Self {
        Self {
            backend: StoreBackend::File,
            redis_url: None,
            redis_prefix: None,
            file_path: Some(path),
        }
    }
}

impl Default for StoreConfig {
    fn default() -> Self {
        StoreConfig::memory()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
enum StoreBackend {
    #[default]
    Memory,
    File,
    Redis,
}

type SharedSessionStore = Arc<dyn SessionStore>;
type SharedPackIndex = Arc<RwLock<PackIndex>>;
type SharedRunnerEvents = Arc<RwLock<Vec<RunnerEvent>>>;

#[derive(Clone)]
#[allow(dead_code)]
struct AppState {
    config: AppConfig,
    session_store: SharedSessionStore,
    runner_proxy: RunnerHostProxy,
    pack_index: SharedPackIndex,
    runner_events: SharedRunnerEvents,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct SessionFilterInput {
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
}

impl SessionFilterInput {
    fn merge_with(self, other: Option<SessionFilterInput>) -> Self {
        let mut merged = self;
        if let Some(override_input) = other {
            if override_input.tenant.is_some() {
                merged.tenant = override_input.tenant;
            }
            if override_input.team.is_some() {
                merged.team = override_input.team;
            }
            if override_input.user.is_some() {
                merged.user = override_input.user;
            }
        }
        merged
    }
}

#[derive(Debug, Serialize)]
struct SessionPurgeResponse {
    removed: usize,
}

#[derive(Debug, Deserialize)]
struct SessionUpsertRequest {
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    tenant: Option<String>,
    #[serde(default)]
    team: Option<String>,
    #[serde(default)]
    user: Option<String>,
    #[serde(default)]
    flow_id: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    context: Option<Value>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionListResponse {
    count: usize,
    sessions: Vec<SessionView>,
}

#[derive(Debug, Serialize)]
struct PackListResponse {
    count: usize,
    packs: Vec<PackInfo>,
    resolved_keys: Vec<String>,
    missing_keys: Vec<String>,
}

#[derive(Debug, Serialize)]
struct PackInfo {
    id: String,
    name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    kind: Option<String>,
    path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RunnerEvent {
    timestamp_ms: u64,
    flow: String,
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
    payload: Value,
    result: Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionView {
    key: String,
    tenant: String,
    team: Option<String>,
    user: Option<String>,
    cursor: SessionCursorView,
    context: Value,
    updated_at_epoch_ms: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct SessionCursorView {
    flow_id: Option<String>,
    node_id: Option<String>,
}

impl From<SessionRecord> for SessionView {
    fn from(record: SessionRecord) -> Self {
        Self {
            key: record.key,
            tenant: record.tenant,
            team: record.team,
            user: record.user,
            cursor: SessionCursorView {
                flow_id: record.flow_id,
                node_id: record.node_id,
            },
            context: record.context,
            updated_at_epoch_ms: record.updated_at_epoch_ms,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();
    match cli.command {
        Command::Serve(args) => serve(args).await?,
        Command::Packs { command } => handle_packs(command)?,
        Command::Sessions { command } => handle_sessions(command)?,
        Command::Runner { command } => handle_runner(command)?,
        Command::Gtest(args) => handle_gtest(args)?,
    }

    Ok(())
}

fn handle_gtest(args: GtestArgs) -> Result<()> {
    let tester = which("greentic-integration-tester")
        .or_else(|_| find_local_gtest_runner())
        .context("greentic-integration-tester not found on PATH or next to greentic-integration")?;
    let mut cmd = ProcessCommand::new(tester);
    cmd.arg("--test").arg(args.test);
    if let Some(workdir) = args.workdir {
        cmd.arg("--workdir").arg(workdir);
    }
    if args.keep_workdir {
        cmd.arg("--keep-workdir");
    }
    if let Some(repo_root) = args.repo_root {
        cmd.arg("--repo-root").arg(repo_root);
    }
    if let Some(prepend_path) = args.prepend_path {
        cmd.arg("--prepend-path").arg(prepend_path);
    }
    if args.fail_fast {
        cmd.arg("--fail-fast");
    }
    cmd.arg("--concurrency").arg(args.concurrency.to_string());
    match args.report {
        GtestReportFormat::Text => {
            cmd.arg("--report").arg("text");
        }
        GtestReportFormat::Json => {
            cmd.arg("--report").arg("json");
        }
    }
    if let Some(report_file) = args.report_file {
        cmd.arg("--report-file").arg(report_file);
    }
    let status = cmd
        .status()
        .context("failed to spawn greentic-integration-tester")?;
    if !status.success() {
        bail!("gtest runner failed with status {:?}", status.code());
    }
    Ok(())
}

fn find_local_gtest_runner() -> Result<std::path::PathBuf> {
    let exe = std::env::current_exe().context("failed to resolve current executable path")?;
    let exe_dir = exe
        .parent()
        .context("current executable has no parent directory")?;
    let mut candidate = exe_dir.join("greentic-integration-tester");
    if cfg!(windows) {
        candidate.set_extension("exe");
    }
    if candidate.exists() {
        return Ok(candidate);
    }
    Err(anyhow!(
        "greentic-integration-tester not found near {}",
        exe_dir.display()
    ))
}

async fn serve(args: ServeArgs) -> Result<()> {
    let config = load_config(args.config.as_ref())?;
    let packs_root = resolve_packs_root(&config.packs)?;
    let session_store = build_session_store(&config.stores.session)?;
    let pack_index = Arc::new(RwLock::new(build_pack_index(&config.packs)?));
    let runner_events = Arc::new(RwLock::new(Vec::new()));
    let (runner_tx, runner_rx) = mpsc::unbounded_channel();
    let runner_base = runner_proxy_base_from_env();
    let runner_proxy = RunnerHostProxy::new(runner_tx, runner_base.clone());
    tokio::spawn(proxy_runner_loop(
        runner_rx,
        runner_events.clone(),
        runner_base,
    ));
    let state = AppState {
        config: config.clone(),
        session_store: session_store.clone(),
        runner_proxy: runner_proxy.clone(),
        pack_index: pack_index.clone(),
        runner_events: runner_events.clone(),
    };

    info!(
        backend = ?config.stores.session.backend,
        file_path = ?config.stores.session.file_path,
        redis_url = ?config.stores.session.redis_url,
        "session store configured"
    );
    info!(
        packs = pack_index.read().entries.len(),
        root = %packs_root,
        "pack index loaded"
    );
    info!(?config, watch = args.watch, "starting integration server");

    runner_proxy.submit(RunnerCommand::ReloadPacks {
        packs: pack_index.read().clone(),
        defaults: config.defaults.clone(),
    });

    let addr: SocketAddr = config
        .server
        .listen_addr
        .parse()
        .with_context(|| format!("invalid listen address {}", config.server.listen_addr))?;
    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| format!("failed to bind {addr}"))?;
    info!(%addr, "listening for HTTP traffic");

    let mut tasks = JoinSet::new();
    if args.watch {
        let watch_state = state.clone();
        let pack_root = packs_root.clone();
        tasks.spawn(async move {
            if let Err(err) = watch_packs(pack_root, watch_state).await {
                warn!(?err, "pack watch failed");
            }
        });
    }

    let server_task = tokio::spawn(async move {
        axum::serve(listener, build_router(state).into_make_service())
            .with_graceful_shutdown(shutdown_signal())
            .await
            .context("server exited with an error")
    });

    if let Err(err) = server_task.await.expect("server task panicked") {
        error!(?err, "server task failed");
    }

    while let Some(res) = tasks.join_next().await {
        if let Err(err) = res {
            warn!(?err, "watch task ended unexpectedly");
        }
    }

    info!("server shut down cleanly");

    Ok(())
}

fn handle_packs(cmd: PacksCommand) -> Result<()> {
    match cmd {
        PacksCommand::Validate => run_pack_validator()?,
        PacksCommand::List(args) => list_packs(args)?,
        PacksCommand::Reload(args) => reload_packs_cli(args)?,
        PacksCommand::Plan(args) => plan_pack(args)?,
    }

    Ok(())
}

fn handle_sessions(cmd: SessionCommand) -> Result<()> {
    match cmd {
        SessionCommand::Purge(args) => purge_sessions(args)?,
        SessionCommand::Resume(args) => resume_session_cli(args)?,
        SessionCommand::List(args) => list_sessions_cli(args)?,
    }

    Ok(())
}

fn purge_sessions(args: SessionPurgeArgs) -> Result<()> {
    let config = load_config(None)?;
    let store = build_session_store(&config.stores.session)?;
    let filter_input = SessionFilterInput {
        tenant: args.tenant.clone(),
        team: args.team.clone(),
        user: args.user.clone(),
    };
    let filter = build_session_filter(filter_input, &config.defaults);
    let removed = store.purge(&filter)?;
    info!(
        removed,
        tenant = ?args.tenant,
        team = ?args.team,
        user = ?args.user,
        "purged matching sessions"
    );
    Ok(())
}

fn resume_session_cli(args: SessionResumeArgs) -> Result<()> {
    let payload = args
        .payload
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .context("invalid JSON payload for session resume")?
        .unwrap_or(Value::Null);
    let mut body = serde_json::Map::new();
    if let Some(tenant) = args.tenant {
        body.insert("tenant".into(), Value::String(tenant));
    }
    if let Some(team) = args.team {
        body.insert("team".into(), Value::String(team));
    }
    if let Some(user) = args.user {
        body.insert("user".into(), Value::String(user));
    } else {
        bail!("--user is required for session resume");
    }
    body.insert("payload".into(), payload);

    let url = format!("{}/sessions/resume", args.server.trim_end_matches('/'));
    let resp = ureq::post(&url)
        .send_json(serde_json::Value::Object(body))
        .map_err(|err| anyhow!("failed to POST {url}: {err}"))?;
    let event: RunnerEvent = resp
        .into_body()
        .read_json()
        .map_err(|err| anyhow!("invalid resume response: {err}"))?;
    println!(
        "Resumed flow {} for tenant={:?} user={:?}; result={}",
        event.flow, event.tenant, event.user, event.result
    );
    Ok(())
}

fn list_sessions_cli(args: SessionListArgs) -> Result<()> {
    let mut url = format!("{}/sessions", args.server.trim_end_matches('/'));
    let mut params = Vec::new();
    if let Some(tenant) = args.tenant {
        params.push(format!("tenant={tenant}"));
    }
    if let Some(team) = args.team {
        params.push(format!("team={team}"));
    }
    if let Some(user) = args.user {
        params.push(format!("user={user}"));
    }
    if !params.is_empty() {
        url.push('?');
        url.push_str(&params.join("&"));
    }

    let resp = ureq::get(&url)
        .call()
        .map_err(|err| anyhow!("failed to GET {url}: {err}"))?;
    let data: SessionListResponse = resp
        .into_body()
        .read_json()
        .map_err(|err| anyhow!("invalid session list response: {err}"))?;
    println!("{} session(s):", data.count);
    for session in data.sessions {
        println!(
            "- key={} tenant={} team={:?} user={:?} flow={:?} node={:?}",
            session.key,
            session.tenant,
            session.team,
            session.user,
            session.cursor.flow_id,
            session.cursor.node_id
        );
    }
    Ok(())
}

fn build_session_store(config: &StoreConfig) -> Result<SharedSessionStore> {
    match config.backend {
        StoreBackend::Memory => Ok(InMemorySessionStore::new()),
        StoreBackend::File => {
            let root = workspace_root().to_path_buf();
            let path = config
                .file_path
                .clone()
                .unwrap_or_else(default_session_store_path);
            let store = FileSessionStore::new(root, path)?;
            Ok(store as SharedSessionStore)
        }
        StoreBackend::Redis => {
            let url = config
                .redis_url
                .as_deref()
                .ok_or_else(|| anyhow!("redis backend requires redis_url"))?;
            let store = crate::session::RedisSessionStore::new(url, config.redis_prefix.clone())?;
            Ok(store as SharedSessionStore)
        }
    }
}

fn build_session_filter(input: SessionFilterInput, defaults: &SeedDefaults) -> SessionFilter {
    let tenant =
        sanitize_optional(input.tenant).or_else(|| sanitize_optional(defaults.tenant.clone()));
    let team = sanitize_optional(input.team).or_else(|| sanitize_optional(defaults.team.clone()));
    let user = sanitize_optional(input.user);
    SessionFilter::new(tenant, team, user)
}

fn normalize_upsert_payload(
    payload: SessionUpsertRequest,
    defaults: &SeedDefaults,
) -> Result<SessionUpsert, StatusCode> {
    let tenant = sanitize_optional(payload.tenant)
        .or_else(|| sanitize_optional(defaults.tenant.clone()))
        .ok_or_else(|| {
            warn!("session upsert missing tenant");
            StatusCode::BAD_REQUEST
        })?;

    let user = sanitize_optional(payload.user);
    if user.is_none() {
        warn!("session upsert missing user");
        return Err(StatusCode::BAD_REQUEST);
    }

    let key = sanitize_optional(payload.key).unwrap_or_else(|| Uuid::new_v4().to_string());
    let team = sanitize_optional(payload.team).or_else(|| sanitize_optional(defaults.team.clone()));
    let flow_id = sanitize_optional(payload.flow_id);
    let node_id = sanitize_optional(payload.node_id);

    Ok(SessionUpsert {
        key,
        tenant,
        team,
        user,
        flow_id,
        node_id,
        context: payload.context.unwrap_or_default(),
    })
}

fn sanitize_optional(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

async fn watch_packs(pack_root: Utf8PathBuf, state: AppState) -> Result<()> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let mut watcher: RecommendedWatcher =
        recommended_watcher(move |res: Result<Event, notify::Error>| match res {
            Ok(event) => {
                trace!(?event, "pack watcher event");
                let _ = tx.send(event);
            }
            Err(err) => warn!(?err, "pack watcher error"),
        })
        .context("failed to initialize pack watcher")?;

    watcher
        .watch(pack_root.as_std_path(), RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {pack_root}"))?;

    info!(root = %pack_root, "pack watcher started");
    let mut last_reload: Option<std::time::Instant> = None;
    while let Some(_event) = rx.recv().await {
        let now = std::time::Instant::now();
        if let Some(prev) = last_reload
            && now.duration_since(prev) < Duration::from_millis(500)
        {
            continue;
        }
        last_reload = Some(now);
        if let Err(err) = reload_packs(&state) {
            warn!(?err, "pack reload failed");
        } else {
            info!("pack index reloaded after change");
        }
    }

    Ok(())
}

fn resolve_packs_root(config: &PackConfig) -> Result<Utf8PathBuf> {
    let workspace = workspace_root();
    let workspace_root = workspace
        .as_std_path()
        .canonicalize()
        .with_context(|| format!("failed to canonicalize workspace root {workspace}"))?;
    let safe_root = normalize_under_root(&workspace_root, config.root.as_std_path())?;
    Utf8PathBuf::from_path_buf(safe_root).map_err(|_| anyhow!("packs root is not valid UTF-8"))
}

fn build_pack_index(config: &PackConfig) -> Result<PackIndex> {
    let root = resolve_packs_root(config)?;
    if !root.exists() {
        warn!(root = %root, "pack root does not exist");
        return Ok(PackIndex::default());
    }

    let mut entries = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("failed to read pack root {root}"))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        let manifest_path = path.join("pack.json");
        if !manifest_path.exists() {
            continue;
        }
        let manifest_display = manifest_path.display().to_string();
        let raw = fs::read(&manifest_path)
            .with_context(|| format!("failed to read {manifest_display}"))?;
        let manifest: serde_json::Value = serde_json::from_slice(&raw)
            .with_context(|| format!("invalid JSON in {manifest_display}"))?;
        let id = manifest
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let name = manifest
            .get("name")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let kind = manifest
            .get("kind")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let pack_path = match Utf8PathBuf::from_path_buf(path.clone()) {
            Ok(p) => p,
            Err(_) => Utf8PathBuf::from(path.to_string_lossy().to_string()),
        };
        entries.push(PackEntry {
            id,
            name,
            kind,
            path: pack_path,
        });
    }

    Ok(PackIndex { entries })
}

fn infer_base_deployment_plan(
    entry: &PackEntry,
    tenant: String,
    environment: String,
) -> Result<DeploymentPlan> {
    let manifest_path = entry.path.join("pack.json");
    let raw = fs::read_to_string(&manifest_path)
        .with_context(|| format!("failed to read {manifest_path}"))?;
    let manifest: PackManifestStub = serde_json::from_str(&raw)
        .with_context(|| format!("invalid pack manifest {manifest_path}"))?;

    let pack_version = Version::parse(&manifest.version)
        .with_context(|| format!("invalid semver version in {manifest_path}"))?;

    let channels = manifest
        .scenarios
        .iter()
        .map(|scenario| ChannelPlan {
            name: scenario.id.clone(),
            flow_id: scenario.id.clone(),
            kind: manifest.r#type.as_deref().unwrap_or("scenario").to_string(),
            config: Value::Null,
        })
        .collect();

    let extra = json!({
        "pack_name": manifest.name,
        "pack_kind": manifest.kind,
    });

    let subjects: Vec<MessagingSubjectPlan> = manifest
        .scenarios
        .iter()
        .map(|scenario| MessagingSubjectPlan {
            name: scenario.id.clone(),
            purpose: manifest.r#type.as_deref().unwrap_or("scenario").to_string(),
            durable: true,
            extra: Value::Null,
        })
        .collect();

    let messaging = (!subjects.is_empty()).then(|| MessagingPlan {
        logical_cluster: "default".into(),
        subjects,
        extra: Value::Null,
    });

    let telemetry_required = manifest
        .kind
        .as_deref()
        .map(|k| k.eq_ignore_ascii_case("application") || k.eq_ignore_ascii_case("deployment"))
        .unwrap_or(false);

    Ok(DeploymentPlan {
        pack_id: manifest.id,
        pack_version,
        tenant,
        environment,
        runners: vec![RunnerPlan {
            name: format!("{}-runner", entry.id),
            replicas: 1,
            capabilities: Value::Null,
        }],
        messaging,
        channels,
        secrets: Vec::new(),
        oauth: Vec::new(),
        telemetry: Some(TelemetryPlan {
            required: telemetry_required,
            suggested_endpoint: None,
            extra: Value::Null,
        }),
        extra,
    })
}

fn run_pack_validator() -> Result<()> {
    let config = load_config(None)?;
    let packs_root = resolve_packs_root(&config.packs)?;
    let script = workspace_root().join("scripts/packs_test.py");
    if !script.exists() {
        bail!("pack validation script not found at {script}");
    }

    info!(root = %packs_root, script = %script, "running pack validation script");
    let status = ProcessCommand::new("python3")
        .arg(script.as_str())
        .current_dir(workspace_root())
        .status()
        .with_context(|| format!("failed to execute {script}"))?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        bail!("pack validation failed with exit code {code}");
    }

    Ok(())
}

fn list_packs(args: PackListArgs) -> Result<()> {
    let config = load_config(None)?;
    let packs_root = resolve_packs_root(&config.packs)?;
    let index = build_pack_index(&config.packs)?;
    let tenant = args.tenant.as_deref().or(config.defaults.tenant.as_deref());
    let team = args.team.as_deref().or(config.defaults.team.as_deref());
    let user = args.user.as_deref();
    let (resolved, resolved_keys, missing_keys) = index.resolve_for(tenant, team, user);

    if resolved.is_empty() {
        println!("No packs found under {packs_root}");
        return Ok(());
    }

    if !resolved_keys.is_empty() {
        println!("Resolved keys: {}", resolved_keys.join(", "));
    } else if tenant.is_some() {
        println!("No overrides matched; showing all packs.");
    }

    if !missing_keys.is_empty() {
        println!("Missing overrides: {}", missing_keys.join(", "));
    }

    println!("Discovered {} pack(s):", resolved.len());
    for entry in resolved {
        let kind = entry.kind.as_deref().unwrap_or("unknown");
        println!(
            "- {} ({}) [{kind}] @ {}",
            entry.id,
            entry.name.as_deref().unwrap_or("unnamed"),
            entry.path
        );
    }
    Ok(())
}

fn plan_pack(args: PlanArgs) -> Result<()> {
    let config = load_config(None)?;
    let packs_root = resolve_packs_root(&config.packs)?;
    let index = build_pack_index(&config.packs)?;
    let tenant = args
        .tenant
        .or_else(|| config.defaults.tenant.clone())
        .unwrap_or_else(|| "dev".to_string());
    let team = config.defaults.team.as_deref();
    let (resolved, _, _) = index.resolve_for(Some(&tenant), team, None);
    if resolved.is_empty() {
        bail!(
            "no packs found under {packs_root} (consider adjusting [packs].root or tenant defaults)"
        );
    }
    let entry = if let Some(pack_id) = args.pack_id.as_deref() {
        resolved
            .into_iter()
            .find(|p| p.id == pack_id)
            .ok_or_else(|| anyhow!("pack id {pack_id} not found in index"))?
    } else {
        resolved
            .first()
            .cloned()
            .ok_or_else(|| anyhow!("no pack resolved"))?
    };

    let plan = infer_base_deployment_plan(&entry, tenant, args.environment)?;
    let json = if args.pretty {
        serde_json::to_string_pretty(&plan)?
    } else {
        serde_json::to_string(&plan)?
    };
    println!("{json}");
    Ok(())
}

fn reload_packs_cli(args: ReloadArgs) -> Result<()> {
    if let Some(server) = args.server {
        let url = format!("{}/packs/reload", server.trim_end_matches('/'));
        let resp = ureq::post(&url)
            .send_empty()
            .map_err(|err| anyhow!("HTTP reload failed: {err}"))?;
        let body: serde_json::Value = resp
            .into_body()
            .read_json()
            .map_err(|err| anyhow!("failed to parse /packs/reload response: {err}"))?;
        println!("Server reload succeeded: {body}");
        return Ok(());
    }

    let config = load_config(None)?;
    let packs_root = resolve_packs_root(&config.packs)?;
    let index = build_pack_index(&config.packs)?;
    println!(
        "Rebuilt pack index with {} entries under {}",
        index.entries.len(),
        packs_root
    );
    for entry in &index.entries {
        println!(
            "- {} ({}) @ {}",
            entry.id,
            entry.name.as_deref().unwrap_or("unnamed"),
            entry.path
        );
    }
    println!(
        "Note: running servers must call /packs/reload or run with --watch to pick up changes."
    );
    Ok(())
}

fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/packs", get(list_packs_http))
        .route("/packs/reload", post(reload_packs_http))
        .route(
            "/runner/events",
            get(list_runner_events).delete(clear_runner_events_http),
        )
        .route("/runner/emit", post(runner_emit_http))
        .route(
            "/sessions",
            get(list_sessions)
                .delete(delete_sessions)
                .post(upsert_session),
        )
        .route("/sessions/resume", post(resume_session_http))
        .layer(Extension(state))
}

async fn healthz(Extension(_state): Extension<AppState>) -> StatusCode {
    StatusCode::OK
}

async fn list_sessions(
    Extension(state): Extension<AppState>,
    Query(query): Query<SessionFilterInput>,
) -> Result<Json<SessionListResponse>, StatusCode> {
    let filter_input = query.merge_with(None);
    let filter = build_session_filter(filter_input, &state.config.defaults);
    state
        .session_store
        .list(&filter)
        .map(|records| {
            let sessions: Vec<SessionView> = records.into_iter().map(SessionView::from).collect();
            Json(SessionListResponse {
                count: sessions.len(),
                sessions,
            })
        })
        .map_err(|err| {
            error!(?err, "failed to list sessions");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

#[derive(Debug, Default, Deserialize)]
struct PackQuery {
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
}

async fn list_packs_http(
    Extension(state): Extension<AppState>,
    Query(query): Query<PackQuery>,
) -> Json<PackListResponse> {
    let index = state.pack_index.read().clone();
    list_packs_filtered(
        &index,
        query
            .tenant
            .as_deref()
            .or(state.config.defaults.tenant.as_deref()),
        query
            .team
            .as_deref()
            .or(state.config.defaults.team.as_deref()),
        query.user.as_deref(),
    )
}

fn list_packs_filtered(
    index: &PackIndex,
    tenant: Option<&str>,
    team: Option<&str>,
    user: Option<&str>,
) -> Json<PackListResponse> {
    let (resolved, resolved_keys, missing_keys) = index.resolve_for(tenant, team, user);
    let packs = resolved
        .iter()
        .map(|entry| PackInfo {
            id: entry.id.clone(),
            name: entry.name.clone(),
            kind: entry.kind.clone(),
            path: entry.path.to_string(),
        })
        .collect::<Vec<_>>();
    Json(PackListResponse {
        count: packs.len(),
        packs,
        resolved_keys,
        missing_keys,
    })
}

async fn list_runner_events(Extension(state): Extension<AppState>) -> Json<Vec<RunnerEvent>> {
    Json(state.runner_events.read().clone())
}

async fn clear_runner_events_http(Extension(state): Extension<AppState>) -> StatusCode {
    state.runner_events.write().clear();
    StatusCode::NO_CONTENT
}

#[derive(Debug, Serialize, Deserialize)]
struct RunnerEmitRequest {
    flow: String,
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
    payload: Option<Value>,
}

async fn runner_emit_http(
    Extension(state): Extension<AppState>,
    Json(req): Json<RunnerEmitRequest>,
) -> Json<RunnerEvent> {
    let event = synthesize_runner_event(
        req.flow,
        req.tenant.or_else(|| state.config.defaults.tenant.clone()),
        req.team.or_else(|| state.config.defaults.team.clone()),
        req.user,
        req.payload.unwrap_or(Value::Null),
    );
    record_runner_event(&state.runner_events, event.clone());
    Json(event)
}

#[derive(Debug, Deserialize)]
struct SessionResumeRequest {
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
    payload: Option<Value>,
}

async fn resume_session_http(
    Extension(state): Extension<AppState>,
    Json(req): Json<SessionResumeRequest>,
) -> Result<Json<RunnerEvent>, StatusCode> {
    let tenant = req.tenant.or_else(|| state.config.defaults.tenant.clone());
    let user = req.user.clone();
    if user.is_none() {
        return Err(StatusCode::BAD_REQUEST);
    }
    let payload = req.payload.unwrap_or(Value::Null);
    let filter = SessionFilter::new(
        tenant.clone(),
        req.team.or_else(|| state.config.defaults.team.clone()),
        user.clone(),
    );
    let session = state
        .session_store
        .find(&filter)
        .map_err(|err| {
            error!(?err, "session lookup failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?
        .ok_or(StatusCode::NOT_FOUND)?;
    let flow = session.flow_id.clone().ok_or(StatusCode::BAD_REQUEST)?;
    let event = synthesize_runner_event(flow, tenant, session.team.clone(), user, payload);
    if let Err(err) = state.session_store.remove(&session.key) {
        error!(?err, key = %session.key, "failed to clear resumed session");
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    record_runner_event(&state.runner_events, event.clone());
    Ok(Json(event))
}

async fn delete_sessions(
    Extension(state): Extension<AppState>,
    Query(query): Query<SessionFilterInput>,
    body: Option<Json<SessionFilterInput>>,
) -> Result<Json<SessionPurgeResponse>, StatusCode> {
    let body_filter = body.map(|Json(inner)| inner);
    let merged = query.merge_with(body_filter);
    let filter = build_session_filter(merged, &state.config.defaults);
    state
        .session_store
        .purge(&filter)
        .map(|removed| Json(SessionPurgeResponse { removed }))
        .map_err(|err| {
            error!(?err, "failed to purge sessions via HTTP");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn reload_packs_http(
    Extension(state): Extension<AppState>,
) -> Result<Json<PackListResponse>, StatusCode> {
    let index = build_pack_index(&state.config.packs).map_err(|err| {
        error!(?err, "failed to rebuild pack index");
        StatusCode::INTERNAL_SERVER_ERROR
    })?;

    {
        let mut guard = state.pack_index.write();
        *guard = index.clone();
    }

    state.runner_proxy.submit(RunnerCommand::ReloadPacks {
        packs: index.clone(),
        defaults: state.config.defaults.clone(),
    });

    Ok(list_packs_filtered(
        &index,
        state.config.defaults.tenant.as_deref(),
        state.config.defaults.team.as_deref(),
        None,
    ))
}

async fn upsert_session(
    Extension(state): Extension<AppState>,
    Json(payload): Json<SessionUpsertRequest>,
) -> Result<Json<SessionView>, StatusCode> {
    let upsert = normalize_upsert_payload(payload, &state.config.defaults)?;
    state
        .session_store
        .upsert(upsert)
        .map(SessionView::from)
        .map(Json)
        .map_err(|err| {
            error!(?err, "failed to upsert session");
            StatusCode::INTERNAL_SERVER_ERROR
        })
}

async fn shutdown_signal() {
    if let Err(err) = signal::ctrl_c().await {
        warn!(?err, "failed to listen for shutdown signal");
        return;
    }
    info!("shutdown signal received");
}

fn load_config(explicit_path: Option<&Utf8PathBuf>) -> Result<AppConfig> {
    let mut figment = Figment::from(Serialized::defaults(DEFAULT_CONFIG.clone()));

    if let Some(path) = explicit_path {
        figment = figment.merge(Toml::file(path));
    } else if let Some(path) = resolve_default_config_path() {
        figment = figment.merge(Toml::file(path));
    } else {
        warn!("no config file found; relying on defaults + env overrides");
    }

    figment = figment.merge(Env::prefixed("GREENTIC_").split("__"));

    figment
        .extract()
        .context("failed to load greentic-integration configuration")
}

fn workspace_root() -> &'static Utf8Path {
    static ROOT: Lazy<Utf8PathBuf> = Lazy::new(|| {
        let manifest_dir = Utf8PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        manifest_dir
            .parent()
            .and_then(|p| p.parent())
            .map(Utf8PathBuf::from)
            .unwrap_or(manifest_dir)
    });
    ROOT.as_path()
}

fn resolve_default_config_path() -> Option<Utf8PathBuf> {
    let repo_relative = workspace_root().join("config/dev.toml");
    if repo_relative.exists() {
        return Some(repo_relative);
    }

    if let Some(dirs) = ProjectDirs::from("ai", "Greentic", APP_NAME)
        && let Ok(path) = Utf8PathBuf::from_path_buf(dirs.config_dir().join("config.toml"))
        && path.exists()
    {
        return Some(path);
    }

    None
}

fn init_tracing() {
    let filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into());
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct RunnerHostProxy {
    tx: mpsc::UnboundedSender<RunnerCommand>,
    runner_base: Option<String>,
}

fn runner_proxy_base_from_env() -> Option<String> {
    std::env::var("RUNNER_PROXY_URL")
        .ok()
        .or_else(|| std::env::var("GREENTIC_RUNNER_URL").ok())
}

fn send_runner_request(base: &str, path: &str, payload: Value) -> Result<()> {
    let url = format!("{}/{}", base.trim_end_matches('/'), path);
    match ureq::post(&url).send_json(payload) {
        Ok(resp) => {
            let status = resp.status().as_u16();
            if status >= 400 {
                bail!("runner request to {} failed with status {}", url, status);
            }
            Ok(())
        }
        Err(err) => bail!("runner request to {} failed: {}", url, err),
    }
}

impl RunnerHostProxy {
    #[allow(dead_code)]
    fn new(tx: mpsc::UnboundedSender<RunnerCommand>, runner_base: Option<String>) -> Self {
        Self { tx, runner_base }
    }

    #[allow(dead_code)]
    fn submit(&self, command: RunnerCommand) {
        if let Err(err) = self.tx.send(command) {
            error!(?err, "failed to submit command to runner proxy");
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)]
enum RunnerCommand {
    Emit(String),
    ReloadPacks {
        packs: PackIndex,
        defaults: SeedDefaults,
    },
    EmitActivity {
        flow: String,
        tenant: Option<String>,
        team: Option<String>,
        user: Option<String>,
        payload: Value,
    },
}

async fn proxy_runner_loop(
    mut rx: mpsc::UnboundedReceiver<RunnerCommand>,
    events: SharedRunnerEvents,
    runner_base: Option<String>,
) {
    while let Some(cmd) = rx.recv().await {
        match cmd {
            RunnerCommand::Emit(message) => {
                info!(%message, "runner proxy emit");
                if let Some(base) = &runner_base
                    && let Err(err) =
                        send_runner_request(base, "runner/emit", json!({ "message": message }))
                {
                    warn!(?err, "runner proxy emit forward failed");
                }
            }
            RunnerCommand::ReloadPacks { packs, defaults } => {
                info!(
                    pack_count = packs.entries.len(),
                    default_tenant = ?defaults.tenant,
                    default_team = ?defaults.team,
                    "runner proxy reload packs"
                );
                for pack in &packs.entries {
                    info!(
                        pack_id = %pack.id,
                        pack_name = ?pack.name,
                        path = %pack.path,
                        "runner proxy indexed pack"
                    );
                }
                if let Some(base) = &runner_base {
                    let payload = json!({
                        "packs": packs.entries,
                        "defaults": defaults,
                    });
                    if let Err(err) = send_runner_request(base, "runner/reload", payload) {
                        warn!(?err, "runner proxy reload forward failed");
                    }
                }
            }
            RunnerCommand::EmitActivity {
                flow,
                tenant,
                team,
                user,
                payload,
            } => {
                let event = synthesize_runner_event(flow, tenant, team, user, payload);
                record_runner_event(&events, event.clone());
                info!(
                flow = %event.flow,
                tenant = ?event.tenant,
                team = ?event.team,
                user = ?event.user,
                    payload = %event.payload,
                    result = %event.result,
                    "runner proxy emit activity"
                );
                if let Some(base) = &runner_base
                    && let Err(err) = send_runner_request(
                        base,
                        "runner/activity",
                        json!({
                            "flow": event.flow,
                            "tenant": event.tenant,
                            "team": event.team,
                            "user": event.user,
                            "payload": event.payload,
                            "result": event.result,
                        }),
                    )
                {
                    warn!(?err, "runner proxy activity forward failed");
                }
            }
        }
    }
    warn!("runner proxy loop exited");
}
impl PackIndex {
    fn resolve_for(
        &self,
        tenant: Option<&str>,
        team: Option<&str>,
        user: Option<&str>,
    ) -> (Vec<PackEntry>, Vec<String>, Vec<String>) {
        let mut desired = Vec::new();
        if let Some(t) = tenant {
            if let Some(team) = team {
                if let Some(user) = user {
                    desired.push(format!("{t}:{team}:{user}"));
                }
                desired.push(format!("{t}:{team}"));
            }
            desired.push(t.to_string());
        }

        if desired.is_empty() {
            return (self.entries.clone(), desired, Vec::new());
        }

        let mut matched = Vec::new();
        let mut matched_keys = Vec::new();
        let mut missing_keys = Vec::new();
        for key in desired {
            if let Some(entry) = self.entries.iter().find(|e| e.id == key) {
                matched.push(entry.clone());
                matched_keys.push(key);
            } else {
                missing_keys.push(key);
            }
        }

        if matched.is_empty() {
            (
                self.entries.clone(),
                Vec::new(),
                missing_keys, /* entire chain missing */
            )
        } else {
            (matched, matched_keys, missing_keys)
        }
    }
}
fn reload_packs(state: &AppState) -> Result<()> {
    let index = build_pack_index(&state.config.packs)?;
    {
        let mut guard = state.pack_index.write();
        *guard = index.clone();
    }
    state.runner_proxy.submit(RunnerCommand::ReloadPacks {
        packs: index.clone(),
        defaults: state.config.defaults.clone(),
    });
    info!(
        pack_count = index.entries.len(),
        "pack index reloaded successfully"
    );
    Ok(())
}

fn runner_emit_cli(args: RunnerEmitArgs) -> Result<()> {
    let config = load_config(None)?;
    let (tx, rx) = mpsc::unbounded_channel();
    let runner_base = runner_proxy_base_from_env();
    let proxy = RunnerHostProxy::new(tx, runner_base.clone());
    let events: SharedRunnerEvents = Arc::new(RwLock::new(Vec::new()));
    tokio::spawn(proxy_runner_loop(rx, events.clone(), runner_base));

    let payload = args
        .payload
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()
        .context("invalid JSON payload for runner emit")?
        .unwrap_or(Value::Null);

    if let Some(server) = args.server {
        let url = format!("{}/runner/emit", server.trim_end_matches('/'));
        let mut body = serde_json::Map::new();
        body.insert("flow".into(), Value::String(args.flow));
        if let Some(tenant) = args.tenant.or_else(|| config.defaults.tenant.clone()) {
            body.insert("tenant".into(), Value::String(tenant));
        }
        if let Some(team) = args.team.or_else(|| config.defaults.team.clone()) {
            body.insert("team".into(), Value::String(team));
        }
        if let Some(user) = args.user {
            body.insert("user".into(), Value::String(user));
        }
        body.insert("payload".into(), payload.clone());
        let resp = ureq::post(&url)
            .send_json(serde_json::Value::Object(body))
            .map_err(|err| anyhow!("failed to POST {url}: {err}"))?;
        let event: RunnerEvent = resp
            .into_body()
            .read_json()
            .map_err(|err| anyhow!("invalid runner emit response: {err}"))?;
        println!(
            "Server runner emit result -> tenant={:?} team={:?} user={:?} result={}",
            event.tenant, event.team, event.user, event.result
        );
        return Ok(());
    }

    proxy.submit(RunnerCommand::EmitActivity {
        flow: args.flow,
        tenant: args.tenant.or_else(|| config.defaults.tenant.clone()),
        team: args.team.or_else(|| config.defaults.team.clone()),
        user: args.user,
        payload,
    });
    println!("Runner emit command submitted (check server logs if running).");
    Ok(())
}

fn runner_events_cli(args: RunnerEventsArgs) -> Result<()> {
    let url = format!("{}/runner/events", args.server.trim_end_matches('/'));
    let resp = ureq::get(&url)
        .call()
        .map_err(|err| anyhow!("failed to GET {url}: {err}"))?;
    let events: Vec<RunnerEvent> = resp
        .into_body()
        .read_json()
        .map_err(|err| anyhow!("invalid runner events response: {err}"))?;
    if events.is_empty() {
        println!("No runner events recorded.");
        return Ok(());
    }
    for event in events {
        println!(
            "[{}] flow={} tenant={:?} team={:?} user={:?} payload={} result={}",
            event.timestamp_ms,
            event.flow,
            event.tenant,
            event.team,
            event.user,
            event.payload,
            event.result
        );
    }
    Ok(())
}

fn runner_clear_cli(args: RunnerClearArgs) -> Result<()> {
    let url = format!("{}/runner/events", args.server.trim_end_matches('/'));
    ureq::delete(&url)
        .call()
        .map_err(|err| anyhow!("failed to DELETE {url}: {err}"))?;
    println!("Cleared runner events on {}", args.server);
    Ok(())
}

fn synthesize_runner_event(
    flow: String,
    tenant: Option<String>,
    team: Option<String>,
    user: Option<String>,
    payload: Value,
) -> RunnerEvent {
    let result = json!({
        "flow": flow,
        "echo": payload,
        "status": "ok",
    });
    RunnerEvent {
        timestamp_ms: now_millis(),
        flow,
        tenant,
        team,
        user,
        payload,
        result,
    }
}

fn record_runner_event(events: &SharedRunnerEvents, event: RunnerEvent) {
    let mut guard = events.write();
    guard.push(event);
    let len = guard.len();
    if len > 100 {
        let excess = len - 100;
        guard.drain(0..excess);
    }
}

fn now_millis() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

#[cfg(test)]
mod app_tests {
    use super::*;
    use crate::deployment::PackKind;
    use axum::{
        Extension,
        body::{self, Body},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    fn state_with_session(flow_id: &str) -> AppState {
        let config = AppConfig::default();
        let session_store = build_session_store(&config.stores.session).unwrap();
        let pack_index = Arc::new(RwLock::new(PackIndex::default()));
        let runner_events = Arc::new(RwLock::new(Vec::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let proxy = RunnerHostProxy::new(tx, None);

        tokio::spawn(proxy_runner_loop(rx, runner_events.clone(), None));

        session_store
            .upsert(SessionUpsert {
                key: "test-sess".into(),
                tenant: "dev".into(),
                team: None,
                user: Some("user-test".into()),
                flow_id: Some(flow_id.into()),
                node_id: Some("node-wait".into()),
                context: json!({"waiting": true}),
            })
            .unwrap();

        AppState {
            config,
            session_store,
            runner_proxy: proxy,
            pack_index,
            runner_events,
        }
    }

    #[tokio::test]
    async fn resume_session_endpoint_removes_entry() {
        let state = state_with_session("flow-test");
        let req = SessionResumeRequest {
            tenant: Some("dev".into()),
            team: None,
            user: Some("user-test".into()),
            payload: Some(json!({"reply": "hi"})),
        };
        let response = resume_session_http(Extension(state.clone()), Json(req))
            .await
            .expect("resume should succeed");
        assert_eq!(response.flow, "flow-test");
        assert!(!state.runner_events.read().is_empty());

        let filter = SessionFilter::new(Some("dev".into()), None, Some("user-test".into()));
        assert!(state.session_store.find(&filter).unwrap().is_none());
    }

    #[tokio::test]
    async fn resume_session_missing_user() {
        let state = state_with_session("flow-test");
        let req = SessionResumeRequest {
            tenant: Some("dev".into()),
            team: None,
            user: Some("unknown".into()),
            payload: None,
        };
        let status = resume_session_http(Extension(state), Json(req))
            .await
            .unwrap_err();
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn sessions_list_endpoint_reports_entries() {
        let state = state_with_session("flow-list");
        let app = build_router(state.clone());
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/sessions")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let data: SessionListResponse = serde_json::from_slice(&body).unwrap();
        assert_eq!(data.count, 1);
        assert_eq!(
            data.sessions[0].cursor.flow_id.as_deref(),
            Some("flow-list")
        );
    }

    #[tokio::test]
    async fn runner_emit_endpoint_records_event() {
        let state = test_state();
        let app = build_router(state.clone());
        let req = RunnerEmitRequest {
            flow: "flow-demo".into(),
            tenant: Some("dev".into()),
            team: None,
            user: Some("user-emit".into()),
            payload: Some(json!({"text": "hi"})),
        };
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runner/emit")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.runner_events.read().len(), 1);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/runner/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn runner_emit_and_list_returns_echoed_result() {
        let state = test_state();
        let app = build_router(state.clone());
        let payload = json!({"hello": "world", "count": 42});
        let req = RunnerEmitRequest {
            flow: "flow-integration".into(),
            tenant: Some("dev".into()),
            team: Some("team-a".into()),
            user: Some("user-x".into()),
            payload: Some(payload.clone()),
        };

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runner/emit")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/runner/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let body = body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let events: Vec<RunnerEvent> = serde_json::from_slice(&body).unwrap();
        assert_eq!(events.len(), 1);
        let ev = &events[0];
        assert_eq!(ev.flow, "flow-integration");
        assert_eq!(ev.tenant.as_deref(), Some("dev"));
        assert_eq!(ev.team.as_deref(), Some("team-a"));
        assert_eq!(ev.user.as_deref(), Some("user-x"));
        assert_eq!(ev.payload, payload);
        assert_eq!(ev.result["status"], "ok");
        assert_eq!(ev.result["echo"], payload);
    }

    #[tokio::test]
    async fn runner_events_can_be_cleared() {
        let state = test_state();
        let app = build_router(state.clone());
        let req = RunnerEmitRequest {
            flow: "flow-clear".into(),
            tenant: None,
            team: None,
            user: None,
            payload: Some(json!({"foo": "bar"})),
        };
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/runner/emit")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&req).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(state.runner_events.read().len(), 1);

        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/runner/events")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NO_CONTENT);
        assert_eq!(state.runner_events.read().len(), 0);
    }

    fn test_state() -> AppState {
        let config = AppConfig::default();
        let session_store = build_session_store(&config.stores.session).unwrap();
        let pack_index = Arc::new(RwLock::new(PackIndex::default()));
        let runner_events = Arc::new(RwLock::new(Vec::new()));
        let (tx, rx) = mpsc::unbounded_channel();
        let proxy = RunnerHostProxy::new(tx, None);
        tokio::spawn(proxy_runner_loop(rx, runner_events.clone(), None));

        AppState {
            config,
            session_store,
            runner_proxy: proxy,
            pack_index,
            runner_events,
        }
    }

    #[test]
    fn pack_kind_round_trip() {
        let json = r#""application""#;
        let parsed: PackKind = serde_json::from_str(json).expect("parse pack kind");
        assert!(matches!(parsed, PackKind::Application));
        let serialized = serde_json::to_string(&parsed).expect("serialize pack kind");
        assert_eq!(serialized, json);
    }

    #[test]
    fn infer_base_plan_from_pack_manifest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let pack_dir = tmp.path().join("demo-pack");
        fs::create_dir_all(&pack_dir).expect("create pack dir");
        let manifest = r#"
        {
            "id": "demo",
            "name": "Demo Pack",
            "version": "0.1.0",
            "kind": "application",
            "type": "events",
            "scenarios": [
                { "id": "flow_a" },
                { "id": "flow_b" }
            ]
        }
        "#;
        fs::write(pack_dir.join("pack.json"), manifest).expect("write manifest");

        let entry = PackEntry {
            id: "demo".to_string(),
            name: Some("Demo Pack".to_string()),
            kind: Some("application".to_string()),
            path: Utf8PathBuf::from_path_buf(pack_dir.clone()).expect("utf8 path"),
        };

        let plan = infer_base_deployment_plan(&entry, "tenant-1".into(), "staging".into())
            .expect("infer plan");
        assert_eq!(plan.pack_id, "demo");
        assert_eq!(plan.pack_version, Version::parse("0.1.0").unwrap());
        assert_eq!(plan.tenant, "tenant-1");
        assert_eq!(plan.environment, "staging");
        assert_eq!(plan.runners.len(), 1);
        assert_eq!(plan.runners[0].name, "demo-runner");
        assert_eq!(plan.channels.len(), 2);
        assert_eq!(plan.channels[0].flow_id, "flow_a");
        assert_eq!(plan.channels[0].kind, "events");
        assert!(plan.messaging.is_some());
        assert_eq!(plan.messaging.as_ref().unwrap().subjects.len(), 2);
        assert!(plan.telemetry.as_ref().unwrap().required);
    }
}
#[derive(Args, Debug, Default)]
struct ReloadArgs {
    /// When provided, issue POST {server}/packs/reload instead of local rebuild
    #[arg(long)]
    server: Option<String>,
}
#[derive(Subcommand, Debug)]
enum RunnerCommandCli {
    /// Emit a synthetic activity via the runner proxy
    Emit(RunnerEmitArgs),
    /// Fetch runner events from a server
    Events(RunnerEventsArgs),
    /// Clear runner events on a server
    Clear(RunnerClearArgs),
}

#[derive(Args, Debug)]
struct RunnerEmitArgs {
    #[arg(long)]
    flow: String,
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    team: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long, value_name = "JSON")]
    payload: Option<String>,
    /// When provided, issues POST {server}/runner/emit instead of local stub
    #[arg(long)]
    server: Option<String>,
}

#[derive(Args, Debug)]
struct RunnerEventsArgs {
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
}

#[derive(Args, Debug)]
struct RunnerClearArgs {
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
}
fn handle_runner(cmd: RunnerCommandCli) -> Result<()> {
    match cmd {
        RunnerCommandCli::Emit(args) => runner_emit_cli(args)?,
        RunnerCommandCli::Events(args) => runner_events_cli(args)?,
        RunnerCommandCli::Clear(args) => runner_clear_cli(args)?,
    }
    Ok(())
}
#[derive(Args, Debug, Default)]
struct SessionListArgs {
    #[arg(long)]
    tenant: Option<String>,
    #[arg(long)]
    team: Option<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long, default_value = "http://localhost:8080")]
    server: String,
}
