//! Integer fixed-point multiplier bounds: 1000 means x1.000.
use rand::RngExt;
use serde_derive::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawRange")]
pub struct UploadRatioRange {
    pub min_milli: u32,
    pub max_milli: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRange {
    min_milli: u32,
    max_milli: u32,
}

impl TryFrom<RawRange> for UploadRatioRange {
    type Error = &'static str;
    fn try_from(raw: RawRange) -> Result<Self, Self::Error> {
        let value = Self {
            min_milli: raw.min_milli,
            max_milli: raw.max_milli,
        };
        value.validate()?;
        Ok(value)
    }
}

impl Default for UploadRatioRange {
    fn default() -> Self {
        Self {
            min_milli: 1500,
            max_milli: 2500,
        }
    }
}

impl UploadRatioRange {
    pub fn validate(self) -> Result<(), &'static str> {
        if self.min_milli == 0 || self.min_milli > self.max_milli {
            return Err(
                "Upload multiplier bounds must be positive and minimum must not exceed maximum",
            );
        }
        Ok(())
    }

    pub(super) fn sample_with(self, rng: &mut impl rand::Rng) -> u32 {
        rng.random_range(self.min_milli..=self.max_milli)
    }

    /// Parse a positive decimal with at most three fractional digits, without
    /// floating-point rounding, exponent syntax, NaN or infinity.
    pub fn parse_multiplier(text: &str) -> Result<u32, &'static str> {
        let text = text.trim();
        let mut parts = text.split(['.', ',']);
        let whole = parts.next().unwrap_or_default();
        let fraction = parts.next().unwrap_or_default();
        if parts.next().is_some()
            || whole.is_empty()
            || fraction.len() > 3
            || !whole.bytes().all(|c| c.is_ascii_digit())
            || !fraction.bytes().all(|c| c.is_ascii_digit())
        {
            return Err("Enter a positive multiplier with at most three decimal places");
        }
        let whole: u32 = whole
            .parse()
            .map_err(|_| "Upload multiplier is too large")?;
        let fraction: u32 = if fraction.is_empty() {
            0
        } else {
            fraction
                .parse::<u32>()
                .map_err(|_| "Invalid upload multiplier")?
                * 10_u32.pow(3 - fraction.len() as u32)
        };
        whole
            .checked_mul(1000)
            .and_then(|value| value.checked_add(fraction))
            .filter(|value| *value > 0)
            .ok_or("Upload multiplier must be positive and fit a 32-bit milli value")
    }

    pub fn format_multiplier(milli: u32) -> String {
        let mut value = format!("{}.{:03}", milli / 1000, milli % 1000);
        while value.ends_with('0') {
            value.pop();
        }
        if value.ends_with('.') {
            value.push('0');
        }
        value
    }

    pub fn label(self) -> String {
        format!(
            "{}–{}",
            Self::format_multiplier(self.min_milli),
            Self::format_multiplier(self.max_milli)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    #[test]
    fn decimal_parsing_is_exact_bounded_and_rejects_non_numbers() {
        for (text, value) in [
            ("1.5", 1500),
            ("2,5", 2500),
            ("0.001", 1),
            ("2", 2000),
            ("4294967.295", u32::MAX),
        ] {
            assert_eq!(UploadRatioRange::parse_multiplier(text), Ok(value));
            assert_eq!(
                UploadRatioRange::parse_multiplier(&UploadRatioRange::format_multiplier(value)),
                Ok(value)
            );
        }
        for text in [
            "0",
            "-1",
            "NaN",
            "inf",
            "1e3",
            "1.0001",
            "4294967.296",
            "1.2.3",
            ".5",
            "",
        ] {
            assert!(UploadRatioRange::parse_multiplier(text).is_err(), "{text}");
        }
    }

    #[test]
    fn seeded_sampling_uses_the_whole_inclusive_range_and_equal_bounds_are_fixed() {
        let mut rng = rand::rngs::StdRng::seed_from_u64(7);
        let range = UploadRatioRange {
            min_milli: 1500,
            max_milli: 1501,
        };
        let samples: std::collections::BTreeSet<_> =
            (0..128).map(|_| range.sample_with(&mut rng)).collect();
        assert_eq!(samples, [1500, 1501].into_iter().collect());
        assert_eq!(
            UploadRatioRange {
                min_milli: 2125,
                max_milli: 2125
            }
            .sample_with(&mut rng),
            2125
        );
        assert!(
            UploadRatioRange {
                min_milli: 0,
                max_milli: 2500
            }
            .validate()
            .is_err()
        );
        assert!(
            UploadRatioRange {
                min_milli: 2501,
                max_milli: 2500
            }
            .validate()
            .is_err()
        );
        assert_eq!(
            UploadRatioRange::default(),
            UploadRatioRange {
                min_milli: 1500,
                max_milli: 2500
            }
        );
    }
}
