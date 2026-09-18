use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::edb::Store;
use crate::navigator::judge::{
    ChildChoice, DocumentState, Judge, ListingState, Transition, TAU_CONFIDENCE,
};
use crate::navigator::trace::GroundStep;
use crate::navigator::walk::{list_dir, read_trimmed, repo_join, ListingFilter};
use crate::navigator::{Answer, Arg, Demand, Navigator, Need};
use crate::syntax::{verbalize_atom, Declare, Term};

const MAX_LISTINGS: usize = 32;
const MAX_DEPTH: usize = 8;
const BODY_BUDGET: usize = 8192;

/// Goal-directed walk of a repo tree. Asks a `Judge` at each listing and
/// document; never evaluates IDB.
pub struct TreeNavigator {
    root: PathBuf,
    filter: ListingFilter,
    judge: Box<dyn Judge>,
    pub listings: Vec<String>,
    pub documents: Vec<String>,
    pub errors: Vec<String>,
    pub trace: Vec<GroundStep>,
    declarations: HashMap<String, Declare>,
}

impl TreeNavigator {
    pub fn new(root: impl Into<PathBuf>, judge: Box<dyn Judge>) -> Self {
        let root = root.into();
        let filter = ListingFilter::from_repo(&root);
        Self {
            root,
            filter,
            judge,
            listings: Vec::new(),
            documents: Vec::new(),
            errors: Vec::new(),
            trace: Vec::new(),
            declarations: HashMap::new(),
        }
    }

