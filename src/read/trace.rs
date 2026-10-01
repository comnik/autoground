use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::oracle::{ExchangeIndex, EXCHANGE_KIND};
use crate::search::trace::UsageRecord;
use crate::value::Value;

use super::Stance;

pub const TRACE_KIND: &str = "read";
pub const STEP_KIND: &str = "read_step";
pub const SPLIT_KIND: &str = "read_split";

/// A file whose body was sent as more than one part.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    pub path: String,
    pub bytes: u64,
    pub parts: u64,
}

impl Split {
    pub fn line(&self) -> String {
        format!(
            "{} is {} bytes, sent as {} parts",
            self.path, self.bytes, self.parts
        )
    }
}

/// One judged database, or the escalation that reconciled a conflict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadStep {
    pub proposition: String,
    pub path: String,
    pub verdict: String,
    pub confidence: f64,
    pub rendered_bytes: u64,
    pub escalation: bool,
    /// Oracle ticket. The round-trip is the `exchange` event that lists it.
    pub ticket: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadTraceFile {
    pub kind: String,
    pub proposition: String,
    pub repo_name: String,
    pub value: Value,
    pub evidence: Vec<String>,
    pub exhausted: bool,
    #[serde(default)]
    pub candidate_bytes: u64,
    #[serde(default)]
    pub evidence_bytes: u64,
    pub stances: Vec<Stance>,
    pub usage: UsageRecord,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub steps: Vec<ReadStep>,
    pub splits: Vec<Split>,
    /// POSTs loaded from this trace, joined by step ticket.
    #[serde(skip)]
    pub exchanges: ExchangeIndex,
}

#[derive(Serialize, Deserialize)]
struct ReadSummary {
    proposition: String,
    repo_name: String,
    value: Value,
    evidence: Vec<String>,
    exhausted: bool,
    #[serde(default)]
    candidate_bytes: u64,
    #[serde(default)]
    evidence_bytes: u64,
    stances: Vec<Stance>,
    usage: UsageRecord,
    started_at_ms: u64,
    ended_at_ms: u64,
}

impl ReadTraceFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path).map_err(|err| err.to_string())?;
        let events = crate::log::parse_jsonl(&src)?;
        assemble(events, path)
    }

    /// Append the session summary. Steps are appended as they are judged.
    pub fn write_summary(&self, log: &crate::log::Log) -> std::io::Result<()> {
        log.append_event(
            TRACE_KIND,
            &ReadSummary {
                proposition: self.proposition.clone(),
                repo_name: self.repo_name.clone(),
                value: self.value,
                evidence: self.evidence.clone(),
                exhausted: self.exhausted,
                candidate_bytes: self.candidate_bytes,
                evidence_bytes: self.evidence_bytes,
                stances: self.stances.clone(),
                usage: self.usage.clone(),
                started_at_ms: self.started_at_ms,
                ended_at_ms: self.ended_at_ms,
            },
        )
    }
}

fn indent_json(value: &serde_json::Value) -> String {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    let mut out = String::new();
    for line in pretty.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn assemble(events: Vec<serde_json::Value>, path: &Path) -> Result<ReadTraceFile, String> {
    let mut steps = Vec::new();
    let mut exchanges = Vec::new();
    let mut splits = Vec::new();
    let mut summary: Option<ReadSummary> = None;
    for event in events {
        match event.get("kind").and_then(|kind| kind.as_str()) {
            Some(STEP_KIND) => {
                steps.push(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            Some(EXCHANGE_KIND) => {
                exchanges.push(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            Some(SPLIT_KIND) => {
                splits.push(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            Some(TRACE_KIND) => {
                summary = Some(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            _ => {}
        }
    }
    let Some(summary) = summary else {
        return Err(format!("{} is not a read trace", path.display()));
    };
    Ok(ReadTraceFile {
        kind: TRACE_KIND.to_string(),
        proposition: summary.proposition,
        repo_name: summary.repo_name,
        value: summary.value,
        evidence: summary.evidence,
        exhausted: summary.exhausted,
        candidate_bytes: summary.candidate_bytes,
        evidence_bytes: summary.evidence_bytes,
        stances: summary.stances,
        usage: summary.usage,
        started_at_ms: summary.started_at_ms,
        ended_at_ms: summary.ended_at_ms,
        steps,
        splits,
        exchanges: ExchangeIndex::from_events(exchanges),
    })
}

/// Value, the evidence paths, whether the candidate window was exhausted, then usage.
pub fn present_read(path: &Path, file: &ReadTraceFile, full: bool) -> io::Result<()> {
    let mut out = io::stdout();
    writeln!(out, "{}", file.value)?;
    for id in &file.evidence {
        writeln!(out, "{id}")?;
    }
    writeln!(out, "{}", if file.exhausted { "exhausted" } else { "open" })?;
    writeln!(
        out,
        "({})",
        crate::search::format_usage(&file.usage.to_usage())
    )?;
    for split in &file.splits {
        writeln!(out, "{}", split.line())?;
    }
    writeln!(out, "trace: {}", path.display())?;
    if full {
        writeln!(out)?;
        for step in &file.steps {
            if step.escalation {
                writeln!(out, "escalation  {}  {:.2}", step.verdict, step.confidence)?;
            } else {
                writeln!(
                    out,
                    "{}  {}  {:.2}",
                    step.path, step.verdict, step.confidence
                )?;
            }
            if let Some(exchange) = file.exchanges.get(step.ticket) {
                writeln!(out, "  oracle request")?;
                write!(out, "{}", indent_json(&exchange.request))?;
                writeln!(out, "  oracle response")?;
                if let Some(response) = &exchange.response {
                    write!(out, "{}", indent_json(response))?;
                }
            }
        }
    }
    out.flush()
}
