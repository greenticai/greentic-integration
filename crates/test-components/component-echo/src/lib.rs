use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use serde_json::Value;

#[cfg(target_arch = "wasm32")]
use serde::Serialize;
#[cfg(target_arch = "wasm32")]
use std::collections::BTreeMap;

#[cfg(target_arch = "wasm32")]
mod descriptor_bindings {
    wit_bindgen::generate!({
        path: "wit-v060",
        world: "descriptor-world",
        pub_export_macro: true,
    });
}

#[cfg(target_arch = "wasm32")]
#[used]
#[unsafe(link_section = ".greentic.wasi")]
static WASI_TARGET_MARKER: [u8; 13] = *b"wasm32-wasip2";

component_entrypoint!({
    manifest: crate::describe_payload,
    invoke: crate::handle_message,
});

#[cfg(target_arch = "wasm32")]
struct ComponentDescriptorExport;

#[cfg(target_arch = "wasm32")]
impl descriptor_bindings::exports::greentic::component::component_descriptor::Guest
    for ComponentDescriptorExport
{
    fn get_component_info() -> Vec<u8> {
        serde_cbor::to_vec(&component_info()).expect("component info should encode")
    }

    fn describe() -> Vec<u8> {
        serde_cbor::to_vec(&component_describe()).expect("component describe should encode")
    }
}

#[cfg(target_arch = "wasm32")]
impl descriptor_bindings::exports::greentic::component::component_qa::Guest
    for ComponentDescriptorExport
{
    fn qa_spec(
        mode: descriptor_bindings::exports::greentic::component::component_qa::QaMode,
    ) -> Vec<u8> {
        let mode = match mode {
            descriptor_bindings::exports::greentic::component::component_qa::QaMode::Default => {
                "default"
            }
            descriptor_bindings::exports::greentic::component::component_qa::QaMode::Setup => {
                "setup"
            }
            descriptor_bindings::exports::greentic::component::component_qa::QaMode::Update => {
                "update"
            }
            descriptor_bindings::exports::greentic::component::component_qa::QaMode::Remove => {
                "remove"
            }
        };
        serde_json::to_vec(&serde_json::json!({
            "mode": mode,
            "title": "component-echo",
            "description": "No setup required.",
            "questions": [],
        }))
        .expect("qa spec should encode")
    }

    fn apply_answers(
        _mode: descriptor_bindings::exports::greentic::component::component_qa::QaMode,
        current_config: Vec<u8>,
        answers: Vec<u8>,
    ) -> Vec<u8> {
        if answers.is_empty() {
            current_config
        } else {
            answers
        }
    }
}

#[cfg(target_arch = "wasm32")]
descriptor_bindings::export!(ComponentDescriptorExport with_types_in descriptor_bindings);

pub fn describe_payload() -> String {
    include_str!("../component.manifest.json").to_string()
}

pub fn handle_message(operation: String, input: String) -> InvokeResult {
    match operation.as_str() {
        "messaging.send" | "messaging.ingress" | "ai.greentic.component-echo" => {
            echo_message(input)
        }
        other => unsupported_operation(other),
    }
}

fn echo_message(input: String) -> InvokeResult {
    if serde_json::from_str::<Value>(&input).is_err() {
        return InvokeResult::Err(NodeError {
            code: "invalid_payload".to_string(),
            message: "message payload must be valid JSON".to_string(),
            retryable: false,
            backoff_ms: None,
            details: None,
        });
    }
    InvokeResult::Ok(input)
}

