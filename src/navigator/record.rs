use super::fixture::{DocumentFixture, JevRequestEvent, ListingFixture};
use super::judge::{
    epoch_ms, ChildChoice, ConflictAnswer, ConflictState, DocumentAnswer, DocumentJudgement,
    DocumentState, Judge, JudgeError, JudgeExchange, ListingState, MultiListingState,
};
use crate::log::Log;

/// Wraps a judge and appends each listing and document answer to the session log.
pub struct RecordingJudge {
    inner: Box<dyn Judge>,
    log: Log,
}

impl RecordingJudge {
    pub fn new(inner: Box<dyn Judge>, log: Log) -> Self {
        Self { inner, log }
    }

    fn record_exchanges(&mut self) {
        for ex in self.inner.last_exchanges() {
            if let Some(event) = jev_event(&ex) {
                let _ = self.log.append_event("jev_request", &event);
            }
        }
    }
}

fn jev_event(ex: &JudgeExchange) -> Option<JevRequestEvent> {
    let started_at = ex.started_at?;
    let ended_at = ex.ended_at?;
    let started_at_ms = epoch_ms(started_at);
    let ended_at_ms = epoch_ms(ended_at);
    Some(JevRequestEvent {
        started_at_ms,
        ended_at_ms,
        duration_ms: ended_at_ms.saturating_sub(started_at_ms),
        input_tokens: ex.input_tokens,
        output_tokens: ex.output_tokens,
        cost_usd: ex.cost_usd,
    })
}

