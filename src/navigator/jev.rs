use serde_json::{json, Value};

use super::judge::{
    Child, ChildChoice, DocumentAnswer, DocumentJudgement, DocumentState, DocumentVerdict, Judge,
    JudgeError, JudgeExchange, ListingState, Transition,
};

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
const CHOICE_CHUNK: usize = 100;
const MAX_RETRIES: u32 = 4;
const DOCUMENT_PARALLELISM: usize = 8;

/// POST body / response transport. Live HTTP in production; a mock in tests.
pub trait Transport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value, JudgeError>;

    /// Default posts one body at a time. Live HTTP overrides this to overlap
    /// independent document requests.
    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Value, JudgeError>> {
        bodies.iter().map(|b| self.post_json(url, b)).collect()
    }
}

pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value, JudgeError> {
        post_json_with_retries(url, body)
    }

    fn post_json_all(&mut self, url: &str, bodies: &[Value]) -> Vec<Result<Value, JudgeError>> {
        parallel_map(bodies, DOCUMENT_PARALLELISM, |body| {
            post_json_with_retries(url, body)
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
        }
    }

    fn evaluate(&mut self, body: &Value) -> Result<Value, JudgeError> {
        let resp = self.transport.post_json(&self.endpoint, body)?;
        self.exchanges.push(JudgeExchange {
            request: body.clone(),
            response: resp.clone(),
        });
        Ok(resp)
    }
}

impl<T: Transport> Judge for JevJudge<T> {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        self.exchanges.clear();
        let mut out = Vec::with_capacity(state.children.len());
        for chunk in state.children.chunks(CHOICE_CHUNK) {
            let body = listing_request(&self.model, state, chunk, out.len());
            let resp = self.evaluate(&body)?;
            out.extend(parse_listing_answers(&resp, chunk, out.len())?);
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
        let bodies: Vec<Value> = states
            .iter()
            .map(|s| investigation_request(&self.model, s))
            .collect();
        let responses = self.transport.post_json_all(&self.endpoint, &bodies);
        let mut out = Vec::with_capacity(states.len());
        for (request, resp) in bodies.into_iter().zip(responses) {
            match resp {
                Ok(response) => {
                    let ex = JudgeExchange {
                        request,
                        response: response.clone(),
                    };
                    self.exchanges.push(ex.clone());
                    out.push(
                        parse_document_answer(&response).map(|judgement| DocumentAnswer {
                            judgement,
                            exchanges: vec![ex],
                        }),
                    );
                }
                Err(e) => out.push(Err(e)),
            }
        }
        out
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
    let mut children = serde_json::Map::new();
    for (i, child) in chunk.iter().enumerate() {
        let id = format!("c{}", id_base + i);
        let loc = format!("children.{id}");
        children.insert(
            id.clone(),
            json!({
                "name": child.name,
                "path": child.rel_path,
                "is_dir": child.is_dir,
                "sample": child.sample,
            }),
        );
        questions.insert(
            id,
            json!({
                "type": "choice",
                "instructions": {
                    "question": format!(
                        "Does `{loc}` contain proof or disproof for `atom.statement`?"
                    ),
                    "inspect": loc,
                },
                "criteria": {
                    "skip": "This location does not contain proof or disproof.",
                    "investigate": "This location contains proof or disproof. Directories are listed; files are opened.",
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
            "children": children,
        },
        "questions": questions,
    })
}

pub fn investigation_request(model: &str, state: &DocumentState) -> Value {
    let atom = atom_text(&state.pred, &state.args, &state.verbalization);
    json!({
        "model": model,
        "state": {
            "atom": { "pred": state.pred, "args": state.args },
            "path": state.path,
            "body": state.body,
            "bindings": state.bindings.iter().map(|(c, e)| json!({
                "constant": c,
                "entity": e,
            })).collect::<Vec<_>>(),
        },
        "questions": {
            "ground": {
                "type": "choice",
                "instructions": atom,
                "criteria": {
                    "true": "The atom holds.",
                    "false": "The atom does not hold.",
                    "unknown": "Cannot tell.",
                }
            }
        }
    })
}

pub fn parse_listing_answers(
    resp: &Value,
    chunk: &[Child],
    id_base: usize,
) -> Result<Vec<ChildChoice>, JudgeError> {
    let answers = resp
        .get("answers")
        .and_then(|a| a.as_object())
        .ok_or_else(|| JudgeError::Parse("response missing answers".into()))?;
    let mut out = Vec::new();
    for (i, child) in chunk.iter().enumerate() {
        let key = format!("c{}", id_base + i);
        let ans = answers
            .get(&key)
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
        out.push(ChildChoice {
            rel_path: child.rel_path.clone(),
            transition,
            confidence,
        });
    }
    Ok(out)
}

pub fn parse_document_answer(resp: &Value) -> Result<DocumentJudgement, JudgeError> {
    let ans = resp
        .pointer("/answers/ground")
        .ok_or_else(|| JudgeError::Parse("response missing answers.ground".into()))?;
    let choice = ans
        .get("choice")
        .and_then(|c| c.as_str())
        .ok_or_else(|| JudgeError::Parse("document answer missing choice".into()))?;
    let verdict = match choice {
        "true" => DocumentVerdict::True,
        "false" => DocumentVerdict::False,
        "unknown" => DocumentVerdict::Unknown,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{Child, DocumentVerdict};

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
        assert_eq!(body["state"]["children"]["c0"]["path"], "frontend");
        assert_eq!(body["state"]["children"]["c1"]["path"], "decoy");
        assert_eq!(body["state"]["children"]["c2"]["path"], "app.py");
        assert_eq!(
            body["state"]["atom"]["statement"],
            "\"Frontend\" is a component in the codebase"
        );
        let instr = &body["questions"]["c0"]["instructions"];
        assert_eq!(
            instr["question"],
            "Does `children.c0` contain proof or disproof for `atom.statement`?"
        );
        assert_eq!(instr["inspect"], "children.c0");
        assert_eq!(
            body["questions"]["c1"]["instructions"]["inspect"],
            "children.c1"
        );
        assert_eq!(
            body["questions"]["c2"]["instructions"]["inspect"],
            "children.c2"
        );
        let criteria = body["questions"]["c0"]["criteria"].as_object().unwrap();
        assert!(criteria.contains_key("skip"));
        assert!(criteria.contains_key("investigate"));
        assert!(!criteria.contains_key("unknown"));
        assert_eq!(body["state"]["children"]["c0"]["sample"], json!(["app.py"]));
        assert!(!body["state"].get("body").is_some());
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
        assert!(criteria.contains_key("true"));
        assert!(criteria.contains_key("false"));
        assert!(criteria.contains_key("unknown"));
        assert!(body["state"].get("children").is_none());
        assert_eq!(body["state"]["path"], "ui/app.py");
        let instr = body["questions"]["ground"]["instructions"]
            .as_str()
            .unwrap();
        assert_eq!(
            instr,
            "file \"ui/app.py\" (partially) defines component \"Backend\""
        );
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
                "ground": { "type": "choice", "choice": "true", "confidence": 0.9 }
            }
        });
        let n = parse_document_answer(&doc_resp).unwrap();
        assert_eq!(n.verdict, DocumentVerdict::True);
        assert!((n.confidence - 0.9).abs() < 1e-9);

        let unknown_resp = json!({
            "answers": {
                "ground": { "type": "choice", "choice": "unknown", "confidence": 0.7 }
            }
        });
        let u = parse_document_answer(&unknown_resp).unwrap();
        assert_eq!(u.verdict, DocumentVerdict::Unknown);
        assert!(!u.closed_true() && !u.closed_false());
    }

