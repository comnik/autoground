//! (Re-)check all claims in a brief.
//!
//! A witness can cite one or more paths. Paths are relative to the brief. $ in place of a
//! path is bound to a specific file `check` is called on at runtime.
//!
//! A claim can be supported directly by a witness, or indirectly through other claims.
//!
//! A witness larger than the body budget is sliced into parts. The claim is discharged when
//! any one part entails the sentence. If no part is sufficient on its own, then the user is
//! forced to split the file into more coherent pieces. An image witness is one PNG or JPEG
//! and is never broken into parts. A witness may mix text and images; the text still has to
//! fit in one state. A witness that cites a `.brief` is rejected. A brief is a model, not
//! evidence.
//!
//! Each claim is only judged once, no matter how many other claims reference it.
//!
//! `and`, `or`, and `not` are evaluated deterministically.

pub mod strategy;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Value as Json};

use crate::oracle::image::{image_asset, image_extension, Asset, ImageError, MAX_IMAGE_BYTES};
use crate::oracle::{Answer, Classification, Oracle, OracleError, Ticket};
use crate::syntax::{parse_dollar_witness, Proposition, Sentence, Support};
use crate::universe::{Filesystem, GivenFile, Universe, UniverseError};
use crate::value::Value;

use strategy::DefaultStrategy;

/// Largest body sent in one request.
pub const DEFAULT_BODY_BYTES: usize = 128 * 1024;

/// A single file larger than this is too large to slice.
///
/// Sixty-four parts at the default body budget. Each part is its own request,
/// and the run cap is one cent.
pub(crate) const MAX_SLICED_BYTES: usize = DEFAULT_BODY_BYTES.saturating_mul(64);

const IMPLICATION_TEXT: &str = "sentences entail this claim";

/// Why a step did not discharge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// A sentence with no witness and no detail anywhere in the brief.
    Unpinned,
    /// A cited path does not resolve to a file.
    MissingFile { path: String },
    /// The cited bodies do not fit in one state, and this witness cannot be sliced.
    TooLarge,
    /// Neutral, or a polarity under the trust bar.
    NotEntailed,
    /// The files, or the body sentences, contradict the sentence at the bar.
    Contradicted,
    /// A witness cites a `.brief` file. A brief is a model, not evidence.
    BriefWitness,
    /// The cited image is not a PNG or JPEG this check can send.
    UnsupportedImage,
    /// The cited image is under the size the model accepts.
    ImageTooSmall,
    /// The cited image is over the size the model accepts.
    ImageTooLarge,
    /// A required `certify` or `ground` certificate failed.
    CertificateFailed { cert: String },
}

impl Failure {
    pub fn label(&self) -> String {
        match self {
            Failure::Unpinned => "unpinned".to_string(),
            Failure::MissingFile { path } => format!("missing file {path}"),
            Failure::TooLarge => "too large".to_string(),
            Failure::NotEntailed => "not entailed".to_string(),
            Failure::Contradicted => "contradicted".to_string(),
            Failure::BriefWitness => "briefs can't be witnesses".to_string(),
            Failure::UnsupportedImage => "unsupported image".to_string(),
            Failure::ImageTooSmall => "image too small".to_string(),
            Failure::ImageTooLarge => "image too large".to_string(),
            Failure::CertificateFailed { cert } => format!("certificate failed: {cert}"),
        }
    }
}

/// One node of the checked proof.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub text: String,
    pub value: Value,
    pub failure: Option<Failure>,
    /// Paths, or part ids, that this judgement cites.
    pub cites: Vec<String>,
    pub children: Vec<Node>,
    /// The body ⇒ head judgement of an IDB claim, printed after [`Node::children`].
    pub implication: Option<Box<Node>>,
    /// The sentence was assumed. Check did not compare it to a file.
    pub assumed: bool,
}

/// A checked brief.
#[derive(Debug, Clone, PartialEq)]
pub struct Report {
    pub claims: Vec<Node>,
}

#[derive(Debug)]
pub enum CheckError {
    Oracle(OracleError),
    Io(String),
    /// `$` was used without a file, or a file was given to a brief with no `$`.
    Bind(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Oracle(err) => write!(f, "{err}"),
            CheckError::Io(err) | CheckError::Bind(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CheckError {}

impl From<OracleError> for CheckError {
    fn from(err: OracleError) -> Self {
        CheckError::Oracle(err)
    }
}

/// Files check opens. `tree` is the brief's directory. `given` is `$`. `named` maps `$name`.
struct Sources<'a> {
    tree: &'a Filesystem,
    given: Option<&'a GivenFile>,
    named: &'a HashMap<String, PathBuf>,
}

/// The file every `(witness $)` reads.
#[derive(Debug, Clone)]
pub struct BoundFile {
    /// Path named in the oracle state and in the proof's cites.
    pub cite: String,
    /// Opened as given. An absolute path is this file, not a missing witness.
    pub path: PathBuf,
}

/// Runtime witness bindings for `$`, `$name`, and `$name/rel` paths.
#[derive(Debug, Clone, Default)]
pub struct BoundWitnesses {
    pub default: Option<BoundFile>,
    pub named: HashMap<String, PathBuf>,
}

struct PreparedCert {
    cert_ids: Vec<String>,
    work: Vec<Work>,
    defs: HashSet<String>,
}

/// Tickets enqueued for one brief, in the order [`Prepared::settle`] reads answers.
pub struct Prepared {
    work: Vec<Work>,
    defs: HashSet<String>,
    imported_certs: Vec<PreparedCert>,
    tickets: Vec<Ticket>,
}

impl Prepared {
    pub fn tickets(&self) -> &[Ticket] {
        &self.tickets
    }

    /// `answers` follows [`Prepared::tickets`]. An oracle error fails the report.
    pub fn settle(self, answers: &[Result<Answer, OracleError>]) -> Result<Report, CheckError> {
        let mut cursor = 0;
        let mut cert_seed: HashMap<String, Value> = HashMap::new();
        let mut cert_roots: HashMap<String, String> = HashMap::new();
        for cert in self.imported_certs {
            let mut cert_judged = Vec::with_capacity(cert.work.len());
            for item in cert.work {
                cert_judged.push(settle(item, answers, &mut cursor)?);
            }
            let cert_lemmas = lemma_values_with_seed(&cert_judged, &cert.defs, &cert_seed);
            record_cert_failures(&cert_judged, &cert_lemmas, &mut cert_roots);
            for cert_id in cert.cert_ids {
                let value = cert_lemmas
                    .get(&cert_id)
                    .copied()
                    .unwrap_or(Value::Unknown);
                cert_seed.insert(cert_id, value);
            }
        }
        let mut judged = Vec::with_capacity(self.work.len());
        for item in self.work {
            judged.push(settle(item, answers, &mut cursor)?);
        }
        let lemmas = lemma_values_with_seed(&judged, &self.defs, &cert_seed);
        record_cert_failures(&judged, &lemmas, &mut cert_roots);
        let claims = judged
            .iter()
            .map(|node| render(node, &lemmas, &cert_roots))
            .collect();
        Ok(Report { claims })
    }
}

/// Whether any witness path is the symbol `$`.
pub fn cites_dollar(claims: &[Proposition]) -> bool {
    claims.iter().any(proposition_cites_dollar)
}

fn proposition_cites_dollar(proposition: &Proposition) -> bool {
    match proposition {
        Proposition::And(xs) | Proposition::Or(xs) => xs.iter().any(proposition_cites_dollar),
        Proposition::Not(inner) => proposition_cites_dollar(inner),
        Proposition::Defined { body, .. } => proposition_cites_dollar(body),
        Proposition::Claim { support, .. } => match support {
            Support::Witness { paths } => paths.iter().any(|path| path == "$"),
            Support::Sentence(inner) => proposition_cites_dollar(inner),
        },
        Proposition::Cert {
            support, imported, ..
        } => {
            imported.iter().any(proposition_cites_dollar)
                || match support {
                    Support::Witness { paths } => paths.iter().any(|path| path == "$"),
                    Support::Sentence(inner) => proposition_cites_dollar(inner),
                }
        }
        Proposition::Grounded {
            witness,
            certificate,
            ..
        } => {
            witness.iter().any(|path| path == "$")
                || certificate.iter().any(proposition_cites_dollar)
        }
        Proposition::Scope { body, .. } => body.iter().any(proposition_cites_dollar),
        Proposition::Sentence(_) | Proposition::Assume(_) => false,
    }
}

/// Named `$name` witness identifiers cited anywhere in `claims` (excluding bare `$`).
pub fn cited_named_dollars(claims: &[Proposition]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for claim in claims {
        collect_named_dollars(claim, &mut out);
    }
    out
}

fn collect_named_dollars(proposition: &Proposition, out: &mut BTreeSet<String>) {
    let record = |paths: &[String], out: &mut BTreeSet<String>| {
        for path in paths {
            if let Some((name, _)) = parse_dollar_witness(path) {
                if !name.is_empty() {
                    out.insert(name.to_string());
                }
            }
        }
    };
    match proposition {
        Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
            for x in xs {
                collect_named_dollars(x, out);
            }
        }
        Proposition::Not(inner) | Proposition::Defined { body: inner, .. } => {
            collect_named_dollars(inner, out);
        }
        Proposition::Claim { support, .. } => match support {
            Support::Witness { paths } => record(paths, out),
            Support::Sentence(inner) => collect_named_dollars(inner, out),
        },
        Proposition::Cert {
            support, imported, ..
        } => {
            for item in imported {
                collect_named_dollars(item, out);
            }
            match support {
                Support::Witness { paths } => record(paths, out),
                Support::Sentence(inner) => collect_named_dollars(inner, out),
            }
        }
        Proposition::Grounded {
            witness,
            certificate,
            ..
        } => {
            record(witness, out);
            for item in certificate {
                collect_named_dollars(item, out);
            }
        }
        Proposition::Sentence(_) | Proposition::Assume(_) => {}
    }
}

/// Enqueue every ticket for `claims`. Does not wait.
///
/// A brief that cites `$` needs `bound`. A `bound` file needs a brief that cites `$`.
/// Either mismatch is [`CheckError::Bind`] and submits nothing.
pub fn submit<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    budget: usize,
    bound: Option<&BoundFile>,
    oracle: &mut O,
) -> Result<Prepared, CheckError> {
    let bound = BoundWitnesses {
        default: bound.cloned(),
        named: HashMap::new(),
    };
    submit_with(claims, base, budget, &bound, oracle)
}

/// [`submit`], with bindings for `$`, `$name`, and `$name/rel` witnesses.
pub fn submit_with<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    budget: usize,
    bound: &BoundWitnesses,
    oracle: &mut O,
) -> Result<Prepared, CheckError> {
    match (cites_dollar(claims), &bound.default) {
        (true, None) => return Err(CheckError::Bind("`$` needs a file".into())),
        (false, Some(_)) => {
            return Err(CheckError::Bind("a file needs `(witness $)`".into()));
        }
        _ => {}
    }
    for name in cited_named_dollars(claims) {
        if !bound.named.contains_key(&name) {
            return Err(CheckError::Bind(format!("`${name}` needs a binding")));
        }
    }
    let defs = defined_heads(claims);
    let tree = Filesystem::at(base);
    let given = bound
        .default
        .as_ref()
        .map(|file| GivenFile::new(file.path.clone()));
    let sources = Sources {
        tree: &tree,
        given: given.as_ref(),
        named: &bound.named,
    };
    let mut imported_certs = Vec::new();
    let mut seen_certs = HashSet::new();
    let mut work = Vec::with_capacity(claims.len());
    for claim in claims {
        work.push(prepare(
            claim,
            &defs,
            &sources,
            bound,
            budget,
            oracle,
            &mut imported_certs,
            &mut seen_certs,
        )?);
    }
    let mut tickets = Vec::new();
    for cert in &imported_certs {
        for item in &cert.work {
            push_tickets(item, &mut tickets);
        }
    }
    for item in &work {
        push_tickets(item, &mut tickets);
    }
    Ok(Prepared {
        work,
        defs,
        imported_certs,
        tickets,
    })
}

