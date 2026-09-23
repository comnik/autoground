use crate::record::HistoryRun;

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 520.0;
const LEFT: f64 = 76.0;
const TOP: f64 = 48.0;
const PLOT_W: f64 = 600.0;
const PLOT_H: f64 = 400.0;

const COLORS: [&str; 6] = [
    "#1f4e79", "#c47b2b", "#2f6f4e", "#7a3e6d", "#9a3b3b", "#3d6b8a",
];

pub fn render(runs: &[HistoryRun]) -> String {
    let runs = shown(runs);
    let bounds = log_bounds(&positive_costs(runs));
    let mut body = String::new();
    body.push_str(&format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">"##
    ));
    body.push_str(r##"<rect width="100%" height="100%" fill="#ffffff"/>"##);
    body.push_str(
        r##"<text x="360" y="28" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="16">accuracy vs cost</text>"##,
    );
    for step in 0..=4 {
        let fraction = step as f64 / 4.0;
        let y = TOP + (1.0 - fraction) * PLOT_H;
        body.push_str(&format!(
            r##"<line x1="{LEFT}" y1="{y:.1}" x2="{}" y2="{y:.1}" stroke="#e6e6e6"/>"##,
            LEFT + PLOT_W
        ));
        let label = (fraction * 100.0).round() as i64;
        body.push_str(&format!(
            r##"<text x="{}" y="{y:.1}" text-anchor="end" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#444">{label}</text>"##,
            LEFT - 8.0
        ));
    }
    if let Some((lo, hi)) = bounds {
        for tick in ticks(lo, hi) {
            let x = x_of(tick, bounds);
            body.push_str(&format!(
                r##"<line x1="{x:.1}" y1="{TOP}" x2="{x:.1}" y2="{}" stroke="#e6e6e6"/>"##,
                TOP + PLOT_H
            ));
            body.push_str(&format!(
                r##"<text x="{x:.1}" y="{}" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#444">{}</text>"##,
                TOP + PLOT_H + 18.0,
                fmt_tick(tick)
            ));
        }
    }
    body.push_str(&format!(
        r##"<text x="{}" y="{}" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="12">cost (USD, log)</text>"##,
        LEFT + PLOT_W / 2.0,
        TOP + PLOT_H + 36.0
    ));
    body.push_str(&format!(
        r##"<text x="16" y="{}" transform="rotate(-90 16 {})" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="12">accuracy %</text>"##,
        TOP + PLOT_H / 2.0,
        TOP + PLOT_H / 2.0
    ));

    let mut legend = 0usize;
    if let Some(cost) = baseline_cost(runs) {
        let y = y_of(1.0);
        let x = x_of(cost, bounds);
        body.push_str(&format!(
            r##"<circle cx="{x:.1}" cy="{y:.1}" r="5" fill="#666"/>"##
        ));
        let label_y = TOP + 14.0 + legend as f64 * 14.0;
        legend += 1;
        let partial = if baseline_partial(runs) {
            " partial"
        } else {
            ""
        };
        body.push_str(&format!(
            r##"<text x="{}" y="{label_y:.1}" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#666">baseline{partial}</text>"##,
            LEFT + 8.0
        ));
    }

    for (index, run) in runs.iter().enumerate() {
        let Some(accuracy) = run.suite.accuracy else {
            continue;
        };
        let color = COLORS[index % COLORS.len()];
        let (x, y) = (x_of(run.read_spend_usd, bounds), y_of(accuracy));
        body.push_str(&format!(
            r##"<circle cx="{x:.1}" cy="{y:.1}" r="4" fill="{color}"/>"##
        ));
        let sha = short_sha(&run.git_sha);
        let dirty = if run.git_dirty { "*" } else { "" };
        let mode = if run.batched { " batch" } else { "" };
        let label_y = TOP + 14.0 + legend as f64 * 14.0;
        legend += 1;
        body.push_str(&format!(
            r##"<text x="{}" y="{label_y:.1}" font-family="ui-sans-serif, sans-serif" font-size="11" fill="{color}">{sha}{dirty}{mode}</text>"##,
            LEFT + 8.0
        ));
    }
    body.push_str("</svg>\n");
    body
}

/// The latest score writes the individual pass and then the batch pass.
/// Older scores stay in history and drop off the chart.
fn shown(runs: &[HistoryRun]) -> &[HistoryRun] {
    let Some(last) = runs.last() else {
        return runs;
    };
    if last.batched {
        if let Some(prev) = runs.len().checked_sub(2) {
            if !runs[prev].batched {
                return &runs[prev..];
            }
        }
    }
    &runs[runs.len() - 1..]
}

