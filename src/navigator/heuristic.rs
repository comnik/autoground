use super::judge::{
    ChildChoice, DocumentJudgement, DocumentState, DocumentVerdict, Judge, ListingState,
    Transition, TAU_CONFIDENCE,
};

/// Local stand-in for Jev on the documented toys. Answers the same questions
/// the System One client will; it is not an import of dir-name as a separate
/// semantic path.
pub struct HeuristicJudge;

impl Judge for HeuristicJudge {
    fn choose(
        &mut self,
        state: &ListingState,
    ) -> Result<Vec<ChildChoice>, super::judge::JudgeError> {
        let labels: Vec<&str> = state.args.iter().map(|s| s.as_str()).collect();
        if labels.iter().all(|s| s.is_empty()) {
            return Ok(enumerate_components(state));
        }
        let hit = |c: &super::judge::Child| {
            labels.iter().any(|label| {
                child_matches(c, label)
                    || sample_matches(c, label)
                    || path_matches(&c.rel_path, label)
            })
        };
        let matches: Vec<&str> = state
            .children
            .iter()
            .filter(|c| hit(c))
            .map(|c| c.rel_path.as_str())
            .collect();
        Ok(state
            .children
            .iter()
            .map(|c| {
                let transition = if c.is_dir
                    && labels
                        .iter()
                        .any(|label| child_matches(c, label) || sample_matches(c, label))
                {
                    Transition::Investigate
                } else if !c.is_dir && labels.iter().any(|label| path_matches(&c.rel_path, label)) {
                    Transition::Investigate
                } else {
                    Transition::Skip
                };
                let confidence: f64 = if matches.len() <= 2 { 0.9 } else { 0.75 };
                ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition,
                    confidence: confidence.max(TAU_CONFIDENCE),
                }
            })
            .collect())
    }

    fn document(
        &mut self,
        state: &DocumentState,
    ) -> Result<DocumentJudgement, super::judge::JudgeError> {
        Ok(match state.pred.as_str() {
            "defines" => document_defines(state),
            _ if state.args.len() >= 2 => {
                let conceptual: Vec<&str> = state
                    .args
                    .iter()
                    .map(|s| s.as_str())
                    .filter(|a| *a != state.path && !looks_like_repo_path(a))
                    .collect();
                if conceptual.is_empty() {
                    document_defines(state)
                } else if conceptual.iter().any(|label| path_matches(&state.path, label))
                {
                    DocumentJudgement {
                        verdict: DocumentVerdict::True,
                        confidence: 0.85,
                    }
                } else {
                    DocumentJudgement {
                        verdict: DocumentVerdict::False,
                        confidence: 0.8,
                    }
                }
            }
            _ => {
                let label = state.args.first().map(|s| s.as_str()).unwrap_or("");
                if path_matches(&state.path, label) {
                    DocumentJudgement {
                        verdict: DocumentVerdict::True,
                        confidence: 0.85,
                    }
                } else {
                    DocumentJudgement {
                        verdict: DocumentVerdict::False,
                        confidence: 0.8,
                    }
                }
            }
        })
    }
}

fn document_defines(state: &DocumentState) -> DocumentJudgement {
    let target = state.args.get(1).map(|s| s.as_str()).unwrap_or("");
    let mut names = aliases(target);
    names.extend(path_import_names(target));
    for (constant, entity) in &state.bindings {
        if constant == target {
            if let Some(base) = entity.rsplit('/').next() {
                names.push(normalize(base));
            }
        }
        if target == entity || target.starts_with(&format!("{entity}/")) {
            names.extend(aliases(constant));
            names.extend(path_import_names(entity));
        }
    }
    names.sort();
    names.dedup();
    let hit = names.iter().any(|n| import_mentions(&state.body, n));
    if hit {
        DocumentJudgement {
            verdict: DocumentVerdict::True,
            confidence: 0.88,
        }
    } else {
        DocumentJudgement {
            verdict: DocumentVerdict::False,
            confidence: 0.85,
        }
    }
}

fn enumerate_components(state: &ListingState) -> Vec<ChildChoice> {
    state
        .children
        .iter()
        .map(|c| {
            ChildChoice {
                rel_path: c.rel_path.clone(),
                transition: Transition::Investigate,
                confidence: 0.85,
            }
        })
        .collect()
}

fn child_matches(child: &super::judge::Child, label: &str) -> bool {
    name_matches(&child.name, label)
}

fn path_matches(path: &str, label: &str) -> bool {
    path.split('/').any(|p| name_matches(p, label))
}

fn sample_matches(child: &super::judge::Child, label: &str) -> bool {
    child.sample.iter().any(|n| name_matches(n, label))
}

