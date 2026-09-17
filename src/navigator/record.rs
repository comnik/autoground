use super::fixture::{DocumentFixture, FixtureFile, ListingFixture};
use super::judge::{
    ChildChoice, DocumentJudgement, DocumentState, Judge, JudgeError, JudgeExchange, ListingState,
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
        let j = self.inner.document(state)?;
        self.file.documents.push(DocumentFixture {
            pred: state.pred.clone(),
            args: state.args.clone(),
            path: state.path.clone(),
            verdict: j.verdict,
            confidence: j.confidence,
        });
        Ok(j)
    }

    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        self.inner.last_exchanges()
    }
}
