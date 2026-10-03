//! Read a proposition against a universe.
//!
//! [`Reader::submit`] enqueues a proposition. [`Reader::wait`] flushes every
//! pending proposition: one shared search, then rounds of per-file Choices.
//! A file body is the oracle state, so every proposition still looking at
//! that file shares a request. The proposition stays on the question.
//!
//! Files go out in relevance order, and every still-open proposition that
//! contains a file is asked about it in that same request. Up to
//! [`POST_PARALLELISM`] files stay in flight. When one finishes, the next
//! file starts; the reader does not wait for the rest of the window. A file
//! that finishes after an earlier one already settled the proposition does
//! not change that decision.
//!
//! The search window is a closed world. A witness closes true, a refutation
//! closes false, and exhausting the window with no witness is false. A file
//! longer than the body budget is sent in parts. Each part is a leaf, and the
//! run reports the split. The evidence view is the databases that decision
//! depends on.

pub mod trace;

pub use trace::{present_read, ReadStep, ReadTraceFile, Split};

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::confidence::{FLOOR, RESOLVE, SUPPORT};
use crate::log::Log;
use crate::oracle::jev::POST_PARALLELISM;
use crate::oracle::{Answer, Classification, Oracle, QuestionKind, State, Usage};
use crate::search::{Decision, Search, View, DEFAULT_BUDGET_BYTES};
use crate::universe::{Universe, UniverseError};
use crate::value::Value;

use trace::{ReadStep as Step, SPLIT_KIND, STEP_KIND};

const DOCUMENT_QUESTION: &str = "How does `body` at `path` ground the proposition?";
const PART_QUESTION: &str = "How does this part of `body` at `path` ground the proposition? Text outside this part is not visible.";
const ESCALATION_QUESTION: &str = "Does the proposition `proposition` hold of the codebase given the conflicting evidence `evidence`?";

/// Largest file body sent in one request.
///
/// A longer file is sent in parts. 128 KiB stays under Jev's 32k-token cap on
/// state plus the longest question for source text.
pub const DEFAULT_BODY_BYTES: usize = 128 * 1024;

/// Handle for a proposition enqueued with [`Reader::submit`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

/// How one database bears on the proposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Entailed,
    Contradicted,
    Neutral,
}

impl Verdict {
    fn label(self) -> &'static str {
        match self {
            Verdict::Entailed => "entailed",
            Verdict::Contradicted => "contradicted",
            Verdict::Neutral => "neutral",
        }
    }
}

/// One evidence database and the stance that put it there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stance {
    pub id: String,
    pub verdict: Verdict,
    pub confidence: f64,
}

/// Truth value of one proposition, plus the databases that settled it.
///
/// `exhausted` means every leaf of the candidate view was judged. A false
/// with an empty evidence view is that closed world: nothing in it entailed
/// the proposition. A false with evidence is a refutation, and the walk may
/// have stopped early.
#[derive(Debug, Clone)]
pub struct Finding<U> {
    pub proposition: String,
    pub value: Value,
    pub evidence: U,
    pub stances: Vec<Stance>,
    pub exhausted: bool,
    /// [`Universe::size`] of the search window.
    pub candidate_bytes: usize,
    /// [`Universe::size`] of the evidence view.
    pub evidence_bytes: usize,
    pub steps: Vec<ReadStep>,
    pub usage: Usage,
}

#[derive(Debug, Clone)]
pub struct ReadError {
    pub usage: Usage,
    message: String,
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ReadError {}

struct Pending {
    ticket: Ticket,
    proposition: String,
}

struct Judgement {
    /// Leaf id. A whole file uses its path. A part names the file and the part.
    id: String,
    /// Universe path. Several parts share one path.
    path: String,
    verdict: Verdict,
    confidence: f64,
    excerpt: String,
}

struct Settlement {
    value: Value,
    ids: Vec<String>,
    exhausted: bool,
}

enum Status {
    Reading,
    Settled(Settlement),
    Failed(String),
}

struct Open<U> {
    ticket: Ticket,
    proposition: String,
    candidate: U,
    candidate_bytes: usize,
    leaves: Vec<String>,
    seen: HashSet<String>,
    judged: Vec<Judgement>,
    steps: Vec<Step>,
    status: Status,
}

impl<U> Open<U> {
    fn is_reading(&self) -> bool {
        matches!(self.status, Status::Reading)
    }

    fn fail(&mut self, message: impl Into<String>) {
        if self.is_reading() {
            self.status = Status::Failed(message.into());
        }
    }

    fn settle(&mut self, value: Value, ids: Vec<String>, exhausted: bool) {
        if self.is_reading() {
            self.status = Status::Settled(Settlement {
                value,
                ids,
                exhausted,
            });
        }
    }

    fn next_leaf(&self) -> Option<&str> {
        self.leaves
            .iter()
            .find(|id| !self.seen.contains(id.as_str()))
            .map(String::as_str)
    }
}

impl<U: Universe> Open<U> {
    fn contains(&self, id: &str) -> bool {
        self.candidate
            .databases()
            .binary_search_by(|item| item.as_str().cmp(id))
            .is_ok()
    }
}

struct Flying {
    asks: Vec<Prepared>,
    tickets: Vec<crate::oracle::Ticket>,
}

struct Prepared {
    index: usize,
    /// Leaf id recorded as the step path and the stance id.
    id: String,
    /// Universe path stored on the judgement and in oracle state.
    path: String,
    proposition: String,
    rendered_bytes: u64,
    excerpt: String,
    state: State,
    part: bool,
}

/// One slice of a file body, cached so every proposition shares it.
struct Piece {
    path: String,
    body: String,
    /// 1-based index and total. Absent when the file was sent whole.
    part: Option<(u32, u32)>,
}

enum Action {
    Continue,
    Escalate,
    Settle(Value, Vec<String>, bool),
}

/// A universe and an oracle shared by every proposition submitted here.
/// `submit` only enqueues. `wait` searches, then reads.
pub struct Reader<U, O> {
    search: Search<U, O>,
    /// Maximum file body, in bytes, sent to the oracle.
    body_budget: usize,
    next: u64,
    pending: Vec<Pending>,
    resolved: HashMap<Ticket, Result<Finding<U>, ReadError>>,
    /// Leaf id to the body posted for it.
    pieces: HashMap<String, Piece>,
    splits: Vec<Split>,
    log: Option<Log>,
}

impl<U, O> Reader<U, O> {
    /// `body_budget` is the largest file body sent in one request. A longer
    /// body is sent in parts. Directory listings use [`DEFAULT_BUDGET_BYTES`],
    /// which is a separate cap.
    pub fn new(universe: U, oracle: O, body_budget: usize, log: Option<Log>) -> Self {
        Self {
            search: Search::new(universe, oracle, DEFAULT_BUDGET_BYTES, log.clone()),
            body_budget,
            next: 0,
            pending: Vec::new(),
            resolved: HashMap::new(),
            pieces: HashMap::new(),
            splits: Vec::new(),
            log,
        }
    }

