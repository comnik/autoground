//! Judge a witness-free brief against every readable database in a universe.
//!
//! The universe is the current directory, a transcript, or a git diff. On the
//! filesystem, sweep optionally keeps paths that match a natural-language
//! filter, then judges each remaining file. Filter questions share a listing
//! state, so the oracle batches them. File questions share that file's body.
//! A diff judges each change's patch. A transcript judges each readable leaf.
//! Different bodies are independent states and overlap. A database matches
//! when any top-level claim is true. A request that fails for one database is
//! reported on that database. The others are still judged.
//!
//! Sweep builds its own oracle requests. The file Choice matches check's
//! wording today, and the two are free to diverge.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use crate::check::{split_body, DEFAULT_BODY_BYTES, MAX_SLICED_BYTES};
use crate::confidence::SUPPORT;
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Answer, Classification, Oracle, OracleError, QuestionKind, State, Ticket};
use crate::syntax::Proposition;
use crate::universe::{Diff, Filesystem, Transcript, Universe, UniverseError};
use crate::value::Value;

const FILTER_QUESTION: &str = "Does `path` match `filter`?";
const DOCUMENT_QUESTION: &str = "How does `body` at `path` ground the proposition?";
const PART_QUESTION: &str = "How does this part of `body` at `path` ground the proposition? Text outside this part is not visible.";

#[derive(Debug)]
pub enum SweepError {
    Io(String),
    Oracle(OracleError),
    Constraint(String),
}

impl std::fmt::Display for SweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SweepError::Io(message) | SweepError::Constraint(message) => write!(f, "{message}"),
            SweepError::Oracle(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SweepError {}

/// One file with at least one true top-level claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: PathBuf,
    pub claims: Vec<String>,
}

/// One file whose request failed. The other files are still judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub message: String,
}

/// The report so far, and a status line while the sweep is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub report: String,
    pub status: Option<String>,
}

/// Stdout for an agent, plus how far the sweep got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub matches: Vec<Match>,
    pub errors: Vec<FileError>,
    pub incomplete: bool,
    pub judged: usize,
    pub candidates: usize,
}

pub fn stopped_message(cap_usd: f64, spent_usd: f64, judged: usize, candidates: usize) -> String {
    format!(
        "sweep stopped at the cost cap of ${cap_usd:.2} (spent ${spent_usd:.4}); {judged} of {candidates} files judged"
    )
}

pub fn failed_message(failed: usize) -> String {
    match failed {
        1 => "1 file failed".to_string(),
        n => format!("{n} files failed"),
    }
}

/// `witness` and `detail` are check's forms. A sweep brief states claims.
pub fn ensure_constraints(claims: &[Proposition]) -> Result<(), SweepError> {
    for claim in claims {
        reject(claim)?;
    }
    Ok(())
}

fn reject(proposition: &Proposition) -> Result<(), SweepError> {
    match proposition {
        Proposition::Witness { .. } => Err(SweepError::Constraint(
            "`witness` is not a sweep constraint".into(),
        )),
        Proposition::Detail { .. } => Err(SweepError::Constraint(
            "`detail` is not a sweep constraint".into(),
        )),
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                reject(proposition)?;
            }
            Ok(())
        }
        Proposition::Not(inner) => reject(inner),
        Proposition::Sentence(_) => Ok(()),
    }
}

/// Where a sweep reads its databases.
pub enum Scope<'a> {
    /// The filesystem at `root`.
    Files {
        root: &'a Path,
        hide: Option<&'a Path>,
    },
    /// A `chat_history.jsonl`, or a directory that contains one.
    Transcript(&'a Path),
    /// `git diff` at `root`. `git_args` are forwarded to git.
    Diff {
        root: &'a Path,
        git_args: &'a [String],
        hide: Option<&'a Path>,
    },
}

pub fn sweep<O: Oracle>(
    claims: &[Proposition],
    scope: Scope<'_>,
    filter: Option<&str>,
    oracle: &mut O,
) -> Result<Outcome, SweepError> {
    sweep_watching(claims, scope, filter, oracle, &mut |_| {})
}

