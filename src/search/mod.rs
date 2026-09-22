//! Prune a [`Universe`] down to the objects that bear on a proposition.
//!
//! [`Search::submit`] enqueues a proposition. [`Search::wait`] flushes every
//! pending proposition together: directory listings are built once, and each
//! proposition adds a prune/keep question on that shared state. The oracle
//! then batches questions that share a state. A directory pruned at
//! [`SUPPORT`](crate::navigator::confidence::SUPPORT) or above drops its
//! subtree. What remains is a clone of the universe whose window is the kept
//! directories and the files under them. File bodies are not judged.

mod tui;

pub mod trace;
pub use trace::{format_steps, SearchTraceFile, UsageRecord};
pub use tui::present_search;

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::log::Log;
use crate::navigator::confidence::SUPPORT;
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Classification, Oracle, Ticket as OracleTicket, Usage};
use crate::universe::{Universe, UniverseError};

use trace::{ExchangeRecord, SearchStep, ViewObject};

/// Default render budget for one directory-listing state. About 12k tokens at
/// 4 bytes each, under Jev's 32k state-side cap.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

/// Handle for a proposition enqueued with [`Search::submit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Applied {
    Prune,
    Keep,
}

impl Applied {
    fn as_str(self) -> &'static str {
        match self {
            Applied::Prune => "prune",
            Applied::Keep => "keep",
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

/// Pruned universe for one proposition, plus the directory decisions that
/// produced the window.
#[derive(Debug, Clone)]
pub struct View<U> {
    pub proposition: String,
    pub universe: U,
    pub trace: Vec<Decision>,
    pub steps: Vec<SearchStep>,
    pub usage: Usage,
    /// [`Universe::estimate`] of every reachable object, before pruning.
    pub bytes_before: usize,
    /// [`Universe::estimate`] of the pruned window.
    pub bytes_after: usize,
}

#[derive(Debug, Clone)]
pub struct SearchError {
    pub steps: Vec<SearchStep>,
    pub trace: Vec<Decision>,
    pub usage: Usage,
    message: String,
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SearchError {}

struct Pending {
    ticket: Ticket,
    proposition: String,
}

struct Group {
    state: Json,
    ids: Vec<String>,
    view: Vec<ViewObject>,
}

/// A universe and an oracle shared by every proposition submitted to this
/// session. `submit` only enqueues. `wait` asks.
pub struct Search<U, O> {
    universe: U,
    oracle: O,
    budget: usize,
    next: u64,
    pending: Vec<Pending>,
    resolved: HashMap<Ticket, Result<View<U>, SearchError>>,
    log: Option<Log>,
}

impl<U, O> Search<U, O> {
    pub fn new(universe: U, oracle: O, budget: usize) -> Self {
        Self {
            universe,
            oracle,
            budget,
            next: 0,
            pending: Vec::new(),
            resolved: HashMap::new(),
            log: None,
        }
    }

    pub fn set_log(&mut self, log: Log) {
        self.log = Some(log);
    }

    pub fn usage(&self) -> Usage
    where
        O: Oracle,
    {
        self.oracle.usage()
    }
}

impl<U, O> Search<U, O>
where
    U: Universe + Clone,
    O: Oracle,
{
    pub fn submit(&mut self, proposition: impl Into<String>) -> Ticket {
        let ticket = Ticket(self.next);
        self.next += 1;
        self.pending.push(Pending {
            ticket,
            proposition: proposition.into(),
        });
        ticket
    }

    /// Flush every pending proposition, then return this one's pruned window.
    ///
    /// Listings are rendered once. Each proposition contributes its own
    /// question per directory, on that same state, so the oracle can fill a
    /// request with questions from many propositions.
    pub fn wait(&mut self, ticket: Ticket) -> Result<View<U>, SearchError> {
        if let Some(done) = self.resolved.get(&ticket) {
            return done.clone();
        }
        if !self.pending.iter().any(|item| item.ticket == ticket) {
            return Err(SearchError {
                steps: Vec::new(),
                trace: Vec::new(),
                usage: self.oracle.usage(),
                message: format!("unknown ticket {}", ticket.0),
            });
        }
        self.flush();
        self.resolved.get(&ticket).cloned().unwrap_or_else(|| {
            Err(SearchError {
                steps: Vec::new(),
                trace: Vec::new(),
                usage: self.oracle.usage(),
                message: format!("missing ticket {}", ticket.0),
            })
        })
    }

    pub fn wait_many(&mut self, tickets: &[Ticket]) -> Vec<Result<View<U>, SearchError>> {
        tickets
            .iter()
            .copied()
            .map(|ticket| self.wait(ticket))
            .collect()
    }

    fn reject_all(&mut self, pending: Vec<Pending>, message: String) {
        let usage = self.oracle.usage();
        for item in pending {
            self.resolved.insert(
                item.ticket,
                Err(SearchError {
                    steps: Vec::new(),
                    trace: Vec::new(),
                    usage,
                    message: message.clone(),
                }),
            );
        }
    }

    fn log_steps(&self, steps: &[SearchStep]) {
        let Some(log) = &self.log else {
            return;
        };
        for step in steps {
            let _ = log.append_event("search_step", step);
        }
    }

    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        let directories = match directories_under(&self.universe, "") {
            Ok(directories) => directories,
            Err(error) => {
                self.reject_all(pending, error.to_string());
                return;
            }
        };
        let chunks = match pack_directories(&self.universe, &directories, self.budget) {
            Ok(chunks) => chunks,
            Err(error) => {
                self.reject_all(pending, error.to_string());
                return;
            }
        };
        let mut groups = Vec::new();
        for chunk in chunks {
            let state = match directory_state(&self.universe, &chunk) {
                Ok(state) => state,
                Err(error) => {
                    self.reject_all(pending, error.to_string());
                    return;
                }
            };
            let view = match capture(&self.universe, &chunk) {
                Ok(view) => view,
                Err(error) => {
                    self.reject_all(pending, error.to_string());
                    return;
                }
            };
            groups.push(Group {
                state,
                ids: chunk,
                view,
            });
        }

        struct Ask {
            search_ticket: Ticket,
            step: usize,
            id: String,
            oracle_ticket: OracleTicket,
        }

        let mut steps: HashMap<Ticket, Vec<SearchStep>> = HashMap::new();
        let mut asks = Vec::new();
        for group in &groups {
            for item in &pending {
                let prop_steps = steps.entry(item.ticket).or_default();
                let step = prop_steps.len();
                prop_steps.push(SearchStep {
                    view: group.view.clone(),
                    classifications: Vec::new(),
                    exchange: None,
                });
                for id in &group.ids {
                    let oracle_ticket =
                        self.oracle
                            .submit(question(&group.state, id, &item.proposition));
                    asks.push(Ask {
                        search_ticket: item.ticket,
                        step,
                        id: id.clone(),
                        oracle_ticket,
                    });
                }
            }
        }

        let mut failed: HashMap<Ticket, String> = HashMap::new();
        for ask in asks {
            match self.oracle.wait(ask.oracle_ticket) {
                Ok(answer) => {
                    let decision = Decision {
                        id: ask.id,
                        leaf: false,
                        raw: answer.choice.clone(),
                        applied: apply_directory(&answer.choice, answer.confidence),
                        confidence: answer.confidence,
                    };
                    let step = &mut steps.get_mut(&ask.search_ticket).unwrap()[ask.step];
                    step.classifications.push(decision);
                    if step.exchange.is_none() {
                        if let Some(exchange) = &answer.exchange {
                            step.exchange = Some(ExchangeRecord::from_oracle(exchange));
                        }
                    }
                }
                Err(error) => {
                    failed
                        .entry(ask.search_ticket)
                        .or_insert_with(|| error.to_string());
                }
            }
        }

        let usage = self.oracle.usage();
        let bytes_before = reachable_estimate(&self.universe).unwrap_or(0);
        for item in pending {
            let prop_steps = steps.remove(&item.ticket).unwrap_or_default();
            self.log_steps(&prop_steps);
            let trace: Vec<Decision> = prop_steps
                .iter()
                .flat_map(|step| step.classifications.clone())
                .collect();
            if let Some(message) = failed.remove(&item.ticket) {
                self.resolved.insert(
                    item.ticket,
                    Err(SearchError {
                        steps: prop_steps,
                        trace,
                        usage,
                        message,
                    }),
                );
                continue;
            }
            let ids = match candidate_ids(&self.universe, &trace) {
                Ok(ids) => ids,
                Err(error) => {
                    self.resolved.insert(
                        item.ticket,
                        Err(SearchError {
                            steps: prop_steps,
                            trace,
                            usage,
                            message: error.to_string(),
                        }),
                    );
                    continue;
                }
            };
            let mut universe = self.universe.clone();
            if let Err(error) = universe.set(ids) {
                self.resolved.insert(
                    item.ticket,
                    Err(SearchError {
                        steps: prop_steps,
                        trace,
                        usage,
                        message: error.to_string(),
                    }),
                );
                continue;
            }
            let bytes_after = universe.estimate().unwrap_or(0);
            self.resolved.insert(
                item.ticket,
                Ok(View {
                    proposition: item.proposition,
                    universe,
                    trace,
                    steps: prop_steps,
                    usage,
                    bytes_before,
                    bytes_after,
                }),
            );
        }
    }
}

/// [`Universe::estimate`] of every reachable file and directory.
fn reachable_estimate<U: Universe + Clone>(universe: &U) -> Result<usize, UniverseError> {
    let mut full = universe.clone();
    let mut ids = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in full.children(&dir)? {
            ids.push(child.clone());
            if !full.is_leaf(&child)? {
                stack.push(child);
            }
        }
    }
    full.set(ids)?;
    full.estimate()
}

/// Paths in the window, directories with a trailing slash.
pub fn window_lines<U: Universe>(universe: &U) -> Result<Vec<String>, UniverseError> {
    let mut lines = Vec::new();
    for id in universe.objects() {
        lines.push(display_path(id, universe.is_leaf(id)?));
    }
    Ok(lines)
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

fn candidate_ids<U: Universe>(
    universe: &U,
    decisions: &[Decision],
) -> Result<Vec<String>, UniverseError> {
    let pruned = pruned_directories(universe, decisions);
    let mut ids: Vec<String> = decisions
        .iter()
        .filter(|decision| !pruned.contains(&decision.id))
        .map(|decision| decision.id.clone())
        .collect();
    ids.extend(candidate_files(universe, &pruned)?);
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn pruned_directories<U: Universe>(universe: &U, decisions: &[Decision]) -> HashSet<String> {
    let mut ids: Vec<&str> = decisions
        .iter()
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

fn capture<U: Universe>(universe: &U, ids: &[String]) -> Result<Vec<ViewObject>, UniverseError> {
    let mut view = Vec::new();
    let summary = universe.summary("")?;
    view.push(ViewObject {
        target: false,
        id: String::new(),
        leaf: false,
        bytes: summary.bytes,
        listing: summary.listing,
    });
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

fn apply_directory(raw: &str, confidence: f64) -> Applied {
    apply_prune(raw, confidence, SUPPORT)
}

/// A directory `prune` at or above `min_confidence` drops its subtree. A live
/// search passes [`SUPPORT`].
fn apply_prune(raw: &str, confidence: f64, min_confidence: f64) -> Applied {
    match raw {
        "prune" if confidence >= min_confidence => Applied::Prune,
        _ => Applied::Keep,
    }
}

/// Object ids kept when directory decisions are applied at `min_confidence`.
///
/// `universe` is the full tree. `children` is read from disk, so a universe
/// whose window is already narrowed still yields the same ids.
pub fn retained_ids<U: Universe>(
    universe: &U,
    decisions: &[Decision],
    min_confidence: f64,
) -> Result<Vec<String>, UniverseError> {
    let adjusted = decisions
        .iter()
        .cloned()
        .map(|mut decision| {
            decision.applied = apply_prune(&decision.raw, decision.confidence, min_confidence);
            decision
        })
        .collect::<Vec<_>>();
    candidate_ids(universe, &adjusted)
}

fn question(state: &Json, id: &str, proposition: &str) -> Classification {
    let ask = "State maps each directory path to its child names. File bodies are not included.\nPrune this directory when nothing in its subtree bears on the proposition. Keep it otherwise.";
    let pairs = [
        ("prune", "Nothing in this subtree bears on the proposition."),
        ("keep", "Something here might bear on the proposition."),
    ];
    Classification {
        state: state.clone(),
        instructions: format!("target: {id}\nkind: directory\n{ask}\nProposition: {proposition}"),
        criteria: pairs
            .into_iter()
            .map(|(label, text)| (label.to_string(), text.to_string()))
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
    use crate::oracle::{Answer, Oracle, OracleError, Ticket as OracleTicket};
    use crate::universe::fs::Filesystem;

    struct Script {
        labels: HashMap<String, (String, f64)>,
        log: Vec<Classification>,
        pending: Vec<(OracleTicket, Classification)>,
        resolved: HashMap<OracleTicket, Answer>,
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

        fn bind(&mut self, proposition: &str, id: &str, choice: &str, confidence: f64) {
            self.labels.insert(
                format!("{proposition}\t{id}"),
                (choice.to_string(), confidence),
            );
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
        fn submit(&mut self, req: Classification) -> OracleTicket {
            let ticket = OracleTicket(self.next);
            self.next += 1;
            self.log.push(req.clone());
            self.pending.push((ticket, req));
            ticket
        }

        fn wait(&mut self, ticket: OracleTicket) -> Result<Answer, OracleError> {
            if let Some(answer) = self.resolved.get(&ticket) {
                return Ok(answer.clone());
            }
            let pending = std::mem::take(&mut self.pending);
            for (pending_ticket, classification) in pending {
                let id = target_of(&classification.instructions);
                let proposition = classification
                    .instructions
                    .lines()
                    .find_map(|line| line.strip_prefix("Proposition: "))
                    .unwrap_or("");
                let (choice, confidence) = self
                    .labels
                    .get(&format!("{proposition}\t{id}"))
                    .or_else(|| self.labels.get(id))
                    .unwrap_or_else(|| panic!("no label for {proposition} {id}"))
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

    fn search_one(
        universe: Filesystem,
        oracle: Script,
        proposition: &str,
        budget: usize,
    ) -> (View<Filesystem>, Script) {
        let mut session = Search::new(universe, oracle, budget);
        let ticket = session.submit(proposition);
        let view = session.wait(ticket).unwrap();
        (view, session.oracle)
    }

    fn ids(view: &View<Filesystem>) -> Vec<String> {
        view.universe.objects().to_vec()
    }

    #[test]
    fn kept_directory_and_its_files_are_the_window() {
        let universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let (view, oracle) = search_one(
            universe,
            Script::new(toy(&[("frontend", "keep", 0.95)])),
            "there is a frontend",
            DEFAULT_BUDGET_BYTES,
        );
        assert_eq!(oracle.state_count(), 1, "targets {:?}", oracle.targets());
        assert_eq!(oracle.of_kind("directory").len(), 3);
        assert!(oracle.of_kind("file").is_empty());
        assert_eq!(keys(oracle.of_kind("directory")[0]), vec!["keep", "prune"]);
        assert!(!oracle.targets().contains(&""));
        assert_eq!(
            ids(&view),
            vec![
                "frontend".to_string(),
                "frontend/__init__.py".to_string(),
                "frontend/app.py".to_string(),
            ]
        );

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
        assert!(filter.get("frontend/app.py").is_none());
        assert!(view.bytes_before > view.bytes_after);
        assert!(view.bytes_after > 0);
    }

    #[test]
    fn weak_prune_keeps_the_directory_and_its_files() {
        let universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let (view, oracle) = search_one(
            universe,
            Script::new(toy(&[("frontend", "prune", 0.5)])),
            "frontend",
            DEFAULT_BUDGET_BYTES,
        );
        let frontend = view
            .trace
            .iter()
            .find(|decision| decision.id == "frontend")
            .unwrap();
        assert_eq!(frontend.raw, "prune");
        assert_eq!(frontend.applied, Applied::Keep);
        assert!(!oracle.targets().contains(&"frontend/app.py"));
        assert_eq!(
            ids(&view),
            vec![
                "frontend".to_string(),
                "frontend/__init__.py".to_string(),
                "frontend/app.py".to_string(),
            ]
        );
    }

    #[test]
    fn parent_prune_drops_a_nested_directory_the_model_kept() {
        let root = scratch("nested");
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        std::fs::write(root.join("nested/keepme.txt"), "keep\n").unwrap();
        std::fs::write(root.join("nested/deep/file.txt"), "deep\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let (view, oracle) = search_one(
            universe,
            Script::new(vec![
                ("nested".into(), "prune".into(), 0.99),
                ("nested/deep".into(), "keep".into(), 0.99),
            ]),
            "anything",
            DEFAULT_BUDGET_BYTES,
        );
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(oracle.targets(), vec!["nested", "nested/deep"]);
        assert!(ids(&view).is_empty());
    }

    #[test]
    fn root_file_stays_when_a_sibling_directory_is_pruned() {
        let root = scratch("root-file");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("README.md"), "read me\n").unwrap();
        std::fs::write(root.join("src/lib.rs"), "fn lib() {}\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let (view, oracle) = search_one(
            universe,
            Script::new(vec![("src".into(), "prune".into(), 0.95)]),
            "anything",
            DEFAULT_BUDGET_BYTES,
        );
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(oracle.targets(), vec!["src"]);
        assert_eq!(ids(&view), vec!["README.md".to_string()]);
    }

    #[test]
    fn tiny_budget_puts_each_directory_in_its_own_state() {
        let universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let (_, oracle) = search_one(universe, Script::new(toy(&[])), "frontend", 1);
        assert_eq!(oracle.of_kind("directory").len(), 3);
        assert_eq!(oracle.state_count(), 3, "targets {:?}", oracle.targets());
        assert!(oracle.of_kind("file").is_empty());
    }

    #[test]
    fn submit_does_not_ask_until_wait() {
        let mut session = Search::new(
            Filesystem::at_root("examples/toy-layered").unwrap(),
            Script::new(toy(&[("frontend", "keep", 0.95)])),
            DEFAULT_BUDGET_BYTES,
        );
        let ticket = session.submit("frontend");
        assert!(session.oracle.log.is_empty());
        session.wait(ticket).unwrap();
        assert_eq!(session.oracle.of_kind("directory").len(), 3);
    }

    #[test]
    fn unknown_ticket_does_not_flush() {
        let mut session = Search::new(
            Filesystem::at_root("examples/toy-layered").unwrap(),
            Script::new(toy(&[])),
            DEFAULT_BUDGET_BYTES,
        );
        let queued = session.submit("frontend");
        let err = match session.wait(Ticket(99)) {
            Err(err) => err,
            Ok(_) => panic!("unknown ticket resolved"),
        };
        assert!(err.to_string().contains("unknown ticket"));
        assert!(session.oracle.log.is_empty());
        session.wait(queued).unwrap();
        assert_eq!(session.oracle.of_kind("directory").len(), 3);
    }

    #[test]
    fn propositions_share_a_directory_state() {
        let mut oracle = Script::new(vec![]);
        for (proposition, id, choice) in [
            ("frontend", "frontend", "keep"),
            ("frontend", "backend", "prune"),
            ("frontend", "decoy", "prune"),
            ("backend", "backend", "keep"),
            ("backend", "frontend", "prune"),
            ("backend", "decoy", "prune"),
        ] {
            oracle.bind(proposition, id, choice, 0.95);
        }
        let mut session = Search::new(
            Filesystem::at_root("examples/toy-layered").unwrap(),
            oracle,
            DEFAULT_BUDGET_BYTES,
        );
        let frontend = session.submit("frontend");
        let backend = session.submit("backend");
        let left = session.wait(frontend).unwrap();
        let asked = session.oracle.log.len();
        let right = session.wait(backend).unwrap();
        assert_eq!(session.oracle.log.len(), asked);
        assert_eq!(asked, 6, "targets {:?}", session.oracle.targets());
        assert_eq!(session.oracle.state_count(), 1);
        assert!(session.oracle.of_kind("file").is_empty());
        let state = session.oracle.log[0].state.to_string();
        assert!(session
            .oracle
            .log
            .iter()
            .all(|classification| classification.state.to_string() == state));
        assert_eq!(
            ids(&left),
            vec![
                "frontend".to_string(),
                "frontend/__init__.py".to_string(),
                "frontend/app.py".to_string(),
            ]
        );
        assert_eq!(
            ids(&right),
            vec![
                "backend".to_string(),
                "backend/__init__.py".to_string(),
                "backend/api.py".to_string(),
            ]
        );
    }

    #[test]
    fn retained_ids_follows_the_prune_floor() {
        let root = scratch("replay");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a\n").unwrap();
        std::fs::write(root.join("other/b.txt"), "b\n").unwrap();
        std::fs::write(root.join("README.md"), "root\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let (view, _) = search_one(
            universe.clone(),
            Script::new(vec![
                ("src".into(), "prune".into(), 0.90),
                ("other".into(), "prune".into(), 0.60),
            ]),
            "anything",
            DEFAULT_BUDGET_BYTES,
        );
        let at_support = retained_ids(&universe, &view.trace, SUPPORT).unwrap();
        assert_eq!(at_support, ids(&view));
        assert_eq!(
            at_support,
            vec![
                "README.md".to_string(),
                "other".to_string(),
                "other/b.txt".to_string(),
            ]
        );
        let at_half = retained_ids(&universe, &view.trace, 0.50).unwrap();
        assert_eq!(at_half, vec!["README.md".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
