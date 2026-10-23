use super::*;
use serde_json::json;

fn valid() -> Value {
    json!({"protocol_version": super::super::PROTOCOL_VERSION, "request_id":"request", "token":"test-token", "instance_id":null, "method":{"name":"discover"}})
}
fn parsed(value: Value) -> Result<Request, ErrorCode> {
    parse_request(&serde_json::to_vec(&value).unwrap())
}

#[test]
fn current_protocol_round_trips_without_losing_request_identity() {
    let request = parsed(valid()).unwrap();
    assert_eq!(request.request_id, "request");
    assert_eq!(request.token, "test-token");
    assert_eq!(serde_json::to_value(request).unwrap(), valid());
}
#[test]
fn zero_protocol_is_incompatible() {
    let mut value = valid();
    value["protocol_version"] = json!(0);
    assert_eq!(parsed(value).err(), Some(ErrorCode::IncompatibleProtocol));
}
#[test]
fn future_protocol_is_incompatible() {
    let mut value = valid();
    value["protocol_version"] = json!(u32::MAX);
    assert_eq!(parsed(value).err(), Some(ErrorCode::IncompatibleProtocol));
}
#[test]
fn empty_request_id_is_invalid() {
    let mut value = valid();
    value["request_id"] = json!("");
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn request_id_limit_is_in_bytes_not_unicode_characters() {
    let mut value = valid();
    value["request_id"] = json!("é".repeat(33));
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn maximum_request_id_is_accepted() {
    let mut value = valid();
    value["request_id"] = json!("x".repeat(64));
    assert!(parsed(value).is_ok());
}
#[test]
fn overlong_request_id_is_rejected() {
    let mut value = valid();
    value["request_id"] = json!("x".repeat(65));
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn maximum_token_is_accepted_by_parser_not_authenticated() {
    let mut value = valid();
    value["token"] = json!("x".repeat(128));
    assert!(parsed(value).is_ok());
}
#[test]
fn overlong_token_is_rejected_before_authentication() {
    let mut value = valid();
    value["token"] = json!("x".repeat(129));
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn missing_token_is_not_an_anonymous_request() {
    let mut value = valid();
    value.as_object_mut().unwrap().remove("token");
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn unknown_envelope_fields_cannot_supply_approval() {
    let mut value = valid();
    value["confirmed"] = json!(true);
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn unknown_method_fields_are_rejected() {
    let mut value = valid();
    value["method"]["approved"] = json!(true);
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn unknown_methods_cannot_invoke_a_shell() {
    let mut value = valid();
    value["method"] = json!({"name":"shell","args":{"command":"anything"}});
    assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn negative_and_fractional_page_values_are_invalid() {
    for offset in [json!(-1), json!(0.5), json!("0"), Value::Null] {
        let mut value = valid();
        value["method"] = json!({"name":"list","args":{"offset":offset,"limit":1}});
        assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
    }
}
#[test]
fn duplicate_authentication_fields_are_invalid() {
    let bytes=br#"{"protocol_version":1,"request_id":"x","token":"first","token":"second","method":{"name":"discover"}}"#;
    assert_eq!(parse_request(bytes).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn trailing_non_whitespace_cannot_smuggle_a_second_request() {
    let mut bytes = serde_json::to_vec(&valid()).unwrap();
    bytes.extend_from_slice(b"{} ");
    assert_eq!(parse_request(&bytes).err(), Some(ErrorCode::InvalidRequest));
}
#[test]
fn exact_payload_size_is_accepted_and_one_more_byte_is_rejected() {
    let mut bytes = serde_json::to_vec(&valid()).unwrap();
    bytes.resize(MAX_REQUEST_BYTES, b' ');
    assert!(parse_request(&bytes).is_ok());
    bytes.push(b' ');
    assert_eq!(parse_request(&bytes).err(), Some(ErrorCode::TooLarge));
}
#[test]
fn mode_and_repair_values_reject_hidden_fields() {
    for command in [
        json!({"kind":"set_mode","value":{"mode":"normal","freeze_after_bytes":1,"approved":true}}),
        json!({"kind":"set_repair_policy","value":{"enabled":true,"max_attempts":1,"approved":true}}),
    ] {
        let mut value = valid();
        value["method"] = json!({"name":"mutate","args":{"torrent_id":"id","command":command,"expected_revision":1,"idempotency_key":"key","deadline_unix_ms":1}});
        assert_eq!(parsed(value).err(), Some(ErrorCode::InvalidRequest));
    }
}
#[test]
fn oversized_response_preserves_correlation_and_drops_its_entire_result() {
    let mut response = Response::success(json!({"secret":"marker".repeat(MAX_RESPONSE_BYTES)}));
    response.request_id = "request".into();
    response.instance_id = "instance".into();
    let bytes = encode_response(&response);
    assert_eq!(bytes.last(), Some(&b'\n'));
    assert_eq!(bytes.iter().filter(|b| **b == b'\n').count(), 1);
    let decoded: Response = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(decoded.error, Some(ErrorCode::TooLarge));
    assert!(decoded.result.is_none());
    assert_eq!(decoded.request_id, "request");
    assert_eq!(decoded.instance_id, "instance");
}
#[test]
fn every_error_has_a_stable_json_value_and_cli_category() {
    use ErrorCode::*;
    for (code, expected_exit) in [
        (InvalidRequest, 2),
        (IncompatibleProtocol, 1),
        (Unauthorized, 4),
        (PermissionDenied, 4),
        (Disabled, 3),
        (NotRunning, 3),
        (Busy, 1),
        (InvalidPage, 2),
        (UnknownTorrent, 1),
        (Conflict, 5),
        (Unsupported, 1),
        (PersistenceUnavailable, 1),
        (EngineRejected, 1),
        (UnknownOperation, 1),
        (IdempotencyConflict, 5),
        (DeadlineExceeded, 1),
        (ApprovalRequired, 1),
        (ReconciliationRequired, 1),
        (PathDenied, 1),
        (TooLarge, 2),
    ] {
        let response: Response =
            serde_json::from_slice(&encode_response(&Response::error(code))).unwrap();
        assert_eq!(response.error, Some(code));
        assert!(response.result.is_none());
        assert_eq!(super::super::adapters::exit_code(Some(code)), expected_exit);
    }
}

proptest::proptest! {
    #[test]
    fn arbitrary_correlated_success_values_round_trip(text in ".{0,1024}") {
        let response=Response::success(json!({"data":text}));let bytes=encode_response(&response);
        let decoded:Response=serde_json::from_slice(&bytes).unwrap();
        proptest::prop_assert!(decoded.error.is_none());proptest::prop_assert_eq!(decoded.result,response.result);
    }
}
