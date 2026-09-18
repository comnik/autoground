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

    fn answer_locate(&mut self, demand: &Demand, store: &Store) -> Answer {
        let pred = demand.pred.as_str();
        let (args, free) = bound_args(demand);
        if free {
            let mut tuples = store.facts_for(pred);
            if !tuples.is_empty() {
                tuples.sort();
                return Answer {
                    tuples,
                    close: true,
                    bindings: Vec::new(),
                    source: format!("nav:{pred}"),
                };
            }
        }
        let located = self.locate(pred, &args, demand.need, store);
        let source = format!("nav:{pred}");
        let unary = demand.args.len() == 1;
        let answer = match located {
            LocateResult::Found(paths) => {
                if free {
                    let bindings: Vec<(String, String)> = paths
                        .iter()
                        .map(|p| {
                            let name = p.rsplit('/').next().unwrap_or(p).to_string();
                            (name, p.clone())
                        })
                        .collect();
                    let tuples = bindings.iter().map(|(n, _)| vec![n.clone()]).collect();
                    Answer {
                        tuples,
                        close: true,
                        bindings,
                        source: source.clone(),
                    }
                } else if unary {
                    let label = args[0].clone();
                    Answer {
                        tuples: vec![vec![label.clone()]],
                        close: true,
                        bindings: paths.iter().map(|p| (label.clone(), p.clone())).collect(),
                        source: source.clone(),
                    }
                } else {
                    // Witness files prove the ground atom; they are not a denotation.
                    Answer {
                        tuples: vec![args.clone()],
                        close: true,
                        bindings: Vec::new(),
                        source: source.clone(),
                    }
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
        let status = if !answer.tuples.is_empty() {
            "found"
        } else if answer.close {
            "absent"
        } else {
            "unknown"
        };
        self.trace.push(GroundStep::Result {
            pred: pred.to_string(),
            args,
            status: status.into(),
            bindings: answer.bindings.clone(),
        });
        answer
    }

    /// One free arg, the rest bound: files under those constants' bindings.
    fn answer_membership(&mut self, demand: &Demand, store: &Store) -> Option<Answer> {
        let source = format!("nav:{}", demand.pred);
        let mut file_i = None;
        let mut bound = Vec::new();
        for (i, a) in demand.args.iter().enumerate() {
            match a {
                Arg::Free(_) if file_i.is_none() => file_i = Some(i),
                Arg::Free(_) => return None,
                Arg::Bound(v) => bound.push((i, v.clone())),
            }
        }
        let file_i = file_i?;
        if bound.is_empty() {
            return None;
        }
        let arity = demand.args.len();
        let mut tuples = Vec::new();
        let mut closed = true;
        let mut any = false;
        for (_, label) in &bound {
            let bindings = store.bindings_for(label);
            if bindings.is_empty() {
                continue;
            }
            any = true;
            for entity in bindings {
                let path = repo_join(&self.root, &entity);
                if path.is_file() {
                    tuples.push(demand_tuple(arity, file_i, &entity, &bound));
                } else if path.is_dir() {
                    match list_files_recursive(&self.root, &entity) {
                        Ok(files) => {
                            for f in files {
                                tuples.push(demand_tuple(arity, file_i, &f, &bound));
                            }
                        }
                        Err(_) => closed = false,
                    }
                } else {
                    closed = false;
                }
            }
        }
        if !any {
            closed = false;
        }
        Some(Answer {
            tuples,
            close: closed,
            bindings: Vec::new(),
            source,
        })
    }

    /// Ground atom with a file and an already-located constant: path-prefix
    /// membership from the binding table, not a document Choice.
    fn answer_member_test(
        &self,
        pred: &str,
        args: &[String],
        file_i: usize,
        store: &Store,
    ) -> Option<Answer> {
        let file = args[file_i].as_str();
        let mut saw_locator = false;
        let mut member = false;
        for (i, a) in args.iter().enumerate() {
            if i == file_i {
                continue;
            }
            if store.bindings_for(a).is_empty() {
                continue;
            }
            saw_locator = true;
            if is_member(store, a, file) {
                member = true;
            }
        }
        if !saw_locator {
            return None;
        }
        Some(Answer {
            tuples: if member {
                vec![args.to_vec()]
            } else {
                Vec::new()
            },
            close: true,
            bindings: Vec::new(),
            source: format!("nav:{pred}"),
        })
    }

    /// An argument is already a repo file: judge that document, no walk.
    fn answer_document(
        &mut self,
        pred: &str,
        args: &[String],
        file_i: usize,
        store: &Store,
    ) -> Answer {
        let source = format!("nav:{pred}");
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
        // A file under y is a member of y; that is not proof that this
        // atom (e.g. a document relation) holds of y.
        if let Some(target) = args.get(1) {
            if is_member(store, target, file) {
                self.trace.push(GroundStep::Document {
                    pred: pred.to_string(),
                    args: args.to_vec(),
                    path: file.to_string(),
                    judgement: crate::navigator::judge::DocumentJudgement {
                        verdict: crate::navigator::judge::DocumentVerdict::False,
                        confidence: 1.0,
                    },
                    preview: String::new(),
                    note: format!("member of {target}; membership is includes, not {pred}"),
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
        let verbalization = self.verbalize(pred, args);
        let judgement = match self.judge.document(&DocumentState {
            pred: pred.to_string(),
            args: args.to_vec(),
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
            args: args.to_vec(),
            path: file.to_string(),
            judgement: judgement.clone(),
            preview: preview_body(&body),
            note,
            exchanges: self.judge.last_exchanges(),
        });
        if judgement.closed_true() {
            Answer {
                tuples: vec![args.to_vec()],
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

    fn verbalize(&self, pred: &str, args: &[String]) -> String {
        let terms: Vec<Term> = args.iter().cloned().map(Term::Const).collect();
        verbalize_atom(pred, &terms, &self.declarations)
    }

    fn locate(&mut self, pred: &str, args: &[String], need: Need, store: &Store) -> LocateResult {
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
            let mut open_files = Vec::new();
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
                            if args.is_empty() {
                                hits.push(child.rel_path.clone());
                            }
                        } else {
                            open_files.push(child.rel_path.clone());
                            effects.push(format!("{}: investigate → open", child.name));
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
            hits.extend(self.confirm_files(pred, args, &open_files, store));
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

    /// Files flagged `investigate` in one listing. Their document Nouls do
    /// not depend on each other; the judge may post them concurrently.
    fn confirm_files(
        &mut self,
        pred: &str,
        args: &[String],
        rels: &[String],
        store: &Store,
    ) -> Vec<String> {
        struct Prepared {
            rel: String,
            body: String,
            state: DocumentState,
        }
        let mut prepared = Vec::new();
        for rel in rels {
            let (body, _) = match read_trimmed(&self.root, rel, BODY_BUDGET) {
                Ok(v) => v,
                Err(_) => continue,
            };
            self.documents.push(rel.clone());
            prepared.push(Prepared {
                rel: rel.clone(),
                body: body.clone(),
                state: DocumentState {
                    pred: pred.to_string(),
                    args: args.to_vec(),
                    path: rel.clone(),
                    body,
                    bindings: store
                        .all_bindings()
                        .into_iter()
                        .map(|b| (b.constant, b.entity))
                        .collect(),
                    verbalization: self.verbalize(pred, args),
                },
            });
        }
        if prepared.is_empty() {
            return Vec::new();
        }
        let states: Vec<DocumentState> = prepared.iter().map(|p| p.state.clone()).collect();
        let answers = self.judge.documents(&states);
        let mut hits = Vec::new();
        for (prep, answer) in prepared.into_iter().zip(answers) {
            let a = match answer {
                Ok(a) => a,
                Err(e) => {
                    self.errors.push(e.to_string());
                    continue;
                }
            };
            let confirm = if a.judgement.closed_true() {
                FileConfirm::Witness
            } else {
                FileConfirm::NotWitness
            };
            if confirm == FileConfirm::Witness {
                hits.push(prep.rel.clone());
            }
            let note = match confirm {
                FileConfirm::Witness => "witness".into(),
                FileConfirm::NotWitness => "not a witness".into(),
            };
            self.trace.push(GroundStep::Document {
                pred: pred.to_string(),
                args: args.to_vec(),
                path: prep.rel,
                judgement: a.judgement,
                preview: preview_body(&prep.body),
                note,
                exchanges: a.exchanges,
            });
        }
        hits
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

#[derive(Clone, Copy, PartialEq, Eq)]
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

fn bound_args(demand: &Demand) -> (Vec<String>, bool) {
    let mut args = Vec::new();
    let mut free = false;
    for a in &demand.args {
        match a {
            Arg::Bound(v) => args.push(v.clone()),
            Arg::Free(_) => free = true,
        }
    }
    (args, free)
}

fn demand_tuple(arity: usize, file_i: usize, file: &str, bound: &[(usize, String)]) -> Vec<String> {
    let mut row = vec![String::new(); arity];
    if file_i < arity {
        row[file_i] = file.to_string();
    }
    for (i, v) in bound {
        if *i < arity {
            row[*i] = v.clone();
        }
    }
    row
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
        if matches!(demand.pred.as_str(), "file" | "File") {
            return self.answer_file(demand);
        }

        let all_bound = demand.args.iter().all(|a| matches!(a, Arg::Bound(_)));
        if all_bound {
            let args: Vec<String> = demand
                .args
                .iter()
                .map(|a| match a {
                    Arg::Bound(s) => s.clone(),
                    Arg::Free(_) => unreachable!(),
                })
                .collect();
            if let Some(file_i) = args.iter().position(|a| repo_join(&self.root, a).is_file()) {
                if let Some(answer) = self.answer_member_test(&demand.pred, &args, file_i, store) {
                    return answer;
                }
                return self.answer_document(&demand.pred, &args, file_i, store);
            }
            return self.answer_locate(demand, store);
        }

        if let Some(answer) = self.answer_membership(demand, store) {
            return answer;
        }

        if demand.args.iter().any(|a| matches!(a, Arg::Bound(_))) {
            return Answer {
                source: format!("nav:{}", demand.pred),
                ..Answer::default()
            };
        }
        // `exists [c] (component c)`: locate entities. A free n-ary atom is
        // an unbounded cartesian; do not walk the repo for that.
        if demand.args.len() <= 1 {
            return self.answer_locate(demand, store);
        }
        Answer {
            source: format!("nav:{}", demand.pred),
            ..Answer::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::navigator::judge::{
        ChildChoice, DocumentAnswer, DocumentJudgement, DocumentVerdict, JudgeError,
    };
    use crate::navigator::{Arg, Demand, Need};
    use std::sync::{Arc, Mutex};

    struct OpenAll;

    impl Judge for OpenAll {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            Ok(state
                .children
                .iter()
                .map(|c| ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition: if c.is_dir {
                        Transition::Skip
                    } else {
                        Transition::Investigate
                    },
                    confidence: 0.9,
                })
                .collect())
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::True,
                confidence: 0.9,
            })
        }
    }

    struct BatchProbe {
        sizes: Arc<Mutex<Vec<usize>>>,
    }

    impl Judge for BatchProbe {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            OpenAll.document(state)
        }

        fn documents(
            &mut self,
            states: &[DocumentState],
        ) -> Vec<Result<DocumentAnswer, JudgeError>> {
            self.sizes.lock().unwrap().push(states.len());
            OpenAll.documents(states)
        }
    }

    #[test]
    fn locate_judges_open_files_as_one_batch() {
        let dir = std::env::temp_dir().join(format!(
            "autoground-doc-batch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "a").unwrap();
        std::fs::write(dir.join("b.py"), "b").unwrap();
        std::fs::write(dir.join("c.py"), "c").unwrap();
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let mut nav = TreeNavigator::new(
            &dir,
            Box::new(BatchProbe {
                sizes: sizes.clone(),
            }),
        );
        let demand = Demand {
            pred: "component".into(),
            args: vec![Arg::Bound("X".into())],
            need: Need::Truth,
        };
        let _ = nav.answer(&demand, &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        let sizes = sizes.lock().unwrap().clone();
        assert!(
            sizes.iter().any(|&n| n >= 3),
            "expected one document batch of 3 files, got {sizes:?}"
        );
        let docs: Vec<_> = nav
            .trace
            .iter()
            .filter_map(|s| match s {
                GroundStep::Document { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(docs, ["a.py", "b.py", "c.py"]);
    }

    struct CaptureDocs {
        docs: Arc<Mutex<Vec<DocumentState>>>,
    }

    impl Judge for CaptureDocs {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            self.docs.lock().unwrap().push(state.clone());
            OpenAll.document(state)
        }
    }

    fn scratch_dir(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "autoground-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn knows_about_compiled() -> crate::core::Compiled {
        let src = r#"
(declare component "{String} is a (conceptual or concrete) component in the codebase")
(declare knows_about "{component} knows about {component}")
"#;
        crate::core::compile(&crate::syntax::parse(src).unwrap()).unwrap()
    }

    #[test]
    fn nary_ground_atom_walks_and_judges_verbalized_document() {
        let dir = scratch_dir("nary-locate");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("hit.py"), "Judge talks to Evaluator\n").unwrap();
        let docs = Arc::new(Mutex::new(Vec::new()));
        let mut nav = TreeNavigator::new(&dir, Box::new(CaptureDocs { docs: docs.clone() }));
        nav.prepare(&knows_about_compiled());
        let demand = Demand {
            pred: "knows_about".into(),
            args: vec![Arg::Bound("Judge".into()), Arg::Bound("Evaluator".into())],
            need: Need::Truth,
        };
        let answer = nav.answer(&demand, &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            answer.tuples,
            vec![vec!["Judge".to_string(), "Evaluator".to_string()]]
        );
        assert!(
            answer.bindings.is_empty(),
            "witness files are not a denotation: {:?}",
            answer.bindings
        );
        assert!(
            nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "expected a tree walk for a conceptual relation"
        );
        let docs = docs.lock().unwrap().clone();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].pred, "knows_about");
        assert_eq!(docs[0].args, ["Judge".to_string(), "Evaluator".to_string()]);
        assert_eq!(
            docs[0].verbalization,
            "component \"Judge\" knows about component \"Evaluator\""
        );
    }

    #[test]
    fn file_path_args_judge_document_without_walk() {
        let dir = scratch_dir("file-args");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "from b import x\n").unwrap();
        std::fs::write(dir.join("b.py"), "x = 1\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(OpenAll));
        let demand = Demand {
            pred: "includes".into(),
            args: vec![Arg::Bound("a.py".into()), Arg::Bound("b.py".into())],
            need: Need::Truth,
        };
        let answer = nav.answer(&demand, &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            answer.tuples,
            vec![vec!["a.py".to_string(), "b.py".to_string()]]
        );
        assert!(
            !nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "already-located file args should not walk"
        );
        assert!(nav
            .trace
            .iter()
            .any(|s| matches!(s, GroundStep::Document { path, .. } if path == "a.py")));
    }

    #[test]
    fn membership_generation_uses_bindings_not_declare_slots() {
        let dir = scratch_dir("membership");
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(dir.join("ui/app.py"), "print(1)\n").unwrap();
        let mut store = Store::new();
        store.bind("Frontend", "ui");
        let mut nav = TreeNavigator::new(&dir, Box::new(OpenAll));
        let demand = Demand {
            pred: "belongs_to".into(),
            args: vec![Arg::Free("f".into()), Arg::Bound("Frontend".into())],
            need: Need::Truth,
        };
        let answer = nav.answer(&demand, &store);
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            answer.tuples,
            vec![vec!["ui/app.py".to_string(), "Frontend".to_string()]]
        );
        assert!(
            !nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "member files come from bindings, not a walk"
        );
    }

    #[test]
    fn free_nary_atom_does_not_walk() {
        let dir = scratch_dir("free-nary");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.py"), "a\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(OpenAll));
        let answer = nav.answer(
            &Demand {
                pred: "includes".into(),
                args: vec![Arg::Free("f1".into()), Arg::Free("f2".into())],
                need: Need::Truth,
            },
            &Store::new(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert!(answer.tuples.is_empty());
        assert!(!answer.close);
        assert!(
            !nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "unbounded n-ary generation must not walk"
        );
    }

    #[test]
    fn ground_membership_is_path_prefix_not_a_document() {
        let dir = scratch_dir("member-test");
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::create_dir_all(dir.join("api")).unwrap();
        std::fs::write(dir.join("ui/app.py"), "from api import client\n").unwrap();
        std::fs::write(dir.join("api/client.py"), "x = 1\n").unwrap();
        let mut store = Store::new();
        store.bind("Frontend", "ui");
        store.bind("Backend", "api");
        let mut nav = TreeNavigator::new(&dir, Box::new(OpenAll));
        let yes = nav.answer(
            &Demand {
                pred: "belongs_to".into(),
                args: vec![
                    Arg::Bound("ui/app.py".into()),
                    Arg::Bound("Frontend".into()),
                ],
                need: Need::Truth,
            },
            &store,
        );
        let no = nav.answer(
            &Demand {
                pred: "belongs_to".into(),
                args: vec![Arg::Bound("ui/app.py".into()), Arg::Bound("Backend".into())],
                need: Need::Truth,
            },
            &store,
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            yes.tuples,
            vec![vec!["ui/app.py".to_string(), "Frontend".to_string()]]
        );
        assert!(
            no.tuples.is_empty(),
            "app.py is not under Backend: {:?}",
            no.tuples
        );
        assert!(
            !nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Document { .. })),
            "membership is not a document Choice"
        );
    }
}
