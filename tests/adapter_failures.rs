//! Executable-level negative contracts. Every process has an isolated profile.
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};
const CLI: &str = env!("CARGO_BIN_EXE_peerlume-control");
const MCP: &str = env!("CARGO_BIN_EXE_peerlume-mcp");
const SECRET: &str = "TEST_SECRET_TOKEN_UNIQUE";

fn run(binary: &str, args: &[&str], input: &[u8], token: Option<&str>, profile: bool) -> Output {
    let root = tempfile::tempdir().unwrap();
    let mut command = Command::new(binary);
    command
        .args(args)
        .current_dir(root.path())
        .env_remove("PEERLUME_CONTROL_TOKEN");
    if profile {
        command.arg("--profile").arg(root.path());
    }
    if let Some(token) = token {
        command.env("PEERLUME_CONTROL_TOKEN", token);
    }
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut writer = child.stdin.take().unwrap();
    if let Err(error) = writer.write_all(input) {
        assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
    }
    drop(writer);
    let output = child.wait_with_output().unwrap();
    assert!(!root.path().join("state.sqlite3").exists());
    assert!(!root.path().join("control.sqlite3").exists());
    output
}
fn json_response(output: &Output) -> Value {
    assert!(
        output.stderr.is_empty(),
        "stderr must not contain credentials or presentation errors"
    );
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
    result
}
fn rpc(messages: &[Value], token: Option<&str>) -> (Output, Vec<Value>) {
    let input = messages
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    let output = run(MCP, &[], input.as_bytes(), token, true);
    let values = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (output, values)
}
fn initialize() -> Value {
    json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}})
}

#[test]
fn cli_help_does_not_require_a_token_or_running_application() {
    let output = run(CLI, &["--help"], b"", None, true);
    assert!(output.status.success());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("peerlume-control")
    );
}
#[test]
fn cli_version_never_echoes_inherited_credentials() {
    let output = run(CLI, &["--version"], b"", Some(SECRET), true);
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("protocol 1"));
    assert!(!text.contains(SECRET));
}
#[test]
fn cli_schema_is_machine_readable_without_authentication() {
    let output = run(CLI, &["--schema"], b"", None, true);
    assert!(output.status.success());
    let schema = json_response(&output);
    assert_eq!(schema["type"], "object");
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["method"]["oneOf"]
            .as_array()
            .unwrap()
            .len(),
        12
    );
}
#[test]
fn cli_missing_credentials_has_a_distinct_exit_code() {
    let output = run(CLI, &["discover"], b"", None, true);
    assert_eq!(output.status.code(), Some(4));
    assert_eq!(json_response(&output)["error"], "unauthorized");
}
#[test]
fn cli_missing_profile_value_is_an_input_error() {
    let output = run(CLI, &["--profile"], b"", Some(SECRET), false);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_response(&output)["error"], "invalid_request");
}
#[test]
fn cli_unknown_subcommand_does_not_launch_an_application() {
    let output = run(CLI, &["unknown-subcommand"], b"", Some(SECRET), true);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_response(&output)["error"], "invalid_request");
}
#[test]
fn cli_empty_request_stdin_is_not_a_discovery_request() {
    let output = run(CLI, &["request"], b"", Some(SECRET), true);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_response(&output)["error"], "invalid_request");
}
#[test]
fn cli_malformed_request_never_echoes_raw_input() {
    let output = run(
        CLI,
        &["request"],
        b"invalid SECRET request",
        Some(SECRET),
        true,
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_response(&output)["error"], "invalid_request");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET request"));
}
#[test]
fn cli_token_cannot_be_supplied_in_request_json() {
    let input = json!({"method":{"name":"discover"},"token":SECRET}).to_string();
    let output = run(CLI, &["request"], input.as_bytes(), Some(SECRET), true);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(json_response(&output)["error"], "invalid_request");
}
#[test]
fn cli_oversized_stdin_returns_bounded_json_error() {
    let output = run(CLI, &["request"], &vec![b'x'; 65537], Some(SECRET), true);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.len() < 1024);
    assert_eq!(json_response(&output)["error"], "too_large");
}
#[test]
fn cli_valid_unterminated_request_preserves_not_running_contract() {
    let output = run(
        CLI,
        &["request"],
        br#"{"method":{"name":"discover"}}"#,
        Some(SECRET),
        true,
    );
    assert_eq!(output.status.code(), Some(3));
    assert_eq!(json_response(&output)["error"], "not_running");
}
#[test]
fn mcp_clean_eof_exits_without_any_protocol_output() {
    let output = run(MCP, &[], b"", None, true);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}
