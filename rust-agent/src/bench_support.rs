//! Shared helpers for ignored release-mode benchmark tests.

use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub(crate) struct DurationSummary {
    pub(crate) min: Duration,
    pub(crate) median: Duration,
    pub(crate) max: Duration,
}

impl DurationSummary {
    pub(crate) fn from_samples(samples: &mut [Duration]) -> Self {
        assert!(!samples.is_empty(), "benchmark samples cannot be empty");
        samples.sort_unstable();
        Self {
            min: samples[0],
            median: samples[samples.len() / 2],
            max: samples[samples.len() - 1],
        }
    }

    pub(crate) fn min_ms(self) -> f64 {
        duration_ms(self.min)
    }

    pub(crate) fn median_ms(self) -> f64 {
        duration_ms(self.median)
    }

    pub(crate) fn max_ms(self) -> f64 {
        duration_ms(self.max)
    }
}

pub(crate) fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

pub(crate) fn usize_per_second(count: usize, duration: Duration) -> f64 {
    f64::from(u32::try_from(count).expect("benchmark count should fit in u32"))
        / duration.as_secs_f64()
}

pub(crate) fn u64_per_second(count: u64, duration: Duration) -> f64 {
    f64::from(u32::try_from(count).expect("benchmark count should fit in u32"))
        / duration.as_secs_f64()
}

pub(crate) fn mib_per_second(bytes: usize, duration: Duration) -> f64 {
    usize_per_second(bytes, duration) / 1024.0 / 1024.0
}
