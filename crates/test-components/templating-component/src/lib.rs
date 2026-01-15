use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use serde_json::{Value, json};

#[allow(dead_code)]
fn manifest() -> String {
    json!({
        "id": "conformance.templating",
        "version": "0.1.0",
        "world": "greentic:component/component@0.5.0",
        "supports": ["job"],
        "profiles": {"default": "default", "supported": ["default"]},
        "capabilities": {"wasi": {}, "host": {}},
        "operations": [
            {"name": "start", "input_schema": {}, "output_schema": {}},
            {"name": "process", "input_schema": {}, "output_schema": {}}
        ]
    })
    .to_string()
}

#[allow(dead_code)]
fn invoke(op: String, input: String) -> InvokeResult {
    match op.as_str() {
        "start" => InvokeResult::Ok(
            json!({
                "user": {"id": 1, "name": "Ada"},
                "status": "ready"
            })
            .to_string(),
        ),
        "process" => {
            let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
            let user_id = payload.get("user_id").cloned().unwrap_or(Value::Null);
            let user_id_type = if user_id.is_number() {
                "number"
            } else if user_id.is_string() {
                "string"
            } else {
                "other"
            };
            let name = payload.get("name").cloned().unwrap_or(Value::Null);
            let status = payload.get("status").cloned().unwrap_or(Value::Null);
            let message = payload.get("message").cloned().unwrap_or(Value::Null);
            InvokeResult::Ok(
                json!({
                    "marker": "templating.process",
                    "user_id": user_id,
                    "user_id_type": user_id_type,
                    "name": name,
                    "status": status,
                    "message": message
                })
                .to_string(),
            )
        }
        _ => InvokeResult::Err(NodeError {
            code: "unsupported_operation".to_string(),
            message: format!("unsupported op: {op}"),
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

component_entrypoint!({ manifest: manifest, invoke: invoke });
