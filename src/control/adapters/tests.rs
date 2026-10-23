use super::*;
#[test]
fn explicit_profile_path_is_not_reinterpreted_as_a_download_directory() {
    let args = vec![
        "request".into(),
        "--profile".into(),
        "profile with spaces".into(),
    ];
    assert_eq!(
        profile(&args).unwrap(),
        PathBuf::from("profile with spaces")
    );
}
#[test]
fn missing_profile_argument_is_an_input_error() {
    assert_eq!(
        profile(&["--profile".into()]),
        Err(ErrorCode::InvalidRequest)
    );
}
#[test]
fn absent_profile_uses_the_application_profile_convention() {
    assert_eq!(profile(&[]).unwrap(), crate::settings::data_directory());
}
#[test]
fn stdin_eof_and_final_unterminated_document_have_distinct_results() {
    let mut reader = io::Cursor::new(b"".as_slice());
    assert!(read_line(&mut reader).unwrap().is_none());
    let mut reader = io::Cursor::new(b"{}".as_slice());
    assert_eq!(read_line(&mut reader).unwrap().unwrap(), b"{}");
}
#[test]
fn stdin_reader_does_not_consume_the_next_json_rpc_message() {
    let mut reader = io::Cursor::new(b"{}\n[]\n".as_slice());
    assert_eq!(read_line(&mut reader).unwrap().unwrap(), b"{}");
    assert_eq!(read_line(&mut reader).unwrap().unwrap(), b"[]");
    assert!(read_line(&mut reader).unwrap().is_none());
}
#[test]
fn stdin_reader_enforces_its_boundary_for_both_terminated_and_unterminated_input() {
    for suffix in [b"".as_slice(), b"\n".as_slice()] {
        let mut bytes = vec![b'x'; wire::MAX_REQUEST_BYTES + 1];
        bytes.extend(suffix);
        assert_eq!(
            read_line(&mut io::Cursor::new(bytes)),
            Err(ErrorCode::TooLarge)
        );
    }
}
#[test]
fn input_cannot_override_injected_authentication_or_request_identity() {
    for field in ["token", "request_id", "protocol_version", "confirmed"] {
        let mut input = json!({"method":{"name":"discover"}});
        input[field] = json!("secret");
        assert!(serde_json::from_value::<Input>(input).is_err());
    }
}
#[test]
fn generated_tool_schema_lists_each_wire_method_once() {
    let schema = input_schema();
    let choices = schema["properties"]["method"]["oneOf"].as_array().unwrap();
    let names: std::collections::BTreeSet<_> = choices
        .iter()
        .map(|choice| choice["properties"]["name"]["const"].as_str().unwrap())
        .collect();
    assert_eq!(names.len(), 12);
    assert_eq!(names.len(), choices.len());
    assert_eq!(schema["additionalProperties"], false);
}
#[test]
fn tool_annotations_do_not_falsely_claim_mutations_are_readonly_or_idempotent() {
    let tools = tools();
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], false);
    assert_eq!(tools[0]["annotations"]["idempotentHint"], false);
    assert_eq!(tools[0]["annotations"]["destructiveHint"], true);
}
#[test]
fn ping_is_json_rpc_and_preserves_string_request_identity() {
    let response = mcp_local(
        &json!({"jsonrpc":"2.0","id":"string-id","method":"ping"}),
        &mut false,
    )
    .unwrap();
    assert_eq!(response["id"], "string-id");
    assert_eq!(response["result"], json!({}));
}
#[test]
fn mcp_invalid_json_rpc_version_never_initializes_the_session() {
    let mut initialized = false;
    let response = mcp_local(
        &json!({"jsonrpc":"1.0","id":1,"method":"initialize"}),
        &mut initialized,
    )
    .unwrap();
    assert_eq!(response["error"]["code"], -32600);
    assert!(!initialized);
}
#[test]
fn mcp_operational_requests_require_initialization() {
    let mut initialized = false;
    let response = mcp_local(
        &json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}),
        &mut initialized,
    )
    .unwrap();
    assert_eq!(response["error"]["code"], -32000);
}
#[test]
fn unknown_mcp_method_is_a_protocol_error_after_initialization() {
    let response = mcp_local(
        &json!({"jsonrpc":"2.0","id":1,"method":"unknown"}),
        &mut true,
    )
    .unwrap();
    assert_eq!(response["error"]["code"], -32601);
}
#[test]
fn mcp_notifications_never_create_stdout_responses() {
    for method in ["notifications/initialized", "notifications/cancelled"] {
        assert!(mcp_local(&json!({"jsonrpc":"2.0","method":method}), &mut true).is_none());
    }
}
#[test]
fn tools_call_is_delegated_to_the_same_controller_adapter() {
    assert!(
        mcp_local(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call"}),
            &mut true
        )
        .is_none()
    );
}
