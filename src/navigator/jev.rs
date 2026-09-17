use serde_json::{json, Value};

use super::judge::{
    Child, ChildChoice, DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError,
    JudgeExchange, ListingState, Transition,
};

const DEFAULT_BASE: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
const CHOICE_CHUNK: usize = 100;
const MAX_RETRIES: u32 = 4;

/// POST body / response transport. Live HTTP in production; a mock in tests.
pub trait Transport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value, JudgeError>;
}

pub struct UreqTransport;

impl Transport for UreqTransport {
    fn post_json(&mut self, url: &str, body: &Value) -> Result<Value, JudgeError> {
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
        self.exchanges.clear();
        let body = document_request(&self.model, state);
        let resp = self.evaluate(&body)?;
        parse_document_answer(&resp)
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
    let atom = atom_text(&state.pred, &state.args, &state.verbalization);
    let mut questions = serde_json::Map::new();
    for (i, child) in chunk.iter().enumerate() {
        questions.insert(
            format!("c{}", id_base + i),
            json!({
                "type": "choice",
                "instructions": format!(
                    "Locate proof or disproof for: {atom} in path {}. Choose one navigation transition. Stop at whole directories where appropriate.",
                    child.rel_path
                ),
                "criteria": {
                    "skip": "Not a witness of the atom and does not contain one.",
                    "descend": "This location is not itself evidence, but is likely to contain one nested inside.",
                    "investigate": "This location contains evidence and should be examined closely.",
                }
            }),
        );
    }
    json!({
        "model": model,
        "state": {
            "atom": { "pred": state.pred, "args": state.args },
            "directory": state.dir,
            "children": state.children.iter().map(|c| json!({
                "name": c.name,
                "path": c.rel_path,
                "is_dir": c.is_dir,
                "sample": c.sample,
            })).collect::<Vec<_>>(),
        },
        "questions": questions,
    })
}

pub fn document_request(model: &str, state: &DocumentState) -> Value {
    let atom = atom_text(&state.pred, &state.args, &state.verbalization);
    let (instructions, criteria) = if state.args.len() >= 2 {
        (
            format!(
                "Does this file make the following true? {atom}. A file that implements or belongs to a mentioned entity does not count as an external relation. Choose true, false, or unknown."
            ),
            json!({
                "true": "The atom holds of this file.",
                "false": "The atom does not hold. Files that belong to or implement a mentioned entity are false for an external relation.",
                "unknown": "Cannot tell from this file."
            }),
        )
    } else {
        (
            format!(
                "Is this single file the entire denotation of: {atom}? Not merely a file that belongs to it. Choose true, false, or unknown."
            ),
            json!({
                "true": "The whole denotation is this one file (e.g. a single-file app.js).",
                "false": "This file is not the whole denotation.",
                "unknown": "Cannot tell."
            }),
        )
    };
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
                "instructions": instructions,
                "criteria": criteria
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
            "descend" => Transition::Descend,
            "part" => Transition::Part,
            "whole" => Transition::Whole,
            other => {
                return Err(JudgeError::Parse(format!(
                    "unknown transition `{other}` for {}",
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
            children: vec![child("frontend", true), child("decoy", true)],
            verbalization: "\"Frontend\" is a component in the codebase".into(),
        };
        let body = listing_request("jev-latest", &state, &state.children, 0);
        assert_eq!(body["model"], "jev-latest");
        assert_eq!(body["questions"]["c0"]["type"], "choice");
        assert_eq!(body["questions"]["c1"]["type"], "choice");
        assert!(body["state"]["children"].as_array().unwrap().len() == 2);
        let instr = body["questions"]["c0"]["instructions"].as_str().unwrap();
        assert_eq!(
            instr,
            "Locate proof or disproof for: \"Frontend\" is a component in the codebase in path frontend. Choose one navigation transition. Stop at whole directories where appropriate."
        );
        assert!(!instr.contains("Listing sample"));
        assert_eq!(body["state"]["children"][0]["sample"], json!(["app.py"]));
        assert!(!body["state"].get("body").is_some());
    }

    #[test]
    fn document_request_is_three_way_choice() {
        let state = DocumentState {
            pred: "defines".into(),
            args: vec!["ui/app.py".into(), "Backend".into()],
            path: "ui/app.py".into(),
            body: "from api import client".into(),
            bindings: vec![("Backend".into(), "api".into())],
            verbalization: "file \"ui/app.py\" (partially) defines component \"Backend\"".into(),
        };
        let body = document_request("jev-latest", &state);
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
        assert!(instr.contains("file \"ui/app.py\" (partially) defines component \"Backend\""));
    }

    #[test]
    fn parse_listing_and_document_choices() {
        let children = vec![child("frontend", true), child("decoy", true)];
        let resp = json!({
            "answers": {
                "c0": { "type": "choice", "choice": "whole", "confidence": 0.91 },
                "c1": { "type": "choice", "choice": "skip", "confidence": 0.88 }
            }
        });
        let choices = parse_listing_answers(&resp, &children, 0).unwrap();
        assert_eq!(choices[0].transition, Transition::Whole);
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
                    "c0": { "type": "choice", "choice": "whole", "confidence": 0.9 }
                }
            })],
            bodies: Vec::new(),
        };
        let mut judge = JevJudge::new(transport, "http://example/v1/systemone", "jev-latest");
        let choices = judge.choose(&state).unwrap();
        assert_eq!(choices[0].transition, Transition::Whole);
        let ex = judge.last_exchanges();
        assert_eq!(ex.len(), 1);
        assert_eq!(ex[0].request["model"], "jev-latest");
        assert_eq!(ex[0].response["answers"]["c0"]["choice"], "whole");
    }
}
