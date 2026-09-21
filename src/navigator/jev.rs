use std::collections::{BTreeMap, HashSet};
use std::hash::{Hash, Hasher};

use serde_json::{json, Value};

use super::judge::{
    Child, ChildChoice, ConflictAnswer, ConflictState, Consistency, DocumentAnswer,
    DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError, JudgeExchange,
    ListingAtom, ListingState, MultiListingState, ResolveValue, Transition,
};
use super::walk::parent_dir;
use crate::oracle::jev::{JevOracle, UreqTransport};
use crate::oracle::{
    Answer as OracleAnswer, Classification, Oracle, OracleError, OracleExchange, Ticket, Usage,
};

pub use crate::oracle::jev::Transport;

/// Jev System One judge. Listing and document phrasing live here; HTTP
/// batching and the spend cap live on [`JevOracle`].
pub struct JevJudge<T: Transport = UreqTransport> {
    inner: JevOracle<T>,
    exchanges: Vec<JudgeExchange>,
}

impl JevJudge<UreqTransport> {
    pub fn from_env() -> Result<Self, JudgeError> {
        Ok(Self {
            inner: JevOracle::from_env()?,
            exchanges: Vec::new(),
        })
    }
}

impl<T: Transport> JevJudge<T> {
    pub fn new(transport: T, endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            inner: JevOracle::new(transport, endpoint, model),
            exchanges: Vec::new(),
        }
    }

    pub fn usage(&self) -> Usage {
        self.inner.usage()
    }

    pub fn transport(&self) -> &T {
        &self.inner.transport
    }

    pub fn set_spent_usd(&mut self, spent: f64) {
        self.inner.set_spent_usd(spent);
    }

    fn capture_exchanges(&mut self) {
        self.exchanges = self
            .inner
            .last_exchanges()
            .iter()
            .cloned()
            .map(judge_exchange)
            .collect();
    }
}

impl From<OracleError> for JudgeError {
    fn from(e: OracleError) -> Self {
        match e {
            OracleError::Config(s) => JudgeError::Config(s),
            OracleError::Http(s) => JudgeError::Http(s),
            OracleError::Parse(s) => JudgeError::Parse(s),
            OracleError::Limit(s) => JudgeError::Limit(s),
        }
    }
}

fn judge_exchange(ex: OracleExchange) -> JudgeExchange {
    JudgeExchange {
        request: ex.request,
        response: ex.response,
        started_at: ex.started_at,
        ended_at: ex.ended_at,
        input_tokens: ex.input_tokens,
        output_tokens: ex.output_tokens,
        cost_usd: ex.cost_usd,
    }
}

impl<T: Transport> Judge for JevJudge<T> {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        let atom = ListingAtom {
            pred: state.pred.clone(),
            args: state.args.clone(),
            verbalization: state.verbalization.clone(),
        };
        let multi = MultiListingState {
            atoms: vec![atom],
            dir: state.dir.clone(),
            children: state.children.clone(),
        };
        let mut rows = self.choose_many(&multi)?;
        Ok(rows.pop().unwrap_or_default())
    }

    fn choose_many(
        &mut self,
        state: &MultiListingState,
    ) -> Result<Vec<Vec<ChildChoice>>, JudgeError> {
        self.exchanges.clear();
        if state.atoms.is_empty() {
            return Ok(Vec::new());
        }
        let shared = listing_shared_state(&state.dir, &state.children, &state.atoms);
        let mut tickets: Vec<Vec<Ticket>> = Vec::with_capacity(state.atoms.len());
        for atom in &state.atoms {
            let statement = atom_text(&atom.pred, &atom.args, &atom.verbalization);
            let mut row = Vec::with_capacity(state.children.len());
            for child in &state.children {
                row.push(self.inner.submit(Classification {
                    state: shared.clone(),
                    instructions: listing_instructions(&listing_path(child), &statement),
                    criteria: listing_criteria(),
                }));
            }
            tickets.push(row);
        }
        let flat: Vec<Ticket> = tickets.iter().flatten().copied().collect();
        let answers = self.inner.wait_many(&flat);
        self.capture_exchanges();
        let mut iter = answers.into_iter();
        let mut out = Vec::with_capacity(state.atoms.len());
        for _ in &state.atoms {
            let mut row = Vec::with_capacity(state.children.len());
            for child in &state.children {
                let ans = iter
                    .next()
                    .ok_or_else(|| JudgeError::Parse("missing listing answer".into()))?;
                row.push(child_choice(child, ans)?);
            }
            out.push(row);
        }
        Ok(out)
    }

    fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
        self.documents(std::slice::from_ref(state))
            .pop()
            .unwrap_or_else(|| Err(JudgeError::Parse("missing document answer".into())))
            .map(|a| a.judgement)
    }

    fn documents(&mut self, states: &[DocumentState]) -> Vec<Result<DocumentAnswer, JudgeError>> {
        self.exchanges.clear();
        if states.is_empty() {
            return Vec::new();
        }
        let tickets: Vec<Ticket> = states
            .iter()
            .map(|s| {
                let statement = atom_text(&s.pred, &s.args, &s.verbalization);
                self.inner.submit(Classification {
                    state: document_shared_state(s),
                    instructions: document_instructions(&statement),
                    criteria: document_criteria(),
                })
            })
            .collect();
        let answers = self.inner.wait_many(&tickets);
        self.capture_exchanges();
        let mut seen = HashSet::new();
        answers
            .into_iter()
            .map(|ans| document_answer(ans, &mut seen))
            .collect()
    }

    fn resolve_conflict(&mut self, state: &ConflictState) -> Result<ConflictAnswer, JudgeError> {
        self.exchanges.clear();
        let shared = conflict_shared_state(state);
        let statement = atom_text(&state.pred, &state.args, &state.verbalization);
        let value_t = self.inner.submit(Classification {
            state: shared.clone(),
            instructions: format!(
                "Given these document judgements of the following statement, what is the codebase's overall answer: {statement}"
            ),
            criteria: conflict_value_criteria(),
        });
        let cons_t = self.inner.submit(Classification {
            state: shared,
            instructions: format!(
                "Do the polar judgements agree about the following statement, or do they conflict: {statement}"
            ),
            criteria: conflict_consistency_criteria(),
        });
        let answers = self.inner.wait_many(&[value_t, cons_t]);
        self.capture_exchanges();
        let value_ans = answers
            .first()
            .cloned()
            .ok_or_else(|| JudgeError::Parse("missing answer value".into()))?;
        let cons_ans = answers
            .get(1)
            .cloned()
            .ok_or_else(|| JudgeError::Parse("missing answer consistency".into()))?;
        conflict_answer(value_ans, cons_ans, self.exchanges.clone())
    }

    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        self.exchanges.clone()
    }
}

