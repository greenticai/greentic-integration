use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use greentic_types::cbor::canonical;
use greentic_types::i18n_text::I18nText;
use greentic_types::schemas::common::schema_ir::{AdditionalProperties, SchemaIr};
use greentic_types::schemas::component::v0_6_0::{
    ComponentDescribe, ComponentInfo, ComponentOperation, ComponentRunInput, ComponentRunOutput,
    schema_hash,
};
use serde_json::Value;

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let manifest_path = PathBuf::from(
        args.next()
            .context("usage: component_describe_cache <manifest.json> <output.describe.cbor>")?,
    );
    let output_path = PathBuf::from(
        args.next()
            .context("usage: component_describe_cache <manifest.json> <output.describe.cbor>")?,
    );
    if args.next().is_some() {
        bail!("usage: component_describe_cache <manifest.json> <output.describe.cbor>");
    }

    let manifest_text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {}", manifest_path.display()))?;
    let manifest: Value = serde_json::from_str(&manifest_text)
        .with_context(|| format!("parse {}", manifest_path.display()))?;

    let config_schema = schema_ir(
        manifest
            .get("config_schema")
            .context("manifest missing config_schema")?,
    )?;

    let info = ComponentInfo {
        id: manifest
            .get("id")
            .and_then(Value::as_str)
            .or_else(|| manifest.get("name").and_then(Value::as_str))
            .context("manifest missing id/name")?
            .to_string(),
        version: manifest
            .get("version")
            .and_then(Value::as_str)
            .context("manifest missing version")?
            .to_string(),
        role: "tool".to_string(),
        display_name: manifest
            .get("name")
            .and_then(Value::as_str)
            .map(|name| I18nText::new("component.display_name", Some(name.to_string()))),
    };

    let mut operations = Vec::new();
    for op in manifest
        .get("operations")
        .and_then(Value::as_array)
        .context("manifest missing operations")?
    {
        let id = op
            .get("name")
            .and_then(Value::as_str)
            .context("operation missing name")?
            .to_string();
        let input_schema = schema_ir(
            op.get("input_schema")
                .context("operation missing input_schema")?,
        )?;
        let output_schema = schema_ir(
            op.get("output_schema")
                .context("operation missing output_schema")?,
        )?;
        let schema_hash = schema_hash(&input_schema, &output_schema, &config_schema)
            .context("compute schema hash")?;

        operations.push(ComponentOperation {
            id,
            display_name: None,
            input: ComponentRunInput {
                schema: input_schema,
            },
            output: ComponentRunOutput {
                schema: output_schema,
            },
            defaults: BTreeMap::new(),
            redactions: Vec::new(),
            constraints: BTreeMap::new(),
            schema_hash,
        });
    }

    let describe = ComponentDescribe {
        info,
        provided_capabilities: Vec::new(),
        required_capabilities: Vec::new(),
        metadata: BTreeMap::new(),
        operations,
        config_schema,
    };

    let bytes = canonical::to_canonical_cbor_allow_floats(&describe).context("encode describe")?;
    fs::write(&output_path, bytes).with_context(|| format!("write {}", output_path.display()))?;
    Ok(())
}

fn schema_ir(value: &Value) -> Result<SchemaIr> {
    let obj = value.as_object().context("schema must be a JSON object")?;

    if let Some(variants) = obj.get("oneOf") {
        let variants = variants
            .as_array()
            .context("oneOf must be an array")?
            .iter()
            .map(schema_ir)
            .collect::<Result<Vec<_>>>()?;
        return Ok(SchemaIr::OneOf { variants });
    }

    if let Some(values) = obj.get("enum") {
        let values = values
            .as_array()
            .context("enum must be an array")?
            .iter()
            .map(json_to_cbor_value)
            .collect::<Result<Vec<_>>>()?;
        return Ok(SchemaIr::Enum { values });
    }

    let schema_type = obj
        .get("type")
        .and_then(Value::as_str)
        .or_else(|| {
            if obj.contains_key("properties")
                || obj.contains_key("required")
                || obj.contains_key("additionalProperties")
            {
                Some("object")
            } else if obj.contains_key("items") {
                Some("array")
            } else {
                None
            }
        })
        .unwrap_or("object");

    match schema_type {
        "object" => {
            let mut properties = BTreeMap::new();
            if let Some(props) = obj.get("properties") {
                let props = props.as_object().context("properties must be an object")?;
                for (name, child) in props {
                    properties.insert(name.clone(), schema_ir(child)?);
                }
            }
            let required = obj
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .map(|item| {
                            item.as_str()
                                .map(|s| s.to_string())
                                .context("required entries must be strings")
                        })
                        .collect::<Result<Vec<_>>>()
                })
                .transpose()?
                .unwrap_or_default();
            let additional = match obj.get("additionalProperties") {
                Some(Value::Bool(false)) => AdditionalProperties::Forbid,
                Some(Value::Object(schema)) => AdditionalProperties::Schema(Box::new(schema_ir(
                    &Value::Object(schema.clone()),
                )?)),
                _ if properties.is_empty() && required.is_empty() => AdditionalProperties::Forbid,
                _ => AdditionalProperties::Allow,
            };
            Ok(SchemaIr::Object {
                properties,
                required,
                additional,
            })
        }
        "array" => {
            let items = Box::new(match obj.get("items") {
                Some(items) => schema_ir(items)?,
                None => SchemaIr::Null,
            });
            let min_items = obj.get("minItems").and_then(Value::as_u64);
            let max_items = obj.get("maxItems").and_then(Value::as_u64);
            Ok(SchemaIr::Array {
                items,
                min_items,
                max_items,
            })
        }
        "string" => Ok(SchemaIr::String {
            min_len: obj.get("minLength").and_then(Value::as_u64),
            max_len: obj.get("maxLength").and_then(Value::as_u64),
            regex: obj
                .get("pattern")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            format: obj
                .get("format")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
        }),
        "integer" => Ok(SchemaIr::Int {
            min: obj.get("minimum").and_then(Value::as_i64),
            max: obj.get("maximum").and_then(Value::as_i64),
        }),
        "number" => Ok(SchemaIr::Float {
            min: obj.get("minimum").and_then(Value::as_f64),
            max: obj.get("maximum").and_then(Value::as_f64),
        }),
        "boolean" => Ok(SchemaIr::Bool),
        "null" => Ok(SchemaIr::Null),
        "string[]" => Ok(SchemaIr::Bytes),
        other => bail!("unsupported schema type `{other}`"),
    }
}

fn json_to_cbor_value(value: &Value) -> Result<ciborium::value::Value> {
    Ok(match value {
        Value::Null => ciborium::value::Value::Null,
        Value::Bool(v) => ciborium::value::Value::Bool(*v),
        Value::Number(v) => {
            if let Some(n) = v.as_i64() {
                ciborium::value::Value::Integer(n.into())
            } else if let Some(n) = v.as_u64() {
                ciborium::value::Value::Integer(n.into())
            } else if let Some(n) = v.as_f64() {
                ciborium::value::Value::Float(n)
            } else {
                bail!("unsupported JSON number")
            }
        }
        Value::String(v) => ciborium::value::Value::Text(v.clone()),
        Value::Array(items) => ciborium::value::Value::Array(
            items
                .iter()
                .map(json_to_cbor_value)
                .collect::<Result<Vec<_>>>()?,
        ),
        Value::Object(map) => ciborium::value::Value::Map(
            map.iter()
                .map(|(k, v)| {
                    Ok((
                        ciborium::value::Value::Text(k.clone()),
                        json_to_cbor_value(v)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?
                .into_iter()
                .collect(),
        ),
    })
}
