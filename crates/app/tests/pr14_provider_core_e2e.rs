use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize, Clone)]
struct ProviderPack {
    id: String,
    #[allow(dead_code)]
    version: String,
    #[serde(default)]
    #[allow(dead_code)]
    name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    kind: Option<String>,
    provider_core: ProviderCoreSpec,
}

#[derive(Debug, Deserialize, Clone)]
struct ProviderCoreSpec {
    provider_type: String,
    capabilities: Vec<String>,
    operations: BTreeMap<String, OperationSpec>,
    config_schema: String,
    prompts_schema: String,
    validate_config: String,
}

#[derive(Debug, Deserialize, Clone)]
struct OperationSpec {
    schema: String,
}

#[derive(Debug, Clone)]
struct MessageReceipt {
    message_id: String,
    echo: String,
    channel: String,
}

#[derive(Debug, Clone)]
struct EventRecord {
    id: String,
    topic: String,
    payload: Value,
}

#[derive(Debug, Clone)]
struct EventReceipt {
    id: String,
    topic: String,
}

#[derive(Debug)]
struct DummyProvider {
    manifest: ProviderPack,
    root: PathBuf,
    secrets: HashMap<String, Value>,
    messages: Vec<MessageReceipt>,
    events: Vec<EventRecord>,
    validate_config_calls: usize,
    message_seq: u64,
    event_seq: u64,
    config: Option<Value>,
}

impl DummyProvider {
    fn install(path: &Path) -> Result<Self> {
        enforce_provider_core_env()?;
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read provider pack at {}", path.display()))?;
        let value: Value = serde_json::from_str(&raw).context("provider pack is not valid JSON")?;
        ensure_no_legacy_protocols(&value)?;
        let manifest: ProviderPack = serde_json::from_value(value.clone())
            .context("provider pack manifest missing required provider_core fields")?;
        let root = path
            .parent()
            .ok_or_else(|| anyhow::anyhow!("provider pack missing parent directory"))?
            .to_path_buf();

        Ok(Self {
            manifest,
            root,
            secrets: HashMap::new(),
            messages: Vec::new(),
            events: Vec::new(),
            validate_config_calls: 0,
            message_seq: 0,
            event_seq: 0,
            config: None,
        })
    }

    fn provider_type(&self) -> &str {
        &self.manifest.provider_core.provider_type
    }

    fn operation_schema(&self, op: &str) -> Result<PathBuf> {
        let spec = self
            .manifest
            .provider_core
            .operations
            .get(op)
            .with_context(|| format!("operation {op} not declared in pack"))?;
        Ok(self.root.join(&spec.schema))
    }

