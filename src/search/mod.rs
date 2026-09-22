//! Coarse-to-fine search over a [`Universe`], judged by an [`Oracle`].
//!
//! The frontier starts at the children of the root. A directory is
//! `prune`, `refine`, `true`, or `false` — a listing can settle the
//! proposition on its own. A file is `true`, `false`, or `unknown`, and
//! that choice is the judgement.

mod heuristic;
mod tui;

pub mod trace;
pub use trace::{format_steps, SearchTraceFile, UsageRecord};
pub use tui::present_search;

use std::collections::HashSet;
use std::fmt;

use serde::Serialize;
use serde_json::{json, Value as Json};

use crate::eval::Value;
use crate::navigator::confidence::SUPPORT;
use crate::oracle::{Classification, Oracle, OracleError, Ticket, Usage};
use crate::universe::{Universe, UniverseError};

use trace::{ExchangeRecord, SearchStep, ViewObject};

pub use heuristic::HeuristicOracle;

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
    /// Leaf judged from its body in the same call that opened it.
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
    Refine,
    True,
    False,
    Unknown,
}

impl Applied {
    fn as_str(self) -> &'static str {
        match self {
            Applied::Prune => "prune",
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
            Applied::Prune | Applied::Refine => None,
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
    let mut asked = HashSet::new();
    let root_children = match universe.children("") {
        Ok(children) => children,
        Err(error) => {
            return Err(SearchError::Universe {
                outcome: SearchOutcome::empty(oracle.usage()),
                error,
            });
        }
    };
    let mut frontier = vec![root_children];
    while frontier.iter().any(|group| !group.is_empty()) {
        let groups = std::mem::take(&mut frontier);
        let mut batch = Vec::new();
        for group in groups {
            if group.is_empty() {
                continue;
            }
            let chunks = match pack(universe, &group, budget) {
                Ok(chunks) => chunks,
                Err(error) => {
                    return Err(SearchError::Universe {
                        outcome: outcome_now(&findings, &trace, &steps, oracle),
                        error,
                    });
                }
            };
            for chunk in chunks {
                if let Err(error) = enqueue(
                    universe,
                    oracle,
                    proposition,
                    budget,
                    chunk,
                    &mut steps,
                    &mut batch,
                ) {
                    return Err(SearchError::Universe {
                        outcome: outcome_now(&findings, &trace, &steps, oracle),
                        error,
                    });
                }
            }
        }
        for pending in &batch {
            asked.insert(pending.id.clone());
        }
        let mut next = Vec::new();
        let mut settled = HashSet::new();
        let mut files = Vec::new();
        for pending in batch {
            let answer = match oracle.wait(pending.ticket) {
                Ok(answer) => answer,
                Err(error) => {
                    return Err(SearchError::Oracle {
                        outcome: outcome_now(&findings, &trace, &steps, oracle),
                        error,
                    });
                }
            };
            let applied = apply(pending.leaf, &answer.choice, answer.confidence);
            let decision = Decision {
                id: pending.id.clone(),
                leaf: pending.leaf,
                raw: answer.choice.clone(),
                applied,
                confidence: answer.confidence,
            };
            trace.push(decision.clone());
            let step = &mut steps[pending.step];
            step.classifications.push(decision);
            if step.exchange.is_none() {
                if let Some(exchange) = &answer.exchange {
                    step.exchange = Some(ExchangeRecord::from_oracle(exchange));
                }
            }
            if pending.leaf {
                files.push((pending.id, answer.confidence, applied));
                continue;
            }
            match applied {
                Applied::Prune => {
                    settled.insert(pending.id);
                }
                Applied::True | Applied::False => {
                    settled.insert(pending.id.clone());
                    if let Some(value) = applied.value() {
                        push_finding(&mut findings, false, pending.id, value, answer.confidence);
                    }
                }
                Applied::Refine => {
                    let kids = match universe.children(&pending.id) {
                        Ok(kids) => kids,
                        Err(error) => {
                            return Err(SearchError::Universe {
                                outcome: outcome_now(&findings, &trace, &steps, oracle),
                                error,
                            });
                        }
                    };
                    let mut follow = Vec::new();
                    for kid in kids {
                        if asked.contains(&kid) {
                            continue;
                        }
                        match unasked_leaf(universe, &kid, &asked) {
                            Ok(true) => follow.push(kid),
                            Ok(false) => {}
                            Err(error) => {
                                return Err(SearchError::Universe {
                                    outcome: outcome_now(&findings, &trace, &steps, oracle),
                                    error,
                                });
                            }
                        }
                    }
                    if !follow.is_empty() {
                        next.push(follow);
                    }
                }
                Applied::Unknown => {}
            }
        }
        for (id, confidence, applied) in files {
            if under_settled(universe, &id, &settled) {
                continue;
            }
            if let Some(value) = applied.value() {
                push_finding(&mut findings, true, id, value, confidence);
            }
        }
        frontier = next;
    }
    Ok(outcome_now(&findings, &trace, &steps, oracle))
}

struct Pending {
    ticket: Ticket,
    id: String,
    leaf: bool,
    step: usize,
}

enum Chunk {
    Group(Vec<String>),
    Trimmed(String),
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

fn push_finding(
    findings: &mut Vec<Finding>,
    leaf: bool,
    id: String,
    value: Value,
    confidence: f64,
) {
    if leaf {
        findings.push(Finding::File {
            id,
            value,
            confidence,
        });
    } else if value != Value::Unknown {
        findings.push(Finding::Directory {
            id,
            value,
            confidence,
        });
    }
}

fn enqueue<U: Universe, O: Oracle>(
    universe: &mut U,
    oracle: &mut O,
    proposition: &str,
    budget: usize,
    chunk: Chunk,
    steps: &mut Vec<SearchStep>,
    batch: &mut Vec<Pending>,
) -> Result<(), UniverseError> {
    let (state, targets, trimmed) = match chunk {
        Chunk::Group(ids) => {
            widen(universe, &ids, budget)?;
            let view = universe.render()?;
            let mut question_ids = ids;
            question_ids.extend(extra_leaves(universe, &question_ids)?);
            (json!({ "view": view }), question_ids, false)
        }
        Chunk::Trimmed(id) => {
            let view = universe.render_capped(&id, budget)?;
            (json!({ "view": view }), vec![id], true)
        }
    };
    let view = capture_view(universe, &targets, trimmed)?;
    let step = steps.len();
    steps.push(SearchStep {
        view,
        classifications: Vec::new(),
        exchange: None,
    });
    for id in targets {
        let leaf = trimmed || universe.is_leaf(&id)?;
        let ticket = oracle.submit(question(&state, &id, leaf, proposition));
        batch.push(Pending {
            ticket,
            id,
            leaf,
            step,
        });
    }
    Ok(())
}

fn capture_view<U: Universe>(
    universe: &U,
    targets: &[String],
    trimmed: bool,
) -> Result<Vec<ViewObject>, UniverseError> {
    let ids = if trimmed {
        targets.to_vec()
    } else {
        let mut ids: Vec<String> = universe.objects().to_vec();
        for sample in universe.samples() {
            if !ids.iter().any(|id| id == sample) {
                ids.push(sample.clone());
            }
        }
        ids
    };
    let mut view = Vec::with_capacity(ids.len());
    for id in ids {
        let summary = universe.summary(&id)?;
        view.push(ViewObject {
            target: targets.iter().any(|target| target == &id),
            id,
            leaf: summary.leaf,
            bytes: summary.bytes,
            listing: summary.listing,
        });
    }
    Ok(view)
}

fn pack<U: Universe>(
    universe: &mut U,
    ids: &[String],
    budget: usize,
) -> Result<Vec<Chunk>, UniverseError> {
    let mut chunks = Vec::new();
    let mut current = Vec::new();
    for id in ids {
        let alone = estimate_ids(universe, std::slice::from_ref(id))?;
        if alone > budget {
            if !current.is_empty() {
                chunks.push(Chunk::Group(std::mem::take(&mut current)));
            }
            if universe.is_leaf(id)? {
                chunks.push(Chunk::Trimmed(id.clone()));
            } else {
                let kids = universe.children(id)?;
                if kids.is_empty() {
                    chunks.push(Chunk::Group(vec![id.clone()]));
                } else {
                    chunks.extend(pack(universe, &kids, budget)?);
                }
            }
            continue;
        }
        let mut trial = current.clone();
        trial.push(id.clone());
        if current.is_empty() || estimate_ids(universe, &trial)? <= budget {
            current = trial;
        } else {
            chunks.push(Chunk::Group(std::mem::take(&mut current)));
            current.push(id.clone());
        }
    }
    if !current.is_empty() {
        chunks.push(Chunk::Group(current));
    }
    Ok(chunks)
}

/// When every leaf under `targets` fits with them, put those leaves in the
/// view so their judgements share this state. Otherwise keep the targets and
/// fill the leftover with samples.
fn widen<U: Universe>(
    universe: &mut U,
    targets: &[String],
    budget: usize,
) -> Result<(), UniverseError> {
    let mut widened = targets.to_vec();
    for leaf in descendant_leaves(universe, targets)? {
        if !widened.iter().any(|id| id == &leaf) {
            widened.push(leaf);
        }
    }
    if estimate_ids(universe, &widened)? <= budget {
        return Ok(());
    }
    universe.set(targets.to_vec())?;
    let base = universe.estimate()?;
    universe.sample(budget.saturating_sub(base))?;
    Ok(())
}

fn descendant_leaves<U: Universe>(
    universe: &U,
    ids: &[String],
) -> Result<Vec<String>, UniverseError> {
    let mut out = Vec::new();
    let mut stack = ids.to_vec();
    while let Some(id) = stack.pop() {
        if universe.is_leaf(&id)? {
            out.push(id);
        } else {
            stack.extend(universe.children(&id)?);
        }
    }
    out.sort();
    out.dedup();
    Ok(out)
}

fn extra_leaves<U: Universe>(
    universe: &U,
    targets: &[String],
) -> Result<Vec<String>, UniverseError> {
    let mut extras = Vec::new();
    for id in universe.objects().iter().chain(universe.samples()) {
        if targets.iter().any(|target| target == id) {
            continue;
        }
        if universe.is_leaf(id)? {
            extras.push(id.clone());
        }
    }
    extras.sort();
    extras.dedup();
    Ok(extras)
}

fn unasked_leaf<U: Universe>(
    universe: &U,
    id: &str,
    asked: &HashSet<String>,
) -> Result<bool, UniverseError> {
    if universe.is_leaf(id)? {
        return Ok(!asked.contains(id));
    }
    for child in universe.children(id)? {
        if unasked_leaf(universe, &child, asked)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn under_settled<U: Universe>(universe: &U, id: &str, settled: &HashSet<String>) -> bool {
    let mut current = universe.parent(id);
    while let Some(parent) = current {
        if settled.contains(&parent) {
            return true;
        }
        current = universe.parent(&parent);
    }
    false
}

fn estimate_ids<U: Universe>(universe: &mut U, ids: &[String]) -> Result<usize, UniverseError> {
    universe.set(ids.to_vec())?;
    universe.estimate()
}

fn apply(leaf: bool, raw: &str, confidence: f64) -> Applied {
    if leaf {
        return match raw {
            "true" if confidence >= SUPPORT => Applied::True,
            "false" if confidence >= SUPPORT => Applied::False,
            _ => Applied::Unknown,
        };
    }
    match raw {
        "prune" if confidence >= SUPPORT => Applied::Prune,
        "true" if confidence >= SUPPORT => Applied::True,
        "false" if confidence >= SUPPORT => Applied::False,
        _ => Applied::Refine,
    }
}

fn question(state: &Json, id: &str, leaf: bool, proposition: &str) -> Classification {
    let kind = if leaf { "file" } else { "directory" };
    let ask = if leaf {
        "Judge whether this file's body supports the proposition, contradicts it, or does neither."
    } else {
        "Judge this directory from the whole view. True or false when it settles the proposition. Prune when nothing here bears on it. Refine only when the view is missing something required to decide."
    };
    let pairs: &[(&str, &str)] = if leaf {
        &[
            ("true", "The body supports the proposition."),
            ("false", "The body contradicts the proposition."),
            ("unknown", "The body does neither, or it is unclear."),
        ]
    } else {
        &[
            ("prune", "Nothing in this subtree bears on the proposition."),
            (
                "refine",
                "The view is missing something required to decide.",
            ),
            ("true", "The view supports the proposition."),
            ("false", "The view contradicts the proposition."),
        ]
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

pub(crate) fn target_of(instructions: &str) -> &str {
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
    use crate::universe::Universe;

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

    #[test]
    fn directory_judgements_share_one_state_and_do_not_open_files() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "true", 0.95),
            ("backend", "false", 0.95),
            ("decoy", "prune", 0.95),
        ]));
        let outcome = search(
            &mut universe,
            &mut oracle,
            "there is a frontend",
            DEFAULT_BUDGET_BYTES,
        )
        .unwrap();
        assert_eq!(oracle.state_count(), 1);
        assert_eq!(oracle.of_kind("file").len(), 5);
        assert!(oracle.targets().contains(&"frontend/app.py"));
        assert_eq!(
            keys(oracle.of_kind("directory")[0]),
            vec!["false", "prune", "refine", "true"]
        );
        assert_eq!(
            outcome.findings,
            vec![
                Finding::Directory {
                    id: "backend".into(),
                    value: Value::False,
                    confidence: 0.95,
                },
                Finding::Directory {
                    id: "frontend".into(),
                    value: Value::True,
                    confidence: 0.95,
                },
            ]
        );
    }

