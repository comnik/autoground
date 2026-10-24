//! Judge a brief against each candidate file by submitting it to check.
//!
//! The filesystem lists the working tree. A transcript and a git diff are
//! written out as one file per call or patch, then swept the same way.
//! `--filter` stays on the filesystem and finishes before any file is
//! submitted. Every candidate is one work item: the same brief, and that
//! file bound to `$`. Sweep submits every item before it waits. A file
//! matches when every top-level claim is true.

mod diff;
mod transcript;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::json;

use crate::check::{self, BoundFile, CheckError, DEFAULT_BODY_BYTES, MAX_SLICED_BYTES};
use crate::confidence::SUPPORT;
use crate::oracle::image::{image_asset, image_extension, MAX_IMAGE_BYTES};
use crate::oracle::jev::QUESTION_BUDGET;
use crate::oracle::{Answer, Classification, Oracle, OracleError, State, Ticket};
use crate::syntax::Proposition;
use crate::universe::{Filesystem, Universe, UniverseError};
use crate::value::Value;

const FILTER_QUESTION: &str = "Does `path` match `filter`?";

#[derive(Debug)]
pub enum SweepError {
    Io(String),
    Oracle(OracleError),
    Constraint(String),
}

impl std::fmt::Display for SweepError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SweepError::Io(message) | SweepError::Constraint(message) => write!(f, "{message}"),
            SweepError::Oracle(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for SweepError {}

/// One file whose every top-level claim is true.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: PathBuf,
    pub claims: Vec<String>,
}

/// One file whose request failed. The other files are still judged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub path: PathBuf,
    pub message: String,
}

/// The report so far, and a status line while the sweep is still running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub report: String,
    pub status: Option<String>,
}

/// Stdout for an agent, plus how far the sweep got.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub text: String,
    pub matches: Vec<Match>,
    pub errors: Vec<FileError>,
    pub incomplete: bool,
    pub judged: usize,
    pub candidates: usize,
}

pub fn stopped_message(cap_usd: f64, spent_usd: f64, judged: usize, candidates: usize) -> String {
    format!(
        "sweep stopped at the cost cap of ${cap_usd:.2} (spent ${spent_usd:.4}); {judged} of {candidates} files judged"
    )
}

pub fn failed_message(failed: usize) -> String {
    match failed {
        1 => "1 file failed".to_string(),
        n => format!("{n} files failed"),
    }
}

/// Where a sweep reads its candidates.
pub enum Scope<'a> {
    /// The filesystem at `root`.
    Files {
        root: &'a Path,
        hide: Option<&'a Path>,
    },
    /// A `chat_history.jsonl`, or a directory that contains one.
    Transcript(&'a Path),
    /// `git diff` at `root`. `git_args` are forwarded to git.
    Diff {
        root: &'a Path,
        git_args: &'a [String],
        hide: Option<&'a Path>,
    },
}

pub fn sweep<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    scope: Scope<'_>,
    filter: Option<&str>,
    oracle: &mut O,
) -> Result<Outcome, SweepError> {
    sweep_watching(claims, base, scope, filter, oracle, &mut |_| {})
}

/// Judge `claims` and call `watch` as the report changes.
///
/// `watch` sees the matches and errors so far, and a status line. The returned
/// [`Outcome`] is the finished report.
pub fn sweep_watching<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    scope: Scope<'_>,
    filter: Option<&str>,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Outcome, SweepError> {
    if !check::cites_dollar(claims) {
        return Err(SweepError::Constraint(
            "a sweep brief needs `(witness $)`".into(),
        ));
    }
    if filter.is_some() && !matches!(&scope, Scope::Files { .. }) {
        return Err(SweepError::Constraint(
            "`filter` applies to a filesystem sweep".into(),
        ));
    }
    let opened = open(scope)?;
    let paths = opened.ids()?;
    let kept = match filter {
        Some(filter) => match filter_paths(&paths, filter, oracle, watch)? {
            Filtered::Incomplete => {
                return Ok(render(Vec::new(), Vec::new(), true, 0, paths.len()));
            }
            Filtered::Kept(paths) => paths,
        },
        None => paths,
    };
    let mut candidates = Vec::new();
    for rel in &kept {
        if let Some(candidate) = opened.candidate(rel)? {
            candidates.push(candidate);
        }
    }
    judge(claims, base, candidates, oracle, watch)
}

