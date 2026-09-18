use super::judge::{Child, ChildChoice, DocumentJudgement, JudgeExchange};

/// One human-facing moment in a grounding walk.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum GroundStep {
    Listing {
        pred: String,
        args: Vec<String>,
        dir: String,
        children: Vec<Child>,
        choices: Vec<ChildChoice>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        effects: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exchanges: Vec<JudgeExchange>,
    },
    Document {
        pred: String,
        args: Vec<String>,
        path: String,
        judgement: DocumentJudgement,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        preview: String,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        note: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        exchanges: Vec<JudgeExchange>,
    },
    Result {
        pred: String,
        args: Vec<String>,
        status: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        bindings: Vec<(String, String)>,
    },
}

impl GroundStep {
    pub fn exchanges(&self) -> &[JudgeExchange] {
        match self {
            Self::Listing { exchanges, .. } | Self::Document { exchanges, .. } => exchanges,
            Self::Result { .. } => &[],
        }
    }
}

pub fn format_atom(pred: &str, args: &[String]) -> String {
    if args.is_empty() {
        format!("({pred} ?x)")
    } else {
        let bits: Vec<String> = args
            .iter()
            .map(|a| {
                if a.starts_with('?') || a == "*" {
                    a.clone()
                } else {
                    format!("\"{a}\"")
                }
            })
            .collect();
        format!("({pred} {})", bits.join(" "))
    }
}