    struct SeqTransport {
        responses: Vec<Value>,
        pub bodies: Vec<Value>,
    }

    impl Transport for SeqTransport {
        fn post_json(&mut self, _url: &str, body: &Value) -> Result<Value, JudgeError> {
            self.bodies.push(body.clone());
            if self.responses.is_empty() {
                return Err(JudgeError::Http("no more responses".into()));
            }
            Ok(self.responses.remove(0))
        }
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
                "ground": { "type": "choice", "choice": "true", "confidence": 0.9 }
            }
        })
    }

    struct BatchSpy {
        batches: std::sync::Arc<std::sync::Mutex<Vec<usize>>>,
        responses: Vec<Value>,
    }

    impl Transport for BatchSpy {
        fn post_json(&mut self, _url: &str, _body: &Value) -> Result<Value, JudgeError> {
            panic!("documents should post via post_json_all");
        }

        fn post_json_all(
            &mut self,
            _url: &str,
            bodies: &[Value],
        ) -> Vec<Result<Value, JudgeError>> {
            self.batches.lock().unwrap().push(bodies.len());
            bodies
                .iter()
                .map(|_| {
                    if self.responses.is_empty() {
                        Err(JudgeError::Http("no more responses".into()))
                    } else {
                        Ok(self.responses.remove(0))
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
            .all(|a| a.as_ref().unwrap().judgement.closed_true()));
        assert_eq!(*batches.lock().unwrap(), vec![3]);
        let ex = judge.last_exchanges();
        assert_eq!(ex.len(), 3);
        assert_eq!(ex[0].request["state"]["path"], "a.py");
        assert_eq!(ex[1].request["state"]["path"], "b.py");
        assert_eq!(ex[2].request["state"]["path"], "c.py");
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
