use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

const DEFAULT_REMOVE_FIELDS: &[&str] = &["meta.trace_id", "meta.timestamp", "envelope.trace_id"];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizeConfig {
    #[serde(default)]
    pub remove: Vec<String>,
}

impl Default for NormalizeConfig {
    fn default() -> Self {
        Self {
            remove: DEFAULT_REMOVE_FIELDS
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }
    }
}

pub fn load_config(path: Option<&Path>) -> Result<NormalizeConfig> {
    let Some(path) = path else {
        return Ok(NormalizeConfig::default());
    };
    let raw = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read normalize config {}", path.display()))?;
    let mut config: NormalizeConfig = serde_json::from_str(&raw)
        .with_context(|| format!("invalid normalize config {}", path.display()))?;
    if config.remove.is_empty() {
        config.remove = NormalizeConfig::default().remove;
    }
    Ok(config)
}

pub fn normalize_value(value: &mut Value, config: &NormalizeConfig) {
    for path in &config.remove {
        remove_path(value, path);
    }
    remove_duration_ms(value);
    sort_keys(value);
}

fn remove_duration_ms(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("duration_ms");
            for (_, v) in map.iter_mut() {
                remove_duration_ms(v);
            }
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                remove_duration_ms(item);
            }
        }
        _ => {}
    }
}

fn remove_path(value: &mut Value, path: &str) {
    let segments = parse_path(path);
    if segments.is_empty() {
        return;
    }
    remove_path_segments(value, &segments);
}

fn remove_path_segments(value: &mut Value, segments: &[String]) {
    if segments.is_empty() {
        return;
    }
    if segments.len() == 1 {
        if let Value::Object(map) = value {
            map.remove(&segments[0]);
        }
        return;
    }
    if let Value::Object(map) = value
        && let Some(child) = map.get_mut(&segments[0])
    {
        remove_path_segments(child, &segments[1..]);
    }
}

fn parse_path(path: &str) -> Vec<String> {
    path.split('.')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .map(|segment| segment.to_string())
        .collect()
}

fn sort_keys(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                if let Some(mut val) = map.remove(&key) {
                    sort_keys(&mut val);
                    sorted.insert(key, val);
                }
            }
            *map = sorted;
        }
        Value::Array(items) => {
            for item in items.iter_mut() {
                sort_keys(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_removes_paths_and_duration_ms() {
        let mut value = serde_json::json!({
            "meta": {"trace_id": "x", "timestamp": "y"},
            "envelope": {"trace_id": "z"},
            "duration_ms": 12,
            "nested": {"duration_ms": 5, "ok": true},
        });
        let config = NormalizeConfig::default();
        normalize_value(&mut value, &config);
        assert!(value.get("duration_ms").is_none());
        assert!(value.get("meta").and_then(|v| v.get("trace_id")).is_none());
        assert!(
            value
                .get("envelope")
                .and_then(|v| v.get("trace_id"))
                .is_none()
        );
        assert!(
            value
                .get("nested")
                .and_then(|v| v.get("duration_ms"))
                .is_none()
        );
    }
}
