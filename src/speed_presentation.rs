//! Shared rendering and draft merge for owner-confirmed live speed controls.
use crate::settings::{
    ClientSettings, QueueSettings, SpeedSettings,
    speed_policy::{SpeedPolicy, SpeedView},
};

pub(crate) fn applied_label(view: &SpeedView) -> String {
    let Some(applied) = &view.applied else {
        return "Waiting for confirmed limits from the background client".into();
    };
    let cap = |kib| {
        if kib == 0 {
            "Unlimited".into()
        } else {
            format!("{kib} KiB/s")
        }
    };
    let profile = if applied.policy == SpeedPolicy::Automatic {
        if applied.quiet {
            "Automatic (Quiet)"
        } else {
            "Automatic (Normal)"
        }
    } else {
        applied.policy.label()
    };
    let slots = if applied.max_active_downloads == 0 {
        "Unlimited".into()
    } else {
        applied.max_active_downloads.to_string()
    };
    format!(
        "Active: {profile} · Down {} · Up {} · Download slots {slots}",
        cap(applied.download_limit_kib),
        cap(applied.upload_limit_kib),
    )
}

pub(crate) fn merge_draft(
    draft: &mut SpeedSettings,
    previous: &SpeedSettings,
    desired: &SpeedSettings,
) {
    if draft.policy() == previous.policy() {
        draft.set_policy(desired.policy());
    }
    macro_rules! adopt_unchanged {
        ($($field:ident),* $(,)?) => {
            $(if draft.$field == previous.$field {
                draft.$field = desired.$field;
            })*
        };
    }
    adopt_unchanged!(
        download_limit_kib,
        upload_limit_kib,
        alternate_download_kib,
        alternate_upload_kib,
        schedule_begin_minutes,
        schedule_end_minutes,
        schedule_days,
        limit_local_peers,
    );
}

pub(crate) fn merge_queue_draft(
    draft: &mut QueueSettings,
    previous: &QueueSettings,
    enabled: bool,
    slots: usize,
) {
    if draft.enabled == previous.enabled {
        draft.enabled = enabled;
    }
    if draft.max_active_downloads == previous.max_active_downloads {
        draft.max_active_downloads = slots;
    }
}

pub(crate) fn accept_view(
    current: &mut ClientSettings,
    draft: Option<&mut ClientSettings>,
    view: &SpeedView,
) {
    if let Some(draft) = draft {
        merge_draft(&mut draft.speed, &current.speed, &view.desired);
        merge_queue_draft(
            &mut draft.queue,
            &current.queue,
            view.desired_queue_enabled,
            view.desired_max_active_downloads,
        );
    }
    current.speed = view.desired.clone();
    current.queue.enabled = view.desired_queue_enabled;
    current.queue.max_active_downloads = view.desired_max_active_downloads;
}

#[cfg(test)]
mod tests;
