use super::*;
use std::time::{Duration, Instant};

fn finish(panel: &mut UpdatesPanel) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while panel.pending.is_some() {
        panel.poll();
        assert!(
            Instant::now() < deadline,
            "package inspection did not finish"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn cancelled_picker_preserves_no_verified_candidate_and_allows_retry() {
    let mut panel = UpdatesPanel::default();
    let ctx = egui::Context::default();
    panel.start(&ctx, || None);
    finish(&mut panel);
    assert!(panel.result.is_none());
    let package = tempfile::tempdir().unwrap();
    let directory = package.path().to_owned();
    panel.start(&ctx, move || Some(directory));
    finish(&mut panel);
    let error = panel.result.unwrap().unwrap_err();
    assert!(error.contains("build-info.json"));
}

#[test]
fn outstanding_picker_cannot_be_replaced_by_another_worker() {
    let mut panel = UpdatesPanel::default();
    let ctx = egui::Context::default();
    let (release, permit) = mpsc::sync_channel(1);
    panel.start(&ctx, move || {
        permit.recv_timeout(Duration::from_secs(5)).unwrap();
        None
    });
    let (duplicate, observed) = mpsc::sync_channel(1);
    panel.start(&ctx, move || {
        let _ = duplicate.send(());
        None
    });
    release.send(()).unwrap();
    finish(&mut panel);
    assert_eq!(observed.try_recv(), Err(TryRecvError::Disconnected));
    assert!(panel.result.is_none());
}

#[test]
fn disconnected_worker_is_an_error_without_leaving_picker_busy() {
    let (worker, result) = mpsc::sync_channel(1);
    let mut panel = UpdatesPanel {
        pending: Some(result),
        ..Default::default()
    };
    drop(worker);
    panel.poll();
    assert!(panel.pending.is_none());
    assert!(
        panel
            .result
            .unwrap()
            .unwrap_err()
            .contains("ended unexpectedly")
    );
}

#[test]
fn preview_renders_hash_verification_and_unsigned_trust_separately() {
    let mut panel = UpdatesPanel {
        result: Some(Ok(VerifiedPackage {
            folder: "test extracted package".into(),
            version: "0.1.0".into(),
            commit: "2d4df223847e4ff07cf2db5f27ae38feefdc742b".into(),
            platform: "windows-x64".into(),
            target: "x86_64-pc-windows-msvc".into(),
            rustc: "rustc 1.93.0".into(),
            hashed_files: 12,
            hashed_bytes: 1024,
        })),
        ..Default::default()
    };
    panel.open();
    let ctx = egui::Context::default();
    let input = || egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1024.0, 768.0),
        )),
        ..Default::default()
    };
    let _ = ctx.run(input(), |ctx| panel.show(ctx));
    let output = ctx.run(input(), |ctx| panel.show(ctx));
    let labels = output
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::epaint::Shape::Text(text) => Some(text.galley.job.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    for expected in [
        "12 file hashes match SHA256SUMS (1024 bytes)",
        "Manifest source commit 2d4df223847e4ff07cf2db5f27ae38feefdc742b",
        "Local integrity check.",
        "Native protocol compatibility requires a separate check.",
    ] {
        assert!(
            labels.iter().any(|label| label.starts_with(expected)),
            "missing {expected}: {labels:?}"
        );
    }
}
