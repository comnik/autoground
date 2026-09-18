use super::fixture::{DocumentFixture, FixtureFile, ListingFixture};
use super::judge::{
    ChildChoice, DocumentAnswer, DocumentJudgement, DocumentState, Judge, JudgeError,
    JudgeExchange, ListingState,
};
use std::path::PathBuf;

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
            file: FixtureFile::default(),
            path: path.into(),
        }
    }
}

impl Drop for RecordingJudge {
    fn drop(&mut self) {
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
            let name = c
                .rel_path
                .rsplit('/')
                .next()
                .unwrap_or(&c.rel_path)
                .to_string();
            map.insert(name, c.transition);
        }
        self.file.listings.push(ListingFixture {
            pred: state.pred.clone(),
            args: state.args.clone(),
            dir: state.dir.clone(),
            choices: map,
        });
        Ok(choices)
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
        out
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
    }
}