/// Judge `claims` and call `watch` as the report changes.
///
/// `watch` sees the matches and errors so far, and a status line. The returned
/// [`Outcome`] is the finished report.
pub fn sweep_watching<O: Oracle>(
    claims: &[Proposition],
    scope: Scope<'_>,
    filter: Option<&str>,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Outcome, SweepError> {
    ensure_constraints(claims)?;
    if filter.is_some() && !matches!(&scope, Scope::Files { .. }) {
        return Err(SweepError::Constraint(
            "`filter` applies to a filesystem sweep".into(),
        ));
    }
    let sentences = sentences_of(claims);
    if sentences.is_empty() {
        return Ok(render(Vec::new(), Vec::new(), false, 0, 0));
    }
    let opened = open(scope)?;
    let paths = opened.ids()?;
    let kept = match filter {
        Some(filter) => match filter_paths(&paths, filter, oracle, watch)? {
            Filtered::Incomplete => {
                return Ok(render(Vec::new(), Vec::new(), true, 0, paths.len()));
            }
            Filtered::Kept(paths) => paths,
        },
        None => paths,
    };
    let mut bodies = Vec::new();
    for rel in &kept {
        if let Some(body) = opened.load(rel)? {
            bodies.push(body);
        }
    }
    judge(claims, &sentences, bodies, oracle, watch)
}

enum Opened<'a> {
    Files { root: &'a Path, files: Filesystem },
    Transcript(Transcript),
    Diff(Diff),
}

fn open(scope: Scope<'_>) -> Result<Opened<'_>, SweepError> {
    match scope {
        Scope::Files { root, hide } => {
            let mut files = Filesystem::open(root).map_err(universe_io)?;
            if let Some(model) = hide {
                files.hide_model(model);
            }
            Ok(Opened::Files { root, files })
        }
        Scope::Transcript(path) => Ok(Opened::Transcript(
            Transcript::open(path).map_err(universe_io)?,
        )),
        Scope::Diff {
            root,
            git_args,
            hide,
        } => Ok(Opened::Diff(
            Diff::open(root, git_args, hide).map_err(universe_io)?,
        )),
    }
}

impl Opened<'_> {
    fn ids(&self) -> Result<Vec<String>, SweepError> {
        match self {
            Opened::Files { files, .. } => readable_ids(files),
            Opened::Transcript(tree) => readable_ids(tree),
            Opened::Diff(diff) => readable_ids(diff),
        }
    }

    fn load(&self, rel: &str) -> Result<Option<Body>, SweepError> {
        match self {
            Opened::Files { root, .. } => load_body(root, rel),
            Opened::Transcript(tree) => read_database(tree, rel),
            Opened::Diff(diff) => read_database(diff, rel),
        }
    }
}

fn read_database(universe: &impl Universe, rel: &str) -> Result<Option<Body>, SweepError> {
    let text = universe.read(rel, usize::MAX).map_err(universe_io)?;
    Ok(body_from_text(rel, PathBuf::from(rel), text))
}

/// Files in the current view. A summarized database contributes its readable descendants.
fn readable_ids(universe: &impl Universe) -> Result<Vec<String>, SweepError> {
    let mut out = Vec::new();
    for id in universe.databases() {
        collect_readable(universe, id, &mut out)?;
    }
    Ok(out)
}

fn collect_readable(
    universe: &impl Universe,
    id: &str,
    out: &mut Vec<String>,
) -> Result<(), SweepError> {
    match universe.summary(id).map_err(universe_io)? {
        None => out.push(id.to_string()),
        Some(_) => {
            for child in universe.children(id).map_err(universe_io)? {
                collect_readable(universe, &child, out)?;
            }
        }
    }
    Ok(())
}

enum Filtered {
    Kept(Vec<String>),
    Incomplete,
}

fn filter_paths<O: Oracle>(
    paths: &[String],
    filter: &str,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Filtered, SweepError> {
    if paths.is_empty() {
        return Ok(Filtered::Kept(Vec::new()));
    }
    let mut pending: Vec<(Ticket, String)> = Vec::new();
    for chunk in paths.chunks(QUESTION_BUDGET) {
        let state = State::new(json!({ "paths": chunk }));
        for path in chunk {
            let name = path.rsplit('/').next().unwrap_or(path.as_str());
            let class = Classification::noul(
                state.clone(),
                json!({
                    "path": path,
                    "name": name,
                    "filter": filter,
                    "question": FILTER_QUESTION,
                }),
                json!({
                    "true": "The path matches the filter. It should be opened.",
                    "false": "The path does not match the filter, or the path alone does not show that it matches.",
                }),
            );
            pending.push((oracle.submit(class), path.clone()));
        }
    }
    let total = pending.len();
    let mut done = 0usize;
    let mut kept = Vec::new();
    let mut limited = false;
    emit(
        watch,
        String::new(),
        format!("filtering {done}/{total} paths"),
    );
    while !pending.is_empty() {
        for (path, answer) in take_ready(oracle, &mut pending)? {
            done += 1;
            match answer {
                Ok(answer) if matches_filter(&answer) => kept.push(path),
                Ok(_) => {}
                Err(OracleError::Limit(_)) => limited = true,
                Err(err) => return Err(SweepError::Oracle(err)),
            }
        }
        emit(
            watch,
            String::new(),
            format!("filtering {done}/{total} paths"),
        );
    }
    if limited {
        Ok(Filtered::Incomplete)
    } else {
        Ok(Filtered::Kept(kept))
    }
}

/// A Noul at the trust bar is a match. Anything weaker leaves the file out.
fn matches_filter(answer: &Answer) -> bool {
    answer.noul.unwrap_or(0.0) >= SUPPORT
}

struct Body {
    rel: String,
    label: PathBuf,
    parts: Vec<String>,
}

fn judge<O: Oracle>(
    claims: &[Proposition],
    sentences: &[String],
    bodies: Vec<Body>,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Outcome, SweepError> {
    let candidates = bodies.len();
    if candidates == 0 {
        return Ok(render(Vec::new(), Vec::new(), false, 0, 0));
    }
    let mut files: HashMap<String, FileState> = HashMap::new();
    let mut pending: Vec<(Ticket, Part)> = Vec::new();
    for body in &bodies {
        let expected = sentences.len() * body.parts.len();
        files.insert(
            body.rel.clone(),
            FileState {
                label: body.label.clone(),
                expected,
                left: expected,
                got: Vec::new(),
                errors: Vec::new(),
                limited: false,
            },
        );
        queue_body(body, sentences, oracle, &mut pending);
    }
    let mut matches = Vec::new();
    let mut errors = Vec::new();
    let mut incomplete = false;
    let mut judged = 0usize;
    let mut done = 0usize;
    emit_progress(watch, &mut matches, &mut errors, done, candidates);
    while !pending.is_empty() {
        for (part, answer) in take_ready(oracle, &mut pending)? {
            let state = files.get_mut(&part.rel).expect("queued file");
            match answer {
                Ok(answer) => state.got.push(PartAnswer {
                    sentence: part.sentence,
                    answer,
                }),
                Err(OracleError::Limit(_)) => state.limited = true,
                Err(err) => note_error(&mut state.errors, &err),
            }
            state.left = state.left.saturating_sub(1);
        }
        let finished: Vec<String> = files
            .iter()
            .filter(|(_, state)| state.left == 0)
            .map(|(rel, _)| rel.clone())
            .collect();
        for rel in finished {
            let state = files.remove(&rel).expect("finished file");
            settle(
                state,
                claims,
                sentences,
                &mut matches,
                &mut errors,
                &mut incomplete,
                &mut judged,
            );
            done += 1;
        }
        emit_progress(watch, &mut matches, &mut errors, done, candidates);
    }
    Ok(render(matches, errors, incomplete, judged, candidates))
}

struct Part {
    rel: String,
    sentence: String,
}

struct PartAnswer {
    sentence: String,
    answer: Answer,
}

struct FileState {
    label: PathBuf,
    expected: usize,
    left: usize,
    got: Vec<PartAnswer>,
    errors: Vec<String>,
    limited: bool,
}

fn settle(
    state: FileState,
    claims: &[Proposition],
    sentences: &[String],
    matches: &mut Vec<Match>,
    errors: &mut Vec<FileError>,
    incomplete: &mut bool,
    judged: &mut usize,
) {
    if state.limited {
        *incomplete = true;
    }
    if !state.errors.is_empty() {
        errors.push(FileError {
            path: state.label,
            message: state.errors.join("; "),
        });
        return;
    }
    if state.got.len() != state.expected {
        *incomplete = true;
        return;
    }
    *judged += 1;
    let values = sentence_values(sentences, &state.got);
    let matched: Vec<String> = claims
        .iter()
        .filter(|claim| value_of(claim, &values) == Value::True)
        .map(statement)
        .collect();
    if matched.is_empty() {
        return;
    }
    matches.push(Match {
        path: state.label,
        claims: matched,
    });
}

fn note_error(errors: &mut Vec<String>, err: &OracleError) {
    let message = err.to_string();
    if !errors.contains(&message) {
        errors.push(message);
    }
}

fn queue_body<O: Oracle>(
    body: &Body,
    sentences: &[String],
    oracle: &mut O,
    pending: &mut Vec<(Ticket, Part)>,
) {
    let total = body.parts.len() as u32;
    for (index, part) in body.parts.iter().enumerate() {
        let state = document_state(&body.rel, part, index as u32, total);
        for sentence in sentences {
            let class = Classification {
                state: state.clone(),
                kind: QuestionKind::Choice,
                instructions: json!({
                    "proposition": sentence,
                    "question": if total > 1 { PART_QUESTION } else { DOCUMENT_QUESTION },
                }),
                criteria: document_criteria(),
            };
            pending.push((
                oracle.submit(class),
                Part {
                    rel: body.rel.clone(),
                    sentence: sentence.clone(),
                },
            ));
        }
    }
}

fn document_state(path: &str, body: &str, index: u32, total: u32) -> State {
    if total > 1 {
        State::new(json!({
            "path": path,
            "body": body,
            "part": index + 1,
            "parts": total,
        }))
    } else {
        State::new(json!({
            "path": path,
            "body": body,
        }))
    }
}

fn document_criteria() -> Json {
    json!({
        "entailed": "`body` supplies information that makes the proposition true",
        "contradicted": "`body` supplies information that makes the proposition false",
        "neutral": "`body` neither supports nor refutes the proposition",
    })
}

fn sentence_values(sentences: &[String], answers: &[PartAnswer]) -> HashMap<String, Value> {
    let mut values = HashMap::new();
    for sentence in sentences {
        let matching: Vec<&Answer> = answers
            .iter()
            .filter(|answer| answer.sentence == *sentence)
            .map(|answer| &answer.answer)
            .collect();
        values.insert(sentence.clone(), settle_parts(&matching));
    }
    values
}

fn settle_parts(answers: &[&Answer]) -> Value {
    let mut contradicted = false;
    for answer in answers {
        match polarity(answer) {
            Some(Value::True) => return Value::True,
            Some(Value::False) => contradicted = true,
            _ => {}
        }
    }
    if contradicted {
        Value::False
    } else {
        Value::Unknown
    }
}

fn polarity(answer: &Answer) -> Option<Value> {
    if answer.confidence < SUPPORT {
        return None;
    }
    match answer.choice.as_str() {
        "entailed" => Some(Value::True),
        "contradicted" => Some(Value::False),
        _ => None,
    }
}

fn value_of(proposition: &Proposition, sentences: &HashMap<String, Value>) -> Value {
    match proposition {
        Proposition::Sentence(sentence) => {
            sentences.get(sentence).copied().unwrap_or(Value::Unknown)
        }
        Proposition::And(xs) => xs
            .iter()
            .map(|proposition| value_of(proposition, sentences))
            .reduce(Value::and)
            .unwrap_or(Value::True),
        Proposition::Or(xs) => xs
            .iter()
            .map(|proposition| value_of(proposition, sentences))
            .reduce(Value::or)
            .unwrap_or(Value::False),
        Proposition::Not(inner) => value_of(inner, sentences).not(),
        Proposition::Witness { .. } | Proposition::Detail { .. } => Value::Unknown,
    }
}

fn statement(proposition: &Proposition) -> String {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Witness { sentence, .. } => sentence.clone(),
        Proposition::Detail { head, .. } => head.clone(),
        Proposition::And(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" and "),
        Proposition::Or(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" or "),
        Proposition::Not(inner) => format!("not ({})", statement(inner)),
    }
}

fn sentences_of(claims: &[Proposition]) -> Vec<String> {
    let mut sentences = Vec::new();
    for claim in claims {
        push_sentences(claim, &mut sentences);
    }
    sentences
}

fn push_sentences(proposition: &Proposition, sentences: &mut Vec<String>) {
    match proposition {
        Proposition::Sentence(sentence) => {
            if !sentences.contains(sentence) {
                sentences.push(sentence.clone());
            }
        }
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                push_sentences(proposition, sentences);
            }
        }
        Proposition::Not(inner) => push_sentences(inner, sentences),
        Proposition::Witness { .. } | Proposition::Detail { .. } => {}
    }
}

