use component_echo::{describe_payload, handle_message};
use greentic_interfaces_guest::component::node::InvokeResult;
use serde_json::Value;

const SAMPLE_MESSAGE: &str = include_str!("../../../../fixtures/inputs/channel_message.json");

#[test]
fn describe_payload_listed_messaging_operations() {
    let payload = describe_payload();
    let json: Value = serde_json::from_str(&payload).expect("manifest should parse as JSON");
    let operations = json["operations"]
        .as_array()
        .expect("operations should be an array");
    assert!(operations.iter().any(|op| op["name"] == "messaging.send"));
    assert!(
        operations
            .iter()
            .any(|op| op["name"] == "messaging.ingress")
    );
}

fn expect_ok_response(op: &str) {
    let response = handle_message(op.to_string(), SAMPLE_MESSAGE.to_string());
    match response {
        InvokeResult::Ok(body) => assert_eq!(body, SAMPLE_MESSAGE),
        other => panic!("expected Ok, got {:?}", other),
    }
}

#[test]
fn messaging_send_invocation_is_idempotent() {
    expect_ok_response("messaging.send");
}

#[test]
fn messaging_ingress_invocation_is_idempotent() {
    expect_ok_response("messaging.ingress");
}

#[test]
fn unknown_operation_returns_error() {
    let response = handle_message("broken".into(), SAMPLE_MESSAGE.to_string());
    match response {
        InvokeResult::Err(err) => assert_eq!(err.code, "unsupported_operation"),
        other => panic!("expected Err, got {:?}", other),
    }
}
