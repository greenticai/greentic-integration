use greentic_interfaces_guest::component::node::{InvokeResult, NodeError};
use greentic_interfaces_guest::component_entrypoint;
use serde_json::Value;

#[cfg(target_arch = "wasm32")]
#[used]
#[unsafe(link_section = ".greentic.wasi")]
static WASI_TARGET_MARKER: [u8; 13] = *b"wasm32-wasip2";

component_entrypoint!({
    manifest: crate::describe_payload,
    invoke: crate::handle_message,
});

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