fn listing_shared_state(dir: &str, children: &[Child], atoms: &[ListingAtom]) -> Value {
    let atoms: Vec<Value> = atoms
        .iter()
        .map(|atom| {
            json!({
                "pred": atom.pred,
                "args": atom.args,
                "statement": atom_text(&atom.pred, &atom.args, &atom.verbalization),
            })
        })
        .collect();
    json!({
        "directory": dir,
        "tree": listing_tree(children),
        "atoms": atoms,
    })
}

fn listing_instructions(path: &str, statement: &str) -> String {
    format!(
        "Should we look in {path} for proof or disproof of the following statement: {statement}"
    )
}

fn listing_criteria() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "skip".into(),
            "Do not look here, or in anything nested under this path. The name, path, and listing tree show this is unrelated.".into(),
        ),
        (
            "investigate".into(),
            "Look here. Nested paths already in this listing are used when a directory is investigated; files are opened and judged.".into(),
        ),
    ])
}

fn document_shared_state(state: &DocumentState) -> Value {
    json!({
        "path": state.path,
        "body": state.body,
        "bindings": state.bindings.iter().map(|(c, e)| json!({
            "constant": c,
            "entity": e,
        })).collect::<Vec<_>>(),
    })
}

fn document_instructions(statement: &str) -> String {
    format!("How does `body` ground the following statement: {statement}")
}

fn document_criteria() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "entailed".into(),
            "body supplies information that makes the statement true".into(),
        ),
        (
            "contradicted".into(),
            "body supplies information that makes the statement false".into(),
        ),
        (
            "neutral".into(),
            "body neither supports nor refutes the statement".into(),
        ),
    ])
}

fn conflict_shared_state(state: &ConflictState) -> Value {
    let statement = atom_text(&state.pred, &state.args, &state.verbalization);
    let evidence: Vec<Value> = state
        .evidence
        .iter()
        .map(|e| {
            json!({
                "path": e.path,
                "verdict": e.verdict.label(),
                "confidence": e.confidence,
                "excerpt": e.excerpt,
            })
        })
        .collect();
    json!({
        "atom": {
            "pred": state.pred,
            "args": state.args,
            "statement": statement,
        },
        "evidence": evidence,
    })
}

fn conflict_value_criteria() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("true".into(), "the statement holds of the codebase".into()),
        (
            "false".into(),
            "the statement does not hold of the codebase".into(),
        ),
        (
            "unknown".into(),
            "the evidence is insufficient to decide".into(),
        ),
    ])
}

fn conflict_consistency_criteria() -> BTreeMap<String, String> {
    BTreeMap::from([
        (
            "consistent".into(),
            "the entailed and contradicted files can be reconciled".into(),
        ),
        (
            "conflicting".into(),
            "the files genuinely disagree about the statement".into(),
        ),
    ])
}

fn child_choice(
    child: &Child,
    ans: Result<OracleAnswer, OracleError>,
) -> Result<ChildChoice, JudgeError> {
    let a = ans?;
    let transition = match a.choice.as_str() {
        "skip" => Transition::Skip,
        "investigate" => Transition::Investigate,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown listing choice `{other}` for {}",
                child.rel_path
            )));
        }
    };
    Ok(ChildChoice {
        rel_path: child.rel_path.clone(),
        transition,
        confidence: a.confidence,
    })
}

fn document_answer(
    ans: Result<OracleAnswer, OracleError>,
    seen: &mut HashSet<u64>,
) -> Result<DocumentAnswer, JudgeError> {
    let a = ans?;
    let verdict = match a.choice.as_str() {
        "entailed" => DocumentVerdict::Entailed,
        "contradicted" => DocumentVerdict::Contradicted,
        "neutral" => DocumentVerdict::Neutral,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown document verdict `{other}`"
            )));
        }
    };
    let exchanges = match &a.exchange {
        Some(ex) => {
            let id = request_digest(&ex.request);
            if seen.insert(id) {
                vec![judge_exchange(ex.clone())]
            } else {
                Vec::new()
            }
        }
        None => Vec::new(),
    };
    Ok(DocumentAnswer {
        judgement: DocumentJudgement {
            verdict,
            confidence: a.confidence,
        },
        exchanges,
    })
}

