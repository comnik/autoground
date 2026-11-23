use std::fmt::Write;

use crate::metrics::{ComparisonReport, RunReport};

pub fn render_run(report: &RunReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "strategy: {}  ({}/{} = {:.1}%, repeats={}, {}ms, ${:.5})",
        report.strategy,
        report.correct,
        report.total,
        report.accuracy * 100.0,
        report.repeats,
        report.elapsed_ms,
        report.usage.spend_usd,
    );
    let _ = writeln!(
        out,
        "errors:   soundness={}  completeness={}  neutral->contradicted={}  contradicted->neutral={}",
        report.soundness_errors,
        report.completeness_errors,
        report.neutral_as_contradicted,
        report.contradicted_as_neutral,
    );
    let auc = report
        .entailed_auc
        .map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "-".into());
    let margin = report
        .entailed_margin
        .map(|v| format!("{v:+.3}"))
        .unwrap_or_else(|| "-".into());
    let brier = report
        .brier_score
        .map(|v| format!("{v:.3}"))
        .unwrap_or_else(|| "-".into());
    let _ = writeln!(
        out,
        "scores:   entailed_auc={auc}  margin(p10-p90)={margin}  brier={brier}"
    );

    let m = &report.metamorphic;
    let mut meta_parts = Vec::new();
    if m.negation_pairs > 0 {
        meta_parts.push(format!(
            "negation={}/{}",
            m.negation_symmetric, m.negation_pairs
        ));
    }
    if m.paraphrase_pairs > 0 {
        meta_parts.push(format!(
            "paraphrase={}/{}",
            m.paraphrase_agreed, m.paraphrase_pairs
        ));
    }
    if report.repeats > 1 {
        meta_parts.push(format!("repeat_stable={}/{}", m.repeat_stable, m.repeat_total));
    }
    if !meta_parts.is_empty() {
        let _ = writeln!(out, "invar:    {}", meta_parts.join("  "));
    }

    let c = &report.confusion.counts;
    let _ = writeln!(out, "\nconfusion (rows=expected, cols=predicted):");
    let _ = writeln!(out, "  {:14} {:>8} {:>12} {:>8}", "", "entailed", "contradicted", "neutral");
    let _ = writeln!(out, "  {:14} {:>8} {:>12} {:>8}", "entailed", c[0][0], c[0][1], c[0][2]);
    let _ = writeln!(out, "  {:14} {:>8} {:>12} {:>8}", "contradicted", c[1][0], c[1][1], c[1][2]);
    let _ = writeln!(out, "  {:14} {:>8} {:>12} {:>8}", "neutral", c[2][0], c[2][1], c[2][2]);

    if !report.by_kind.is_empty() {
        let _ = writeln!(out, "\nby kind:");
        for (kind, s) in &report.by_kind {
            let _ = writeln!(
                out,
                "  {:12} {}/{} ({:5.1}%)  sound_err={}  comp_err={}",
                kind,
                s.correct,
                s.total,
                s.accuracy * 100.0,
                s.soundness_errors,
                s.completeness_errors
            );
        }
    }

    if !report.by_tag.is_empty() {
        let _ = writeln!(out, "\nby tag:");
        for (tag, s) in &report.by_tag {
            let _ = writeln!(
                out,
                "  {:22} {}/{} ({:5.1}%)  sound_err={}  comp_err={}",
                tag,
                s.correct,
                s.total,
                s.accuracy * 100.0,
                s.soundness_errors,
                s.completeness_errors
            );
        }
    }

    if !report.threshold_sweep.is_empty() {
        let _ = writeln!(out, "\nthreshold sweep:");
        for pt in &report.threshold_sweep {
            let _ = writeln!(
                out,
                "  bar={:.2}  acc={:5.1}%  sound_err={}  comp_err={}",
                pt.threshold,
                pt.accuracy * 100.0,
                pt.soundness_errors,
                pt.completeness_errors
            );
        }
    }

    let failures: Vec<_> = report.outcomes.iter().filter(|o| !o.correct).collect();
    if !failures.is_empty() {
        let _ = writeln!(out, "\nmismatches ({}):", failures.len());
        for f in failures {
            let _ = writeln!(
                out,
                "  {:<28} [{:<11}] exp={:<12} got={:<12} (raw={:<12} conf={:.2} score={:.2})",
                f.id,
                f.kind.as_str(),
                f.expected.as_str(),
                f.predicted.as_str(),
                f.raw_choice.as_str(),
                f.confidence,
                f.entailed_score
            );
        }
    }

    let _ = writeln!(
        out,
        "\nusage: requests={}  tokens={}  fill={:.1}%  batch={:.1}%  p50={}ms  p95={}ms  spend=${:.5}",
        report.usage.requests,
        report.usage.input_tokens,
        report.usage.fill_factor.unwrap_or(0.0) * 100.0,
        report.usage.batch_size.unwrap_or(0.0) * 100.0,
        report.usage.latency_p50_ms.unwrap_or(0),
        report.usage.latency_p95_ms.unwrap_or(0),
        report.usage.spend_usd,
    );
    let _ = writeln!(out, "trace: {}", report.trace_path);
    out
}

