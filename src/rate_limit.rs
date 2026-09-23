use std::time::Duration;

#[derive(Clone, Debug)]
pub struct TokenBucket {
    rate_bytes_per_second: u64,
    capacity: u64,
    available: u64,
    fractional: u128,
}

impl TokenBucket {
    pub fn new(rate_bytes_per_second: u64, capacity: u64) -> Result<Self, String> {
        if rate_bytes_per_second == 0 || capacity == 0 {
            return Err("rate and capacity must be greater than zero".into());
        }
        Ok(Self {
            rate_bytes_per_second,
            capacity,
            available: capacity,
            fractional: 0,
        })
    }

    pub fn refill(&mut self, elapsed: Duration) {
        const NANOS_PER_SECOND: u128 = 1_000_000_000;
        let generated = (self.rate_bytes_per_second as u128)
            .saturating_mul(elapsed.as_nanos())
            .saturating_add(self.fractional);
        let whole = generated / NANOS_PER_SECOND;
        self.fractional = generated % NANOS_PER_SECOND;
        self.available = self
            .available
            .saturating_add(whole.try_into().unwrap_or(u64::MAX))
            .min(self.capacity);
    }

    pub fn try_take(&mut self, bytes: u64) -> bool {
        if bytes > self.available {
            return false;
        }
        self.available -= bytes;
        true
    }

    pub fn available(&self) -> u64 {
        self.available
    }

    pub fn set_rate(&mut self, rate_bytes_per_second: u64) -> Result<(), String> {
        if rate_bytes_per_second == 0 {
            return Err("rate must be greater than zero".into());
        }
        self.rate_bytes_per_second = rate_bytes_per_second;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_is_bounded_and_refills_deterministically() {
        let mut bucket = TokenBucket::new(1_000, 2_000).unwrap();
        assert!(bucket.try_take(1_500));
        assert!(!bucket.try_take(501));
        bucket.refill(Duration::from_millis(250));
        assert_eq!(bucket.available(), 750);
        bucket.refill(Duration::from_secs(10));
        assert_eq!(bucket.available(), 2_000);
    }

    #[test]
    fn fractional_refills_are_not_lost() {
        let mut bucket = TokenBucket::new(3, 10).unwrap();
        assert!(bucket.try_take(10));
        for _ in 0..10 {
            bucket.refill(Duration::from_millis(100));
        }
        assert_eq!(bucket.available(), 3);
    }
}