struct Candidate {
    /// Printed path: absolute for a working-tree file, the git path, or the transcript id.
    label: PathBuf,
    /// File check reads. For a diff or a transcript this lives under `scratch`.
    path: PathBuf,
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn new() -> Result<Self, SweepError> {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join("autoground").join(format!(
            "sweep-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).map_err(|err| SweepError::Io(format!("creating {path:?}: {err}")))?;
        Ok(Self { path })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

enum Opened<'a> {
    Files {
        root: &'a Path,
        files: Filesystem,
    },
    Exploded {
        scratch: Scratch,
        items: Vec<Candidate>,
    },
}

fn open(scope: Scope<'_>) -> Result<Opened<'_>, SweepError> {
    match scope {
        Scope::Files { root, hide } => {
            let mut files = Filesystem::open(root).map_err(universe_io)?;
            if let Some(model) = hide {
                files.hide_model(model);
            }
            Ok(Opened::Files { root, files })
        }
        Scope::Transcript(path) => {
            let exchanges = transcript::exchanges(path).map_err(SweepError::Io)?;
            let scratch = Scratch::new()?;
            let owned: Vec<(&str, &str)> = exchanges
                .iter()
                .map(|exchange| (exchange.id.as_str(), exchange.body.as_str()))
                .collect();
            let items = write_bodies(&scratch, &owned)?;
            Ok(Opened::Exploded { scratch, items })
        }
        Scope::Diff {
            root,
            git_args,
            hide,
        } => {
            let patches = diff::changes(root, git_args, hide).map_err(SweepError::Io)?;
            let scratch = Scratch::new()?;
            let items = write_bodies(
                &scratch,
                &patches
                    .iter()
                    .map(|patch| (patch.id.as_str(), patch.body.as_str()))
                    .collect::<Vec<_>>(),
            )?;
            Ok(Opened::Exploded { scratch, items })
        }
    }
}

fn write_bodies(scratch: &Scratch, bodies: &[(&str, &str)]) -> Result<Vec<Candidate>, SweepError> {
    let mut items = Vec::with_capacity(bodies.len());
    for (id, body) in bodies {
        let rel = Path::new(id);
        if rel.is_absolute()
            || rel
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            return Err(SweepError::Io(format!("`{id}` is not a relative path")));
        }
        let path = scratch.path.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| SweepError::Io(format!("creating {}: {err}", parent.display())))?;
        }
        fs::write(&path, body)
            .map_err(|err| SweepError::Io(format!("writing {}: {err}", path.display())))?;
        items.push(Candidate {
            label: PathBuf::from(id),
            path,
        });
    }
    Ok(items)
}

impl Opened<'_> {
    fn ids(&self) -> Result<Vec<String>, SweepError> {
        match self {
            Opened::Files { files, .. } => readable_ids(files),
            Opened::Exploded { scratch, items } => {
                let _ = &scratch.path;
                Ok(items
                    .iter()
                    .map(|item| item.label.display().to_string())
                    .collect())
            }
        }
    }

    fn candidate(&self, rel: &str) -> Result<Option<Candidate>, SweepError> {
        match self {
            Opened::Files { root, .. } => {
                let path = root.join(rel);
                if !admit(&path)? {
                    return Ok(None);
                }
                Ok(Some(Candidate {
                    label: absolute(root, rel),
                    path,
                }))
            }
            Opened::Exploded { items, .. } => {
                let Some(item) = items.iter().find(|item| item.label.display().to_string() == rel) else {
                    return Ok(None);
                };
                if !admit(&item.path)? {
                    return Ok(None);
                }
                Ok(Some(Candidate {
                    label: item.label.clone(),
                    path: item.path.clone(),
                }))
            }
        }
    }
}

/// Files in the current view. A summarized database contributes its readable descendants.
fn readable_ids(universe: &impl Universe) -> Result<Vec<String>, SweepError> {
    let mut out = Vec::new();
    for id in universe.databases() {
        collect_readable(universe, id, &mut out)?;
    }
    Ok(out)
}

fn collect_readable(
    universe: &impl Universe,
    id: &str,
    out: &mut Vec<String>,
) -> Result<(), SweepError> {
    match universe.summary(id).map_err(universe_io)? {
        None => out.push(id.to_string()),
        Some(_) => {
            for child in universe.children(id).map_err(universe_io)? {
                collect_readable(universe, &child, out)?;
            }
        }
    }
    Ok(())
}

enum Filtered {
    Kept(Vec<String>),
    Incomplete,
}