pub fn render_comparison(comp: &ComparisonReport) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "A/B: {} (baseline) vs {} (candidate)\n",
        comp.baseline.strategy, comp.candidate.strategy
    );
    let _ = writeln!(
        out,
        "  {:22} {:>12} {:>12} {:>10}",
        "metric", "baseline", "candidate", "delta"
    );
    let _ = writeln!(
        out,
        "  {:22} {:>11.1}% {:>11.1}% {:>+9.1}%",
        "accuracy",
        comp.baseline.accuracy * 100.0,
        comp.candidate.accuracy * 100.0,
        comp.accuracy_delta * 100.0
    );
    let _ = writeln!(
        out,
        "  {:22} {:>12} {:>12} {:>+10}",
        "soundness_errors",
        comp.baseline.soundness_errors,
        comp.candidate.soundness_errors,
        comp.soundness_delta
    );
    let _ = writeln!(
        out,
        "  {:22} {:>12} {:>12} {:>+10}",
        "completeness_errors",
        comp.baseline.completeness_errors,
        comp.candidate.completeness_errors,
        comp.completeness_delta
    );
    if let (Some(b), Some(c), Some(d)) = (
        comp.baseline.entailed_auc,
        comp.candidate.entailed_auc,
        comp.auc_delta,
    ) {
        let _ = writeln!(
            out,
            "  {:22} {:>12.3} {:>12.3} {:>+10.3}",
            "entailed_auc", b, c, d
        );
    }
    if let (Some(b), Some(c), Some(d)) = (
        comp.baseline.entailed_margin,
        comp.candidate.entailed_margin,
        comp.margin_delta,
    ) {
        let _ = writeln!(
            out,
            "  {:22} {:>+12.3} {:>+12.3} {:>+10.3}",
            "entailed_margin", b, c, d
        );
    }
    if let (Some(b), Some(c), Some(d)) = (
        comp.baseline.brier_score,
        comp.candidate.brier_score,
        comp.brier_delta,
    ) {
        let _ = writeln!(
            out,
            "  {:22} {:>12.3} {:>12.3} {:>+10.3}",
            "brier_score", b, c, d
        );
    }
    let _ = writeln!(
        out,
        "  {:22} {:>12} {:>12} {:>+10}",
        "input_tokens",
        comp.baseline.usage.input_tokens,
        comp.candidate.usage.input_tokens,
        comp.candidate.usage.input_tokens as isize - comp.baseline.usage.input_tokens as isize
    );
    let _ = writeln!(
        out,
        "  {:22} {:>11.5}$ {:>11.5}$",
        "spend_usd", comp.baseline.usage.spend_usd, comp.candidate.usage.spend_usd
    );

    if !comp.fixes.is_empty() {
        let _ = writeln!(out, "\nfixes ({}):", comp.fixes.len());
        for f in &comp.fixes {
            let _ = writeln!(
                out,
                "  + {:<28} exp={:<12} {} -> {}  (score {:.2} -> {:.2})",
                f.id,
                f.expected.as_str(),
                f.baseline.as_str(),
                f.candidate.as_str(),
                f.baseline_score,
                f.candidate_score
            );
        }
    }

    if !comp.regressions.is_empty() {
        let _ = writeln!(out, "\nregressions ({}):", comp.regressions.len());
        for r in &comp.regressions {
            let _ = writeln!(
                out,
                "  - {:<28} exp={:<12} {} -> {}  (score {:.2} -> {:.2})",
                r.id,
                r.expected.as_str(),
                r.baseline.as_str(),
                r.candidate.as_str(),
                r.baseline_score,
                r.candidate_score
            );
        }
    }

    if !comp.largest_shifts.is_empty() {
        let _ = writeln!(out, "\nlargest entailed-score shifts:");
        for s in &comp.largest_shifts {
            let _ = writeln!(
                out,
                "  {:<28} exp={:<12} {:.3} -> {:.3} ({:+.3})",
                s.id,
                s.expected.as_str(),
                s.baseline_score,
                s.candidate_score,
                s.delta
            );
        }
    }

    let _ = writeln!(out, "\nbaseline trace:  {}", comp.baseline.trace_path);
    let _ = writeln!(out, "candidate trace: {}", comp.candidate.trace_path);
    out
}