    fn validate_against(&self, schema_path: &Path, payload: &Value) -> Result<()> {
        let schema = load_json(schema_path)?;
        if schema
            .get("type")
            .and_then(|v| v.as_str())
            .map(|t| t != "object")
            .unwrap_or(false)
        {
            bail!(
                "schema at {} is not an object schema",
                schema_path.display()
            );
        }
        let payload_obj = payload
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("payload is not an object"))?;
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .cloned()
            .unwrap_or_default();
        let required = schema
            .get("required")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for field in required {
            let Some(name) = field.as_str() else {
                continue;
            };
            let Some(value) = payload_obj.get(name) else {
                bail!(
                    "schema validation failed ({}): missing required field {}",
                    schema_path.display(),
                    name
                );
            };
            if let Some(def) = properties.get(name)
                && !value_matches_type(value, def.get("type"))
            {
                bail!(
                    "schema validation failed ({}): field {} has wrong type",
                    schema_path.display(),
                    name
                );
            }
        }
        Ok(())
    }

    fn validate_config(&mut self, config: Value) -> Result<Vec<Value>> {
        let provider_core = &self.manifest.provider_core;
        let schema_path = self.root.join(&provider_core.config_schema);
        self.validate_against(&schema_path, &config)?;

        let prompts_path = self.root.join(&provider_core.prompts_schema);
        let prompts: Value = load_json(&prompts_path)?;
        let prompts_array = prompts.as_array().cloned().unwrap_or_default();
        if prompts_array.is_empty() {
            bail!("prompts file {} missing or empty", prompts_path.display());
        }

        let validate_path = self.root.join(&provider_core.validate_config);
        let hook: Value = load_json(&validate_path)?;
        let status = hook
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        if status != "ok" {
            bail!(
                "validate-config hook under {} did not report ok status",
                validate_path.display()
            );
        }
        self.validate_config_calls += 1;
        self.config = Some(config);
        Ok(prompts_array)
    }

    fn secrets_put(&mut self, key: &str, value: Value) -> Result<()> {
        if self.provider_type() != "secrets" {
            bail!("secrets_put called on non-secrets provider");
        }
        let payload = json!({ "key": key, "value": value.clone() });
        let schema = self.operation_schema("put")?;
        self.validate_against(&schema, &payload)?;
        let namespaced_key = self.namespaced_key(key);
        self.secrets.insert(namespaced_key, value);
        Ok(())
    }

    fn secrets_get(&self, key: &str) -> Result<Value> {
        if self.provider_type() != "secrets" {
            bail!("secrets_get called on non-secrets provider");
        }
        let payload = json!({ "key": key });
        let schema = self.operation_schema("get")?;
        self.validate_against(&schema, &payload)?;
        let namespaced_key = self.namespaced_key(key);
        self.secrets
            .get(&namespaced_key)
            .cloned()
            .with_context(|| format!("secret {namespaced_key} missing from in-memory store"))
    }

    fn send_message(
        &mut self,
        text: &str,
        channel_override: Option<&str>,
    ) -> Result<MessageReceipt> {
        if self.provider_type() != "messaging" {
            bail!("send_message called on non-messaging provider");
        }
        let channel = match (channel_override, &self.config) {
            (Some(c), _) => c.to_string(),
            (None, Some(cfg)) => cfg
                .get("channel")
                .and_then(|v| v.as_str())
                .ok_or_else(|| anyhow::anyhow!("provider config missing channel"))?
                .to_string(),
            (None, None) => bail!("no channel provided and provider not onboarded"),
        };
        let payload = json!({ "text": text, "channel": channel });
        let schema = self.operation_schema("send")?;
        self.validate_against(&schema, &payload)?;
        self.message_seq += 1;
        let receipt = MessageReceipt {
            message_id: format!("msg-{}", self.message_seq),
            echo: text.to_string(),
            channel: payload
                .get("channel")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string(),
        };
        self.messages.push(receipt.clone());
        Ok(receipt)
    }

    fn publish_event(&mut self, topic: &str, payload: Value) -> Result<EventReceipt> {
        if self.provider_type() != "events" {
            bail!("publish_event called on non-events provider");
        }
        let payload_with_topic = json!({ "topic": topic, "payload": payload });
        let schema = self.operation_schema("publish")?;
        self.validate_against(&schema, &payload_with_topic)?;
        self.event_seq += 1;
        let final_topic = if let Some(cfg) = &self.config {
            if let Some(prefix) = cfg.get("topic_prefix").and_then(|v| v.as_str()) {
                format!("{prefix}.{}", topic)
            } else {
                topic.to_string()
            }
        } else {
            topic.to_string()
        };
        let record = EventRecord {
            id: format!("evt-{}", self.event_seq),
            topic: final_topic.clone(),
            payload: payload_with_topic
                .get("payload")
                .cloned()
                .unwrap_or(Value::Null),
        };
        self.events.push(record.clone());
        Ok(EventReceipt {
            id: record.id,
            topic: record.topic,
        })
    }

    fn namespaced_key(&self, key: &str) -> String {
        let mut namespaced = String::new();
        if let Some(cfg) = &self.config
            && let Some(ns) = cfg.get("namespace").and_then(|v| v.as_str())
        {
            namespaced.push_str(ns);
            namespaced.push('/');
        }
        namespaced.push_str(key);
        namespaced
    }

    fn message_log(&self) -> &[MessageReceipt] {
        &self.messages
    }

    fn event_log(&self) -> &[EventRecord] {
        &self.events
    }
}

#[test]
fn pr14_provider_core_flows_and_index() -> Result<()> {
    enforce_provider_core_env()?;

    // Secrets flow: in-memory provider-core pack, put/get round-trip.
    let secrets_path = pack_fixture("provider_secrets_dummy/provider_secrets_dummy.gtpack");
    let mut secrets = DummyProvider::install(&secrets_path)?;
    let secrets_config = load_json(&secrets.root.join("schemas/config.example.json"))?;
    secrets.validate_config(secrets_config)?;
    secrets.secrets_put("a", json!(1))?;
    let retrieved = secrets.secrets_get("a")?;
    assert_eq!(retrieved, json!(1));

    // Messaging flow: send should echo text and emit a message id.
    let messaging_path = pack_fixture("provider_messaging_dummy/provider_messaging_dummy.gtpack");
    let mut messaging = DummyProvider::install(&messaging_path)?;
    let messaging_config = load_json(&messaging.root.join("schemas/config.example.json"))?;
    messaging.validate_config(messaging_config)?;
    let sent = messaging.send_message("hello provider-core", None)?;
    assert!(sent.message_id.starts_with("msg-"));
    assert_eq!(sent.echo, "hello provider-core");
    assert_eq!(sent.channel, "echo-room");
    assert_eq!(messaging.message_log().len(), 1);

    // Events flow: publish should apply topic prefix and persist state.
    let events_path = pack_fixture("provider_events_dummy/provider_events_dummy.gtpack");
    let mut events = DummyProvider::install(&events_path)?;
    let events_config = load_json(&events.root.join("schemas/config.example.json"))?;
    events.validate_config(events_config)?;
    let event_payload = json!({"detail": "demo"});
    let receipt = events.publish_event("topic.t", event_payload.clone())?;
    assert!(receipt.id.starts_with("evt-"));
    assert_eq!(receipt.topic, "demo.topic.t");
    assert_eq!(events.event_log().len(), 1);
    assert_eq!(events.event_log()[0].payload, event_payload);

    // Provider index should surface provider_type, capabilities, and ops for every pack fixture.
    let index = build_provider_index()?;
    assert_eq!(index.len(), 3, "expected three provider fixtures indexed");
    assert_provider_entry(
        &index,
        "provider_secrets_dummy",
        "secrets",
        &["put", "get"],
        &["put", "get"],
    );
    assert_provider_entry(
        &index,
        "provider_messaging_dummy",
        "messaging",
        &["send"],
        &["send"],
    );
    assert_provider_entry(
        &index,
        "provider_events_dummy",
        "events",
        &["publish"],
        &["publish"],
    );

    Ok(())
}

