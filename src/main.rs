use std::path::{Path, PathBuf};
use std::process::ExitCode;

use autoground::edb::Store;
use autoground::eval::run;
use autoground::log::Log;
use autoground::navigator::judge::epoch_ms;
use autoground::navigator::{FixtureJudge, JevJudge, RecordingJudge, RequireTrace, TreeNavigator};
use autoground::oracle::{HeuristicOracle, JevOracle, Oracle, Usage};
use autoground::read::{present_read, ReadTraceFile, Reader};
use autoground::report::{
    format_require_traces, format_run_body, format_run_header, jev_request_count, load_run_log,
    require_traces, run_exit_code, RunTiming,
};
use autoground::search::trace::SearchStep;
use autoground::search::trace::UsageRecord;
use autoground::search::{
    present_search, window_lines, Search, SearchTraceFile, DEFAULT_BUDGET_BYTES,
};
use autoground::universe::{Filesystem, Universe};
use autoground::{compile, parse, verbalize_source};
use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(
    name = "autoground",
    about = "Self-grounding conceptual models of a codebase"
)]
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
    /// Parse a model and evaluate its requires against EDB, grounding from a
    /// repository via the navigator (default: the current directory).
    ///
    /// Prints the evaluation result and grounding. On a TTY, left/right opens
    /// the step-by-step debug view. A session log is written under
    /// `/tmp/autoground/`.
    ///
    /// Examples:
    ///   autoground run examples/layered.ag --repo examples/toy-layered
    ///   autoground run --repo examples/toy-split '(require (component "Frontend"))'
    ///   autoground run --inspect /tmp/autoground/trace-….jsonl
    Run {
        /// Path to a `.ag` model, or a model source string.
        #[arg(required_unless_present = "inspect")]
        model: Option<String>,
        /// Repository to ground against.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        #[arg(long)]
        fixtures: Option<PathBuf>,
        /// Reopen a recorded trace and render it as if the run had just completed.
        #[arg(long, value_name = "TRACE")]
        inspect: Option<PathBuf>,
    },
    /// Verbalize an atom or a model using `declare` templates.
    ///
    /// Examples:
    ///   autoground verbalize '(Component "Frontend")'
    ///   autoground verbalize examples/layered.ag
    Verbalize {
        /// Atom, model source, or path to a `.ag` file.
        input: String,
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
    ///   autoground search --inspect /tmp/autoground/trace-….jsonl
    Search {
        /// Natural-language proposition.
        #[arg(required_unless_present = "inspect")]
        proposition: Option<String>,
        /// Repository to search. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Render budget in bytes. Leftover room is filled with one level of samples.
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
    /// Read a proposition against a repository.
    ///
    /// Searches for a candidate window, then judges the files in it. The
    /// window is a closed world: a witness is true, a refutation is false,
    /// and exhausting it with no witness is false. On a TTY the session log
    /// is written under `/tmp/autoground/`.
    ///
    /// Examples:
    ///   autoground read "the codebase has a frontend" --repo examples/toy-layered
    ///   autoground read "frontend" --judge heuristic --repo examples/toy-layered
    ///   autoground read --inspect /tmp/autoground/trace-….jsonl
    Read {
        /// Natural-language proposition.
        #[arg(required_unless_present = "inspect")]
        proposition: Option<String>,
        /// Repository to read. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Render budget in bytes for one file body.
        #[arg(long, default_value_t = DEFAULT_BUDGET_BYTES)]
        budget: usize,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        /// Also print every judged object as text.
        #[arg(long)]
        full: bool,
        /// Reopen a recorded read trace.
        #[arg(long, value_name = "TRACE")]
        inspect: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Commands::Run {
            model,
            repo,
            judge,
            fixtures,
            inspect,
        } => {
            let result = if let Some(path) = inspect {
                inspect_trace(&path)
            } else {
                run_model(
                    model.as_deref().expect("model required"),
                    &repo,
                    judge,
                    fixtures.as_ref(),
                )
            };
            match result {
                Ok(code) => ExitCode::from(code as u8),
                Err(err) => {
                    eprintln!("{err}");
                    ExitCode::from(3)
                }
            }
        }
        Commands::Verbalize { input } => match verbalize_cmd(&input) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Search {
            proposition,
            repo,
            budget,
            judge,
            full,
            inspect,
        } => match search_cmd(
            proposition.as_deref(),
            &repo,
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
        Commands::Read {
            proposition,
            repo,
            budget,
            judge,
            full,
            inspect,
        } => match read_cmd(
            proposition.as_deref(),
            &repo,
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
    let universe = Filesystem::at_root(repo)?;
    let searched = match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            run_search(universe, oracle, proposition, budget, &log)?
        }
        JudgeKind::Jev => run_search(universe, JevOracle::from_env()?, proposition, budget, &log)?,
    };
    let ended_at = std::time::SystemTime::now();
    let repo_name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let file = SearchTraceFile {
        kind: autoground::search::trace::TRACE_KIND.to_string(),
        proposition: proposition.to_string(),
        repo_name: repo_name.to_string(),
        objects: searched.objects,
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
    objects: Vec<String>,
    steps: Vec<SearchStep>,
    usage: Usage,
    bytes_before: u64,
    bytes_after: u64,
    failure: Option<String>,
}

fn run_search<O: Oracle>(
    universe: Filesystem,
    oracle: O,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<Searched, Box<dyn std::error::Error>> {
    let mut session = Search::new(universe, oracle, budget);
    session.set_log(log.clone());
    let ticket = session.submit(proposition);
    match session.wait(ticket) {
        Ok(view) => Ok(Searched {
            objects: window_lines(&view.universe)?,
            steps: view.steps,
            usage: view.usage,
            bytes_before: view.bytes_before as u64,
            bytes_after: view.bytes_after as u64,
            failure: None,
        }),
        Err(error) => {
            let message = error.to_string();
            Ok(Searched {
                objects: Vec::new(),
                steps: error.steps,
                usage: error.usage,
                bytes_before: 0,
                bytes_after: 0,
                failure: Some(message),
            })
        }
    }
}

fn read_cmd(
    proposition: Option<&str>,
    repo: &Path,
    budget: usize,
    judge: JudgeKind,
    full: bool,
    inspect: Option<&PathBuf>,
) -> Result<i32, Box<dyn std::error::Error>> {
    if let Some(path) = inspect {
        let file = ReadTraceFile::load(path)
            .map_err(|e| format!("could not read trace {}: {e}", path.display()))?;
        present_read(path, &file, full)?;
        return Ok(0);
    }
    let proposition = proposition.expect("proposition required");
    let started_at = std::time::SystemTime::now();
    let log = Log::create()?;
    let universe = Filesystem::at_root(repo)?;
    let read = match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            run_read(universe, oracle, proposition, budget, &log)?
        }
        JudgeKind::Jev => run_read(universe, JevOracle::from_env()?, proposition, budget, &log)?,
    };
    let ended_at = std::time::SystemTime::now();
    let repo_name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let file = ReadTraceFile {
        kind: autoground::read::trace::TRACE_KIND.to_string(),
        proposition: proposition.to_string(),
        repo_name: repo_name.to_string(),
        value: read.value,
        evidence: read.evidence,
        exhausted: read.exhausted,
        candidate_bytes: read.candidate_bytes as u64,
        evidence_bytes: read.evidence_bytes as u64,
        stances: read.stances,
        usage: UsageRecord::from_usage(&read.usage),
        started_at_ms: epoch_ms(started_at),
        ended_at_ms: epoch_ms(ended_at),
        steps: read.steps,
    };
    file.write_summary(&log)?;
    present_read(log.path(), &file, full)?;
    if let Some(err) = read.failure {
        eprintln!("{err}");
        Ok(3)
    } else {
        Ok(0)
    }
}

struct ReadOutcome {
    value: autoground::Value,
    evidence: Vec<String>,
    exhausted: bool,
    candidate_bytes: usize,
    evidence_bytes: usize,
    stances: Vec<autoground::read::Stance>,
    steps: Vec<autoground::read::ReadStep>,
    usage: Usage,
    failure: Option<String>,
}

fn run_read<O: Oracle>(
    universe: Filesystem,
    oracle: O,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<ReadOutcome, Box<dyn std::error::Error>> {
    let mut session = Reader::new(universe, oracle, budget);
    session.set_log(log.clone());
    let ticket = session.submit(proposition);
    match session.wait(ticket) {
        Ok(finding) => Ok(ReadOutcome {
            value: finding.value,
            evidence: finding.evidence.objects().to_vec(),
            exhausted: finding.exhausted,
            candidate_bytes: finding.candidate_bytes,
            evidence_bytes: finding.evidence_bytes,
            stances: finding.stances,
            steps: finding.steps,
            usage: finding.usage,
            failure: None,
        }),
        Err(error) => Ok(ReadOutcome {
            value: autoground::Value::Unknown,
            evidence: Vec::new(),
            exhausted: false,
            candidate_bytes: 0,
            evidence_bytes: 0,
            stances: Vec::new(),
            steps: Vec::new(),
            usage: error.usage,
            failure: Some(error.to_string()),
        }),
    }
}

fn verbalize_cmd(input: &str) -> Result<(), Box<dyn std::error::Error>> {
    let src = model_source(input)?;
    for line in verbalize_source(&src)? {
        println!("{line}");
    }
    Ok(())
}

fn run_model(
    model: &str,
    repo: &Path,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
) -> Result<i32, Box<dyn std::error::Error>> {
    let started_at = std::time::SystemTime::now();
    let src = model_source(model)?;
    let program = parse(&src)?;
    let compiled = compile(&program)?;
    let mut store = Store::new();
    let repo_name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("repo");
    let log = Log::create()?;

    let mut nav = make_nav(repo, judge, fixtures, &log, model)?;
    let result = run(&compiled, &mut store, Some(&mut nav));
    let ended_at = std::time::SystemTime::now();
    let judge_failed = !nav.errors.is_empty();
    if judge_failed {
        for e in &nav.errors {
            eprintln!("judge error: {e}");
        }
    }
    let steps = std::mem::take(&mut nav.trace);
    let requires = require_traces(&compiled, &result);
    log.append_event(
        "run",
        &RunSummary {
            outcome: &result.outcome,
            repo_name,
            requires: &requires,
            started_at_ms: epoch_ms(started_at),
            ended_at_ms: epoch_ms(ended_at),
        },
    )?;
    drop(nav);

    let header = format_run_header(&result.outcome, log.path(), jev_request_count(&steps));
    let body = format_run_body(&compiled, &result);
    autoground::report::tui::present_run(
        &header,
        &body,
        &steps,
        repo_name,
        RunTiming {
            started_at,
            ended_at,
        },
    )?;
    if judge_failed {
        Ok(3)
    } else {
        Ok(run_exit_code(&result.outcome))
    }
}

fn inspect_trace(path: &Path) -> Result<i32, Box<dyn std::error::Error>> {
    let file = load_run_log(path)?;
    let timing = match (file.started_at_ms, file.ended_at_ms) {
        (Some(s), Some(e)) => RunTiming::from_millis(s, e),
        _ => {
            let now = std::time::SystemTime::now();
            RunTiming {
                started_at: now,
                ended_at: now,
            }
        }
    };
    let header = format_run_header(&file.outcome, path, jev_request_count(&file.steps));
    let body = format_require_traces(&file.requires);
    autoground::report::tui::present_run(&header, &body, &file.steps, &file.repo_name, timing)?;
    Ok(run_exit_code(&file.outcome))
}

#[derive(serde::Serialize)]
struct RunSummary<'a> {
    outcome: &'a autoground::Outcome,
    repo_name: &'a str,
    requires: &'a [RequireTrace],
    started_at_ms: u64,
    ended_at_ms: u64,
}

/// Read `input` as a `.ag` file when it names one, otherwise treat it as source.
fn model_source(input: &str) -> Result<String, Box<dyn std::error::Error>> {
    let path = Path::new(input);
    if path.is_file() {
        return Ok(std::fs::read_to_string(path)?);
    }
    let trimmed = input.trim_start();
    if trimmed.starts_with('(') {
        return Ok(input.to_string());
    }
    if input.ends_with(".ag") || path.exists() {
        return Err(format!("model file not found: {input}").into());
    }
    Ok(input.to_string())
}

fn make_nav(
    repo: &Path,
    judge: JudgeKind,
    fixtures: Option<&PathBuf>,
    log: &Log,
    model: &str,
) -> Result<TreeNavigator, Box<dyn std::error::Error>> {
    let inner: Box<dyn autoground::navigator::Judge> = if let Some(path) = fixtures {
        Box::new(FixtureJudge::load(path)?)
    } else {
        match judge {
            JudgeKind::Heuristic => Box::new(autoground::HeuristicJudge),
            JudgeKind::Jev => Box::new(JevJudge::from_env()?),
        }
    };
    let mut nav = TreeNavigator::new(repo, Box::new(RecordingJudge::new(inner, log.clone())));
    nav.set_log(log.clone());
    let model_path = Path::new(model);
    if model_path.is_file() {
        nav.hide_model(model_path);
    }
    Ok(nav)
}