fn request_digest(request: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match serde_json::to_vec(request) {
        Ok(bytes) => bytes.hash(&mut hasher),
        Err(_) => 0.hash(&mut hasher),
    }
    hasher.finish()
}

fn conflict_answer(
    value_ans: Result<OracleAnswer, OracleError>,
    cons_ans: Result<OracleAnswer, OracleError>,
    exchanges: Vec<JudgeExchange>,
) -> Result<ConflictAnswer, JudgeError> {
    let value = value_ans?;
    let consistency = cons_ans?;
    let value_choice = match value.choice.as_str() {
        "true" => ResolveValue::True,
        "false" => ResolveValue::False,
        "unknown" => ResolveValue::Unknown,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown resolve value `{other}`"
            )));
        }
    };
    let consistency_choice = match consistency.choice.as_str() {
        "consistent" => Consistency::Consistent,
        "conflicting" => Consistency::Conflicting,
        other => {
            return Err(JudgeError::Parse(format!("unknown consistency `{other}`")));
        }
    };
    Ok(ConflictAnswer {
        value: value_choice,
        consistency: consistency_choice,
        value_confidence: value.confidence,
        consistency_confidence: consistency.confidence,
        exchanges,
    })
}

pub fn listing_request(
    model: &str,
    state: &ListingState,
    chunk: &[Child],
    id_base: usize,
) -> Value {
    let statement = atom_text(&state.pred, &state.args, &state.verbalization);
    let mut questions = serde_json::Map::new();
    for (i, child) in chunk.iter().enumerate() {
        let path = listing_path(child);
        questions.insert(
            format!("c{}", id_base + i),
            json!({
                "type": "choice",
                "instructions": listing_instructions(&path, &statement),
                "criteria": listing_criteria(),
            }),
        );
    }
    json!({
        "model": model,
        "state": {
            "atom": {
                "pred": state.pred,
                "args": state.args,
                "statement": statement,
            },
            "directory": state.dir,
            "tree": listing_tree(&state.children),
            "children": children_by_question(chunk, id_base),
        },
        "questions": questions,
    })
}

fn listing_pair_key(atom: usize, child: usize) -> String {
    format!("a{atom}c{child}")
}

/// One listing tree, a Choice per (atom, child) pair. `pairs` are
/// `(atom_index, child_index)` into `state`.
pub fn listing_request_many(
    model: &str,
    state: &MultiListingState,
    pairs: &[(usize, usize)],
) -> Value {
    let mut questions = serde_json::Map::new();
    let mut atoms = serde_json::Map::new();
    let mut children = serde_json::Map::new();
    for &(a, c) in pairs {
        let atom = &state.atoms[a];
        let child = &state.children[c];
        let statement = atom_text(&atom.pred, &atom.args, &atom.verbalization);
        let path = listing_path(child);
        atoms.entry(format!("a{a}")).or_insert_with(|| {
            json!({
                "pred": atom.pred,
                "args": atom.args,
                "statement": statement,
            })
        });
        children
            .entry(format!("c{c}"))
            .or_insert_with(|| json!(path));
        questions.insert(
            listing_pair_key(a, c),
            json!({
                "type": "choice",
                "instructions": listing_instructions(&path, &statement),
                "criteria": listing_criteria(),
            }),
        );
    }
    json!({
        "model": model,
        "state": {
            "atoms": atoms,
            "directory": state.dir,
            "tree": listing_tree(&state.children),
            "children": children,
        },
        "questions": questions,
    })
}

/// Parent directory → child names. Directory names end with `/`.
/// Sample names appear only when that child was not packed.
fn listing_tree(children: &[Child]) -> Value {
    let mut dirs: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let packed: HashSet<&str> = children.iter().map(|c| c.rel_path.as_str()).collect();
    for child in children {
        dirs.entry(dir_key(parent_dir(&child.rel_path)))
            .or_default()
            .push(entry_name(child));
    }
    for child in children {
        if !child.is_dir {
            continue;
        }
        let key = dir_key(Some(&child.rel_path));
        let entries = dirs.entry(key).or_default();
        for name in &child.sample {
            let rel = if child.rel_path.is_empty() {
                name.clone()
            } else {
                format!("{}/{name}", child.rel_path.trim_end_matches('/'))
            };
            if packed.contains(rel.as_str()) {
                continue;
            }
            if entries.iter().any(|e| e.trim_end_matches('/') == name) {
                continue;
            }
            entries.push(name.clone());
        }
    }
    for names in dirs.values_mut() {
        names.sort();
        names.dedup();
    }
    json!(dirs)
}

fn children_by_question(chunk: &[Child], id_base: usize) -> Value {
    let mut map = serde_json::Map::new();
    for (i, child) in chunk.iter().enumerate() {
        map.insert(format!("c{}", id_base + i), json!(listing_path(child)));
    }
    Value::Object(map)
}

fn dir_key(parent: Option<&str>) -> String {
    match parent {
        Some(p) if !p.is_empty() => format!("{}/", p.trim_end_matches('/')),
        _ => "/".into(),
    }
}

fn entry_name(child: &Child) -> String {
    if child.is_dir {
        format!("{}/", child.name.trim_end_matches('/'))
    } else {
        child.name.clone()
    }
}

pub fn investigation_request(model: &str, state: &DocumentState) -> Value {
    let statement = atom_text(&state.pred, &state.args, &state.verbalization);
    json!({
        "model": model,
        "state": {
            "atom": {
                "pred": state.pred,
                "args": state.args,
                "statement": statement,
            },
            "path": state.path,
            "body": state.body,
            "bindings": state.bindings.iter().map(|(c, e)| json!({
                "constant": c,
                "entity": e,
            })).collect::<Vec<_>>(),
        },
        "questions": {
            "ground": document_question(&statement),
        }
    })
}

