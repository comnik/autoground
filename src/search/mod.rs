//! Prune a [`Universe`] down to where information about a proposition is likely
//! to be found.
//!
//! [`Search::submit`] enqueues a proposition. [`Search::wait`] flushes every
//! pending proposition together: summaries are built once, and each proposition
//! adds one yes/no question for every summarizable database except the root. The
//! question asks whether that subtree is irrelevant to the proposition. The
//! shared state is the child-name tree. Each question also carries that
//! database's [`Universe::summary`]: a directory listing on a repository, the
//! prompt text on a transcript. The oracle batches questions that share a
//! state. A summarizable database drops when its own answer is irrelevant at or
//! above [`PRUNE_FLOOR`]. What remains is a clone of the universe whose window
//! is the kept databases and the readable databases under them. Bodies are not judged.

mod tui;

pub mod trace;
pub use trace::{format_steps, SearchTraceFile, UsageRecord};
pub use tui::present_search;

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::Serialize;
use serde_json::{json, Map, Value as Json};

use crate::log::Log;
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Classification, Oracle, Ticket as OracleTicket, Usage};
use crate::universe::{Universe, UniverseError};

use trace::{ExchangeRecord, SearchStep, ViewDatabase};

/// Default byte budget for one directory-listing state. About 12k tokens at
/// 4 bytes each, under Jev's 32k state-side cap.
pub const DEFAULT_BUDGET_BYTES: usize = 48 * 1024;

/// Irrelevance probability at which a directory drops. On this question a
/// 0.70 no keeps the evidence and still removes about a fifth of the tree.
pub const PRUNE_FLOOR: f64 = 0.70;

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
    /// `1 - P(irrelevant)`. Higher means information about the proposition is
    /// more likely under this database. Keep and prune are derived from this
    /// and a floor; the number does not change meaning with the label.
    pub signal: f64,
    pub applied: Applied,
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
    /// [`Universe::size`] of every reachable database, before pruning.
    pub bytes_before: usize,
    /// [`Universe::size`] of the pruned window.
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
    view: Vec<ViewDatabase>,
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

    pub fn oracle_mut(&mut self) -> &mut O {
        &mut self.oracle
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
    /// Listings are rendered once. Each proposition contributes one Noul per
    /// directory, on that same state, so the oracle can fill a request with
    /// questions from many propositions.
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
        let summarizable = match summarizable_under(&self.universe, "") {
            Ok(summarizable) => summarizable,
            Err(error) => {
                self.reject_all(pending, error.to_string());
                return;
            }
        };
        let chunks = match pack_summarizable(&self.universe, &summarizable, self.budget) {
            Ok(chunks) => chunks,
            Err(error) => {
                self.reject_all(pending, error.to_string());
                return;
            }
        };
        let mut groups = Vec::new();
        for chunk in chunks {
            let state = match child_tree(&self.universe, &chunk) {
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
            path: String,
            oracle_ticket: OracleTicket,
        }

        let names = match listing_names(&self.universe) {
            Ok(names) => names,
            Err(error) => {
                self.reject_all(pending, error.to_string());
                return;
            }
        };
        let mut listings: HashMap<String, String> = HashMap::new();
        for name in &names {
            if !name.summarizable {
                continue;
            }
            match self.universe.summary(&name.path) {
                Ok(Some(listing)) => {
                    listings.insert(name.path.clone(), listing);
                }
                Ok(None) => {
                    self.reject_all(pending, format!("{} has no summary", name.path));
                    return;
                }
                Err(error) => {
                    self.reject_all(pending, error.to_string());
                    return;
                }
            }
        }

        let mut steps: HashMap<Ticket, Vec<SearchStep>> = HashMap::new();
        let mut asks = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            for item in &pending {
                let prop_steps = steps.entry(item.ticket).or_default();
                prop_steps.push(SearchStep {
                    view: group.view.clone(),
                    classifications: Vec::new(),
                    exchange: None,
                });
                for name in names_in_group(&groups, index, &names) {
                    if !name.summarizable {
                        continue;
                    }
                    let listing = listings.get(&name.path).map(String::as_str).unwrap_or("");
                    let oracle_ticket = self.oracle.submit(irrelevant_question(
                        &group.state,
                        &name.path,
                        &item.proposition,
                        listing,
                    ));
                    asks.push(Ask {
                        search_ticket: item.ticket,
                        step: index,
                        path: name.path.clone(),
                        oracle_ticket,
                    });
                }
            }
        }

        struct Hit {
            search_ticket: Ticket,
            step: usize,
            path: String,
            noul: f64,
            exchange: Option<crate::oracle::OracleExchange>,
        }

        let mut hits = Vec::new();
        let mut failed: HashMap<Ticket, String> = HashMap::new();
        for ask in asks {
            match self.oracle.wait(ask.oracle_ticket) {
                Ok(answer) => hits.push(Hit {
                    search_ticket: ask.search_ticket,
                    step: ask.step,
                    path: ask.path,
                    noul: answer.noul.unwrap_or(0.0),
                    exchange: answer.exchange,
                }),
                Err(error) => {
                    failed
                        .entry(ask.search_ticket)
                        .or_insert_with(|| error.to_string());
                }
            }
        }

        for (index, group) in groups.iter().enumerate() {
            for item in &pending {
                if failed.contains_key(&item.ticket) {
                    continue;
                }
                let step = &mut steps.get_mut(&item.ticket).unwrap()[index];
                for id in &group.ids {
                    let irrelevant = hits
                        .iter()
                        .find(|hit| hit.search_ticket == item.ticket && hit.path == *id)
                        .map(|hit| hit.noul)
                        .unwrap_or(0.0);
                    let signal = 1.0 - irrelevant;
                    step.classifications.push(decision_from_signal(id, signal));
                }
                if step.exchange.is_none() {
                    if let Some(hit) = hits
                        .iter()
                        .find(|hit| hit.search_ticket == item.ticket && hit.step == index)
                    {
                        if let Some(exchange) = &hit.exchange {
                            step.exchange = Some(ExchangeRecord::from_oracle(exchange));
                        }
                    }
                }
            }
        }

        let usage = self.oracle.usage();
        let bytes_before = reachable_size(&self.universe).unwrap_or(0);
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
            let bytes_after = universe.size().unwrap_or(0);
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

/// [`Universe::size`] of every reachable file and directory.
fn reachable_size<U: Universe + Clone>(universe: &U) -> Result<usize, UniverseError> {
    let mut full = universe.clone();
    let mut ids = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in full.children(&dir)? {
            ids.push(child.clone());
            if summarized(&full, &child)? {
                stack.push(child);
            }
        }
    }
    full.set(ids)?;
    full.size()
}

