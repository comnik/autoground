use autoground::Value;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Provenance {
    pub path: String,
    pub verdict: String,
    pub excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub id: String,
    pub proposition: String,
    pub model: Option<String>,
    pub session_id: String,
    pub value: Value,
    pub provenance: Vec<Provenance>,
    pub usage: TokenUsage,
    pub estimated_usd: Option<f64>,
    pub estimate_partial: bool,
    pub server_cost_usd: Option<f64>,
    /// Wall time of the Grok process that produced this judgement.
    #[serde(default)]
    pub duration_ms: u64,
    pub priced_at: String,
    pub rate_band: String,
    pub price_source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryRun {
    pub git_sha: String,
    pub git_dirty: bool,
    pub scored_at_ms: u64,
    pub judge: String,
    pub batched: bool,
    pub read_spend_usd: f64,
    /// Wall time of this read pass, every judgement included.
    #[serde(default)]
    pub read_duration_ms: u64,
    pub baseline_estimated_usd: Option<f64>,
    pub baseline_estimate_partial: bool,
    /// Sum of the stored Grok session times.
    #[serde(default)]
    pub baseline_duration_ms: u64,
    pub suite: SuiteSummary,
    pub cases: Vec<CaseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteSummary {
    pub accuracy: Option<f64>,
    pub scored: usize,
    pub failed: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRecord {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_spend_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reader_value: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched: Option<bool>,
    #[serde(default)]
    pub baseline_paths: Vec<String>,
    #[serde(default)]
    pub reader_evidence: Vec<String>,
}
