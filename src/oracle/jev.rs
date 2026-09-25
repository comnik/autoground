use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use super::{
    Answer, Classification, Oracle, OracleError, OracleExchange, QuestionKind, Ticket, Usage,
};

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";

const MAX_RETRIES: u32 = 4;
/// In-flight POSTs. Jev publishes 250k input tokens per second and 1,200
/// requests per minute; both can change without notice. A full search
/// request is about 15k tokens, so 16 fills the token budget. The request
/// cap is looser.
pub const POST_PARALLELISM: usize = 16;
/// Published Jev input price. Output tokens are unmetered.
const INPUT_USD_PER_MILLION_TOKENS: f64 = 0.042;
/// Hard cap on recorded Jev spend for one oracle instance.
const RUN_COST_LIMIT_USD: f64 = 1.0;
/// Max Choice questions in one System One POST.
pub const QUESTION_BUDGET: usize = 100;
/// Documented Jev cap on `state` plus the longest question. Fill uses billed
/// `input_tokens` against this 32k budget, not the 64k total envelope.
pub const CONTEXT_TOKEN_BUDGET: u64 = 32_000;

/// One transport round-trip, with wall-clock bounds for the trace.
#[derive(Debug, Clone)]
pub struct Posted {
    pub value: Value,
    pub started_at: SystemTime,
    pub ended_at: SystemTime,
}

impl Posted {
    pub fn capture(f: impl FnOnce() -> Result<Value, OracleError>) -> Result<Self, OracleError> {
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
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Posted, OracleError>;

    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Posted, OracleError>> {
        bodies.iter().map(|b| self.post_json(url, b)).collect()
    }
}

pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Posted, OracleError> {
        Posted::capture(|| post_json_with_retries(url, body))
    }

    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Posted, OracleError>> {
        parallel_map(bodies, POST_PARALLELISM, |body| {
            Posted::capture(|| post_json_with_retries(url, body))
        })
    }
}

fn post_json_with_retries(url: &str, body: &Value) -> Result<Value, OracleError> {
    let key = std::env::var("TYPESAFE_API_KEY")
        .map_err(|_| OracleError::Config("TYPESAFE_API_KEY is not set".into()))?;
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
                    .map_err(|e| OracleError::Parse(e.to_string()));
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
                return Err(OracleError::Http(format!("HTTP {code}: {last}")));
            }
            Err(e) => {
                last = e.to_string();
                if attempt + 1 < MAX_RETRIES {
                    std::thread::sleep(std::time::Duration::from_millis(delay_ms));
                    delay_ms = delay_ms.saturating_mul(2);
                    continue;
                }
                return Err(OracleError::Http(last));
            }
        }
    }
    Err(OracleError::Http(last))
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
                out.push(handle.join().expect("parallel oracle request panicked"));
            }
        });
    }
    out
}

#[derive(Clone)]
struct Pending {
    ticket: Ticket,
    req: Classification,
}

/// Jev System One oracle. `submit` queues; `wait` groups by state digest,
/// chunks at [`QUESTION_BUDGET`], and overlaps independent POSTs.
pub struct JevOracle<T: Transport = UreqTransport> {
    pub transport: T,
    endpoint: String,
    model: String,
    next_ticket: u64,
    pending: Vec<Pending>,
    resolved: HashMap<Ticket, Result<Answer, OracleError>>,
    spent_usd: f64,
    input_tokens: u64,
    requests: u64,
    token_requests: u64,
    questions: u64,
    latencies: Vec<Duration>,
    last_exchanges: Vec<OracleExchange>,
    /// Production HTTP overlaps posts. Tests post inline so a flush is deterministic.
    parallel: bool,
    inflight: usize,
    next_flight: u64,
    flights: HashMap<u64, Flight>,
    done_tx: Sender<Done>,
    done_rx: Receiver<Done>,
}

struct Flight {
    request: Value,
    tickets: Vec<(Ticket, QuestionKind)>,
}

struct Done {
    id: u64,
    result: Result<Posted, OracleError>,
}