    pub fn hide_model(&mut self, model: &Path) {
        self.filter.set_model(&self.root, model);
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
        let source = format!("nav:{pred}");
        let any_free = demand.args.iter().any(|a| matches!(a, Arg::Free(_)));
        if any_free {
            let matching = facts_matching(store, demand);
            if store.pattern_closed(pred, &demand_pattern(demand))
                || (demand.args.len() == 1 && !matching.is_empty())
            {
                return Answer {
                    tuples: matching,
                    close: true,
                    bindings: Vec::new(),
                    source,
                };
            }
        }
        let located = self.locate(demand, store);
        let bound = bound_values(demand);
        let unary = demand.args.len() == 1;
        let answer = match located {
            LocateResult::Found(paths) => {
                if unary && any_free {
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
                    let label = bound[0].clone();
                    Answer {
                        tuples: vec![vec![label.clone()]],
                        close: true,
                        bindings: paths.iter().map(|p| (label.clone(), p.clone())).collect(),
                        source: source.clone(),
                    }
                } else if any_free {
                    let tuples = paths.iter().map(|p| ground_demand(demand, p)).collect();
                    Answer {
                        tuples,
                        close: true,
                        bindings: Vec::new(),
                        source: source.clone(),
                    }
                } else {
                    Answer {
                        tuples: vec![bound.clone()],
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
            args: bound,
            status: status.into(),
            bindings: answer.bindings.clone(),
        });
        answer
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

    fn verbalize_demand(&self, demand: &Demand) -> String {
        let terms: Vec<Term> = demand
            .args
            .iter()
            .map(|a| match a {
                Arg::Bound(s) => Term::Const(s.clone()),
                Arg::Free(v) => Term::Var(v.clone()),
            })
            .collect();
        verbalize_atom(&demand.pred, &terms, &self.declarations)
    }

    fn locate(&mut self, demand: &Demand, store: &Store) -> LocateResult {
        let pred = demand.pred.as_str();
        let args = bound_values(demand);
        let need = demand.need;
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
            let children = match list_dir(&self.root, &dir, &self.filter) {
                Ok(c) => c,
                Err(_) => {
                    all_skips_confident = false;
                    continue;
                }
            };
            self.listings.push(dir.clone());
            listings += 1;
            let verbalization = self.verbalize_demand(demand);
            let choices = match self.judge.choose(&ListingState {
                pred: pred.to_string(),
                args: args.clone(),
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
                let choice = choices
                    .iter()
                    .find(|c| c.rel_path == child.rel_path)
                    .cloned()
                    .unwrap_or(ChildChoice {
                        rel_path: child.rel_path.clone(),
                        transition: Transition::Skip,
                        confidence: 0.0,
                    });
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
            hits.extend(self.confirm_files(demand, &open_files, store));
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
        demand: &Demand,
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
            let args = ground_demand(demand, rel);
            prepared.push(Prepared {
                rel: rel.clone(),
                body: body.clone(),
                state: DocumentState {
                    pred: demand.pred.clone(),
                    args: args.clone(),
                    path: rel.clone(),
                    body,
                    bindings: store
                        .all_bindings()
                        .into_iter()
                        .map(|b| (b.constant, b.entity))
                        .collect(),
                    verbalization: self.verbalize(&demand.pred, &args),
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
                pred: demand.pred.clone(),
                args: prep.state.args.clone(),
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

fn bound_values(demand: &Demand) -> Vec<String> {
    demand
        .args
        .iter()
        .filter_map(|a| match a {
            Arg::Bound(v) => Some(v.clone()),
            Arg::Free(_) => None,
        })
        .collect()
}

fn demand_pattern(demand: &Demand) -> Vec<Option<String>> {
    demand
        .args
        .iter()
        .map(|a| match a {
            Arg::Bound(v) => Some(v.clone()),
            Arg::Free(_) => None,
        })
        .collect()
}

fn ground_demand(demand: &Demand, file: &str) -> Vec<String> {
    demand
        .args
        .iter()
        .map(|a| match a {
            Arg::Bound(v) => v.clone(),
            Arg::Free(_) => file.to_string(),
        })
        .collect()
}

fn facts_matching(store: &Store, demand: &Demand) -> Vec<Vec<String>> {
    let n = demand.args.len();
    let mut rows: Vec<Vec<String>> = store
        .facts_for(&demand.pred)
        .into_iter()
        .filter(|row| {
            row.len() == n
                && demand.args.iter().zip(row.iter()).all(|(a, cell)| match a {
                    Arg::Bound(v) => v == cell,
                    Arg::Free(_) => true,
                })
        })
        .collect();
    rows.sort();
    rows
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
                return self.answer_document(&demand.pred, &args, file_i, store);
            }
            return self.answer_locate(demand, store);
        }

        if demand.args.iter().any(|a| matches!(a, Arg::Bound(_))) {
            return self.answer_locate(demand, store);
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
                    transition: Transition::Investigate,
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
    fn mixed_nary_walks_and_fills_free_slots_from_witnesses() {
        let dir = scratch_dir("membership");
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(dir.join("ui/app.py"), "print(1)\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(OpenAll));
        let demand = Demand {
            pred: "belongs_to".into(),
            args: vec![Arg::Free("f".into()), Arg::Bound("Frontend".into())],
            need: Need::Truth,
        };
        let answer = nav.answer(&demand, &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            answer.tuples,
            vec![vec!["ui/app.py".to_string(), "Frontend".to_string()]]
        );
        assert!(
            nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "mixed n-ary is a walk, not a binding-table listing"
        );
        assert!(nav.trace.iter().any(
            |s| matches!(s, GroundStep::Document { path, .. } if path == "ui/app.py")
        ));
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
    fn ground_atom_with_a_file_arg_is_a_document() {
        let dir = scratch_dir("member-test");
        std::fs::create_dir_all(dir.join("ui")).unwrap();
        std::fs::write(dir.join("ui/app.py"), "from api import client\n").unwrap();
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
            &Store::new(),
        );
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(
            yes.tuples,
            vec![vec!["ui/app.py".to_string(), "Frontend".to_string()]]
        );
        assert!(
            nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Document { path, .. } if path == "ui/app.py")),
            "ground belongs_to is a document Choice"
        );
        assert!(
            !nav.trace
                .iter()
                .any(|s| matches!(s, GroundStep::Listing { .. })),
            "already-located file args should not walk"
        );
    }
}
