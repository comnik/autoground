use super::fixture::{DocumentFixture, FixtureFile, JevRequestEvent, ListingFixture};
use super::judge::{
    epoch_ms, ChildChoice, ConflictAnswer, ConflictState, DocumentAnswer, DocumentJudgement,
    DocumentState, Judge, JudgeError, JudgeExchange, ListingState, MultiListingState,
};
use std::path::PathBuf;
use std::time::SystemTime;

/// Wraps a judge and writes listing/document Choice answers as a fixture file on drop.
pub struct RecordingJudge {
    inner: Box<dyn Judge>,
    file: FixtureFile,
    path: PathBuf,
}

impl RecordingJudge {
    pub fn new(inner: Box<dyn Judge>, path: impl Into<PathBuf>) -> Self {
        Self {
            inner,
            file: FixtureFile {
                started_at_ms: Some(epoch_ms(SystemTime::now())),
                ..FixtureFile::default()
            },
            path: path.into(),
        }
    }

    fn record_exchanges(&mut self) {
        for ex in self.inner.last_exchanges() {
            if let Some(event) = jev_event(&ex) {
                self.file.jev_requests.push(event);
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

impl Drop for RecordingJudge {
    fn drop(&mut self) {
        self.file.ended_at_ms = Some(epoch_ms(SystemTime::now()));
        if let Ok(text) = serde_json::to_string_pretty(&self.file) {
            let _ = std::fs::write(&self.path, text);
        }
    }
}

impl Judge for RecordingJudge {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        let choices = self.inner.choose(state)?;
        let mut map = std::collections::HashMap::new();
        for c in &choices {
            map.insert(c.rel_path.clone(), c.transition);
        }
        self.file.listings.push(ListingFixture {
            pred: state.pred.clone(),
            args: state.args.clone(),
            dir: state.dir.clone(),
            choices: map,
        });
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
            self.file.listings.push(ListingFixture {
                pred: atom.pred.clone(),
                args: atom.args.clone(),
                dir: state.dir.clone(),
                choices: map,
            });
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
                self.file.documents.push(DocumentFixture {
                    pred: state.pred.clone(),
                    args: state.args.clone(),
                    path: state.path.clone(),
                    verdict: a.judgement.verdict,
                    confidence: a.judgement.confidence,
                });
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

    #[test]
    fn records_a_document_batch() {
        let path = std::env::temp_dir().join(format!(
            "autoground-record-batch-{}-{}.json",
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
            let mut rec = RecordingJudge::new(Box::new(HeuristicJudge), &path);
            let out = rec.documents(&states);
            assert_eq!(out.len(), 2);
            assert!(out.iter().all(|r| r.is_ok()));
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let file: FixtureFile = serde_json::from_str(&text).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(file.documents.len(), 2);
        assert_eq!(file.documents[0].path, "a.py");
        assert_eq!(file.documents[1].path, "b.py");
        assert!(file.started_at_ms.is_some());
        assert!(file.ended_at_ms.is_some());
        assert!(file.ended_at_ms.unwrap() >= file.started_at_ms.unwrap());
        assert!(file.jev_requests.is_empty());
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
            "autoground-record-timing-{}-{}.json",
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
            let mut rec = RecordingJudge::new(Box::new(stub), &path);
            let _ = rec.documents(&states);
        }
        let text = std::fs::read_to_string(&path).unwrap();
        let file: FixtureFile = serde_json::from_str(&text).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(file.jev_requests.len(), 1);
        let ev = &file.jev_requests[0];
        assert_eq!(ev.started_at_ms, 1_700_000_000_000);
        assert_eq!(ev.ended_at_ms, 1_700_000_000_340);
        assert_eq!(ev.duration_ms, 340);
        assert_eq!(ev.input_tokens, Some(1000));
        assert_eq!(ev.output_tokens, Some(4));
        assert!((ev.cost_usd.unwrap() - 0.000042).abs() < 1e-12);
        assert!(file.started_at_ms.is_some());
        assert!(file.ended_at_ms.is_some());
    }
}
