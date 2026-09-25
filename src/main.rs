use std::path::{Path, PathBuf};
use std::process::ExitCode;

use autoground::log::Log;
use autoground::navigator::judge::epoch_ms;
use autoground::oracle::{HeuristicOracle, JevOracle, Oracle, Usage};
use autoground::read::{present_read, ReadTraceFile, Reader};
use autoground::search::trace::SearchStep;
use autoground::search::trace::UsageRecord;
use autoground::search::{
    present_search, window_lines, Search, SearchTraceFile, DEFAULT_BUDGET_BYTES,
};
use autoground::universe::{Filesystem, Transcript, Universe};
use autoground::verbalize_source;
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
    /// Verbalize a model: fill each reading's holes with names.
    ///
    /// Examples:
    ///   autoground verbalize '(name Frontend) (name Backend) (sense part_of "{Name} is a part of {Name}") (part_of Frontend Backend)'
    ///   autoground verbalize examples/diagram.ag
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
    ///   autoground read "the agent read the universe trait" --transcript chat_history.jsonl
    ///   autoground read --inspect /tmp/autoground/trace-….jsonl
    Read {
        /// Natural-language proposition.
        #[arg(required_unless_present = "inspect")]
        proposition: Option<String>,
        /// Repository to read. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Coding-agent transcript (`chat_history.jsonl`, or a session directory).
        /// When set, this is the universe instead of `--repo`.
        #[arg(long, value_name = "PATH")]
        transcript: Option<PathBuf>,
        /// Render budget in bytes for one file body.
        #[arg(long, default_value_t = DEFAULT_BUDGET_BYTES)]
        budget: usize,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        /// Also print every judged database as text.
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
        Commands::Read {
            proposition,
            repo,
            transcript,
            budget,
            judge,
            full,
            inspect,
        } => match read_cmd(
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
        JudgeKind::Jev => run_search(universe, JevOracle::from_env()?, proposition, budget, log),
    }
}

fn run_search<U: Universe + Clone, O: Oracle>(
    universe: U,
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

fn read_cmd(
    proposition: Option<&str>,
    repo: &Path,
    transcript: Option<&PathBuf>,
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
    let (repo_name, read) = if let Some(path) = transcript {
        let universe = Transcript::open(path)?;
        let read = run_judged_read(universe, judge, proposition, budget, &log)?;
        (path_label(path, "transcript"), read)
    } else {
        let universe = Filesystem::at_root(repo)?;
        let read = run_judged_read(universe, judge, proposition, budget, &log)?;
        (path_label(repo, "repo"), read)
    };
    let ended_at = std::time::SystemTime::now();
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

fn run_judged_read<U: Universe + Clone>(
    universe: U,
    judge: JudgeKind,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<ReadOutcome, Box<dyn std::error::Error>> {
    match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            run_read(universe, oracle, proposition, budget, log)
        }
        JudgeKind::Jev => run_read(universe, JevOracle::from_env()?, proposition, budget, log),
    }
}

fn run_read<U: Universe + Clone, O: Oracle>(
    universe: U,
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
            evidence: finding.evidence.databases().to_vec(),
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
