use super::judge::{Child, ChildChoice, DocumentJudgement};

/// One human-facing moment in a grounding walk.
#[derive(Debug, Clone)]
pub enum GroundStep {
    Listing {
        pred: String,
        args: Vec<String>,
        dir: String,
        children: Vec<Child>,
        choices: Vec<ChildChoice>,
        effects: Vec<String>,
    },
    Document {
        pred: String,
        args: Vec<String>,
        path: String,
        judgement: DocumentJudgement,
        preview: String,
        note: String,
    },
    Result {
        pred: String,
        args: Vec<String>,
        status: String,
        bindings: Vec<(String, String)>,
    },
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