/// Judge `claims` against the files they cite. A witness path is relative to `base`.
///
/// `budget` is the largest text body, in bytes, sent in one request. A longer
/// single text file is sent in parts. A joint witness whose text does not fit
/// is [`Failure::TooLarge`] and is not sent. An image does not spend this budget.
pub fn check<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    budget: usize,
    oracle: &mut O,
) -> Result<Report, CheckError> {
    check_bound(claims, base, budget, None, oracle)
}

/// [`check`], with `bound` standing for every `(witness $)`.
pub fn check_bound<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    budget: usize,
    bound: Option<&BoundFile>,
    oracle: &mut O,
) -> Result<Report, CheckError> {
    let bound = BoundWitnesses {
        default: bound.cloned(),
        named: HashMap::new(),
    };
    check_bound_with(claims, base, budget, &bound, oracle)
}

/// [`check`], with bindings for `$`, `$name`, and `$name/rel` witnesses.
pub fn check_bound_with<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    budget: usize,
    bound: &BoundWitnesses,
    oracle: &mut O,
) -> Result<Report, CheckError> {
    let prepared = submit_with(claims, base, budget, bound, oracle)?;
    let answers = oracle.wait_many(prepared.tickets());
    prepared.settle(&answers)
}

fn defined_heads(claims: &[Proposition]) -> HashSet<String> {
    let mut heads = HashSet::new();
    for claim in claims {
        collect_heads(claim, &mut heads);
    }
    heads
}

fn collect_heads(proposition: &Proposition, heads: &mut HashSet<String>) {
    match proposition {
        Proposition::And(xs) | Proposition::Or(xs) => {
            for proposition in xs {
                collect_heads(proposition, heads);
            }
        }
        Proposition::Not(inner) => collect_heads(inner, heads),
        Proposition::Sentence(_) | Proposition::Grounded { .. } => {}
        Proposition::Defined { body, .. } => collect_heads(body, heads),
        Proposition::Claim { sentence, support } => {
            heads.insert(sentence.text.clone());
            if let Support::Sentence(inner) = support {
                collect_heads(inner, heads);
            }
        }
        Proposition::Cert {
            cert_id, support, ..
        } => {
            heads.insert(cert_id.clone());
            if let Support::Sentence(inner) = support {
                collect_heads(inner, heads);
            }
        }
        Proposition::Assume(sentence) => {
            heads.insert(sentence.text.clone());
        }
        Proposition::Scope { body, .. } => {
            for proposition in body {
                collect_heads(proposition, heads);
            }
        }
    }
}

fn collect_cert_ids(props: &[Proposition], out: &mut Vec<String>) {
    for prop in props {
        match prop {
            Proposition::And(xs) | Proposition::Or(xs) | Proposition::Scope { body: xs } => {
                collect_cert_ids(xs, out);
            }
            Proposition::Not(inner) | Proposition::Defined { body: inner, .. } => {
                collect_cert_ids(std::slice::from_ref(inner), out);
            }
            Proposition::Claim { support, .. } => {
                if let Support::Sentence(inner) = support {
                    collect_cert_ids(std::slice::from_ref(inner), out);
                }
            }
            Proposition::Cert {
                cert_id, support, ..
            } => {
                if let Support::Sentence(inner) = support {
                    collect_cert_ids(std::slice::from_ref(inner), out);
                }
                if !out.contains(cert_id) {
                    out.push(cert_id.clone());
                }
            }
            Proposition::Sentence(_)
            | Proposition::Assume(_)
            | Proposition::Grounded { .. } => {}
        }
    }
}

/// The tree, one step per line. A failure class is a trailing comment.
pub fn write_report(report: &Report) -> String {
    let mut out = String::new();
    for claim in &report.claims {
        write_node(claim, 0, &mut out);
    }
    out
}

fn write_node(node: &Node, indent: usize, out: &mut String) {
    for _ in 0..indent {
        out.push_str("  ");
    }
    out.push_str(&node.value.to_string());
    out.push_str("  ");
    out.push_str(&node.text);
    if let Some(failure) = &node.failure {
        out.push_str("  ;; ");
        out.push_str(&failure.label());
    }
    out.push('\n');
    for cite in &node.cites {
        for _ in 0..(indent + 1) {
            out.push_str("  ");
        }
        out.push_str(cite);
        out.push('\n');
    }
    for child in &node.children {
        write_node(child, indent + 1, out);
    }
    if let Some(implication) = &node.implication {
        write_node(implication, indent + 1, out);
    }
}

struct Asked {
    ticket: Ticket,
    /// Set when this ticket is one slice of a file.
    part: Option<String>,
}

struct Work {
    text: String,
    cert_id: Option<String>,
    requires: Vec<String>,
    failure: Option<Failure>,
    cites: Vec<String>,
    children: Vec<Work>,
    implication: Option<Box<Work>>,
    asked: Vec<Asked>,
    kind: Kind,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Leaf,
    /// A sentence defined by a claim or an assumption. Its value is that definition's value.
    Use,
    /// A claim supported by a sentence. True when the support is true and the
    /// implication is entailed. False when the support is true and the
    /// implication is contradicted.
    Claim,
    /// A sentence defined by a `define` form. Takes the exact Kleene value of its body.
    Defined,
    /// A `ground` certificate.
    Cert,
    /// Reference to a `ground` certificate's settled value.
    CertUse,
    /// A sentence grounded via a certified `ground` witness (`=>` soundness).
    Grounded,
    Assume,
    Scope,
    And,
    Or,
    Not,
}

