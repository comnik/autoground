use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::edb::Store;
use crate::navigator::judge::{
    ChildChoice, DocumentState, Judge, ListingState, Transition, TAU_CONFIDENCE,
};
use crate::navigator::trace::GroundStep;
use crate::navigator::walk::{
    is_noise_name, list_dir, list_files_recursive, read_trimmed, repo_join,
};
use crate::navigator::{Answer, Arg, Demand, Navigator, Need};
use crate::syntax::{verbalize_atom, Declare, Term};

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
    declarations: HashMap<String, Declare>,
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
            declarations: HashMap::new(),
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

    fn answer_locate(&mut self, demand: &Demand, _store: &Store) -> Answer {
        let pred = demand.pred.as_str();
        let (args, free) = match bound_at(&demand.args, 0) {
            Some(label) => (vec![label.to_string()], false),
            None => (Vec::new(), true),
        };
        let located = self.locate(pred, &args, demand.need);
        let source = format!("nav:{pred}");
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
                    source: source.clone(),
                }
            }
            LocateResult::Unknown => Answer {
                tuples: Vec::new(),
                close: false,
                bindings: Vec::new(),
                source: source.clone(),
            },
            LocateResult::Absent => Answer {
                tuples: Vec::new(),
                close: demand.need == Need::Closed,
                bindings: Vec::new(),
                source: source.clone(),
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
            pred: pred.to_string(),
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

    fn answer_relation(&mut self, demand: &Demand, store: &Store) -> Answer {
        let pred = demand.pred.as_str();
        let source = format!("nav:{pred}");
        let bound: Option<Vec<String>> = demand
            .args
            .iter()
            .map(|a| match a {
                Arg::Bound(s) => Some(s.clone()),
                Arg::Free(_) => None,
            })
            .collect();
        let Some(args) = bound else {
            return Answer {
                source,
                ..Answer::default()
            };
        };
        let Some(file_i) = self.file_arg_index(pred, &args) else {
            return Answer {
                source,
                ..Answer::default()
            };
        };
        let file = args[file_i].as_str();
        let path = repo_join(&self.root, file);
        if !path.is_file() {
            return Answer {
                tuples: Vec::new(),
                close: true,
                bindings: Vec::new(),
                source,
            };
        }
        // Membership is `includes`. A file that sits under y does not
        // *depend on* y; treating that as defines(f, y) makes depends_on(y, y).
        if pred == "defines" {
            if let Some(target) = args.get(1) {
                if is_member(store, target, file) {
                    self.trace.push(GroundStep::Document {
                        pred: pred.to_string(),
                        args: args.clone(),
                        path: file.to_string(),
                        judgement: crate::navigator::judge::DocumentJudgement {
                            verdict: crate::navigator::judge::DocumentVerdict::False,
                            confidence: 1.0,
                        },
                        preview: String::new(),
                        note: format!(
                            "member of {target}; defines is a dependency, not membership"
                        ),
                        exchanges: Vec::new(),
                    });
                    return Answer {
                        tuples: Vec::new(),
                        close: true,
                        bindings: Vec::new(),
                        source,
                    };
                }
            }
        }
        let (body, _trimmed) = match read_trimmed(&self.root, file, BODY_BUDGET) {
            Ok(v) => v,
            Err(_) => {
                return Answer {
                    tuples: Vec::new(),
                    close: false,
                    bindings: Vec::new(),
                    source,
                };
            }
        };
        self.documents.push(file.to_string());
        let verbalization = self.verbalize(pred, &args);
        let judgement = match self.judge.document(&DocumentState {
            pred: pred.to_string(),
            args: args.clone(),
            path: file.to_string(),
            body: body.clone(),
            bindings: store
                .all_bindings()
                .into_iter()
                .map(|b| (b.constant, b.entity))
                .collect(),
            verbalization,
        }) {
            Ok(j) => j,
            Err(e) => {
                self.errors.push(e.to_string());
                return Answer {
                    tuples: Vec::new(),
                    close: false,
                    bindings: Vec::new(),
                    source,
                };
            }
        };
        let note = if judgement.closed_true() {
            "holds".into()
        } else if judgement.closed_false() {
            "does not hold".into()
        } else {
            "undecided".into()
        };
        self.trace.push(GroundStep::Document {
            pred: pred.to_string(),
            args: args.clone(),
            path: file.to_string(),
            judgement: judgement.clone(),
            preview: preview_body(&body),
            note,
            exchanges: self.judge.last_exchanges(),
        });
        if judgement.closed_true() {
            Answer {
                tuples: vec![args],
                close: true,
                bindings: Vec::new(),
                source,
            }
        } else if judgement.closed_false() {
            Answer {
                tuples: Vec::new(),
                close: true,
                bindings: Vec::new(),
                source,
            }
        } else {
            Answer {
                tuples: Vec::new(),
                close: false,
                bindings: Vec::new(),
                source,
            }
        }
    }

    fn file_arg_index(&self, pred: &str, args: &[String]) -> Option<usize> {
        if let Some(d) = self.declarations.get(pred) {
            if let Some(i) = d.slots.iter().position(|s| s.eq_ignore_ascii_case("file")) {
                return Some(i);
            }
        }
        args.iter().position(|a| repo_join(&self.root, a).is_file())
    }

    fn verbalize(&self, pred: &str, args: &[String]) -> String {
        let terms: Vec<Term> = args.iter().cloned().map(Term::Const).collect();
        verbalize_atom(pred, &terms, &self.declarations)
    }

    fn locate(&mut self, pred: &str, args: &[String], need: Need) -> LocateResult {
        let mut frontier: Vec<(String, usize)> = vec![("".to_string(), 0)];
        let mut visited = HashSet::new();
        let mut hits = Vec::new();
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
            let verbalization = self.verbalize(pred, args);
            let choices = match self.judge.choose(&ListingState {
                pred: pred.to_string(),
                args: args.to_vec(),
                dir: dir.clone(),
                children: children.clone(),
                verbalization,
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
                    Transition::Investigate => {
                        if child.is_dir {
                            frontier.push((child.rel_path.clone(), depth + 1));
                            effects.push(format!("{}: investigate → list", child.name));
                        } else {
                            match self.confirm_file(pred, args, &child.rel_path) {
                                FileConfirm::Witness => {
                                    hits.push(child.rel_path.clone());
                                    effects
                                        .push(format!("{}: investigate → bind file", child.name));
                                }
                                FileConfirm::NotWitness => {
                                    effects.push(format!("{}: file is not a witness", child.name));
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
            if need == Need::Truth && !hits.is_empty() && frontier.is_empty() {
                break;
            }
        }

        let mut binding = tightest_wholes(hits);
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
            Err(_) => return FileConfirm::NotWitness,
        };
        self.documents.push(rel.to_string());
        let verbalization = self.verbalize(pred, args);
        let j = match self.judge.document(&DocumentState {
            pred: pred.to_string(),
            args: args.to_vec(),
            path: rel.to_string(),
            body: body.clone(),
            bindings: Vec::new(),
            verbalization,
        }) {
            Ok(j) => j,
            Err(e) => {
                self.errors.push(e.to_string());
                return FileConfirm::NotWitness;
            }
        };
        let confirm = if j.closed_true() {
            FileConfirm::Witness
        } else {
            FileConfirm::NotWitness
        };
        let note = match confirm {
            FileConfirm::Witness => "witness".into(),
            FileConfirm::NotWitness => "not a witness".into(),
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
    Witness,
    NotWitness,
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
    fn prepare(&mut self, compiled: &crate::core::Compiled) {
        self.declarations = compiled.declarations.clone();
    }

    fn answer(&mut self, demand: &Demand, store: &Store) -> Answer {
        match demand.pred.as_str() {
            "file" | "File" => self.answer_file(demand),
            "includes" => self.answer_includes(demand, store),
            _ if demand.args.len() <= 1 => self.answer_locate(demand, store),
            _ => self.answer_relation(demand, store),
        }
    }
}