fn name_matches(name: &str, label: &str) -> bool {
    let stem = stem(name);
    let n = normalize(&stem);
    if aliases(label).iter().any(|a| a == &n) {
        return true;
    }
    // A lone JS/TS entry file can be the whole frontend; app.py is not.
    let label_n = normalize(label);
    (label_n == "frontend" || label_n == "ui")
        && n == "app"
        && matches!(
            ext(name).as_str(),
            "js" | "jsx" | "ts" | "tsx" | "mjs" | "cjs" | "vue"
        )
}

fn looks_like_repo_path(s: &str) -> bool {
    s.contains('/') || s.contains('\\') || !ext(s).is_empty()
}

fn ext(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[i + 1..].to_lowercase(),
        _ => String::new(),
    }
}

fn stem(name: &str) -> String {
    match name.rfind('.') {
        Some(i) if i > 0 => name[..i].to_string(),
        _ => name.to_string(),
    }
}

fn normalize(s: &str) -> String {
    s.chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| *c != '-' && *c != '_')
        .collect()
}

fn path_import_names(path: &str) -> Vec<String> {
    let trimmed = path.trim_matches('/');
    if trimmed.is_empty() {
        return Vec::new();
    }
    let no_ext = match trimmed.rfind('.') {
        Some(i) if i > 0 && !trimmed[..i].contains('/') => &trimmed[..i],
        Some(i) if i > trimmed.rfind('/').unwrap_or(0) => &trimmed[..i],
        _ => trimmed,
    };
    let mut names = vec![no_ext.replace('/', ".")];
    if let Some(stem) = no_ext.rsplit('/').next() {
        if stem != "__init__" {
            names.push(stem.to_string());
        }
    }
    if let Some((parent, _)) = no_ext.split_once('/') {
        names.push(parent.to_string());
    }
    names
}

fn aliases(label: &str) -> Vec<String> {
    let n = normalize(label);
    let mut out = vec![n.clone()];
    match n.as_str() {
        "frontend" => {
            out.push("ui".into());
        }
        "backend" => {
            out.push("api".into());
            out.push("server".into());
        }
        "ui" => out.push("frontend".into()),
        "api" | "server" => out.push("backend".into()),
        _ => {}
    }
    out
}

fn import_mentions(body: &str, name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let patterns = [
        format!("from {name} "),
        format!("from {name}."),
        format!("from {name} import"),
        format!("import {name}"),
        format!("import {name}."),
        format!("from \"./{name}\""),
        format!("from './{name}'"),
        format!("\"{name}\""),
        format!("'/{name}'"),
        format!("\"./{name}.js\""),
        format!("'./{name}.js'"),
    ];
    let lower = body.to_lowercase();
    patterns.iter().any(|p| lower.contains(&p.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{Child, DocumentState, DocumentVerdict};

    #[test]
    fn file_import_mentions_module_path() {
        let state = DocumentState {
            pred: "includes".into(),
            args: vec!["ui/app.py".into(), "api/client.py".into()],
            path: "ui/app.py".into(),
            body: "from api import client\n".into(),
            bindings: vec![("Backend".into(), "api/client.py".into())],
            verbalization: String::new(),
        };
        let j = document_defines(&state);
        assert_eq!(j.verdict, DocumentVerdict::True);
    }

    #[test]
    fn frontend_locks_directory() {
        let mut j = HeuristicJudge;
        let state = ListingState {
            pred: "component".into(),
            args: vec!["Frontend".into()],
            dir: "".into(),
            children: vec![
                child("frontend", true, vec![]),
                child("backend", true, vec![]),
                child("decoy", true, vec!["noise.py".into()]),
            ],
            verbalization: String::new(),
        };
        let choices = j.choose(&state).unwrap();
        let t = |name: &str| {
            choices
                .iter()
                .find(|c| c.rel_path == name)
                .unwrap()
                .transition
        };
        assert_eq!(t("frontend"), Transition::Investigate);
        assert_eq!(t("backend"), Transition::Skip);
        assert_eq!(t("decoy"), Transition::Skip);
    }

    #[test]
    fn listing_matches_any_argument() {
        let mut j = HeuristicJudge;
        let state = ListingState {
            pred: "knows_about".into(),
            args: vec!["Judge".into(), "Evaluator".into()],
            dir: "src".into(),
            children: vec![
                child("judge.rs", false, vec![]),
                child("evaluator.rs", false, vec![]),
                child("noise.rs", false, vec![]),
            ],
            verbalization: String::new(),
        };
        let choices = j.choose(&state).unwrap();
        let t = |name: &str| {
            choices
                .iter()
                .find(|c| c.rel_path == name)
                .unwrap()
                .transition
        };
        assert_eq!(t("judge.rs"), Transition::Investigate);
        assert_eq!(t("evaluator.rs"), Transition::Investigate);
        assert_eq!(t("noise.rs"), Transition::Skip);
    }

    fn child(name: &str, is_dir: bool, sample: Vec<String>) -> Child {
        Child {
            name: name.into(),
            rel_path: name.into(),
            is_dir,
            sample,
        }
    }
}