#[allow(clippy::too_many_arguments)]
fn prepare<O: Oracle>(
    proposition: &Proposition,
    defs: &HashSet<String>,
    sources: &Sources<'_>,
    bound: &BoundWitnesses,
    budget: usize,
    oracle: &mut O,
    imported_certs: &mut Vec<PreparedCert>,
    seen_certs: &mut HashSet<String>,
) -> Result<Work, CheckError> {
    match proposition {
        Proposition::Sentence(sentence) => {
            let defined = defs.contains(&sentence.text);
            Ok(Work {
                text: sentence.text.clone(),
                cert_id: None,
                requires: Vec::new(),
                failure: if defined {
                    None
                } else {
                    Some(Failure::Unpinned)
                },
                cites: Vec::new(),
                children: Vec::new(),
                implication: None,
                asked: Vec::new(),
                kind: if defined { Kind::Use } else { Kind::Leaf },
            })
        }
        Proposition::Defined { sentence, body } => {
            let child = prepare(
                body,
                defs,
                sources,
                bound,
                budget,
                oracle,
                imported_certs,
                seen_certs,
            )?;
            Ok(Work {
                text: sentence.text.clone(),
                cert_id: None,
                requires: Vec::new(),
                failure: None,
                cites: Vec::new(),
                children: vec![child],
                implication: None,
                asked: Vec::new(),
                kind: Kind::Defined,
            })
        }
        Proposition::Claim { sentence, support } => match support {
            Support::Witness { paths } => {
                prepare_witness(sentence, paths, sources, bound, budget, oracle)
            }
            Support::Sentence(support) => {
                let child = prepare(
                    support,
                    defs,
                    sources,
                    bound,
                    budget,
                    oracle,
                    imported_certs,
                    seen_certs,
                )?;
                let implication = Box::new(prepare_implication(
                    sentence,
                    support,
                    IMPLICATION_TEXT,
                    oracle,
                ));
                Ok(Work {
                    text: sentence.text.clone(),
                    cert_id: None,
                    requires: Vec::new(),
                    failure: None,
                    cites: Vec::new(),
                    children: vec![child],
                    implication: Some(implication),
                    asked: Vec::new(),
                    kind: Kind::Claim,
                })
            }
        },
        Proposition::Cert {
            cert_id,
            label,
            targets,
            support,
            requires,
            imported,
        } => {
            if !imported.is_empty() && seen_certs.insert(format!("deps:{cert_id}")) {
                let cert_defs = defined_heads(imported);
                let mut cert_ids = Vec::new();
                collect_cert_ids(imported, &mut cert_ids);
                let mut cert_work = Vec::with_capacity(imported.len());
                for item in imported {
                    cert_work.push(prepare(
                        item,
                        &cert_defs,
                        sources,
                        bound,
                        budget,
                        oracle,
                        imported_certs,
                        seen_certs,
                    )?);
                }
                imported_certs.push(PreparedCert {
                    cert_ids,
                    work: cert_work,
                    defs: cert_defs,
                });
            }
            match support {
                Support::Witness { paths } => {
                    let mut children = Vec::with_capacity(targets.len());
                    for target in targets {
                        children.push(prepare_witness(
                            target, paths, sources, bound, budget, oracle,
                        )?);
                    }
                    Ok(Work {
                        text: label.clone(),
                        cert_id: Some(cert_id.clone()),
                        requires: requires.clone(),
                        failure: None,
                        cites: Vec::new(),
                        children,
                        implication: None,
                        asked: Vec::new(),
                        kind: Kind::Cert,
                    })
                }
                Support::Sentence(support) => {
                    let child = prepare(
                        support,
                        defs,
                        sources,
                        bound,
                        budget,
                        oracle,
                        imported_certs,
                        seen_certs,
                    )?;
                    let implication = if targets.len() == 1 {
                        Box::new(prepare_implication(
                            &targets[0],
                            support,
                            &targets[0].text,
                            oracle,
                        ))
                    } else {
                        let steps = targets
                            .iter()
                            .map(|target| prepare_implication(target, support, &target.text, oracle))
                            .collect();
                        Box::new(Work {
                            text: IMPLICATION_TEXT.to_string(),
                            cert_id: None,
                            requires: Vec::new(),
                            failure: None,
                            cites: Vec::new(),
                            children: steps,
                            implication: None,
                            asked: Vec::new(),
                            kind: Kind::And,
                        })
                    };
                    Ok(Work {
                        text: label.clone(),
                        cert_id: Some(cert_id.clone()),
                        requires: requires.clone(),
                        failure: None,
                        cites: Vec::new(),
                        children: vec![child],
                        implication: Some(implication),
                        asked: Vec::new(),
                        kind: Kind::Cert,
                    })
                }
            }
        }
        Proposition::Grounded {
            sentence,
            cert_id,
            certificate,
            witness,
        } => {
            if !certificate.is_empty() && seen_certs.insert(cert_id.clone()) {
                let cert_defs = defined_heads(certificate);
                let mut cert_ids = Vec::new();
                collect_cert_ids(certificate, &mut cert_ids);
                if !cert_ids.contains(cert_id) {
                    cert_ids.push(cert_id.clone());
                }
                let mut cert_work = Vec::with_capacity(certificate.len());
                for item in certificate {
                    cert_work.push(prepare(
                        item,
                        &cert_defs,
                        sources,
                        bound,
                        budget,
                        oracle,
                        imported_certs,
                        seen_certs,
                    )?);
                }
                imported_certs.push(PreparedCert {
                    cert_ids,
                    work: cert_work,
                    defs: cert_defs,
                });
            }
            let cert_use = Work {
                text: cert_id.clone(),
                cert_id: Some(cert_id.clone()),
                requires: Vec::new(),
                failure: None,
                cites: Vec::new(),
                children: Vec::new(),
                implication: None,
                asked: Vec::new(),
                kind: Kind::CertUse,
            };
            let witness_work = prepare_witness(sentence, witness, sources, bound, budget, oracle)?;
            Ok(Work {
                text: sentence.text.clone(),
                cert_id: None,
                requires: Vec::new(),
                failure: None,
                cites: Vec::new(),
                children: vec![cert_use, witness_work],
                implication: None,
                asked: Vec::new(),
                kind: Kind::Grounded,
            })
        }
        Proposition::Assume(sentence) => Ok(Work {
            text: sentence.text.clone(),
            cert_id: None,
            requires: Vec::new(),
            failure: None,
            cites: Vec::new(),
            children: Vec::new(),
            implication: None,
            asked: Vec::new(),
            kind: Kind::Assume,
        }),
        Proposition::Scope { body } => {
            let mut children = Vec::with_capacity(body.len());
            for claim in body {
                children.push(prepare(
                    claim,
                    defs,
                    sources,
                    bound,
                    budget,
                    oracle,
                    imported_certs,
                    seen_certs,
                )?);
            }
            Ok(Work {
                text: "detail".to_string(),
                cert_id: None,
                requires: Vec::new(),
                failure: None,
                cites: Vec::new(),
                children,
                implication: None,
                asked: Vec::new(),
                kind: Kind::Scope,
            })
        }
        Proposition::And(xs) => prepare_bool(
            "and",
            Kind::And,
            xs,
            defs,
            sources,
            bound,
            budget,
            oracle,
            imported_certs,
            seen_certs,
        ),
        Proposition::Or(xs) => prepare_bool(
            "or",
            Kind::Or,
            xs,
            defs,
            sources,
            bound,
            budget,
            oracle,
            imported_certs,
            seen_certs,
        ),
        Proposition::Not(inner) => {
            let child = prepare(
                inner,
                defs,
                sources,
                bound,
                budget,
                oracle,
                imported_certs,
                seen_certs,
            )?;
            Ok(Work {
                text: "not".to_string(),
                cert_id: None,
                requires: Vec::new(),
                failure: None,
                cites: Vec::new(),
                children: vec![child],
                implication: None,
                asked: Vec::new(),
                kind: Kind::Not,
            })
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn prepare_bool<O: Oracle>(
    text: &str,
    kind: Kind,
    xs: &[Proposition],
    defs: &HashSet<String>,
    sources: &Sources<'_>,
    bound: &BoundWitnesses,
    budget: usize,
    oracle: &mut O,
    imported_certs: &mut Vec<PreparedCert>,
    seen_certs: &mut HashSet<String>,
) -> Result<Work, CheckError> {
    let mut children = Vec::with_capacity(xs.len());
    for proposition in xs {
        children.push(prepare(
            proposition,
            defs,
            sources,
            bound,
            budget,
            oracle,
            imported_certs,
            seen_certs,
        )?);
    }
    Ok(Work {
        text: text.to_string(),
        cert_id: None,
        requires: Vec::new(),
        failure: None,
        cites: Vec::new(),
        children,
        implication: None,
        asked: Vec::new(),
        kind,
    })
}

fn prepare_witness<O: Oracle>(
    sentence: &Sentence,
    paths: &[String],
    sources: &Sources<'_>,
    bound: &BoundWitnesses,
    budget: usize,
    oracle: &mut O,
) -> Result<Work, CheckError> {
    let cites: Vec<String> = paths.iter().map(|path| cite_path(path, bound)).collect();
    let mut work = Work {
        text: sentence.text.clone(),
        cert_id: None,
        requires: Vec::new(),
        failure: None,
        cites: cites.clone(),
        children: Vec::new(),
        implication: None,
        asked: Vec::new(),
        kind: Kind::Leaf,
    };
    if paths
        .iter()
        .zip(&cites)
        .any(|(path, cite)| is_brief_file(path) || is_brief_file(cite))
    {
        work.failure = Some(Failure::BriefWitness);
        return Ok(work);
    }
    let mut located = Vec::with_capacity(paths.len());
    for (path, cite) in paths.iter().zip(cites) {
        match locate(sources, path, &cite)? {
            None => {
                work.failure = Some(Failure::MissingFile { path: cite.clone() });
                work.cites = vec![cite];
                return Ok(work);
            }
            Some((len, origin)) => located.push((cite, origin, len)),
        }
    }
    let mut files = Vec::with_capacity(located.len());
    for (path, origin, len) in located {
        match load_witness(sources, &path, origin, len)? {
            WitnessLoad::Reject(failure) => {
                work.failure = Some(failure);
                return Ok(work);
            }
            WitnessLoad::File(file) => files.push(file),
        }
    }
    let budget = budget.max(1);
    if files.len() == 1 {
        return submit_one(
            &sentence.grounded,
            files.pop().unwrap(),
            sources,
            budget,
            oracle,
            work,
        );
    }
    submit_joint(&sentence.grounded, files, sources, budget, oracle, work)
}

enum WitnessLoad {
    File(WitnessFile),
    Reject(Failure),
}

enum WitnessFile {
    Text {
        path: String,
        origin: Origin,
        len: u64,
    },
    Image {
        path: String,
        asset: Asset,
    },
}

fn load_witness(
    sources: &Sources<'_>,
    path: &str,
    origin: Origin,
    len: u64,
) -> Result<WitnessLoad, CheckError> {
    let Some(format) = image_extension(path).or_else(|| image_extension(&origin.opened())) else {
        return Ok(WitnessLoad::File(WitnessFile::Text {
            path: path.to_string(),
            origin,
            len,
        }));
    };
    if len > MAX_IMAGE_BYTES as u64 {
        return Ok(WitnessLoad::Reject(Failure::ImageTooLarge));
    }
    let bytes = read_origin(sources, &origin, path)?;
    match image_asset(&bytes, format) {
        Ok(asset) => Ok(WitnessLoad::File(WitnessFile::Image {
            path: path.to_string(),
            asset,
        })),
        Err(err) => Ok(WitnessLoad::Reject(image_failure(err))),
    }
}

fn image_failure(err: ImageError) -> Failure {
    match err {
        ImageError::Unsupported => Failure::UnsupportedImage,
        ImageError::TooSmall => Failure::ImageTooSmall,
        ImageError::TooLarge => Failure::ImageTooLarge,
    }
}

fn submit_one<O: Oracle>(
    sentence: &str,
    file: WitnessFile,
    sources: &Sources<'_>,
    budget: usize,
    oracle: &mut O,
    mut work: Work,
) -> Result<Work, CheckError> {
    match file {
        WitnessFile::Image { path, asset } => {
            let class = document_image(sentence, &path, asset);
            work.asked.push(Asked {
                ticket: oracle.submit(class),
                part: None,
            });
            Ok(work)
        }
        WitnessFile::Text { path, origin, len } => {
            if len > MAX_SLICED_BYTES as u64 {
                work.failure = Some(Failure::TooLarge);
                return Ok(work);
            }
            let body = read_text(sources, &origin, &path)?;
            if body.len() <= budget {
                let class = document_question(sentence, &path, &body, None);
                work.asked.push(Asked {
                    ticket: oracle.submit(class),
                    part: None,
                });
                return Ok(work);
            }
            let parts = split_body(&body, budget);
            let total_parts = parts.len() as u32;
            for (index, part) in parts.iter().enumerate() {
                let n = (index as u32) + 1;
                let class = document_question(sentence, &path, part, Some((n, total_parts)));
                work.asked.push(Asked {
                    ticket: oracle.submit(class),
                    part: Some(format!("{path} part {n}/{total_parts}")),
                });
            }
            Ok(work)
        }
    }
}

fn submit_joint<O: Oracle>(
    sentence: &str,
    files: Vec<WitnessFile>,
    sources: &Sources<'_>,
    budget: usize,
    oracle: &mut O,
    mut work: Work,
) -> Result<Work, CheckError> {
    let text_total: u64 = files
        .iter()
        .map(|file| match file {
            WitnessFile::Text { len, .. } => *len,
            WitnessFile::Image { .. } => 0,
        })
        .sum();
    let text_over = files.iter().any(|file| match file {
        WitnessFile::Text { len, .. } => *len > budget as u64,
        WitnessFile::Image { .. } => false,
    });
    if text_over || text_total > budget as u64 {
        work.failure = Some(Failure::TooLarge);
        return Ok(work);
    }
    let mut bodies = Vec::with_capacity(files.len());
    let mut assets = Vec::new();
    for file in files {
        match file {
            WitnessFile::Text { path, origin, .. } => {
                bodies.push(json!({ "path": path, "body": read_text(sources, &origin, &path)? }));
            }
            WitnessFile::Image { path, asset } => {
                bodies.push(json!({ "path": path, "body": asset.reference() }));
                assets.push(asset);
            }
        }
    }
    let class = joint_question(sentence, bodies, dedup_assets(assets));
    work.asked.push(Asked {
        ticket: oracle.submit(class),
        part: None,
    });
    Ok(work)
}

fn dedup_assets(assets: Vec<Asset>) -> Vec<Asset> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for asset in assets {
        if seen.insert(asset.reference().to_string()) {
            out.push(asset);
        }
    }
    out
}

/// A `.brief` file is a model, not evidence a witness can cite.
fn is_brief_file(path: &str) -> bool {
    let Some(name) = Path::new(path).file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    name.eq_ignore_ascii_case(".brief")
        || (name.len() > ".brief".len()
            && name[name.len() - ".brief".len()..].eq_ignore_ascii_case(".brief"))
}

fn prepare_implication<O: Oracle>(
    head: &Sentence,
    support: &Proposition,
    label: &str,
    oracle: &mut O,
) -> Work {
    let premises = premise_sentences(std::slice::from_ref(support));
    let mut work = Work {
        text: label.to_string(),
        cert_id: None,
        requires: Vec::new(),
        failure: Some(Failure::NotEntailed),
        cites: Vec::new(),
        children: Vec::new(),
        implication: None,
        asked: Vec::new(),
        kind: Kind::Leaf,
    };
    if premises.is_empty() {
        return work;
    }
    let class = implication_question(&head.grounded, &premises);
    work.failure = None;
    work.asked.push(Asked {
        ticket: oracle.submit(class),
        part: None,
    });
    work
}

fn document_question(
    sentence: &str,
    path: &str,
    body: &str,
    part: Option<(u32, u32)>,
) -> Classification {
    DefaultStrategy::document_classification(sentence, path, body, part)
}

fn document_image(sentence: &str, path: &str, asset: Asset) -> Classification {
    DefaultStrategy::document_image(sentence, path, asset)
}

fn joint_question(sentence: &str, files: Vec<Json>, assets: Vec<Asset>) -> Classification {
    DefaultStrategy::joint_with_assets(sentence, files, assets)
}

fn implication_question(head: &str, premises: &[String]) -> Classification {
    DefaultStrategy::implication_classification(head, premises)
}

/// Sentences the implication may use. A nested claim contributes its sentence.
/// An `and` contributes each conjunct. An `or` or a `not` contributes one
/// sentence, because those connectives are not a list of premises.
fn premise_sentences(body: &[Proposition]) -> Vec<String> {
    let mut sentences = Vec::new();
    for claim in body {
        push_premises(claim, &mut sentences);
    }
    sentences
}

fn push_premises(proposition: &Proposition, sentences: &mut Vec<String>) {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Assume(sentence) => {
            sentences.push(sentence.grounded.clone());
        }
        Proposition::Claim { sentence, .. }
        | Proposition::Defined { sentence, .. }
        | Proposition::Grounded { sentence, .. } => sentences.push(sentence.grounded.clone()),
        Proposition::And(xs) => {
            for proposition in xs {
                push_premises(proposition, sentences);
            }
        }
        Proposition::Or(_) | Proposition::Not(_) => sentences.push(statement(proposition)),
        Proposition::Scope { .. } | Proposition::Cert { .. } => {}
    }
}

fn statement(proposition: &Proposition) -> String {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Assume(sentence) => {
            sentence.grounded.clone()
        }
        Proposition::Claim { sentence, .. }
        | Proposition::Defined { sentence, .. }
        | Proposition::Grounded { sentence, .. } => sentence.grounded.clone(),
        Proposition::Cert { label, .. } => label.clone(),
        Proposition::And(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" and "),
        Proposition::Or(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" or "),
        Proposition::Not(inner) => format!("not ({})", statement(inner)),
        Proposition::Scope { .. } => "detail".to_string(),
    }
}

