use std::collections::{BTreeMap, HashSet};
use std::time::SystemTime;

use serde_json::{json, Value};

use super::judge::{
    Child, ChildChoice, ConflictAnswer, ConflictState, Consistency, DocumentAnswer,
    DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError, JudgeExchange,
    ListingState, MultiListingState, ResolveValue, Transition, LISTING_CHOICE_BUDGET,
};
use super::walk::parent_dir;

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";

const MAX_RETRIES: u32 = 4;
const DOCUMENT_PARALLELISM: usize = 8;
/// Published Jev input price. Output tokens are unmetered.
const INPUT_USD_PER_MILLION_TOKENS: f64 = 0.042;
/// Hard cap on recorded Jev spend for one `autoground run`.
const RUN_COST_LIMIT_USD: f64 = 1.0;

/// One transport round-trip, with wall-clock bounds for the trace.
#[derive(Debug, Clone)]
pub struct Posted {
    pub value: Value,
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}

impl Posted {
    fn capture(f: impl FnOnce() -> Result<Value, JudgeError>) -> Result<Self, JudgeError> {
        let started_at = SystemTime::now();
        let value = f()?;
        Ok(Self {
            value,
            started_at,
            ended_at: SystemTime::now(),
        })
    }
}

/// POST body / response transport. Live HTTP in production; a mock in tests.
pub trait Transport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Posted, JudgeError>;

    /// Default posts one body at a time. Live HTTP overrides this to overlap
    /// independent document requests.
    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Posted, JudgeError>> {
        bodies.iter().map(|b| self.post_json(url, b)).collect()
    }
}

pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Posted, JudgeError> {
        Posted::capture(|| post_json_with_retries(url, body))
    }

    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Posted, JudgeError>> {
        parallel_map(bodies, DOCUMENT_PARALLELISM, |body| {
            Posted::capture(|| post_json_with_retries(url, body))
        })
    }
}

fn post_json_with_retries(url: &str, body: &Value) -> Result<Value, JudgeError> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| JudgeError::Config("TYPESAFE_API_KEY is not set".into()))?;
    let mut delay_ms = 200u64;
    let mut last = String::new();
    for attempt in 0..MAX_RETRIES {
        let req = ureq::post(url)
            .set("Authorization", &format!("Bearer {key}"))
            .set("Content-Type", "application/json")
            .timeout(std::time::Duration::from_secs(120));
        match req.send_json(body.clone()) {
            Ok(resp) => {
                return resp
                    .into_json()
                    .map_err(|e| JudgeError::Parse(e.to_string()));
            }
            Err(ureq::Error::Status(code, resp)) => {
                last = resp
                    .into_string()
                    .unwrap_or_else(|_| format!("HTTP {code}"));
                if matches!(code, 429 | 529 | 500 | 502 | 503) && attempt + 1 < MAX_RETRIES {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = delay_ms.saturating_mul(2);
                    continue;
                }
                return Err(JudgeError::Http(format!("HTTP {code}: {last}")));
            }
            Err(e) => {
                last = e.to_string();
                if attempt + 1 < MAX_RETRIES {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = delay_ms.saturating_mul(2);
                    continue;
                }
                return Err(JudgeError::Http(last));
            }
        }
    }
    Err(JudgeError::Http(last))
}

fn parallel_map<T, R, F>(items: &[T], limit: usize, f: F) -> Vec<R>
where
    T: Sync,
    R: Send,
    F: Fn(&T) -> R + Sync,
{
    if items.is_empty() {
        return Vec::new();
    }
    let limit = limit.max(1);
    if items.len() == 1 || limit == 1 {
        return items.iter().map(f).collect();
    }
    let mut out = Vec::with_capacity(items.len());
    for chunk in items.chunks(limit) {
        std::thread::scope(|scope| {
            let handles: Vec<_> = chunk.iter().map(|item| scope.spawn(|| f(item))).collect();
            for handle in handles {
                out.push(handle.join().expect("parallel document request panicked"));
            }
        });
    }
    out
}

