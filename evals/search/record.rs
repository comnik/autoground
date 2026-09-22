use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct TokenUsage {
    pub input_tokens: u64,
    pub cache_read_input_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Baseline {
    pub id: String,
    pub proposition: String,
    pub model: Option<String>,
    pub session_id: String,
    pub evidence: Vec<String>,
    pub dropped: Vec<String>,
    pub navigation: Vec<String>,
    pub usage: TokenUsage,
    pub estimated_usd: Option<f64>,
    pub estimate_partial: bool,
    pub server_cost_usd: Option<f64>,
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
    pub search_spend_usd: f64,
    pub baseline_estimated_usd: Option<f64>,
    pub baseline_estimate_partial: bool,
    pub suite: SuiteSummary,
    pub curve: Vec<CurvePoint>,
    pub cases: Vec<CaseRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuiteSummary {
    pub floor: f64,
    pub mean_pruned: Option<f64>,
    pub mean_evidence: Option<f64>,
    pub micro_evidence: Option<f64>,
    pub scored: usize,
    pub unscored: usize,
    pub failed: usize,
    /// Mean, across scored propositions, of the prune rate of a window that
    /// contains exactly that proposition's evidence files.
    #[serde(default)]
    pub ideal_pruned: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CurvePoint {
    pub floor: f64,
    pub mean_pruned: Option<f64>,
    pub mean_evidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRecord {
    pub id: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_spend_usd: Option<f64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub floors: Vec<FloorRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FloorRecord {
    pub floor: f64,
    pub pruned: Option<f64>,
    pub evidence: Option<f64>,
    pub bytes_before: u64,
    pub bytes_after: u64,
    pub selected: usize,
    pub missed: Vec<String>,
}
