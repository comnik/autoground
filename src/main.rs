use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use autoground::align::{lower_mermaid, mismatch, model_of};
use autoground::check::{self, write_report, DEFAULT_BODY_BYTES};
use autoground::compiler::{link_parts, Vocabulary};
use autoground::log::{Log, DEFAULT_TRACE_DIR};
use autoground::oracle::jev::DEFAULT_RUN_COST_CENTS;
use autoground::oracle::{JevOracle, Oracle, Usage, UsageRecord};
use autoground::prepare;
use autoground::report::summary::{print_assumptions, print_file_line, print_run_summary};
use autoground::{
    brief_source, compile, display_path, parse, scan_briefs, scan_namespaces, Proposition,
};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser, Debug)]
#[command(
    name = "autoground",
    version,
    about = "An informal proof assistant that helps your agents ground explanations in the reality of your codebase."
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Re-check a brief against the files its claims cite.
    ///
    /// Briefs compile to a set of inter-dependent claims, which check then validates
    /// starting from the referenced witnesses.
    ///
    /// At every step in the proof the oracle is provided with the collected support
    /// (witnesses or other claims) and is asked to judge the proposed conclusion as
    /// "supported", "contradicted", or "neutral". Every claim is a deductive step. Only
    /// logical operators are resolved deterministically, without involving the oracle. Each
    /// unique claim is only proved once, no matter how often it is referred to.
    ///
    /// Exit status is 0 when every top-level claim is discharged, 1 otherwise, and 3 on a
    /// compile, IO, or oracle failure. Each check writes a detailed trace of all proof
    /// steps along with the requests sent to the oracle.
    ///
    /// Examples:
    ///
    /// autoground check .
    ///
    /// autoground check README.md
    ///
    /// autoground check briefs/architecture/architecture.brief
    Check {
        /// Brief source, a `.brief` file, a Markdown file of `brief` blocks, or a directory of those files.
        input: String,
        /// File to bind to templated witnesses `(witness $)`. Other witness paths stay
        /// relative to the brief. `-` reads that file from stdin.
        file: Option<PathBuf>,
        /// Cost cap for this check, in cents. In-flight requests that cross the cap still
        /// finish. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
        /// Output directory for this check, will contain `trace.jsonl` and other artefacts.
        /// Defaults to `/tmp/autoground`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Compare the names, senses, and statements of two briefs for consistency.
    ///
    /// Each argument is a `.brief` file, a Markdown file of `brief` blocks, a `.mmd`
    /// flowchart, or brief source. A flowchart's node labels are names. A label resolves to
    /// a nullary sense used as a term, so the picture keeps the bare label. An edge labeled
    /// with a sense name is that sense applied to its endpoints. A leading `not ` is
    /// refused. Any other edge label is the wording between the two names. A `%%` line that
    /// starts with `(` is brief source, which is how a picture carries a sense template.
    ///
    /// Exit status is 1 when a name, a sense, or a statement is on only one side. The
    /// oracle is not asked.
    ///
    /// Examples:
    ///
    /// autoground align model.brief picture.mmd
    ///
    /// autoground align model.brief notes.md
    Align {
        /// A `.brief` file, Markdown `brief` blocks, a `.mmd` flowchart, or brief source.
        left: String,
        /// A `.brief` file, Markdown `brief` blocks, a `.mmd` flowchart, or brief source.
        right: String,
    },
    /// Map other autoground targets to a directory.
    ///
    /// `transcript` writes one file per tool call. `diff` writes one file per `git diff`
    /// change.
    Prepare {
        #[command(subcommand)]
        kind: PrepareKind,
    },
}

#[derive(Subcommand, Debug)]
enum PrepareKind {
    /// Write one file per tool call. `path` is `chat_history.jsonl`, or a directory that contains it.
    Transcript {
        /// `chat_history.jsonl`, or a directory that contains it.
        path: PathBuf,
    },
    /// Write one file per change in `git diff`. Arguments are passed to `git diff`.
    Diff {
        /// Arguments forwarded to `git diff`.
        #[arg(
            trailing_var_arg = true,
            allow_hyphen_values = true,
            value_name = "ARGS"
        )]
        git_args: Vec<String>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check {
            input,
            file,
            budget,
            out,
        } => match check_cmd(&input, file.as_deref(), budget, out.as_deref()) {
            Ok(code) => ExitCode::from(code),
            Err(err) => fail(err),
        },
        Commands::Align { left, right } => match align_cmd(&left, &right) {
            Ok(code) => ExitCode::from(code),
            Err(err) => fail(err),
        },
        Commands::Prepare { kind } => match prepare_cmd(kind) {
            Ok(code) => ExitCode::from(code),
            Err(err) => fail(err),
        },
    }
}

