//! Re-check a brief against the files it cites.
//!
//! A claim supported by a witness is one sentence judged against the bodies of
//! its paths. A claim supported by a sentence is discharged when that support
//! is true and those sentences entail the claim. When the support is true and
//! the sentences contradict the claim, the claim is false. A sentence that a
//! claim or an assumption defines is a use of that lemma: it takes the lemma's
//! value, and the proof is not asked again. `and`, `or`, and `not` stay Kleene.
//!
//! A file larger than the body budget is sliced into parts. The witness is
//! discharged when one part entails the sentence, so the proof cites the file
//! and the kernel finds the region. A joint witness that does not fit is too
//! large: the agent splits it into single-file witnesses, and each of those
//! can be sliced. An image witness is one PNG or JPEG and is not sliced. A
//! joint witness may mix text and images; the text still has to fit in one
//! state. A witness that cites a `.brief` file is not sent. A brief is a
//! model, not evidence.
//!
//! Check builds its own oracle requests. Sweep does not call these builders,
//! so the two commands can be calibrated apart.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value as Json};

use crate::confidence::SUPPORT;
use crate::oracle::image::{image_asset, image_extension, Asset, ImageError, MAX_IMAGE_BYTES};
use crate::oracle::{Answer, Classification, Oracle, OracleError, QuestionKind, State, Ticket};
use crate::syntax::{Proposition, Support};
use crate::value::Value;

/// Largest body sent in one request.
pub const DEFAULT_BODY_BYTES: usize = 128 * 1024;

/// A single file larger than this is too large to slice.
///
/// Sixty-four parts at the default body budget. Each part is its own request,
/// and the run cap is one cent.
pub(crate) const MAX_SLICED_BYTES: usize = DEFAULT_BODY_BYTES.saturating_mul(64);