/// Paths in the window, directories with a trailing slash.
pub fn window_lines<U: Universe>(universe: &U) -> Result<Vec<String>, UniverseError> {
    let mut lines = Vec::new();
    for id in universe.databases() {
        lines.push(display_path(id, summarized(universe, id)?));
    }
    Ok(lines)
}

fn summarized<U: Universe>(universe: &U, id: &str) -> Result<bool, UniverseError> {
    Ok(universe.summary(id)?.is_some())
}

fn summarizable_under<U: Universe>(universe: &U, root: &str) -> Result<Vec<String>, UniverseError> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_string()];
    while let Some(dir) = stack.pop() {
        for child in universe.children(&dir)? {
            if !summarized(universe, &child)? {
                continue;
            }
            out.push(child.clone());
            stack.push(child);
        }
    }
    out.sort();
    Ok(out)
}

fn readable_under<U: Universe>(
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
            if summarized(universe, &child)? {
                if !pruned.contains(&child) {
                    stack.push(child);
                }
            } else {
                out.push(child);
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
    let pruned = pruned_ids(universe, decisions);
    let mut ids: Vec<String> = decisions
        .iter()
        .filter(|decision| !pruned.contains(&decision.id))
        .map(|decision| decision.id.clone())
        .collect();
    ids.extend(readable_under(universe, &pruned)?);
    ids.sort();
    ids.dedup();
    Ok(ids)
}

fn pruned_ids<U: Universe>(universe: &U, decisions: &[Decision]) -> HashSet<String> {
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

fn pack_summarizable<U: Universe>(
    universe: &U,
    ids: &[String],
    budget: usize,
) -> Result<Vec<Vec<String>>, UniverseError> {
    pack_by(ids, budget, |chunk| {
        child_tree(universe, chunk).map(|state| json_len(&state))
    })
}

fn child_tree<U: Universe>(universe: &U, ids: &[String]) -> Result<Json, UniverseError> {
    let mut keys = vec![String::new()];
    keys.extend(ids.iter().cloned());
    keys.sort();
    keys.dedup();
    let mut map = Map::new();
    for id in keys {
        let mut names = Vec::new();
        for child in universe.children(&id)? {
            let name = child.rsplit('/').next().unwrap_or(&child);
            if summarized(universe, &child)? {
                names.push(Json::String(format!("{name}/")));
            } else {
                names.push(Json::String(name.to_string()));
            }
        }
        map.insert(id, Json::Array(names));
    }
    Ok(json!({ "tree": Json::Object(map) }))
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

fn capture<U: Universe>(universe: &U, ids: &[String]) -> Result<Vec<ViewDatabase>, UniverseError> {
    let mut view = Vec::new();
    let root = universe.summary("")?.unwrap_or_default();
    view.push(ViewDatabase {
        target: false,
        id: String::new(),
        bytes: root.len() as u64,
        listing: root,
    });
    for id in ids {
        let listing = universe.summary(id)?.unwrap_or_default();
        view.push(ViewDatabase {
            target: true,
            id: id.clone(),
            bytes: listing.len() as u64,
            listing,
        });
    }
    Ok(view)
}

/// Drop a directory when its irrelevance, `1 - signal`, is at least `floor`.
/// A live search passes [`PRUNE_FLOOR`].
fn apply_signal(signal: f64, floor: f64) -> Applied {
    if (1.0 - signal) >= floor {
        Applied::Prune
    } else {
        Applied::Keep
    }
}

/// Database ids kept when each directory's irrelevance is compared with `floor`.
///
/// `universe` is the full tree. `children` is read from disk, so a universe
/// whose window is already narrowed still yields the same ids.
pub fn retained_ids<U: Universe>(
    universe: &U,
    decisions: &[Decision],
    floor: f64,
) -> Result<Vec<String>, UniverseError> {
    let adjusted = decisions
        .iter()
        .cloned()
        .map(|mut decision| {
            decision.applied = apply_signal(decision.signal, floor);
            decision
        })
        .collect::<Vec<_>>();
    candidate_ids(universe, &adjusted)
}

const IRRELEVANT_QUESTION: &str = "Is the subtree at `path` irrelevant to `proposition`?";

fn irrelevant_criteria() -> Json {
    json!({
        "true": "A reader looking for information about the proposition would skip this subtree.",
        "false": "A reader looking for information about the proposition would open this subtree.",
    })
}

struct ListedName {
    path: String,
    summarizable: bool,
    parent: String,
}

fn listing_names<U: Universe>(universe: &U) -> Result<Vec<ListedName>, UniverseError> {
    let mut out = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        for child in universe.children(&dir)? {
            let summarizable = summarized(universe, &child)?;
            out.push(ListedName {
                path: child.clone(),
                summarizable,
                parent: dir.clone(),
            });
            if summarizable {
                stack.push(child);
            }
        }
    }
    out.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(out)
}

fn names_in_group<'a>(
    groups: &[Group],
    index: usize,
    names: &'a [ListedName],
) -> Vec<&'a ListedName> {
    names
        .iter()
        .filter(|name| group_for(groups, name) == index)
        .collect()
}

