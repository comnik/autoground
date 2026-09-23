use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

use crate::oracle::{LatencyPercentiles, OracleExchange, Usage};

use super::{Applied, Decision};

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
    /// Window after pruning. Directories carry a trailing slash.
    pub objects: Vec<String>,
    /// [`crate::universe::Universe::estimate`] of every reachable object.
    #[serde(default)]
    pub bytes_before: u64,
    /// [`crate::universe::Universe::estimate`] of the pruned window.
    #[serde(default)]
    pub bytes_after: u64,
    pub usage: UsageRecord,
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub steps: Vec<SearchStep>,
}

#[derive(Serialize, Deserialize)]
struct SearchSummary {
    proposition: String,
    repo_name: String,
    objects: Vec<String>,
    #[serde(default)]
    bytes_before: u64,
    #[serde(default)]
    bytes_after: u64,
    usage: UsageRecord,
    started_at_ms: u64,
    ended_at_ms: u64,
}

impl SearchTraceFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let path = path.as_ref();
        let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let events = crate::log::parse_jsonl(&src)?;
        assemble(events, path)
    }

    /// Append the session summary. Steps are appended as they are decided.
    pub fn write_summary(&self, log: &crate::log::Log) -> std::io::Result<()> {
        log.append_event(
            TRACE_KIND,
            &SearchSummary {
                proposition: self.proposition.clone(),
                repo_name: self.repo_name.clone(),
                objects: self.objects.clone(),
                bytes_before: self.bytes_before,
                bytes_after: self.bytes_after,
                usage: self.usage.clone(),
                started_at_ms: self.started_at_ms,
                ended_at_ms: self.ended_at_ms,
            },
        )
    }
}

fn assemble(events: Vec<Json>, path: &Path) -> Result<SearchTraceFile, String> {
    let mut steps = Vec::new();
    let mut summary: Option<SearchSummary> = None;
    for event in events {
        match event.get("kind").and_then(|kind| kind.as_str()) {
            Some("search_step") => {
                steps.push(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            Some(TRACE_KIND) => {
                summary = Some(serde_json::from_value(event).map_err(|err| err.to_string())?);
            }
            _ => {}
        }
    }
    let Some(summary) = summary else {
        return Err(format!("{} is not a search trace", path.display()));
    };
    Ok(SearchTraceFile {
        kind: TRACE_KIND.to_string(),
        proposition: summary.proposition,
        repo_name: summary.repo_name,
        objects: summary.objects,
        bytes_before: summary.bytes_before,
        bytes_after: summary.bytes_after,
        usage: summary.usage,
        started_at_ms: summary.started_at_ms,
        ended_at_ms: summary.ended_at_ms,
        steps,
    })
}

/// One line of the search summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryLine {
    pub text: String,
    /// Collapsed pruned directory.
    pub pruned: bool,
    /// The size-reduction headline.
    pub prominent: bool,
}

pub fn format_summary(path: &Path, file: &SearchTraceFile) -> String {
    summary_lines(path, file)
        .into_iter()
        .map(|line| line.text)
        .collect()
}

/// Usage, trace path, and the size cut, then the nested listing.
/// A pruned directory is one line, with no children.
pub fn summary_lines(path: &Path, file: &SearchTraceFile) -> Vec<SummaryLine> {
    let mut lines = Vec::new();
    if let Some(headline) = format_reduction(file.bytes_before, file.bytes_after) {
        lines.push(SummaryLine {
            text: format!("{headline}\n"),
            pruned: false,
            prominent: true,
        });
    }
    lines.push(SummaryLine {
        text: format!("({})\n", super::format_usage(&file.usage.to_usage())),
        pruned: false,
        prominent: false,
    });
    lines.push(SummaryLine {
        text: format!("trace: {}\n", path.display()),
        pruned: false,
        prominent: false,
    });
    lines.push(SummaryLine {
        text: "\n".into(),
        pruned: false,
        prominent: false,
    });
    let mut listing = listing_lines(file);
    if listing.is_empty() {
        listing.push(SummaryLine {
            text: "(empty)\n".into(),
            pruned: false,
            prominent: false,
        });
    }
    lines.extend(listing);
    lines
}

/// Share of the original view that pruning removed, plus the two estimates.
pub fn format_reduction(before: u64, after: u64) -> Option<String> {
    if before == 0 {
        return None;
    }
    let pct = before.saturating_sub(after) as f64 * 100.0 / before as f64;
    Some(format!(
        "{pct:.1}% smaller   {} → {}",
        format_bytes(before),
        format_bytes(after)
    ))
}

fn format_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n:.0} B")
    } else if n < MB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{:.1} MB", n / MB)
    }
}

