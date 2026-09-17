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
        let label = state.args.first().map(|s| s.as_str()).unwrap_or("");
        if label.is_empty() {
            return Ok(enumerate_components(state));
        }
        let matches: Vec<&str> = state
            .children
            .iter()
            .filter(|c| child_matches(c, label) || sample_matches(c, label))
            .map(|c| c.rel_path.as_str())
            .collect();
        let unique_direct = state
            .children
            .iter()
            .filter(|c| child_matches(c, label))
            .count()
            == 1;

        Ok(state
            .children
            .iter()
            .map(|c| {
                let transition = if crate::navigator::walk::is_noise_name(&c.name) {
                    Transition::Skip
                } else if child_matches(c, label) {
                    if c.is_dir {
                        if unique_direct {
                            Transition::Whole
                        } else {
                            Transition::Part
                        }
                    } else {
                        Transition::Whole
                    }
                } else if c.is_dir && sample_matches(c, label) {
                    Transition::Descend
                } else {
                    Transition::Skip
                };
                let confidence: f64 = if crate::navigator::walk::is_noise_name(&c.name) {
                    0.95
                } else if matches.len() <= 2 {
                    0.9
                } else {
                    0.75
                };
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
            _ if state.args.len() >= 2 => document_defines(state),
            _ => {
                let label = state.args.first().map(|s| s.as_str()).unwrap_or("");
                let name = state.path.rsplit('/').next().unwrap_or(&state.path);
                if name_matches(name, label) {
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
    for (constant, entity) in &state.bindings {
        if constant == target {
            if let Some(base) = entity.rsplit('/').next() {
                names.push(normalize(base));
            }
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
            let transition = if crate::navigator::walk::is_noise_name(&c.name) {
                Transition::Skip
            } else if c.is_dir && is_container(&c.name) {
                Transition::Descend
            } else if c.is_dir {
                Transition::Part
            } else {
                Transition::Skip
            };
            ChildChoice {
                rel_path: c.rel_path.clone(),
                transition,
                confidence: 0.85,
            }
        })
        .collect()
}

fn is_container(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "src" | "lib" | "libs" | "packages" | "pkg" | "apps" | "services"
    )
}

fn child_matches(child: &super::judge::Child, label: &str) -> bool {
    name_matches(&child.name, label)
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
    use crate::navigator::judge::Child;

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
        assert_eq!(t("frontend"), Transition::Whole);
        assert_eq!(t("backend"), Transition::Skip);
        assert_eq!(t("decoy"), Transition::Skip);
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
