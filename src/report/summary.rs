//! Closing lines of `autoground run`: where the trace went, how full the
//! in-flight cap stayed, the shape of round-trip latency, and what the run
//! cost.

use std::io::{self, stdout, IsTerminal, Write};
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use crossterm::{
    queue,
    style::{Attribute, Color, Print, ResetColor, SetAttribute, SetForegroundColor},
    terminal,
};

use crate::oracle::{Flight, Usage};

const FLIGHT_BUCKETS: usize = 24;
const HIST_BINS: usize = 16;
const FLIGHT_BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const HIST_BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Paint {
    Plain,
    Green,
    Yellow,
    Red,
    Dim,
}

struct Painted {
    text: String,
    paint: Paint,
    bold: bool,
}

/// `requests`, `tokens`, `spend`, `batch`, and `fill`, without latency.
pub fn format_usage_stats(usage: &Usage) -> String {
    let batch = match usage.batch_size {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    let fill = match usage.fill_factor {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    format!(
        "requests={} tokens={} spend={} batch={batch} fill={fill}",
        usage.requests,
        usage.input_tokens,
        super::format_usd(usage.spend_usd),
    )
}

pub fn print_run_summary(
    usage: &Usage,
    flights: &[Flight],
    flight_limit: Option<usize>,
    true_count: usize,
    total: usize,
    elapsed: Duration,
    trace: &Path,
) -> io::Result<()> {
    let color = stdout().is_terminal();
    let width = terminal::size()
        .map(|(columns, _)| columns as usize)
        .unwrap_or(80);
    let mut out = stdout();
    if color {
        writeln!(out)?;
    }
    for line in painted(
        usage,
        flights,
        flight_limit,
        true_count,
        total,
        elapsed,
        trace,
        width,
    ) {
        write_line(&mut out, &line, color)?;
    }
    out.flush()
}

fn painted(
    usage: &Usage,
    flights: &[Flight],
    flight_limit: Option<usize>,
    true_count: usize,
    total: usize,
    elapsed: Duration,
    trace: &Path,
    width: usize,
) -> Vec<Painted> {
    let mut lines = vec![
        rule(width),
        plain(format!("trace available at {}", trace.display())),
        throughput_line(usage, flights, flight_limit),
    ];
    if let Some(text) = latency_line(usage, flights) {
        lines.push(plain(text));
    }
    lines.push(plain(utilization_line(usage)));
    lines.push(verdict(usage, true_count, total, elapsed));
    lines
}

fn plain(text: String) -> Painted {
    Painted {
        text,
        paint: Paint::Plain,
        bold: false,
    }
}

fn rule(width: usize) -> Painted {
    Painted {
        text: "─".repeat(width.max(20)),
        paint: Paint::Dim,
        bold: false,
    }
}

/// One column for the throughput and latency charts. `utilization` is longer
/// than that column, so it keeps a single space before `batch`.
fn summary_label(label: &str) -> String {
    format!("{label:<10} ")
}

fn throughput_line(usage: &Usage, flights: &[Flight], flight_limit: Option<usize>) -> Painted {
    let label = summary_label("throughput");
    let Some((spark, avg, limit, paint)) = inflight(flights, flight_limit) else {
        return plain(format!("{label}{} requests", usage.requests));
    };
    Painted {
        text: format!(
            "{label}{spark}  {} requests averaging {avg:.1}/{limit}",
            usage.requests
        ),
        paint,
        bold: false,
    }
}

fn inflight(
    flights: &[Flight],
    flight_limit: Option<usize>,
) -> Option<(String, f64, usize, Paint)> {
    let limit = flight_limit.filter(|limit| *limit > 0)?;
    let windows = flight_windows(flights);
    if windows.is_empty() {
        return None;
    }
    let spark: String = occupancy(&windows, FLIGHT_BUCKETS)
        .into_iter()
        .map(|level| flight_char(level / limit as f64))
        .collect();
    let avg = mean_inflight(&windows);
    let frac = avg / limit as f64;
    let paint = if frac >= 0.85 {
        Paint::Green
    } else if frac >= 0.45 {
        Paint::Yellow
    } else {
        Paint::Dim
    };
    Some((spark, avg, limit, paint))
}

fn latency_line(usage: &Usage, flights: &[Flight]) -> Option<String> {
    let samples: Vec<Duration> = flights
        .iter()
        .filter_map(|flight| flight.ended.duration_since(flight.started).ok())
        .collect();
    if samples.is_empty() {
        return None;
    }
    let mut text = format!("{}{}", summary_label("latency"), hist_spark(&samples));
    if let Some(latency) = usage.latency {
        text.push_str(&format!(
            "  p50 {}  p95 {}  p99 {}",
            super::format_duration(latency.p50),
            super::format_duration(latency.p95),
            super::format_duration(latency.p99),
        ));
    }
    Some(text)
}

fn utilization_line(usage: &Usage) -> String {
    let batch = match usage.batch_size {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    let fill = match usage.fill_factor {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    format!("{}batch={batch} fill={fill}", summary_label("utilization"))
}

fn format_tokens(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        let millions = tokens as f64 / 1_000_000.0;
        if tokens % 1_000_000 == 0 {
            format!("{millions:.0}m")
        } else {
            format!("{millions:.1}m")
        }
    } else if tokens >= 1_000 {
        format!("{}k", tokens / 1_000)
    } else {
        tokens.to_string()
    }
}

fn verdict(usage: &Usage, true_count: usize, total: usize, elapsed: Duration) -> Painted {
    let text = format!(
        "{true_count}/{total} claims validated in {} for {} ({} tokens)",
        super::format_duration(elapsed),
        super::format_usd(usage.spend_usd),
        format_tokens(usage.input_tokens),
    );
    let paint = if total > 0 && true_count == total {
        Paint::Green
    } else if total > 0 {
        Paint::Red
    } else {
        Paint::Plain
    };
    Painted {
        text,
        paint,
        bold: total > 0,
    }
}

fn flight_windows(flights: &[Flight]) -> Vec<(u128, u128)> {
    flights
        .iter()
        .filter_map(|flight| {
            let started = flight.started.duration_since(UNIX_EPOCH).ok()?.as_nanos();
            let ended = flight.ended.duration_since(UNIX_EPOCH).ok()?.as_nanos();
            (ended >= started).then_some((started, ended))
        })
        .collect()
}

fn occupancy(flights: &[(u128, u128)], buckets: usize) -> Vec<f64> {
    if buckets == 0 || flights.is_empty() {
        return Vec::new();
    }
    let t0 = flights.iter().map(|flight| flight.0).min().unwrap();
    let t1 = flights.iter().map(|flight| flight.1).max().unwrap();
    if t1 <= t0 {
        return vec![flights.len() as f64; buckets];
    }
    let span = t1 - t0;
    (0..buckets)
        .map(|bucket| {
            let start = t0 + span * bucket as u128 / buckets as u128;
            let mut end = t0 + span * (bucket + 1) as u128 / buckets as u128;
            if end <= start {
                end = start + 1;
            }
            let area: u128 = flights
                .iter()
                .map(|(flight_start, flight_end)| {
                    let lo = (*flight_start).max(start);
                    let hi = (*flight_end).min(end);
                    hi.saturating_sub(lo)
                })
                .sum();
            area as f64 / (end - start) as f64
        })
        .collect()
}

fn mean_inflight(flights: &[(u128, u128)]) -> f64 {
    let t0 = flights.iter().map(|flight| flight.0).min().unwrap();
    let t1 = flights.iter().map(|flight| flight.1).max().unwrap();
    if t1 <= t0 {
        return flights.len() as f64;
    }
    let area: u128 = flights
        .iter()
        .map(|(start, end)| end.saturating_sub(*start))
        .sum();
    area as f64 / (t1 - t0) as f64
}

fn flight_char(frac: f64) -> char {
    if !frac.is_finite() || frac <= 0.0 {
        return FLIGHT_BLOCKS[0];
    }
    let last = FLIGHT_BLOCKS.len() - 1;
    let index = (frac.clamp(0.0, 1.0) * last as f64).round() as usize;
    FLIGHT_BLOCKS[index.min(last)]
}

fn hist_spark(samples: &[Duration]) -> String {
    let nanos: Vec<u128> = samples.iter().map(|sample| sample.as_nanos()).collect();
    let counts = histogram(&nanos);
    let max = counts.iter().copied().max().unwrap_or(0);
    counts
        .into_iter()
        .map(|count| {
            if count == 0 || max == 0 {
                '·'
            } else {
                let index = (count * (HIST_BLOCKS.len() - 1)) / max;
                HIST_BLOCKS[index]
            }
        })
        .collect()
}

fn histogram(samples: &[u128]) -> Vec<usize> {
    let min = *samples.iter().min().unwrap();
    let max = *samples.iter().max().unwrap();
    if samples.len() == 1 || max <= min {
        return vec![samples.len()];
    }
    let bins = samples.len().clamp(1, HIST_BINS);
    let mut counts = vec![0usize; bins];
    let span = max - min;
    for sample in samples {
        let mut index = ((sample - min) * bins as u128 / span) as usize;
        if index >= bins {
            index = bins - 1;
        }
        counts[index] += 1;
    }
    counts
}

fn paint_color(paint: Paint) -> Color {
    match paint {
        Paint::Plain => Color::Reset,
        Paint::Green => Color::Green,
        Paint::Yellow => Color::Yellow,
        Paint::Red => Color::Red,
        Paint::Dim => Color::DarkGrey,
    }
}

fn write_line(out: &mut impl Write, line: &Painted, color: bool) -> io::Result<()> {
    if color && line.paint != Paint::Plain {
        if line.bold {
            queue!(out, SetAttribute(Attribute::Bold))?;
        }
        queue!(
            out,
            SetForegroundColor(paint_color(line.paint)),
            Print(&line.text),
            ResetColor,
            SetAttribute(Attribute::Reset),
        )?;
        writeln!(out)
    } else {
        writeln!(out, "{}", line.text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oracle::LatencyPercentiles;
    use std::time::UNIX_EPOCH;

    fn texts(
        usage: &Usage,
        flights: &[Flight],
        flight_limit: Option<usize>,
        true_count: usize,
        total: usize,
        elapsed: Duration,
    ) -> Vec<String> {
        painted(
            usage,
            flights,
            flight_limit,
            true_count,
            total,
            elapsed,
            Path::new("/tmp/trace.jsonl"),
            40,
        )
        .into_iter()
        .map(|line| line.text)
        .collect()
    }

    fn flight(start_ms: u64, end_ms: u64) -> Flight {
        Flight {
            started: UNIX_EPOCH + Duration::from_millis(start_ms),
            ended: UNIX_EPOCH + Duration::from_millis(end_ms),
        }
    }

    #[test]
    fn stats_omit_percentiles_and_keep_batch_and_fill() {
        let lines = texts(
            &Usage {
                spend_usd: 0.01,
                input_tokens: 307_367,
                requests: 111,
                batch_size: Some(0.05),
                fill_factor: Some(0.087),
                ..Default::default()
            },
            &[],
            None,
            2,
            4,
            Duration::from_millis(5_910),
        );
        assert_eq!(
            lines,
            vec![
                "─".repeat(40),
                "trace available at /tmp/trace.jsonl".to_string(),
                "throughput 111 requests".to_string(),
                "utilization batch=5.0% fill=8.7%".to_string(),
                "2/4 claims validated in 5.91s for $0.01 (307k tokens)".to_string(),
            ]
        );
        assert!(lines.iter().all(|line| !line.contains("p50")));
    }

    #[test]
    fn full_window_is_a_solid_bar() {
        let flights = vec![flight(0, 1_000); 16];
        let lines = texts(
            &Usage::default(),
            &flights,
            Some(16),
            4,
            4,
            Duration::from_secs(6),
        );
        let throughput = lines
            .iter()
            .find(|line| line.starts_with("throughput"))
            .unwrap();
        assert!(throughput.contains(&"█".repeat(FLIGHT_BUCKETS)));
        assert!(
            throughput.contains("0 requests averaging 16.0/16"),
            "{throughput}"
        );
        assert!(lines.iter().any(|line| line.starts_with("latency")));
    }

    #[test]
    fn half_window_stays_below_the_cap() {
        let flights = vec![flight(0, 1_000); 8];
        let lines = texts(
            &Usage::default(),
            &flights,
            Some(16),
            1,
            2,
            Duration::from_secs(1),
        );
        let throughput = lines
            .iter()
            .find(|line| line.starts_with("throughput"))
            .unwrap();
        assert!(throughput.contains('▄'), "{throughput}");
        assert!(!throughput.contains('█'), "{throughput}");
        assert!(throughput.contains("averaging 8.0/16"), "{throughput}");
    }

    #[test]
    fn latency_histogram_carries_percentiles() {
        let flights = vec![
            flight(0, 400),
            flight(0, 663),
            flight(0, 700),
            flight(0, 965),
            flight(0, 1_100),
        ];
        let lines = texts(
            &Usage {
                latency: Some(LatencyPercentiles {
                    p50: Duration::from_millis(663),
                    p95: Duration::from_millis(965),
                    p99: Duration::from_millis(1_100),
                }),
                ..Default::default()
            },
            &flights,
            Some(16),
            3,
            4,
            Duration::from_millis(1_420),
        );
        let latency = lines
            .iter()
            .find(|line| line.starts_with("latency"))
            .unwrap();
        assert!(latency.contains('█') || latency.contains('▇'), "{latency}");
        assert_eq!(&latency[..11], "latency    ", "{latency}");
        assert!(latency.contains("p50 663ms"), "{latency}");
        assert!(latency.contains("p95 965ms"), "{latency}");
        assert!(latency.contains("p99 1.10s"), "{latency}");
        assert!(!latency.contains("p50="), "{latency}");
        let throughput = lines
            .iter()
            .find(|line| line.starts_with("throughput"))
            .unwrap();
        assert_eq!(&throughput[..11], "throughput ");
    }

    #[test]
    fn no_flights_skips_the_charts() {
        let lines = texts(&Usage::default(), &[], None, 0, 0, Duration::from_millis(4));
        assert!(lines.iter().all(|line| !line.starts_with("latency")));
        assert_eq!(
            lines,
            vec![
                "─".repeat(40),
                "trace available at /tmp/trace.jsonl".to_string(),
                "throughput 0 requests".to_string(),
                "utilization batch=none fill=none".to_string(),
                "0/0 claims validated in 4ms for $0.000000 (0 tokens)".to_string(),
            ]
        );
    }

    #[test]
    fn token_counts_collapse_past_a_thousand() {
        assert_eq!(format_tokens(0), "0");
        assert_eq!(format_tokens(999), "999");
        assert_eq!(format_tokens(405_947), "405k");
        assert_eq!(format_tokens(2_000_000), "2m");
        assert_eq!(format_tokens(1_500_000), "1.5m");
    }
}
