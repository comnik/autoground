//! Filter, then batched map, then a deterministic reduce over a [`Universe`].
//!
//! The filter sends directory paths and child names, never file bodies.
//! A directory pruned at [`SUPPORT`](crate::navigator::confidence::SUPPORT)
//! or above drops its subtree. Every file that remains is a candidate,
//! including files that sit directly at the root. The root itself is the
//! search scope: it is listed and never asked.
//!
//! The map packs those bodies into one JSON object per batch,
//! `{"<path>": "<content>"}`, and asks `true` / `false` / `unknown` per
//! file. Reduce is [`trace::verdict_of`].

mod tui;

pub mod trace;
pub use trace::{format_steps, SearchTraceFile, UsageRecord};
pub use tui::present_search;

use std::collections::HashSet;
use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::eval::Value;
use crate::navigator::confidence::SUPPORT;
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Classification, Oracle, OracleError, Ticket, Usage};
use crate::universe::{Universe, UniverseError};

use trace::{ExchangeRecord, SearchStep, ViewObject};

/// Default render budget. About 12k tokens at 4 bytes each, under Jev's
/// 32k state-side cap so quoting and the question text still fit.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub enum Finding {
    /// Non-leaf judged from its own listing. `value` is true or false.
    Directory {
        id: String,
        value: Value,
        confidence: f64,
    },
    /// Leaf judged from its body.
    File {
        id: String,
        value: Value,
        confidence: f64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Applied {
    Prune,
    Keep,
    Refine,
    True,
    False,
    Unknown,
}

impl Applied {
    fn as_str(self) -> &'static str {
        match self {
            Applied::Prune => "prune",
            Applied::Keep => "keep",
            Applied::Refine => "refine",
            Applied::True => "true",
            Applied::False => "false",
            Applied::Unknown => "unknown",
        }
    }

    fn value(self) -> Option<Value> {
        match self {
            Applied::True => Some(Value::True),
            Applied::False => Some(Value::False),
            Applied::Unknown => Some(Value::Unknown),
            Applied::Prune | Applied::Keep | Applied::Refine => None,
        }
    }
}