fn group_for(groups: &[Group], name: &ListedName) -> usize {
    let key = if name.summarizable {
        name.path.as_str()
    } else {
        name.parent.as_str()
    };
    if key.is_empty() {
        return 0;
    }
    groups
        .iter()
        .position(|group| group.ids.iter().any(|id| id == key))
        .unwrap_or(0)
}

fn irrelevant_question(
    state: &Json,
    path: &str,
    proposition: &str,
    listing: &str,
) -> Classification {
    let name = path.rsplit('/').next().unwrap_or(path);
    Classification::noul(
        state.clone(),
        json!({
            "path": path,
            "name": name,
            "listing": listing,
            "proposition": proposition,
            "question": IRRELEVANT_QUESTION,
        }),
        irrelevant_criteria(),
    )
}

/// `signal` is how likely it is that information about the proposition is
/// under the directory: one minus that directory's own "irrelevant" answer.
/// The listing in the question's state is already the subtree. The directory
/// drops when that irrelevance is at least [`PRUNE_FLOOR`].
fn decision_from_signal(id: &str, signal: f64) -> Decision {
    Decision {
        id: id.to_string(),
        signal,
        applied: apply_signal(signal, PRUNE_FLOOR),
    }
}

fn display_path(id: &str, summarized: bool) -> String {
    if id.is_empty() {
        "/".to_string()
    } else if summarized {
        format!("{id}/")
    } else {
        id.to_string()
    }
}

