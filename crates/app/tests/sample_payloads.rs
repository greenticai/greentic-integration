use greentic_integration::fixtures::Fixture;

#[test]
fn build_status_event_payload_has_expected_fields() {
    let payload = Fixture::load_json("inputs/build_status_event.json").expect("fixture");
    assert_eq!(payload["topic"], "greentic.repo.build.status");
    assert_eq!(payload["type"], "com.greentic.repo.build.status.v1");
    assert_eq!(payload["subject"], "repo:my-service");
    assert!(payload["tenant"].is_object());
    assert!(payload["payload"].is_object());
    assert!(payload["metadata"].is_object());
}

#[test]
fn channel_message_payload_has_expected_fields() {
    let payload = Fixture::load_json("inputs/channel_message.json").expect("fixture");
    assert_eq!(payload["channel"], "webchat");
    assert_eq!(payload["session_id"], "sess-789");
    assert_eq!(
        payload["text"],
        "Build succeeded for my-service @1a2b3c (status: success)"
    );
    assert!(payload["tenant"].is_object());
    assert!(payload["attachments"].is_array());
    assert!(payload["metadata"].is_object());
    assert!(payload["from"].is_object());
    assert_eq!(payload["from"]["id"], "user-1");
    assert_eq!(payload["from"]["kind"], "user");
    assert!(payload["to"].is_array());
    assert!(payload["to"].as_array().unwrap().is_empty());
}

#[test]
fn rebuild_request_event_payload_has_expected_fields() {
    let payload = Fixture::load_json("inputs/rebuild_request_event.json").expect("fixture");
    assert_eq!(payload["topic"], "greentic.repo.build.request");
    assert_eq!(payload["type"], "com.greentic.repo.build.request.v1");
    assert_eq!(payload["subject"], "repo:my-service");
    assert!(payload["tenant"].is_object());
    assert!(payload["payload"].is_object());
    assert!(payload["metadata"].is_object());
}
