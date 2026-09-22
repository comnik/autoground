use std::collections::HashMap;
use std::path::Path;

use super::confidence::FLOOR;
use super::judge::{
    ChildChoice, DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError,
    ListingState, Transition,
};
use super::walk::parent_dir;
use crate::eval::{EdbAtom, Value, Witness};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FixtureFile {
    pub listings: Vec<ListingFixture>,
    pub documents: Vec<DocumentFixture>,
}

/// One `require` as shown in the run summary, with its printed formula.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RequireTrace {
    pub formula: String,
    pub value: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub witness: Option<Witness>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edb: Vec<EdbAtom>,
}

impl FixtureFile {
    pub fn load(path: impl AsRef<Path>) -> Result<Self, String> {
        let src = std::fs::read_to_string(path.as_ref()).map_err(|e| e.to_string())?;
        serde_json::from_str(&src).map_err(|e| e.to_string())
    }
}

/// One Jev HTTP round-trip recorded for timing and cost.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct JevRequestEvent {
    pub started_at_ms: u64,
    pub ended_at_ms: u64,
    pub duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
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
        let file = FixtureFile::load(path)?;
        Ok(Self {
            listings: file.listings,
            documents: file.documents,
        })
    }
}

fn lookup_listing_choice(
    related: &[&ListingFixture],
    child: &super::judge::Child,
) -> Option<Transition> {
    for f in related {
        if let Some(t) = f.choices.get(&child.rel_path) {
            return Some(*t);
        }
    }
    let parent = parent_dir(&child.rel_path).unwrap_or("");
    for f in related {
        if f.dir == parent {
            if let Some(t) = f.choices.get(&child.name) {
                return Some(*t);
            }
        }
    }
    None
}

impl Judge for FixtureJudge {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
        let related: Vec<&ListingFixture> = self
            .listings
            .iter()
            .filter(|f| f.pred == state.pred && f.args == state.args)
            .collect();
        let atom_known = !related.is_empty();
        Ok(state
            .children
            .iter()
            .map(|c| {
                let transition = lookup_listing_choice(&related, c).unwrap_or(Transition::Skip);
                ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition,
                    confidence: if atom_known { 0.9 } else { FLOOR - 0.1 },
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
                verdict: DocumentVerdict::Neutral,
                confidence: 0.0,
            }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{Child, Judge, ListingState, Transition};

    #[test]
    fn packed_listing_replays_per_dir_fixture_entries() {
        let mut j = FixtureJudge::load("tests/fixtures/toy-layered.json").unwrap();
        let state = ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![
                Child {
                    name: "frontend".into(),
                    rel_path: "frontend".into(),
                    is_dir: true,
                    sample: vec![],
                },
                Child {
                    name: "app.py".into(),
                    rel_path: "frontend/app.py".into(),
                    is_dir: false,
                    sample: vec![],
                },
                Child {
                    name: "backend".into(),
                    rel_path: "backend".into(),
                    is_dir: true,
                    sample: vec![],
                },
            ],
            verbalization: String::new(),
        };
        let choices = j.choose(&state).unwrap();
        let t = |p: &str| choices.iter().find(|c| c.rel_path == p).unwrap().transition;
        assert_eq!(t("frontend"), Transition::Investigate);
        assert_eq!(t("frontend/app.py"), Transition::Investigate);
        assert_eq!(t("backend"), Transition::Skip);
    }
}
