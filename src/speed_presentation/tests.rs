use super::*;
use crate::settings::speed_policy::EffectiveSpeed;

#[test]
fn a_live_profile_change_preserves_unsaved_cap_edits() {
    let previous = SpeedSettings {
        download_limit_kib: 500,
        ..Default::default()
    };
    let mut draft = previous.clone();
    draft.download_limit_kib = 700;
    draft.schedule_begin_minutes = 90;
    let mut desired = previous.clone();
    desired.set_policy(SpeedPolicy::Quiet);
    desired.download_limit_kib = 600;
    desired.upload_limit_kib = 42;
    merge_draft(&mut draft, &previous, &desired);
    assert_eq!(draft.policy(), SpeedPolicy::Quiet);
    assert_eq!(draft.download_limit_kib, 700);
    assert_eq!(draft.upload_limit_kib, 42);
    assert_eq!(draft.schedule_begin_minutes, 90);
}

#[test]
fn an_explicit_policy_draft_survives_an_external_switch() {
    let previous = SpeedSettings::default();
    let mut draft = previous.clone();
    draft.set_policy(SpeedPolicy::Automatic);
    let mut desired = previous.clone();
    desired.set_policy(SpeedPolicy::Quiet);
    merge_draft(&mut draft, &previous, &desired);
    assert_eq!(draft.policy(), SpeedPolicy::Automatic);
}

#[test]
fn status_displays_the_engine_readback_while_a_new_profile_is_pending() {
    let mut desired = SpeedSettings::default();
    desired.set_policy(SpeedPolicy::Quiet);
    let mut view = SpeedView {
        desired,
        saved: None,
        desired_queue_enabled: true,
        saved_queue_enabled: None,
        desired_max_active_downloads: 2,
        saved_max_active_downloads: None,
        applied: None,
        pending: true,
    };
    assert!(applied_label(&view).starts_with("Waiting for confirmed limits"));
    view.applied = Some(EffectiveSpeed {
        policy: SpeedPolicy::Automatic,
        quiet: false,
        download_limit_kib: 200,
        upload_limit_kib: 0,
        max_active_downloads: 3,
    });
    let label = applied_label(&view);
    assert!(label.contains("Automatic (Normal)"));
    assert!(label.contains("Down 200 KiB/s"));
    assert!(label.contains("Up Unlimited"));
    assert!(label.contains("Download slots 3"));
}

#[test]
fn a_live_queue_change_merges_untouched_fields_and_keeps_the_disabled_cap() {
    let mut current = ClientSettings::default();
    let mut draft = current.clone();
    draft.queue.max_active_downloads = 8;
    let view = SpeedView {
        desired: current.speed.clone(),
        saved: None,
        desired_queue_enabled: false,
        saved_queue_enabled: None,
        desired_max_active_downloads: 5,
        saved_max_active_downloads: None,
        applied: None,
        pending: true,
    };
    accept_view(&mut current, Some(&mut draft), &view);
    assert!(!current.queue.enabled);
    assert!(!draft.queue.enabled);
    assert_eq!(current.queue.max_active_downloads, 5);
    assert_eq!(draft.queue.max_active_downloads, 8);
}