fn filter_paths<O: Oracle>(
    paths: &[String],
    filter: &str,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Filtered, SweepError> {
    if paths.is_empty() {
        return Ok(Filtered::Kept(Vec::new()));
    }
    let mut pending: Vec<(Ticket, String)> = Vec::new();
    for chunk in paths.chunks(QUESTION_BUDGET) {
        let state = State::new(json!({ "paths": chunk }));
        for path in chunk {
            let name = path.rsplit('/').next().unwrap_or(path.as_str());
            let class = Classification::noul(
                state.clone(),
                json!({
                    "path": path,
                    "name": name,
                    "filter": filter,
                    "question": FILTER_QUESTION,
                }),
                json!({
                    "true": "The path matches the filter. It should be opened.",
                    "false": "The path does not match the filter, or the path alone does not show that it matches.",
                }),
            );
            pending.push((oracle.submit(class), path.clone()));
        }
    }
    let total = pending.len();
    let mut done = 0usize;
    let mut kept = Vec::new();
    let mut limited = false;
    emit(
        watch,
        String::new(),
        format!("filtering {done}/{total} paths"),
    );
    while !pending.is_empty() {
        for (path, answer) in take_ready(oracle, &mut pending)? {
            done += 1;
            match answer {
                Ok(answer) if matches_filter(&answer) => kept.push(path),
                Ok(_) => {}
                Err(OracleError::Limit(_)) => limited = true,
                Err(err) => return Err(SweepError::Oracle(err)),
            }
        }
        emit(
            watch,
            String::new(),
            format!("filtering {done}/{total} paths"),
        );
    }
    if limited {
        Ok(Filtered::Incomplete)
    } else {
        Ok(Filtered::Kept(kept))
    }
}

/// A Noul at the trust bar is a match. Anything weaker leaves the file out.
fn matches_filter(answer: &Answer) -> bool {
    answer.noul.unwrap_or(0.0) >= SUPPORT
}

fn judge<O: Oracle>(
    claims: &[Proposition],
    base: &Path,
    candidates: Vec<Candidate>,
    oracle: &mut O,
    watch: &mut dyn FnMut(&Snapshot),
) -> Result<Outcome, SweepError> {
    let total = candidates.len();
    if total == 0 {
        return Ok(render(Vec::new(), Vec::new(), false, 0, 0));
    }
    let mut jobs: Vec<Option<FileJob>> = Vec::with_capacity(total);
    let mut pending: Vec<(Ticket, Queued)> = Vec::new();
    for (index, candidate) in candidates.into_iter().enumerate() {
        let bound = BoundFile {
            cite: candidate.label.display().to_string(),
            path: candidate.path,
        };
        let prepared = check::submit(claims, base, DEFAULT_BODY_BYTES, Some(&bound), oracle)
            .map_err(check_err)?;
        for ticket in prepared.tickets() {
            pending.push((*ticket, Queued { ticket: *ticket, index }));
        }
        jobs.push(Some(FileJob {
            label: candidate.label,
            prepared,
            answers: HashMap::new(),
        }));
    }
    let mut matches = Vec::new();
    let mut errors = Vec::new();
    let mut incomplete = false;
    let mut judged = 0usize;
    let mut done = 0usize;
    emit_progress(watch, &mut matches, &mut errors, done, total);
    while settle_next(
        &mut jobs,
        &mut matches,
        &mut errors,
        &mut incomplete,
        &mut judged,
    ) {
        done += 1;
        emit_progress(watch, &mut matches, &mut errors, done, total);
    }
    while !pending.is_empty() {
        for (queued, answer) in take_ready(oracle, &mut pending)? {
            let job = jobs[queued.index].as_mut().expect("file still open");
            job.answers.insert(queued.ticket, answer);
        }
        while settle_next(
            &mut jobs,
            &mut matches,
            &mut errors,
            &mut incomplete,
            &mut judged,
        ) {
            done += 1;
            emit_progress(watch, &mut matches, &mut errors, done, total);
        }
    }
    Ok(render(matches, errors, incomplete, judged, total))
}

struct Queued {
    ticket: Ticket,
    index: usize,
}

struct FileJob {
    label: PathBuf,
    prepared: check::Prepared,
    answers: HashMap<Ticket, Result<Answer, OracleError>>,
}

/// Settle one file whose tickets have all arrived.
fn settle_next(
    jobs: &mut [Option<FileJob>],
    matches: &mut Vec<Match>,
    errors: &mut Vec<FileError>,
    incomplete: &mut bool,
    judged: &mut usize,
) -> bool {
    for slot in jobs.iter_mut() {
        let ready = slot
            .as_ref()
            .is_some_and(|job| job.answers.len() == job.prepared.tickets().len());
        if !ready {
            continue;
        }
        let job = slot.take().expect("ready file");
        settle_file(job, matches, errors, incomplete, judged);
        return true;
    }
    false
}

fn settle_file(
    job: FileJob,
    matches: &mut Vec<Match>,
    errors: &mut Vec<FileError>,
    incomplete: &mut bool,
    judged: &mut usize,
) {
    let answers: Vec<Result<Answer, OracleError>> = job
        .prepared
        .tickets()
        .iter()
        .map(|ticket| {
            job.answers
                .get(ticket)
                .cloned()
                .unwrap_or_else(|| Err(OracleError::Parse("missing oracle answer".into())))
        })
        .collect();
    let mut limited = false;
    let mut messages = Vec::new();
    for answer in &answers {
        match answer {
            Ok(_) => {}
            Err(OracleError::Limit(_)) => limited = true,
            Err(err) => note_error(&mut messages, err),
        }
    }
    if limited {
        *incomplete = true;
    }
    if !messages.is_empty() {
        errors.push(FileError {
            path: job.label,
            message: messages.join("; "),
        });
        return;
    }
    if limited {
        return;
    }
    let report = match job.prepared.settle(&answers) {
        Ok(report) => report,
        Err(err) => {
            errors.push(FileError {
                path: job.label,
                message: err.to_string(),
            });
            return;
        }
    };
    *judged += 1;
    if report.claims.is_empty() || report.claims.iter().any(|claim| claim.value != Value::True) {
        return;
    }
    matches.push(Match {
        path: job.label,
        claims: report.claims.into_iter().map(|claim| claim.text).collect(),
    });
}

fn note_error(errors: &mut Vec<String>, err: &OracleError) {
    let message = err.to_string();
    if !errors.contains(&message) {
        errors.push(message);
    }
}

fn admit(path: &Path) -> Result<bool, SweepError> {
    let meta = fs::metadata(path)
        .map_err(|err| SweepError::Io(format!("{}: {err}", path.display())))?;
    if !meta.is_file() {
        return Ok(false);
    }
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or("");
    if is_brief_name(name) {
        return Ok(false);
    }
    let text = path.to_string_lossy();
    if let Some(format) = image_extension(&text) {
        if meta.len() > MAX_IMAGE_BYTES as u64 {
            return Ok(false);
        }
        let bytes = fs::read(path)
            .map_err(|err| SweepError::Io(format!("{}: {err}", path.display())))?;
        return Ok(image_asset(&bytes, format).is_ok());
    }
    if meta.len() as usize > MAX_SLICED_BYTES {
        return Ok(false);
    }
    let bytes = fs::read(path)
        .map_err(|err| SweepError::Io(format!("{}: {err}", path.display())))?;
    Ok(!bytes.contains(&0) && std::str::from_utf8(&bytes).is_ok())
}

fn is_brief_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name == ".brief" || name.ends_with(".brief")
}