fn load_body(root: &Path, rel: &str) -> Result<Option<Body>, SweepError> {
    let path = root.join(rel);
    let meta = fs::metadata(&path).map_err(|err| SweepError::Io(format!("{rel}: {err}")))?;
    if meta.len() as usize > MAX_SLICED_BYTES {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|err| SweepError::Io(format!("{rel}: {err}")))?;
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(None);
    }
    let text = String::from_utf8(bytes).expect("utf-8 checked");
    Ok(body_from_text(rel, absolute(root, rel), text))
}

fn body_from_text(rel: &str, label: PathBuf, text: String) -> Option<Body> {
    if text.len() > MAX_SLICED_BYTES || text.contains('\0') {
        return None;
    }
    Some(Body {
        rel: rel.to_string(),
        label,
        parts: split_body(&text, DEFAULT_BODY_BYTES),
    })
}

fn absolute(root: &Path, rel: &str) -> PathBuf {
    let path = root.join(rel);
    fs::canonicalize(&path).unwrap_or(path)
}

fn render(
    mut matches: Vec<Match>,
    mut errors: Vec<FileError>,
    incomplete: bool,
    judged: usize,
    candidates: usize,
) -> Outcome {
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path));
    let mut text = progress_report(&matches, &errors);
    if matches.is_empty() && errors.is_empty() && !incomplete {
        text.push_str("CLEAN\n");
    } else if incomplete {
        text.push_str("INCOMPLETE\n");
    }
    Outcome {
        text,
        matches,
        errors,
        incomplete,
        judged,
        candidates,
    }
}

