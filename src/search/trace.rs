use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::eval::Value;
use crate::oracle::{LatencyPercentiles, OracleExchange, Usage};

use super::{Decision, Finding};

pub const TRACE_KIND: &str = "search";

/// One oracle call: the universe view, then how each target was classified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchStep {
    pub view: Vec<ViewObject>,
    pub classifications: Vec<Decision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exchange: Option<ExchangeRecord>,
}

/// One database in the view. A leaf is path plus size; a directory keeps its listing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ViewObject {
    pub id: String,
    pub leaf: bool,
    pub bytes: u64,
    /// Directory listing. Empty for a leaf.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub listing: String,
    /// Classified in this call. Samples are context only.
    pub target: bool,
}

/// Raw oracle round-trip, shared by every question in the step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExchangeRecord {
    pub request: Json,
    pub response: Json,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

impl ExchangeRecord {
    pub fn from_oracle(ex: &OracleExchange) -> Self {
        Self {
            request: ex.request.clone(),
            response: ex.response.clone(),
            started_at_ms: ex.started_at.map(epoch_ms),
            ended_at_ms: ex.ended_at.map(epoch_ms),
            input_tokens: ex.input_tokens,
            output_tokens: ex.output_tokens,
            cost_usd: ex.cost_usd,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    True,
    False,
    Unknown,
    Conflict,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::True => "[TRUE]",
            Verdict::False => "[FALSE]",
            Verdict::Unknown => "[UNKNOWN]",
            Verdict::Conflict => "[CONFLICT]",
        }
    }
}

impl std::fmt::Display for Verdict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// True when any evidence is true and none is false, and the mirror for false.
/// Both is a conflict. Neither is unknown.
pub fn verdict_of(findings: &[Finding]) -> Verdict {
    let mut saw_true = false;
    let mut saw_false = false;
    for finding in findings {
        match finding_value(finding) {
            Value::True => saw_true = true,
            Value::False => saw_false = true,
            Value::Unknown => {}
        }
    }
    match (saw_true, saw_false) {
        (true, true) => Verdict::Conflict,
        (true, false) => Verdict::True,
        (false, true) => Verdict::False,
        (false, false) => Verdict::Unknown,
    }
}

pub fn evidence(findings: &[Finding]) -> Vec<Finding> {
    findings
        .iter()
        .filter(|finding| matches!(finding_value(finding), Value::True | Value::False))
        .cloned()
        .collect()
}

fn finding_value(finding: &Finding) -> Value {
    match finding {
        Finding::Directory { value, .. } | Finding::File { value, .. } => *value,
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageRecord {
    pub spend_usd: f64,
    pub input_tokens: u64,
    pub requests: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_size: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fill_factor: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p50_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p95_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_p99_ms: Option<u64>,
}

impl UsageRecord {
    pub fn from_usage(usage: &Usage) -> Self {
        let (p50, p95, p99) = usage
            .latency
            .map(|lat| {
                (
                    Some(lat.p50.as_millis() as u64),
                    Some(lat.p95.as_millis() as u64),
                    Some(lat.p99.as_millis() as u64),
                )
            })
            .unwrap_or((None, None, None));
        Self {
            spend_usd: usage.spend_usd,
            input_tokens: usage.input_tokens,
            requests: usage.requests,
            batch_size: usage.batch_size,
            fill_factor: usage.fill_factor,
            latency_p50_ms: p50,
            latency_p95_ms: p95,
            latency_p99_ms: p99,
        }
    }

    pub fn to_usage(&self) -> Usage {
        let latency = match (
            self.latency_p50_ms,
            self.latency_p95_ms,
            self.latency_p99_ms,
        ) {
            (Some(p50), Some(p95), Some(p99)) => Some(LatencyPercentiles {
                p50: Duration::from_millis(p50),
                p95: Duration::from_millis(p95),
                p99: Duration::from_millis(p99),
            }),
            _ => None,
        };
        Usage {
            spend_usd: self.spend_usd,
            input_tokens: self.input_tokens,
            requests: self.requests,
            batch_size: self.batch_size,
            fill_factor: self.fill_factor,
            latency,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SearchTraceFile {
    pub kind: String,
    pub proposition: String,
    pub repo_name: String,
    pub findings: Vec<Finding>,
    pub usage: UsageRecord,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub steps: Vec<SearchStep>,
}

impl SearchTraceFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let src = std::fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;
        let file: Self = serde_json::from_str(&src).map_err(|e| e.to_string())?;
        if file.kind != TRACE_KIND {
            return Err(format!("{} is not a search trace", path.as_ref().display()));
        }
        Ok(file)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path.as_ref(), text).map_err(|e| e.to_string())
    }
}

pub fn format_summary(path: &Path, file: &SearchTraceFile) -> String {
    let usage = file.usage.to_usage();
    let mut out = format!(
        "{} ({})\n",
        verdict_of(&file.findings),
        super::format_usage(&usage)
    );
    out.push_str(&format!("trace: {}\n", path.display()));
    let evidence = evidence(&file.findings);
    if evidence.is_empty() {
        out.push_str("\n(no evidence)\n");
    } else {
        out.push('\n');
        out.push_str(&super::format_findings(&evidence));
    }
    out
}

pub fn format_steps(steps: &[SearchStep]) -> String {
    let n = steps.len();
    let mut out = String::new();
    for (i, step) in steps.iter().enumerate() {
        if i > 0 {
            out.push_str("\n────────────────────────────────\n\n");
        }
        out.push_str(&format_step(step, i, n));
    }
    out
}

pub fn format_step(step: &SearchStep, index: usize, total: usize) -> String {
    let mut out = format!("  step {}/{total}\n\n", index + 1);
    if step.classifications.is_empty() {
        out.push_str("  (empty)\n");
    }
    for decision in &step.classifications {
        out.push_str("  ");
        out.push_str(&display_id(&decision.id, decision.leaf));
        out.push('\n');
    }
    out.push_str("\n  oracle\n");
    if step.classifications.is_empty() {
        out.push_str("    (no classifications)\n");
    }
    for decision in &step.classifications {
        let name = display_id(&decision.id, decision.leaf);
        out.push_str(&format!(
            "  {name:<28} {:<8} {:.2}\n",
            decision.raw, decision.confidence
        ));
    }
    out
}

pub fn format_exchange(step: &SearchStep) -> String {
    let Some(ex) = &step.exchange else {
        return "\n  (no oracle request)\n".into();
    };
    let mut out = String::from("\n  oracle request\n");
    out.push_str(&indent_json(&ex.request));
    out.push_str("  oracle response\n");
    out.push_str(&indent_json(&ex.response));
    out
}

fn display_id(id: &str, leaf: bool) -> String {
    if id.is_empty() {
        "/".to_string()
    } else if leaf {
        id.to_string()
    } else {
        format!("{id}/")
    }
}

fn indent_json(value: &Json) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let mut out = String::new();
    for line in pretty.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn epoch_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