pub fn parse_listing_answers(
    resp: &Value,
    chunk: &[Child],
    id_base: usize,
) -> Result<Vec<ChildChoice>, JudgeError> {
    let mut out = Vec::new();
    for (i, child) in chunk.iter().enumerate() {
        let key = format!("c{}", id_base + i);
        out.push(parse_listing_choice(resp, &key, child)?);
    }
    Ok(out)
}

fn parse_listing_choice(resp: &Value, key: &str, child: &Child) -> Result<ChildChoice, JudgeError> {
    let answers = resp
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| JudgeError::Parse("response missing answers".into()))?;
    let ans = answers
        .get(key)
        .ok_or_else(|| JudgeError::Parse(format!("missing answer {key}")))?;
    let choice = ans.get("choice").and_then(|c| c.as_str()).unwrap_or("skip");
    let confidence = ans
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    let transition = match choice {
        "skip" => Transition::Skip,
        "investigate" => Transition::Investigate,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown listing choice `{other}` for {}",
                child.rel_path
            )));
        }
    };
    Ok(ChildChoice {
        rel_path: child.rel_path.clone(),
        transition,
        confidence,
    })
}

pub fn parse_document_answer(resp: &Value) -> Result<DocumentJudgement, JudgeError> {
    parse_document_choice(
        resp.pointer("/answers/ground")
            .ok_or_else(|| JudgeError::Parse("response missing answers.ground".into()))?,
    )
}

fn parse_document_choice(ans: &Value) -> Result<DocumentJudgement, JudgeError> {
    let choice = ans
        .get("choice")
        .and_then(|c| c.as_str())
        .ok_or_else(|| JudgeError::Parse("document answer missing choice".into()))?;
    let verdict = match choice {
        "entailed" => DocumentVerdict::Entailed,
        "contradicted" => DocumentVerdict::Contradicted,
        "neutral" => DocumentVerdict::Neutral,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown document verdict `{other}`"
            )));
        }
    };
    let confidence = ans
        .get("confidence")
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    Ok(DocumentJudgement {
        verdict,
        confidence,
    })
}

pub fn investigation_request_many(model: &str, states: &[DocumentState]) -> Value {
    let first = &states[0];
    let mut questions = serde_json::Map::new();
    let mut atoms = serde_json::Map::new();
    for (i, s) in states.iter().enumerate() {
        let key = format!("q{i}");
        let statement = atom_text(&s.pred, &s.args, &s.verbalization);
        atoms.insert(
            key.clone(),
            json!({
                "pred": s.pred,
                "args": s.args,
                "statement": statement,
            }),
        );
        questions.insert(key, document_question(&statement));
    }
    json!({
        "model": model,
        "state": {
            "path": first.path,
            "body": first.body,
            "bindings": first.bindings.iter().map(|(c, e)| json!({
                "constant": c,
                "entity": e,
            })).collect::<Vec<_>>(),
            "atoms": atoms,
        },
        "questions": questions,
    })
}

pub fn conflict_request(model: &str, state: &ConflictState) -> Value {
    let statement = atom_text(&state.pred, &state.args, &state.verbalization);
    let evidence: Vec<Value> = state
        .evidence
        .iter()
        .map(|e| {
            json!({
                "path": e.path,
                "verdict": e.verdict.label(),
                "confidence": e.confidence,
                "excerpt": e.excerpt,
            })
        })
        .collect();
    json!({
        "model": model,
        "state": {
            "atom": {
                "pred": state.pred,
                "args": state.args,
                "statement": statement,
            },
            "evidence": evidence,
        },
        "questions": {
            "value": {
                "type": "choice",
                "instructions": format!(
                    "Given these document judgements of the following statement, what is the codebase's overall answer: {statement}"
                ),
                "criteria": {
                    "true": "the statement holds of the codebase",
                    "false": "the statement does not hold of the codebase",
                    "unknown": "the evidence is insufficient to decide",
                }
            },
            "consistency": {
                "type": "choice",
                "instructions": format!(
                    "Do the polar judgements agree about the following statement, or do they conflict: {statement}"
                ),
                "criteria": {
                    "consistent": "the entailed and contradicted files can be reconciled",
                    "conflicting": "the files genuinely disagree about the statement",
                }
            }
        }
    })
}

#[cfg(test)]
fn parse_conflict_answer(resp: &Value) -> Result<ConflictAnswer, JudgeError> {
    let answers = resp
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| JudgeError::Parse("response missing answers".into()))?;
    let value = answers
        .get("value")
        .and_then(|v| v.get("choice"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| JudgeError::Parse("missing answer value".into()))?;
    let consistency = answers
        .get("consistency")
        .and_then(|v| v.get("choice"))
        .and_then(|c| c.as_str())
        .ok_or_else(|| JudgeError::Parse("missing answer consistency".into()))?;
    let value = match value {
        "true" => ResolveValue::True,
        "false" => ResolveValue::False,
        "unknown" => ResolveValue::Unknown,
        other => {
            return Err(JudgeError::Parse(format!(
                "unknown resolve value `{other}`"
            )));
        }
    };
    let consistency = match consistency {
        "consistent" => Consistency::Consistent,
        "conflicting" => Consistency::Conflicting,
        other => {
            return Err(JudgeError::Parse(format!("unknown consistency `{other}`")));
        }
    };
    let value_confidence = answers
        .get("value")
        .and_then(|v| v.get("confidence"))
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    let consistency_confidence = answers
        .get("consistency")
        .and_then(|v| v.get("confidence"))
        .and_then(|c| c.as_f64())
        .unwrap_or(0.0);
    Ok(ConflictAnswer {
        value,
        consistency,
        value_confidence,
        consistency_confidence,
        exchanges: Vec::new(),
    })
}