impl JevOracle<UreqTransport> {
    pub fn from_env() -> Result<Self, OracleError> {
        if std::env::var("TYPESAFE_API_KEY").is_err() {
            return Err(OracleError::Config(
                "TYPESAFE_API_KEY is not set; pass --judge heuristic or set the key".into(),
            ));
        }
        let base = std::env::var("TYPESAFE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE.into());
        let model =
            std::env::var("TYPESAFE_DEFAULT_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.into());
        let mut oracle = Self::new(
            UreqTransport,
            format!("{}/v1/systemone", base.trim_end_matches('/')),
            model,
        );
        oracle.parallel = true;
        Ok(oracle)
    }
}

impl<T: Transport> JevOracle<T> {
    pub fn new(transport: T, endpoint: impl Into<String>, model: impl Into<String>) -> Self {
        let (done_tx, done_rx) = mpsc::channel();
        Self {
            transport,
            endpoint: endpoint.into(),
            model: model.into(),
            next_ticket: 0,
            pending: Vec::new(),
            resolved: HashMap::new(),
            spent_usd: 0.0,
            input_tokens: 0,
            requests: 0,
            token_requests: 0,
            questions: 0,
            latencies: Vec::new(),
            last_exchanges: Vec::new(),
            parallel: false,
            inflight: 0,
            next_flight: 0,
            flights: HashMap::new(),
            done_tx,
            done_rx,
        }
    }

    pub fn last_exchanges(&self) -> &[OracleExchange] {
        &self.last_exchanges
    }

    pub fn set_spent_usd(&mut self, spent: f64) {
        self.spent_usd = spent;
    }

    fn budget_error(&self) -> OracleError {
        OracleError::Limit(format!(
            "run cost limit of ${:.2} exceeded (spent ${:.4})",
            RUN_COST_LIMIT_USD, self.spent_usd
        ))
    }

    fn over_budget(&self) -> bool {
        self.spent_usd >= RUN_COST_LIMIT_USD
    }

    fn add_spend(&mut self, cost: Option<f64>) {
        if let Some(c) = cost {
            self.spent_usd += c;
        }
    }

    fn record_request(&mut self, ex: &OracleExchange, questions: usize) {
        self.add_spend(ex.cost_usd);
        if let Some(n) = ex.input_tokens {
            self.input_tokens += n;
            self.token_requests += 1;
        }
        self.requests += 1;
        self.questions += questions as u64;
        if let (Some(start), Some(end)) = (ex.started_at, ex.ended_at) {
            if let Ok(d) = end.duration_since(start) {
                self.latencies.push(d);
            }
        }
    }

    fn is_pending(&self, ticket: Ticket) -> bool {
        self.pending.iter().any(|p| p.ticket == ticket)
    }

    fn is_inflight(&self, ticket: Ticket) -> bool {
        self.flights
            .values()
            .any(|flight| flight.tickets.iter().any(|(id, _)| *id == ticket))
    }

    fn known(&self, ticket: Ticket) -> bool {
        self.resolved.contains_key(&ticket) || self.is_pending(ticket) || self.is_inflight(ticket)
    }

    /// Start queued posts until [`POST_PARALLELISM`] are running.
    ///
    /// A sync transport posts inline, so the cap never stays full and every
    /// queued chunk goes out before `wait` returns. Live HTTP leaves the
    /// other posts running and starts the next queued one when a slot frees.
    fn pump(&mut self) {
        if self.over_budget() {
            let err = self.budget_error();
            for item in self.pending.drain(..) {
                self.resolved.insert(item.ticket, Err(err.clone()));
            }
            return;
        }
        while self.inflight < POST_PARALLELISM {
            let Some(items) = self.take_chunk() else {
                return;
            };
            let state = items[0].req.state.clone();
            let request = request_body(&self.model, &state, &items);
            let tickets = items
                .iter()
                .map(|item| (item.ticket, item.req.kind))
                .collect::<Vec<_>>();
            if self.parallel {
                self.spawn(request, tickets);
            } else {
                let result = self.transport.post_json(&self.endpoint, &request);
                self.finish(request, tickets, result);
            }
        }
    }

    /// One same-state batch, at most [`QUESTION_BUDGET`] questions.
    fn take_chunk(&mut self) -> Option<Vec<Pending>> {
        if self.pending.is_empty() {
            return None;
        }
        let digest = state_digest(&self.pending[0].req.state);
        let mut taken = Vec::new();
        let mut kept = Vec::new();
        for item in self.pending.drain(..) {
            if state_digest(&item.req.state) == digest && taken.len() < QUESTION_BUDGET {
                taken.push(item);
            } else {
                kept.push(item);
            }
        }
        self.pending = kept;
        Some(taken)
    }

    fn spawn(&mut self, request: Value, tickets: Vec<(Ticket, QuestionKind)>) {
        let id = self.next_flight;
        self.next_flight += 1;
        let endpoint = self.endpoint.clone();
        let body = request.clone();
        let tx = self.done_tx.clone();
        self.flights.insert(id, Flight { request, tickets });
        self.inflight += 1;
        thread::spawn(move || {
            let result = Posted::capture(|| post_json_with_retries(&endpoint, &body));
            let _ = tx.send(Done { id, result });
        });
    }

    fn reap_ready(&mut self) {
        while let Ok(done) = self.done_rx.try_recv() {
            self.accept(done);
        }
    }

    fn reap_one(&mut self) -> Result<(), OracleError> {
        let done = self
            .done_rx
            .recv()
            .map_err(|_| OracleError::Http("oracle post ended without a result".into()))?;
        self.accept(done);
        self.reap_ready();
        Ok(())
    }

    fn accept(&mut self, done: Done) {
        self.inflight = self.inflight.saturating_sub(1);
        let Some(flight) = self.flights.remove(&done.id) else {
            return;
        };
        self.finish(flight.request, flight.tickets, done.result);
    }

    fn finish(
        &mut self,
        request: Value,
        tickets: Vec<(Ticket, QuestionKind)>,
        result: Result<Posted, OracleError>,
    ) {
        match result {
            Ok(posted) => {
                let response = posted.value.clone();
                let n = tickets.len();
                let ex = exchange_from_posted(request, posted);
                self.record_request(&ex, n);
                self.last_exchanges.push(ex.clone());
                for (ticket, kind) in tickets {
                    let key = question_key(ticket);
                    let parsed = parse_answer(&response, &key, kind).map(|mut answer| {
                        answer.exchange = Some(ex.clone());
                        answer
                    });
                    self.resolved.insert(ticket, parsed);
                }
            }
            Err(error) => {
                for (ticket, _) in tickets {
                    self.resolved.insert(ticket, Err(error.clone()));
                }
            }
        }
    }
}

impl<T: Transport> Oracle for JevOracle<T> {
    fn submit(&mut self, req: Classification) -> Ticket {
        let ticket = Ticket(self.next_ticket);
        self.next_ticket += 1;
        self.pending.push(Pending { ticket, req });
        ticket
    }

    fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
        if !self.known(ticket) {
            return Err(OracleError::Parse(format!("unknown ticket {}", ticket.0)));
        }
        loop {
            self.pump();
            if let Some(done) = self.resolved.get(&ticket) {
                return done.clone();
            }
            self.reap_one()?;
        }
    }

    fn wait_any(&mut self, tickets: &[Ticket]) -> Result<Ticket, OracleError> {
        if tickets.is_empty() {
            return Err(OracleError::Parse("no tickets to wait for".into()));
        }
        loop {
            self.pump();
            if let Some(ticket) = tickets
                .iter()
                .copied()
                .find(|ticket| self.resolved.contains_key(ticket))
            {
                return Ok(ticket);
            }
            if tickets.iter().all(|ticket| !self.known(*ticket)) {
                return Err(OracleError::Parse("unknown ticket".into()));
            }
            self.reap_one()?;
        }
    }

    fn completed(&self, tickets: &[Ticket]) -> Vec<Ticket> {
        tickets
            .iter()
            .copied()
            .filter(|ticket| self.resolved.contains_key(ticket))
            .collect()
    }

    fn usage(&self) -> Usage {
        Usage {
            spend_usd: self.spent_usd,
            input_tokens: self.input_tokens,
            requests: self.requests,
            batch_size: (self.requests > 0)
                .then_some(self.questions as f64 / (self.requests as f64 * QUESTION_BUDGET as f64)),
            fill_factor: (self.token_requests > 0).then_some(
                self.input_tokens as f64
                    / (self.token_requests as f64 * CONTEXT_TOKEN_BUDGET as f64),
            ),
            latency: super::latency_percentiles(&self.latencies),
        }
    }
}

fn question_key(ticket: Ticket) -> String {
    format!("q{}", ticket.0)
}

fn request_body(model: &str, state: &Value, items: &[Pending]) -> Value {
    let mut questions = serde_json::Map::new();
    for p in items {
        let mut question = serde_json::Map::new();
        question.insert("type".into(), json!(p.req.kind.as_str()));
        question.insert("instructions".into(), p.req.instructions.clone());
        if !p.req.criteria.is_null() {
            question.insert("criteria".into(), p.req.criteria.clone());
        }
        questions.insert(question_key(p.ticket), Value::Object(question));
    }
    json!({
        "model": model,
        "state": state,
        "questions": questions,
    })
}

/// Digest of classification `state`. Callers pass that JSON; the oracle hashes
/// it and does not ask Universe for a digest.
fn state_digest(state: &Value) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    match serde_json::to_vec(state) {
        Ok(bytes) => bytes.hash(&mut hasher),
        Err(_) => 0.hash(&mut hasher),
    }
    hasher.finish()
}