    /// Files sent in parts, in the order they were split.
    pub fn splits(&self) -> &[Split] {
        &self.splits
    }

    pub fn usage(&self) -> Usage
    where
        O: Oracle,
    {
        self.search.usage()
    }

    pub fn flights(&self) -> &[crate::oracle::Flight]
    where
        O: Oracle,
    {
        self.search.flights()
    }

    pub fn flight_limit(&self) -> Option<usize>
    where
        O: Oracle,
    {
        self.search.flight_limit()
    }

    pub fn into_oracle(self) -> O {
        self.search.into_oracle()
    }
}

impl<U, O> Reader<U, O>
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

    /// Flush every pending proposition, then return this one's finding.
    ///
    /// Search listings are built once. Document questions then share a file
    /// body across every proposition whose candidate view still contains it.
    pub fn wait(&mut self, ticket: Ticket) -> Result<Finding<U>, ReadError> {
        if let Some(done) = self.resolved.get(&ticket) {
            return done.clone();
        }
        if !self.pending.iter().any(|item| item.ticket == ticket) {
            return Err(ReadError {
                usage: self.search.usage(),
                message: format!("unknown ticket {}", ticket.0),
            });
        }
        self.flush();
        self.resolved.get(&ticket).cloned().unwrap_or_else(|| {
            Err(ReadError {
                usage: self.search.usage(),
                message: format!("missing ticket {}", ticket.0),
            })
        })
    }

    pub fn wait_many(&mut self, tickets: &[Ticket]) -> Vec<Result<Finding<U>, ReadError>> {
        tickets
            .iter()
            .copied()
            .map(|ticket| self.wait(ticket))
            .collect()
    }

    fn log_step(&self, step: &Step) {
        let Some(log) = &self.log else {
            return;
        };
        let _ = log.append_event(STEP_KIND, step);
    }

    fn flush(&mut self) {
        let pending = std::mem::take(&mut self.pending);
        if pending.is_empty() {
            return;
        }
        let mut early = Vec::new();
        let mut opens = Vec::new();
        let mut search_tickets = Vec::new();
        for item in &pending {
            search_tickets.push((item.ticket, self.search.submit(item.proposition.clone())));
        }
        for (ticket, search_ticket) in search_tickets {
            match self.search.wait(search_ticket) {
                Ok(view) => match Open::from_view(ticket, view) {
                    Ok(open) => opens.push(open),
                    Err(message) => early.push((ticket, message)),
                },
                Err(error) => early.push((ticket, error.to_string())),
            }
        }
        self.rounds(&mut opens);
        let usage = self.search.usage();
        for (ticket, message) in early {
            self.resolved
                .insert(ticket, Err(ReadError { usage, message }));
        }
        for open in opens {
            let (ticket, result) = open.into_result(usage);
            self.resolved.insert(ticket, result);
        }
    }

    fn rounds(&mut self, opens: &mut [Open<U>]) {
        for open in opens.iter_mut() {
            if open.is_reading() && open.leaves.is_empty() {
                open.settle(Value::False, Vec::new(), true);
            }
        }
        let mut flying: Vec<Flying> = Vec::new();
        let mut blocked: HashSet<String> = HashSet::new();
        loop {
            if opens.iter().all(|open| !open.is_reading()) {
                break;
            }
            self.fill(opens, &mut flying, &mut blocked);
            if flying.is_empty() {
                self.settle_ready(opens);
                for open in opens.iter_mut() {
                    if open.is_reading() {
                        open.settle(Value::False, Vec::new(), true);
                    }
                }
                break;
            }
            let tickets: Vec<_> = flying
                .iter()
                .flat_map(|file| file.tickets.iter().copied())
                .collect();
            if let Err(error) = self.search.oracle_mut().wait_any(&tickets) {
                let message = error.to_string();
                for open in opens.iter_mut() {
                    if open.is_reading() {
                        open.fail(message.clone());
                    }
                }
                break;
            }
            let ready: HashSet<_> = self
                .search
                .oracle_mut()
                .completed(&tickets)
                .into_iter()
                .collect();
            let mut still = Vec::new();
            for file in flying.drain(..) {
                if file.tickets.iter().all(|ticket| ready.contains(ticket)) {
                    self.record_file(opens, file);
                } else {
                    still.push(file);
                }
            }
            flying = still;
            self.settle_ready(opens);
        }
    }

    /// Submit the next relevant files until [`POST_PARALLELISM`] are outstanding.
    fn fill(
        &mut self,
        opens: &mut [Open<U>],
        flying: &mut Vec<Flying>,
        blocked: &mut HashSet<String>,
    ) {
        while flying.len() < POST_PARALLELISM {
            let mut skip = blocked.clone();
            for file in flying.iter() {
                if let Some(ask) = file.asks.first() {
                    skip.insert(ask.id.clone());
                }
            }
            let Some(path) = next_file(opens, &skip) else {
                break;
            };
            let prepared = match self.prepare(opens, &path) {
                Ok(prepared) => prepared,
                Err(()) => {
                    blocked.insert(path);
                    continue;
                }
            };
            if prepared.is_empty() {
                blocked.insert(path);
                continue;
            }
            let tickets = prepared
                .iter()
                .map(|ask| {
                    self.search.oracle_mut().submit(document_question(
                        &ask.state,
                        &ask.proposition,
                        ask.part,
                    ))
                })
                .collect();
            flying.push(Flying {
                asks: prepared,
                tickets,
            });
        }
    }

    fn record_file(&mut self, opens: &mut [Open<U>], file: Flying) {
        for (ask, ticket) in file.asks.into_iter().zip(file.tickets) {
            let answer = self.search.oracle_mut().wait(ticket);
            if !opens[ask.index].is_reading() {
                continue;
            }
            match answer {
                Ok(answer) => match parse_verdict(&answer.choice) {
                    Ok(verdict) => {
                        let step = self.step_for(&ask, verdict, &answer, ticket);
                        opens[ask.index].record(
                            step,
                            &ask.path,
                            verdict,
                            answer.confidence,
                            ask.excerpt,
                        );
                    }
                    Err(message) => opens[ask.index].fail(message),
                },
                Err(error) => opens[ask.index].fail(error.to_string()),
            }
        }
    }