/// Which universe holds a witness, and the path used for image detection.
enum Origin {
    Tree { id: String },
    Bound { opened: String },
    NamedFile { path: PathBuf },
    NamedDir { root: PathBuf, id: String },
}

impl Origin {
    fn opened(&self) -> String {
        match self {
            Origin::Tree { id } => id.clone(),
            Origin::Bound { opened } => opened.clone(),
            Origin::NamedFile { path } => path.display().to_string(),
            Origin::NamedDir { id, .. } => id.clone(),
        }
    }
}

fn cite_path(path: &str, bound: &BoundWitnesses) -> String {
    match parse_dollar_witness(path) {
        Some(("", None)) => bound
            .default
            .as_ref()
            .map(|bound| bound.cite.clone())
            .unwrap_or_else(|| "$".to_string()),
        Some((name, None)) => bound
            .named
            .get(name)
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| path.to_string()),
        Some((_, Some(rel))) => rel.to_string(),
        None => path.to_string(),
    }
}

fn locate(
    sources: &Sources<'_>,
    pin: &str,
    cite: &str,
) -> Result<Option<(u64, Origin)>, CheckError> {
    match parse_dollar_witness(pin) {
        Some(("", None)) => {
            let Some(given) = sources.given else {
                return Ok(None);
            };
            let opened = given.path().display().to_string();
            match open_err(cite, given.stat(pin))? {
                Some(info) => Ok(Some((info.len, Origin::Bound { opened }))),
                None => Ok(None),
            }
        }
        Some((name, None)) => {
            let Some(path) = sources.named.get(name) else {
                return Ok(None);
            };
            let given = GivenFile::new(path.clone());
            match open_err(cite, given.stat(pin))? {
                Some(info) => Ok(Some((info.len, Origin::NamedFile { path: path.clone() }))),
                None => Ok(None),
            }
        }
        Some((name, Some(rel))) => {
            let Some(root) = sources.named.get(name) else {
                return Ok(None);
            };
            let fs = Filesystem::at(root);
            match open_err(cite, fs.stat(rel))? {
                Some(info) => Ok(Some((
                    info.len,
                    Origin::NamedDir {
                        root: root.clone(),
                        id: rel.to_string(),
                    },
                ))),
                None => Ok(None),
            }
        }
        None => match open_err(cite, sources.tree.stat(pin))? {
            Some(info) => Ok(Some((
                info.len,
                Origin::Tree {
                    id: pin.to_string(),
                },
            ))),
            None => Ok(None),
        },
    }
}

fn open_err<T>(cite: &str, result: Result<T, UniverseError>) -> Result<T, CheckError> {
    result.map_err(|err| match err {
        UniverseError::Io(err) => CheckError::Io(format!("{cite}: {err}")),
        UniverseError::Missing(_) => CheckError::Io(format!("{cite}: not found")),
    })
}

fn read_origin(sources: &Sources<'_>, origin: &Origin, cite: &str) -> Result<Vec<u8>, CheckError> {
    let result = match origin {
        Origin::Tree { id } => sources.tree.read(id),
        Origin::Bound { .. } => {
            let Some(given) = sources.given else {
                return Err(CheckError::Bind("`$` needs a file".into()));
            };
            given.read("$")
        }
        Origin::NamedFile { path } => GivenFile::new(path.clone()).read("$"),
        Origin::NamedDir { root, id } => Filesystem::at(root).read(id),
    };
    open_err(cite, result)
}

fn read_text(sources: &Sources<'_>, origin: &Origin, cite: &str) -> Result<String, CheckError> {
    let bytes = read_origin(sources, origin, cite)?;
    if bytes.contains(&0) || std::str::from_utf8(&bytes).is_err() {
        return Ok(format!("<binary {} bytes>", bytes.len()));
    }
    Ok(String::from_utf8(bytes).expect("utf-8 checked"))
}

fn push_tickets(work: &Work, tickets: &mut Vec<Ticket>) {
    for child in &work.children {
        push_tickets(child, tickets);
    }
    if let Some(implication) = &work.implication {
        push_tickets(implication, tickets);
    }
    for asked in &work.asked {
        tickets.push(asked.ticket);
    }
}

/// A settled step, before lemma uses are filled in.
///
/// A use names a detail that may be settled later in the brief, so its value
/// waits until every detail has a value.
struct Judged {
    text: String,
    cert_id: Option<String>,
    requires: Vec<String>,
    kind: Kind,
    children: Vec<Judged>,
    implication: Option<Box<Judged>>,
    /// Set for a leaf. The triple is the value, the failure, and the cites.
    leaf: Option<(Value, Option<Failure>, Vec<String>)>,
}

fn settle(
    mut work: Work,
    answers: &[Result<Answer, OracleError>],
    cursor: &mut usize,
) -> Result<Judged, CheckError> {
    let child_work = std::mem::take(&mut work.children);
    let implication_work = work.implication.take();
    let mut children = Vec::with_capacity(child_work.len());
    for child in child_work {
        children.push(settle(child, answers, cursor)?);
    }
    let implication = match implication_work {
        Some(implication) => Some(Box::new(settle(*implication, answers, cursor)?)),
        None => None,
    };
    let asked = take_answers(&work.asked, answers, cursor)?;
    let leaf = match work.kind {
        Kind::Leaf => Some(judge_leaf(&work, &asked)),
        Kind::Use
        | Kind::Claim
        | Kind::Defined
        | Kind::Cert
        | Kind::CertUse
        | Kind::Grounded
        | Kind::Assume
        | Kind::Scope
        | Kind::And
        | Kind::Or
        | Kind::Not => None,
    };
    Ok(Judged {
        text: work.text,
        cert_id: work.cert_id,
        requires: work.requires,
        kind: work.kind,
        children,
        implication,
        leaf,
    })
}

fn lemma_values_with_seed(
    nodes: &[Judged],
    defs: &HashSet<String>,
    seed: &HashMap<String, Value>,
) -> HashMap<String, Value> {
    let mut values = seed.clone();
    let steps = count_nodes(nodes);
    for _ in 0..=steps {
        if !propagate(nodes, defs, &mut values) {
            break;
        }
    }
    values
}

fn count_nodes(nodes: &[Judged]) -> usize {
    let mut count = 0;
    for node in nodes {
        count += 1;
        count += count_nodes(&node.children);
        if let Some(implication) = &node.implication {
            count += count_nodes(std::slice::from_ref(implication));
        }
    }
    count
}

fn propagate(
    nodes: &[Judged],
    defs: &HashSet<String>,
    values: &mut HashMap<String, Value>,
) -> bool {
    let mut changed = false;
    for node in nodes {
        changed |= propagate(&node.children, defs, values);
        if let Some(implication) = &node.implication {
            changed |= propagate(std::slice::from_ref(implication), defs, values);
        }
        let key = match node.kind {
            Kind::Claim | Kind::Assume => Some(node.text.as_str()),
            Kind::Cert => node.cert_id.as_deref(),
            Kind::Leaf if defs.contains(&node.text) => Some(node.text.as_str()),
            Kind::Leaf
            | Kind::Use
            | Kind::Defined
            | Kind::CertUse
            | Kind::Grounded
            | Kind::Scope
            | Kind::And
            | Kind::Or
            | Kind::Not => None,
        };
        if let Some(key) = key {
            let value = eval_value(node, values);
            if values.get(key) != Some(&value) {
                values.insert(key.to_string(), value);
                changed = true;
            }
        }
    }
    changed
}

fn eval_value(node: &Judged, lemmas: &HashMap<String, Value>) -> Value {
    match node.kind {
        Kind::Leaf => node
            .leaf
            .as_ref()
            .map(|leaf| leaf.0)
            .unwrap_or(Value::Unknown),
        Kind::Use => lemmas.get(&node.text).copied().unwrap_or(Value::Unknown),
        Kind::CertUse => node
            .cert_id
            .as_deref()
            .and_then(|id| lemmas.get(id).copied())
            .unwrap_or(Value::Unknown),
        Kind::Assume => Value::True,
        Kind::Defined => node
            .children
            .first()
            .map(|child| eval_value(child, lemmas))
            .unwrap_or(Value::Unknown),
        Kind::And => node
            .children
            .iter()
            .fold(Value::True, |acc, child| acc.and(eval_value(child, lemmas))),
        Kind::Or => node
            .children
            .iter()
            .fold(Value::False, |acc, child| acc.or(eval_value(child, lemmas))),
        Kind::Not => node
            .children
            .first()
            .map(|child| eval_value(child, lemmas).not())
            .unwrap_or(Value::Unknown),
        Kind::Scope => node
            .children
            .iter()
            .fold(Value::True, |acc, child| acc.and(eval_value(child, lemmas))),
        Kind::Claim => {
            let support = node
                .children
                .first()
                .map(|child| eval_value(child, lemmas))
                .unwrap_or(Value::Unknown);
            if support != Value::True {
                Value::Unknown
            } else {
                node.implication
                    .as_ref()
                    .map(|step| eval_value(step, lemmas))
                    .unwrap_or(Value::Unknown)
            }
        }
        Kind::Cert => {
            if node
                .requires
                .iter()
                .any(|req| lemmas.get(req).copied() != Some(Value::True))
            {
                return Value::Unknown;
            }
            if let Some(step) = &node.implication {
                let support = node
                    .children
                    .first()
                    .map(|child| eval_value(child, lemmas))
                    .unwrap_or(Value::Unknown);
                if support != Value::True {
                    Value::Unknown
                } else {
                    eval_value(step, lemmas)
                }
            } else {
                node.children
                    .iter()
                    .fold(Value::True, |acc, child| acc.and(eval_value(child, lemmas)))
            }
        }
        Kind::Grounded => {
            let cert = node
                .children
                .first()
                .map(|child| eval_value(child, lemmas))
                .unwrap_or(Value::Unknown);
            let wit = node
                .children
                .get(1)
                .map(|child| eval_value(child, lemmas))
                .unwrap_or(Value::Unknown);
            if cert == Value::True && wit == Value::True {
                Value::True
            } else {
                Value::Unknown
            }
        }
    }
}

