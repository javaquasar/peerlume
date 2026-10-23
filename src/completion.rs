//! Shared, engine-confirmed completion and integrity-check presentation.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum CompletionStatus {
    #[default]
    Pending,
    Checking {
        checked_bytes: u64,
        total_bytes: u64,
    },
    Verified,
    Failed {
        message: String,
    },
}

impl CompletionStatus {
    pub fn checking_progress(&self) -> Option<f32> {
        match self {
            Self::Checking {
                checked_bytes,
                total_bytes,
            } => Some(if *total_bytes == 0 {
                0.0
            } else {
                (*checked_bytes as f64 / *total_bytes as f64).clamp(0.0, 1.0) as f32
            }),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checking_progress_is_separate_bounded_and_handles_zero_size() {
        for (checked, total, expected) in [(0, 0, 0.0), (25, 100, 0.25), (200, 100, 1.0)] {
            assert_eq!(
                CompletionStatus::Checking {
                    checked_bytes: checked,
                    total_bytes: total
                }
                .checking_progress(),
                Some(expected)
            );
        }
        assert_eq!(CompletionStatus::Verified.checking_progress(), None);
    }
}
