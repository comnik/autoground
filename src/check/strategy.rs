use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value as Json};

use crate::confidence::SUPPORT;
use crate::oracle::image::Asset;
use crate::oracle::{Answer, Classification, QuestionKind, State};
use crate::syntax::template::apply;
use crate::syntax::Sentence;
use crate::value::Value;

pub const DOCUMENT_QUESTION: &str = "How does `body` at `path` ground the proposition?";
pub const PART_QUESTION: &str =
    "How does this part of `body` at `path` ground the proposition? Text outside this part is not visible.";
pub const JOINT_QUESTION: &str = "How do the files in `files` together ground the proposition?";
pub const IMPLICATION_QUESTION: &str = "Do the sentences establish the proposition?";

/// A proposition submitted to a [`Strategy`].
///
/// `grounded` is the filled reading. When the proposition came from a sense
/// template, `template` and `roles` carry the unfilled reading and its
/// positional `(hole, value)` assignments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statement {
    pub grounded: String,
    pub template: Option<String>,
    pub roles: Vec<(String, String)>,
}

impl Statement {
    pub fn plain(grounded: impl Into<String>) -> Self {
        Self {
            grounded: grounded.into(),
            template: None,
            roles: Vec::new(),
        }
    }

    pub fn from_template(
        template: impl Into<String>,
        roles: Vec<(String, String)>,
    ) -> Result<Self, String> {
        let template = template.into();
        let values: Vec<String> = roles.iter().map(|(_, v)| v.clone()).collect();
        let grounded = apply(&template, &values)?;
        Ok(Self {
            grounded,
            template: Some(template),
            roles,
        })
    }
}

impl From<&Sentence> for Statement {
    fn from(sentence: &Sentence) -> Self {
        Self::plain(sentence.grounded.clone())
    }
}

