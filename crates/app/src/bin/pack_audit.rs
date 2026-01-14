use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow};
use greentic_types::{
    cbor::decode_pack_manifest,
    flow::FlowKind,
    pack_manifest::{PackKind, PackManifest},
    provider::{PROVIDER_EXTENSION_ID, ProviderRuntimeRef},
};
use regex::Regex;
use serde::Serialize;
use tempfile::tempdir_in;
use walkdir::WalkDir;
use zip::ZipArchive;

#[derive(Clone, Debug)]
struct Config {
    org: String,
    mode: Mode,
    limit: Option<usize>,
    include: Option<Regex>,
    exclude: Option<Regex>,
    github_token: String,
    ghcr_token: String,
    github_actor: String,
    auto_login: bool,
    crane_bin: String,
    output_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Latest,
    All,
}

#[derive(Debug, Serialize)]
struct AuditIndex {
    generated_at: String,
    org: String,
    mode: String,
    entries: Vec<AuditEntry>,
}

#[derive(Debug, Serialize)]
struct AuditEntry {
    package: String,
    tag: String,
    oci_ref: String,
    manifest: Option<ManifestSummary>,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct ManifestSummary {
    pack_id: String,
    version: String,
    schema_version: String,
    kind: String,
    components: usize,
    supports: Vec<String>,
    extensions: Vec<String>,
    categories: Vec<String>,
    provider_count: usize,
    providers: Vec<ProviderSummary>,
    has_provider_extension: bool,
}

#[derive(Debug, Serialize)]
struct ProviderSummary {
    provider_type: String,
    config_schema_ref: Option<String>,
    runtime: Option<ProviderRuntimeSummary>,
}

#[derive(Debug, Serialize)]
struct ProviderRuntimeSummary {
    component_ref: String,
    export: String,
    world: String,
}

#[derive(Debug, serde::Deserialize)]
struct GithubPackage {
    name: String,
}

#[derive(Debug, serde::Deserialize)]
#[allow(dead_code)]
struct GithubPackageVersion {
    id: u64,
    name: Option<String>,
    updated_at: Option<String>,
    metadata: Option<PackageVersionMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct PackageVersionMetadata {
    container: Option<PackageContainerMetadata>,
}

#[derive(Debug, serde::Deserialize)]
struct PackageContainerMetadata {
    tags: Option<Vec<String>>,
}

fn main() -> Result<()> {
    let cfg = Config::from_env()?;
    fs::create_dir_all(&cfg.output_dir)
        .with_context(|| format!("failed to create {}", cfg.output_dir.display()))?;

    let agent = github_agent()?;
    let packages = fetch_packages(&agent, &cfg)?;
    let targets = collect_targets(&agent, &cfg, &packages)?;

    ensure_crane_available(&cfg)?;

    if targets.is_empty() {
        println!("no packages selected; check filters");
        return Ok(());
    }

    if cfg.auto_login {
        login_crane(&cfg)?;
    }

    preflight_auth(&cfg, &targets[0].oci_ref())?;

    let mut entries = Vec::new();
    for target in targets {
        entries.push(
            audit_single(&cfg, &target).unwrap_or_else(|err| AuditEntry {
                package: target.package.clone(),
                tag: target.tag.clone(),
                oci_ref: target.oci_ref(),
                manifest: None,
                error: Some(err.to_string()),
            }),
        );
    }

    let index = AuditIndex {
        generated_at: humantime::format_rfc3339_seconds(SystemTime::now()).to_string(),
        org: cfg.org.clone(),
        mode: cfg.mode.as_str().to_string(),
        entries,
    };

    let index_path = cfg.output_dir.join("pack_index.json");
    fs::write(&index_path, serde_json::to_vec_pretty(&index)?)
        .with_context(|| format!("failed to write {}", index_path.display()))?;

    let summary_path = cfg.output_dir.join("pack_index.md");
    fs::write(&summary_path, render_summary(&index))
        .with_context(|| format!("failed to write {}", summary_path.display()))?;

    println!(
        "wrote {} and {} ({} entries)",
        index_path.display(),
        summary_path.display(),
        index.entries.len()
    );

    Ok(())
}

impl Config {
    fn from_env() -> Result<Self> {
        let org = env::var("GT_PACKS_ORG").unwrap_or_else(|_| "greentic-ai".to_string());
        let mode = match env::var("GT_PACKS_MODE")
            .unwrap_or_else(|_| "latest".to_string())
            .as_str()
        {
            "latest" => Mode::Latest,
            "all" => Mode::All,
            other => anyhow::bail!("unsupported GT_PACKS_MODE '{}'", other),
        };
        let limit = env::var("GT_PACKS_LIMIT").ok().and_then(|v| v.parse().ok());
        let include = env::var("GT_PACKS_INCLUDE_REGEX")
            .ok()
            .and_then(|v| Regex::new(&v).ok());
        let exclude = env::var("GT_PACKS_EXCLUDE_REGEX")
            .ok()
            .and_then(|v| Regex::new(&v).ok());
        let github_token = env::var("GITHUB_TOKEN")
            .context("GITHUB_TOKEN is required to list/pull packages from GHCR")?;
        let ghcr_token = env::var("GHCR_TOKEN").unwrap_or_else(|_| github_token.clone());
        let github_actor = env::var("GITHUB_ACTOR").unwrap_or_else(|_| "oauth2".to_string());
        let auto_login = env::var("GT_CRANE_LOGIN")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        let crane_bin = env::var("CRANE_BIN").unwrap_or_else(|_| "crane".to_string());
        let output_dir = env::var("GT_PACK_AUDIT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .parent()
                    .expect("workspace root")
                    .join("target")
                    .join("pack-audit")
            });

