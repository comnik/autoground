use std::collections::HashMap;
use std::path::Path;

use super::judge::{
    ChildChoice, DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError,
    ListingState, Transition, TAU_CONFIDENCE,
};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FixtureFile {
    pub listings: Vec<ListingFixture>,
    pub documents: Vec<DocumentFixture>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ListingFixture {
    pub pred: String,
    pub args: Vec<String>,
    pub dir: String,
    pub choices: HashMap<String, Transition>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DocumentFixture {
    pub pred: String,
    pub args: Vec<String>,
    pub path: String,
    pub verdict: DocumentVerdict,
    pub confidence: f64,
}

/// Replays recorded Choice/Noul answers. Missing entries skip with low
/// confidence so the atom stays open instead of closing by accident.
pub struct FixtureJudge {
    listings: Vec<ListingFixture>,
    documents: Vec<DocumentFixture>,
}

impl FixtureJudge {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let src = std::fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;
        let file: FixtureFile = serde_json::from_str(&src).map_err(|e| e.to_string())?;
        Ok(Self {
            listings: file.listings,
            documents: file.documents,
        })
    }
}

impl Judge for FixtureJudge {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        let hit = self
            .listings
            .iter()
            .find(|f| f.pred == state.pred && f.args == state.args && f.dir == state.dir);
        Ok(state
            .children
            .iter()
            .map(|c| {
                let transition = hit
                    .and_then(|f| {
                        f.choices
                            .get(&c.name)
                            .or_else(|| f.choices.get(&c.rel_path))
                    })
                    .copied()
                    .unwrap_or(Transition::Skip);
                ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition,
                    confidence: if hit.is_some() {
                        0.9
                    } else {
                        TAU_CONFIDENCE - 0.1
                    },
                }
            })
            .collect())
    }

    fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
        Ok(self
            .documents
            .iter()
            .find(|f| f.pred == state.pred && f.args == state.args && f.path == state.path)
            .map(|f| DocumentJudgement {
                verdict: f.verdict,
                confidence: f.confidence,
            })
            .unwrap_or(DocumentJudgement {
                verdict: DocumentVerdict::Unknown,
                confidence: 0.0,
            }))
    }
}
