/// A child of a location state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub sample: Vec<String>,
}

/// Directory listing presented to the judge. Names and samples, not file bodies.
#[derive(Debug, Clone)]
pub struct ListingState {
    pub pred: String,
    pub args: Vec<String>,
    pub dir: String,
    pub children: Vec<Child>,
}

/// One file presented to the judge. Separate request from listings.
#[derive(Debug, Clone)]
pub struct DocumentState {
    pub pred: String,
    pub args: Vec<String>,
    pub path: String,
    pub body: String,
    /// Current bindings for other constants (e.g. Backend roots when judging defines).
    pub bindings: Vec<(String, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    Skip,
    Descend,
    Part,
    Whole,
}

impl Transition {
    pub fn label(self) -> &'static str {
        match self {
            Transition::Skip => "skip",
            Transition::Descend => "descend",
            Transition::Part => "part",
            Transition::Whole => "whole",
        }
    }

    pub fn mark(self) -> &'static str {
        match self {
            Transition::Skip => ".",
            Transition::Descend => ">",
            Transition::Part => "+",
            Transition::Whole => "*",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChildChoice {
    pub rel_path: String,
    pub transition: Transition,
    pub confidence: f64,
}

/// Three-way document judgement. Unknown is a named option, not a
/// threshold on P(yes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentVerdict {
    True,
    False,
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DocumentJudgement {
    pub verdict: DocumentVerdict,
    pub confidence: f64,
}

/// One HTTP round-trip from a Jev `choose` or `document` call.
/// Local judges leave this empty.
#[derive(Debug, Clone)]
pub struct JudgeExchange {
    pub request: serde_json::Value,
    pub response: serde_json::Value,
}

pub const TAU_CONFIDENCE: f64 = 0.6;

impl DocumentJudgement {
    pub fn closed_true(&self) -> bool {
        self.verdict == DocumentVerdict::True && self.confidence >= TAU_CONFIDENCE
    }

    pub fn closed_false(&self) -> bool {
        self.verdict == DocumentVerdict::False && self.confidence >= TAU_CONFIDENCE
    }
}

#[derive(Debug)]
pub enum JudgeError {
    Config(String),
    Http(String),
    Parse(String),
}

impl std::fmt::Display for JudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JudgeError::Config(s) | JudgeError::Http(s) | JudgeError::Parse(s) => {
                write!(f, "{s}")
            }
        }
    }
}

impl std::error::Error for JudgeError {}

/// Answers location Choices and document Nouls. Jev implements this over HTTP;
/// tests use a heuristic stand-in or recorded fixtures.
pub trait Judge {
    fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError>;
    fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError>;

    /// Request/response pairs from the most recent `choose` or `document`.
    /// Empty for heuristic and fixture judges.
    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        Vec::new()
    }
}
