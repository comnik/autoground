use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use autoground::check::{self, write_report, DEFAULT_BODY_BYTES};
use autoground::compiler::{link_parts, Vocabulary};
use autoground::log::{Log, DEFAULT_TRACE_DIR};
use autoground::oracle::jev::DEFAULT_RUN_COST_CENTS;
use autoground::oracle::{HeuristicOracle, JevOracle, Oracle, Usage};
use autoground::report::context::write_context;
use autoground::report::mermaid::{diagram_of, write_diagram, Diagram, Styles};
use autoground::report::summary::print_run_summary;
use autoground::search::trace::SearchStep;
use autoground::search::trace::UsageRecord;
use autoground::search::{
    present_search, window_lines, Search, SearchTraceFile, DEFAULT_BUDGET_BYTES,
};
use autoground::universe::{Filesystem, Transcript, Universe};
use autoground::{compile, display_path, lect_source, parse, scan_namespaces, Proposition};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;

#[derive(Parser)]
#[command(name = "autoground", about = "Check a lect against the files it cites")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum JudgeKind {
    /// TypeSafe Jev via TYPESAFE_API_KEY (default).
    Jev,
    /// Local stand-in. No API key.
    Heuristic,
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
    /// Judge a proposition against a repository, coarse to fine.
    ///
    /// Starts at the children of the repo root. A directory can settle the
    /// proposition (`true` / `false`); otherwise the walk refines. A file is
    /// judged from its body in that same call. A session log is written under
    /// `/tmp/autoground/`. On a TTY, left/right opens each oracle call.
    ///
    /// Examples:
    ///   autoground search "the codebase has a frontend" --repo examples/toy-layered
    ///   autoground search "frontend" --judge heuristic --repo examples/toy-layered
    ///   autoground search "the agent read the universe trait" --transcript chat_history.jsonl
    ///   autoground search --inspect /tmp/autoground/trace-….jsonl
    Search {
        /// Natural-language proposition.
        #[arg(required_unless_present = "inspect")]
        proposition: Option<String>,
        /// Repository to search. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Coding-agent transcript (`chat_history.jsonl`, or a session directory).
        /// When set, this is the universe instead of `--repo`.
        #[arg(long, value_name = "PATH")]
        transcript: Option<PathBuf>,
        /// Byte budget for one directory-listing state sent to the oracle.
        #[arg(long, default_value_t = DEFAULT_BUDGET_BYTES)]
        budget: usize,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        /// Also print every step (view and classifications) as text.
        #[arg(long)]
        full: bool,
        /// Reopen a recorded search trace.
        #[arg(long, value_name = "TRACE")]
        inspect: Option<PathBuf>,
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
        Commands::Search {
            proposition,
            repo,
            transcript,
            budget,
            judge,
            full,
            inspect,
        } => match search_cmd(
            proposition.as_deref(),
            &repo,
            transcript.as_ref(),
            budget,
            judge,
            full,
            inspect.as_ref(),
        ) {
            Ok(code) => ExitCode::from(code as u8),
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
    }
}

fn search_cmd(
    proposition: Option<&str>,
    repo: &Path,
    transcript: Option<&PathBuf>,
    budget: usize,
    judge: JudgeKind,
    full: bool,
    inspect: Option<&PathBuf>,
) -> Result<i32, Box<dyn std::error::Error>> {
    if let Some(path) = inspect {
        let file = SearchTraceFile::load(path)
            .map_err(|e| format!("could not read trace {}: {e}", path.display()))?;
        present_search(path, &file, full)?;
        return Ok(0);
    }
    let proposition = proposition.expect("proposition required");
    let started_at = std::time::SystemTime::now();
    let log = Log::create()?;
    let (repo_name, searched) = if let Some(path) = transcript {
        let universe = Transcript::open(path)?;
        let searched = run_judged_search(universe, judge, proposition, budget, &log)?;
        (path_label(path, "transcript"), searched)
    } else {
        let universe = Filesystem::at_root(repo)?;
        let searched = run_judged_search(universe, judge, proposition, budget, &log)?;
        (path_label(repo, "repo"), searched)
    };
    let ended_at = std::time::SystemTime::now();
    let file = SearchTraceFile {
        kind: autoground::search::trace::TRACE_KIND.to_string(),
        proposition: proposition.to_string(),
        repo_name: repo_name.to_string(),
        databases: searched.databases,
        bytes_before: searched.bytes_before,
        bytes_after: searched.bytes_after,
        usage: UsageRecord::from_usage(&searched.usage),
        started_at_ms: epoch_ms(started_at),
        ended_at_ms: epoch_ms(ended_at),
        steps: searched.steps,
    };
    file.write_summary(&log)?;
    present_search(log.path(), &file, full)?;
    if let Some(err) = searched.failure {
        eprintln!("{err}");
        Ok(3)
    } else {
        Ok(0)
    }
}

struct Searched {
    databases: Vec<String>,
    steps: Vec<SearchStep>,
    usage: Usage,
    bytes_before: u64,
    bytes_after: u64,
    failure: Option<String>,
}

fn epoch_ms(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn path_label(path: &Path, fallback: &str) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(fallback)
        .to_string()
}

fn run_judged_search<U: Universe + Clone>(
    universe: U,
    judge: JudgeKind,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<Searched, Box<dyn std::error::Error>> {
    match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            run_search(universe, oracle, proposition, budget, log)
        }
        JudgeKind::Jev => run_search(
            universe,
            JevOracle::from_env(log.clone())?,
            proposition,
            budget,
            log,
        ),
    }
}

fn run_search<U: Universe + Clone, O: Oracle>(
    universe: U,
    oracle: O,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<Searched, Box<dyn std::error::Error>> {
    let mut session = Search::new(universe, oracle, budget, Some(log.clone()));
    let ticket = session.submit(proposition);
    match session.wait(ticket) {
        Ok(view) => Ok(Searched {
            databases: window_lines(&view.universe)?,
            steps: view.steps,
            usage: view.usage,
            bytes_before: view.bytes_before as u64,
            bytes_after: view.bytes_after as u64,
            failure: None,
        }),
        Err(error) => {
            let message = error.to_string();
            Ok(Searched {
                databases: Vec::new(),
                steps: error.steps,
                usage: error.usage,
                bytes_before: 0,
                bytes_after: 0,
                failure: Some(message),
            })
        }
    }
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