    /// Apply the witness / refutation rule to whatever has been judged.
    fn settle_ready(&mut self, opens: &mut [Open<U>]) {
        let mut escalate = Vec::new();
        for (index, open) in opens.iter_mut().enumerate() {
            if !open.is_reading() {
                continue;
            }
            match open.action() {
                Action::Continue => {}
                Action::Escalate => escalate.push(index),
                Action::Settle(value, ids, exhausted) => open.settle(value, ids, exhausted),
            }
        }
        if !escalate.is_empty() {
            self.escalate(opens, &escalate);
        }
    }

    /// Post `leaf`, splitting its file into parts the first time it is read.
    ///
    /// `Err` means the body could not be read and the claims that contain it
    /// have failed. An empty `Ok` means no still-open claim wants this leaf.
    fn prepare(&mut self, opens: &mut [Open<U>], leaf: &str) -> Result<Vec<Prepared>, ()> {
        if self.pieces.contains_key(leaf) {
            return Ok(self.asks_for(opens, leaf));
        }
        let body = match read_whole(opens, leaf) {
            Ok(body) => body,
            Err(message) => {
                for open in opens.iter_mut() {
                    if open.is_reading() && open.contains(leaf) && !open.seen.contains(leaf) {
                        open.fail(message.clone());
                    }
                }
                return Err(());
            }
        };
        let parts = split_body(&body, self.body_budget);
        if parts.len() <= 1 {
            self.pieces.insert(
                leaf.to_string(),
                Piece {
                    path: leaf.to_string(),
                    body,
                    part: None,
                },
            );
            return Ok(self.asks_for(opens, leaf));
        }
        let total = parts.len() as u32;
        self.note_split(Split {
            path: leaf.to_string(),
            bytes: body.len() as u64,
            parts: u64::from(total),
        });
        for (offset, part) in parts.into_iter().enumerate() {
            let index = (offset as u32) + 1;
            self.pieces.insert(
                part_id(leaf, index, total),
                Piece {
                    path: leaf.to_string(),
                    body: part,
                    part: Some((index, total)),
                },
            );
        }
        for open in opens.iter_mut() {
            if open.is_reading() {
                splice_parts(&mut open.leaves, leaf, total);
            }
        }
        Ok(self.asks_for(opens, &part_id(leaf, 1, total)))
    }

    fn asks_for(&self, opens: &[Open<U>], leaf: &str) -> Vec<Prepared> {
        let Some(piece) = self.pieces.get(leaf) else {
            return Vec::new();
        };
        let part = piece.part.is_some();
        let state = match piece.part {
            Some((index, total)) => State::new(json!({
                "path": piece.path,
                "body": piece.body,
                "part": index,
                "parts": total,
            })),
            None => State::new(json!({ "path": piece.path, "body": piece.body })),
        };
        let rendered_bytes = piece.body.len() as u64;
        let mut prepared = Vec::new();
        for (index, open) in opens.iter().enumerate() {
            if open.is_reading() && open.contains(&piece.path) && !open.seen.contains(leaf) {
                prepared.push(Prepared {
                    index,
                    id: leaf.to_string(),
                    path: piece.path.clone(),
                    proposition: open.proposition.clone(),
                    rendered_bytes,
                    excerpt: piece.body.clone(),
                    state: state.clone(),
                    part,
                });
            }
        }
        prepared
    }

    fn note_split(&mut self, split: Split) {
        if let Some(log) = &self.log {
            let _ = log.append_event(SPLIT_KIND, &split);
        }
        self.splits.push(split);
    }

    fn step_for(
        &self,
        ask: &Prepared,
        verdict: Verdict,
        answer: &Answer,
        ticket: crate::oracle::Ticket,
    ) -> Step {
        let step = Step {
            proposition: ask.proposition.clone(),
            path: ask.id.clone(),
            verdict: verdict.label().to_string(),
            confidence: answer.confidence,
            rendered_bytes: ask.rendered_bytes,
            escalation: false,
            ticket: ticket.0,
        };
        self.log_step(&step);
        step
    }

    fn escalate(&mut self, opens: &mut [Open<U>], indexes: &[usize]) {
        let mut jobs = Vec::new();
        for &index in indexes {
            let open = &opens[index];
            let rows = conflict_rows(open);
            let ticket = self
                .search
                .oracle_mut()
                .submit(escalation_question(&open.proposition, &rows));
            jobs.push((index, ticket));
        }
        for (index, ticket) in jobs {
            let answer = self.search.oracle_mut().wait(ticket);
            let open = &mut opens[index];
            if !open.is_reading() {
                continue;
            }
            match answer {
                Ok(answer) => match parse_resolution(&answer.choice) {
                    Ok(mut value) => {
                        if value != Value::Unknown && answer.confidence < RESOLVE {
                            value = Value::Unknown;
                        }
                        let step = Step {
                            proposition: open.proposition.clone(),
                            path: String::new(),
                            verdict: value.to_string(),
                            confidence: answer.confidence,
                            rendered_bytes: 0,
                            escalation: true,
                            ticket: ticket.0,
                        };
                        self.log_step(&step);
                        open.steps.push(step);
                        let ids = conflict_ids(open);
                        let exhausted = open.decision_reached_the_end(&open.decisive_prefix());
                        open.settle(value, ids, exhausted);
                    }
                    Err(message) => open.fail(message),
                },
                Err(error) => open.fail(error.to_string()),
            }
        }
    }
}

impl<U: Universe> Open<U> {
    fn from_view(ticket: Ticket, view: View<U>) -> Result<Self, String> {
        let leaves = ranked_leaves(&view.universe, &view.trace).map_err(|err| err.to_string())?;
        Ok(Self {
            ticket,
            proposition: view.proposition,
            candidate_bytes: view.bytes_after,
            candidate: view.universe,
            leaves,
            seen: HashSet::new(),
            judged: Vec::new(),
            steps: Vec::new(),
            status: Status::Reading,
        })
    }

    fn record(
        &mut self,
        step: Step,
        path: &str,
        verdict: Verdict,
        confidence: f64,
        excerpt: String,
    ) {
        self.seen.insert(step.path.clone());
        self.judged.push(Judgement {
            id: step.path.clone(),
            path: path.to_string(),
            verdict,
            confidence,
            excerpt,
        });
        self.steps.push(step);
    }

    fn action(&self) -> Action {
        let judged = self.decisive_prefix();
        let witness = judged
            .iter()
            .any(|item| is_witness(item.verdict, item.confidence));
        let veto = judged
            .iter()
            .any(|item| vetoes(item.verdict, item.confidence));
        let refutation = judged
            .iter()
            .any(|item| is_refutation(item.verdict, item.confidence));
        let exhausted = self.decision_reached_the_end(&judged);
        if witness && veto {
            Action::Escalate
        } else if witness {
            Action::Settle(Value::True, ids_matching(&judged, is_witness), exhausted)
        } else if refutation {
            Action::Settle(
                Value::False,
                ids_matching(&judged, is_refutation),
                exhausted,
            )
        } else if self.next_leaf().is_some() {
            Action::Continue
        } else {
            Action::Settle(Value::False, Vec::new(), true)
        }
    }