fn fail(err: impl std::fmt::Display) -> ExitCode {
    let mut stderr = io::stderr();
    let _ = writeln!(stderr, "{err}");
    let _ = stderr.flush();
    ExitCode::from(3)
}

fn prepare_cmd(kind: PrepareKind) -> Result<u8, Box<dyn std::error::Error>> {
    let dir = match kind {
        PrepareKind::Transcript { path } => prepare::transcript(&path)?,
        PrepareKind::Diff { git_args } => prepare::diff(Path::new("."), &git_args)?,
    };
    println!("{}", dir.display());
    Ok(0)
}

fn check_cmd(
    input: &str,
    file: Option<&Path>,
    budget_cents: u64,
    out: Option<&Path>,
) -> Result<u8, Box<dyn std::error::Error>> {
    if Path::new(input).is_dir() {
        if file.is_some() {
            return Err("a directory check takes no file".into());
        }
        return check_dir(input, budget_cents, out);
    }
    let started = std::time::Instant::now();
    let base = brief_dir(input);
    let (src, program, compiled) = load_compiled(input, &base)?;
    let (bound, stdin_tmp) = match file {
        Some(path) if path == Path::new("-") => {
            if !check::cites_dollar(&compiled.claims) {
                return Err("a file needs `(witness $)`".into());
            }
            let tmp = read_stdin_file()?;
            (
                Some(check::BoundFile {
                    cite: "-".to_string(),
                    path: tmp.clone(),
                }),
                Some(tmp),
            )
        }
        Some(path) => (
            Some(check::BoundFile {
                cite: path.display().to_string(),
                path: path.to_path_buf(),
            }),
            None,
        ),
        None => (None, None),
    };
    let result = (|| -> Result<u8, Box<dyn std::error::Error>> {
        match (check::cites_dollar(&compiled.claims), bound.is_some()) {
            (true, false) => return Err("`$` needs a file".into()),
            (false, true) => return Err("a file needs `(witness $)`".into()),
            _ => {}
        }
        let log = Log::create_under(out.unwrap_or_else(|| Path::new(DEFAULT_TRACE_DIR)))?;
        let mut oracle = JevOracle::from_env(log.clone())?;
        oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);
        finish_check(
            &src,
            &program,
            &compiled.claims,
            &compiled.assumptions,
            &base,
            bound.as_ref(),
            &mut oracle,
            &log,
            started,
        )
    })();
    if let Some(path) = stdin_tmp {
        let _ = std::fs::remove_file(path);
    }
    result
}