#[test]
fn pr14_provider_core_schema_onboarding() -> Result<()> {
    enforce_provider_core_env()?;
    let secrets_path = pack_fixture("provider_secrets_dummy/provider_secrets_dummy.gtpack");
    let mut provider = DummyProvider::install(&secrets_path)?;

    // Invalid config should fail schema validation.
    let bad = json!({});
    let err = provider.validate_config(bad).unwrap_err();
    assert!(
        err.to_string().contains("schema validation failed"),
        "expected schema validation to fail for missing fields"
    );

    // Valid config flows through prompts and validate-config hook.
    let config = load_json(&provider.root.join("schemas/config.example.json"))?;
    let prompts = provider.validate_config(config)?;
    assert_eq!(
        provider.validate_config_calls, 1,
        "validate-config hook not invoked"
    );
    assert!(
        !prompts.is_empty(),
        "prompts should be returned for onboarding"
    );
    let prompt_fields: Vec<String> = prompts
        .iter()
        .filter_map(|p| {
            p.get("field")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .collect();
    assert!(prompt_fields.contains(&"namespace".to_string()));

    Ok(())
}

fn pack_fixture(relative: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root missing")
        .join("tests")
        .join("fixtures")
        .join("packs");
    root.join(relative)
}

fn load_json(path: &Path) -> Result<Value> {
    let data = fs::read_to_string(path)
        .with_context(|| format!("failed to read JSON at {}", path.display()))?;
    serde_json::from_str(&data).with_context(|| format!("invalid JSON at {}", path.display()))
}

fn value_matches_type(value: &Value, type_field: Option<&Value>) -> bool {
    let Some(type_field) = type_field else {
        return true;
    };
    let matches = |expected: &str| match expected {
        "string" => value.is_string(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "array" => value.is_array(),
        _ => true,
    };
    match type_field {
        Value::String(s) => matches(s),
        Value::Array(items) => items
            .iter()
            .any(|v| v.as_str().map(matches).unwrap_or(false)),
        _ => true,
    }
}

fn ensure_no_legacy_protocols(raw: &Value) -> Result<()> {
    let Some(map) = raw.as_object() else {
        return Ok(());
    };
    let forbidden_keys = ["legacy_provider", "provider_protocol", "legacy_protocols"];
    for key in forbidden_keys {
        if map.contains_key(key) {
            bail!("legacy provider protocol key '{key}' present in pack manifest");
        }
    }
    Ok(())
}

fn enforce_provider_core_env() -> Result<()> {
    let value = std::env::var("GREENTIC_PROVIDER_CORE_ONLY").unwrap_or_default();
    if value != "1" {
        bail!(
            "GREENTIC_PROVIDER_CORE_ONLY must be set to 1 for provider-core E2E tests (got {value:?})"
        );
    }
    Ok(())
}

fn build_provider_index() -> Result<Vec<ProviderIndexEntry>> {
    let root = pack_fixture("");
    let mut entries = Vec::new();
    for entry in fs::read_dir(&root).with_context(|| format!("scan {}", root.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let pack_path = entry
            .path()
            .join(format!("{}.gtpack", entry.file_name().to_string_lossy()));
        if !pack_path.exists() {
            continue;
        }
        let manifest: Value = load_json(&pack_path)?;
        ensure_no_legacy_protocols(&manifest)?;
        let provider: ProviderPack = serde_json::from_value(manifest)?;
        entries.push(ProviderIndexEntry {
            id: provider.id,
            provider_type: provider.provider_core.provider_type,
            capabilities: provider.provider_core.capabilities,
            operations: provider.provider_core.operations.keys().cloned().collect(),
        });
    }
    Ok(entries)
}

#[derive(Debug)]
struct ProviderIndexEntry {
    id: String,
    provider_type: String,
    capabilities: Vec<String>,
    operations: Vec<String>,
}

fn assert_provider_entry(
    index: &[ProviderIndexEntry],
    id: &str,
    provider_type: &str,
    capabilities: &[&str],
    operations: &[&str],
) {
    let entry = index
        .iter()
        .find(|e| e.id == id)
        .unwrap_or_else(|| panic!("missing provider {id} in index"));
    assert_eq!(entry.provider_type, provider_type);
    for capability in capabilities {
        assert!(
            entry.capabilities.contains(&capability.to_string()),
            "provider {id} missing capability {capability}"
        );
    }
    for op in operations {
        assert!(
            entry.operations.contains(&op.to_string()),
            "provider {id} missing op {op}"
        );
    }
}