    /// Judgements in leaf order, through the first file that settles or escalates.
    ///
    /// A round may read later files at the same time. Those requests already
    /// happened; they do not move the decision past the earlier file.
    fn decisive_prefix(&self) -> Vec<&Judgement> {
        let mut out = Vec::new();
        for id in &self.leaves {
            let Some(item) = self.judged.iter().find(|item| item.id == *id) else {
                break;
            };
            out.push(item);
            if stops(&out) {
                break;
            }
        }
        out
    }

    /// True when the decision used the last leaf, not when a speculative read
    /// happened to judge files past that point.
    fn decision_reached_the_end(&self, prefix: &[&Judgement]) -> bool {
        let Some(last) = prefix.last() else {
            return self.leaves.is_empty();
        };
        self.leaves.last().is_some_and(|id| id == &last.id)
    }
}

impl<U: Universe + Clone> Open<U> {
    fn into_result(self, usage: Usage) -> (Ticket, Result<Finding<U>, ReadError>) {
        let ticket = self.ticket;
        match self.status {
            Status::Failed(message) => (ticket, Err(ReadError { usage, message })),
            Status::Reading => (
                ticket,
                Err(ReadError {
                    usage,
                    message: format!("{} was not settled", self.proposition),
                }),
            ),
            Status::Settled(settlement) => {
                let mut evidence = self.candidate.clone();
                let paths = universe_paths(&self.judged, &settlement.ids);
                if let Err(error) = evidence.set(paths) {
                    return (
                        ticket,
                        Err(ReadError {
                            usage,
                            message: error.to_string(),
                        }),
                    );
                }
                let evidence_bytes = evidence.size().unwrap_or(0);
                let stances = stances_for(&self.judged, &settlement.ids);
                (
                    ticket,
                    Ok(Finding {
                        proposition: self.proposition,
                        value: settlement.value,
                        evidence,
                        stances,
                        exhausted: settlement.exhausted,
                        candidate_bytes: self.candidate_bytes,
                        evidence_bytes,
                        steps: self.steps,
                        usage,
                    }),
                )
            }
        }
    }
}

fn read_whole<U: Universe>(opens: &[Open<U>], path: &str) -> Result<String, String> {
    let open = opens
        .iter()
        .find(|open| open.is_reading() && open.contains(path))
        .ok_or_else(|| format!("no proposition contains {path}"))?;
    open.candidate
        .read(path, usize::MAX)
        .map_err(|err| err.to_string())
}

/// Leaf id for one part. The run prints this as the evidence id.
fn part_id(path: &str, index: u32, total: u32) -> String {
    format!("{path} part {index}/{total}")
}

fn splice_parts(leaves: &mut Vec<String>, path: &str, total: u32) {
    let Some(pos) = leaves.iter().position(|id| id == path) else {
        return;
    };
    let parts = (1..=total).map(|index| part_id(path, index, total));
    leaves.splice(pos..=pos, parts);
}

/// Split `text` into the fewest pieces that each fit in `budget`.
///
/// A piece ends on a newline when one fits. A line longer than `budget` is
/// cut on a character boundary. The pieces concatenate back to `text`.
fn split_body(text: &str, budget: usize) -> Vec<String> {
    if text.len() <= budget {
        return vec![text.to_string()];
    }
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= budget {
            parts.push(rest.to_string());
            break;
        }
        let cut = cut_at(rest, budget);
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts
}

fn cut_at(text: &str, budget: usize) -> usize {
    let end = floor_char_boundary(text, budget.min(text.len()));
    if end == 0 {
        return text.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    }
    if let Some(newline) = text[..end].rfind('\n') {
        let cut = newline + 1;
        if cut < text.len() {
            return cut;
        }
    }
    end
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn universe_paths(judged: &[Judgement], ids: &[String]) -> Vec<String> {
    let mut paths = Vec::new();
    for id in ids {
        let Some(item) = judged.iter().find(|item| item.id == *id) else {
            continue;
        };
        if !paths.contains(&item.path) {
            paths.push(item.path.clone());
        }
    }
    paths
}

/// The next unread file in round-robin over each proposition's ranked leaves.
///
/// Heads come first, so every proposition's most relevant file is submitted
/// before anyone's second file. `skip` is the set already in flight.
fn next_file<U>(opens: &[Open<U>], skip: &HashSet<String>) -> Option<String> {
    let mut cursors = vec![0usize; opens.len()];
    loop {
        let mut progressed = false;
        for (open, cursor) in opens.iter().zip(cursors.iter_mut()) {
            if !open.is_reading() {
                continue;
            }
            let Some(id) = take_unseen(open, cursor) else {
                continue;
            };
            progressed = true;
            if !skip.contains(id) {
                return Some(id.to_string());
            }
        }
        if !progressed {
            return None;
        }
    }
}

fn take_unseen<'a, U>(open: &'a Open<U>, cursor: &mut usize) -> Option<&'a str> {
    while *cursor < open.leaves.len() {
        let id = open.leaves[*cursor].as_str();
        *cursor += 1;
        if !open.seen.contains(id) {
            return Some(id);
        }
    }
    None
}

fn document_question(state: &State, proposition: &str, part: bool) -> Classification {
    Classification {
        state: state.clone(),
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": proposition,
            "question": if part { PART_QUESTION } else { DOCUMENT_QUESTION },
        }),
        criteria: json!({
            "entailed": "`body` at `path` supplies information that makes the proposition true",
            "contradicted": "`body` at `path` supplies information that makes the proposition false",
            "neutral": "`body` at `path` neither supports nor refutes the proposition",
        }),
    }
}

fn escalation_question(proposition: &str, evidence: &[Json]) -> Classification {
    Classification {
        state: State::new(json!({
            "proposition": proposition,
            "evidence": evidence,
        })),
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": proposition,
            "question": ESCALATION_QUESTION,
        }),
        criteria: json!({
            "true": "the proposition holds of the codebase",
            "false": "the proposition does not hold of the codebase",
            "unknown": "the evidence is insufficient to decide",
        }),
    }
}

fn conflict_rows<U: Universe>(open: &Open<U>) -> Vec<Json> {
    let mut rows = Vec::new();
    for item in open.decisive_prefix() {
        if is_witness(item.verdict, item.confidence) || vetoes(item.verdict, item.confidence) {
            rows.push(json!({
                "path": item.id,
                "verdict": item.verdict.label(),
                "confidence": item.confidence,
                "excerpt": item.excerpt,
            }));
        }
    }
    rows
}