    #[test]
    fn refine_judges_files_once_with_eval_criteria() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "refine", 0.95),
            ("backend", "prune", 0.95),
            ("decoy", "prune", 0.95),
            ("frontend/app.py", "true", 0.95),
        ]));
        let outcome = search(&mut universe, &mut oracle, "frontend", DEFAULT_BUDGET_BYTES).unwrap();
        assert_eq!(oracle.state_count(), 1);
        let files = oracle.of_kind("file");
        assert_eq!(files.len(), 5);
        assert_eq!(keys(files[0]), vec!["false", "true", "unknown"]);
        assert!(!keys(files[0]).contains(&"prune"));
        assert_eq!(
            oracle
                .targets()
                .iter()
                .filter(|id| **id == "frontend/app.py")
                .count(),
            1
        );
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
    fn weak_directory_prune_refines() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "prune", 0.5),
            ("frontend/app.py", "false", 0.95),
        ]));
        let outcome = search(&mut universe, &mut oracle, "frontend", DEFAULT_BUDGET_BYTES).unwrap();
        assert_eq!(oracle.state_count(), 1);
        assert!(oracle.targets().contains(&"frontend/app.py"));
        assert!(oracle.targets().contains(&"backend/api.py"));
        assert!(!outcome.findings.iter().any(|finding| matches!(
            finding,
            Finding::File { id, .. } if id == "backend/api.py"
        )));
        assert_eq!(outcome.trace[2].id, "frontend");
        assert_eq!(outcome.trace[2].raw, "prune");
        assert_eq!(outcome.trace[2].applied, Applied::Refine);
        assert!(outcome.findings.iter().any(|finding| matches!(
            finding,
            Finding::File { id, value: Value::False, .. } if id == "frontend/app.py"
        )));
    }

    #[test]
    fn weak_directory_true_refines() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[("frontend", "true", 0.5)]));
        search(&mut universe, &mut oracle, "frontend", DEFAULT_BUDGET_BYTES).unwrap();
        assert_eq!(oracle.state_count(), 1);
        assert!(oracle.targets().contains(&"frontend/app.py"));
        assert!(oracle.targets().iter().any(|id| id.starts_with("backend/")));
    }

    #[test]
    fn weak_file_true_is_stored_as_unknown() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "refine", 0.95),
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
    fn tight_budget_splits_one_sibling_group() {
        let mut probe = Filesystem::at_root("examples/toy-layered").unwrap();
        let ids = probe.objects().to_vec();
        let mut largest = 0;
        for id in &ids {
            probe.set(vec![id.clone()]).unwrap();
            largest = largest.max(probe.estimate().unwrap());
        }
        probe.set(ids).unwrap();
        let together = probe.estimate().unwrap();
        assert!(together > largest);

        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "prune", 0.95),
            ("backend", "prune", 0.95),
            ("decoy", "prune", 0.95),
        ]));
        search(&mut universe, &mut oracle, "frontend", largest).unwrap();
        assert!(oracle.state_count() >= 2, "states {}", oracle.state_count());
        for id in ["backend", "decoy", "frontend"] {
            assert!(oracle.targets().contains(&id), "{id}");
        }
    }

    #[test]
    fn fitting_repo_is_one_state_even_when_every_directory_refines() {
        let mut universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let mut oracle = Script::new(toy(&[
            ("frontend", "refine", 0.99),
            ("backend", "refine", 0.99),
            ("decoy", "refine", 0.99),
        ]));
        let outcome = search(
            &mut universe,
            &mut oracle,
            "the codebase has a frontend",
            DEFAULT_BUDGET_BYTES,
        )
        .unwrap();
        assert_eq!(oracle.state_count(), 1, "targets {:?}", oracle.targets());
        assert_eq!(oracle.targets().len(), 8);
        assert_eq!(outcome.findings.len(), 5);
        assert_eq!(outcome.steps.len(), 1);
        let app = outcome.steps[0]
            .view
            .iter()
            .find(|obj| obj.id == "frontend/app.py")
            .unwrap();
        assert!(app.leaf);
        assert!(app.listing.is_empty());
        assert!(app.bytes > 0);
        let shown = crate::search::trace::format_step(&outcome.steps[0], 0, 1);
        assert!(!shown.contains("from backend import api"), "{shown}");
        assert!(shown.contains("frontend/app.py"));
    }

    #[test]
    fn oversized_leaf_is_trimmed_once() {
        let root =
            std::env::temp_dir().join(format!("autoground-search-trim-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("big.txt"), "x".repeat(8_000)).unwrap();
        let mut universe = Filesystem::at_root(&root).unwrap();
        let mut oracle = Script::new(vec![("big.txt".into(), "unknown".into(), 0.95)]);
        let outcome = search(&mut universe, &mut oracle, "anything", 64).unwrap();
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(oracle.targets(), vec!["big.txt"]);
        let view = oracle.log[0].state["view"].as_str().unwrap();
        assert!(view.contains('…'), "{view}");
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
