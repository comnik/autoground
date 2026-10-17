use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Instant;

use autoground::check::{self, write_report, DEFAULT_BODY_BYTES};
use autoground::compiler::{link_parts, Vocabulary};
use autoground::log::{Log, DEFAULT_TRACE_DIR};
use autoground::oracle::jev::DEFAULT_RUN_COST_CENTS;
use autoground::oracle::{JevOracle, Oracle, UsageRecord};
use autoground::report::context::write_context;
use autoground::report::mermaid::{diagram_of, write_diagram, Diagram, Styles};
use autoground::report::summary::{print_run_summary, print_sweep_summary};
use autoground::sweep;
use autoground::{compile, display_path, lect_source, parse, scan_namespaces, Proposition};
use clap::{Parser, Subcommand};
use serde_json::json;

#[derive(Parser)]
#[command(
    name = "autoground",
    about = "Check a lect, or sweep a tree for files that match one"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Re-check a lect against the files its witnesses cite.
    ///
    /// A witness path is relative to the lect file. A file longer than the
    /// body budget is sent in parts, and the sentence is discharged when one
    /// part entails it. A detail is discharged when its body is discharged
    /// and those sentences entail the head. `--budget` caps this run in cents
    /// and defaults to 1. On a terminal the spinner names the sentence in
    /// flight, and the finished view is the lect: each judged clause carries
    /// its own mark, and an undischarged clause carries its failure comment.
    /// The trace, the context, and the diagram are written in a new directory
    /// under `/tmp/autoground/`, or under `--out`. Exit status is 1 when a
    /// top-level claim is not discharged.
    ///
    /// Examples:
    ///   autoground check examples/search-calls-oracle.lect
    ///   autoground check examples/sqrt2.lect
    Check {
        /// Model source, a `.lect` file, or a Markdown file of `lect` blocks.
        input: String,
        /// Cost cap for this check, in cents. The request that crosses the
        /// cap still finishes. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
        /// Parent directory for this check. A new subdirectory holds
        /// `trace.jsonl`, `context.md`, and `diagram.mmd`.
        /// Defaults to `/tmp/autoground`.
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
    },
    /// Judge a lect against every file in the current directory.
    ///
    /// The lect has no witness and no detail. `--filter` keeps paths that
    /// match it, from the listing alone, before any file is read. Each
    /// remaining file is judged until `--budget` cents are spent. A file
    /// matches when any top-level claim is true. Stdout is `CLEAN`, or the
    /// absolute path of each match. `--transcript` judges a session instead,
    /// and each match is a database id. `--diff` stays on the filesystem and
    /// keeps only files `git diff HEAD` names. A run that hits the cap ends
    /// with `INCOMPLETE`. The trace is `/tmp/autoground/sweep-<id>.jsonl`.
    ///
    /// Examples:
    ///   autoground sweep examples/no-historic-comment.lect --filter "source file"
    ///   autoground sweep examples/no-historic-comment.lect --diff
    ///   autoground sweep examples/no-historic-comment.lect --transcript chat_history.jsonl
    Sweep {
        /// Model source, a `.lect` file, or a Markdown file of `lect` blocks.
        input: String,
        /// Natural-language filter for which paths to judge. Listing only.
        #[arg(long)]
        filter: Option<String>,
        /// Cost cap for this sweep, in cents. The request that crosses the
        /// cap still finishes. Later requests are refused.
        #[arg(long, value_name = "CENTS", default_value_t = DEFAULT_RUN_COST_CENTS)]
        budget: u64,
        /// Session directory or `chat_history.jsonl`, in place of the tree.
        #[arg(long, value_name = "PATH", conflicts_with = "diff")]
        transcript: Option<PathBuf>,
        /// Judge only files `git diff HEAD` names. Staged and unstaged.
        #[arg(long, conflicts_with = "transcript")]
        diff: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Check { input, budget, out } => match check_cmd(&input, budget, out.as_deref()) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Sweep {
            input,
            filter,
            budget,
            transcript,
            diff,
        } => match sweep_cmd(
            &input,
            filter.as_deref(),
            budget,
            transcript.as_deref(),
            diff,
        ) {
            Ok(code) => ExitCode::from(code),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
    }
}

