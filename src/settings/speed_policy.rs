//! Live global speed policy and its engine-confirmed readback.
use super::{ClientSettings, SpeedSettings};
use chrono::{Datelike, Timelike};
use librqbit::{Session, limits::LimitsConfig};
use serde::{Deserialize, Serialize};
use std::{num::NonZeroU32, sync::Mutex, sync::atomic::Ordering};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpeedPolicy {
    #[default]
    Normal,
    Quiet,
    Automatic,
}

impl SpeedPolicy {
    pub const ALL: [Self; 3] = [Self::Normal, Self::Quiet, Self::Automatic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Normal => "Normal",
            Self::Quiet => "Quiet",
            Self::Automatic => "Automatic",
        }
    }
}

impl SpeedSettings {
    pub fn policy(&self) -> SpeedPolicy {
        if self.schedule_enabled {
            SpeedPolicy::Automatic
        } else if self.alternate_enabled {
            SpeedPolicy::Quiet
        } else {
            SpeedPolicy::Normal
        }
    }

    pub fn set_policy(&mut self, policy: SpeedPolicy) {
        self.alternate_enabled = policy == SpeedPolicy::Quiet;
        self.schedule_enabled = policy == SpeedPolicy::Automatic;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffectiveSpeed {
    pub policy: SpeedPolicy,
    pub quiet: bool,
    pub download_limit_kib: u64,
    pub upload_limit_kib: u64,
    /// Zero means no global download queue cap.
    pub max_active_downloads: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpeedView {
    pub desired: SpeedSettings,
    pub saved: Option<SpeedSettings>,
    pub desired_queue_enabled: bool,
    pub saved_queue_enabled: Option<bool>,
    pub desired_max_active_downloads: usize,
    pub saved_max_active_downloads: Option<usize>,
    pub applied: Option<EffectiveSpeed>,
    pub pending: bool,
}

fn download_slots(settings: &ClientSettings) -> usize {
    if settings.queue.enabled {
        settings.queue.max_active_downloads
    } else {
        0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SpeedPlan {
    speed: SpeedSettings,
    normal: LimitsConfig,
    quiet: LimitsConfig,
    slots: usize,
}

impl SpeedPlan {
    pub(crate) fn from_settings(settings: &ClientSettings) -> Result<Self, String> {
        let speed = &settings.speed;
        if speed.schedule_begin_minutes >= 1440
            || speed.schedule_end_minutes >= 1440
            || (speed.schedule_enabled
                && (speed.schedule_days == 0 || speed.schedule_days & !0x7f != 0))
        {
            return Err("Invalid speed schedule".into());
        }
        let slots = download_slots(settings);
        if settings.queue.enabled && slots == 0 {
            return Err("Queueing requires at least one download slot".into());
        }
        let cap = |kib: u64| {
            u32::try_from(kib.saturating_mul(1024))
                .map(NonZeroU32::new)
                .map_err(|_| "Rate limit exceeds the engine range".to_owned())
        };
        Ok(Self {
            speed: speed.clone(),
            normal: LimitsConfig {
                download_bps: cap(speed.download_limit_kib)?,
                upload_bps: cap(speed.upload_limit_kib)?,
            },
            quiet: if speed.policy() == SpeedPolicy::Normal {
                LimitsConfig::default()
            } else {
                LimitsConfig {
                    download_bps: cap(speed.alternate_download_kib)?,
                    upload_bps: cap(speed.alternate_upload_kib)?,
                }
            },
            slots,
        })
    }

    pub(crate) fn initial_limits(&self) -> LimitsConfig {
        let (day, minute) = local_minute();
        self.limits_at(day, minute).1
    }

    fn limits_at(&self, day: u32, minute: u16) -> (bool, LimitsConfig) {
        let quiet = match self.speed.policy() {
            SpeedPolicy::Normal => false,
            SpeedPolicy::Quiet => true,
            SpeedPolicy::Automatic => schedule_active(&self.speed, day, minute),
        };
        (quiet, if quiet { self.quiet } else { self.normal })
    }
}

fn local_minute() -> (u32, u16) {
    let now = chrono::Local::now();
    (
        now.weekday().num_days_from_sunday(),
        (now.hour() * 60 + now.minute()) as u16,
    )
}

fn schedule_active(speed: &SpeedSettings, day: u32, minute: u16) -> bool {
    let start = speed.schedule_begin_minutes;
    let end = speed.schedule_end_minutes;
    let selected = |day| speed.schedule_days & (1_u8 << day) != 0;
    if start <= end {
        selected(day) && (start..end).contains(&minute)
    } else if minute >= start {
        selected(day)
    } else {
        minute < end && selected((day + 6) % 7)
    }
}

#[derive(Default)]
struct SpeedState {
    requested: Option<SpeedPlan>,
    applied: Option<(SpeedPlan, EffectiveSpeed)>,
}

/// A single latest request, read and applied by the existing engine actor.
#[derive(Default)]
pub(crate) struct LiveSpeed(Mutex<SpeedState>);

impl LiveSpeed {
    pub(crate) fn request(&self, plan: SpeedPlan) {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).requested = Some(plan);
    }

    pub(crate) fn set_queue(&self, slots: usize) {
        if let Some(plan) = self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .requested
            .as_mut()
        {
            plan.slots = slots;
        }
    }

    pub(crate) fn readback(&self) -> (Option<EffectiveSpeed>, bool) {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let applied = state.applied.as_ref();
        (
            applied.map(|(_, value)| *value),
            state.requested.as_ref() != applied.map(|(plan, _)| plan),
        )
    }

    pub(crate) fn refresh(&self, session: &Session) {
        self.refresh_with_clock(session, local_minute);
    }

    fn refresh_with_clock(&self, session: &Session, clock: impl FnOnce() -> (u32, u16)) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(plan) = state.requested.as_ref() else {
            return;
        };
        let (day, minute) = if plan.speed.policy() == SpeedPolicy::Automatic {
            clock()
        } else {
            (0, 0)
        };
        let (quiet, limits) = plan.limits_at(day, minute);
        let previous = session.ratelimits.get_config();
        if previous.download_bps != limits.download_bps {
            session.ratelimits.set_download_bps(limits.download_bps);
        }
        if previous.upload_bps != limits.upload_bps {
            session.ratelimits.set_upload_bps(limits.upload_bps);
        }
        if session.max_active_downloads.load(Ordering::Relaxed) != plan.slots {
            session
                .max_active_downloads
                .store(plan.slots, Ordering::Relaxed);
        }
        let actual = session.ratelimits.get_config();
        let applied = EffectiveSpeed {
            policy: plan.speed.policy(),
            quiet,
            download_limit_kib: actual
                .download_bps
                .map_or(0, |bps| u64::from(bps.get()) / 1024),
            upload_limit_kib: actual
                .upload_bps
                .map_or(0, |bps| u64::from(bps.get()) / 1024),
            max_active_downloads: session.max_active_downloads.load(Ordering::Relaxed),
        };
        if state
            .applied
            .as_ref()
            .is_none_or(|(old, value)| old != plan || *value != applied)
        {
            state.applied = Some((plan.clone(), applied));
        }
    }
}

#[cfg(test)]
mod tests;