        Ok(Self {
            org,
            mode,
            limit,
            include,
            exclude,
            github_token,
            ghcr_token,
            github_actor,
            auto_login,
            crane_bin,
            output_dir,
        })
    }
}

impl Mode {
    fn as_str(&self) -> &'static str {
        match self {
            Mode::Latest => "latest",
            Mode::All => "all",
        }
    }
}

#[derive(Clone, Debug)]
struct AuditTarget {
    package: String,
    tag: String,
    org: String,
}

impl AuditTarget {
    fn oci_ref(&self) -> String {
        format!("ghcr.io/{}/{}:{}", self.org, self.package, self.tag)
    }
}

fn github_agent() -> Result<ureq::Agent> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .user_agent("greentic-integration-pack-audit/0.1")
        .build()
        .into();
    Ok(agent)
}

fn fetch_packages(agent: &ureq::Agent, cfg: &Config) -> Result<Vec<GithubPackage>> {
    let mut page = 1;
    let mut packages = Vec::new();
    loop {
        let url = format!(
            "https://api.github.com/orgs/{}/packages?package_type=container&per_page=100&page={}",
            cfg.org, page
        );
        let resp = agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", &format!("Bearer {}", cfg.github_token))
            .call()
            .context("failed to fetch packages")?;
        if resp.status() == 304 {
            break;
        }
        let body = resp
            .into_body()
            .read_to_string()
            .context("read packages body")?;
        let chunk: Vec<GithubPackage> =
            serde_json::from_str(&body).context("parse packages response")?;
        if chunk.is_empty() {
            break;
        }
        for pkg in chunk {
            if !pkg.name.starts_with("greentic-packs/") {
                continue;
            }
            if let Some(ref include) = cfg.include
                && !include.is_match(&pkg.name)
            {
                continue;
            }
            if let Some(ref exclude) = cfg.exclude
                && exclude.is_match(&pkg.name)
            {
                continue;
            }
            packages.push(pkg);
            if let Some(limit) = cfg.limit
                && packages.len() >= limit
            {
                return Ok(packages);
            }
        }
        page += 1;
    }
    Ok(packages)
}

fn collect_targets(
    agent: &ureq::Agent,
    cfg: &Config,
    packages: &[GithubPackage],
) -> Result<Vec<AuditTarget>> {
    let mut targets = Vec::new();
    for pkg in packages {
        let versions = fetch_versions(agent, cfg, &pkg.name)?;
        match cfg.mode {
            Mode::Latest => {
                if let Some(version) = versions.into_iter().find(|v| !v.tags().is_empty())
                    && let Some(tag) = version.tags().first().cloned()
                {
                    targets.push(AuditTarget {
                        package: pkg.name.clone(),
                        tag,
                        org: cfg.org.clone(),
                    });
                }
            }
            Mode::All => {
                for version in versions {
                    for tag in version.tags() {
                        targets.push(AuditTarget {
                            package: pkg.name.clone(),
                            tag,
                            org: cfg.org.clone(),
                        });
                    }
                }
            }
        }
    }
    Ok(targets)
}

impl GithubPackageVersion {
    fn tags(&self) -> Vec<String> {
        self.metadata
            .as_ref()
            .and_then(|m| m.container.as_ref())
            .and_then(|c| c.tags.clone())
            .unwrap_or_default()
    }
}