/// Jev System One judge. One listing request = one Choice per child.
/// Document requests are a single Noul and never share a listing state.
pub struct JevJudge<T: Transport = UreqTransport> {
    transport: T,
    endpoint: String,
    model: String,
    exchanges: Vec<JudgeExchange>,
    spent_usd: f64,
}

impl JevJudge<UreqTransport> {
    pub fn from_env() -> Result<Self, JudgeError> {
        if std::env::var("TYPESAFE_API_KEY").is_err() {
            return Err(JudgeError::Config(
                "TYPESAFE_API_KEY is not set; pass --judge heuristic or set the key".into(),
            ));
        }
        let base = std::env::var("TYPESAFE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.into());
        let model =
            std::env::var("TYPESAFE_DEFAULT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        Ok(Self {
            transport: UreqTransport,
            endpoint: format!("{}/v1/systemone", base.trim_end_matches('/')),
            model,
            exchanges: Vec::new(),
            spent_usd: 0.0,
        })
    }
}

impl<T: Transport> JevJudge<T> {
    pub fn new(transport: T, endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            transport,
            endpoint: endpoint.into(),
            model: model.into(),
            exchanges: Vec::new(),
            spent_usd: 0.0,
        }
    }

    fn guard_budget(&self) -> Result<(), JudgeError> {
        if self.spent_usd >= RUN_COST_LIMIT_USD {
            Err(self.budget_error())
        } else {
            Ok(())
        }
    }

    fn budget_error(&self) -> JudgeError {
        JudgeError::Limit(format!(
            "run cost limit of ${:.2} exceeded (spent ${:.4})",
            RUN_COST_LIMIT_USD, self.spent_usd
        ))
    }

    fn add_spend(&mut self, cost: Option<f64>) {
        if let Some(c) = cost {
            self.spent_usd += c;
        }
    }

    fn evaluate(&mut self, body: &Value) -> Result<Value, JudgeError> {
        self.guard_budget()?;
        let posted = self.transport.post_json(&self.endpoint, body)?;
        let value = posted.value.clone();
        let ex = exchange_from_posted(body.clone(), posted);
        self.add_spend(ex.cost_usd);
        self.exchanges.push(ex);
        Ok(value)
    }
}