const DOCUMENT_QUESTION: &str = "How does `body` at `path` ground the proposition?";
const PART_QUESTION: &str = "How does this part of `body` at `path` ground the proposition? Text outside this part is not visible.";
const JOINT_QUESTION: &str = "How do the files in `files` together ground the proposition?";
const IMPLICATION_QUESTION: &str = "Do the sentences establish the proposition?";
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
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CheckError::Oracle(err) => write!(f, "{err}"),
            CheckError::Io(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for CheckError {}

impl From<OracleError> for CheckError {
    fn from(err: OracleError) -> Self {
        CheckError::Oracle(err)
    }
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
    let defs = defined_heads(claims);
    let mut work = Vec::with_capacity(claims.len());
    for claim in claims {
        work.push(prepare(claim, &defs, base, budget, oracle)?);
    }
    let tickets = {
        let mut tickets = Vec::new();
        for item in &work {
            push_tickets(item, &mut tickets);
        }
        tickets
    };
    let answers = oracle.wait_many(&tickets);
    let mut cursor = 0;
    let mut judged = Vec::with_capacity(work.len());
    for item in work {
        judged.push(settle(item, &answers, &mut cursor)?);
    }
    let lemmas = lemma_values(&judged, &defs);
    let claims = judged.iter().map(|node| render(node, &lemmas)).collect();
    Ok(Report { claims })
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
        Proposition::Sentence(_) => {}
        Proposition::Claim { sentence, support } => {
            heads.insert(sentence.clone());
            if let Support::Sentence(inner) = support {
                collect_heads(inner, heads);
            }
        }
        Proposition::Assume(sentence) => {
            heads.insert(sentence.clone());
        }
        Proposition::Scope { body, .. } => {
            for proposition in body {
                collect_heads(proposition, heads);
            }
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
    Assume,
    Scope,
    And,
    Or,
    Not,
}

fn prepare<O: Oracle>(
    proposition: &Proposition,
    defs: &HashSet<String>,
    base: &Path,
    budget: usize,
    oracle: &mut O,
) -> Result<Work, CheckError> {
    match proposition {
        Proposition::Sentence(sentence) => {
            let defined = defs.contains(sentence);
            Ok(Work {
                text: sentence.clone(),
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
        Proposition::Claim { sentence, support } => match support {
            Support::Witness { paths } => prepare_witness(sentence, paths, base, budget, oracle),
            Support::Sentence(support) => {
                let child = prepare(support, defs, base, budget, oracle)?;
                let implication = Box::new(prepare_implication(sentence, support, oracle));
                Ok(Work {
                    text: sentence.clone(),
                    failure: None,
                    cites: Vec::new(),
                    children: vec![child],
                    implication: Some(implication),
                    asked: Vec::new(),
                    kind: Kind::Claim,
                })
            }
        },
        Proposition::Assume(sentence) => Ok(Work {
            text: sentence.clone(),
            failure: None,
            cites: Vec::new(),
            children: Vec::new(),
            implication: None,
            asked: Vec::new(),
            kind: Kind::Assume,
        }),
        Proposition::Scope { intent, body } => {
            let mut children = Vec::with_capacity(body.len());
            for claim in body {
                children.push(prepare(claim, defs, base, budget, oracle)?);
            }
            Ok(Work {
                text: intent.clone().unwrap_or_else(|| "detail".to_string()),
                failure: None,
                cites: Vec::new(),
                children,
                implication: None,
                asked: Vec::new(),
                kind: Kind::Scope,
            })
        }
        Proposition::And(xs) => prepare_bool("and", Kind::And, xs, defs, base, budget, oracle),
        Proposition::Or(xs) => prepare_bool("or", Kind::Or, xs, defs, base, budget, oracle),
        Proposition::Not(inner) => {
            let child = prepare(inner, defs, base, budget, oracle)?;
            Ok(Work {
                text: "not".to_string(),
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

fn prepare_bool<O: Oracle>(
    text: &str,
    kind: Kind,
    xs: &[Proposition],
    defs: &HashSet<String>,
    base: &Path,
    budget: usize,
    oracle: &mut O,
) -> Result<Work, CheckError> {
    let mut children = Vec::with_capacity(xs.len());
    for proposition in xs {
        children.push(prepare(proposition, defs, base, budget, oracle)?);
    }
    Ok(Work {
        text: text.to_string(),
        failure: None,
        cites: Vec::new(),
        children,
        implication: None,
        asked: Vec::new(),
        kind,
    })
}

fn prepare_witness<O: Oracle>(
    sentence: &str,
    paths: &[String],
    base: &Path,
    budget: usize,
    oracle: &mut O,
) -> Result<Work, CheckError> {
    let mut work = Work {
        text: sentence.to_string(),
        failure: None,
        cites: paths.to_vec(),
        children: Vec::new(),
        implication: None,
        asked: Vec::new(),
        kind: Kind::Leaf,
    };
    if paths.iter().any(|path| is_brief_file(path)) {
        work.failure = Some(Failure::BriefWitness);
        return Ok(work);
    }
    let mut located = Vec::with_capacity(paths.len());
    for path in paths {
        match locate(base, path) {
            Locate::Missing => {
                work.failure = Some(Failure::MissingFile { path: path.clone() });
                work.cites = vec![path.clone()];
                return Ok(work);
            }
            Locate::Io(err) => return Err(CheckError::Io(err)),
            Locate::File { full, len } => located.push((path.clone(), full, len)),
        }
    }
    let mut files = Vec::with_capacity(located.len());
    for (path, full, len) in located {
        match load_witness(&path, &full, len)? {
            WitnessLoad::Reject(failure) => {
                work.failure = Some(failure);
                return Ok(work);
            }
            WitnessLoad::File(file) => files.push(file),
        }
    }
    let budget = budget.max(1);
    if files.len() == 1 {
        return submit_one(sentence, files.pop().unwrap(), budget, oracle, work);
    }
    submit_joint(sentence, files, budget, oracle, work)
}

enum WitnessLoad {
    File(WitnessFile),
    Reject(Failure),
}

enum WitnessFile {
    Text {
        path: String,
        full: PathBuf,
        len: u64,
    },
    Image {
        path: String,
        asset: Asset,
    },
}

fn load_witness(path: &str, full: &Path, len: u64) -> Result<WitnessLoad, CheckError> {
    let Some(format) = image_extension(path) else {
        return Ok(WitnessLoad::File(WitnessFile::Text {
            path: path.to_string(),
            full: full.to_path_buf(),
            len,
        }));
    };
    if len > MAX_IMAGE_BYTES as u64 {
        return Ok(WitnessLoad::Reject(Failure::ImageTooLarge));
    }
    let bytes =
        fs::read(full).map_err(|err| CheckError::Io(format!("{}: {err}", full.display())))?;
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
        WitnessFile::Text { path, full, len } => {
            if len > MAX_SLICED_BYTES as u64 {
                work.failure = Some(Failure::TooLarge);
                return Ok(work);
            }
            let body = read_body(&full)?;
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
            WitnessFile::Text { path, full, .. } => {
                bodies.push(json!({ "path": path, "body": read_body(&full)? }));
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

fn prepare_implication<O: Oracle>(head: &str, support: &Proposition, oracle: &mut O) -> Work {
    let premises = premise_sentences(std::slice::from_ref(support));
    let mut work = Work {
        text: IMPLICATION_TEXT.to_string(),
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
    let class = implication_question(head, &premises);
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
    let state = match part {
        Some((index, total)) => State::new(json!({
            "path": path,
            "body": body,
            "part": index,
            "parts": total,
        })),
        None => State::new(json!({
            "path": path,
            "body": body,
        })),
    };
    Classification {
        state,
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": sentence,
            "question": if part.is_some() { PART_QUESTION } else { DOCUMENT_QUESTION },
        }),
        criteria: document_criteria(),
    }
}

fn document_image(sentence: &str, path: &str, asset: Asset) -> Classification {
    let reference = asset.reference().to_string();
    Classification {
        state: State::with_assets(
            json!({
                "path": path,
                "body": reference,
            }),
            vec![asset],
        ),
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": sentence,
            "question": DOCUMENT_QUESTION,
        }),
        criteria: document_criteria(),
    }
}

fn joint_question(sentence: &str, files: Vec<Json>, assets: Vec<Asset>) -> Classification {
    Classification {
        state: State::with_assets(json!({ "files": files }), assets),
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": sentence,
            "question": JOINT_QUESTION,
        }),
        criteria: document_criteria(),
    }
}

fn implication_question(head: &str, premises: &[String]) -> Classification {
    Classification {
        state: State::new(json!({ "sentences": premises })),
        kind: QuestionKind::Choice,
        instructions: json!({
            "proposition": head,
            "question": IMPLICATION_QUESTION,
        }),
        criteria: json!({
            "entailed": "the sentences together establish the proposition",
            "contradicted": "the sentences rule the proposition out",
            "neutral": "the sentences do not settle the proposition",
        }),
    }
}

fn document_criteria() -> Json {
    json!({
        "entailed": "`body` supplies information that makes the proposition true",
        "contradicted": "`body` supplies information that makes the proposition false",
        "neutral": "`body` neither supports nor refutes the proposition",
    })
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
            sentences.push(sentence.clone());
        }
        Proposition::Claim { sentence, .. } => sentences.push(sentence.clone()),
        Proposition::And(xs) => {
            for proposition in xs {
                push_premises(proposition, sentences);
            }
        }
        Proposition::Or(_) | Proposition::Not(_) => sentences.push(statement(proposition)),
        Proposition::Scope { .. } => {}
    }
}

fn statement(proposition: &Proposition) -> String {
    match proposition {
        Proposition::Sentence(sentence) | Proposition::Assume(sentence) => sentence.clone(),
        Proposition::Claim { sentence, .. } => sentence.clone(),
        Proposition::And(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" and "),
        Proposition::Or(xs) => xs.iter().map(statement).collect::<Vec<_>>().join(" or "),
        Proposition::Not(inner) => format!("not ({})", statement(inner)),
        Proposition::Scope { intent, .. } => intent.clone().unwrap_or_else(|| "detail".to_string()),
    }
}

enum Locate {
    Missing,
    Io(String),
    File { full: PathBuf, len: u64 },
}

fn locate(base: &Path, pin: &str) -> Locate {
    let path = Path::new(pin);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::Prefix(_)))
    {
        return Locate::Missing;
    }
    let full = base.join(path);
    match fs::metadata(&full) {
        Ok(meta) if meta.is_file() => Locate::File {
            full,
            len: meta.len(),
        },
        Ok(_) => Locate::Missing,
        Err(err) if err.kind() == io::ErrorKind::NotFound => Locate::Missing,
        Err(err) => Locate::Io(format!("{pin}: {err}")),
    }
}

fn read_body(path: &Path) -> Result<String, CheckError> {
    let bytes =
        fs::read(path).map_err(|err| CheckError::Io(format!("{}: {err}", path.display())))?;
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
        Kind::Use | Kind::Claim | Kind::Assume | Kind::Scope | Kind::And | Kind::Or | Kind::Not => {
            None
        }
    };
    Ok(Judged {
        text: work.text,
        kind: work.kind,
        children,
        implication,
        leaf,
    })
}

fn lemma_values(nodes: &[Judged], defs: &HashSet<String>) -> HashMap<String, Value> {
    let mut values = HashMap::new();
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
        let defines = match node.kind {
            Kind::Claim | Kind::Assume => true,
            Kind::Leaf => defs.contains(&node.text),
            Kind::Use | Kind::Scope | Kind::And | Kind::Or | Kind::Not => false,
        };
        if defines {
            let value = eval_value(node, values);
            if values.get(&node.text) != Some(&value) {
                values.insert(node.text.clone(), value);
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
        Kind::Assume => Value::True,
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
    }
}

fn render(node: &Judged, lemmas: &HashMap<String, Value>) -> Node {
    let children = node
        .children
        .iter()
        .map(|child| render(child, lemmas))
        .collect();
    let implication = node
        .implication
        .as_ref()
        .map(|step| Box::new(render(step, lemmas)));
    let (failure, cites) = match node.kind {
        Kind::Claim => (claim_failure(node, lemmas), Vec::new()),
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
fn claim_failure(node: &Judged, lemmas: &HashMap<String, Value>) -> Option<Failure> {
    let support_holds = node
        .children
        .first()
        .is_some_and(|child| eval_value(child, lemmas) == Value::True);
    if !support_holds {
        return None;
    }
    let step = node.implication.as_ref()?;
    if eval_value(step, lemmas) == Value::True {
        return None;
    }
    step.leaf.as_ref().and_then(|leaf| leaf.1.clone())
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
    if answer.confidence < SUPPORT {
        return None;
    }
    match answer.choice.as_str() {
        "entailed" => Some(Value::True),
        "contradicted" => Some(Value::False),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::syntax::parse;
    use std::collections::HashMap;
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
            &compiled("(\"Search calls Oracle\")"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle.seen.is_empty());
        assert_eq!(report.claims[0].value, Value::Unknown);
        assert_eq!(report.claims[0].failure, Some(Failure::Unpinned));
        assert!(write_report(&report).contains(";; unpinned"));
    }

    #[test]
    fn a_missing_file_is_its_own_failure() {
        let dir = scratch("missing");
        let mut oracle = Rec::new(entailed);
        let report = check(
            &compiled("(claim (\"Search calls Oracle\") (witness \"src/nope.rs\"))"),
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
            "(claim (\"alpha\") (witness \"notes.brief\"))",
            "(claim (\"alpha\") (witness \"../Notes.BRIEF\"))",
            "(claim (\"alpha\") (witness \".brief\"))",
            "(claim (\"alpha\") (witness \"note.txt\" \"notes.brief\"))",
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
            &compiled("(claim (\"alpha\") (witness \"file.briefing\"))"),
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
            &compiled("(claim (\"alpha\") (witness \"../note.txt\"))"),
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
            &compiled("(claim (\"search finds the window\") (witness \"src/search.rs\"))"),
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
            &compiled("(claim (\"together\") (witness \"src/a.rs\" \"src/b.rs\"))"),
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
(and
  (claim ("one") (witness "a.rs"))
  (claim ("two") (witness "a.rs")))
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
(claim ("holds")
  (claim ("holds in the file") (witness "a.rs")))
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
(claim ("head")
  (claim ("leaf") (witness "a.rs")))
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
(not (claim ("head")
  (claim ("nope") (witness "a.rs"))))
"#,
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let not = &report.claims[0];
        assert_eq!(not.children[0].children[0].value, Value::False);
        assert_eq!(not.children[0].value, Value::Unknown);
        assert_eq!(not.value, Value::Unknown);
    }

    #[test]
    fn negation_of_a_contradicted_witness_is_true() {
        let dir = scratch("absent");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled("(not (claim (\"nope\") (witness \"a.rs\")))"),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(report.claims[0].children[0].value, Value::False);
        assert_eq!(report.claims[0].value, Value::True);
    }

    #[test]
    fn an_outer_detail_sees_the_inner_head() {
        let dir = scratch("nested");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        check(
            &compiled(
                r#"
(claim ("outer")
  (claim ("inner")
    (claim ("holds in the file") (witness "a.rs"))))
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
            &compiled("(claim (\"in the second part\") (witness \"big.rs\"))"),
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
            &compiled("(claim (\"both\") (witness \"big.rs\" \"small.rs\"))"),
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
            &compiled("(claim (\"leaf\") (witness \"a.rs\"))"),
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
  (claim ("holds in the file") (witness "a.rs")))
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
(claim ("theorem holds")
  ("lemma"))
(claim ("lemma")
  (claim ("nope") (witness "a.rs")))
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
(assume ("the file is small"))
(claim ("the brief may treat the file as small") ("the file is small"))
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
                "(not (claim (\"nope\") (claim (\"holds in the file\") (witness \"a.rs\"))))",
            ),
            &dir,
            DEFAULT_BODY_BYTES,
            &mut oracle,
        )
        .unwrap();
        let claim = &report.claims[0].children[0];
        assert_eq!(claim.children[0].value, Value::True);
        assert_eq!(claim.value, Value::False);
        assert_eq!(claim.failure, Some(Failure::Contradicted));
        assert_eq!(report.claims[0].value, Value::True);
    }

    #[test]
    fn a_failed_support_keeps_a_contradicted_claim_unknown() {
        let dir = scratch("idb-open");
        fs::write(dir.join("a.rs"), "aaa").unwrap();
        let mut oracle = Rec::new(by_proposition);
        let report = check(
            &compiled("(claim (\"nope\") (claim (\"nope too\") (witness \"a.rs\")))"),
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
        let src = format!("(claim (\"shot\") (witness \"{name}\"))");
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
            &compiled("(claim (\"the shot\") (witness \"shot.png\"))"),
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
            &compiled("(claim (\"the shot\") (witness \"shot.png\"))"),
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
            &compiled("(claim (\"the shot\") (witness \"shot.jpeg\"))"),
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
            &compiled("(claim (\"together\") (witness \"note.txt\" \"shot.png\"))"),
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
            &compiled("(claim (\"same\") (witness \"a.png\" \"b.png\"))"),
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
            &compiled("(claim (\"shot\") (witness \"big.png\"))"),
            &dir,
            32,
            &mut oracle,
        )
        .unwrap();
        assert!(oracle.seen.is_empty());
        assert_eq!(report.claims[0].failure, Some(Failure::ImageTooLarge));
        assert!(write_report(&report).contains(";; image too large"));
    }
}