fn absolute(root: &Path, rel: &str) -> PathBuf {
    let path = root.join(rel);
    fs::canonicalize(&path).unwrap_or(path)
}

fn check_err(err: CheckError) -> SweepError {
    match err {
        CheckError::Oracle(err) => SweepError::Oracle(err),
        CheckError::Io(message) | CheckError::Bind(message) => SweepError::Io(message),
    }
}

fn render(
    mut matches: Vec<Match>,
    mut errors: Vec<FileError>,
    incomplete: bool,
    judged: usize,
    candidates: usize,
) -> Outcome {
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path));
    let mut text = progress_report(&matches, &errors);
    if matches.is_empty() && errors.is_empty() && !incomplete {
        text.push_str("CLEAN\n");
    } else if incomplete {
        text.push_str("INCOMPLETE\n");
    }
    Outcome {
        text,
        matches,
        errors,
        incomplete,
        judged,
        candidates,
    }
}

fn progress_report(matches: &[Match], errors: &[FileError]) -> String {
    let mut text = String::new();
    write_matches(&mut text, matches);
    write_errors(&mut text, errors);
    text
}

fn write_matches(text: &mut String, matches: &[Match]) {
    for matched in matches {
        text.push_str(&matched.path.display().to_string());
        text.push('\n');
        for claim in &matched.claims {
            text.push_str("  ");
            text.push_str(claim);
            text.push('\n');
        }
    }
}

fn write_errors(text: &mut String, errors: &[FileError]) {
    for error in errors {
        text.push_str("Error: ");
        text.push_str(&error.path.display().to_string());
        text.push('\n');
        for line in error.message.lines() {
            text.push_str("  ");
            text.push_str(line);
            text.push('\n');
        }
    }
}

fn emit(watch: &mut dyn FnMut(&Snapshot), report: String, status: String) {
    watch(&Snapshot {
        report,
        status: Some(status),
    });
}

fn emit_progress(
    watch: &mut dyn FnMut(&Snapshot),
    matches: &mut Vec<Match>,
    errors: &mut Vec<FileError>,
    done: usize,
    candidates: usize,
) {
    matches.sort_by(|left, right| left.path.cmp(&right.path));
    errors.sort_by(|left, right| left.path.cmp(&right.path));
    emit(
        watch,
        progress_report(matches, errors),
        format!("judging {done}/{candidates} files"),
    );
}