fn baseline_cost(runs: &[HistoryRun]) -> Option<f64> {
    runs.iter().rev().find_map(|run| run.baseline_estimated_usd)
}

fn baseline_partial(runs: &[HistoryRun]) -> bool {
    runs.iter()
        .rev()
        .find(|run| run.baseline_estimated_usd.is_some())
        .is_some_and(|run| run.baseline_estimate_partial)
}

fn positive_costs(runs: &[HistoryRun]) -> Vec<f64> {
    let mut costs = Vec::new();
    if let Some(cost) = baseline_cost(runs) {
        if cost > 0.0 {
            costs.push(cost);
        }
    }
    for run in runs {
        if run.suite.accuracy.is_some() && run.read_spend_usd > 0.0 {
            costs.push(run.read_spend_usd);
        }
    }
    costs
}

fn log_bounds(costs: &[f64]) -> Option<(f64, f64)> {
    let mut logs: Vec<f64> = costs
        .iter()
        .copied()
        .filter(|cost| *cost > 0.0 && cost.is_finite())
        .map(f64::log10)
        .collect();
    if logs.is_empty() {
        return None;
    }
    logs.sort_by(|left, right| left.partial_cmp(right).unwrap_or(std::cmp::Ordering::Equal));
    let mut lo = logs[0];
    let mut hi = *logs.last().unwrap();
    if hi - lo < 1e-6 {
        lo -= 0.5;
        hi += 0.5;
    } else {
        let pad = (hi - lo) * 0.08;
        lo -= pad;
        hi += pad;
    }
    Some((lo, hi))
}

fn ticks(lo: f64, hi: f64) -> Vec<f64> {
    let mut ticks = Vec::new();
    let start = lo.floor() as i32;
    let end = hi.ceil() as i32;
    for exp in start..=end {
        let log = exp as f64;
        if log + 1e-9 >= lo && log - 1e-9 <= hi {
            ticks.push(10f64.powi(exp));
        }
    }
    ticks
}

fn x_of(cost: f64, bounds: Option<(f64, f64)>) -> f64 {
    let Some((lo, hi)) = bounds else {
        return LEFT;
    };
    if cost <= 0.0 || hi <= lo {
        return LEFT;
    }
    let t = ((cost.log10() - lo) / (hi - lo)).clamp(0.0, 1.0);
    LEFT + t * PLOT_W
}

fn y_of(accuracy: f64) -> f64 {
    TOP + (1.0 - accuracy.clamp(0.0, 1.0)) * PLOT_H
}

fn fmt_tick(value: f64) -> String {
    let exp = value.log10().round() as i32;
    let pow = 10f64.powi(exp);
    if exp >= 0 {
        format!("${pow:.0}")
    } else {
        format!("${pow:.prec$}", prec = (-exp) as usize)
    }
}

fn short_sha(sha: &str) -> &str {
    let end = sha.len().min(7);
    &sha[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::SuiteSummary;

    #[test]
    fn the_latest_score_is_accuracy_against_cost() {
        let older = sample("oldsha111", false, 0.2, 0.01);
        let each = sample("newsha222", false, 0.5, 0.02);
        let batch = sample("newsha222", true, 0.5, 0.015);
        let svg = render(&[older, each, batch]);
        assert!(svg.contains("accuracy"), "{svg}");
        assert!(svg.contains("cost"), "{svg}");
        assert!(svg.contains("baseline"), "{svg}");
        assert!(svg.contains("batch"), "{svg}");
        assert!(svg.contains("newsha2"), "{svg}");
        assert!(!svg.contains("oldsha1"), "{svg}");
        assert_eq!(svg.matches("<circle ").count(), 3, "{svg}");
    }

    #[test]
    fn a_zero_cost_sits_on_the_left_edge() {
        let run = sample("abcdef123456", false, 0.5, 0.0);
        let svg = render(&[run]);
        assert!(svg.contains(&format!("cx=\"{LEFT:.1}\"")), "{svg}");
    }

    fn sample(sha: &str, batched: bool, accuracy: f64, spend: f64) -> HistoryRun {
        HistoryRun {
            git_sha: sha.to_string(),
            git_dirty: false,
            scored_at_ms: 0,
            judge: "jev".to_string(),
            batched,
            read_spend_usd: spend,
            read_duration_ms: 0,
            baseline_estimated_usd: Some(1.9),
            baseline_estimate_partial: false,
            baseline_duration_ms: 0,
            suite: SuiteSummary {
                accuracy: Some(accuracy),
                scored: 1,
                failed: 0,
            },
            cases: Vec::new(),
        }
    }
}