impl fmt::Display for Applied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Decision {
    pub id: String,
    pub leaf: bool,
    pub raw: String,
    pub applied: Applied,
    pub confidence: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchOutcome {
    pub findings: Vec<Finding>,
    pub trace: Vec<Decision>,
    pub steps: Vec<SearchStep>,
    pub usage: Usage,
}

impl SearchOutcome {
    fn empty(usage: Usage) -> Self {
        Self {
            findings: Vec::new(),
            trace: Vec::new(),
            steps: Vec::new(),
            usage,
        }
    }
}

#[derive(Debug)]
pub enum SearchError {
    Universe {
        outcome: SearchOutcome,
        error: UniverseError,
    },
    Oracle {
        outcome: SearchOutcome,
        error: OracleError,
    },
}

impl SearchError {
    pub fn outcome(&self) -> &SearchOutcome {
        match self {
            SearchError::Universe { outcome, .. } | SearchError::Oracle { outcome, .. } => outcome,
        }
    }
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SearchError::Universe { error, .. } => write!(f, "{error}"),
            SearchError::Oracle { error, .. } => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SearchError {}

pub fn search<U: Universe, O: Oracle>(
    universe: &mut U,
    oracle: &mut O,
    proposition: &str,
    budget: usize,
) -> Result<SearchOutcome, SearchError> {
    let mut findings = Vec::new();
    let mut trace = Vec::new();
    let mut steps = Vec::new();

    let directories = match directories_under(universe, "") {
        Ok(directories) => directories,
        Err(error) => {
            return Err(SearchError::Universe {
                outcome: SearchOutcome::empty(oracle.usage()),
                error,
            });
        }
    };
    let filter_chunks = match pack_directories(universe, &directories, budget) {
        Ok(chunks) => chunks,
        Err(error) => {
            return Err(SearchError::Universe {
                outcome: outcome_now(&findings, &trace, &steps, oracle),
                error,
            });
        }
    };
    let mut filter_groups = Vec::new();
    for chunk in filter_chunks {
        let state = match directory_state(universe, &chunk) {
            Ok(state) => state,
            Err(error) => {
                return Err(SearchError::Universe {
                    outcome: outcome_now(&findings, &trace, &steps, oracle),
                    error,
                });
            }
        };
        filter_groups.push(Group {
            state,
            ids: chunk,
            leaf: false,
        });
    }
    let filter = ask(
        universe,
        oracle,
        proposition,
        filter_groups,
        &findings,
        &mut trace,
        &mut steps,
    )?;
    let pruned = pruned_directories(universe, &filter);

    let files = match candidate_files(universe, &pruned) {
        Ok(files) => files,
        Err(error) => {
            return Err(SearchError::Universe {
                outcome: outcome_now(&findings, &trace, &steps, oracle),
                error,
            });
        }
    };
    let batches = match pack_files(universe, &files, budget) {
        Ok(batches) => batches,
        Err(error) => {
            return Err(SearchError::Universe {
                outcome: outcome_now(&findings, &trace, &steps, oracle),
                error,
            });
        }
    };
    let mut map_groups = Vec::new();
    for batch in batches {
        let mut ids = Vec::with_capacity(batch.len());
        let mut map = Map::new();
        for file in &batch {
            ids.push(file.id.clone());
            map.insert(file.id.clone(), Json::String(file.content.clone()));
        }
        map_groups.push(Group {
            state: Json::Object(map),
            ids,
            leaf: true,
        });
    }
    let judged = ask(
        universe,
        oracle,
        proposition,
        map_groups,
        &findings,
        &mut trace,
        &mut steps,
    )?;
    for decision in judged {
        if let Some(value) = decision.applied.value() {
            findings.push(Finding::File {
                id: decision.id,
                value,
                confidence: decision.confidence,
            });
        }
    }
    Ok(outcome_now(&findings, &trace, &steps, oracle))
}

struct Group {
    state: Json,
    ids: Vec<String>,
    leaf: bool,
}

struct Pending {
    ticket: Ticket,
    id: String,
    leaf: bool,
    step: usize,
}

#[derive(Clone)]
struct Body {
    id: String,
    content: String,
}

fn ask<U: Universe, O: Oracle>(
    universe: &U,
    oracle: &mut O,
    proposition: &str,
    groups: Vec<Group>,
    findings: &[Finding],
    trace: &mut Vec<Decision>,
    steps: &mut Vec<SearchStep>,
) -> Result<Vec<Decision>, SearchError> {
    let mut batch = Vec::new();
    for group in groups {
        let view = match capture(universe, &group.ids, group.leaf) {
            Ok(view) => view,
            Err(error) => {
                return Err(SearchError::Universe {
                    outcome: outcome_now(findings, trace, steps, oracle),
                    error,
                });
            }
        };
        let step = steps.len();
        steps.push(SearchStep {
            view,
            classifications: Vec::new(),
            exchange: None,
        });
        for id in group.ids {
            let ticket = oracle.submit(question(&group.state, &id, group.leaf, proposition));
            batch.push(Pending {
                ticket,
                id,
                leaf: group.leaf,
                step,
            });
        }
    }
    let mut decisions = Vec::new();
    for pending in batch {
        let answer = match oracle.wait(pending.ticket) {
            Ok(answer) => answer,
            Err(error) => {
                return Err(SearchError::Oracle {
                    outcome: outcome_now(findings, trace, steps, oracle),
                    error,
                });
            }
        };
        let applied = if pending.leaf {
            apply_file(&answer.choice, answer.confidence)
        } else {
            apply_directory(&answer.choice, answer.confidence)
        };
        let decision = Decision {
            id: pending.id,
            leaf: pending.leaf,
            raw: answer.choice,
            applied,
            confidence: answer.confidence,
        };
        trace.push(decision.clone());
        let step = &mut steps[pending.step];
        step.classifications.push(decision.clone());
        if step.exchange.is_none() {
            if let Some(exchange) = &answer.exchange {
                step.exchange = Some(ExchangeRecord::from_oracle(exchange));
            }
        }
        decisions.push(decision);
    }
    Ok(decisions)
}

fn outcome_now<O: Oracle>(
    findings: &[Finding],
    trace: &[Decision],
    steps: &[SearchStep],
    oracle: &O,
) -> SearchOutcome {
    SearchOutcome {
        findings: findings.to_vec(),
        trace: trace.to_vec(),
        steps: steps.to_vec(),
        usage: oracle.usage(),
    }
}

fn directories_under<U: Universe>(universe: &U, root: &str) -> Result<Vec<String>, UniverseError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_string()];
    while let Some(dir) = stack.pop() {
        for child in universe.children(&dir)? {
            if universe.is_leaf(&child)? {
                continue;
            }
            out.push(child.clone());
            stack.push(child);
        }
    }
    out.sort();
    Ok(out)
}