/// Answers that have landed.
///
/// A sync oracle's `wait_any` returns the answer error for its first ticket
/// instead of the ticket. That error belongs to the file. A failure of the
/// wait itself names no ticket.
fn take_ready<O: Oracle, T>(
    oracle: &mut O,
    pending: &mut Vec<(Ticket, T)>,
) -> Result<Vec<(T, Result<Answer, OracleError>)>, SweepError> {
    let tickets: Vec<Ticket> = pending.iter().map(|(ticket, _)| *ticket).collect();
    let ready: HashSet<Ticket> = ready_tickets(oracle, &tickets)?.into_iter().collect();
    let mut left = Vec::new();
    let mut done = Vec::new();
    for (ticket, item) in pending.drain(..) {
        if ready.contains(&ticket) {
            done.push((ticket, item));
        } else {
            left.push((ticket, item));
        }
    }
    *pending = left;
    let mut out = Vec::with_capacity(done.len());
    for (ticket, item) in done {
        out.push((item, oracle.wait(ticket)));
    }
    if out.is_empty() {
        return Err(SweepError::Oracle(OracleError::Parse(
            "oracle reported no finished ticket".into(),
        )));
    }
    Ok(out)
}

fn ready_tickets<O: Oracle>(oracle: &mut O, tickets: &[Ticket]) -> Result<Vec<Ticket>, SweepError> {
    match oracle.wait_any(tickets) {
        Ok(ticket) => {
            let ready = oracle.completed(tickets);
            if ready.is_empty() {
                Ok(vec![ticket])
            } else {
                Ok(ready)
            }
        }
        Err(err) if is_wait_failure(&err) => Err(SweepError::Oracle(err)),
        Err(_) => Ok(vec![tickets[0]]),
    }
}

fn is_wait_failure(err: &OracleError) -> bool {
    match err {
        OracleError::Config(_) => true,
        OracleError::Parse(message)
            if message == "no tickets to wait for" || message.starts_with("unknown ticket") =>
        {
            true
        }
        OracleError::Http(message) if message.contains("ended without a result") => true,
        OracleError::Limit(_) | OracleError::Http(_) | OracleError::Parse(_) => false,
    }
}

