use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::edb::Store;
use crate::navigator::judge::{
    ChildChoice, DocumentState, Judge, ListingState, Transition, TAU_CONFIDENCE,
};
use crate::navigator::trace::GroundStep;
use crate::navigator::walk::{
    is_noise_name, list_dir, list_files_recursive, parent_dir, read_trimmed, repo_join,
};
use crate::navigator::{Answer, Arg, Demand, Navigator, Need};

const MAX_LISTINGS: usize = 32;
const MAX_DEPTH: usize = 8;
const BODY_BUDGET: usize = 8192;

/// Goal-directed walk of a repo tree. Asks a `Judge` at each listing and
/// document; never evaluates IDB.
pub struct TreeNavigator {
    root: PathBuf,
    judge: Box<dyn Judge>,
    pub listings: Vec<String>,
    pub documents: Vec<String>,
    pub errors: Vec<String>,
    pub trace: Vec<GroundStep>,
}

impl TreeNavigator {
    pub fn new(root: impl Into<PathBuf>, judge: Box<dyn Judge>) -> Self {
        Self {
            root: root.into(),
            judge,
            listings: Vec::new(),
            documents: Vec::new(),
            errors: Vec::new(),
            trace: Vec::new(),
        }
    }

    pub fn heuristic(root: impl Into<PathBuf>) -> Self {
        Self::new(root, Box::new(super::heuristic::HeuristicJudge))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn answer_file(&self, demand: &Demand) -> Answer {
        let Some(path) = bound_at(&demand.args, 0) else {
            return Answer {
                source: "nav:file".into(),
                ..Answer::default()
            };
        };
        let exists = repo_join(&self.root, path).is_file();
        Answer {
            tuples: if exists {
                vec![vec![path.to_string()]]
            } else {
                Vec::new()
            },
            close: true,
            bindings: Vec::new(),
            source: "nav:file".into(),
        }
    }

    fn answer_component(&mut self, demand: &Demand, _store: &Store) -> Answer {
        let (args, free) = match bound_at(&demand.args, 0) {
            Some(label) => (vec![label.to_string()], false),
            None => (Vec::new(), true),
        };
        let located = self.locate("component", &args, demand.need);
        let answer = match located {
            LocateResult::Found(paths) => {
                let bindings = if free {
                    paths
                        .iter()
                        .map(|p| {
                            let name = p.rsplit('/').next().unwrap_or(p).to_string();
                            (name, p.clone())
                        })
                        .collect()
                } else {
                    paths
                        .iter()
                        .map(|p| (args[0].clone(), p.clone()))
                        .collect::<Vec<_>>()
                };
                let tuples = if free {
                    bindings.iter().map(|(n, _)| vec![n.clone()]).collect()
                } else {
                    vec![vec![args[0].clone()]]
                };
                Answer {
                    tuples,
                    close: true,
                    bindings,
                    source: "nav:component".into(),
                }
            }
            LocateResult::Unknown => Answer {
                tuples: Vec::new(),
                close: false,
                bindings: Vec::new(),
                source: "nav:component".into(),
            },
            LocateResult::Absent => Answer {
                tuples: Vec::new(),
                close: demand.need == Need::Closed,
                bindings: Vec::new(),
                source: "nav:component".into(),
            },
        };
        let status = if answer.bindings.is_empty() {
            if answer.close {
                "absent"
            } else {
                "unknown"
            }
        } else {
            "found"
        };
        self.trace.push(GroundStep::Result {
            pred: "component".into(),
            args,
            status: status.into(),
            bindings: answer.bindings.clone(),
        });
        answer
    }

    fn answer_includes(&mut self, demand: &Demand, store: &Store) -> Answer {
        let Some(label) = bound_at(&demand.args, 0) else {
            return Answer {
                source: "nav:includes".into(),
                ..Answer::default()
            };
        };
        if let Some(file) = bound_at(&demand.args, 1) {
            let member = store
                .bindings_for(label)
                .iter()
                .any(|root| file == root || file.starts_with(&format!("{root}/")));
            return Answer {
                tuples: if member {
                    vec![vec![label.to_string(), file.to_string()]]
                } else {
                    Vec::new()
                },
                close: true,
                bindings: Vec::new(),
                source: "nav:includes".into(),
            };
        }
        let mut tuples = Vec::new();
        let mut closed = true;
        let bindings = store.bindings_for(label);
        if bindings.is_empty() {
            closed = false;
        }
        for entity in bindings {
            let path = repo_join(&self.root, &entity);
            if path.is_file() {
                tuples.push(vec![label.to_string(), entity]);
            } else if path.is_dir() {
                match list_files_recursive(&self.root, &entity) {
                    Ok(files) => {
                        for f in files {
                            tuples.push(vec![label.to_string(), f]);
                        }
                    }
                    Err(_) => closed = false,
                }
            } else {
                closed = false;
            }
        }
        Answer {
            tuples,
            close: closed,
            bindings: Vec::new(),
            source: "nav:includes".into(),
        }
    }

    fn answer_defines(&mut self, demand: &Demand, store: &Store) -> Answer {
        let (Some(file), Some(target)) = (bound_at(&demand.args, 0), bound_at(&demand.args, 1))
        else {
            return Answer {
                source: "nav:defines".into(),
                ..Answer::default()
            };
        };
        let path = repo_join(&self.root, file);
        if !path.is_file() {
            return Answer {
                tuples: Vec::new(),
                close: true,
                bindings: Vec::new(),
                source: "nav:defines".into(),
            };
        }
        // Membership is `includes`. A file that sits under y does not
        // *depend on* y; treating that as defines(f, y) makes depends_on(y, y).
        if is_member(store, target, file) {
            self.trace.push(GroundStep::Document {
                pred: "defines".into(),
                args: vec![file.to_string(), target.to_string()],
                path: file.to_string(),
                judgement: crate::navigator::judge::DocumentJudgement {
                    verdict: crate::navigator::judge::DocumentVerdict::False,
                    confidence: 1.0,
                },
                preview: String::new(),
                note: format!("member of {target}; defines is a dependency, not membership"),
                exchanges: Vec::new(),
            });
            return Answer {
                tuples: Vec::new(),
                close: true,
                bindings: Vec::new(),
                source: "nav:defines".into(),
            };
        }
        let (body, _trimmed) = match read_trimmed(&self.root, file, BODY_BUDGET) {
            Ok(v) => v,
            Err(_) => {
                return Answer {
                    tuples: Vec::new(),
                    close: false,
                    bindings: Vec::new(),
                    source: "nav:defines".into(),
                };
            }
        };
        self.documents.push(file.to_string());
        let judgement = match self.judge.document(&DocumentState {
            pred: "defines".into(),
            args: vec![file.to_string(), target.to_string()],
            path: file.to_string(),
            body: body.clone(),
            bindings: store
                .all_bindings()
                .into_iter()
                .map(|b| (b.constant, b.entity))
                .collect(),
        }) {
            Ok(j) => j,
            Err(e) => {
                self.errors.push(e.to_string());
                return Answer {
                    tuples: Vec::new(),
                    close: false,
                    bindings: Vec::new(),
                    source: "nav:defines".into(),
                };
            }
        };
        let note = if judgement.closed_true() {
            format!("depends on {target}")
        } else if judgement.closed_false() {
            format!("no dependency on {target}")
        } else {
            "undecided".into()
        };
        self.trace.push(GroundStep::Document {
            pred: "defines".into(),
            args: vec![file.to_string(), target.to_string()],
            path: file.to_string(),
            judgement: judgement.clone(),
            preview: preview_body(&body),
            note,
            exchanges: self.judge.last_exchanges(),
        });
        if judgement.closed_true() {
            Answer {
                tuples: vec![vec![file.to_string(), target.to_string()]],
                close: true,
                bindings: Vec::new(),
                source: "nav:defines".into(),
            }
        } else if judgement.closed_false() {
            Answer {
                tuples: Vec::new(),
                close: true,
                bindings: Vec::new(),
                source: "nav:defines".into(),
            }
        } else {
            Answer {
                tuples: Vec::new(),
                close: false,
                bindings: Vec::new(),
                source: "nav:defines".into(),
            }
        }
    }

    fn locate(&mut self, pred: &str, args: &[String], need: Need) -> LocateResult {
        let mut frontier: Vec<(String, usize)> = vec![("".to_string(), 0)];
        let mut visited = HashSet::new();
        let mut parts = Vec::new();
        let mut wholes = Vec::new();
        let mut listings = 0;
        let mut all_skips_confident = true;

        while let Some((dir, depth)) = frontier.pop() {
            if listings >= MAX_LISTINGS || depth > MAX_DEPTH {
                all_skips_confident = false;
                break;
            }
            if !visited.insert(dir.clone()) {
                continue;
            }
            let children = match list_dir(&self.root, &dir) {
                Ok(c) => c,
                Err(_) => {
                    all_skips_confident = false;
                    continue;
                }
            };
            self.listings.push(dir.clone());
            listings += 1;
            let choices = match self.judge.choose(&ListingState {
                pred: pred.to_string(),
                args: args.to_vec(),
                dir: dir.clone(),
                children: children.clone(),
            }) {
                Ok(c) => c,
                Err(e) => {
                    self.errors.push(e.to_string());
                    all_skips_confident = false;
                    continue;
                }
            };
            let exchanges = self.judge.last_exchanges();
            let mut applied = Vec::new();
            let mut effects = Vec::new();
            for child in &children {
                let mut choice = choices
                    .iter()
                    .find(|c| c.rel_path == child.rel_path)
                    .cloned()
                    .unwrap_or(ChildChoice {
                        rel_path: child.rel_path.clone(),
                        transition: Transition::Skip,
                        confidence: 0.0,
                    });
                if is_noise_name(&child.name) {
                    if choice.transition != Transition::Skip {
                        effects.push(format!(
                            "{}: judge said {} → skip (noise)",
                            child.name,
                            choice.transition.label()
                        ));
                    }
                    choice.transition = Transition::Skip;
                    choice.confidence = choice.confidence.max(0.9);
                }
                match choice.transition {
                    Transition::Skip => {
                        if choice.confidence < TAU_CONFIDENCE {
                            all_skips_confident = false;
                        }
                    }
                    Transition::Descend => {
                        if child.is_dir {
                            frontier.push((child.rel_path.clone(), depth + 1));
                            effects.push(format!("{}: descend", child.name));
                        }
                    }
                    Transition::Part => {
                        if choice.confidence < TAU_CONFIDENCE {
                            all_skips_confident = false;
                            effects.push(format!("{}: part ignored (low confidence)", child.name));
                        } else if child.is_dir {
                            parts.push(child.rel_path.clone());
                            effects.push(format!("{}: part → bind", child.name));
                        } else {
                            effects.push(format!(
                                "{}: part ignored (file is membership, not a root)",
                                child.name
                            ));
                        }
                    }
                    Transition::Whole => {
                        if choice.confidence < TAU_CONFIDENCE && child.is_dir {
                            all_skips_confident = false;
                            effects.push(format!("{}: whole ignored (low confidence)", child.name));
                        } else if child.is_dir {
                            wholes.push(child.rel_path.clone());
                            effects.push(format!("{}: whole → bind", child.name));
                        } else {
                            match self.confirm_file(pred, args, &child.rel_path) {
                                FileConfirm::Entire => {
                                    wholes.push(child.rel_path.clone());
                                    effects.push(format!(
                                        "{}: whole file, entire component",
                                        child.name
                                    ));
                                }
                                FileConfirm::Undecided => {
                                    if let Some(parent) = parent_dir(&child.rel_path) {
                                        parts.push(parent.to_string());
                                        effects.push(format!(
                                            "{}: not confirmed entire → lift {}",
                                            child.name, parent
                                        ));
                                    } else {
                                        all_skips_confident = false;
                                    }
                                }
                                FileConfirm::NotEntire => {
                                    effects
                                        .push(format!("{}: not the whole component", child.name));
                                }
                            }
                        }
                    }
                }
                applied.push(choice);
            }
            self.trace.push(GroundStep::Listing {
                pred: pred.to_string(),
                args: args.to_vec(),
                dir: dir.clone(),
                children: children.clone(),
                choices: applied,
                effects,
                exchanges,
            });
            if need == Need::Truth && !wholes.is_empty() && parts.is_empty() {
                // Keep walking only if the frontier might refine a nested whole.
                if frontier.is_empty() {
                    break;
                }
            }
        }

        let tight = tightest_wholes(wholes);
        if tight.len() > 1 {
            // Two non-nested wholes: leave unknown rather than guess.
            return LocateResult::Unknown;
        }
        let mut binding = tight;
        binding.extend(parts);
        binding.sort();
        binding.dedup();

        if binding.is_empty() {
            if need == Need::Closed && all_skips_confident {
                LocateResult::Absent
            } else {
                LocateResult::Unknown
            }
        } else {
            LocateResult::Found(binding)
        }
    }

    fn confirm_file(&mut self, pred: &str, args: &[String], rel: &str) -> FileConfirm {
        let (body, _) = match read_trimmed(&self.root, rel, BODY_BUDGET) {
            Ok(v) => v,
            Err(_) => return FileConfirm::Undecided,
        };
        self.documents.push(rel.to_string());
        let j = match self.judge.document(&DocumentState {
            pred: pred.to_string(),
            args: args.to_vec(),
            path: rel.to_string(),
            body: body.clone(),
            bindings: Vec::new(),
        }) {
            Ok(j) => j,
            Err(e) => {
                self.errors.push(e.to_string());
                return FileConfirm::Undecided;
            }
        };
        let confirm = if j.closed_true() {
            FileConfirm::Entire
        } else if j.closed_false() {
            FileConfirm::NotEntire
        } else {
            FileConfirm::Undecided
        };
        let note = match confirm {
            FileConfirm::Entire => "entire component".into(),
            FileConfirm::NotEntire => "not the whole component".into(),
            FileConfirm::Undecided => "unconfirmed; may lift parent directory".into(),
        };
        self.trace.push(GroundStep::Document {
            pred: pred.to_string(),
            args: args.to_vec(),
            path: rel.to_string(),
            judgement: j,
            preview: preview_body(&body),
            note,
            exchanges: self.judge.last_exchanges(),
        });
        confirm
    }
}

fn preview_body(body: &str) -> String {
    body.lines()
        .take(8)
        .map(|l| {
            if l.len() > 80 {
                format!("{}…", &l[..80])
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

enum FileConfirm {
    Entire,
    NotEntire,
    Undecided,
}

enum LocateResult {
    Found(Vec<String>),
    Unknown,
    Absent,
}

fn is_member(store: &Store, label: &str, file: &str) -> bool {
    store
        .bindings_for(label)
        .iter()
        .any(|root| file == root || file.starts_with(&format!("{root}/")))
}

fn bound_at<'a>(args: &'a [Arg], i: usize) -> Option<&'a str> {
    match args.get(i) {
        Some(Arg::Bound(s)) => Some(s.as_str()),
        _ => None,
    }
}

fn tightest_wholes(mut paths: Vec<String>) -> Vec<String> {
    paths.sort_by_key(|p| p.len());
    let mut keep = Vec::new();
    for p in paths.into_iter().rev() {
        if keep
            .iter()
            .any(|k: &String| k.starts_with(&format!("{p}/")) || k == &p)
        {
            continue;
        }
        keep.push(p);
    }
    keep
}

impl Navigator for TreeNavigator {
    fn answer(&mut self, demand: &Demand, store: &Store) -> Answer {
        match demand.pred.as_str() {
            "file" => self.answer_file(demand),
            "component" => self.answer_component(demand, store),
            "includes" => self.answer_includes(demand, store),
            "defines" => self.answer_defines(demand, store),
            _ => Answer {
                source: format!("nav:{}", demand.pred),
                ..Answer::default()
            },
        }
    }
}
