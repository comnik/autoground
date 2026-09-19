use std::time::{SystemTime, UNIX_EPOCH};

/// A child of a location state.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Child {
    pub name: String,
    pub rel_path: String,
    pub is_dir: bool,
    pub sample: Vec<String>,
}

/// Directory listing presented to the judge. Names and samples, not file bodies.
/// `children` may include nested paths from greedily packed subdirectories.
#[derive(Debug, Clone)]
pub struct ListingState {
    pub pred: String,
    pub args: Vec<String>,
    pub dir: String,
    pub children: Vec<Child>,
    /// Human sentence for the demanded atom, from `declare` templates.
    pub verbalization: String,
}

/// One atom sharing a listing tree with others.
#[derive(Debug, Clone)]
pub struct ListingAtom {
    pub pred: String,
    pub args: Vec<String>,
    pub verbalization: String,
}

/// Several atoms, one directory tree. Each child is a Choice per atom.
#[derive(Debug, Clone)]
pub struct MultiListingState {
    pub atoms: Vec<ListingAtom>,
    pub dir: String,
    pub children: Vec<Child>,
}

impl MultiListingState {
    pub fn for_atom(atom: ListingAtom, dir: String, children: Vec<Child>) -> Self {
        Self {
            atoms: vec![atom],
            dir,
            children,
        }
    }

    pub fn listing_state(&self, atom: &ListingAtom) -> ListingState {
        ListingState {
            pred: atom.pred.clone(),
            args: atom.args.clone(),
            dir: self.dir.clone(),
            children: self.children.clone(),
            verbalization: atom.verbalization.clone(),
        }
    }
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
    /// Human sentence for the demanded atom, from `declare` templates.
    pub verbalization: String,
}

/// Listing choice: skip this location, or look here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transition {
    Skip,
    /// List a directory or open a file.
    Investigate,
}

impl Transition {
    pub fn label(self) -> &'static str {
        match self {
            Transition::Skip => "skip",
            Transition::Investigate => "investigate",
        }
    }

    pub fn mark(self) -> &'static str {
        match self {
            Transition::Skip => ".",
            Transition::Investigate => "*",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ChildChoice {
    pub rel_path: String,
    pub transition: Transition,
    pub confidence: f64,
}

/// How a file body relates to the demanded statement.
/// `true`/`false`/`unknown` aliases load older traces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocumentVerdict {
    #[serde(alias = "true")]
    Entailed,
    #[serde(alias = "false")]
    Contradicted,
    #[serde(alias = "unknown")]
    Neutral,
}

impl DocumentVerdict {
    pub fn label(self) -> &'static str {
        match self {
            Self::Entailed => "entailed",
            Self::Contradicted => "contradicted",
            Self::Neutral => "neutral",
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DocumentJudgement {
    pub verdict: DocumentVerdict,
    pub confidence: f64,
}

/// One HTTP round-trip from a Jev `choose` or `document` call.
/// Local judges leave this empty.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct JudgeExchange {
    pub request: serde_json::Value,
    pub response: serde_json::Value,
    #[serde(
        default,
        rename = "started_at_ms",
        skip_serializing_if = "Option::is_none",
        with = "opt_epoch_ms"
    )]
    pub started_at: Option<SystemTime>,
    #[serde(
        default,
        rename = "ended_at_ms",
        skip_serializing_if = "Option::is_none",
        with = "opt_epoch_ms"
    )]
    pub ended_at: Option<SystemTime>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// USD, when the response reports usage (or an explicit cost).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

pub fn epoch_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

pub fn from_epoch_ms(ms: u64) -> SystemTime {
    UNIX_EPOCH + std::time::Duration::from_millis(ms)
}

mod opt_epoch_ms {
    use super::{epoch_ms, from_epoch_ms, SystemTime};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(v: &Option<SystemTime>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        v.map(epoch_ms).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<SystemTime>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Option::<u64>::deserialize(deserializer)?.map(from_epoch_ms))
    }
}

impl JudgeExchange {
    pub fn new(request: serde_json::Value, response: serde_json::Value) -> Self {
        Self {
            request,
            response,
            ..Self::default()
        }
    }
}

/// One document judgement plus the exchanges that produced it.
#[derive(Debug, Clone)]
pub struct DocumentAnswer {
    pub judgement: DocumentJudgement,
    pub exchanges: Vec<JudgeExchange>,
}

