use super::*;
use crate::session::SessionRuntime;

#[test]
fn quiet_and_normal_caps_use_kib_and_accept_zero_as_unlimited() {
    let mut settings = ClientSettings::default();
    settings.speed.download_limit_kib = 123;
    settings.speed.upload_limit_kib = 0;
    settings.speed.alternate_download_kib = 0;
    settings.speed.alternate_upload_kib = 45;
    let normal = SpeedPlan::from_settings(&settings).unwrap().limits_at(1, 0);
    assert!(!normal.0);
    assert_eq!(normal.1.download_bps.map(NonZeroU32::get), Some(123 * 1024));
    assert_eq!(normal.1.upload_bps, None);
    settings.speed.set_policy(SpeedPolicy::Quiet);
    let quiet = SpeedPlan::from_settings(&settings).unwrap().limits_at(1, 0);
    assert!(quiet.0);
    assert_eq!(quiet.1.download_bps, None);
    assert_eq!(quiet.1.upload_bps.map(NonZeroU32::get), Some(45 * 1024));
    settings.speed.alternate_upload_kib = u64::MAX;
    assert!(SpeedPlan::from_settings(&settings).is_err());
    assert!(
        settings
            .validate()
            .iter()
            .any(|issue| issue.field == "speed.alternate_upload_kib")
    );
    settings.speed.set_policy(SpeedPolicy::Normal);
    assert!(SpeedPlan::from_settings(&settings).is_ok());
    assert!(settings.validate().is_empty());
}

#[test]
fn automatic_intervals_are_half_open_and_the_start_day_owns_overnight_hours() {
    let mut settings = ClientSettings::default();
    settings.speed.set_policy(SpeedPolicy::Automatic);
    settings.speed.schedule_days = 1 << 6;
    settings.speed.schedule_begin_minutes = 22 * 60;
    settings.speed.schedule_end_minutes = 2 * 60;
    let plan = SpeedPlan::from_settings(&settings).unwrap();
    for (day, minute, quiet) in [
        (6, 1319, false),
        (6, 1320, true),
        (6, 1439, true),
        (0, 0, true),
        (0, 119, true),
        (0, 120, false),
        (0, 1320, false),
        (1, 30, false),
    ] {
        assert_eq!(
            plan.limits_at(day, minute).0,
            quiet,
            "day {day} minute {minute}"
        );
    }
    settings.speed.schedule_begin_minutes = 120;
    settings.speed.schedule_end_minutes = 120;
    assert!(
        !SpeedPlan::from_settings(&settings)
            .unwrap()
            .limits_at(6, 120)
            .0
    );
    settings.speed.schedule_days = 0;
    assert!(SpeedPlan::from_settings(&settings).is_err());
    settings.speed.schedule_days = 0x80;
    assert!(SpeedPlan::from_settings(&settings).is_err());
}

#[test]
fn engine_readback_changes_only_after_actor_applies_and_reevaluates_wall_clock_jumps() {
    let directory = tempfile::tempdir().unwrap();
    let runtime = SessionRuntime::start_with_options(
        8,
        directory.path().into(),
        librqbit::SessionOptions {
            dht: None,
            listen: None,
            disable_trackers: true,
            disable_local_service_discovery: true,
            ..Default::default()
        },
    )
    .unwrap();
    let engine = runtime.test_engine().unwrap();
    let state = LiveSpeed::default();
    let mut settings = ClientSettings::default();
    settings.speed.set_policy(SpeedPolicy::Automatic);
    settings.speed.schedule_days = 1;
    settings.speed.schedule_begin_minutes = 2 * 60;
    settings.speed.schedule_end_minutes = 3 * 60;
    settings.speed.alternate_download_kib = 77;
    settings.speed.alternate_upload_kib = 88;
    settings.queue.max_active_downloads = 4;
    state.request(SpeedPlan::from_settings(&settings).unwrap());
    assert_eq!(state.readback(), (None, true));
    for (minute, quiet) in [
        (119, false),
        (180, false),
        (121, true),
        (179, true),
        (120, true),
        (180, false),
    ] {
        state.refresh_with_clock(&engine, || (0, minute));
        let (applied, pending) = state.readback();
        let applied = applied.unwrap();
        assert!(!pending);
        assert_eq!(applied.quiet, quiet);
        assert_eq!(applied.policy, SpeedPolicy::Automatic);
        assert_eq!(applied.download_limit_kib, if quiet { 77 } else { 0 });
        assert_eq!(applied.upload_limit_kib, if quiet { 88 } else { 0 });
        assert_eq!(applied.max_active_downloads, 4);
        assert_eq!(
            engine.ratelimits.get_download_bps().map(NonZeroU32::get),
            quiet.then_some(77 * 1024)
        );
    }
    drop(engine);
    runtime.shutdown().unwrap();
}

#[tokio::test]
async fn live_speed_raise_or_unlimit_wakes_a_block_waiting_for_old_quota() {
    use std::time::Duration;

    let block = NonZeroU32::new(16_384).unwrap();
    for next in [NonZeroU32::new(512 * 1024), None] {
        let limits = librqbit::limits::Limits::new(LimitsConfig {
            download_bps: NonZeroU32::new(1024),
            upload_bps: None,
        });
        limits.prepare_for_download(block).await.unwrap();
        let blocked = limits.prepare_for_download(block);
        tokio::pin!(blocked);
        assert!(
            tokio::time::timeout(Duration::from_millis(30), &mut blocked)
                .await
                .is_err(),
            "the second block must wait on the original one-block burst"
        );
        limits.set_download_bps(next);
        tokio::time::timeout(Duration::from_secs(1), blocked)
            .await
            .expect("live change must release a wait for the previous quota")
            .unwrap();
    }
}
