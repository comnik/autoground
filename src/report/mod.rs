pub mod run;
pub mod summary;

use std::time::Duration;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_duration_picks_units() {
        assert_eq!(format_duration(Duration::from_millis(340)), "340ms");
        assert_eq!(format_duration(Duration::from_millis(1420)), "1.42s");
        assert_eq!(format_duration(Duration::from_secs(72)), "1m 12s");
    }
}