fn record_cert_failures(
    nodes: &[Judged],
    lemmas: &HashMap<String, Value>,
    roots: &mut HashMap<String, String>,
) {
    for node in nodes {
        record_cert_failures(&node.children, lemmas, roots);
        if let Some(implication) = &node.implication {
            record_cert_failures(std::slice::from_ref(implication), lemmas, roots);
        }
        if node.kind == Kind::Cert {
            if let Some(id) = &node.cert_id {
                if lemmas.get(id).copied() != Some(Value::True) {
                    let root = node
                        .requires
                        .iter()
                        .find(|req| lemmas.get(*req).copied() != Some(Value::True))
                        .map(|req| roots.get(req).cloned().unwrap_or_else(|| req.clone()))
                        .unwrap_or_else(|| id.clone());
                    roots.insert(id.clone(), root);
                }
            }
        }
    }
}

fn render(
    node: &Judged,
    lemmas: &HashMap<String, Value>,
    cert_roots: &HashMap<String, String>,
) -> Node {
    let children = node
        .children
        .iter()
        .map(|child| render(child, lemmas, cert_roots))
        .collect();
    let implication = node
        .implication
        .as_ref()
        .map(|step| Box::new(render(step, lemmas, cert_roots)));
    let (failure, cites) = match node.kind {
        Kind::Claim | Kind::Cert => (claim_failure(node, lemmas, cert_roots), Vec::new()),
        Kind::Defined => (defined_failure(node, lemmas), Vec::new()),
        Kind::Grounded => (grounded_failure(node, lemmas, cert_roots), Vec::new()),
        _ => match &node.leaf {
            Some((_, failure, cites)) => (failure.clone(), cites.clone()),
            None => (None, Vec::new()),
        },
    };
    Node {
        text: node.text.clone(),
        value: eval_value(node, lemmas),
        failure,
        cites,
        children,
        implication,
        assumed: matches!(node.kind, Kind::Assume),
    }
}

/// The implication's failure, once the support holds. A support that failed
/// leaves the claim unknown and keeps the comment on the support.
fn claim_failure(
    node: &Judged,
    lemmas: &HashMap<String, Value>,
    cert_roots: &HashMap<String, String>,
) -> Option<Failure> {
    if let Some(step) = &node.implication {
        let support_holds = node
            .children
            .first()
            .is_some_and(|child| eval_value(child, lemmas) == Value::True);
        if !support_holds {
            return None;
        }
        if eval_value(step, lemmas) != Value::True {
            return first_failure(step, lemmas);
        }
        if node.kind == Kind::Cert {
            return failed_prereq_cert(node, lemmas, cert_roots);
        }
        return None;
    }
    if node.kind == Kind::Cert && eval_value(node, lemmas) != Value::True {
        return first_failure(node, lemmas)
            .or_else(|| failed_prereq_cert(node, lemmas, cert_roots));
    }
    None
}

fn failed_prereq_cert(
    node: &Judged,
    lemmas: &HashMap<String, Value>,
    cert_roots: &HashMap<String, String>,
) -> Option<Failure> {
    node.requires
        .iter()
        .find(|req| lemmas.get(*req).copied() != Some(Value::True))
        .map(|req| Failure::CertificateFailed {
            cert: cert_roots.get(req).cloned().unwrap_or_else(|| req.clone()),
        })
}

fn defined_failure(node: &Judged, lemmas: &HashMap<String, Value>) -> Option<Failure> {
    if eval_value(node, lemmas) == Value::True {
        return None;
    }
    node.children
        .first()
        .and_then(|child| first_failure(child, lemmas))
}

fn grounded_failure(
    node: &Judged,
    lemmas: &HashMap<String, Value>,
    cert_roots: &HashMap<String, String>,
) -> Option<Failure> {
    if eval_value(node, lemmas) == Value::True {
        return None;
    }
    let cert_holds = node
        .children
        .first()
        .is_some_and(|child| eval_value(child, lemmas) == Value::True);
    if !cert_holds {
        let cert = node
            .children
            .first()
            .and_then(|child| child.cert_id.as_ref())
            .map(|id| cert_roots.get(id).cloned().unwrap_or_else(|| id.clone()))
            .unwrap_or_else(|| "certificate".to_string());
        return Some(Failure::CertificateFailed { cert });
    }
    node.children
        .get(1)
        .and_then(|child| first_failure(child, lemmas))
}

fn first_failure(node: &Judged, lemmas: &HashMap<String, Value>) -> Option<Failure> {
    if eval_value(node, lemmas) == Value::True {
        return None;
    }
    if let Some((_, Some(failure), _)) = &node.leaf {
        return Some(failure.clone());
    }
    for child in &node.children {
        if let Some(failure) = first_failure(child, lemmas) {
            return Some(failure);
        }
    }
    node.implication
        .as_deref()
        .and_then(|step| first_failure(step, lemmas))
}

fn take_answers(
    asked: &[Asked],
    answers: &[Result<Answer, OracleError>],
    cursor: &mut usize,
) -> Result<Vec<(Answer, Option<String>)>, CheckError> {
    let mut out = Vec::with_capacity(asked.len());
    for asked in asked {
        let answer = answers.get(*cursor).ok_or_else(|| {
            CheckError::Oracle(OracleError::Parse("missing oracle answer".into()))
        })?;
        *cursor += 1;
        let answer = answer
            .as_ref()
            .map_err(|err| CheckError::Oracle(err.clone()))?;
        out.push((answer.clone(), asked.part.clone()));
    }
    Ok(out)
}

fn judge_leaf(
    work: &Work,
    asked: &[(Answer, Option<String>)],
) -> (Value, Option<Failure>, Vec<String>) {
    if work.asked.is_empty() {
        return (Value::Unknown, work.failure.clone(), work.cites.clone());
    }
    let mut entailed = Vec::new();
    let mut contradicted = Vec::new();
    let parts = asked.iter().any(|(_, part)| part.is_some());
    for (answer, part) in asked {
        let cite = part.clone().unwrap_or_default();
        match polarity(answer) {
            Some(Value::True) => entailed.push(cite),
            Some(Value::False) => contradicted.push(cite),
            _ => {}
        }
    }
    if !entailed.is_empty() {
        let cites = if parts { entailed } else { work.cites.clone() };
        (Value::True, None, cites)
    } else if !contradicted.is_empty() {
        let cites = if parts {
            contradicted
        } else {
            work.cites.clone()
        };
        (Value::False, Some(Failure::Contradicted), cites)
    } else {
        let cites = if parts {
            asked.iter().filter_map(|(_, part)| part.clone()).collect()
        } else {
            work.cites.clone()
        };
        (Value::Unknown, Some(Failure::NotEntailed), cites)
    }
}

/// Split `text` into the fewest pieces that each fit in `budget`.
///
/// A piece ends on a newline when one fits. A line longer than `budget` is
/// cut on a character boundary. The pieces concatenate back to `text`.
pub(crate) fn split_body(text: &str, budget: usize) -> Vec<String> {
    if text.len() <= budget {
        return vec![text.to_string()];
    }
    let mut parts = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.len() <= budget {
            parts.push(rest.to_string());
            break;
        }
        let cut = cut_at(rest, budget);
        parts.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    parts
}

fn cut_at(text: &str, budget: usize) -> usize {
    let end = floor_char_boundary(text, budget.min(text.len()));
    if end == 0 {
        return text.chars().next().map(|c| c.len_utf8()).unwrap_or(0);
    }
    if let Some(newline) = text[..end].rfind('\n') {
        let cut = newline + 1;
        if cut < text.len() {
            return cut;
        }
    }
    end
}

fn floor_char_boundary(text: &str, mut index: usize) -> usize {
    if index >= text.len() {
        return text.len();
    }
    while index > 0 && !text.is_char_boundary(index) {
        index -= 1;
    }
    index
}

