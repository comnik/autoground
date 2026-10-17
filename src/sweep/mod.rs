//! Judge a witness-free lect against every file in a tree.
//!
//! Sweep lists the filesystem universe, optionally keeps paths that match a
//! natural-language filter, then judges each remaining file. Filter questions
//! share a listing state, so the oracle batches them. File questions share
//! that file's body. Different bodies are independent states and overlap.
//! A file matches when any top-level claim is true.
//!
//! Sweep builds its own oracle requests. The file Choice matches check's
//! wording today, and the two are free to diverge.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use crate::check::{split_body, DEFAULT_BODY_BYTES, MAX_SLICED_BYTES};
use crate::confidence::SUPPORT;
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Answer, Classification, Oracle, OracleError, QuestionKind, State, Ticket};
use crate::syntax::Proposition;
use crate::universe::{Filesystem, Universe, UniverseError};
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

/// Stdout for an agent, plus how far the sweep got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub matches: Vec<Match>,
    pub incomplete: bool,
    pub judged: usize,
    pub candidates: usize,
}

pub fn stopped_message(cap_usd: f64, spent_usd: f64, judged: usize, candidates: usize) -> String {
    format!(
        "sweep stopped at the cost cap of ${cap_usd:.2} (spent ${spent_usd:.4}); {judged} of {candidates} files judged"
    )
}

/// `witness` and `detail` are check's forms. A sweep lect states claims.
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

pub fn sweep<O: Oracle>(
    claims: &[Proposition],
    root: &Path,
    hide: Option<&Path>,
    filter: Option<&str>,
    oracle: &mut O,
) -> Result<Outcome, SweepError> {
    ensure_constraints(claims)?;
    let sentences = sentences_of(claims);
    if sentences.is_empty() {
        return Ok(render(Vec::new(), false, 0, 0));
    }
    let mut files = Filesystem::open(root).map_err(universe_io)?;
    if let Some(model) = hide {
        files.hide_model(model);
    }
    let paths = files.databases().to_vec();
    let kept = match filter {
        Some(filter) => match filter_paths(&paths, filter, oracle)? {
            Filtered::Incomplete => {
                return Ok(render(Vec::new(), true, 0, paths.len()));
            }
            Filtered::Kept(paths) => paths,
        },
        None => paths,
    };
    judge(claims, &sentences, root, &kept, oracle)
}

enum Filtered {
    Kept(Vec<String>),
    Incomplete,
}

fn filter_paths<O: Oracle>(
    paths: &[String],
    filter: &str,
    oracle: &mut O,
) -> Result<Filtered, SweepError> {
    if paths.is_empty() {
        return Ok(Filtered::Kept(Vec::new()));
    }
    let mut queued: Vec<(String, Ticket)> = Vec::new();
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
            queued.push((path.clone(), oracle.submit(class)));
        }
    }
    let tickets: Vec<Ticket> = queued.iter().map(|(_, ticket)| *ticket).collect();
    let answers = oracle.wait_many(&tickets);
    let mut kept = Vec::new();
    let mut limited = false;
    for ((path, _), answer) in queued.into_iter().zip(answers) {
        match answer {
            Ok(answer) if matches_filter(&answer) => kept.push(path),
            Ok(_) => {}
            Err(OracleError::Limit(_)) => limited = true,
            Err(err) => return Err(SweepError::Oracle(err)),
        }
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
    parts: Vec<String>,
}

