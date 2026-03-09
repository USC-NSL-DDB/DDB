use std::time::Duration;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct SummaryStats {
    pub samples: usize,
    pub min_ms: f64,
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub max_ms: f64,
}

impl SummaryStats {
    pub fn from_durations(samples: &[Duration]) -> Self {
        assert!(
            !samples.is_empty(),
            "summary statistics require at least one sample"
        );

        let mut values = samples
            .iter()
            .map(|duration| duration.as_secs_f64() * 1_000.0)
            .collect::<Vec<_>>();
        values.sort_by(|left, right| left.total_cmp(right));

        let sum = values.iter().copied().sum::<f64>();

        Self {
            samples: values.len(),
            min_ms: values[0],
            mean_ms: sum / values.len() as f64,
            p50_ms: percentile(&values, 0.50),
            p95_ms: percentile(&values, 0.95),
            p99_ms: percentile(&values, 0.99),
            max_ms: values[values.len() - 1],
        }
    }
}

fn percentile(sorted_values_ms: &[f64], quantile: f64) -> f64 {
    assert!(
        (0.0..=1.0).contains(&quantile),
        "quantile must be in [0.0, 1.0]"
    );

    if sorted_values_ms.len() == 1 {
        return sorted_values_ms[0];
    }

    let max_index = (sorted_values_ms.len() - 1) as f64;
    let rank = quantile * max_index;
    let lower = rank.floor() as usize;
    let upper = rank.ceil() as usize;

    if lower == upper {
        return sorted_values_ms[lower];
    }

    let weight = rank - lower as f64;
    sorted_values_ms[lower] + (sorted_values_ms[upper] - sorted_values_ms[lower]) * weight
}