struct Node {
    id: String,
    label: String,
    dir: bool,
    pruned: bool,
}

fn listing_lines(file: &SearchTraceFile) -> Vec<SummaryLine> {
    let nodes = listing_nodes(file);
    let mut lines = Vec::new();
    write_listing(&nodes, "", 0, &mut lines);
    lines
}

fn listing_nodes(file: &SearchTraceFile) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut kept = HashSet::new();
    for object in &file.objects {
        let dir = object.ends_with('/');
        let id = object.trim_end_matches('/').to_string();
        if id.is_empty() {
            continue;
        }
        kept.insert(id.clone());
        nodes.push(Node {
            label: entry_label(&id, dir),
            id,
            dir,
            pruned: false,
        });
    }
    let pruned: HashSet<String> = file
        .steps
        .iter()
        .flat_map(|step| step.classifications.iter())
        .filter(|decision| decision.applied == Applied::Prune)
        .map(|decision| decision.id.clone())
        .filter(|id| !id.is_empty() && !kept.contains(id))
        .collect();
    let collapse: Vec<String> = pruned
        .iter()
        .filter(|id| !pruned.iter().any(|other| is_under(id, other)))
        .cloned()
        .collect();
    for id in collapse {
        nodes.push(Node {
            label: entry_label(&id, true),
            id,
            dir: true,
            pruned: true,
        });
    }
    nodes
}

fn write_listing(nodes: &[Node], parent: &str, depth: usize, out: &mut Vec<SummaryLine>) {
    let mut kids: Vec<&Node> = nodes
        .iter()
        .filter(|node| parent_id(&node.id) == parent)
        .collect();
    kids.sort_by(|a, b| basename(&a.id).cmp(basename(&b.id)).then(a.id.cmp(&b.id)));
    for node in kids {
        out.push(SummaryLine {
            text: format!("{}{}\n", " ".repeat(depth * 2), node.label),
            pruned: node.pruned,
            prominent: false,
        });
        if node.dir && !node.pruned {
            write_listing(nodes, &node.id, depth + 1, out);
        }
    }
}

fn entry_label(id: &str, dir: bool) -> String {
    let name = basename(id);
    if dir {
        format!("{name}/")
    } else {
        name.to_string()
    }
}

fn basename(id: &str) -> &str {
    id.rsplit_once('/').map(|(_, name)| name).unwrap_or(id)
}

fn parent_id(id: &str) -> &str {
    id.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("")
}

