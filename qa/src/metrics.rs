use std::collections::{BTreeMap, HashMap};

use autoground::check::strategy::{Verdict, VerdictKind};
use autoground::oracle::UsageRecord;
use serde::Serialize;

use crate::suite::{Probe, ProbeKind};

#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutcome {
    pub id: String,
    pub group_id: String,
    pub kind: ProbeKind,
    pub expected: VerdictKind,
    pub predicted: VerdictKind,
    pub raw_choice: VerdictKind,
    pub confidence: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<BTreeMap<String, f64>>,
    pub correct: bool,
    pub entailed_score: f64,
    pub tags: Vec<String>,
    pub repeats_consistent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ConfusionMatrix {
    /// Indexed by `[expected][predicted]` with order: entailed=0, contradicted=1, neutral=2.
    pub counts: [[usize; 3]; 3],
}

impl ConfusionMatrix {
    fn idx(k: VerdictKind) -> usize {
        match k {
            VerdictKind::Entailed => 0,
            VerdictKind::Contradicted => 1,
            VerdictKind::Neutral => 2,
        }
    }

    pub fn record(&mut self, expected: VerdictKind, predicted: VerdictKind) {
        self.counts[Self::idx(expected)][Self::idx(predicted)] += 1;
    }

    pub fn total(&self) -> usize {
        self.counts.iter().flatten().sum()
    }

    pub fn correct(&self) -> usize {
        self.counts[0][0] + self.counts[1][1] + self.counts[2][2]
    }

    /// Contradicted or neutral falsely settled as entailed.
    pub fn soundness_errors(&self) -> usize {
        self.counts[1][0] + self.counts[2][0]
    }

    /// Entailed missed as contradicted or neutral.
    pub fn completeness_errors(&self) -> usize {
        self.counts[0][1] + self.counts[0][2]
    }

    /// Neutral falsely marked contradicted (breaks `not` and sliced witnesses).
    pub fn neutral_as_contradicted(&self) -> usize {
        self.counts[2][1]
    }