/// Max children in one listing Choice. Packing and Jev chunking share this so
/// a filled listing is one HTTP request.
pub const LISTING_CHOICE_BUDGET: usize = 100;

impl DocumentJudgement {
    /// Entailed at [`super::confidence::SUPPORT`]: this file may be a witness.
    pub fn is_entailed(&self) -> bool {
        super::confidence::is_witness(self.verdict, self.confidence)
    }

    /// Contradicted at [`super::confidence::FLOOR`]: blocks support.
    pub fn is_contradicted(&self) -> bool {
        super::confidence::vetoes(self.verdict, self.confidence)
    }

    /// Contradicted at [`super::confidence::SUPPORT`]: may close the atom false.
    pub fn is_refutation(&self) -> bool {
        super::confidence::is_refutation(self.verdict, self.confidence)
    }

    pub fn note(&self) -> &'static str {
        if self.is_entailed() {
            "entailed"
        } else if self.is_contradicted() {
            "contradicted"
        } else if super::confidence::confident_look(self.confidence) {
            "neutral"
        } else {
            "undecided"
        }
    }
}

/// Polar document evidence for a ground-atom conflict resolve.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConflictEvidence {
    pub path: String,
    pub verdict: DocumentVerdict,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub excerpt: String,
}

#[derive(Debug, Clone)]
pub struct ConflictState {
    pub pred: String,
    pub args: Vec<String>,
    pub verbalization: String,
    pub evidence: Vec<ConflictEvidence>,
}

/// Eval-facing 3VL from a conflict resolve.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResolveValue {
    True,
    False,
    Unknown,
}

impl ResolveValue {
    pub fn label(self) -> &'static str {
        match self {
            Self::True => "true",
            Self::False => "false",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Consistency {
    Consistent,
    Conflicting,
}

impl Consistency {
    pub fn label(self) -> &'static str {
        match self {
            Self::Consistent => "consistent",
            Self::Conflicting => "conflicting",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ConflictAnswer {
    pub value: ResolveValue,
    pub consistency: Consistency,
    pub value_confidence: f64,
    pub consistency_confidence: f64,
    pub exchanges: Vec<JudgeExchange>,
}

impl ConflictAnswer {
    /// Local judges that already picked a side.
    pub fn sure(value: ResolveValue, consistency: Consistency) -> Self {
        Self {
            value,
            consistency,
            value_confidence: super::confidence::sure_confidence(),
            consistency_confidence: super::confidence::sure_confidence(),
            exchanges: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum JudgeError {
    Config(String),
    Http(String),
    Parse(String),
    /// Cumulative Jev spend for this run hit the hard cap.
    Limit(String),
}

impl std::fmt::Display for JudgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            JudgeError::Config(s)
            | JudgeError::Http(s)
            | JudgeError::Parse(s)
            | JudgeError::Limit(s) => {
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

    /// One listing tree, Choices for every atom. Default calls `choose` per atom.
    /// Outer results line up with `state.atoms`; each inner vec is that atom's
    /// per-child Choices. Jev overrides this to post one (chunked) request.
    fn choose_many(
        &mut self,
        state: &MultiListingState,
    ) -> Result<Vec<Vec<ChildChoice>>, JudgeError> {
        state
            .atoms
            .iter()
            .map(|atom| self.choose(&state.listing_state(atom)))
            .collect()
    }

    /// Judge several documents. Default calls `document` in order.
    /// Results line up with `states`. Jev overrides this to post concurrently,
    /// grouping questions that share a file into one request.
    fn documents(&mut self, states: &[DocumentState]) -> Vec<Result<DocumentAnswer, JudgeError>> {
        states
            .iter()
            .map(|s| {
                self.document(s).map(|judgement| DocumentAnswer {
                    judgement,
                    exchanges: self.last_exchanges(),
                })
            })
            .collect()
    }

    /// Resolve mixed entailed/contradicted documents for one ground atom.
    /// Default leaves the atom unknown and marks the evidence conflicting.
    fn resolve_conflict(&mut self, _state: &ConflictState) -> Result<ConflictAnswer, JudgeError> {
        Ok(ConflictAnswer::sure(
            ResolveValue::Unknown,
            Consistency::Conflicting,
        ))
    }

    /// Request/response pairs from the most recent `choose`, `document`, or
    /// `resolve_conflict`. Empty for heuristic and fixture judges.
    fn last_exchanges(&self) -> Vec<JudgeExchange> {
        Vec::new()
    }
}
