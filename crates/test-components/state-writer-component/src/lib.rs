#![allow(dead_code)]

use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use greentic_interfaces_guest::state_store::{HostError, OpAck, TenantCtx, delete, write};
use serde_json::{Value, json};

fn manifest() -> String {
    json!({
        "id": "conformance.state_writer",
        "version": "0.1.0",
        "world": "greentic:component/component@0.5.0",
        "supports": ["job"],
        "profiles": {"default": "default", "supported": ["default"]},
        "capabilities": {
            "wasi": {},
            "host": {"state": {"write": true}}
        },
        "operations": [
            {"name": "write", "input_schema": {}, "output_schema": {}},
            {"name": "delete", "input_schema": {}, "output_schema": {}}
        ]
    })
    .to_string()
}

fn invoke(op: String, input: String) -> InvokeResult {
    let payload: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    match op.as_str() {
        "write" => handle_write(&payload),
        "delete" => handle_delete(&payload),
        _ => InvokeResult::Err(NodeError {
            code: "unsupported_operation".to_string(),
            message: format!("unsupported op: {op}"),
            retryable: false,
            backoff_ms: None,
            details: None,
        }),
    }
}

fn handle_write(payload: &Value) -> InvokeResult {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("conformance-key")
        .to_string();
    let skip_write = payload
        .get("skip_write")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if skip_write {
        return InvokeResult::Ok(
            json!({"marker": "state.write", "status": "skipped", "key": key}).to_string(),
        );
    }
    let value = payload.get("value").cloned().unwrap_or(Value::Null);
    let bytes = serde_json::to_vec(&value).unwrap_or_else(|_| value.to_string().into_bytes());
    let ctx = tenant_ctx(payload);
    match write(&key, &bytes, ctx.as_ref()) {
        Ok(OpAck::Ok) => InvokeResult::Ok(
            json!({"marker": "state.write", "status": "wrote", "key": key}).to_string(),
        ),
        Err(err) => InvokeResult::Err(node_error("state_write", err)),
    }
}

fn handle_delete(payload: &Value) -> InvokeResult {
    let key = payload
        .get("key")
        .and_then(Value::as_str)
        .unwrap_or("conformance-key")
        .to_string();
    let should_delete = payload
        .get("delete")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !should_delete {
        return InvokeResult::Ok(
            json!({"marker": "state.delete", "status": "skipped", "key": key}).to_string(),
        );
    }
    let ctx = tenant_ctx(payload);
    match delete(&key, ctx.as_ref()) {
        Ok(OpAck::Ok) => InvokeResult::Ok(
            json!({"marker": "state.delete", "status": "deleted", "key": key}).to_string(),
        ),
        Err(err) => InvokeResult::Err(node_error("state_delete", err)),
    }
}

fn tenant_ctx(payload: &Value) -> Option<TenantCtx> {
    let tenant = payload.get("tenant")?.as_str()?;
    Some(TenantCtx {
        env: "test".to_string(),
        tenant: tenant.to_string(),
        tenant_id: tenant.to_string(),
        i18n_id: None,
        team: None,
        team_id: None,
        user: None,
        user_id: None,
        trace_id: None,
        correlation_id: None,
        attributes: Vec::new(),
        session_id: None,
        flow_id: None,
        node_id: None,
        provider_id: None,
        deadline_ms: None,
        attempt: 1,
        idempotency_key: None,
        impersonation: None,
    })
}

fn node_error(code: &str, err: HostError) -> NodeError {
    NodeError {
        code: code.to_string(),
        message: format!("{}: {}", err.code, err.message),
        retryable: false,
        backoff_ms: None,
        details: None,
    }
}

component_entrypoint!({ manifest: manifest, invoke: invoke });
