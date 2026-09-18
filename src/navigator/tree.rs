use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::edb::Store;
use crate::navigator::judge::{
    ChildChoice, ConflictAnswer, ConflictEvidence, ConflictState, Consistency, DocumentState,
    DocumentVerdict, Judge, ListingAtom, MultiListingState, ResolveValue, Transition,
    LISTING_CHOICE_BUDGET, TAU_CONFIDENCE,
};
use crate::navigator::trace::GroundStep;
use crate::navigator::walk::{pack_listing, parent_dir, read_trimmed, repo_join, ListingFilter};
use crate::navigator::{Answer, Arg, Demand, Navigator, Need};
use crate::syntax::{verbalize_atom, Declare, Term};

const MAX_LISTINGS: usize = 32;
const MAX_DEPTH: usize = 8;
const BODY_BUDGET: usize = 8192;

/// Goal-directed walk of a repo tree. Locate atoms share one listing request
/// (a Choice per atom per child). Document questions on the same file share
/// one request. Never evaluates IDB.
pub struct TreeNavigator {
    root: PathBuf,
    filter: ListingFilter,
    judge: Box<dyn Judge>,
    pub listings: Vec<String>,
    pub documents: Vec<String>,
    pub errors: Vec<String>,
    pub trace: Vec<GroundStep>,
    declarations: HashMap<String, Declare>,
    answered: HashMap<(String, Vec<Option<String>>, Need), Answer>,
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
            answered: HashMap::new(),
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

    fn cached(&self, demand: &Demand) -> Option<Answer> {
        self.answered
            .get(&(demand.pred.clone(), demand_pattern(demand), demand.need))
            .cloned()
    }

    fn remember(&mut self, demand: &Demand, answer: Answer) {
        self.answered.insert(
            (demand.pred.clone(), demand_pattern(demand), demand.need),
            answer,
        );
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
        self.finish_locate(demand, located)
    }

