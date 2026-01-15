#![allow(dead_code)]

use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use greentic_interfaces_guest::state_store::read;
use serde_json::{Value, json};

fn manifest() -> String {
    json!({
        "id": "conformance.state_nocap",
        "version": "0.1.0",
        "world": "greentic:component/component@0.5.0",
        "supports": ["job"],
        "profiles": {"default": "default", "supported": ["default"]},
        "capabilities": {"wasi": {}, "host": {}},
        "operations": [
            {"name": "touch", "input_schema": {}, "output_schema": {}}
        ]
    })
    .to_string()
}

fn invoke(op: String, input: String) -> InvokeResult {
    if op.as_str() != "touch" {
        return InvokeResult::Err(NodeError {
            code: "unsupported_operation".to_string(),
            message: format!("unsupported op: {op}"),
            retryable: false,
            backoff_ms: None,
            details: None,
        });
    }
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("conformance-key")
        .to_string();
    match read(&key, None) {
        Ok(_) => InvokeResult::Ok(
            json!({"marker": "state.nocap", "status": "unexpected_access"}).to_string(),
        ),
        Err(err) => InvokeResult::Ok(
            json!({"marker": "state.nocap", "status": "error", "error": {"code": err.code, "message": err.message}}).to_string(),
        ),
    }
}

component_entrypoint!({ manifest: manifest, invoke: invoke });
