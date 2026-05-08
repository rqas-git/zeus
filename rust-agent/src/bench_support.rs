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