fn universe_io(err: UniverseError) -> SweepError {
    SweepError::Io(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compile;
    use crate::oracle::QuestionKind;
    use crate::syntax::parse;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn scratch(label: &str) -> PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "autoground-sweep-{}-{}-{}",
            label,
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn compiled(src: &str) -> Vec<Proposition> {
        compile(&parse(src).unwrap()).unwrap().claims
    }

    fn files(root: &Path) -> Scope<'_> {
        Scope::Files { root, hide: None }
    }

    fn answer(choice: &str, confidence: f64) -> Answer {
        Answer {
            choice: choice.to_string(),
            confidence,
            noul: None,
        }
    }

    fn noul(probability: f64) -> Answer {
        Answer {
            choice: String::new(),
            confidence: 0.0,
            noul: Some(probability),
        }
    }

    struct Script {
        next: u64,
        pending: HashMap<u64, Classification>,
        seen: Vec<Classification>,
        answer: fn(&Classification) -> Result<Answer, OracleError>,
    }

    impl Script {
        fn new(answer: fn(&Classification) -> Result<Answer, OracleError>) -> Self {
            Self {
                next: 0,
                pending: HashMap::new(),
                seen: Vec::new(),
                answer,
            }
        }
    }

    impl Oracle for Script {
        fn submit(&mut self, req: Classification) -> Ticket {
            let ticket = Ticket(self.next);
            self.next += 1;
            self.pending.insert(ticket.0, req.clone());
            self.seen.push(req);
            ticket
        }

        fn wait(&mut self, ticket: Ticket) -> Result<Answer, OracleError> {
            let req = self
                .pending
                .get(&ticket.0)
                .cloned()
                .expect("ticket was submitted");
            (self.answer)(&req)
        }
    }

    const NORTHSTAR: &str = "(claim (\"Contains no comment that references a historic state of the codebase.\") (witness $))\n";
    const NORTHSTAR_CLAIM: &str =
        "Contains no comment that references a historic state of the codebase.";

    fn matched_line(path: &std::path::Path) -> String {
        format!("{}\n  {NORTHSTAR_CLAIM}\n", path.display())
    }

    #[test]
    fn a_brief_without_a_dollar_submits_nothing() {
        let dir = scratch("bind");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        let mut oracle = Script::new(|_| Ok(answer("entailed", 0.9)));
        let err = sweep(
            &compiled("(claim (\"alpha\") (witness \"a.txt\"))\n"),
            Path::new("."),
            files(&dir),
            None,
            &mut oracle,
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("a sweep brief needs `(witness $)`"),
            "{err}"
        );
        assert!(oracle.seen.is_empty());
    }

    #[test]
    fn without_a_filter_every_file_is_judged() {
        let dir = scratch("all");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|req| {
            assert_eq!(req.kind, QuestionKind::Choice);
            assert!(req.state.get("body").is_some());
            Ok(answer("entailed", 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        let a = fs::canonicalize(dir.join("a.txt")).unwrap();
        let b = fs::canonicalize(dir.join("b.txt")).unwrap();
        assert_eq!(
            outcome.text,
            format!("{}{}", matched_line(&a), matched_line(&b))
        );
        assert_eq!(oracle.seen.len(), 2);
        assert!(oracle
            .seen
            .iter()
            .all(|req| req.kind == QuestionKind::Choice));
    }

    #[test]
    fn a_filter_keeps_only_a_confident_match() {
        let dir = scratch("filter");
        fs::write(dir.join("src.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("README.md"), "# notes\n").unwrap();
        let mut oracle = Script::new(|req| match req.kind {
            QuestionKind::Noul => {
                assert!(req.state.get("body").is_none(), "{req:?}");
                assert_eq!(req.instructions["filter"], "source file");
                assert_eq!(req.instructions["question"], FILTER_QUESTION);
                assert_eq!(
                    req.criteria["true"],
                    "The path matches the filter. It should be opened."
                );
                let path = req.instructions["path"].as_str().unwrap();
                let probability = if path.ends_with("src.rs") { 0.95 } else { 0.5 };
                Ok(noul(probability))
            }
            QuestionKind::Choice => {
                assert!(req.state.get("body").is_some());
                assert!(req.state["path"].as_str().unwrap().ends_with("src.rs"));
                Ok(answer("entailed", 0.9))
            }
        });
        let outcome = sweep(
            &compiled(NORTHSTAR),
            Path::new("."),
            files(&dir),
            Some("source file"),
            &mut oracle,
        )
        .unwrap();
        let src = fs::canonicalize(dir.join("src.rs")).unwrap();
        assert_eq!(outcome.text, matched_line(&src));
        assert!(!outcome.text.contains("README.md"));
        let filters: Vec<_> = oracle
            .seen
            .iter()
            .filter(|req| req.kind == QuestionKind::Noul)
            .collect();
        assert_eq!(filters.len(), 2);
        assert_eq!(filters[0].state, filters[1].state);
        assert!(filters[0].state["paths"].as_array().is_some());
        assert_eq!(
            oracle
                .seen
                .iter()
                .filter(|req| req.kind == QuestionKind::Choice)
                .count(),
            1
        );
    }

    #[test]
    fn a_true_claim_is_a_match() {
        let dir = scratch("match");
        fs::write(dir.join("old.rs"), "// used to parse declare\n").unwrap();
        fs::write(dir.join("new.rs"), "fn main() {}\n").unwrap();
        fs::write(dir.join("unsure.rs"), "maybe\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            let choice = if path.ends_with("new.rs") {
                "entailed"
            } else if path.ends_with("old.rs") {
                "contradicted"
            } else {
                "neutral"
            };
            Ok(answer(choice, 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        let path = fs::canonicalize(dir.join("new.rs")).unwrap();
        assert_eq!(outcome.text, matched_line(&path));
        assert!(!outcome.text.contains("old.rs"));
        assert!(!outcome.text.contains("unsure.rs"));
        assert!(!outcome.incomplete);
    }

    #[test]
    fn a_file_matches_when_every_top_level_claim_holds() {
        let dir = scratch("every");
        fs::write(dir.join("both.txt"), "one\n").unwrap();
        fs::write(dir.join("one.txt"), "half\n").unwrap();
        fs::write(dir.join("low.txt"), "none\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            let proposition = req.instructions["proposition"].as_str().unwrap();
            let (choice, confidence) = if path.ends_with("both.txt") {
                ("entailed", 0.9)
            } else if path.ends_with("one.txt") && proposition == "alpha" {
                ("entailed", 0.9)
            } else if path.ends_with("one.txt") {
                ("contradicted", 0.9)
            } else {
                ("entailed", 0.5)
            };
            Ok(answer(choice, confidence))
        });
        let outcome = sweep(
            &compiled("(claim (\"alpha\") (witness $))\n(claim (\"beta\") (witness $))\n"),
            Path::new("."),
            files(&dir),
            None,
            &mut oracle,
        )
        .unwrap();
        let path = fs::canonicalize(dir.join("both.txt")).unwrap();
        assert_eq!(
            outcome.text,
            format!("{}\n  alpha\n  beta\n", path.display())
        );
        assert!(!outcome.text.contains("one.txt"));
        assert!(!outcome.text.contains("low.txt"));
    }

    #[test]
    fn a_refused_file_is_incomplete_even_when_nothing_matched() {
        let dir = scratch("cap");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|req| {
            if req.state["path"].as_str().unwrap().ends_with("b.txt") {
                return Err(OracleError::Limit("capped".into()));
            }
            Ok(answer("contradicted", 0.9))
        });
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.text, "INCOMPLETE\n");
        assert!(!outcome.text.contains("CLEAN"));
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.candidates, 2);
        assert_eq!(
            stopped_message(0.01, 0.0104, outcome.judged, outcome.candidates),
            "sweep stopped at the cost cap of $0.01 (spent $0.0104); 1 of 2 files judged"
        );
    }

    #[test]
    fn an_oracle_error_reports_the_file_and_keeps_the_rest() {
        let dir = scratch("http");
        fs::write(dir.join("big.py"), "x\n").unwrap();
        fs::write(dir.join("ok.py"), "y\n").unwrap();
        let mut oracle = Script::new(|req| {
            let path = req.state["path"].as_str().unwrap();
            if path.ends_with("big.py") {
                return Err(OracleError::Http(
                    "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}".into(),
                ));
            }
            Ok(answer("entailed", 0.9))
        });
        let mut statuses = Vec::new();
        let outcome = sweep_watching(
            &compiled("(claim (\"alpha\") (witness $))\n(claim (\"beta\") (witness $))\n"),
            Path::new("."),
            files(&dir),
            None,
            &mut oracle,
            &mut |snapshot| {
                if let Some(status) = &snapshot.status {
                    statuses.push(status.clone());
                }
            },
        )
        .unwrap();
        let ok = fs::canonicalize(dir.join("ok.py")).unwrap();
        let big = fs::canonicalize(dir.join("big.py")).unwrap();
        assert_eq!(
            outcome.text,
            format!(
                "{}\n  alpha\n  beta\nError: {}\n  HTTP 400: {{\"detail\":{{\"error_type\":\"max_tokens_exceeded\"}}}}\n",
                ok.display(),
                big.display()
            )
        );
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.candidates, 2);
        assert_eq!(outcome.errors.len(), 1);
        assert_eq!(
            outcome.errors[0].message,
            "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}"
        );
        assert!(!outcome.incomplete);
        assert!(!outcome.text.contains("CLEAN"));
        assert!(statuses.contains(&"judging 0/2 files".to_string()));
        assert!(statuses.contains(&"judging 1/2 files".to_string()));
        assert!(statuses.contains(&"judging 2/2 files".to_string()));
        assert_eq!(failed_message(1), "1 file failed");
        assert_eq!(failed_message(2), "2 files failed");
    }

    #[test]
    fn an_error_with_no_match_is_not_clean() {
        let dir = scratch("only-error");
        fs::write(dir.join("big.py"), "x\n").unwrap();
        let mut oracle = Script::new(|_| {
            Err(OracleError::Http(
                "HTTP 400: {\"detail\":{\"error_type\":\"max_tokens_exceeded\"}}".into(),
            ))
        });
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        let big = fs::canonicalize(dir.join("big.py")).unwrap();
        assert_eq!(
            outcome.text,
            format!(
                "Error: {}\n  HTTP 400: {{\"detail\":{{\"error_type\":\"max_tokens_exceeded\"}}}}\n",
                big.display()
            )
        );
        assert!(outcome.matches.is_empty());
        assert!(!outcome.incomplete);
        assert_eq!(outcome.judged, 0);
        assert_eq!(outcome.candidates, 1);
    }

    #[test]
    fn a_png_is_judged_as_an_image_asset() {
        use crate::oracle::image::{synthetic_png, ImageFormat};
        let dir = scratch("png");
        let png = synthetic_png(280, 280);
        fs::write(dir.join("shot.png"), &png).unwrap();
        let mut oracle = Script::new(|_| Ok(answer("contradicted", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.judged, 1);
        assert_eq!(outcome.candidates, 1);
        assert_eq!(oracle.seen.len(), 1);
        let state = &oracle.seen[0].state;
        assert!(state["path"].as_str().unwrap().ends_with("shot.png"));
        let reference = state["body"].as_str().unwrap();
        assert!(reference.starts_with("img-"));
        assert!(state.get("part").is_none());
        assert_eq!(state.assets().len(), 1);
        assert_eq!(state.assets()[0].reference(), reference);
        assert_eq!(state.assets()[0].format(), ImageFormat::Png);
        assert_eq!(state.assets()[0].bytes(), png.as_slice());
        assert!(!outcome.text.contains("Error"));
    }

    #[test]
    fn an_oversized_png_is_scaled_and_judged() {
        use crate::oracle::image::{image_asset, solid_png, ImageFormat};
        let dir = scratch("scaled-png");
        let png = solid_png(2000, 1000);
        let path = dir.join("shot.png");
        fs::write(&path, &png).unwrap();
        let mut oracle = Script::new(|_| Ok(answer("contradicted", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.judged, 1);
        assert_eq!(oracle.seen.len(), 1);
        let asset = image_asset(&png, ImageFormat::Png).unwrap();
        assert_eq!(oracle.seen[0].state.assets()[0].bytes(), asset.bytes());
        assert_ne!(asset.bytes(), png.as_slice());
        assert_eq!(fs::read(&path).unwrap(), png);
    }

    #[test]
    fn a_rejected_png_is_not_judged() {
        use crate::oracle::image::{synthetic_png, MAX_IMAGE_BYTES};
        let dir = scratch("rejected-png");
        fs::write(dir.join("tiny.png"), synthetic_png(10, 10)).unwrap();
        fs::write(dir.join("note.txt"), "alpha\n").unwrap();
        let big = dir.join("big.png");
        fs::File::create(&big)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES as u64 + 1)
            .unwrap();
        let mut oracle = Script::new(|_| Ok(answer("contradicted", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.candidates, 1);
        assert_eq!(outcome.judged, 1);
        assert_eq!(oracle.seen.len(), 1);
        assert!(oracle.seen[0].state["path"]
            .as_str()
            .unwrap()
            .ends_with("note.txt"));
        assert!(oracle.seen[0].state.assets().is_empty());
    }

    #[test]
    fn a_tree_with_no_true_claim_is_clean() {
        let dir = scratch("clean");
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        let mut oracle = Script::new(|_| Ok(answer("contradicted", 0.9)));
        let outcome = sweep(&compiled(NORTHSTAR), Path::new("."), files(&dir), None, &mut oracle).unwrap();
        assert_eq!(outcome.text, "CLEAN\n");
        assert!(outcome.matches.is_empty());
        assert!(!outcome.incomplete);
    }

    #[test]
    fn a_transcript_sweep_judges_readable_leaves() {
        let dir = scratch("transcript");
        let path = dir.join("chat_history.jsonl");
        fs::write(
            &path,
            r#"{"type":"system","content":"SECRET_SYSTEM"}
{"type":"user","prompt_index":0,"content":[{"type":"text","text":"<user_query>\nFind alpha\n</user_query>"}]}
{"type":"assistant","content":"","tool_calls":[{"id":"c1","name":"read_file","arguments":"{\"target_file\":\"a.txt\"}"}]}
{"type":"tool_result","tool_call_id":"c1","content":"alpha is here"}
{"type":"assistant","content":"Found it."}
"#,
        )
        .unwrap();
        let mut oracle = Script::new(|req| {
            let body = req.state["body"].as_str().unwrap();
            let choice = if body.contains("alpha is here") {
                "entailed"
            } else {
                "contradicted"
            };
            Ok(answer(choice, 0.9))
        });
        let outcome = sweep(
            &compiled(NORTHSTAR),
            Path::new("."),
            Scope::Transcript(&path),
            None,
            &mut oracle,
        )
        .unwrap();
        let paths: Vec<_> = oracle
            .seen
            .iter()
            .map(|req| req.state["path"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(paths, vec!["p0/c0/00-read_file", "p0/c1/message"]);
        assert_eq!(
            outcome.text,
            format!("p0/c0/00-read_file\n  {NORTHSTAR_CLAIM}\n")
        );
        assert!(!outcome.text.contains("SECRET_SYSTEM"));
    }

    #[test]
    fn a_diff_sweep_judges_each_patch() {
        let dir = scratch("diff");
        git(&dir, &["init", "-q"]);
        git(&dir, &["config", "user.email", "test@example.com"]);
        git(&dir, &["config", "user.name", "test"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        fs::write(dir.join("a.txt"), "alpha\n").unwrap();
        fs::write(dir.join("b.txt"), "beta\n").unwrap();
        git(&dir, &["add", "a.txt", "b.txt"]);
        git(&dir, &["commit", "-qm", "init"]);
        fs::write(dir.join("a.txt"), "beta\n").unwrap();
        fs::write(dir.join("b.txt"), "staged\n").unwrap();
        git(&dir, &["add", "b.txt"]);
        fs::write(dir.join("c.txt"), "untracked\n").unwrap();
        let mut oracle = Script::new(|req| {
            let body = req.state["body"].as_str().unwrap();
            let choice =
                if body.contains("diff --git") && body.contains("-alpha") && body.contains("+beta")
                {
                    "entailed"
                } else {
                    "contradicted"
                };
            Ok(answer(choice, 0.9))
        });
        let git_args: &[String] = &[];
        let outcome = sweep(
            &compiled(NORTHSTAR),
            Path::new("."),
            Scope::Diff {
                root: &dir,
                git_args,
                hide: None,
            },
            None,
            &mut oracle,
        )
        .unwrap();
        assert_eq!(outcome.text, format!("a.txt\n  {NORTHSTAR_CLAIM}\n"));
        assert_eq!(outcome.candidates, 1);
        assert_eq!(
            oracle
                .seen
                .iter()
                .map(|req| req.state["path"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["a.txt"]
        );
    }

    #[test]
    fn a_filter_applies_to_the_filesystem_only() {
        let git_args: &[String] = &[];
        let err = sweep(
            &compiled(NORTHSTAR),
            Path::new("."),
            Scope::Diff {
                root: Path::new("."),
                git_args,
                hide: None,
            },
            Some("source"),
            &mut Script::new(|_| Ok(answer("entailed", 0.9))),
        )
        .unwrap_err();
        assert!(
            err.to_string()
                .contains("`filter` applies to a filesystem sweep"),
            "{err}"
        );
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
