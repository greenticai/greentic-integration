use std::fs;
use std::path::PathBuf;

use serde_yaml_bw as serde_yaml;
use serde_yaml_bw::Value;

fn load_flow(relative_path: &str) -> Value {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let flow_path = manifest_dir.join("..").join("..").join(relative_path);
    let data = fs::read_to_string(&flow_path)
        .unwrap_or_else(|_| panic!("Failed to read flow file at {flow_path:?}"));
    serde_yaml::from_str(&data).expect("flow YAML should deserialize")
}

#[test]
fn echo_pack_flow_invokes_component_operation() {
    let flow = load_flow("crates/test-packs/echo-pack/flows/main.ygtc");

    assert_eq!(flow["type"], "messaging");
    assert_eq!(flow["id"], "echo");

    let nodes = flow["nodes"].as_mapping().expect("nodes mapping");
    let echo = nodes
        .get(Value::from("echo"))
        .and_then(Value::as_mapping)
        .expect("echo node present");
    assert_eq!(echo.get(Value::from("routing")), Some(&Value::from("out")));

    let op = echo
        .get(Value::from("ai.greentic.component-echo"))
        .and_then(Value::as_mapping)
        .expect("component operator present");
    assert_eq!(
        op.get(Value::from("operation")),
        Some(&Value::from("messaging.send"))
    );
    assert_eq!(
        op.get(Value::from("channel")),
        Some(&Value::from("webchat"))
    );
    assert_eq!(op.get(Value::from("text")), Some(&Value::from("Echo test")));
}