fn format_atom(pred: &str, args: &[String]) -> String {
    let args = args
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(" ");
    if args.is_empty() {
        format!("({pred})")
    } else {
        format!("({pred} {args})")
    }
}

fn atom_text(pred: &str, args: &[String], verbalization: &str) -> String {
    if verbalization.is_empty() {
        format_atom(pred, args)
    } else {
        verbalization.to_string()
    }
}

fn document_question(statement: &str) -> Value {
    json!({
        "type": "choice",
        "instructions": document_instructions(statement),
        "criteria": document_criteria(),
    })
}

fn listing_path(child: &Child) -> String {
    if child.is_dir {
        format!("{}/", child.rel_path.trim_end_matches('/'))
    } else {
        child.rel_path.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{
        Child, ConflictEvidence, ConflictState, Consistency, DocumentVerdict, ListingAtom,
        MultiListingState, ResolveValue,
    };

    fn child(name: &str, is_dir: bool) -> Child {
        Child {
            name: name.into(),
            rel_path: name.into(),
            is_dir,
            sample: vec!["app.py".into()],
        }
    }

    #[test]
    fn listing_request_one_choice_per_child() {
        let state = ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![
                child("frontend", true),
                child("decoy", true),
                child("app.py", false),
            ],
            verbalization: "\"Frontend\" is a component in the codebase".into(),
        };
        let body = listing_request("jev-latest", &state, &state.children, 0);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["c0"]["type"], "choice");
        assert_eq!(body["questions"]["c1"]["type"], "choice");
        assert_eq!(body["questions"]["c2"]["type"], "choice");
        assert_eq!(body["state"]["children"]["c0"], "frontend/");
        assert_eq!(body["state"]["children"]["c1"], "decoy/");
        assert_eq!(body["state"]["children"]["c2"], "app.py");
        assert_eq!(
            body["state"]["tree"],
            json!({
                "/": ["app.py", "decoy/", "frontend/"],
                "decoy/": ["app.py"],
                "frontend/": ["app.py"],
            })
        );
        assert_eq!(
            body["state"]["atom"]["statement"],
            "\"Frontend\" is a component in the codebase"
        );
        let instr = body["questions"]["c0"]["instructions"].as_str().unwrap();
        assert_eq!(
            instr,
            "Should we look in frontend/ for proof or disproof of the following statement: \"Frontend\" is a component in the codebase"
        );
        assert_eq!(
            body["questions"]["c1"]["instructions"].as_str().unwrap(),
            "Should we look in decoy/ for proof or disproof of the following statement: \"Frontend\" is a component in the codebase"
        );
        assert_eq!(
            body["questions"]["c2"]["instructions"].as_str().unwrap(),
            "Should we look in app.py for proof or disproof of the following statement: \"Frontend\" is a component in the codebase"
        );
        let criteria = body["questions"]["c0"]["criteria"].as_object().unwrap();
        assert!(criteria.contains_key("skip"));
        assert!(criteria.contains_key("investigate"));
        assert!(!criteria.contains_key("unknown"));
        assert!(
            !criteria["skip"]
                .as_str()
                .unwrap()
                .contains("does not contain proof or disproof"),
            "skip must not mean 'no proof yet'; that makes every listing skip"
        );
        assert!(
            criteria["skip"]
                .as_str()
                .unwrap()
                .contains("nested under this path"),
            "skip must drop packed descendants"
        );
        assert!(
            criteria["investigate"]
                .as_str()
                .unwrap()
                .contains("Nested paths already in this listing"),
            "{:?}",
            criteria["investigate"]
        );
        assert!(!body["state"].get("body").is_some());
    }

    #[test]
    fn listing_request_many_one_choice_per_atom_and_child() {
        let state = MultiListingState {
            atoms: vec![
                ListingAtom {
                    pred: "component".into(),
                    args: vec!["Frontend".into()],
                    verbalization: "\"Frontend\" is a component in the codebase".into(),
                },
                ListingAtom {
                    pred: "component".into(),
                    args: vec!["Backend".into()],
                    verbalization: "\"Backend\" is a component in the codebase".into(),
                },
            ],
            dir: "".into(),
            children: vec![child("frontend", true), child("backend", true)],
        };
        let pairs = vec![(0, 0), (0, 1), (1, 0), (1, 1)];
        let body = listing_request_many("jev-latest", &state, &pairs);
        assert_eq!(body["questions"]["a0c0"]["type"], "choice");
        assert_eq!(body["questions"]["a1c1"]["type"], "choice");
        assert_eq!(body["state"]["children"]["c0"], "frontend/");
        assert_eq!(body["state"]["atoms"]["a0"]["args"][0], "Frontend");
        assert_eq!(body["state"]["atoms"]["a1"]["args"][0], "Backend");
        assert!(body["state"].get("atom").is_none());
        let instr = body["questions"]["a0c0"]["instructions"].as_str().unwrap();
        assert!(instr.contains("frontend/"));
        assert!(instr.contains("\"Frontend\" is a component in the codebase"));
        let instr_b = body["questions"]["a1c0"]["instructions"].as_str().unwrap();
        assert!(instr_b.contains("\"Backend\" is a component in the codebase"));
    }

    #[test]
    fn listing_tree_indexes_dirs_and_keeps_unpacked_samples() {
        let children = vec![
            Child {
                name: "api".into(),
                rel_path: "api".into(),
                is_dir: true,
                sample: vec!["client.py".into(), "handlers.py".into()],
            },
            Child {
                name: "client.py".into(),
                rel_path: "api/client.py".into(),
                is_dir: false,
                sample: vec![],
            },
            Child {
                name: "handlers.py".into(),
                rel_path: "api/handlers.py".into(),
                is_dir: false,
                sample: vec![],
            },
            Child {
                name: "src".into(),
                rel_path: "src".into(),
                is_dir: true,
                sample: vec!["frontend".into(), "lib".into()],
            },
        ];
        assert_eq!(
            listing_tree(&children),
            json!({
                "/": ["api/", "src/"],
                "api/": ["client.py", "handlers.py"],
                "src/": ["frontend", "lib"],
            })
        );
    }

    #[test]
    fn investigation_request_is_three_way_choice() {
        let state = DocumentState {
            pred: "defines".into(),
            args: vec!["ui/app.py".into(), "Backend".into()],
            path: "ui/app.py".into(),
            body: "from api import client".into(),
            bindings: vec![("Backend".into(), "api".into())],
            verbalization: "file \"ui/app.py\" (partially) defines component \"Backend\"".into(),
        };
        let body = investigation_request("jev-latest", &state);
        assert_eq!(body["questions"]["ground"]["type"], "choice");
        let criteria = body["questions"]["ground"]["criteria"].as_object().unwrap();
        assert!(criteria.contains_key("entailed"));
        assert!(criteria.contains_key("contradicted"));
        assert!(criteria.contains_key("neutral"));
        assert!(!criteria.contains_key("true"));
        assert!(!criteria.contains_key("false"));
        assert!(!criteria.contains_key("unknown"));
        assert!(criteria["entailed"]
            .as_str()
            .unwrap()
            .contains("body supplies information"));
        assert!(criteria["neutral"]
            .as_str()
            .unwrap()
            .contains("neither supports nor refutes"));
        assert!(body["state"].get("children").is_none());
        assert_eq!(body["state"]["path"], "ui/app.py");
        assert_eq!(
            body["state"]["atom"]["statement"],
            "file \"ui/app.py\" (partially) defines component \"Backend\""
        );
        let instr = body["questions"]["ground"]["instructions"]
            .as_str()
            .unwrap();
        assert_eq!(
            instr,
            "How does `body` ground the following statement: file \"ui/app.py\" (partially) defines component \"Backend\""
        );
    }

    #[test]
    fn conflict_request_asks_value_and_consistency() {
        let state = ConflictState {
            pred: "GPUCompiler".into(),
            args: vec![],
            verbalization: "the system has a gpu-based compiler".into(),
            evidence: vec![
                ConflictEvidence {
                    path: "a.rs".into(),
                    verdict: DocumentVerdict::Entailed,
                    confidence: 0.9,
                    excerpt: "gpu backend".into(),
                },
                ConflictEvidence {
                    path: "b.rs".into(),
                    verdict: DocumentVerdict::Contradicted,
                    confidence: 0.8,
                    excerpt: "cpu only".into(),
                },
            ],
        };
        let body = conflict_request("jev-latest", &state);
        assert_eq!(body["questions"]["value"]["type"], "choice");
        assert_eq!(body["questions"]["consistency"]["type"], "choice");
        let value = body["questions"]["value"]["criteria"].as_object().unwrap();
        assert!(value.contains_key("true"));
        assert!(value.contains_key("false"));
        assert!(value.contains_key("unknown"));
        let cons = body["questions"]["consistency"]["criteria"]
            .as_object()
            .unwrap();
        assert!(cons.contains_key("consistent"));
        assert!(cons.contains_key("conflicting"));
        assert_eq!(body["state"]["evidence"].as_array().unwrap().len(), 2);
        assert_eq!(body["state"]["evidence"][0]["verdict"], "entailed");
        assert_eq!(body["state"]["evidence"][1]["verdict"], "contradicted");
        assert!(body["questions"]["value"]["instructions"]
            .as_str()
            .unwrap()
            .contains("the system has a gpu-based compiler"));
        assert!(body["state"].get("body").is_none());

        let parsed = parse_conflict_answer(&json!({
            "answers": {
                "value": { "type": "choice", "choice": "unknown", "confidence": 0.7 },
                "consistency": { "type": "choice", "choice": "conflicting", "confidence": 0.9 }
            }
        }))
        .unwrap();
        assert_eq!(parsed.value, ResolveValue::Unknown);
        assert_eq!(parsed.consistency, Consistency::Conflicting);
        assert!((parsed.value_confidence - 0.7).abs() < 1e-9);
        assert!((parsed.consistency_confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn parse_listing_and_document_choices() {
        let children = vec![child("frontend", true), child("decoy", true)];
        let resp = json!({
            "answers": {
                "c0": { "type": "choice", "choice": "investigate", "confidence": 0.91 },
                "c1": { "type": "choice", "choice": "skip", "confidence": 0.88 }
            }
        });
        let choices = parse_listing_answers(&resp, &children, 0).unwrap();
        assert_eq!(choices[0].transition, Transition::Investigate);
        assert_eq!(choices[1].transition, Transition::Skip);

        let doc_resp = json!({
            "answers": {
                "ground": { "type": "choice", "choice": "entailed", "confidence": 0.9 }
            }
        });
        let n = parse_document_answer(&doc_resp).unwrap();
        assert_eq!(n.verdict, DocumentVerdict::Entailed);
        assert!((n.confidence - 0.9).abs() < 1e-9);

        let unknown_resp = json!({
            "answers": {
                "ground": { "type": "choice", "choice": "neutral", "confidence": 0.7 }
            }
        });
        let u = parse_document_answer(&unknown_resp).unwrap();
        assert_eq!(u.verdict, DocumentVerdict::Neutral);
        assert!(!u.is_entailed() && !u.is_contradicted());
    }

    use crate::oracle::jev::Posted;
    use crate::oracle::OracleError;

    struct SeqTransport {
        responses: Vec<Value>,
        pub bodies: Vec<Value>,
    }

    impl Transport for SeqTransport {
        fn post_json(&mut self, _url: &str, body: &Value) -> Result<Posted, OracleError> {
            self.bodies.push(body.clone());
            if self.responses.is_empty() {
                return Err(OracleError::Http("no more responses".into()));
            }
            Posted::capture(|| Ok(self.responses.remove(0)))
        }
    }

    #[test]
    fn choose_many_posts_one_listing_for_two_atoms() {
        let state = MultiListingState {
            atoms: vec![
                ListingAtom {
                    pred: "component".into(),
                    args: vec!["Frontend".into()],
                    verbalization: String::new(),
                },
                ListingAtom {
                    pred: "component".into(),
                    args: vec!["Backend".into()],
                    verbalization: String::new(),
                },
            ],
            dir: "".into(),
            children: vec![child("frontend", true), child("backend", true)],
        };
        let transport = SeqTransport {
            responses: vec![json!({
                "answers": {
                    "q0": { "type": "choice", "choice": "investigate", "confidence": 0.9 },
                    "q1": { "type": "choice", "choice": "skip", "confidence": 0.9 },
                    "q2": { "type": "choice", "choice": "skip", "confidence": 0.9 },
                    "q3": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
                }
            })],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let choices = judge.choose_many(&state).unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0][0].transition, Transition::Investigate);
        assert_eq!(choices[0][1].transition, Transition::Skip);
        assert_eq!(choices[1][0].transition, Transition::Skip);
        assert_eq!(choices[1][1].transition, Transition::Investigate);
        assert_eq!(judge.transport().bodies.len(), 1);
        let qs = judge.transport().bodies[0]["questions"]
            .as_object()
            .unwrap();
        assert_eq!(qs.len(), 4);
        assert!(judge.transport().bodies[0]["state"].get("atom").is_none());
        assert!(judge.transport().bodies[0]["state"].get("atoms").is_some());
        assert_eq!(judge.last_exchanges().len(), 1);
    }

    #[test]
    fn judge_uses_transport() {
        let state = ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![child("frontend", true)],
            verbalization: String::new(),
        };
        let transport = SeqTransport {
            responses: vec![json!({
                "answers": {
                    "q0": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
                }
            })],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let choices = judge.choose(&state).unwrap();
        assert_eq!(choices[0].transition, Transition::Investigate);
        let ex = judge.last_exchanges();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].request["model"], "jev-latest");
        assert_eq!(ex[0].response["answers"]["q0"]["choice"], "investigate");
        assert!(ex[0].started_at.is_some());
        assert!(ex[0].ended_at.is_some());
        assert!(ex[0].ended_at.unwrap() >= ex[0].started_at.unwrap());
    }

    fn doc_state(path: &str) -> DocumentState {
        DocumentState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            path: path.into(),
            body: format!("// {path}"),
            bindings: Vec::new(),
            verbalization: String::new(),
        }
    }

    fn fill_answers(body: &Value, choice: &str, confidence: f64) -> Value {
        let mut answers = serde_json::Map::new();
        if let Some(qs) = body.get("questions").and_then(|q| q.as_object()) {
            for key in qs.keys() {
                answers.insert(
                    key.clone(),
                    json!({ "type": "choice", "choice": choice, "confidence": confidence }),
                );
            }
        }
        json!({ "answers": answers })
    }

    struct BatchSpy {
        batches: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
        /// If set, used as the response for each body (question keys must match).
        responses: Vec<Value>,
        /// When `responses` is empty, every question in the body gets this choice.
        default_choice: Option<(&'static str, f64)>,
    }

    impl Transport for BatchSpy {
        fn post_json(&mut self, _url: &str, _body: &Value) -> Result<Posted, OracleError> {
            panic!("documents should post via post_json_all");
        }

        fn post_json_all(
            &mut self,
            _url: &str,
            bodies: &[Value],
        ) -> Vec<Result<Posted, OracleError>> {
            self.batches.lock().unwrap().push(bodies.len());
            bodies
                .iter()
                .map(|body| {
                    if !self.responses.is_empty() {
                        Posted::capture(|| Ok(self.responses.remove(0)))
                    } else if let Some((choice, conf)) = self.default_choice {
                        Posted::capture(|| Ok(fill_answers(body, choice, conf)))
                    } else {
                        Err(OracleError::Http("no more responses".into()))
                    }
                })
                .collect()
        }
    }

    #[test]
    fn documents_posts_one_batch() {
        let batches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = BatchSpy {
            batches: batches.clone(),
            responses: Vec::new(),
            default_choice: Some(("entailed", 0.9)),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let states = vec![doc_state("a.py"), doc_state("b.py"), doc_state("c.py")];
        let answers = judge.documents(&states);
        assert_eq!(answers.len(), 3);
        assert!(answers
            .iter()
            .all(|a| a.as_ref().unwrap().judgement.is_entailed()));
        assert_eq!(*batches.lock().unwrap(), vec![3]);
        let ex = judge.last_exchanges();
        assert_eq!(ex.len(), 3);
        assert_eq!(ex[0].request["state"]["path"], "a.py");
        assert_eq!(ex[1].request["state"]["path"], "b.py");
        assert_eq!(ex[2].request["state"]["path"], "c.py");
        assert!(ex
            .iter()
            .all(|e| e.started_at.is_some() && e.ended_at.is_some()));
    }

    fn many_doc_resp() -> Value {
        json!({
            "answers": {
                "q0": { "type": "choice", "choice": "entailed", "confidence": 0.9 },
                "q1": { "type": "choice", "choice": "contradicted", "confidence": 0.8 }
            }
        })
    }

    #[test]
    fn documents_group_same_path_into_one_post() {
        let batches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = BatchSpy {
            batches: batches.clone(),
            responses: vec![many_doc_resp()],
            default_choice: None,
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let a = DocumentState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            path: "readme.md".into(),
            body: "hello".into(),
            bindings: Vec::new(),
            verbalization: "\"Frontend\" is a component in the codebase".into(),
        };
        let b = DocumentState {
            pred: "component".into(),
            args: vec!["Backend".into()],
            path: "readme.md".into(),
            body: "hello".into(),
            bindings: Vec::new(),
            verbalization: "\"Backend\" is a component in the codebase".into(),
        };
        let answers = judge.documents(&[a, b]);
        assert_eq!(*batches.lock().unwrap(), vec![1]);
        assert!(answers[0].as_ref().unwrap().judgement.is_entailed());
        assert!(answers[1].as_ref().unwrap().judgement.is_contradicted());
        let ex = judge.last_exchanges();
        assert_eq!(ex.len(), 1);
        assert!(ex[0].request["questions"].get("q0").is_some());
        assert!(ex[0].request["questions"].get("q1").is_some());
        assert!(ex[0].request["questions"].get("ground").is_none());
        assert_eq!(ex[0].request["state"]["path"], "readme.md");
        assert_eq!(
            ex[0].request["questions"]["q0"]["instructions"],
            "How does `body` ground the following statement: \"Frontend\" is a component in the codebase"
        );
        assert_eq!(
            ex[0].request["questions"]["q1"]["instructions"],
            "How does `body` ground the following statement: \"Backend\" is a component in the codebase"
        );
        let criteria = ex[0].request["questions"]["q0"]["criteria"]
            .as_object()
            .unwrap();
        assert!(criteria.contains_key("entailed"));
        assert!(criteria.contains_key("contradicted"));
        assert!(criteria.contains_key("neutral"));
    }

    #[test]
    fn evaluate_records_usage_on_exchange() {
        let state = ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![child("frontend", true)],
            verbalization: String::new(),
        };
        let transport = SeqTransport {
            responses: vec![json!({
                "answers": {
                    "q0": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
                },
                "usage": { "input_tokens": 2000, "output_tokens": 8 }
            })],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.choose(&state).unwrap();
        let ex = &judge.last_exchanges()[0];
        assert_eq!(ex.input_tokens, Some(2000));
        assert_eq!(ex.output_tokens, Some(8));
        assert!((ex.cost_usd.unwrap() - 2000.0 * 0.042 / 1_000_000.0).abs() < 1e-15);
    }

    fn listing_state() -> ListingState {
        ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![child("frontend", true)],
            verbalization: String::new(),
        }
    }

    fn listing_resp(ticket: u64, cost_usd: f64) -> Value {
        let mut answers = serde_json::Map::new();
        answers.insert(
            format!("q{ticket}"),
            json!({ "type": "choice", "choice": "investigate", "confidence": 0.9 }),
        );
        json!({
            "answers": answers,
            "usage": { "input_tokens": 10, "cost_usd": cost_usd }
        })
    }

    #[test]
    fn keeps_the_request_that_crosses_the_run_cost_limit() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(0, 1.25)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let choices = judge.choose(&state).unwrap();
        assert_eq!(choices[0].transition, Transition::Investigate);
        assert!((judge.last_exchanges()[0].cost_usd.unwrap() - 1.25).abs() < 1e-12);
        assert!((judge.usage().spend_usd - 1.25).abs() < 1e-12);
    }

    #[test]
    fn refuses_further_jev_after_run_cost_limit() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(0, 1.0), listing_resp(1, 0.01)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.choose(&state).unwrap();
        let err = judge.choose(&state).unwrap_err();
        assert!(matches!(err, JudgeError::Limit(_)), "{err}");
        assert!(err.to_string().contains("$1.00"));
        assert_eq!(judge.transport().bodies.len(), 1);
    }

    #[test]
    fn requests_under_the_run_cost_limit_keep_going() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(0, 0.4), listing_resp(1, 0.4)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.choose(&state).unwrap();
        judge.choose(&state).unwrap();
        assert_eq!(judge.transport().bodies.len(), 2);
        assert!((judge.usage().spend_usd - 0.8).abs() < 1e-12);
    }

    #[test]
    fn documents_do_not_post_after_run_cost_limit() {
        let batches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = BatchSpy {
            batches: batches.clone(),
            responses: Vec::new(),
            default_choice: Some(("entailed", 0.9)),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.set_spent_usd(1.0);
        let answers = judge.documents(&[doc_state("a.py")]);
        assert!(matches!(answers[0], Err(JudgeError::Limit(_))));
        assert!(batches.lock().unwrap().is_empty());
    }
}