    /// Contradicted left unsettled as neutral.
    pub fn contradicted_as_neutral(&self) -> usize {
        self.counts[1][2]
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SliceSummary {
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub soundness_errors: usize,
    pub completeness_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThresholdPoint {
    pub threshold: f64,
    pub accuracy: f64,
    pub soundness_errors: usize,
    pub completeness_errors: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct MetamorphicSummary {
    pub negation_pairs: usize,
    pub negation_symmetric: usize,
    pub paraphrase_pairs: usize,
    pub paraphrase_agreed: usize,
    pub repeat_stable: usize,
    pub repeat_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunReport {
    pub strategy: String,
    pub repeats: usize,
    pub total: usize,
    pub correct: usize,
    pub accuracy: f64,
    pub confusion: ConfusionMatrix,
    pub soundness_errors: usize,
    pub completeness_errors: usize,
    pub neutral_as_contradicted: usize,
    pub contradicted_as_neutral: usize,
    pub entailed_auc: Option<f64>,
    pub entailed_margin: Option<f64>,
    pub brier_score: Option<f64>,
    pub threshold_sweep: Vec<ThresholdPoint>,
    pub by_kind: BTreeMap<String, SliceSummary>,
    pub by_tag: BTreeMap<String, SliceSummary>,
    pub metamorphic: MetamorphicSummary,
    pub usage: UsageRecord,
    pub trace_path: String,
    pub elapsed_ms: u64,
    pub outcomes: Vec<ProbeOutcome>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Flip {
    pub id: String,
    pub kind: ProbeKind,
    pub expected: VerdictKind,
    pub baseline: VerdictKind,
    pub candidate: VerdictKind,
    pub baseline_score: f64,
    pub candidate_score: f64,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreShift {
    pub id: String,
    pub expected: VerdictKind,
    pub baseline_score: f64,
    pub candidate_score: f64,
    pub delta: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ComparisonReport {
    pub baseline: RunReport,
    pub candidate: RunReport,
    pub accuracy_delta: f64,
    pub soundness_delta: isize,
    pub completeness_delta: isize,
    pub auc_delta: Option<f64>,
    pub margin_delta: Option<f64>,
    pub brier_delta: Option<f64>,
    pub fixes: Vec<Flip>,
    pub regressions: Vec<Flip>,
    pub largest_shifts: Vec<ScoreShift>,
}

/// Continuous score for `entailed` discrimination, preferring raw `P(entailed)`
/// from Jev's `probabilities` and falling back to confidence when omitted.
pub fn entailed_score(verdict: &Verdict) -> f64 {
    if let Some(probs) = &verdict.probabilities {
        if let Some(&p) = probs.get("entailed") {
            return p;
        }
    }
    match verdict.kind {
        VerdictKind::Entailed => 0.5 + 0.5 * verdict.confidence,
        VerdictKind::Neutral => 0.5 * (1.0 - verdict.confidence),
        VerdictKind::Contradicted => 0.25 * (1.0 - verdict.confidence),
    }
}

pub fn summarize(
    strategy: String,
    repeats: usize,
    probes: &[Probe],
    verdicts_per_probe: &[Vec<Verdict>],
    usage: UsageRecord,
    trace_path: String,
    elapsed_ms: u64,
) -> RunReport {
    let mut confusion = ConfusionMatrix::default();
    let mut outcomes = Vec::with_capacity(probes.len());
    let mut by_kind_raw: BTreeMap<String, ConfusionMatrix> = BTreeMap::new();
    let mut by_tag_raw: BTreeMap<String, ConfusionMatrix> = BTreeMap::new();

    let mut pos_scores = Vec::new();
    let mut neg_scores = Vec::new();
    let mut brier_sum = 0.0;
    let mut brier_n = 0usize;
    let mut repeat_stable = 0usize;

    for (probe, runs) in probes.iter().zip(verdicts_per_probe.iter()) {
        let first = &runs[0];
        let predicted = majority_verdict(runs);
        let consistent = runs.iter().all(|v| v.effective_kind() == predicted);
        if consistent {
            repeat_stable += 1;
        }
        let mean_conf = runs.iter().map(|v| v.confidence).sum::<f64>() / runs.len() as f64;
        let mean_score = runs.iter().map(entailed_score).sum::<f64>() / runs.len() as f64;
        let mean_probs = average_probabilities(runs);

        confusion.record(probe.expected, predicted);
        by_kind_raw
            .entry(probe.state.kind().as_str().to_string())
            .or_default()
            .record(probe.expected, predicted);
        for tag in &probe.tags {
            by_tag_raw
                .entry(tag.clone())
                .or_default()
                .record(probe.expected, predicted);
        }

        if probe.expected == VerdictKind::Entailed {
            pos_scores.push(mean_score);
        } else {
            neg_scores.push(mean_score);
        }

        if let Some(probs) = &mean_probs {
            brier_sum += three_class_brier(probe.expected, probs);
            brier_n += 1;
        }

        outcomes.push(ProbeOutcome {
            id: probe.id.clone(),
            group_id: probe.group_id.clone(),
            kind: probe.state.kind(),
            expected: probe.expected,
            predicted,
            raw_choice: first.kind,
            confidence: mean_conf,
            probabilities: mean_probs,
            correct: predicted == probe.expected,
            entailed_score: mean_score,
            tags: probe.tags.clone(),
            repeats_consistent: consistent,
            notes: probe.notes.clone(),
        });
    }

    let total = confusion.total();
    let correct = confusion.correct();
    let accuracy = ratio(correct, total);

    let by_kind = by_kind_raw
        .into_iter()
        .map(|(k, cm)| (k, slice_summary(&cm)))
        .collect();
    let by_tag = by_tag_raw
        .into_iter()
        .map(|(k, cm)| (k, slice_summary(&cm)))
        .collect();

    let entailed_auc = roc_auc(&pos_scores, &neg_scores);
    let entailed_margin = separation_margin(&pos_scores, &neg_scores);
    let brier_score = (brier_n > 0).then_some(brier_sum / brier_n as f64);
    let threshold_sweep = sweep_thresholds(probes, verdicts_per_probe);
    let metamorphic = compute_metamorphic(probes, &outcomes, repeat_stable);

    RunReport {
        strategy,
        repeats,
        total,
        correct,
        accuracy,
        soundness_errors: confusion.soundness_errors(),
        completeness_errors: confusion.completeness_errors(),
        neutral_as_contradicted: confusion.neutral_as_contradicted(),
        contradicted_as_neutral: confusion.contradicted_as_neutral(),
        confusion,
        entailed_auc,
        entailed_margin,
        brier_score,
        threshold_sweep,
        by_kind,
        by_tag,
        metamorphic,
        usage,
        trace_path,
        elapsed_ms,
        outcomes,
    }
}

pub fn compare(baseline: RunReport, candidate: RunReport) -> ComparisonReport {
    let base_map: HashMap<&str, &ProbeOutcome> = baseline
        .outcomes
        .iter()
        .map(|o| (o.id.as_str(), o))
        .collect();
    let mut fixes = Vec::new();
    let mut regressions = Vec::new();
    let mut shifts = Vec::new();

    for cand in &candidate.outcomes {
        let Some(base) = base_map.get(cand.id.as_str()) else {
            continue;
        };
        if !base.correct && cand.correct {
            fixes.push(Flip {
                id: cand.id.clone(),
                kind: cand.kind,
                expected: cand.expected,
                baseline: base.predicted,
                candidate: cand.predicted,
                baseline_score: base.entailed_score,
                candidate_score: cand.entailed_score,
                tags: cand.tags.clone(),
            });
        } else if base.correct && !cand.correct {
            regressions.push(Flip {
                id: cand.id.clone(),
                kind: cand.kind,
                expected: cand.expected,
                baseline: base.predicted,
                candidate: cand.predicted,
                baseline_score: base.entailed_score,
                candidate_score: cand.entailed_score,
                tags: cand.tags.clone(),
            });
        }
        let delta = cand.entailed_score - base.entailed_score;
        shifts.push(ScoreShift {
            id: cand.id.clone(),
            expected: cand.expected,
            baseline_score: base.entailed_score,
            candidate_score: cand.entailed_score,
            delta,
        });
    }

    shifts.sort_by(|a, b| {
        b.delta
            .abs()
            .partial_cmp(&a.delta.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    shifts.truncate(10);

    let accuracy_delta = candidate.accuracy - baseline.accuracy;
    let soundness_delta = candidate.soundness_errors as isize - baseline.soundness_errors as isize;
    let completeness_delta =
        candidate.completeness_errors as isize - baseline.completeness_errors as isize;
    let auc_delta = match (candidate.entailed_auc, baseline.entailed_auc) {
        (Some(c), Some(b)) => Some(c - b),
        _ => None,
    };
    let margin_delta = match (candidate.entailed_margin, baseline.entailed_margin) {
        (Some(c), Some(b)) => Some(c - b),
        _ => None,
    };
    let brier_delta = match (candidate.brier_score, baseline.brier_score) {
        (Some(c), Some(b)) => Some(c - b),
        _ => None,
    };

    ComparisonReport {
        baseline,
        candidate,
        accuracy_delta,
        soundness_delta,
        completeness_delta,
        auc_delta,
        margin_delta,
        brier_delta,
        fixes,
        regressions,
        largest_shifts: shifts,
    }
}

fn majority_verdict(runs: &[Verdict]) -> VerdictKind {
    let mut counts = [0usize; 3];
    for v in runs {
        let idx = match v.effective_kind() {
            VerdictKind::Entailed => 0,
            VerdictKind::Contradicted => 1,
            VerdictKind::Neutral => 2,
        };
        counts[idx] += 1;
    }
    if counts[0] > counts[1] && counts[0] > counts[2] {
        VerdictKind::Entailed
    } else if counts[1] > counts[0] && counts[1] > counts[2] {
        VerdictKind::Contradicted
    } else if counts[2] >= counts[0] && counts[2] >= counts[1] {
        VerdictKind::Neutral
    } else {
        runs[0].effective_kind()
    }
}

fn average_probabilities(runs: &[Verdict]) -> Option<BTreeMap<String, f64>> {
    let mut sums: BTreeMap<String, f64> = BTreeMap::new();
    let mut n = 0usize;
    for v in runs {
        if let Some(probs) = &v.probabilities {
            n += 1;
            for (k, val) in probs {
                *sums.entry(k.clone()).or_insert(0.0) += val;
            }
        }
    }
    if n == 0 {
        return None;
    }
    Some(sums.into_iter().map(|(k, s)| (k, s / n as f64)).collect())
}

fn three_class_brier(expected: VerdictKind, probs: &BTreeMap<String, f64>) -> f64 {
    let labels = [
        ("entailed", VerdictKind::Entailed),
        ("contradicted", VerdictKind::Contradicted),
        ("neutral", VerdictKind::Neutral),
    ];
    labels
        .iter()
        .map(|(name, kind)| {
            let p = probs.get(*name).copied().unwrap_or(0.0);
            let y = if *kind == expected { 1.0 } else { 0.0 };
            (p - y) * (p - y)
        })
        .sum()
}

fn slice_summary(cm: &ConfusionMatrix) -> SliceSummary {
    let total = cm.total();
    let correct = cm.correct();
    SliceSummary {
        total,
        correct,
        accuracy: ratio(correct, total),
        soundness_errors: cm.soundness_errors(),
        completeness_errors: cm.completeness_errors(),
    }
}

fn roc_auc(positives: &[f64], negatives: &[f64]) -> Option<f64> {
    if positives.is_empty() || negatives.is_empty() {
        return None;
    }
    let mut wins = 0.0;
    for &p in positives {
        for &n in negatives {
            if p > n {
                wins += 1.0;
            } else if (p - n).abs() < 1e-12 {
                wins += 0.5;
            }
        }
    }
    Some(wins / (positives.len() as f64 * negatives.len() as f64))
}

/// Difference between p10 of true-entailed scores and p90 of non-entailed scores.
fn separation_margin(positives: &[f64], negatives: &[f64]) -> Option<f64> {
    if positives.is_empty() || negatives.is_empty() {
        return None;
    }
    Some(quantile(positives, 0.10) - quantile(negatives, 0.90))
}

fn quantile(values: &[f64], q: f64) -> f64 {
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = q.clamp(0.0, 1.0) * (sorted.len() - 1) as f64;
    let lo = rank.floor() as usize;
    let hi = (lo + 1).min(sorted.len() - 1);
    let w = rank - lo as f64;
    sorted[lo] + (sorted[hi] - sorted[lo]) * w
}

fn sweep_thresholds(probes: &[Probe], verdicts_per_probe: &[Vec<Verdict>]) -> Vec<ThresholdPoint> {
    let bars = [0.70, 0.75, 0.80, 0.85, 0.90, 0.95];
    bars.iter()
        .map(|&bar| {
            let mut cm = ConfusionMatrix::default();
            for (probe, runs) in probes.iter().zip(verdicts_per_probe.iter()) {
                let v = &runs[0];
                let pred = if v.confidence < bar {
                    VerdictKind::Neutral
                } else {
                    v.kind
                };
                cm.record(probe.expected, pred);
            }
            ThresholdPoint {
                threshold: bar,
                accuracy: ratio(cm.correct(), cm.total()),
                soundness_errors: cm.soundness_errors(),
                completeness_errors: cm.completeness_errors(),
            }
        })
        .collect()
}

fn compute_metamorphic(
    probes: &[Probe],
    outcomes: &[ProbeOutcome],
    repeat_stable: usize,
) -> MetamorphicSummary {
    let by_id: HashMap<&str, &ProbeOutcome> =
        outcomes.iter().map(|o| (o.id.as_str(), o)).collect();
    let mut negation_pairs = 0;
    let mut negation_symmetric = 0;
    let mut paraphrase_pairs = 0;
    let mut paraphrase_agreed = 0;

    for probe in probes {
        let Some(this) = by_id.get(probe.id.as_str()) else {
            continue;
        };
        if let Some(target_id) = &probe.negation_of {
            if let Some(other) = by_id.get(target_id.as_str()) {
                negation_pairs += 1;
                let ok = match (this.predicted, other.predicted) {
                    (VerdictKind::Entailed, VerdictKind::Contradicted)
                    | (VerdictKind::Contradicted, VerdictKind::Entailed)
                    | (VerdictKind::Neutral, VerdictKind::Neutral) => true,
                    _ => false,
                };
                if ok {
                    negation_symmetric += 1;
                }
            }
        }
        if let Some(target_id) = &probe.paraphrase_of {
            if let Some(other) = by_id.get(target_id.as_str()) {
                paraphrase_pairs += 1;
                if this.predicted == other.predicted {
                    paraphrase_agreed += 1;
                }
            }
        }
    }

    MetamorphicSummary {
        negation_pairs,
        negation_symmetric,
        paraphrase_pairs,
        paraphrase_agreed,
        repeat_stable,
        repeat_total: probes.len(),
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 {
        0.0
    } else {
        n as f64 / d as f64
    }
}
