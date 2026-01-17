use std::{
    env, fs,
    io::{Cursor, Read},
    path::PathBuf,
    time::SystemTime,
};

use anyhow::{Context, Result, anyhow};
use flate2::read::GzDecoder;
use greentic_distributor_client::{OciPackFetcher, PackFetchOptions};
use greentic_types::{
    cbor::decode_pack_manifest,
    flow::FlowKind,
    pack_manifest::{PackKind, PackManifest},
    provider::{PROVIDER_EXTENSION_ID, ProviderRuntimeRef},
};
use regex::Regex;
use serde::Serialize;
use serde_yaml_bw::{Mapping, Value as YamlValue};
use tar::Archive;
use zip::ZipArchive;
use zstd::stream::read::Decoder as ZstdDecoder;

#[derive(Clone, Debug)]
struct Config {
    org: String,
    owner_type: OwnerType,
    mode: Mode,
    limit: Option<usize>,
    include: Option<Regex>,
    exclude: Option<Regex>,
    github_token: String,
    output_dir: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OwnerType {
    Org,
    User,
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

    if targets.is_empty() {
        println!("no packages selected; check filters");
        return Ok(());
    }

    let mut pack_opts = PackFetchOptions {
        allow_tags: true,
        ..PackFetchOptions::default()
    };
    let legacy_layer_media_type = "application/vnd.greentic.gtpack+zip";
    if !pack_opts
        .accepted_layer_media_types
        .iter()
        .any(|ty| ty == legacy_layer_media_type)
    {
        pack_opts
            .accepted_layer_media_types
            .push(legacy_layer_media_type.to_string());
    }
    if !pack_opts
        .preferred_layer_media_types
        .iter()
        .any(|ty| ty == legacy_layer_media_type)
    {
        pack_opts
            .preferred_layer_media_types
            .push(legacy_layer_media_type.to_string());
    }
    let pack_fetcher = OciPackFetcher::new(pack_opts);
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to initialize async runtime")?;

    let mut entries = Vec::new();
    for target in targets {
        entries.push(
            audit_single(&pack_fetcher, &runtime, &target).unwrap_or_else(|err| AuditEntry {
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
        let github_user = env_nonempty("GREENTIC_GITHUB_USER")?;
        let github_org = env_nonempty("GREENTIC_GITHUB_ORG")?;
        let (owner_type, org) = match (github_user, github_org) {
            (Some(_), Some(_)) => {
                anyhow::bail!("set only one of GREENTIC_GITHUB_USER or GREENTIC_GITHUB_ORG");
            }
            (Some(user), None) => (OwnerType::User, user.clone()),
            (None, Some(org)) => (OwnerType::Org, org),
            (None, None) => {
                anyhow::bail!(
                    "missing required env var: set GREENTIC_GITHUB_USER or GREENTIC_GITHUB_ORG"
                );
            }
        };
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
        let github_token = require_env_nonempty("GREENTIC_GITHUB_TOKEN")?;
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
            owner_type,
            mode,
            limit,
            include,
            exclude,
            github_token,
            output_dir,
        })
    }
}

fn env_nonempty(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => {
            if value.trim().is_empty() {
                anyhow::bail!("{name} is set but empty; set it to a non-empty value");
            }
            Ok(Some(value))
        }
        Err(_) => Ok(None),
    }
}

fn require_env_nonempty(name: &str) -> Result<String> {
    match env_nonempty(name)? {
        Some(value) => Ok(value),
        None => anyhow::bail!("missing required env var: {name}"),
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

impl OwnerType {
    fn api_segment(&self) -> &'static str {
        match self {
            OwnerType::Org => "orgs",
            OwnerType::User => "users",
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
            "https://api.github.com/{}/{}/packages?package_type=container&per_page=100&page={}",
            cfg.owner_type.api_segment(),
            cfg.org,
            page
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
    let encoded_package = urlencoding::encode(package);
    loop {
        let url = format!(
            "https://api.github.com/{}/{}/packages/container/{}/versions?per_page=100&page={}",
            cfg.owner_type.api_segment(),
            cfg.org,
            encoded_package,
            page
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

fn audit_single(
    fetcher: &OciPackFetcher,
    runtime: &tokio::runtime::Runtime,
    target: &AuditTarget,
) -> Result<AuditEntry> {
    let oci_ref = target.oci_ref();
    let resolved = runtime
        .block_on(fetcher.fetch_pack_to_cache(&oci_ref))
        .map_err(|err| anyhow!("failed to fetch pack {oci_ref}: {err}"))?;
    let pack_bytes = fs::read(&resolved.path)
        .with_context(|| format!("failed to read cached pack {}", resolved.path.display()))?;
    let summary = summarize_pack_bytes(&pack_bytes, target).with_context(|| {
        format!(
            "failed to extract manifest from {} (media type {})",
            oci_ref, resolved.media_type
        )
    })?;
    Ok(AuditEntry {
        package: target.package.clone(),
        tag: target.tag.clone(),
        oci_ref,
        manifest: Some(summary),
        error: None,
    })
}

fn summarize_pack_bytes(pack_bytes: &[u8], target: &AuditTarget) -> Result<ManifestSummary> {
    if let Some(manifest_bytes) = extract_manifest_bytes(pack_bytes)? {
        let manifest = decode_pack_manifest(&manifest_bytes).context("decode manifest.cbor")?;
        return Ok(summarize_manifest(&manifest, target));
    }
    if let Some(yaml_bytes) = extract_gtpack_yaml(pack_bytes)? {
        return summarize_gtpack_yaml(&yaml_bytes, target);
    }
    Err(anyhow!(
        "manifest.cbor or gtpack.yaml not found in pack payload"
    ))
}

fn extract_manifest_bytes(pack_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    if let Ok(mut zip) = ZipArchive::new(Cursor::new(pack_bytes)) {
        if let Some(found) = read_zip_member_bytes(&mut zip, "manifest.cbor") {
            return Ok(Some(found));
        }
        if let Some(nested) = read_zip_nested_suffix(&mut zip, "manifest.cbor")? {
            return Ok(Some(nested));
        }
        return Ok(None);
    }

    let tar_bytes = maybe_decompress_tar_bytes(pack_bytes)?;
    let mut archive = Archive::new(Cursor::new(tar_bytes));
    let mut fallback_manifest: Option<Vec<u8>> = None;

    let entries = archive
        .entries()
        .context("reading tar entries for pack payload")?;
    for entry in entries {
        let mut entry = entry.context("reading tar entry in pack payload")?;
        let path = entry
            .path()
            .context("reading tar entry path in pack payload")?
            .into_owned();
        let path_str = path.to_string_lossy().to_string();
        if path_str.ends_with(".gtpack") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let mut zip = ZipArchive::new(Cursor::new(buf))
                .with_context(|| format!("failed to read zip {}", path_str))?;
            if let Some(found) = read_zip_member_bytes(&mut zip, "manifest.cbor") {
                return Ok(Some(found));
            }
        } else if path_str.ends_with("manifest.cbor") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            fallback_manifest = Some(buf);
        }
    }

    Ok(fallback_manifest)
}

fn extract_gtpack_yaml(pack_bytes: &[u8]) -> Result<Option<Vec<u8>>> {
    if let Ok(mut zip) = ZipArchive::new(Cursor::new(pack_bytes)) {
        if let Some(found) = read_zip_member_bytes(&mut zip, "gtpack.yaml") {
            return Ok(Some(found));
        }
        if let Some(nested) = read_zip_nested_suffix(&mut zip, "gtpack.yaml")? {
            return Ok(Some(nested));
        }
        return Ok(None);
    }

    let tar_bytes = maybe_decompress_tar_bytes(pack_bytes)?;
    let mut archive = Archive::new(Cursor::new(tar_bytes));
    let mut yaml_bytes: Option<Vec<u8>> = None;

    let entries = archive
        .entries()
        .context("reading tar entries for pack payload")?;
    for entry in entries {
        let mut entry = entry.context("reading tar entry in pack payload")?;
        let path = entry
            .path()
            .context("reading tar entry path in pack payload")?
            .into_owned();
        let path_str = path.to_string_lossy().to_string();
        if path_str.ends_with(".gtpack") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            let mut zip = ZipArchive::new(Cursor::new(buf))
                .with_context(|| format!("failed to read zip {}", path_str))?;
            if let Some(found) = read_zip_member_bytes(&mut zip, "gtpack.yaml") {
                return Ok(Some(found));
            }
        } else if path_str.ends_with("gtpack.yaml") {
            let mut buf = Vec::new();
            entry.read_to_end(&mut buf)?;
            yaml_bytes = Some(buf);
        }
    }

    Ok(yaml_bytes)
}

fn maybe_decompress_tar_bytes(pack_bytes: &[u8]) -> Result<Vec<u8>> {
    if pack_bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(pack_bytes);
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .context("failed to decompress gzip pack payload")?;
        return Ok(buf);
    }
    if pack_bytes.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        let mut decoder = ZstdDecoder::new(pack_bytes)
            .context("failed to initialize zstd decoder for pack payload")?;
        let mut buf = Vec::new();
        decoder
            .read_to_end(&mut buf)
            .context("failed to decompress zstd pack payload")?;
        return Ok(buf);
    }
    Ok(pack_bytes.to_vec())
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

fn summarize_gtpack_yaml(yaml_bytes: &[u8], target: &AuditTarget) -> Result<ManifestSummary> {
    let doc: YamlValue = serde_yaml_bw::from_slice(yaml_bytes).context("parse gtpack.yaml")?;
    let pack_id = yaml_str(&doc, "id").ok_or_else(|| anyhow!("gtpack.yaml missing id"))?;
    let version = yaml_str(&doc, "version").unwrap_or("unknown").to_string();
    let schema_version = yaml_str(&doc, "schema_version")
        .unwrap_or("pack-v1")
        .to_string();
    let kind = yaml_str(&doc, "kind").unwrap_or("application").to_string();
    let components = yaml_sequence_len(&doc, "components");
    let supports = Vec::new();
    let extensions = yaml_map_keys(&doc, "extensions");
    let mut providers = yaml_provider_summaries(&doc);
    let default_config_schema = yaml_mapping(&doc)
        .and_then(|map| yaml_mapping_get_str(map, "schemas"))
        .and_then(YamlValue::as_mapping)
        .and_then(|map| yaml_mapping_get_str(map, "config"))
        .and_then(YamlValue::as_str)
        .map(|value| value.to_string());
    if let Some(config_schema_ref) = default_config_schema {
        for provider in &mut providers {
            if provider.config_schema_ref.is_none() {
                provider.config_schema_ref = Some(config_schema_ref.clone());
            }
        }
    }
    let categories = classify_from_fields(&kind, pack_id, target, &supports);

    Ok(ManifestSummary {
        pack_id: pack_id.to_string(),
        version,
        schema_version,
        kind,
        components,
        supports,
        extensions: extensions.clone(),
        categories,
        provider_count: providers.len(),
        providers,
        has_provider_extension: extensions.iter().any(|ext| ext == PROVIDER_EXTENSION_ID),
    })
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
    classify_from_fields(
        pack_kind_str(manifest.kind),
        manifest.pack_id.as_ref(),
        target,
        supports,
    )
}

fn classify_from_fields(
    kind: &str,
    pack_id: &str,
    target: &AuditTarget,
    supports: &[String],
) -> Vec<String> {
    let mut categories = Vec::new();
    if supports.iter().any(|s| s == "messaging") {
        categories.push("messaging".to_string());
    }
    if supports.iter().any(|s| s == "event") {
        categories.push("events".to_string());
    }
    if kind == "provider" && (pack_id.contains("secret") || target.package.contains("secret")) {
        categories.push("secrets".to_string());
    }
    if categories.is_empty() {
        categories.push("other".to_string());
    }
    categories
}

fn yaml_str<'a>(doc: &'a YamlValue, key: &str) -> Option<&'a str> {
    yaml_mapping(doc)
        .and_then(|map| yaml_mapping_get_str(map, key))
        .and_then(YamlValue::as_str)
}

fn yaml_sequence_len(doc: &YamlValue, key: &str) -> usize {
    yaml_mapping(doc)
        .and_then(|map| yaml_mapping_get_str(map, key))
        .and_then(YamlValue::as_sequence)
        .map(|seq| seq.iter().count())
        .unwrap_or(0)
}

fn yaml_map_keys(doc: &YamlValue, key: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let Some(map) = yaml_mapping(doc)
        .and_then(|map| yaml_mapping_get_str(map, key))
        .and_then(YamlValue::as_mapping)
    else {
        return keys;
    };

    for (k, _) in map {
        if let YamlValue::String(value, _) = k {
            keys.push(value.clone());
        }
    }
    keys
}

fn yaml_provider_summaries(doc: &YamlValue) -> Vec<ProviderSummary> {
    let Some(extensions) = yaml_mapping(doc)
        .and_then(|map| yaml_mapping_get_str(map, "extensions"))
        .and_then(YamlValue::as_mapping)
    else {
        return Vec::new();
    };
    let Some(provider_ext) = yaml_mapping_get_str(extensions, PROVIDER_EXTENSION_ID) else {
        return Vec::new();
    };

    if let Some(provider) =
        yaml_mapping(provider_ext).and_then(|map| yaml_mapping_get_str(map, "provider"))
    {
        if let Some(summary) = yaml_provider_summary(provider) {
            return vec![summary];
        }
        return Vec::new();
    }

    if let Some(providers) = yaml_mapping(provider_ext)
        .and_then(|map| yaml_mapping_get_str(map, "providers"))
        .and_then(YamlValue::as_sequence)
    {
        return providers.iter().filter_map(yaml_provider_summary).collect();
    }

    Vec::new()
}

fn yaml_provider_summary(value: &YamlValue) -> Option<ProviderSummary> {
    let provider_type = yaml_mapping(value)
        .and_then(|map| {
            yaml_mapping_get_str(map, "id")
                .or_else(|| yaml_mapping_get_str(map, "provider_type"))
                .or_else(|| yaml_mapping_get_str(map, "type"))
        })
        .and_then(YamlValue::as_str)?
        .to_string();
    let config_schema_ref = yaml_mapping(value)
        .and_then(|map| yaml_mapping_get_str(map, "config_schema_ref"))
        .and_then(YamlValue::as_str)
        .map(|s| s.to_string());
    let runtime = yaml_mapping(value)
        .and_then(|map| yaml_mapping_get_str(map, "runtime"))
        .and_then(YamlValue::as_mapping)
        .map(|map| ProviderRuntimeSummary {
            component_ref: yaml_mapping_get_str(map, "component_ref")
                .or_else(|| yaml_mapping_get_str(map, "component"))
                .and_then(YamlValue::as_str)
                .unwrap_or_default()
                .to_string(),
            export: yaml_mapping_get_str(map, "export")
                .and_then(YamlValue::as_str)
                .unwrap_or_default()
                .to_string(),
            world: yaml_mapping_get_str(map, "world")
                .and_then(YamlValue::as_str)
                .unwrap_or_default()
                .to_string(),
        });

    Some(ProviderSummary {
        provider_type,
        config_schema_ref,
        runtime,
    })
}

fn yaml_mapping(doc: &YamlValue) -> Option<&Mapping> {
    doc.as_mapping()
}

fn yaml_mapping_get_str<'a>(map: &'a Mapping, key: &str) -> Option<&'a YamlValue> {
    map.get(YamlValue::String(key.to_string(), None))
}

fn read_zip_member_bytes<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    suffix: &str,
) -> Option<Vec<u8>> {
    for i in 0..zip.len() {
        let Ok(mut f) = zip.by_index(i) else {
            continue;
        };
        if !f.name().ends_with(suffix) {
            continue;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_ok() {
            return Some(buf);
        }
    }
    None
}

fn read_zip_nested_suffix<R: Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    suffix: &str,
) -> Result<Option<Vec<u8>>> {
    for i in 0..zip.len() {
        let Ok(mut f) = zip.by_index(i) else {
            continue;
        };
        if !f.name().ends_with(".gtpack") {
            continue;
        }
        let mut buf = Vec::new();
        if f.read_to_end(&mut buf).is_err() {
            continue;
        }
        let mut nested = ZipArchive::new(Cursor::new(buf))
            .with_context(|| format!("failed to read zip {}", f.name()))?;
        if let Some(found) = read_zip_member_bytes(&mut nested, suffix) {
            return Ok(Some(found));
        }
    }
    Ok(None)
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