fn conflict_ids<U: Universe>(open: &Open<U>) -> Vec<String> {
    let judged = open.decisive_prefix();
    let mut ids = ids_matching(&judged, is_witness);
    ids.extend(ids_matching(&judged, vetoes));
    ids.sort();
    ids.dedup();
    ids
}

fn ids_matching(judged: &[&Judgement], pred: fn(Verdict, f64) -> bool) -> Vec<String> {
    let mut ids: Vec<String> = judged
        .iter()
        .filter(|item| pred(item.verdict, item.confidence))
        .map(|item| item.id.clone())
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

fn stops(judged: &[&Judgement]) -> bool {
    let witness = judged
        .iter()
        .any(|item| is_witness(item.verdict, item.confidence));
    let veto = judged
        .iter()
        .any(|item| vetoes(item.verdict, item.confidence));
    let refutation = judged
        .iter()
        .any(|item| is_refutation(item.verdict, item.confidence));
    (witness && veto) || witness || refutation
}

fn stances_for(judged: &[Judgement], ids: &[String]) -> Vec<Stance> {
    ids.iter()
        .filter_map(|id| {
            judged
                .iter()
                .find(|item| item.id == *id)
                .map(|item| Stance {
                    id: id.clone(),
                    verdict: item.verdict,
                    confidence: item.confidence,
                })
        })
        .collect()
}

fn is_witness(verdict: Verdict, confidence: f64) -> bool {
    verdict == Verdict::Entailed && confidence >= SUPPORT
}

fn vetoes(verdict: Verdict, confidence: f64) -> bool {
    verdict == Verdict::Contradicted && confidence >= FLOOR
}

fn is_refutation(verdict: Verdict, confidence: f64) -> bool {
    verdict == Verdict::Contradicted && confidence >= SUPPORT
}

fn parse_verdict(choice: &str) -> Result<Verdict, String> {
    match choice {
        "entailed" => Ok(Verdict::Entailed),
        "contradicted" => Ok(Verdict::Contradicted),
        "neutral" => Ok(Verdict::Neutral),
        other => Err(format!("unknown document verdict `{other}`")),
    }
}

fn parse_resolution(choice: &str) -> Result<Value, String> {
    match choice {
        "true" => Ok(Value::True),
        "false" => Ok(Value::False),
        "unknown" => Ok(Value::Unknown),
        other => Err(format!("unknown resolution `{other}`")),
    }
}

fn ranked_leaves<U: Universe>(
    universe: &U,
    decisions: &[Decision],
) -> Result<Vec<String>, UniverseError> {
    let mut signals: HashMap<&str, f64> = HashMap::new();
    for decision in decisions {
        signals.insert(decision.id.as_str(), decision.signal);
    }
    let mut leaves = Vec::new();
    for id in universe.databases() {
        if universe.summary(id)?.is_some() {
            continue;
        }
        let signal = match universe.parent(id) {
            Some(parent) if !parent.is_empty() => signals.get(parent.as_str()).copied(),
            _ => None,
        };
        leaves.push((signal, id.clone()));
    }
    leaves.sort_by(cmp_leaf);
    Ok(leaves.into_iter().map(|(_, id)| id).collect())
}

fn cmp_leaf(a: &(Option<f64>, String), b: &(Option<f64>, String)) -> std::cmp::Ordering {
    match (a.0, b.0) {
        (Some(left), Some(right)) => right.total_cmp(&left).then_with(|| a.1.cmp(&b.1)),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.1.cmp(&b.1),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;

    use super::*;
    use crate::log::Log;
    use crate::oracle::{HeuristicOracle, OracleError};
    use crate::universe::Filesystem;

    struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(files: &[(&str, &str)]) -> (Scratch, Filesystem) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "autoground-read-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        for (rel, body) in files {
            let path = root.join(rel);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, body).unwrap();
        }
        let fs = Filesystem::at_root(&root).unwrap();
        (Scratch(root), fs)
    }

    #[derive(Clone)]
    struct Scripted {
        inner: Rc<RefCell<ScriptInner>>,
    }

    struct ScriptInner {
        next: u64,
        pending: Vec<(crate::oracle::Ticket, Answer)>,
        resolved: HashMap<crate::oracle::Ticket, Answer>,
        submitted: Vec<Classification>,
        nouls: HashMap<(String, String), f64>,
        documents: HashMap<(String, String), (String, f64)>,
        escalation: (String, f64),
        escalations: usize,
    }

    impl Scripted {
        fn new() -> Self {
            Self {
                inner: Rc::new(RefCell::new(ScriptInner {
                    next: 0,
                    pending: Vec::new(),
                    resolved: HashMap::new(),
                    submitted: Vec::new(),
                    nouls: HashMap::new(),
                    documents: HashMap::new(),
                    escalation: ("unknown".into(), 0.95),
                    escalations: 0,
                })),
            }
        }

        fn noul(self, path: &str, proposition: &str, noul: f64) -> Self {
            self.inner
                .borrow_mut()
                .nouls
                .insert((path.into(), proposition.into()), noul);
            self
        }

        fn document(self, path: &str, proposition: &str, choice: &str, confidence: f64) -> Self {
            self.inner.borrow_mut().documents.insert(
                (path.into(), proposition.into()),
                (choice.into(), confidence),
            );
            self
        }

        fn part(
            self,
            path: &str,
            part: u64,
            proposition: &str,
            choice: &str,
            confidence: f64,
        ) -> Self {
            self.inner.borrow_mut().documents.insert(
                (format!("{path}#{part}"), proposition.into()),
                (choice.into(), confidence),
            );
            self
        }

        fn escalation(self, choice: &str, confidence: f64) -> Self {
            self.inner.borrow_mut().escalation = (choice.into(), confidence);
            self
        }
    }

    impl Oracle for Scripted {
        fn submit(&mut self, req: Classification) -> crate::oracle::Ticket {
            let mut inner = self.inner.borrow_mut();
            let ticket = crate::oracle::Ticket(inner.next);
            inner.next += 1;
            let answer = scripted_answer(&inner, &req);
            if req.kind == QuestionKind::Choice && req.criteria.get("entailed").is_none() {
                inner.escalations += 1;
            }
            inner.submitted.push(req);
            inner.pending.push((ticket, answer));
            ticket
        }

        fn wait(&mut self, ticket: crate::oracle::Ticket) -> Result<Answer, OracleError> {
            let mut inner = self.inner.borrow_mut();
            if let Some(answer) = inner.resolved.get(&ticket) {
                return Ok(answer.clone());
            }
            let pending = std::mem::take(&mut inner.pending);
            for (pending, answer) in pending {
                inner.resolved.insert(pending, answer);
            }
            inner
                .resolved
                .get(&ticket)
                .cloned()
                .ok_or_else(|| OracleError::Parse(format!("unknown ticket {}", ticket.0)))
        }
    }

    fn scripted_answer(inner: &ScriptInner, req: &Classification) -> Answer {
        let proposition = json_str(&req.instructions, "proposition");
        let path = asked_path(req);
        match req.kind {
            QuestionKind::Noul => {
                let noul = inner
                    .nouls
                    .get(&(path.to_string(), proposition.to_string()))
                    .copied()
                    .unwrap_or(0.05);
                Answer {
                    choice: String::new(),
                    confidence: noul,
                    noul: Some(noul),
                }
            }
            QuestionKind::Choice if req.criteria.get("entailed").is_some() => {
                let (choice, confidence) = inner
                    .documents
                    .get(&document_key(req))
                    .cloned()
                    .unwrap_or_else(|| ("neutral".into(), 0.95));
                Answer {
                    choice,
                    confidence,
                    noul: None,
                }
            }
            QuestionKind::Choice => Answer {
                choice: inner.escalation.0.clone(),
                confidence: inner.escalation.1,
                noul: None,
            },
        }
    }

    fn document_key(req: &Classification) -> (String, String) {
        let proposition = json_str(&req.instructions, "proposition").to_string();
        let mut path = asked_path(req).to_string();
        if let Some(part) = req.state.get("part").and_then(|value| value.as_u64()) {
            path = format!("{path}#{part}");
        }
        (path, proposition)
    }

    fn asked_path(req: &Classification) -> &str {
        let on_question = json_str(&req.instructions, "path");
        if on_question.is_empty() {
            json_str(&req.state, "path")
        } else {
            on_question
        }
    }

    fn json_str<'a>(value: &'a Json, key: &str) -> &'a str {
        value.get(key).and_then(|item| item.as_str()).unwrap_or("")
    }

    fn read_with(fs: Filesystem, oracle: Scripted) -> (Reader<Filesystem, Scripted>, Scripted) {
        let shared = oracle.clone();
        let reader = Reader::new(fs, oracle, DEFAULT_BODY_BYTES, None);
        (reader, shared)
    }

    fn document_paths(script: &Scripted, proposition: &str) -> Vec<String> {
        script
            .inner
            .borrow()
            .submitted
            .iter()
            .filter(|req| {
                req.kind == QuestionKind::Choice && req.criteria.get("entailed").is_some()
            })
            .filter(|req| json_str(&req.instructions, "proposition") == proposition)
            .map(|req| json_str(&req.state, "path").to_string())
            .collect()
    }

    #[test]
    fn unknown_ticket_does_not_flush() {
        let (_scratch, fs) = scratch(&[("a.txt", "alpha")]);
        let oracle = Scripted::new();
        let (mut reader, script) = read_with(fs, oracle);
        let pending = reader.submit("alpha");
        match reader.wait(Ticket(99)) {
            Err(err) => assert!(err.to_string().contains("unknown"), "{err}"),
            Ok(_) => panic!("unknown ticket resolved"),
        }
        assert!(script.inner.borrow().submitted.is_empty());
        let finding = reader.wait(pending).unwrap();
        assert_eq!(finding.value, Value::False);
    }

    #[test]
    fn witness_ignores_a_later_file_read_in_the_same_round() {
        let (_scratch, fs) = scratch(&[("a.txt", "one"), ("b.txt", "two")]);
        let oracle = Scripted::new()
            .document("a.txt", "p", "entailed", 0.95)
            .document("b.txt", "p", "entailed", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::True);
        assert_eq!(finding.evidence.databases(), &["a.txt".to_string()]);
        assert!(!finding.exhausted);
        assert_eq!(
            document_paths(&script, "p"),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
    }

    #[test]
    fn refutation_ignores_a_later_witness_read_in_the_same_round() {
        let (_scratch, fs) = scratch(&[("a.txt", "one"), ("b.txt", "two")]);
        let oracle = Scripted::new()
            .document("a.txt", "p", "contradicted", 0.9)
            .document("b.txt", "p", "entailed", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        assert_eq!(finding.evidence.databases(), &["a.txt".to_string()]);
        assert!(!finding.exhausted);
        assert_eq!(
            document_paths(&script, "p"),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        assert_eq!(script.inner.borrow().escalations, 0);
    }

    #[test]
    fn a_shared_next_file_reads_ahead_to_the_post_cap() {
        let owned: Vec<(String, String)> = (0..20)
            .map(|i| (format!("{i:02}.txt"), "x".to_string()))
            .collect();
        let pairs: Vec<(&str, &str)> = owned
            .iter()
            .map(|(name, body)| (name.as_str(), body.as_str()))
            .collect();
        let (_scratch, fs) = scratch(&pairs);
        let oracle = Scripted::new().document("00.txt", "p", "entailed", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let first = reader.submit("p");
        let second = reader.submit("p");
        let found = reader.wait_many(&[first, second]);
        assert!(found.iter().all(|finding| {
            let finding = finding.as_ref().unwrap();
            finding.value == Value::True
                && finding.evidence.databases() == ["00.txt".to_string()]
                && !finding.exhausted
        }));
        let paths = document_paths(&script, "p");
        let mut unique = paths.clone();
        unique.sort();
        unique.dedup();
        let want: Vec<String> = (0..16).map(|i| format!("{i:02}.txt")).collect();
        assert_eq!(unique, want, "{paths:?}");
        assert_eq!(paths.len(), 32, "{paths:?}");
    }

    #[test]
    fn weak_entailed_is_not_a_witness() {
        let (_scratch, fs) = scratch(&[("a.txt", "one")]);
        let oracle = Scripted::new().document("a.txt", "p", "entailed", 0.84);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        assert!(finding.exhausted);
        assert!(finding.evidence.databases().is_empty());
        assert!(finding.stances.is_empty());
        assert_eq!(document_paths(&script, "p"), vec!["a.txt".to_string()]);
        assert_eq!(finding.steps[0].verdict, "entailed");
    }

    #[test]
    fn veto_does_not_close_false_before_exhaustion() {
        let (_scratch, fs) = scratch(&[("a.txt", "one"), ("b.txt", "two")]);
        let oracle = Scripted::new()
            .document("a.txt", "p", "contradicted", 0.6)
            .document("b.txt", "p", "neutral", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        assert!(finding.exhausted);
        assert!(finding.evidence.databases().is_empty());
        assert_eq!(
            document_paths(&script, "p"),
            vec!["a.txt".to_string(), "b.txt".to_string()]
        );
        assert_eq!(script.inner.borrow().escalations, 0);
    }

    #[test]
    fn witness_and_veto_escalate_once() {
        let (_scratch, fs) = scratch(&[("veto.txt", "no"), ("witness.txt", "yes")]);
        let oracle = Scripted::new()
            .document("veto.txt", "p", "contradicted", 0.6)
            .document("witness.txt", "p", "entailed", 0.9)
            .escalation("true", 0.9);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::True);
        assert_eq!(script.inner.borrow().escalations, 1);
        assert_eq!(
            finding.evidence.databases(),
            &["veto.txt".to_string(), "witness.txt".to_string()]
        );
        assert!(finding.steps.iter().any(|step| step.escalation));
        let escalation = script
            .inner
            .borrow()
            .submitted
            .iter()
            .find(|req| req.kind == QuestionKind::Choice && req.criteria.get("true").is_some())
            .unwrap()
            .clone();
        assert_eq!(json_str(&escalation.state, "proposition"), "p");
        assert!(escalation.state.get("evidence").is_some());
        assert!(escalation.state.get("body").is_none());
        let question = json_str(&escalation.instructions, "question");
        assert!(question.contains("`proposition`"), "{question}");
        assert!(question.contains("`evidence`"), "{question}");
        assert_eq!(json_str(&escalation.instructions, "proposition"), "p");
    }

    #[test]
    fn escalation_below_resolve_stays_unknown() {
        let (_scratch, fs) = scratch(&[("veto.txt", "no"), ("witness.txt", "yes")]);
        let oracle = Scripted::new()
            .document("veto.txt", "p", "contradicted", 0.6)
            .document("witness.txt", "p", "entailed", 0.9)
            .escalation("true", 0.5);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::Unknown);
        assert_eq!(script.inner.borrow().escalations, 1);
        assert_eq!(finding.evidence.databases().len(), 2);
    }

    #[test]
    fn nominated_file_is_asked_of_every_proposition_that_contains_it() {
        let (_scratch, fs) = scratch(&[
            ("hi/a.txt", "alpha file"),
            ("hi/shared.txt", "shared"),
            ("lo/b.txt", "beta file"),
        ]);
        let oracle = Scripted::new()
            .noul("hi", "alpha", 0.1)
            .noul("lo", "alpha", 0.9)
            .noul("hi", "beta", 0.6)
            .noul("lo", "beta", 0.05)
            .document("hi/a.txt", "alpha", "entailed", 0.95)
            .document("hi/a.txt", "beta", "neutral", 0.95)
            .document("lo/b.txt", "beta", "neutral", 0.95)
            .document("hi/shared.txt", "beta", "neutral", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let alpha = reader.submit("alpha");
        let beta = reader.submit("beta");
        let found = reader.wait_many(&[alpha, beta]);
        let alpha = found[0].as_ref().unwrap();
        let beta = found[1].as_ref().unwrap();
        assert_eq!(alpha.value, Value::True);
        assert_eq!(alpha.evidence.databases(), &["hi/a.txt".to_string()]);
        assert_eq!(
            document_paths(&script, "alpha"),
            vec!["hi/a.txt".to_string(), "hi/shared.txt".to_string()]
        );
        assert_eq!(beta.value, Value::False);
        assert!(beta.exhausted);
        assert!(beta.evidence.databases().is_empty());

        let shared: Vec<Classification> = script
            .inner
            .borrow()
            .submitted
            .iter()
            .filter(|req| {
                req.kind == QuestionKind::Choice
                    && req.criteria.get("entailed").is_some()
                    && json_str(&req.state, "path") == "hi/a.txt"
            })
            .cloned()
            .collect();
        assert_eq!(shared.len(), 2, "{shared:?}");
        assert_eq!(shared[0].state, shared[1].state);
        assert!(shared[0].state.get("proposition").is_none());
        assert!(shared[0].state.get("body").is_some());
        let props: Vec<&str> = shared
            .iter()
            .map(|req| json_str(&req.instructions, "proposition"))
            .collect();
        assert!(
            props.contains(&"alpha") && props.contains(&"beta"),
            "{props:?}"
        );
        assert!(!document_paths(&script, "alpha")
            .iter()
            .any(|p| p == "lo/b.txt"));
    }

    #[test]
    fn pruned_window_is_false_without_opening_a_body() {
        let (_scratch, fs) = scratch(&[("decoy/noise.txt", "noise")]);
        let oracle = Scripted::new().noul("decoy", "quiet", 0.95);
        let (mut reader, script) = read_with(fs, oracle);
        let ticket = reader.submit("quiet");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        assert!(finding.exhausted);
        assert!(finding.evidence.databases().is_empty());
        assert!(document_paths(&script, "quiet").is_empty());
    }

    #[test]
    fn heuristic_frontend_is_true_and_an_unmatched_proposition_is_false() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/toy-layered");
        let universe = Filesystem::at_root(&root).unwrap();
        let oracle = HeuristicOracle::new(universe.clone());
        let mut reader = Reader::new(universe, oracle, DEFAULT_BODY_BYTES, None);
        let frontend = reader.submit("frontend");
        let absent = reader.submit("zzzz-not-a-path");
        let found = reader.wait_many(&[frontend, absent]);
        let frontend = found[0].as_ref().unwrap();
        let absent = found[1].as_ref().unwrap();
        assert_eq!(frontend.value, Value::True);
        assert!(!frontend.exhausted);
        assert!(frontend
            .evidence
            .databases()
            .iter()
            .all(|id| id.starts_with("frontend/")));
        assert!(frontend
            .evidence
            .databases()
            .contains(&"frontend/__init__.py".into()));
        assert!(!frontend
            .evidence
            .databases()
            .iter()
            .any(|id| id.contains("app.py")));
        assert_eq!(absent.value, Value::False);
        assert!(absent.exhausted);
        assert!(absent.evidence.databases().is_empty());
    }

    #[test]
    fn a_file_that_fits_is_sent_whole() {
        let text = "0123456789";
        let (_scratch, fs) = scratch(&[("a.txt", text)]);
        let oracle = Scripted::new().document("a.txt", "p", "neutral", 0.95);
        let (mut reader, script) = read_at(fs, oracle, text.len());
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        let submitted = script.inner.borrow();
        let body = submitted
            .submitted
            .iter()
            .find(|req| json_str(&req.state, "path") == "a.txt")
            .unwrap()
            .state
            .get("body")
            .unwrap()
            .as_str()
            .unwrap();
        assert_eq!(body, text);
        assert!(submitted
            .submitted
            .iter()
            .all(|req| req.state.get("part").is_none()));
        assert_eq!(finding.steps[0].rendered_bytes, text.len() as u64);
        assert!(reader.splits().is_empty());
    }

    #[test]
    fn a_file_over_the_budget_is_shared_across_claims_in_parts() {
        let text = "0123456789abcdef";
        let (_scratch, fs) = scratch(&[("big.txt", text)]);
        let oracle = Scripted::new()
            .part("big.txt", 1, "one", "neutral", 0.95)
            .part("big.txt", 2, "one", "neutral", 0.95)
            .part("big.txt", 1, "two", "neutral", 0.95)
            .part("big.txt", 2, "two", "neutral", 0.95);
        let log_path = std::env::temp_dir().join(format!(
            "autoground-split-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let log = Log::open(&log_path).unwrap();
        let mut reader = Reader::new(fs, oracle.clone(), 8, Some(log));
        let one = reader.submit("one");
        let two = reader.submit("two");
        let found = reader.wait_many(&[one, two]);
        for finding in &found {
            let finding = finding.as_ref().unwrap();
            assert_eq!(finding.value, Value::False);
            assert!(finding.exhausted);
        }
        let submitted = oracle.inner.borrow();
        let mut bodies = Vec::new();
        for proposition in ["one", "two"] {
            let posts: Vec<_> = submitted
                .submitted
                .iter()
                .filter(|req| {
                    req.kind == QuestionKind::Choice && req.criteria.get("entailed").is_some()
                })
                .filter(|req| json_str(&req.instructions, "proposition") == proposition)
                .collect();
            assert_eq!(posts.len(), 2, "{proposition}");
            let mut joined = String::new();
            for (index, post) in posts.iter().enumerate() {
                let body = post.state.get("body").unwrap().as_str().unwrap();
                assert!(body.len() <= 8, "{body}");
                assert_eq!(
                    post.state.get("part").unwrap().as_u64(),
                    Some(index as u64 + 1)
                );
                assert_eq!(post.state.get("parts").unwrap().as_u64(), Some(2));
                assert_eq!(json_str(&post.state, "path"), "big.txt");
                let question = json_str(&post.instructions, "question");
                assert!(question.contains("part"), "{question}");
                assert!(question.contains("not visible"), "{question}");
                joined.push_str(body);
            }
            bodies.push(joined);
        }
        assert_eq!(bodies[0], text);
        assert_eq!(bodies[1], text);
        let shared: Vec<_> = submitted
            .submitted
            .iter()
            .filter(|req| {
                json_str(&req.state, "path") == "big.txt"
                    && req.state.get("part").and_then(|value| value.as_u64()) == Some(1)
            })
            .map(|req| req.state.clone())
            .collect();
        assert_eq!(shared.len(), 2);
        assert_eq!(shared[0], shared[1]);
        drop(submitted);
        assert_eq!(
            reader.splits(),
            &[Split {
                path: "big.txt".into(),
                bytes: text.len() as u64,
                parts: 2,
            }]
        );
        let events = crate::log::parse_jsonl(&std::fs::read_to_string(&log_path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&log_path);
        assert!(events.iter().any(|event| {
            event["kind"] == "read_split"
                && event["path"] == "big.txt"
                && event["bytes"].as_u64() == Some(text.len() as u64)
                && event["parts"].as_u64() == Some(2)
        }));
    }

    #[test]
    fn a_later_part_witness_settles_after_an_earlier_neutral() {
        let (_scratch, fs) = scratch(&[("big.txt", "0123456789abcdef")]);
        let oracle = Scripted::new()
            .part("big.txt", 1, "p", "neutral", 0.95)
            .part("big.txt", 2, "p", "entailed", 0.95);
        let (mut reader, _) = read_at(fs, oracle, 8);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::True);
        assert_eq!(finding.stances[0].id, "big.txt part 2/2");
        assert_eq!(finding.evidence.databases(), &["big.txt".to_string()]);
        assert_eq!(
            reader.splits()[0].line(),
            "big.txt is 16 bytes, sent as 2 parts"
        );
    }

    #[test]
    fn an_earlier_part_refutation_ignores_a_later_witness() {
        let (_scratch, fs) = scratch(&[("big.txt", "0123456789abcdef")]);
        let oracle = Scripted::new()
            .part("big.txt", 1, "p", "contradicted", 0.9)
            .part("big.txt", 2, "p", "entailed", 0.95);
        let (mut reader, script) = read_at(fs, oracle, 8);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::False);
        assert!(!finding.exhausted);
        assert_eq!(finding.stances[0].id, "big.txt part 1/2");
        assert_eq!(finding.evidence.databases(), &["big.txt".to_string()]);
        assert_eq!(script.inner.borrow().escalations, 0);
        assert_eq!(document_paths(&script, "p").len(), 2);
    }

    #[test]
    fn a_part_veto_then_a_later_witness_escalates() {
        let (_scratch, fs) = scratch(&[("big.txt", "0123456789abcdef")]);
        let oracle = Scripted::new()
            .part("big.txt", 1, "p", "contradicted", 0.6)
            .part("big.txt", 2, "p", "entailed", 0.9)
            .escalation("true", 0.9);
        let (mut reader, script) = read_at(fs, oracle, 8);
        let ticket = reader.submit("p");
        let finding = reader.wait(ticket).unwrap();
        assert_eq!(finding.value, Value::True);
        assert_eq!(script.inner.borrow().escalations, 1);
        assert_eq!(
            finding
                .stances
                .iter()
                .map(|stance| stance.id.as_str())
                .collect::<Vec<_>>(),
            vec!["big.txt part 1/2", "big.txt part 2/2"]
        );
        assert_eq!(finding.evidence.databases(), &["big.txt".to_string()]);
    }

    #[test]
    fn split_body_keeps_lines_and_a_long_line_and_a_wide_character() {
        assert_eq!(
            split_body("aaaa\nbbbb\ncc", 8),
            vec!["aaaa\n".to_string(), "bbbb\ncc".to_string()]
        );
        assert_eq!(
            split_body("abcdefghij", 4),
            vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
        );
        assert_eq!(
            split_body("ééé", 3),
            vec!["é".to_string(), "é".to_string(), "é".to_string()]
        );
        let joined = split_body("aaaa\nbbbb\ncc", 8).concat();
        assert_eq!(joined, "aaaa\nbbbb\ncc");
    }

    fn read_at(
        fs: Filesystem,
        oracle: Scripted,
        body_budget: usize,
    ) -> (Reader<Filesystem, Scripted>, Scripted) {
        let shared = oracle.clone();
        let reader = Reader::new(fs, oracle, body_budget, None);
        (reader, shared)
    }
}