fn is_under(id: &str, ancestor: &str) -> bool {
    id.strip_prefix(ancestor)
        .is_some_and(|rest| rest.starts_with('/'))
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
            decision.applied, decision.signal
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::{Applied, Decision};

    fn decision(id: &str, applied: Applied) -> Decision {
        Decision {
            id: id.into(),
            leaf: false,
            signal: match applied {
                Applied::Keep => 0.95,
                Applied::Prune => 0.05,
            },
            applied,
        }
    }

    fn file(objects: Vec<&str>, decisions: Vec<Decision>) -> SearchTraceFile {
        SearchTraceFile {
            kind: TRACE_KIND.into(),
            proposition: "p".into(),
            repo_name: "repo".into(),
            objects: objects.into_iter().map(str::to_string).collect(),
            bytes_before: 0,
            bytes_after: 0,
            usage: UsageRecord {
                spend_usd: 0.0,
                input_tokens: 0,
                requests: 0,
                batch_size: None,
                fill_factor: None,
                latency_p50_ms: None,
                latency_p95_ms: None,
                latency_p99_ms: None,
            },
            started_at_ms: 0,
            ended_at_ms: 0,
            steps: vec![SearchStep {
                view: Vec::new(),
                classifications: decisions,
                exchange: None,
            }],
        }
    }

    #[test]
    fn summary_nests_kept_files_and_collapses_prunes() {
        let file = file(
            vec![
                "frontend/",
                "frontend/app.py",
                "frontend/__init__.py",
                "keep/",
                "keep/nested/",
                "keep/nested/file.txt",
            ],
            vec![
                decision("backend", Applied::Prune),
                decision("decoy", Applied::Prune),
                decision("frontend", Applied::Keep),
                decision("nested", Applied::Prune),
                decision("nested/deep", Applied::Keep),
            ],
        );
        let shown = summary_lines(Path::new("/tmp/t.json"), &file);
        assert!(
            shown[0].text.starts_with('('),
            "usage leads when size is unknown"
        );
        assert!(shown[1].text.starts_with("trace:"));
        let lines: Vec<&str> = shown
            .iter()
            .skip_while(|line| !line.text.starts_with("trace:"))
            .skip(1)
            .skip_while(|line| line.text == "\n")
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(
            lines,
            vec![
                "backend/\n",
                "decoy/\n",
                "frontend/\n",
                "  __init__.py\n",
                "  app.py\n",
                "keep/\n",
                "  nested/\n",
                "    file.txt\n",
                "nested/\n",
            ]
        );
        let pruned: Vec<&str> = shown
            .iter()
            .filter(|line| line.pruned)
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(pruned, vec!["backend/\n", "decoy/\n", "nested/\n"]);
    }

    #[test]
    fn reduction_leads_the_summary() {
        let mut file = file(vec!["frontend/", "frontend/app.py"], vec![]);
        file.bytes_before = 4096;
        file.bytes_after = 1024;
        let shown = summary_lines(Path::new("/tmp/t.json"), &file);
        assert!(shown[0].prominent);
        assert_eq!(shown[0].text, "75.0% smaller   4.0 KB → 1.0 KB\n");
        assert!(shown[1].text.starts_with('('));
        assert!(shown[2].text.starts_with("trace:"));
        assert_eq!(shown[3].text, "\n");
        assert_eq!(shown[4].text, "frontend/\n");
    }

    #[test]
    fn reduction_is_absent_without_an_original_size() {
        assert_eq!(format_reduction(0, 0), None);
        assert_eq!(
            format_reduction(1000, 250).as_deref(),
            Some("75.0% smaller   1000 B → 250 B")
        );
    }

    #[test]
    fn jsonl_log_reloads_steps_and_summary() {
        let path = std::env::temp_dir().join(format!(
            "autoground-search-log-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = crate::log::Log::open(&path).unwrap();
        let mut traced = file(vec!["frontend/"], vec![decision("frontend", Applied::Keep)]);
        traced.bytes_before = 10;
        traced.bytes_after = 4;
        traced.started_at_ms = 5;
        traced.ended_at_ms = 9;
        log.append_event("search_step", &traced.steps[0]).unwrap();
        traced.write_summary(&log).unwrap();
        let loaded = SearchTraceFile::load(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.objects, traced.objects);
        assert_eq!(loaded.steps, traced.steps);
        assert_eq!(loaded.bytes_before, 10);
        assert_eq!(loaded.bytes_after, 4);
        assert_eq!(loaded.proposition, "p");
    }
}