    fn finish_locate(&mut self, demand: &Demand, located: LocateResult) -> Answer {
        let pred = demand.pred.as_str();
        let source = format!("nav:{pred}");
        let any_free = demand.args.iter().any(|a| matches!(a, Arg::Free(_)));
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
                close: true,
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
        self.remember(demand, answer.clone());
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
        self.trace.push(GroundStep::Document {
            pred: pred.to_string(),
            args: args.to_vec(),
            path: file.to_string(),
            judgement: judgement.clone(),
            preview: preview_body(&body),
            note: judgement.note().into(),
            exchanges: self.judge.last_exchanges(),
        });
        if judgement.is_entailed() {
            Answer {
                tuples: vec![args.to_vec()],
                close: true,
                bindings: Vec::new(),
                source,
            }
        } else if judgement.is_contradicted() {
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
        self.locate_many(std::slice::from_ref(demand), store)
            .pop()
            .unwrap_or(LocateResult::Unknown)
    }

    fn locate_many(&mut self, demands: &[Demand], store: &Store) -> Vec<LocateResult> {
        if demands.is_empty() {
            return Vec::new();
        }
        let mut frontier: Vec<(String, usize)> = vec![("".to_string(), 0)];
        let mut already = HashSet::new();
        let mut listings = 0;
        let mut atoms: Vec<AtomWalk> = demands
            .iter()
            .map(|d| AtomWalk {
                args: bound_values(d),
                need: d.need,
                generating: d.args.iter().any(|a| matches!(a, Arg::Free(_))),
                hits: Vec::new(),
                refuted: false,
                polar: Vec::new(),
                skipped: HashSet::new(),
                all_skips_confident: true,
                aborted: false,
                looked: false,
                docs_decided: true,
            })
            .collect();

        while !frontier.is_empty() {
            if listings >= MAX_LISTINGS {
                abort_walk(&mut atoms);
                break;
            }
            if frontier.iter().all(|(_, depth)| *depth > MAX_DEPTH) {
                abort_walk(&mut atoms);
                break;
            }
            let packed = pack_listing(
                &self.root,
                &frontier,
                &self.filter,
                &already,
                LISTING_CHOICE_BUDGET,
                MAX_DEPTH,
            );
            listings += 1;
            self.listings.extend(packed.dir_depth.keys().cloned());
            if packed.depth_limited {
                abort_walk(&mut atoms);
            }
            if !packed.failed.is_empty() {
                abort_walk(&mut atoms);
            }
            if packed.children.is_empty() {
                if packed.unfinished.is_empty() || packed.unfinished == frontier {
                    break;
                }
                frontier = packed.unfinished;
                continue;
            }

            let asking: Vec<usize> = (0..atoms.len())
                .filter(|&i| {
                    packed
                        .children
                        .iter()
                        .any(|c| atom_asks(&atoms[i], c.rel_path.as_str()))
                })
                .collect();
            let listing_atoms: Vec<ListingAtom> = asking
                .iter()
                .map(|&i| ListingAtom {
                    pred: demands[i].pred.clone(),
                    args: atoms[i].args.clone(),
                    verbalization: self.verbalize_demand(&demands[i]),
                })
                .collect();
            let multi = MultiListingState {
                atoms: listing_atoms,
                dir: packed.origin.clone(),
                children: packed.children.clone(),
            };
            let choices_by_ask = if asking.is_empty() {
                Vec::new()
            } else {
                match self.judge.choose_many(&multi) {
                    Ok(c) => c,
                    Err(e) => {
                        self.errors.push(e.to_string());
                        abort_walk(&mut atoms);
                        frontier = packed.unfinished;
                        continue;
                    }
                }
            };
            for child in &packed.children {
                already.insert(child.rel_path.clone());
            }
            let exchanges = self.judge.last_exchanges();

            let mut open_by_atom: Vec<Vec<String>> = vec![Vec::new(); atoms.len()];
            let mut incomplete: Vec<(String, usize)> = Vec::new();
            let mut incomplete_seen = HashSet::new();

            for (ask_i, &atom_i) in asking.iter().enumerate() {
                let choices = choices_by_ask.get(ask_i).cloned().unwrap_or_default();
                let mut applied = Vec::new();
                let mut effects = Vec::new();
                for child in &packed.children {
                    let choice = choices
                        .iter()
                        .find(|c| c.rel_path == child.rel_path)
                        .cloned()
                        .unwrap_or(ChildChoice {
                            rel_path: child.rel_path.clone(),
                            transition: Transition::Skip,
                            confidence: 0.0,
                        });
                    if has_skipped_ancestor(&child.rel_path, &atoms[atom_i].skipped)
                        || atoms[atom_i].skipped.contains(&child.rel_path)
                    {
                        if choice.transition == Transition::Investigate {
                            effects.push(format!("{}: ignored (parent skip)", child.rel_path));
                        }
                        applied.push(choice);
                        continue;
                    }
                    match choice.transition {
                        Transition::Skip => {
                            if child.is_dir {
                                atoms[atom_i].skipped.insert(child.rel_path.clone());
                            }
                            if choice.confidence < TAU_CONFIDENCE && atoms[atom_i].generating {
                                atoms[atom_i].all_skips_confident = false;
                            }
                        }
                        Transition::Investigate => {
                            if child.is_dir {
                                if names_entity_from_directory(&demands[atom_i]) {
                                    atoms[atom_i].hits.push(child.rel_path.clone());
                                }
                                if !packed.complete.contains(&child.rel_path) {
                                    if incomplete_seen.insert(child.rel_path.clone()) {
                                        incomplete.push((
                                            child.rel_path.clone(),
                                            child_depth(&child.rel_path, &packed.dir_depth),
                                        ));
                                    }
                                    effects.push(format!("{}: investigate → list", child.rel_path));
                                } else {
                                    effects.push(format!("{}: investigate", child.rel_path));
                                }
                            } else {
                                open_by_atom[atom_i].push(child.rel_path.clone());
                                effects.push(format!("{}: investigate → open", child.rel_path));
                            }
                        }
                    }
                    applied.push(choice);
                }
                let step_exchanges = if ask_i == 0 {
                    exchanges.clone()
                } else {
                    Vec::new()
                };
                self.trace.push(GroundStep::Listing {
                    pred: demands[atom_i].pred.clone(),
                    args: atoms[atom_i].args.clone(),
                    dir: packed.origin.clone(),
                    children: packed.children.clone(),
                    choices: applied,
                    effects,
                    exchanges: step_exchanges,
                });
            }

            let mut next = Vec::new();
            let mut seen_next = HashSet::new();
            for (dir, depth) in packed.unfinished {
                if depth > MAX_DEPTH {
                    abort_walk(&mut atoms);
                    continue;
                }
                if atoms
                    .iter()
                    .all(|a| a.skipped.contains(&dir) || has_skipped_ancestor(&dir, &a.skipped))
                {
                    continue;
                }
                if seen_next.insert(dir.clone()) {
                    next.push((dir, depth));
                }
            }
            for (dir, depth) in incomplete {
                if depth > MAX_DEPTH {
                    abort_walk(&mut atoms);
                    continue;
                }
                if seen_next.insert(dir.clone()) {
                    next.push((dir, depth));
                }
            }

            let batch: Vec<(Demand, Vec<String>)> = demands
                .iter()
                .zip(open_by_atom)
                .map(|(d, files)| (d.clone(), files))
                .collect();
            let confirmed = self.confirm_files_batch(&batch, store);
            for (atom, c) in atoms.iter_mut().zip(confirmed) {
                atom.hits.extend(c.hits);
                atom.refuted |= c.refuted;
                atom.polar.extend(c.polar);
                atom.looked |= c.looked;
                if !c.decided {
                    atom.docs_decided = false;
                    atom.all_skips_confident = false;
                }
            }
            frontier = next;
            if atoms
                .iter()
                .all(|a| a.need == Need::Truth && !a.hits.is_empty() && frontier.is_empty())
            {
                break;
            }
            if frontier.is_empty() {
                break;
            }
        }

        let mut out = Vec::with_capacity(atoms.len());
        for (demand, atom) in demands.iter().zip(atoms) {
            let mut binding = tightest_wholes(atom.hits.clone());
            binding.sort();
            binding.dedup();
            let loc = if !atom.generating && polar_conflict(&atom.polar) {
                self.resolve_atom(demand, &atom, binding)
            } else {
                reduce_locate(
                    atom.generating,
                    atom.refuted,
                    atom.all_skips_confident,
                    atom.looked && atom.docs_decided && !atom.aborted,
                    binding,
                )
            };
            out.push(loc);
        }
        out
    }

    fn resolve_atom(
        &mut self,
        demand: &Demand,
        atom: &AtomWalk,
        binding: Vec<String>,
    ) -> LocateResult {
        let state = ConflictState {
            pred: demand.pred.clone(),
            args: bound_values(demand),
            verbalization: self.verbalize_demand(demand),
            evidence: atom.polar.clone(),
        };
        let answer = match self.judge.resolve_conflict(&state) {
            Ok(a) => a,
            Err(e) => {
                self.errors.push(e.to_string());
                ConflictAnswer {
                    value: ResolveValue::Unknown,
                    consistency: Consistency::Conflicting,
                    exchanges: Vec::new(),
                }
            }
        };
        self.trace.push(GroundStep::Conflict {
            pred: demand.pred.clone(),
            args: bound_values(demand),
            evidence: atom.polar.clone(),
            value: answer.value,
            consistency: answer.consistency,
            exchanges: answer.exchanges,
        });
        match answer.value {
            ResolveValue::True => LocateResult::Found(binding),
            ResolveValue::False => LocateResult::Absent,
            ResolveValue::Unknown => LocateResult::Unknown,
        }
    }

    /// Files flagged `investigate` in one listing. Same-path questions
    /// (several atoms, one file) share one request.
    fn confirm_files_batch(
        &mut self,
        per_atom: &[(Demand, Vec<String>)],
        store: &Store,
    ) -> Vec<ConfirmedFiles> {
        struct Prepared {
            atom: usize,
            rel: String,
            body: String,
            state: DocumentState,
        }
        let mut out: Vec<ConfirmedFiles> = (0..per_atom.len())
            .map(|_| ConfirmedFiles {
                hits: Vec::new(),
                refuted: false,
                polar: Vec::new(),
                decided: true,
                looked: false,
            })
            .collect();
        let mut prepared = Vec::new();
        for (atom, (demand, rels)) in per_atom.iter().enumerate() {
            for rel in rels {
                let (body, _) = match read_trimmed(&self.root, rel, BODY_BUDGET) {
                    Ok(v) => v,
                    Err(_) => {
                        out[atom].decided = false;
                        continue;
                    }
                };
                self.documents.push(rel.clone());
                let args = ground_demand(demand, rel);
                prepared.push(Prepared {
                    atom,
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
        }
        if prepared.is_empty() {
            return out;
        }
        let states: Vec<DocumentState> = prepared.iter().map(|p| p.state.clone()).collect();
        let answers = self.judge.documents(&states);
        for (prep, answer) in prepared.into_iter().zip(answers) {
            let a = match answer {
                Ok(a) => a,
                Err(e) => {
                    self.errors.push(e.to_string());
                    out[prep.atom].decided = false;
                    continue;
                }
            };
            if a.judgement.confidence < TAU_CONFIDENCE {
                out[prep.atom].decided = false;
            } else {
                out[prep.atom].looked = true;
            }
            if a.judgement.is_entailed() {
                out[prep.atom].hits.push(prep.rel.clone());
                out[prep.atom].polar.push(ConflictEvidence {
                    path: prep.rel.clone(),
                    verdict: DocumentVerdict::Entailed,
                    confidence: a.judgement.confidence,
                    excerpt: excerpt_body(&prep.body),
                });
            } else if a.judgement.is_contradicted() {
                out[prep.atom].refuted = true;
                out[prep.atom].polar.push(ConflictEvidence {
                    path: prep.rel.clone(),
                    verdict: DocumentVerdict::Contradicted,
                    confidence: a.judgement.confidence,
                    excerpt: excerpt_body(&prep.body),
                });
            }
            self.trace.push(GroundStep::Document {
                pred: prep.state.pred,
                args: prep.state.args,
                path: prep.rel,
                judgement: a.judgement.clone(),
                preview: preview_body(&prep.body),
                note: a.judgement.note().into(),
                exchanges: a.exchanges,
            });
        }
        out
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

fn excerpt_body(body: &str) -> String {
    body.lines()
        .take(4)
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

fn polar_conflict(polar: &[ConflictEvidence]) -> bool {
    let entailed = polar.iter().any(|e| e.verdict == DocumentVerdict::Entailed);
    let contradicted = polar
        .iter()
        .any(|e| e.verdict == DocumentVerdict::Contradicted);
    entailed && contradicted
}

struct ConfirmedFiles {
    hits: Vec<String>,
    refuted: bool,
    polar: Vec<ConflictEvidence>,
    decided: bool,
    looked: bool,
}

enum LocateResult {
    Found(Vec<String>),
    Unknown,
    Absent,
}

struct AtomWalk {
    args: Vec<String>,
    need: Need,
    generating: bool,
    hits: Vec<String>,
    refuted: bool,
    polar: Vec<ConflictEvidence>,
    skipped: HashSet<String>,
    all_skips_confident: bool,
    aborted: bool,
    looked: bool,
    docs_decided: bool,
}

fn abort_walk(atoms: &mut [AtomWalk]) {
    for atom in atoms {
        atom.aborted = true;
        atom.all_skips_confident = false;
    }
}

fn reduce_locate(
    generating: bool,
    refuted: bool,
    generating_complete: bool,
    ground_complete: bool,
    binding: Vec<String>,
) -> LocateResult {
    let entailed = !binding.is_empty();
    if generating {
        if entailed {
            LocateResult::Found(binding)
        } else if generating_complete {
            LocateResult::Absent
        } else {
            LocateResult::Unknown
        }
    } else if entailed && refuted {
        LocateResult::Unknown
    } else if entailed {
        LocateResult::Found(binding)
    } else if refuted || ground_complete {
        LocateResult::Absent
    } else {
        LocateResult::Unknown
    }
}

fn atom_asks(atom: &AtomWalk, rel: &str) -> bool {
    !atom.skipped.contains(rel) && !has_skipped_ancestor(rel, &atom.skipped)
}

/// Investigated directories name a free argument (`api/` → `"api"`).
fn names_entity_from_directory(demand: &Demand) -> bool {
    demand.args.iter().any(|a| matches!(a, Arg::Free(_)))
        && demand.args.iter().all(|a| matches!(a, Arg::Free(_)))
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

fn has_skipped_ancestor(rel: &str, skipped: &HashSet<String>) -> bool {
    let mut path = rel;
    while let Some(parent) = parent_dir(path) {
        if skipped.contains(parent) {
            return true;
        }
        path = parent;
    }
    false
}

fn child_depth(rel: &str, dir_depth: &HashMap<String, usize>) -> usize {
    match parent_dir(rel) {
        Some(p) => dir_depth.get(p).copied().unwrap_or(0) + 1,
        None => dir_depth.get("").copied().unwrap_or(0) + 1,
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

fn walks_tree(demand: &Demand, root: &Path) -> bool {
    if matches!(demand.pred.as_str(), "file" | "File") {
        return false;
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
        if args.iter().any(|a| repo_join(root, a).is_file()) {
            return false;
        }
        return true;
    }
    if demand.args.iter().any(|a| matches!(a, Arg::Bound(_))) {
        return true;
    }
    demand.args.len() <= 1
}

impl Navigator for TreeNavigator {
    fn prepare(&mut self, compiled: &crate::core::Compiled) {
        self.declarations = compiled.declarations.clone();
    }

    fn answer(&mut self, demand: &Demand, store: &Store) -> Answer {
        if let Some(cached) = self.cached(demand) {
            return cached;
        }
        let answer = self.answer_fresh(demand, store);
        self.remember(demand, answer.clone());
        answer
    }

    fn answer_many(&mut self, demands: &[Demand], store: &Store) -> Vec<Answer> {
        if demands.len() <= 1 {
            return demands.iter().map(|d| self.answer(d, store)).collect();
        }
        let mut walk = Vec::new();
        let mut rest = Vec::new();
        for (i, d) in demands.iter().enumerate() {
            if self.cached(d).is_some() {
                rest.push(i);
            } else if walks_tree(d, &self.root) {
                walk.push(i);
            } else {
                rest.push(i);
            }
        }
        let mut out: Vec<Option<Answer>> = vec![None; demands.len()];
        for i in rest {
            out[i] = Some(self.answer(&demands[i], store));
        }
        if walk.len() == 1 {
            let i = walk[0];
            out[i] = Some(self.answer(&demands[i], store));
        } else if walk.len() > 1 {
            let walk_demands: Vec<Demand> = walk.iter().map(|&i| demands[i].clone()).collect();
            let located = self.locate_many(&walk_demands, store);
            for (&i, loc) in walk.iter().zip(located) {
                out[i] = Some(self.finish_locate(&demands[i], loc));
            }
        }
        out.into_iter()
            .map(|a| a.expect("every demand answered"))
            .collect()
    }
}

impl TreeNavigator {
    fn answer_fresh(&mut self, demand: &Demand, store: &Store) -> Answer {
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
        ChildChoice, ConflictAnswer, Consistency, DocumentAnswer, DocumentJudgement,
        DocumentVerdict, JudgeError, ListingState, MultiListingState, ResolveValue,
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
                verdict: DocumentVerdict::Entailed,
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
        assert!(nav
            .trace
            .iter()
            .any(|s| matches!(s, GroundStep::Document { path, .. } if path == "ui/app.py")));
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

    struct CaptureListings {
        states: Arc<Mutex<Vec<ListingState>>>,
    }

    impl Judge for CaptureListings {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            self.states.lock().unwrap().push(state.clone());
            OpenAll.choose(state)
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            OpenAll.document(state)
        }
    }

    #[test]
    fn locate_packs_nested_listings_into_one_choose() {
        let states = Arc::new(Mutex::new(Vec::new()));
        let mut nav = TreeNavigator::new(
            "examples/toy-split",
            Box::new(CaptureListings {
                states: states.clone(),
            }),
        );
        let demand = Demand {
            pred: "component".into(),
            args: vec![Arg::Bound("Frontend".into())],
            need: Need::Truth,
        };
        let _ = nav.answer(&demand, &Store::new());
        let states = states.lock().unwrap().clone();
        assert_eq!(
            states.len(),
            1,
            "expected one packed listing, got {}",
            states.len()
        );
        let paths: Vec<&str> = states[0]
            .children
            .iter()
            .map(|c| c.rel_path.as_str())
            .collect();
        assert!(paths.contains(&"ui"), "{paths:?}");
        assert!(paths.contains(&"ui/app.py"), "{paths:?}");
        assert!(paths.contains(&"api"), "{paths:?}");
        assert!(paths.contains(&"api/client.py"), "{paths:?}");
        let listings: Vec<_> = nav
            .trace
            .iter()
            .filter(|s| matches!(s, GroundStep::Listing { .. }))
            .collect();
        assert_eq!(listings.len(), 1);
    }

    struct CaptureChooseMany {
        n_atoms: Arc<Mutex<Vec<usize>>>,
    }

    impl Judge for CaptureChooseMany {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn choose_many(
            &mut self,
            state: &MultiListingState,
        ) -> Result<Vec<Vec<ChildChoice>>, JudgeError> {
            self.n_atoms.lock().unwrap().push(state.atoms.len());
            state
                .atoms
                .iter()
                .map(|atom| self.choose(&state.listing_state(atom)))
                .collect()
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            OpenAll.document(state)
        }
    }

    #[test]
    fn two_locate_atoms_share_one_listing() {
        let n_atoms = Arc::new(Mutex::new(Vec::new()));
        let mut nav = TreeNavigator::new(
            "examples/toy-split",
            Box::new(CaptureChooseMany {
                n_atoms: n_atoms.clone(),
            }),
        );
        let demands = vec![
            Demand {
                pred: "component".into(),
                args: vec![Arg::Bound("Frontend".into())],
                need: Need::Truth,
            },
            Demand {
                pred: "component".into(),
                args: vec![Arg::Bound("Backend".into())],
                need: Need::Truth,
            },
        ];
        let answers = nav.answer_many(&demands, &Store::new());
        assert_eq!(answers.len(), 2);
        assert!(!answers[0].tuples.is_empty());
        assert!(!answers[1].tuples.is_empty());
        let n = n_atoms.lock().unwrap().clone();
        assert!(
            n.iter().any(|&k| k == 2),
            "expected a shared listing of both atoms, got {n:?}"
        );
        assert!(
            n.iter().all(|&k| k == 2),
            "follow-up listings should keep both atoms, got {n:?}"
        );
    }

    struct SkipDecoyInvestigateRest;

    impl Judge for SkipDecoyInvestigateRest {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            Ok(state
                .children
                .iter()
                .map(|c| ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition: if c.rel_path == "decoy" {
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
                verdict: DocumentVerdict::Entailed,
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn skip_on_parent_ignores_packed_descendants() {
        let mut nav = TreeNavigator::new("examples/toy-split", Box::new(SkipDecoyInvestigateRest));
        let demand = Demand {
            pred: "component".into(),
            args: vec![Arg::Bound("Frontend".into())],
            need: Need::Truth,
        };
        let _ = nav.answer(&demand, &Store::new());
        let docs: Vec<_> = nav
            .trace
            .iter()
            .filter_map(|s| match s {
                GroundStep::Document { path, .. } => Some(path.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            docs.iter().all(|p| !p.contains("decoy")),
            "skipped decoy/ must not open packed descendants: {docs:?}"
        );
        assert!(
            nav.trace.iter().any(|s| match s {
                GroundStep::Listing { effects, .. } => effects
                    .iter()
                    .any(|e| e.contains("decoy/noise.py") && e.contains("parent skip")),
                _ => false,
            }),
            "expected parent-skip effect on decoy/noise.py"
        );
    }

    struct InvestigateAllUnknownDocs;

    impl Judge for InvestigateAllUnknownDocs {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::Neutral,
                confidence: 0.9,
            })
        }
    }

    fn nullary(pred: &str) -> Demand {
        Demand {
            pred: pred.into(),
            args: vec![],
            need: Need::Truth,
        }
    }

    #[test]
    fn nullary_atom_is_not_proved_by_investigating_a_directory() {
        let mut nav =
            TreeNavigator::new("examples/toy-layered", Box::new(InvestigateAllUnknownDocs));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        assert!(
            answer.tuples.is_empty(),
            "investigating a directory is not a witness for a ground atom: {:?}",
            answer.tuples
        );
        assert!(
            answer.close,
            "complete locate with no entailed witness is closed-world false"
        );
    }

    struct InvestigateAllFalseDocs;

    impl Judge for InvestigateAllFalseDocs {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::Contradicted,
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn complete_locate_with_only_contradicted_documents_is_false() {
        let mut nav = TreeNavigator::new("examples/toy-layered", Box::new(InvestigateAllFalseDocs));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        assert!(answer.tuples.is_empty());
        assert!(answer.close);
    }

    struct SkipAllLowConfidence;

    impl Judge for SkipAllLowConfidence {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            Ok(state
                .children
                .iter()
                .map(|c| ChildChoice {
                    rel_path: c.rel_path.clone(),
                    transition: Transition::Skip,
                    confidence: 0.2,
                })
                .collect())
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::Neutral,
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn incomplete_locate_does_not_close_as_false() {
        let mut nav = TreeNavigator::new("examples/toy-layered", Box::new(SkipAllLowConfidence));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        assert!(answer.tuples.is_empty());
        assert!(!answer.close);
    }

    struct NeutralFilesSkipDirsLowConf;

    impl Judge for NeutralFilesSkipDirsLowConf {
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
                    confidence: if c.is_dir { 0.2 } else { 0.9 },
                })
                .collect())
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::Neutral,
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn ground_atom_closes_false_after_neutral_documents_despite_low_skips() {
        let dir = scratch_dir("cwa-neutrals");
        std::fs::create_dir_all(dir.join("extra")).unwrap();
        std::fs::write(dir.join("a.py"), "hello\n").unwrap();
        std::fs::write(dir.join("extra/b.py"), "there\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(NeutralFilesSkipDirsLowConf));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(answer.tuples.is_empty());
        assert!(
            answer.close,
            "looked at files, only neutrals: low-confidence skips must not keep a ground atom open: {:?}",
            answer
        );
    }

    struct ContradictFilesSkipRest;

    impl Judge for ContradictFilesSkipRest {
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
                    confidence: if c.is_dir { 0.2 } else { 0.9 },
                })
                .collect())
        }

        fn document(&mut self, _state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            Ok(DocumentJudgement {
                verdict: DocumentVerdict::Contradicted,
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn a_contradicted_document_falsifies_even_if_the_walk_is_open() {
        let dir = scratch_dir("refute-open");
        std::fs::create_dir_all(dir.join("extra")).unwrap();
        std::fs::write(dir.join("a.py"), "no gpu\n").unwrap();
        std::fs::write(dir.join("extra/b.py"), "also no\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(ContradictFilesSkipRest));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(answer.tuples.is_empty());
        assert!(
            answer.close,
            "a confident refutation is as strong as a witness: {:?}",
            answer
        );
    }

    struct MixedDocs;

    impl Judge for MixedDocs {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            OpenAll.choose(state)
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            let true_file = state.path.ends_with("yes.py");
            Ok(DocumentJudgement {
                verdict: if true_file {
                    DocumentVerdict::Entailed
                } else {
                    DocumentVerdict::Contradicted
                },
                confidence: 0.9,
            })
        }
    }

    #[test]
    fn entailed_and_contradicted_documents_conflict() {
        let dir = scratch_dir("conflict");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("yes.py"), "gpu compiler\n").unwrap();
        std::fs::write(dir.join("no.py"), "cpu only\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(MixedDocs));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert!(answer.tuples.is_empty());
        assert!(
            !answer.close,
            "default resolve leaves a conflict unknown: {:?}",
            answer
        );
        assert!(
            nav.trace.iter().any(|s| matches!(
                s,
                GroundStep::Conflict {
                    consistency: Consistency::Conflicting,
                    value: ResolveValue::Unknown,
                    ..
                }
            )),
            "mixed polar documents should surface a conflict step"
        );
    }

    struct ResolveConflictTrue;

    impl Judge for ResolveConflictTrue {
        fn choose(&mut self, state: &ListingState) -> Result<Vec<ChildChoice>, JudgeError> {
            MixedDocs.choose(state)
        }

        fn document(&mut self, state: &DocumentState) -> Result<DocumentJudgement, JudgeError> {
            MixedDocs.document(state)
        }

        fn resolve_conflict(
            &mut self,
            _state: &ConflictState,
        ) -> Result<ConflictAnswer, JudgeError> {
            Ok(ConflictAnswer {
                value: ResolveValue::True,
                consistency: Consistency::Conflicting,
                exchanges: Vec::new(),
            })
        }
    }

    #[test]
    fn conflict_resolve_can_accept_the_entailed_side() {
        let dir = scratch_dir("conflict-true");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("yes.py"), "gpu compiler\n").unwrap();
        std::fs::write(dir.join("no.py"), "cpu only\n").unwrap();
        let mut nav = TreeNavigator::new(&dir, Box::new(ResolveConflictTrue));
        let answer = nav.answer(&nullary("GPUCompiler"), &Store::new());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(answer.tuples, vec![Vec::<String>::new()]);
        assert!(answer.close);
        assert!(nav.trace.iter().any(|s| matches!(
            s,
            GroundStep::Conflict {
                value: ResolveValue::True,
                consistency: Consistency::Conflicting,
                ..
            }
        )));
    }

    #[test]
    fn free_unary_still_binds_investigated_directories() {
        let mut nav =
            TreeNavigator::new("examples/toy-layered", Box::new(InvestigateAllUnknownDocs));
        let answer = nav.answer(
            &Demand {
                pred: "component".into(),
                args: vec![Arg::Free("c".into())],
                need: Need::Truth,
            },
            &Store::new(),
        );
        assert!(
            !answer.tuples.is_empty(),
            "free unary locate takes names from investigated directories: {:?}",
            answer.tuples
        );
        assert!(answer.close);
    }

    #[test]
    fn nullary_atom_is_proved_by_a_true_document() {
        let mut nav = TreeNavigator::new("examples/toy-layered", Box::new(OpenAll));
        let answer = nav.answer(
            &Demand {
                pred: "CLI".into(),
                args: vec![],
                need: Need::Truth,
            },
            &Store::new(),
        );
        assert_eq!(answer.tuples, vec![Vec::<String>::new()]);
        assert!(answer.close);
    }
}