impl<T: Transport> Judge for JevJudge<T> {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        self.exchanges.clear();
        let mut out = Vec::with_capacity(state.children.len());
        for chunk in state.children.chunks(LISTING_CHOICE_BUDGET) {
            let body = listing_request(&self.model, state, chunk, out.len());
            let resp = self.evaluate(&body)?;
            out.extend(parse_listing_answers(&resp, chunk, out.len())?);
        }
        Ok(out)
    }

    fn choose_many(
        &mut self,
        state: &MultiListingState,
    ) -> Result<Vec<Vec<ChildChoice>>, JudgeError> {
        if state.atoms.len() <= 1 {
            let choices = match state.atoms.first() {
                Some(atom) => self.choose(&state.listing_state(atom))?,
                None => Vec::new(),
            };
            return Ok(state.atoms.iter().map(|_| choices.clone()).collect());
        }
        self.exchanges.clear();
        let n_atoms = state.atoms.len();
        let n_children = state.children.len();
        let mut tables: Vec<Vec<Option<ChildChoice>>> = vec![vec![None; n_children]; n_atoms];
        let pairs: Vec<(usize, usize)> = (0..n_atoms)
            .flat_map(|a| (0..n_children).map(move |c| (a, c)))
            .collect();
        for chunk in pairs.chunks(LISTING_CHOICE_BUDGET) {
            let body = listing_request_many(&self.model, state, chunk);
            let resp = self.evaluate(&body)?;
            for &(a, c) in chunk {
                let key = listing_pair_key(a, c);
                tables[a][c] = Some(parse_listing_choice(&resp, &key, &state.children[c])?);
            }
        }
        tables
            .into_iter()
            .map(|row| {
                row.into_iter()
                    .enumerate()
                    .map(|(c, choice)| {
                        choice.ok_or_else(|| {
                            JudgeError::Parse(format!(
                                "missing listing choice for {}",
                                state.children[c].rel_path
                            ))
                        })
                    })
                    .collect()
            })
            .collect()
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
        if let Err(e) = self.guard_budget() {
            return states.iter().map(|_| Err(e.clone())).collect();
        }
        let groups = group_documents_by_path(states);
        let bodies: Vec<Value> = groups
            .iter()
            .map(|idxs| document_group_request(&self.model, states, idxs))
            .collect();
        let responses = self.transport.post_json_all(&self.endpoint, &bodies);
        let mut out: Vec<Option<Result<DocumentAnswer, JudgeError>>> =
            (0..states.len()).map(|_| None).collect();
        for ((idxs, request), resp) in groups.iter().zip(bodies).zip(responses) {
            match resp {
                Ok(posted) => {
                    let response = posted.value.clone();
                    let ex = exchange_from_posted(request, posted);
                    self.add_spend(ex.cost_usd);
                    self.exchanges.push(ex.clone());
                    if idxs.len() == 1 {
                        out[idxs[0]] = Some(parse_document_answer(&response).map(|judgement| {
                            DocumentAnswer {
                                judgement,
                                exchanges: vec![ex],
                            }
                        }));
                    } else {
                        match parse_document_answers_many(&response, idxs.len()) {
                            Ok(judgements) => {
                                for (k, &i) in idxs.iter().enumerate() {
                                    let exchanges =
                                        if k == 0 { vec![ex.clone()] } else { Vec::new() };
                                    out[i] = Some(Ok(DocumentAnswer {
                                        judgement: judgements[k].clone(),
                                        exchanges,
                                    }));
                                }
                            }
                            Err(e) => {
                                for &i in idxs {
                                    out[i] = Some(Err(e.clone()));
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    for &i in idxs {
                        out[i] = Some(Err(e.clone()));
                    }
                }
            }
        }
        out.into_iter()
            .map(|o| o.expect("document group covered every state"))
            .collect()
    }

    fn resolve_conflict(&mut self, state: &ConflictState) -> Result<ConflictAnswer, JudgeError> {
        self.exchanges.clear();
        let body = conflict_request(&self.model, state);
        let resp = self.evaluate(&body)?;
        let mut answer = parse_conflict_answer(&resp)?;
        answer.exchanges = self.exchanges.clone();
        Ok(answer)
    }

    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        self.exchanges.clone()
    }
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
                "instructions": format!(
                    "Should we look in {path} for proof or disproof of the following statement: {statement}"
                ),
                "criteria": {
                    "skip": "Do not look here, or in anything nested under this path. The name, path, and listing tree show this is unrelated.",
                    "investigate": "Look here. Nested paths already in this listing are used when a directory is investigated; files are opened and judged.",
                }
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
                "instructions": format!(
                    "Should we look in {path} for proof or disproof of the following statement: {statement}"
                ),
                "criteria": {
                    "skip": "Do not look here, or in anything nested under this path. The name, path, and listing tree show this is unrelated.",
                    "investigate": "Look here. Nested paths already in this listing are used when a directory is investigated; files are opened and judged.",
                }
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

fn parse_document_answers_many(
    resp: &Value,
    n: usize,
) -> Result<Vec<DocumentJudgement>, JudgeError> {
    let answers = resp
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| JudgeError::Parse("response missing answers".into()))?;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let key = format!("q{i}");
        let ans = answers
            .get(&key)
            .ok_or_else(|| JudgeError::Parse(format!("missing answer {key}")))?;
        out.push(parse_document_choice(ans)?);
    }
    Ok(out)
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

fn group_documents_by_path(states: &[DocumentState]) -> Vec<Vec<usize>> {
    let mut order = Vec::new();
    let mut by_path: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, s) in states.iter().enumerate() {
        let entry = by_path.entry(s.path.clone()).or_insert_with(|| {
            order.push(s.path.clone());
            Vec::new()
        });
        entry.push(i);
    }
    order
        .into_iter()
        .map(|p| by_path.remove(&p).unwrap_or_default())
        .collect()
}

fn document_group_request(model: &str, states: &[DocumentState], idxs: &[usize]) -> Value {
    if idxs.len() == 1 {
        investigation_request(model, &states[idxs[0]])
    } else {
        let group: Vec<DocumentState> = idxs.iter().map(|&i| states[i].clone()).collect();
        investigation_request_many(model, &group)
    }
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

fn exchange_from_posted(request: Value, posted: Posted) -> JudgeExchange {
    let (input_tokens, output_tokens, cost_usd) = usage_from(&posted.value);
    JudgeExchange {
        request,
        response: posted.value,
        started_at: Some(posted.started_at),
        ended_at: Some(posted.ended_at),
        input_tokens,
        output_tokens,
        cost_usd,
    }
}

/// Token counts and USD cost from a System One response. Cost prefers an
/// explicit `usage.cost` / `usage.cost_usd` field; otherwise it is input
/// tokens at the published Jev input price.
fn usage_from(response: &Value) -> (Option<u64>, Option<u64>, Option<f64>) {
    let Some(usage) = response.get("usage") else {
        return (None, None, None);
    };
    let input = int_field(usage, "input_tokens");
    let output = int_field(usage, "output_tokens");
    let cost = usage
        .get("cost_usd")
        .or_else(|| usage.get("cost"))
        .and_then(|v| v.as_f64())
        .or_else(|| input.map(|n| n as f64 * INPUT_USD_PER_MILLION_TOKENS / 1_000_000.0));
    (input, output, cost)
}

fn int_field(obj: &Value, key: &str) -> Option<u64> {
    obj.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_i64().and_then(|n| u64::try_from(n).ok()))
            .or_else(|| v.as_f64().and_then(|n| (n >= 0.0).then_some(n as u64)))
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
        "instructions": format!(
            "How does document '`body`' at `path`  ground the following statement: {statement}"
        ),
        "criteria": {
            "entailed": "body supplies information that makes the statement true",
            "contradicted": "body supplies information that makes the statement false",
            "neutral": "body neither supports nor refutes the statement",
        }
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

    struct SeqTransport {
        responses: Vec<Value>,
        pub bodies: Vec<Value>,
    }

    impl Transport for SeqTransport {
        fn post_json(&mut self, _url: &str, body: &Value) -> Result<Posted, JudgeError> {
            self.bodies.push(body.clone());
            if self.responses.is_empty() {
                return Err(JudgeError::Http("no more responses".into()));
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
                    "a0c0": { "type": "choice", "choice": "investigate", "confidence": 0.9 },
                    "a0c1": { "type": "choice", "choice": "skip", "confidence": 0.9 },
                    "a1c0": { "type": "choice", "choice": "skip", "confidence": 0.9 },
                    "a1c1": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
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
        assert_eq!(judge.transport.bodies.len(), 1);
        assert!(judge.transport.bodies[0]["questions"].get("a0c0").is_some());
        assert!(judge.transport.bodies[0]["state"].get("atom").is_none());
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
                    "c0": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
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
        assert_eq!(ex[0].response["answers"]["c0"]["choice"], "investigate");
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

    fn true_doc_resp() -> Value {
        json!({
            "answers": {
                "ground": { "type": "choice", "choice": "entailed", "confidence": 0.9 }
            }
        })
    }

    struct BatchSpy {
        batches: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
        responses: Vec<Value>,
    }

    impl Transport for BatchSpy {
        fn post_json(&mut self, _url: &str, _body: &Value) -> Result<Posted, JudgeError> {
            panic!("documents should post via post_json_all");
        }

        fn post_json_all(
            &mut self,
            _url: &str,
            bodies: &[Value],
        ) -> Vec<Result<Posted, JudgeError>> {
            self.batches.lock().unwrap().push(bodies.len());
            bodies
                .iter()
                .map(|_| {
                    if self.responses.is_empty() {
                        Err(JudgeError::Http("no more responses".into()))
                    } else {
                        Posted::capture(|| Ok(self.responses.remove(0)))
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
            responses: vec![true_doc_resp(), true_doc_resp(), true_doc_resp()],
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
    fn usage_from_tokens_computes_jev_input_price() {
        let (input, output, cost) = usage_from(&json!({
            "answers": {},
            "usage": { "input_tokens": 1_000_000, "output_tokens": 12 }
        }));
        assert_eq!(input, Some(1_000_000));
        assert_eq!(output, Some(12));
        assert!((cost.unwrap() - 0.042).abs() < 1e-12);
    }

    #[test]
    fn usage_from_prefers_explicit_cost() {
        let (_, _, cost) = usage_from(&json!({
            "usage": { "input_tokens": 10, "cost_usd": 1.25 }
        }));
        assert!((cost.unwrap() - 1.25).abs() < 1e-12);
    }

    #[test]
    fn usage_from_missing_is_none() {
        assert_eq!(usage_from(&json!({"answers": {}})), (None, None, None));
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
                    "c0": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
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

    fn listing_resp(cost_usd: f64) -> Value {
        json!({
            "answers": {
                "c0": { "type": "choice", "choice": "investigate", "confidence": 0.9 }
            },
            "usage": { "input_tokens": 10, "cost_usd": cost_usd }
        })
    }

    #[test]
    fn keeps_the_request_that_crosses_the_run_cost_limit() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(1.25)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let choices = judge.choose(&state).unwrap();
        assert_eq!(choices[0].transition, Transition::Investigate);
        assert!((judge.last_exchanges()[0].cost_usd.unwrap() - 1.25).abs() < 1e-12);
        assert!((judge.spent_usd - 1.25).abs() < 1e-12);
    }

    #[test]
    fn refuses_further_jev_after_run_cost_limit() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(1.0), listing_resp(0.01)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.choose(&state).unwrap();
        let err = judge.choose(&state).unwrap_err();
        assert!(matches!(err, JudgeError::Limit(_)), "{err}");
        assert!(err.to_string().contains("$1.00"));
        assert_eq!(judge.transport.bodies.len(), 1);
    }

    #[test]
    fn requests_under_the_run_cost_limit_keep_going() {
        let state = listing_state();
        let transport = SeqTransport {
            responses: vec![listing_resp(0.4), listing_resp(0.4)],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.choose(&state).unwrap();
        judge.choose(&state).unwrap();
        assert_eq!(judge.transport.bodies.len(), 2);
        assert!((judge.spent_usd - 0.8).abs() < 1e-12);
    }

    #[test]
    fn documents_do_not_post_after_run_cost_limit() {
        let batches = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let transport = BatchSpy {
            batches: batches.clone(),
            responses: vec![true_doc_resp()],
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        judge.spent_usd = RUN_COST_LIMIT_USD;
        let answers = judge.documents(&[doc_state("a.py")]);
        assert!(matches!(answers[0], Err(JudgeError::Limit(_))));
        assert!(batches.lock().unwrap().is_empty());
    }

    #[test]
    fn parallel_map_overlaps_work() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let inflight = AtomicUsize::new(0);
        let max = AtomicUsize::new(0);
        let items: Vec<u8> = (0..6).collect();
        let _ = parallel_map(&items, 6, |_| {
            let n = inflight.fetch_add(1, Ordering::SeqCst) + 1;
            max.fetch_max(n, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(30));
            inflight.fetch_sub(1, Ordering::SeqCst);
            1u8
        });
        assert!(
            max.load(Ordering::SeqCst) > 1,
            "expected overlapping document work, max inflight {}",
            max.load(Ordering::SeqCst)
        );
    }
}