fn fetch_versions(
    agent: &ureq::Agent,
    cfg: &Config,
    package: &str,
) -> Result<Vec<GithubPackageVersion>> {
    let mut versions = Vec::new();
    let mut page = 1;
    loop {
        let url = format!(
            "https://api.github.com/orgs/{}/packages/container/{}/versions?per_page=100&page={}",
            cfg.org, package, page
        );
        let resp = agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("Authorization", &format!("Bearer {}", cfg.github_token))
            .call()
            .with_context(|| format!("failed to fetch versions for {package}"))?;
        let body = resp
            .into_body()
            .read_to_string()
            .context("read versions body")?;
        let chunk: Vec<GithubPackageVersion> =
            serde_json::from_str(&body).context("parse versions response")?;
        if chunk.is_empty() {
            break;
        }
        versions.extend(chunk);
        page += 1;
    }
    Ok(versions)
}

fn ensure_crane_available(cfg: &Config) -> Result<()> {
    let status = Command::new(&cfg.crane_bin)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    match status {
        Ok(status) if status.success() => Ok(()),
        _ => anyhow::bail!(
            "crane not found or not executable. Install crane (https://github.com/google/go-containerregistry) and ensure it is on PATH."
        ),
    }
}

fn login_crane(cfg: &Config) -> Result<()> {
    let status = Command::new(&cfg.crane_bin)
        .args([
            "auth",
            "login",
            "ghcr.io",
            "--username",
            &cfg.github_actor,
            "--password-stdin",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .and_then(|mut child| {
            if let Some(stdin) = child.stdin.as_mut() {
                stdin.write_all(cfg.ghcr_token.as_bytes())?;
            }
            child.wait()
        })
        .map_err(|err| anyhow!("failed to run crane auth login: {err}"))?;
    if !status.success() {
        anyhow::bail!(
            "crane auth login exited with {status:?}. Tried username '{}' with provided token.",
            cfg.github_actor
        );
    }
    Ok(())
}

fn preflight_auth(cfg: &Config, sample_ref: &str) -> Result<()> {
    let mut cmd = Command::new(&cfg.crane_bin);
    cmd.args(["manifest", sample_ref]);
    if let Ok(cfg_path) = env::var("DOCKER_CONFIG") {
        cmd.env("DOCKER_CONFIG", cfg_path);
    }
    cmd.env("GITHUB_TOKEN", &cfg.ghcr_token);
    let output = cmd
        .output()
        .with_context(|| format!("failed to run crane manifest for {sample_ref}"))?;
    if output.status.success() {
        return Ok(());
    }

    let suggestion = "crane is not authenticated to ghcr.io (or token lacks packages:read).\n\
         Remediation:\n  echo \"${GITHUB_TOKEN}\" | crane auth login ghcr.io -u \"${GITHUB_ACTOR:-oauth2}\" --password-stdin\n\
         Ensure GitHub Actions permissions include: packages: read"
        .to_string();
    let stderr = String::from_utf8_lossy(&output.stderr);
    anyhow::bail!(
        "crane manifest {sample_ref} failed (status {:?}): {}\n{}",
        output.status.code(),
        stderr,
        suggestion
    );
}

fn audit_single(cfg: &Config, target: &AuditTarget) -> Result<AuditEntry> {
    let oci_ref = target.oci_ref();
    let tmp = tempdir_in(&cfg.output_dir)?;
    run_crane_export(&cfg.crane_bin, &cfg.ghcr_token, &oci_ref, tmp.path())?;
    let manifest_bytes = extract_manifest_bytes(tmp.path())
        .with_context(|| format!("failed to extract manifest from {}", oci_ref))?;
    let manifest =
        decode_pack_manifest(&manifest_bytes).with_context(|| format!("decode {}", oci_ref))?;
    let summary = summarize_manifest(&manifest, target);
    Ok(AuditEntry {
        package: target.package.clone(),
        tag: target.tag.clone(),
        oci_ref,
        manifest: Some(summary),
        error: None,
    })
}

fn run_crane_export(crane_bin: &str, token: &str, oci_ref: &str, out_dir: &Path) -> Result<()> {
    let mut cmd = Command::new(crane_bin);
    cmd.args(["export", oci_ref, out_dir.to_str().unwrap()]);
    if let Ok(cfg) = env::var("DOCKER_CONFIG") {
        cmd.env("DOCKER_CONFIG", cfg);
    }
    cmd.env("GITHUB_TOKEN", token);
    let status = cmd
        .status()
        .with_context(|| format!("failed to spawn crane for {}", oci_ref))?;
    if !status.success() {
        anyhow::bail!("crane export failed for {}: {status:?}", oci_ref);
    }
    Ok(())
}

fn extract_manifest_bytes(root: &Path) -> Result<Vec<u8>> {
    // Prefer .gtpack artifacts.
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("gtpack") {
            let file = fs::File::open(entry.path())
                .with_context(|| format!("failed to open {}", entry.path().display()))?;
            let mut archive = ZipArchive::new(file)
                .with_context(|| format!("failed to read zip {}", entry.path().display()))?;
            for i in 0..archive.len() {
                let mut f = archive.by_index(i)?;
                if f.name().ends_with("manifest.cbor") {
                    let mut buf = Vec::new();
                    f.read_to_end(&mut buf)?;
                    return Ok(buf);
                }
            }
        }
    }

    // Fallback to raw manifest.cbor.
    for entry in WalkDir::new(root).into_iter().filter_map(Result::ok) {
        if entry
            .file_name()
            .to_str()
            .map(|n| n == "manifest.cbor")
            .unwrap_or(false)
        {
            return fs::read(entry.path())
                .with_context(|| format!("failed to read {}", entry.path().display()));
        }
    }

    Err(anyhow!("manifest.cbor not found under {}", root.display()))
}

fn summarize_manifest(manifest: &PackManifest, target: &AuditTarget) -> ManifestSummary {
    let supports = manifest
        .components
        .iter()
        .flat_map(|c| c.supports.iter())
        .map(flow_kind_str)
        .collect::<Vec<_>>();

    let extensions = manifest
        .extensions
        .as_ref()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();

    let providers = manifest
        .provider_extension_inline()
        .map(|inline| {
            inline
                .providers
                .iter()
                .map(|p| ProviderSummary {
                    provider_type: p.provider_type.clone(),
                    config_schema_ref: Some(p.config_schema_ref.clone()),
                    runtime: Some(ProviderRuntimeSummary::from(&p.runtime)),
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let categories = classify(manifest, target, &supports);

    ManifestSummary {
        pack_id: manifest.pack_id.to_string(),
        version: manifest.version.to_string(),
        schema_version: manifest.schema_version.clone(),
        kind: pack_kind_str(manifest.kind).to_string(),
        components: manifest.components.len(),
        supports,
        extensions,
        categories,
        provider_count: providers.len(),
        providers,
        has_provider_extension: manifest
            .extensions
            .as_ref()
            .map(|m| m.contains_key(PROVIDER_EXTENSION_ID))
            .unwrap_or(false),
    }
}

impl From<&ProviderRuntimeRef> for ProviderRuntimeSummary {
    fn from(value: &ProviderRuntimeRef) -> Self {
        Self {
            component_ref: value.component_ref.clone(),
            export: value.export.clone(),
            world: value.world.clone(),
        }
    }
}

fn classify(manifest: &PackManifest, target: &AuditTarget, supports: &[String]) -> Vec<String> {
    let mut categories = Vec::new();
    if supports.iter().any(|s| s == "messaging") {
        categories.push("messaging".to_string());
    }
    if supports.iter().any(|s| s == "event") {
        categories.push("events".to_string());
    }
    if manifest.kind == PackKind::Provider
        && (manifest.pack_id.to_string().contains("secret") || target.package.contains("secret"))
    {
        categories.push("secrets".to_string());
    }
    if categories.is_empty() {
        categories.push("other".to_string());
    }
    categories
}

fn flow_kind_str(kind: &FlowKind) -> String {
    match kind {
        FlowKind::Messaging => "messaging",
        FlowKind::Event => "event",
        FlowKind::ComponentConfig => "component_config",
        FlowKind::Job => "job",
        FlowKind::Http => "http",
    }
    .to_string()
}

fn pack_kind_str(kind: PackKind) -> &'static str {
    match kind {
        PackKind::Application => "application",
        PackKind::Provider => "provider",
        PackKind::Infrastructure => "infrastructure",
        PackKind::Library => "library",
    }
}

fn render_summary(index: &AuditIndex) -> String {
    let mut out = String::new();
    out.push_str("# Pack Audit Summary\n\n");
    out.push_str(&format!(
        "- org: {}\n- mode: {}\n- entries: {}\n\n",
        index.org,
        index.mode,
        index.entries.len()
    ));

    let mut category_counts = std::collections::BTreeMap::<String, usize>::new();
    let mut failures = Vec::new();

    for entry in &index.entries {
        if let Some(err) = &entry.error {
            failures.push(format!("{} ({}): {}", entry.package, entry.tag, err));
            continue;
        }
        if let Some(manifest) = &entry.manifest {
            for cat in &manifest.categories {
                *category_counts.entry(cat.clone()).or_default() += 1;
            }
        }
    }

    out.push_str("## Categories\n");
    for (cat, count) in category_counts {
        out.push_str(&format!("- {}: {}\n", cat, count));
    }

    if !failures.is_empty() {
        out.push_str("\n## Failures\n");
        for fail in failures {
            out.push_str(&format!("- {fail}\n"));
        }
    }

    out
}
