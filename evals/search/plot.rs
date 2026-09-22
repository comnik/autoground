use crate::record::HistoryRun;

const WIDTH: f64 = 720.0;
const HEIGHT: f64 = 520.0;
const LEFT: f64 = 64.0;
const TOP: f64 = 48.0;
const PLOT_W: f64 = 600.0;
const PLOT_H: f64 = 400.0;

const COLORS: [&str; 6] = [
    "#1f4e79", "#c47b2b", "#2f6f4e", "#7a3e6d", "#9a3b3b", "#3d6b8a",
];

pub fn render(runs: &[HistoryRun]) -> String {
    let mut body = String::new();
    body.push_str(&format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{HEIGHT}" viewBox="0 0 {WIDTH} {HEIGHT}">"##
    ));
    body.push_str(r##"<rect width="100%" height="100%" fill="#ffffff"/>"##);
    body.push_str(
        r##"<text x="360" y="28" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="16">universe pruned vs evidence selected</text>"##,
    );
    for step in 0..=4 {
        let fraction = step as f64 / 4.0;
        let x = LEFT + fraction * PLOT_W;
        let y = TOP + (1.0 - fraction) * PLOT_H;
        body.push_str(&format!(
            r##"<line x1="{x:.1}" y1="{TOP}" x2="{x:.1}" y2="{}" stroke="#e6e6e6"/>"##,
            TOP + PLOT_H
        ));
        body.push_str(&format!(
            r##"<line x1="{LEFT}" y1="{y:.1}" x2="{}" y2="{y:.1}" stroke="#e6e6e6"/>"##,
            LEFT + PLOT_W
        ));
        let label = (fraction * 100.0).round() as i64;
        body.push_str(&format!(
            r##"<text x="{x:.1}" y="{}" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#444">{label}</text>"##,
            TOP + PLOT_H + 18.0
        ));
        body.push_str(&format!(
            r##"<text x="{}" y="{y:.1}" text-anchor="end" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#444">{label}</text>"##,
            LEFT - 8.0
        ));
    }
    body.push_str(&format!(
        r##"<text x="{}" y="{}" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="12">pruned %</text>"##,
        LEFT + PLOT_W / 2.0,
        TOP + PLOT_H + 36.0
    ));
    body.push_str(&format!(
        r##"<text x="16" y="{}" transform="rotate(-90 16 {})" text-anchor="middle" font-family="ui-sans-serif, sans-serif" font-size="12">evidence %</text>"##,
        TOP + PLOT_H / 2.0,
        TOP + PLOT_H / 2.0
    ));

    if let Some(ideal) = runs.iter().rev().find_map(|run| run.suite.ideal_pruned) {
        let (x, _) = xy(ideal, 1.0);
        body.push_str(&format!(
            r##"<line x1="{x:.1}" y1="{TOP}" x2="{x:.1}" y2="{}" stroke="#666" stroke-dasharray="4 3" stroke-width="1.2"/>"##,
            TOP + PLOT_H
        ));
        let anchor = if ideal > 0.75 { "end" } else { "start" };
        let label_x = if ideal > 0.75 { x - 6.0 } else { x + 6.0 };
        body.push_str(&format!(
            r##"<text x="{label_x:.1}" y="{}" text-anchor="{anchor}" font-family="ui-sans-serif, sans-serif" font-size="11" fill="#666">evidence only</text>"##,
            TOP + 14.0
        ));
    }

    let last = runs.len().saturating_sub(1);
    for (index, run) in runs.iter().enumerate() {
        let color = COLORS[index % COLORS.len()];
        let mut points = Vec::new();
        for point in &run.curve {
            if let (Some(pruned), Some(evidence)) = (point.mean_pruned, point.mean_evidence) {
                points.push(xy(pruned, evidence));
            }
        }
        if points.len() >= 2 {
            let width = if index == last { 2.4 } else { 1.2 };
            let opacity = if index == last { 1.0 } else { 0.45 };
            let rendered = points
                .iter()
                .map(|(x, y)| format!("{x:.1},{y:.1}"))
                .collect::<Vec<_>>()
                .join(" ");
            body.push_str(&format!(
                r##"<polyline fill="none" stroke="{color}" stroke-width="{width}" opacity="{opacity}" points="{rendered}"/>"##
            ));
        }
        if let Some(mark) = run
            .curve
            .iter()
            .find(|point| (point.floor - 0.85).abs() < 1e-9)
        {
            if let (Some(pruned), Some(evidence)) = (mark.mean_pruned, mark.mean_evidence) {
                let (x, y) = xy(pruned, evidence);
                let radius = if index == last { 4.5 } else { 3.0 };
                body.push_str(&format!(
                    r##"<circle cx="{x:.1}" cy="{y:.1}" r="{radius}" fill="{color}"/>"##
                ));
            }
        }
        let sha = short_sha(&run.git_sha);
        let dirty = if run.git_dirty { "*" } else { "" };
        let y = TOP + 14.0 + index as f64 * 14.0;
        body.push_str(&format!(
            r##"<text x="{}" y="{y:.1}" font-family="ui-sans-serif, sans-serif" font-size="11" fill="{color}">{sha}{dirty}</text>"##,
            LEFT + 8.0
        ));
    }
    body.push_str("</svg>\n");
    body
}

fn xy(pruned: f64, evidence: f64) -> (f64, f64) {
    let x = LEFT + pruned.clamp(0.0, 1.0) * PLOT_W;
    let y = TOP + (1.0 - evidence.clamp(0.0, 1.0)) * PLOT_H;
    (x, y)
}

fn short_sha(sha: &str) -> &str {
    let end = sha.len().min(7);
    &sha[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::record::{CurvePoint, HistoryRun, SuiteSummary};

    #[test]
    fn the_operating_point_is_marked() {
        let run = HistoryRun {
            git_sha: "abcdef123456".to_string(),
            git_dirty: false,
            scored_at_ms: 0,
            judge: "jev".to_string(),
            search_spend_usd: 0.0,
            baseline_estimated_usd: None,
            baseline_estimate_partial: false,
            suite: SuiteSummary {
                floor: 0.85,
                mean_pruned: Some(0.4),
                mean_evidence: Some(0.8),
                micro_evidence: Some(0.8),
                scored: 1,
                unscored: 0,
                failed: 0,
                ideal_pruned: Some(0.9),
            },
            curve: vec![CurvePoint {
                floor: 0.85,
                mean_pruned: Some(0.4),
                mean_evidence: Some(0.8),
            }],
            cases: Vec::new(),
        };
        let svg = render(&[run]);
        assert!(svg.contains("<circle "), "{svg}");
        assert!(svg.contains("abcdef1"), "{svg}");
        assert!(svg.contains("stroke-dasharray"), "{svg}");
        assert!(svg.contains("evidence only"), "{svg}");
    }
}