fn read_stdin_file() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let mut bytes = Vec::new();
    io::stdin().read_to_end(&mut bytes)?;
    let dir = Path::new(DEFAULT_TRACE_DIR);
    std::fs::create_dir_all(dir)?;
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    for n in 0u32..10_000 {
        let path = dir.join(format!("stdin-{nanos}-{pid}-{n}"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                if let Err(err) = file.write_all(&bytes) {
                    let _ = std::fs::remove_file(&path);
                    return Err(err.into());
                }
                return Ok(path);
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err("could not allocate a stdin file".into())
}

fn print_report(
    src: &str,
    program: &autoground::Program,
    report: &check::Report,
) -> io::Result<()> {
    if std::io::stdout().is_terminal() {
        autoground::report::run::write_brief(src, &program.clauses, report)
    } else {
        print!("{}", write_report(report));
        Ok(())
    }
}

fn finish_check<O: Oracle>(
    src: &str,
    program: &autoground::Program,
    claims: &[Proposition],
    assumptions: &[String],
    base: &Path,
    bound: Option<&check::BoundFile>,
    oracle: &mut O,
    log: &Log,
    started: std::time::Instant,
) -> Result<u8, Box<dyn std::error::Error>> {
    let report = check::check_bound(claims, base, DEFAULT_BODY_BYTES, bound, oracle)?;
    let usage = oracle.usage();
    let flights = oracle.flights().to_vec();
    let flight_limit = oracle.flight_limit();
    record_check(log, src, program, &report, &usage, None)?;
    print_report(src, program, &report)?;
    let true_count = report
        .claims
        .iter()
        .filter(|claim| claim.value == autoground::Value::True)
        .count();
    print_run_summary(
        &usage,
        &flights,
        flight_limit,
        true_count,
        report.claims.len(),
        started.elapsed(),
        log.path(),
        assumptions,
    )?;
    let discharged = report
        .claims
        .iter()
        .all(|claim| claim.value == autoground::Value::True);
    Ok(if discharged { 0 } else { 1 })
}

struct ReadyBrief {
    path: String,
    base: PathBuf,
    src: String,
    program: autoground::Program,
    compiled: autoground::Compiled,
}

enum PreparedBrief {
    Ready(ReadyBrief),
    Failed { path: String, message: String },
}

fn check_dir(
    input: &str,
    budget_cents: u64,
    out: Option<&Path>,
) -> Result<u8, Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    let files = scan_briefs(Path::new(input))?;
    if files.is_empty() {
        println!("no briefs");
        return Ok(0);
    }
    let prepared: Vec<PreparedBrief> = files
        .iter()
        .map(|file| match prepare_brief(file) {
            Ok(ready) => PreparedBrief::Ready(ready),
            Err(message) => PreparedBrief::Failed {
                path: file.path.clone(),
                message,
            },
        })
        .collect();
    if prepared
        .iter()
        .all(|item| matches!(item, PreparedBrief::Failed { .. }))
    {
        print_prepare_errors(&prepared)?;
        return Ok(3);
    }

    let log = Log::create_under(out.unwrap_or_else(|| Path::new(DEFAULT_TRACE_DIR)))?;
    let mut oracle = match JevOracle::from_env(log.clone()) {
        Ok(oracle) => oracle,
        Err(err) => {
            print_prepare_errors(&prepared)?;
            return Err(err.into());
        }
    };
    oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);

    let mut compile_failed = false;
    let mut all_true = true;
    let mut true_count = 0usize;
    let mut total = 0usize;
    for item in prepared {
        match item {
            PreparedBrief::Failed { path, message } => {
                compile_failed = true;
                print_brief_failure(&path, &message)?;
            }
            PreparedBrief::Ready(ready) => {
                let report = check::check_bound(
                    &ready.compiled.claims,
                    &ready.base,
                    DEFAULT_BODY_BYTES,
                    None,
                    &mut oracle,
                )?;
                let validated = report
                    .claims
                    .iter()
                    .filter(|claim| claim.value == autoground::Value::True)
                    .count();
                let claim_count = report.claims.len();
                let passed = validated == claim_count;
                print_file_line(passed, &ready.path, Some((validated, claim_count)))?;
                if !passed {
                    print_report(&ready.src, &ready.program, &report)?;
                    print_assumptions(&ready.compiled.assumptions)?;
                    println!();
                    all_true = false;
                }
                let usage = oracle.usage();
                record_check(
                    &log,
                    &ready.src,
                    &ready.program,
                    &report,
                    &usage,
                    Some(&ready.path),
                )?;
                true_count += validated;
                total += claim_count;
            }
        }
    }

    let usage = oracle.usage();
    let flights = oracle.flights().to_vec();
    let flight_limit = oracle.flight_limit();
    print_run_summary(
        &usage,
        &flights,
        flight_limit,
        true_count,
        total,
        started.elapsed(),
        log.path(),
        &[],
    )?;
    if compile_failed {
        Ok(3)
    } else if all_true {
        Ok(0)
    } else {
        Ok(1)
    }
}

fn prepare_brief(file: &autoground::BriefFile) -> Result<ReadyBrief, String> {
    let input = file.absolute.to_string_lossy().into_owned();
    let base = brief_dir(&input);
    match load_compiled(&input, &base) {
        Ok((src, program, compiled)) => {
            if check::cites_dollar(&compiled.claims) {
                Err("`$` needs a file".to_string())
            } else {
                Ok(ReadyBrief {
                    path: file.path.clone(),
                    base,
                    src,
                    program,
                    compiled,
                })
            }
        }
        Err(err) => Err(err.to_string()),
    }
}

fn print_prepare_errors(items: &[PreparedBrief]) -> io::Result<()> {
    for item in items {
        if let PreparedBrief::Failed { path, message } = item {
            print_brief_failure(path, message)?;
        }
    }
    Ok(())
}

fn print_brief_failure(path: &str, message: &str) -> io::Result<()> {
    print_file_line(false, path, None)?;
    println!("{message}");
    Ok(())
}

fn record_check(
    log: &Log,
    src: &str,
    program: &autoground::Program,
    report: &check::Report,
    usage: &Usage,
    file: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    let recorded: Vec<_> = report
        .claims
        .iter()
        .zip(program.clauses.iter())
        .map(|(node, clause)| {
            let claim = src
                .get(clause.span.clone())
                .map(str::trim)
                .unwrap_or(node.text.as_str());
            json!({
                "claim": claim,
                "value": node.value,
            })
        })
        .collect();
    let mut body = json!({
        "claims": recorded,
        "usage": UsageRecord::from_usage(usage),
    });
    if let Some(file) = file {
        body["file"] = json!(file);
    }
    log.append_event("check", &body)?;
    Ok(())
}