impl Judge for RecordingJudge {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        let choices = self.inner.choose(state)?;
        let mut map = std::collections::HashMap::new();
        for c in &choices {
            map.insert(c.rel_path.clone(), c.transition);
        }
        let _ = self.log.append_event(
            "listing",
            &ListingFixture {
                pred: state.pred.clone(),
                args: state.args.clone(),
                dir: state.dir.clone(),
                choices: map,
            },
        );
        self.record_exchanges();
        Ok(choices)
    }

    fn choose_many(
        &mut self,
        state: &MultiListingState,
    ) -> Result<Vec<Vec<ChildChoice>>, JudgeError> {
        let all = self.inner.choose_many(state)?;
        for (atom, choices) in state.atoms.iter().zip(&all) {
            let mut map = std::collections::HashMap::new();
            for c in choices {
                map.insert(c.rel_path.clone(), c.transition);
            }
            let _ = self.log.append_event(
                "listing",
                &ListingFixture {
                    pred: atom.pred.clone(),
                    args: atom.args.clone(),
                    dir: state.dir.clone(),
                    choices: map,
                },
            );
        }
        self.record_exchanges();
        Ok(all)
    }

    fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
        self.documents(std::slice::from_ref(state))
            .pop()
            .unwrap_or_else(|| Err(JudgeError::Parse("missing document answer".into())))
            .map(|a| a.judgement)
    }

    fn documents(&mut self, states: &[DocumentState]) -> Vec<Result<DocumentAnswer, JudgeError>> {
        let out = self.inner.documents(states);
        for (state, ans) in states.iter().zip(&out) {
            if let Ok(a) = ans {
                let _ = self.log.append_event(
                    "document",
                    &DocumentFixture {
                        pred: state.pred.clone(),
                        args: state.args.clone(),
                        path: state.path.clone(),
                        verdict: a.judgement.verdict,
                        confidence: a.judgement.confidence,
                    },
                );
            }
        }
        self.record_exchanges();
        out
    }

    fn resolve_conflict(&mut self, state: &ConflictState) -> Result<ConflictAnswer, JudgeError> {
        let answer = self.inner.resolve_conflict(state)?;
        self.record_exchanges();
        Ok(answer)
    }

    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        self.inner.last_exchanges()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::heuristic::HeuristicJudge;
    use crate::navigator::judge::DocumentState;

    fn events(path: &std::path::Path) -> Vec<serde_json::Value> {
        let found = crate::log::read_events(path).unwrap();
        let _ = std::fs::remove_file(path);
        found
    }

    #[test]
    fn records_a_document_batch() {
        let path = std::env::temp_dir().join(format!(
            "autoground-record-batch-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let states = vec![
            DocumentState {
                pred: "component".into(),
                args: vec!["A".into()],
                path: "a.py".into(),
                body: "a".into(),
                bindings: Vec::new(),
                verbalization: String::new(),
            },
            DocumentState {
                pred: "component".into(),
                args: vec!["A".into()],
                path: "b.py".into(),
                body: "b".into(),
                bindings: Vec::new(),
                verbalization: String::new(),
            },
        ];
        {
            let log = Log::open(&path).unwrap();
            let mut rec = RecordingJudge::new(Box::new(HeuristicJudge), log);
            let out = rec.documents(&states);
            assert_eq!(out.len(), 2);
            assert!(out.iter().all(|r| r.is_ok()));
        }
        let events = events(&path);
        let docs: Vec<_> = events
            .iter()
            .filter(|event| event["kind"] == "document")
            .collect();
        assert_eq!(docs.len(), 2);
        assert_eq!(docs[0]["path"], "a.py");
        assert_eq!(docs[1]["path"], "b.py");
        assert!(events.iter().all(|event| event["kind"] != "jev_request"));
    }

    struct TimedStub {
        exchanges: Vec<JudgeExchange>,
    }

    impl Judge for TimedStub {
        fn choose(&mut self, _state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            Ok(Vec::new())
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            unimplemented!("documents")
        }

        fn documents(
            &mut self,
            states: &[DocumentState],
        ) -> Vec<Result<DocumentAnswer, JudgeError>> {
            states
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    Ok(DocumentAnswer {
                        judgement: DocumentJudgement {
                            verdict: crate::navigator::judge::DocumentVerdict::Entailed,
                            confidence: 0.9,
                        },
                        exchanges: vec![self.exchanges[i].clone()],
                    })
                })
                .collect()
        }

        fn last_exchanges(&self) -> Vec<JudgeExchange> {
            self.exchanges.clone()
        }
    }

    #[test]
    fn records_jev_request_timing_and_cost() {
        use std::time::{Duration, UNIX_EPOCH};
        let path = std::env::temp_dir().join(format!(
            "autoground-record-timing-{}-{}.jsonl",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let t0 = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let t1 = t0 + Duration::from_millis(340);
        let states = vec![DocumentState {
            pred: "component".into(),
            args: vec!["A".into()],
            path: "a.py".into(),
            body: "a".into(),
            bindings: Vec::new(),
            verbalization: String::new(),
        }];
        {
            let stub = TimedStub {
                exchanges: vec![JudgeExchange {
                    request: serde_json::json!({"model": "jev-latest"}),
                    response: serde_json::json!({
                        "usage": { "input_tokens": 1000, "output_tokens": 4 }
                    }),
                    started_at: Some(t0),
                    ended_at: Some(t1),
                    input_tokens: Some(1000),
                    output_tokens: Some(4),
                    cost_usd: Some(0.000042),
                }],
            };
            let log = Log::open(&path).unwrap();
            let mut rec = RecordingJudge::new(Box::new(stub), log);
            let _ = rec.documents(&states);
        }
        let events = events(&path);
        let ev = events
            .iter()
            .find(|event| event["kind"] == "jev_request")
            .expect("jev request");
        assert_eq!(ev["started_at_ms"], 1_700_000_000_000u64);
        assert_eq!(ev["ended_at_ms"], 1_700_000_000_340u64);
        assert_eq!(ev["duration_ms"], 340);
        assert_eq!(ev["input_tokens"], 1000);
        assert_eq!(ev["output_tokens"], 4);
        assert!((ev["cost_usd"].as_f64().unwrap() - 0.000042).abs() < 1e-12);
    }
}