fn progress_report(matches: &[Match], errors: &[FileError]) -> String {
    let mut text = String::new();
    write_matches(&mut text, matches);
    write_errors(&mut text, errors);
    text
}

fn write_matches(text: &mut String, matches: &[Match]) {
    for matched in matches {
        text.push_str(&matched.path.display().to_string());
        text.push('\n');
        for claim in &matched.claims {
            text.push_str("  ");
            text.push_str(claim);
            text.push('\n');
        }
    }
}

fn write_errors(text: &mut String, errors: &[FileError]) {
    for error in errors {
        text.push_str("Error: ");
        text.push_str(&error.path.display().to_string());
        text.push('\n');
        for line in error.message.lines() {
            text.push_str("  ");
            text.push_str(line);
            text.push('\n');
        }
    }
}

fn emit(watch: &mut dyn FnMut(&Snapshot), report: String, status: String) {
    watch(&Snapshot {
        report,
        status: Some(status),
    });
}

fn emit_progress(
    watch: &mut dyn FnMut(&Snapshot),
    matches: &mut Vec<Match>,
    errors: &mut Vec<FileError>,
    done: usize,
    candidates: usize,
) {
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path));
    emit(
        watch,
        progress_report(matches, errors),
        format!("judging {done}/{candidates} files"),
    );
}