/// Directory witness paths are relative to. A brief file uses its own directory.
/// Source passed on the command line uses the current directory.
fn brief_dir(input: &str) -> PathBuf {
    let path = Path::new(input);
    if path.is_file() {
        match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
            _ => PathBuf::from("."),
        }
    } else {
        PathBuf::from(".")
    }
}

fn align_cmd(left: &str, right: &str) -> Result<u8, Box<dyn std::error::Error>> {
    let left = load_model(left)?;
    let right = load_model(right)?;
    match mismatch(&left, &right) {
        None => Ok(0),
        Some(report) => {
            print!("{report}");
            Ok(1)
        }
    }
}

fn load_model(input: &str) -> Result<autoground::align::Model, Box<dyn std::error::Error>> {
    let src = align_source(input)?;
    let program = parse(&src)?;
    let base = brief_dir(input);
    let (compiled, _) = compile_linked(&program, input, &base)?;
    Ok(model_of(&program, &compiled))
}

/// `.mmd` lowers to a brief. Every other model is read the way `check` reads it.
fn align_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if has_ext(path, "mmd") {
        if !path.is_file() {
            return Err(format!("model file not found: {input}").into());
        }
        let text = std::fs::read_to_string(path)?;
        return match lower_mermaid(&text) {
            Ok(brief) => Ok(brief),
            Err(err) => Err(format!("{err} in {input}").into()),
        };
    }
    model_source(input)
}

fn load_compiled(
    input: &str,
    base: &Path,
) -> Result<(String, autoground::Program, autoground::Compiled), Box<dyn std::error::Error>> {
    let src = model_source(input)?;
    let program = parse(&src)?;
    let (compiled, _) = compile_linked(&program, input, base)?;
    Ok((src, program, compiled))
}

fn compile_linked(
    program: &autoground::Program,
    input: &str,
    base: &Path,
) -> Result<(autoground::Compiled, Vocabulary), Box<dyn std::error::Error>> {
    if program
        .namespace
        .as_ref()
        .is_some_and(|ns| !ns.requires.is_empty())
    {
        let files = scan_namespaces(base)?;
        let entry = Path::new(input);
        let entry_path = entry.is_file().then(|| display_path(base, entry));
        Ok(link_parts(program, entry_path.as_deref(), &files)?)
    } else {
        Ok((compile(program)?, Vocabulary::none()))
    }
}

/// Read `input` as a model file when it names one, otherwise treat it as source.
///
/// A `.brief` file is the model. A Markdown file contributes every fenced `brief` block.
fn model_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if path.is_file() {
        let text = std::fs::read_to_string(path)?;
        if has_ext(path, "md") {
            return Ok(brief_source(&text).map_err(|err| format!("{err} in {input}"))?);
        }
        return Ok(text);
    }
    let trimmed = input.trim_start();
    if trimmed.starts_with('(') {
        return Ok(input.to_string());
    }
    if has_ext(path, "brief") || has_ext(path, "md") || path.exists() {
        return Err(format!("model file not found: {input}").into());
    }
    Ok(input.to_string())
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepare_commands_take_a_transcript_or_a_diff() {
        let transcript =
            Cli::try_parse_from(["autoground", "prepare", "transcript", "chat_history.jsonl"])
                .unwrap();
        match transcript.command {
            Commands::Prepare { kind } => match kind {
                PrepareKind::Transcript { path } => {
                    assert_eq!(path, PathBuf::from("chat_history.jsonl"));
                }
                PrepareKind::Diff { .. } => panic!("diff"),
            },
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }

        let diff = Cli::try_parse_from([
            "autoground",
            "prepare",
            "diff",
            "--cached",
            "-U0",
            "origin/main...HEAD",
            "--",
            "src",
        ])
        .unwrap();
        match diff.command {
            Commands::Prepare { kind } => match kind {
                PrepareKind::Diff { git_args } => {
                    assert_eq!(
                        git_args,
                        vec!["--cached", "-U0", "origin/main...HEAD", "--", "src",]
                    );
                }
                PrepareKind::Transcript { .. } => panic!("transcript"),
            },
            Commands::Check { .. } => panic!("check"),
            Commands::Align { .. } => panic!("align"),
        }
    }
}
