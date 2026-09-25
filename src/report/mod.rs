pub mod step;
pub mod tui;

use std::time::{Duration, SystemTime};

use crate::navigator::GroundStep;
use crate::value::Value;

pub use crate::log::{allocate_unique_file, DEFAULT_TRACE_DIR};

pub fn jev_request_count(steps: &[GroundStep]) -> usize {
    steps.iter().map(|s| s.exchanges().len()).sum()
}

/// Wall-clock span of one `autoground run`.
#[derive(Debug, Clone, Copy)]
pub struct RunTiming {
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}

impl RunTiming {
    pub fn from_millis(started_at_ms: u64, ended_at_ms: u64) -> Self {
        Self {
            started_at: crate::navigator::judge::from_epoch_ms(started_at_ms),
            ended_at: crate::navigator::judge::from_epoch_ms(ended_at_ms),
        }
    }

    pub fn elapsed(self) -> Duration {
        self.ended_at
            .duration_since(self.started_at)
            .unwrap_or(Duration::ZERO)
    }
}

/// Start/end of each Jev round-trip that recorded wall-clock times.
pub fn jev_request_intervals(steps: &[GroundStep]) -> Vec<(SystemTime, SystemTime)> {
    steps
        .iter()
        .flat_map(|s| s.exchanges())
        .filter_map(|ex| Some((ex.started_at?, ex.ended_at?)))
        .collect()
}

/// Sum of recorded Jev costs, if any exchange reported one.
pub fn jev_request_cost_usd(steps: &[GroundStep]) -> Option<f64> {
    let mut any = false;
    let mut sum = 0.0;
    for ex in steps.iter().flat_map(|s| s.exchanges()) {
        if let Some(c) = ex.cost_usd {
            any = true;
            sum += c;
        }
    }
    any.then_some(sum)
}

pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.2}s", d.as_secs_f64())
    } else {
        let secs = d.as_secs();
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

pub fn format_usd(cost: f64) -> String {
    if cost >= 0.01 {
        format!("${cost:.2}")
    } else if cost >= 0.0001 {
        format!("${cost:.4}")
    } else {
        format!("${cost:.6}")
    }
}

pub fn outcome_exit_code(value: &Value) -> i32 {
    match value {
        Value::True => 0,
        Value::False => 2,
        Value::Unknown => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_unique_file_uses_distinct_names() {
        let dir =
            std::env::temp_dir().join(format!("autoground-trace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = allocate_unique_file(&dir, "json").unwrap();
        let b = allocate_unique_file(&dir, "json").unwrap();
        assert_ne!(a, b);
        assert!(a.extension().is_some_and(|e| e == "json"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn jev_request_count_sums_exchanges() {
        use crate::navigator::judge::JudgeExchange;
        let empty = GroundStep::Result {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            status: "bound".into(),
            bindings: vec![],
        };
        let listing = GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: String::new(),
            children: vec![],
            choices: vec![],
            effects: vec![],
            exchanges: vec![
                JudgeExchange::new(serde_json::json!({}), serde_json::json!({})),
                JudgeExchange::new(serde_json::json!({}), serde_json::json!({})),
            ],
        };
        assert_eq!(jev_request_count(&[empty, listing]), 2);
    }

    #[test]
    fn format_duration_picks_units() {
        assert_eq!(format_duration(Duration::from_millis(340)), "340ms");
        assert_eq!(format_duration(Duration::from_millis(1420)), "1.42s");
        assert_eq!(format_duration(Duration::from_secs(72)), "1m 12s");
    }

    #[test]
    fn jev_request_cost_sums_reported_costs() {
        use crate::navigator::judge::JudgeExchange;
        let listing = GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: String::new(),
            children: vec![],
            choices: vec![],
            effects: vec![],
            exchanges: vec![
                JudgeExchange {
                    cost_usd: Some(0.0001),
                    ..JudgeExchange::default()
                },
                JudgeExchange {
                    cost_usd: Some(0.0002),
                    ..JudgeExchange::default()
                },
            ],
        };
        assert!((jev_request_cost_usd(&[listing]).unwrap() - 0.0003).abs() < 1e-12);
        assert_eq!(jev_request_cost_usd(&[]), None);
    }
}