/// Answers that have landed.
///
/// A sync oracle's `wait_any` returns the answer error for its first ticket
/// instead of the ticket. That error belongs to the file. A failure of the
/// wait itself names no ticket.
fn take_ready<O: Oracle, T>(
    oracle: &mut O,
    pending: &mut Vec<(Ticket, T)>,
) -> Result<Vec<(T, Result<Answer, OracleError>)>, SweepError> {
    let tickets: Vec<Ticket> = pending.iter().map(|(ticket, _)| *ticket).collect();
    let ready: HashSet<Ticket> = ready_tickets(oracle, &tickets)?.into_iter().collect();
    let mut left = Vec::new();
    let mut done = Vec::new();
    for (ticket, item) in pending.drain(..) {
        if ready.contains(&ticket) {
            done.push((ticket, item));
        } else {
            left.push((ticket, item));
        }
    }
    *pending = left;
    let mut out = Vec::with_capacity(done.len());
    for (ticket, item) in done {
        out.push((item, oracle.wait(ticket)));
    }
    if out.is_empty() {
        return Err(SweepError::Oracle(OracleError::Parse(
            "oracle reported no finished ticket".into(),
        )));
    }
    Ok(out)
}

fn ready_tickets<O: Oracle>(oracle: &mut O, tickets: &[Ticket]) -> Result<Vec<Ticket>, SweepError> {
    match oracle.wait_any(tickets) {
        Ok(ticket) => {
            let ready = oracle.completed(tickets);
            if ready.is_empty() {
                Ok(vec![ticket])
            } else {
                Ok(ready)
            }
        }
        Err(err) if is_wait_failure(&err) => Err(SweepError::Oracle(err)),
        Err(_) => Ok(vec![tickets[0]]),
    }
}

fn is_wait_failure(err: &OracleError) -> bool {
    match err {
        OracleError::Config(_) => true,
        OracleError::Parse(message)
            if message == "no tickets to wait for" || message.starts_with("unknown ticket") =>
        {
            true
        }
        OracleError::Http(message) if message.contains("ended without a result") => true,
        OracleError::Limit(_) | OracleError::Http(_) | OracleError::Parse(_) => false,
    }
}