#[test]
fn mcp_missing_profile_value_exits_as_invalid_input() {
    let output = run(MCP, &["--profile"], b"", None, false);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
#[test]
fn mcp_malformed_json_is_a_parse_error_and_not_a_process_failure() {
    let output = run(MCP, &[], b"not JSON SECRET\n", Some(SECRET), true);
    assert!(output.status.success());
    let reply = json_response(&output);
    assert_eq!(reply["error"]["code"], -32700);
    assert!(reply["id"].is_null());
}
#[test]
fn mcp_continues_after_parse_error_without_leaking_input() {
    let output = run(
        MCP,
        &[],
        b"not JSON\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"ping\"}\n",
        Some(SECRET),
        true,
    );
    assert!(output.status.success());
    let replies: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(replies[1]["result"], json!({}));
}
#[test]
fn mcp_notifications_do_not_produce_spurious_responses() {
    let (output, replies) = rpc(
        &[
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1}}),
        ],
        None,
    );
    assert!(output.status.success());
    assert!(replies.is_empty());
}
#[test]
fn mcp_discovery_before_initialization_is_rejected() {
    let (output, replies) = rpc(
        &[json!({"jsonrpc":"2.0","id":1,"method":"tools/list"})],
        None,
    );
    assert!(output.status.success());
    assert_eq!(replies[0]["error"]["code"], -32000);
}
#[test]
fn mcp_unknown_tool_is_a_protocol_error_not_an_engine_request() {
    let (output, replies) = rpc(
        &[
            initialize(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"untrusted_shell"}}),
        ],
        Some(SECRET),
    );
    assert!(output.status.success());
    assert_eq!(replies[1]["error"]["code"], -32602);
}
#[test]
fn mcp_invalid_tool_arguments_return_a_structured_tool_error() {
    let (output, replies) = rpc(
        &[
            initialize(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"peerlume_request","arguments":{"confirmed":true}}}),
        ],
        Some(SECRET),
    );
    assert!(output.status.success());
    assert_eq!(replies[1]["result"]["isError"], true);
    let response: Value =
        serde_json::from_str(replies[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(response["error"], "invalid_request");
}
#[test]
fn mcp_not_running_is_a_tool_error_and_does_not_create_a_session() {
    let (output, replies) = rpc(
        &[
            initialize(),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"peerlume_request","arguments":{"method":{"name":"discover"}}}}),
        ],
        Some(SECRET),
    );
    assert!(output.status.success());
    assert_eq!(replies[1]["result"]["isError"], true);
    let response: Value =
        serde_json::from_str(replies[1]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(response["error"], "not_running");
    assert!(!String::from_utf8_lossy(&output.stdout).contains(SECRET));
}
#[test]
fn mcp_schema_output_remains_usable_without_an_application() {
    let (output, replies) = rpc(
        &[
            initialize(),
            json!({"jsonrpc":"2.0","id":"schema","method":"tools/list"}),
        ],
        None,
    );
    assert!(output.status.success());
    assert_eq!(replies[1]["id"], "schema");
    assert_eq!(
        replies[1]["result"]["tools"][0]["inputSchema"]["type"],
        "object"
    );
}
#[test]
fn mcp_oversized_message_exits_without_unbounded_output() {
    let output = run(MCP, &[], &vec![b'x'; 65537], Some(SECRET), true);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stdout.is_empty());
}
