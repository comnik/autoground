use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use autoground::log::Log;
use autoground::oracle::{Flight, HeuristicOracle, JevOracle, Oracle, Usage};
use autoground::plan::Plan;
use autoground::read::{
    present_read, Finding, ReadTraceFile, Reader, Split, Stance, Verdict, DEFAULT_BODY_BYTES,
};
use autoground::report::context::write_context;
use autoground::report::run::{Explain, Ground};
use autoground::report::summary::print_run_summary;
use autoground::search::trace::SearchStep;
use autoground::search::trace::UsageRecord;
use autoground::search::{
    present_search, window_lines, Search, SearchTraceFile, DEFAULT_BUDGET_BYTES,
};
use autoground::universe::{Filesystem, Transcript, Universe};
use autoground::{
    compile, display_path, lect_source, link, parse, scan_namespaces, verbalize_proposition,
    Proposition,
};
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;

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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ExplainMode {
    /// Comment each false clause with the file that contradicted it.
    Contradictions,
    /// Also comment each true clause with the file that supported it.
    All,
}

impl From<ExplainMode> for Explain {
    fn from(mode: ExplainMode) -> Self {
        match mode {
            ExplainMode::Contradictions => Explain::Contradictions,
            ExplainMode::All => Explain::All,
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Verbalize a model: fill each reading's holes with names.
    ///
    /// Examples:
    ///   autoground verbalize '(name Frontend) (name Backend) (sense part_of "{Name} is a part of {Name}") (part_of Frontend Backend)'
    ///   autoground verbalize examples/diagram.lect
    Verbalize {
        /// Atom, model source, a `.lect` file, or a Markdown file of `lect` blocks.
        input: String,
        /// Where required namespaces are found. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Decompose a model into atomic sentences.
    ///
    /// Each claim's readings are filled, then the distinct sentences are listed
    /// once, in the order the claims mention them. `and`, `or`, and `not` stay
    /// with the claim; this command does not read a repository.
    ///
    /// Examples:
    ///   autoground plan examples/diagram.lect
    ///   autoground plan '(name Frontend) (sense part_of "{Name} is a part of {Name}") (part_of Frontend Frontend)'
    Plan {
        /// Model source, a `.lect` file, or a Markdown file of `lect` blocks.
        input: String,
        /// Where required namespaces are found. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
    },
    /// Judge each claim in a model.
    ///
    /// Plans the model, reads every atomic sentence against the repository, and
    /// combines those truth values with `and`, `or`, and `not`. On a terminal
    /// each claim shows a spinner while the run is planning, searching, or
    /// reading, and the finished view is the model file with true and false
    /// claims marked. A false clause carries a comment naming the file that
    /// contradicted it. `--explain all` also comments true clauses. A session
    /// log is written under `/tmp/autoground/`.
    ///
    /// Examples:
    ///   autoground run examples/diagram.lect --repo .
    ///   autoground run examples/diagram.lect --judge heuristic --repo examples/toy-layered
    Run {
        /// Model source, a `.lect` file, or a Markdown file of `lect` blocks.
        input: String,
        /// Repository to read. Defaults to the current directory.
        #[arg(long, default_value = ".")]
        repo: PathBuf,
        /// Coding-agent transcript (`chat_history.jsonl`, or a session directory).
        /// When set, this is the universe instead of `--repo`.
        #[arg(long, value_name = "PATH")]
        transcript: Option<PathBuf>,
        /// Largest file body, in bytes, sent in one request. A longer file
        /// is sent in parts, and the run reports that split.
        #[arg(long, default_value_t = DEFAULT_BODY_BYTES)]
        budget: usize,
        #[arg(long, value_enum, default_value_t = JudgeKind::Jev)]
        judge: JudgeKind,
        /// Comments in the finished view. Defaults to contradictions.
        #[arg(long, value_enum, default_value_t = ExplainMode::Contradictions)]
        explain: ExplainMode,
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
    /// and exhausting it with no witness is false. A file over `--budget`
    /// is sent in parts, and the read reports that split. On a TTY the
    /// session log is written under `/tmp/autoground/`.
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
        /// Largest file body, in bytes, sent in one request. A longer file
        /// is sent in parts, and the read reports that split.
        #[arg(long, default_value_t = DEFAULT_BODY_BYTES)]
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
        Commands::Verbalize { input, repo } => match verbalize_cmd(&input, &repo) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Plan { input, repo } => match plan_cmd(&input, &repo) {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("{err}");
                ExitCode::from(3)
            }
        },
        Commands::Run {
            input,
            repo,
            transcript,
            budget,
            judge,
            explain,
        } => match run_cmd(&input, &repo, transcript.as_ref(), budget, judge, explain) {
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
        splits: Vec::new(),
        exchanges: Default::default(),
    };
    file.write_summary(&log)?;
    let file = ReadTraceFile::load(log.path())?;
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
        JudgeKind::Jev => run_read(
            universe,
            JevOracle::from_env(log.clone())?,
            proposition,
            budget,
            log,
        ),
    }
}

fn run_read<U: Universe + Clone, O: Oracle>(
    universe: U,
    oracle: O,
    proposition: &str,
    budget: usize,
    log: &Log,
) -> Result<ReadOutcome, Box<dyn std::error::Error>> {
    let mut session = Reader::new(universe, oracle, budget, Some(log.clone()));
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

fn verbalize_cmd(input: &str, repo: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (_src, _program, compiled) = load_compiled(input, repo)?;
    for claim in &compiled.claims {
        println!("{}", verbalize_proposition(claim));
    }
    Ok(())
}

fn plan_cmd(input: &str, repo: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let (_src, _program, compiled) = load_compiled(input, repo)?;
    let planned = Plan::of(&compiled);
    for atom in &planned.atoms {
        println!(
            "{}",
            verbalize_proposition(&Proposition::Sentence(atom.clone()))
        );
    }
    Ok(())
}

fn run_cmd(
    input: &str,
    repo: &Path,
    transcript: Option<&PathBuf>,
    budget: usize,
    judge: JudgeKind,
    explain: ExplainMode,
) -> Result<(), Box<dyn std::error::Error>> {
    let started = std::time::Instant::now();
    let live = if std::io::stdout().is_terminal() {
        Some(autoground::report::run::Live::open()?)
    } else {
        None
    };
    if let Some(live) = &live {
        live.show_planning()?;
    }
    let (src, program, compiled) = load_compiled(input, repo)?;
    let planned = Plan::of(&compiled);
    let log = Log::create()?;
    if let Some(live) = &live {
        if !planned.atoms.is_empty() {
            live.show_plan(&planned)?;
            let ui = live.clone();
            log.set_hook(move |event| ui.on_event(event));
        }
    }
    let judged = if planned.atoms.is_empty() {
        Judged {
            values: Vec::new(),
            grounds: Vec::new(),
            stances: Vec::new(),
            usage: Usage::default(),
            flights: Vec::new(),
            flight_limit: None,
            splits: Vec::new(),
        }
    } else if let Some(path) = transcript {
        let universe = Transcript::open(path)?;
        read_atoms(universe, judge, &planned.atoms, budget, &log)?
    } else {
        let mut universe = Filesystem::at_root(repo)?;
        let model = Path::new(input);
        if model.is_file() {
            universe.hide_model(model);
        }
        read_atoms(universe, judge, &planned.atoms, budget, &log)?
    };
    log.clear_hook();
    let atom_values = judged.values;
    let grounds = judged.grounds;
    let stances = judged.stances;
    let usage = judged.usage;
    let flights = judged.flights;
    let flight_limit = judged.flight_limit;
    let claim_values = planned.evaluate(&atom_values);
    let claims: Vec<_> = planned
        .claims
        .iter()
        .zip(&claim_values)
        .map(|(claim, value)| {
            json!({
                "claim": claim.verbalized,
                "value": value,
            })
        })
        .collect();
    log.append_event(
        "run",
        &json!({
            "claims": claims,
            "usage": UsageRecord::from_usage(&usage),
        }),
    )?;
    write_context(log.path(), &planned, &claim_values, &stances)?;
    let true_count = claim_values
        .iter()
        .filter(|value| **value == autoground::Value::True)
        .count();
    if let Some(live) = live {
        live.finish(
            &src,
            &program.clauses,
            &planned,
            &atom_values,
            &grounds,
            explain.into(),
        )?;
    } else {
        for (claim, value) in planned.claims.iter().zip(&claim_values) {
            println!("{:<7}  {}", value.to_string(), claim.verbalized);
        }
    }
    for split in &judged.splits {
        println!("{}", split.line());
    }
    print_run_summary(
        &usage,
        &flights,
        flight_limit,
        true_count,
        claim_values.len(),
        started.elapsed(),
        log.path(),
    )?;
    Ok(())
}

fn load_compiled(
    input: &str,
    repo: &Path,
) -> Result<(String, autoground::Program, autoground::Compiled), Box<dyn std::error::Error>> {
    let src = model_source(input)?;
    let program = parse(&src)?;
    let compiled = if program
        .namespace
        .as_ref()
        .is_some_and(|ns| !ns.requires.is_empty())
    {
        let files = scan_namespaces(repo)?;
        let entry = Path::new(input);
        let entry_path = entry.is_file().then(|| display_path(repo, entry));
        link(&program, entry_path.as_deref(), &files)?
    } else {
        compile(&program)?
    };
    Ok((src, program, compiled))
}

struct Judged {
    values: Vec<autoground::Value>,
    grounds: Vec<Ground>,
    stances: Vec<Vec<Stance>>,
    usage: Usage,
    flights: Vec<Flight>,
    flight_limit: Option<usize>,
    splits: Vec<Split>,
}

fn read_atoms<U: Universe + Clone>(
    universe: U,
    judge: JudgeKind,
    atoms: &[String],
    budget: usize,
    log: &Log,
) -> Result<Judged, Box<dyn std::error::Error>> {
    match judge {
        JudgeKind::Heuristic => {
            let oracle = HeuristicOracle::new(universe.clone());
            read_with(universe, oracle, atoms, budget, log)
        }
        JudgeKind::Jev => read_with(
            universe,
            JevOracle::from_env(log.clone())?,
            atoms,
            budget,
            log,
        ),
    }
}

fn read_with<U: Universe + Clone, O: Oracle>(
    universe: U,
    oracle: O,
    atoms: &[String],
    budget: usize,
    log: &Log,
) -> Result<Judged, Box<dyn std::error::Error>> {
    let mut reader = Reader::new(universe, oracle, budget, Some(log.clone()));
    let tickets: Vec<_> = atoms
        .iter()
        .map(|atom| reader.submit(atom.clone()))
        .collect();
    let mut values = Vec::with_capacity(tickets.len());
    let mut grounds = Vec::with_capacity(tickets.len());
    let mut stances = Vec::with_capacity(tickets.len());
    for result in reader.wait_many(&tickets) {
        let finding = result?;
        values.push(finding.value);
        grounds.push(ground_of(&finding));
        stances.push(finding.stances);
    }
    let flights = reader.flights().to_vec();
    let flight_limit = reader.flight_limit();
    let usage = reader.usage();
    let splits = reader.splits().to_vec();
    Ok(Judged {
        values,
        grounds,
        stances,
        usage,
        flights,
        flight_limit,
        splits,
    })
}

fn ground_of<U>(finding: &Finding<U>) -> Ground {
    let mut entailed = Vec::new();
    let mut contradicted = Vec::new();
    for stance in &finding.stances {
        match stance.verdict {
            Verdict::Entailed => entailed.push(stance.id.clone()),
            Verdict::Contradicted => contradicted.push(stance.id.clone()),
            Verdict::Neutral => {}
        }
    }
    match finding.value {
        autoground::Value::True => Ground::Entailed(entailed),
        autoground::Value::False if !contradicted.is_empty() => Ground::Contradicted(contradicted),
        autoground::Value::False => Ground::Unsupported,
        autoground::Value::Unknown => Ground::Unknown,
    }
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
