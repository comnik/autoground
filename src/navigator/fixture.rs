use std::collections::HashMap;
use std::path::Path;

use super::judge::{
    ChildChoice, DocumentJudgement, DocumentState, DocumentVerdict, Judge, JudgeError,
    ListingState, Transition, TAU_CONFIDENCE,
};
use super::trace::GroundStep;
use crate::eval::{EdbAtom, Outcome, Value, Witness};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct FixtureFile {
    pub listings: Vec<ListingFixture>,
    pub documents: Vec<DocumentFixture>,
    /// Wall-clock start of the run, milliseconds since Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<u64>,
    /// Wall-clock end of the run, milliseconds since Unix epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jev_requests: Vec<JevRequestEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub repo_name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<RequireTrace>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<GroundStep>,
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

    pub fn save(&self, path: impl AsRef<Path>) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path.as_ref(), text).map_err(|e| e.to_string())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::Outcome;
    use crate::navigator::judge::{Child, ChildChoice, JudgeExchange};
    use crate::navigator::trace::GroundStep;
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn loads_legacy_fixture_without_run_fields() {
        let file = FixtureFile::load("tests/fixtures/toy-layered.json").unwrap();
        assert!(!file.listings.is_empty());
        assert!(!file.documents.is_empty());
        assert!(file.outcome.is_none());
        assert!(file.steps.is_empty());
        assert!(file.requires.is_empty());
    }

    #[test]
    fn run_trace_round_trips_steps_and_outcome() {
        let t0 = UNIX_EPOCH + Duration::from_millis(1_700_000_000_000);
        let step = GroundStep::Listing {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: String::new(),
            children: vec![Child {
                name: "ui".into(),
                rel_path: "ui".into(),
                is_dir: true,
                sample: vec!["app.py".into()],
            }],
            choices: vec![ChildChoice {
                rel_path: "ui".into(),
                transition: Transition::Investigate,
                confidence: 0.9,
            }],
            effects: vec!["ui: investigate → list".into()],
            exchanges: vec![JudgeExchange {
                request: serde_json::json!({"model": "jev-latest"}),
                response: serde_json::json!({"usage": {"input_tokens": 10}}),
                started_at: Some(t0),
                ended_at: Some(t0 + Duration::from_millis(40)),
                input_tokens: Some(10),
                output_tokens: None,
                cost_usd: Some(0.00000042),
            }],
        };
        let file = FixtureFile {
            outcome: Some(Outcome::Grounded),
            repo_name: "toy-layered".into(),
            requires: vec![RequireTrace {
                formula: "(require (component \"Frontend\"))".into(),
                value: crate::eval::Value::True,
                witness: None,
                edb: vec![],
            }],
            steps: vec![step.clone()],
            started_at_ms: Some(1_700_000_000_000),
            ended_at_ms: Some(1_700_000_000_100),
            ..FixtureFile::default()
        };
        let text = serde_json::to_string_pretty(&file).unwrap();
        assert!(text.contains("\"type\": \"listing\""), "{text}");
        assert!(text.contains("\"started_at_ms\": 1700000000000"), "{text}");
        let back: FixtureFile = serde_json::from_str(&text).unwrap();
        assert_eq!(back.outcome, Some(Outcome::Grounded));
        assert_eq!(back.repo_name, "toy-layered");
        assert_eq!(back.steps, vec![step]);
        assert_eq!(back.requires.len(), 1);
    }
}