fn universe_io(err: UniverseError) -> SweepError {
    SweepError::Io(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::syntax::parse;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "autoground-sweep-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn compiled(src: &str) -> Vec<Proposition> {
        compile(&parse(src).unwrap()).unwrap().claims
    }

    fn files(root: &Path) -> Scope<'_> {
        Scope::Files { root, hide: None }
    }

    fn answer(choice: &str, confidence: f64) -> Answer {
        Answer {
            choice: choice.to_string(),
            confidence,
            noul: None,
        }
    }

    fn noul(probability: f64) -> Answer {
        Answer {
            choice: String::new(),
            confidence: 0.0,
            noul: Some(probability),
        }
    }

    struct Script {
        next: u64,
        pending: HashMap<u64, Classification>,
        seen: Vec<Classification>,
        answer: fn(&Classification) -> Result<Answer, OracleError>,
    }

    impl Script {
        fn new(answer: fn(&Classification) -> Result<Answer, OracleError>) -> Self {
            Self {
                next: 0,
                pending: HashMap::new(),
                seen: Vec::new(),
                answer,
            }
        }
    }

    impl Oracle for Script {
        fn submit(&mut self, req: Classification) -> Ticket {
            let ticket = Ticket(self.next);
            self.next += 1;
            self.pending.insert(ticket.0, req.clone());
            self.seen.push(req);
            ticket
        }

        fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
            let req = self
                .pending
                .get(&ticket.0)
                .cloned()
                .expect("ticket was submitted");
            (self.answer)(&req)
        }
    }

    const NORTHSTAR: &str =
        "(not (\"Contains a comment that references a historic state of the codebase.\"))\n";
    const NORTHSTAR_CLAIM: &str =
        "not (Contains a comment that references a historic state of the codebase.)";

    fn matched_line(path: &std::path::Path) -> String {
        format!("{}\n  {NORTHSTAR_CLAIM}\n", path.display())
    }

    #[test]
    fn a_witness_and_a_detail_are_not_constraints() {
        let witness = ensure_constraints(&compiled("(witness (\"a\") \"a.rs\")")).unwrap_err();
        assert!(
            witness
                .to_string()
                .contains("`witness` is not a sweep constraint"),
            "{witness}"
        );
        let detail = ensure_constraints(&compiled("(detail (\"head\") (\"body\"))")).unwrap_err();
        assert!(
            detail
                .to_string()
                .contains("`detail` is not a sweep constraint"),
            "{detail}"
        );
        ensure_constraints(&compiled(NORTHSTAR)).unwrap();
    }

    #[test]
    fn without_a_filter_every_file_is_judged() {
        let dir = scratch("all");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|req| {
            assert_eq!(req.kind, QuestionKind::Choice);
            assert!(req.state.get("body").is_some());
            Ok(answer("contradicted", 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), files(&dir), None, &mut oracle).unwrap();
        let a = fs::canonicalize(dir.join("a.txt")).unwrap();
        let b = fs::canonicalize(dir.join("b.txt")).unwrap();
        assert_eq!(
            outcome.text,
            format!("{}{}", matched_line(&a), matched_line(&b))
        );
        assert_eq!(oracle.seen.len(), 2);
        assert!(oracle
            .seen
            .iter()
            .all(|req| req.kind == QuestionKind::Choice));
    }

    #[test]
    fn a_filter_keeps_only_a_confident_match() {
        let dir = scratch("filter");
        fs::write(dir.join("src.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("README.md"), "# notes\n").unwrap();
        let mut oracle = Script::new(|req| match req.kind {
            QuestionKind::Noul => {
                assert!(req.state.get("body").is_none(), "{req:?}");
                assert_eq!(req.instructions["filter"], "source file");
                assert_eq!(req.instructions["question"], FILTER_QUESTION);
                assert_eq!(
                    req.criteria["true"],
                    "The path matches the filter. It should be opened."
                );
                let path = req.instructions["path"].as_str().unwrap();
                let probability = if path.ends_with("src.rs") { 0.95 } else { 0.5 };
                Ok(noul(probability))
            }
            QuestionKind::Choice => {
                assert!(req.state.get("body").is_some());
                assert!(req.state["path"].as_str().unwrap().ends_with("src.rs"));
                Ok(answer("contradicted", 0.9))
            }
        });
        let outcome = sweep(
            &compiled(NORTHSTAR),
            files(&dir),
            Some("source file"),
            &mut oracle,
        )
        .unwrap();
        let src = fs::canonicalize(dir.join("src.rs")).unwrap();
        assert_eq!(outcome.text, matched_line(&src));
        assert!(!outcome.text.contains("README.md"));
        let filters: Vec<_> = oracle
            .seen
            .iter()
            .filter(|req| req.kind == QuestionKind::Noul)
            .collect();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].state, filters[1].state);
        assert!(filters[0].state["paths"].as_array().is_some());
        assert_eq!(
            oracle
                .seen
                .iter()
                .filter(|req| req.kind == QuestionKind::Choice)
                .count(),
            1
        );
    }

    #[test]
    fn a_true_claim_is_a_match() {
        let dir = scratch("match");
        fs::write(dir.join("old.rs"), "// used to parse declare\n").unwrap();
        fs::write(dir.join("new.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("unsure.rs"), "maybe\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            let choice = if path.ends_with("old.rs") {
                "entailed"
            } else if path.ends_with("new.rs") {
                "contradicted"
            } else {
                "neutral"
            };
            Ok(answer(choice, 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), files(&dir), None, &mut oracle).unwrap();
        let path = fs::canonicalize(dir.join("new.rs")).unwrap();
        assert_eq!(outcome.text, matched_line(&path));
        assert!(!outcome.text.contains("old.rs"));
        assert!(!outcome.text.contains("unsure.rs"));
        assert!(!outcome.incomplete);
    }

    #[test]
    fn any_true_claim_matches_the_file() {
        let dir = scratch("any");
        fs::write(dir.join("has-alpha.txt"), "one\n").unwrap();
        fs::write(dir.join("low.txt"), "none\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            let proposition = req.instructions["proposition"].as_str().unwrap();
            let (choice, confidence) = if path.ends_with("has-alpha.txt") && proposition == "alpha"
            {
                ("entailed", 0.9)
            } else if path.ends_with("has-alpha.txt") {
                ("contradicted", 0.9)
            } else {
                ("entailed", 0.5)
            };
            Ok(answer(choice, confidence))
        });
        let outcome = sweep(
            &compiled("(\"alpha\")\n(\"beta\")\n"),
            files(&dir),
            None,
            &mut oracle,
        )
        .unwrap();
        let path = fs::canonicalize(dir.join("has-alpha.txt")).unwrap();
        assert_eq!(outcome.text, format!("{}\n  alpha\n", path.display()));
        assert!(!outcome.text.contains("low.txt"));
        assert!(!outcome.text.contains("beta"));
    }

    #[test]
    fn a_refused_file_is_incomplete_even_when_nothing_matched() {
        let dir = scratch("cap");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|req| {
            if req.state["path"].as_str().unwrap().ends_with("b.txt") {
                return Err(OracleError::Limit("capped".into()));
            }
            Ok(answer("entailed", 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.text, "INCOMPLETE\n");
        assert!(!outcome.text.contains("CLEAN"));
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.candidates, 2);
        assert_eq!(
            stopped_message(0.01, 0.0104, outcome.judged, outcome.candidates),
            "sweep stopped at the cost cap of $0.01 (spent $0.0104); 1 of 2 files judged"
        );
    }

    #[test]
    fn an_oracle_error_reports_the_file_and_keeps_the_rest() {
        let dir = scratch("http");
        fs::write(dir.join("big.py"), "x\n").unwrap();
        fs::write(dir.join("ok.py"), "y\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            if path.ends_with("big.py") {
                return Err(OracleError::Http(
                    "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}".into(),
                ));
            }
            let proposition = req.instructions["proposition"].as_str().unwrap();
            if proposition == "alpha" {
                Ok(answer("entailed", 0.9))
            } else {
                Ok(answer("contradicted", 0.9))
            }
        });
        let mut statuses = Vec::new();
        let outcome = sweep_watching(
            &compiled("(\"alpha\")\n(\"beta\")\n"),
            files(&dir),
            None,
            &mut oracle,
            &mut |snapshot| {
                if let Some(status) = &snapshot.status {
                    statuses.push(status.clone());
                }
            },
        )
        .unwrap();
        let ok = fs::canonicalize(dir.join("ok.py")).unwrap();
        let big = fs::canonicalize(dir.join("big.py")).unwrap();
        assert_eq!(
            outcome.text,
            format!(
                "{}\n  alpha\nError: {}\n  HTTP 400: {{\"detail\":{{\"error_type\":\"max_tokens_exceeded\"}}}}\n",
                ok.display(),
                big.display()
            )
        );
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.candidates, 2);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(
            outcome.errors[0].message,
            "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}"
        );
        assert!(!outcome.incomplete);
        assert!(!outcome.text.contains("CLEAN"));
        assert!(statuses.contains(&"judging 0/2 files".to_string()));
        assert!(statuses.contains(&"judging 1/2 files".to_string()));
        assert!(statuses.contains(&"judging 2/2 files".to_string()));
        assert_eq!(failed_message(1), "1 file failed");
        assert_eq!(failed_message(2), "2 files failed");
    }

    #[test]
    fn an_error_with_no_match_is_not_clean() {
        let dir = scratch("only-error");
        fs::write(dir.join("big.py"), "x\n").unwrap();
        let mut oracle = Script::new(|_| {
            Err(OracleError::Http(
                "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}".into(),
            ))
        });
        let outcome = sweep(&compiled(NORTHSTAR), files(&dir), None, &mut oracle).unwrap();
        let big = fs::canonicalize(dir.join("big.py")).unwrap();
        assert_eq!(
            outcome.text,
            format!(
                "Error: {}\n  HTTP 400: {{\"detail\":{{\"error_type\":\"max_tokens_exceeded\"}}}}\n",
                big.display()
            )
        );
        assert!(outcome.matches.is_empty());
        assert!(!outcome.incomplete);
        assert_eq!(outcome.judged, 0);
        assert_eq!(outcome.candidates, 1);
    }

    #[test]
    fn a_tree_with_no_true_claim_is_clean() {
        let dir = scratch("clean");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|_| Ok(answer("entailed", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.text, "CLEAN\n");
        assert!(outcome.matches.is_empty());
        assert!(!outcome.incomplete);
    }

    #[test]
    fn a_transcript_sweep_judges_readable_leaves() {
        let dir = scratch("transcript");
        let path = dir.join("chat_history.jsonl");
        fs::write(
            &path,
            r#"{"type":"system","content":"SECRET_SYSTEM"}
{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nFind alpha\n</user_query>"}]}
{"type":"assistant","content":"","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"a.txt\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"alpha is here"}
{"type":"assistant","content":"Found it."}
"#,
        )
        .unwrap();
        let mut oracle = Script::new(|req| {
            let body = req.state["body"].as_str().unwrap();
            let choice = if body.contains("alpha is here") {
                "contradicted"
            } else {
                "entailed"
            };
            Ok(answer(choice, 0.9))
        });
        let outcome = sweep(
            &compiled(NORTHSTAR),
            Scope::Transcript(&path),
            None,
            &mut oracle,
        )
        .unwrap();
        let paths: Vec<_> = oracle
            .seen
            .iter()
            .map(|req| req.state["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            paths,
            vec![
                "p0/c0/00-call-read_file",
                "p0/c0/01-result-read_file",
                "p0/c1/00-message",
            ]
        );
        assert_eq!(
            outcome.text,
            format!("p0/c0/01-result-read_file\n  {NORTHSTAR_CLAIM}\n")
        );
        assert!(!outcome.text.contains("SECRET_SYSTEM"));
    }

    #[test]
    fn a_diff_sweep_judges_each_patch() {
        let dir = scratch("diff");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        git(&dir, &["config", "user.name", "test"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        git(&dir, &["add", "a.txt", "b.txt"]);
        git(&dir, &["commit", "-qm", "init"]);
        fs::write(dir.join("a.txt"), "beta\n").unwrap();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        git(&dir, &["add", "b.txt"]);
        fs::write(dir.join("c.txt"), "untracked\n").unwrap();
        let mut oracle = Script::new(|req| {
            let body = req.state["body"].as_str().unwrap();
            let choice =
                if body.contains("diff --git") && body.contains("-alpha") && body.contains("+beta")
                {
                    "contradicted"
                } else {
                    "entailed"
                };
            Ok(answer(choice, 0.9))
        });
        let git_args: &[String] = &[];
        let outcome = sweep(
            &compiled(NORTHSTAR),
            Scope::Diff {
                root: &dir,
                git_args,
                hide: None,
            },
            None,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(outcome.text, format!("a.txt\n  {NORTHSTAR_CLAIM}\n"));
        assert_eq!(outcome.candidates, 1);
        assert_eq!(
            oracle
                .seen
                .iter()
                .map(|req| req.state["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a.txt"]
        );
    }

    #[test]
    fn a_filter_applies_to_the_filesystem_only() {
        let git_args: &[String] = &[];
        let err = sweep(
            &compiled(NORTHSTAR),
            Scope::Diff {
                root: Path::new("."),
                git_args,
                hide: None,
            },
            Some("source"),
            &mut Script::new(|_| Ok(answer("entailed", 0.9))),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("`filter` applies to a filesystem sweep"),
            "{err}"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
