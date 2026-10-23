//! Exercise copied executables outside the checkout against the owner's real IPC.
use peerlume::{
    client::ClientBackend,
    control::{
        Controller,
        access::Grants,
        host::{Host, InstanceLock},
        wire::ErrorCode,
    },
};
use serde_json::{Value, json};
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

fn installed(root: &std::path::Path, source: &str, name: &str) -> std::path::PathBuf {
    let suffix = if cfg!(windows) { ".exe" } else { "" };
    let target = root.join(format!("{name}{suffix}"));
    std::fs::copy(source, &target).unwrap();
    target
}

#[test]
fn copied_cli_and_mcp_attach_to_same_isolated_profile_without_repository_paths() {
    let root = tempfile::tempdir().unwrap();
    let profile = root.path().join("profile");
    std::fs::create_dir(&profile).unwrap();
    let _instance = InstanceLock::acquire(&profile).unwrap();
    let cli = installed(
        root.path(),
        env!("CARGO_BIN_EXE_peerlume-control"),
        "control",
    );
    let mcp = installed(root.path(), env!("CARGO_BIN_EXE_peerlume-mcp"), "bridge");
    let mut control = Controller::new(ClientBackend::offline(), vec![]);
    control
        .configure_agents(&profile, root.path().into())
        .unwrap();
    let agent = control.agent_access().unwrap();
    agent.enabled = true;
    let token = agent.grant(Grants::default()).unwrap();
    let instance = agent.instance_id.clone();
    let owner = Arc::new(Mutex::new(control));
    let host = Host::start(owner.clone(), |c| c, profile.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let output = loop {
        let output = Command::new(&cli)
            .arg("discover")
            .arg("--profile")
            .arg(&profile)
            .env("PEERLUME_CONTROL_TOKEN", &token)
            .current_dir(root.path())
            .output()
            .unwrap();
        if output.status.success() {
            break output;
        }
        assert!(
            Instant::now() < deadline,
            "CLI failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply["instance_id"], instance);
    assert_eq!(reply["result"]["grants"]["manage"], false);
    let denied = Command::new(&cli)
        .arg("discover")
        .arg("--profile")
        .arg(&profile)
        .env("PEERLUME_CONTROL_TOKEN", "wrong")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(denied.status.code(), Some(4));
    assert_eq!(
        serde_json::from_slice::<Value>(&denied.stdout).unwrap()["error"],
        "unauthorized"
    );
    let mut child = Command::new(&mcp)
        .arg("--profile")
        .arg(&profile)
        .env("PEERLUME_CONTROL_TOKEN", &token)
        .current_dir(root.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    for message in [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"fixture","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"peerlume_request","arguments":{"instance_id":instance,"method":{"name":"list","args":{"offset":0,"limit":10,"revision":null,"after_revision":null}}}}}),
    ] {
        writeln!(input, "{message}").unwrap();
    }
    drop(input);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let messages: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(messages.len(), 3);
    assert_eq!(messages[0]["result"]["protocolVersion"], "2025-11-25");
    let bridge_reply: Value = serde_json::from_str(
        messages[2]["result"]["content"][0]["text"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(bridge_reply["instance_id"], instance);
    assert_eq!(bridge_reply["result"]["total"], 0);
    assert!(host.error.lock().unwrap().is_none());
    drop(host);
    drop(owner);
}

#[test]
fn installed_cli_reports_not_running_and_never_launches_application() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_peerlume-control"))
        .arg("discover")
        .arg("--profile")
        .arg(root.path())
        .env("PEERLUME_CONTROL_TOKEN", "test-token")
        .current_dir(root.path())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(3));
    let reply: peerlume::control::wire::Response = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(reply.error, Some(ErrorCode::NotRunning));
    assert!(!root.path().join("state.sqlite3").exists());
}