pub fn format_trace(trace: &[Decision]) -> String {
    let mut out = String::new();
    for decision in trace {
        out.push_str(&format!(
            "{:<8}{}  signal={:.2}\n",
            decision.applied.as_str(),
            display_path(&decision.id, true),
            decision.signal
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
    use crate::navigator::confidence::SUPPORT;
    use crate::oracle::{Answer, Oracle, OracleError, QuestionKind, Ticket as OracleTicket};
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

        fn paths(&self) -> Vec<&str> {
            self.log
                .iter()
                .filter_map(|classification| classification.instructions["path"].as_str())
                .collect()
        }

        fn state_count(&self) -> usize {
            self.log
                .iter()
                .map(|classification| classification.state.to_string())
                .collect::<HashSet<_>>()
                .len()
        }
    }

    fn instruction_str<'a>(classification: &'a Classification, key: &str) -> &'a str {
        classification
            .instructions
            .get(key)
            .and_then(|value| value.as_str())
            .unwrap_or("")
    }

    fn labeled_noul(labels: &HashMap<String, (String, f64)>, proposition: &str, path: &str) -> f64 {
        let mut best: Option<(usize, f64)> = None;
        for (key, (choice, confidence)) in labels {
            let (label_prop, id) = match key.split_once('\t') {
                Some(pair) => pair,
                None => ("", key.as_str()),
            };
            if !label_prop.is_empty() && label_prop != proposition {
                continue;
            }
            let hit = id == path || path.starts_with(&format!("{id}/"));
            if !hit {
                continue;
            }
            let replace = best.is_none_or(|(len, _)| id.len() > len);
            if replace {
                // The Noul is P(irrelevant). A keep label is a low score.
                let noul = if choice == "keep" {
                    1.0 - *confidence
                } else {
                    *confidence
                };
                best = Some((id.len(), noul));
            }
        }
        best.map(|(_, noul)| noul).unwrap_or(0.95)
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
                let path = instruction_str(&classification, "path");
                let proposition = instruction_str(&classification, "proposition");
                let noul = labeled_noul(&self.labels, proposition, path);
                self.resolved.insert(
                    pending_ticket,
                    Answer {
                        choice: String::new(),
                        confidence: noul,
                        noul: Some(noul),
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

    fn scratch(tag: &str) -> std::path::PathBuf {
        let root =
            std::env::temp_dir().join(format!("autoground-search-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn search_one<U: Universe + Clone>(
        universe: U,
        oracle: Script,
        proposition: &str,
        budget: usize,
    ) -> (View<U>, Script) {
        let mut session = Search::new(universe, oracle, budget);
        let ticket = session.submit(proposition);
        let view = session.wait(ticket).unwrap();
        (view, session.oracle)
    }

    fn ids<U: Universe>(view: &View<U>) -> Vec<String> {
        view.universe.databases().to_vec()
    }

    #[test]
    fn kept_directory_and_its_files_are_the_window() {
        let universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let frontend_listing = universe.summary("frontend").unwrap().unwrap();
        let (view, oracle) = search_one(
            universe,
            Script::new(toy(&[("frontend", "keep", 0.95)])),
            "there is a frontend",
            DEFAULT_BUDGET_BYTES,
        );
        assert_eq!(oracle.state_count(), 1, "paths {:?}", oracle.paths());
        assert_eq!(oracle.log.len(), 3, "one question per directory");
        assert!(oracle
            .log
            .iter()
            .all(|classification| classification.kind == QuestionKind::Noul));
        let questions: HashSet<&str> = oracle
            .log
            .iter()
            .filter_map(|classification| classification.instructions["question"].as_str())
            .collect();
        assert_eq!(questions, HashSet::from([IRRELEVANT_QUESTION]));
        assert!(oracle.paths().contains(&"frontend"));
        assert!(!oracle.paths().contains(&"frontend/app.py"));
        assert!(!oracle.paths().contains(&""));
        let sample = &oracle.log[0];
        assert!(sample.instructions["proposition"].is_string());
        assert!(sample.criteria["true"].is_string());
        assert!(sample.criteria.get("keep").is_none());
        let frontend = oracle
            .log
            .iter()
            .find(|classification| classification.instructions["path"] == "frontend")
            .unwrap();
        assert_eq!(frontend.instructions["listing"], frontend_listing);
        assert!(
            frontend_listing.starts_with("frontend/\n"),
            "{frontend_listing}"
        );
        assert_eq!(
            ids(&view),
            vec![
                "frontend".to_string(),
                "frontend/__init__.py".to_string(),
                "frontend/app.py".to_string(),
            ]
        );

        let filter = &oracle.log[0].state["tree"];
        assert!(oracle.log[0].state.get("view").is_none());
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
        let (view, _) = search_one(
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
        assert_eq!(frontend.applied, Applied::Keep);
        assert!((frontend.signal - 0.5).abs() < 1e-9);
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
    fn a_directory_answer_drops_its_subtree() {
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
        assert!(oracle.paths().contains(&"nested"));
        assert!(oracle.paths().contains(&"nested/deep"));
        assert!(!oracle.paths().contains(&"nested/deep/file.txt"));
        let nested = view
            .trace
            .iter()
            .find(|decision| decision.id == "nested")
            .unwrap();
        assert!((nested.signal - 0.01).abs() < 1e-9);
        assert_eq!(nested.applied, Applied::Prune);
        assert!(ids(&view).is_empty());
    }

    #[test]
    fn a_nested_prune_leaves_the_parent() {
        let root = scratch("nested-keep");
        std::fs::create_dir_all(root.join("nested/deep")).unwrap();
        std::fs::write(root.join("nested/keepme.txt"), "keep\n").unwrap();
        std::fs::write(root.join("nested/deep/file.txt"), "deep\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let (view, _) = search_one(
            universe,
            Script::new(vec![
                ("nested".into(), "keep".into(), 0.95),
                ("nested/deep".into(), "prune".into(), 0.99),
            ]),
            "anything",
            DEFAULT_BUDGET_BYTES,
        );
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(
            ids(&view),
            vec!["nested".to_string(), "nested/keepme.txt".to_string(),]
        );
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
        assert!(oracle.paths().contains(&"src"));
        assert!(!oracle.paths().contains(&"src/lib.rs"));
        assert!(!oracle.paths().contains(&"README.md"));
        assert_eq!(ids(&view), vec!["README.md".to_string()]);
    }

    #[test]
    fn tiny_budget_puts_each_directory_in_its_own_state() {
        let universe = Filesystem::at_root("examples/toy-layered").unwrap();
        let (_, oracle) = search_one(universe, Script::new(toy(&[])), "frontend", 1);
        assert_eq!(oracle.log.len(), 3);
        assert_eq!(oracle.state_count(), 3, "paths {:?}", oracle.paths());
        let frontend = oracle
            .log
            .iter()
            .find(|classification| classification.instructions["path"] == "frontend")
            .unwrap();
        assert!(frontend.state["tree"].get("frontend").is_some());
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
        assert_eq!(session.oracle.log.len(), 3);
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
        assert_eq!(session.oracle.log.len(), 3);
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
        assert_eq!(asked, 6, "paths {:?}", session.oracle.paths());
        assert_eq!(session.oracle.state_count(), 1);
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
        std::fs::create_dir_all(root.join("mid")).unwrap();
        std::fs::create_dir_all(root.join("other")).unwrap();
        std::fs::write(root.join("src/a.txt"), "a\n").unwrap();
        std::fs::write(root.join("mid/c.txt"), "c\n").unwrap();
        std::fs::write(root.join("other/b.txt"), "b\n").unwrap();
        std::fs::write(root.join("README.md"), "root\n").unwrap();
        let universe = Filesystem::at_root(&root).unwrap();
        let (view, _) = search_one(
            universe.clone(),
            Script::new(vec![
                ("src".into(), "prune".into(), 0.90),
                ("mid".into(), "prune".into(), 0.75),
                ("other".into(), "prune".into(), 0.60),
            ]),
            "anything",
            DEFAULT_BUDGET_BYTES,
        );
        let at_floor = retained_ids(&universe, &view.trace, PRUNE_FLOOR).unwrap();
        assert_eq!(at_floor, ids(&view));
        assert_eq!(
            at_floor,
            vec![
                "README.md".to_string(),
                "other".to_string(),
                "other/b.txt".to_string(),
            ]
        );
        let at_support = retained_ids(&universe, &view.trace, SUPPORT).unwrap();
        assert_eq!(
            at_support,
            vec![
                "README.md".to_string(),
                "mid".to_string(),
                "mid/c.txt".to_string(),
                "other".to_string(),
                "other/b.txt".to_string(),
            ]
        );
        let at_half = retained_ids(&universe, &view.trace, 0.50).unwrap();
        assert_eq!(at_half, vec!["README.md".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