impl From<&str> for Statement {
    fn from(text: &str) -> Self {
        Self::plain(text)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VerdictKind {
    Entailed,
    Contradicted,
    Neutral,
}

impl VerdictKind {
    pub fn as_str(self) -> &'static str {
        match self {
            VerdictKind::Entailed => "entailed",
            VerdictKind::Contradicted => "contradicted",
            VerdictKind::Neutral => "neutral",
        }
    }

    pub fn from_choice(choice: &str) -> Self {
        match choice {
            "entailed" => VerdictKind::Entailed,
            "contradicted" => VerdictKind::Contradicted,
            _ => VerdictKind::Neutral,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// Raw winner reported by the strategy, before applying any trust bar.
    pub kind: VerdictKind,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probabilities: Option<BTreeMap<String, f64>>,
    /// Settled truth value after applying the strategy's trust bar (`None` = unsettled/neutral).
    pub settled: Option<Value>,
}

impl Verdict {
    pub fn effective_kind(&self) -> VerdictKind {
        match self.settled {
            Some(Value::True) => VerdictKind::Entailed,
            Some(Value::False) => VerdictKind::Contradicted,
            _ => VerdictKind::Neutral,
        }
    }
}

/// Pluggable oracle prompting and settlement strategy.
///
/// Returning a `Vec<Classification>` lets a strategy ask one `Choice` or
/// multiple questions (e.g. separate `Noul` probes) and combine their answers
/// in [`Strategy::settle`].
pub trait Strategy {
    fn name(&self) -> &str;

    fn document(
        &self,
        statement: &Statement,
        path: &str,
        body: &str,
        part: Option<(u32, u32)>,
    ) -> Vec<Classification>;

    fn joint(&self, statement: &Statement, files: &[(String, String)]) -> Vec<Classification>;

    fn implication(&self, head: &Statement, premises: &[Statement]) -> Vec<Classification>;

    fn settle(&self, answers: &[Answer]) -> Verdict;
}

/// Production prompting strategy used by `autoground check`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultStrategy;

impl DefaultStrategy {
    pub fn document_criteria() -> Json {
        json!({
            "entailed": "`body` supplies information that makes the proposition true",
            "contradicted": "`body` supplies information that makes the proposition false",
            "neutral": "`body` neither supports nor refutes the proposition",
        })
    }

    pub fn implication_criteria() -> Json {
        json!({
            "entailed": "the sentences together establish the proposition",
            "contradicted": "the sentences rule the proposition out",
            "neutral": "the sentences do not settle the proposition",
        })
    }

    pub fn document_classification(
        sentence: &str,
        path: &str,
        body: &str,
        part: Option<(u32, u32)>,
    ) -> Classification {
        let state = match part {
            Some((index, total)) => State::new(json!({
                "path": path,
                "body": body,
                "part": index,
                "parts": total,
            })),
            None => State::new(json!({
                "path": path,
                "body": body,
            })),
        };
        Classification {
            state,
            kind: QuestionKind::Choice,
            instructions: json!({
                "proposition": sentence,
                "question": if part.is_some() { PART_QUESTION } else { DOCUMENT_QUESTION },
            }),
            criteria: Self::document_criteria(),
        }
    }

    pub(crate) fn document_image(sentence: &str, path: &str, asset: Asset) -> Classification {
        let reference = asset.reference().to_string();
        Classification {
            state: State::with_assets(
                json!({
                    "path": path,
                    "body": reference,
                }),
                vec![asset],
            ),
            kind: QuestionKind::Choice,
            instructions: json!({
                "proposition": sentence,
                "question": DOCUMENT_QUESTION,
            }),
            criteria: Self::document_criteria(),
        }
    }

    pub(crate) fn joint_with_assets(
        sentence: &str,
        files: Vec<Json>,
        assets: Vec<Asset>,
    ) -> Classification {
        Classification {
            state: State::with_assets(json!({ "files": files }), assets),
            kind: QuestionKind::Choice,
            instructions: json!({
                "proposition": sentence,
                "question": JOINT_QUESTION,
            }),
            criteria: Self::document_criteria(),
        }
    }

    pub fn implication_classification(head: &str, premises: &[String]) -> Classification {
        Classification {
            state: State::new(json!({ "sentences": premises })),
            kind: QuestionKind::Choice,
            instructions: json!({
                "proposition": head,
                "question": IMPLICATION_QUESTION,
            }),
            criteria: Self::implication_criteria(),
        }
    }

    pub fn settle_choice(answer: &Answer, threshold: f64) -> Verdict {
        let kind = VerdictKind::from_choice(&answer.choice);
        let settled = if answer.confidence < threshold {
            None
        } else {
            match kind {
                VerdictKind::Entailed => Some(Value::True),
                VerdictKind::Contradicted => Some(Value::False),
                VerdictKind::Neutral => None,
            }
        };
        Verdict {
            kind,
            confidence: answer.confidence,
            probabilities: answer.probabilities.clone(),
            settled,
        }
    }
}

impl Strategy for DefaultStrategy {
    fn name(&self) -> &str {
        "default"
    }

    fn document(
        &self,
        statement: &Statement,
        path: &str,
        body: &str,
        part: Option<(u32, u32)>,
    ) -> Vec<Classification> {
        vec![Self::document_classification(
            &statement.grounded,
            path,
            body,
            part,
        )]
    }

    fn joint(&self, statement: &Statement, files: &[(String, String)]) -> Vec<Classification> {
        let bodies = files
            .iter()
            .map(|(path, body)| json!({ "path": path, "body": body }))
            .collect();
        vec![Self::joint_with_assets(
            &statement.grounded,
            bodies,
            Vec::new(),
        )]
    }

    fn implication(&self, head: &Statement, premises: &[Statement]) -> Vec<Classification> {
        let sentences: Vec<String> = premises.iter().map(|s| s.grounded.clone()).collect();
        vec![Self::implication_classification(&head.grounded, &sentences)]
    }

    fn settle(&self, answers: &[Answer]) -> Verdict {
        let Some(answer) = answers.first() else {
            return Verdict {
                kind: VerdictKind::Neutral,
                confidence: 0.0,
                probabilities: None,
                settled: None,
            };
        };
        Self::settle_choice(answer, SUPPORT)
    }
}