fn candidate_files<U: Universe>(
    universe: &U,
    pruned: &HashSet<String>,
) -> Result<Vec<String>, UniverseError> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        if pruned.contains(&dir) {
            continue;
        }
        for child in universe.children(&dir)? {
            if universe.is_leaf(&child)? {
                out.push(child);
            } else if !pruned.contains(&child) {
                stack.push(child);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn pruned_directories<U: Universe>(universe: &U, decisions: &[Decision]) -> HashSet<String> {
    let mut ids: Vec<&str> = decisions
        .iter()
        .filter(|decision| !decision.leaf)
        .map(|decision| decision.id.as_str())
        .collect();
    ids.sort_by_key(|id| id.matches('/').count());
    let mut pruned = HashSet::new();
    for id in ids {
        let dropped = ancestor_pruned(universe, id, &pruned);
        let decision = decisions.iter().find(|decision| decision.id == id);
        if dropped || decision.is_some_and(|decision| decision.applied == Applied::Prune) {
            pruned.insert(id.to_string());
        }
    }
    pruned
}

fn ancestor_pruned<U: Universe>(universe: &U, id: &str, pruned: &HashSet<String>) -> bool {
    let mut current = universe.parent(id);
    while let Some(parent) = current {
        if pruned.contains(&parent) {
            return true;
        }
        current = universe.parent(&parent);
    }
    false
}

fn pack_directories<U: Universe>(
    universe: &U,
    ids: &[String],
    budget: usize,
) -> Result<Vec<Vec<String>>, UniverseError> {
    pack_by(ids, budget, |chunk| {
        directory_state(universe, chunk).map(|state| json_len(&state))
    })
}

fn directory_state<U: Universe>(universe: &U, ids: &[String]) -> Result<Json, UniverseError> {
    let mut keys = vec![String::new()];
    keys.extend(ids.iter().cloned());
    keys.sort();
    keys.dedup();
    let mut map = Map::new();
    for id in keys {
        let mut names = Vec::new();
        for child in universe.children(&id)? {
            let name = child.rsplit('/').next().unwrap_or(&child);
            if universe.is_leaf(&child)? {
                names.push(Json::String(name.to_string()));
            } else {
                names.push(Json::String(format!("{name}/")));
            }
        }
        map.insert(id, Json::Array(names));
    }
    Ok(Json::Object(map))
}

fn pack_files<U: Universe>(
    universe: &U,
    ids: &[String],
    budget: usize,
) -> Result<Vec<Vec<Body>>, UniverseError> {
    let mut batches = Vec::new();
    let mut current: Vec<Body> = Vec::new();
    for id in ids {
        let full = universe.read(id, usize::MAX)?;
        if json_len(&body_state(id, &full)) > budget {
            if !current.is_empty() {
                batches.push(std::mem::take(&mut current));
            }
            batches.push(vec![Body {
                id: id.clone(),
                content: shrink_body(universe, id, budget)?,
            }]);
            continue;
        }
        let mut trial = current.clone();
        trial.push(Body {
            id: id.clone(),
            content: full.clone(),
        });
        let too_wide = trial.len() > QUESTION_BUDGET || json_len(&bodies_state(&trial)) > budget;
        if current.is_empty() || !too_wide {
            current = trial;
        } else {
            batches.push(std::mem::take(&mut current));
            current.push(Body {
                id: id.clone(),
                content: full,
            });
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    Ok(batches)
}

fn shrink_body<U: Universe>(
    universe: &U,
    id: &str,
    budget: usize,
) -> Result<String, UniverseError> {
    let full = universe.read(id, usize::MAX)?;
    if json_len(&body_state(id, &full)) <= budget {
        return Ok(full);
    }
    let mut best = String::new();
    let mut lo = 0usize;
    let mut hi = full.len();
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let trimmed = universe.read(id, mid.max(1))?;
        if json_len(&body_state(id, &trimmed)) <= budget {
            best = trimmed;
            lo = mid + 1;
        } else if mid == 0 {
            break;
        } else {
            hi = mid;
        }
    }
    Ok(best)
}

fn body_state(id: &str, content: &str) -> Json {
    bodies_state(&[Body {
        id: id.to_string(),
        content: content.to_string(),
    }])
}

fn bodies_state(files: &[Body]) -> Json {
    let mut map = Map::new();
    for file in files {
        map.insert(file.id.clone(), Json::String(file.content.clone()));
    }
    Json::Object(map)
}

fn json_len(value: &Json) -> usize {
    value.to_string().len()
}

fn pack_by<F>(ids: &[String], budget: usize, mut cost: F) -> Result<Vec<Vec<String>>, UniverseError>
where
    F: FnMut(&[String]) -> Result<usize, UniverseError>,
{
    let mut chunks = Vec::new();
    let mut current: Vec<String> = Vec::new();
    for id in ids {
        if cost(std::slice::from_ref(id))? > budget {
            if !current.is_empty() {
                chunks.push(std::mem::take(&mut current));
            }
            chunks.push(vec![id.clone()]);
            continue;
        }
        let mut trial = current.clone();
        trial.push(id.clone());
        let too_wide = trial.len() > QUESTION_BUDGET || cost(&trial)? > budget;
        if current.is_empty() || !too_wide {
            current = trial;
        } else {
            chunks.push(std::mem::take(&mut current));
            current.push(id.clone());
        }
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    Ok(chunks)
}

fn capture<U: Universe>(
    universe: &U,
    ids: &[String],
    leaf: bool,
) -> Result<Vec<ViewObject>, UniverseError> {
    let mut view = Vec::new();
    if !leaf {
        let summary = universe.summary("")?;
        view.push(ViewObject {
            target: false,
            id: String::new(),
            leaf: false,
            bytes: summary.bytes,
            listing: summary.listing,
        });
    }
    for id in ids {
        let summary = universe.summary(id)?;
        view.push(ViewObject {
            target: true,
            id: id.clone(),
            leaf: summary.leaf,
            bytes: summary.bytes,
            listing: summary.listing,
        });
    }
    Ok(view)
}

fn apply_file(raw: &str, confidence: f64) -> Applied {
    match raw {
        "true" if confidence >= SUPPORT => Applied::True,
        "false" if confidence >= SUPPORT => Applied::False,
        _ => Applied::Unknown,
    }
}

fn apply_directory(raw: &str, confidence: f64) -> Applied {
    match raw {
        "prune" if confidence >= SUPPORT => Applied::Prune,
        _ => Applied::Keep,
    }
}

fn question(state: &Json, id: &str, leaf: bool, proposition: &str) -> Classification {
    let (kind, ask, pairs): (&str, &str, &[(&str, &str)]) = if leaf {
        (
            "file",
            "State maps each file path to its body.\nJudge whether this file's body supports the proposition, contradicts it, or does neither.",
            &[
                ("true", "The body supports the proposition."),
                ("false", "The body contradicts the proposition."),
                ("unknown", "The body does neither, or it is unclear."),
            ],
        )
    } else {
        (
            "directory",
            "State maps each directory path to its child names. File bodies are not included.\nPrune this directory when nothing in its subtree bears on the proposition. Keep it otherwise.",
            &[
                ("prune", "Nothing in this subtree bears on the proposition."),
                (
                    "keep",
                    "Something here might bear on the proposition.",
                ),
            ],
        )
    };
    Classification {
        state: state.clone(),
        instructions: format!("target: {id}\nkind: {kind}\n{ask}\nProposition: {proposition}"),
        criteria: pairs
            .iter()
            .map(|(label, text)| ((*label).to_string(), (*text).to_string()))
            .collect(),
    }
}

#[cfg(test)]
fn target_of(instructions: &str) -> &str {
    instructions
        .lines()
        .find_map(|line| line.strip_prefix("target: "))
        .unwrap_or("")
}

fn display_path(id: &str, leaf: bool) -> String {
    if id.is_empty() {
        "/".to_string()
    } else if leaf {
        id.to_string()
    } else {
        format!("{id}/")
    }
}

pub fn format_findings(findings: &[Finding]) -> String {
    let mut out = String::new();
    for finding in findings {
        match finding {
            Finding::Directory {
                id,
                value,
                confidence,
            } => {
                out.push_str(&format!(
                    "{:<8}{}  {confidence:.2}\n",
                    value.to_string(),
                    display_path(id, false)
                ));
            }
            Finding::File {
                id,
                value,
                confidence,
            } => {
                out.push_str(&format!(
                    "{:<8}{}  {confidence:.2}\n",
                    value.to_string(),
                    display_path(id, true)
                ));
            }
        }
    }
    out
}

pub fn format_trace(trace: &[Decision]) -> String {
    let mut out = String::new();
    for decision in trace {
        out.push_str(&format!(
            "{:<8}{}  raw={}  {:.2}\n",
            decision.applied.as_str(),
            display_path(&decision.id, decision.leaf),
            decision.raw,
            decision.confidence
        ));
    }
    out
}

pub fn format_usage(usage: &Usage) -> String {
    let batch = match usage.batch_size {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    let fill = match usage.fill_factor {
        Some(factor) => format!("{:.1}%", factor * 100.0),
        None => "none".to_string(),
    };
    let mut line = format!(
        "requests={} tokens={} spend={} batch={batch} fill={fill}",
        usage.requests,
        usage.input_tokens,
        crate::report::format_usd(usage.spend_usd),
    );
    if let Some(latency) = usage.latency {
        line.push_str(&format!(
            " p50={} p95={} p99={}",
            crate::report::format_duration(latency.p50),
            crate::report::format_duration(latency.p95),
            crate::report::format_duration(latency.p99),
        ));
    }
    line
}

#[cfg(test)]
mod tests {
    use crate::oracle::Usage;

    #[test]
    fn usage_prints_batch_and_fill_as_percent() {
        let line = format_usage(&Usage {
            spend_usd: 0.0018,
            input_tokens: 42_050,
            requests: 5,
            batch_size: Some(0.104),
            fill_factor: Some(0.263),
            latency: None,
        });
        assert!(line.contains("batch=10.4%"), "{line}");
        assert!(line.contains("fill=26.3%"), "{line}");
        assert!(!line.contains("batch=0.104"), "{line}");
    }

    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::oracle::{Answer, Oracle, Ticket};
    use crate::universe::fs::Filesystem;

    struct Script {
        labels: HashMap<String, (String, f64)>,
        log: Vec<Classification>,
        pending: Vec<(Ticket, Classification)>,
        resolved: HashMap<Ticket, Answer>,
        next: u64,
    }

    impl Script {
        fn new(labels: Vec<(String, String, f64)>) -> Self {
            Self {
                labels: labels
                    .into_iter()
                    .map(|(id, choice, confidence)| (id, (choice, confidence)))
                    .collect(),
                log: Vec::new(),
                pending: Vec::new(),
                resolved: HashMap::new(),
                next: 0,
            }
        }

        fn targets(&self) -> Vec<&str> {
            self.log
                .iter()
                .map(|classification| target_of(&classification.instructions))
                .collect()
        }

        fn state_count(&self) -> usize {
            self.log
                .iter()
                .map(|classification| classification.state.to_string())
                .collect::<HashSet<_>>()
                .len()
        }

        fn of_kind(&self, kind: &str) -> Vec<&Classification> {
            let marker = format!("kind: {kind}\n");
            self.log
                .iter()
                .filter(|classification| classification.instructions.contains(&marker))
                .collect()
        }
    }

    impl Oracle for Script {
        fn submit(&mut self, req: Classification) -> Ticket {
            let ticket = Ticket(self.next);
            self.next += 1;
            self.log.push(req.clone());
            self.pending.push((ticket, req));
            ticket
        }

        fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
            if let Some(answer) = self.resolved.get(&ticket) {
                return Ok(answer.clone());
            }
            let pending = std::mem::take(&mut self.pending);
            for (pending_ticket, classification) in pending {
                let id = target_of(&classification.instructions);
                let (choice, confidence) = self
                    .labels
                    .get(id)
                    .unwrap_or_else(|| panic!("no label for {id}"))
                    .clone();
                self.resolved.insert(
                    pending_ticket,
                    Answer {
                        choice,
                        confidence,
                        exchange: None,
                    },
                );
            }
            self.resolved
                .get(&ticket)
                .cloned()
                .ok_or_else(|| OracleError::Parse(format!("unknown ticket {}", ticket.0)))
        }
    }

    fn toy(overrides: &[(&str, &str, f64)]) -> Vec<(String, String, f64)> {
        let mut labels = vec![
            ("backend", "prune", 0.95),
            ("decoy", "prune", 0.95),
            ("frontend", "prune", 0.95),
            ("backend/__init__.py", "unknown", 0.95),
            ("backend/api.py", "unknown", 0.95),
            ("decoy/noise.py", "unknown", 0.95),
            ("frontend/__init__.py", "unknown", 0.95),
            ("frontend/app.py", "unknown", 0.95),
        ];
        for (id, choice, confidence) in overrides {
            if let Some(slot) = labels.iter_mut().find(|(existing, _, _)| existing == id) {
                *slot = (*id, *choice, *confidence);
            } else {
                labels.push((*id, *choice, *confidence));
            }
        }
        labels
            .into_iter()
            .map(|(id, choice, confidence)| (id.to_string(), choice.to_string(), confidence))
            .collect()
    }

    fn keys(classification: &Classification) -> Vec<&str> {
        classification
            .criteria
            .keys()
            .map(|key| key.as_str())
            .collect()
    }

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("autoground-search-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn filter_is_one_path_state_and_map_sends_bodies_by_path() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "keep", 0.95),
            ("frontend/app.py", "true", 0.95),
        ]));
        let outcome = search(
            &mut universe,
            &mut oracle,
            "there is a frontend",
            DEFAULT_BUDGET_BYTES,
        )
        .unwrap();
        assert_eq!(oracle.state_count(), 2, "targets {:?}", oracle.targets());
        assert_eq!(oracle.of_kind("directory").len(), 3);
        assert_eq!(keys(oracle.of_kind("directory")[0]), vec!["keep", "prune"]);
        let files = oracle.of_kind("file");
        assert_eq!(files.len(), 2, "{:?}", oracle.targets());
        assert_eq!(keys(files[0]), vec!["false", "true", "unknown"]);
        assert!(!oracle.targets().contains(&""));
        assert!(!oracle.targets().contains(&"backend/api.py"));
        assert!(!oracle.targets().contains(&"decoy/noise.py"));

        let filter = &oracle.of_kind("directory")[0].state;
        assert!(filter.get("view").is_none());
        assert_eq!(
            filter[""].as_array().unwrap(),
            &vec![
                serde_json::json!("backend/"),
                serde_json::json!("decoy/"),
                serde_json::json!("frontend/")
            ]
        );
        assert_eq!(
            filter["frontend"].as_array().unwrap(),
            &vec![
                serde_json::json!("__init__.py"),
                serde_json::json!("app.py")
            ]
        );
        assert!(filter["frontend"]
            .as_array()
            .unwrap()
            .iter()
            .all(|value| value.is_string()));

        let body = files[0].state["frontend/app.py"].as_str().unwrap();
        assert!(body.contains("def main"), "{body}");
        assert!(!body.contains("=== frontend/app.py ==="), "{body}");
        assert!(files[0].state.get("backend/api.py").is_none());
        assert_eq!(
            outcome.findings,
            vec![
                Finding::File {
                    id: "frontend/__init__.py".into(),
                    value: Value::Unknown,
                    confidence: 0.95,
                },
                Finding::File {
                    id: "frontend/app.py".into(),
                    value: Value::True,
                    confidence: 0.95,
                },
            ]
        );
    }

    #[test]
    fn weak_prune_keeps_the_directory_and_still_judges_its_files() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "prune", 0.5),
            ("frontend/app.py", "false", 0.95),
        ]));
        let outcome = search(&mut universe, &mut oracle, "frontend", DEFAULT_BUDGET_BYTES).unwrap();
        let frontend = outcome
            .trace
            .iter()
            .find(|decision| decision.id == "frontend")
            .unwrap();
        assert_eq!(frontend.raw, "prune");
        assert_eq!(frontend.applied, Applied::Keep);
        assert!(oracle.targets().contains(&"frontend/app.py"));
        assert!(!oracle.targets().contains(&"backend/api.py"));
        assert!(outcome.findings.iter().any(|finding| matches!(
            finding,
            Finding::File { id, value: Value::False, .. } if id == "frontend/app.py"
        )));
    }

    #[test]
    fn weak_file_true_is_stored_as_unknown() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "keep", 0.95),
            ("frontend/app.py", "true", 0.5),
        ]));
        let outcome = search(&mut universe, &mut oracle, "frontend", DEFAULT_BUDGET_BYTES).unwrap();
        assert_eq!(
            outcome.findings,
            vec![
                Finding::File {
                    id: "frontend/__init__.py".into(),
                    value: Value::Unknown,
                    confidence: 0.95,
                },
                Finding::File {
                    id: "frontend/app.py".into(),
                    value: Value::Unknown,
                    confidence: 0.5,
                },
            ]
        );
        let app = outcome
            .trace
            .iter()
            .find(|decision| decision.id == "frontend/app.py")
            .unwrap();
        assert_eq!(app.raw, "true");
        assert_eq!(app.applied, Applied::Unknown);
    }

    #[test]
    fn parent_prune_drops_a_nested_directory_the_model_kept() {
        let root = scratch("nested");
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        std::fs::write(root.join("nested/keepme.txt"), "keep\n").unwrap();
        std::fs::write(root.join("nested/deep/file.txt"), "deep\n").unwrap();
        let mut universe = Filesystem::at_root(&root).unwrap();
        let mut oracle = Script::new(vec![
            ("nested".into(), "prune".into(), 0.99),
            ("nested/deep".into(), "keep".into(), 0.99),
        ]);
        let outcome = search(&mut universe, &mut oracle, "anything", DEFAULT_BUDGET_BYTES).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(oracle.targets(), vec!["nested", "nested/deep"]);
        assert!(outcome.findings.is_empty());
    }

    #[test]
    fn root_file_is_judged_when_a_sibling_directory_is_pruned() {
        let root = scratch("root-file");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "read me\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn lib() {}\n").unwrap();
        let mut universe = Filesystem::at_root(&root).unwrap();
        let mut oracle = Script::new(vec![
            ("src".into(), "prune".into(), 0.95),
            ("README.md".into(), "unknown".into(), 0.9),
        ]);
        let outcome = search(&mut universe, &mut oracle, "anything", DEFAULT_BUDGET_BYTES).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert!(oracle.targets().contains(&"README.md"));
        assert!(oracle.targets().contains(&"src"));
        assert!(!oracle.targets().contains(&"src/lib.rs"));
        assert_eq!(
            outcome.findings,
            vec![Finding::File {
                id: "README.md".into(),
                value: Value::Unknown,
                confidence: 0.9,
            }]
        );
    }

    #[test]
    fn tiny_budget_puts_each_directory_in_its_own_state() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[]));
        search(&mut universe, &mut oracle, "frontend", 1).unwrap();
        assert_eq!(oracle.of_kind("directory").len(), 3);
        assert_eq!(oracle.state_count(), 3, "targets {:?}", oracle.targets());
        assert!(oracle.of_kind("file").is_empty());
    }

    #[test]
    fn tight_budget_splits_file_bodies_across_states() {
        let root = scratch("split-bodies");
        std::fs::create_dir_all(root.join("keep")).unwrap();
        std::fs::write(root.join("keep/a.txt"), "a".repeat(80)).unwrap();
        std::fs::write(root.join("keep/b.txt"), "b".repeat(80)).unwrap();
        let mut universe = Filesystem::at_root(&root).unwrap();
        let mut oracle = Script::new(vec![
            ("keep".into(), "keep".into(), 0.95),
            ("keep/a.txt".into(), "unknown".into(), 0.9),
            ("keep/b.txt".into(), "unknown".into(), 0.9),
        ]);
        search(&mut universe, &mut oracle, "anything", 120).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        let files = oracle.of_kind("file");
        assert_eq!(files.len(), 2, "{:?}", oracle.targets());
        assert_ne!(
            files[0].state.to_string(),
            files[1].state.to_string(),
            "bodies shared a state"
        );
    }

    #[test]
    fn oversized_leaf_is_trimmed_in_the_path_state() {
        let root = scratch("trim");
        std::fs::write(root.join("big.txt"), "x".repeat(8_000)).unwrap();
        let mut universe = Filesystem::at_root(&root).unwrap();
        let mut oracle = Script::new(vec![("big.txt".into(), "unknown".into(), 0.95)]);
        let outcome = search(&mut universe, &mut oracle, "anything", 64).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(oracle.targets(), vec!["big.txt"]);
        let body = oracle.log[0].state["big.txt"].as_str().unwrap();
        assert!(body.contains('…'), "{body}");
        assert!(body.len() < 8_000, "{}", body.len());
        assert!(oracle.log[0].state.get("view").is_none());
        assert_eq!(
            outcome.findings,
            vec![Finding::File {
                id: "big.txt".into(),
                value: Value::Unknown,
                confidence: 0.95,
            }]
        );
    }
}