fn unsupported_operation(op: &str) -> InvokeResult {
    InvokeResult::Err(NodeError {
        code: "unsupported_operation".to_string(),
        message: format!("unsupported op: {op}"),
        retryable: false,
        backoff_ms: None,
        details: None,
    })
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct ComponentInfoV060 {
    id: String,
    version: String,
    role: String,
    display_name: Option<I18nTextV060>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct ComponentDescribeV060 {
    info: ComponentInfoV060,
    provided_capabilities: Vec<String>,
    required_capabilities: Vec<String>,
    metadata: BTreeMap<String, serde_cbor::Value>,
    operations: Vec<ComponentOperationV060>,
    config_schema: SchemaIrV060,
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct ComponentOperationV060 {
    id: String,
    display_name: Option<I18nTextV060>,
    input: ComponentRunIoV060,
    output: ComponentRunIoV060,
    defaults: BTreeMap<String, serde_cbor::Value>,
    redactions: Vec<RedactionRuleV060>,
    constraints: BTreeMap<String, serde_cbor::Value>,
    schema_hash: String,
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct ComponentRunIoV060 {
    schema: SchemaIrV060,
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct I18nTextV060 {
    key: String,
    default: Option<String>,
}

#[cfg(target_arch = "wasm32")]
#[derive(Serialize)]
struct RedactionRuleV060 {
    json_pointer: String,
    kind: RedactionKindV060,
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RedactionKindV060 {
    Secret,
    Mask,
    Drop,
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[derive(Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum SchemaIrV060 {
    Object {
        #[serde(default)]
        properties: BTreeMap<String, SchemaIrV060>,
        #[serde(default)]
        required: Vec<String>,
        #[serde(default)]
        additional: AdditionalPropertiesV060,
    },
    Array {
        items: Box<SchemaIrV060>,
        #[serde(default)]
        min_items: Option<u64>,
        #[serde(default)]
        max_items: Option<u64>,
    },
    String {
        #[serde(default)]
        min_len: Option<u64>,
        #[serde(default)]
        max_len: Option<u64>,
        #[serde(default)]
        regex: Option<String>,
        #[serde(default)]
        format: Option<String>,
    },
    Bool,
}

#[cfg(target_arch = "wasm32")]
#[allow(dead_code)]
#[derive(Default, Serialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "schema")]
enum AdditionalPropertiesV060 {
    #[default]
    Allow,
    Forbid,
    Schema(Box<SchemaIrV060>),
}

#[cfg(target_arch = "wasm32")]
fn component_info() -> ComponentInfoV060 {
    ComponentInfoV060 {
        id: "ai.greentic.component-echo".to_string(),
        version: "0.1.0".to_string(),
        role: "tool".to_string(),
        display_name: Some(I18nTextV060 {
            key: "component-echo.display-name".to_string(),
            default: Some("component-echo".to_string()),
        }),
    }
}

#[cfg(target_arch = "wasm32")]
fn component_describe() -> ComponentDescribeV060 {
    ComponentDescribeV060 {
        info: component_info(),
        provided_capabilities: vec!["messaging".to_string()],
        required_capabilities: vec![],
        metadata: BTreeMap::new(),
        operations: vec![
            component_operation("messaging.send"),
            component_operation("messaging.ingress"),
        ],
        config_schema: empty_object_schema(),
    }
}

#[cfg(target_arch = "wasm32")]
fn component_operation(id: &str) -> ComponentOperationV060 {
    let input = ComponentRunIoV060 {
        schema: message_schema(),
    };
    let output = ComponentRunIoV060 {
        schema: message_schema(),
    };

    ComponentOperationV060 {
        id: id.to_string(),
        display_name: None,
        input,
        output,
        defaults: BTreeMap::new(),
        redactions: vec![],
        constraints: BTreeMap::new(),
        schema_hash: "0fcb7e91126b505027225d38958ede9d89ae4a479a95c47e3b197f213d5a1f28".to_string(),
    }
}

#[cfg(target_arch = "wasm32")]
fn empty_object_schema() -> SchemaIrV060 {
    SchemaIrV060::Object {
        properties: BTreeMap::new(),
        required: vec![],
        additional: AdditionalPropertiesV060::Forbid,
    }
}

#[cfg(target_arch = "wasm32")]
fn message_schema() -> SchemaIrV060 {
    let mut properties = BTreeMap::new();
    properties.insert(
        "attachments".to_string(),
        SchemaIrV060::Array {
            items: Box::new(SchemaIrV060::Object {
                properties: BTreeMap::new(),
                required: vec![],
                additional: AdditionalPropertiesV060::Forbid,
            }),
            min_items: None,
            max_items: None,
        },
    );
    properties.insert(
        "channel".to_string(),
        SchemaIrV060::String {
            min_len: None,
            max_len: None,
            regex: None,
            format: None,
        },
    );
    properties.insert(
        "id".to_string(),
        SchemaIrV060::String {
            min_len: None,
            max_len: None,
            regex: None,
            format: None,
        },
    );
    properties.insert(
        "metadata".to_string(),
        SchemaIrV060::Object {
            properties: BTreeMap::new(),
            required: vec![],
            additional: AdditionalPropertiesV060::Forbid,
        },
    );
    properties.insert(
        "session_id".to_string(),
        SchemaIrV060::String {
            min_len: None,
            max_len: None,
            regex: None,
            format: None,
        },
    );
    properties.insert(
        "tenant".to_string(),
        SchemaIrV060::Object {
            properties: BTreeMap::new(),
            required: vec![],
            additional: AdditionalPropertiesV060::Forbid,
        },
    );
    properties.insert(
        "text".to_string(),
        SchemaIrV060::String {
            min_len: None,
            max_len: None,
            regex: None,
            format: None,
        },
    );
    SchemaIrV060::Object {
        properties,
        required: vec![],
        additional: AdditionalPropertiesV060::Allow,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_MESSAGE: &str = include_str!("../../../../fixtures/inputs/channel_message.json");

    #[test]
    fn describe_payload_reports_messaging_support() {
        let payload = describe_payload();
        let json: serde_json::Value =
            serde_json::from_str(&payload).expect("manifest should parse");
        let supports = json["supports"]
            .as_array()
            .expect("supports should be an array");
        assert!(supports.iter().any(|value| value == "messaging"));
    }

    #[test]
    fn component_describe_reports_echo_component() {
        let encoded = {
            #[cfg(target_arch = "wasm32")]
            {
                serde_cbor::to_vec(&component_describe()).expect("descriptor encodes")
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                serde_cbor::to_vec(&serde_json::json!({
                    "info": { "id": "ai.greentic.component-echo" }
                }))
                .expect("descriptor encodes")
            }
        };
        let decoded: serde_json::Value =
            serde_cbor::from_slice(&encoded).expect("descriptor decodes");
        assert_eq!(decoded["info"]["id"], "ai.greentic.component-echo");
    }

    #[test]
    fn messaging_send_returns_input_unchanged() {
        let response = handle_message("messaging.send".into(), SAMPLE_MESSAGE.into());
        match response {
            InvokeResult::Ok(body) => assert_eq!(body, SAMPLE_MESSAGE),
            other => panic!("expected Ok response, got {:?}", other),
        }
    }

    #[test]
    fn messaging_ingress_returns_input_unchanged() {
        let response = handle_message("messaging.ingress".into(), SAMPLE_MESSAGE.into());
        match response {
            InvokeResult::Ok(body) => assert_eq!(body, SAMPLE_MESSAGE),
            other => panic!("expected Ok response, got {:?}", other),
        }
    }

    #[test]
    fn unsupported_operation_is_rejected() {
        let response = handle_message("undefined".into(), SAMPLE_MESSAGE.into());
        match response {
            InvokeResult::Err(err) => assert_eq!(err.code, "unsupported_operation"),
            other => panic!("expected Err response, got {:?}", other),
        }
    }

    #[test]
    fn component_echo_operation_reads_message() {
        let response = handle_message("ai.greentic.component-echo".into(), SAMPLE_MESSAGE.into());
        match response {
            InvokeResult::Ok(body) => assert_eq!(body, SAMPLE_MESSAGE),
            other => panic!("expected Ok response, got {:?}", other),
        }
    }
}