fn judge<O: Oracle>(
    claims: &[Proposition],
    sentences: &[String],
    root: &Path,
    paths: &[String],
    oracle: &mut O,
) -> Result<Outcome, SweepError> {
    let mut bodies = Vec::new();
    for rel in paths {
        if let Some(body) = load_body(root, rel)? {
            bodies.push(body);
        }
    }
    let candidates = bodies.len();
    let mut queued: Vec<Queued> = Vec::new();
    for body in &bodies {
        queue_body(body, sentences, oracle, &mut queued);
    }
    let tickets: Vec<Ticket> = queued.iter().map(|item| item.ticket).collect();
    let answers = if tickets.is_empty() {
        Vec::new()
    } else {
        oracle.wait_many(&tickets)
    };
    let mut by_file: HashMap<String, Vec<PartAnswer>> = HashMap::new();
    let mut incomplete = false;
    for (item, answer) in queued.into_iter().zip(answers) {
        match answer {
            Ok(answer) => by_file.entry(item.rel).or_default().push(PartAnswer {
                sentence: item.sentence,
                answer,
            }),
            Err(OracleError::Limit(_)) => incomplete = true,
            Err(err) => return Err(SweepError::Oracle(err)),
        }
    }
    let mut matches = Vec::new();
    let mut judged = 0;
    for body in &bodies {
        let got = by_file.remove(&body.rel).unwrap_or_default();
        let expected = sentences.len() * body.parts.len();
        if got.len() != expected {
            incomplete = true;
            continue;
        }
        judged += 1;
        let values = sentence_values(sentences, &got);
        let matched: Vec<String> = claims
            .iter()
            .filter(|claim| value_of(claim, &values) == Value::True)
            .map(statement)
            .collect();
        if matched.is_empty() {
            continue;
        }
        matches.push(Match {
            path: absolute(root, &body.rel),
            claims: matched,
        });
    }
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(render(matches, incomplete, judged, candidates))
}

struct Queued {
    rel: String,
    sentence: String,
    ticket: Ticket,
}

struct PartAnswer {
    sentence: String,
    answer: Answer,
}

fn queue_body<O: Oracle>(
    body: &Body,
    sentences: &[String],
    oracle: &mut O,
    queued: &mut Vec<Queued>,
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
            queued.push(Queued {
                rel: body.rel.clone(),
                sentence: sentence.clone(),
                ticket: oracle.submit(class),
            });
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
    Ok(Some(Body {
        rel: rel.to_string(),
        parts: split_body(&text, DEFAULT_BODY_BYTES),
    }))
}

fn absolute(root: &Path, rel: &str) -> PathBuf {
    let path = root.join(rel);
    fs::canonicalize(&path).unwrap_or(path)
}

fn render(matches: Vec<Match>, incomplete: bool, judged: usize, candidates: usize) -> Outcome {
    let mut text = String::new();
    if matches.is_empty() && !incomplete {
        text.push_str("CLEAN\n");
    } else {
        for matched in &matches {
            text.push_str(&matched.path.display().to_string());
            text.push('\n');
            for claim in &matched.claims {
                text.push_str("  Matched: ");
                text.push_str(claim);
                text.push('\n');
            }
        }
        if incomplete {
            text.push_str("INCOMPLETE\n");
        }
    }
    Outcome {
        text,
        matches,
        incomplete,
        judged,
        candidates,
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
        format!("{}\n  Matched: {NORTHSTAR_CLAIM}\n", path.display())
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
        let outcome = sweep(&compiled(NORTHSTAR), &dir, None, None, &mut oracle).unwrap();
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
            &dir,
            None,
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
        let outcome = sweep(&compiled(NORTHSTAR), &dir, None, None, &mut oracle).unwrap();
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
            &dir,
            None,
            None,
            &mut oracle,
        )
        .unwrap();
        let path = fs::canonicalize(dir.join("has-alpha.txt")).unwrap();
        assert_eq!(
            outcome.text,
            format!("{}\n  Matched: alpha\n", path.display())
        );
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
        let outcome = sweep(&compiled(NORTHSTAR), &dir, None, None, &mut oracle).unwrap();
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
    fn a_tree_with_no_true_claim_is_clean() {
        let dir = scratch("clean");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|_| Ok(answer("entailed", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), &dir, None, None, &mut oracle).unwrap();
        assert_eq!(outcome.text, "CLEAN\n");
        assert!(outcome.matches.is_empty());
        assert!(!outcome.incomplete);
    }
}