/// Entailed or contradicted at the trust bar. Anything weaker is unsettled.
fn polarity(answer: &Answer) -> Option<Value> {
    DefaultStrategy::settle_choice(answer, crate::confidence::SUPPORT).settled
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::syntax::parse;
    use std::collections::HashMap;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "autoground-check-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    struct Rec {
        next: u64,
        pending: HashMap<u64, Answer>,
        resolved: HashMap<u64, Answer>,
        seen: Vec<Classification>,
        answer: fn(&Classification) -> Answer,
    }

    impl Rec {
        fn new(answer: fn(&Classification) -> Answer) -> Self {
            Self {
                next: 0,
                pending: HashMap::new(),
                resolved: HashMap::new(),
                seen: Vec::new(),
                answer,
            }
        }
    }

    impl Oracle for Rec {
        fn submit(&mut self, req: Classification) -> Ticket {
            let ticket = Ticket(self.next);
            self.next += 1;
            let answer = (self.answer)(&req);
            self.pending.insert(ticket.0, answer);
            self.seen.push(req);
            ticket
        }

        fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
            if let Some(answer) = self.resolved.get(&ticket.0) {
                return Ok(answer.clone());
            }
            for (id, answer) in self.pending.drain() {
                self.resolved.insert(id, answer);
            }
            self.resolved
                .get(&ticket.0)
                .cloned()
                .ok_or_else(|| OracleError::Parse(format!("unknown ticket {}", ticket.0)))
        }
    }

    fn choice(label: &str, confidence: f64) -> Answer {
        Answer {
            choice: label.to_string(),
            confidence,
            probabilities: None,
            noul: None,
        }
    }

    fn entailed(_: &Classification) -> Answer {
        choice("entailed", 0.95)
    }

    fn neutral(_: &Classification) -> Answer {
        choice("neutral", 0.95)
    }

    fn by_proposition(req: &Classification) -> Answer {
        let proposition = req.instructions["proposition"].as_str().unwrap_or("");
        if proposition.contains("holds") || proposition == "inner" {
            choice("entailed", 0.95)
        } else if proposition.contains("nope") {
            choice("contradicted", 0.95)
        } else {
            choice("neutral", 0.95)
        }
    }

    fn compiled(src: &str) -> Vec<Proposition> {
        compile(&parse(src).unwrap()).unwrap().claims
    }

    #[test]
    fn an_unpinned_sentence_is_not_sent() {
        let dir = scratch("unpinned");
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"head\" \"Search calls Oracle\")"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle
            .seen
            .iter()
            .all(|req| req.state.get("path").is_none()));
        assert_eq!(report.claims[0].children[0].value, Value::Unknown);
        assert_eq!(
            report.claims[0].children[0].failure,
            Some(Failure::Unpinned)
        );
        assert!(write_report(&report).contains(";; unpinned"));
    }

    #[test]
    fn a_missing_file_is_its_own_failure() {
        let dir = scratch("missing");
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"Search calls Oracle\" (witness \"src/nope.rs\"))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle.seen.is_empty());
        assert_eq!(
            report.claims[0].failure,
            Some(Failure::MissingFile {
                path: "src/nope.rs".into()
            })
        );
    }

    #[test]
    fn a_witness_that_cites_a_brief_is_rejected() {
        let dir = scratch("brief-witness");
        let brief_dir = dir.join("examples");
        fs::create_dir(&brief_dir).unwrap();
        fs::write(dir.join("Notes.BRIEF"), "alpha\n").unwrap();
        fs::write(brief_dir.join("notes.brief"), "alpha\n").unwrap();
        fs::write(brief_dir.join("note.txt"), "alpha\n").unwrap();
        fs::write(brief_dir.join("file.briefing"), "alpha\n").unwrap();
        let mut oracle = Rec::new(entailed);
        for src in [
            "(claim \"alpha\" (witness \"notes.brief\"))",
            "(claim \"alpha\" (witness \"../Notes.BRIEF\"))",
            "(claim \"alpha\" (witness \".brief\"))",
            "(claim \"alpha\" (witness \"note.txt\" \"notes.brief\"))",
        ] {
            let report =
                check(&compiled(src), &brief_dir, DEFAULT_BODY_BYTES, &mut oracle).unwrap();
            assert_eq!(report.claims[0].value, Value::Unknown, "{src}");
            assert_eq!(
                report.claims[0].failure,
                Some(Failure::BriefWitness),
                "{src}"
            );
            assert!(
                write_report(&report).contains(";; briefs can't be witnesses"),
                "{src}"
            );
        }
        assert!(oracle.seen.is_empty());

        let report = check(
            &compiled("(claim \"alpha\" (witness \"file.briefing\"))"),
            &brief_dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
    }

    #[test]
    fn a_parent_path_is_read_from_the_brief_directory() {
        let dir = scratch("parent");
        let brief_dir = dir.join("examples");
        fs::create_dir(&brief_dir).unwrap();
        fs::write(dir.join("note.txt"), "alpha\n").unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"alpha\" (witness \"../note.txt\"))"),
            &brief_dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[0].cites, vec!["../note.txt".to_string()]);
        assert!(!oracle.seen.is_empty());
    }

    #[test]
    fn a_file_that_entails_the_sentence_discharges_it() {
        let dir = scratch("one");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/search.rs"), "fn search() {}").unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"search finds the window\" (witness \"src/search.rs\"))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[0].cites, vec!["src/search.rs".to_string()]);
        assert_eq!(oracle.seen.len(), 1);
        assert_eq!(
            oracle.seen[0].state["body"].as_str().unwrap(),
            "fn search() {}"
        );
        assert!(oracle.seen[0].state.assets().is_empty());
    }

    #[test]
    fn a_joint_witness_sends_both_bodies_once() {
        let dir = scratch("joint");
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(dir.join("src/a.rs"), "aaa").unwrap();
        fs::write(dir.join("src/b.rs"), "bbb").unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"together\" (witness \"src/a.rs\" \"src/b.rs\"))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        let files = oracle.seen[0].state["files"].as_array().unwrap();
        assert_eq!(files.len(), 2);
        assert_eq!(files[0]["body"].as_str().unwrap(), "aaa");
        assert_eq!(files[1]["body"].as_str().unwrap(), "bbb");
        assert!(oracle.seen[0].state.assets().is_empty());
        assert_eq!(
            report.claims[0].cites,
            vec!["src/a.rs".to_string(), "src/b.rs".to_string()]
        );
    }

    #[test]
    fn the_same_file_is_one_state_for_two_witnesses() {
        let dir = scratch("batch");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(entailed);
        check(
            &compiled(
                r#"
(claim "one" (witness "a.rs"))
(claim "two" (witness "a.rs"))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 2);
        assert_eq!(oracle.seen[0].state.digest(), oracle.seen[1].state.digest());
    }

    #[test]
    fn a_detail_needs_the_body_and_the_implication() {
        let dir = scratch("detail");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled(
                r#"
(claim "holds"
  (claim "holds in the file" (witness "a.rs")))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let detail = &report.claims[0];
        assert_eq!(detail.value, Value::True);
        assert_eq!(detail.children[0].value, Value::True);
        assert_eq!(detail.implication.as_ref().unwrap().value, Value::True);
        let sentences = oracle
            .seen
            .iter()
            .find(|req| req.state.get("sentences").is_some())
            .unwrap();
        assert_eq!(
            sentences.state["sentences"][0].as_str().unwrap(),
            "holds in the file"
        );
        assert_eq!(
            sentences.instructions["proposition"].as_str().unwrap(),
            "holds"
        );
    }

    #[test]
    fn a_true_body_with_a_neutral_implication_stays_undischarged() {
        let dir = scratch("gap");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(|req| {
            if req.state.get("sentences").is_some() {
                neutral(req)
            } else {
                entailed(req)
            }
        });
        let report = check(
            &compiled(
                r#"
(claim "head"
  (claim "leaf" (witness "a.rs")))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let detail = &report.claims[0];
        assert_eq!(detail.value, Value::Unknown);
        assert_ne!(detail.value, Value::False);
        assert_eq!(detail.children[0].value, Value::True);
        assert_eq!(
            detail.implication.as_ref().unwrap().failure,
            Some(Failure::NotEntailed)
        );
    }

    #[test]
    fn a_broken_subproof_does_not_make_the_head_false() {
        let dir = scratch("broken");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled(
                r#"
(claim "wrap"
  (not (claim "head"
    (claim "nope" (witness "a.rs")))))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let not = &report.claims[0].children[0];
        assert_eq!(not.children[0].children[0].value, Value::False);
        assert_eq!(not.children[0].value, Value::Unknown);
        assert_eq!(not.value, Value::Unknown);
    }

    fn route(req: &Classification) -> Answer {
        match req.instructions["proposition"].as_str() {
            Some("route A") | Some("A") => choice("entailed", 0.95),
            Some("route B") | Some("B") => choice("contradicted", 0.95),
            _ => choice("neutral", 0.95),
        }
    }

    fn x_holds_false(req: &Classification) -> Answer {
        if req.instructions["proposition"].as_str() == Some("X holds") {
            choice("contradicted", 0.95)
        } else {
            choice("neutral", 0.95)
        }
    }

    #[test]
    fn an_or_of_witness_claims_passes_when_one_claim_is_true() {
        let dir = scratch("or-form");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        fs::write(dir.join("b.rs"), "bbb").unwrap();
        let mut oracle = Rec::new(route);
        let report = check(
            &compiled(
                r#"
(or
  (claim "route A" (witness "a.rs"))
  (claim "route B" (witness "b.rs")))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 2);
        assert!(oracle
            .seen
            .iter()
            .all(|req| req.state.get("sentences").is_none()));
        let or = &report.claims[0];
        assert_eq!(or.value, Value::True);
        assert_eq!(or.children[0].value, Value::True);
        assert_eq!(or.children[1].value, Value::False);
    }

    #[test]
    fn sibling_claims_are_both_required() {
        let dir = scratch("siblings");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        fs::write(dir.join("b.rs"), "bbb").unwrap();
        let mut oracle = Rec::new(route);
        let report = check(
            &compiled(
                r#"
(claim "route A" (witness "a.rs"))
(claim "route B" (witness "b.rs"))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[1].value, Value::False);
        assert!(report.claims.iter().any(|claim| claim.value != Value::True));
    }

    #[test]
    fn a_top_level_not_is_true_only_when_the_claim_is_false() {
        let dir = scratch("not-form");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let src = "(not (claim \"X holds\" (witness \"a.rs\")))";
        let mut oracle = Rec::new(x_holds_false);
        let report = check(&compiled(src), &dir, DEFAULT_BODY_BYTES, &mut oracle).unwrap();
        assert_eq!(oracle.seen.len(), 1);
        assert!(oracle.seen[0].state.get("sentences").is_none());
        assert_eq!(report.claims[0].children[0].value, Value::False);
        assert_eq!(report.claims[0].value, Value::True);

        let mut oracle = Rec::new(neutral);
        let report = check(&compiled(src), &dir, DEFAULT_BODY_BYTES, &mut oracle).unwrap();
        assert_eq!(report.claims[0].children[0].value, Value::Unknown);
        assert_eq!(report.claims[0].value, Value::Unknown);
    }

    #[test]
    fn a_detail_of_an_or_discharges_when_the_or_does() {
        let dir = scratch("detail-or");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        fs::write(dir.join("b.rs"), "bbb").unwrap();
        let mut oracle = Rec::new(route);
        let report = check(
            &compiled(
                r#"
(detail
  (or
    (claim "A" (witness "a.rs"))
    (claim "B" (witness "b.rs"))))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let detail = &report.claims[0];
        assert_eq!(detail.value, Value::True);
        assert_eq!(detail.children[0].value, Value::True);
        assert_eq!(detail.children[0].children[1].value, Value::False);
    }

    #[test]
    fn negation_of_a_contradicted_witness_is_true() {
        let dir = scratch("absent");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled("(claim \"outer\" (not (claim \"nope\" (witness \"a.rs\"))))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let not = &report.claims[0].children[0];
        assert_eq!(not.children[0].value, Value::False);
        assert_eq!(not.value, Value::True);
    }

    #[test]
    fn an_outer_detail_sees_the_inner_head() {
        let dir = scratch("nested");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        check(
            &compiled(
                r#"
(claim "outer"
  (claim "inner"
    (claim "holds in the file" (witness "a.rs"))))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let outer = oracle
            .seen
            .iter()
            .find(|req| req.instructions["proposition"].as_str() == Some("outer"))
            .unwrap();
        assert_eq!(outer.state["sentences"][0].as_str().unwrap(), "inner");
    }

    #[test]
    fn a_long_file_discharges_on_the_part_that_entails_it() {
        let dir = scratch("parts");
        fs::write(dir.join("big.rs"), "aaaa\nbbbb\ncc").unwrap();
        let mut oracle = Rec::new(|req| {
            let part = req.state.get("part").and_then(|value| value.as_u64());
            if part == Some(2) {
                choice("entailed", 0.95)
            } else {
                choice("neutral", 0.95)
            }
        });
        let report = check(
            &compiled("(claim \"in the second part\" (witness \"big.rs\"))"),
            &dir,
            8,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 2);
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[0].cites, vec!["big.rs part 2/2".to_string()]);
        assert!(write_report(&report).contains("big.rs part 2/2"));
    }

    #[test]
    fn a_joint_witness_over_a_long_file_is_too_large() {
        let dir = scratch("joint-large");
        fs::write(dir.join("big.rs"), "aaaaaaaaaa").unwrap();
        fs::write(dir.join("small.rs"), "bb").unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"both\" (witness \"big.rs\" \"small.rs\"))"),
            &dir,
            8,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle.seen.is_empty());
        assert_eq!(report.claims[0].failure, Some(Failure::TooLarge));
        assert_eq!(report.claims[0].value, Value::Unknown);
    }

    #[test]
    fn entailed_under_the_bar_does_not_discharge() {
        let dir = scratch("bar");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(|_| choice("entailed", 0.84));
        let report = check(
            &compiled("(claim \"leaf\" (witness \"a.rs\"))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::Unknown);
        assert_eq!(report.claims[0].failure, Some(Failure::NotEntailed));
    }

    #[test]
    fn a_lemma_use_takes_the_proved_value() {
        let dir = scratch("lemma");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled(
                r#"
(sense lemma-1 "lemma holds")
(sense theorem "theorem holds")
(claim (theorem)
  (lemma-1))
(claim (lemma-1)
  (claim "holds in the file" (witness "a.rs")))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 3);
        let theorem_step = oracle
            .seen
            .iter()
            .find(|req| req.instructions["proposition"].as_str() == Some("theorem holds"))
            .unwrap();
        assert_eq!(
            theorem_step.state["sentences"][0].as_str().unwrap(),
            "lemma holds"
        );
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[0].children[0].value, Value::True);
        assert!(report.claims[0].children[0].children.is_empty());
        assert_eq!(report.claims[0].children[0].failure, None);
        assert_eq!(report.claims[1].value, Value::True);
    }

    #[test]
    fn an_undischarged_lemma_use_stays_unknown() {
        let dir = scratch("lemma-open");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled(
                r#"
(claim "theorem holds"
  "lemma")
(claim "lemma"
  (claim "nope" (witness "a.rs")))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[1].value, Value::Unknown);
        assert_eq!(report.claims[0].children[0].value, Value::Unknown);
        assert_eq!(report.claims[0].children[0].failure, None);
        assert_eq!(report.claims[0].value, Value::Unknown);
        assert!(!write_report(&report).contains("unpinned"));
    }

    #[test]
    fn split_body_keeps_lines_and_a_long_line_and_a_wide_character() {
        assert_eq!(
            split_body("aaaa\nbbbb\ncc", 8),
            vec!["aaaa\n".to_string(), "bbbb\ncc".to_string()]
        );
        assert_eq!(
            split_body("abcdefghij", 4),
            vec!["abcd".to_string(), "efgh".to_string(), "ij".to_string()]
        );
        assert_eq!(
            split_body("ééé", 3),
            vec!["é".to_string(), "é".to_string(), "é".to_string()]
        );
        let joined = split_body("aaaa\nbbbb\ncc", 8).concat();
        assert_eq!(joined, "aaaa\nbbbb\ncc");
    }

    #[test]
    fn an_assumption_discharges_a_citing_claim_without_a_file_question() {
        let dir = scratch("assume");
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled(
                r#"
(assume "the file is small")
(claim "the brief may treat the file as small" "the file is small")
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 1);
        assert!(oracle.seen[0].state.get("sentences").is_some());
        assert_eq!(
            oracle.seen[0].state["sentences"][0].as_str().unwrap(),
            "the file is small"
        );
        assert!(report.claims[0].assumed);
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[1].value, Value::True);
        assert!(oracle.seen[0].state.get("path").is_none());
    }

    #[test]
    fn a_contradicted_implication_makes_the_claim_false() {
        let dir = scratch("idb-false");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled(
                "(claim \"outer\" (not (claim \"nope\" (claim \"holds in the file\" (witness \"a.rs\")))))",
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let not = &report.claims[0].children[0];
        let claim = &not.children[0];
        assert_eq!(claim.children[0].value, Value::True);
        assert_eq!(claim.value, Value::False);
        assert_eq!(claim.failure, Some(Failure::Contradicted));
        assert_eq!(not.value, Value::True);
    }

    #[test]
    fn a_failed_support_keeps_a_contradicted_claim_unknown() {
        let dir = scratch("idb-open");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled("(claim \"nope\" (claim \"nope too\" (witness \"a.rs\")))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let claim = &report.claims[0];
        assert_eq!(claim.children[0].value, Value::False);
        assert_eq!(claim.value, Value::Unknown);
        assert_eq!(claim.failure, None);
    }

    fn rejected_image(label: &str, name: &str, bytes: &[u8], failure: Failure, comment: &str) {
        let dir = scratch(label);
        fs::write(dir.join(name), bytes).unwrap();
        let mut oracle = Rec::new(entailed);
        let src = format!("(claim \"shot\" (witness \"{name}\"))");
        let report = check(&compiled(&src), &dir, 32, &mut oracle).unwrap();
        assert!(oracle.seen.is_empty(), "{label}");
        assert_eq!(report.claims[0].failure, Some(failure), "{label}");
        assert_eq!(report.claims[0].value, Value::Unknown, "{label}");
        assert!(write_report(&report).contains(comment), "{label}");
    }

    #[test]
    fn a_png_witness_sends_one_asset_and_is_not_sliced() {
        use crate::oracle::image::{synthetic_png, ImageFormat};
        let dir = scratch("png");
        let png = synthetic_png(280, 280);
        fs::write(dir.join("shot.png"), &png).unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"the shot\" (witness \"shot.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        let state = &oracle.seen[0].state;
        assert!(state.get("part").is_none());
        assert_eq!(state["path"], "shot.png");
        let reference = state["body"].as_str().unwrap();
        assert!(reference.starts_with("img-"));
        assert_eq!(reference.len(), 4 + 64);
        assert_eq!(state.assets().len(), 1);
        assert_eq!(state.assets()[0].reference(), reference);
        assert_eq!(state.assets()[0].format(), ImageFormat::Png);
        assert_eq!(state.assets()[0].bytes(), png.as_slice());
    }

    #[test]
    fn an_oversized_png_is_scaled_and_sent() {
        use crate::oracle::image::solid_png;
        let dir = scratch("scaled-png");
        let png = solid_png(2000, 1000);
        let path = dir.join("shot.png");
        fs::write(&path, &png).unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"the shot\" (witness \"shot.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert!(report.claims[0].failure.is_none());
        assert_eq!(oracle.seen.len(), 1);
        assert!(oracle.seen[0].state.get("part").is_none());
        assert_eq!(oracle.seen[0].state["path"], "shot.png");
        assert_ne!(oracle.seen[0].state.assets()[0].bytes(), png.as_slice());
        assert_eq!(fs::read(&path).unwrap(), png);
    }

    #[test]
    fn a_jpeg_witness_uses_the_jpg_format() {
        use crate::oracle::image::{synthetic_jpeg, ImageFormat};
        let dir = scratch("jpeg");
        fs::write(dir.join("shot.jpeg"), synthetic_jpeg(280, 280)).unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"the shot\" (witness \"shot.jpeg\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        assert!(oracle.seen[0].state.get("part").is_none());
        assert_eq!(oracle.seen[0].state["path"], "shot.jpeg");
        assert_eq!(oracle.seen[0].state.assets()[0].format(), ImageFormat::Jpeg);
        assert_eq!(oracle.seen[0].state.assets()[0].format().as_str(), "jpg");
    }

    #[test]
    fn a_joint_witness_keeps_text_beside_one_image() {
        use crate::oracle::image::synthetic_png;
        let dir = scratch("joint-image");
        fs::write(dir.join("note.txt"), "hello").unwrap();
        fs::write(dir.join("shot.png"), synthetic_png(280, 280)).unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"together\" (witness \"note.txt\" \"shot.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        let state = &oracle.seen[0].state;
        assert!(state.get("part").is_none());
        let files = state["files"].as_array().unwrap();
        assert_eq!(files[0]["body"].as_str().unwrap(), "hello");
        assert!(files[1]["body"].as_str().unwrap().starts_with("img-"));
        assert_eq!(state.assets().len(), 1);
        assert_eq!(
            state.assets()[0].reference(),
            files[1]["body"].as_str().unwrap()
        );
    }

    #[test]
    fn two_copies_of_one_image_are_one_asset() {
        use crate::oracle::image::synthetic_png;
        let dir = scratch("dedup");
        let png = synthetic_png(280, 280);
        fs::write(dir.join("a.png"), &png).unwrap();
        fs::write(dir.join("b.png"), &png).unwrap();
        let mut oracle = Rec::new(entailed);
        check(
            &compiled("(claim \"same\" (witness \"a.png\" \"b.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(oracle.seen.len(), 1);
        let state = &oracle.seen[0].state;
        let files = state["files"].as_array().unwrap();
        assert_eq!(files[0]["body"], files[1]["body"]);
        assert_ne!(files[0]["path"], files[1]["path"]);
        assert_eq!(state.assets().len(), 1);
        assert_eq!(
            state.assets()[0].reference(),
            files[0]["body"].as_str().unwrap()
        );
    }

    #[test]
    fn a_rejected_image_is_not_sent() {
        use crate::oracle::image::{synthetic_jpeg, synthetic_png};
        rejected_image(
            "tiny-image",
            "shot.png",
            &synthetic_png(100, 100),
            Failure::ImageTooSmall,
            ";; image too small",
        );
        rejected_image(
            "huge-image",
            "shot.png",
            &synthetic_png(4000, 4000),
            Failure::ImageTooLarge,
            ";; image too large",
        );
        rejected_image(
            "mislabeled-image",
            "shot.png",
            &synthetic_jpeg(280, 280),
            Failure::UnsupportedImage,
            ";; unsupported image",
        );
        rejected_image(
            "text-png",
            "shot.png",
            b"not an image",
            Failure::UnsupportedImage,
            ";; unsupported image",
        );
    }

    #[test]
    fn an_image_over_sixteen_mebibytes_is_too_large() {
        let dir = scratch("huge-file");
        let path = dir.join("big.png");
        let file = fs::File::create(&path).unwrap();
        file.set_len(MAX_IMAGE_BYTES as u64 + 1).unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim \"shot\" (witness \"big.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle.seen.is_empty());
        assert_eq!(report.claims[0].failure, Some(Failure::ImageTooLarge));
        assert!(write_report(&report).contains(";; image too large"));
    }

    #[test]
    fn a_dollar_witness_reads_the_bound_file() {
        let dir = scratch("dollar");
        let file = dir.join("note.txt");
        fs::write(&file, "alpha\n").unwrap();
        let bound = BoundFile {
            cite: "note.txt".to_string(),
            path: file,
        };
        let mut oracle = Rec::new(entailed);
        let report = check_bound(
            &compiled("(claim \"alpha\" (witness $))"),
            Path::new("/tmp"),
            DEFAULT_BODY_BYTES,
            Some(&bound),
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        assert_eq!(oracle.seen[0].state["path"], "note.txt");
        assert_eq!(oracle.seen[0].state["body"], "alpha\n");
    }

    #[test]
    fn a_dollar_without_a_file_submits_nothing() {
        let mut oracle = Rec::new(entailed);
        let err = match submit(
            &compiled("(claim \"alpha\" (witness $))"),
            Path::new("."),
            DEFAULT_BODY_BYTES,
            None,
            &mut oracle,
        ) {
            Err(err) => err,
            Ok(_) => panic!("`$` without a file submitted tickets"),
        };
        assert!(err.to_string().contains("`$` needs a file"), "{err}");
        assert!(oracle.seen.is_empty());
    }

    #[test]
    fn a_file_without_a_dollar_submits_nothing() {
        let bound = BoundFile {
            cite: "note.txt".to_string(),
            path: PathBuf::from("note.txt"),
        };
        let mut oracle = Rec::new(entailed);
        let err = match submit(
            &compiled("(claim \"alpha\" (witness \"note.txt\"))"),
            Path::new("."),
            DEFAULT_BODY_BYTES,
            Some(&bound),
            &mut oracle,
        ) {
            Err(err) => err,
            Ok(_) => panic!("a file without `$` submitted tickets"),
        };
        assert!(
            err.to_string().contains("a file needs `(witness $)`"),
            "{err}"
        );
        assert!(oracle.seen.is_empty());
    }

    #[test]
    fn a_term_is_shown_by_its_name_and_asked_by_its_text() {
        let dir = scratch("term");
        fs::write(dir.join("logic.py"), "output_table_names\n").unwrap();
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled(
                r#"
(name LogicReasoner)
(sense LogicOutputTables "the tables named in a logic block's output_table_names")
(sense creates "{reasoner} plans deploy units that create {object}")
(claim (creates LogicReasoner (LogicOutputTables))
  (witness "logic.py"))
(claim "the plan names those tables"
  (creates LogicReasoner (LogicOutputTables)))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(
            report.claims[0].text,
            "LogicReasoner plans deploy units that create LogicOutputTables"
        );
        assert_eq!(
            oracle.seen[0].instructions["proposition"].as_str().unwrap(),
            "LogicReasoner plans deploy units that create the tables named in a logic block's output_table_names"
        );
        let premises = oracle
            .seen
            .iter()
            .find(|req| req.state.get("sentences").is_some())
            .unwrap();
        assert_eq!(
            premises.state["sentences"][0].as_str().unwrap(),
            "LogicReasoner plans deploy units that create the tables named in a logic block's output_table_names"
        );
        assert!(write_report(&report)
            .contains("LogicReasoner plans deploy units that create LogicOutputTables"));
        assert!(!write_report(&report).contains("output_table_names"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn define_evaluates_deterministically_and_passes_sense_to_implications() {
        let dir = scratch("define");
        fs::write(dir.join("cust.txt"), "Acme is a customer\n").unwrap();
        fs::write(dir.join("sales.txt"), "Acme sales\n").unwrap();
        let mut oracle = Rec::new(|req| {
            let prop = req.instructions["proposition"].as_str().unwrap_or("");
            if prop.contains("Acme") {
                choice("entailed", 0.95)
            } else {
                choice("contradicted", 0.95)
            }
        });
        let report = check(
            &compiled(
                r#"
(name Acme)
(name Globex)
(sense customer "{c} is a customer")
(sense hv "{c} is a high-value customer")
(define (hv c)
  (and (customer c) ("{c} has > $25M sales" c)))

(claim (customer Acme) (witness "cust.txt"))
(claim ("{c} has > $25M sales" Acme) (witness "sales.txt"))
(claim (customer Globex) (witness "cust.txt"))
(claim ("{c} has > $25M sales" Globex) (witness "sales.txt"))

(claim "Acme gets priority" (hv Acme))
(not (hv Globex))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[4].value, Value::True);
        assert_eq!(report.claims[5].value, Value::True);
        let implications: Vec<_> = oracle
            .seen
            .iter()
            .filter(|req| req.state.get("sentences").is_some())
            .collect();
        assert_eq!(implications.len(), 1);
        assert_eq!(
            implications[0].state["sentences"][0].as_str().unwrap(),
            "Acme is a high-value customer"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ground_checks_imported_certificate_once_and_enforces_soundness() {
        let dir = scratch("ground-linked");
        fs::create_dir_all(dir.join("generators/hv")).unwrap();
        fs::create_dir_all(dir.join("analysis")).unwrap();
        fs::write(dir.join("generators/hv/query.sql"), "SELECT ...\n").unwrap();
        fs::write(
            dir.join("generators/hv/out.csv"),
            "customer_id,is_high_value\nAcme,true\nGlobex,false\n",
        )
        .unwrap();

        let files = vec![
            crate::Loaded {
                path: "ontology.brief".into(),
                program: parse(
                    r#"
(ns acme.ontology)
(sense customer "{c} is a customer")
(sense hv "{c} is a high-value customer")
(define (hv c)
  (and (customer c) ("{c} has > $25M sales" c)))
"#,
                )
                .unwrap(),
            },
            crate::Loaded {
                path: "generators/hv/certificate.brief".into(),
                program: parse(
                    r#"
(ns generators.hv (:require [acme.ontology :refer [hv]]))
(claim "query.sql selects hv customers" (witness "query.sql"))
(assume "out.csv is up to date")
(ground (hv c)
  (witness "out.csv")
  (and "query.sql selects hv customers" "out.csv is up to date"))
"#,
                )
                .unwrap(),
            },
        ];
        let entry = parse(
            r#"
(ns acme.analysis
  (:require [acme.ontology :refer [hv]] [generators.hv]))
(name Acme)
(name Globex)
(and (hv Acme))
(and (hv Globex))
"#,
        )
        .unwrap();
        let compiled = crate::link(&entry, Some("analysis/decision.brief"), &files).unwrap();

        let mut oracle = Rec::new(|req| {
            let prop = req.instructions["proposition"].as_str().unwrap_or("");
            if prop.starts_with("Globex") {
                choice("contradicted", 0.95)
            } else {
                choice("entailed", 0.95)
            }
        });
        let report = check(
            &compiled.claims,
            &dir.join("analysis"),
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[1].value, Value::Unknown);

        // Certificate checked once (1 witness for query.sql + 2 forall implications),
        // plus 2 instance checks against out.csv (Acme, Globex) = 5 total questions.
        let cert_questions = oracle
            .seen
            .iter()
            .filter(|req| {
                req.instructions["proposition"]
                    .as_str()
                    .is_some_and(|p| p.starts_with("For all c,"))
            })
            .count();
        assert_eq!(cert_questions, 2);
        assert_eq!(oracle.seen.len(), 5);

        // When the certificate fails, even an entailed witness stays Unknown.
        let mut failing_cert = Rec::new(|req| {
            let prop = req.instructions["proposition"].as_str().unwrap_or("");
            if prop.starts_with("For all c,") {
                choice("neutral", 0.95)
            } else {
                choice("entailed", 0.95)
            }
        });
        let failed = check(
            &compiled.claims,
            &dir.join("analysis"),
            DEFAULT_BODY_BYTES,
            &mut failing_cert,
        )
        .unwrap();
        assert_eq!(failed.claims[0].value, Value::Unknown);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bodyless_define_is_true_without_oracle_calls_and_supports_implications() {
        let dir = scratch("bodyless-define");
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled(
                r#"
(name CorporateAction)
(name Merger)
(sense is_a "{X} is a sub-type of {Y}")
(define (is_a Merger CorporateAction))
(and (is_a Merger CorporateAction))
(claim "mergers are corporate actions" (is_a Merger CorporateAction))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[1].value, Value::True);
        assert_eq!(oracle.seen.len(), 1);
        assert_eq!(
            oracle.seen[0].state["sentences"][0].as_str().unwrap(),
            "Merger is a sub-type of CorporateAction"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn certify_dependency_failure_blocks_downstream_certify_and_ground() {
        use crate::compiler::{link, Loaded};
        let dir = scratch("certify-deps");
        fs::create_dir_all(dir.join("impl")).unwrap();
        fs::create_dir_all(dir.join("run")).unwrap();
        fs::create_dir_all(dir.join("analysis")).unwrap();
        fs::write(dir.join("impl/cancel.py"), "canceled = odd_chain\n").unwrap();
        fs::write(dir.join("impl/ibor.py"), "erased = canceled & canceled_by\n").unwrap();
        fs::write(dir.join("run/erased.csv"), "tx,cancel\nTX1,C1\n").unwrap();

        let files = vec![
            Loaded {
                path: "ontology.brief".into(),
                program: parse(
                    r#"
(ns acme.ibor)
(sense stacked "{Transaction} has {Count} cancels stacked on it")
(sense is_odd "{Count} is odd")
(sense canceled_by "{Transaction} is canceled by {Cancel}")
(sense canceled "{Transaction} is canceled")
(sense erased "{Transaction} is erased by {Cancel}")
(define (canceled t) (exists [n] (and (stacked t n) (is_odd n))))
(define (erased t c) (and (canceled t) (canceled_by t c)))
"#,
                )
                .unwrap(),
            },
            Loaded {
                path: "impl/cert.brief".into(),
                program: parse(
                    r#"
(ns acme.ibor.impl (:require [acme.ibor :refer :all]))
(certify (canceled t) (witness "cancel.py"))
(certify (erased t c) (witness "ibor.py"))
"#,
                )
                .unwrap(),
            },
            Loaded {
                path: "run/ground.brief".into(),
                program: parse(
                    r#"
(ns acme.ibor.run
  (:require
    [acme.ibor :refer :all]
    [acme.ibor.impl]))
(ground (erased t c)
  (witness "erased.csv")
  (witness "../impl/ibor.py"))
"#,
                )
                .unwrap(),
            },
        ];
        let decision = parse(
            r#"
(ns acme.decision
  (:require
    [acme.ibor :refer :all]
    [acme.ibor.run]))
(name TX1)
(name C1)
(and (erased TX1 C1))
"#,
        )
        .unwrap();
        let compiled = link(&decision, Some("analysis/decision.brief"), &files).unwrap();

        let mut all_pass = Rec::new(entailed);
        let ok = check(
            &compiled.claims,
            &dir.join("analysis"),
            DEFAULT_BODY_BYTES,
            &mut all_pass,
        )
        .unwrap();
        assert_eq!(ok.claims[0].value, Value::True);

        // If the upstream `canceled` certify fails, `erased` certify, `ground`, and `(erased TX1 C1)` all fail closed.
        let mut bad_cancel = Rec::new(|req| {
            let prop = req.instructions["proposition"].as_str().unwrap_or("");
            if prop.contains("is canceled if and only if") {
                choice("contradicted", 0.95)
            } else {
                choice("entailed", 0.95)
            }
        });
        let blocked = check(
            &compiled.claims,
            &dir.join("analysis"),
            DEFAULT_BODY_BYTES,
            &mut bad_cancel,
        )
        .unwrap();
        assert_eq!(blocked.claims[0].value, Value::Unknown);
        assert_eq!(
            blocked.claims[0].children[0].failure,
            Some(Failure::CertificateFailed {
                cert: "certify:acme.ibor.impl:acme.ibor/canceled".into(),
            })
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn named_dollar_witnesses_resolve_file_and_dir_and_reject_missing_or_brief() {
        let dir = scratch("named-dollars");
        let run_dir = dir.join("run-1");
        fs::create_dir_all(&run_dir).unwrap();
        let diff_file = dir.join("patch.diff");
        fs::write(&diff_file, "+hello\n").unwrap();
        fs::write(run_dir.join("out.csv"), "customer_id\nAcme\n").unwrap();
        let brief_file = dir.join("model.brief");
        fs::write(&brief_file, "(assume \"x\")\n").unwrap();

        let program = parse(
            r#"
(param customer)
(claim ("{c} is in out.csv" customer) (witness $run/out.csv))
(claim "diff adds hello" (witness $diff))
"#,
        )
        .unwrap();
        let bindings = HashMap::from([("customer".to_string(), "Acme".to_string())]);
        let compiled = crate::compile_with_bindings(&program, &bindings).unwrap();

        // Missing `$run` / `$diff` binding fails before submitting any oracle calls.
        let mut oracle = Rec::new(entailed);
        let err = check_bound_with(
            &compiled.claims,
            &dir,
            DEFAULT_BODY_BYTES,
            &BoundWitnesses::default(),
            &mut oracle,
        )
        .unwrap_err();
        assert!(matches!(err, CheckError::Bind(_)), "{err:?}");
        assert!(oracle.seen.is_empty());

        // Bound file and bound directory both discharge; dir subpath cites only `out.csv`.
        let bound = BoundWitnesses {
            default: None,
            named: HashMap::from([
                ("run".to_string(), run_dir.clone()),
                ("diff".to_string(), diff_file.clone()),
            ]),
        };
        let mut oracle = Rec::new(entailed);
        let report =
            check_bound_with(&compiled.claims, &dir, DEFAULT_BODY_BYTES, &bound, &mut oracle)
                .unwrap();
        assert_eq!(report.claims[0].text, "Acme is in out.csv");
        assert_eq!(report.claims[0].value, Value::True);
        assert_eq!(report.claims[0].cites, vec!["out.csv"]);
        assert_eq!(report.claims[1].value, Value::True);
        assert_eq!(oracle.seen[0].state["path"], serde_json::json!("out.csv"));

        // Binding `$diff` to a `.brief` file is rejected as a brief witness.
        let bad_bound = BoundWitnesses {
            default: None,
            named: HashMap::from([
                ("run".to_string(), run_dir),
                ("diff".to_string(), brief_file),
            ]),
        };
        let mut oracle = Rec::new(entailed);
        let report = check_bound_with(
            &compiled.claims,
            &dir,
            DEFAULT_BODY_BYTES,
            &bad_bound,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[1].failure, Some(Failure::BriefWitness));

        let _ = fs::remove_dir_all(&dir);
    }
}