fn sweep_cmd(
    input: &str,
    filter: Option<&str>,
    budget_cents: u64,
    transcript: Option<&Path>,
    diff: bool,
) -> Result<u8, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let base = lect_dir(input);
    let (_src, _program, compiled, _vocab) = load_compiled(input, &base)?;
    sweep::ensure_constraints(&compiled.claims)?;
    let log = Log::create_sweep()?;
    let mut oracle = JevOracle::from_env(log.clone())?;
    oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);
    let model = Path::new(input);
    let hide = model.is_file().then_some(model);
    let scope = match transcript {
        Some(path) => sweep::Scope::Transcript(path),
        None => sweep::Scope::Files {
            root: Path::new("."),
            hide,
            diff,
        },
    };
    let outcome = sweep::sweep(&compiled.claims, scope, filter, &mut oracle)
        .map_err(|err| format!("{err}\ntrace: {}", log.path().display()))?;
    let matches: Vec<_> = outcome
        .matches
        .iter()
        .map(|matched| {
            json!({
                "path": matched.path.display().to_string(),
                "claims": matched.claims,
            })
        })
        .collect();
    log.append_event(
        "sweep",
        &json!({
            "filter": filter,
            "diff": diff,
            "transcript": transcript.map(|path| path.display().to_string()),
            "incomplete": outcome.incomplete,
            "judged": outcome.judged,
            "candidates": outcome.candidates,
            "matches": matches,
            "usage": UsageRecord::from_usage(&oracle.usage()),
        }),
    )?;
    print!("{}", outcome.text);
    let note = outcome.incomplete.then(|| {
        sweep::stopped_message(
            budget_cents as f64 / 100.0,
            oracle.usage().spend_usd,
            outcome.judged,
            outcome.candidates,
        )
    });
    print_sweep_summary(
        &oracle.usage(),
        oracle.flights(),
        oracle.flight_limit(),
        outcome.judged,
        outcome.candidates,
        started.elapsed(),
        log.path(),
        note.as_deref(),
    )?;
    Ok(if outcome.incomplete || !outcome.matches.is_empty() {
        1
    } else {
        0
    })
}

fn check_cmd(
    input: &str,
    budget_cents: u64,
    out: Option<&Path>,
) -> Result<u8, Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    let live = if std::io::stdout().is_terminal() {
        let live = autoground::report::run::Live::open()?;
        live.show_checking()?;
        Some(live)
    } else {
        None
    };
    let base = lect_dir(input);
    let (src, program, compiled, vocab) = load_compiled(input, &base)?;
    let mut diagram = diagram_of(&program, &vocab);
    let log = Log::create_under(out.unwrap_or_else(|| Path::new(DEFAULT_TRACE_DIR)))?;
    if let Some(live) = &live {
        let ui = live.clone();
        log.set_hook(move |event| ui.on_event(event));
    }
    let mut oracle = JevOracle::from_env(log.clone())?;
    oracle.set_run_cost_limit_usd(budget_cents as f64 / 100.0);
    finish_check(
        &src,
        &program,
        &compiled.claims,
        &base,
        &mut oracle,
        &mut diagram,
        &log,
        live,
        started,
    )
}

fn finish_check<O: Oracle>(
    src: &str,
    program: &autoground::Program,
    claims: &[Proposition],
    base: &Path,
    oracle: &mut O,
    diagram: &mut Diagram,
    log: &Log,
    live: Option<autoground::report::run::Live>,
    started: std::time::Instant,
) -> Result<u8, Box<dyn std::error::Error>> {
    let report = check::check(claims, base, DEFAULT_BODY_BYTES, oracle)?;
    log.clear_hook();
    let context = autoground::report::context::render(&report);
    let styles = if diagram.has_style_questions() {
        Styles::ask(oracle, diagram, &context)
    } else {
        Styles::plain()
    };
    let usage = oracle.usage();
    let flights = oracle.flights().to_vec();
    let flight_limit = oracle.flight_limit();
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
    log.append_event(
        "check",
        &json!({
            "claims": recorded,
            "usage": UsageRecord::from_usage(&usage),
        }),
    )?;
    write_context(log.path(), &report)?;
    diagram.mark_contradicted(&check::contradicted_sentences(&report));
    write_diagram(log.path(), diagram, &styles)?;
    if let Some(live) = live {
        live.finish(src, &program.clauses, &report)?;
    } else {
        print!("{}", write_report(&report));
    }
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
    )?;
    let discharged = report
        .claims
        .iter()
        .all(|claim| claim.value == autoground::Value::True);
    Ok(if discharged { 0 } else { 1 })
}

/// Directory witness paths are relative to. A lect file uses its own directory.
/// Source passed on the command line uses the current directory.
fn lect_dir(input: &str) -> PathBuf {
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

fn load_compiled(
    input: &str,
    base: &Path,
) -> Result<
    (
        String,
        autoground::Program,
        autoground::Compiled,
        Vocabulary,
    ),
    Box<dyn std::error::Error>,
> {
    let src = model_source(input)?;
    let program = parse(&src)?;
    let (compiled, vocab) = if program
        .namespace
        .as_ref()
        .is_some_and(|ns| !ns.requires.is_empty())
    {
        let files = scan_namespaces(base)?;
        let entry = Path::new(input);
        let entry_path = entry.is_file().then(|| display_path(base, entry));
        link_parts(&program, entry_path.as_deref(), &files)?
    } else {
        (compile(&program)?, Vocabulary::none())
    };
    Ok((src, program, compiled, vocab))
}

/// Read `input` as a model file when it names one, otherwise treat it as source.
///
/// A `.lect` file is the model. A Markdown file contributes every fenced `lect` block.
fn model_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if path.is_file() {
        let text = std::fs::read_to_string(path)?;
        if has_ext(path, "md") {
            return Ok(lect_source(&text).map_err(|err| format!("{err} in {input}"))?);
        }
        return Ok(text);
    }
    let trimmed = input.trim_start();
    if trimmed.starts_with('(') {
        return Ok(input.to_string());
    }
    if has_ext(path, "lect") || has_ext(path, "md") || path.exists() {
        return Err(format!("model file not found: {input}").into());
    }
    Ok(input.to_string())
}

fn has_ext(path: &Path, ext: &str) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(ext))
}
