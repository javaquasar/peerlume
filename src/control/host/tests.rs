use super::*;
use serde_json::json;
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn request() -> Request {
    Request {
        protocol_version: 1,
        request_id: "request".into(),
        token: "token".into(),
        instance_id: None,
        method: wire::Method::Discover,
    }
}
fn frame() -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&request()).unwrap();
    bytes.push(b'\n');
    bytes
}
async fn reply(reader: tokio::io::DuplexStream) -> Response {
    let bytes = read_frame(&mut BufReader::new(reader), wire::MAX_RESPONSE_BYTES + 1)
        .await
        .unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

#[test]
fn profile_lock_is_exclusive_and_released_by_drop() {
    let root = tempfile::tempdir().unwrap();
    let first = InstanceLock::acquire(root.path()).unwrap();
    assert!(InstanceLock::acquire(root.path()).is_err());
    drop(first);
    assert!(InstanceLock::acquire(root.path()).is_ok());
}
#[test]
fn different_profiles_can_have_independent_owners() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let _a = InstanceLock::acquire(a.path()).unwrap();
    let _b = InstanceLock::acquire(b.path()).unwrap();
    assert_ne!(endpoint(a.path()).unwrap(), endpoint(b.path()).unwrap());
}
#[test]
fn endpoint_uses_canonical_profile_identity() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("child")).unwrap();
    assert_eq!(
        endpoint(root.path()).unwrap(),
        endpoint(&root.path().join("child/..")).unwrap()
    );
}
#[test]
fn nonexistent_profile_has_no_discoverable_endpoint() {
    let root = tempfile::tempdir().unwrap();
    assert!(endpoint(&root.path().join("missing")).is_err());
    assert!(!root.path().join("missing").exists());
}
#[test]
fn exact_frame_boundary_and_multiple_frames_are_not_conflated() {
    runtime().block_on(async {
        let mut reader = BufReader::new(b"1234\nnext\n".as_slice());
        assert_eq!(read_frame(&mut reader, 5).await.unwrap(), b"1234");
        assert_eq!(read_frame(&mut reader, 5).await.unwrap(), b"next");
    });
}
#[test]
fn frame_limit_includes_its_delimiter() {
    runtime().block_on(async {
        let mut reader = BufReader::new(b"12345\n".as_slice());
        assert_eq!(read_frame(&mut reader, 5).await, Err(ErrorCode::TooLarge));
    });
}
#[test]
fn eof_before_newline_is_not_a_complete_ipc_request() {
    runtime().block_on(async {
        for data in [b"".as_slice(), b"{}".as_slice()] {
            let mut reader = BufReader::new(data);
            assert_eq!(
                read_frame(&mut reader, 100).await,
                Err(ErrorCode::InvalidRequest)
            );
        }
    });
}
#[test]
fn fragmented_requests_are_assembled_before_delivery() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        let handler = tokio::spawn(connection(server, tx));
        let bytes = frame();
        let split = bytes.len() / 2;
        client.write_all(&bytes[..split]).await.unwrap();
        assert!(rx.try_recv().is_err());
        client.write_all(&bytes[split..]).await.unwrap();
        let envelope = rx.recv().await.unwrap();
        assert_eq!(envelope.request.request_id, "request");
        let _ = envelope.reply.send(Response::success(json!({"ok":true})));
        assert_eq!(reply(client).await.result.unwrap()["ok"], true);
        handler.await.unwrap();
    });
}
#[test]
fn malformed_requests_do_not_enter_the_controller_queue() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        let handler = tokio::spawn(connection(server, tx));
        client.write_all(b"not JSON SECRET\n").await.unwrap();
        let response = reply(client).await;
        assert_eq!(response.error, Some(ErrorCode::InvalidRequest));
        assert!(!serde_json::to_string(&response).unwrap().contains("SECRET"));
        handler.await.unwrap();
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn oversized_requests_do_not_enter_the_controller_queue() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(wire::MAX_REQUEST_BYTES + 4096);
        let (tx, mut rx) = mpsc::channel(1);
        let handler = tokio::spawn(connection(server, tx));
        let mut bytes = vec![b'x'; wire::MAX_REQUEST_BYTES];
        bytes.push(b'\n');
        client.write_all(&bytes).await.unwrap();
        assert_eq!(reply(client).await.error, Some(ErrorCode::TooLarge));
        handler.await.unwrap();
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn full_controller_queue_returns_busy_without_replacing_queued_work() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        let (sender, _receiver) = oneshot::channel();
        let mut first = request();
        first.request_id = "first".into();
        tx.try_send(Envelope {
            request: first,
            reply: sender,
        })
        .unwrap_or_else(|_| panic!("empty queue"));
        let handler = tokio::spawn(connection(server, tx));
        client.write_all(&frame()).await.unwrap();
        assert_eq!(reply(client).await.error, Some(ErrorCode::Busy));
        assert_eq!(rx.recv().await.unwrap().request.request_id, "first");
        handler.await.unwrap();
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn closed_controller_queue_returns_busy() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let handler = tokio::spawn(connection(server, tx));
        client.write_all(&frame()).await.unwrap();
        assert_eq!(reply(client).await.error, Some(ErrorCode::Busy));
        handler.await.unwrap();
    });
}
#[test]
fn dropped_controller_reply_is_not_reported_as_success() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        let handler = tokio::spawn(connection(server, tx));
        client.write_all(&frame()).await.unwrap();
        drop(rx.recv().await.unwrap());
        assert_eq!(reply(client).await.error, Some(ErrorCode::Busy));
        handler.await.unwrap();
    });
}
#[test]
fn disconnected_client_cannot_panic_the_connection_handler() {
    runtime().block_on(async {
        let (client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        drop(client);
        connection(server, tx).await;
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn one_connection_accepts_only_one_request() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(2);
        let handler = tokio::spawn(connection(server, tx));
        let mut bytes = frame();
        bytes.extend(frame());
        client.write_all(&bytes).await.unwrap();
        let envelope = rx.recv().await.unwrap();
        let _ = envelope.reply.send(Response::success(json!({})));
        assert!(reply(client).await.error.is_none());
        handler.await.unwrap();
        assert!(rx.try_recv().is_err());
    });
}
#[test]
fn oversized_controller_response_is_bounded_on_the_wire() {
    runtime().block_on(async {
        let (mut client, server) = tokio::io::duplex(4096);
        let (tx, mut rx) = mpsc::channel(1);
        let handler = tokio::spawn(connection(server, tx));
        client.write_all(&frame()).await.unwrap();
        let envelope = rx.recv().await.unwrap();
        let _ = envelope.reply.send(Response::success(
            json!({"huge":"x".repeat(wire::MAX_RESPONSE_BYTES)}),
        ));
        assert_eq!(reply(client).await.error, Some(ErrorCode::TooLarge));
        handler.await.unwrap();
    });
}
#[test]
fn dropping_host_does_not_wait_for_a_stalled_frontend_lock() {
    let root = tempfile::tempdir().unwrap();
    let owner = Arc::new(Mutex::new(Controller::new(
        crate::client::ClientBackend::offline(),
        vec![],
    )));
    let host = Host::start(owner.clone(), |c| c, root.path().into()).unwrap();
    let guard = owner.lock().unwrap();
    drop(host);
    assert!(
        guard
            .read_handle()
            .page(0, 1, None, None)
            .unwrap()
            .controller_available
    );
}
#[cfg(unix)]
#[test]
fn unix_listener_never_removes_a_non_socket_discovery_path() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("control.sock");
    std::fs::write(&path, b"keep existing user file").unwrap();
    let error = Arc::new(Mutex::new(None));
    let (tx, _rx) = mpsc::channel(1);
    runtime().block_on(listen(
        path.to_string_lossy().into_owned(),
        tx,
        Arc::new(AtomicBool::new(true)),
        error.clone(),
    ));
    assert_eq!(*error.lock().unwrap(), Some(ErrorCode::PathDenied));
    assert_eq!(std::fs::read(path).unwrap(), b"keep existing user file");
}