fn parse_answer(resp: &Value, key: &str, kind: QuestionKind) -> Result<Answer, OracleError> {
    let answers = resp
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| OracleError::Parse("response missing answers".into()))?;
    let ans = answers
        .get(key)
        .ok_or_else(|| OracleError::Parse(format!("missing answer {key}")))?;
    match kind {
        QuestionKind::Choice => {
            let choice = ans
                .get("choice")
                .and_then(|c| c.as_str())
                .ok_or_else(|| OracleError::Parse(format!("answer {key} missing choice")))?;
            let confidence = ans
                .get("confidence")
                .and_then(|c| c.as_f64())
                .unwrap_or(0.0);
            Ok(Answer {
                choice: choice.to_string(),
                confidence,
                noul: None,
                exchange: None,
            })
        }
        QuestionKind::Noul => {
            let noul = ans
                .get("noul")
                .and_then(|c| c.as_f64())
                .ok_or_else(|| OracleError::Parse(format!("answer {key} missing noul")))?;
            Ok(Answer {
                choice: String::new(),
                confidence: noul,
                noul: Some(noul),
                exchange: None,
            })
        }
    }
}

fn exchange_from_posted(request: Value, posted: Posted) -> OracleExchange {
    let (input_tokens, output_tokens, cost_usd) = usage_from(&posted.value);
    OracleExchange {
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
pub fn usage_from(response: &Value) -> (Option<u64>, Option<u64>, Option<f64>) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::UNIX_EPOCH;

    fn criteria(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn classify(state: Value, instructions: &str) -> Classification {
        Classification::choice(
            state,
            instructions,
            criteria(&[("yes", "yes"), ("no", "no")]),
        )
    }

    fn choice_resp(keys: &[&str], choice: &str) -> Value {
        let mut answers = serde_json::Map::new();
        for k in keys {
            answers.insert(
                (*k).into(),
                json!({ "type": "choice", "choice": choice, "confidence": 0.9 }),
            );
        }
        json!({ "answers": answers })
    }

    struct SeqTransport {
        responses: Vec<Value>,
        pub bodies: Vec<Value>,
        latencies: Vec<Duration>,
    }

    impl Transport for SeqTransport {
        fn post_json(&mut self, _url: &str, body: &Value) -> Result<Posted, OracleError> {
            self.bodies.push(body.clone());
            if self.responses.is_empty() {
                return Err(OracleError::Http("no more responses".into()));
            }
            let value = self.responses.remove(0);
            if self.latencies.is_empty() {
                Posted::capture(|| Ok(value))
            } else {
                let d = self.latencies.remove(0);
                Ok(Posted {
                    value,
                    started_at: UNIX_EPOCH,
                    ended_at: UNIX_EPOCH + d,
                })
            }
        }
    }

    fn oracle(responses: Vec<Value>) -> JevOracle<SeqTransport> {
        JevOracle::new(
            SeqTransport {
                responses,
                bodies: Vec::new(),
                latencies: Vec::new(),
            },
            "http://example/v1/systemone",
            "jev-latest",
        )
    }

    fn oracle_with_latencies(
        responses: Vec<Value>,
        latencies: Vec<Duration>,
    ) -> JevOracle<SeqTransport> {
        JevOracle::new(
            SeqTransport {
                responses,
                bodies: Vec::new(),
                latencies,
            },
            "http://example/v1/systemone",
            "jev-latest",
        )
    }

    #[test]
    fn noul_posts_its_type_and_reads_the_probability() {
        let mut o = oracle(vec![json!({
            "answers": { "q0": { "type": "noul", "noul": 0.17 } }
        })]);
        let ticket = o.submit(Classification::noul(
            json!({ "tree": {} }),
            json!({
                "path": "src/core/mod.rs",
                "name": "mod.rs",
                "proposition": "The codebase has a search component.",
                "question": "Does `name` name something `proposition` is about?"
            }),
            json!({ "true": "The name denotes it.", "false": "The name does not." }),
        ));
        let answer = o.wait(ticket).unwrap();
        assert_eq!(answer.noul, Some(0.17));
        let posted = &o.transport.bodies[0]["questions"]["q0"];
        assert_eq!(posted["type"], "noul");
        assert_eq!(posted["instructions"]["path"], "src/core/mod.rs");
        assert!(posted["instructions"]["proposition"].is_string());
    }

    #[test]
    fn submit_does_not_post() {
        let mut o = oracle(vec![choice_resp(&["q0"], "yes")]);
        let _t = o.submit(classify(json!({"n": 1}), "a"));
        assert!(o.transport.bodies.is_empty());
        assert_eq!(o.usage(), Usage::default());
    }

    #[test]
    fn wait_one_flushes_same_state_siblings() {
        let mut o = oracle(vec![choice_resp(&["q0", "q1", "q2"], "yes")]);
        let state = json!({"path": "a.rs", "body": "fn main() {}"});
        let t0 = o.submit(classify(state.clone(), "p0"));
        let t1 = o.submit(classify(state.clone(), "p1"));
        let t2 = o.submit(classify(state, "p2"));
        let a0 = o.wait(t0).unwrap();
        assert_eq!(a0.choice, "yes");
        assert_eq!(o.transport.bodies.len(), 1);
        let qs = o.transport.bodies[0]["questions"].as_object().unwrap();
        assert_eq!(qs.len(), 3);
        assert!(qs.contains_key("q0") && qs.contains_key("q1") && qs.contains_key("q2"));
        assert_eq!(o.wait(t1).unwrap().choice, "yes");
        assert_eq!(o.wait(t2).unwrap().choice, "yes");
        assert_eq!(o.transport.bodies.len(), 1);
        assert_eq!(o.last_exchanges().len(), 1);
    }

    #[test]
    fn wait_flushes_pending_other_states() {
        let mut o = oracle(vec![
            choice_resp(&["q0"], "yes"),
            choice_resp(&["q1"], "no"),
        ]);
        let t0 = o.submit(classify(json!({"path": "a.rs"}), "p0"));
        let t1 = o.submit(classify(json!({"path": "b.rs"}), "p1"));
        let a0 = o.wait(t0).unwrap();
        assert_eq!(a0.choice, "yes");
        assert_eq!(o.transport.bodies.len(), 2);
        assert_eq!(o.wait(t1).unwrap().choice, "no");
        assert_eq!(o.transport.bodies.len(), 2);
    }

    #[test]
    fn chunks_same_state_at_question_budget() {
        let n = QUESTION_BUDGET + 1;
        let keys0: Vec<String> = (0..QUESTION_BUDGET).map(|i| format!("q{i}")).collect();
        let keys0_ref: Vec<&str> = keys0.iter().map(|s| s.as_str()).collect();
        let extra = format!("q{QUESTION_BUDGET}");
        let mut o = oracle(vec![
            choice_resp(&keys0_ref, "yes"),
            choice_resp(&[extra.as_str()], "no"),
        ]);
        let state = json!({"tree": "/"});
        let mut tickets = Vec::new();
        for i in 0..n {
            tickets.push(o.submit(classify(state.clone(), &format!("p{i}"))));
        }
        drop(keys0);
        let a = o.wait(tickets[0]).unwrap();
        assert_eq!(a.choice, "yes");
        assert_eq!(o.transport.bodies.len(), 2);
        assert_eq!(
            o.transport.bodies[0]["questions"]
                .as_object()
                .unwrap()
                .len(),
            QUESTION_BUDGET
        );
        assert_eq!(
            o.transport.bodies[1]["questions"]
                .as_object()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(o.wait(tickets[QUESTION_BUDGET]).unwrap().choice, "no");
    }

    #[test]
    fn submit_then_submit_then_wait_posts_once() {
        let mut o = oracle(vec![choice_resp(&["q0", "q1"], "yes")]);
        let state = json!({"n": 1});
        o.submit(classify(state.clone(), "a"));
        let t1 = o.submit(classify(state, "b"));
        assert!(o.transport.bodies.is_empty());
        o.wait(t1).unwrap();
        assert_eq!(o.transport.bodies.len(), 1);
        assert_eq!(
            o.transport.bodies[0]["questions"]
                .as_object()
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn cost_cap_keeps_crossing_request_then_refuses() {
        let mut o = oracle(vec![json!({
            "answers": { "q0": { "type": "choice", "choice": "yes", "confidence": 0.9 } },
            "usage": { "input_tokens": 10, "cost_usd": 1.25 }
        })]);
        let t0 = o.submit(classify(json!({"n": 1}), "a"));
        o.wait(t0).unwrap();
        assert!((o.usage().spend_usd - 1.25).abs() < 1e-12);
        let t1 = o.submit(classify(json!({"n": 2}), "b"));
        let err = o.wait(t1).unwrap_err();
        assert!(matches!(err, OracleError::Limit(_)), "{err}");
        assert!(err.to_string().contains("$1.00"));
        assert_eq!(o.transport.bodies.len(), 1);
    }

    #[test]
    fn already_at_limit_does_not_post_pending() {
        let mut o = oracle(vec![choice_resp(&["q0"], "yes")]);
        o.set_spent_usd(RUN_COST_LIMIT_USD);
        let t = o.submit(classify(json!({"n": 1}), "a"));
        assert!(matches!(o.wait(t), Err(OracleError::Limit(_))));
        assert!(o.transport.bodies.is_empty());
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
    fn records_usage_on_exchange() {
        let mut o = oracle(vec![json!({
            "answers": { "q0": { "type": "choice", "choice": "yes", "confidence": 0.9 } },
            "usage": { "input_tokens": 2000, "output_tokens": 8 }
        })]);
        let t = o.submit(classify(json!({"n": 1}), "a"));
        let a = o.wait(t).unwrap();
        let ex = a.exchange.unwrap();
        assert_eq!(ex.input_tokens, Some(2000));
        assert_eq!(ex.output_tokens, Some(8));
        assert!((ex.cost_usd.unwrap() - 2000.0 * 0.042 / 1_000_000.0).abs() < 1e-15);
        assert!(ex.started_at.is_some() && ex.ended_at.is_some());
    }

    #[test]
    fn usage_accumulates_across_flushes() {
        let mut o = oracle(vec![
            json!({
                "answers": { "q0": { "type": "choice", "choice": "yes", "confidence": 0.9 } },
                "usage": { "input_tokens": 1000, "cost_usd": 0.01 }
            }),
            json!({
                "answers": { "q1": { "type": "choice", "choice": "no", "confidence": 0.9 } },
                "usage": { "input_tokens": 3000, "cost_usd": 0.03 }
            }),
        ]);
        let t0 = o.submit(classify(json!({"n": 1}), "a"));
        o.wait(t0).unwrap();
        let t1 = o.submit(classify(json!({"n": 2}), "b"));
        o.wait(t1).unwrap();
        let u = o.usage();
        assert_eq!(u.requests, 2);
        assert_eq!(u.input_tokens, 4000);
        assert!((u.spend_usd - 0.04).abs() < 1e-12);
        assert!((u.batch_size.unwrap() - 1.0 / QUESTION_BUDGET as f64).abs() < 1e-12);
        assert!(
            (u.fill_factor.unwrap() - 4000.0 / (2.0 * CONTEXT_TOKEN_BUDGET as f64)).abs() < 1e-12
        );
        assert!(u.latency.is_some());
    }

    #[test]
    fn usage_batch_size_is_questions_over_budget() {
        let mut o = oracle(vec![choice_resp(&["q0", "q1", "q2"], "yes")]);
        let state = json!({"n": 1});
        let t0 = o.submit(classify(state.clone(), "a"));
        o.submit(classify(state.clone(), "b"));
        o.submit(classify(state, "c"));
        o.wait(t0).unwrap();
        let u = o.usage();
        assert_eq!(u.requests, 1);
        assert!((u.batch_size.unwrap() - 3.0 / QUESTION_BUDGET as f64).abs() < 1e-12);
        assert_eq!(u.fill_factor, None);
    }

    #[test]
    fn usage_fill_factor_is_tokens_over_context_budget() {
        let mut o = oracle(vec![json!({
            "answers": { "q0": { "type": "choice", "choice": "yes", "confidence": 0.9 } },
            "usage": { "input_tokens": 16_000 }
        })]);
        let t = o.submit(classify(json!({"n": 1}), "a"));
        o.wait(t).unwrap();
        let u = o.usage();
        assert_eq!(u.requests, 1);
        assert!((u.fill_factor.unwrap() - 16_000.0 / CONTEXT_TOKEN_BUDGET as f64).abs() < 1e-12);
        assert!((u.batch_size.unwrap() - 1.0 / QUESTION_BUDGET as f64).abs() < 1e-12);
    }

    #[test]
    fn usage_batch_size_averages_full_and_partial_chunks() {
        let n = QUESTION_BUDGET + 1;
        let keys0: Vec<String> = (0..QUESTION_BUDGET).map(|i| format!("q{i}")).collect();
        let keys0_ref: Vec<&str> = keys0.iter().map(|s| s.as_str()).collect();
        let extra = format!("q{QUESTION_BUDGET}");
        let mut o = oracle(vec![
            choice_resp(&keys0_ref, "yes"),
            choice_resp(&[extra.as_str()], "no"),
        ]);
        let state = json!({"tree": "/"});
        let mut tickets = Vec::new();
        for i in 0..n {
            tickets.push(o.submit(classify(state.clone(), &format!("p{i}"))));
        }
        drop(keys0);
        o.wait(tickets[0]).unwrap();
        let u = o.usage();
        assert_eq!(u.requests, 2);
        let expected = (QUESTION_BUDGET as f64 + 1.0) / (2.0 * QUESTION_BUDGET as f64);
        assert!((u.batch_size.unwrap() - expected).abs() < 1e-12);
        assert_eq!(u.fill_factor, None);
    }

    #[test]
    fn usage_latency_percentiles_from_round_trips() {
        let mut o = oracle_with_latencies(
            vec![
                choice_resp(&["q0"], "yes"),
                choice_resp(&["q1"], "yes"),
                choice_resp(&["q2"], "yes"),
                choice_resp(&["q3"], "yes"),
                choice_resp(&["q4"], "yes"),
            ],
            vec![
                Duration::from_millis(10),
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(40),
                Duration::from_millis(50),
            ],
        );
        for i in 0..5 {
            let t = o.submit(classify(json!({"n": i}), "a"));
            o.wait(t).unwrap();
        }
        let lat = o.usage().latency.unwrap();
        assert_eq!(lat.p50, Duration::from_millis(30));
        assert_eq!(lat.p95, Duration::from_millis(48));
        assert_eq!(lat.p99, Duration::from_nanos(49_600_000));
    }

    #[test]
    fn wait_unknown_ticket_does_not_flush() {
        let mut o = oracle(vec![choice_resp(&["q0"], "yes")]);
        let _pending = o.submit(classify(json!({"n": 1}), "a"));
        let err = o.wait(Ticket(99)).unwrap_err();
        assert!(matches!(err, OracleError::Parse(_)));
        assert!(o.transport.bodies.is_empty());
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
            "expected overlapping work, max inflight {}",
            max.load(Ordering::SeqCst)
        );
    }
}
